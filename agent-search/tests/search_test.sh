#!/usr/bin/env bash
# Runs agent-search against a canned `herdr pane list` and a fake herdr that records calls.
# The interactive popup is tested in Rust; everything it shows and does goes through the subcommands
# tested here.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
# The binary under test: AGENT_SEARCH_BIN, else the release build.
bin="${AGENT_SEARCH_BIN:-$here/../target/release/agent-search}"
pk() { "$bin" "$@"; }
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/herdr" <<'FAKE'
#!/usr/bin/env bash
echo "$*" >> "$FAKE_CALLS"
[ "$1 $2" = "agent focus" ] && [ -n "${FAKE_AGENT_FOCUS_FAIL:-}" ] && exit 1
if [ "$1 $2" = "workspace focus" ] && [ -n "${FAKE_WS_FAIL:-}" ]; then echo "herdr: no such workspace" >&2; exit 1; fi
if [ "$1 $2" = "pane list" ]; then [ -n "${FAKE_PANE_LIST_FAIL:-}" ] && exit 1; cat "$FAKE_LIST"; fi
# report-metadata edits the list file agent-search reads, as Herdr would change its own pane list.
# FAKE_META_FAIL=1 fails every call, FAKE_META_FAIL=<pane id> only the calls for that pane.
if [ "$1 $2" = "pane report-metadata" ]; then
  case "${FAKE_META_FAIL:-}" in 1|"$3") [ -n "${FAKE_META_STDERR:-}" ] && echo "herdr: boom" >&2; exit 1 ;; esac
  f=${AGENT_SEARCH_LIST:-$FAKE_LIST}
  id=$3
  shift 3
  while [ $# -gt 0 ]; do
    case "$1" in
      --token)
        jq --arg id "$id" --arg k "${2%%=*}" --arg v "${2#*=}" \
          '(.result.panes[] | select(.pane_id == $id) | .tokens) |= ((. // {}) + {($k): $v})' "$f" > "$f.new" && command mv -f "$f.new" "$f"
        shift 2 ;;
      --clear-token)
        jq --arg id "$id" --arg k "$2" '(.result.panes[] | select(.pane_id == $id)) |= del(.tokens[$k])' "$f" > "$f.new" && command mv -f "$f.new" "$f"
        shift 2 ;;
      *) shift ;;
    esac
  done
  # FAKE_WRITE_STATE_PANE=<pane id> + FAKE_WRITE_STATE_JSON=<json>: overwrite the plugin's
  # state.json while this call runs, like a writer that finished between restore's plan and now.
  [ "$id" = "${FAKE_WRITE_STATE_PANE:-}" ] && printf %s "$FAKE_WRITE_STATE_JSON" > "$HERDR_PLUGIN_STATE_DIR/state.json"
fi
exit 0
FAKE
chmod +x "$tmp/herdr"

cat > "$tmp/list.orig" <<'JSON'
{"result":{"panes":[
 {"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1","agent":"claude","agent_status":"done",
  "agent_session":{"value":"s1"},"title":"fix login","cwd":"/src/alpha",
  "tokens":{"repo":"alpha","rank":"2"}},
 {"pane_id":"w1:p2","workspace_id":"w1","tab_id":"w1:t2","agent":"claude","agent_status":"unknown",
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
  # Set per command where a test wants one; without it the state file is state.json.
  unset HERDR_SOCKET_PATH
  cp "$tmp/list.orig" "$tmp/list.json"
  export AGENT_SEARCH_LIST="$tmp/list.json" FAKE_LIST="$tmp/list.json" HERDR_BIN_PATH="$tmp/herdr" FAKE_CALLS="$tmp/calls$n"
  mkdir -p "$HERDR_PLUGIN_CONFIG_DIR"
  : > "$FAKE_CALLS"
}
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1"; echo "  want: $3"; echo "  got:  $2"; fails=$((fails + 1)); fi
}
ids() { pk lines | cut -f1 | tr '\n' ' '; }
run() { pk "$@"; }
# BSD sed has no \x escapes, so drop ESC with tr and the rest of the color code with sed.
plain() { tr -d '\033' | sed 's/\[[0-9;]*m//g'; }
# The first 12 hex characters of a string's SHA-256, the tag a session's state file is named for.
tag() { printf %s "$1" | { sha256sum 2>/dev/null || shasum -a 256; } | cut -c1-12; }


fresh
check "default order follows the pane list" "$(ids)" "w1:p1 w1:p2 w2:p1 w2:p2 "
check "search text holds every token value" \
  "$(pk lines | grep '^w2:p1' | cut -f3 | plain)" "write docs /src/alpha devin working alpha urgent kaneo"
check "search text holds the cwd" "$(pk lines | grep '^w1:p2' | cut -f3 | grep -c '/src/beta/')" "1"
check "header names the first sort and group, then the keys" "$(run header)" "sort: priority  group: none
ctrl-s sort  ctrl-g group  alt-up/alt-down move  alt-1..9 prio  alt-0 clear"

fresh
run act group-next
check "group-next moves to repo" "$(run header | head -1)" "sort: priority  group: repo"
check "groups by repo, falls back to the cwd basename, in first-seen order" "$(ids)" \
  "-:0 w1:p1 w2:p1 -:1 w1:p2 -:2 w2:p2 "
check "group headers show the name and count" \
  "$(pk lines | grep '^-:' | cut -f2 | plain | tr '\n' ';')" "── alpha (2) ──;── beta (1) ──;── gamma (1) ──;"
check "group headers have no search text" "$(pk lines | grep '^-:' | cut -f3 | tr -d '\n')" ""
run act group-next
run act group-next
check "group-next wraps back to none" "$(run header | head -1)" "sort: priority  group: none"

fresh
jq '.result.panes[3].tokens.repo = "delta"' "$tmp/list.json" > "$tmp/delta.json"
export AGENT_SEARCH_LIST="$tmp/delta.json"
run act group-next
run act prio 2 w2:p2
check "the repo token wins over the cwd, and the group of the top pane comes first" "$(ids)" \
  "-:0 w2:p2 -:1 w1:p1 w2:p1 -:2 w1:p2 "
export AGENT_SEARCH_LIST="$tmp/list.json"

fresh
run act prio 5 w2:p2
check "act prio asks herdr to set the priority token" "$(cat "$FAKE_CALLS")" \
  "pane report-metadata w2:p2 --source takashiaihara.agent-search --token prio=5"
check "act prio records the priority in the state under the pane key" "$(jq -c .prio "$HERDR_PLUGIN_STATE_DIR/state.json")" '{"w2:p2":5}'
run act prio 3 w1:p2
check "priority sort puts higher priority first" "$(ids)" "w2:p2 w1:p2 w1:p1 w2:p1 "
check "priority shows as a star column" "$(pk lines | sed -n 1p | cut -f2 | grep -c '^★5 ')" "1"
: > "$FAKE_CALLS"
run act clear w2:p2
check "act clear asks herdr to clear the priority token" "$(cat "$FAKE_CALLS")" \
  "pane report-metadata w2:p2 --source takashiaihara.agent-search --clear-token prio"
check "clear removes the priority" "$(ids)" "w1:p2 w1:p1 w2:p1 w2:p2 "
check "clear removes the saved priority" "$(jq -c .prio "$HERDR_PLUGIN_STATE_DIR/state.json")" '{"w1:p2":3}'
check "prio rejects 0" "$(run act prio 0 w1:p1 2>&1; echo "rc=$?")" "agent-search: prio needs 1-9
rc=1"

fresh
run act prio 4 w2:p1
sed 's/"w2:p1"/"w8:p8"/' "$tmp/list.json" > "$tmp/moved.json"
check "the moved pane keeps its saved priority on the next write" \
  "$(AGENT_SEARCH_LIST="$tmp/moved.json" pk act prio 2 w2:p2; jq -c '.prio' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  '{"s3":4,"w2:p2":2}'
run act move down w1:p1
run act prio 6 w1:p2
jq '.result.panes |= map(select(.pane_id != "w1:p2"))' "$tmp/list.json" > "$tmp/closed.json"
check "closed panes are dropped from priorities and positions on the next write" \
  "$(AGENT_SEARCH_LIST="$tmp/closed.json" pk act prio 1 w2:p2; jq -c '[.prio, (.order | keys)]' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  '[{"s3":4,"w2:p2":1},["s1","s3","w2:p2"]]'
echo garbage > "$tmp/garbage.json"
check "a failed pane list fails the move and leaves the state untouched" \
  "$(AGENT_SEARCH_LIST="$tmp/garbage.json" pk act move down w1:p1 2>/dev/null; echo "rc=$?"; jq -c '.prio' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  'rc=1
{"s3":4,"w2:p2":1}'

fresh
jq '.result.panes[0].agent_session.value = "" | .result.panes[2].agent_session.value = ""' "$tmp/list.json" > "$tmp/nosession.json"
export AGENT_SEARCH_LIST="$tmp/nosession.json"
run act prio 3 w1:p1
check "an empty session id falls back to the pane id" "$(jq -c '.prio' "$HERDR_PLUGIN_STATE_DIR/state.json")" '{"w1:p1":3}'
export AGENT_SEARCH_LIST="$tmp/list.json"

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
jq '.result.panes[3].tokens.prio = "7" | .result.panes[0].tokens.prio = "2"' "$tmp/list.json" > "$tmp/fieldprio.json"
export AGENT_SEARCH_LIST="$tmp/fieldprio.json"
echo '{"columns":["prio","title"]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "priority comes from the pane token" "$(ids)" "w2:p2 w1:p1 w1:p2 w2:p1 "
check "configured columns are shown" "$(pk lines | sed -n 2p | cut -f2)" "★2  fix login"
run act prio 1 w2:p2
check "setting a priority writes the token, which replaces the old value" "$(ids)" "w1:p1 w2:p2 w1:p2 w2:p1 "
jq '.result.panes[3].tokens.prio = "x"' "$tmp/list.json" > "$tmp/badprio.json"
check "a token that is not a number is no priority" \
  "$(AGENT_SEARCH_LIST="$tmp/badprio.json" pk lines | grep -c '★')" "0"
export AGENT_SEARCH_LIST="$tmp/list.json"

fresh
jq '.result.panes[3].tokens.rank = "9"' "$tmp/list.json" > "$tmp/rank.json"
export AGENT_SEARCH_LIST="$tmp/rank.json"
echo '{"priority_token":"rank"}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "priority_token names the token that holds the priority" "$(ids)" "w2:p2 w1:p1 w1:p2 w2:p1 "
run act prio 3 w1:p2
check "act prio sets the configured token" "$(cat "$FAKE_CALLS")" \
  "pane report-metadata w1:p2 --source takashiaihara.agent-search --token rank=3"
export AGENT_SEARCH_LIST="$tmp/list.json"

fresh
check "HERDR_PLUGIN_ID is the metadata source" \
  "$(HERDR_PLUGIN_ID=someone.else pk act prio 1 w1:p1; cat "$FAKE_CALLS")" \
  "pane report-metadata w1:p1 --source someone.else --token prio=1"

fresh
run act prio 4 w2:p1
before=$(cat "$HERDR_PLUGIN_STATE_DIR/state.json")
check "a failed herdr call fails act prio and leaves the state untouched" \
  "$(FAKE_META_FAIL=1 pk act prio 2 w2:p2 2>&1; echo "rc=$?"; [ "$(cat "$HERDR_PLUGIN_STATE_DIR/state.json")" = "$before" ] && echo same)" \
  "agent-search: cannot set the priority of w2:p2
rc=1
same"
check "a failed herdr call fails act clear too" "$(FAKE_META_FAIL=1 pk act clear w2:p1 2>&1; echo "rc=$?")" "agent-search: cannot set the priority of w2:p1
rc=1"
check "the failed call changed no token" "$(ids)" "w2:p1 w1:p1 w1:p2 w2:p2 "

fresh
run act prio 4 w2:p1
run act prio 2 w2:p2
jq 'del(.result.panes[] | select(.pane_id == "w2:p1") | .tokens.prio) | (.result.panes[] | select(.pane_id == "w2:p1")).pane_id = "w8:p8"' \
  "$tmp/list.json" > "$tmp/restart.json"
: > "$FAKE_CALLS"
check "restore sets the saved priority again, only on panes that lost the token, under their new id" \
  "$(AGENT_SEARCH_LIST="$tmp/restart.json" pk restore 2>&1; echo "rc=$?"; cat "$FAKE_CALLS")" \
  "rc=0
pane report-metadata w8:p8 --source takashiaihara.agent-search --token prio=4"
: > "$FAKE_CALLS"
check "restore sends nothing when no token is missing" "$(pk restore 2>&1; echo "rc=$?"; cat "$FAKE_CALLS")" "rc=0"
jq 'del(.result.panes[].tokens.prio)' "$tmp/list.json" > "$tmp/lost.json"
: > "$FAKE_CALLS"
check "restore reports each failure and still exits 0" \
  "$(FAKE_META_FAIL=w2:p1 AGENT_SEARCH_LIST="$tmp/lost.json" pk restore 2>&1; echo "rc=$?"; cat "$FAKE_CALLS" | tr '\n' ';')" \
  "agent-search: cannot restore the priority of w2:p1
rc=0
pane report-metadata w2:p1 --source takashiaihara.agent-search --token prio=4;pane report-metadata w2:p2 --source takashiaihara.agent-search --token prio=2;"

fresh
run act prio 4 w2:p1
jq 'del(.result.panes[].tokens.prio)' "$tmp/list.json" > "$tmp/lostkeys.json"
echo '{"keys":{"sort":"x","up":"ctrl-nonsense"}}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
: > "$FAKE_CALLS"
check "restore does not need valid keys" \
  "$(AGENT_SEARCH_LIST="$tmp/lostkeys.json" pk restore 2>&1; echo "rc=$?"; cat "$FAKE_CALLS")" \
  "rc=0
pane report-metadata w2:p1 --source takashiaihara.agent-search --token prio=4"
echo '{"keys":[]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "restore does not need keys that are even an object" \
  "$(AGENT_SEARCH_LIST="$tmp/lostkeys.json" pk restore 2>&1; echo "rc=$?")" "rc=0"
echo '{"columns":["title."]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a bad field still fails restore" \
  "$(AGENT_SEARCH_LIST="$tmp/lostkeys.json" pk restore 2>&1 >/dev/null; echo "rc=$?")" \
  "agent-search: $HERDR_PLUGIN_CONFIG_DIR/config.json: invalid field \"title.\": its path has an empty part
rc=1"
echo '{"columns":"title"}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a shape error in restore no longer names keys" \
  "$(AGENT_SEARCH_LIST="$tmp/lostkeys.json" pk restore 2>&1 >/dev/null; echo "rc=$?")" \
  "agent-search: $HERDR_PLUGIN_CONFIG_DIR/config.json: columns and search must be non-empty lists of fields, sorts and groups non-empty lists of {name, by} with unique names, priority_token a Herdr token name (1-32 letters, digits, _ or -)
rc=1"

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
  "$(pk lines | sed -n 1p | cut -f2 | plain)" "── (none) (4) ──"

fresh
jq '.result.panes[1].title = "a\tb\nc\u001b[31m"' "$tmp/list.json" > "$tmp/ctrl.json"
check "control characters in a value cannot split or color a row" \
  "$(AGENT_SEARCH_LIST="$tmp/ctrl.json" pk lines | wc -l | tr -d ' ')|$(AGENT_SEARCH_LIST="$tmp/ctrl.json" pk lines | sed -n 2p | cut -f2 | tr -s ' ')" \
  "4| unknown a b c [31m"
jq '.result.panes[3].title = "write"' "$tmp/list.json" > "$tmp/prefix.json"
echo '{"sorts":[{"name":"t","by":["title:desc"]}]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "descending text sort puts a string after its longer extensions" \
  "$(AGENT_SEARCH_LIST="$tmp/prefix.json" pk lines | cut -f1 | tr '\n' ' ')" "w2:p1 w2:p2 w1:p2 w1:p1 "

fresh
jq '.result.panes[0].title = "日本" | .result.panes[1].title = "ab" | .result.panes[2].title = "abc" | .result.panes[3].title = "x"' \
  "$tmp/list.json" > "$tmp/wide.json"
echo '{"columns":["title","agent"]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "columns are padded by terminal cell width" \
  "$(AGENT_SEARCH_LIST="$tmp/wide.json" pk lines | cut -f2 | sed -n '1p;3p' | tr '\n' ';')" "日本  claude;abc   devin;"

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
  printf '%s\n%s' "agent-search: $HERDR_PLUGIN_CONFIG_DIR/config.json: columns and search must be non-empty lists of fields, sorts and groups non-empty lists of {name, by} with unique names, priority_token a Herdr token name (1-32 letters, digits, _ or -), and keys sort/group/up/down/clear non-empty strings" "rc=1"
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
  "agent-search: $HERDR_PLUGIN_CONFIG_DIR/config.json is not a JSON object
rc=1"

fresh
echo '{"sorts":[{"name":"a","by":["title"]},{"name":"a","by":["cwd"]}]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "duplicate sort names are a config error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "$(shape_error)"
fresh
for bad in '"a b"' '""' '"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"' '5' '{"a":1}'; do
  fresh
  echo "{\"priority_token\":$bad}" > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
  check "priority_token $bad is a config error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "$(shape_error)"
done
fresh
echo '{"priority_token":"xxxxxxxxxxxxxxxxxxxxxxxxxxxx-_X9"}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a 32-character priority_token of letters, digits, _ and - is accepted" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "rc=0"
fresh
echo '{"priority":"manual"}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "the old priority setting is ignored" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "rc=0"
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
  "$(AGENT_SEARCH_LIST="$tmp/root.json" pk lines | grep '^-:1' | cut -f2 | plain)" "── / (1) ──"

fresh
jq '.result.panes[0].agent_session = "s1"' "$tmp/list.json" > "$tmp/strsession.json"
AGENT_SEARCH_LIST="$tmp/strsession.json" pk act prio 2 w1:p1
check "a session that is not an object falls back to the pane id" "$(jq -c .prio "$HERDR_PLUGIN_STATE_DIR/state.json")" '{"w1:p1":2}'

fresh
echo '{"result":{}}' > "$tmp/noresult.json"
check "a pane list without .result.panes fails" "$(AGENT_SEARCH_LIST="$tmp/noresult.json" pk lines 2>&1 >/dev/null | tail -1; echo "rc=${PIPESTATUS[0]}")" \
  "agent-search: cannot read the pane list
rc=1"

fresh
jq '.result.panes[1].title = "x\u001b]0;evil\u0007y"' "$tmp/list.json" > "$tmp/esc.json"
check "preview strips control characters" \
  "$(AGENT_SEARCH_LIST="$tmp/esc.json" pk preview w1:p2 | grep '^title' | tr -d '\033\007' | sed -E 's/^[^ ]+ +//')" "x ]0;evil y"

fresh
jq '.result.panes[3].agent_session = {"value": "s1"}' "$tmp/list.json" > "$tmp/samesession.json"
export AGENT_SEARCH_LIST="$tmp/samesession.json"
run act sort-next
run act move down w1:p2
check "panes sharing a session id are ordered and prioritized apart" \
  "$(ids)$(run act prio 3 w2:p2; jq -c '[.prio, (.order | keys)]' "$HERDR_PLUGIN_STATE_DIR/state.json")" \
  'w1:p1 w2:p1 w1:p2 w2:p2 [{"w2:p2":3},["s1","s3","w1:p2","w2:p2"]]'
export AGENT_SEARCH_LIST="$tmp/list.json"

fresh
run act prio 4 w1:p1
jq '.result.panes += [.result.panes[0] | .pane_id = "w5:p5" | .agent_session.value = "s1"]' "$tmp/list.json" > "$tmp/laterdup.json"
check "a later pane with the same session id leaves the first pane's priority alone" \
  "$(AGENT_SEARCH_LIST="$tmp/laterdup.json" pk act prio 2 w5:p5; jq -c .prio "$HERDR_PLUGIN_STATE_DIR/state.json")" '{"s1":4,"w5:p5":2}'
check "when the duplicate goes away the first pane still has its priority" \
  "$(pk lines | sed -n 1p | grep -c '^w1:p1	★4 ')" "1"

fresh
jq '.result.panes[1].tokens = {"k\u001bx": "v"}' "$tmp/list.json" > "$tmp/esckey.json"
check "preview strips control characters from token names" \
  "$(AGENT_SEARCH_LIST="$tmp/esckey.json" pk preview w1:p2 | grep -c $'\033')" "0"

fresh
jq 'del(.result.panes[1].agent) | .result.panes += [.result.panes[1]]' "$tmp/list.json" > "$tmp/dup.json"
AGENT_SEARCH_LIST="$tmp/dup.json" pk focus w1:p2
check "a duplicated pane id is focused once" "$(cat "$FAKE_CALLS" | tr '\n' ';')" "workspace focus w1;tab focus w1:t2;"

fresh
jq '.result.panes += [{"pane_id":"w3:p1","workspace_id":"w3","tab_id":"w3:t1","title":"zsh","cwd":"/tmp"}, {"pane_id":"w3:p2","agent":"","title":"empty agent"}]' \
  "$tmp/list.json" > "$tmp/withplain.json"
check "only agent panes are listed" "$(AGENT_SEARCH_LIST="$tmp/withplain.json" pk lines | cut -f1 | tr '\n' ' ')" "w1:p1 w1:p2 w2:p1 w2:p2 "
echo '{"groups":[{"name":"ws","by":["workspace_id"]}]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "plain panes make no group of their own" \
  "$(AGENT_SEARCH_LIST="$tmp/withplain.json" pk lines | grep '^-:' | cut -f2 | plain | tr '\n' ';')" "── w1 (2) ──;── w2 (2) ──;"
rm "$HERDR_PLUGIN_CONFIG_DIR/config.json"
fresh
AGENT_SEARCH_LIST="$tmp/withplain.json" pk act move down w2:p2
check "a plain pane is not a move neighbor" "$(jq -c .order "$HERDR_PLUGIN_STATE_DIR/state.json")" "{}"
check "a plain pane can still be previewed" "$(AGENT_SEARCH_LIST="$tmp/withplain.json" pk preview w3:p1 | grep '^title' | sed -E 's/^[^ ]+ +//')" "zsh"
fresh
: > "$FAKE_CALLS"
AGENT_SEARCH_LIST="$tmp/withplain.json" pk focus w3:p1
check "a plain pane is still focused through its workspace and tab" "$(cat "$FAKE_CALLS" | tr '\n' ';')" "workspace focus w3;tab focus w3:t1;"

fresh
check "enter on a pane accepts" "$(run enter w1:p1)" "accept"
check "enter on a group header steps down" "$(run enter -:alpha)" "down"
check "enter with no match does nothing" "$(run enter '')" ""

fresh
run focus w1:p1
check "an agent pane is focused through agent focus" "$(cat "$FAKE_CALLS")" "agent focus w1:p1"
: > "$FAKE_CALLS"
jq 'del(.result.panes[1].agent)' "$tmp/list.json" > "$tmp/plain1.json"
AGENT_SEARCH_LIST="$tmp/plain1.json" pk focus w1:p2
check "a plain pane focuses its workspace then its tab" "$(cat "$FAKE_CALLS" | tr '\n' ';')" \
  "workspace focus w1;tab focus w1:t2;"
: > "$FAKE_CALLS"
FAKE_AGENT_FOCUS_FAIL=1 pk focus w2:p1
check "an agent pane whose agent is gone falls back to workspace and tab" "$(cat "$FAKE_CALLS" | tr '\n' ';')" \
  "agent focus w2:p1;workspace focus w2;tab focus w2:t1;"
: > "$FAKE_CALLS"
check "a pane that is gone fails" "$(run focus w7:p7 2>&1; echo "rc=$?")" "agent-search: pane w7:p7 is gone
rc=1"
check "a group header focuses nothing and succeeds" "$(run focus -:alpha 2>&1; echo "rc=$?"; cat "$FAKE_CALLS")" "rc=0"

fresh
run open
check "open launches the search pane entrypoint" "$(cat "$FAKE_CALLS")" \
  "plugin pane open --plugin takashiaihara.agent-search --entrypoint search"

fresh
check "preview lists fields and tokens" "$(run preview w2:p1 | grep -E '^(title|tokens.task)' | sed -E 's/^([^ ]+) +/\1=/' | tr '\n' ';')" \
  "title=write docs;tokens.task=urgent kaneo;"
check "preview values start in one column" "$(run preview w2:p1 | awk '{match($0, /^[^ ]+ +/); print RLENGTH}' | LC_ALL=C sort -u | wc -l | tr -d ' ')" "1"
check "preview has more than one line" "$([ "$(run preview w2:p1 | wc -l)" -gt 3 ] && echo yes)" "yes"
check "preview of a group header is empty" "$(run preview -:alpha)" ""

fresh
mkdir -p "$HERDR_PLUGIN_STATE_DIR"
echo '{"prio":{"w3:p1":5,"gone":2},"order":{"w3:p1":1,"gone":3,"s1":9}}' > "$HERDR_PLUGIN_STATE_DIR/state.json"
jq '.result.panes += [{"pane_id":"w3:p1","workspace_id":"w3","tab_id":"w3:t1","title":"zsh"}]' "$tmp/list.json" > "$tmp/plain3.json"
AGENT_SEARCH_LIST="$tmp/plain3.json" pk act sort-next
check "pruning keeps the state of a pane that is in the list but not an agent, and drops one that is gone" \
  "$(jq -c '[.prio, .order]' "$HERDR_PLUGIN_STATE_DIR/state.json")" '[{"w3:p1":5},{"w3:p1":1,"s1":9}]'
AGENT_SEARCH_LIST="$tmp/plain3.json" pk act move down w1:p1
check "a move renumbers over the saved keys too, so a pane hidden from the list keeps its place" \
  "$(jq -c '.order | [.["w3:p1"], .["w1:p2"], .s1]' "$HERDR_PLUGIN_STATE_DIR/state.json")" "[0,1,2]"
echo '{"result":{"panes":[]}}' > "$tmp/nopanes.json"
AGENT_SEARCH_LIST="$tmp/nopanes.json" pk act sort-next
check "an empty pane list prunes nothing" "$(jq -c '.prio' "$HERDR_PLUGIN_STATE_DIR/state.json")" '{"w3:p1":5}'

fresh
check "a state dir that cannot be written fails before herdr is called" \
  "$(echo x > "$tmp/notadir"; HERDR_PLUGIN_STATE_DIR="$tmp/notadir/sub" pk act prio 2 w2:p2 2>&1; echo "rc=$?"; cat "$FAKE_CALLS")" \
  "agent-search: cannot create $tmp/notadir/sub
rc=1"

fresh
check "a failed herdr call leaves no state file when there was none" \
  "$(FAKE_META_FAIL=1 pk act prio 2 w2:p2 2>&1; echo "rc=$?"; [ -e "$HERDR_PLUGIN_STATE_DIR/state.json" ] || echo no-state-file)" \
  "agent-search: cannot set the priority of w2:p2
rc=1
no-state-file"
check "herdr's stderr is shown once, in the message, not written to the terminal" \
  "$(FAKE_META_STDERR=1 FAKE_META_FAIL=1 pk act prio 2 w2:p2 2>&1)" \
  "agent-search: cannot set the priority of w2:p2 (herdr: boom)"
run act prio 4 w2:p1
jq 'del(.result.panes[].tokens.prio)' "$tmp/list.json" > "$tmp/lost2.json"
check "restore failures carry herdr's stderr in the one line" \
  "$(FAKE_META_STDERR=1 FAKE_META_FAIL=1 AGENT_SEARCH_LIST="$tmp/lost2.json" pk restore 2>&1)" \
  "agent-search: cannot restore the priority of w2:p1 (herdr: boom)"

fresh
mkdir -p "$HERDR_PLUGIN_STATE_DIR/state.json"
check "a state file that exists but cannot be read aborts the act" \
  "$(pk act prio 2 w2:p2 2>&1; echo "rc=$?"; cat "$FAKE_CALLS")" \
  "agent-search: cannot read $HERDR_PLUGIN_STATE_DIR/state.json
rc=1"
check "an unreadable state does not stop the read-only commands" "$(ids)" "w1:p1 w1:p2 w2:p1 w2:p2 "

fresh
mkdir -p "$HERDR_PLUGIN_STATE_DIR"
: > "$HERDR_PLUGIN_STATE_DIR/state.json.lock" || { echo "FAIL could not prepare the lock file"; fails=$((fails + 1)); }
check "a lock file left behind by a process that is gone blocks nobody" \
  "$(pk act sort-next 2>&1; echo "rc=$?"; ls "$HERDR_PLUGIN_STATE_DIR" | tr '\n' ' ')" "rc=0
state.json state.json.lock "

fresh
# Ten runs: the sort cycles every 3 writes, so 10 serialized writes land on "manual" and a lost
# update lands on "status" or null. Each run's exit status is collected too, so a run that could
# not take the lock shows up instead of being silently counted.
rcs=$(
  for i in 1 2 3 4 5 6 7 8 9 10; do pk act sort-next 2>&1 || echo "rc=$?" & done
  wait
)
check "ten concurrent acts are serialized, none is lost" "$(jq -c '.sort' "$HERDR_PLUGIN_STATE_DIR/state.json")$rcs" '"manual"'

fresh
echo '{"sorts":[{"name":"a","by":["title"]},{"name":"bad","by":["tokens.repo.x"]}]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a sort the list cannot be built under is refused" "$(pk act sort-next 2>&1; echo "rc=$?")" "agent-search: cannot read the pane list
rc=1"
check "and it is not saved, so later commands still work" \
  "$([ -e "$HERDR_PLUGIN_STATE_DIR/state.json" ] || echo no-state-file; ids)" "no-state-file
w1:p1 w2:p2 w1:p2 w2:p1 "

fresh
echo '{"keys":{"up":"ctrl-nonsense"}}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "an unknown key name is a config error naming the key" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" \
  "agent-search: $HERDR_PLUGIN_CONFIG_DIR/config.json: unknown key name \"ctrl-nonsense\" in keys.up
rc=1"
echo '{"keys":{"sort":"x"}}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a key that would make a character untypable in the search is a config error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" \
  "agent-search: $HERDR_PLUGIN_CONFIG_DIR/config.json: keys.sort \"x\" is already used by the popup (alt-1..alt-9, esc, enter, backspace, up, down, ctrl-p, ctrl-n, ctrl-c, or a character typed into the search)
rc=1"
echo '{"keys":{"sort":"alt-A","group":"alt-shift-a"}}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "two spellings of one key are a config error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" \
  "agent-search: $HERDR_PLUGIN_CONFIG_DIR/config.json: keys.group \"alt-shift-a\" is the same key as keys.sort
rc=1"
echo '{"keys":{"sort":"shift-1"}}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "shift with a non-letter is a config error naming the key" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" \
  "agent-search: $HERDR_PLUGIN_CONFIG_DIR/config.json: \"shift-1\": shift only combines with a letter or a named key; the terminal reports the shifted character itself, so name that character in keys.sort
rc=1"
echo '{"keys":{"sort":"ctrl-m"}}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a ctrl name that is another key's byte is a config error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" \
  "agent-search: $HERDR_PLUGIN_CONFIG_DIR/config.json: keys.sort \"ctrl-m\" is the same key as enter to a terminal
rc=1"
echo '{"keys":{"sort":"shift-tab"}}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "shift-tab is accepted" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" "rc=0"
echo '{"columns":["title."]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "a field path with an empty part is a config error, not a pane list error" "$(run lines 2>&1 >/dev/null; echo "rc=$?")" \
  "agent-search: $HERDR_PLUGIN_CONFIG_DIR/config.json: invalid field \"title.\": its path has an empty part
rc=1"
echo '{"search":["tokens..repo"]}' > "$HERDR_PLUGIN_CONFIG_DIR/config.json"
check "an empty part in a search field is a config error too" "$(run lines 2>&1 >/dev/null | grep -c 'invalid field "tokens..repo"')" "1"

fresh
jq '.result.panes[0].agent = 5' "$tmp/list.json" > "$tmp/agent5.json"
check "an agent that is not a string makes no agent pane" "$(AGENT_SEARCH_LIST="$tmp/agent5.json" pk lines | cut -f1 | tr '\n' ' ')" "w1:p2 w2:p1 w2:p2 "
AGENT_SEARCH_LIST="$tmp/agent5.json" pk focus w1:p1
check "and focus treats it as a plain pane" "$(cat "$FAKE_CALLS" | tr '\n' ';')" "workspace focus w1;tab focus w1:t1;"

fresh
sa="state-$(tag /run/herdr/a.sock).json"
sb="state-$(tag /run/herdr/b.sock).json"
HERDR_SOCKET_PATH=/run/herdr/a.sock pk act prio 3 w1:p2
jq 'del(.result.panes[] | select(.pane_id == "w1:p2"))' "$tmp/list.json" > "$tmp/blist.json"
AGENT_SEARCH_LIST="$tmp/blist.json" HERDR_SOCKET_PATH=/run/herdr/b.sock pk act prio 5 w2:p2
check "each session's state file is named after its own socket" \
  "$(ls "$HERDR_PLUGIN_STATE_DIR" | sort | tr '\n' ' ')" \
  "$(printf '%s\n' "$sa" "$sa.lock" "$sb" "$sb.lock" | sort | tr '\n' ' ')"
check "a prune in one session keeps the other's priorities" \
  "$(jq -c .prio "$HERDR_PLUGIN_STATE_DIR/$sa"; jq -c .prio "$HERDR_PLUGIN_STATE_DIR/$sb")" \
  '{"w1:p2":3}
{"w2:p2":5}'

fresh
check "without HERDR_SOCKET_PATH the state file is still state.json" \
  "$(pk act sort-next; ls "$HERDR_PLUGIN_STATE_DIR" | sort | tr '\n' ' ')" \
  "state.json state.json.lock "

fresh
mkdir -p "$HERDR_PLUGIN_STATE_DIR"
echo '{"prio":{"s1":4,"w1:p2":3,"w2:p2":2}}' > "$HERDR_PLUGIN_STATE_DIR/state.json"
jq 'del(.result.panes[].tokens.prio)' "$tmp/list.json" > "$tmp/lost3.json"
: > "$FAKE_CALLS"
check "restore skips a saved priority that changed or went away after it read the state" \
  "$(FAKE_WRITE_STATE_PANE=w1:p1 FAKE_WRITE_STATE_JSON='{"prio":{"s1":4,"w1:p2":9}}' AGENT_SEARCH_LIST="$tmp/lost3.json" pk restore 2>&1; echo "rc=$?"; cat "$FAKE_CALLS")" \
  "rc=0
pane report-metadata w1:p1 --source takashiaihara.agent-search --token prio=4"

if [ "$fails" -gt 0 ]; then echo "$fails failed"; exit 1; fi
echo "all passed"
