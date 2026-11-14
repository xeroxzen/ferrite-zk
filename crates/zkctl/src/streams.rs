//! What the header fields of streamed UDP `Data` packets hold, from a
//! [`StreamSample`]. Over TCP the session ID carries a sequence number;
//! whether anything similar exists over UDP is unknown (DECISIONS.md D25),
//! and this answers it on real hardware.

use serde::Serialize;
use zkteco_client::{SampledPacket, StreamSample};
use zkteco_proto::Command;

/// Header fields of the `Data` packets in one sampled stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StreamHeaders {
    /// Bytes requested.
    pub requested: u32,
    /// Whether the device announced a stream with `PrepareData`.
    pub prepare_data: bool,
    /// `Data` packets received.
    pub data_packets: usize,
    /// Whether the reply ended within the timeout.
    pub complete: bool,
    /// What the session ID field held, packet by packet.
    pub session_ids: String,
    /// What the reply ID field held, compared with the request's.
    pub reply_ids: String,
    /// Payload sizes of `Data` packets other than the last: the device's
    /// UDP packet size.
    pub packet_sizes: Vec<usize>,
}

/// Describe `sample`.
#[must_use]
pub fn analyse(sample: &StreamSample) -> StreamHeaders {
    let data: Vec<&SampledPacket> = sample
        .packets
        .iter()
        .filter(|p| p.command == Command::Data)
        .collect();
    let mut packet_sizes: Vec<usize> = data.iter().rev().skip(1).map(|p| p.len).collect();
    packet_sizes.sort_unstable();
    packet_sizes.dedup();
    StreamHeaders {
        requested: sample.requested,
        prepare_data: sample.packets.iter().any(|p| p.command == Command::PrepareData),
        data_packets: data.len(),
        complete: sample.complete,
        session_ids: classify_sessions(&data, sample.session_id),
        reply_ids: classify_replies(&data, sample.request_reply_id),
        packet_sizes,
    }
}

fn classify_sessions(data: &[&SampledPacket], session: u16) -> String {
    if data.len() < 2 {
        return "fewer than two data packets; nothing to compare".into();
    }
    if data.iter().zip(0u16..).all(|(p, i)| p.session_id == i) {
        return "sequence from 0 (as over TCP)".into();
    }
    if data.iter().all(|p| p.session_id == session) {
        return "always the session ID (no sequence)".into();
    }
    let first: Vec<u16> = data.iter().take(8).map(|p| p.session_id).collect();
    format!("other; first values {first:?}")
}

fn classify_replies(data: &[&SampledPacket], request: u16) -> String {
    if data.is_empty() {
        return "no data packets".into();
    }
    let echoed = data.iter().filter(|p| p.reply_id == request).count();
    if echoed == data.len() {
        return "echo the request's reply ID".into();
    }
    format!("{} of {} echo the request's reply ID", echoed, data.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(packets: &[(Command, u16, u16, usize)]) -> StreamSample {
        StreamSample {
            session_id: 9,
            request_reply_id: 4,
            requested: 2048,
            packets: packets
                .iter()
                .map(|&(command, session_id, reply_id, len)| SampledPacket {
                    command,
                    session_id,
                    reply_id,
                    len,
                })
                .collect(),
            complete: true,
        }
    }

    #[test]
    fn sequence_numbers_are_recognised() {
        let h = analyse(&sample(&[
            (Command::PrepareData, 9, 4, 8),
            (Command::Data, 0, 4, 1016),
            (Command::Data, 1, 4, 1016),
            (Command::Data, 2, 4, 16),
            (Command::AckOk, 9, 4, 0),
        ]));
        assert!(h.prepare_data);
        assert_eq!(h.data_packets, 3);
        assert!(h.session_ids.starts_with("sequence"), "{}", h.session_ids);
        assert!(h.reply_ids.starts_with("echo"), "{}", h.reply_ids);
        assert_eq!(h.packet_sizes, [1016]);
    }

    #[test]
    fn constant_session_and_stale_reply_ids() {
        let h = analyse(&sample(&[
            (Command::PrepareData, 9, 4, 8),
            (Command::Data, 9, 4, 1008),
            (Command::Data, 9, 3, 1008),
            (Command::AckOk, 9, 4, 0),
        ]));
        assert!(
            h.session_ids.starts_with("always the session"),
            "{}",
            h.session_ids
        );
        assert_eq!(h.reply_ids, "1 of 2 echo the request's reply ID");
    }
}
