# Device compatibility

Record layouts and protocol behaviour observed on real devices. Each row is
one device configuration, reported with `zkctl diagnose` (whose JSON output
has one field per column) and checked by a maintainer.

The table is also code: `crates/zkteco-client/src/compat.rs` holds the same
rows, and a test fails if the two differ. When a write needs the user record
layout and the device has no users to measure, the client looks it up here
(DECISIONS.md D23, D27).

## Columns

| Column | Source | Matching |
|---|---|---|
| Model | `~DeviceName` | identifying |
| Firmware | `CMD_GET_VERSION` | identifying |
| Platform | `~Platform` | identifying |
| ~UserExtFmt | option value, or `unavailable` | identifying |
| ~ExtendFmt | option value, or `unavailable` | identifying |
| User record | bytes per user record, or `-` | result |
| Attendance record | bytes per attendance record, or `-` | result |
| Reply IDs | `echoed`, `mismatches discarded`, `not echoed (lenient required)` | informational |
| Transport | `tcp`, `udp` or `tcp+udp` | informational |
| Verified by | who checked the report, and how | rows without it are ignored |

A row applies to a device when every identifying column equals the device's
value or is `*`. If the applicable verified rows give different user record
sizes, the result is inconclusive and the write fails, asking for
`ClientConfig::user_format`.

Rules for adding a row:

- Only from a `zkctl diagnose` report on real hardware. No rows from
  documentation, forum posts or pyzk's device list.
- Use `*` only where reports from several firmware versions or models agree.
- Cells are plain text: no backticks, no `|`.

## Table

| Model | Firmware | Platform | ~UserExtFmt | ~ExtendFmt | User record | Attendance record | Reply IDs | Transport | Verified by |
|---|---|---|---|---|---|---|---|---|---|
