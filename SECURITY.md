# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through
[GitHub Security Advisories](https://github.com/xeroxzen/ferrite-zk/security/advisories/new)
("Report a vulnerability" on the repository's Security tab). Do not open a
public issue.

Include the affected crate and version, what an attacker can do, and the
steps to reproduce. Fixes are released as soon as they are ready, and the
advisory is published with credit unless you prefer otherwise.

Only the latest release is supported while the project is in alpha.

## Scope

In scope:

- `zkteco-proto`, `zkteco-client` and `zkctl`.
- Decoding of untrusted device input: panics, unbounded memory or loops,
  or wrong data returned without an error.
- Read-only mode or dry-run letting a write reach the device.
- `zkctl` leaking secrets or personal data: passwords written to disk or
  logs, export files readable by other users, identifiers or templates
  left in scrubbed reports or captures.

Out of scope:

- Weaknesses of the ZK protocol itself: it has no encryption, and the comm
  key is a weak, reversible obfuscation. Anyone on the network path can
  read and alter traffic. Keep devices on an isolated network.
- Vulnerabilities in device firmware.
- `tools/`, which is never distributed.

## Never post credentials or biometrics

Do not put device passwords (comm keys), fingerprint or face templates,
names, user IDs, card numbers, serial numbers, MAC addresses or unscrubbed
packet captures in issues, pull requests or advisories. If a report needs
such data to reproduce, say so and a maintainer will arrange a private
channel. Reports containing it will be edited or deleted.
