# zkctl

Command-line tool for ZKTeco biometric and access-control devices: device
information, users, attendance export, live event streaming and
fingerprint enrolment. Built on
[`zkteco-client`](https://crates.io/crates/zkteco-client). Part of the
[Ferrite](https://github.com/xeroxzen/ferrite-zk) project.

**Status: alpha, untested on physical devices.** Every command is tested
against a mock device.

```sh
cargo install zkctl --version 0.1.0-alpha.2

zkctl device add front --host 192.168.1.201
zkctl device set-password front          # stored in the OS keyring
zkctl -d front info
zkctl -d front attendance pull --since 24h --output ndjson
zkctl -d front -d back live --reconnect --backfill --state live.json --output ndjson
```

## Commands

| Command | |
|---|---|
| `info` | Firmware, platform, name, counters and record layouts |
| `time get` | Print the device clock |
| `users list` | List users (`--filter`) |
| `attendance pull` | Export attendance (`--since`, `--until`, `--with-users`, `--state` for incremental export); several `-d` at once |
| `live` | Stream live events until Ctrl-C (`--reconnect`, `--backfill`, `--state` to catch up after a restart, `--with-users`); several `-d` merged |
| `diagnose` | Survey a device and print a report for the compatibility table |
| `time set` | **Destructive.** Set the clock (`<RFC 3339>` or `--sync`) |
| `users add`, `update`, `delete` | **Destructive.** Change users, by `--user-id` or `--uid` |
| `enroll` | **Destructive.** Enrol a fingerprint, showing progress |
| `templates export`, `import` | Fingerprint templates to and from a file. Biometric data |
| `attendance clear`, `data clear` | **Destructive and irreversible** |
| `restart`, `poweroff` | **Destructive** |
| `device add`, `list`, `remove`, `show`, `set-password` | Named devices in the config file |
| `completions <shell>` | Shell completion script |

Read commands connect read-only: the client refuses any command outside a
fixed allowlist of reads before it is sent. Destructive commands do nothing
without `--yes`; `--dry-run` prints the packets that would be sent and
sends no writes. The clears also ask for the device name to be typed (or
`--confirm-device NAME`). Template commands need
`--i-understand-biometric-data` as well and write files with mode 0600.

## Output and exit codes

Data goes to standard output, logs to standard error (`-v`, `-vv`, `-q`).
`--output json` gives one JSON document; `--output ndjson` one object per
line, for streams and record lists. Attendance records and live events
share one versioned schema, documented in the
[guide](https://github.com/xeroxzen/ferrite-zk/blob/main/docs/GUIDE.md#the-event-schema)
and [as JSON Schema](https://github.com/xeroxzen/ferrite-zk/blob/main/docs/schema/event-v1.json).

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Error |
| 2 | Usage error (bad arguments, missing `--yes`, unknown device) |
| 3 | Connection error or timeout |
| 4 | Authentication failed |
| 5 | The device refused a command, or a table kept changing while it was read (transient; safe to retry) |
| 6 | Partial success (rejected records; see standard error) |
| 7 | Aborted (confirmation declined, or interrupted) |

## Passwords

Taken from `--password`, then `ZK_PASSWORD`, then the OS keyring
(`zkctl device set-password NAME`), then an interactive prompt. They are
never written to the config file.

See the [user guide](https://github.com/xeroxzen/ferrite-zk/blob/main/docs/GUIDE.md)
for configuration, live capture in production and data protection.

## License

MIT or Apache-2.0, at your option.
