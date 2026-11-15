//! `zkctl device add|list|remove|show|set-password`.

use anyhow::Result;
use serde::Serialize;

use crate::cli::{DeviceCommand, Format, Global, TransportArg};
use crate::exit::{Exit, Fail};
use crate::output;
use crate::profiles::{self, Profile};
use crate::secrets;

/// A device as printed: every key present, `null` when unset.
#[derive(Debug, Serialize)]
struct Named<'a> {
    name: &'a str,
    host: &'a str,
    port: Option<u16>,
    transport: Option<&'a str>,
    timeout: Option<&'a str>,
}

impl<'a> Named<'a> {
    fn new(name: &'a str, p: &'a Profile) -> Self {
        Self {
            name,
            host: &p.host,
            port: p.port,
            transport: p.transport.as_deref(),
            timeout: p.timeout.as_deref(),
        }
    }
}

pub(super) async fn run(g: &Global, cmd: DeviceCommand) -> Result<Exit> {
    let path = profiles::path(g.config.as_deref())?;
    let mut file = profiles::load(&path)?;
    match cmd {
        DeviceCommand::Add { name } => {
            let host = g
                .host
                .clone()
                .ok_or_else(|| Fail::usage("device add needs --host <HOST>"))?;
            if g.password.is_some() {
                eprintln!("note: passwords are not stored in the config file; use `zkctl device set-password {name}`");
            }
            let profile = Profile {
                host,
                port: g.port,
                transport: g.transport.map(|t| transport_name(t).to_owned()),
                timeout: g.timeout.map(|d| format!("{}ms", d.as_millis())),
            };
            file.devices.insert(name.clone(), profile);
            profiles::save(&path, &file)?;
            eprintln!("saved {name} in {}", path.display());
        }
        DeviceCommand::List => print_list(g.output, &file)?,
        DeviceCommand::Show { name } => {
            let profile = profiles::get(&file, &name, &path)?;
            match g.output {
                Format::Human => output::text(&format!(
                    "{name}\n{}",
                    toml::to_string_pretty(profile)?.trim_end()
                ))?,
                _ => output::json(&Named::new(&name, profile))?,
            }
        }
        DeviceCommand::Remove { name } => {
            if file.devices.remove(&name).is_none() {
                return Err(Fail::usage(format!("no device named {name:?}")));
            }
            profiles::save(&path, &file)?;
            secrets::keyring_delete(&name).await?;
            eprintln!("removed {name}");
        }
        DeviceCommand::SetPassword { name } => {
            profiles::get(&file, &name, &path)?;
            let password = match g.password {
                Some(pw) => pw,
                None => secrets::prompt(&name)?
                    .ok_or_else(|| Fail::usage("no terminal to ask on: pass ZK_PASSWORD"))?,
            };
            secrets::keyring_set(&name, password).await?;
            eprintln!("stored the password for {name} in the OS keyring");
        }
    }
    Ok(Exit::Ok)
}

fn transport_name(t: TransportArg) -> &'static str {
    match t {
        TransportArg::Auto => "auto",
        TransportArg::Tcp => "tcp",
        TransportArg::Udp => "udp",
    }
}

fn print_list(format: Format, file: &profiles::ConfigFile) -> Result<()> {
    let named: Vec<Named<'_>> = file
        .devices
        .iter()
        .map(|(name, profile)| Named::new(name, profile))
        .collect();
    match format {
        Format::Json => output::json(&named),
        Format::Ndjson => named.iter().try_for_each(output::ndjson),
        Format::Human => {
            let rows: Vec<Vec<String>> = named
                .iter()
                .map(|n| {
                    vec![
                        n.name.to_owned(),
                        n.host.to_owned(),
                        n.port.map(|p| p.to_string()).unwrap_or_default(),
                        n.transport.unwrap_or_default().to_owned(),
                        n.timeout.unwrap_or_default().to_owned(),
                    ]
                })
                .collect();
            output::text(&output::table(
                &["NAME", "HOST", "PORT", "TRANSPORT", "TIMEOUT"],
                &rows,
            ))
        }
    }
}
