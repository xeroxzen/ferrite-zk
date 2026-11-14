# Ferrite: crate layout

Repository [`xeroxzen/ferrite-zk`](https://github.com/xeroxzen/ferrite-zk). "Ferrite" is the project name; the crate names are
descriptive so that people searching for ZKTeco can find them. No crate uses
a bare `zk` prefix, because on crates.io that means zero-knowledge proofs.

| Crate | Type | Purpose | Status |
|---|---|---|---|
| `zkteco-proto` | lib | Sans-IO protocol: packets, checksums, comm key, records, time | 0.1.0-alpha.1 (phase 2, done) |
| `zkteco-client` | lib | Async tokio client over TCP and UDP | unpublished; phases 3 to 5 |
| `zkctl` | bin | CLI for manual testing and ops against devices | unpublished; phase 6 |

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
│   ├── zkteco-client/         # phases 3 to 5
│   │   └── tests/support/     # mock device (tokio, TCP + UDP)
│   └── zkctl/                 # phase 6
├── tests/fixtures/captures/   # maintainer's device captures (excluded from crates)
├── tools/                     # GPL-2.0-only fixture generator (never packaged)
├── docs/{PROTOCOL,ARCHITECTURE,DECISIONS}.md
└── .github/workflows/ci.yml   # fmt, clippy -D warnings, tests, docs, MSRV, fixture drift
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

## Client design (phases 3 to 5)

- **One request in flight per connection.** The protocol is strict
  request/response, so the client uses `&mut self` methods and needs no
  locking. For fleets, run one `Client` per device.
- **Live capture takes the connection.** `client.live_capture(opts)` consumes
  the `Client` and returns `LiveCapture`, a
  `Stream<Item = Result<LiveEvent>>`. `LiveCapture::stop().await`
  unsubscribes and gives the `Client` back. Dropping it closes the socket
  (there is no async drop). A reconnecting variant owns the config and
  re-connects, re-authenticates and re-subscribes with backoff. The
  regression tests for unexpected payload lengths and coalesced TCP events
  are written against the mock before the stream is implemented.
- **Errors** (`zkteco_client::Error`): `Timeout`, `Unauthorized`,
  `DeviceError { reply }`, `Malformed`, `UnsupportedRecordFormat`,
  `ShortRead { expected, got }`, `ConnectionClosed`, `Io`, and wrapped
  `ProtoError`.
- **Destructive operations** (`clear_attendance`, `clear_data`,
  `delete_user`, `set_user`, `poweroff`, `restart`) are tagged in rustdoc.
  `zkctl` requires `--yes` for them, and hardware tests run them only with
  `ZK_ALLOW_DESTRUCTIVE=1`.

## Dependencies

- `zkteco-proto`: `bytes`, `chrono` (no default features), `encoding_rs`,
  `thiserror`, `bitflags`, `tracing`. Dev: `proptest`, `serde_json`, `hex`.
- `zkteco-client`, planned: `tokio`, `futures-core`, `pin-project-lite`.
- `zkctl`, planned: `clap`, `anyhow`, `tracing-subscriber`.

Every crate uses `unsafe_code = "forbid"`. The MSRV is 1.88 (D19).
