//! `zkctl live` end to end: the real binary, streaming from mock devices.

#[path = "../../zkteco-client/tests/support/mod.rs"]
mod support;

use std::collections::BTreeSet;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use support::{DeviceState, MockDevice};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command as Process};
use zkteco_client::READ_ONLY_COMMANDS;
use zkteco_proto::Command;

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
