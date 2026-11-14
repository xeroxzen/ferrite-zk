//! Record layouts: users, attendance, templates, real-time events, device
//! counters and options.
//!
//! Decoders that read tables of many records isolate failures per record:
//! a bad record lands in [`Batch::rejected`] with its raw bytes, offset and
//! reason, and decoding continues. Nothing is dropped silently.

use bytes::Bytes;

use crate::error::ProtoError;

pub mod attendance;
pub mod event;
pub mod finger;
pub mod ids;
pub mod options;
pub mod sizes;
pub mod user;

/// Records decoded from a table, plus the ones that could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch<T> {
    /// Successfully decoded records, in table order.
    pub records: Vec<T>,
    /// Records that could not be decoded.
    pub rejected: Vec<RejectedRecord>,
}

impl<T> Default for Batch<T> {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            rejected: Vec::new(),
        }
    }
}

impl<T> Batch<T> {
    /// Whether every record decoded.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.rejected.is_empty()
    }

    pub(crate) fn reject(&mut self, offset: usize, raw: &[u8], reason: ProtoError) {
        tracing::warn!(offset, len = raw.len(), %reason, "rejected record");
        self.rejected.push(RejectedRecord {
            offset,
            raw: Bytes::copy_from_slice(raw),
            reason,
        });
    }
}

/// A record that could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedRecord {
    /// Byte offset of the record within the table's record area.
    pub offset: usize,
    /// The record's raw bytes.
    pub raw: Bytes,
    /// Why it was rejected.
    pub reason: ProtoError,
}

/// Split a buffered-read result into its leading `u32` length and the
/// record area it describes.
///
/// Returns the record area and any bytes after it. If the data is shorter
/// than announced, the record area is what is available; callers detect
/// the shortfall when a record comes up short.
pub(crate) fn split_table(data: &[u8]) -> Result<(usize, &[u8], &[u8]), ProtoError> {
    crate::error::need("table length prefix", data, 4)?;
    let announced = usize::try_from(u32_at(data, 0)).unwrap_or(usize::MAX);
    let body = &data[4..];
    let end = announced.min(body.len());
    Ok((announced, &body[..end], &body[end..]))
}

pub(crate) fn check_len(rec: &[u8], expected: usize) -> Result<(), ProtoError> {
    if rec.len() == expected {
        Ok(())
    } else {
        Err(ProtoError::InvalidField {
            field: "record",
            reason: format!("{} bytes, layout needs {expected}", rec.len()),
        })
    }
}

pub(crate) fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

pub(crate) fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Decode fixed-size records from `area`, rejecting a trailing partial
/// record and reporting bytes past the announced length.
pub(crate) fn decode_fixed<T>(
    announced: usize,
    area: &[u8],
    trailing: &[u8],
    record_len: usize,
    mut decode: impl FnMut(&[u8]) -> Result<T, ProtoError>,
) -> Batch<T> {
    assert!(record_len > 0, "record length must be positive");
    let mut batch = Batch::default();
    let mut chunks = area.chunks_exact(record_len);
    for (i, rec) in (&mut chunks).enumerate() {
        match decode(rec) {
            Ok(r) => batch.records.push(r),
            Err(e) => batch.reject(i * record_len, rec, e),
        }
    }
    let rest = chunks.remainder();
    if !rest.is_empty() {
        let reason = ProtoError::Truncated {
            what: "record",
            needed: record_len,
            available: rest.len(),
        };
        batch.reject(area.len() - rest.len(), rest, reason);
    }
    if area.len() < announced {
        let reason = ProtoError::ShortRead {
            expected: announced,
            got: area.len(),
        };
        batch.reject(area.len(), &[], reason);
    }
    if !trailing.is_empty() {
        let reason = ProtoError::Overrun {
            expected: area.len(),
            got: area.len() + trailing.len(),
        };
        batch.reject(area.len(), trailing, reason);
    }
    batch
}
