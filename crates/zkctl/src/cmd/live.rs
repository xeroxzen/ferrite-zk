//! `zkctl live`: stream events from one or more devices until Ctrl-C.
//!
//! One task per device owns a `LiveCapture`; events reach a single printer
//! through a channel, so lines from different devices never interleave.
//! Every connection is read-only with event subscription allowed, and the
//! capture skips the preparation commands (cancel capture, verify mode),
//! which a read-only client may not send.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};
use zkteco_client::{CaptureOptions, CapturedEvent, Client, LiveCapture, LiveEvent, ReconnectPolicy, Resume};

use super::live_state::LiveState;
use super::live_status::{self, Link, Wording};

use crate::cli::{Format, Global, LiveArgs};
use crate::event_id::DeviceKey;
use crate::exit::{classify, Exit, Fail};
use crate::output::{self, EventOut};
use crate::target::{self, Access, Target};

/// Events buffered between the device tasks and the printer.
const CHANNEL: usize = 4096;

/// How often a device task checks its connection for status lines.
const STATUS_TICK: Duration = Duration::from_millis(250);

enum Msg {
    /// An event, with the device's name and its key for event ids.
    Event(String, DeviceKey, CapturedEvent),
    /// A status line for a device (human mode only).
    Status(String, String),
    /// A device task finished, with its outcome.
    Ended(String, Exit),
}

pub(super) async fn run(g: &Global, args: &LiveArgs) -> Result<Exit> {
    if g.output == Format::Json {
        return Err(Fail::usage("live is a stream: use --output ndjson (or human)"));
    }
    let targets = target::resolve(g)?;
    let state = args.state.as_deref().map(LiveState::load);
    let (tx, rx) = mpsc::channel(CHANNEL);
    let (stop_tx, stop_rx) = watch::channel(false);
    let wording = (g.output == Format::Human).then_some(Wording {
        verbose: g.verbose > 0,
        reconnect: args.reconnect,
        backfill: args.backfill,
    });
    let mut tasks = tokio::task::JoinSet::new();
    for t in targets {
        let resume = state
            .as_ref()
            .filter(|_| args.backfill)
            .and_then(|s| s.resume(&t.name));
        if let Some(r) = &resume {
            tracing::info!(device = t.name, since = %r.since, "resuming from the state file");
        }
        tasks.spawn(device(
            t,
            options(args, resume),
            (args.reconnect, wording),
            tx.clone(),
            stop_rx.clone(),
        ));
    }
    drop(tx);
    let saved = args.state.as_deref().zip(state).map(|(path, state)| Saved {
        path,
        state,
        every: args.state_flush,
        dirty: false,
        last: Instant::now(),
    });
    let mut printer = Printer {
        format: g.output,
        color: g.output == Format::Human && live_status::use_color(&std::io::stdout()),
        status_color: live_status::use_color(&std::io::stderr()),
        limit: args.count,
        printed: 0,
        unemitted: 0,
        saved,
    };
    let exit = print_until_done(&mut printer, args.duration, rx, &stop_tx).await?;
    while let Some(joined) = tasks.join_next().await {
        if let Err(e) = joined {
            tracing::error!(%e, "device task panicked");
        }
    }
    Ok(exit.worst(printer.finish()))
}

/// The single writer of standard output, and of the state file.
struct Printer<'a> {
    format: Format,
    /// Human output to a terminal, without `NO_COLOR`.
    color: bool,
    /// Standard error is a terminal, without `NO_COLOR`.
    status_color: bool,
    limit: Option<u64>,
    printed: u64,
    /// Events that arrived after the limit was reached.
    unemitted: u64,
    saved: Option<Saved<'a>>,
}

/// The `--state` file and when it was last written.
struct Saved<'a> {
    path: &'a Path,
    state: LiveState,
    every: Duration,
    dirty: bool,
    last: Instant,
}

impl Saved<'_> {
    /// When the next save is due, if anything changed.
    fn due(&self) -> Option<Instant> {
        self.dirty.then(|| self.last + self.every)
    }

    /// Write the file. A failure is reported and retried at the next save;
    /// the stream goes on.
    fn save(&mut self) -> bool {
        self.last = Instant::now();
        match self.state.save(self.path) {
            Ok(()) => {
                self.dirty = false;
                true
            }
            Err(e) => {
                eprintln!("warning: saving {}: {e:#}", self.path.display());
                false
            }
        }
    }
}

impl Printer<'_> {
    fn limit_reached(&self) -> bool {
        self.limit.is_some_and(|n| self.printed >= n)
    }

    /// Emit one event, unless the limit was already reached. `true` when
    /// this event reached the limit.
    fn event(&mut self, device: &str, key: &DeviceKey, event: &CapturedEvent) -> Result<bool> {
        if self.limit_reached() {
            self.unemitted += 1;
            return Ok(false);
        }
        print(self.format, self.color, device, key, event)?;
        self.printed += 1;
        if let (Some(saved), LiveEvent::Attendance(a)) = (self.saved.as_mut(), &event.event) {
            saved.dirty |= saved.state.record(device, a);
            if saved.due().is_some_and(|at| at <= Instant::now()) {
                saved.save();
            }
        }
        Ok(self.limit_reached())
    }

    /// A status line on standard error: the host's local time, the
    /// device, what changed.
    fn status(&self, device: &str, text: &str) -> Result<()> {
        use std::io::Write;
        let line = format!("{}  {device}  {text}", output::local_now());
        writeln!(
            std::io::stderr().lock(),
            "{}",
            live_status::dim(&line, self.status_color)
        )?;
        Ok(())
    }

    /// The final save and the `--count` report. Error if the state could
    /// not be saved.
    fn finish(&mut self) -> Exit {
        if let Some(n) = self.limit.filter(|_| self.limit_reached()) {
            eprintln!(
                "stopped after {n} events; {} further events arrived while unsubscribing and were not emitted",
                self.unemitted
            );
        }
        match self.saved.as_mut() {
            Some(saved) if saved.dirty => {
                if saved.save() {
                    Exit::Ok
                } else {
                    Exit::Error
                }
            }
            _ => Exit::Ok,
        }
    }
}

/// Print events until every device task has ended. Ctrl-C, `--count` and
/// `--duration` ask the tasks to stop; they then unsubscribe and end.
async fn print_until_done(
    printer: &mut Printer<'_>,
    duration: Option<Duration>,
    mut rx: mpsc::Receiver<Msg>,
    stop: &watch::Sender<bool>,
) -> Result<Exit> {
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
    let deadline = tokio::time::sleep(duration.unwrap_or(Duration::MAX));
    let mut deadline = std::pin::pin!(deadline);
    let (mut stopping, mut exit) = (false, Exit::Ok);
    // Ends when every device task has dropped its sender.
    loop {
        let save_at = printer.saved.as_ref().and_then(Saved::due);
        tokio::select! {
            msg = rx.recv() => match msg {
                None => return Ok(exit),
                Some(Msg::Ended(name, outcome)) => {
                    tracing::info!(device = name, ?outcome, "device stream ended");
                    exit = exit.worst(outcome);
                }
                Some(Msg::Status(name, text)) => printer.status(&name, &text)?,
                Some(Msg::Event(name, key, event)) => {
                    if printer.event(&name, &key, &event)? && !stopping {
                        stopping = true;
                        stop.send_replace(true);
                    }
                }
            },
            () = tokio::time::sleep_until(save_at.unwrap_or_else(Instant::now)), if save_at.is_some() => {
                if let Some(saved) = printer.saved.as_mut() {
                    saved.save();
                }
            }
            result = &mut ctrl_c, if !stopping => {
                result?;
                eprintln!("stopping (unsubscribing from every device)");
                stopping = true;
                stop.send_replace(true);
            }
            () = &mut deadline, if duration.is_some() && !stopping => {
                stopping = true;
                stop.send_replace(true);
            }
        }
    }
}

fn print(format: Format, color: bool, device: &str, key: &DeviceKey, event: &CapturedEvent) -> Result<()> {
    let out = EventOut::captured(device, key, event);
    if format == Format::Ndjson {
        return output::ndjson(&out);
    }
    let what = match &out.raw {
        None => format!(
            "{} {} {}",
            who(out.user_id.as_deref(), out.uid),
            out.status.unwrap_or_default(),
            out.punch.unwrap_or_default()
        ),
        Some(raw) => format!("event flags {:#x} payload {}", raw.flags, raw.payload_hex),
    };
    let when = out.device_time.as_deref().unwrap_or("-");
    let source = live_status::source_tag(out.source, color);
    output::text(&format!("{when}  {device}  {source}  {what}"))
}

/// The person, with only what is known.
fn who(user_id: Option<&str>, uid: Option<u16>) -> String {
    match (user_id, uid) {
        (Some(id), Some(uid)) => format!("user {id} (uid {uid})"),
        (Some(id), None) => format!("user {id}"),
        (None, Some(uid)) => format!("uid {uid}"),
        (None, None) => "unknown user".into(),
    }
}

fn options(args: &LiveArgs, resume: Option<Resume>) -> CaptureOptions {
    CaptureOptions {
        prepare: false,
        resolve_users: args.with_users,
        reconnect: args.reconnect.then_some(ReconnectPolicy {
            max_attempts: u32::MAX,
            ..ReconnectPolicy::default()
        }),
        backfill: args.backfill,
        resume,
        ..CaptureOptions::default()
    }
}

/// One device: connect (retrying with `--reconnect`), stream until asked to
/// stop or the stream ends, unsubscribe, and pass on any event still
/// queued. In human mode (`wording`), connection changes become status
/// lines.
async fn device(
    t: Target,
    opts: CaptureOptions,
    (reconnect, wording): (bool, Option<Wording>),
    tx: mpsc::Sender<Msg>,
    mut stop: watch::Receiver<bool>,
) {
    let config = target::config_for(&t, Access::Live).await;
    let Some((mut capture, key)) = subscribe(&t, (reconnect, wording), config, opts, &tx, &mut stop).await
    else {
        return;
    };
    tracing::info!(device = t.name, "subscribed");
    let mut outcome = Exit::Ok;
    let mut link = Link::default();
    let mut tick = tokio::time::interval(STATUS_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Ends when asked to stop, when the stream ends, or when the printer
    // has gone.
    loop {
        tokio::select! {
            item = capture.next() => {
                // Before the event, so a reconnect is announced before
                // what it backfilled.
                let stats = capture.stats();
                if let Some(w) = wording {
                    status(&tx, &t.name, link.change(&stats, w)).await;
                }
                match item {
                    Some(Ok(event)) => {
                        if tx.send(Msg::Event(t.name.clone(), key.clone(), event)).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        report_error(&t.name, &e, &stats, wording, &mut link, &tx).await;
                        outcome = classify(&anyhow::Error::from(e));
                    }
                    None => {
                        eprintln!("error: {}: stream ended", t.name);
                        outcome = outcome.worst(Exit::Connection);
                        break;
                    }
                }
            },
            _ = tick.tick(), if wording.is_some() => {
                if let Some(w) = wording {
                    status(&tx, &t.name, link.change(&capture.stats(), w)).await;
                }
            }
            _ = stop.changed() => {
                outcome = Exit::Ok;
                break;
            }
        }
    }
    finish(&t, &key, capture, &tx, outcome).await;
}

/// An error from the stream. In human mode, one that came with a lost
/// connection is covered by the status lines (a failed reconnect attempt
/// shows with `-v`); anything else is a warning.
async fn report_error(
    device: &str,
    e: &zkteco_client::Error,
    stats: &zkteco_client::CaptureStats,
    wording: Option<Wording>,
    link: &mut Link,
    tx: &mpsc::Sender<Msg>,
) {
    match wording {
        Some(w) if !stats.connected => status(tx, device, link.failed_attempt(stats, e, w)).await,
        _ => eprintln!("warning: {device}: {e}"),
    }
}

/// Send a status line to the printer, if there is one.
async fn status(tx: &mpsc::Sender<Msg>, device: &str, line: Option<String>) {
    let Some(line) = line else {
        return;
    };
    // Fails only if the printer has already returned.
    if tx.send(Msg::Status(device.to_owned(), line)).await.is_err() {
        tracing::debug!(device, "printer gone before a status line");
    }
}

/// The first subscription, and the device's key for event ids, read on the
/// same connection. With `--reconnect`, failures are retried with backoff
/// until it works or the command stops; in human mode the first failure
/// and the eventual success are status lines.
async fn subscribe(
    t: &Target,
    (reconnect, wording): (bool, Option<Wording>),
    config: zkteco_client::ClientConfig,
    opts: CaptureOptions,
    tx: &mpsc::Sender<Msg>,
    stop: &mut watch::Receiver<bool>,
) -> Option<(LiveCapture, DeviceKey)> {
    let policy = ReconnectPolicy::default();
    let started = Instant::now();
    // Bounded by `--reconnect` (one attempt without it) and by the stop
    // signal; with it, retries go on until either succeeds.
    for failures in 0u32.. {
        let attempt = tokio::select! {
            r = connect(t, config.clone(), opts.clone()) => r,
            _ = stop.changed() => {
                ended(tx, &t.name, Exit::Ok).await;
                return None;
            }
        };
        let e = match attempt {
            Ok(subscribed) => {
                if failures > 0 && wording.is_some() {
                    status(tx, &t.name, Some(live_status::connected_after(started.elapsed()))).await;
                }
                return Some(subscribed);
            }
            Err(e) => e,
        };
        let shown = e.to_string();
        let e = anyhow::Error::from(e).context(format!("subscribing to {}", t.name));
        let fatal = classify(&e) == Exit::Auth;
        match wording {
            Some(w) if reconnect && !fatal => {
                status(tx, &t.name, live_status::connect_failed(failures, &shown, w)).await;
            }
            _ => eprintln!("error: {e:#}"),
        }
        if !reconnect || fatal {
            ended(tx, &t.name, classify(&e)).await;
            return None;
        }
        tokio::select! {
            () = tokio::time::sleep(policy.backoff(failures)) => {}
            _ = stop.changed() => {
                ended(tx, &t.name, Exit::Ok).await;
                return None;
            }
        }
    }
    None
}

/// Connect, read the device key, subscribe.
async fn connect(
    t: &Target,
    config: zkteco_client::ClientConfig,
    opts: CaptureOptions,
) -> zkteco_client::Result<(LiveCapture, DeviceKey)> {
    let mut client = Client::connect(config).await?;
    let key = DeviceKey::read(&mut client, &t.name).await;
    Ok((client.live_capture(opts).await?, key))
}

/// Unsubscribe, pass on events that arrived meanwhile, disconnect.
async fn finish(t: &Target, key: &DeviceKey, capture: LiveCapture, tx: &mpsc::Sender<Msg>, outcome: Exit) {
    let stats = capture.stats();
    let (mut client, result) = capture.stop().await;
    if let Err(e) = result {
        eprintln!("warning: {}: unsubscribing: {e}", t.name);
    }
    for captured in client.take_pending_captured() {
        if tx
            .send(Msg::Event(t.name.clone(), key.clone(), captured))
            .await
            .is_err()
        {
            break;
        }
    }
    if let Err(e) = client.disconnect().await {
        tracing::debug!(%e, device = t.name, "disconnect");
    }
    tracing::info!(device = t.name, ?stats, "capture stopped");
    ended(tx, &t.name, outcome).await;
}

/// Report a device task's outcome to the printer.
async fn ended(tx: &mpsc::Sender<Msg>, device: &str, outcome: Exit) {
    // Fails only if the printer has already returned, after which no
    // outcome matters.
    if tx.send(Msg::Ended(device.to_owned(), outcome)).await.is_err() {
        tracing::debug!(device, "printer gone before the device ended");
    }
}
