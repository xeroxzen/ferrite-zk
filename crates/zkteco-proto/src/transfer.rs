//! Multi-packet data transfers.
//!
//! Reading a large table works like this: the host stages the table with
//! [`Command::PrepareBuffer`] and reads it in chunks with
//! [`Command::ReadBuffer`]. The device answers each chunk request either
//! with the data inline in a [`Command::Data`] reply, or with
//! [`Command::PrepareData`] announcing a size, then a stream of `Data`
//! packets, then [`Command::AckOk`]. [`ChunkReceiver`] consumes those
//! replies and checks the result.
//!
//! Uploads go the other way: [`Command::PrepareData`] with the total size,
//! then `Data` packets of at most [`UPLOAD_PACKET_LEN`] bytes.

use bytes::{Bytes, BytesMut};

use crate::command::Command;
use crate::error::{need, ProtoError, Result};
use crate::packet::Packet;
use crate::records::u32_at;

/// Transport, where it changes protocol behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transport {
    /// TCP with framing.
    Tcp,
    /// Raw UDP datagrams.
    Udp,
}

impl Transport {
    /// Largest chunk to request with [`Command::ReadBuffer`].
    #[must_use]
    pub const fn max_read_chunk(self) -> u32 {
        match self {
            Self::Tcp => 0xFFC0,
            Self::Udp => 16 * 1024,
        }
    }
}

/// Largest `Data` payload the host sends during an upload.
pub const UPLOAD_PACKET_LEN: usize = 1024;

/// The `(start, size)` chunks covering `total` bytes, each at most
/// [`Transport::max_read_chunk`].
pub fn chunk_plan(total: u32, transport: Transport) -> impl Iterator<Item = (u32, u32)> {
    let max = transport.max_read_chunk();
    (0..total.div_ceil(max)).map(move |i| {
        let start = i * max;
        (start, max.min(total - start))
    })
}

/// Split an upload into `Data` payloads.
pub fn upload_packets(buffer: &[u8]) -> std::slice::Chunks<'_, u8> {
    buffer.chunks(UPLOAD_PACKET_LEN)
}

/// The payload of a device-sent [`Command::PrepareData`]: total size, and on
/// TCP the size of each following `Data` packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrepareData {
    /// Total bytes that will follow.
    pub size: u32,
    /// Payload size of each `Data` packet, when present. Not in the manual.
    #[doc = "Unverified against hardware."]
    pub packet_size: Option<u32>,
}

impl PrepareData {
    /// Decode the payload.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Truncated`] if shorter than 4 bytes.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        need("prepare data", payload, 4)?;
        Ok(Self {
            size: u32_at(payload, 0),
            packet_size: (payload.len() >= 8).then(|| u32_at(payload, 4)),
        })
    }

    /// Encode the payload.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.size.to_le_bytes().to_vec();
        if let Some(p) = self.packet_size {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out
    }
}

/// Result of feeding a packet to a [`ChunkReceiver`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// More packets are needed.
    NeedMore,
    /// The chunk is complete.
    Complete(Bytes),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    AwaitingReply,
    Streaming { size: usize, next_seq: u16 },
    Done,
}

/// Collects the reply to one chunk request.
///
/// Feed it every packet that arrives after the request, in order, until it
/// returns [`Progress::Complete`] or an error.
#[derive(Debug)]
pub struct ChunkReceiver {
    requested: Option<usize>,
    check_sequence: bool,
    state: State,
    data: BytesMut,
}

impl ChunkReceiver {
    /// Create a receiver.
    ///
    /// `requested` is the size asked for, if known; the device's announced
    /// size must then match it. On TCP the `Data` packets' sequence numbers
    /// (the `session_id` field) are checked; UDP has none.
    #[must_use]
    pub fn new(requested: Option<usize>, transport: Transport) -> Self {
        Self {
            requested,
            check_sequence: transport == Transport::Tcp,
            state: State::AwaitingReply,
            data: BytesMut::new(),
        }
    }

    /// The size announced by `PrepareData`, once a stream has started and
    /// until it completes.
    #[must_use]
    pub fn streaming_size(&self) -> Option<usize> {
        match self.state {
            State::Streaming { size, .. } => Some(size),
            _ => None,
        }
    }

    /// Bytes collected so far.
    #[must_use]
    pub fn received(&self) -> usize {
        self.data.len()
    }

    /// Feed the next packet.
    ///
    /// # Errors
    ///
    /// [`ProtoError::UnexpectedReply`] for a packet that does not belong in
    /// the transfer (including `AckError`), [`ProtoError::ShortRead`] if the
    /// stream ends early, [`ProtoError::Overrun`] if it delivers too much,
    /// and [`ProtoError::SequenceMismatch`] for a missing or reordered TCP
    /// packet. After an error the receiver must not be reused.
    pub fn on_packet(&mut self, packet: &Packet) -> Result<Progress> {
        let cmd = packet.header.command;
        match (self.state, cmd) {
            (State::AwaitingReply, Command::Data) => {
                self.check_size(packet.payload.len())?;
                self.state = State::Done;
                Ok(Progress::Complete(packet.payload.clone()))
            }
            (State::AwaitingReply, Command::PrepareData) => {
                let announced = PrepareData::decode(&packet.payload)?;
                let size = usize::try_from(announced.size).unwrap_or(usize::MAX);
                self.check_size(size)?;
                self.state = State::Streaming { size, next_seq: 0 };
                Ok(Progress::NeedMore)
            }
            (State::Streaming { size, next_seq }, Command::Data) => {
                let seq = packet.header.session_id;
                if self.check_sequence && seq != next_seq {
                    return Err(ProtoError::SequenceMismatch {
                        expected: next_seq,
                        got: seq,
                    });
                }
                self.data.extend_from_slice(&packet.payload);
                if self.data.len() > size {
                    return Err(ProtoError::Overrun {
                        expected: size,
                        got: self.data.len(),
                    });
                }
                self.state = State::Streaming {
                    size,
                    next_seq: next_seq.wrapping_add(1),
                };
                Ok(Progress::NeedMore)
            }
            (State::Streaming { size, .. }, Command::AckOk) => {
                if self.data.len() != size {
                    return Err(ProtoError::ShortRead {
                        expected: size,
                        got: self.data.len(),
                    });
                }
                self.state = State::Done;
                Ok(Progress::Complete(std::mem::take(&mut self.data).freeze()))
            }
            (State::AwaitingReply, _) => Err(ProtoError::UnexpectedReply {
                got: cmd,
                context: "awaiting a chunk reply",
            }),
            (State::Streaming { .. }, _) => Err(ProtoError::UnexpectedReply {
                got: cmd,
                context: "receiving a data stream",
            }),
            (State::Done, _) => Err(ProtoError::UnexpectedReply {
                got: cmd,
                context: "after the chunk completed",
            }),
        }
    }

    fn check_size(&self, size: usize) -> Result<()> {
        match self.requested {
            Some(r) if size < r => Err(ProtoError::ShortRead {
                expected: r,
                got: size,
            }),
            Some(r) if size > r => Err(ProtoError::Overrun {
                expected: r,
                got: size,
            }),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{decode, encode, ChecksumPolicy};

    fn pkt(cmd: Command, session: u16, payload: &[u8]) -> Packet {
        decode(encode(cmd, session, 0, payload).freeze(), ChecksumPolicy::Reject).unwrap()
    }

    #[test]
    fn plan_covers_total_exactly() {
        let plan: Vec<_> = chunk_plan(70_000, Transport::Tcp).collect();
        assert_eq!(plan, [(0, 65_472), (65_472, 4_528)]);
        let plan: Vec<_> = chunk_plan(32_768, Transport::Udp).collect();
        assert_eq!(plan, [(0, 16_384), (16_384, 16_384)]);
        assert_eq!(chunk_plan(0, Transport::Udp).count(), 0);
        let plan: Vec<_> = chunk_plan(u32::MAX, Transport::Udp).collect();
        assert_eq!(
            plan.iter().map(|c| u64::from(c.1)).sum::<u64>(),
            u64::from(u32::MAX)
        );
    }

    #[test]
    fn inline_reply() {
        let mut r = ChunkReceiver::new(Some(3), Transport::Tcp);
        assert_eq!(
            r.on_packet(&pkt(Command::Data, 0, b"abc")).unwrap(),
            Progress::Complete(Bytes::from_static(b"abc"))
        );
        let mut r = ChunkReceiver::new(Some(4), Transport::Udp);
        assert!(matches!(
            r.on_packet(&pkt(Command::Data, 0, b"abc")),
            Err(ProtoError::ShortRead { expected: 4, got: 3 })
        ));
    }

    #[test]
    fn streamed_reply_tcp() {
        let mut r = ChunkReceiver::new(Some(5), Transport::Tcp);
        assert_eq!(r.streaming_size(), None);
        let prep = PrepareData {
            size: 5,
            packet_size: Some(3),
        }
        .encode();
        assert_eq!(
            r.on_packet(&pkt(Command::PrepareData, 0, &prep)).unwrap(),
            Progress::NeedMore
        );
        assert_eq!(r.streaming_size(), Some(5));
        assert_eq!(
            r.on_packet(&pkt(Command::Data, 0, b"abc")).unwrap(),
            Progress::NeedMore
        );
        assert_eq!(
            r.on_packet(&pkt(Command::Data, 1, b"de")).unwrap(),
            Progress::NeedMore
        );
        assert_eq!(
            r.on_packet(&pkt(Command::AckOk, 0, b"")).unwrap(),
            Progress::Complete(Bytes::from_static(b"abcde"))
        );
        assert_eq!(r.streaming_size(), None);
    }

    #[test]
    fn stream_errors() {
        let prep = PrepareData {
            size: 5,
            packet_size: None,
        }
        .encode();

        let mut r = ChunkReceiver::new(None, Transport::Udp);
        r.on_packet(&pkt(Command::PrepareData, 0, &prep)).unwrap();
        r.on_packet(&pkt(Command::Data, 0, b"abc")).unwrap();
        assert_eq!(
            r.on_packet(&pkt(Command::AckOk, 0, b"")),
            Err(ProtoError::ShortRead { expected: 5, got: 3 })
        );

        let mut r = ChunkReceiver::new(None, Transport::Tcp);
        r.on_packet(&pkt(Command::PrepareData, 0, &prep)).unwrap();
        r.on_packet(&pkt(Command::Data, 0, b"ab")).unwrap();
        assert_eq!(
            r.on_packet(&pkt(Command::Data, 2, b"cde")),
            Err(ProtoError::SequenceMismatch { expected: 1, got: 2 })
        );

        let mut r = ChunkReceiver::new(None, Transport::Udp);
        r.on_packet(&pkt(Command::PrepareData, 0, &prep)).unwrap();
        assert!(matches!(
            r.on_packet(&pkt(Command::Data, 7, b"abcdef")),
            Err(ProtoError::Overrun { .. })
        ));

        let mut r = ChunkReceiver::new(None, Transport::Udp);
        assert!(matches!(
            r.on_packet(&pkt(Command::AckError, 0, b"")),
            Err(ProtoError::UnexpectedReply {
                got: Command::AckError,
                ..
            })
        ));

        let mut r = ChunkReceiver::new(Some(10), Transport::Udp);
        assert!(matches!(
            r.on_packet(&pkt(Command::PrepareData, 0, &prep)),
            Err(ProtoError::ShortRead { expected: 10, got: 5 })
        ));
    }
}
