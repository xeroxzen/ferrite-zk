//! Measurements for diagnostics that the normal read path cannot make.

use tokio::time::Instant;
use zkteco_proto::{requests, Command, Transport};

use crate::client::{failure, Client};
use crate::data::RawTable;
use crate::error::{Error, Result};

/// Bytes requested by [`Client::sample_udp_stream`]: pyzk's UDP chunk, large
/// enough that a device streams it in several `Data` packets.
const SAMPLE_LEN: u32 = 16 * 1024;

/// Packets recorded at most in one sample.
const MAX_SAMPLE_PACKETS: usize = 1024;

/// Header fields of one packet received during a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampledPacket {
    /// The packet's command code.
    pub command: Command,
    /// Its session ID field.
    pub session_id: u16,
    /// Its reply ID field.
    pub reply_id: u16,
    /// Payload length.
    pub len: usize,
}

/// The packets a device sent in answer to one large `ReadBuffer` over UDP:
/// header fields only, payloads discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSample {
    /// The connection's session ID.
    pub session_id: u16,
    /// The reply ID of the `ReadBuffer` request.
    pub request_reply_id: u16,
    /// Bytes requested.
    pub requested: u32,
    /// Every packet received, in arrival order, whatever its reply ID. Live
    /// events are acknowledged and queued as usual, and not listed.
    pub packets: Vec<SampledPacket>,
    /// Whether the reply ended (`AckOk`, an inline `Data`, or a failure
    /// reply) within the request timeout.
    pub complete: bool,
}

impl Client {
    /// Stage `which`, request one chunk of up to 16 KiB over UDP, and record
    /// the header fields of every packet that answers it, then release the
    /// buffer. Returns `None` if the device sent the whole table inline (too
    /// small to stream).
    ///
    /// This measures whether UDP `Data` packets carry a sequence number
    /// (DECISIONS.md D25). It returns no table data, so a reordered stream
    /// cannot produce wrong records.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] over TCP; device, timeout or connection errors.
    pub async fn sample_udp_stream(&mut self, which: RawTable) -> Result<Option<StreamSample>> {
        if self.transport() != Transport::Udp {
            return Err(Error::Unsupported("stream sampling is for UDP connections"));
        }
        let (command, selector) = which.staging();
        let reply = self
            .request(
                Command::PrepareBuffer,
                &requests::prepare_buffer(command, selector, 0),
            )
            .await?;
        let size = match reply.header.command {
            Command::Data => return Ok(None),
            Command::AckOk => requests::decode_prepare_buffer_ack(&reply.payload)?,
            other => return Err(failure(Command::PrepareBuffer, other)),
        };
        let sample = self.sample_chunk(size.min(SAMPLE_LEN)).await;
        if self.is_connected() {
            self.free_data().await?;
        }
        sample.map(Some)
    }

    async fn sample_chunk(&mut self, len: u32) -> Result<StreamSample> {
        let reply_id = self
            .send(Command::ReadBuffer, &requests::read_buffer(0, len))
            .await?;
        let deadline = Instant::now() + self.config.request_timeout;
        let mut sample = StreamSample {
            session_id: self.session_id(),
            request_reply_id: reply_id,
            requested: len,
            packets: Vec::new(),
            complete: false,
        };
        let mut streaming = false;
        for _ in 0..MAX_SAMPLE_PACKETS {
            let Some(p) = self.recv_by(deadline).await? else {
                break;
            };
            let command = p.header.command;
            if command == Command::RegEvent {
                self.queue_event(&p).await?;
                continue;
            }
            sample.packets.push(SampledPacket {
                command,
                session_id: p.header.session_id,
                reply_id: p.header.reply_id,
                len: p.payload.len(),
            });
            streaming |= command == Command::PrepareData;
            let inline = command == Command::Data && !streaming;
            if inline || command == Command::AckOk || !command.is_success_reply() {
                sample.complete = true;
                break;
            }
        }
        if !sample.complete {
            self.drain().await?;
        }
        Ok(sample)
    }
}
