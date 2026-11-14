# Device captures

Packet captures from real ZKTeco devices. They are the only evidence that a
layout is verified against hardware. They are excluded from published crates.

## Rules for committing a capture

Captures come from devices that hold biometric and personal data. Before a
capture is committed:

1. **Never commit real fingerprint or face templates.** Replace every
   template payload with synthetic bytes of **identical length**, recompute
   the packet checksum (and, for TCP, keep the frame length unchanged), and
   set `"synthetic_templates": true` at the top level of the file. Template
   payloads appear in:
   - single-template replies (command 88),
   - buffered reads of the template table (`CMD_DB_RRQ`, table 2),
   - host-to-device `CMD_DATA` uploads (bulk template saves).
2. **Scrub personal data.** Replace names, user IDs, card numbers and
   passwords with synthetic values of the same encoded length, in user
   tables, attendance records and live events alike. Recompute checksums.
3. **Scrub device identity.** Replace serial numbers and MAC addresses (in
   options replies such as `~SerialNumber` and `MAC`, and anywhere else they
   appear) with synthetic values of the same length. Keep the device model,
   platform and firmware version: those are the point of the capture.
4. Prefer a test device enrolled with synthetic users over scrubbing a
   production capture.

Scrubbing must keep lengths unchanged so the capture still exercises the
same record layouts.

### Enforcement

`crates/zkteco-proto/tests/captures.rs` (run by `cargo test` in CI) fails if
a capture contains a template-sized payload (64 bytes or more) in any of the
template exchanges above and the file is not marked
`"synthetic_templates": true`. The check is deliberately broad; it cannot see
names, IDs, cards or serials, so rules 2 and 3 rely on review.

To run the same check before every commit:

```sh
git config core.hooksPath .githooks
```

## Format

One JSON file per capture session:

```json
{
  "device": "K40",
  "firmware": "Ver 6.60 Apr 28 2017",
  "platform": "ZMM220_TFT",
  "transport": "tcp",
  "encoding": "utf-8",
  "synthetic_templates": false,
  "notes": "get_time",
  "packets": [
    { "direction": "host_to_device", "hex": "5050827d08000000c900e4d4512a0100" },
    { "direction": "device_to_host", "hex": "5050827d0c000000d007f2fa512a0100b1a23930" }
  ]
}
```

- `transport`: `tcp` or `udp`. For TCP, `hex` holds bytes exactly as read
  from the socket, including the `5050827d` frame prefix; chunks may split or
  join frames. For UDP, each entry is one datagram.
- `encoding`: optional WHATWG label for text fields (default `utf-8`).
- `synthetic_templates`: `true` once every template payload has been
  replaced (rule 1). Omit or set `false` for captures without templates.
- Entries are in capture order across both directions.

`crates/zkteco-proto/tests/captures.rs` replays every file: each packet must
frame correctly and carry a valid checksum, and every `EF_ATTLOG` event must
decode with no raw remainder.
