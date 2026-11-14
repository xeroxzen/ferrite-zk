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
//!   [`ClientConfig::request_timeout`]. Under the default
//!   [`ReplyIdPolicy::Strict`], packets with another reply ID are discarded
//!   while waiting. A timeout returns
//!   [`Error::Timeout`] and closes the connection, so a late reply can never
//!   be mistaken for the answer to a later request. Later calls return
//!   [`Error::ConnectionClosed`] until [`Client::reconnect`].
//! - [`Transport::Auto`] (the default) uses TCP, falling back to UDP.
//! - Table reads stage the table on the device, read it in chunks (64 KiB
//!   over TCP; one datagram per request over UDP, see [`Transport::Udp`];
//!   each retried per [`RetryPolicy`] if its reply is malformed or short),
//!   and release it.
//! - [`ClientConfig::read_only`] refuses every command outside
//!   [`READ_ONLY_COMMANDS`] before it is sent.
//! - TCP and UDP behave the same, except that over UDP a stream that stalls
//!   mid-chunk (a lost datagram) is a retryable [`Error::ShortRead`] rather
//!   than a connection-closing timeout.
//! - Table decoding isolates bad records: see [`zkteco_proto::Batch`].
//! - Live events that arrive while waiting for a reply are acknowledged and
//!   queued, never dropped silently: see [`Client::take_pending_events`].
//! - [`Client::live_capture`] turns the client into a cancel-safe
//!   [`LiveCapture`] stream, optionally reconnecting; [`LiveCapture::stop`]
//!   gives the client back.
//! - Users have two identities, [`Uid`] (device slot) and [`UserId`] (the ID
//!   people see); they are distinct types.
//! - Methods documented as **Destructive** change or erase device data.
//!
//! Packets are logged with `tracing`: requests and replies at `trace` with
//! hex dumps, recoveries at `warn`.

mod client;
pub mod compat;
mod config;
mod data;
mod device;
mod diag;
mod enroll;
mod error;
mod link;
mod live;
mod record;
mod transfer;
mod write;

pub use client::{probe, Client};
pub use config::{ClientConfig, ReplyIdPolicy, RetryPolicy, Transport, DEFAULT_PORT, READ_ONLY_COMMANDS};
pub use data::{RawTable, ResolvedAttendance, UserIdSource};
pub use device::NetworkParams;
pub use diag::{SampledPacket, StreamSample};
pub use enroll::{EnrollOptions, EnrollOutcome};
pub use error::{Error, Result};
pub use live::{CaptureOptions, LiveCapture, ReconnectPolicy};
pub use record::{Direction, RecordedPacket, Recording};
pub use zkteco_proto;
pub use zkteco_proto::records::attendance::{Attendance, AttendanceBatch};
pub use zkteco_proto::records::event::{EnrollReport, LiveEvent};
pub use zkteco_proto::records::finger::Template;
pub use zkteco_proto::records::user::{Privilege, User};
pub use zkteco_proto::{Batch, EventFlags, Uid, UserId};
