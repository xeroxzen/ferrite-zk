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
5. [The event schema](#the-event-schema)
6. [Several devices](#several-devices)
7. [Errors and exit codes](#errors-and-exit-codes)
8. [Configuration and passwords](#configuration-and-passwords)
9. [Data protection](#data-protection)

## Getting started

The device must be reachable on port 4370 (TCP or UDP). Its *comm key*
(communication password, a number, `0` when unset) is set in the device
menu under communication settings. No device at hand? See
[Try it without hardware](#try-it-without-hardware).

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

### Try it without hardware

`demo_device` runs the mock device from the test suite as a stand-alone
device on 127.0.0.1. It has ten fictional users (user IDs 1001 to 1010,
names like "Ada Example"), a dozen earlier punches in its log, and:

- a punch every 2 to 4 seconds, by a random user, alternating check-in
  and check-out;
- every 30 seconds, 5 seconds offline: connections are closed and new ones
  refused, and punches go on being recorded in the log;
- serial number `DEMO-<NAME>`, so two demo devices have different event
  ids ([Deduplicating](#deduplicating-on-id)).

From a clone of this repository, start two devices, each in its own
terminal:

```sh
cargo run -p zkteco-client --example demo_device -- --name front --port 14370
cargo run -p zkteco-client --example demo_device -- --name lobby --port 14371
```

Then, in a third, register both and stream from them:

```sh
zkctl device add front --host 127.0.0.1 --port 14370 --transport tcp
zkctl device add lobby --host 127.0.0.1 --port 14371 --transport tcp
zkctl live -d front -d lobby --reconnect --backfill
```

Each punch arrives once: as `live` while connected, or as `backfill` after
a drop, announced by status lines:

```text
2026-11-16T00:50:06  front  live      user 1009 password check_in
2026-11-16T00:50:06  front  disconnected, reconnecting…
2026-11-16T00:50:14  front  reconnected after 7.8 s, backfilled 5 events
2026-11-16T00:50:07  front  backfill  user 1010 (uid 10) fingerprint check_out
```

Add `-v` to see each failed reconnect attempt and the client's logs, or
`--output ndjson` for the event objects.

| Option | Default | |
|---|---|---|
| `--port N` | 14370 | TCP and UDP port on 127.0.0.1 |
| `--name NAME` | `front` | Device name, in the serial number and the printed commands |
| `--interval SECS` | 3 | Mean time between punches |
| `--drop-every SECS` | 30 | How often to go offline, for 5 s |
| `--no-drops` | | Never go offline |
| `--seed N` | 1 | The same seed gives the same people and punches, for recordings |

The device accepts writes too (`users add`, `time set`, `attendance
clear`); they change its memory only and are gone when it stops. Remove
the profiles afterwards with `zkctl device remove front` (and `lobby`).
[`docs/demo/live.tape`](demo/live.tape) records the session above as a GIF
with [vhs](https://github.com/charmbracelet/vhs).

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
zkctl -d front live --reconnect --backfill --state /var/lib/zkctl/live.json \
    --with-users --output ndjson
```

```json
{"v":1,"id":"ff4c21a7a3b3bc43f22e41caef023d77","device":"front","source":"live","device_time":"2026-01-02T03:04:05","received_at":"2026-01-02T03:04:05.120Z","kind":"attendance","uid":7,"user_id":"1001","user_id_source":"record","status":"fingerprint","status_code":1,"punch":"check_in","punch_code":0,"workcode":null}
```

Each line is one event in the [event schema](#the-event-schema). `source`
is `live`, or `backfill` for punches read from the log after a reconnect or
restart. Order by `device_time` within a device; do not compare it across
devices whose clocks may differ.

### What happens when things go wrong

| Situation | Without `--reconnect` | With `--reconnect` |
|---|---|---|
| Device unreachable at start | Exit 3 | Retries with backoff (1 s doubling to 30 s) until it connects or Ctrl-C |
| Connection lost | That device's stream ends; exit 3 when all end | Reconnects and resubscribes (status lines in human output, a warning on standard error in ndjson) |
| Wrong password | Exit 4 | Exit 4 (not retried) |
| Device silent (power loss without a TCP close) | Detected by keepalive and by the heartbeat | Same, then reconnects |

The heartbeat (every 60 s without a packet) asks the device for its clock.
It tolerates up to three unanswered heartbeats in a row before treating
the connection as dead, because it is not yet known whether every
firmware answers requests while subscribed (Q6).

In human output (the default), `live` reports each device's connection as
status lines on standard error, stamped with the host's local time.
Standard output carries only the events, so `zkctl live > punches.txt`
keeps a clean record while the status shows on the terminal:

| Line | When |
|---|---|
| `front  cannot connect (…), retrying…` | The first connection fails, with `--reconnect` |
| `front  connected after 3.0 s` | It then works |
| `front  disconnected, reconnecting…` | The connection is lost (`disconnected` without `--reconnect`) |
| `front  reconnected after 6.2 s, backfilled 3 events` | Resubscribed, and with `--backfill`, caught up |
| `front  reconnect failed (attempt 2): …` | Each failed attempt, only with `-v` |

The client's own logs (retries, reconnect attempts) appear only with `-v`;
log timestamps are local time too. Status lines are dimmed when standard
error is a terminal, and `backfill` is coloured when standard output is,
unless `NO_COLOR` is set. Event lines show the
uid only when it is known. `--output ndjson` is unchanged: no status
lines, warnings and logs on standard error as before.

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

### Surviving restarts: `--state`

Backfill after a reconnect works from memory. To cover the time the
process itself was not running, give `live` a state file:

```sh
zkctl -d front -d back live --reconnect --backfill --state /var/lib/zkctl/live.json --output ndjson
```

- Per device, the file records the newest device time emitted and the
  punches emitted at that time (uid, user ID, verification method and
  punch state). It is written atomically (temporary file, fsync, rename)
  with mode 0600, at most `--state-flush` apart while events arrive
  (default 1 s; `0` writes after every event), and on exit.
- On start with `--backfill`, each device in the file is caught up first:
  the log is read after subscribing, and every record at or after the
  saved time that was not already emitted is emitted as `backfill`, before
  any live event.
- Only emitted events are recorded, so events dropped by `--count` are
  caught up on the next run.
- A missing file starts fresh. An unreadable or corrupt one is reported on
  standard error and replaced; the stream starts anyway.
- After a clean stop (SIGINT, `--count`, `--duration`), each punch is
  emitted exactly once across runs. After a crash or SIGKILL, punches
  emitted within the last `--state-flush` before it may be emitted again
  (at-least-once); deduplicate on `id` (see
  [Deduplicating](#deduplicating-on-id)).
- Records stored with a device time earlier than the saved one (after the
  device clock was set back) are not caught up.

### Stopping

Ctrl-C (or SIGINT from a supervisor) unsubscribes from every device, prints
any events that arrived meanwhile, saves the state file, and exits 0. Kill
with SIGTERM or SIGKILL and the device stays subscribed to a dead
connection until it notices; nothing is lost from the device's log, and
`--state` with `--backfill` recovers it on the next start. `--duration 8h`
stops after a time. `--count N` stops after N events; events that arrive
while it unsubscribes are not emitted, and their number is reported on
standard error.

### Running it as a service

- Run `zkctl live --reconnect --backfill --output ndjson` under a
  supervisor (systemd, a container runtime) that restarts it on exit and
  stops it with SIGINT.
- Read standard output line by line; each line is one complete JSON
  object. Logs go to standard error.
- Add `--state FILE` so a restart catches up on what it missed.
- For incremental export without streaming, run
  `zkctl -d front attendance pull --state /var/lib/zkctl/front.json --output ndjson`
  from cron.

In a Rust program, use `LiveCapture` directly: see
[`live_with_shutdown`](../crates/zkteco-client/examples/live_with_shutdown.rs).
`LiveCapture::next` is cancel-safe, so it can race a shutdown signal in
`tokio::select!` without losing an event. `LiveCapture::stop` returns the
`Client`; `Client::take_pending_captured` then yields the events that
arrived during the stop, with their source.

## The event schema

`attendance pull` and `live` print the same object for every record, from
every source. The formal definition is
[docs/schema/event-v1.json](schema/event-v1.json) (JSON Schema 2020-12);
the tests validate real output against it.

| Key | Type | Meaning |
|---|---|---|
| `v` | integer | Schema version, `1` |
| `id` | string | Deterministic identifier, 32 lowercase hex digits; see [Deduplicating](#deduplicating-on-id) |
| `device` | string | Profile name (`-d`), or the host with `--host` |
| `source` | string | `pull`, `live`, or `backfill` (read from the log by `live`) |
| `device_time` | string or null | Device clock in the record, `YYYY-MM-DDTHH:MM:SS`, device-local, no zone. Null for raw events |
| `received_at` | string or null | Host arrival time, RFC 3339 UTC with milliseconds; for backfill, when the log was read. Null for `pull` |
| `kind` | string | `attendance`, or `raw` for an event this version does not decode |
| `uid` | integer or null | Device slot, when the record has it or it was resolved (`--with-users`) |
| `user_id` | string or null | The user ID people see, when known |
| `user_id_source` | string or null | `record` (the record carries it) or `user_table` (looked up by uid, `--with-users`). Null when unresolved, and for raw events |
| `status` | string or null | Verification method: `password`, `fingerprint`, `card`, `face`, `unknown` |
| `status_code` | integer or null | The method code as stored |
| `punch` | string or null | `check_in`, `check_out`, `break_out`, `break_in`, `overtime_in`, `overtime_out`, `unknown` |
| `punch_code` | integer or null | The punch code as stored |
| `workcode` | integer or null | Work code, in layouts that have one |
| `raw` | object | Only when `kind` is `raw`: `{"flags": integer, "payload_hex": string}` |

Every key except `raw` is always present. For `kind: attendance`,
`device_time`, `status`, `status_code`, `punch` and `punch_code` are never
null; for `raw` they are. Which of `uid` and `user_id` a record carries
depends on the device's layout; `--with-users` fills in the other from the
user table.

On a log layout without user IDs (8 bytes), `attendance pull` reads the user
table even without `--with-users`: one extra table read per device per run,
the same read `--with-users` makes, so that `id` names the person by user ID
as live events do. Without `--with-users` the user IDs found this way are
used only for `id`; `user_id` and `user_id_source` stay null. On layouts
whose records carry user IDs, no extra read is made.

Compatibility: a field may be added without changing `v`, so ignore keys
you do not know. Removing or renaming a field, or changing its type or
meaning, would be version 2, with a new schema file. `--output json`
prints the same objects in one array (`attendance pull` only).

### Deduplicating on `id`

`id` identifies a punch, not a line of output: the same punch has the same
`id` whether it comes from `attendance pull`, a live event or backfill, in
any run. Delivery is at-least-once in two places, so consumers that store
events should treat `id` as a unique key and ignore repeats:

- `live --state` after a crash or SIGKILL re-emits what was emitted in the
  last `--state-flush` interval before it.
- `attendance pull` without `--state` emits the whole log every time, and
  overlaps with `live`.

What goes into `id`, so you can recompute it: the first 16 bytes of
SHA-256, as lowercase hex, over a sequence of fields, each preceded by its
length as an 8-byte big-endian integer.

- `kind: attendance`: `zkctl-event-v1`, `attendance`, the device key,
  `device_time`, the person (`user_id:` and the user ID if there is one,
  otherwise `uid:` and the uid, otherwise empty), the status code as one
  byte, the punch code as one byte.
- `kind: raw`: `zkctl-event-v1`, `raw`, the device key, the flags as a
  4-byte big-endian integer, the payload bytes.

The device key is `serial:` and the device's serial number, so renaming a
profile, or reaching one device through two profiles or by host, keeps its
ids. It is not in the output: run `zkctl -d <device> info` to see the
serial number. If the serial number cannot be read, `zkctl` warns and uses
`profile:` and the profile name (or host) instead; those ids change if the
profile is renamed.

For example, user ID `1001` at `2026-01-02T03:04:05` on the device with
serial number `A`, fingerprint (1), check-in (0), is
`ff4c21a7a3b3bc43f22e41caef023d77`, with or without a uid.

Limits, because the device log has no identifier of its own:

- **Identical punches in one second.** The device records time to the
  second. Two punches by the same person, with the same method and punch
  state, in the same second, get the same `id` and are indistinguishable;
  `live` emits them once, and deduplicating on `id` keeps one. Two different
  people, methods or punch states in the same second get different ids.
- **The person must be named the same way.** When a user ID is known it is
  used, and the uid ignored, because live events carry only the user ID.
  On a log without user IDs, pull and backfill look the user ID up by uid
  (above). A punch whose uid is no longer in the user table (the user was
  deleted) is named by uid in the log, so it gets a different `id` there
  than the live event did.
- **The device is part of it, by serial number.** Renaming a profile, or
  switching between profiles and `--host` for one device, keeps its ids.
  When the serial number cannot be read, the ids use the profile name (or
  host) and change with it; `zkctl` warns when that happens.
- Raw events with the same flags and payload from one device share an
  `id`.

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
- `attendance pull --state` and `live --state` files hold user IDs and
  uids of recent punches; they are written with mode 0600.
- `diagnose` reports scrub serial numbers and MACs unless
  `--include-identifiers` is given, and contain no user records. Its packet
  captures replace names, IDs, cards, PINs and timestamps with synthetic
  values and zero the comm key.
- Logs at `-vvv` contain packet hex dumps, which include whatever the
  device sent. Do not share them unscrubbed.

Never post passwords, templates, captures or logs with personal data in
issues; see [SECURITY.md](../SECURITY.md).
