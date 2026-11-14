use std::time::Duration;

use zkteco_proto::{Command, ProtoError, RecordKind, UserId};

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors returned by [`Client`](crate::Client).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A socket operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// No reply arrived in time. The connection is closed afterwards, because
    /// a late reply could otherwise be taken as the answer to a later request.
    #[error("timed out after {after:?} waiting for the reply to {command:?}")]
    Timeout {
        /// The request that went unanswered (`Connect` for the TCP connect).
        command: Command,
        /// The timeout that elapsed.
        after: Duration,
    },

    /// The device rejected the password (`CMD_ACK_UNAUTH`), or required one
    /// and none was configured.
    #[error("authentication failed")]
    Unauthorized,

    /// The device answered with a failure reply such as `CMD_ACK_ERROR`.
    #[error("device replied {reply:?} to {command:?}")]
    DeviceError {
        /// The request.
        command: Command,
        /// The reply code.
        reply: Command,
    },

    /// A packet could not be decoded or did not fit the exchange.
    #[error("malformed reply: {0}")]
    Malformed(#[source] ProtoError),

    /// A table's record layout is unknown or cannot be determined.
    #[error("unsupported {kind:?} record format: {detail}")]
    UnsupportedRecordFormat {
        /// Which table.
        kind: RecordKind,
        /// What was found.
        detail: String,
    },

    /// A transfer ended with fewer bytes than announced.
    #[error("short read: expected {expected} bytes, got {got}")]
    ShortRead {
        /// Bytes announced.
        expected: usize,
        /// Bytes received.
        got: usize,
    },

    /// The connection is closed: the peer closed it, or an earlier timeout
    /// or framing error made it unusable. Call
    /// [`Client::reconnect`](crate::Client::reconnect).
    #[error("connection closed")]
    ConnectionClosed,

    /// The operation is not available with this configuration.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),

    /// No user has this user ID.
    #[error("no user with user ID {0}")]
    UserNotFound(UserId),

    /// The client is read-only ([`ClientConfig::read_only`](crate::ClientConfig::read_only))
    /// and this command is not in
    /// [`READ_ONLY_COMMANDS`](crate::READ_ONLY_COMMANDS). Nothing was sent.
    #[error("{0:?} refused: the client is read-only")]
    ReadOnly(Command),

    /// An argument cannot be encoded for the device (for example a name too
    /// long for its field).
    #[error("invalid argument: {0}")]
    InvalidArgument(#[source] ProtoError),
}

impl From<ProtoError> for Error {
    fn from(e: ProtoError) -> Self {
        match e {
            ProtoError::UnsupportedRecordFormat { kind, detail } => {
                Self::UnsupportedRecordFormat { kind, detail }
            }
            ProtoError::ShortRead { expected, got } => Self::ShortRead { expected, got },
            other => Self::Malformed(other),
        }
    }
}

impl Error {
    /// Whether this kind of error always closes the connection (I/O,
    /// timeout, closed). A TCP framing error, reported as
    /// [`Error::Malformed`], closes it too;
    /// [`Client::is_connected`](crate::Client::is_connected) is authoritative.
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Io(_) | Self::Timeout { .. } | Self::ConnectionClosed)
    }
}
