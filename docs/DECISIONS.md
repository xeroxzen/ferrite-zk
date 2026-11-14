# Decisions

Each entry records what we decided, why, and where it applies. "pyzk"
always means [fananimi/pyzk](https://github.com/fananimi/pyzk) at
`1af361a0`, used as a behavioural cross-reference.

## D1. License and clean-room discipline (approved, phase 1)

- The crates are **MIT OR Apache-2.0**.
- The **ZKTeco 2006 communication protocol manual** is the primary source.
  pyzk is a behavioural cross-reference only: we observe what it sends and how
  it interprets replies.
- No pyzk code, comments or identifiers are copied, beyond protocol constant
  names. `docs/PROTOCOL.md` describes protocol facts, not pyzk's code structure.
- pyzk's test captures are **not** committed. Golden fixtures come from
  (a) `tools/gen_fixtures.py` and (b) the maintainer's own device captures in
  `tests/fixtures/captures/`.
- `tools/` is **GPL-2.0-only** (it imports pyzk) and has its own `LICENSE`.
  It sits outside every crate root, so `cargo package` cannot include it. Each
  manifest also lists `/tools` and `/tests/fixtures/captures` in `exclude` as a
  guard.
- The fixture JSON is program output (plain data), committed under the crate.
- Phases 1 and 2 were squashed into one commit before the repository was
  published, after a sweep for byte strings from pyzk's test captures. The
  sweep compared every 8-byte window of the hex, escaped-byte and byte-array
  literals in the tree with those in pyzk's test files. The only matches left
  are structural: the TCP prefix with a frame length, one- to four-digit
  ASCII IDs padded with NULs (random data from the generator), and the fixed
  request that stages the user table.

## D2. Checksums (approved)

Outgoing packets use the standard 16-bit ones'-complement checksum, which is
byte-identical to pyzk's output (verified on 200,000 random packets and by
the `packets` golden fixtures). Incoming packets are checked according to
`ChecksumPolicy::{Warn, Reject, Ignore}`. The default is `Warn`, which logs
via `tracing::warn!` and accepts the packet.

## D3. Privileges (approved)

There is no silent coercion. `Privilege::{User, Enroller, Manager, Admin,
Unknown(u8)}` decodes the role bits (1 to 3). The disabled flag (bit 0) is a
separate `User::disabled: bool`. Unknown values read from a device are kept
as `Unknown(role_bits)`. Writing `Unknown` is
`ProtoError::InvalidPrivilege`. pyzk instead forces every privilege other
than 0 and 14 to 0 on write, which clears the disabled flag and demotes
managers and enrollers.

## D4. Per-record error isolation (approved)

Table decoders return `Batch<T> { records, rejected: Vec<RejectedRecord> }`,
with `AttendanceBatch = Batch<Attendance>`. `RejectedRecord` carries the raw
bytes, the byte offset and a typed reason (`ProtoError`). The same applies
to users, templates and live-event payloads. Every rejection is also logged
at `warn`. A table that is shorter than announced, a trailing partial
record, and bytes past the announced length each produce a rejection, so
nothing disappears silently.

Evidence that this is needed: pyzk issues #211 and #221 report real devices
returning impossible dates such as 30 February, which abort pyzk's whole
download. Records that decode validly but look like padding (for example
#211's uid 255 with an empty user ID and time 2000-01-01 00:00:00) are
**kept**, because we cannot tell them from real data.

## D5. Strict format detection

The record size is `table_len / count`, which must divide exactly and match
a known size: users 28/72/120, attendance 8/16/40/49. Anything else is
`UnsupportedRecordFormat`. pyzk silently falls back (users) or floors the
division (attendance). Callers that know the format can bypass detection
with `decode_table_as`. The client will expose this as a config override.

## D6. Time

- All timestamps are `chrono::NaiveDateTime` in **device-local wall-clock
  time**, because the wire carries no zone. Converting them is the caller's
  job.
- `encode_packed` returns `YearOutOfRange` outside 2000 to 2099 instead of
  wrapping the year modulo 100.
- An impossible date is `InvalidTimestamp`. Inside tables this becomes a
  per-record rejection (D4).

## D7. Text fields

- Text is decoded with a configurable `TextEncoding` (WHATWG labels via
  `encoding_rs`; default UTF-8). pyzk accepts Python codec names, and some of
  them are spelled differently (for example `cp1252` here is
  `windows-1252`).
- Invalid byte sequences become U+FFFD, so problems stay visible. pyzk drops
  them.
- Values are **not** whitespace-trimmed. pyzk strips names and groups.
- An empty name stays empty. `User::display_name()` gives pyzk's `NN-<user_id>`
  fallback.
- On write, a value that is too long for its field is `FieldTooLong` rather
  than truncated. Characters the encoding cannot represent are
  `Unencodable`. A value may fill its field exactly, with no terminating NUL;
  that matches what pyzk sends.

## D8. Comm key overflow

`commkey::derive` adds the session ID with 32-bit wrap-around. This matches
the C SDK the algorithm comes from. pyzk raises `struct.error` instead (only
for very large passwords).

## D9. Live events (revised after phase 2 review)

`event::decode` turns one `RegEvent` packet into `Vec<LiveEvent>`, where
`LiveEvent` is `Attendance(Attendance)` or `Raw { flag, payload }`. Nothing
is ever dropped.

- **Gating.** The payload is decoded as attendance if the header's event
  flags (in the `session_id` field) include `ATTLOG`, **or** if the flags are
  zero and the payload length splits exactly into known event-record lengths
  (`is_attendance_length`). The second case logs `tracing::warn!`. Any other
  packet is one `Raw` with the whole payload. A non-zero flag without
  `ATTLOG` is never guessed as attendance.
- **Why.** pyzk parses every event packet as attendance and never checks the
  flag, so firmware that omits the flag would work with pyzk and silently
  lose events with a strict check. The length fallback keeps parity there
  while the warning makes it visible. **Unverified:** no capture yet shows
  whether such firmware exists.
- **Records.** They are chosen by remaining length: exactly 10/12/14/32/36/37,
  or 52 when at least 52 bytes remain. Concatenated 52-byte records are
  supported. A record that fails to decode (e.g. an impossible date) becomes
  `Raw` with that record's bytes; later records still decode. A remainder of
  unknown length becomes `Raw` with those bytes, and decoding always
  terminates. pyzk drops such fragments or loops on them.
- An empty payload yields one empty `Raw`, so every packet produces at least
  one event.
- `decode_attendance_payload` (the `Batch` form, for callers that want typed
  rejection reasons) is unchanged.
- Multiple events in one TCP read are handled by `TcpFrameDecoder`.

## D10. Transfers

`ChunkReceiver` checks the announced size against the requested size, TCP
sequence numbers, short streams (`ShortRead`) and overruns. pyzk returns
partial UDP data as success. The client surfaces it as
`Error::ShortRead { expected, got }` on both transports (D25). Orchestration (staging,
chunk loop, retries, `FreeData`) lives in the client. `zkteco-proto`
provides the plan (`chunk_plan`), the reply state machine and the payload
codecs.

## D11. Options

`decode_value` returns the text after the first `=`, up to the first NUL.
pyzk also deletes every `=` from three values (serial number, platform,
fingerprint version). We keep values unchanged.

## D12. 49-byte attendance records

There are two incompatible reports:

- pyzk handles 49 bytes as a 9-byte marker `ff '255' 00×5` followed by the
  40-byte body. Issue #174 (MB560-VL) confirms 49-byte records exist.
- Unmerged PR #247 (MB40-VL) parses the 40-byte body's fields at offset 0,
  followed by 17 unknown bytes.

The decoder picks the layout per record: marker present means prefixed,
otherwise the PR #247 layout. Both are unverified. The encoder emits the
prefixed form.

## D13. Single-template cleanup

The reply to `GetUserTemp` has its final byte removed and, if it then ends
in six zero bytes, those too. This matches pyzk. It is a heuristic and would
corrupt a template that really ends in six zeros. Kept for parity, documented
on `finger::clean_single`, unverified.

## D14. Reachability (approved, for phase 3)

There is no ICMP ping and no shelling out. "Reachable" means a TCP connect
or a `CMD_CONNECT` reply within the timeout. The `ommit_ping` equivalent
skips the separate pre-check.

## D15. Template delete by user ID over TCP (approved)

`requests::template_by_user_id` (command 134, 24-byte user ID + finger
index) is implemented from the protocol description. pyzk's own
implementation of this path cannot run on Python 3, so it has never been
exercised. It is marked unverified and listed for hardware testing.

## D16. "Unverified against hardware" marking

Every record layout and undocumented field carries
`#[doc = "Unverified against hardware."]`. For now that is all of them:
the crate has no hardware captures yet. pyzk's test suite includes captures
for some layouts, but D1 keeps them out of this repository and they are not
our evidence. The marking comes off per layout once a capture in
`tests/fixtures/captures/` covers it.

## D17. One `Command` enum

Request and reply codes share the header's first field, and their numeric
ranges do not collide, so a single `Command` enum (with `Unknown(u16)`)
covers both. This replaces the separate `Reply` enum proposed in
ARCHITECTURE.md. `Command::is_success_reply()` captures which replies mean
success (`AckOk`, `PrepareData`, `Data`).

## D18. Field widths for uid-based commands

`DeleteUserTemp` and `GetUserTemp` carry the uid as a `u16`. pyzk packs a
signed `i16`, which gives the same bytes up to 32767 and fails above it. Bulk
template saves reject a finger index above 9.

## D19. MSRV 1.88

Current `encoding_rs` and `proptest` require Rust 1.88. Rust 1.88 is over a
year old, and CI tests it.

## D20. Repository, copyright and publishing

- `repository` is `https://github.com/xeroxzen/ferrite-zk`.
- Copyright holder: "Andile Jaden Mbele and the Ferrite contributors"
  (`LICENSE-MIT`). `LICENSE-APACHE` is the unmodified license text and there
  is no `NOTICE` file. Manifests do not use the optional `authors` field.
- `zkteco-proto` is `0.1.0-alpha.1`, prepared and dry-run only; the
  maintainer publishes it. `zkteco-client` and `zkctl` are `publish = false`
  until they have real functionality. The workspace shares one version.

## D21. Biometric and personal data in captures

Real fingerprint or face templates are never committed. Template payloads in
captures are replaced with synthetic bytes of the same length (checksums
recomputed) and the file is marked `"synthetic_templates": true`. Names,
user IDs, card numbers, passwords, serial numbers and MAC addresses are
replaced with same-length synthetic values. `tests/captures.rs` rejects
unmarked files with template-sized payloads in template exchanges; it runs
in CI and from an opt-in pre-commit hook (`.githooks/pre-commit`). Details in
`tests/fixtures/captures/README.md`.

## D22. Client request/reply discipline (revised after phase 3 review)

- **One request in flight**, `&mut self` methods, one `Client` per device.
- **A timeout closes the connection.** Without it, a late reply could be
  read as the answer to the next request. The error is
  `Error::Timeout { command, after }`; later calls return `ConnectionClosed`
  until `Client::reconnect`. I/O and TCP framing errors close it too. The
  one exception is a UDP data stream that stalls under Strict (D25). A bad
  checksum under `ChecksumPolicy::Reject` does not, because the frame was
  consumed whole and the stream is still in sync.
- **Reply IDs: `ReplyIdPolicy::Strict` by default.** A packet whose reply ID
  differs from the outstanding request's is logged at `warn`, discarded,
  and the client keeps waiting until the request's original deadline (one
  `request_timeout` from the start of the wait, not per packet). This applies
  to every packet of a reply, including each packet of a data stream.
  `ReplyIdPolicy::Lenient` (opt-in) logs and accepts the packet, for
  firmware that does not echo the reply ID.
  **Evidence:** pyzk's test fixtures show data-stream packets echoing the
  request's reply ID, but one fixture ends a stream with an `AckOk` carrying
  an earlier reply ID. Those fixtures are partly hand-made (many replies are
  reused regardless of the request), so this is not conclusive. If hardware
  shows chunked reads timing out under Strict, this is the first suspect,
  and Lenient is the workaround.
- **Live events that arrive mid-reply are never dropped silently.** They
  are acknowledged (reply ID 0), decoded (`LiveEvent`, D9) and pushed onto a
  bounded queue (`ClientConfig::pending_event_capacity`, default 1024),
  including during a drain after a failed stream. `take_pending_events()`
  returns them oldest first. When the queue is full the oldest event is
  dropped, `dropped_events()` is incremented and a warning is logged. The
  queue and the counter survive `reconnect`. Phase 5's live stream yields the
  queued events before reading the socket.
- **Retries** (`RetryPolicy`, default 3 attempts, as pyzk): connection-level
  failures while connecting, and malformed, short or refused chunk replies.
  Before a chunk retry the client drains the line until `AckOk` or 250 ms of
  silence. Authentication failures are never retried.
- **`FreeData`** follows every staged read, including failed ones while the
  connection is open. An inline (`Data`) reply stages nothing, so no
  `FreeData` is sent (as pyzk). Kept after review.

## D23. Client operation semantics (revised after phase 3 review)

- **User layout.** `users`/`attendance` detect the layout from the announced
  table length and the count from `read_sizes`, unless
  `ClientConfig::user_format` / `attendance_format` is set. The user layout is
  remembered for writes. For a write with no configured or remembered layout,
  the client reads the user table. If the device has no users, it would next
  try device information, but **neither PROTOCOL.md nor pyzk documents any
  option or firmware value that identifies the layout**. pyzk's only
  heuristic, "TCP reachable means 72 bytes", is pitfall 6 in PROTOCOL.md.
  So detection is inconclusive and the write fails with
  `UnsupportedRecordFormat`. The message includes the firmware version and
  the `~UserExtFmt` and `~ExtendFmt` values and tells the caller to set
  `user_format`. When hardware captures tie one of these values to a layout,
  the rule goes in `write.rs::user_format_for_write`.
- **Attendance.** `attendance()` returns records as stored. The explicit
  `attendance_with_users()` reads the user table at most once (only if some
  record lacks a user ID) and returns `Batch<ResolvedAttendance>`; each record
  is tagged `UserIdSource::{Record, UserTable, Unresolved}`. Unresolved
  records are kept with `user_id: None`. Rejected records pass through.
- **Single templates.** `user_template` returns `Ok(None)` only for
  `CMD_ACK_ERROR`, which is how devices report a missing template. The
  protocol has no distinct "not found" code, so an `AckError` for another
  reason is indistinguishable and also reads as `None`; that case is logged
  at `debug`. Any other failure reply is an error and is not retried.
  Malformed replies are retried.
- **Template deletes** return `Ok(false)` for `AckError` (pyzk: "probably
  empty"); other failures are errors.
- **By-user-ID operations.** `delete_user_by_user_id` and
  `user_template_by_user_id` resolve the `uid` from the user table and return
  `Error::UserNotFound(UserId)` if no user has the ID. If several users share
  an ID (corrupt table), the first is used and a warning is logged.
- `lock_state` reports whether `CMD_DOORSTATE_RRQ` succeeded, as pyzk
  does. The payload is undecoded.
- Option getters return the raw text. pyzk converts some to integers;
  `records::options::parse_int` is available.
- `restart`, `poweroff` and `disconnect` consume the client.
- Enrolment and live capture come in phase 5.

## D24. Identity types

`Uid(u16)` (device slot) and `UserId(String)` (the ID people see) are
distinct newtypes in `zkteco-proto` (`records::ids`, re-exported at the crate
root and from `zkteco-client`). They are used by `User`, `Attendance`,
`Template`, every request encoder and every client method, so the two
cannot be swapped. There are no conversions between them. `UserId`
dereferences to `str` and compares with `&str` for convenience.
`requests::start_enroll_udp` now takes a `&UserId` and fails if it is not
numeric.

## D25. UDP transport (phase 4)

- **Framing.** One packet per datagram, no prefix. The socket is connected
  to the device address, so the OS drops datagrams from anywhere else.
  Each receive uses a 64 KiB buffer, the largest possible datagram, so
  nothing is truncated (pitfall 2).
- **Chunks** are 16 KiB (`ReadBuffer` requests), against 0xFFC0 over TCP,
  as pyzk. Within a chunk the device streams `Data` packets of its own size.
- **Loss and duplication** are detected by size only. UDP `Data` packets
  carry no known sequence number, so `ChunkReceiver` counts bytes: a missing
  packet shows up as a short stream when the closing `AckOk` arrives, a
  duplicate as an overrun. Both are retried like any corrupt chunk (D22).
- **Reordering is undetectable.** Two packets swapped in transit give the
  right byte count with the bytes in the wrong order, and the chunk is
  accepted. The record decoders may reject the damaged records, or may not
  (a swapped attendance block decodes as different, plausible records).
  This is a known risk of UDP; prefer TCP where the device supports it. On
  a single LAN segment reordering is rare.
- **A stalled stream** (the tail of a chunk lost, so no `AckOk` arrives)
  is the one timeout that does not close the connection: under
  `ReplyIdPolicy::Strict`, once the stream has started, the client returns
  a retryable `ShortRead { expected, got }`, drains, and re-requests the
  chunk on the same socket. Strict reply IDs make this safe, because a late
  packet from the abandoned stream carries the old reply ID and is
  discarded. Under Lenient that guarantee is gone, so a stall is a fatal
  `Timeout`, as on TCP. A stall before the stream starts is also a `Timeout`.
- **`probe`** over UDP sends `Connect` and, if answered, `Exit`. There is
  no UDP equivalent of a bare TCP connect.
- **Template delete by user ID** over UDP resolves the `uid` from the user
  table and sends `DeleteUserTemp` (as pyzk); command 134 is TCP only (D15).
- **Hardware tests** run over UDP with `ZK_TRANSPORT=udp`.
