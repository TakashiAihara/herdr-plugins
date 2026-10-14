#!/usr/bin/env bash
# Herdr reports a pane whose Claude Code shows the development-channels warning as
# blocked. Enter is sent only when that dialog is the live prompt at the bottom of the
# screen: the same words higher up (a transcript that printed this file) sit above some
# other prompt, whose own options are then the bottom lines, so it is left alone.
set -u

json="${HERDR_PLUGIN_EVENT_JSON:-}"
herdr="${HERDR_BIN_PATH:-herdr}"

# The event's own fields lead the data object in this order (herdr 0.9.3). Matching
# them as one run keeps a same-named key inside a nested object such as state_labels
# from being read instead; any other shape is ignored.
re='"data":\{"type":"pane_agent_status_changed","pane_id":"([^"]+)","workspace_id":"[^"]*","agent_status":"([^"]+)","agent":"([^"]+)"'
[[ "$json" =~ $re ]] || exit 0
pane="${BASH_REMATCH[1]}"
[[ "${BASH_REMATCH[2]}" == blocked && "${BASH_REMATCH[3]}" == claude ]] || exit 0

screen="$("$herdr" pane read "$pane" --source visible --lines 40 2>/dev/null)" || exit 0
[[ "$screen" == *"WARNING: Loading development channels"* ]] || exit 0

tail4=()
while IFS= read -r line; do
  line="${line#"${line%%[![:space:]]*}"}"
  line="${line%"${line##*[![:space:]]}"}"
  [[ -n "$line" ]] && tail4+=("$line")
done <<< "$screen"
n=${#tail4[@]}
(( n >= 3 )) || exit 0
bottom="${tail4[n-3]}|${tail4[n-2]}|${tail4[n-1]}"
[[ "$bottom" == "❯ 1. I am using this for local development|2. Exit|Enter to confirm"* ]] || exit 0

# Each event runs its own hook, so two blocked events for one dialog would both see it.
# The first to create the pane's lock sends; others within 10 s stop. An empty stamp
# means the owner has not written it yet, which also counts as fresh.
state="${HERDR_PLUGIN_STATE_DIR:-${TMPDIR:-/tmp}}"
lock="$state/sent-${pane//[^A-Za-z0-9]/_}"
now=$(date +%s)
if ! mkdir "$lock" 2>/dev/null; then
  last=$(cat "$lock/at" 2>/dev/null)
  [[ -n "$last" ]] && (( now - last >= 10 )) || exit 0
  rm -rf "$lock"
  mkdir "$lock" 2>/dev/null || exit 0
fi
echo "$now" > "$lock/at"

exec "$herdr" pane send-keys "$pane" Enter
