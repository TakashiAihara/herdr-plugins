#!/usr/bin/env bash
# Runs picker.sh against a canned `herdr pane list` and a fake herdr that records calls.
# The fzf UI itself is not driven; everything it shows and does goes through the subcommands
# tested here.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
picker="$here/../picker.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/herdr" <<'FAKE'
#!/usr/bin/env bash
echo "$*" >> "$FAKE_CALLS"
FAKE
chmod +x "$tmp/herdr"

cat > "$tmp/list.json" <<'JSON'
{"result":{"panes":[
 {"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1","agent":"claude","agent_status":"done",
  "agent_session":{"value":"s1"},"title":"fix login","cwd":"/src/alpha",
  "tokens":{"repo":"alpha","prio":"2"}},
 {"pane_id":"w1:p2","workspace_id":"w1","tab_id":"w1:t2","agent_status":"unknown",
  "title":"shell","cwd":"/src/beta/"},
 {"pane_id":"w2:p1","workspace_id":"w2","tab_id":"w2:t1","agent":"devin","agent_status":"working",
  "agent_session":{"value":"s3"},"title":"write docs","cwd":"/src/alpha",
  "tokens":{"repo":"alpha","task":"urgent kaneo"}},
 {"pane_id":"w2:p2","workspace_id":"w2","tab_id":"w2:t2","agent":"claude","agent_status":"idle",
  "title":"review","cwd":"/src/gamma","tokens":{"repo":"gamma"}}
]}}
JSON

fails=0
n=0
fresh() {
  n=$((n + 1))
  export HERDR_PLUGIN_STATE_DIR="$tmp/state$n" HERDR_PLUGIN_CONFIG_DIR="$tmp/config$n"
  export PANE_PICKER_LIST="$tmp/list.json" HERDR_BIN_PATH="$tmp/herdr" FAKE_CALLS="$tmp/calls$n"
  mkdir -p "$HERDR_PLUGIN_CONFIG_DIR"
  : > "$FAKE_CALLS"
}
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1"; echo "  want: $3"; echo "  got:  $2"; fails=$((fails + 1)); fi
}
ids() { bash "$picker" lines | cut -f1 | tr '\n' ' '; }
run() { bash "$picker" "$@"; }

fresh
check "default order follows the pane list" "$(ids)" "w1:p1 w1:p2 w2:p1 w2:p2 "
check "search text holds every token value" \
  "$(bash "$picker" lines | grep '^w2:p1' | cut -f3 | grep -c 'urgent kaneo')" "1"
check "search text holds the cwd" "$(bash "$picker" lines | grep '^w1:p2' | cut -f3 | grep -c '/src/beta/')" "1"
check "header names the first sort and group" "$(run header | head -1)" "sort: priority  group: none"

fresh
run act group-next
check "group-next moves to repo" "$(run header | head -1)" "sort: priority  group: repo"
check "groups by repo, falls back to the cwd basename, in first-seen order" "$(ids)" \
  "- w1:p1 w2:p1 - w1:p2 - w2:p2 "
check "group header shows the name and count" \
  "$(bash "$picker" lines | sed -n 1p | cut -f2 | sed 's/\x1b\[[0-9;]*m//g')" "── alpha (2) ──"
check "the second group is beta from cwd" \
  "$(bash "$picker" lines | sed -n 4p | cut -f2 | sed 's/\x1b\[[0-9;]*m//g')" "── beta (1) ──"
run act group-next
run act group-next
check "group-next wraps back to none" "$(run header | head -1)" "sort: priority  group: none"

fresh
run act prio 5 w2:p2
run act prio 3 w1:p2
check "priority sort puts higher priority first" "$(ids)" "w2:p2 w1:p2 w1:p1 w2:p1 "
check "priority shows as a star column" "$(bash "$picker" lines | sed -n 1p | cut -f2 | cut -c1-2)" "★5"
run act clear w2:p2
check "clear removes the priority" "$(ids)" "w1:p2 w1:p1 w2:p1 w2:p2 "
check "prio rejects 0" "$(run act prio 0 w1:p1 2>&1; echo "rc=$?")" "pane-picker: prio needs 1-9
rc=1"

fresh
run act prio 4 w1:p1
sed 's/"w1:p1"/"w9:p9"/' "$tmp/list.json" > "$tmp/moved.json"
check "priority follows the agent session when the pane id changes" \
  "$(PANE_PICKER_LIST="$tmp/moved.json" bash "$picker" lines | sed -n 1p | cut -f1)" "w9:p9"
check "the moved pane keeps its priority on the next write" \
  "$(PANE_PICKER_LIST="$tmp/moved.json" bash "$picker" act prio 2 w2:p2; jq -c '.prio' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  '{"s1":4,"w2:p2":2}'
jq '.result.panes |= map(select(.pane_id != "w2:p2"))' "$tmp/list.json" > "$tmp/closed.json"
check "closed panes are dropped from the priority map on the next write" \
  "$(PANE_PICKER_LIST="$tmp/closed.json" bash "$picker" act prio 1 w1:p2; jq -c '.prio' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  '{"s1":4,"w1:p2":1}'
echo garbage > "$tmp/empty.json"
check "a failed pane list fails the move and leaves the state untouched" \
  "$(PANE_PICKER_LIST="$tmp/empty.json" bash "$picker" act move down w1:p1 2>/dev/null; echo "rc=$?"; jq -c '.prio' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  'rc=1
{"s1":4,"w1:p2":1}'

fresh
run act move down w1:p1
check "move down swaps with the next pane" "$(ids)" "w1:p2 w1:p1 w2:p1 w2:p2 "
run act move up w2:p1
check "move up swaps with the previous pane" "$(ids)" "w1:p2 w2:p1 w1:p1 w2:p2 "
run act move up w1:p2
check "move up at the top changes nothing" "$(ids)" "w1:p2 w2:p1 w1:p1 w2:p2 "
run act move down w2:p2
check "move down at the bottom changes nothing" "$(ids)" "w1:p2 w2:p1 w1:p1 w2:p2 "
run act sort-next
check "manual sort keeps the moved order" "$(run header | head -1)$(ids)" "sort: manual  group: nonew1:p2 w2:p1 w1:p1 w2:p2 "
run act sort-next
check "status sort orders by agent_status" "$(ids)" "w1:p1 w2:p2 w1:p2 w2:p1 "

fresh
echo '{"priority":{"source":"tokens.prio"},"columns":["prio","title"]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "priority can come from a pane field" "$(ids)" "w1:p1 w1:p2 w2:p1 w2:p2 "
check "configured columns are shown" "$(bash "$picker" lines | sed -n 1p | cut -f2)" "★2  fix login"
run act prio 9 w2:p2
check "a manual priority overrides the field" "$(ids)" "w2:p2 w1:p1 w1:p2 w2:p1 "

fresh
echo '{"groups":[{"name":"ws","by":["workspace_id"]}],"sorts":[{"name":"title","by":["title:desc"]}]}' \
  > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "configured sorts and groups replace the defaults" "$(run header | head -1)" "sort: title  group: ws"
check "descending text sort inside configured groups" "$(ids)" "- w2:p1 w2:p2 - w1:p2 w1:p1 "

fresh
echo '{"groups":[{"name":"x","by":["tokens.nothing"]}]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "panes without any group value go to (none)" \
  "$(bash "$picker" lines | sed -n 1p | cut -f2 | sed 's/\x1b\[[0-9;]*m//g')" "── (none) (4) ──"

fresh
mkdir -p "$HERDR_PLUGIN_STATE_DIR"
echo 'garbage' > "$HERDR_PLUGIN_STATE_DIR/state.json"
check "a broken state file is treated as empty" "$(ids)" "w1:p1 w1:p2 w2:p1 w2:p2 "

fresh
echo '{"sorts":[{"name":"repo","by":["tokens.repo"]}]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "panes without the sort value go last" "$(ids)" "w1:p1 w2:p1 w2:p2 w1:p2 "

fresh
echo 'not json' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a broken config fails loudly" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" \
  "pane-picker: $HERDR_PLUGIN_CONFIG_DIR/config.json is not a JSON object
rc=1"

fresh
run focus w1:p1
check "an agent pane is focused through agent focus" "$(cat "$FAKE_CALLS")" "agent focus w1:p1"
: > "$FAKE_CALLS"
run focus w1:p2
check "a plain pane focuses its workspace then its tab" "$(cat "$FAKE_CALLS" | tr '\n' ';')" \
  "workspace focus w1;tab focus w1:t2;"
: > "$FAKE_CALLS"
check "a pane that is gone fails" "$(run focus w7:p7 2>&1; echo "rc=$?")" "pane-picker: pane w7:p7 is gone
rc=1"
check "a group header focuses nothing and succeeds" "$(run focus - 2>&1; echo "rc=$?"; cat "$FAKE_CALLS")" "rc=0"

fresh
run open
check "open launches the picker pane entrypoint" "$(cat "$FAKE_CALLS")" \
  "plugin pane open --plugin takashiaihara.pane-picker --entrypoint picker"

fresh
check "preview lists fields and tokens" "$(run preview w2:p1 | grep -E '^(title|tokens.task)' | tr '\t' '=' | tr '\n' ';')" \
  "title=write docs;tokens.task=urgent kaneo;"

if [ "$fails" -gt 0 ]; then echo "$fails failed"; exit 1; fi
echo "all passed"
