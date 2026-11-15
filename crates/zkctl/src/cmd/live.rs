//! `zkctl live`: stream events from one or more devices until Ctrl-C.
//!
//! One task per device owns a `LiveCapture`; events reach a single printer
//! through a channel, so lines from different devices never interleave.
//! Every connection is read-only with event subscription allowed, and the
//! capture skips the preparation commands (cancel capture, verify mode),
//! which a read-only client may not send.

use anyhow::Result;
use tokio::sync::{mpsc, watch};
use zkteco_client::{CaptureOptions, CapturedEvent, LiveCapture, ReconnectPolicy};

use crate::cli::{Format, Global, LiveArgs};
use crate::exit::{classify, Exit, Fail};
use crate::output::{self, EventBody, EventOut};
use crate::target::{self, Access, Target};

/// Events buffered between the device tasks and the printer.
const CHANNEL: usize = 4096;

enum Msg {
    Event(String, CapturedEvent),
    /// A device task finished, with its outcome.
    Ended(String, Exit),
}

pub(super) async fn run(g: &Global, args: &LiveArgs) -> Result<Exit> {
    if g.output == Format::Json {
        return Err(Fail::usage("live is a stream: use --output ndjson (or human)"));
    }
    let targets = target::resolve(g)?;
    let (tx, rx) = mpsc::channel(CHANNEL);
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    for t in targets {
        tasks.spawn(device(t, args.clone(), tx.clone(), stop_rx.clone()));
    }
    drop(tx);
    let exit = print_until_done(g.output, args, rx, &stop_tx).await?;
    while let Some(joined) = tasks.join_next().await {
        if let Err(e) = joined {
            tracing::error!(%e, "device task panicked");
        }
    }
    Ok(exit)
}

/// Print events until every device task has ended. Ctrl-C, `--count` and
/// `--duration` ask the tasks to stop; they then unsubscribe and end.
async fn print_until_done(
    format: Format,
    args: &LiveArgs,
    mut rx: mpsc::Receiver<Msg>,
    stop: &watch::Sender<bool>,
) -> Result<Exit> {
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
    let deadline = tokio::time::sleep(args.duration.unwrap_or(std::time::Duration::MAX));
    let mut deadline = std::pin::pin!(deadline);
    let limited = args.duration.is_some();
    let (mut printed, mut stopping, mut exit) = (0u64, false, Exit::Ok);
    // Ends when every device task has dropped its sender.
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                None => return Ok(exit),
                Some(Msg::Ended(name, outcome)) => {
                    tracing::info!(device = name, ?outcome, "device stream ended");
                    exit = exit.worst(outcome);
                }
                Some(Msg::Event(name, event)) => {
                    if args.count.is_some_and(|n| printed >= n) {
                        continue;
                    }
                    print(format, &name, &event)?;
                    printed += 1;
                    if args.count.is_some_and(|n| printed >= n) && !stopping {
                        stopping = true;
                        stop.send_replace(true);
                    }
                }
            },
            result = &mut ctrl_c, if !stopping => {
                result?;
                eprintln!("stopping (unsubscribing from every device)");
                stopping = true;
                stop.send_replace(true);
            }
            () = &mut deadline, if limited && !stopping => {
                stopping = true;
                stop.send_replace(true);
            }
        }
    }
}

fn print(format: Format, device: &str, event: &CapturedEvent) -> Result<()> {
    let out = EventOut::new(device, event);
    if format == Format::Ndjson {
        return output::ndjson(&out);
    }
    let what = match &out.body {
        EventBody::Attendance(p) => format!(
            "user {} (uid {}) {} {}",
            p.user_id.as_deref().unwrap_or("?"),
            p.uid.map_or_else(|| "?".to_owned(), |u| u.to_string()),
            p.status,
            p.punch
        ),
        EventBody::Raw { flags, payload_hex } => format!("event flags {flags:#x} payload {payload_hex}"),
    };
    let when = out.device_time.as_deref().unwrap_or("-");
    output::text(&format!("{when}  {device}  {:<8}  {what}", out.source))
}

fn options(args: &LiveArgs) -> CaptureOptions {
    CaptureOptions {
        prepare: false,
        resolve_users: args.with_users,
        reconnect: args.reconnect.then_some(ReconnectPolicy {
            max_attempts: u32::MAX,
            ..ReconnectPolicy::default()
        }),
        backfill: args.backfill,
        ..CaptureOptions::default()
    }
}

/// One device: connect (retrying with `--reconnect`), stream until asked to
/// stop or the stream ends, unsubscribe, and pass on any event still
/// queued.
async fn device(t: Target, args: LiveArgs, tx: mpsc::Sender<Msg>, mut stop: watch::Receiver<bool>) {
    let config = target::config_for(&t, Access::Live).await;
    let opts = options(&args);
    let Some(mut capture) = subscribe(&t, &args, config, opts, &tx, &mut stop).await else {
        return;
    };
    tracing::info!(device = t.name, "subscribed");
    let mut outcome = Exit::Ok;
    // Ends when asked to stop, when the stream ends, or when the printer
    // has gone.
    loop {
        tokio::select! {
            item = capture.next() => match item {
                Some(Ok(event)) => {
                    if tx.send(Msg::Event(t.name.clone(), event)).await.is_err() {
                        break;
                    }
                }
                Some(Err(e)) => {
                    eprintln!("warning: {}: {e}", t.name);
                    outcome = classify(&anyhow::Error::from(e));
                }
                None => {
                    eprintln!("error: {}: stream ended", t.name);
                    outcome = outcome.worst(Exit::Connection);
                    break;
                }
            },
            _ = stop.changed() => {
                outcome = Exit::Ok;
                break;
            }
        }
    }
    finish(&t, capture, &tx, outcome).await;
}

/// The first subscription. With `--reconnect`, failures are retried with
/// backoff until it works or the command stops.
async fn subscribe(
    t: &Target,
    args: &LiveArgs,
    config: zkteco_client::ClientConfig,
    opts: CaptureOptions,
    tx: &mpsc::Sender<Msg>,
    stop: &mut watch::Receiver<bool>,
) -> Option<LiveCapture> {
    let policy = ReconnectPolicy::default();
    // Bounded by `--reconnect` (one attempt without it) and by the stop
    // signal; with it, retries go on until either succeeds.
    for failures in 0u32.. {
        let attempt = tokio::select! {
            r = LiveCapture::connect(config.clone(), opts.clone()) => r,
            _ = stop.changed() => {
                ended(tx, &t.name, Exit::Ok).await;
                return None;
            }
        };
        match attempt {
            Ok(capture) => return Some(capture),
            Err(e) => {
                let e = anyhow::Error::from(e).context(format!("subscribing to {}", t.name));
                eprintln!("error: {e:#}");
                let fatal = classify(&e) == Exit::Auth;
                if !args.reconnect || fatal {
                    ended(tx, &t.name, classify(&e)).await;
                    return None;
                }
            }
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

/// Unsubscribe, pass on events that arrived meanwhile, disconnect.
async fn finish(t: &Target, capture: LiveCapture, tx: &mpsc::Sender<Msg>, outcome: Exit) {
    let stats = capture.stats();
    let (mut client, result) = capture.stop().await;
    if let Err(e) = result {
        eprintln!("warning: {}: unsubscribing: {e}", t.name);
    }
    for captured in client.take_pending_captured() {
        if tx.send(Msg::Event(t.name.clone(), captured)).await.is_err() {
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
