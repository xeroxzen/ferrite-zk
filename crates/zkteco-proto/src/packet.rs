//! The 8-byte packet header, the checksum, and packet encoding and decoding.

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::command::{Command, EventFlags};
use crate::error::{need, ProtoError, Result};

/// Length of the packet header.
pub const HEADER_LEN: usize = 8;

/// Byte offset of the checksum field within the header.
const CHECKSUM_OFFSET: usize = 2;

/// Highest `reply_id` value used on the wire. The counter wraps from 65534 to 0.
pub const MAX_REPLY_ID: u16 = 65534;

/// What to do when an incoming packet's checksum is wrong.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChecksumPolicy {
    /// Log a warning through `tracing` and accept the packet.
    #[default]
    Warn,
    /// Fail with [`ProtoError::ChecksumMismatch`].
    Reject,
    /// Do not check.
    Ignore,
}

/// The four header fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Command or reply code.
    pub command: Command,
    /// Checksum as carried on the wire.
    pub checksum: u16,
    /// Session identifier. Some packets reuse this field; see
    /// [`Header::event_flags`] and [`Header::data_sequence`].
    pub session_id: u16,
    /// Request counter, echoed by the device in its reply.
    pub reply_id: u16,
}

impl Header {
    /// For an event packet ([`Command::RegEvent`] sent by the device), the
    /// event flag that the device puts in the `session_id` field.
    #[must_use]
    pub fn event_flags(&self) -> Option<EventFlags> {
        (self.command == Command::RegEvent).then(|| EventFlags::from_bits_retain(u32::from(self.session_id)))
    }

    /// For a [`Command::Data`] packet that is part of a multi-packet TCP
    /// transfer, the 0-based sequence number the device puts in the
    /// `session_id` field.
    ///
    /// The field is present on every `Data` packet; whether it holds a
    /// sequence number depends on context (the device sends it during
    /// TCP chunk streams). Not in the manual.
    #[doc = "Unverified against hardware."]
    #[must_use]
    pub fn data_sequence(&self) -> Option<u16> {
        (self.command == Command::Data).then_some(self.session_id)
    }
}

/// A decoded packet: header plus payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Header fields.
    pub header: Header,
    /// Bytes after the header.
    pub payload: Bytes,
}

/// The 16-bit ones'-complement checksum used by the protocol.
///
/// Sums the input as little-endian 16-bit words (an odd trailing byte
/// counts as the low byte of a final word), folds carries back in, and
/// returns the complement. When computing a packet checksum, the checksum
/// field itself must be zero in the input.
#[must_use]
pub fn checksum(bytes: &[u8]) -> u16 {
    !fold_sum(0, bytes)
}

/// Ones'-complement sum of `bytes` added to `sum`. `bytes` must start at an
/// even offset of the checksummed data.
fn fold_sum(sum: u16, bytes: &[u8]) -> u16 {
    let mut acc = u32::from(sum);
    let (words, remainder) = bytes.as_chunks::<2>();
    for w in words {
        acc += u32::from(u16::from_le_bytes(*w));
        acc = (acc & 0xFFFF) + (acc >> 16);
    }
    if let [last] = remainder {
        acc += u32::from(*last);
        acc = (acc & 0xFFFF) + (acc >> 16);
    }
    debug_assert!(acc <= 0xFFFF);
    u16::try_from(acc).unwrap_or(u16::MAX)
}

/// Checksum of a full packet, treating its checksum field as zero.
fn packet_checksum(packet: &[u8]) -> u16 {
    debug_assert!(packet.len() >= HEADER_LEN);
    let head = fold_sum(0, &packet[..CHECKSUM_OFFSET]);
    !fold_sum(head, &packet[CHECKSUM_OFFSET + 2..])
}

/// Encode a packet with its checksum filled in.
#[must_use]
pub fn encode(command: Command, session_id: u16, reply_id: u16, payload: &[u8]) -> BytesMut {
    let mut buf = BytesMut::with_capacity(HEADER_LEN + payload.len());
    buf.put_u16_le(command.to_u16());
    buf.put_u16_le(0);
    buf.put_u16_le(session_id);
    buf.put_u16_le(reply_id);
    buf.put_slice(payload);
    let sum = packet_checksum(&buf);
    buf[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 2].copy_from_slice(&sum.to_le_bytes());
    tracing::trace!(?command, session_id, reply_id, bytes = %HexDump(&buf), "encoded packet");
    buf
}

/// Decode a packet, checking its checksum according to `policy`.
///
/// # Errors
///
/// [`ProtoError::Truncated`] if shorter than a header, or
/// [`ProtoError::ChecksumMismatch`] under [`ChecksumPolicy::Reject`].
pub fn decode(mut packet: Bytes, policy: ChecksumPolicy) -> Result<Packet> {
    need("packet header", &packet, HEADER_LEN)?;
    let word = |i: usize| u16::from_le_bytes([packet[i], packet[i + 1]]);
    let header = Header {
        command: Command::from_u16(word(0)),
        checksum: word(2),
        session_id: word(4),
        reply_id: word(6),
    };
    tracing::trace!(command = ?header.command, bytes = %HexDump(&packet), "decoding packet");
    if policy != ChecksumPolicy::Ignore {
        let expected = packet_checksum(&packet);
        if expected != header.checksum {
            let err = ProtoError::ChecksumMismatch {
                expected,
                actual: header.checksum,
            };
            if policy == ChecksumPolicy::Reject {
                return Err(err);
            }
            tracing::warn!(command = ?header.command, %err, "accepting packet with bad checksum");
        }
    }
    packet.advance(HEADER_LEN);
    Ok(Packet {
        header,
        payload: packet,
    })
}

/// Advance a `reply_id` counter: 0, 1, ..., 65534, 0, ...
#[must_use]
pub const fn next_reply_id(current: u16) -> u16 {
    if current >= MAX_REPLY_ID {
        0
    } else {
        current + 1
    }
}

/// Formats bytes as lowercase hex for trace logs.
pub struct HexDump<'a>(pub &'a [u8]);

impl std::fmt::Display for HexDump<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(bytes: &[u8]) -> u16 {
        let mut sum: u64 = 0;
        for (i, b) in bytes.iter().enumerate() {
            sum += u64::from(*b) << (8 * (i % 2));
        }
        while sum > 0xFFFF {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        !u16::try_from(sum).unwrap()
    }

    #[test]
    fn checksum_matches_naive_definition() {
        let samples: [&[u8]; 6] = [
            &[],
            &[0x01],
            &[0xFF, 0xFF],
            &[0xFF, 0xFF, 0x01, 0x00],
            &[0xE8, 0x03, 0, 0, 0, 0, 0, 0],
            &[0x12, 0x34, 0x56, 0x78, 0x9A],
        ];
        for s in samples {
            assert_eq!(checksum(s), naive(s), "{s:02x?}");
        }
    }

    #[test]
    fn packet_checksum_equals_checksum_with_zeroed_field() {
        let pkt = encode(Command::Connect, 0, 0, &[]);
        let mut zeroed = pkt.to_vec();
        zeroed[2] = 0;
        zeroed[3] = 0;
        assert_eq!(u16::from_le_bytes([pkt[2], pkt[3]]), checksum(&zeroed));
    }

    #[test]
    fn worked_checksum_example() {
        // PROTOCOL.md 3.1: sum = 0x00c9 + 0x2a51 + 0x0001 = 0x2b1b, checksum = !0x2b1b.
        assert_eq!(
            encode(Command::GetTime, 0x2a51, 1, &[]).as_ref(),
            &[0xc9, 0x00, 0xe4, 0xd4, 0x51, 0x2a, 0x01, 0x00]
        );
    }

    #[test]
    fn decode_round_trip_and_policies() {
        let pkt = encode(Command::AckOk, 0x1234, 7, b"abc").freeze();
        let p = decode(pkt.clone(), ChecksumPolicy::Reject).unwrap();
        assert_eq!(p.header.command, Command::AckOk);
        assert_eq!(p.header.session_id, 0x1234);
        assert_eq!(p.header.reply_id, 7);
        assert_eq!(&p.payload[..], b"abc");

        let mut bad = pkt.to_vec();
        bad[2] ^= 1;
        let bad = Bytes::from(bad);
        assert!(matches!(
            decode(bad.clone(), ChecksumPolicy::Reject),
            Err(ProtoError::ChecksumMismatch { .. })
        ));
        assert!(decode(bad.clone(), ChecksumPolicy::Warn).is_ok());
        assert!(decode(bad, ChecksumPolicy::Ignore).is_ok());
    }

    #[test]
    fn decode_rejects_short_input() {
        assert!(matches!(
            decode(Bytes::from_static(&[1, 2, 3]), ChecksumPolicy::Ignore),
            Err(ProtoError::Truncated { .. })
        ));
    }

    #[test]
    fn reply_counter_wraps_before_65535() {
        assert_eq!(next_reply_id(0), 1);
        assert_eq!(next_reply_id(65533), 65534);
        assert_eq!(next_reply_id(65534), 0);
        assert_eq!(next_reply_id(65535), 0);
    }

    #[test]
    fn reused_fields() {
        let ev = Header {
            command: Command::RegEvent,
            checksum: 0,
            session_id: 1,
            reply_id: 0,
        };
        assert_eq!(ev.event_flags(), Some(EventFlags::ATTLOG));
        assert_eq!(ev.data_sequence(), None);
        let data = Header {
            command: Command::Data,
            session_id: 3,
            ..ev
        };
        assert_eq!(data.data_sequence(), Some(3));
        assert_eq!(data.event_flags(), None);
    }
}
