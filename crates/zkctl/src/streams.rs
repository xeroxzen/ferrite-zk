//! What the header fields of streamed `Data` packets hold, measured from a
//! recording. Over TCP the session ID carries a sequence number; whether
//! anything similar exists over UDP is unknown (DECISIONS.md D25), and this
//! answers it on real hardware.

use serde::Serialize;
use zkteco_client::{Direction, Recording};
use zkteco_proto::packet::{self, Packet};
use zkteco_proto::{ChecksumPolicy, Command};

/// Header fields of the `Data` packets in multi-packet streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StreamHeaders {
    /// Streams (`PrepareData` ... `AckOk`) seen.
    pub streams: usize,
    /// Streams with more than one `Data` packet.
    pub multi_packet_streams: usize,
    /// `Data` packets inside streams.
    pub data_packets: usize,
    /// What the session ID field held, packet by packet within a stream.
    pub session_ids: String,
    /// What the reply ID field held, compared with the request's.
    pub reply_ids: String,
    /// Payload sizes of `Data` packets other than the last of each stream.
    pub packet_sizes: Vec<usize>,
}

struct DataPacket {
    index: u16,
    session_id: u16,
    reply_id: u16,
    request: Option<u16>,
    len: usize,
    last: bool,
}

/// Measure the streams in `rec`.
#[must_use]
pub fn analyse(rec: &Recording) -> StreamHeaders {
    let (packets, streams, session) = collect(rec);
    let multi = packets.iter().filter(|p| p.index == 1).count();
    let mut packet_sizes: Vec<usize> = packets.iter().filter(|p| !p.last).map(|p| p.len).collect();
    packet_sizes.sort_unstable();
    packet_sizes.dedup();
    StreamHeaders {
        streams,
        multi_packet_streams: multi,
        data_packets: packets.len(),
        session_ids: classify_sessions(&packets, session),
        reply_ids: classify_replies(&packets),
        packet_sizes,
    }
}

/// The streamed `Data` packets, the stream count and the session ID.
fn collect(rec: &Recording) -> (Vec<DataPacket>, usize, Option<u16>) {
    let mut out: Vec<DataPacket> = Vec::new();
    let (mut streams, mut session, mut request, mut in_stream) = (0, None, None, None::<u16>);
    for p in rec
        .packets
        .iter()
        .filter_map(|r| Some((r.direction, decode(&r.bytes)?)))
    {
        match p {
            (Direction::HostToDevice, pkt) if pkt.header.command == Command::ReadBuffer => {
                request = Some(pkt.header.reply_id);
            }
            (Direction::HostToDevice, _) => {}
            (Direction::DeviceToHost, pkt) => {
                if pkt.header.command != Command::RegEvent {
                    session.get_or_insert(pkt.header.session_id);
                }
                match (pkt.header.command, in_stream) {
                    (Command::PrepareData, _) => {
                        streams += 1;
                        in_stream = Some(0);
                    }
                    (Command::Data, Some(i)) => {
                        out.push(DataPacket {
                            index: i,
                            session_id: pkt.header.session_id,
                            reply_id: pkt.header.reply_id,
                            request,
                            len: pkt.payload.len(),
                            last: false,
                        });
                        in_stream = Some(i.wrapping_add(1));
                    }
                    (Command::AckOk, Some(_)) => {
                        if let Some(last) = out.last_mut() {
                            last.last = true;
                        }
                        in_stream = None;
                    }
                    _ => {}
                }
            }
        }
    }
    (out, streams, session)
}

fn decode(bytes: &bytes::Bytes) -> Option<Packet> {
    packet::decode(bytes.clone(), ChecksumPolicy::Ignore).ok()
}

fn classify_sessions(packets: &[DataPacket], session: Option<u16>) -> String {
    if packets.is_empty() {
        return "no streamed data packets".into();
    }
    if packets.iter().all(|p| p.session_id == p.index) {
        return "sequence from 0 within each stream (as over TCP)".into();
    }
    if session.is_some_and(|s| packets.iter().all(|p| p.session_id == s)) {
        return "always the session ID (no sequence)".into();
    }
    let first: Vec<u16> = packets.iter().take(8).map(|p| p.session_id).collect();
    format!("other; first values {first:?}")
}

fn classify_replies(packets: &[DataPacket]) -> String {
    if packets.is_empty() {
        return "no streamed data packets".into();
    }
    let echoed = packets.iter().filter(|p| Some(p.reply_id) == p.request).count();
    if echoed == packets.len() {
        return "echo the request's reply ID".into();
    }
    format!("{} of {} echo the request's reply ID", echoed, packets.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zkteco_client::RecordedPacket;

    fn rec(packets: &[(Direction, Command, u16, u16, &[u8])]) -> Recording {
        Recording {
            packets: packets
                .iter()
                .map(|&(direction, c, s, r, p)| RecordedPacket {
                    direction,
                    bytes: packet::encode(c, s, r, p).freeze(),
                })
                .collect(),
            truncated: false,
        }
    }

    use Direction::{DeviceToHost as D, HostToDevice as H};

    #[test]
    fn sequence_numbers_are_recognised() {
        let r = rec(&[
            (D, Command::AckOk, 9, 0, b""),
            (H, Command::ReadBuffer, 9, 4, b""),
            (D, Command::PrepareData, 9, 4, &[4, 0, 0, 0]),
            (D, Command::Data, 0, 4, b"ab"),
            (D, Command::Data, 1, 4, b"cd"),
            (D, Command::AckOk, 9, 4, b""),
        ]);
        let h = analyse(&r);
        assert_eq!((h.streams, h.multi_packet_streams, h.data_packets), (1, 1, 2));
        assert!(h.session_ids.starts_with("sequence"), "{}", h.session_ids);
        assert!(h.reply_ids.starts_with("echo"), "{}", h.reply_ids);
        assert_eq!(h.packet_sizes, [2]);
    }

    #[test]
    fn constant_session_and_stale_reply_ids() {
        let r = rec(&[
            (D, Command::AckOk, 9, 0, b""),
            (H, Command::ReadBuffer, 9, 4, b""),
            (D, Command::PrepareData, 9, 4, &[4, 0, 0, 0]),
            (D, Command::Data, 9, 4, b"ab"),
            (D, Command::Data, 9, 3, b"cd"),
            (D, Command::AckOk, 9, 4, b""),
        ]);
        let h = analyse(&r);
        assert!(
            h.session_ids.starts_with("always the session"),
            "{}",
            h.session_ids
        );
        assert_eq!(h.reply_ids, "1 of 2 echo the request's reply ID");
    }
}
