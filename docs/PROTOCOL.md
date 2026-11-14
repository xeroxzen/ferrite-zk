# The ZK device protocol

This document describes the protocol spoken by ZKTeco attendance and
access-control terminals on port 4370 over UDP and TCP. It records protocol
facts and observed device behaviour. Implementation choices are in
[DECISIONS.md](DECISIONS.md).

## 1. Sources and evidence

Primary source: ZKSoftware, *Communication protocol manual* (2006).
Behavioural cross-reference: the pyzk library (`1af361a0`), observed through
`tools/gen_fixtures.py`, which records what pyzk sends and how it
interprets replies.

Each fact below is tagged:

| Tag | Meaning |
|---|---|
| **[manual]** | Stated in the 2006 manual. |
| **[ref]** | Behaviour of the reference implementation, pinned by golden fixtures. |
| **[observed]** | Seen in third-party device captures. Not redistributed here, so it does not count as our hardware evidence. |
| **[report]** | A community report (pyzk issue or PR number given). |

**No layout in this document is verified against hardware yet.** Our own
captures go in `tests/fixtures/captures/`, and a layout loses its
"unverified" marking only once a capture covers it (D16).

All integers are little-endian.

---

## 2. Transport

### 2.1 UDP [manual]

One packet (section 3) per datagram. Datagrams can be large: a buffered read
over UDP requests chunks of up to 16 KiB (section 7). A receiver must read
with a buffer big enough for the largest datagram. A short buffer silently
truncates.

### 2.2 TCP framing [ref] [observed]

Each packet is preceded by an 8-byte prefix:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 4 | magic `50 50 82 7d` (as two u16: `0x5050`, `0x7D82`) |
| 4 | 4 | u32 length of the packet that follows (header + payload) |

TCP is a byte stream. Frames can arrive split across reads, and several
frames can arrive in one read; both are seen in real captures [observed].
Receivers must buffer and cut frames by the length field.

---

## 3. Packet header [manual]

| Offset | Size | Field |
|---:|---:|---|
| 0 | 2 | command (request code or reply code) |
| 2 | 2 | checksum |
| 4 | 2 | session ID |
| 6 | 2 | reply ID |
| 8 | n | payload |

### 3.1 Checksum [manual] [ref]

This is the 16-bit ones'-complement sum of the whole packet, with the
checksum field set to zero during computation:

1. Sum the packet as little-endian u16 words. If the length is odd, the last
   byte is the low byte of a final word.
2. Fold carries back in: `sum = (sum & 0xFFFF) + (sum >> 16)`.
3. Take the complement: `checksum = !sum`.

Example: `CMD_GET_TIME` (command 201) in session `0x2A51` with reply ID 1
sums to `0x00C9 + 0x2A51 + 0x0001 = 0x2B1B`, so its checksum is `0xD4E4` and
the packet is `c9 00 e4 d4 51 2a 01 00`.

The reference implementation reaches the same bytes by a different route:
its final step is one too low, and it sums a reply ID one lower than the one
it sends, so the two errors cancel. Verified on 200,000 random packets. A
re-implementation that copies only one of the two errors produces checksums
that are off by one, which firmware that validates strictly will reject.

Devices send correct checksums [observed]. The manual does not say whether
devices validate incoming ones. Some firmware does.

### 3.2 Session ID [manual]

- Requests carry 0 until the device answers `CMD_CONNECT`. The reply's
  session ID is then used for the rest of the connection.
- **Reused in event packets** [manual] [observed]: in a `CMD_REG_EVENT`
  packet sent by the device, this field holds the event type (`EF_*` flag).
- **Reused in data streams** [observed]: in each `CMD_DATA` packet of a
  multi-packet TCP transfer (section 7.2), this field holds a 0-based
  sequence number. Not in the manual.

### 3.3 Reply ID [manual] [ref]

A per-connection counter that distinguishes requests. The device echoes it in
the reply. The first request is sent with 0. Each later request adds one,
wrapping from 65534 to 0 (65535 is never sent). Acknowledgements of
device-initiated event packets use reply ID 0 and do not advance the counter.

---

## 4. Replies [manual]

| Code | Name | Meaning |
|---:|---|---|
| 2000 | `CMD_ACK_OK` | success |
| 2001 | `CMD_ACK_ERROR` | failure |
| 2002 | `CMD_ACK_DATA` | data follows |
| 2003 | `CMD_ACK_RETRY` | listed by the manual under registered events |
| 2004 | `CMD_ACK_REPEAT` | |
| 2005 | `CMD_ACK_UNAUTH` | session not authenticated |
| 0xFFFB | `CMD_ACK_ERROR_DATA` | |
| 0xFFFC | `CMD_ACK_ERROR_INIT` | not initialised |
| 0xFFFD | `CMD_ACK_ERROR_CMD` | invalid command |
| 0xFFFF | `CMD_ACK_UNKNOWN` | unknown command |

A request succeeds if the reply is `CMD_ACK_OK`, or `CMD_PREPARE_DATA` /
`CMD_DATA` when the device starts or delivers a data transfer [ref].

---

## 5. Session lifecycle [manual]

```
host                                       device
  CMD_CONNECT (session 0) ─────────────────▶
  ◀──────────── CMD_ACK_OK | CMD_ACK_UNAUTH   (header carries new session ID)
  [if UNAUTH] CMD_AUTH (comm key, §6) ─────▶
  ◀──────────── CMD_ACK_OK | CMD_ACK_UNAUTH   (UNAUTH: wrong password)
  ... requests ...
  CMD_EXIT ─────────────────────────────────▶
  ◀──────────── CMD_ACK_OK
```

- A device with a comm key set answers `CMD_CONNECT` with `CMD_ACK_UNAUTH`,
  and the host must then authenticate [manual].
- After `CMD_RESTART` or `CMD_POWEROFF` succeeds, the device drops the
  connection.
- The protocol has no reachability probe. A TCP connect or a `CMD_CONNECT`
  reply within a timeout is sufficient (D14).

---

## 6. Comm key (`CMD_AUTH` payload) [ref]

Input: numeric password (u32), session ID (u16), and a "ticks" byte (50 by
convention; any value works because it is carried in the output).

1. `k = reverse_bits_32(password) + session_id`, with 32-bit wrap-around (D8).
2. `b = k.to_le_bytes()`, then XOR with `"ZKSO"`: `b[0]^'Z', b[1]^'K', b[2]^'S', b[3]^'O'`.
3. Swap the two 16-bit halves: `[b2, b3, b0, b1]`.
4. XOR bytes 0, 1 and 3 with `ticks`, and **replace** byte 2 with `ticks`.

Example: password 0, session 0, ticks 50 gives `61 7d 32 79`.

The password must be numeric.

---

## 7. Large data transfers

### 7.1 Buffered read [ref] [observed]

Used for the user, attendance and template tables. These commands are not in
the manual.

1. **`CMD_PREPARE_BUFFER` (1503)** stages a table. Payload (11 bytes):
   `u8 1`, `u16` table command (e.g. 9 `CMD_USERTEMP_RRQ`, 13 `CMD_ATTLOG_RRQ`,
   7 `CMD_DB_RRQ`), `u32` table selector (`FCT_*`, section 14), `u32` 0.
   The reply is one of:
   - `CMD_DATA`: the whole table, inline. No further steps, and no free.
   - `CMD_ACK_OK`: payload `u8 ?`, `u32 size`, then 8 bytes of unknown
     meaning (unverified).
2. Read the table in chunks with **`CMD_READ_BUFFER` (1504)**, payload
   `u32 offset, u32 length`. Chunk length is at most **65 472 (`0xFFC0`) over
   TCP** and **16 384 over UDP** [ref]. The reply is handled as in 7.2.
3. Release the buffer with **`CMD_FREE_DATA` (1502)**.

The table is a `u32` byte length followed by the records.

### 7.2 Chunk reply [ref] [observed]

- `CMD_DATA`: the chunk inline.
- `CMD_PREPARE_DATA` (1500), payload `u32 size` and, observed on TCP, `u32`
  per-packet size (1008 seen; not in the manual). It is followed by `CMD_DATA`
  packets carrying `size` bytes in total, then `CMD_ACK_OK`.
  - On TCP, each `CMD_DATA` carries its 0-based sequence number in the
    session ID field (3.2) [observed].
  - On UDP, packets carry up to 1024 payload bytes [ref]. Sequence numbers are
    unknown, so loss is detected only by comparing the total against `size`.

The reference implementation does not compare the received total against
`size`, so a short UDP stream counts as success. That is a data-loss risk (D10).

### 7.3 Upload [manual] [ref]

`CMD_PREPARE_DATA` with payload `u32 total`, then `CMD_DATA` packets of at
most 1024 bytes, each acknowledged. The manual says to send `CMD_FREE_DATA`
*after* the data "to tell the machine the data is complete". The reference
implementation sends it *before* `CMD_PREPARE_DATA` instead, then follows the
upload with the command that consumes the buffer (11.3). Which order firmware
requires is unverified.

---

## 8. Time

### 8.1 Packed u32 [manual]

```
value = ((year % 100) * 12 * 31 + (month - 1) * 31 + (day - 1)) * 86400
        + (hour * 60 + minute) * 60 + second
```

Used by attendance records, `CMD_GET_TIME` and `CMD_SET_TIME`. Every month
gets 31 day slots, so a value can name an impossible date. Real devices
produce such values [report #211, #221]. Only years 2000 to 2099 can be
represented.

### 8.2 Compact 6 bytes (real-time events) [manual] [ref]

`year - 2000, month, day, hour, minute, second`, one byte each.

Times are device-local wall-clock time. No zone is carried.

---

## 9. Users

### 9.1 Layout detection [ref]

`record size = table length / user count`, where the user count comes from
`CMD_GET_FREE_SIZES` (section 13.1).

No option or firmware value is known to identify the layout. With no users
on the device, the layout cannot be measured. The reference implementation
then assumes 72 bytes if the device is reachable over TCP and 28 otherwise,
a guess (pitfall 6 in section 15).

### 9.2 28-byte record [ref] (unverified against hardware)

| Off | Size | Field |
|---:|---:|---|
| 0 | 2 | uid (device-internal index) |
| 2 | 1 | privilege (9.5) |
| 3 | 5 | password (NUL-padded) |
| 8 | 8 | name |
| 16 | 4 | card number |
| 20 | 1 | pad |
| 21 | 1 | group (u8) |
| 22 | 2 | time zone (i16) |
| 24 | 4 | user ID (u32, shown as decimal) |

### 9.3 72-byte record [ref] (unverified against hardware)

| Off | Size | Field |
|---:|---:|---|
| 0 | 2 | uid |
| 2 | 1 | privilege |
| 3 | 8 | password |
| 11 | 24 | name |
| 35 | 4 | card number |
| 39 | 1 | unknown flag. The reference implementation writes 0 for single-user writes and 1 in bulk saves. |
| 40 | 7 | group (string) |
| 47 | 1 | pad |
| 48 | 24 | user ID (string) |

### 9.4 120-byte record [ref] [report #240, #264] (unverified against hardware)

Reported for NGTeco NG-K4 and NG-MB2.

| Off | Size | Field |
|---:|---:|---|
| 0 | 2 | uid |
| 2 | 1 | privilege |
| 3 | 8 | password |
| 11 | 24 | unknown |
| 35 | 24 | first name |
| 59 | 24 | last name |
| 83 | 4 | card number |
| 87 | 9 | unknown |
| 96 | 24 | user ID |

There is no write support: with two unknown regions, a write cannot be
constructed safely.

### 9.5 Privilege byte [ref]

Bit 0 means **disabled**. Bits 1 to 3 hold the role: 0 user, 2 enroller,
6 manager, 14 administrator. Other role values exist in the wild.

### 9.6 Commands

- **`CMD_USER_WRQ` (8)**: the payload is one record in the device's layout
  (9.2 or 9.3). The time zone is written as 0. Follow with
  `CMD_REFRESHDATA` (1013).
- **`CMD_DELETE_USER` (18)**: payload `u16 uid`. Follow with
  `CMD_REFRESHDATA`.
- The next free uid is one past the highest existing uid. The next free user
  ID is that number as a string, incremented until unused (a host-side
  convention [ref]).

---

## 10. Attendance

Read the table with a buffered read (7.1) of `CMD_ATTLOG_RRQ` (13).
`record size = table length / record count` (13.1).
Clear with `CMD_CLEAR_ATTLOG` (15).

### 10.1 8-byte [ref] (unverified)

`u16 uid | u8 status | u32 time | u8 punch`. There is no user ID string;
join on uid with the user table.

### 10.2 16-byte [ref] (unverified)

`u32 user ID | u32 time | u8 status | u8 punch | 2 reserved | u32 work code`.

### 10.3 40-byte [ref] (unverified)

`u16 uid | 24-byte user ID | u8 status | u32 time | u8 punch | 8 reserved`.

### 10.4 49-byte (unverified), two conflicting reports

- 9-byte marker `ff 32 35 35 00 00 00 00 00` followed by the 40-byte record
  [ref] [report #174, MB560-VL].
- The 40-byte record's fields at offset 0, then 17 unknown bytes
  [report: unmerged PR #247, MB40-VL].

Distinguish them per record by the marker (D12).

### 10.5 Field meanings (unverified)

- **status**: verification method, defined by firmware. Commonly 0 password,
  1 fingerprint, 2 card, 15 face.
- **punch**: attendance state. Commonly 0 check-in, 1 check-out, 2 break-out,
  3 break-in, 4 overtime-in, 5 overtime-out. 255 has been reported
  [report #211].

---

## 11. Fingerprint templates

### 11.1 Table [ref] (unverified)

Buffered read of `CMD_DB_RRQ` (7) with selector `FCT_FINGERTMP` (2). The
table is a `u32` length, then entries of:
`u16 entry size (including this 6-byte header) | u16 uid | u8 finger index | u8 valid flag | template`.

An entry size below 6, or past the end of the data, means the rest cannot
be located.

### 11.2 Single template (undocumented command 88) [ref] [observed] (unverified)

Request payload: `u16 uid | u8 finger index`. The reply is a data transfer
(7.2). The data has one extra trailing byte. The reference implementation
also removes six trailing zero bytes if present, as presumed padding (D13).

### 11.3 Bulk save (undocumented command 110) [ref] (unverified)

Upload (7.3) a buffer laid out as:

```
u32 len(users) | u32 len(index) | u32 len(templates)
users:     per user, 0x02 + 28- or 72-byte record (72-byte flag byte = 1)
index:     per template, 0x02 | u16 uid | u8 (0x10 + finger index) | u32 offset into templates
templates: per template, u16 length | template
```

Then send command 110 with payload `u32 12 | u16 0 | u16 8` (meaning
unknown), then `CMD_REFRESHDATA`.

### 11.4 Delete

- `CMD_DELETE_USERTEMP` (19): `u16 uid | u8 finger index` [manual] [ref].
- Undocumented command 134: `24-byte user ID | u8 finger index`
  (unverified; the reference implementation's code for this path cannot run,
  D15).

### 11.5 Enrolment (unverified)

1. Send `CMD_CANCELCAPTURE` (62).
2. Send `CMD_STARTENROLL` (61). The payload over TCP is
   `24-byte user ID | u8 finger index | u8 1`. Over UDP it is
   `u32 user ID | u8 finger index` [ref].
3. The device then sends event packets. Each is answered with `CMD_ACK_OK`.
   The first u16 of the payload is a result code [ref]:

| Code | Meaning |
|---:|---|
| `0x64` | scan accepted, place the finger again |
| 0 | stored; may be followed by `u16` size and `u16` position |
| 4 | failed |
| 5 | duplicate finger |
| 6 | timed out |

The reference flow expects three accepted scans before the final result.
Afterwards it unregisters events, cancels capture and sends
`CMD_STARTVERIFY` (60).

---

## 12. Real-time events [manual] [ref]

Subscribe with **`CMD_REG_EVENT` (500)** and payload `u32` event mask.
Unsubscribe with mask 0. The device then sends `CMD_REG_EVENT` packets whose
session ID field holds the event type. Every event must be acknowledged with
`CMD_ACK_OK` [manual: "needs to reply the successful order"].

Before subscribing, the reference flow cancels any capture, enters verify
mode and enables the device. Afterwards it unsubscribes and restores the
previous enabled state.

### 12.1 `EF_ATTLOG` payload [ref] [observed] (unverified)

Choose the layout by the remaining payload length:

| Length | Layout |
|---:|---|
| 10 | `u16 user ID, u8 status, u8 punch, 6-byte time` |
| 12 | `u32 user ID, u8 status, u8 punch, 6-byte time` |
| 14 | 10-byte layout + 4 unknown bytes |
| 32 | `24-byte user ID, u8 status, u8 punch, 6-byte time` |
| 36 | 32-byte layout + 4 unknown bytes |
| 37 | 32-byte layout + 5 unknown bytes |
| ≥ 52 | 32-byte layout + 20 unknown bytes; then continue with the remainder |

The manual's 2006 description of this payload (u16 user, a validity nibble,
"matching way") matches none of these layouts. A payload whose length fits no
row must be kept as raw bytes, not looped on or dropped (D9).

Which packets carry attendance: those with `EF_ATTLOG` set. The reference
implementation never checks the flag and parses every event packet as
attendance, so firmware that sends attendance with a zero flag would work
with it. The Rust decoder therefore also accepts a zero flag when the payload
length splits exactly into rows of this table (D9).

### 12.2 Other events [manual] (undecoded)

`EF_ENROLLUSER` carries a u16 user index. `EF_BUTTON` carries the key value.
`EF_VERIFY` carries the user index. `EF_FPFTR` carries a match score.
`EF_ALARM` is described by payload length:

- 4 bytes: first byte 55 means tamper, 53 means exit button.
- 8 bytes: first byte 54 means door closed.
- 12 bytes: the first two bytes `0xFFFF` mean duress, bytes 5 to 6 the duress
  finger, bytes 7 to 8 the alarm type, and bytes 9 to 12 the matching method.

---

## 13. Other commands

| Command | Payload | Reply |
|---|---|---|
| `CMD_ENABLEDEVICE` 1002, `CMD_DISABLEDEVICE` 1003 | none | ack |
| `CMD_RESTART` 1004, `CMD_POWEROFF` 1005 | none | ack, then disconnect |
| `CMD_REFRESHDATA` 1013 | none | ack |
| `CMD_TESTVOICE` 1017 | `u32` prompt index | ack |
| `CMD_GET_VERSION` 1100 | none | NUL-terminated string |
| `CMD_GET_TIME` 201 | none | `u32` packed time |
| `CMD_SET_TIME` 202 | `u32` packed time | ack |
| `CMD_GET_FREE_SIZES` 50 | none | 13.1 |
| `CMD_GET_PINWIDTH` 69 | `" P"` (2 bytes) | first byte = width [ref] |
| `CMD_UNLOCK` 31 | `u32` duration in tenths of a second [ref] | ack |
| `CMD_DOORSTATE_RRQ` 75 | none | ack; payload meaning unknown |
| `CMD_WRITE_LCD` 66 | `i16 line, u8 0, ' ', text` [ref] | ack |
| `CMD_CLEAR_LCD` 67 | none | ack |
| `CMD_CLEAR_DATA` 14 | none (clears users, attendance and templates) | ack |
| `CMD_CLEAR_ATTLOG` 15 | none | ack |
| `CMD_CANCELCAPTURE` 62, `CMD_STARTVERIFY` 60 | none | ack |
| `CMD_OPTIONS_WRQ` 12 | `key=value` (sent without NUL [ref]) | ack |

### 13.1 `CMD_GET_FREE_SIZES` reply [ref] [observed] (unverified)

20 u32 words, optionally followed by 3 words of face data.

| Word | Meaning |
|---:|---|
| 4 | users |
| 6 | fingerprint templates |
| 8 | attendance records |
| 12 | cards |
| 14 | fingerprint capacity |
| 15 | user capacity |
| 16 | record capacity |
| 17 | fingerprint slots free |
| 18 | user slots free |
| 19 | record slots free |
| 20 / 22 | faces / face capacity (face block) |

The other words have no known meaning.

### 13.2 Options (`CMD_OPTIONS_RRQ`, 11) [manual] [ref]

The request is `key\0`. The reply is `key=value\0`. Keys used for parity:
`~SerialNumber`, `~Platform`, `MAC`, `~DeviceName`, `ZKFaceVersion`,
`~ZKFPVersion`, `~ExtendFmt`, `~UserExtFmt`, `FaceFunOn`, `CompatOldFirmware`,
`IPAddress`, `NetMask`, `GATEIPAddress`. A failing key answers with
`CMD_ACK_ERROR`. The reference implementation then sends `CMD_ACK_ERROR`
once and `CMD_ACK_UNKNOWN` three times *as requests*. That recovery step is
undocumented, and its purpose is unknown (unverified).

---

## 14. Constants

**Session / control**: `CMD_CONNECT` 1000, `CMD_EXIT` 1001,
`CMD_ENABLEDEVICE` 1002, `CMD_DISABLEDEVICE` 1003, `CMD_RESTART` 1004,
`CMD_POWEROFF` 1005, `CMD_SLEEP` 1006, `CMD_RESUME` 1007,
`CMD_REFRESHDATA` 1013, `CMD_REFRESHOPTION` 1014, `CMD_TESTVOICE` 1017,
`CMD_GET_VERSION` 1100, `CMD_CHANGE_SPEED` 1101, `CMD_AUTH` 1102,
`CMD_ENABLE_CLOCK` 57, `CMD_STATE_RRQ` 64, `CMD_GET_FREE_SIZES` 50,
`CMD_GET_PINWIDTH` 69.

**Data transfer**: `CMD_PREPARE_DATA` 1500, `CMD_DATA` 1501,
`CMD_FREE_DATA` 1502, `CMD_PREPARE_BUFFER` 1503\*, `CMD_READ_BUFFER` 1504\*.

**Data read/write**: `CMD_DB_RRQ` 7, `CMD_USER_WRQ` 8, `CMD_USERTEMP_RRQ` 9,
`CMD_USERTEMP_WRQ` 10, `CMD_OPTIONS_RRQ` 11, `CMD_OPTIONS_WRQ` 12,
`CMD_ATTLOG_RRQ` 13, `CMD_OPLOG_RRQ` 34, get-user-template 88\*,
save-users-and-templates 110\*.

**Delete / clear**: `CMD_CLEAR_DATA` 14, `CMD_CLEAR_ATTLOG` 15,
`CMD_DELETE_USER` 18, `CMD_DELETE_USERTEMP` 19, `CMD_CLEAR_ADMIN` 20,
`CMD_CLEAR_ACC` 32, `CMD_CLEAR_OPLOG` 33, delete-template-by-user-ID 134\*.

**Access control**: `CMD_USERGRP_RRQ`/`WRQ` 21/22, `CMD_USERTZ_RRQ`/`WRQ`
23/24, `CMD_GRPTZ_RRQ`/`WRQ` 25/26, `CMD_TZ_RRQ`/`WRQ` 27/28,
`CMD_ULG_RRQ`/`WRQ` 29/30, `CMD_UNLOCK` 31, `CMD_DOORSTATE_RRQ` 75.

**Biometrics**: `CMD_STARTVERIFY` 60, `CMD_STARTENROLL` 61,
`CMD_CANCELCAPTURE` 62, `CMD_CAPTUREFINGER` 1009, `CMD_TEST_TEMP` 1011,
`CMD_CAPTUREIMAGE` 1012.

**LCD / SMS / Mifare**: `CMD_WRITE_LCD` 66, `CMD_CLEAR_LCD` 67,
`CMD_SMS_WRQ` 70, `CMD_SMS_RRQ` 71, `CMD_DELETE_SMS` 72, `CMD_UDATA_WRQ` 73,
`CMD_DELETE_UDATA` 74, `CMD_WRITE_MIFARE` 76, `CMD_EMPTY_MIFARE` 78.

**Time**: `CMD_GET_TIME` 201, `CMD_SET_TIME` 202.

**Events**: `CMD_REG_EVENT` 500. Flags: `EF_ATTLOG` 1, `EF_FINGER` 2,
`EF_ENROLLUSER` 4, `EF_ENROLLFINGER` 8, `EF_BUTTON` 16, `EF_UNLOCK` 32,
`EF_VERIFY` 128, `EF_FPFTR` 256, `EF_ALARM` 512.

**Replies**: section 4.

**Table selectors (`FCT_*`)**: attendance 1, fingerprint templates 2,
operation log 4, users 5, SMS 6, user SMS 7, work codes 8.

**Privileges**: section 9.5.

\* Not in the manual.

---

## 15. Pitfalls seen in the reference implementation

These are behaviours to avoid, each with where the Rust port handles it.

| # | Pitfall | Handling |
|---|---|---|
| 1 | TCP treated as one read per frame; split or merged frames mishandled | `framing::TcpFrameDecoder` |
| 2 | UDP reads with small buffers truncate silently | client reads with large buffers (phase 3) |
| 3 | Short or lossy chunk streams returned as success | `ChunkReceiver` checks size and sequence (D10) |
| 4 | Incoming checksums never checked | `ChecksumPolicy` (D2) |
| 5 | Reachability via a shell `ping` built from the address string | no shelling out (D14) |
| 6 | Record layout guessed from TCP reachability before any data is read | detect from data or config (D5) |
| 7 | Non-integral record sizes fall back silently to the 72-byte parser | `UnsupportedRecordFormat` (D5) |
| 8 | One impossible date aborts a whole attendance download | per-record rejection (D4, D6) |
| 9 | Privileges other than 0/14 coerced to 0 on write | explicit error (D3) |
| 10 | Unknown live-event lengths loop forever | kept as `LiveEvent::Raw`, decoding always terminates (D9) |
| 11 | Comm-key overflow raises instead of wrapping | wrap (D8) |
| 12 | Template table walk unbounded on a zero entry size | bounded walk, rejection |
| 13 | Over-long text silently truncated on write | `FieldTooLong` (D7) |
| 14 | 16-byte attendance records yield a string uid | `uid: Option<u16>` |
| 15 | Enrolment result read at the wrong offset over TCP | payload-relative decoding (`decode_enroll_report`) |
| 16 | Inconsistent failure reporting (exceptions vs. False/None/"") | uniform `Result` |
