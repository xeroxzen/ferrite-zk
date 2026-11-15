//! `zkctl live` end to end: the real binary, streaming from mock devices.

#[path = "../../zkteco-client/tests/support/mod.rs"]
mod support;

use std::collections::BTreeSet;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use support::{event_with, DeviceState, MockDevice};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command as Process};
use zkteco_client::READ_ONLY_COMMANDS;
use zkteco_proto::{Command, EventFlags};

fn zkctl(config: &std::path::Path, args: &[String]) -> Child {
    Process::new(env!("CARGO_BIN_EXE_zkctl"))
        .args(args)
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
    let out = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("zkctl live finished in time")
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    let lines = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    (lines, out.status.code().unwrap_or(-1), stderr)
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
        // A backstop: a missing event then fails the assertions below,
        // showing what arrived, instead of timing out.
        "--duration",
        "10s",
    ]);
    let child = zkctl(&config, &args);
    subscribed(&front, 1).await;
    subscribed(&back, 1).await;
    front.push_events(2);
    back.push_events(2);
    // Let both live events reach the device's client before cutting it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    back.drop_connection();
    back.record_offline(2);
    subscribed(&back, 2).await;
    back.push_events(1);
    let (lines, code, stderr) = finish(child).await;
    assert_eq!(code, 0, "{stderr}");
    let seen: Vec<(String, String, String)> = lines
        .iter()
        .map(|l| {
            (
                l["device"].as_str().unwrap().to_owned(),
                l["user_id"].as_str().unwrap().to_owned(),
                l["source"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let unique: BTreeSet<(&str, &str)> = seen.iter().map(|(d, u, _)| (d.as_str(), u.as_str())).collect();
    assert_eq!(seen.len(), 7, "{seen:?}");
    assert_eq!(unique.len(), 7, "no duplicates: {seen:?}");
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
    assert_eq!(unique, expected);
    for (d, u, s) in &seen {
        let want = match (d.as_str(), u.as_str()) {
            ("back", "3" | "4") => "backfill",
            ("back", "5") => continue,
            _ => "live",
        };
        assert_eq!(s, want, "{d} {u}");
    }
    assert_eq!(back.state().connections, 2);
    assert_live_read_only(&front);
    assert_live_read_only(&back);
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
