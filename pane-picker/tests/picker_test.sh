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
[ "$1 $2" = "agent focus" ] && [ -n "${FAKE_AGENT_FOCUS_FAIL:-}" ] && exit 1
if [ "$1 $2" = "workspace focus" ] && [ -n "${FAKE_WS_FAIL:-}" ]; then echo "herdr: no such workspace" >&2; exit 1; fi
if [ "$1 $2" = "pane list" ]; then [ -n "${FAKE_PANE_LIST_FAIL:-}" ] && exit 1; cat "$FAKE_LIST"; fi
exit 0
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
# BSD sed has no \x escapes, so drop ESC with tr and the rest of the color code with sed.
plain() { tr -d '\033' | sed 's/\[[0-9;]*m//g'; }


fresh
check "default order follows the pane list" "$(ids)" "w1:p1 w1:p2 w2:p1 w2:p2 "
check "search text holds every token value" \
  "$(bash "$picker" lines | grep '^w2:p1' | cut -f3 | plain)" "write docs /src/alpha devin working alpha urgent kaneo"
check "search text holds the cwd" "$(bash "$picker" lines | grep '^w1:p2' | cut -f3 | grep -c '/src/beta/')" "1"
check "header names the first sort and group, then the keys" "$(run header)" "sort: priority  group: none
ctrl-s sort  ctrl-g group  alt-up/alt-down move  alt-1..9 prio  alt-0 clear"

fresh
run act group-next
check "group-next moves to repo" "$(run header | head -1)" "sort: priority  group: repo"
check "groups by repo, falls back to the cwd basename, in first-seen order" "$(ids)" \
  "-:0 w1:p1 w2:p1 -:1 w1:p2 -:2 w2:p2 "
check "group headers show the name and count" \
  "$(bash "$picker" lines | grep '^-:' | cut -f2 | plain | tr '\n' ';')" "── alpha (2) ──;── beta (1) ──;── gamma (1) ──;"
check "group headers have no search text" "$(bash "$picker" lines | grep '^-:' | cut -f3 | tr -d '\n')" ""
run act group-next
run act group-next
check "group-next wraps back to none" "$(run header | head -1)" "sort: priority  group: none"

fresh
jq '.result.panes[3].tokens.repo = "delta"' "$tmp/list.json" > "$tmp/delta.json"
export PANE_PICKER_LIST="$tmp/delta.json"
run act group-next
run act prio 2 w2:p2
check "the repo token wins over the cwd, and the group of the top pane comes first" "$(ids)" \
  "-:0 w2:p2 -:1 w1:p1 w2:p1 -:2 w1:p2 "
export PANE_PICKER_LIST="$tmp/list.json"

fresh
run act prio 5 w2:p2
run act prio 3 w1:p2
check "priority sort puts higher priority first" "$(ids)" "w2:p2 w1:p2 w1:p1 w2:p1 "
check "priority shows as a star column" "$(bash "$picker" lines | sed -n 1p | cut -f2 | grep -c '^★5 ')" "1"
run act clear w2:p2
check "clear removes the priority" "$(ids)" "w1:p2 w1:p1 w2:p1 w2:p2 "
check "prio rejects 0" "$(run act prio 0 w1:p1 2>&1; echo "rc=$?")" "pane-picker: prio needs 1-9
rc=1"

fresh
run act prio 4 w2:p1
sed 's/"w2:p1"/"w8:p8"/' "$tmp/list.json" > "$tmp/moved.json"
check "priority follows the agent session when the pane id changes" \
  "$(PANE_PICKER_LIST="$tmp/moved.json" bash "$picker" lines | sed -n 1p | cut -f1)" "w8:p8"
check "the moved pane keeps its priority on the next write" \
  "$(PANE_PICKER_LIST="$tmp/moved.json" bash "$picker" act prio 2 w2:p2; jq -c '.prio' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  '{"s3":4,"w2:p2":2}'
run act move down w1:p1
run act prio 6 w1:p2
jq '.result.panes |= map(select(.pane_id != "w1:p2"))' "$tmp/list.json" > "$tmp/closed.json"
check "closed panes are dropped from priorities and positions on the next write" \
  "$(PANE_PICKER_LIST="$tmp/closed.json" bash "$picker" act prio 1 w2:p2; jq -c '[.prio, (.order | keys)]' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  '[{"s3":4,"w2:p2":1},["s1","s3","w2:p2"]]'
echo garbage > "$tmp/garbage.json"
check "a failed pane list fails the move and leaves the state untouched" \
  "$(PANE_PICKER_LIST="$tmp/garbage.json" bash "$picker" act move down w1:p1 2>/dev/null; echo "rc=$?"; jq -c '.prio' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  'rc=1
{"s3":4,"w2:p2":1}'

fresh
jq '.result.panes[0].agent_session.value = "" | .result.panes[2].agent_session.value = ""' "$tmp/list.json" > "$tmp/nosession.json"
export PANE_PICKER_LIST="$tmp/nosession.json"
run act prio 3 w1:p1
check "an empty session id falls back to the pane id" "$(jq -c '.prio' "$HERDR_PLUGIN_STATE_DIR/state.json")" '{"w1:p1":3}'
export PANE_PICKER_LIST="$tmp/list.json"

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
run act move down w1:p1
run act sort-next
check "a move under another sort only trades the two panes' manual positions" "$(run header | head -1)$(ids)" \
  "sort: priority  group: nonew1:p2 w2:p1 w2:p2 w1:p1 "

fresh
run act group-next
run act move down w2:p1
check "move does not cross a group header" "$(ids)$(jq -c '.order' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  "-:0 w1:p1 w2:p1 -:1 w1:p2 -:2 w2:p2 {}"
run act move up w2:p1
check "move inside a group works while grouped" "$(ids)" "-:0 w2:p1 w1:p1 -:1 w1:p2 -:2 w2:p2 "

fresh
jq '.result.panes[3].tokens.prio = "7"' "$tmp/list.json" > "$tmp/fieldprio.json"
export PANE_PICKER_LIST="$tmp/fieldprio.json"
echo '{"priority":{"source":"tokens.prio"},"columns":["prio","title"]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "priority can come from a pane field" "$(ids)" "w2:p2 w1:p1 w1:p2 w2:p1 "
check "configured columns are shown" "$(bash "$picker" lines | sed -n 2p | cut -f2)" "★2  fix login"
run act prio 1 w2:p2
check "a manual priority overrides the field, even when lower" "$(ids)" "w1:p1 w2:p2 w1:p2 w2:p1 "
export PANE_PICKER_LIST="$tmp/list.json"

fresh
echo '{"groups":[{"name":"ws","by":["workspace_id"]}],"sorts":[{"name":"title","by":["title:desc"]}]}' \
  > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "configured sorts and groups replace the defaults" "$(run header | head -1)" "sort: title  group: ws"
check "descending text sort inside configured groups" "$(ids)" "-:0 w2:p1 w2:p2 -:1 w1:p2 w1:p1 "

fresh
echo '{"sorts":[{"name":"dir","by":["cwd:basename:desc"]}]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a sort field keeps :basename and :desc" "$(ids)" "w2:p2 w1:p2 w1:p1 w2:p1 "

fresh
echo '{"groups":[{"name":"x","by":["tokens.nothing"]}]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "panes without any group value go to (none)" \
  "$(bash "$picker" lines | sed -n 1p | cut -f2 | plain)" "── (none) (4) ──"

fresh
jq '.result.panes[1].title = "a\tb\nc\u001b[31m"' "$tmp/list.json" > "$tmp/ctrl.json"
check "control characters in a value cannot split or color a row" \
  "$(PANE_PICKER_LIST="$tmp/ctrl.json" bash "$picker" lines | wc -l | tr -d ' ')|$(PANE_PICKER_LIST="$tmp/ctrl.json" bash "$picker" lines | sed -n 2p | cut -f2 | tr -s ' ')" \
  "4| unknown a b c [31m"
jq '.result.panes[3].title = "write"' "$tmp/list.json" > "$tmp/prefix.json"
echo '{"sorts":[{"name":"t","by":["title:desc"]}]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "descending text sort puts a string after its longer extensions" \
  "$(PANE_PICKER_LIST="$tmp/prefix.json" bash "$picker" lines | cut -f1 | tr '\n' ' ')" "w2:p1 w2:p2 w1:p2 w1:p1 "

fresh
jq '.result.panes[0].title = "日本" | .result.panes[1].title = "ab" | .result.panes[2].title = "abc" | .result.panes[3].title = "x"' \
  "$tmp/list.json" > "$tmp/wide.json"
echo '{"columns":["title","agent"]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "columns are padded by terminal cell width" \
  "$(PANE_PICKER_LIST="$tmp/wide.json" bash "$picker" lines | cut -f2 | sed -n '1p;3p' | tr '\n' ';')" "日本  claude;abc   devin;"

fresh
mkdir -p "$HERDR_PLUGIN_STATE_DIR"
echo 'garbage' > "$HERDR_PLUGIN_STATE_DIR/state.json"
check "a broken state file is treated as empty" "$(ids)" "w1:p1 w1:p2 w2:p1 w2:p2 "
echo '{"prio":[1]}' > "$HERDR_PLUGIN_STATE_DIR/state.json"
check "a state file of the wrong shape is treated as empty" "$(ids)" "w1:p1 w1:p2 w2:p1 w2:p2 "

fresh
echo '{"sorts":[{"name":"repo","by":["tokens.repo"]}]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "panes without the sort value go last" "$(ids)" "w1:p1 w2:p1 w2:p2 w1:p2 "

fresh
echo '{"keys":{"sort":"ctrl-x"}}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a partial keys object keeps the other default keys" "$(run header | sed -n 2p)" \
  "ctrl-x sort  ctrl-g group  alt-up/alt-down move  alt-1..9 prio  alt-0 clear"

shape_error() {
  printf '%s\n%s' "pane-picker: $HERDR_PLUGIN_CONFIG_DIR/config.json: columns and search must be non-empty lists of fields, sorts and groups non-empty lists of {name, by} with unique names, priority.source a string, and keys sort/group/up/down/clear non-empty strings" "rc=1"
}
fresh
echo '{"sorts":[]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "an empty sort list is a config error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "$(shape_error)"
fresh
echo '{"keys":{"up":""}}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "an empty key is a config error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "$(shape_error)"
fresh
echo '{"columns":"title"}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "columns that are not a list are a config error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "$(shape_error)"

fresh
echo 'not json' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a broken config fails loudly" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" \
  "pane-picker: $HERDR_PLUGIN_CONFIG_DIR/config.json is not a JSON object
rc=1"

fresh
echo '{"sorts":[{"name":"a","by":["title"]},{"name":"a","by":["cwd"]}]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "duplicate sort names are a config error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "$(shape_error)"
fresh
echo '{"priority":"manual"}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a priority that is not an object is a config error, without raw jq errors" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "$(shape_error)"
fresh
echo '{"search":[]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "an empty search list is a config error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "$(shape_error)"
fresh
echo '{"columns":[]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "an empty column list is a config error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "$(shape_error)"
fresh
echo '{"keys":[]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "keys that are not an object are a config error, without raw jq errors" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "$(shape_error)"

fresh
mkdir -p "$HERDR_PLUGIN_STATE_DIR"
echo '{"sort":"status","order":{"w2:p2":1000001}}' > "$HERDR_PLUGIN_STATE_DIR/state.json"
run act move down w1:p1
run act sort-next
run act sort-next
check "renumbering breaks manual ties by list order, not by the sort on screen" "$(run header | head -1)$(ids)" \
  "sort: manual  group: nonew2:p2 w1:p2 w1:p1 w2:p1 "

fresh
mkdir -p "$HERDR_PLUGIN_STATE_DIR"
echo '{"sort":"manual","order":{"s1":1000001}}' > "$HERDR_PLUGIN_STATE_DIR/state.json"
run act move down w1:p1
check "move works when a saved position ties with a default one" "$(ids)" "w1:p2 w1:p1 w2:p1 w2:p2 "

fresh
jq '.result.panes[1].cwd = "/"' "$tmp/list.json" > "$tmp/root.json"
run act group-next
check "the basename of / is /" \
  "$(PANE_PICKER_LIST="$tmp/root.json" bash "$picker" lines | grep '^-:1' | cut -f2 | plain)" "── / (1) ──"

fresh
jq '.result.panes[0].agent_session = "s1"' "$tmp/list.json" > "$tmp/strsession.json"
PANE_PICKER_LIST="$tmp/strsession.json" bash "$picker" act prio 2 w1:p1
check "a session that is not an object falls back to the pane id" "$(jq -c .prio "$HERDR_PLUGIN_STATE_DIR/state.json")" '{"w1:p1":2}'

fresh
echo '{"result":{}}' > "$tmp/noresult.json"
check "a pane list without .result.panes fails" "$(PANE_PICKER_LIST="$tmp/noresult.json" bash "$picker" lines 2>&1 >/dev/null | tail -1; echo "rc=${PIPESTATUS[0]}")" \
  "pane-picker: cannot read the pane list
rc=1"

fresh
jq '.result.panes[1].title = "x\u001b]0;evil\u0007y"' "$tmp/list.json" > "$tmp/esc.json"
check "preview strips control characters" \
  "$(PANE_PICKER_LIST="$tmp/esc.json" bash "$picker" preview w1:p2 | grep '^title' | tr -d '\033\007' | cut -f2)" "x ]0;evil y"

fresh
jq '.result.panes[3].agent_session = {"value": "s1"}' "$tmp/list.json" > "$tmp/samesession.json"
export PANE_PICKER_LIST="$tmp/samesession.json"
run act sort-next
run act move down w1:p2
check "panes sharing a session id are ordered and prioritized apart" \
  "$(ids)$(run act prio 3 w2:p2; jq -c '[.prio, (.order | keys)]' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  'w1:p1 w2:p1 w1:p2 w2:p2 [{"w2:p2":3},["s3","w1:p1","w1:p2","w2:p2"]]'
export PANE_PICKER_LIST="$tmp/list.json"

fresh
jq '.result.panes[1].tokens = {"k\u001bx": "v"}' "$tmp/list.json" > "$tmp/esckey.json"
check "preview strips control characters from token names" \
  "$(PANE_PICKER_LIST="$tmp/esckey.json" bash "$picker" preview w1:p2 | grep -c $'\033')" "0"

fresh
jq '.result.panes += [.result.panes[1]]' "$tmp/list.json" > "$tmp/dup.json"
PANE_PICKER_LIST="$tmp/dup.json" bash "$picker" focus w1:p2
check "a duplicated pane id is focused once" "$(cat "$FAKE_CALLS" | tr '\n' ';')" "workspace focus w1;tab focus w1:t2;"

fresh
check "enter on a pane accepts" "$(run enter w1:p1)" "accept"
check "enter on a group header steps down" "$(run enter -:alpha)" "down"
check "enter with no match does nothing" "$(run enter '')" ""

fresh
run focus w1:p1
check "an agent pane is focused through agent focus" "$(cat "$FAKE_CALLS")" "agent focus w1:p1"
: > "$FAKE_CALLS"
run focus w1:p2
check "a plain pane focuses its workspace then its tab" "$(cat "$FAKE_CALLS" | tr '\n' ';')" \
  "workspace focus w1;tab focus w1:t2;"
: > "$FAKE_CALLS"
FAKE_AGENT_FOCUS_FAIL=1 bash "$picker" focus w2:p1
check "an agent pane whose agent is gone falls back to workspace and tab" "$(cat "$FAKE_CALLS" | tr '\n' ';')" \
  "agent focus w2:p1;workspace focus w2;tab focus w2:t1;"
: > "$FAKE_CALLS"
check "a pane that is gone fails" "$(run focus w7:p7 2>&1; echo "rc=$?")" "pane-picker: pane w7:p7 is gone
rc=1"
check "a group header focuses nothing and succeeds" "$(run focus -:alpha 2>&1; echo "rc=$?"; cat "$FAKE_CALLS")" "rc=0"

fresh
run open
check "open launches the picker pane entrypoint" "$(cat "$FAKE_CALLS")" \
  "plugin pane open --plugin takashiaihara.pane-picker --entrypoint picker"

fresh
check "preview lists fields and tokens" "$(run preview w2:p1 | grep -E '^(title|tokens.task)' | tr '\t' '=' | tr '\n' ';')" \
  "title=write docs;tokens.task=urgent kaneo;"
check "preview of a group header is empty" "$(run preview -:alpha)" ""

# The popup itself: a fake fzf records its argv and answers with FAKE_FZF_OUT / FAKE_FZF_RC.
mkdir -p "$tmp/bin"
cat > "$tmp/bin/fzf" <<'FAKE'
#!/usr/bin/env bash
if [ "${1:-}" = --version ]; then echo "${FAKE_FZF_VERSION:-0.72.0} (fake)"; exit 0; fi
printf '%s\n' "$@" > "$FAKE_FZF_ARGS"
cat > "$FAKE_FZF_ARGS.stdin"
[ -n "${FAKE_FZF_OUT:-}" ] && echo "$FAKE_FZF_OUT"
exit "${FAKE_FZF_RC:-0}"
FAKE
chmod +x "$tmp/bin/fzf"
ui() { PATH="$tmp/bin:$PATH" FAKE_FZF_ARGS="$tmp/fzf-args$n" bash "$picker" ui </dev/null; }

fresh
check "ui refuses an fzf older than 0.71" "$(FAKE_FZF_VERSION=0.70.1 ui 2>&1; echo "rc=$?")" \
  "pane-picker: needs fzf 0.71+ (found 0.70.1)
Press Enter.
rc=1"
check "ui accepts fzf 1.0" "$(FAKE_FZF_VERSION=1.0.0 FAKE_FZF_RC=130 ui 2>&1; echo "rc=$?")" "rc=0"

fresh
FAKE_FZF_OUT=w1:p2 ui
check "ui focuses the pane fzf returns" "$(cat "$FAKE_CALLS" | tr '\n' ';')" "workspace focus w1;tab focus w1:t2;"
check "ui feeds fzf the list rows" "$(cut -f1 "$tmp/fzf-args$n.stdin" | tr '\n' ' ')" "w1:p1 w1:p2 w2:p1 w2:p2 "
check "ui searches only the search text and returns the id" \
  "$(grep -cxE -e '--nth=2' -e '--accept-nth=1' -e '--with-nth=2\.\.' -e '--id-nth=1' "$tmp/fzf-args$n")" "4"
check "ui binds every configured key" \
  "$(grep -oE '^(ctrl-s|ctrl-g|alt-up|alt-down|alt-0|alt-[1-9]|enter):' "$tmp/fzf-args$n" | tr -d '\n')" \
  "ctrl-s:ctrl-g:alt-up:alt-down:alt-0:enter:alt-1:alt-2:alt-3:alt-4:alt-5:alt-6:alt-7:alt-8:alt-9:"

fresh
check "ui treats Esc as a clean close" "$(FAKE_FZF_RC=130 ui 2>&1; echo "rc=$?"; cat "$FAKE_CALLS")" "rc=0"
check "ui treats no match as a clean close" "$(FAKE_FZF_RC=1 ui 2>&1; echo "rc=$?"; cat "$FAKE_CALLS")" "rc=0"
check "ui reports an fzf error" "$(FAKE_FZF_RC=2 ui 2>&1; echo "rc=$?")" "pane-picker: fzf exited with 2
Press Enter.
rc=1"

fresh
echo '{"sorts":[]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "ui shows a config error in the popup" "$(ui 2>&1 | head -1 | cut -c1-40; echo "rc=${PIPESTATUS[0]}")" \
  "pane-picker: $(printf '%s' "$HERDR_PLUGIN_CONFIG_DIR" | cut -c1-27)
rc=1"
fresh
check "ui shows a broken pane list in the popup" "$(PANE_PICKER_LIST="$tmp/garbage.json" ui 2>&1; echo "rc=$?")" \
  "pane-picker: cannot read the pane list
Press Enter.
rc=1"

fresh
check "ui shows a focus failure in the popup" "$(FAKE_FZF_OUT=w7:p7 ui 2>&1; echo "rc=$?")" "pane-picker: pane w7:p7 is gone
Press Enter.
rc=1"
fresh
check "ui shows the last line of a focus failure" "$(FAKE_WS_FAIL=1 FAKE_FZF_OUT=w1:p2 ui 2>&1; echo "rc=$?")" "pane-picker: cannot focus w1:p2
Press Enter.
rc=1"
fresh
echo '{"sorts":[]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "ui shows a config error in one line" "$(ui 2>&1 | wc -l | tr -d ' ')" "2"
fresh
echo '{"result":{"panes":[]}}' > "$tmp/nopanes.json"
PANE_PICKER_LIST="$tmp/nopanes.json" FAKE_FZF_RC=1 ui
check "ui gives fzf no rows for an empty pane list" "$(wc -c < "$tmp/fzf-args$n.stdin" | tr -d ' ')" "0"

fresh
export FAKE_LIST="$tmp/list.json"
mkdir -p "$tmp/uitmp"
check "a jump exits 0 and removes the snapshot" \
  "$(unset PANE_PICKER_LIST; TMPDIR="$tmp/uitmp" FAKE_FZF_OUT=w1:p2 ui 2>&1; echo "rc=$?"; ls "$tmp/uitmp" | wc -l | tr -d ' ')" "rc=0
0"
: > "$FAKE_CALLS"
check "ui takes one pane list snapshot for the list and the jump" \
  "$(unset PANE_PICKER_LIST; FAKE_FZF_OUT=w1:p1 ui; cat "$FAKE_CALLS" | tr '\n' ';')" "pane list;agent focus w1:p1;"
check "ui shows a failed herdr pane list in the popup" \
  "$(unset PANE_PICKER_LIST; FAKE_PANE_LIST_FAIL=1 ui 2>&1; echo "rc=$?")" "pane-picker: herdr pane list failed
Press Enter.
rc=1"

if [ "$fails" -gt 0 ]; then echo "$fails failed"; exit 1; fi
echo "all passed"
