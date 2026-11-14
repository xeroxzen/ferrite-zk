//! Async client for `ZKTeco` biometric and access-control devices, built on
//! [`zkteco-proto`](zkteco_proto).
//!
//! ```no_run
//! use zkteco_client::{Client, ClientConfig};
//!
//! # async fn run() -> zkteco_client::Result<()> {
//! let mut client = Client::connect(ClientConfig::new("192.168.1.201").password(0)).await?;
//! println!("firmware {}", client.firmware_version().await?);
//! let users = client.users().await?;
//! for user in &users.records {
//!     println!("{} {}", user.user_id, user.display_name());
//! }
//! for bad in &users.rejected {
//!     eprintln!("record at offset {} rejected: {}", bad.offset, bad.reason);
//! }
//! client.disconnect().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Behaviour
//!
//! - One request is in flight at a time; methods take `&mut self`.
//! - Every reply waits at most
//!   [`ClientConfig::request_timeout`]. A timeout returns
//!   [`Error::Timeout`] and closes the connection, so a late reply can never
//!   be mistaken for the answer to a later request. Later calls return
//!   [`Error::ConnectionClosed`] until [`Client::reconnect`].
//! - Table reads stage the table on the device, read it in chunks (each
//!   retried per [`RetryPolicy`] if its reply is malformed), and release it.
//! - Table decoding isolates bad records: see [`zkteco_proto::Batch`].
//! - Methods documented as **Destructive** change or erase device data.
//! - Only TCP is implemented so far.
//!
//! Packets are logged with `tracing`: requests and replies at `trace` with
//! hex dumps, recoveries at `warn`.

mod client;
mod config;
mod data;
mod device;
mod error;
mod link;
mod transfer;

pub use client::{probe, Client};
pub use config::{ClientConfig, RetryPolicy, DEFAULT_PORT};
pub use device::NetworkParams;
pub use error::{Error, Result};
pub use zkteco_proto;
pub use zkteco_proto::records::attendance::{Attendance, AttendanceBatch};
pub use zkteco_proto::records::finger::Template;
pub use zkteco_proto::records::user::{Privilege, User};
pub use zkteco_proto::Batch;
