//! List the users on a device.
//!
//! ```sh
//! ZK_HOST=192.168.1.201 [ZK_PORT=4370] [ZK_PASSWORD=0] cargo run --example list_users
//! ```

use zkteco_client::{Client, ClientConfig, User};

/// Connect read-only and return the users. Records that fail to decode are
/// reported, not dropped.
///
/// # Errors
///
/// Connection and read failures.
pub async fn run(config: ClientConfig) -> zkteco_client::Result<Vec<User>> {
    let mut client = Client::connect(config.read_only(true)).await?;
    let users = client.users().await?;
    for bad in &users.rejected {
        eprintln!("user record at offset {} rejected: {}", bad.offset, bad.reason);
    }
    client.disconnect().await?;
    Ok(users.records)
}

/// Settings from `ZK_HOST`, `ZK_PORT` and `ZK_PASSWORD`.
#[must_use]
pub fn config_from_env() -> ClientConfig {
    let host = std::env::var("ZK_HOST").unwrap_or_else(|_| "192.168.1.201".into());
    let mut config = ClientConfig::new(host);
    if let Some(port) = std::env::var("ZK_PORT").ok().and_then(|p| p.parse().ok()) {
        config = config.port(port);
    }
    if let Some(pw) = std::env::var("ZK_PASSWORD").ok().and_then(|p| p.parse().ok()) {
        config = config.password(pw);
    }
    config
}

#[tokio::main]
async fn main() -> zkteco_client::Result<()> {
    for user in run(config_from_env()).await? {
        println!("{:>5}  {:<12}  {}", user.uid.0, user.user_id, user.display_name());
    }
    Ok(())
}
