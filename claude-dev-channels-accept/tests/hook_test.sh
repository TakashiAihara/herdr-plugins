#!/usr/bin/env bash
# Runs hook.sh against a fake herdr that prints a canned screen and process group and
# records send-keys.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
hook="$here/../hook.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/herdr" <<'FAKE'
#!/usr/bin/env bash
case "$1 $2" in
  "pane read") cat "$FAKE_SCREEN" ;;
  "pane send-keys") echo "$3 $4" >> "$FAKE_SENT" ;;
  "pane process-info") printf '{"result":{"process_info":{"foreground_process_group_id":%s,"pane_id":"%s"}}}' "$FAKE_PG" "$4" ;;
esac
FAKE
chmod +x "$tmp/herdr"

dialog='  WARNING: Loading development channels

  --dangerously-load-development-channels is for local channel development only.

  Channels: server:akapen

  ❯ 1. I am using this for local development
    2. Exit

  Enter to confirm · Esc to cancel'
permission='  Bash command
    rm -rf build

  Do you want to proceed?
  ❯ 1. Yes
    2. No

  Esc to cancel'
stale="$dialog
⏺ Read README.md
$permission"

fail=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1: $2"; fail=1; }
run() {
  printf '%s\n' "$2" > "$tmp/screen"
  HERDR_PLUGIN_EVENT_JSON="$1" HERDR_BIN_PATH="$tmp/herdr" HERDR_PLUGIN_STATE_DIR="$tmp/state" \
    FAKE_SCREEN="$tmp/screen" FAKE_SENT="$tmp/sent" FAKE_PG="${FAKE_PG:-100}" "$BASH" "$hook"
}
sent() { cat "$tmp/sent"; }
check() {
  local name="$1" json="$2" screen="$3" want="$4"
  rm -rf "$tmp/state"; mkdir -p "$tmp/state"; : > "$tmp/sent"
  run "$json" "$screen"
  if [[ "$(sent)" == "$want" ]]; then pass "$name"; else fail "$name" "want [$want] got [$(sent)]"; fi
}

ev() { printf '{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"%s","workspace_id":"w9","agent_status":"%s","agent":"%s"%s}}' "$1" "$2" "$3" "${4:-}"; }

check "blocked claude on the dialog gets Enter" "$(ev w9:p1 blocked claude)" "$dialog" "w9:p1 Enter"
check "labels after the fields do not matter" "$(ev w9:p1 blocked claude ',"state_labels":{"blocked":"x"}')" "$dialog" "w9:p1 Enter"
check "permission prompt is left alone" "$(ev w9:p1 blocked claude)" "$permission" ""
check "permission prompt below stale dialog text is left alone" "$(ev w9:p1 blocked claude)" "$stale" ""
check "idle pane is left alone" "$(ev w9:p1 idle claude)" "$dialog" ""
check "non-claude agent is left alone" "$(ev w9:p1 blocked codex)" "$dialog" ""
check "dialog body without its title is left alone" "$(ev w9:p1 blocked claude)" "${dialog#*only.}" ""
check "dialog with Exit selected is left alone" "$(ev w9:p1 blocked claude)" "${dialog/❯ 1./  1.}" ""
check "dialog cut before its footer is left alone" "$(ev w9:p1 blocked claude)" "${dialog%Enter to confirm*}" ""
check "trailing spaces and CR on the lines are ignored" "$(ev w9:p1 blocked claude)" "$(printf '%s\n' "$dialog" | sed "s/\$/   $(printf '\r')/")" "w9:p1 Enter"
check "footer with more text after it still counts" "$(ev w9:p1 blocked claude)" "${dialog/Esc to cancel/Esc to cancel · ctrl+c to quit}" "w9:p1 Enter"
check "footer that does not start the last line is left alone" "$(ev w9:p1 blocked claude)" "${dialog/Enter to confirm/Press Enter to confirm}" ""
check "nested agent without a top-level one is ignored" '{"event":"x","data":{"type":"pane_agent_status_changed","pane_id":"w9:p1","workspace_id":"w9","agent_status":"blocked","state_labels":{"agent":"claude"}}}' "$dialog" ""
check "empty event is ignored" "" "$dialog" ""

rm -rf "$tmp/state"; mkdir -p "$tmp/state"; : > "$tmp/sent"
run "$(ev w9:p1 blocked claude)" "$dialog"; run "$(ev w9:p1 blocked claude)" "$dialog"
if [[ "$(sent)" == "w9:p1 Enter" ]]; then pass "second event for the same dialog sends nothing"; else fail "second event" "got [$(sent)]"; fi

: > "$tmp/sent"
FAKE_PG=200 run "$(ev w9:p1 blocked claude)" "$dialog"
if [[ "$(sent)" == "w9:p1 Enter" ]]; then pass "claude started again in the same pane is answered"; else fail "restart" "got [$(sent)]"; fi
if [[ ! -e "$tmp/state/sent-w9_p1-100" && -d "$tmp/state/sent-w9_p1-200" ]]; then pass "the previous claude's claim is removed"; else fail "claims" "$(ls "$tmp/state")"; fi

: > "$tmp/sent"
FAKE_PG=200 run "$(ev w9:p10 blocked claude)" "$dialog"
if [[ "$(sent)" == "w9:p10 Enter" && -d "$tmp/state/sent-w9_p1-200" ]]; then pass "another pane's claim neither blocks nor is removed"; else fail "other pane" "got [$(sent)] $(ls "$tmp/state")"; fi

rm -rf "$tmp/state"; : > "$tmp/sent"
printf '%s\n' "$dialog" > "$tmp/screen"
HERDR_PLUGIN_EVENT_JSON="$(ev w9:p1 blocked claude)" HERDR_BIN_PATH="$tmp/herdr" FAKE_SCREEN="$tmp/screen" FAKE_SENT="$tmp/sent" FAKE_PG=100 "$BASH" "$hook"
if [[ -z "$(sent)" ]]; then pass "without a plugin state dir nothing is sent"; else fail "no state dir" "got [$(sent)]"; fi

exit "$fail"
