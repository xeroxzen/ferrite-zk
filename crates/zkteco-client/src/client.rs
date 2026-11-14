use std::collections::VecDeque;

use tokio::time::Instant;
use zkteco_proto::commkey;
use zkteco_proto::packet::{self, Packet};
use zkteco_proto::records::event::{self, LiveEvent};
use zkteco_proto::records::user::UserFormat;
use zkteco_proto::session::Session;
use zkteco_proto::{Command, ProtoError, Transport};

use crate::config::{ClientConfig, ReplyIdPolicy};
use crate::error::{Error, Result};
use crate::link::TcpLink;

/// Packets (events, mismatched replies) accepted at most while waiting for
/// one reply.
const MAX_PACKETS_PER_REPLY: usize = 4096;

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
    /// Live events received while waiting for replies.
    pending: VecDeque<LiveEvent>,
    dropped_events: u64,
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
            pending: VecDeque::new(),
            dropped_events: 0,
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
    /// same configuration. Pending events and the dropped-event count are
    /// kept.
    ///
    /// # Errors
    ///
    /// As for [`connect`](Self::connect).
    pub async fn reconnect(&mut self) -> Result<()> {
        self.close().await;
        let fresh = Self::connect(self.config.clone()).await?;
        let pending = std::mem::take(&mut self.pending);
        let dropped = self.dropped_events;
        *self = fresh;
        self.pending = pending;
        self.dropped_events = dropped;
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

    /// Send a request and return its reply, whatever its code.
    pub(crate) async fn request(&mut self, command: Command, payload: &[u8]) -> Result<Packet> {
        let reply_id = self.send(command, payload).await?;
        self.recv_reply(command, reply_id).await
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

    /// Send a request; returns its reply ID.
    pub(crate) async fn send(&mut self, command: Command, payload: &[u8]) -> Result<u16> {
        let (reply_id, bytes) = self.session.request(command, payload);
        self.send_raw(&bytes).await?;
        Ok(reply_id)
    }

    async fn send_raw(&mut self, bytes: &[u8]) -> Result<()> {
        let link = self.link.as_mut().ok_or(Error::ConnectionClosed)?;
        if let Err(e) = link.send(bytes).await {
            self.close().await;
            return Err(e);
        }
        Ok(())
    }

    /// Wait up to one request timeout for the packet answering `reply_id`.
    ///
    /// Live events are acknowledged and queued. A packet with another reply
    /// ID is discarded under [`ReplyIdPolicy::Strict`] and accepted under
    /// [`ReplyIdPolicy::Lenient`], with a warning either way. A timeout,
    /// I/O or framing error closes the connection.
    pub(crate) async fn recv_reply(&mut self, command: Command, reply_id: u16) -> Result<Packet> {
        let after = self.config.request_timeout;
        let deadline = Instant::now() + after;
        // The deadline ends the wait; the count bounds the loop regardless.
        for _ in 0..MAX_PACKETS_PER_REPLY {
            let Some(packet) = self.recv_by(deadline).await? else {
                self.close().await;
                return Err(Error::Timeout { command, after });
            };
            if packet.header.command == Command::RegEvent {
                self.queue_event(&packet).await?;
                continue;
            }
            let got = packet.header.reply_id;
            if got == reply_id {
                return Ok(packet);
            }
            if self.config.reply_id_policy == ReplyIdPolicy::Lenient {
                tracing::warn!(
                    ?command,
                    expected = reply_id,
                    got,
                    "reply ID mismatch; accepted (lenient)"
                );
                return Ok(packet);
            }
            tracing::warn!(?command, expected = reply_id, got, reply = ?packet.header.command, "reply ID mismatch; discarded");
        }
        Err(Error::Malformed(ProtoError::UnexpectedReply {
            got: Command::RegEvent,
            context: "too many unrelated packets while waiting for a reply",
        }))
    }

    /// Receive one packet before `deadline`. `None` on timeout, which leaves
    /// the connection open; I/O and framing errors close it.
    pub(crate) async fn recv_by(&mut self, deadline: Instant) -> Result<Option<Packet>> {
        let link = self.link.as_mut().ok_or(Error::ConnectionClosed)?;
        match tokio::time::timeout_at(deadline, link.recv()).await {
            Ok(Ok(raw)) => Ok(Some(packet::decode(raw, self.config.checksum_policy)?)),
            Ok(Err(e)) => {
                self.close().await;
                Err(e)
            }
            Err(_) => Ok(None),
        }
    }

    /// Acknowledge a live event and add its contents to the pending queue,
    /// dropping the oldest entries when it is full.
    pub(crate) async fn queue_event(&mut self, packet: &Packet) -> Result<()> {
        let ack = self.session.event_ack();
        self.send_raw(&ack).await?;
        let capacity = self.config.pending_event_capacity;
        for event in event::decode(packet, self.config.encoding)? {
            if self.pending.len() >= capacity {
                self.pending.pop_front();
                self.dropped_events += 1;
                tracing::warn!(
                    capacity,
                    dropped_total = self.dropped_events,
                    "pending event queue full; dropped the oldest event"
                );
                if capacity == 0 {
                    continue;
                }
            }
            self.pending.push_back(event);
        }
        Ok(())
    }

    /// Remove and return the live events that arrived while the client was
    /// waiting for replies, oldest first. Each was acknowledged on arrival.
    pub fn take_pending_events(&mut self) -> Vec<LiveEvent> {
        self.pending.drain(..).collect()
    }

    /// Events dropped because the pending queue was full (see
    /// [`ClientConfig::pending_event_capacity`]), since the client was
    /// created.
    #[must_use]
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
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
