//! `zkctl live` end to end: the real binary, streaming from mock devices.

#[path = "../../zkteco-client/tests/support/mod.rs"]
mod support;

use std::collections::BTreeSet;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use support::{event_with, DeviceState, MockDevice};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command as Process};
use zkctl::event_id::{self, DeviceKey};
use zkteco_client::READ_ONLY_COMMANDS;
use zkteco_proto::records::attendance::AttendanceFormat;
use zkteco_proto::{Command, EventFlags, UserId};

fn zkctl(config: &std::path::Path, args: &[String]) -> Child {
    zkctl_with_env(config, args, &[])
}

fn zkctl_with_env(config: &std::path::Path, args: &[String], env: &[(&str, &str)]) -> Child {
    Process::new(env!("CARGO_BIN_EXE_zkctl"))
        .args(args)
        .envs(env.iter().copied())
        .env("ZKCTL_CONFIG", config)
        .env("ZKCTL_NO_KEYRING", "1")
        .env_remove("ZK_PASSWORD")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

fn host(dev: &MockDevice) -> Vec<String> {
    [
        "--host",
        &dev.addr.ip().to_string(),
        "--port",
        &dev.addr.port().to_string(),
        "--transport",
        "tcp",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

/// Wait until `dev` has received `n` subscription requests.
async fn subscribed(dev: &MockDevice, n: usize) {
    for _ in 0..1000 {
        if dev.state().event_masks.len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{} subscriptions, wanted {n}", dev.state().event_masks.len());
}

/// Wait for the process and return its stdout lines as JSON, and its code.
async fn finish(child: Child) -> (Vec<Value>, i32, String) {
    let (stdout, code, stderr) = finish_text(child).await;
    let lines = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    (lines, code, stderr)
}

/// Wait for the process and return its stdout, its code and its stderr.
async fn finish_text(child: Child) -> (String, i32, String) {
    let out = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("zkctl live finished in time")
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    (stdout, out.status.code().unwrap_or(-1), stderr)
}

/// Stderr of a human-mode run at default verbosity: status lines (local
/// time, then the device) and the `--count` report; no logs or warnings.
fn assert_no_logs(stderr: &str) {
    for line in stderr.lines() {
        let status = line.len() > 19
            && line.as_bytes()[..19]
                .iter()
                .all(|b| b.is_ascii_digit() || b"-T:".contains(b))
            && line[19..].starts_with("  127.0.0.1  ");
        assert!(
            status || line.starts_with("stopped after "),
            "unexpected on stderr: {stderr}"
        );
    }
}

/// Stderr with the host's clock masked: status lines start with the local
/// time, and say how long a reconnect took.
fn mask_clock(stderr: &str) -> String {
    let mut masked = String::new();
    for l in stderr.lines() {
        let rest = match l.get(19..) {
            Some(rest) if rest.starts_with("  ") => {
                masked.push_str("[local time]");
                rest
            }
            _ => l,
        };
        match rest.split_once(" after ") {
            Some((head, tail)) if tail.contains(" s") => {
                let tail = tail.split_once(" s").map_or(tail, |(_, t)| t);
                masked.push_str(head);
                masked.push_str(" after [n] s");
                masked.push_str(tail);
            }
            _ => masked.push_str(rest),
        }
        masked.push('\n');
    }
    masked
}

/// Fail unless `event` matches docs/schema/event-v1.json.
fn assert_event_schema(event: &Value) {
    let schema: Value = serde_json::from_str(include_str!("../../../docs/schema/event-v1.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).expect("the schema is valid");
    if let Err(e) = validator.validate(event) {
        panic!("{e}: {event}");
    }
}

/// The next stdout line, as JSON.
async fn next_line(reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) -> Value {
    let line = tokio::time::timeout(Duration::from_secs(10), reader.next_line())
        .await
        .expect("a line in time")
        .unwrap()
        .expect("stdout still open");
    serde_json::from_str(&line).unwrap()
}

/// `(user_id, source)` of each event.
fn punches_of(lines: &[Value]) -> Vec<(String, String)> {
    lines
        .iter()
        .map(|l| {
            (
                l["user_id"].as_str().unwrap().to_owned(),
                l["source"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Only read-only commands, plus the subscription itself.
fn assert_live_read_only(dev: &MockDevice) {
    for c in dev.commands() {
        assert!(
            READ_ONLY_COMMANDS.contains(&c) || c == Command::RegEvent,
            "{c:?} sent by live"
        );
    }
}

#[tokio::test]
async fn streams_ndjson_and_unsubscribes() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut args = strings(&["live", "--output", "ndjson", "--count", "3"]);
    args.extend(host(&dev));
    let child = zkctl(&dir.path().join("c.toml"), &args);
    subscribed(&dev, 1).await;
    dev.push_events(3);
    let (mut lines, code, stderr) = finish(child).await;
    assert_eq!(code, 0, "{stderr}");
    lines.iter().for_each(assert_event_schema);
    for l in &mut lines {
        assert!(l["received_at"].as_str().unwrap().ends_with('Z'));
        l["received_at"] = "[received_at]".into();
        l["device"] = "[host]".into();
    }
    insta::assert_json_snapshot!("live_ndjson", lines);
    assert_eq!(dev.state().event_masks, [1, 0], "unsubscribed on exit");
    assert_live_read_only(&dev);
}

#[tokio::test]
async fn json_output_is_refused() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut args = strings(&["live", "--output", "json"]);
    args.extend(host(&dev));
    let (_, code, _) = finish(zkctl(&dir.path().join("c.toml"), &args)).await;
    assert_eq!(code, 2);
}

#[tokio::test]
async fn duration_ends_the_stream() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut args = strings(&["live", "--output", "ndjson", "--duration", "300ms"]);
    args.extend(host(&dev));
    let (lines, code, stderr) = finish(zkctl(&dir.path().join("c.toml"), &args)).await;
    assert_eq!((lines.len(), code), (0, 0), "{stderr}");
    assert_eq!(dev.state().event_masks, [1, 0]);
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_stops_cleanly() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut args = strings(&["live", "--output", "ndjson"]);
    args.extend(host(&dev));
    let mut child = zkctl(&dir.path().join("c.toml"), &args);
    subscribed(&dev, 1).await;
    dev.push_events(1);
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();
    let first = tokio::time::timeout(Duration::from_secs(5), reader.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(first.contains("\"user_id\":\"1\""), "{first}");
    let pid = child.id().unwrap().to_string();
    let status = std::process::Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .unwrap();
    assert!(status.success());
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(dev.state().event_masks, [1, 0]);
}

/// Two devices merged into one stream; one drops and reconnects, and the
/// punches it stored meanwhile are backfilled. Every punch appears exactly
/// once, tagged with its device.
#[tokio::test]
async fn merges_devices_across_a_reconnect() {
    let run = merge_across_reconnect(false).await;
    if let Err(e) = &run.verdict {
        panic!("{e}\n{}", run.timeline());
    }
}

/// Loops [`merges_devices_across_a_reconnect`] under CPU load until it
/// fails or `STRESS_RUNS` (default 500) runs pass. On failure, the run's
/// timeline goes to a file under `CARGO_TARGET_TMPDIR`. `STRESS_LOAD` sets
/// the busy threads (default: one per CPU); `STRESS_DEBUG_LOGS=0` runs
/// zkctl without debug logs.
#[tokio::test]
#[ignore = "stress harness: cargo test -p zkctl --test live stress_ -- --ignored --nocapture"]
async fn stress_merges_devices_across_a_reconnect() {
    let env = |name: &str| std::env::var(name).ok();
    let runs: u32 = env("STRESS_RUNS").map_or(500, |v| v.parse().expect("STRESS_RUNS"));
    let load: usize = env("STRESS_LOAD").map_or_else(
        || std::thread::available_parallelism().map_or(4, std::num::NonZero::get),
        |v| v.parse().expect("STRESS_LOAD"),
    );
    let debug_logs = env("STRESS_DEBUG_LOGS").as_deref() != Some("0");
    let stop = Arc::new(AtomicBool::new(false));
    let busy: Vec<_> = (0..load)
        .map(|_| {
            let stop = Arc::clone(&stop);
            // Bounded by the harness: spins until `stop` is set below.
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::hint::black_box((0..10_000u64).sum::<u64>());
                }
            })
        })
        .collect();
    eprintln!("stress: {runs} runs, {load} busy threads, debug logs {debug_logs}");
    let mut failure = None;
    for n in 1..=runs {
        let run = merge_across_reconnect(debug_logs).await;
        if let Err(e) = &run.verdict {
            let path =
                std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("merge-failure-{n}.log"));
            std::fs::write(&path, format!("run {n}: {e}\n\n{}", run.timeline())).unwrap();
            failure = Some(format!("run {n} failed: {e}; timeline in {}", path.display()));
            break;
        }
        if n % 25 == 0 {
            eprintln!("stress: {n} runs passed");
        }
    }
    stop.store(true, Ordering::Relaxed);
    for t in busy {
        t.join().expect("busy thread");
    }
    if let Some(f) = failure {
        panic!("{f}");
    }
}

/// What one run of the merge scenario saw, on one clock.
struct MergeRun {
    start: Instant,
    /// The test's own steps and zkctl's output lines, stamped on arrival.
    notes: Vec<(Instant, String)>,
    /// Each mock device's trace.
    devices: Vec<(&'static str, Vec<(Instant, String)>)>,
    verdict: Result<(), String>,
}

impl MergeRun {
    /// Everything, in time order, as offsets from the start.
    fn timeline(&self) -> String {
        let mut all: Vec<(Instant, String)> = self.notes.clone();
        for (name, trace) in &self.devices {
            all.extend(
                trace
                    .iter()
                    .map(|(at, what)| (*at, format!("mock {name}: {what}"))),
            );
        }
        all.sort_by_key(|(at, _)| *at);
        all.iter()
            .map(|(at, what)| {
                let offset = at.saturating_duration_since(self.start).as_secs_f64();
                format!("{offset:9.4}s  {what}\n")
            })
            .collect::<Vec<_>>()
            .concat()
    }
}

type Notes = Arc<Mutex<Vec<(Instant, String)>>>;

fn note(notes: &Notes, what: impl Into<String>) {
    notes.lock().unwrap().push((Instant::now(), what.into()));
}

/// Collect `stream`'s lines into `notes`, prefixed, and return them.
fn collect_lines(
    stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    prefix: &'static str,
    notes: Notes,
) -> tokio::task::JoinHandle<Vec<String>> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        let mut seen = Vec::new();
        // Bounded by the child: ends at end of stream.
        while let Ok(Some(line)) = lines.next_line().await {
            note(&notes, format!("{prefix}: {line}"));
            seen.push(line);
        }
        seen
    })
}

/// The scenario of [`merges_devices_across_a_reconnect`], recording a
/// timeline instead of panicking.
async fn merge_across_reconnect(debug_logs: bool) -> MergeRun {
    let front = MockDevice::start(DeviceState::default()).await;
    let back = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("c.toml");
    let profile = |name: &str, dev: &MockDevice| {
        format!(
            "[devices.{name}]\nhost = \"{}\"\nport = {}\ntransport = \"tcp\"\ntimeout = \"500ms\"\n",
            dev.addr.ip(),
            dev.addr.port()
        )
    };
    std::fs::write(&config, profile("front", &front) + "\n" + &profile("back", &back)).unwrap();
    let args = strings(&[
        "live",
        "-d",
        "front",
        "-d",
        "back",
        "--reconnect",
        "--backfill",
        "--output",
        "ndjson",
        "--count",
        "7",
        // A backstop: a missing event then fails the checks below,
        // showing what arrived, instead of timing out.
        "--duration",
        "10s",
    ]);
    let start = Instant::now();
    let notes: Notes = Arc::default();
    let logs: &[(&str, &str)] = if debug_logs {
        &[("RUST_LOG", "zkteco_client=debug,zkctl=debug")]
    } else {
        &[]
    };
    let mut child = zkctl_with_env(&config, &args, logs);
    let stdout = collect_lines(child.stdout.take().unwrap(), "zkctl stdout", Arc::clone(&notes));
    let stderr = collect_lines(child.stderr.take().unwrap(), "zkctl stderr", Arc::clone(&notes));
    note(&notes, "test: zkctl started");
    subscribed(&front, 1).await;
    note(&notes, "test: front subscribed (RegEvent seen)");
    subscribed(&back, 1).await;
    note(&notes, "test: back subscribed (RegEvent seen)");
    front.push_events(2);
    back.push_events(2);
    note(&notes, "test: pushed front 1-2, back 1-2");
    // Let both live events reach the device's client before cutting it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    back.drop_connection();
    back.record_offline(2);
    note(&notes, "test: dropped back; stored back 3-4 offline");
    subscribed(&back, 2).await;
    note(&notes, "test: back resubscribed (second RegEvent seen)");
    back.push_events(1);
    note(&notes, "test: pushed back 5");
    let exit = tokio::time::timeout(Duration::from_secs(20), child.wait()).await;
    let code = if let Ok(status) = exit {
        status.unwrap().code().unwrap_or(-1)
    } else {
        child.start_kill().unwrap();
        -2
    };
    note(&notes, format!("test: zkctl exited, code {code}"));
    let lines = stdout.await.unwrap();
    let errors = stderr.await.unwrap();
    let verdict = check_merge(&lines, code, &errors);
    let devices = vec![
        ("front", front.state().trace.clone()),
        ("back", back.state().trace.clone()),
    ];
    let notes = std::mem::take(&mut *notes.lock().unwrap());
    MergeRun {
        start,
        notes,
        devices,
        verdict,
    }
}

/// Every punch exactly once, tagged with its device; the punches stored
/// while `back` was offline as `backfill`.
fn check_merge(lines: &[String], code: i32, stderr: &[String]) -> Result<(), String> {
    if code != 0 {
        return Err(format!("exit code {code}: {stderr:?}"));
    }
    let mut seen = Vec::new();
    for l in lines {
        let v: Value = serde_json::from_str(l).map_err(|e| format!("{e}: {l}"))?;
        let field = |k: &str| v[k].as_str().map(str::to_owned).ok_or(format!("no {k}: {l}"));
        seen.push((field("device")?, field("user_id")?, field("source")?));
    }
    let unique: BTreeSet<(&str, &str)> = seen.iter().map(|(d, u, _)| (d.as_str(), u.as_str())).collect();
    let expected: BTreeSet<(&str, &str)> = [
        ("front", "1"),
        ("front", "2"),
        ("back", "1"),
        ("back", "2"),
        ("back", "3"),
        ("back", "4"),
        ("back", "5"),
    ]
    .into();
    if seen.len() != 7 || unique != expected {
        return Err(format!("wanted each of {expected:?} once, got {seen:?}"));
    }
    for (d, u, s) in &seen {
        let want = match (d.as_str(), u.as_str()) {
            ("back", "3" | "4") => "backfill",
            ("back", "5") => continue,
            _ => "live",
        };
        if s != want {
            return Err(format!("{d} {u} came as {s}, wanted {want}: {seen:?}"));
        }
    }
    Ok(())
}

#[tokio::test]
async fn raw_events_match_the_schema() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut args = strings(&["live", "--output", "ndjson", "--count", "1"]);
    args.extend(host(&dev));
    let child = zkctl(&dir.path().join("c.toml"), &args);
    subscribed(&dev, 1).await;
    dev.push_packets(vec![event_with(EventFlags::ALARM, &[55, 0, 0, 0])]);
    let (lines, code, stderr) = finish(child).await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(lines[0]["kind"], "raw");
    assert_eq!(lines[0]["raw"]["payload_hex"], "37000000");
    assert_event_schema(&lines[0]);
}

#[tokio::test]
async fn count_reports_events_left_unemitted() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut args = strings(&["live", "--output", "ndjson", "--count", "3"]);
    args.extend(host(&dev));
    let child = zkctl(&dir.path().join("c.toml"), &args);
    subscribed(&dev, 1).await;
    dev.push_events(5);
    let (lines, code, stderr) = finish(child).await;
    assert_eq!((lines.len(), code), (3, 0), "{stderr}");
    assert!(
        stderr.contains(
            "stopped after 3 events; 2 further events arrived while unsubscribing and were not emitted"
        ),
        "{stderr}"
    );
}

/// `live --state` arguments for `dev`, stopping after `count` events if
/// given.
fn stateful(dev: &MockDevice, state: &std::path::Path, count: Option<&str>) -> Vec<String> {
    let mut args = strings(&[
        "live",
        "--reconnect",
        "--backfill",
        "--state",
        state.to_str().unwrap(),
        "--state-flush",
        "0",
        "--output",
        "ndjson",
    ]);
    if let Some(n) = count {
        args.extend(strings(&["--count", n]));
    }
    args.extend(host(dev));
    args
}

/// Wait until the state file lists `n` delivered punches.
async fn state_holds(path: &std::path::Path, n: usize) {
    for _ in 0..500 {
        let held = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| {
                let devices = v["devices"].as_object()?.clone();
                Some(
                    devices
                        .values()
                        .map(|d| d["delivered"].as_array().map_or(0, Vec::len))
                        .sum::<usize>(),
                )
            });
        if held == Some(n) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "state never held {n} punches: {:?}",
        std::fs::read_to_string(path)
    );
}

/// Killed mid-stream (no chance to save or unsubscribe), restarted after
/// punches were stored meanwhile: every punch is emitted exactly once
/// across the two runs, the missed ones as backfill.
#[tokio::test]
async fn restart_after_a_kill_emits_each_punch_once() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (config, state) = (dir.path().join("c.toml"), dir.path().join("live.json"));

    let mut first = zkctl(&config, &stateful(&dev, &state, None));
    subscribed(&dev, 1).await;
    dev.push_events(2);
    let mut reader = BufReader::new(first.stdout.take().unwrap()).lines();
    let mut before = vec![next_line(&mut reader).await, next_line(&mut reader).await];
    state_holds(&state, 2).await;
    first.start_kill().unwrap();
    first.wait().await.unwrap();
    assert_eq!(dev.state().event_masks, [1], "killed without unsubscribing");

    dev.record_offline(3);
    let second = zkctl(&config, &stateful(&dev, &state, Some("4")));
    subscribed(&dev, 2).await;
    dev.push_events(1);
    let (after, code, stderr) = finish(second).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("warning"), "{stderr}");

    assert_eq!(
        punches_of(&before),
        [("1".into(), "live".into()), ("2".into(), "live".into())]
    );
    let after_punches = punches_of(&after);
    let missed: Vec<_> = after_punches.iter().take(3).cloned().collect();
    let backfill = |u: &str| (u.to_owned(), "backfill".to_owned());
    assert_eq!(missed, [backfill("3"), backfill("4"), backfill("5")]);
    assert_eq!(
        after_punches[3].0, "6",
        "then the live punch (live or backfill, as it raced the log read)"
    );
    before.extend(after);
    let users: BTreeSet<String> = punches_of(&before).into_iter().map(|(u, _)| u).collect();
    assert_eq!((before.len(), users.len()), (6, 6), "each punch exactly once");
    state_holds(&state, 6).await;
}

#[tokio::test]
async fn a_corrupt_state_file_is_replaced() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (config, state) = (dir.path().join("c.toml"), dir.path().join("live.json"));
    std::fs::write(&state, "{ not json").unwrap();
    let child = zkctl(&config, &stateful(&dev, &state, Some("1")));
    subscribed(&dev, 1).await;
    dev.push_events(1);
    let (lines, code, stderr) = finish(child).await;
    assert_eq!((lines.len(), code), (1, 0), "{stderr}");
    assert!(
        stderr.contains("is corrupt") && stderr.contains("starting fresh"),
        "{stderr}"
    );
    state_holds(&state, 1).await;
}

/// One punch reaching zkctl three ways (live, backfill from a resumed
/// state, and `attendance pull`) carries one `id`, while the live event has
/// no uid and the log record has one.
#[tokio::test]
async fn the_same_punch_has_one_id_however_it_arrives() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (config, state) = (dir.path().join("c.toml"), dir.path().join("live.json"));

    let mut args = strings(&["live", "--output", "ndjson", "--count", "1"]);
    args.extend(host(&dev));
    let child = zkctl(&config, &args);
    subscribed(&dev, 1).await;
    dev.push_events(1);
    let (live, code, stderr) = finish(child).await;
    assert_eq!(code, 0, "{stderr}");

    // A state from before the punch, so the restart backfills it.
    let at = serde_json::to_value(dev.state().time).unwrap();
    let device = dev.addr.ip().to_string();
    let resume = serde_json::json!({"v": 1, "devices": {device: {"device_time": at, "delivered": []}}});
    std::fs::write(&state, resume.to_string()).unwrap();
    let child = zkctl(&config, &stateful(&dev, &state, Some("1")));
    let (backfill, code, stderr) = finish(child).await;
    assert_eq!(code, 0, "{stderr}");

    let mut args = strings(&["attendance", "pull", "--output", "ndjson"]);
    args.extend(host(&dev));
    let (pulled, code, stderr) = finish(zkctl(&config, &args)).await;
    assert_eq!(code, 0, "{stderr}");
    let pulled: Vec<&Value> = pulled.iter().filter(|r| r["user_id"] == "1").collect();

    let (l, b, p) = (&live[0], &backfill[0], pulled[0]);
    assert_eq!(
        [&l["source"], &b["source"], &p["source"]],
        [&Value::from("live"), &"backfill".into(), &"pull".into()]
    );
    assert_eq!(
        (&l["uid"], &p["uid"]),
        (&Value::Null, &Value::from(1)),
        "the shapes differ"
    );
    assert_eq!(l["id"], b["id"]);
    assert_eq!(l["id"], p["id"]);
    for e in [l, b, p] {
        assert_eq!(e["user_id_source"], "record");
        assert_event_schema(e);
    }
}

/// A config file with one profile, `name`, for `dev`.
fn profile_config(dir: &std::path::Path, name: &str, dev: &MockDevice) -> std::path::PathBuf {
    let config = dir.join("c.toml");
    let text = format!(
        "[devices.{name}]\nhost = \"{}\"\nport = {}\ntransport = \"tcp\"\ntimeout = \"500ms\"\n",
        dev.addr.ip(),
        dev.addr.port()
    );
    std::fs::write(&config, text).unwrap();
    config
}

/// On a log without user IDs (the 8-byte layout), a plain `attendance
/// pull` and `live` give the same punch the same `id`: pull resolves the
/// user ID for the id, though it shows it only with `--with-users`.
#[tokio::test]
async fn pull_and_live_agree_on_a_log_without_user_ids() {
    let mut state = DeviceState {
        attendance_format: AttendanceFormat::Short8,
        ..DeviceState::default()
    };
    state.events_sent = 1;
    let punch = state.last_event_record();
    state.events_sent = 0;
    state.attendance = vec![punch.clone()];
    state.users = support::users(1);
    state.users[0].user_id = punch.user_id.clone().unwrap();
    let dev = MockDevice::start(state).await;
    let dir = tempfile::tempdir().unwrap();
    let config = profile_config(dir.path(), "front", &dev);

    let pull = strings(&["attendance", "pull", "-d", "front", "--output", "ndjson"]);
    let (pulled, code, stderr) = finish(zkctl(&config, &pull)).await;
    assert_eq!((pulled.len(), code), (1, 0), "{stderr}");
    assert_eq!(
        (&pulled[0]["user_id"], &pulled[0]["user_id_source"]),
        (&Value::Null, &Value::Null),
        "shown only with --with-users"
    );

    let live = strings(&["live", "-d", "front", "--output", "ndjson", "--count", "1"]);
    let child = zkctl(&config, &live);
    subscribed(&dev, 1).await;
    dev.push_events(1);
    let (lines, code, stderr) = finish(child).await;
    assert_eq!((lines.len(), code), (1, 0), "{stderr}");
    assert_eq!(lines[0]["user_id"], "1");
    assert_eq!(lines[0]["device_time"], pulled[0]["device_time"]);
    assert_eq!(lines[0]["id"], pulled[0]["id"]);
    let key = DeviceKey::serial("MOCK0000000001");
    assert_eq!(lines[0]["id"], event_id::attendance(&key, &punch));
}

/// The serial number cannot be read (an error, or an empty value): the
/// key falls back to `profile:<name>` with a warning, and the stream goes
/// on.
#[tokio::test]
async fn live_ids_fall_back_to_the_profile_name() {
    for empty in [false, true] {
        let dev = MockDevice::start(DeviceState::default()).await;
        {
            let mut state = dev.state();
            state.faults.serial_number_error = !empty;
            state.faults.empty_serial_number = empty;
        }
        let dir = tempfile::tempdir().unwrap();
        let config = profile_config(dir.path(), "front", &dev);
        let args = strings(&["live", "-d", "front", "--output", "ndjson", "--count", "2"]);
        let child = zkctl(&config, &args);
        subscribed(&dev, 1).await;
        dev.push_events(2);
        let (lines, code, stderr) = finish(child).await;
        assert_eq!((lines.len(), code), (2, 0), "empty: {empty}: {stderr}");
        let key = DeviceKey::profile("front");
        for (n, line) in (1u32..).zip(&lines) {
            let mut punch = dev.state().last_event_record();
            punch.user_id = Some(UserId(n.to_string()));
            assert_eq!(line["id"], event_id::attendance(&key, &punch), "empty: {empty}");
        }
        assert!(
            stderr.contains("warning: front: cannot read the serial number"),
            "empty: {empty}: {stderr}"
        );
    }
}

/// Human output shows only what is known: live events in the 12-byte
/// layout carry a user ID but no uid, so no `(uid ?)`. Nothing from the
/// client's logs on stderr, and no colour when not on a terminal.
#[tokio::test]
async fn human_output_shows_known_fields_only() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut args = strings(&["live", "--count", "2"]);
    args.extend(host(&dev));
    let child = zkctl(&dir.path().join("c.toml"), &args);
    subscribed(&dev, 1).await;
    dev.push_events(2);
    let (stdout, code, stderr) = finish_text(child).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(!stdout.contains('\x1b'), "{stdout:?}");
    assert_no_logs(&stderr);
    insta::assert_snapshot!("live_human", stdout);
}

/// A lost connection is one status line on stderr, the reconnect another,
/// with the backfilled count; stdout carries only events, and the
/// client's own logs and retries stay hidden.
#[tokio::test]
async fn human_status_lines_across_a_reconnect() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut args = strings(&["live", "--reconnect", "--backfill", "--count", "3"]);
    args.extend(host(&dev));
    let mut child = zkctl(&dir.path().join("c.toml"), &args);
    let mut reader = BufReader::new(child.stdout.take().unwrap()).lines();
    subscribed(&dev, 1).await;
    // With --backfill, subscribing ends by reading the device clock;
    // dropping before that would fail the first connection instead.
    for _ in 0..1000 {
        if dev.count(Command::GetTime) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(dev.count(Command::GetTime), 1, "subscribed");
    dev.drop_connection();
    dev.record_offline(2);
    // The live punch only after the backfill, or it would be backfilled too.
    let mut lines = vec![
        next_text_line(&mut reader).await,
        next_text_line(&mut reader).await,
    ];
    dev.push_events(1);
    lines.push(next_text_line(&mut reader).await);
    let (_, code, stderr) = finish_text(child).await;
    assert_eq!(code, 0, "{stderr}");
    assert_no_logs(&stderr);
    insta::assert_snapshot!("live_human_reconnect_stdout", lines.join("\n") + "\n");
    insta::assert_snapshot!("live_human_reconnect_stderr", mask_clock(&stderr));
}

/// The next stdout line, as text.
async fn next_text_line(reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) -> String {
    tokio::time::timeout(Duration::from_secs(10), reader.next_line())
        .await
        .expect("a line in time")
        .unwrap()
        .expect("stdout still open")
}
