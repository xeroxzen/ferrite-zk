# tools/

**License: GPL-2.0-only** (see `LICENSE` in this directory). These scripts
import [pyzk](https://github.com/fananimi/pyzk), which is GPL-2.0. Nothing in
this directory is part of any published crate; the Rust crates are
MIT OR Apache-2.0.

## gen_fixtures.py

Drives pyzk (pinned to a commit in the script header) against an in-process
fake device and writes JSON fixtures to
`crates/zkteco-proto/tests/fixtures/generated/`. The fixtures record what pyzk
sends and how it parses what it receives, so `zkteco-proto`'s golden tests
check byte-for-byte agreement with the reference implementation.

```sh
uv run tools/gen_fixtures.py
```

Output is deterministic (fixed seed); CI regenerates it and fails on drift.
The fixtures prove agreement with pyzk, not with hardware. Hardware evidence
comes from `tests/fixtures/captures/`.
