# Ferrite guide

How to use `zkctl` and `zkteco-client` with ZKTeco devices, and what they
promise when the network or the device misbehaves.

**Alpha, untested on physical devices.** Everything here is tested against
a mock device. Where real devices may differ, this guide says so and points
to the open questions in [DECISIONS.md](DECISIONS.md).

1. [Getting started](#getting-started)
2. [Transports](#transports)
3. [Reliability semantics](#reliability-semantics)
4. [Live capture in production](#live-capture-in-production)
5. [Several devices](#several-devices)
6. [Errors and exit codes](#errors-and-exit-codes)
7. [Configuration and passwords](#configuration-and-passwords)
8. [Data protection](#data-protection)

## Getting started

The device must be reachable on port 4370 (TCP or UDP). Its *comm key*
(communication password, a number, `0` when unset) is set in the device
menu under communication settings.

### Command line

```sh
cargo install zkctl --version 0.1.0-alpha.1

zkctl --host 192.168.1.201 info              # one-off, by address
zkctl device add front --host 192.168.1.201  # or give it a name
zkctl device set-password front              # prompted; stored in the OS keyring
zkctl -d front info
zkctl -d front time get
zkctl -d front users list --filter alice
zkctl -d front attendance pull --since 7d --output json > punches.json
zkctl -d front live                          # Ctrl-C to stop
```

`zkctl help <command>` describes every option. Shell completions:
`zkctl completions bash > ~/.local/share/bash-completion/completions/zkctl`
(also `zsh`, `fish`, `powershell`, `elvish`).

Commands that change the device do nothing without `--yes`. Try them with
`--dry-run` first: it prints the packets that would be sent and sends no
writes.

```sh
zkctl -d front users add --user-id 1001 --name "A. Person" --dry-run
zkctl -d front users add --user-id 1001 --name "A. Person" --yes
zkctl -d front enroll --user-id 1001 --finger 0 --yes   # then scan three times
```

### Library

```rust
use zkteco_client::{Client, ClientConfig};

#[tokio::main]
async fn main() -> zkteco_client::Result<()> {
    let config = ClientConfig::new("192.168.1.201").password(0).read_only(true);
    let mut client = Client::connect(config).await?;
    println!("firmware {}", client.firmware_version().await?);
    let users = client.users().await?;
    for user in &users.records {
        println!("{} {}", user.user_id, user.display_name());
    }
    for bad in &users.rejected {
        eprintln!("record at offset {} rejected: {}", bad.offset, bad.reason);
    }
    client.disconnect().await
}
```

The [examples](../crates/zkteco-client/examples) cover reading users,
attendance since a time, live capture with a clean shutdown, and several
devices at once.

Users have two identities, kept as distinct types: `Uid`, the device's
internal slot number, and `UserId`, the ID people see and type. Commands
that pick a user take one or the other explicitly (`--uid` or `--user-id`).

## Transports

| `Transport` / `--transport` | Behaviour |
|---|---|
| `auto` (default) | TCP, falling back to UDP if the device cannot be reached over TCP |
| `tcp` | TCP only |
| `udp` | UDP only |

Prefer TCP. Over UDP, table reads ask for one datagram's worth at a time,
because UDP can reorder datagrams and the protocol has no known way to
detect it within a larger chunk (DECISIONS D25). UDP reads are therefore
slower for large tables. pyzk's 16 KiB UDP chunks are not offered: they can
return wrong records without an error.

TCP connections use keepalive (`ClientConfig::tcp_keepalive`, 30 s idle by
default), so a device that loses power is noticed even when idle.

## Reliability semantics

- **One request at a time.** The protocol is strict request and reply.
  `Client` methods take `&mut self`; use one `Client` per device.
- **Timeouts close the connection.** Every reply waits at most
  `ClientConfig::request_timeout` (`--timeout`, default 10 s in `zkctl`). A
  timeout returns `Error::Timeout` and closes the connection, so a late
  reply can never be mistaken for the answer to a later request. Later
  calls return `Error::ConnectionClosed` until `Client::reconnect`.
- **Retries** cover connecting and corrupt or short chunks within a table
  read (`RetryPolicy`). Writes are never retried automatically: a write
  whose reply was lost may or may not have happened.
- **Bad records are isolated.** Table reads return a `Batch`: the records
  that decoded, and a `rejected` list with the offset and reason of each
  that did not. Nothing is dropped silently. `zkctl` prints rejected
  records to standard error and exits with 6.
- **Events are never dropped silently.** Live events that arrive while the
  client waits for a reply are acknowledged and queued
  (`Client::take_pending_events`). The queue is bounded
  (`ClientConfig::pending_event_capacity`); overflow drops the oldest,
  logs a warning and is counted (`Client::dropped_events`).
- **Read-only mode.** `ClientConfig::read_only(true)` refuses every command
  outside `READ_ONLY_COMMANDS` before it is sent. Every `zkctl` read
  command, `live` and `diagnose` use it.
- **Dry run.** `ClientConfig::dry_run(true)` records every write instead of
  sending it, and answers it locally with success; `Client::take_planned`
  returns the recorded packets. Reads still go to the device, because
  writes often depend on them (the next free uid, the user layout).
- **Device time has no time zone.** Records carry the device's local clock
  as a `NaiveDateTime`. `zkctl` prints it without an offset. Host times
  (`received_at`) are UTC.

## Live capture in production

`zkctl live` and `LiveCapture` are the basis for systems that react to
punches as they happen.

```sh
zkctl -d front live --reconnect --backfill --with-users --output ndjson
```

```json
{"device":"front","source":"live","received_at":"2026-01-02T03:04:05.120Z","device_time":"2026-01-02T03:04:05","kind":"attendance","uid":7,"user_id":"1001","status":"fingerprint","status_code":1,"punch":"check_in","punch_code":0,"workcode":null}
```

- `kind` is `attendance` for decoded punches, or `raw` (with `flags` and
  `payload_hex`) for events this version does not decode. Raw events are
  passed on, never dropped.
- `source` is `live`, or `backfill` for punches read from the log after a
  reconnect.
- `device_time` is the device clock in the record; `received_at` is when
  the host received it (for backfill, when the log was read). Order by
  `device_time` within a device; do not compare it across devices whose
  clocks may differ.

### What happens when things go wrong

| Situation | Without `--reconnect` | With `--reconnect` |
|---|---|---|
| Device unreachable at start | Exit 3 | Retries with backoff (1 s doubling to 30 s) until it connects or Ctrl-C |
| Connection lost | That device's stream ends; exit 3 when all end | Reconnects and resubscribes; a warning on standard error |
| Wrong password | Exit 4 | Exit 4 (not retried) |
| Device silent (power loss without a TCP close) | Detected by keepalive and by the heartbeat | Same, then reconnects |

The heartbeat (every 60 s without a packet) asks the device for its clock.
It tolerates up to three unanswered heartbeats in a row before treating
the connection as dead, because it is not yet known whether every
firmware answers requests while subscribed (Q6).

### Backfill and duplicates

With `--backfill` (library: `CaptureOptions::backfill`, which needs
`reconnect`), each reconnect reads the attendance log and delivers, as
`backfill`, every record at or after the newest punch already delivered.
Live events and backfilled records pass through one duplicate filter keyed
on user, device time, status and punch, remembering the last 4,096
deliveries. Each punch is delivered once, even if it was delivered live
before the connection dropped. Two genuine punches by the same user with
the same status in the same second are indistinguishable and delivered
once; the log has no other identifier.

Backfill reads the whole log (the protocol has no "since" query), which
takes a few seconds on a device with many records. It cannot recover
punches the device never stored.

### Stopping

Ctrl-C (or SIGINT from a supervisor) unsubscribes from every device, prints
any events that arrived meanwhile, and exits 0. Kill with SIGTERM or SIGKILL
and the device stays subscribed to a dead connection until it notices;
nothing is lost from the device's log, but events in flight are. `--count N`
stops after N events; `--duration 8h` after a time.

### Running it as a service

- Run `zkctl live --reconnect --backfill --output ndjson` under a
  supervisor (systemd, a container runtime) that restarts it on exit and
  stops it with SIGINT.
- Read standard output line by line; each line is one complete JSON
  object. Logs go to standard error.
- After a restart of the process itself, backfill does not cover the gap
  (the watermark lives in memory). Close it with
  `attendance pull --state FILE` on start, which emits only records after
  the previous run's.
- For incremental export without streaming, run
  `zkctl -d front attendance pull --state /var/lib/zkctl/front.json --output ndjson`
  from cron.

In a Rust program, use `LiveCapture` directly: see
[`live_with_shutdown`](../crates/zkteco-client/examples/live_with_shutdown.rs).
`LiveCapture::next` is cancel-safe, so it can race a shutdown signal in
`tokio::select!` without losing an event. `LiveCapture::stop` returns the
`Client`; `Client::take_pending_captured` then yields the events that
arrived during the stop, with their source.

## Several devices

`zkctl attendance pull` and `zkctl live` accept `-d` more than once:

```sh
zkctl -d front -d back -d warehouse attendance pull --since 24h --output ndjson
zkctl -d front -d back live --reconnect --backfill --output ndjson
```

Devices are handled concurrently, and every record or event carries a
`device` field with the profile name. One device failing does not stop the
others; the exit code is the most serious outcome among them. In `live`, events
from all devices are printed by one writer, so lines never interleave;
order across devices is arrival order.

In a program, run one `Client` or `LiveCapture` per device in its own task
and merge through a channel: see
[`multi_device_live`](../crates/zkteco-client/examples/multi_device_live.rs).

## Errors and exit codes

`zkteco_client::Error`:

| Variant | Meaning |
|---|---|
| `Io` | Socket error |
| `Timeout { command, after }` | No reply in time; the connection is closed |
| `ConnectionClosed` | Used after a timeout or disconnect; call `reconnect` |
| `Unauthorized` | Wrong comm key |
| `DeviceError { command, reply }` | The device refused a command |
| `ReadOnly(command)` | Refused locally by read-only mode; nothing sent |
| `Malformed`, `ShortRead`, `UnsupportedRecordFormat` | The reply did not decode |
| `UserNotFound(user_id)` | No such user |
| `InvalidArgument`, `Unsupported` | The request cannot be expressed |

`zkctl` exit codes:

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Error |
| 2 | Usage error (bad arguments, missing `--yes`, unknown device) |
| 3 | Connection error or timeout |
| 4 | Authentication failed |
| 5 | The device refused a command (also: enrolment not stored) |
| 6 | Partial success: some records were rejected (see standard error) |
| 7 | Aborted: confirmation declined, or interrupted |

With several devices, the exit code is the most serious per-device outcome:
any failure outranks partial success, which outranks success. Between two
failures, the higher code wins.

## Configuration and passwords

Named devices live in a TOML file: `~/.config/zkctl/config.toml` on Linux,
`~/Library/Application Support/zkctl/config.toml` on macOS,
`%APPDATA%\zkctl\config\config.toml` on Windows. `--config FILE` or
`ZKCTL_CONFIG` overrides it.

```toml
[devices.front]
host = "192.168.1.201"
port = 4370
transport = "tcp"
timeout = "5s"
```

Manage it with `zkctl device add|list|show|remove`. Command-line options
override the profile's values.

The comm key is taken from, in order: `--password`, `ZK_PASSWORD`, the OS
keyring (`zkctl device set-password NAME`; macOS Keychain, Windows
Credential Manager, the Secret Service on Linux), then an interactive
prompt if a terminal is attached and the device rejects the connection.
Passwords are never written to the config file; a `password` key there is
an error. `--password` is visible in shell history and process lists;
prefer the others. `ZKCTL_NO_KEYRING=1` turns the keyring off.

## Data protection

Devices hold personal data: names, user IDs, card numbers, PINs,
attendance times, and fingerprint templates, which are biometric data
under GDPR article 9, BIPA and similar laws.

- `users list` shows whether a user has a PIN, never the PIN.
- `templates export` needs `--yes` and `--i-understand-biometric-data`,
  prints a warning, and writes the file with mode 0600 (owner only). The
  file holds the templates and the users' PINs. Encrypt it at rest, keep
  it no longer than needed, and do not send it anywhere it is not needed.
- `templates import --dry-run` prints the packets with template payloads
  redacted.
- `diagnose` reports scrub serial numbers and MACs unless
  `--include-identifiers` is given, and contain no user records. Its packet
  captures replace names, IDs, cards, PINs and timestamps with synthetic
  values and zero the comm key.
- Logs at `-vvv` contain packet hex dumps, which include whatever the
  device sent. Do not share them unscrubbed.

Never post passwords, templates, captures or logs with personal data in
issues; see [SECURITY.md](../SECURITY.md).
