//! Packet recording, for diagnostics and captures.

use bytes::Bytes;

/// Packets kept at most per recording. Later packets are not recorded and
/// the recording is marked truncated.
const MAX_RECORDED_PACKETS: usize = 1 << 20;

/// Which way a recorded packet went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Sent by this client.
    HostToDevice,
    /// Received from the device.
    DeviceToHost,
}

/// One recorded packet: header and payload as sent or received (no TCP
/// frame prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPacket {
    /// Which way it went.
    pub direction: Direction,
    /// The packet bytes.
    pub bytes: Bytes,
}

/// Packets recorded by [`Client::start_recording`](crate::Client::start_recording).
///
/// A recording holds everything on the wire, including personal data and
/// templates if they were read. Scrub it before sharing (see
/// [`zkteco_proto::scrub`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recording {
    /// The packets, in the order they were sent or received.
    pub packets: Vec<RecordedPacket>,
    /// Whether packets were left out because the recording was full.
    pub truncated: bool,
}

impl Recording {
    pub(crate) fn push(&mut self, direction: Direction, bytes: &[u8]) {
        if self.packets.len() >= MAX_RECORDED_PACKETS {
            self.truncated = true;
            return;
        }
        self.packets.push(RecordedPacket {
            direction,
            bytes: Bytes::copy_from_slice(bytes),
        });
    }
}
