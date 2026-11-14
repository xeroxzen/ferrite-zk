# Ferrite

A Rust implementation of the ZKTeco device protocol (port 4370, TCP and UDP),
built for feature parity with [pyzk](https://github.com/fananimi/pyzk).

**Status:** phase 3 of 6. `zkteco-proto` (the sans-IO protocol layer) is
implemented and tested, version `0.1.0-alpha.1`. `zkteco-client` works over
TCP (tested against a mock device; not yet published). UDP, live capture,
enrolment and `zkctl` are still to come.

| Crate | Purpose |
|---|---|
| [`zkteco-proto`](crates/zkteco-proto) | Packets, checksums, comm key, record layouts, time encoding. No I/O. |
| [`zkteco-client`](crates/zkteco-client) | Async tokio client (TCP so far) |
| [`zkctl`](crates/zkctl) | CLI for manual testing (planned) |

See [docs/PROTOCOL.md](docs/PROTOCOL.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
and [docs/DECISIONS.md](docs/DECISIONS.md).

## Parity table

"Unverified" means the wire layout has not been confirmed by a capture from
real hardware in `tests/fixtures/captures/`. Today that is every layout (see
D16 in DECISIONS.md). "Proto" is the `zkteco-proto` support; "Client" is the
`zkteco-client` method. "client (TCP)" means implemented and tested against
the mock device over TCP; UDP is phase 4.

| pyzk | Client | Proto support | Status | Unverified | HW tested |
|---|---|---|---|---|---|
| `connect` | `Client::connect` | `session`, `commkey`, `framing` | client (TCP) | yes | N |
| `disconnect` | `Client::disconnect` | `Command::Exit` | client (TCP) | yes | N |
| `enable_device` / `disable_device` | `enable_device` / `disable_device` | commands | client (TCP) | yes | N |
| `restart` / `poweroff` | `restart` / `poweroff` (destructive) | commands | client (TCP) | yes | N |
| `test_voice` | `test_voice` | `requests::test_voice` | client (TCP) | yes | N |
| `refresh_data` | `refresh_data` | command | client (TCP) | yes | N |
| `read_sizes` | `read_sizes` | `records::sizes` | client (TCP) | yes | N |
| `free_data` | internal | command | client (TCP) | yes | N |
| `get_firmware_version` | `firmware_version` | `requests::decode_cstring` | client (TCP) | yes | N |
| `get_serialnumber`, `get_platform`, `get_device_name`, `get_mac` | `serial_number`, `platform`, `device_name`, `mac` | `records::options` | client (TCP) | yes | N |
| `get_face_version`, `get_fp_version`, `get_extend_fmt`, `get_user_extend_fmt`, `get_face_fun_on`, `get_compat_old_firmware` | same names without `get_` | `records::options` | client (TCP) | yes | N |
| `get_network_params` | `network_params` | `records::options` | client (TCP) | yes | N |
| `get_pin_width` | `pin_width` | `requests::decode_pin_width` | client (TCP) | yes | N |
| `get_time` / `set_time` | `time` / `set_time` | `time`, `requests` | client (TCP) | yes | N |
| `get_users` | `users` -> `Batch<User>` | `records::user` (28/72/120) | client (TCP) | yes | N |
| `set_user` | `set_user` (destructive) | `user::encode_record` | client (TCP) | yes | N |
| `delete_user` | `delete_user(Uid)`, `delete_user_by_user_id(&UserId)` (destructive) | `requests::delete_user` | client (TCP) | yes | N |
| `next_uid` / `next_user_id` | `user::next_free_ids` | done | proto done | n/a | n/a |
| `get_templates` | `templates` | `records::finger` | client (TCP) | yes | N |
| `get_user_template` | `user_template(Uid, fid)`, `user_template_by_user_id(&UserId, fid)` | `finger::clean_single` | client (TCP) | yes | N |
| `save_user_template`, `HR_save_usertemplates` | `save_user_templates` | `finger::encode_bulk_save` | client (TCP) | yes | N |
| `delete_user_template` (by uid) | `delete_user_template` | `requests::template_by_uid` | client (TCP) | yes | N |
| `delete_user_template` (by user ID, TCP) | `delete_user_template_by_user_id` | `requests::template_by_user_id` | client (TCP) | **yes, never exercised by pyzk** | N (on HW list) |
| `enroll_user` | `enroll_user` | `requests::start_enroll_*`, `event::decode_enroll_report` | proto done; client phase 5 | yes | N |
| `cancel_capture` / `verify_user` | same | commands | client (TCP) | yes | N |
| `get_attendance` | `attendance` (as stored), `attendance_with_users` (user IDs resolved, D23) | `records::attendance` (8/16/40/49) | client (TCP) | yes | N |
| `clear_attendance` / `clear_data` | same (destructive) | commands | client (TCP) | yes | N |
| `live_capture` | `live_capture` -> `Stream` of `LiveEvent` | `records::event` (`LiveEvent`), `TcpFrameDecoder` | proto done; client phase 5 | yes | N |
| `reg_event` | internal | `requests::reg_event` | proto done; client phase 5 | yes | N |
| `unlock` / `get_lock_state` | `unlock` / `lock_state` | `requests::unlock` | client (TCP) | yes (lock state payload unknown) | N |
| `write_lcd` / `clear_lcd` | same | `requests::write_lcd` | client (TCP) | yes | N |
| `set_sdk_build_1` | `set_option` | `options::encode_write` | client (TCP) | yes | N |
| `read_with_buffer` | internal | `transfer` | client (TCP) | yes | N |
| `ZK_helper.test_ping` / `test_tcp` | `probe` (TCP connect, no ICMP) | n/a | client (TCP) | n/a | N |

## Testing

```sh
cargo test --workspace            # unit, golden, property, capture and mock-device tests
```

Hardware tests run against a real device and are opt-in:

```sh
ZK_HOST=192.168.1.201 ZK_PORT=4370 ZK_PASSWORD=0 \
    cargo test -p zkteco-client --features hardware --test hardware -- --nocapture
```

Tests that change device data (a temporary user, a copied template deleted
by user ID, setting the clock to its current value) run only with
`ZK_ALLOW_DESTRUCTIVE=1` and clean up after themselves. Output never
includes names, user IDs, cards, serial numbers or MAC addresses.

## License

The crates are licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. `tools/` is GPL-2.0-only and is
not part of any crate (see [tools/README.md](tools/README.md)).
