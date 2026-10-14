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
  Channels: server:akapen
  ❯ 1. I am using this for local development
    2. Exit'
other='  Do you want to make this edit?
  ❯ 1. Yes
    2. No'

fail=0
check() {
  local name="$1" json="$2" screen="$3" want="$4"
  printf '%s\n' "$screen" > "$tmp/screen"
  : > "$tmp/sent"
  HERDR_PLUGIN_EVENT_JSON="$json" HERDR_BIN_PATH="$tmp/herdr" FAKE_SCREEN="$tmp/screen" FAKE_SENT="$tmp/sent" "$BASH" "$hook"
  local got; got="$(cat "$tmp/sent")"
  if [[ "$got" == "$want" ]]; then echo "ok   $name"; else echo "FAIL $name: want [$want] got [$got]"; fail=1; fi
}

ev() { printf '{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"%s","workspace_id":"w9","agent_status":"%s","agent":"%s"}}' "$1" "$2" "$3"; }

check "blocked claude on the dialog gets Enter" "$(ev w9:p1 blocked claude)" "$dialog" "w9:p1 Enter"
check "other blocked prompt is left alone" "$(ev w9:p1 blocked claude)" "$other" ""
check "idle pane is left alone" "$(ev w9:p1 idle claude)" "$dialog" ""
check "non-claude agent is left alone" "$(ev w9:p1 blocked codex)" "$dialog" ""
check "warning title alone is not enough" "$(ev w9:p1 blocked claude)" "  WARNING: Loading development channels" ""
check "accept option alone is not enough" "$(ev w9:p1 blocked claude)" "  ❯ 1. I am using this for local development" ""
check "event without pane_id is ignored" '{"event":"x","data":{"agent_status":"blocked","agent":"claude"}}' "$dialog" ""
check "empty event is ignored" "" "$dialog" ""
exit "$fail"
