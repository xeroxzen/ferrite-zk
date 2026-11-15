# Ferrite: crate layout

Repository [`xeroxzen/ferrite-zk`](https://github.com/xeroxzen/ferrite-zk). "Ferrite" is the project name; the crate names are
descriptive so that people searching for ZKTeco can find them. No crate uses
a bare `zk` prefix, because on crates.io that means zero-knowledge proofs.

| Crate | Type | Purpose | Status |
|---|---|---|---|
| `zkteco-proto` | lib | Sans-IO protocol: packets, checksums, comm key, records, time | 0.1.0-alpha.1 |
| `zkteco-client` | lib | Async tokio client over TCP and UDP | 0.1.0-alpha.1 |
| `zkctl` | bin + lib | CLI for operations and diagnostics against devices | 0.1.0-alpha.1 |
| `xtask` | bin | Man pages and completion scripts (`cargo xtask`) | never published |

## Workspace

```
ferrite-zk/
├── Cargo.toml                 # workspace, shared metadata, lints (clippy pedantic)
├── crates/
│   ├── zkteco-proto/
│   │   ├── src/
│   │   │   ├── command.rs     # Command (requests + replies, Unknown(u16)), EventFlags, table selectors
│   │   │   ├── packet.rs      # header, checksum, encode/decode, ChecksumPolicy, reused fields
│   │   │   ├── framing.rs     # TCP prefix + streaming TcpFrameDecoder
│   │   │   ├── session.rs     # session ID and reply-ID sequencing, event acks
│   │   │   ├── commkey.rs     # CMD_AUTH payload
│   │   │   ├── time.rs        # packed u32 and 6-byte compact time <-> NaiveDateTime
│   │   │   ├── text.rs        # TextEncoding: fixed-width NUL-padded fields
│   │   │   ├── requests.rs    # per-command payload encoders, small reply decoders
│   │   │   ├── transfer.rs    # chunk plan, ChunkReceiver, PrepareData, upload split
│   │   │   ├── records/       # Batch/RejectedRecord + user, attendance, finger,
│   │   │   │                  # event, sizes, options (decoders and encoders)
│   │   │   └── error.rs       # ProtoError
│   │   └── tests/
│   │       ├── fixtures/generated/   # from tools/gen_fixtures.py
│   │       ├── golden.rs      # byte-for-byte agreement with pyzk
│   │       ├── proptest.rs    # round trips; decoders total and terminating
│   │       └── captures.rs    # replays tests/fixtures/captures/*.json
│   ├── zkteco-client/
│   │   ├── src/
│   │   │   ├── client.rs      # connect/auth/disconnect, request/reply, timeouts
│   │   │   ├── transfer.rs    # staged chunked reads, drain, uploads
│   │   │   ├── data.rs        # users, attendance, templates
│   │   │   ├── device.rs      # info, options, time, control commands
│   │   │   ├── link.rs        # Link: framed TCP stream or connected UDP socket
│   │   │   ├── compat.rs      # COMPATIBILITY table (mirrors docs/COMPATIBILITY.md), device facts
│   │   │   ├── live/          # LiveCapture: capture loop, reconnect, backfill, dedup
│   │   │   ├── enroll.rs      # fingerprint enrolment
│   │   │   ├── write.rs       # user and template writes and deletes
│   │   │   ├── record.rs      # packet recording for diagnostics
│   │   │   ├── diag.rs        # UDP stream sampling (headers only)
│   │   │   ├── config.rs      # ClientConfig, Transport, RetryPolicy, READ_ONLY_COMMANDS
│   │   │   └── error.rs       # Error
│   │   ├── examples/          # run against the mock by tests/examples.rs
│   │   └── tests/
│   │       ├── support/       # mock device with fault injection
│   │       ├── tcp.rs         # client against the mock over TCP
│   │       ├── udp.rs         # client against the mock over UDP (loss, duplication, reordering)
│   │       ├── modes.rs       # Transport::Auto, read-only mode
│   │       └── hardware.rs    # real device, --features hardware
│   └── zkctl/
│       ├── src/
│       │   ├── main.rs        # parse, logging, runtime, exit code
│       │   ├── cli.rs         # clap definitions (also used by xtask)
│       │   ├── cmd/           # one module per command
│       │   ├── target.rs      # -d/--host -> ClientConfig, password lookup, connect
│       │   ├── profiles.rs    # config.toml
│       │   ├── secrets.rs     # OS keyring, password prompt
│       │   ├── output.rs      # human/json/ndjson records
│       │   ├── exit.rs        # exit codes and error classification
│       │   ├── diagnose.rs    # read-only survey -> COMPATIBILITY row
│       │   ├── live_probe.rs  # diagnose --probe-live
│       │   ├── capture.rs     # recording -> scrubbed capture JSON
│       │   └── streams.rs     # describes a UDP stream sample
│       └── tests/             # real binary against zkteco-client's mock (via #[path]); insta snapshots
├── xtask/                     # cargo xtask man|completions
├── tests/fixtures/captures/   # maintainer's device captures (excluded from crates)
├── tools/                     # GPL-2.0-only fixture generator (never packaged)
├── docs/{GUIDE,PROTOCOL,ARCHITECTURE,DECISIONS,COMPATIBILITY,RELEASING}.md
├── deny.toml                  # cargo-deny: licenses, advisories, sources
└── .github/workflows/ci.yml   # fmt, clippy, tests, docs, MSRV, deny, publish dry run, fixture drift
```

## Boundaries

- **`zkteco-proto` has no I/O and no async.** It provides the pieces of every
  exchange: build the request bytes, cut frames, decode replies, and validate
  multi-packet transfers. Encoders exist for device-side structures (tables,
  events, counters) too, so the mock device in `zkteco-client` reuses the
  same codecs.
- **The client orchestrates** each multi-step flow: connect and auth, staging
  and chunk loops with retries, uploads, enrolment and live capture. It owns
  sockets, timeouts, retry policy and reconnects.

## Client design

- **One request in flight per connection.** The protocol is strict
  request/response, so the client uses `&mut self` methods and needs no
  locking. For fleets, run one `Client` per device.
- **Live capture takes the connection.** `client.live_capture(opts)` consumes
  the `Client` and returns `LiveCapture`, a
  `Stream<Item = Result<LiveEvent>>`. `LiveCapture::stop().await`
  unsubscribes and gives the `Client` back. Dropping it closes the socket
  (there is no async drop). With `CaptureOptions::reconnect` it
  re-connects, re-authenticates and re-subscribes with backoff. The stream
  stores its in-flight step, so `next()` is cancel-safe (D29).
- **Enrolment** (`enroll_user`) is a plain `&mut self` method that reads
  enrolment reports until a result, then restores verify mode (D30).
- **Errors** (`zkteco_client::Error`): `Io`, `Timeout { command, after }`,
  `Unauthorized`, `DeviceError { command, reply }`, `Malformed(ProtoError)`,
  `UnsupportedRecordFormat { kind, detail }`, `ShortRead { expected, got }`,
  `ConnectionClosed`, `Unsupported`, `UserNotFound(UserId)`,
  `ReadOnly(Command)`, `InvalidArgument(ProtoError)`.
- **Timeouts close the connection**; retries cover connecting and corrupt
  chunks (D22). Over UDP a stream stalled mid-chunk is a retryable short
  read instead (D25).
- **Destructive operations** (`clear_attendance`, `clear_data`,
  `delete_user`, `set_user`, `poweroff`, `restart`, `enroll_user`) are tagged in rustdoc.
  `zkctl` requires `--yes` for them, and hardware tests run them only with
  `ZK_ALLOW_DESTRUCTIVE=1`.

## Dependencies

- `zkteco-proto`: `bytes`, `chrono` (no default features), `encoding_rs`,
  `thiserror`, `bitflags`, `tracing`. Dev: `proptest`, `serde_json`, `hex`.
- `zkteco-client`: `tokio` (net, time, io-util), plus the shared `bytes`,
  `chrono`, `thiserror`, `tracing`, and `futures-core` for the
  live-capture `Stream` trait. Dev: `futures-util`.
- `zkctl`: `clap`, `clap_complete`, `anyhow`, `serde`/`serde_json`, `toml`,
  `directories`, `keyring`, `rpassword`, `chrono`, `hex`,
  `tracing-subscriber`. Dev: `assert_cmd`, `insta`, `tempfile`.
- `xtask`: `clap_mangen`, `clap_complete`.

Every crate uses `unsafe_code = "forbid"`. The MSRV is 1.88 (D19).
