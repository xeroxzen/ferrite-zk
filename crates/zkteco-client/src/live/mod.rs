//! Real-time events (`CMD_REG_EVENT`) as a [`Stream`].

mod capture;
mod dedup;

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use chrono::NaiveDateTime;
use futures_core::{FusedStream, Stream};
use zkteco_proto::records::attendance::Attendance;
use zkteco_proto::records::event::LiveEvent;
use zkteco_proto::EventFlags;

use crate::client::Client;
use crate::config::ClientConfig;
use crate::data::UserIdSource;
use crate::error::Result;
use capture::{Capture, Step};

pub use dedup::DEDUP_CAPACITY;

/// The longest a capture step waits for a packet before yielding to the
/// executor. Bounds how long [`LiveCapture::stop`] waits for a step in
/// flight while idle.
const IDLE_TICK: Duration = Duration::from_secs(1);

/// How [`Client::live_capture`] subscribes and keeps the subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOptions {
    /// Events to subscribe to. Default: [`EventFlags::ATTLOG`].
    pub events: EventFlags,
    /// Send `CMD_CANCELCAPTURE` and `CMD_STARTVERIFY` before subscribing,
    /// as the reference implementation does. Default: `true`.
    pub prepare: bool,
    /// Read the user table when subscribing and fill in whichever of
    /// `uid` and `user_id` an attendance event lacks. Users added during
    /// the capture are not resolved until the next subscription (after a
    /// reconnect). Default: `false`.
    pub resolve_users: bool,
    /// After this long without a packet from the device, send
    /// `CMD_GET_TIME` to check that it still answers. `None` never checks.
    /// Default: 60 seconds. TCP keepalive
    /// ([`ClientConfig::tcp_keepalive`]) detects a dead connection
    /// independently.
    pub heartbeat: Option<Duration>,
    /// Unanswered heartbeats in a row before the connection is treated as
    /// lost (closed, and reported as [`Error::Timeout`](crate::Error::Timeout)).
    /// Any packet from the device resets the count. Whether devices answer
    /// requests while subscribed is not known (DECISIONS.md Q6), so one
    /// silence is not enough. Default: 3; 0 counts as 1.
    pub heartbeat_misses: u32,
    /// Reconnect and resubscribe when the connection is lost. `None` ends
    /// the stream instead. Default: `None`.
    pub reconnect: Option<ReconnectPolicy>,
    /// After each reconnect, read the attendance log and deliver the
    /// records stored while disconnected, marked [`Source::Backfill`].
    /// Records already delivered are skipped (see [`CapturedEvent`]).
    /// Needs [`reconnect`](Self::reconnect). Default: `false`.
    pub backfill: bool,
    /// Where an earlier capture stopped. With [`backfill`](Self::backfill),
    /// the first subscription also delivers the records stored since then
    /// (as [`Source::Backfill`], before any live event), skipping those
    /// already delivered. Default: `None`.
    pub resume: Option<Resume>,
}

/// The point an earlier capture reached, persisted by the application so a
/// restarted capture can deliver what was missed while it was not running.
///
/// Backfill takes every record at or after `since`, so the records already
/// delivered at exactly `since` are listed to be skipped. Records stored
/// with a device time before `since` (after the device clock was set back)
/// are not recovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resume {
    /// The latest device time delivered.
    pub since: NaiveDateTime,
    /// The attendance records already delivered with device time `since`.
    pub delivered: Vec<Attendance>,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            events: EventFlags::ATTLOG,
            prepare: true,
            resolve_users: false,
            heartbeat: Some(Duration::from_secs(60)),
            heartbeat_misses: 3,
            reconnect: None,
            backfill: false,
            resume: None,
        }
    }
}

/// How a [`LiveCapture`] reconnects after losing the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// Consecutive failed reconnects (each one a full
    /// [`Client::reconnect`] with its own retries) before the stream ends.
    pub max_attempts: u32,
    /// Wait before the first reconnect; doubles after each failure.
    pub initial_backoff: Duration,
    /// Upper limit for the wait.
    pub max_backoff: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 10,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }
}

impl ReconnectPolicy {
    /// The wait before reconnect attempt `failures + 1`.
    #[must_use]
    pub fn backoff(&self, failures: u32) -> Duration {
        let factor = 1u32 << failures.min(16);
        self.initial_backoff.saturating_mul(factor).min(self.max_backoff)
    }
}

/// Where a [`CapturedEvent`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// Pushed by the device as it happened.
    Live,
    /// Read from the attendance log after a reconnect: it happened while
    /// the connection was down.
    Backfill,
}

/// One event from a [`LiveCapture`], with when and how it arrived.
///
/// Attendance is delivered at most once per capture: an event whose user,
/// device time, status and punch match one of the last
/// [`DEDUP_CAPACITY`] delivered is skipped, whether it arrives live or by
/// backfill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedEvent {
    /// The event.
    pub event: LiveEvent,
    /// Live, or backfilled after a reconnect.
    pub source: Source,
    /// For attendance events, where `user_id` came from: the event itself,
    /// the user table ([`CaptureOptions::resolve_users`], or backfill's
    /// read), or nowhere. `None` for undecoded events.
    pub user_id_source: Option<UserIdSource>,
    /// The device's clock reading in the event (attendance time; device
    /// local, no time zone). `None` for undecoded events.
    pub device_time: Option<NaiveDateTime>,
    /// Host wall-clock time when the packet arrived (for backfill, when the
    /// log was read).
    pub received_at: SystemTime,
    /// Host monotonic time of arrival, for ordering and intervals that
    /// clock changes cannot disturb.
    pub received_instant: std::time::Instant,
}

/// Where an attendance event's user ID came from. `known` is what an
/// earlier step established (a backfill read); `filled` says the user ID
/// was absent from the event and has since been filled in from the user
/// table. `None` for undecoded events.
pub(crate) fn user_id_source(
    event: &LiveEvent,
    known: Option<UserIdSource>,
    filled: bool,
) -> Option<UserIdSource> {
    let LiveEvent::Attendance(a) = event else {
        return None;
    };
    Some(match (known, a.user_id.is_some()) {
        (_, false) => UserIdSource::Unresolved,
        (Some(UserIdSource::UserTable), true) => UserIdSource::UserTable,
        (_, true) if filled => UserIdSource::UserTable,
        (_, true) => UserIdSource::Record,
    })
}

/// Counters for one [`LiveCapture`], since it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureStats {
    /// Events delivered (live and backfilled).
    pub events: u64,
    /// Of those, backfilled.
    pub backfilled: u64,
    /// Duplicate attendance events skipped.
    pub duplicates: u64,
    /// Successful reconnects.
    pub reconnects: u64,
    /// Heartbeats sent.
    pub heartbeats_sent: u64,
    /// Heartbeats answered.
    pub heartbeats_answered: u64,
    /// Heartbeats not answered within the request timeout.
    pub heartbeats_missed: u64,
}

#[derive(Debug, Default)]
struct Counters {
    events: AtomicU64,
    backfilled: AtomicU64,
    duplicates: AtomicU64,
    reconnects: AtomicU64,
    heartbeats_sent: AtomicU64,
    heartbeats_answered: AtomicU64,
    heartbeats_missed: AtomicU64,
}

type StepFuture = Pin<Box<dyn Future<Output = (Box<Capture>, Step)> + Send>>;

enum State {
    Ready(Box<Capture>),
    Stepping(StepFuture),
    /// Only while a poll is moving between the other two.
    Moving,
}

/// A stream of live events from one device.
///
/// Created by [`Client::live_capture`] or [`LiveCapture::connect`]. Yields
/// `Ok(event)` for each event, in arrival order, after acknowledging it.
/// An `Err` item reports a failure without ending the stream: what follows
/// depends on the connection. If it is still open (for example after a
/// packet with a bad checksum), capture continues. If it is lost, the stream
/// reconnects under [`CaptureOptions::reconnect`], or ends (`None`) without
/// it. The stream also ends when reconnecting gives up, or when the device
/// rejects the password on reconnect.
///
/// Events that arrived before the capture started (queued while the client
/// waited for replies) come first.
///
/// [`next`](Self::next) is cancel-safe: the work in progress lives in the
/// `LiveCapture`, not in the future `next` returns, so dropping that future
/// (for example in `tokio::select!`) loses nothing.
///
/// Call [`stop`](Self::stop) to unsubscribe and get the [`Client`] back.
/// Dropping a `LiveCapture` closes the connection without unsubscribing.
pub struct LiveCapture {
    state: State,
    counters: Arc<Counters>,
}

impl fmt::Debug for LiveCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match &self.state {
            State::Ready(c) if c.ended => "ended",
            State::Ready(_) => "ready",
            State::Stepping(_) => "stepping",
            State::Moving => "moving",
        };
        f.debug_struct("LiveCapture")
            .field("state", &state)
            .field("counters", &self.counters)
            .finish()
    }
}

impl Client {
    /// Subscribe to live events and return them as a stream. This is
    /// pyzk's `live_capture`.
    ///
    /// Subscribing: with [`CaptureOptions::resolve_users`], read the user
    /// table; with [`CaptureOptions::prepare`], cancel any capture and
    /// enter verify mode; enable the device if this client disabled it
    /// (it is disabled again by [`LiveCapture::stop`]); send
    /// `CMD_REG_EVENT` with [`CaptureOptions::events`]; with
    /// [`CaptureOptions::backfill`], read the device clock as the starting
    /// point for backfill.
    ///
    /// # Errors
    ///
    /// Any failure while subscribing. The client is dropped (its connection
    /// closed) in that case; reconnect with [`LiveCapture::connect`] or a
    /// new [`Client`].
    pub async fn live_capture(self, opts: CaptureOptions) -> Result<LiveCapture> {
        let counters = Arc::new(Counters::default());
        let mut capture = Capture::new(self, opts, Arc::clone(&counters));
        capture.subscribe().await?;
        capture.resume().await?;
        Ok(LiveCapture {
            state: State::Ready(Box::new(capture)),
            counters,
        })
    }
}

impl LiveCapture {
    /// Connect and subscribe: [`Client::connect`], then
    /// [`Client::live_capture`]. [`CaptureOptions::reconnect`] applies
    /// only once this has succeeded.
    ///
    /// # Errors
    ///
    /// As for those two.
    pub async fn connect(config: ClientConfig, opts: CaptureOptions) -> Result<Self> {
        Client::connect(config).await?.live_capture(opts).await
    }

    /// The next event; `None` once the stream has ended. Cancel-safe.
    pub async fn next(&mut self) -> Option<Result<CapturedEvent>> {
        std::future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await
    }

    /// Counters since the capture started.
    #[must_use]
    pub fn stats(&self) -> CaptureStats {
        let c = &self.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        CaptureStats {
            events: get(&c.events),
            backfilled: get(&c.backfilled),
            duplicates: get(&c.duplicates),
            reconnects: get(&c.reconnects),
            heartbeats_sent: get(&c.heartbeats_sent),
            heartbeats_answered: get(&c.heartbeats_answered),
            heartbeats_missed: get(&c.heartbeats_missed),
        }
    }

    /// Unsubscribe (`CMD_REG_EVENT` with an empty mask), disable the device
    /// again if the capture enabled it, and return the client.
    ///
    /// A step in progress is finished first: up to a second while idle,
    /// longer during a heartbeat, a reconnect or a backfill. Events not yet
    /// delivered (from that step, backfilled, or received before
    /// unsubscribing) are kept in the client's pending queue
    /// ([`Client::take_pending_events`]). Nothing is sent if the
    /// connection is closed.
    pub async fn stop(self) -> (Client, Result<()>) {
        let mut capture = match self.state {
            State::Ready(capture) => {
                let mut capture = capture;
                capture.requeue(None);
                capture
            }
            State::Stepping(step) => {
                let (mut capture, step) = step.await;
                match step {
                    Step::Event(event) => capture.requeue(Some(event)),
                    Step::Failed(e) => {
                        tracing::warn!(%e, "capture step failed while stopping");
                        capture.requeue(None);
                    }
                    Step::Idle | Step::End => capture.requeue(None),
                }
                capture
            }
            State::Moving => unreachable!("a LiveCapture is never left moving"),
        };
        let result = capture.unsubscribe().await;
        (capture.client, result)
    }
}

impl Stream for LiveCapture {
    type Item = Result<CapturedEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let mut step = match std::mem::replace(&mut this.state, State::Moving) {
            State::Ready(capture) => Box::pin(capture.step()) as StepFuture,
            State::Stepping(step) => step,
            State::Moving => unreachable!("a LiveCapture is never left moving"),
        };
        let Poll::Ready((capture, outcome)) = step.as_mut().poll(cx) else {
            this.state = State::Stepping(step);
            return Poll::Pending;
        };
        this.state = State::Ready(capture);
        match outcome {
            Step::Event(event) => Poll::Ready(Some(Ok(event))),
            Step::Failed(e) => Poll::Ready(Some(Err(e))),
            Step::End => Poll::Ready(None),
            Step::Idle => {
                // Poll again for the next step instead of looping here.
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
}

impl FusedStream for LiveCapture {
    fn is_terminated(&self) -> bool {
        matches!(&self.state, State::Ready(c) if c.ended)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use zkteco_proto::records::attendance::{AttendanceStatus, Punch};
    use zkteco_proto::UserId;

    #[test]
    fn backoff_doubles_up_to_the_limit() {
        let p = ReconnectPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(500),
        };
        let waits: Vec<u128> = (0..5).map(|n| p.backoff(n).as_millis()).collect();
        assert_eq!(waits, [100, 200, 400, 500, 500]);
        assert_eq!(p.backoff(u32::MAX), Duration::from_millis(500));
    }

    #[test]
    fn capture_is_send() {
        fn send<T: Send>() {}
        send::<LiveCapture>();
    }

    fn punch(user_id: Option<&str>) -> LiveEvent {
        LiveEvent::Attendance(Attendance {
            uid: None,
            user_id: user_id.map(|s| UserId(s.into())),
            timestamp: NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(8, 0, 0)
                .unwrap(),
            status: AttendanceStatus::Fingerprint,
            punch: Punch::CheckIn,
            workcode: None,
        })
    }

    #[test]
    fn user_id_source_follows_where_the_id_came_from() {
        use UserIdSource::{Record, Unresolved, UserTable};
        let has = punch(Some("7"));
        assert_eq!(user_id_source(&has, None, false), Some(Record));
        assert_eq!(user_id_source(&has, None, true), Some(UserTable), "filled in");
        assert_eq!(
            user_id_source(&has, Some(UserTable), false),
            Some(UserTable),
            "by backfill"
        );
        assert_eq!(user_id_source(&has, Some(Record), false), Some(Record));
        assert_eq!(user_id_source(&punch(None), None, true), Some(Unresolved));
        let raw = LiveEvent::Raw {
            flag: EventFlags::ALARM,
            payload: bytes::Bytes::new(),
        };
        assert_eq!(user_id_source(&raw, None, false), None);
    }
}
