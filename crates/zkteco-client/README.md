# zkteco-client

Async (tokio) client for ZKTeco biometric and access-control devices, over
TCP and UDP on port 4370, with feature parity with
[pyzk](https://github.com/fananimi/pyzk). Built on
[`zkteco-proto`](https://crates.io/crates/zkteco-proto). Part of the
[Ferrite](https://github.com/xeroxzen/ferrite-zk) project.

**Status: alpha, untested on physical devices.** Every flow is tested
against a mock device over both transports; no layout has yet been
confirmed by a capture from real hardware.

```rust
use zkteco_client::{Client, ClientConfig};

let mut client = Client::connect(ClientConfig::new("192.168.1.201").read_only(true)).await?;
let users = client.users().await?;
for user in &users.records {
    println!("{} {}", user.user_id, user.display_name());
}
client.disconnect().await?;
```

## What it does

- Device information, clock, options, counters and network parameters.
- Users, fingerprint templates and the attendance log: read, write, delete.
  Bad records are isolated and reported, never dropped silently.
- Live events as a cancel-safe `Stream` (`LiveCapture`), with optional
  reconnect and backfill of punches stored while disconnected, each
  delivered once.
- Fingerprint enrolment with progress reports.
- `ClientConfig::read_only` refuses every command outside a fixed allowlist
  of reads before anything is sent. `ClientConfig::dry_run` records writes
  instead of sending them.

Methods that change or erase device data are marked **Destructive** in
their docs.

## Examples

Each runs against a device (`ZK_HOST`, `ZK_PORT`, `ZK_PASSWORD`), and in CI
against the mock device:

| Example | Shows |
|---|---|
| `list_users` | A read-only connection and rejected-record reporting |
| `pull_attendance_since` | Attendance from a point in time, with user IDs resolved |
| `live_with_shutdown` | Streaming until Ctrl-C, then a clean unsubscribe |
| `multi_device_live` | Several devices merged into one channel, each event tagged |

```sh
ZK_HOST=192.168.1.201 cargo run -p zkteco-client --example live_with_shutdown
```

## Documentation

- [API docs](https://docs.rs/zkteco-client)
- [User guide](https://github.com/xeroxzen/ferrite-zk/blob/main/docs/GUIDE.md):
  transports, reliability semantics, live capture in production, errors.
- [Design decisions](https://github.com/xeroxzen/ferrite-zk/blob/main/docs/DECISIONS.md)

## MSRV

Rust 1.88.

## License

MIT or Apache-2.0, at your option.
