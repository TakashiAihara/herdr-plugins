#!/usr/bin/env bash
# Runs the session-resume binary against a fake herdr that records what it was asked to do and
# against transcripts made here, never the real ~/.claude. The popup itself is tested in Rust;
# here are the subcommands as the plugin manifest runs them.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
bin="${SESSION_RESUME_BIN:-$here/../target/release/session-resume}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
unset HERDR_PANE_ID TMUX TMUX_PANE HERDR_PLUGIN_ID XDG_CONFIG_HOME XDG_STATE_HOME

cat > "$tmp/herdr" <<'FAKE'
#!/usr/bin/env bash
{ printf '%s\n' "$@"; echo '--'; } >> "$FAKE_LOG"
[ -n "${FAKE_FAIL:-}" ] && { echo denied >&2; exit 1; }
exit 0
FAKE
chmod +x "$tmp/herdr"
export HERDR_BIN_PATH="$tmp/herdr" FAKE_LOG="$tmp/log"
export HOME="$tmp/home" CLAUDE_CONFIG_DIR="$tmp/claude"
export HERDR_PLUGIN_CONFIG_DIR="$tmp/cfg" HERDR_PLUGIN_STATE_DIR="$tmp/state"
mkdir -p "$HOME" "$HERDR_PLUGIN_CONFIG_DIR" "$CLAUDE_CONFIG_DIR/projects/-w-x"

fail=0
check() { if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1"; echo "  want: $3"; echo "  got:  $2"; fail=1; fi; }
reset() { rm -f "$FAKE_LOG"; }
# The popup draws on /dev/tty when it can; with no controlling terminal it reports the error it
# found while preparing instead, which is what is checked here and cannot hang waiting for a key.
# Without setsid (macOS) a controlling terminal cannot be dropped portably, so the two ui cases are
# skipped there when one exists; CI has none, so they still run on every platform in CI.
can_notty() { command -v setsid >/dev/null 2>&1 || ! { : </dev/tty; } 2>/dev/null; }
notty() { if command -v setsid >/dev/null 2>&1; then setsid -w "$@"; else "$@"; fi; }

reset; "$bin" open; rc=$?
check "open exits 0" "$rc" 0
check "open opens the picker popup" \
  "$(sed -n '/^plugin$/,/^--$/p' "$FAKE_LOG" | tr '\n' '|')" \
  "plugin|pane|open|--plugin|takashiaihara.session-resume|--entrypoint|picker|--|"

reset; FAKE_FAIL=1 "$bin" open 2>"$tmp/err"; rc=$?
check "open fails with the herdr error when the popup cannot open" "$rc:$(grep -c denied "$tmp/err")" "1:1"

# index: transcripts made here, one of them with a line that is cut off.
{
  printf '%s\n' '{"type":"user","timestamp":"2026-11-04T10:00:00Z","cwd":"/w/x","message":{"content":"hello"}}'
  printf '%s' '{"type":"user","timestamp":"2026-11-04T10:'
} > "$CLAUDE_CONFIG_DIR/projects/-w-x/s1.jsonl"
printf '%s\n' '{"type":"attachment"}' > "$CLAUDE_CONFIG_DIR/projects/-w-x/s2.jsonl"
out="$("$bin" index 2>"$tmp/err")"; rc=$?
check "index exits 0" "$rc" 0
check "index counts the listed session and reads both transcripts" \
  "$(printf '%s' "$out" | sed 's/, [0-9]* ms$//')" "sessions 1, refreshed 2"
check "index keeps the text in files only its user can read" \
  "$(stat -c '%a' "$HERDR_PLUGIN_STATE_DIR/index" 2>/dev/null || stat -f '%Lp' "$HERDR_PLUGIN_STATE_DIR/index")" 700
check "index wrote the lowercased user text" "$(cat "$HERDR_PLUGIN_STATE_DIR/index/s1.user")" hello
out="$("$bin" index 2>/dev/null)"
check "index run again reads nothing" "$(printf '%s' "$out" | sed 's/, [0-9]* ms$//')" "sessions 1, refreshed 0"

printf '{"scope":"nope"}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
"$bin" index 2>"$tmp/err"; rc=$?
check "index with an invalid config fails naming the problem" "$rc:$(grep -c 'scope must be one of' "$tmp/err")" "1:1"

if can_notty; then
# ui: an invalid config is an error naming the problem, not a fallback, and Herdr is not touched.
reset; notty "$bin" ui </dev/null 2>"$tmp/err"; rc=$?
check "ui with an invalid config fails naming the problem" "$rc:$(grep -c 'scope must be one of' "$tmp/err")" "1:1"
check "ui with an invalid config touches nothing" "$([ -e "$FAKE_LOG" ] && echo called || echo none)" none
rm -f "$HERDR_PLUGIN_CONFIG_DIR/config.json"

# ui: a projects directory that is not there is an error, not an empty list.
reset; CLAUDE_CONFIG_DIR="$tmp/absent" notty "$bin" ui </dev/null 2>"$tmp/err"; rc=$?
check "ui without a projects directory fails naming it" "$rc:$(grep -c 'cannot read the projects directory' "$tmp/err")" "1:1"
else
echo "skip ui cases: no setsid to drop the controlling terminal"
fi

reset; "$bin" bogus 2>"$tmp/err"; rc=$?
check "an unknown subcommand is a usage error" "$rc:$(grep -c usage "$tmp/err")" "1:1"
check "version names the binary" "$("$bin" version | cut -d' ' -f1)" session-resume

exit $fail
