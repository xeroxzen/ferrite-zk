# Records `zkctl live` merging two demo devices across dropped connections,
# with punches recorded while offline coming back as `backfill`, to
# docs/demo/live.gif. From the repository root:
#
#   vhs docs/demo/live.tape
#
# Needs vhs (https://github.com/charmbracelet/vhs) 0.8 or later. Uses a
# throwaway config file, so your own zkctl profiles are not touched. The
# seeds make the people and punches repeatable; times are the real clock.

Output docs/demo/live.gif

Set Shell "bash"
Set FontSize 14
Set Width 1200
Set Height 640
Set Padding 16
Set TypingSpeed 35ms

Env ZKCTL_CONFIG "/tmp/zkctl-demo/config.toml"
Env ZKCTL_NO_KEYRING "1"

Hide
Type "cargo build -q -p zkctl && cargo build -q -p zkteco-client --example demo_device && echo built"
Enter
Wait@10m /built/
Type "export PATH=$PWD/target/debug:$PATH && rm -rf /tmp/zkctl-demo && mkdir -p /tmp/zkctl-demo"
Enter
Type "target/debug/examples/demo_device --name front --port 14370 --seed 1 --drop-every 20 >/dev/null &"
Enter
Type "target/debug/examples/demo_device --name lobby --port 14371 --seed 2 --drop-every 26 >/dev/null &"
Enter
Sleep 1s
Type "clear"
Enter
Show

Type "# Two demo devices are running: front on 14370, lobby on 14371."
Enter
Sleep 1s
Type "zkctl device add front --host 127.0.0.1 --port 14370 --transport tcp"
Enter
Sleep 1s
Type "zkctl device add lobby --host 127.0.0.1 --port 14371 --transport tcp"
Enter
Sleep 1s
Type "zkctl live -d front -d lobby --reconnect --backfill"
Enter
Sleep 48s
Ctrl+C
Sleep 2s

Hide
Type "kill %1 %2"
Enter
