//! Print the attendance records at or after a time, with user IDs
//! resolved from the user table where the log lacks them.
//!
//! ```sh
//! ZK_HOST=192.168.1.201 cargo run --example pull_attendance_since -- 2026-01-01T00:00:00
//! ```

use chrono::NaiveDateTime;
use zkteco_client::{Client, ClientConfig, ResolvedAttendance};

/// Records at or after `since` (device local time), in log order.
///
/// # Errors
///
/// Connection and read failures.
pub async fn run(
    config: ClientConfig,
    since: NaiveDateTime,
) -> zkteco_client::Result<Vec<ResolvedAttendance>> {
    let mut client = Client::connect(config.read_only(true)).await?;
    let batch = client.attendance_with_users().await?;
    for bad in &batch.rejected {
        eprintln!(
            "attendance record at offset {} rejected: {}",
            bad.offset, bad.reason
        );
    }
    client.disconnect().await?;
    Ok(batch
        .records
        .into_iter()
        .filter(|r| r.record.timestamp >= since)
        .collect())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let since = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "2000-01-01T00:00:00".into());
    let since = NaiveDateTime::parse_from_str(&since, "%Y-%m-%dT%H:%M:%S").map_err(|e| e.to_string())?;
    let host = std::env::var("ZK_HOST").unwrap_or_else(|_| "192.168.1.201".into());
    for r in run(ClientConfig::new(host), since).await? {
        let who = r.record.user_id.as_ref().map_or("?", |id| id.as_str());
        println!(
            "{}  {who:<12}  {:?}  {:?}",
            r.record.timestamp, r.record.status, r.record.punch
        );
    }
    Ok(())
}
