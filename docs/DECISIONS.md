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
partial UDP data as success. The client will surface `ShortRead` as
`Error::ShortRead { expected, got }` (phase 4). Orchestration (staging,
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

## D22. Client request/reply discipline (phase 3)

- **One request in flight**, `&mut self` methods, one `Client` per device.
- **A timeout closes the connection.** Without it, a late reply could be
  read as the answer to the next request (pyzk's failure mode). The error is
  `Error::Timeout { command, after }`; later calls return
  `ConnectionClosed` until `Client::reconnect`. I/O and TCP framing errors
  close it too. A bad checksum under `ChecksumPolicy::Reject` does not: the
  frame was consumed whole, so the stream is still in sync.
- **Reply ID mismatches are logged and accepted.** No capture shows whether
  every firmware echoes the reply ID; rejecting would break such devices.
  Because a timeout closes the connection, a stale reply cannot arrive on a
  live connection anyway.
- **Stray live events** that arrive while waiting for a reply are
  acknowledged (reply ID 0) and skipped, at most 256 per reply.
- **Retries** (`RetryPolicy`, default 3 attempts, as pyzk): connection-level
  failures while connecting, and malformed, short or refused chunk replies.
  Before a chunk retry the client drains the line until `AckOk` or 250 ms of
  silence. Authentication failures are never retried.
- **`FreeData`** follows every staged read, including failed ones while the
  connection is open. An inline (`Data`) reply stages nothing, so no
  `FreeData` is sent (as pyzk).

## D23. Client operation semantics (phase 3)

- `users`/`attendance` detect the layout from the announced table length and
  the count from `read_sizes`, unless `ClientConfig::user_format` /
  `attendance_format` is set. The user layout is remembered for writes.
  `set_user` and `save_user_templates` on a device with no users and no
  configured layout fail with `UnsupportedRecordFormat` instead of guessing
  (pyzk guesses 72 bytes from TCP reachability).
- `attendance` does not fill user IDs into short (8/16-byte) records from
  the user table, as pyzk does with an extra full user download. Join on
  `uid` if needed.
- `user_template` returns `None` when the device replies with a failure,
  and retries only malformed replies. pyzk retries any failure three times
  and then returns `None`.
- `delete_user_template{,_by_user_id}` return `Ok(false)` for `AckError`
  (pyzk: "probably empty"); other failures are errors.
- `lock_state` reports whether `CMD_DOORSTATE_RRQ` succeeded, as pyzk
  does. The payload is undecoded.
- Option getters return the raw text. pyzk converts some to integers;
  `records::options::parse_int` is available.
- `restart`, `poweroff` and `disconnect` consume the client.
- Only TCP is implemented; `Transport::Udp` returns `Error::Unsupported`
  until phase 4. Enrolment and live capture come in phase 5.

