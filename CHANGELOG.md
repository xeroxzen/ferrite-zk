# Changelog

All notable changes to the crates in this repository are recorded here. The
format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the crates follow [Semantic Versioning](https://semver.org/). The three
crates share one version.

## [Unreleased]

## [0.1.0-alpha.2] - 2026-11-16

### zkteco-client

- **Breaking:** `CaptureOptions`, `ReconnectPolicy`, `Resume`,
  `CaptureStats`, `RetryPolicy` and `EnrollOptions` are
  `#[non_exhaustive]`, so fields can be added without a breaking release.
  Build them from `Default` (or `Resume::new`) and chained setters named
  after the fields, as `ClientConfig` already works:
  `CaptureOptions::default().reconnect(ReconnectPolicy::default()).backfill(true)`.
  Fields stay public for reading and assignment.
- Fixed: reading attendance failed with `UnsupportedRecordFormat` when a
  punch was stored between the record-count query and the table read
  (the layout is inferred from both), and a log that grew enough could
  even be decoded with a wrong layout. A detected layout now counts only
  if the count, read again after the table, is unchanged; otherwise the
  table is read again, up to three reads, then
  `Error::TableChanging { kind, attempts }` (new, retryable) with nothing
  decoded. The same holds for the user table. This mostly hit backfills
  right after a reconnect, and `attendance pull` on a busy device.
- Detected user and attendance layouts are kept across
  `Client::reconnect` to the same device, by serial number (read once per
  connection that needs it); a different serial number drops them.
- Fixed: a backfill that failed after a reconnect was reported once and
  then skipped, losing the punches stored while disconnected until the
  next drop. It now fails the reconnect attempt, which is retried under
  the reconnect policy and backfills from the same point.
- `CaptureStats::connected` and `CaptureStats::reconnect_failures`: whether
  the capture is subscribed (false from a lost connection until the
  reconnect and its backfill are done) and the failed reconnect attempts
  since.
- `demo_device` example: a stand-alone demo device on 127.0.0.1 with
  fictional users and punches, and periodic dropped connections, to try
  `zkctl` without hardware (`--port`, `--name`, `--interval`,
  `--drop-every`, `--no-drops`, `--seed`).
- The mock device moved from `tests/support` into the crate as
  `zkteco_client::demo`, behind the `demo` feature: off by default, hidden
  from the documentation, not part of the API.

### zkteco-proto

- **Breaking:** `User`, `Attendance` and `Template` are
  `#[non_exhaustive]`. Build them with `User::new(uid, user_id)`,
  `Attendance::new(timestamp, status, punch)` and
  `Template::new(uid, fid, data)`, then set the other public fields.

### zkctl

- `Error::TableChanging` (a table that changed during every read) exits
  with 5, as a device error; the exit-code table in `--help`, the guide
  and the README say it is transient and safe to retry.
- `live` in human output reports each device's connection as status lines
  on standard error (`disconnected, reconnecting…`, `reconnected after
  6.2 s, backfilled 3 events`), in the host's local time. The client's
  logs and retry attempts need `-v`; log timestamps are local time, and
  logs are coloured only on a terminal without `NO_COLOR`. Event lines
  show the uid only when known (no `(uid ?)`). On a terminal, status lines
  are dimmed and `backfill` is coloured. `ndjson` output is unchanged.

### Documentation

- "Try it without hardware" in the README and guide, and a vhs tape
  (`docs/demo/live.tape`) that records it.

## [0.1.0-alpha.1] - 2026-11-15

First release. **Alpha, untested on physical devices**: everything is
tested against a mock device only.

### zkteco-proto

- Sans-IO ZK protocol: packet header and checksum, TCP framing with a
  streaming decoder, session and reply-ID sequencing, comm key.
- Record layouts: users (28, 72 and 120 bytes), attendance (8, 16, 40 and
  49 bytes), fingerprint templates, live events, counters and options,
  with encoders for each. Bad records are isolated in a `Batch`, not
  dropped.
- Device time encodings, chunked transfers and uploads.
- Byte-for-byte agreement with pyzk, checked by golden fixtures.

### zkteco-client

- Async tokio client over TCP and UDP, with `Transport::Auto` falling back
  from TCP to UDP.
- Device information, clock, options, users, templates and attendance:
  read, write, delete, clear.
- `LiveCapture`: a cancel-safe stream of live events with reconnect,
  backfill of punches stored while disconnected, duplicate suppression, a
  heartbeat that tolerates missed answers, and TCP keepalive. Events carry
  their source, device time and host arrival times.
- Fingerprint enrolment with progress reports.
- `CaptureOptions::resume` to start a capture where an earlier one
  stopped, catching up on what was stored in between.
- `CapturedEvent::user_id_source`: whether an event's user ID came from
  the event or the user table.
- Read-only mode, refusing every command outside an allowlist before it is
  sent, and dry-run mode, recording writes instead of sending them.
- Examples: `list_users`, `pull_attendance_since`, `live_with_shutdown`,
  `multi_device_live`.

### zkctl

- Read commands: `info`, `time get`, `users list`, `attendance pull`
  (several devices, `--since`, `--until`, `--with-users`, `--state` for
  incremental export), `live` (several devices, `--reconnect`,
  `--backfill`, `--state` to catch up after a restart, `--count` reporting
  events left unemitted), `diagnose` (with
  `--probe-live` and scrubbed captures).
- Commands that change the device, all needing `--yes` and offering
  `--dry-run`: `time set`, `users add|update|delete`, `enroll`,
  `templates export|import` (also `--i-understand-biometric-data`),
  `attendance clear`, `data clear` (also the device name typed),
  `restart`, `poweroff`.
- Named devices in a config file; passwords from the command line,
  `ZK_PASSWORD`, the OS keyring or a prompt, never the file.
- Output as text, JSON or NDJSON. Attendance records and live events share
  one versioned schema (`docs/schema/event-v1.json`), including a
  deterministic `id` to deduplicate on (keyed on the device's serial
  number, so it survives renaming a profile; on logs without user IDs,
  `attendance pull` reads the user table so ids match live) and
  `user_id_source`. Documented
  exit codes.
- State, cursor and export files are written atomically and durably, with
  mode 0600.
- Shell completions; man pages via `cargo xtask man`.

### Known issues

- One zkctl test, `merges_devices_across_a_reconnect`, hung once in 12 runs
  of the live test suite and has not failed since in 100 runs, including
  15 under heavy CPU load. Suspected cause: slow reconnects under load
  with the test's short timeouts; whether it points at a bug in
  reconnecting is not established. Details in `docs/DECISIONS.md` (Known
  issues).

[Unreleased]: https://github.com/xeroxzen/ferrite-zk/compare/v0.1.0-alpha.2...HEAD
[0.1.0-alpha.2]: https://github.com/xeroxzen/ferrite-zk/releases/tag/v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/xeroxzen/ferrite-zk/releases/tag/v0.1.0-alpha.1
