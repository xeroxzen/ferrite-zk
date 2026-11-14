#![doc = include_str!("../README.md")]

pub mod command;
pub mod commkey;
mod error;
pub mod framing;
pub mod packet;
pub mod records;
pub mod requests;
pub mod session;
mod text;
pub mod time;
pub mod transfer;

pub use command::{Command, EventFlags};
pub use error::{ProtoError, RecordKind, Result};
pub use packet::{ChecksumPolicy, Header, Packet};
pub use records::ids::{Uid, UserId};
pub use records::{Batch, RejectedRecord};
pub use text::TextEncoding;
pub use transfer::Transport;
