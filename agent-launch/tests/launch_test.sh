#!/usr/bin/env bash
# Runs launch.sh against a fake herdr that records what it was asked to do.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
script="$here/../launch.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
unset HERDR_PANE_ID TMUX TMUX_PANE

cat > "$tmp/herdr" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_LOG"
case "$1 $2" in
  "pane list") cat "$FAKE_LIST" ;;
  "plugin pane") printf '%s\n' "$@" > "$FAKE_OPEN" ;;
  "workspace create") [ -n "${FAKE_WS_FAIL:-}" ] && exit 1
    printf '{"result":{"workspace":{"workspace_id":"w9"},"root_pane":{"cwd":"/x","focused":true,"pane_id":"w9:p1","revision":0}}}' ;;
  "pane run") printf '%s' "$4" > "$FAKE_RUN" ;;
esac
FAKE
chmod +x "$tmp/herdr"

export HERDR_BIN_PATH="$tmp/herdr" FAKE_LOG="$tmp/log" FAKE_LIST="$tmp/list" FAKE_OPEN="$tmp/open" FAKE_RUN="$tmp/run"
export HERDR_PLUGIN_CONFIG_DIR="$tmp/cfg"
mkdir -p "$HERDR_PLUGIN_CONFIG_DIR"

fail=0
check() { if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1"; echo "  want: $3"; echo "  got:  $2"; fail=1; fi; }
reset() { rm -f "$FAKE_LOG" "$FAKE_OPEN" "$FAKE_RUN" "$HERDR_PLUGIN_CONFIG_DIR/command"; }

# open: the focused pane's cwd goes to the popup; another pane's does not.
printf '{"result":{"panes":[{"agent":"claude","cwd":"/a","focused":false,"pane_id":"p1"},{"cwd":"/b c","focused":true,"pane_id":"p2"}]}}' > "$FAKE_LIST"
reset; bash "$script" open; rc=$?
check "open exits 0" "$rc" 0
check "open passes the focused pane's cwd" "$(grep -c '^AGENT_LAUNCH_CWD=/b c$' "$FAKE_OPEN")" 1
check "open opens the prompt entrypoint" "$(grep -c '^prompt$' "$FAKE_OPEN")" 1

printf '{"result":{"panes":[{"cwd":"/a","focused":false,"pane_id":"p1"}]}}' > "$FAKE_LIST"
reset; bash "$script" open 2>/dev/null; rc=$?
check "open without a focused pane fails" "$rc" 1
check "open without a focused pane opens nothing" "$([ -e "$FAKE_OPEN" ] && echo opened || echo none)" none

# ui: the default command gets the prompt as one quoted word, run in a workspace at the cwd.
reset; printf '%s\n' "fix it's \$HOME \`x\`" | AGENT_LAUNCH_CWD=/b MODE= bash "$script" ui >/dev/null; rc=$?
check "ui exits 0" "$rc" 0
check "ui creates the workspace at the remembered cwd" "$(grep -c '^workspace create --cwd /b --label fix it' "$FAKE_LOG")" 1
check "ui runs the command in the new root pane" "$(grep -c '^pane run w9:p1 ' "$FAKE_LOG")" 1
got="$(cat "$FAKE_RUN")"
check "prompt is one literal word" "$(eval "printf '%s|' $got")" "claude|fix it's \$HOME \`x\`|"

# ui: a template with {repo} asks for it; empty repo drops the placeholder.
reset; printf '%s\n' 'ccxn {repo} {prompt}' > "$HERDR_PLUGIN_CONFIG_DIR/command"
printf '%s\n%s\n' "o/r" "do x" | AGENT_LAUNCH_CWD=/b bash "$script" ui >/dev/null
check "repo is passed as its own word" "$(cat "$FAKE_RUN")" "ccxn 'o/r' 'do x'"
reset; printf '%s\n' 'ccxn {repo} {prompt}' > "$HERDR_PLUGIN_CONFIG_DIR/command"
printf '\n%s\n' "do x" | AGENT_LAUNCH_CWD=/b bash "$script" ui >/dev/null
check "empty repo leaves it out" "$(cat "$FAKE_RUN")" "ccxn  'do x'"

# ui: an empty prompt starts nothing.
reset; printf '\n' | AGENT_LAUNCH_CWD=/b bash "$script" ui >/dev/null; rc=$?
check "empty prompt exits 0" "$rc" 0
check "empty prompt creates nothing" "$([ -e "$FAKE_LOG" ] && echo called || echo none)" none

# ui: a failed workspace create runs nothing and fails.
reset; printf '%s\n\n' "do x" | FAKE_WS_FAIL=1 AGENT_LAUNCH_CWD=/b bash "$script" ui >/dev/null 2>&1; rc=$?
check "failed workspace create exits 1" "$rc" 1
check "failed workspace create runs nothing" "$([ -e "$FAKE_RUN" ] && echo ran || echo none)" none

exit $fail
