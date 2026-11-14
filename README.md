# Ferrite

A Rust implementation of the ZKTeco device protocol (port 4370, TCP and UDP),
built for feature parity with [pyzk](https://github.com/fananimi/pyzk).

**Status:** phase 2 of 6. `zkteco-proto` (the sans-IO protocol layer) is
implemented and tested, version `0.1.0-alpha.1`. `zkteco-client` and `zkctl`
are in development and not published.

| Crate | Purpose |
|---|---|
| [`zkteco-proto`](crates/zkteco-proto) | Packets, checksums, comm key, record layouts, time encoding. No I/O. |
| [`zkteco-client`](crates/zkteco-client) | Async tokio client (planned) |
| [`zkctl`](crates/zkctl) | CLI for manual testing (planned) |

See [docs/PROTOCOL.md](docs/PROTOCOL.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
and [docs/DECISIONS.md](docs/DECISIONS.md).

## Parity table

"Unverified" means the wire layout has not been confirmed by a capture from
real hardware in `tests/fixtures/captures/`. Today that is every layout (see
D16 in DECISIONS.md). "Proto" is the `zkteco-proto` support that exists now.
"Client" lists the planned `zkteco-client` method.

| pyzk | Client (planned) | Proto support | Status | Unverified | HW tested |
|---|---|---|---|---|---|
| `connect` | `Client::connect` | `session`, `commkey`, `framing` | proto done | yes | N |
| `disconnect` | `Client::disconnect` | `Command::Exit` | proto done | yes | N |
| `enable_device` / `disable_device` | `enable_device` / `disable_device` | commands | proto done | yes | N |
| `restart` / `poweroff` | `restart` / `poweroff` (destructive) | commands | proto done | yes | N |
| `test_voice` | `test_voice` | `requests::test_voice` | proto done | yes | N |
| `refresh_data` | `refresh_data` | command | proto done | yes | N |
| `read_sizes` | `read_sizes` | `records::sizes` | proto done | yes | N |
| `free_data` | internal | command | proto done | yes | N |
| `get_firmware_version` | `firmware_version` | `requests::decode_cstring` | proto done | yes | N |
| `get_serialnumber`, `get_platform`, `get_device_name`, `get_mac` | `serial_number`, `platform`, `device_name`, `mac` | `records::options` | proto done | yes | N |
| `get_face_version`, `get_fp_version`, `get_extend_fmt`, `get_user_extend_fmt`, `get_face_fun_on`, `get_compat_old_firmware` | same names without `get_` | `records::options` | proto done | yes | N |
| `get_network_params` | `network_params` | `records::options` | proto done | yes | N |
| `get_pin_width` | `pin_width` | `requests::decode_pin_width` | proto done | yes | N |
| `get_time` / `set_time` | `time` / `set_time` | `time`, `requests` | proto done | yes | N |
| `get_users` | `users` -> `UserBatch` | `records::user` (28/72/120) | proto done | yes | N |
| `set_user` | `set_user` (destructive) | `user::encode_record` | proto done | yes | N |
| `delete_user` | `delete_user` (destructive) | `requests::delete_user` | proto done | yes | N |
| `next_uid` / `next_user_id` | `user::next_free_ids` | done | proto done | n/a | n/a |
| `get_templates` | `templates` | `records::finger` | proto done | yes | N |
| `get_user_template` | `user_template` | `finger::clean_single` | proto done | yes | N |
| `save_user_template`, `HR_save_usertemplates` | `save_user_templates` | `finger::encode_bulk_save` | proto done | yes | N |
| `delete_user_template` (by uid) | `delete_user_template` | `requests::template_by_uid` | proto done | yes | N |
| `delete_user_template` (by user ID, TCP) | `delete_user_template_by_user_id` | `requests::template_by_user_id` | proto done | **yes, never exercised by pyzk** | N (on HW list) |
| `enroll_user` | `enroll_user` | `requests::start_enroll_*`, `event::decode_enroll_report` | proto done | yes | N |
| `cancel_capture` / `verify_user` | same | commands | proto done | yes | N |
| `get_attendance` | `attendance` -> `AttendanceBatch` | `records::attendance` (8/16/40/49) | proto done | yes | N |
| `clear_attendance` / `clear_data` | same (destructive) | commands | proto done | yes | N |
| `live_capture` | `live_capture` -> `Stream` of `LiveEvent` | `records::event` (`LiveEvent`), `TcpFrameDecoder` | proto done | yes | N |
| `reg_event` | internal | `requests::reg_event` | proto done | yes | N |
| `unlock` / `get_lock_state` | `unlock` / `lock_state` | `requests::unlock` | proto done | yes (lock state payload unknown) | N |
| `write_lcd` / `clear_lcd` | same | `requests::write_lcd` | proto done | yes | N |
| `set_sdk_build_1` | `set_option` | `options::encode_write` | proto done | yes | N |
| `read_with_buffer` | internal | `transfer` | proto done | yes | N |
| `ZK_helper.test_ping` / `test_tcp` | `Client::probe` (TCP / `CMD_CONNECT`, no ICMP) | n/a | planned | n/a | N |

## License

The crates are licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. `tools/` is GPL-2.0-only and is
not part of any crate (see [tools/README.md](tools/README.md)).
