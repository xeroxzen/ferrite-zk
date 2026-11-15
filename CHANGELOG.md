# Changelog

All notable changes to the crates in this repository are recorded here. The
format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the crates follow [Semantic Versioning](https://semver.org/). The three
crates share one version.

## [Unreleased]

## [0.1.0-alpha.1]

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
  deterministic `id` to deduplicate on and `user_id_source`. Documented
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

[Unreleased]: https://github.com/xeroxzen/ferrite-zk/compare/v0.1.0-alpha.1...HEAD
[0.1.0-alpha.1]: https://github.com/xeroxzen/ferrite-zk/releases/tag/v0.1.0-alpha.1
