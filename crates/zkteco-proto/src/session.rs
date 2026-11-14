//! Per-connection header state: the session ID and the `reply_id` counter.

use bytes::BytesMut;

use crate::command::Command;
use crate::packet::{self, MAX_REPLY_ID};

/// Builds outgoing packets for one connection.
///
/// The first request goes out with `reply_id` 0, and each later request
/// increments it, wrapping from 65534 to 0. The device echoes the value in
/// its reply.
#[derive(Debug, Clone)]
pub struct Session {
    session_id: u16,
    last_reply_id: u16,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// A session before `CMD_CONNECT`: session ID 0.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            session_id: 0,
            last_reply_id: MAX_REPLY_ID,
        }
    }

    /// The session ID.
    #[must_use]
    pub const fn session_id(&self) -> u16 {
        self.session_id
    }

    /// Adopt the session ID from the reply to `CMD_CONNECT`.
    pub fn set_session_id(&mut self, id: u16) {
        self.session_id = id;
    }

    /// The `reply_id` of the most recent request.
    #[must_use]
    pub const fn last_reply_id(&self) -> u16 {
        self.last_reply_id
    }

    /// Build the next request. Returns its `reply_id` and encoded bytes.
    pub fn request(&mut self, command: Command, payload: &[u8]) -> (u16, BytesMut) {
        self.last_reply_id = packet::next_reply_id(self.last_reply_id);
        let bytes = packet::encode(command, self.session_id, self.last_reply_id, payload);
        (self.last_reply_id, bytes)
    }

    /// Build the acknowledgement for a device-initiated event packet:
    /// `AckOk` with this session's ID and `reply_id` 0. Does not advance the
    /// counter.
    #[must_use]
    pub fn event_ack(&self) -> BytesMut {
        packet::encode(Command::AckOk, self.session_id, 0, &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_sequence() {
        let mut s = Session::new();
        assert_eq!(s.request(Command::Connect, &[]).0, 0);
        s.set_session_id(0x2a51);
        let (id, bytes) = s.request(Command::GetFreeSizes, &[]);
        assert_eq!(id, 1);
        assert_eq!(&bytes[4..8], &[0x51, 0x2a, 1, 0]);
        let ack = s.event_ack();
        assert_eq!(&ack[6..8], &[0, 0]);
        assert_eq!(s.last_reply_id(), 1);
    }
}
