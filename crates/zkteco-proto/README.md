# zkteco-proto

Sans-IO implementation of the ZKTeco device protocol (the "ZK protocol"
spoken on port 4370 over TCP and UDP), with behavioural parity with
[pyzk](https://github.com/fananimi/pyzk). Part of the
[Ferrite](https://github.com/xeroxzen/ferrite-zk) project.

This crate only turns bytes into typed values and back. It opens no sockets,
so every part of it can be tested directly and driven by any I/O layer. The
async client is `zkteco-client` (in development).

- [`packet`]: the 8-byte header, checksum, encode/decode.
- [`framing`]: TCP framing and a streaming frame decoder.
- [`session`]: session ID and `reply_id` sequencing.
- [`commkey`]: the `CMD_AUTH` payload derived from the device password.
- [`time`]: device timestamp encodings.
- [`records`]: users, attendance, templates, live events, counters, options.
- [`requests`]: payload encoders for individual commands.
- [`transfer`]: chunked reads and uploads.

**Status: alpha.** Every record layout is implemented from the protocol
manual and cross-checked byte for byte against pyzk, but none is yet
confirmed by a capture from real hardware. Such layouts are marked
"Unverified against hardware" in their docs.

## Building a request

```rust
use zkteco_proto::framing::{encode_tcp, TcpFrameDecoder};
use zkteco_proto::session::Session;
use zkteco_proto::{packet, ChecksumPolicy, Command};

let mut session = Session::new();
session.set_session_id(0x2A51); // from the device's reply to CMD_CONNECT
let (reply_id, request) = session.request(Command::GetTime, &[]);
assert_eq!(reply_id, 0); // the first request after CMD_CONNECT
let on_the_wire = encode_tcp(&request);

// Reading side: frames may arrive split or coalesced.
let mut frames = TcpFrameDecoder::default();
frames.extend(&on_the_wire[..5]);
assert!(frames.next_packet()?.is_none());
frames.extend(&on_the_wire[5..]);
let raw = frames.next_packet()?.expect("one whole frame");
let p = packet::decode(raw, ChecksumPolicy::Reject)?;
assert_eq!(p.header.command, Command::GetTime);
# Ok::<(), zkteco_proto::ProtoError>(())
```

## Decoding a table without losing records

Table decoders return a [`Batch`]: the records that decoded, plus every
rejected record with its offset, raw bytes and reason. One bad record never
aborts a download.

```rust
use chrono::NaiveDate;
use zkteco_proto::records::attendance::{
    self, Attendance, AttendanceFormat, AttendanceStatus, Punch,
};
use zkteco_proto::{TextEncoding, Uid};

let at = NaiveDate::from_ymd_opt(2026, 3, 2).unwrap().and_hms_opt(8, 59, 4).unwrap();
let mut punch = Attendance::new(at, AttendanceStatus::Fingerprint, Punch::CheckIn);
punch.uid = Some(Uid(7));
punch.user_id = Some("1007".into());
let table = attendance::encode_table(&[punch.clone()], AttendanceFormat::Long40, TextEncoding::UTF8)?;

let batch = attendance::decode_table(&table, 1, TextEncoding::UTF8)?;
assert!(batch.is_clean());
assert_eq!(batch.records, [punch]);
# Ok::<(), zkteco_proto::ProtoError>(())
```

## Live events

A live event packet decodes to a list of [`LiveEvent`]s. Bytes that are not
attendance are kept as `LiveEvent::Raw`; nothing is dropped.

```rust
use zkteco_proto::records::event::{self, LiveEvent};
use zkteco_proto::{packet, ChecksumPolicy, Command, EventFlags, TextEncoding};

// EF_ALARM (0x200) with a 4-byte payload: not attendance, returned raw.
let raw = packet::encode(Command::RegEvent, 0x200, 0, &[55, 0, 0, 0]).freeze();
let p = packet::decode(raw, ChecksumPolicy::Reject)?;
let events = event::decode(&p, TextEncoding::UTF8)?;
assert!(matches!(&events[..], [LiveEvent::Raw { flag, .. }] if *flag == EventFlags::ALARM));
# Ok::<(), zkteco_proto::ProtoError>(())
```

## Documentation

The repository's
[`docs/PROTOCOL.md`](https://github.com/xeroxzen/ferrite-zk/blob/main/docs/PROTOCOL.md)
describes the protocol in full, and
[`docs/DECISIONS.md`](https://github.com/xeroxzen/ferrite-zk/blob/main/docs/DECISIONS.md)
records every place this crate deliberately differs from pyzk.

## License

Licensed under either of MIT or Apache-2.0, at your option.

[`packet`]: https://docs.rs/zkteco-proto/latest/zkteco_proto/packet/
[`framing`]: https://docs.rs/zkteco-proto/latest/zkteco_proto/framing/
[`session`]: https://docs.rs/zkteco-proto/latest/zkteco_proto/session/
[`commkey`]: https://docs.rs/zkteco-proto/latest/zkteco_proto/commkey/
[`time`]: https://docs.rs/zkteco-proto/latest/zkteco_proto/time/
[`records`]: https://docs.rs/zkteco-proto/latest/zkteco_proto/records/
[`requests`]: https://docs.rs/zkteco-proto/latest/zkteco_proto/requests/
[`transfer`]: https://docs.rs/zkteco-proto/latest/zkteco_proto/transfer/
[`Batch`]: https://docs.rs/zkteco-proto/latest/zkteco_proto/struct.Batch.html
[`LiveEvent`]: https://docs.rs/zkteco-proto/latest/zkteco_proto/records/event/enum.LiveEvent.html
