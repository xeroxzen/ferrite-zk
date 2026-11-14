use crate::command::Command;

/// Errors produced while encoding or decoding protocol data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProtoError {
    /// The input ended before a complete structure could be read.
    #[error("truncated {what}: need {needed} bytes, have {available}")]
    Truncated {
        /// What was being decoded.
        what: &'static str,
        /// Bytes required.
        needed: usize,
        /// Bytes available.
        available: usize,
    },

    /// A packet checksum did not match its contents.
    #[error("checksum mismatch: packet carries {actual:#06x}, computed {expected:#06x}")]
    ChecksumMismatch {
        /// Checksum computed over the packet.
        expected: u16,
        /// Checksum carried in the packet.
        actual: u16,
    },

    /// A TCP frame did not start with the `50 50 82 7d` magic.
    #[error("bad TCP frame magic {found:02x?}")]
    BadTcpMagic {
        /// The four bytes found where the magic was expected.
        found: [u8; 4],
    },

    /// A TCP frame announced a length outside the accepted range.
    #[error("TCP frame length {len} outside accepted range {min}..={max}")]
    BadFrameLength {
        /// Announced length.
        len: usize,
        /// Minimum accepted length (a bare header).
        min: usize,
        /// Maximum accepted length.
        max: usize,
    },

    /// A record table or event payload has a layout this crate does not know.
    #[error("unsupported {kind} format: {detail}")]
    UnsupportedRecordFormat {
        /// Kind of record.
        kind: RecordKind,
        /// Human-readable description of what was seen.
        detail: String,
    },

    /// A packed timestamp does not correspond to a real calendar date and time.
    #[error("invalid device timestamp {0:#010x}")]
    InvalidTimestamp(u32),

    /// A 6-byte compact timestamp does not correspond to a real date and time.
    #[error("invalid compact timestamp {0:02x?}")]
    InvalidCompactTimestamp([u8; 6]),

    /// A date cannot be represented on the device (years 2000 to 2099 only).
    #[error("year {0} cannot be represented on the device (2000..=2099)")]
    YearOutOfRange(i32),

    /// A text field does not fit its fixed-width slot.
    #[error("field `{field}` is {len} bytes encoded, maximum is {max}")]
    FieldTooLong {
        /// Field name.
        field: &'static str,
        /// Encoded length.
        len: usize,
        /// Maximum length.
        max: usize,
    },

    /// A field value is not valid for the target layout.
    #[error("invalid value for `{field}`: {reason}")]
    InvalidField {
        /// Field name.
        field: &'static str,
        /// Why it is invalid.
        reason: String,
    },

    /// Text could not be represented in the configured encoding.
    #[error("field `{field}` cannot be encoded as {encoding}")]
    Unencodable {
        /// Field name.
        field: &'static str,
        /// Encoding name.
        encoding: &'static str,
    },

    /// Attempted to write a privilege value the protocol does not define.
    #[error("privilege value {0:#04x} is not a known role and cannot be written")]
    InvalidPrivilege(u8),

    /// A multi-packet transfer ended with fewer bytes than announced.
    #[error("short read: expected {expected} bytes, got {got}")]
    ShortRead {
        /// Bytes announced by the device.
        expected: usize,
        /// Bytes received.
        got: usize,
    },

    /// A multi-packet transfer delivered more bytes than announced.
    #[error("overrun: expected {expected} bytes, got at least {got}")]
    Overrun {
        /// Bytes announced by the device.
        expected: usize,
        /// Bytes received so far.
        got: usize,
    },

    /// A data packet arrived out of sequence.
    #[error("data packet out of sequence: expected {expected}, got {got}")]
    SequenceMismatch {
        /// Expected sequence number.
        expected: u16,
        /// Received sequence number.
        got: u16,
    },

    /// A packet with an unexpected command code arrived.
    #[error("unexpected {got:?} while {context}")]
    UnexpectedReply {
        /// Received command code.
        got: Command,
        /// What the decoder was doing.
        context: &'static str,
    },

    /// The requested operation is not supported for this record layout.
    #[error("{0}")]
    Unsupported(&'static str),
}

/// Record families, used in [`ProtoError::UnsupportedRecordFormat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RecordKind {
    /// User table.
    User,
    /// Attendance log.
    Attendance,
    /// Fingerprint template table.
    Template,
    /// Real-time event payload.
    Event,
}

impl std::fmt::Display for RecordKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::User => "user record",
            Self::Attendance => "attendance record",
            Self::Template => "template record",
            Self::Event => "event payload",
        })
    }
}

/// Shorthand result type.
pub type Result<T, E = ProtoError> = std::result::Result<T, E>;

pub(crate) fn need(what: &'static str, buf: &[u8], needed: usize) -> Result<()> {
    if buf.len() < needed {
        return Err(ProtoError::Truncated {
            what,
            needed,
            available: buf.len(),
        });
    }
    Ok(())
}
