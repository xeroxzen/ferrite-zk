//! Fingerprint enrolment on the device's sensor (`CMD_STARTENROLL`).

use std::time::Duration;

use tokio::time::Instant;
use zkteco_proto::records::event::{decode_enroll_report, EnrollReport};
use zkteco_proto::{requests, Command, EventFlags, ProtoError, UserId};

use crate::client::Client;
use crate::error::{Error, Result};

/// Reports accepted in one enrolment before giving up. The reference flow
/// expects seven (two per scan for three scans, then the result).
const MAX_ENROLL_REPORTS: usize = 32;

/// Packets accepted while waiting for one report.
const MAX_PACKETS_PER_REPORT: usize = 64;

/// The highest finger index.
const MAX_FINGER: u8 = 9;

/// Options for [`Client::enroll_user`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EnrollOptions {
    /// How long to wait for each report from the device (each scan
    /// produces some). Default: 60 seconds, as the reference
    /// implementation.
    pub report_timeout: Duration,
}

impl EnrollOptions {
    /// Set how long to wait for each report.
    #[must_use]
    pub const fn report_timeout(mut self, report_timeout: Duration) -> Self {
        self.report_timeout = report_timeout;
        self
    }
}

impl Default for EnrollOptions {
    fn default() -> Self {
        Self {
            report_timeout: Duration::from_secs(60),
        }
    }
}

/// How an enrolment ended.
#[doc = "Unverified against hardware: see DECISIONS.md D30."]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollOutcome {
    /// The device reported the template stored.
    Stored {
        /// Template size, when reported.
        size: Option<u16>,
        /// Storage position, when reported.
        position: Option<u16>,
        /// Scans the device accepted before storing.
        scans: u8,
    },
    /// The finger is already enrolled.
    Duplicate,
    /// The device reported failure.
    Failed,
    /// The device timed out waiting for a finger.
    TimedOut,
    /// A "stored" report before any accepted scan. The reference
    /// implementation treats this as failure; it is reported separately
    /// so it can be checked on the device.
    Unexpected {
        /// The report.
        report: EnrollReport,
        /// Scans accepted before it.
        scans: u8,
    },
    /// No report within [`EnrollOptions::report_timeout`].
    NoReport {
        /// Scans accepted before the silence.
        scans: u8,
    },
}

impl Client {
    /// **Destructive.** Enrol a fingerprint for `user_id` on finger `fid`
    /// (0 to 9) by scanning it on the device. This is pyzk's
    /// `enroll_user`. The user must already exist.
    ///
    /// Cancels any capture, sends `CMD_STARTENROLL` (over UDP the user ID
    /// must be numeric), then reads the device's reports, passing each to
    /// `progress`, until one ends the enrolment. Afterwards it unsubscribes
    /// from events, cancels the capture and returns the device to verify
    /// mode. Attendance events that arrive meanwhile are queued
    /// ([`Client::take_pending_events`]).
    ///
    /// Not cancel-safe: dropping the future mid-way leaves the device in
    /// enrolment mode; call [`cancel_capture`](Self::cancel_capture) and
    /// [`verify_user`](Self::verify_user) to recover.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] for a finger index above 9 or a user ID
    /// that cannot be encoded; device, timeout or connection errors. If the
    /// cleanup after an outcome fails, its error is returned and the
    /// outcome is logged.
    pub async fn enroll_user(
        &mut self,
        user_id: &UserId,
        fid: u8,
        opts: EnrollOptions,
        mut progress: impl FnMut(EnrollReport),
    ) -> Result<EnrollOutcome> {
        let payload = self.start_enroll_payload(user_id, fid)?;
        self.cancel_capture().await?;
        self.ack(Command::StartEnroll, &payload).await?;
        let outcome = self.enroll_reports(opts, &mut progress).await;
        let cleanup = self.end_enroll().await;
        match (outcome, cleanup) {
            (Ok(outcome), Ok(())) => Ok(outcome),
            (Ok(outcome), Err(e)) => {
                tracing::warn!(?outcome, %e, "enrolment ended but cleanup failed");
                Err(e)
            }
            (Err(e), cleanup) => {
                // The enrolment error is the one to report; a cleanup
                // failure after it is usually the same lost connection.
                if let Err(c) = cleanup {
                    tracing::warn!(%c, "cleanup after failed enrolment also failed");
                }
                Err(e)
            }
        }
    }

    fn start_enroll_payload(&self, user_id: &UserId, fid: u8) -> Result<Vec<u8>> {
        if fid > MAX_FINGER {
            return Err(Error::InvalidArgument(ProtoError::InvalidField {
                field: "fid",
                reason: format!("finger index {fid} is above {MAX_FINGER}"),
            }));
        }
        let payload = match self.transport() {
            zkteco_proto::Transport::Tcp => requests::start_enroll_tcp(user_id, fid, self.config.encoding),
            zkteco_proto::Transport::Udp => requests::start_enroll_udp(user_id, fid).map(Vec::from),
        };
        payload.map_err(Error::InvalidArgument)
    }

    async fn enroll_reports(
        &mut self,
        opts: EnrollOptions,
        progress: &mut impl FnMut(EnrollReport),
    ) -> Result<EnrollOutcome> {
        let mut scans: u8 = 0;
        for _ in 0..MAX_ENROLL_REPORTS {
            let Some(report) = self.next_enroll_report(opts.report_timeout).await? else {
                return Ok(EnrollOutcome::NoReport { scans });
            };
            tracing::debug!(?report, scans, "enrolment report");
            progress(report);
            let outcome = match report {
                EnrollReport::ScanAccepted => {
                    scans = scans.saturating_add(1);
                    continue;
                }
                EnrollReport::Other(_) => continue,
                EnrollReport::Stored { .. } if scans == 0 => EnrollOutcome::Unexpected { report, scans },
                EnrollReport::Stored { size, position } => EnrollOutcome::Stored {
                    size,
                    position,
                    scans,
                },
                EnrollReport::Duplicate => EnrollOutcome::Duplicate,
                EnrollReport::Failed => EnrollOutcome::Failed,
                EnrollReport::TimedOut => EnrollOutcome::TimedOut,
            };
            return Ok(outcome);
        }
        Err(Error::Malformed(ProtoError::UnexpectedReply {
            got: Command::RegEvent,
            context: "too many enrolment reports without a result",
        }))
    }

    /// The next enrolment report, or `None` if none arrives within
    /// `timeout`. Attendance events are queued; other packets are skipped.
    async fn next_enroll_report(&mut self, timeout: Duration) -> Result<Option<EnrollReport>> {
        let deadline = Instant::now() + timeout;
        for _ in 0..MAX_PACKETS_PER_REPORT {
            let Some(packet) = self.recv_by(deadline).await? else {
                return Ok(None);
            };
            let Some(flags) = packet.header.event_flags() else {
                tracing::warn!(command = ?packet.header.command, "unexpected packet during enrolment; ignored");
                continue;
            };
            if flags.contains(EventFlags::ATTLOG) {
                self.queue_event(&packet).await?;
                continue;
            }
            self.ack_event().await?;
            match decode_enroll_report(&packet.payload) {
                Ok(report) => return Ok(Some(report)),
                Err(e) => tracing::warn!(%e, ?flags, "undecodable enrolment report; ignored"),
            }
        }
        Err(Error::Malformed(ProtoError::UnexpectedReply {
            got: Command::RegEvent,
            context: "too many packets without an enrolment report",
        }))
    }

    /// Unsubscribe, cancel the capture, return to verify mode.
    async fn end_enroll(&mut self) -> Result<()> {
        let mask = requests::reg_event(EventFlags::empty());
        self.ack(Command::RegEvent, &mask).await?;
        self.cancel_capture().await?;
        self.verify_user().await
    }
}
