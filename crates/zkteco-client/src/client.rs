use std::time::Duration;

use zkteco_proto::commkey;
use zkteco_proto::packet::{self, Packet};
use zkteco_proto::records::user::UserFormat;
use zkteco_proto::session::Session;
use zkteco_proto::{Command, ProtoError, Transport};

use crate::config::ClientConfig;
use crate::error::{Error, Result};
use crate::link::TcpLink;

/// Stray event packets tolerated while waiting for one reply.
const MAX_STRAY_EVENTS: usize = 256;

/// A connection to one device.
///
/// The protocol allows one request in flight, so methods take `&mut self`.
/// For several devices, use one `Client` per device.
///
/// Dropping a `Client` closes the socket without telling the device; call
/// [`disconnect`](Self::disconnect) to end the session cleanly.
///
/// Methods marked **Destructive** change or erase data on the device.
#[derive(Debug)]
pub struct Client {
    pub(crate) config: ClientConfig,
    link: Option<TcpLink>,
    session: Session,
    /// User layout detected from the last user table read.
    pub(crate) detected_user_format: Option<UserFormat>,
}

impl Client {
    /// Connect and authenticate.
    ///
    /// Sends `CMD_CONNECT`; if the device answers `CMD_ACK_UNAUTH`, sends
    /// `CMD_AUTH` with the comm key derived from
    /// [`ClientConfig::password`]. Connection-level failures (I/O, timeout,
    /// closed) are retried according to [`ClientConfig::retry`].
    ///
    /// # Errors
    ///
    /// [`Error::Unauthorized`] if the password is rejected,
    /// [`Error::DeviceError`] for any other refusal, [`Error::Timeout`] or
    /// [`Error::Io`] if the device cannot be reached, and
    /// [`Error::Unsupported`] for UDP (not implemented yet).
    pub async fn connect(config: ClientConfig) -> Result<Self> {
        if config.transport == Transport::Udp {
            return Err(Error::Unsupported("UDP transport is not implemented yet"));
        }
        if config.pre_check {
            probe(&config).await?;
        }
        let attempts = config.retry.attempts();
        let mut attempt = 1;
        loop {
            match Self::connect_once(config.clone()).await {
                Ok(client) => return Ok(client),
                Err(e) if e.is_fatal() && attempt < attempts => {
                    tracing::warn!(attempt, %e, "connect failed; retrying");
                    tokio::time::sleep(config.retry.backoff).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn connect_once(config: ClientConfig) -> Result<Self> {
        let link = TcpLink::connect(&config.host, config.port, config.connect_timeout).await?;
        let mut client = Self {
            config,
            link: Some(link),
            session: Session::new(),
            detected_user_format: None,
        };
        client.handshake().await?;
        Ok(client)
    }

    async fn handshake(&mut self) -> Result<()> {
        let reply = self.request(Command::Connect, &[]).await?;
        self.session.set_session_id(reply.header.session_id);
        tracing::debug!(session_id = reply.header.session_id, "connected");
        let reply = if reply.header.command == Command::AckUnauth {
            let key = commkey::derive(
                self.config.password.unwrap_or(0),
                reply.header.session_id,
                commkey::DEFAULT_TICKS,
            );
            self.request(Command::Auth, &key).await?
        } else {
            reply
        };
        match reply.header.command {
            Command::AckOk => Ok(()),
            other => {
                self.close().await;
                Err(failure(Command::Connect, other))
            }
        }
    }

    /// End the session with `CMD_EXIT` and close the socket. The socket is
    /// closed even if the device does not acknowledge.
    ///
    /// # Errors
    ///
    /// The error from `CMD_EXIT`, if any. A client that is already closed
    /// returns `Ok`.
    pub async fn disconnect(mut self) -> Result<()> {
        if self.link.is_none() {
            return Ok(());
        }
        let result = self.ack(Command::Exit, &[]).await;
        self.close().await;
        result.map(drop)
    }

    /// Close the current connection, if any, and connect again with the
    /// same configuration.
    ///
    /// # Errors
    ///
    /// As for [`connect`](Self::connect).
    pub async fn reconnect(&mut self) -> Result<()> {
        self.close().await;
        *self = Self::connect(self.config.clone()).await?;
        Ok(())
    }

    /// Whether the connection is open. It closes after a timeout, an I/O or
    /// framing error, or when the device closes it.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.link.is_some()
    }

    /// The session ID the device assigned.
    #[must_use]
    pub fn session_id(&self) -> u16 {
        self.session.session_id()
    }

    /// The configuration in use.
    #[must_use]
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    pub(crate) async fn close(&mut self) {
        if let Some(link) = self.link.take() {
            link.shutdown().await;
        }
    }

    /// Send a request and return the first reply, whatever its code.
    pub(crate) async fn request(&mut self, command: Command, payload: &[u8]) -> Result<Packet> {
        let reply_id = self.send(command, payload).await?;
        let reply = self.recv(command).await?;
        if reply.header.reply_id != reply_id {
            // Accepted anyway: firmware is not known to echo it reliably.
            tracing::warn!(
                ?command,
                expected = reply_id,
                got = reply.header.reply_id,
                "reply ID mismatch"
            );
        }
        Ok(reply)
    }

    /// Send a request and require a success reply (`AckOk`, `Data` or
    /// `PrepareData`).
    pub(crate) async fn ack(&mut self, command: Command, payload: &[u8]) -> Result<Packet> {
        let reply = self.request(command, payload).await?;
        if reply.header.command.is_success_reply() {
            Ok(reply)
        } else {
            Err(failure(command, reply.header.command))
        }
    }

    pub(crate) async fn send(&mut self, command: Command, payload: &[u8]) -> Result<u16> {
        let link = self.link.as_mut().ok_or(Error::ConnectionClosed)?;
        let (reply_id, bytes) = self.session.request(command, payload);
        if let Err(e) = link.send(&bytes).await {
            self.close().await;
            return Err(e);
        }
        Ok(reply_id)
    }

    /// Receive the next packet that is not a live event, acknowledging any
    /// events that arrive first. Any timeout, I/O or framing error closes
    /// the connection.
    pub(crate) async fn recv(&mut self, command: Command) -> Result<Packet> {
        let after = self.config.request_timeout;
        for _ in 0..MAX_STRAY_EVENTS {
            let packet = self.recv_within(command, after).await?;
            if packet.header.command != Command::RegEvent {
                return Ok(packet);
            }
            tracing::debug!(flags = packet.header.session_id, "acknowledging stray event");
            let ack = self.session.event_ack();
            let link = self.link.as_mut().ok_or(Error::ConnectionClosed)?;
            if let Err(e) = link.send(&ack).await {
                self.close().await;
                return Err(e);
            }
        }
        Err(Error::Malformed(ProtoError::UnexpectedReply {
            got: Command::RegEvent,
            context: "too many events while waiting for a reply",
        }))
    }

    /// Receive one packet within `after`. A timeout becomes
    /// [`Error::Timeout`] for `command` and closes the connection.
    pub(crate) async fn recv_within(&mut self, command: Command, after: Duration) -> Result<Packet> {
        let link = self.link.as_mut().ok_or(Error::ConnectionClosed)?;
        let raw = match tokio::time::timeout(after, link.recv()).await {
            Ok(Ok(raw)) => raw,
            Ok(Err(e)) => {
                self.close().await;
                return Err(e);
            }
            Err(_) => {
                self.close().await;
                return Err(Error::Timeout { command, after });
            }
        };
        Ok(packet::decode(raw, self.config.checksum_policy)?)
    }

    /// Like [`recv_within`](Self::recv_within), but a timeout leaves the
    /// connection open and returns `None`.
    pub(crate) async fn try_recv_within(&mut self, after: Duration) -> Result<Option<Packet>> {
        let link = self.link.as_mut().ok_or(Error::ConnectionClosed)?;
        match tokio::time::timeout(after, link.recv()).await {
            Ok(Ok(raw)) => Ok(Some(packet::decode(raw, self.config.checksum_policy)?)),
            Ok(Err(e)) => {
                self.close().await;
                Err(e)
            }
            Err(_) => Ok(None),
        }
    }
}

/// Map a non-success reply to an error.
pub(crate) fn failure(command: Command, reply: Command) -> Error {
    match reply {
        Command::AckUnauth => Error::Unauthorized,
        reply => Error::DeviceError { command, reply },
    }
}

/// Check that the device accepts TCP connections, without starting a
/// session. This replaces pyzk's ping and TCP test; there is no ICMP.
///
/// # Errors
///
/// [`Error::Timeout`] or [`Error::Io`] if the connection fails.
pub async fn probe(config: &ClientConfig) -> Result<()> {
    let link = TcpLink::connect(&config.host, config.port, config.connect_timeout).await?;
    link.shutdown().await;
    Ok(())
}
