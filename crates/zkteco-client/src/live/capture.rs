//! The capture state machine: one step at a time, owned by the stream.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::SystemTime;

use chrono::NaiveDateTime;
use tokio::time::Instant;
use zkteco_proto::records::event::LiveEvent;
use zkteco_proto::{requests, Command, EventFlags, Uid, UserId};

use super::dedup::{Dedup, Key};
use super::{CaptureOptions, CapturedEvent, Counters, Source, IDLE_TICK};
use crate::client::{Client, Queued};
use crate::error::{Error, Result};

/// What one capture step produced.
#[derive(Debug)]
pub(super) enum Step {
    Event(CapturedEvent),
    /// Nothing to deliver yet; poll again.
    Idle,
    Failed(Error),
    End,
}

/// The capture state, owned by whichever step is running.
#[derive(Debug)]
pub(super) struct Capture {
    pub(super) client: Client,
    opts: CaptureOptions,
    by_uid: HashMap<Uid, UserId>,
    by_user_id: HashMap<UserId, Uid>,
    last_heard: Instant,
    missed_heartbeats: u32,
    /// The device was disabled when the capture started, and the capture
    /// enabled it.
    enabled_device: bool,
    /// Consecutive failed reconnects.
    failures: u32,
    pub(super) ended: bool,
    /// Backfilled records not yet delivered.
    backfilled: VecDeque<CapturedEvent>,
    /// Backfill takes records from this device time on: the device clock at
    /// the first subscription, then the latest attendance time delivered.
    watermark: Option<NaiveDateTime>,
    seen: Dedup,
    counters: Arc<Counters>,
}

impl Capture {
    pub(super) fn new(client: Client, opts: CaptureOptions, counters: Arc<Counters>) -> Self {
        Self {
            client,
            opts,
            by_uid: HashMap::new(),
            by_user_id: HashMap::new(),
            last_heard: Instant::now(),
            missed_heartbeats: 0,
            enabled_device: false,
            failures: 0,
            ended: false,
            backfilled: VecDeque::new(),
            watermark: None,
            seen: Dedup::default(),
            counters,
        }
    }

    pub(super) async fn step(mut self: Box<Self>) -> (Box<Self>, Step) {
        let step = self.advance().await;
        (self, step)
    }

    async fn advance(&mut self) -> Step {
        if self.ended {
            return Step::End;
        }
        if let Some(event) = self.backfilled.pop_front() {
            return Step::Event(event);
        }
        if let Some(queued) = self.client.pop_pending() {
            return self.admit(queued).map_or(Step::Idle, Step::Event);
        }
        if !self.client.is_connected() {
            return self.recover().await;
        }
        if let Err(e) = self.listen().await {
            tracing::warn!(%e, connected = self.client.is_connected(), "live capture error");
            return Step::Failed(e);
        }
        match self.client.pop_pending() {
            Some(queued) => self.admit(queued).map_or(Step::Idle, Step::Event),
            None => Step::Idle,
        }
    }

    /// Resolve, deduplicate and timestamp an event. `None` for a duplicate.
    fn admit(&mut self, queued: Queued) -> Option<CapturedEvent> {
        let source = queued.source;
        let event = self.resolve(queued.event);
        let device_time = match &event {
            LiveEvent::Attendance(a) => {
                if let Some(key) = Key::of(a) {
                    if !self.seen.insert(key) {
                        tracing::debug!(?source, "duplicate attendance event skipped");
                        self.counters.duplicates.fetch_add(1, Ordering::Relaxed);
                        return None;
                    }
                }
                self.watermark = Some(self.watermark.map_or(a.timestamp, |w| w.max(a.timestamp)));
                Some(a.timestamp)
            }
            LiveEvent::Raw { .. } => None,
        };
        self.counters.events.fetch_add(1, Ordering::Relaxed);
        Some(CapturedEvent {
            event,
            source,
            device_time,
            received_at: queued.at,
            received_instant: queued.instant,
        })
    }

    /// Wait up to one tick for a packet, or send the heartbeat if it is due.
    async fn listen(&mut self) -> Result<()> {
        let now = Instant::now();
        let tick_end = now + IDLE_TICK;
        let deadline = match self.opts.heartbeat {
            Some(every) if now >= self.last_heard + every => return self.heartbeat().await,
            Some(every) => tick_end.min(self.last_heard + every),
            None => tick_end,
        };
        let Some(packet) = self.client.recv_by(deadline).await? else {
            return Ok(());
        };
        self.heard();
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

    fn heard(&mut self) {
        self.last_heard = Instant::now();
        self.missed_heartbeats = 0;
    }

    /// Send `CMD_GET_TIME` and wait one request timeout for the answer. An
    /// unanswered heartbeat is tolerated, without closing the connection,
    /// until [`CaptureOptions::heartbeat_misses`] in a row.
    async fn heartbeat(&mut self) -> Result<()> {
        self.counters.heartbeats_sent.fetch_add(1, Ordering::Relaxed);
        let reply_id = self.client.send(Command::GetTime, &[]).await?;
        if self
            .client
            .wait_reply(Command::GetTime, reply_id)
            .await?
            .is_some()
        {
            self.counters.heartbeats_answered.fetch_add(1, Ordering::Relaxed);
            self.heard();
            return Ok(());
        }
        self.counters.heartbeats_missed.fetch_add(1, Ordering::Relaxed);
        self.missed_heartbeats += 1;
        self.last_heard = Instant::now();
        let limit = self.opts.heartbeat_misses.max(1);
        tracing::warn!(missed = self.missed_heartbeats, limit, "heartbeat unanswered");
        if self.missed_heartbeats >= limit {
            return Err(self.client.timed_out(Command::GetTime).await);
        }
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
        if let Err(e) = attempt.await {
            self.failures = self.failures.saturating_add(1);
            if matches!(e, Error::Unauthorized) {
                self.ended = true;
            }
            tracing::warn!(%e, failures = self.failures, "live capture reconnect failed");
            return Step::Failed(e);
        }
        tracing::info!(after_failures = self.failures, "live capture resubscribed");
        self.failures = 0;
        self.counters.reconnects.fetch_add(1, Ordering::Relaxed);
        if self.opts.backfill {
            if let Err(e) = self.backfill().await {
                tracing::warn!(%e, "backfill after reconnect failed");
                return Step::Failed(e);
            }
        }
        Step::Idle
    }

    /// Queue the attendance records stored since the watermark that were not
    /// delivered live. Runs after resubscribing, so nothing falls between
    /// the table read and the subscription.
    async fn backfill(&mut self) -> Result<()> {
        let Some(since) = self.watermark else {
            return Ok(());
        };
        let batch = self.client.attendance_with_users().await?;
        for bad in &batch.rejected {
            tracing::warn!(offset = bad.offset, reason = %bad.reason, "backfill: record rejected");
        }
        let (at, instant) = (SystemTime::now(), std::time::Instant::now());
        let mut added = 0u64;
        for record in batch.records.into_iter().filter(|r| r.record.timestamp >= since) {
            let queued = Queued {
                event: LiveEvent::Attendance(record.record),
                source: Source::Backfill,
                at,
                instant,
            };
            if let Some(event) = self.admit(queued) {
                self.backfilled.push_back(event);
                added += 1;
            }
        }
        tracing::info!(added, %since, "backfill after reconnect");
        self.counters.backfilled.fetch_add(added, Ordering::Relaxed);
        Ok(())
    }

    pub(super) async fn subscribe(&mut self) -> Result<()> {
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
        if self.opts.backfill && self.watermark.is_none() {
            self.watermark = Some(self.client.time().await?);
        }
        self.heard();
        tracing::debug!(events = ?self.opts.events, "subscribed to live events");
        Ok(())
    }

    /// Return undelivered events to the client's pending queue, in order:
    /// `first` (from a step that finished while stopping), then backfilled
    /// records, then what was already pending.
    pub(super) fn requeue(&mut self, first: Option<CapturedEvent>) {
        let rest = std::mem::take(&mut self.backfilled);
        for event in first.into_iter().chain(rest).rev() {
            self.client.unpop_pending(Queued {
                event: event.event,
                source: event.source,
                at: event.received_at,
                instant: event.received_instant,
            });
        }
    }

    pub(super) async fn unsubscribe(&mut self) -> Result<()> {
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
