#!/usr/bin/env bash
# Runs the agent-launch binary against a fake herdr that records what it was asked to do. The
# popup itself is tested in Rust; here are the subcommands as the plugin manifest runs them.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
bin="${AGENT_LAUNCH_BIN:-$here/../target/release/agent-launch}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
unset HERDR_PANE_ID TMUX TMUX_PANE AGENT_LAUNCH_CWD HERDR_PLUGIN_ID

cat > "$tmp/herdr" <<'FAKE'
#!/usr/bin/env bash
{ printf '%s\n' "$@"; echo '--'; } >> "$FAKE_LOG"
[ "$1 $2" = "pane list" ] && cat "$FAKE_LIST"
exit 0
FAKE
chmod +x "$tmp/herdr"
export HERDR_BIN_PATH="$tmp/herdr" FAKE_LOG="$tmp/log" FAKE_LIST="$tmp/list"
export HERDR_PLUGIN_CONFIG_DIR="$tmp/cfg"
mkdir -p "$HERDR_PLUGIN_CONFIG_DIR"

fail=0
check() { if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1"; echo "  want: $3"; echo "  got:  $2"; fail=1; fi; }
reset() { rm -f "$FAKE_LOG"; }
# The popup draws on /dev/tty when it can; with no controlling terminal it reports the error it
# found while preparing instead, which is what is checked here and cannot hang waiting for a key.
# Without setsid (macOS) a controlling terminal cannot be dropped portably, so the two ui cases are
# skipped there when one exists; CI has none, so they still run on every platform in CI.
can_notty() { command -v setsid >/dev/null 2>&1 || ! { : </dev/tty; } 2>/dev/null; }
notty() { if command -v setsid >/dev/null 2>&1; then setsid -w "$@"; else "$@"; fi; }

printf '{"result":{"panes":[{"agent":"claude","cwd":"/a","focused":false,"pane_id":"p1"},{"cwd":"/b c","focused":true,"pane_id":"p2"}]}}' > "$FAKE_LIST"
reset; "$bin" open; rc=$?
check "open exits 0" "$rc" 0
check "open opens the prompt popup with the focused pane's cwd as one argument" \
  "$(sed -n '/^plugin$/,/^--$/p' "$FAKE_LOG" | tr '\n' '|')" \
  "plugin|pane|open|--plugin|takashiaihara.agent-launch|--entrypoint|prompt|--env|AGENT_LAUNCH_CWD=/b c|--|"

printf '{"result":{"panes":[{"cwd":"/a","focused":false,"pane_id":"p1"}]}}' > "$FAKE_LIST"
reset; "$bin" open 2>"$tmp/err"; rc=$?
check "open without a focused pane fails" "$rc" 1
check "open without a focused pane opens nothing" "$(grep -c '^plugin$' "$FAKE_LOG")" 0
check "open without a focused pane says why" "$(grep -c 'no focused pane' "$tmp/err")" 1

printf '{"result":{"panes":[{"cwd":"/a","focused":true,"pane_id":"p1"},{"cwd":"/b","focused":true,"pane_id":"p2"}]}}' > "$FAKE_LIST"
reset; "$bin" open 2>/dev/null; rc=$?
check "two focused panes fail and open nothing" "$rc:$(grep -c '^plugin$' "$FAKE_LOG")" "1:0"

if can_notty; then
# ui: without the remembered directory it refuses before touching Herdr, even with no terminal.
reset; notty "$bin" ui </dev/null 2>"$tmp/err"; rc=$?
check "ui without the cwd fails naming the variable" "$rc:$(grep -c AGENT_LAUNCH_CWD "$tmp/err")" "1:1"
check "ui without the cwd creates nothing" "$([ -e "$FAKE_LOG" ] && echo called || echo none)" none

# ui: an invalid config is an error naming the problem, not a fallback.
printf '{"agents":[]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
reset; AGENT_LAUNCH_CWD=/b notty "$bin" ui </dev/null 2>"$tmp/err"; rc=$?
check "ui with an invalid config fails naming the problem" "$rc:$(grep -c 'agents must be a non-empty list' "$tmp/err")" "1:1"
check "ui with an invalid config creates nothing" "$([ -e "$FAKE_LOG" ] && echo called || echo none)" none
rm -f "$HERDR_PLUGIN_CONFIG_DIR/config.json"
else
echo "skip ui cases: no setsid to drop the controlling terminal"
fi

reset; "$bin" bogus 2>"$tmp/err"; rc=$?
check "an unknown subcommand is a usage error" "$rc:$(grep -c usage "$tmp/err")" "1:1"
check "version prints the name and the package version" "$("$bin" version)" \
  "agent-launch $(sed -n 's/^version = "\(.*\)"$/\1/p' "$here/../Cargo.toml")"

exit $fail
