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

# Each event runs its own hook, and herdr can report the same status twice, so one
# dialog can reach here more than once; a second Enter would answer whatever prompt
# comes next. Enter is sent once per pane and foreground process group: the group is
# the claude that showed this dialog, so a claude started again in the same pane gets
# a new group and is answered too. mkdir is the atomic claim. A failed send gives the
# claim back so a later event for the same dialog can try again (the screen check above
# still has to pass, so an Enter that did land is not repeated once claude redraws).
# Two sweeps keep the directory small: the loop drops this pane's older claims at once,
# and the age sweep drops claims of panes that never show the dialog again (closed).
info="$("$herdr" pane process-info --pane "$pane" 2>/dev/null)" || exit 0
re_pg='"foreground_process_group_id":([0-9]+)'
[[ "$info" =~ $re_pg ]] || exit 0
pg="${BASH_REMATCH[1]}"
state="${HERDR_PLUGIN_STATE_DIR:-}"
[[ -n "$state" && -d "$state" ]] || { echo "HERDR_PLUGIN_STATE_DIR is not a directory" >&2; exit 1; }
key="sent-${pane//[^A-Za-z0-9]/_}"
mkdir "$state/$key-$pg" 2>/dev/null || exit 0
for old in "$state/$key"-*; do
  [[ "$old" == "$state/$key-$pg" ]] || rm -rf "$old"
done
find "$state" -maxdepth 1 -name 'sent-*' -mmin +1440 -exec rm -rf {} + 2>/dev/null

"$herdr" pane send-keys "$pane" Enter || { rmdir "$state/$key-$pg"; exit 1; }
