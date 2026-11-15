//! Named devices in the config file. Passwords are never stored here: the
//! file rejects a `password` key.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::exit::Fail;

/// The config file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Devices by name.
    #[serde(default)]
    pub devices: BTreeMap<String, Profile>,
}

/// One named device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Host name or address.
    pub host: String,
    /// Port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// `auto`, `tcp` or `udp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// Per-reply timeout, as on the command line (`5s`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

/// The config file path: `explicit`, or `config.toml` in the platform
/// config directory.
///
/// # Errors
///
/// If there is no home directory to put it in.
pub fn path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let dirs = directories::ProjectDirs::from("", "", "zkctl")
        .context("no home directory for the zkctl config file; pass --config")?;
    Ok(dirs.config_dir().join("config.toml"))
}

/// Load the file; a missing file is an empty config.
///
/// # Errors
///
/// If it cannot be read or parsed.
pub fn load(path: &Path) -> Result<ConfigFile> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ConfigFile::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Write the file, replacing it atomically.
///
/// # Errors
///
/// If it cannot be written.
pub fn save(path: &Path, config: &ConfigFile) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = toml::to_string_pretty(config)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

/// The profile called `name`.
///
/// # Errors
///
/// A usage error if there is none.
pub fn get<'a>(config: &'a ConfigFile, name: &str, path: &Path) -> Result<&'a Profile> {
    config.devices.get(name).ok_or_else(|| {
        Fail::usage(format!(
            "no device named {name:?} in {} (see `zkctl device list`)",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_password_rejected() {
        let mut c = ConfigFile::default();
        c.devices.insert(
            "front".into(),
            Profile {
                host: "10.0.0.5".into(),
                port: Some(4370),
                transport: Some("tcp".into()),
                timeout: None,
            },
        );
        let text = toml::to_string_pretty(&c).unwrap();
        assert_eq!(toml::from_str::<ConfigFile>(&text).unwrap(), c);
        let with_pw = "[devices.front]\nhost = \"10.0.0.5\"\npassword = 1\n";
        assert!(toml::from_str::<ConfigFile>(with_pw).is_err());
    }
}
