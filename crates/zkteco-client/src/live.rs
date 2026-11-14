//! Real-time events (`CMD_REG_EVENT`) as a [`Stream`].

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::{FusedStream, Stream};
use tokio::time::Instant;
use zkteco_proto::records::event::LiveEvent;
use zkteco_proto::{requests, Command, EventFlags, Uid, UserId};

use crate::client::Client;
use crate::config::ClientConfig;
use crate::error::{Error, Result};

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
    /// `CMD_GET_TIME` to check that it still answers. A device that does
    /// not answer within the request timeout ends the connection. `None`
    /// never checks: a device that disappears without closing the
    /// connection then goes unnoticed. Default: 60 seconds.
    pub heartbeat: Option<Duration>,
    /// Reconnect and resubscribe when the connection is lost. `None` ends
    /// the stream instead. Default: `None`.
    pub reconnect: Option<ReconnectPolicy>,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            events: EventFlags::ATTLOG,
            prepare: true,
            resolve_users: false,
            heartbeat: Some(Duration::from_secs(60)),
            reconnect: None,
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

/// What one capture step produced.
#[derive(Debug)]
enum Step {
    Event(LiveEvent),
    /// Nothing yet; poll again.
    Idle,
    Failed(Error),
    End,
}

/// The capture state, owned by whichever step is running.
#[derive(Debug)]
struct Capture {
    client: Client,
    opts: CaptureOptions,
    by_uid: HashMap<Uid, UserId>,
    by_user_id: HashMap<UserId, Uid>,
    last_heard: Instant,
    /// The device was disabled when the capture started, and the capture
    /// enabled it.
    enabled_device: bool,
    /// Consecutive failed reconnects.
    failures: u32,
    ended: bool,
}

type StepFuture = Pin<Box<dyn Future<Output = (Capture, Step)> + Send>>;

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
}

impl fmt::Debug for LiveCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match &self.state {
            State::Ready(c) => {
                if c.ended {
                    "ended"
                } else {
                    "ready"
                }
            }
            State::Stepping(_) => "stepping",
            State::Moving => "moving",
        };
        f.debug_struct("LiveCapture").field("state", &state).finish()
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
    /// `CMD_REG_EVENT` with [`CaptureOptions::events`].
    ///
    /// # Errors
    ///
    /// Any failure while subscribing. The client is dropped (its connection
    /// closed) in that case; reconnect with [`LiveCapture::connect`] or a
    /// new [`Client`].
    pub async fn live_capture(self, opts: CaptureOptions) -> Result<LiveCapture> {
        let mut capture = Capture {
            client: self,
            opts,
            by_uid: HashMap::new(),
            by_user_id: HashMap::new(),
            last_heard: Instant::now(),
            enabled_device: false,
            failures: 0,
            ended: false,
        };
        capture.subscribe().await?;
        Ok(LiveCapture {
            state: State::Ready(Box::new(capture)),
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
    pub async fn next(&mut self) -> Option<Result<LiveEvent>> {
        std::future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await
    }

    /// Unsubscribe (`CMD_REG_EVENT` with an empty mask), disable the device
    /// again if the capture enabled it, and return the client.
    ///
    /// A step in progress is finished first: up to a second while idle,
    /// longer during a heartbeat or a reconnect. An event it received is
    /// kept, with any other event received before unsubscribing, in the
    /// client's pending queue ([`Client::take_pending_events`]). Nothing is
    /// sent if the connection is closed.
    pub async fn stop(self) -> (Client, Result<()>) {
        let mut capture = match self.state {
            State::Ready(capture) => *capture,
            State::Stepping(step) => {
                let (mut capture, step) = step.await;
                match step {
                    Step::Event(event) => capture.client.unpop_pending(event),
                    Step::Failed(e) => tracing::warn!(%e, "capture step failed while stopping"),
                    Step::Idle | Step::End => {}
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
    type Item = Result<LiveEvent>;

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
        this.state = State::Ready(Box::new(capture));
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

impl Capture {
    async fn step(mut self: Box<Self>) -> (Self, Step) {
        let step = self.advance().await;
        (*self, step)
    }

    async fn advance(&mut self) -> Step {
        if self.ended {
            return Step::End;
        }
        if let Some(event) = self.client.pop_pending() {
            return Step::Event(self.resolve(event));
        }
        if !self.client.is_connected() {
            return self.recover().await;
        }
        if let Err(e) = self.listen().await {
            tracing::warn!(%e, connected = self.client.is_connected(), "live capture error");
            return Step::Failed(e);
        }
        match self.client.pop_pending() {
            Some(event) => Step::Event(self.resolve(event)),
            None => Step::Idle,
        }
    }

    /// Wait up to one tick for a packet, or send the heartbeat if it is due.
    async fn listen(&mut self) -> Result<()> {
        let now = Instant::now();
        let tick_end = now + IDLE_TICK;
        let deadline = match self.opts.heartbeat {
            Some(every) if now >= self.last_heard + every => {
                tracing::debug!("heartbeat");
                self.client.ack(Command::GetTime, &[]).await?;
                self.last_heard = Instant::now();
                return Ok(());
            }
            Some(every) => tick_end.min(self.last_heard + every),
            None => tick_end,
        };
        let Some(packet) = self.client.recv_by(deadline).await? else {
            return Ok(());
        };
        self.last_heard = Instant::now();
        if packet.header.command == Command::RegEvent {
            return self.client.queue_event(&packet).await;
        }
        tracing::warn!(
            command = ?packet.header.command,
            reply_id = packet.header.reply_id,
            "unexpected packet during live capture; ignored"
        );
        Ok(())
    }

    /// One reconnect attempt, after the backoff, under the reconnect policy.
    async fn recover(&mut self) -> Step {
        let Some(policy) = self.opts.reconnect else {
            self.ended = true;
            return Step::End;
        };
        if self.failures >= policy.max_attempts {
            tracing::warn!(failures = self.failures, "live capture: giving up reconnecting");
            self.ended = true;
            return Step::End;
        }
        tokio::time::sleep(policy.backoff(self.failures)).await;
        let attempt = async {
            self.client.reconnect().await?;
            self.subscribe().await
        };
        match attempt.await {
            Ok(()) => {
                tracing::info!(after_failures = self.failures, "live capture resubscribed");
                self.failures = 0;
                Step::Idle
            }
            Err(e) => {
                self.failures = self.failures.saturating_add(1);
                if matches!(e, Error::Unauthorized) {
                    self.ended = true;
                }
                tracing::warn!(%e, failures = self.failures, "live capture reconnect failed");
                Step::Failed(e)
            }
        }
    }

    async fn subscribe(&mut self) -> Result<()> {
        if self.opts.resolve_users {
            let users = self.client.users().await?.records;
            self.by_uid = users.iter().map(|u| (u.uid, u.user_id.clone())).collect();
            self.by_user_id = users.into_iter().map(|u| (u.user_id, u.uid)).collect();
        }
        if self.opts.prepare {
            self.client.cancel_capture().await?;
            self.client.verify_user().await?;
        }
        if self.client.device_disabled {
            self.client.enable_device().await?;
            self.enabled_device = true;
        }
        let mask = requests::reg_event(self.opts.events);
        self.client.ack(Command::RegEvent, &mask).await?;
        self.last_heard = Instant::now();
        tracing::debug!(events = ?self.opts.events, "subscribed to live events");
        Ok(())
    }

    async fn unsubscribe(&mut self) -> Result<()> {
        if !self.client.is_connected() {
            return Ok(());
        }
        let mask = requests::reg_event(EventFlags::empty());
        self.client.ack(Command::RegEvent, &mask).await?;
        if self.enabled_device {
            self.client.disable_device().await?;
        }
        Ok(())
    }

    fn resolve(&self, event: LiveEvent) -> LiveEvent {
        let LiveEvent::Attendance(mut a) = event else {
            return event;
        };
        if a.user_id.is_none() {
            a.user_id = a.uid.and_then(|uid| self.by_uid.get(&uid).cloned());
        }
        if a.uid.is_none() {
            a.uid = a.user_id.as_ref().and_then(|id| self.by_user_id.get(id).copied());
        }
        LiveEvent::Attendance(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
