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
`Error::ShortRead { expected, got }` on both transports. Over UDP, reads
default to single-datagram chunks (D25). Orchestration (staging,
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
- All three crates are `0.1.0-alpha.1`, prepared and dry-run only; the
  maintainer publishes them (docs/RELEASING.md). The workspace shares one
  version. `xtask` is `publish = false`. (Until phase 6, `zkteco-client`
  and `zkctl` were `publish = false`.)

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
  `user_format`. Since the phase 4 review, the client first looks the
  device up in `docs/COMPATIBILITY.md` (D27); new hardware evidence goes in
  that table, not in code.
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

## D25. UDP transport (revised twice after phase 4 review)

- **Framing.** One packet per datagram, no prefix. The socket is connected
  to the device address, so the OS drops datagrams from anywhere else.
  Each receive uses a 64 KiB buffer, the largest possible datagram, so
  nothing is truncated (pitfall 2).
- **Reordering, investigated.** Over TCP, streamed `Data` packets carry a
  sequence number in the session ID field (PROTOCOL.md 3.2). For UDP:
  - (a) *Do the session ID or reply ID vary across UDP `Data` packets?*
    **Unknown; no source shows a UDP `Data` packet.** The 2006 manual does
    not describe the header of streamed packets. pyzk's UDP path reads only
    the command code. Every streamed `Data` packet in pyzk's test fixtures
    is a TCP frame. The other Rust ports copy pyzk. Nothing is implemented
    on a guess; `zkctl diagnose` measures it (`udp_stream_headers` in its
    report) with `Client::sample_udp_stream`, which requests one 16 KiB
    chunk and returns only the header fields of the packets that answer;
    no data from it is kept, so it cannot yield wrong records. If hardware
    shows a sequence, `ChunkReceiver` checks it as on TCP, and larger UDP
    chunks become safe to offer.
  - (b) *Does the 2006 manual define a buffer checksum command?* **No.** Its
    command table (`pyzk/docs/_static/Communication_protocol_manual_CMD.pdf`)
    has no checksum or CRC command for buffers, and `CMD_PREPARE_BUFFER` /
    `CMD_READ_BUFFER` (1503/1504) are not in it at all. The only checksum
    is the per-packet header checksum, which a reordering does not affect.
    The manual does say each streamed packet is at most 1 KiB.
- **Single-datagram chunks, always.** `ReadBuffer` requests over UDP ask
  for at most 1008 bytes (why 1008, below), so each reply holds one `Data`
  packet and there is nothing to reorder. What can
  still be reordered is detected: an `AckOk` ahead of its `Data` is a short
  read (retried), and a `Data` ahead of its `PrepareData` is the whole
  chunk, accepted (the bytes are the same). If the device splits a reply
  into several `Data` packets anyway, the chunk is discarded (its order
  cannot be trusted), the chunk size shrinks to the first packet's length
  (not below 64 bytes), and the chunk is read again. Cost: one round trip
  per KiB, so 100 000 40-byte attendance records (4 MB) take about 4 000
  requests.
- **No large chunks.** pyzk's 16 KiB UDP chunks were briefly an opt-in
  (`UdpChunking::Large`) and were removed from the public API: two `Data`
  datagrams swapped in transit give a chunk of the right length with its
  bytes in the wrong order, it is accepted, and the result is wrong
  attendance records with no error. Gating them on a compatibility row was
  considered and rejected: reordering happens in the network, not the
  device, so no device report can show it will not happen. The property
  that would make large chunks safe is a sequence number in UDP `Data`
  packets, which the client would then check; (a) above measures it.
- **Why 1008 and not 1016.** The manual's 1 KiB limit does not say what it
  covers. The only measurements are in pyzk's TCP fixtures: three
  `PrepareData` replies announce a per-packet size of 1008 (`f0 03 00 00`),
  and every full `Data` frame is 1016 bytes long (`f8 03`), that is an
  8-byte header and 1008 bytes of payload. With the 8-byte TCP prefix that
  is exactly 1024 bytes on the wire, so on TCP the device fills a 1 KiB
  buffer with prefix, header and payload. Over UDP there is no prefix, so
  the payload could be 1016 if the device sizes datagrams the same way, or
  stay 1008 if the payload size is fixed; no UDP capture exists to tell.
  1008 is the only size observed, and it is safe either way: on a device
  that sends 1016-byte payloads it costs 0.8% more requests; 1016 on a
  device that sends 1008-byte payloads would split the first chunk, which
  is rejected and re-read at 1008, once per connection with a warning.
  `diagnose` reports the device's UDP packet size (`packet_sizes`), so this
  can be revisited with evidence.
- **Loss and duplication** are detected by size and retried like any
  corrupt chunk (D22).
- **A stalled stream** (the tail of a chunk lost, so no `AckOk` arrives)
  is the one timeout that does not close the connection: under
  `ReplyIdPolicy::Strict`, once the stream has started, the client returns
  a retryable `ShortRead { expected, got }`, drains, and re-requests the
  chunk on the same socket. Strict reply IDs make this safe, because a late
  packet from the abandoned stream carries the old reply ID and is
  discarded. Under Lenient that guarantee is gone, so a stall is a fatal
  `Timeout`, as on TCP. A stall before the stream starts is also a `Timeout`.
- **`probe`** over UDP sends `Connect` and, if answered, `Exit`.
- **Template delete by user ID** over UDP resolves the `uid` from the user
  table and sends `DeleteUserTemp` (as pyzk); command 134 is TCP only (D15).

## D26. Transport selection

`zkteco_client::Transport` is `Auto` (default), `Tcp` or `Udp`. It is the
client's own enum; `zkteco_proto::Transport` stays `Tcp | Udp`, because the
protocol layer needs the concrete transport, and `Client::transport()`
returns that. `Auto` tries TCP and falls back to UDP when the TCP attempt
gets no answer: I/O error, timeout, closed connection or a malformed reply.
A refusal (wrong password, failure reply) is an answer, so it does not fall
back. The choice is logged at `info`. Each connect (including `reconnect`)
chooses again, so a device that recovers its TCP port is used over TCP. The
worst case is one TCP connect timeout plus the UDP attempt.

## D27. Compatibility table

`docs/COMPATIBILITY.md` records layouts and behaviour per device, one row
per `zkctl diagnose` report checked by a maintainer.
`zkteco_client::compat::COMPATIBILITY` holds the same rows, and a unit test
compares the two, so adding a row means editing both and nothing else.
`user_format_for_write` (D23) consults it when the device has no users to
measure: a row applies if every identifying column (model, firmware,
platform, `~UserExtFmt`, `~ExtendFmt`) equals the device's value or is `*`;
rows without "verified by" are ignored; applicable rows that disagree make
the result inconclusive. The table starts empty: there are no hardware
reports yet, and rows are never taken from documentation or pyzk's device
list.

## D28. Read-only mode and `zkctl diagnose`

- `ClientConfig::read_only` makes the client refuse any packet whose
  command is not in `READ_ONLY_COMMANDS`, checked in `send_raw`, which every
  outgoing packet passes through. The refusal is `Error::ReadOnly(command)`;
  nothing is sent and the session stays usable. The allowlist is session
  control, reads, buffer staging and release (`PrepareBuffer`, `ReadBuffer`,
  `FreeData`) and `AckOk` for live-event acknowledgements. Unit tests pin
  the list exactly and check it against every write, clear, delete, enrol,
  device-state, upload and power command and every unnamed code.
- `zkctl diagnose` (brought forward from phase 6) uses only read-only
  clients. It reads the device facts and counters, then reads the user and
  attendance tables twice (TCP 64 KiB chunks, UDP single-datagram chunks),
  each first under Strict reply IDs and, if that times out, under Lenient.
  Over UDP it also takes one stream sample (D25 a). It reports one
  `docs/COMPATIBILITY.md` row plus the measurements behind it, whether the
  UDP tables equal the TCP ones, and the header fields of streamed UDP
  packets. It never reads templates.
- Serial number and MAC are replaced by `scrubbed` unless
  `--include-identifiers`. The report holds counts, sizes and layout names,
  never records.
- `--capture DIR` writes one capture per probe in the
  `tests/fixtures/captures` format. Captures are always scrubbed (the flag
  above affects only the report): table data is reassembled by request
  offset and scrubbed with `zkteco_proto::scrub` (synthetic user IDs that
  still join across tables, synthetic names and timestamps, cleared
  passwords and cards, unknown regions zeroed); identifying options are
  scrubbed; the `CMD_AUTH` payload is zeroed, because the comm key can be
  reversed into the password; unattributable `Data` payloads are zeroed;
  live events are left out. Lengths are kept and checksums recomputed, so
  the captures pass `zkteco-proto`'s replay test.
- The password can come from `ZK_PASSWORD`, which keeps it out of shell
  history.

## D29. Live capture

- `Client::live_capture(self, CaptureOptions)` subscribes and returns
  `LiveCapture`, a `Stream<Item = Result<LiveEvent>>` that also has an
  inherent `next()`. Subscribing follows the reference flow: optionally
  read the user table (`resolve_users`), cancel any capture and enter
  verify mode (`prepare`, default on), enable the device if this client had
  disabled it, then `CMD_REG_EVENT` with the mask (default `ATTLOG`).
  `stop()` sends an empty mask, disables the device again if the capture
  enabled it, and returns the `Client`.
- Every event is acknowledged on arrival, decoded with
  `records::event::decode` (unknown lengths and other event types become
  `LiveEvent::Raw`, never dropped, D9) and queued; the stream yields from
  the queue, so events queued before the capture come first and several
  records in one packet, or several packets in one TCP read, all arrive in
  order.
- Cancel safety: the stream owns a boxed future for the step in progress,
  which owns the capture state. Dropping a `next()` future drops nothing
  but the poll; the next call resumes the same step. Each step waits at
  most one second (`IDLE_TICK`) for a packet; an idle step returns
  `Pending` after waking itself instead of looping, so there is no
  unbounded loop inside the stream. `stop()` finishes a step in flight and
  puts any event it received back on the client's pending queue.
- Errors do not end the stream by themselves. If the connection survives
  (a bad checksum), capture continues. If it is lost, the stream ends, or
  with `CaptureOptions::reconnect` (a `ReconnectPolicy`: attempts, initial
  and maximum backoff, doubling) reconnects and resubscribes, yielding each
  failure as an `Err`. It ends after `max_attempts` consecutive failures or
  on `Unauthorized`.
- Heartbeat (default 60 s): after that long without a packet the stream
  sends `CMD_GET_TIME`. Without it a device that vanishes without closing
  TCP (power loss) would never be noticed. pyzk has no heartbeat. Whether
  devices answer commands while subscribed is not known (open question
  Q6); if one does not, the symptom is a timeout every heartbeat, and
  `heartbeat: None` turns it off.
- `resolve_users` fills whichever of `uid` and `user_id` an attendance
  event lacks from the user table read at subscription. The map is not
  refreshed until the next subscription.
- Reconnecting is inside the stream rather than a wrapper type, so that
  the client (with its pending queue and counters) survives it via
  `Client::reconnect`.

## D30. Enrolment

- `enroll_user(&UserId, fid, EnrollOptions, progress)` is **destructive**
  (it adds a template). It checks `fid <= 9` and encodes the user ID
  before sending anything, then sends `CMD_CANCELCAPTURE` and
  `CMD_STARTENROLL` (TCP: 24-byte user ID, finger, 1; UDP: `u32` user ID,
  finger, so the user ID must be numeric). By-uid enrolment is not
  offered; resolve the user ID with `users()` first.
- It then reads event packets, acknowledging each, with
  `EnrollOptions::report_timeout` (default 60 s, as pyzk) per report and at
  most 32 reports. Packets with `ATTLOG` set are queued as live events;
  every other event packet is an enrolment report (first `u16` a result
  code). Each report is passed to `progress`.
- The outcome follows the reports, not a fixed count of scans: `0x64`
  counts a scan, other unknown codes are passed on and ignored, and the
  first of stored (0), failed (4), duplicate (5) or timed out (6) ends it.
  pyzk instead reads exactly two reports per scan for three scans, and over
  TCP treats a 0 in the first report of a pair as failure; reports carry no
  flag it checks. Since the meaning of a stored report before any accepted
  scan is unclear, it is returned as `EnrollOutcome::Unexpected`. No report
  in time is `NoReport`.
- Cleanup always runs: unsubscribe (empty mask), cancel capture, verify
  mode. If cleanup fails after an outcome, the cleanup error is returned and
  the outcome logged; if the enrolment itself failed, its error wins.
- Not cancel-safe, documented: a dropped future leaves the device in
  enrolment mode.

## D31. Live capture after the phase 5 review

- **TCP keepalive** (`ClientConfig::tcp_keepalive`, default 30 s idle, via
  `socket2`) on every TCP connection, so the operating system notices a
  dead peer even when nothing is sent. Probe interval and count are left to
  the OS; not every platform lets them be set.
- **Tolerant heartbeat.** An unanswered heartbeat no longer closes the
  connection: the wait ends without the usual timeout handling, a late
  reply is discarded by its reply ID, and only `heartbeat_misses` (default
  3) unanswered heartbeats in a row close the connection, reported as a
  `GetTime` timeout. Any packet from the device resets the count. This
  keeps working if Q6 turns out to be "no" for some firmware, as long as
  keepalive or events show the device is there.
- **Event times.** Items are `CapturedEvent { event, source, device_time,
  received_at, received_instant }`. `device_time` is the device clock in
  the record (no time zone); `received_at` is host wall-clock time when the
  packet arrived (stamped on arrival in the client's queue, not when
  delivered); `received_instant` is the host monotonic clock.
- **Backfill** (`CaptureOptions::backfill`, needs `reconnect`). The first
  subscription reads the device clock as the starting watermark; every
  delivered attendance event moves it forward. After each reconnect and
  resubscription (so nothing falls between), the attendance log is read and
  records at or after the watermark are delivered as `Source::Backfill`, in
  log order, before live events. The log is read whole; there is no "since"
  query in the protocol.
- **Dedup.** Every attendance delivery (live or backfill) is keyed by user
  (user ID, else uid), device time, status and punch. A key among the last
  4,096 delivered is skipped and counted. This covers punches already
  delivered live that reappear in the log, and a device resending an event
  whose acknowledgement was lost. Two genuine punches with the same key in
  the same second are indistinguishable and delivered once; the log has no
  other identifier.
- `LiveCapture::stats()` exposes counters (events, backfilled, duplicates,
  reconnects, heartbeats sent, answered, missed).
- `stop()` returns undelivered backfill to the client's pending queue with
  the rest, so nothing read is lost.
- **`diagnose --probe-live`** subscribes for a set time (default 60 s) with
  a read-only client that additionally allows `CMD_REG_EVENT`
  (`ClientConfig::allow_event_subscription`) and skips the preparation
  commands. It reports event flags and payload lengths, never payloads, and
  heartbeat answers; it answers Q5 and Q6.

## D32. `zkctl` (phase 6)

- **Thin layer.** Protocol behaviour stays in the client; the CLI resolves
  targets, confirms, formats and maps errors to exit codes. Dry run is a
  client feature for that reason: `ClientConfig::dry_run` routes every
  command outside `READ_ONLY_COMMANDS` to a recording, answers it locally
  with `CMD_ACK_OK`, and `Client::take_planned` returns the packets. So
  `--dry-run` prints exactly what the real run would send, built by the same
  code. **Reads still go to the device** (the brief said "sends nothing"):
  writes depend on them (the next free uid, the user layout, resolving a
  user ID). Tests assert that no non-allowlisted command reaches the mock.
- **Access per command.** Read commands, `live` and `diagnose` use
  read-only clients. `live` additionally allows `CMD_REG_EVENT`
  (`allow_event_subscription`) and skips the preparation commands
  (`CMD_CANCELCAPTURE`, `CMD_STARTVERIFY`), which change device state; the
  device stays in whatever mode it was. `templates export` is read-only
  too, but sits with the destructive commands in needing `--yes`, because
  it moves biometric data.
- **Exit codes** 0 to 7 as in `zkctl --help`. Classification walks the
  error chain to the first `zkteco_client::Error`. Over several devices the
  most serious outcome wins (any failure over partial over success).
  Enrolment that ends in anything but "stored" exits 5.
- **Output.** `--output json` is one document, `ndjson` one object per
  line; `live` refuses `json` (exit 2), since a stream has no end to close
  a document at. Field sets are pinned by snapshot tests; attendance
  records and events follow one schema (D34). Device times are
  printed without an offset (the device has no time zone); host times are
  UTC with milliseconds. PINs are never printed (`has_password` instead),
  except inside a template export file, which needs them to recreate users.
- **`live`.** One task per device, each owning a `LiveCapture`, feeding one
  printer over a bounded channel (4,096), so lines never interleave. Stop
  (Ctrl-C, `--count`, `--duration`) goes to every task through a watch
  channel; each unsubscribes and then emits what arrived during the stop
  (`Client::take_pending_captured`, which keeps each event's source, so
  undelivered backfill stays `backfill`). With `--reconnect`, attempts are
  unlimited and the first connection is retried too; a rejected password
  is never retried. `--count N` drops events after the Nth while stopping
  and reports how many on stderr.
- **`attendance pull --state FILE`.** A JSON cursor per device: the last
  record emitted (user, time, status, punch). The next run emits records
  after that record's position in the log; if it is no longer in the log
  (cleared), records after its time. The file is replaced atomically after
  output is written.
- **Destructive confirmation.** `--yes` (dry run exempt). The clears also
  need the device name typed, or `--confirm-device NAME`; without a
  terminal and without the flag they abort (7). `restart` and `poweroff`
  close the connection afterwards, so they take `&mut self` in the client
  (they took `&self` before).
- **Profiles and passwords.** `config.toml` under the platform config
  directory (`directories`), `deny_unknown_fields`, so a `password` key is
  an error rather than silently ignored. Password order: `--password`,
  `ZK_PASSWORD`, keyring, prompt (only after the device rejects the
  connection, and only with a terminal). Keyring calls run on the blocking
  pool. The keyring crate is `keyring` 4 with its `v1` stores (macOS
  Keychain, Windows Credential Manager, Secret Service over zbus on Linux).
  The zbus 5 crates it pulls in are flagged unmaintained (RUSTSEC-2026-0370
  to 0372, notices, not vulnerabilities; zbus 6 absorbed them); `deny.toml`
  ignores those three IDs with that reason until the Secret Service store
  moves to zbus 6.
- **`diagnose`** moved under the shared device options. Its file flag is
  `--out`/`-o`, because `--output` is now the global format flag; its
  timeout is the global `--timeout` (10 s default, was 5 s).
- **Man pages** come from the clap definitions via `cargo xtask man DIR`
  (`clap_mangen`), completions from `zkctl completions SHELL` or
  `cargo xtask completions DIR`.

## D33. Release preparation

- `cargo-deny` (`deny.toml`): permissive licenses only (MIT, Apache-2.0,
  BSD-3-Clause, Unicode-3.0, Zlib, and MPL-2.0 for `option-ext` only,
  unmodified, file-level copyleft), nothing GPL; advisories deny, with the
  three ignores in D32, to be rechecked by 2027-02-15; duplicate versions
  warn; crates.io only.
- `zkctl`'s tests use the mock device from `zkteco-client/tests` via
  `#[path]`, so they are excluded from its package; they run in this
  repository only.
- Library crates use `#![deny(missing_docs)]`; CI builds docs with
  `-D rustdoc::broken_intra_doc_links`.

## D34. One event schema (after the phase 6 review)

- `attendance pull` and `live` (live and backfill) print one object shape:
  `v`, `id` (D36), `device`, `source` (`pull`, `live`, `backfill`),
  `device_time`, `received_at` (null for pull), `kind`, `uid`, `user_id`,
  `user_id_source`, `status`, `status_code`, `punch`, `punch_code`,
  `workcode`. Every key is always
  present, so consumers need no per-source parsing. Pull's `timestamp`
  became `device_time`.
- `kind: raw` events also carry `raw: {flags, payload_hex}`, and only they
  do; their attendance fields are null. Raw events cannot be dropped (D9)
  and their bytes have to go somewhere; one optional key keeps attendance
  objects exactly the listed shape.
- `user_id_source` is on every object: `record`, `user_table`, or null
  (unresolved, and raw events). Pull without `--with-users` reports
  `record` when the record has a user ID, and hides user IDs it resolved
  from the user table (it resolves them anyway, for `id`; D36). For live events the client now
  says where the ID came from (`CapturedEvent::user_id_source`): the event,
  the user table (`resolve_users`, or backfill's read, which always
  resolves), or nowhere. (It was briefly dropped in review and restored
  before v1 was published.)
- `v` is the schema version, 1. Added fields keep it; removed, renamed or
  retyped fields would make version 2. The formal definition is
  `docs/schema/event-v1.json` (JSON Schema 2020-12, with conditions for
  `raw` and `pull`), and the CLI tests validate real `pull`, `live` and raw
  output against it with the `jsonschema` crate (a dev-dependency only).

## D35. `live --state` (after the phase 6 review)

- The client gained `CaptureOptions::resume: Option<Resume>` with
  `Resume { since, delivered }`. It sets the backfill watermark before the
  first subscription and seeds the duplicate filter with `delivered`. With
  `backfill`, the first subscription then backfills from `since` (after
  `CMD_REG_EVENT`, so nothing falls between), before any live event. No
  protocol behaviour changed.
- The CLI keeps, per device, the newest device time it has *emitted* and
  the punches emitted at exactly that time (uid, user ID, status code,
  punch code; at most 4,096). All punches in a second share a device time,
  so the time alone would re-emit some or skip others; a list of the
  punches at that time, used as dedup keys, gives exactly once. A record
  older than the watermark leaves it unchanged.
- The state is written by the printer, after the line is flushed to
  stdout, so the file never runs ahead of the output. Writes are atomic
  and durable (temporary file, `sync_all`, rename, directory sync; mode
  0600), at most `--state-flush` (default 1 s, `0` for every event) apart
  while events arrive, and on exit. A failed write is reported and retried
  at the next one; a failed final write exits 1.
- Guarantee: exactly once across a clean stop and restart; after a crash,
  punches emitted within the last flush interval may be emitted again
  (at-least-once). A missing state file starts fresh; an unreadable,
  corrupt or other-version one is reported on stderr and replaced, never
  fatal.
- `--state` without `--backfill` still records the watermark but does not
  catch up. `--backfill` still needs `--reconnect`.
- `attendance pull --state` and template exports now use the same atomic
  write (pull's cursor gained the fsyncs and mode 0600).

## D36. Event `id`

- `id` is the first 16 bytes of SHA-256 (lowercase hex) over
  length-prefixed fields (8-byte big-endian lengths, so field boundaries
  cannot shift): `zkctl-event-v1`, the kind, the device key, then for
  attendance the device time, the person, the status code and the punch
  code; for raw events the flags (4 bytes) and the payload. The exact
  encoding is in docs/GUIDE.md and `zkctl::event_id`, with a pinned test
  vector that was also reproduced independently from the documentation.
- The person is `user_id:<id>` when a user ID is known, else `uid:<n>`.
  Hashing both, as first specified, would give one punch two ids: live
  events in the 12-byte layout carry only the user ID, the log carries
  both. This matches the live duplicate filter's key (D31). Every live
  event layout carries the user ID, and backfill always resolves, so
  `attendance pull` resolves too, with or without `--with-users`: on a log
  layout with no user ID that costs one user-table read per device per run
  (none on other layouts), and `--with-users` only decides whether the
  resolved user ID is shown. The alternative, telling users to pass
  `--with-users` everywhere, left default pulls with ids that never matched
  live. A uid missing from the user table is still named by uid.
- The device key is `serial:<serial number>`, read when `zkctl` connects,
  not the profile name: as first specified, renaming a profile, or
  reaching one device through two profiles or by host, changed every id
  and defeated dedup across them. If the serial number cannot be read (or
  is empty) the key is `profile:<name>`, with a warning on stderr; the
  prefixes keep the two from colliding. `device` in the output stays the
  profile name, for people. Reading it costs one option read per
  connection; `live` reads it on the connection it subscribes on, before
  subscribing, and keeps it across reconnects to the same target.
- `user_id_source`, `uid`, `workcode`, `source` and arrival time are not
  in the hash: they describe how the punch was read, not which punch it
  is.
- Identical punches (person, second, method, state) share an `id`; the
  log has nothing else to tell them apart (the same limit as D31's
  dedup). Delivery is at-least-once after a crash (D35) and across pull and
  live, so consumers deduplicate on `id`.
- SHA-256 via `sha2` (already in the lock file through a dev-dependency),
  over BLAKE3: one fewer new crate, and recomputable with any standard
  library. Changing the encoding changes every id, so it would need schema
  version 2.

## D37. The demo device (after the 0.1.0-alpha.1 release)

- `cargo run -p zkteco-client --example demo_device` runs the test suite's
  mock device stand-alone, so `zkctl` can be tried without hardware. It
  reuses the mock rather than a second implementation: one protocol
  responder to keep correct.
- The mock moved from `tests/support` (pulled into each test crate with
  `#[path]`) into the crate as `zkteco_client::demo`, behind a `demo`
  feature that is off by default and `#[doc(hidden)]`. It is a fixture,
  not API: it panics on malformed input and may change in any release.
  The library's own build is unchanged (the feature only adds tokio's `rt`
  and `sync`).
- `zkteco-client` has a dev-dependency on itself with `demo`, so tests and
  examples get the feature and `cargo run --example demo_device` needs no
  `--features`; Cargo strips path dev-dependencies when publishing.
  `zkctl`'s tests enable it through their own dev-dependency.
  `tests/support/mod.rs` is now a re-export, so the tests are unchanged.
  The example and the tests that use the mock declare
  `required-features = ["demo"]`, so a build from the published package
  without the feature skips them instead of failing.
- The mock gained what the demo needs and the tests may use: binding a
  given port (`start_on`), pushing an arbitrary punch (`push_punch`), and
  going offline (`set_offline`: closes connections, refuses new ones,
  ignores datagrams, keeps recording). Its private exchange state moved
  into a `Session` field so other crates can still write
  `..DeviceState::default()`.
- The example sends a punch as an event only while a client is
  subscribed, as a device does; otherwise the punch is only stored. Each
  device's serial number is `DEMO-<NAME>`, so two demo devices do not
  share event ids (D36). It keeps the device clock at the host's local
  time, since backfill starts from the device clock (D31). Randomness is a
  seeded SplitMix64 in the example, not a new dependency.

## D38. `live` status lines in human output

- Human `live` output reported connection trouble through the client's
  tracing (retries, reconnect attempts, UTC timestamps) and `warning:`
  lines, several per drop. Now each device gets one status line per change
  (disconnected, reconnected with the backfilled count; the first
  connection failing and then working), and the client's logs need `-v`.
  Machine output (`ndjson`) is unchanged.
- Status lines go to standard error: data on standard output, everything
  else on standard error, so redirecting stdout keeps only events. They
  are written by the single printer, so on a terminal they still appear
  in order with the events they explain (a reconnect line before what it
  backfilled). They are human output only; `ndjson` never carries them.
- zkctl cannot tell a lost connection from other stream errors, or see a
  reconnect that backfilled nothing, from the stream alone. The client now
  says so: `CaptureStats::connected` (false from the loss until the
  resubscription and its backfill are done, so the count is final) and
  `reconnect_failures`. Each device task reads them on every item and
  every 250 ms. Adding public fields to `CaptureStats` breaks struct
  literals without `..Default::default()`; acceptable while alpha.
- Times in status lines and logs are the host's local time, in the format
  of event lines (which show the device's time). Colour (dimmed status,
  coloured `backfill`, coloured logs) only on a terminal and without
  `NO_COLOR`.

## D39. `#[non_exhaustive]` on options, stats and constructed records

- Adding a field to a public struct with public fields breaks every
  caller that builds it with a struct literal; D38 did that to
  `CaptureStats`. While still alpha, the structs callers build are made
  `#[non_exhaustive]` once, so later fields are not breaking.
- zkteco-client: `CaptureOptions`, `ReconnectPolicy`, `Resume`,
  `CaptureStats`, `RetryPolicy`, `EnrollOptions` (`ClientConfig` already
  was). Construction is `Default` plus chained setters named after the
  fields, the pattern `ClientConfig` set; `Option` fields take the inner
  value (`reconnect(policy)`), except `heartbeat`, where `None` is a
  meaningful setting. `Resume` has no sensible default, so `Resume::new`.
- zkteco-proto: `User`, `Attendance`, `Template`, the records callers build
  to write or to compare. Each gets `new` with the fields that identify it;
  the rest are set by assignment. No setters: records are data, and
  assignment reads plainly.
- Not changed: structs the library builds and callers read
  (`CapturedEvent`, `NetworkParams`, `DeviceFacts`), the capture and
  diagnostic records (`Recording`, `RecordedPacket`, `StreamSample`,
  `SampledPacket`; zkctl builds these too), the wire types (`Header`,
  `Packet`), and the `demo` module (hidden, not API). These can follow
  before 0.1.0 if any is expected to grow.

## Known issues

- **`merges_devices_across_a_reconnect` (zkctl `tests/live.rs`) hung once.**
  On 2026-11-15, during phase 6 review work, it failed one of 12
  consecutive runs of the live test suite: the second device's events
  never all arrived, and the test hit its 20 s wait. It then passed 25
  runs alone, 30 runs of the live suite, 30 runs of the whole zkctl suite,
  and 15 runs of the live suite with eight CPU-bound processes running
  (100 runs, no failure; it also passed 5 of 5 in phase 5). Hypothesis,
  unconfirmed: under load, a reply timeout (the test profile uses 500 ms)
  during the reconnect or the backfill read fails an attempt, and the
  reconnect backoff (1 s, doubling) pushes completion past 20 s. The
  test now passes `--duration 10s`, so a recurrence ends with the events
  received and fails on the assertions showing which are missing. Not
  fixed; to be revisited if it recurs.
  Recurred once on 2026-11-16 (D38 work), with events missing at the
  `--duration` backstop, while three cargo builds and test runs shared
  the machine; it then passed 30 consecutive runs of the live suite.
  Consistent with the load hypothesis; still not fixed.

## Open questions (blocked on `zkctl diagnose` reports from hardware)

No hardware is available. Nothing in the code assumes an answer to these;
the defaults are the conservative choice in each case. They are listed so
that the first diagnose reports can close them.

- **Q1. UDP sequence numbers.** Do streamed UDP `Data` packets carry a
  sequence number in the session ID field, as over TCP? Until a report
  shows it, UDP reads stay single-datagram (D25) and nothing checks or
  relies on UDP sequence numbers. Answered by
  `udp_stream_headers.session_ids` in the report.
- **Q2. UDP packet size.** Is the UDP `Data` payload 1,008 bytes (the TCP
  value), 1,016 (1,024 minus the 8-byte header) or something else? The
  single-datagram chunk stays 1,008 and shrinks if replies are split; no
  code assumes 1,016. Answered by `udp_stream_headers.packet_sizes`.
- **Q3. Reply IDs.** Do all devices echo the request's reply ID on every
  packet of a reply, including streamed packets? `Strict` stays the
  default; diagnose falls back to `Lenient` only to measure.
- **Q4. User and attendance layouts per firmware.** The compatibility table
  (D27) stays empty until diagnose reports fill it; no rows come from
  documentation or issue reports.
- **Q5. Zero-flag events.** Does any firmware send attendance events with
  an empty flag field? Decoding accepts it by payload length (D9). Answered
  by `live.packets` in a `diagnose --probe-live` report.
- **Q6. Requests while subscribed.** Does a device answer commands (the
  heartbeat's `CMD_GET_TIME`) while events are registered? The heartbeat
  tolerates silence (D31) rather than assuming either answer. Answered by
  `live.answers_while_subscribed`.
- **Q7. Enrolment reports.** Which event flag carries enrolment reports, how
  many reports a scan produces, and what a 0 code before any accepted scan
  means (D30).
