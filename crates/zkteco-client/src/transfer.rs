//! Buffered reads and uploads (multi-packet transfers).

use std::time::Duration;

use bytes::{Bytes, BytesMut};
use tokio::time::Instant;
use zkteco_proto::transfer::{self, ChunkReceiver, Progress};
use zkteco_proto::{requests, Command, ProtoError, Transport};

use crate::client::{failure, Client};
use crate::config::ReplyIdPolicy;
use crate::error::{Error, Result};

/// How long the line must stay quiet before a failed stream counts as
/// drained.
const DRAIN_QUIET: Duration = Duration::from_millis(250);

/// Packets discarded at most while draining. A full 64 KiB chunk in 1 KiB
/// packets is about 64.
const MAX_DRAIN_PACKETS: usize = 1024;

/// Packets accepted at most for a reply of unknown size.
const MAX_UNSIZED_PACKETS: usize = 1 << 16;

/// Upper bound on the initial buffer reservation, whatever size the device
/// announces.
const MAX_RESERVE: usize = 1 << 20;

impl Client {
    /// Read a whole data table: stage it with `PrepareBuffer`, read it in
    /// chunks, then release it with `FreeData`.
    pub(crate) async fn read_buffered(&mut self, command: Command, table: u32, ext: u32) -> Result<Bytes> {
        let staging = requests::prepare_buffer(command, table, ext);
        let reply = self.request(Command::PrepareBuffer, &staging).await?;
        let size = match reply.header.command {
            Command::Data => return Ok(reply.payload),
            Command::AckOk => requests::decode_prepare_buffer_ack(&reply.payload)?,
            other => return Err(failure(Command::PrepareBuffer, other)),
        };
        tracing::debug!(?command, table, size, "buffered read staged");
        let data = self.read_chunks(size).await;
        if !self.is_connected() {
            return data;
        }
        let freed = self.free_data().await;
        let data = data?;
        freed?;
        Ok(data)
    }

    async fn read_chunks(&mut self, size: u32) -> Result<Bytes> {
        let reserve = usize::try_from(size).map_or(MAX_RESERVE, |s| s.min(MAX_RESERVE));
        let mut out = BytesMut::with_capacity(reserve);
        for (start, len) in transfer::chunk_plan(size, self.config.transport) {
            let chunk = self.read_chunk(start, len).await?;
            out.extend_from_slice(&chunk);
        }
        Ok(out.freeze())
    }

    /// Read one chunk, retrying malformed replies per the retry policy.
    async fn read_chunk(&mut self, start: u32, len: u32) -> Result<Bytes> {
        let attempts = self.config.retry.attempts();
        let mut attempt = 1;
        loop {
            match self.read_chunk_once(start, len).await {
                Ok(data) => return Ok(data),
                Err(e) if retryable(&e) && attempt < attempts => {
                    tracing::warn!(start, len, attempt, %e, "chunk read failed; retrying");
                    self.drain().await?;
                    tokio::time::sleep(self.config.retry.backoff).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn read_chunk_once(&mut self, start: u32, len: u32) -> Result<Bytes> {
        let requested = usize::try_from(len).map_err(|_| Error::Unsupported("chunk larger than memory"))?;
        let reply_id = self
            .send(Command::ReadBuffer, &requests::read_buffer(start, len))
            .await?;
        self.receive_data(Command::ReadBuffer, reply_id, Some(requested))
            .await
    }

    /// Collect the data reply to the request with `reply_id`: inline `Data`,
    /// or `PrepareData`, a stream of `Data` packets, and `AckOk`. Each packet
    /// gets its own request timeout.
    ///
    /// The loop is bounded by a packet count derived from the requested
    /// size; the total may not exceed the announced size, and on TCP each
    /// `Data` packet must carry the next sequence number.
    pub(crate) async fn receive_data(
        &mut self,
        command: Command,
        reply_id: u16,
        requested: Option<usize>,
    ) -> Result<Bytes> {
        let mut rx = ChunkReceiver::new(requested, self.config.transport);
        let limit = requested.map_or(MAX_UNSIZED_PACKETS, |r| r.saturating_add(3));
        for _ in 0..limit {
            let Some(packet) = self.wait_reply(command, reply_id).await? else {
                return Err(self.stalled(command, &rx).await);
            };
            let code = packet.header.command;
            if !code.is_success_reply() {
                return Err(failure(command, code));
            }
            if let Progress::Complete(data) = rx.on_packet(&packet)? {
                return Ok(data);
            }
        }
        Err(Error::Malformed(ProtoError::UnexpectedReply {
            got: Command::Data,
            context: "data stream did not end",
        }))
    }

    /// The error for a stream that went quiet before its reply completed.
    ///
    /// Over UDP with [`ReplyIdPolicy::Strict`], a stream that stalls after
    /// `PrepareData` has most likely lost a datagram. That is a
    /// [`Error::ShortRead`] (retryable) and the socket stays open: late
    /// datagrams of this attempt carry its reply ID, so a retry discards
    /// them. Otherwise it is a timeout, which closes the connection (D22).
    async fn stalled(&mut self, command: Command, rx: &ChunkReceiver) -> Error {
        let udp_strict =
            self.config.transport == Transport::Udp && self.config.reply_id_policy == ReplyIdPolicy::Strict;
        match rx.streaming_size() {
            Some(expected) if udp_strict => {
                tracing::warn!(
                    ?command,
                    expected,
                    got = rx.received(),
                    "UDP stream stalled; datagram lost"
                );
                Error::ShortRead {
                    expected,
                    got: rx.received(),
                }
            }
            _ => self.timed_out(command).await,
        }
    }

    /// After a failed stream, discard packets until `AckOk` or until the
    /// line is quiet, so the next request starts in sync. Live events are
    /// still acknowledged and queued, never discarded.
    pub(crate) async fn drain(&mut self) -> Result<()> {
        let quiet = DRAIN_QUIET.min(self.config.request_timeout);
        for _ in 0..MAX_DRAIN_PACKETS {
            match self.recv_by(Instant::now() + quiet).await {
                Ok(None) => return Ok(()),
                Ok(Some(p)) if p.header.command == Command::AckOk => return Ok(()),
                Ok(Some(p)) if p.header.command == Command::RegEvent => self.queue_event(&p).await?,
                Ok(Some(p)) => tracing::debug!(command = ?p.header.command, "drained packet"),
                Err(e) if e.is_fatal() => return Err(e),
                Err(e) => tracing::debug!(%e, "drained undecodable packet"),
            }
        }
        // Still streaming after this many packets: give up on the link.
        self.close().await;
        Err(Error::ConnectionClosed)
    }

    /// Upload `data` with `PrepareData` and 1 KiB `Data` packets, each
    /// acknowledged, after releasing any staged buffer.
    pub(crate) async fn upload(&mut self, data: &[u8]) -> Result<()> {
        let size = u32::try_from(data.len()).map_err(|_| Error::Unsupported("upload larger than 4 GiB"))?;
        self.free_data().await?;
        self.ack(Command::PrepareData, &requests::prepare_data(size))
            .await?;
        for packet in transfer::upload_packets(data) {
            self.ack(Command::Data, packet).await?;
        }
        Ok(())
    }

    /// Release the device's staged transfer buffer (`CMD_FREE_DATA`).
    pub(crate) async fn free_data(&mut self) -> Result<()> {
        self.ack(Command::FreeData, &[]).await.map(drop)
    }
}

/// Errors after which re-requesting the same chunk can help.
pub(crate) fn retryable(e: &Error) -> bool {
    matches!(
        e,
        Error::Malformed(_) | Error::ShortRead { .. } | Error::DeviceError { .. }
    )
}
