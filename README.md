# Ferrite

A Rust implementation of the ZKTeco device protocol (port 4370, TCP and UDP),
built for feature parity with [pyzk](https://github.com/fananimi/pyzk).

**Status:** phase 5 of 6. `zkteco-proto` (the sans-IO protocol layer) is
implemented and tested, version `0.1.0-alpha.1`. `zkteco-client` works over
TCP and UDP, with live capture and fingerprint enrolment (tested against a
mock device; not yet published). `zkctl` has `diagnose`; the rest of it is
phase 6.

**Hardware status: untested on physical devices.** Every layout and flow is
implemented from the protocol manual and the reference implementation and
tested against a mock device only. See
[Help wanted: device reports](#help-wanted-device-reports).

| Crate | Purpose |
|---|---|
| [`zkteco-proto`](crates/zkteco-proto) | Packets, checksums, comm key, record layouts, time encoding. No I/O. |
| [`zkteco-client`](crates/zkteco-client) | Async tokio client (TCP and UDP) |
| [`zkctl`](crates/zkctl) | CLI; `zkctl diagnose` so far (read-only) |

See [docs/PROTOCOL.md](docs/PROTOCOL.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md),
[docs/DECISIONS.md](docs/DECISIONS.md) and
[docs/COMPATIBILITY.md](docs/COMPATIBILITY.md).

## Transports

`ClientConfig::transport` defaults to `Transport::Auto`: TCP, falling back to
UDP if the device cannot be reached over TCP. Over UDP, table reads request
one datagram's worth at a time, because UDP can reorder datagrams and the
protocol gives no known way to detect it in larger chunks. pyzk's 16 KiB UDP
chunks are not offered: they can return wrong records without an error
(D25).

## Surveying a device

`zkctl diagnose` connects read-only (the client refuses any command outside
an allowlist) and prints a JSON report shaped like a
[COMPATIBILITY.md](docs/COMPATIBILITY.md) row:

```sh
ZK_PASSWORD=0 cargo run -p zkctl -- diagnose 192.168.1.201 \
    --capture ./captures -o report.json
```

Serial number and MAC are scrubbed unless `--include-identifiers` is given.
`--capture` writes scrubbed packet captures in the
[tests/fixtures/captures](tests/fixtures/captures/README.md) format; review
them before committing.

## Live capture

```rust
let client = Client::connect(ClientConfig::new("192.168.1.201")).await?;
let mut capture = client.live_capture(CaptureOptions::default()).await?;
while let Some(event) = capture.next().await {
    if let LiveEvent::Attendance(punch) = event?.event {
        println!("{:?} at {}", punch.user_id, punch.timestamp);
    }
}
let (client, result) = capture.stop().await;
```

`next()` is cancel-safe, so it can sit in a `tokio::select!` with a
shutdown signal. Each item is a `CapturedEvent`: the event, its source
(live or backfill), the device time, and the host's wall-clock and
monotonic arrival times. `CaptureOptions::reconnect` keeps the stream alive
across dropped connections, and `backfill` then reads the punches stored
while disconnected, delivering each punch once. TCP keepalive and a
heartbeat that tolerates missed answers detect a device that went away
without closing the connection (D29, D31).

## Help wanted: device reports

The project has no device to test against. If you own a ZKTeco device,
running `zkctl diagnose` against it is the most useful contribution you can
make. It is read-only: the client refuses every command outside a fixed
allowlist of reads before anything is sent, so it cannot change users,
logs or settings.

```sh
ZK_PASSWORD=0 cargo run -p zkctl -- diagnose <device-ip> --probe-live -o report.json
```

`--probe-live` listens for live events for 60 seconds after the table
reads; punch in on the device a few times meanwhile. It reports only event
flags and payload lengths, and whether the device answers requests while
subscribed.

The report holds the firmware version, platform, record layouts, counts and
packet sizes. Serial number and MAC are scrubbed by default; it contains no
user records. Please read it before sharing, then open an issue with the
report attached and the device model from its label. Each report becomes a
row in [COMPATIBILITY.md](docs/COMPATIBILITY.md) and answers some of the
open questions in [DECISIONS.md](docs/DECISIONS.md#open-questions-blocked-on-zkctl-diagnose-reports-from-hardware).
Packet captures (`--capture DIR`) help even more; they are scrubbed too, but
review them before attaching.

## Parity table

"Unverified" means the wire layout has not been confirmed by a capture from
real hardware in `tests/fixtures/captures/`. Today that is every layout (see
D16 in DECISIONS.md). "Proto" is the `zkteco-proto` support; "Client" is the
`zkteco-client` method. "client (TCP, UDP)" means implemented and tested against
the mock device over both transports.

| pyzk | Client | Proto support | Status | Unverified | HW tested |
|---|---|---|---|---|---|
| `connect` | `Client::connect` | `session`, `commkey`, `framing` | client (TCP, UDP) | yes | N |
| `disconnect` | `Client::disconnect` | `Command::Exit` | client (TCP, UDP) | yes | N |
| `enable_device` / `disable_device` | `enable_device` / `disable_device` | commands | client (TCP, UDP) | yes | N |
| `restart` / `poweroff` | `restart` / `poweroff` (destructive) | commands | client (TCP, UDP) | yes | N |
| `test_voice` | `test_voice` | `requests::test_voice` | client (TCP, UDP) | yes | N |
| `refresh_data` | `refresh_data` | command | client (TCP, UDP) | yes | N |
| `read_sizes` | `read_sizes` | `records::sizes` | client (TCP, UDP) | yes | N |
| `free_data` | internal | command | client (TCP, UDP) | yes | N |
| `get_firmware_version` | `firmware_version` | `requests::decode_cstring` | client (TCP, UDP) | yes | N |
| `get_serialnumber`, `get_platform`, `get_device_name`, `get_mac` | `serial_number`, `platform`, `device_name`, `mac` | `records::options` | client (TCP, UDP) | yes | N |
| `get_face_version`, `get_fp_version`, `get_extend_fmt`, `get_user_extend_fmt`, `get_face_fun_on`, `get_compat_old_firmware` | same names without `get_` | `records::options` | client (TCP, UDP) | yes | N |
| `get_network_params` | `network_params` | `records::options` | client (TCP, UDP) | yes | N |
| `get_pin_width` | `pin_width` | `requests::decode_pin_width` | client (TCP, UDP) | yes | N |
| `get_time` / `set_time` | `time` / `set_time` | `time`, `requests` | client (TCP, UDP) | yes | N |
| `get_users` | `users` -> `Batch<User>` | `records::user` (28/72/120) | client (TCP, UDP) | yes | N |
| `set_user` | `set_user` (destructive) | `user::encode_record` | client (TCP, UDP) | yes | N |
| `delete_user` | `delete_user(Uid)`, `delete_user_by_user_id(&UserId)` (destructive) | `requests::delete_user` | client (TCP, UDP) | yes | N |
| `next_uid` / `next_user_id` | `user::next_free_ids` | done | proto done | n/a | n/a |
| `get_templates` | `templates` | `records::finger` | client (TCP, UDP) | yes | N |
| `get_user_template` | `user_template(Uid, fid)`, `user_template_by_user_id(&UserId, fid)` | `finger::clean_single` | client (TCP, UDP) | yes | N |
| `save_user_template`, `HR_save_usertemplates` | `save_user_templates` | `finger::encode_bulk_save` | client (TCP, UDP) | yes | N |
| `delete_user_template` (by uid) | `delete_user_template` | `requests::template_by_uid` | client (TCP, UDP) | yes | N |
| `delete_user_template` (by user ID) | `delete_user_template_by_user_id` (TCP: command 134; UDP: resolves the uid, as pyzk) | `requests::template_by_user_id` | client (TCP, UDP) | **yes, never exercised by pyzk** | N (on HW list) |
| `enroll_user` | `enroll_user(&UserId, fid, opts, progress)` (destructive; no by-uid form) | `requests::start_enroll_*`, `event::decode_enroll_report` | client (TCP, UDP) | yes (report semantics, D30) | N |
| `cancel_capture` / `verify_user` | same | commands | client (TCP, UDP) | yes | N |
| `get_attendance` | `attendance` (as stored), `attendance_with_users` (user IDs resolved, D23) | `records::attendance` (8/16/40/49) | client (TCP, UDP) | yes | N |
| `clear_attendance` / `clear_data` | same (destructive) | commands | client (TCP, UDP) | yes | N |
| `live_capture` | `live_capture` -> `LiveCapture` (`Stream` of `LiveEvent`), `LiveCapture::connect` | `records::event` (`LiveEvent`), `TcpFrameDecoder` | client (TCP, UDP) | yes | N |
| `reg_event` | `CaptureOptions::events` | `requests::reg_event` | client (TCP, UDP) | yes | N |
| `end_live_capture` | `LiveCapture::stop` | `requests::reg_event` (empty mask) | client (TCP, UDP) | yes | N |
| `unlock` / `get_lock_state` | `unlock` / `lock_state` | `requests::unlock` | client (TCP, UDP) | yes (lock state payload unknown) | N |
| `write_lcd` / `clear_lcd` | same | `requests::write_lcd` | client (TCP, UDP) | yes | N |
| `set_sdk_build_1` | `set_option` | `options::encode_write` | client (TCP, UDP) | yes | N |
| `read_with_buffer` | internal | `transfer` | client (TCP, UDP) | yes | N |
| `ZK_helper.test_ping` / `test_tcp` | `probe` (TCP connect, or UDP `Connect` + `Exit`; no ICMP) | n/a | client (TCP, UDP) | n/a | N |

## Testing

```sh
cargo test --workspace            # unit, golden, property, capture and mock-device tests
```

Hardware tests run against a real device and are opt-in:

```sh
ZK_HOST=192.168.1.201 ZK_PORT=4370 ZK_PASSWORD=0 \
    cargo test -p zkteco-client --features hardware --test hardware -- --nocapture
```

`ZK_TRANSPORT` is `tcp` (default), `udp` or `auto`.

Tests that change device data (a temporary user, a copied template deleted
by user ID, setting the clock to its current value) run only with
`ZK_ALLOW_DESTRUCTIVE=1` and clean up after themselves. Output never
includes names, user IDs, cards, serial numbers or MAC addresses.

## License

The crates are licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. `tools/` is GPL-2.0-only and is
not part of any crate (see [tools/README.md](tools/README.md)).
