//! Device passwords: the OS keyring and the interactive prompt. The
//! command line and `ZK_PASSWORD` are handled by clap; the config file
//! never holds passwords.

use std::io::IsTerminal;

use anyhow::{Context, Result};

use crate::exit::Fail;

const SERVICE: &str = "zkctl";

/// Whether the keyring may be used. `ZKCTL_NO_KEYRING=1` turns it off (for
/// tests and machines without a keyring service).
#[must_use]
pub fn keyring_enabled() -> bool {
    std::env::var("ZKCTL_NO_KEYRING").map_or(true, |v| v != "1")
}

/// The stored password for device `name`, if any. A keyring that cannot be
/// reached counts as none (logged).
pub async fn keyring_get(name: &str) -> Option<u32> {
    if !keyring_enabled() {
        return None;
    }
    let name = name.to_owned();
    let found = tokio::task::spawn_blocking(move || {
        keyring::v1::Entry::new(SERVICE, &name).and_then(|e| e.get_password())
    })
    .await;
    match found {
        Ok(Ok(text)) => {
            let parsed = text.parse().ok();
            if parsed.is_none() {
                tracing::warn!("keyring password for this device is not a number; ignored");
            }
            parsed
        }
        Ok(Err(keyring::v1::Error::NoEntry)) => None,
        Ok(Err(e)) => {
            tracing::warn!(%e, "keyring unavailable");
            None
        }
        Err(e) => {
            tracing::warn!(%e, "keyring lookup failed");
            None
        }
    }
}

/// Store `password` for device `name`.
///
/// # Errors
///
/// If the keyring is off or refuses.
pub async fn keyring_set(name: &str, password: u32) -> Result<()> {
    if !keyring_enabled() {
        return Err(Fail::usage("the keyring is turned off (ZKCTL_NO_KEYRING=1)"));
    }
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || {
        keyring::v1::Entry::new(SERVICE, &name).and_then(|e| e.set_password(&password.to_string()))
    })
    .await?
    .context("storing the password in the OS keyring")
}

/// Remove the stored password for `name`, if any.
///
/// # Errors
///
/// If the keyring refuses (a missing entry is fine).
pub async fn keyring_delete(name: &str) -> Result<()> {
    if !keyring_enabled() {
        return Ok(());
    }
    let name = name.to_owned();
    let result = tokio::task::spawn_blocking(move || {
        keyring::v1::Entry::new(SERVICE, &name).and_then(|e| e.delete_credential())
    })
    .await?;
    match result {
        Ok(()) | Err(keyring::v1::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).context("removing the password from the OS keyring"),
    }
}

/// Whether a person is at the terminal to answer prompts.
#[must_use]
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Ask for a password on the terminal. `None` when nobody can answer.
///
/// # Errors
///
/// If the terminal cannot be read or the answer is not a number.
pub fn prompt(device: &str) -> Result<Option<u32>> {
    if !interactive() {
        return Ok(None);
    }
    let text =
        rpassword::prompt_password(format!("Password for {device}: ")).context("reading the password")?;
    let pw = text
        .trim()
        .parse()
        .map_err(|_| Fail::usage("device passwords are numbers"))?;
    Ok(Some(pw))
}
