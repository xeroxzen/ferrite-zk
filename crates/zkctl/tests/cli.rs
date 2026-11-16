//! `zkctl` end to end: the real binary against the mock device.
//!
//! Each test runs the binary on a blocking thread while the mock serves
//! from the test's runtime. The keyring is off and the config file points
//! into a temporary directory, so nothing outside the test is touched.

#[path = "../../zkteco-client/tests/support/mod.rs"]
mod support;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::assert::Assert;
use serde_json::Value;
use support::{punches, templates, users, DeviceState, MockDevice};
use zkctl::event_id::{self, DeviceKey};
use zkteco_client::READ_ONLY_COMMANDS;
use zkteco_proto::records::attendance::AttendanceFormat;
use zkteco_proto::Command;

/// A temporary home for the config and state files.
struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("config.toml")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Write a config file naming these devices.
    fn devices(&self, devices: &[(&str, &MockDevice)]) {
        let mut text = String::new();
        for (name, dev) in devices {
            writeln!(
                text,
                "[devices.{name}]\nhost = \"{}\"\nport = {}\ntransport = \"tcp\"\ntimeout = \"500ms\"\n",
                dev.addr.ip(),
                dev.addr.port()
            )
            .unwrap();
        }
        std::fs::write(self.config(), text).unwrap();
    }

    async fn run(&self, args: &[&str]) -> Assert {
        Assert::new(self.output(args).await)
    }

    async fn output(&self, args: &[&str]) -> Output {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_zkctl"));
        cmd.args(args)
            .env("ZKCTL_CONFIG", self.config())
            .env("ZKCTL_NO_KEYRING", "1")
            .env_remove("ZK_PASSWORD")
            .env_remove("RUST_LOG")
            .stdin(std::process::Stdio::null());
        tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap()
    }
}

/// `--host` options for a mock.
fn at(dev: &MockDevice) -> Vec<String> {
    vec![
        "--host".into(),
        dev.addr.ip().to_string(),
        "--port".into(),
        dev.addr.port().to_string(),
        "--transport".into(),
        "tcp".into(),
        "--timeout".into(),
        "500ms".into(),
    ]
}

fn args<'a>(dev: &'a [String], rest: &[&'a str]) -> Vec<&'a str> {
    rest.iter()
        .copied()
        .chain(dev.iter().map(String::as_str))
        .collect()
}

fn stdout(a: &Assert) -> String {
    String::from_utf8(a.get_output().stdout.clone()).unwrap()
}

fn stderr(a: &Assert) -> String {
    String::from_utf8(a.get_output().stderr.clone()).unwrap()
}

/// Fail unless `event` matches docs/schema/event-v1.json.
fn assert_event_schema(event: &Value) {
    let schema: Value = serde_json::from_str(include_str!("../../../docs/schema/event-v1.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).expect("the schema is valid");
    if let Err(e) = validator.validate(event) {
        panic!("{e}: {event}");
    }
}

fn json(a: &Assert) -> Value {
    serde_json::from_str(&stdout(a)).unwrap_or_else(|e| panic!("{e}: {}", stdout(a)))
}

fn lines(a: &Assert) -> Vec<Value> {
    stdout(a)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Every command the device received is on the read-only allowlist (plus
/// `extra`).
fn assert_read_only(dev: &MockDevice, extra: &[Command]) {
    for c in dev.commands() {
        assert!(
            READ_ONLY_COMMANDS.contains(&c) || extra.contains(&c),
            "{c:?} sent by a read command"
        );
    }
}

/// Nothing outside the allowlist was sent: what a dry run or refused
/// command must leave.
fn assert_nothing_written(dev: &MockDevice) {
    assert_read_only(dev, &[]);
}

async fn device() -> MockDevice {
    MockDevice::start(DeviceState {
        users: users(3),
        attendance: punches(4),
        ..DeviceState::default()
    })
    .await
}

// ---- general ----

#[tokio::test]
async fn help_documents_exit_codes() {
    let env = Env::new();
    let a = env.run(&["--help"]).await.code(0);
    let help = stdout(&a);
    for code in [
        "0  success",
        "3  connection",
        "4  authentication",
        "5  the device refused a command, or a table kept changing",
        "safe to retry",
        "6  partial",
        "7  aborted",
    ] {
        assert!(help.contains(code), "{code} missing from --help");
    }
}

/// An attendance log that changes during every read: exit 5, nothing
/// printed.
#[tokio::test]
async fn changing_table_exits_5() {
    let mut state = DeviceState {
        attendance: punches(4),
        ..DeviceState::default()
    };
    state.faults.punches_after_sizes = usize::MAX;
    let dev = MockDevice::start(state).await;
    let env = Env::new();
    let host = at(&dev);
    let a = env
        .run(&args(&host, &["attendance", "pull", "--output", "ndjson"]))
        .await
        .code(5);
    assert_eq!(stdout(&a), "");
    assert!(
        stderr(&a).contains("changed during each of 3 reads"),
        "{}",
        stderr(&a)
    );
}

#[tokio::test]
async fn usage_errors_exit_2() {
    let env = Env::new();
    env.run(&["--bogus"]).await.code(2);
    let a = env.run(&["time", "get"]).await.code(2);
    assert!(stderr(&a).contains("no device"));
    let a = env.run(&["-d", "nowhere", "time", "get"]).await.code(2);
    assert!(stderr(&a).contains("nowhere"));
}

#[tokio::test]
async fn unreachable_device_exits_3() {
    let env = Env::new();
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let port = closed.port().to_string();
    env.run(&[
        "time",
        "get",
        "--host",
        "127.0.0.1",
        "--port",
        &port,
        "--transport",
        "tcp",
    ])
    .await
    .code(3);
}

#[tokio::test]
async fn wrong_password_exits_4() {
    let env = Env::new();
    let dev = MockDevice::start(DeviceState {
        password: Some(1234),
        ..DeviceState::default()
    })
    .await;
    let host = at(&dev);
    env.run(&args(&host, &["time", "get"])).await.code(4);
    env.run(&args(&host, &["time", "get", "--password", "1"]))
        .await
        .code(4);
    env.run(&args(&host, &["time", "get", "--password", "1234"]))
        .await
        .code(0);
}

#[tokio::test]
async fn device_refusal_exits_5() {
    let env = Env::new();
    let dev = device().await;
    dev.state().faults.ack_error = Some(Command::SetTime);
    let host = at(&dev);
    env.run(&args(&host, &["time", "set", "2026-03-04T05:06:07", "--yes"]))
        .await
        .code(5);
}

#[tokio::test]
async fn completions_are_printed() {
    let env = Env::new();
    let a = env.run(&["completions", "bash"]).await.code(0);
    assert!(stdout(&a).contains("zkctl"));
}

// ---- read commands ----

#[tokio::test]
async fn info_json() {
    let env = Env::new();
    let dev = device().await;
    let host = at(&dev);
    let a = env.run(&args(&host, &["info", "--output", "json"])).await.code(0);
    let mut v = json(&a);
    v["device"] = "[host]".into();
    insta::assert_json_snapshot!("info", v);
    assert_read_only(&dev, &[]);
    let a = env
        .run(&args(
            &host,
            &["info", "--include-identifiers", "--output", "json"],
        ))
        .await
        .code(0);
    assert_eq!(json(&a)["serial_number"], "MOCK0000000001");
}

#[tokio::test]
async fn time_get() {
    let env = Env::new();
    let dev = device().await;
    let host = at(&dev);
    let a = env.run(&args(&host, &["time", "get"])).await.code(0);
    assert_eq!(stdout(&a).trim(), "2026-01-02T03:04:05");
    let a = env
        .run(&args(&host, &["time", "get", "--output", "json"]))
        .await
        .code(0);
    assert_eq!(json(&a)["time"], "2026-01-02T03:04:05");
    env.run(&args(&host, &["time", "get", "--output", "ndjson"]))
        .await
        .code(2);
    assert_read_only(&dev, &[]);
}

#[tokio::test]
async fn users_list() {
    let env = Env::new();
    let dev = device().await;
    let host = at(&dev);
    let a = env
        .run(&args(&host, &["users", "list", "--output", "json"]))
        .await
        .code(0);
    insta::assert_json_snapshot!("users_list", json(&a));
    let a = env
        .run(&args(
            &host,
            &["users", "list", "--filter", "user 2", "--output", "ndjson"],
        ))
        .await
        .code(0);
    let found = lines(&a);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["user_id"], "1002");
    let a = env.run(&args(&host, &["users", "list"])).await.code(0);
    assert!(stdout(&a).starts_with("UID"), "{}", stdout(&a));
    assert_read_only(&dev, &[]);
}

#[tokio::test]
async fn attendance_pull_formats() {
    let env = Env::new();
    let dev = device().await;
    let host = at(&dev);
    let a = env
        .run(&args(&host, &["attendance", "pull", "--output", "json"]))
        .await
        .code(0);
    let mut records = json(&a);
    for r in records.as_array_mut().unwrap() {
        r["device"] = "[host]".into();
    }
    insta::assert_json_snapshot!("attendance_pull_json", records);
    let a = env
        .run(&args(&host, &["attendance", "pull", "--output", "ndjson"]))
        .await
        .code(0);
    let first = &lines(&a)[0];
    assert_eq!(first["device_time"], "2026-01-01T08:00:00");
    assert_eq!(
        (&first["v"], &first["source"], &first["received_at"]),
        (&1.into(), &"pull".into(), &Value::Null)
    );
    lines(&a).iter().for_each(assert_event_schema);
    insta::assert_snapshot!(
        "attendance_pull_ndjson",
        stdout(&a).replace(
            &format!("\"device\":\"{}\"", dev.addr.ip()),
            "\"device\":\"[host]\""
        )
    );
    assert_read_only(&dev, &[]);
}

#[tokio::test]
async fn attendance_pull_filters_by_time() {
    let env = Env::new();
    let dev = device().await;
    let host = at(&dev);
    let pull = |since: &'static str, until: &'static str| {
        let mut a = args(&host, &["attendance", "pull", "--output", "ndjson"]);
        a.extend(["--since", since, "--until", until]);
        a
    };
    let a = env
        .run(&pull("2026-01-01T08:01:00", "2026-01-01T08:03:00"))
        .await
        .code(0);
    let times: Vec<Value> = lines(&a).iter().map(|r| r["device_time"].clone()).collect();
    assert_eq!(times, ["2026-01-01T08:01:00", "2026-01-01T08:02:00"]);
    // The device clock reads 2026-01-02T03:04:05: 20 hours back is 07:04.
    let a = env.run(&pull("20h", "1s")).await.code(0);
    assert_eq!(lines(&a).len(), 4);
    let a = env.run(&pull("19h", "1s")).await.code(0);
    assert_eq!(lines(&a).len(), 0);
}

#[tokio::test]
async fn attendance_pull_state_is_incremental() {
    let env = Env::new();
    let dev = device().await;
    let host = at(&dev);
    let state = env.path("cursor.json");
    let state = state.to_str().unwrap();
    let a = env
        .run(&args(
            &host,
            &["attendance", "pull", "--state", state, "--output", "ndjson"],
        ))
        .await
        .code(0);
    assert_eq!(lines(&a).len(), 4);
    let a = env
        .run(&args(
            &host,
            &["attendance", "pull", "--state", state, "--output", "ndjson"],
        ))
        .await
        .code(0);
    assert_eq!(lines(&a).len(), 0, "nothing new");
    dev.state().attendance.extend(punches(6).into_iter().skip(4));
    let a = env
        .run(&args(
            &host,
            &["attendance", "pull", "--state", state, "--output", "ndjson"],
        ))
        .await
        .code(0);
    let new: Vec<Value> = lines(&a).iter().map(|r| r["device_time"].clone()).collect();
    assert_eq!(new, ["2026-01-01T08:04:00", "2026-01-01T08:05:00"]);
}

#[tokio::test]
async fn attendance_pull_reports_rejected_records() {
    let env = Env::new();
    let dev = device().await;
    dev.state().corrupt_attendance = vec![1];
    let a = env
        .run(&args(&at(&dev), &["attendance", "pull", "--output", "json"]))
        .await
        .code(6);
    assert_eq!(json(&a).as_array().unwrap().len(), 3);
    assert!(stderr(&a).contains("rejected"), "{}", stderr(&a));
}

/// A log without user IDs (the 8-byte layout): `--with-users` fills them in
/// from the user table and says so; without it they stay null. Either way
/// the user table is read and `id` names the person by user ID.
#[tokio::test]
async fn user_id_source_shows_where_ids_came_from() {
    let env = Env::new();
    let dev = device().await;
    dev.state().attendance_format = AttendanceFormat::Short8;
    let host = at(&dev);
    let a = env
        .run(&args(&host, &["attendance", "pull", "--output", "ndjson"]))
        .await
        .code(0);
    let plain = &lines(&a)[0];
    assert_eq!(
        (&plain["user_id"], &plain["user_id_source"]),
        (&Value::Null, &Value::Null)
    );
    let a = env
        .run(&args(
            &host,
            &["attendance", "pull", "--with-users", "--output", "ndjson"],
        ))
        .await
        .code(0);
    let resolved = &lines(&a)[0];
    assert_eq!(resolved["user_id"], "1001");
    assert_eq!(resolved["user_id_source"], "user_table");
    assert_eq!(
        plain["id"], resolved["id"],
        "the id is computed from the resolved user ID in both"
    );
    assert_event_schema(plain);
    assert_event_schema(resolved);
}

#[tokio::test]
async fn attendance_pull_from_several_devices() {
    let env = Env::new();
    let (front, back) = (device().await, device().await);
    back.state().attendance.truncate(1);
    env.devices(&[("front", &front), ("back", &back)]);
    let a = env
        .run(&[
            "attendance",
            "pull",
            "-d",
            "front",
            "-d",
            "back",
            "--with-users",
            "--output",
            "ndjson",
        ])
        .await
        .code(0);
    let devices: Vec<Value> = lines(&a).iter().map(|r| r["device"].clone()).collect();
    assert_eq!(devices, ["front", "front", "front", "front", "back"]);
    assert_eq!(lines(&a)[0]["source"], "pull");
}

#[tokio::test]
async fn one_device_down_does_not_stop_the_others() {
    let env = Env::new();
    let up = device().await;
    let down = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    std::fs::write(
        env.config(),
        format!(
            "[devices.up]\nhost = \"{}\"\nport = {}\ntransport = \"tcp\"\n\n[devices.down]\nhost = \"127.0.0.1\"\nport = {}\ntransport = \"tcp\"\n",
            up.addr.ip(),
            up.addr.port(),
            down.port()
        ),
    )
    .unwrap();
    let a = env
        .run(&[
            "attendance",
            "pull",
            "-d",
            "up",
            "-d",
            "down",
            "--output",
            "ndjson",
        ])
        .await
        .code(3);
    assert_eq!(lines(&a).len(), 4);
    assert!(stderr(&a).contains("down"));
}

// ---- destructive commands ----

#[tokio::test]
async fn destructive_commands_need_yes() {
    let env = Env::new();
    let dev = device().await;
    let host = at(&dev);
    let refused: [&[&str]; 9] = [
        &["time", "set", "--sync"],
        &["users", "add", "--user-id", "77"],
        &["users", "update", "--uid", "1", "--name", "X"],
        &["users", "delete", "--uid", "1"],
        &["attendance", "clear"],
        &["data", "clear"],
        &["restart"],
        &["poweroff"],
        &["enroll", "--user-id", "1001", "--finger", "0"],
    ];
    for cmd in refused {
        let a = env.run(&args(&host, cmd)).await.code(2);
        assert!(stderr(&a).contains("--yes"), "{cmd:?}: {}", stderr(&a));
    }
    assert_eq!(dev.state().connections, 0, "refused before connecting");
}

#[tokio::test]
async fn dry_runs_send_no_writes() {
    let env = Env::new();
    let dev = device().await;
    let host = at(&dev);
    let runs: [&[&str]; 8] = [
        &["time", "set", "2026-05-06T07:08:09"],
        &["users", "add", "--user-id", "77", "--name", "New"],
        &["users", "update", "--user-id", "1002", "--name", "Renamed"],
        &["users", "delete", "--uid", "1"],
        &["attendance", "clear"],
        &["data", "clear"],
        &["restart"],
        &["enroll", "--user-id", "1001", "--finger", "0"],
    ];
    for cmd in runs {
        let mut full = args(&host, cmd);
        full.extend(["--dry-run", "--output", "json"]);
        let a = env.run(&full).await.code(0);
        let plan = json(&a);
        assert_eq!(plan["dry_run"], true, "{cmd:?}");
        assert!(!plan["would_send"].as_array().unwrap().is_empty(), "{cmd:?}");
    }
    assert_nothing_written(&dev);
    assert_eq!(dev.state().users.len(), 3);
    assert_eq!(dev.state().attendance.len(), 4);
}

#[tokio::test]
async fn dry_run_shows_the_exact_packet() {
    let env = Env::new();
    let dev = device().await;
    let a = env
        .run(&args(
            &at(&dev),
            &[
                "users",
                "delete",
                "--user-id",
                "1002",
                "--dry-run",
                "--output",
                "json",
            ],
        ))
        .await
        .code(0);
    insta::assert_json_snapshot!("dry_run_users_delete", json(&a));
}

#[tokio::test]
async fn time_set_writes_the_clock() {
    let env = Env::new();
    let dev = device().await;
    env.run(&args(&at(&dev), &["time", "set", "2026-05-06T07:08:09", "--yes"]))
        .await
        .code(0);
    assert_eq!(dev.state().time.to_string(), "2026-05-06 07:08:09");
}

#[tokio::test]
async fn users_add_update_delete() {
    let env = Env::new();
    let dev = device().await;
    let host = at(&dev);
    env.run(&args(
        &host,
        &["users", "add", "--user-id", "2001", "--name", "Ann", "--yes"],
    ))
    .await
    .code(0);
    let added = dev
        .state()
        .users
        .iter()
        .find(|u| u.user_id.as_str() == "2001")
        .cloned()
        .unwrap();
    assert_eq!((added.uid.0, added.name.as_str()), (4, "Ann"));
    env.run(&args(&host, &["users", "add", "--user-id", "2001", "--yes"]))
        .await
        .code(2);
    env.run(&args(
        &host,
        &["users", "update", "--user-id", "2001", "--card", "99", "--yes"],
    ))
    .await
    .code(0);
    assert!(dev
        .state()
        .users
        .iter()
        .any(|u| u.user_id.as_str() == "2001" && u.card == 99 && u.name == "Ann"));
    env.run(&args(
        &host,
        &["users", "delete", "--uid", "1", "--user-id", "1001", "--yes"],
    ))
    .await
    .code(2);
    env.run(&args(&host, &["users", "delete", "--uid", "4", "--yes"]))
        .await
        .code(0);
    assert!(!dev.state().users.iter().any(|u| u.user_id.as_str() == "2001"));
}

#[tokio::test]
async fn clears_need_the_device_name() {
    let env = Env::new();
    let dev = device().await;
    env.devices(&[("lobby", &dev)]);
    let a = env
        .run(&["-d", "lobby", "attendance", "clear", "--yes"])
        .await
        .code(7);
    assert!(stderr(&a).contains("--confirm-device lobby"), "{}", stderr(&a));
    env.run(&[
        "-d",
        "lobby",
        "attendance",
        "clear",
        "--yes",
        "--confirm-device",
        "hall",
    ])
    .await
    .code(7);
    assert_eq!(dev.state().attendance.len(), 4, "nothing cleared");
    env.run(&[
        "-d",
        "lobby",
        "attendance",
        "clear",
        "--yes",
        "--confirm-device",
        "lobby",
    ])
    .await
    .code(0);
    assert_eq!(dev.state().attendance, Vec::new());
    env.run(&[
        "-d",
        "lobby",
        "data",
        "clear",
        "--yes",
        "--confirm-device",
        "lobby",
    ])
    .await
    .code(0);
    assert_eq!(dev.state().users, Vec::new());
}

#[tokio::test]
async fn restart_is_sent() {
    let env = Env::new();
    let dev = device().await;
    env.run(&args(&at(&dev), &["restart", "--yes"])).await.code(0);
    assert_eq!(dev.count(Command::Restart), 1);
}

#[tokio::test]
async fn enroll_shows_progress() {
    let env = Env::new();
    let dev = device().await;
    let code = |c: u16| (zkteco_proto::EventFlags::ENROLL_FINGER, c.to_le_bytes().to_vec());
    dev.state().enroll_script = vec![code(0x64), code(0x64), code(0x64), code(0)];
    let a = env
        .run(&args(
            &at(&dev),
            &[
                "enroll",
                "--user-id",
                "1001",
                "--finger",
                "2",
                "--yes",
                "--output",
                "ndjson",
            ],
        ))
        .await
        .code(0);
    insta::assert_snapshot!("enroll_ndjson", stdout(&a));
    env.run(&args(
        &at(&dev),
        &["enroll", "--user-id", "1001", "--finger", "10", "--yes"],
    ))
    .await
    .code(2);
}

#[tokio::test]
async fn templates_need_the_biometric_flag() {
    let env = Env::new();
    let dev = device().await;
    let out = env.path("t.json");
    let out = out.to_str().unwrap();
    let a = env
        .run(&args(&at(&dev), &["templates", "export", "--out", out, "--yes"]))
        .await
        .code(2);
    assert!(stderr(&a).contains("--i-understand-biometric-data"));
    assert!(!Path::new(out).exists());
}

#[tokio::test]
async fn templates_export_and_import() {
    let env = Env::new();
    let dev = MockDevice::start(DeviceState {
        users: users(2),
        templates: templates(&[1, 2], 300),
        ..DeviceState::default()
    })
    .await;
    let host = at(&dev);
    let file = env.path("templates.json");
    let file = file.to_str().unwrap();
    let flags = ["--yes", "--i-understand-biometric-data"];
    let mut export = args(&host, &["templates", "export", "--out", file]);
    export.extend(flags);
    let a = env.run(&export).await.code(0);
    assert!(stderr(&a).contains("biometric personal data"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!(saved["entries"].as_array().unwrap().len(), 2);
    assert_read_only(&dev, &[]);

    let mut dry = args(
        &host,
        &["templates", "import", file, "--dry-run", "--output", "json"],
    );
    dry.extend(flags);
    let a = env.run(&dry).await.code(0);
    let plan = json(&a);
    assert!(plan["would_send"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["payload_hex"].is_null()));
    assert_eq!(dev.state().saved_bulk, Vec::<Vec<u8>>::new());

    let mut import = args(&host, &["templates", "import", file]);
    import.extend(flags);
    env.run(&import).await.code(0);
    assert_eq!(dev.state().saved_bulk.len(), 1);
}

// ---- device profiles ----

/// The `id`s of a plain `attendance pull`, with these selection arguments.
async fn pulled_ids(env: &Env, select: &[&str]) -> Vec<Value> {
    let mut a = vec!["attendance", "pull", "--output", "ndjson"];
    a.extend_from_slice(select);
    let out = env.run(&a).await.code(0);
    lines(&out).iter().map(|r| r["id"].clone()).collect()
}

/// `id` follows the device's serial number, not the profile name: renaming
/// a profile keeps the ids, and two profiles (or `--host`) reaching the
/// same device give the same ids.
#[tokio::test]
async fn ids_follow_the_device_not_the_profile() {
    let env = Env::new();
    let dev = device().await;
    env.devices(&[("front", &dev)]);
    let before = pulled_ids(&env, &["-d", "front"]).await;
    assert_eq!(before.len(), 4);
    env.devices(&[("lobby", &dev)]);
    assert_eq!(pulled_ids(&env, &["-d", "lobby"]).await, before, "renamed");

    env.devices(&[("front", &dev), ("lobby", &dev)]);
    let both = pulled_ids(&env, &["-d", "front", "-d", "lobby"]).await;
    assert_eq!(both[..4], before[..], "front");
    assert_eq!(both[4..], before[..], "lobby");
    let host = at(&dev);
    let mut by_host = vec!["attendance", "pull", "--output", "ndjson"];
    by_host.extend(host.iter().map(String::as_str));
    let out = env.run(&by_host).await.code(0);
    let ids: Vec<Value> = lines(&out).iter().map(|r| r["id"].clone()).collect();
    assert_eq!(ids, before, "by host");
}

/// The serial number cannot be read (an error, or an empty value): the
/// key falls back to `profile:<name>` with a warning, and the pull still
/// succeeds.
#[tokio::test]
async fn ids_fall_back_to_the_profile_name() {
    for empty in [false, true] {
        let env = Env::new();
        let dev = device().await;
        {
            let mut state = dev.state();
            state.faults.serial_number_error = !empty;
            state.faults.empty_serial_number = empty;
        }
        env.devices(&[("front", &dev)]);
        let a = env
            .run(&["attendance", "pull", "-d", "front", "--output", "ndjson"])
            .await
            .code(0);
        let records = lines(&a);
        assert_eq!(records.len(), 4, "empty: {empty}");
        let key = DeviceKey::profile("front");
        let expected: Vec<String> = punches(4).iter().map(|p| event_id::attendance(&key, p)).collect();
        let ids: Vec<&str> = records.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, expected, "empty: {empty}");
        let err = stderr(&a);
        assert!(
            err.contains("warning: front: cannot read the serial number"),
            "empty: {empty}: {err}"
        );
        if empty {
            assert!(err.contains("returned an empty one"), "{err}");
        }
    }
}

#[tokio::test]
async fn device_profiles() {
    let env = Env::new();
    env.run(&[
        "device",
        "add",
        "front",
        "--host",
        "10.1.2.3",
        "--port",
        "4371",
        "--transport",
        "udp",
    ])
    .await
    .code(0);
    env.run(&["device", "add", "back", "--host", "10.1.2.4"])
        .await
        .code(0);
    let a = env.run(&["device", "list", "--output", "json"]).await.code(0);
    insta::assert_json_snapshot!("device_list", json(&a));
    let a = env
        .run(&["device", "show", "front", "--output", "json"])
        .await
        .code(0);
    assert_eq!(json(&a)["port"], 4371);
    env.run(&["device", "remove", "back"]).await.code(0);
    env.run(&["device", "show", "back"]).await.code(2);
    let text = std::fs::read_to_string(env.config()).unwrap();
    assert!(!text.contains("password"));
    std::fs::write(env.config(), "[devices.x]\nhost = \"h\"\npassword = 1\n").unwrap();
    env.run(&["device", "list"]).await.code(1);
}

// ---- diagnose ----

#[tokio::test]
async fn diagnose_runs_read_only() {
    let env = Env::new();
    let dev = device().await;
    let port = dev.addr.port().to_string();
    let a = env
        .run(&[
            "diagnose",
            &dev.addr.ip().to_string(),
            "--port",
            &port,
            "--timeout",
            "500ms",
        ])
        .await
        .code(0);
    assert!(json(&a)["row"]["firmware"].is_string());
    assert_read_only(&dev, &[]);
}
