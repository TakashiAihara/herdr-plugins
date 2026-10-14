#!/usr/bin/env bash
# Runs hook.sh against a fake herdr that prints a canned screen and records send-keys.
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
run() {
  printf '%s\n' "$2" > "$tmp/screen"
  HERDR_PLUGIN_EVENT_JSON="$1" HERDR_BIN_PATH="$tmp/herdr" HERDR_PLUGIN_STATE_DIR="$tmp/state" \
    FAKE_SCREEN="$tmp/screen" FAKE_SENT="$tmp/sent" "$BASH" "$hook"
}
check() {
  local name="$1" json="$2" screen="$3" want="$4"
  rm -rf "$tmp/state"; mkdir -p "$tmp/state"; : > "$tmp/sent"
  run "$json" "$screen"
  local got; got="$(cat "$tmp/sent")"
  if [[ "$got" == "$want" ]]; then echo "ok   $name"; else echo "FAIL $name: want [$want] got [$got]"; fail=1; fi
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
check "nested agent without a top-level one is ignored" '{"event":"x","data":{"type":"pane_agent_status_changed","pane_id":"w9:p1","workspace_id":"w9","agent_status":"blocked","state_labels":{"agent":"claude"}}}' "$dialog" ""
check "empty event is ignored" "" "$dialog" ""

rm -rf "$tmp/state"; mkdir -p "$tmp/state"; : > "$tmp/sent"
run "$(ev w9:p1 blocked claude)" "$dialog"; run "$(ev w9:p1 blocked claude)" "$dialog"
if [[ "$(cat "$tmp/sent")" == "w9:p1 Enter" ]]; then echo "ok   second event for the same dialog sends nothing"; else echo "FAIL second event: got [$(cat "$tmp/sent")]"; fail=1; fi

rm -f "$tmp/state/sent-w9_p1/at"; : > "$tmp/sent"
run "$(ev w9:p1 blocked claude)" "$dialog"
if [[ -z "$(cat "$tmp/sent")" ]]; then echo "ok   a lock whose stamp is not written yet counts as fresh"; else echo "FAIL unwritten stamp: got [$(cat "$tmp/sent")]"; fail=1; fi

echo "$(( $(date +%s) - 11 ))" > "$tmp/state/sent-w9_p1/at"; : > "$tmp/sent"
run "$(ev w9:p1 blocked claude)" "$dialog"
if [[ "$(cat "$tmp/sent")" == "w9:p1 Enter" ]]; then echo "ok   a lock older than 10 s lets the next dialog through"; else echo "FAIL stale lock: got [$(cat "$tmp/sent")]"; fail=1; fi

exit "$fail"
