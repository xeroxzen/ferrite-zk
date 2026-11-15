//! Which devices a command talks to, and how it connects to them.

use anyhow::{Context, Result};
use zkteco_client::{Client, ClientConfig, Error, Transport};

use crate::cli::Global;
use crate::duration::parse_duration;
use crate::exit::Fail;
use crate::profiles;
use crate::secrets;

/// One device to talk to.
#[derive(Debug, Clone)]
pub struct Target {
    /// The profile name, or the host when given with `--host`.
    pub name: String,
    /// Connection settings, without the password unless given on the
    /// command line or in `ZK_PASSWORD`.
    pub config: ClientConfig,
    /// Whether `name` is a profile (and may have a keyring password).
    pub profile: bool,
}

/// What a connection may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read-only: the client refuses anything outside its allowlist.
    ReadOnly,
    /// Read-only, plus subscribing to live events.
    Live,
    /// Writes allowed; with `dry_run`, writes are recorded, not sent.
    Write {
        /// Record writes instead of sending them.
        dry_run: bool,
    },
}

/// The devices named on the command line.
///
/// # Errors
///
/// A usage error if none is given, a profile is unknown or invalid.
pub fn resolve(global: &Global) -> Result<Vec<Target>> {
    if let Some(host) = &global.host {
        let mut config = ClientConfig::new(host.clone());
        config = apply_overrides(config, global);
        return Ok(vec![Target {
            name: host.clone(),
            config,
            profile: false,
        }]);
    }
    if global.devices.is_empty() {
        return Err(Fail::usage("no device: pass --host <HOST> or -d <NAME>"));
    }
    let path = profiles::path(global.config.as_deref())?;
    let file = profiles::load(&path)?;
    let mut targets = Vec::with_capacity(global.devices.len());
    for name in &global.devices {
        if targets.iter().any(|t: &Target| &t.name == name) {
            return Err(Fail::usage(format!("device {name:?} given twice")));
        }
        let profile = profiles::get(&file, name, &path)?;
        let mut config = ClientConfig::new(profile.host.clone());
        if let Some(port) = profile.port {
            config = config.port(port);
        }
        if let Some(t) = &profile.transport {
            config = config.transport(parse_transport(t)?);
        }
        if let Some(t) = &profile.timeout {
            let d = parse_duration(t).map_err(|e| Fail::usage(format!("device {name:?}: timeout: {e}")))?;
            config = config.request_timeout(d);
        }
        targets.push(Target {
            name: name.clone(),
            config: apply_overrides(config, global),
            profile: true,
        });
    }
    Ok(targets)
}

/// Exactly one device.
///
/// # Errors
///
/// As for [`resolve`], or a usage error for more than one.
pub fn single(global: &Global) -> Result<Target> {
    let mut targets = resolve(global)?;
    if targets.len() > 1 {
        return Err(Fail::usage("this command takes one device; pass a single -d"));
    }
    Ok(targets.remove(0))
}

fn apply_overrides(mut config: ClientConfig, global: &Global) -> ClientConfig {
    if let Some(port) = global.port {
        config = config.port(port);
    }
    if let Some(t) = global.transport {
        config = config.transport(t.into());
    }
    if let Some(t) = global.timeout {
        config = config.request_timeout(t);
    }
    if let Some(pw) = global.password {
        config = config.password(pw);
    }
    config
}

/// Parse a profile's transport.
///
/// # Errors
///
/// A usage error for anything but `auto`, `tcp` or `udp`.
pub fn parse_transport(text: &str) -> Result<Transport> {
    match text {
        "auto" => Ok(Transport::Auto),
        "tcp" => Ok(Transport::Tcp),
        "udp" => Ok(Transport::Udp),
        other => Err(Fail::usage(format!("transport {other:?}: use auto, tcp or udp"))),
    }
}

/// The configuration for `access`, with the keyring password filled in if
/// none was given.
pub async fn config_for(target: &Target, access: Access) -> ClientConfig {
    let mut config = target.config.clone();
    config = match access {
        Access::ReadOnly => config.read_only(true),
        Access::Live => config.read_only(true).allow_event_subscription(true),
        Access::Write { dry_run } => config.dry_run(dry_run),
    };
    if config.password.is_none() && target.profile {
        if let Some(pw) = secrets::keyring_get(&target.name).await {
            config = config.password(pw);
        }
    }
    config
}

/// Connect for `access`. If the device wants a password that was not
/// given and someone is at the terminal, ask once.
///
/// # Errors
///
/// Connection failures, with the device named.
pub async fn connect(target: &Target, access: Access) -> Result<Client> {
    let config = config_for(target, access).await;
    let had_password = config.password.is_some();
    match Client::connect(config.clone()).await {
        Err(Error::Unauthorized) if !had_password => {
            let Some(pw) = secrets::prompt(&target.name)? else {
                return Err(anyhow::Error::from(Error::Unauthorized)).with_context(|| {
                    format!(
                        "{}: the device needs a password (--password, ZK_PASSWORD or `zkctl device set-password`)",
                        target.name
                    )
                });
            };
            Client::connect(config.password(pw))
                .await
                .with_context(|| format!("connecting to {}", target.name))
        }
        other => other.with_context(|| format!("connecting to {}", target.name)),
    }
}
