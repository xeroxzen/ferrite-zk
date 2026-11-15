# Contributing

Thank you for helping. The most valuable contribution right now needs no
code: a `zkctl diagnose` report from a real device (see the
[README](README.md#help-wanted-device-reports)).

## Ground rules

- **No personal or biometric data, anywhere.** Never put device passwords,
  fingerprint or face templates, names, user IDs, card numbers, serial
  numbers or MAC addresses in issues, pull requests, logs or test data.
  Captures must be scrubbed; see
  [tests/fixtures/captures/README.md](tests/fixtures/captures/README.md).
- **Clean room.** [pyzk](https://github.com/fananimi/pyzk) is GPL-2.0 and is
  a behavioural cross-reference only. Do not copy its code, comments or
  identifiers (beyond protocol constant names) into the crates, and do not
  commit its test captures. `tools/` is GPL-2.0-only, uses pyzk to generate
  fixtures, and is never part of a crate.
- **Destructive operations** (anything that writes, deletes, clears,
  enrols, restarts or powers off) must be marked **Destructive** in rustdoc,
  require `--yes` in `zkctl`, and run in hardware tests only with
  `ZK_ALLOW_DESTRUCTIVE=1`.
- **Protocol behaviour tied to the open questions** (Q1 to Q7 in
  [DECISIONS.md](docs/DECISIONS.md)) changes only with evidence from a
  device capture or diagnose report.

## Development

Rust 1.88 or later. Before opening a pull request:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo deny check                      # cargo install cargo-deny
```

CI runs these on stable and on 1.88. Clippy runs in pedantic mode
(workspace `[lints]`); keep it free of warnings rather than adding
`allow`s, and give any `allow` a `reason`.

Everything is tested against the mock device in
`crates/zkteco-client/tests/support/`, which can inject faults (dropped
connections and replies, corrupt checksums, split and coalesced TCP
frames, UDP loss, duplication and reordering). New client behaviour comes
with a mock test. `zkctl` tests run the real binary against the mock
(`assert_cmd`), with JSON outputs pinned by `insta` snapshots:

```sh
cargo insta review                    # cargo install cargo-insta
```

Hardware tests are opt-in; see the [README](README.md#testing).

The pre-commit hook in `.githooks/` refuses unscrubbed captures:

```sh
git config core.hooksPath .githooks
```

Man pages and completion scripts:

```sh
cargo xtask man target/man
cargo xtask completions target/completions
```

## Design records

Decisions with lasting consequences are recorded in
[docs/DECISIONS.md](docs/DECISIONS.md). A change that alters one should
update it in the same pull request.

## License

Contributions are licensed as MIT OR Apache-2.0, at the user's option,
without additional terms (unless you state otherwise). Contributions to
`tools/` are GPL-2.0-only.
