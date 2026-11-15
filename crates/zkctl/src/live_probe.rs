//! `zkctl diagnose --probe-live`: subscribe to live events for a while and
//! report their shape, and whether the device answers requests while
//! subscribed (DECISIONS.md Q5 and Q6).
//!
//! The client is read-only with event subscription allowed: it sends
//! `CMD_REG_EVENT`, acknowledgements and heartbeats (`CMD_GET_TIME`), and
//! skips the capture preparation (`CMD_CANCELCAPTURE`, `CMD_STARTVERIFY`).
//! Only flags and payload lengths are reported, never payloads.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use zkteco_client::{CaptureOptions, Client, ClientConfig, Direction, LiveEvent, Recording};
use zkteco_proto::{Command, EventFlags};

/// Heartbeat interval during the probe: several heartbeats fit in a short
/// probe, which is what answers Q6.
const PROBE_HEARTBEAT: Duration = Duration::from_secs(5);

/// What the live probe saw.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LiveProbe {
    /// Seconds spent listening.
    pub seconds: u64,
    /// Why subscribing failed, if it did.
    pub error: Option<String>,
    /// Events delivered.
    pub events: u64,
    /// Of those, decoded as attendance.
    pub attendance: u64,
    /// Of those, left undecoded.
    pub raw: u64,
    /// Event packets by flags and payload length.
    pub packets: Vec<EventShape>,
    /// Heartbeats (`CMD_GET_TIME`) sent while subscribed.
    pub heartbeats_sent: u64,
    /// Heartbeats answered.
    pub heartbeats_answered: u64,
    /// Heartbeats unanswered within the timeout.
    pub heartbeats_missed: u64,
    /// Whether the device answered a request while subscribed (Q6); `None`
    /// if no heartbeat was sent.
    pub answers_while_subscribed: Option<bool>,
}

/// One kind of event packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventShape {
    /// The flags field, as a number.
    pub flags: u32,
    /// The flags by name.
    pub flag_names: String,
    /// Payload bytes.
    pub payload_len: usize,
    /// Packets of this shape.
    pub count: usize,
}

/// Configuration for the probe, from the survey's read-only one.
#[must_use]
pub fn config(read_only: ClientConfig) -> ClientConfig {
    read_only.allow_event_subscription(true)
}

/// Listen for `seconds`.
pub async fn run(config: ClientConfig, listen: Duration) -> LiveProbe {
    let mut probe = LiveProbe {
        seconds: listen.as_secs(),
        ..LiveProbe::default()
    };
    let mut client = match Client::connect(config).await {
        Ok(c) => c,
        Err(e) => {
            probe.error = Some(format!("connect: {e}"));
            return probe;
        }
    };
    client.start_recording();
    let opts = CaptureOptions::default()
        .prepare(false)
        .heartbeat(Some(PROBE_HEARTBEAT))
        .heartbeat_misses(u32::MAX);
    let mut capture = match client.live_capture(opts).await {
        Ok(c) => c,
        Err(e) => {
            probe.error = Some(format!("subscribe: {e}"));
            return probe;
        }
    };
    eprintln!(
        "listening for live events for {} s; punch in on the device now",
        listen.as_secs()
    );
    let until = tokio::time::Instant::now() + listen;
    // Ends at the deadline or when the stream ends.
    while let Ok(Some(item)) = tokio::time::timeout_at(until, capture.next()).await {
        match item {
            Ok(e) if matches!(e.event, LiveEvent::Attendance(_)) => probe.attendance += 1,
            Ok(_) => probe.raw += 1,
            Err(e) => tracing::warn!(%e, "live probe"),
        }
    }
    let stats = capture.stats();
    probe.events = stats.events;
    probe.heartbeats_sent = stats.heartbeats_sent;
    probe.heartbeats_answered = stats.heartbeats_answered;
    probe.heartbeats_missed = stats.heartbeats_missed;
    probe.answers_while_subscribed = (stats.heartbeats_sent > 0).then_some(stats.heartbeats_answered > 0);
    let (mut client, result) = capture.stop().await;
    if let Err(e) = result {
        tracing::warn!(%e, "unsubscribing after the live probe");
    }
    probe.packets = shapes(&client.take_recording());
    if let Err(e) = client.disconnect().await {
        tracing::warn!(%e, "disconnect after the live probe");
    }
    probe
}

/// Event packets received, grouped by flags and payload length.
#[must_use]
pub fn shapes(rec: &Recording) -> Vec<EventShape> {
    let mut counts: BTreeMap<(u32, usize), usize> = BTreeMap::new();
    for p in &rec.packets {
        let b = &p.bytes;
        if p.direction != Direction::DeviceToHost || b.len() < 8 {
            continue;
        }
        if Command::from(u16::from_le_bytes([b[0], b[1]])) != Command::RegEvent {
            continue;
        }
        let flags = u32::from(u16::from_le_bytes([b[4], b[5]]));
        *counts.entry((flags, b.len() - 8)).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|((flags, payload_len), count)| EventShape {
            flags,
            flag_names: format!("{:?}", EventFlags::from_bits_retain(flags)),
            payload_len,
            count,
        })
        .collect()
}
