use std::collections::VecDeque;
use std::time::SystemTime;

use tokio::time::Instant;
use zkteco_proto::commkey;
use zkteco_proto::packet::{self, Packet};
use zkteco_proto::records::attendance::AttendanceFormat;
use zkteco_proto::records::event::{self, LiveEvent};
use zkteco_proto::records::user::UserFormat;
use zkteco_proto::session::Session;
use zkteco_proto::{Command, ProtoError};

use crate::config::{ClientConfig, ReplyIdPolicy, Transport, READ_ONLY_COMMANDS};
use crate::data::UserIdSource;
use crate::error::{Error, Result};
use crate::link::{Link, TcpLink};
use crate::live::{user_id_source, CapturedEvent, Source};
use crate::record::{Direction, Recording};

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
    link: Option<Link>,
    session: Session,
    /// User layout detected from the last user table read.
    pub(crate) detected_user_format: Option<UserFormat>,
    /// Attendance layout detected from the last attendance read.
    pub(crate) detected_attendance_format: Option<AttendanceFormat>,
    /// Live events received while waiting for replies.
    pending: VecDeque<Queued>,
    dropped_events: u64,
    discarded_replies: u64,
    recording: Option<Recording>,
    /// The transport of the current or last connection.
    wire: zkteco_proto::Transport,
    /// Chunk size for single-datagram UDP reads; shrinks if the device
    /// splits replies into smaller packets.
    pub(crate) udp_chunk: u32,
    /// Writes recorded instead of sent, in dry-run mode.
    planned: Vec<Packet>,
    /// Synthesized replies to planned writes, answered before the socket.
    synthetic: VecDeque<Packet>,
    /// Whether this client disabled the device (`CMD_DISABLEDEVICE`) and
    /// has not enabled it since.
    pub(crate) device_disabled: bool,
}

/// The first single-datagram chunk size over UDP: the per-packet size seen
/// in `PrepareData` replies (PROTOCOL.md 7.2), under the manual's 1 KiB
/// limit.
pub(crate) const UDP_SINGLE_DATAGRAM: u32 = 1008;

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
    /// [`Error::Io`] if the device cannot be reached.
    pub async fn connect(config: ClientConfig) -> Result<Self> {
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
        use zkteco_proto::Transport as Wire;
        match config.transport {
            Transport::Tcp => Self::connect_over(config, Wire::Tcp).await,
            Transport::Udp => Self::connect_over(config, Wire::Udp).await,
            Transport::Auto => match Self::connect_over(config.clone(), Wire::Tcp).await {
                Ok(client) => {
                    tracing::info!(host = %config.host, "connected over TCP (auto)");
                    Ok(client)
                }
                Err(e) if falls_back(&e) => {
                    tracing::info!(host = %config.host, %e, "TCP failed; trying UDP (auto)");
                    let client = Self::connect_over(config, Wire::Udp).await?;
                    tracing::info!(host = %client.config.host, "connected over UDP (auto)");
                    Ok(client)
                }
                Err(e) => Err(e),
            },
        }
    }

    async fn connect_over(config: ClientConfig, wire: zkteco_proto::Transport) -> Result<Self> {
        let link = Link::connect(&config, wire).await?;
        let mut client = Self {
            config,
            link: Some(link),
            session: Session::new(),
            detected_user_format: None,
            detected_attendance_format: None,
            pending: VecDeque::new(),
            dropped_events: 0,
            discarded_replies: 0,
            recording: None,
            wire,
            udp_chunk: UDP_SINGLE_DATAGRAM,
            device_disabled: false,
            planned: Vec::new(),
            synthetic: VecDeque::new(),
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
        let recording = self.recording.take();
        let (dropped, discarded) = (self.dropped_events, self.discarded_replies);
        let disabled = self.device_disabled;
        let planned = std::mem::take(&mut self.planned);
        *self = fresh;
        self.planned = planned;
        self.device_disabled = disabled;
        self.pending = pending;
        self.recording = recording;
        self.dropped_events = dropped;
        self.discarded_replies = discarded;
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

    /// The transport of the current connection (or the last one, once
    /// closed). With [`Transport::Auto`] this is the one that worked.
    #[must_use]
    pub fn transport(&self) -> zkteco_proto::Transport {
        self.wire
    }

    /// Replies discarded under [`ReplyIdPolicy::Strict`] because their reply
    /// ID did not match the request, since the client was created.
    #[must_use]
    pub fn discarded_replies(&self) -> u64 {
        self.discarded_replies
    }

    /// Record every packet sent and received from now on, until
    /// [`take_recording`](Self::take_recording). Restarts a recording in
    /// progress.
    pub fn start_recording(&mut self) {
        self.recording = Some(Recording::default());
    }

    /// Stop recording and return what was recorded (empty if recording was
    /// not started).
    pub fn take_recording(&mut self) -> Recording {
        self.recording.take().unwrap_or_default()
    }

    /// The user layout in use: configured, or detected by the last user
    /// table read. `None` before any read.
    #[must_use]
    pub fn user_format(&self) -> Option<UserFormat> {
        self.config.user_format.or(self.detected_user_format)
    }

    /// The attendance layout in use: configured, or detected by the last
    /// attendance read. `None` before any read.
    #[must_use]
    pub fn attendance_format(&self) -> Option<AttendanceFormat> {
        self.config.attendance_format.or(self.detected_attendance_format)
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

    /// Send one packet. Every outgoing packet passes here, so this is where
    /// read-only mode is enforced.
    async fn send_raw(&mut self, bytes: &[u8]) -> Result<()> {
        let command = Command::from(u16::from_le_bytes([bytes[0], bytes[1]]));
        if self.config.dry_run && !READ_ONLY_COMMANDS.contains(&command) {
            return self.plan(bytes);
        }
        if self.config.read_only {
            let subscription = command == Command::RegEvent && self.config.allow_event_subscription;
            if !READ_ONLY_COMMANDS.contains(&command) && !subscription {
                tracing::warn!(?command, "refused: read-only client");
                return Err(Error::ReadOnly(command));
            }
        }
        let link = self.link.as_mut().ok_or(Error::ConnectionClosed)?;
        if let Some(rec) = &mut self.recording {
            rec.push(Direction::HostToDevice, bytes);
        }
        if let Err(e) = link.send(bytes).await {
            self.close().await;
            return Err(e);
        }
        Ok(())
    }

    /// Record a write instead of sending it, and queue a synthesized
    /// `CMD_ACK_OK` for it.
    fn plan(&mut self, bytes: &[u8]) -> Result<()> {
        let packet = packet::decode(
            bytes::Bytes::copy_from_slice(bytes),
            zkteco_proto::ChecksumPolicy::Reject,
        )?;
        tracing::info!(command = ?packet.header.command, len = packet.payload.len(), "dry run: not sent");
        let reply = packet::encode(
            Command::AckOk,
            packet.header.session_id,
            packet.header.reply_id,
            &[],
        );
        self.synthetic.push_back(packet::decode(
            reply.freeze(),
            zkteco_proto::ChecksumPolicy::Reject,
        )?);
        self.planned.push(packet);
        Ok(())
    }

    /// The packets a dry-run client would have sent, in order, since the
    /// last call. Empty unless [`ClientConfig::dry_run`] is set.
    pub fn take_planned(&mut self) -> Vec<Packet> {
        std::mem::take(&mut self.planned)
    }

    /// Wait up to one request timeout for the packet answering `reply_id`.
    ///
    /// Live events are acknowledged and queued. A packet with another reply
    /// ID is discarded under [`ReplyIdPolicy::Strict`] and accepted under
    /// [`ReplyIdPolicy::Lenient`], with a warning either way. A timeout,
    /// I/O or framing error closes the connection.
    pub(crate) async fn recv_reply(&mut self, command: Command, reply_id: u16) -> Result<Packet> {
        match self.wait_reply(command, reply_id).await? {
            Some(packet) => Ok(packet),
            None => Err(self.timed_out(command).await),
        }
    }

    /// Close the connection and return the timeout error for `command`.
    pub(crate) async fn timed_out(&mut self, command: Command) -> Error {
        self.close().await;
        Error::Timeout {
            command,
            after: self.config.request_timeout,
        }
    }

    /// As [`recv_reply`](Self::recv_reply), but `None` when the deadline
    /// passes, leaving the connection open for the caller to decide.
    pub(crate) async fn wait_reply(&mut self, command: Command, reply_id: u16) -> Result<Option<Packet>> {
        if let Some(reply) = self.synthetic.pop_front() {
            debug_assert_eq!(reply.header.reply_id, reply_id);
            return Ok(Some(reply));
        }
        let deadline = Instant::now() + self.config.request_timeout;
        // The deadline ends the wait; the count bounds the loop regardless.
        for _ in 0..MAX_PACKETS_PER_REPLY {
            let Some(packet) = self.recv_by(deadline).await? else {
                return Ok(None);
            };
            if packet.header.command == Command::RegEvent {
                self.queue_event(&packet).await?;
                continue;
            }
            let got = packet.header.reply_id;
            if got == reply_id {
                return Ok(Some(packet));
            }
            let reply = packet.header.command;
            if self.config.reply_id_policy == ReplyIdPolicy::Lenient {
                tracing::warn!(
                    ?command,
                    expected = reply_id,
                    got,
                    ?reply,
                    "reply ID mismatch; accepted (lenient)"
                );
                return Ok(Some(packet));
            }
            self.discarded_replies += 1;
            tracing::warn!(
                ?command,
                expected = reply_id,
                got,
                ?reply,
                "reply ID mismatch; discarded"
            );
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
            Ok(Ok(raw)) => {
                if let Some(rec) = &mut self.recording {
                    rec.push(Direction::DeviceToHost, &raw);
                }
                Ok(Some(packet::decode(raw, self.config.checksum_policy)?))
            }
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
        self.ack_event().await?;
        let capacity = self.config.pending_event_capacity;
        let (at, instant) = (SystemTime::now(), std::time::Instant::now());
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
            self.pending.push_back(Queued {
                event,
                source: Source::Live,
                user_id_source: None,
                at,
                instant,
            });
        }
        Ok(())
    }

    /// Acknowledge an event packet (`CMD_ACK_OK` with reply ID 0).
    pub(crate) async fn ack_event(&mut self) -> Result<()> {
        let ack = self.session.event_ack();
        self.send_raw(&ack).await
    }

    /// Put an event back at the front of the pending queue.
    pub(crate) fn unpop_pending(&mut self, queued: Queued) {
        self.pending.push_front(queued);
    }

    /// The oldest pending live event, if any.
    pub(crate) fn pop_pending(&mut self) -> Option<Queued> {
        self.pending.pop_front()
    }

    /// Remove and return the live events that arrived while the client was
    /// waiting for replies, oldest first. Each was acknowledged on arrival.
    pub fn take_pending_events(&mut self) -> Vec<LiveEvent> {
        self.pending.drain(..).map(|q| q.event).collect()
    }

    /// Like [`take_pending_events`](Self::take_pending_events), with each
    /// event's source and arrival times. After [`LiveCapture::stop`] this
    /// includes backfilled records that were not yet delivered, still
    /// marked [`Source::Backfill`].
    ///
    /// [`LiveCapture::stop`]: crate::LiveCapture::stop
    pub fn take_pending_captured(&mut self) -> Vec<CapturedEvent> {
        self.pending
            .drain(..)
            .map(|q| CapturedEvent {
                device_time: match &q.event {
                    LiveEvent::Attendance(a) => Some(a.timestamp),
                    LiveEvent::Raw { .. } => None,
                },
                user_id_source: user_id_source(&q.event, q.user_id_source, false),
                event: q.event,
                source: q.source,
                received_at: q.at,
                received_instant: q.instant,
            })
            .collect()
    }

    /// Events dropped because the pending queue was full (see
    /// [`ClientConfig::pending_event_capacity`]), since the client was
    /// created.
    #[must_use]
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }
}

/// A live event and when it arrived.
#[derive(Debug, Clone)]
pub(crate) struct Queued {
    pub(crate) event: LiveEvent,
    /// Live, or a backfilled record put back by `LiveCapture::stop`.
    pub(crate) source: Source,
    /// Where the user ID came from, when already known (backfill reads
    /// resolve it; live events are judged when delivered).
    pub(crate) user_id_source: Option<UserIdSource>,
    /// Wall-clock arrival time.
    pub(crate) at: SystemTime,
    /// Monotonic arrival time.
    pub(crate) instant: std::time::Instant,
}

/// Map a non-success reply to an error.
pub(crate) fn failure(command: Command, reply: Command) -> Error {
    match reply {
        Command::AckUnauth => Error::Unauthorized,
        reply => Error::DeviceError { command, reply },
    }
}

/// Whether a failed TCP attempt under [`Transport::Auto`] should fall back
/// to UDP: the device could not be reached or did not answer. A refusal
/// (wrong password, failure reply) is an answer, so it does not.
fn falls_back(e: &Error) -> bool {
    matches!(
        e,
        Error::Io(_) | Error::Timeout { .. } | Error::ConnectionClosed | Error::Malformed(_)
    )
}

/// Check that the device is reachable. This replaces pyzk's ping and TCP
/// test; there is no ICMP.
///
/// Over TCP: a TCP connect, without starting a session. Over UDP, which has
/// no connection: `CMD_CONNECT` (and `CMD_AUTH` if asked) answered within the
/// request timeout, then `CMD_EXIT`. With [`Transport::Auto`]: TCP, then UDP
/// if TCP fails.
///
/// # Errors
///
/// [`Error::Timeout`] or [`Error::Io`] if the device does not answer; over
/// UDP also the errors of [`Client::connect`].
pub async fn probe(config: &ClientConfig) -> Result<()> {
    let tcp = || TcpLink::connect(&config.host, config.port, config.connect_timeout);
    let udp = || async {
        Client::connect_over(config.clone(), zkteco_proto::Transport::Udp)
            .await?
            .disconnect()
            .await
    };
    match config.transport {
        Transport::Tcp => {
            tcp().await?.shutdown().await;
            Ok(())
        }
        Transport::Udp => udp().await,
        Transport::Auto => match tcp().await {
            Ok(link) => {
                link.shutdown().await;
                Ok(())
            }
            Err(e) => {
                tracing::info!(%e, "TCP probe failed; trying UDP (auto)");
                udp().await
            }
        },
    }
}
