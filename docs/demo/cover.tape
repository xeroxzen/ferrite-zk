# Records the README cover: `zkctl live` merging two demo devices that drop
# their connections, as a still at docs/demo/cover.png (1200 x 630, a
# social-card size). The recording itself goes to /tmp: only the still is
# kept. From the repository root:
#
#   vhs docs/demo/cover.tape
#
# Needs vhs (https://github.com/charmbracelet/vhs) 0.7 or later, for
# Screenshot. Builds zkctl from this checkout and uses a throwaway config,
# so neither an installed zkctl nor your own profiles are involved.

Output "/tmp/zkctl-cover.gif"

Set Shell zsh
Set FontSize 22
Set Width 1200
Set Height 630
Set Padding 36
Set Theme "Catppuccin Mocha"
Set WindowBar Colorful

Env ZKCTL_CONFIG "/tmp/zkctl-cover/config.toml"
Env ZKCTL_NO_KEYRING "1"

Hide
# The marker is computed, so the typed command does not match it.
Type "cargo build -q -p zkctl && cargo build -q -p zkteco-client --example demo_device && echo built-$((1+1))"
Enter
Wait+Screen@10m /built-2/
Type "export PATH=$PWD/target/debug:$PATH && rm -rf /tmp/zkctl-cover && mkdir -p /tmp/zkctl-cover"
Enter
Type "zkctl device add front --host 127.0.0.1 --port 14370 --transport tcp >/dev/null"
Enter
Type "zkctl device add lobby --host 127.0.0.1 --port 14371 --transport tcp >/dev/null"
Enter
Type "target/debug/examples/demo_device --port 14370 --name front --seed 7 --drop-every 8 >/dev/null 2>&1 &"
Enter
Type "target/debug/examples/demo_device --port 14371 --name lobby --seed 11 --drop-every 8 >/dev/null 2>&1 &"
Enter
Sleep 1s
Type "clear"
Enter
Show

Type "zkctl live -d front -d lobby --reconnect --backfill"
Enter
# Both devices drop 8 s after zkctl connects, stay offline 5 s, and are
# back on zkctl's third attempt (backoff 1, 2, 4 s), at about 16 s. The
# still keeps the command line, the drops, the reconnects and the backfill.
Sleep 17s
Screenshot docs/demo/cover.png
# The screenshot is taken on the next frame; let it land before Ctrl+C.
Sleep 500ms
Ctrl+C
Sleep 1s

Hide
Type "kill %1 %2"
Enter
