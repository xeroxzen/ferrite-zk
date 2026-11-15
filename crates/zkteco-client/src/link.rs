//! The byte transport under a [`Client`](crate::Client).

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use zkteco_proto::framing::{encode_tcp, TcpFrameDecoder};
use zkteco_proto::packet::HexDump;
use zkteco_proto::{Command, Transport};

use crate::config::ClientConfig;
use crate::error::{Error, Result};

/// TCP read buffer size. Large enough for a full 64 KiB chunk in a few
/// reads.
const READ_BUF: usize = 16 * 1024;

/// UDP receive buffer: the largest possible datagram, so nothing is ever
/// truncated (PROTOCOL.md 2.1).
const DATAGRAM_BUF: usize = 64 * 1024;

/// A connection to the device over either transport. Both carry whole
/// packets (header and payload).
#[derive(Debug)]
pub(crate) enum Link {
    Tcp(TcpLink),
    Udp(UdpLink),
}

impl Link {
    pub(crate) async fn connect(config: &ClientConfig, transport: Transport) -> Result<Self> {
        let (host, port, timeout) = (config.host.as_str(), config.port, config.connect_timeout);
        Ok(match transport {
            Transport::Tcp => {
                let link = TcpLink::connect(host, port, timeout).await?;
                if let Some(idle) = config.tcp_keepalive {
                    link.keepalive(idle)?;
                }
                Self::Tcp(link)
            }
            Transport::Udp => Self::Udp(UdpLink::connect(host, port, timeout).await?),
        })
    }

    pub(crate) async fn send(&mut self, packet: &[u8]) -> Result<()> {
        match self {
            Self::Tcp(l) => l.send(packet).await,
            Self::Udp(l) => l.send(packet).await,
        }
    }

    /// Receive the next packet. Cancel-safe for both transports.
    pub(crate) async fn recv(&mut self) -> Result<Bytes> {
        match self {
            Self::Tcp(l) => l.recv().await,
            Self::Udp(l) => l.recv().await,
        }
    }

    pub(crate) async fn shutdown(self) {
        match self {
            Self::Tcp(l) => l.shutdown().await,
            // Nothing to tell the peer; dropping the socket releases it.
            Self::Udp(_) => {}
        }
    }
}

/// A UDP socket connected to the device: one packet per datagram.
#[derive(Debug)]
pub(crate) struct UdpLink {
    socket: UdpSocket,
    buf: Box<[u8]>,
}

impl UdpLink {
    pub(crate) async fn connect(host: &str, port: u16, timeout: Duration) -> Result<Self> {
        let timed_out = || Error::Timeout {
            command: Command::Connect,
            after: timeout,
        };
        let addr = tokio::time::timeout(timeout, tokio::net::lookup_host((host, port)))
            .await
            .map_err(|_| timed_out())??
            .next()
            .ok_or_else(|| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no address for host",
                ))
            })?;
        let local: SocketAddr = if addr.is_ipv4() {
            ([0, 0, 0, 0], 0).into()
        } else {
            ([0u16; 8], 0).into()
        };
        let socket = UdpSocket::bind(local).await?;
        socket.connect(addr).await?;
        Ok(Self {
            socket,
            buf: vec![0; DATAGRAM_BUF].into_boxed_slice(),
        })
    }

    async fn send(&mut self, packet: &[u8]) -> Result<()> {
        tracing::trace!(bytes = %HexDump(packet), "udp send");
        let sent = self.socket.send(packet).await?;
        if sent != packet.len() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                format!("sent {sent} of {} bytes", packet.len()),
            )));
        }
        Ok(())
    }

    /// Receive one datagram. Cancel-safe: a datagram is either fully
    /// received or left in the socket.
    async fn recv(&mut self) -> Result<Bytes> {
        let n = self.socket.recv(&mut self.buf).await?;
        tracing::trace!(bytes = %HexDump(&self.buf[..n]), "udp recv");
        Ok(Bytes::copy_from_slice(&self.buf[..n]))
    }
}

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

    /// Turn on TCP keepalive with this idle time; probes then follow at
    /// the operating system's interval.
    fn keepalive(&self, idle: Duration) -> Result<()> {
        let params = socket2::TcpKeepalive::new().with_time(idle);
        socket2::SockRef::from(&self.stream).set_tcp_keepalive(&params)?;
        Ok(())
    }

    /// Send one packet (header and payload), adding the TCP frame prefix.
    async fn send(&mut self, packet: &[u8]) -> Result<()> {
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
    async fn recv(&mut self) -> Result<Bytes> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keepalive_is_set_on_the_socket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let link = TcpLink::connect("127.0.0.1", port, Duration::from_secs(2))
            .await
            .unwrap();
        let sock = socket2::SockRef::from(&link.stream);
        assert!(!sock.keepalive().unwrap());
        link.keepalive(Duration::from_secs(30)).unwrap();
        assert!(sock.keepalive().unwrap());
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert_eq!(sock.keepalive_time().unwrap(), Duration::from_secs(30));
    }
}
