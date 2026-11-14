//! TCP framing: an 8-byte prefix (`50 50 82 7d` + u32 length) before each
//! packet, and a streaming decoder that copes with frames split across or
//! coalesced within reads.

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::error::{ProtoError, Result};
use crate::packet::HEADER_LEN;

/// The four magic bytes that start every TCP frame.
pub const TCP_MAGIC: [u8; 4] = [0x50, 0x50, 0x82, 0x7d];

/// Length of the TCP frame prefix.
pub const TCP_PREFIX_LEN: usize = 8;

/// Default upper bound on an announced frame length.
///
/// The largest frames in normal operation carry one buffered-read chunk
/// (65 472 bytes) plus a header. The bound guards against a corrupt length
/// field making the decoder buffer without limit.
pub const DEFAULT_MAX_FRAME_LEN: usize = 1 << 20;

/// Wrap a packet in a TCP frame.
///
/// # Panics
///
/// If the packet is 4 GiB or larger, which no protocol message approaches.
#[must_use]
pub fn encode_tcp(packet: &[u8]) -> BytesMut {
    let len = u32::try_from(packet.len()).expect("packet length fits in u32");
    let mut buf = BytesMut::with_capacity(TCP_PREFIX_LEN + packet.len());
    buf.put_slice(&TCP_MAGIC);
    buf.put_u32_le(len);
    buf.put_slice(packet);
    buf
}

/// Incremental TCP frame decoder.
///
/// Feed it bytes as they arrive with [`TcpFrameDecoder::extend`], then call
/// [`TcpFrameDecoder::next_packet`] until it returns `Ok(None)`. Each
/// returned value is one complete packet with the frame prefix removed.
#[derive(Debug)]
pub struct TcpFrameDecoder {
    buf: BytesMut,
    max_frame_len: usize,
}

impl Default for TcpFrameDecoder {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_FRAME_LEN)
    }
}

impl TcpFrameDecoder {
    /// Create a decoder that rejects frames announcing more than
    /// `max_frame_len` packet bytes.
    ///
    /// # Panics
    ///
    /// If `max_frame_len` is smaller than a packet header.
    #[must_use]
    pub fn new(max_frame_len: usize) -> Self {
        assert!(max_frame_len >= HEADER_LEN, "max_frame_len below header size");
        Self {
            buf: BytesMut::new(),
            max_frame_len,
        }
    }

    /// Append received bytes.
    pub fn extend(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Number of buffered bytes not yet returned as packets.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Discard everything buffered, for example after a decoding error.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Take the next complete packet, if one is buffered.
    ///
    /// # Errors
    ///
    /// [`ProtoError::BadTcpMagic`] or [`ProtoError::BadFrameLength`] if the
    /// stream is out of sync. The decoder is then in an unknown state; the
    /// caller should [`clear`](Self::clear) it and usually drop the connection.
    pub fn next_packet(&mut self) -> Result<Option<Bytes>> {
        if self.buf.len() < TCP_PREFIX_LEN {
            return Ok(None);
        }
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&self.buf[..4]);
        if magic != TCP_MAGIC {
            return Err(ProtoError::BadTcpMagic { found: magic });
        }
        let len_bytes = [self.buf[4], self.buf[5], self.buf[6], self.buf[7]];
        let len = usize::try_from(u32::from_le_bytes(len_bytes)).unwrap_or(usize::MAX);
        if !(HEADER_LEN..=self.max_frame_len).contains(&len) {
            return Err(ProtoError::BadFrameLength {
                len,
                min: HEADER_LEN,
                max: self.max_frame_len,
            });
        }
        if self.buf.len() < TCP_PREFIX_LEN + len {
            return Ok(None);
        }
        self.buf.advance(TCP_PREFIX_LEN);
        Ok(Some(self.buf.split_to(len).freeze()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use crate::packet::encode;

    #[test]
    fn prefix_layout() {
        let f = encode_tcp(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&f[..8], &[0x50, 0x50, 0x82, 0x7d, 8, 0, 0, 0]);
    }

    #[test]
    fn split_and_coalesced_frames() {
        let a = encode_tcp(&encode(Command::AckOk, 1, 1, b"first"));
        let b = encode_tcp(&encode(Command::RegEvent, 1, 0, &[9; 12]));
        let mut stream = a.to_vec();
        stream.extend_from_slice(&b);

        let mut dec = TcpFrameDecoder::default();
        dec.extend(&stream[..5]);
        assert_eq!(dec.next_packet().unwrap(), None);
        dec.extend(&stream[5..a.len() + 3]);
        assert_eq!(dec.next_packet().unwrap().unwrap().len(), a.len() - 8);
        assert_eq!(dec.next_packet().unwrap(), None);
        dec.extend(&stream[a.len() + 3..]);
        assert_eq!(dec.next_packet().unwrap().unwrap().len(), b.len() - 8);
        assert_eq!(dec.next_packet().unwrap(), None);
        assert_eq!(dec.buffered(), 0);
    }

    #[test]
    fn rejects_bad_magic_and_lengths() {
        let mut dec = TcpFrameDecoder::default();
        dec.extend(&[0, 0, 0, 0, 8, 0, 0, 0]);
        assert!(matches!(dec.next_packet(), Err(ProtoError::BadTcpMagic { .. })));

        let mut dec = TcpFrameDecoder::new(64);
        dec.extend(&[0x50, 0x50, 0x82, 0x7d, 65, 0, 0, 0]);
        assert!(matches!(
            dec.next_packet(),
            Err(ProtoError::BadFrameLength { len: 65, .. })
        ));

        let mut dec = TcpFrameDecoder::default();
        dec.extend(&[0x50, 0x50, 0x82, 0x7d, 7, 0, 0, 0]);
        assert!(matches!(
            dec.next_packet(),
            Err(ProtoError::BadFrameLength { len: 7, .. })
        ));
    }
}
