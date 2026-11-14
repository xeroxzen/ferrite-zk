//! The byte transport under a [`Client`](crate::Client).

use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use zkteco_proto::framing::{encode_tcp, TcpFrameDecoder};
use zkteco_proto::packet::HexDump;
use zkteco_proto::Command;

use crate::error::{Error, Result};

/// Read buffer size. Large enough for a full 64 KiB chunk in a few reads.
const READ_BUF: usize = 16 * 1024;

/// A framed TCP connection.
#[derive(Debug)]
pub(crate) struct TcpLink {
    stream: TcpStream,
    frames: TcpFrameDecoder,
    buf: Box<[u8]>,
}

impl TcpLink {
    pub(crate) async fn connect(host: &str, port: u16, timeout: Duration) -> Result<Self> {
        let stream = tokio::time::timeout(timeout, TcpStream::connect((host, port)))
            .await
            .map_err(|_| Error::Timeout {
                command: Command::Connect,
                after: timeout,
            })??;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            frames: TcpFrameDecoder::default(),
            buf: vec![0; READ_BUF].into_boxed_slice(),
        })
    }

    /// Send one packet (header and payload), adding the TCP frame prefix.
    pub(crate) async fn send(&mut self, packet: &[u8]) -> Result<()> {
        let frame = encode_tcp(packet);
        tracing::trace!(bytes = %HexDump(&frame), "tcp send");
        self.stream.write_all(&frame).await?;
        Ok(())
    }

    /// Receive the next packet (header and payload, prefix removed).
    ///
    /// Cancel-safe: bytes already read stay buffered in the frame decoder.
    /// A framing error leaves the stream unsynchronised; the caller must
    /// close the link.
    pub(crate) async fn recv(&mut self) -> Result<Bytes> {
        // Each pass either returns or buffers at least one byte; the
        // caller's timeout bounds the wait, and the decoder bounds the frame
        // size.
        loop {
            if let Some(packet) = self.frames.next_packet()? {
                return Ok(packet);
            }
            let n = self.stream.read(&mut self.buf).await?;
            if n == 0 {
                return Err(Error::ConnectionClosed);
            }
            tracing::trace!(bytes = %HexDump(&self.buf[..n]), "tcp recv");
            self.frames.extend(&self.buf[..n]);
        }
    }

    /// Close the write half; errors are irrelevant because the link is
    /// being dropped.
    pub(crate) async fn shutdown(mut self) {
        if let Err(e) = self.stream.shutdown().await {
            tracing::debug!(%e, "shutdown after close");
        }
    }
}
