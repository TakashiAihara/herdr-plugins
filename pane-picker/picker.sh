#!/usr/bin/env bash
# Pane picker for Herdr. Subcommands:
#   open                 action entry: open the popup pane
#   ui                   popup entry: run fzf over the pane list
#   lines                print the fzf input (id<TAB>display<TAB>search text); group headers have ids "-:<index>"
#   header               print the fzf header (current sort / group, keys)
#   enter <id>           print the fzf action for Enter on a row
#   act <op> [args]      change state: sort-next | group-next | move up|down <id> | prio <n> <id> | clear <id>
#   focus <pane_id>      jump to a pane
#   preview <pane_id>    print every field of a pane
# PANE_PICKER_LIST (a file with `herdr pane list` output) replaces the live list. The popup sets it to
# one snapshot so preview and reload do not call Herdr on every keystroke; the tests use it too.
set -uo pipefail

HERDR=${HERDR_BIN_PATH:-herdr}
SELF=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/picker.sh
CONFIG_DIR=${HERDR_PLUGIN_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/herdr-pane-picker}
STATE_DIR=${HERDR_PLUGIN_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/herdr-pane-picker}
CONFIG=$CONFIG_DIR/config.json
STATE=$STATE_DIR/state.json
FZF_MIN=0.71

DEFAULTS='{
  "columns": ["prio", "agent_status", "title"],
  "search": ["title", "cwd", "agent", "agent_status", "terminal_title_stripped", "tokens.*"],
  "sorts": [
    {"name": "priority", "by": ["prio:desc", "manual"]},
    {"name": "manual", "by": ["manual"]},
    {"name": "status", "by": ["agent_status", "manual"]}
  ],
  "groups": [
    {"name": "none", "by": []},
    {"name": "repo", "by": ["tokens.repo", "cwd:basename"]},
    {"name": "workspace", "by": ["workspace_id"]}
  ],
  "priority": {"source": "manual"},
  "keys": {"sort": "ctrl-s", "group": "ctrl-g", "up": "alt-up", "down": "alt-down", "clear": "alt-0"}
}'

die() { printf 'pane-picker: %s\n' "$*" >&2; exit 1; }

# jq `*` merges objects (keys, priority) key by key and replaces arrays (columns, sorts, ...) whole.
config() {
  local merged
  if [ -f "$CONFIG" ]; then
    jq -e 'type == "object"' "$CONFIG" >/dev/null 2>&1 || die "$CONFIG is not a JSON object"
    merged=$(jq -n --argjson d "$DEFAULTS" --slurpfile u "$CONFIG" '$d * $u[0]') || die "cannot read $CONFIG"
  else
    merged=$DEFAULTS
  fi
  jq -e '
    def strs: type == "array" and all(.[]; type == "string" and . != "");
    def named: type == "array" and length > 0
      and all(.[]; type == "object" and (.name | type) == "string" and .name != "" and (.by | strs))
      and (map(.name) | length == (unique | length));
    (.columns | strs and length > 0) and (.search | strs and length > 0) and (.sorts | named) and (.groups | named)
    and (.priority.source | type) == "string"
    and ([.keys.sort, .keys.group, .keys.up, .keys.down, .keys.clear] | strs)' <<<"$merged" >/dev/null 2>&1 ||
    die "$CONFIG: columns and search must be non-empty lists of fields, sorts and groups non-empty lists of {name, by} with unique names, priority.source a string, and keys sort/group/up/down/clear non-empty strings"
  printf '%s' "$merged"
}

state() {
  if [ -f "$STATE" ] && jq -e 'type == "object" and ((.prio // {}) | type) == "object" and ((.order // {}) | type) == "object"' "$STATE" >/dev/null 2>&1; then
    cat "$STATE"
  else
    echo '{}'
  fi
}

# Takes the new state as an argument (not stdin), so a failed jq upstream stops before the file is touched.
write_state() {
  mkdir -p "$STATE_DIR" || die "cannot create $STATE_DIR"
  local tmp
  tmp=$(mktemp "$STATE_DIR/.state.XXXXXX") || die "cannot write $STATE_DIR"
  printf '%s\n' "$1" > "$tmp" && mv -f "$tmp" "$STATE" || { rm -f "$tmp"; die "cannot write $STATE"; }
}

pane_list() {
  if [ -n "${PANE_PICKER_LIST:-}" ]; then
    cat "$PANE_PICKER_LIST"
  else
    "$HERDR" pane list
  fi
}

# Shared jq: resolve config, state and panes into rows in display order.
# Row: {id, key, group, manual, cols, search}. A pane's key is its agent session id when it has one, so
# manual order and priority survive a pane id change; otherwise the pane id.
JQ_ROWS='
def field($p; $pane):
  ($p | split(":")) as $parts
  | ($parts[0] | split(".")) as $path
  | (if $path[-1] == "*" then ($pane | getpath($path[:-1]) // {} | [.[]?] | map(tostring) | join(" "))
     else ($pane | getpath($path)) end) as $v
  | if $parts[1] == "basename" and $v != null then (($v | tostring | split("/") | map(select(. != "")) | last) // $v)
    else $v end;

def blank: . == null or . == "";

# Tabs, newlines and escapes in a value would split or fake fzf rows, so they become spaces.
def clean: tostring | gsub("[\u0000-\u001f\u007f]"; " ");

# Only a trailing ":desc" is a direction; anything else (":basename") stays part of the field.
def spec: if endswith(":desc") then {f: .[:-5], desc: true} else {f: ., desc: false} end;

($cfg[0]) as $c | ($st[0]) as $s
| if (.result.panes | type) != "array" then error("herdr pane list has no .result.panes list") else . end
| def sid: (.agent_session | objects | .value | strings) // "";
# Panes that share a session id would share one position and one priority, so they use the pane id.
([.result.panes[] | sid | select(. != "")] | group_by(.) | map(select(length > 1) | .[0])) as $dup
| .result.panes | to_entries
| map(.key as $i | .value as $p
    | ($p | sid) as $sid
    | (if $sid != "" and ($dup | index([$sid]) | not) then $sid else $p.pane_id end) as $k
    | (($s.prio // {})[$k]
        // (if $c.priority.source != "manual" then ([field($c.priority.source; $p) | tonumber?] | first) else null end)) as $prio
    | {pane: $p, id: $p.pane_id, key: $k, prio: $prio, idx: $i,
       manual: (($s.order // {})[$k] // (1000000 + $i))})
| ($c.sorts | map(.name) | index($s.sort // "") // 0) as $si
| ($c.groups | map(.name) | index($s.group // "") // 0) as $gi
| $c.sorts[$si].by as $by
| $c.groups[$gi].by as $gby
| sort_by(. as $r | $by | map(
    spec as $sp
    | (if $sp.f == "prio" then $r.prio elif $sp.f == "manual" then $r.manual else field($sp.f; $r.pane) end) as $v
    | [($v | blank),
       (if $sp.desc then (if ($v | type) == "number" then -$v else ($v // "" | tostring | explode | map(-.) + [0]) end)
        else (if ($v | type) == "number" then $v else ($v // "" | tostring) end) end)]))
| map(. as $r
    | .group = ([$gby[] | field(.; $r.pane) | select(blank | not) | clean] | first // (if ($gby | length) > 0 then "(none)" else null end))
    | .cols = [$c.columns[] | if . == "prio" then (if $r.prio == null then "  " else "★\($r.prio)" end)
                              else (field(.; $r.pane) // "" | clean) end]
    | .search = ([$c.search[] | field(.; $r.pane) | select(blank | not) | clean] | join(" "))
    | del(.pane))
| if ($gby | length) == 0 then .
  else (map(.group) | reduce .[] as $g ([]; if index([$g]) then . else . + [$g] end)) as $order
       | group_by(.group) | sort_by(.[0].group as $g | $order | index($g)) | add // []
  end
| .[]
'

rows() {
  local cfg st
  cfg=$(config) || exit 1
  st=$(state)
  pane_list | jq -c --argjson cfg "[$cfg]" --argjson st "[$st]" "$JQ_ROWS" || die "cannot read the pane list"
}

# Terminal cell width: East Asian wide characters and most emoji take two cells, combining marks
# and variation selectors none. shortcut: ambiguous-width characters (★) count as one cell.
JQ_WIDTH='
def width: explode | map(
  if (. >= 4352 and . <= 4447) or (. >= 11904 and . <= 42191) or (. >= 44032 and . <= 55203)
     or (. >= 63744 and . <= 64255) or (. >= 65040 and . <= 65049) or (. >= 65072 and . <= 65135)
     or (. >= 65280 and . <= 65376) or (. >= 65504 and . <= 65510) or (. >= 127744 and . <= 129791)
     or (. >= 131072 and . <= 262141) then 2
  elif (. >= 768 and . <= 879) or (. >= 65024 and . <= 65039) or . == 8203 or . == 8205 then 0
  else 1 end) | add // 0;
'

cmd_lines() {
  local out
  out=$(rows) || exit 1
  jq -rs "$JQ_WIDTH"'
    (map(.cols) | transpose | map(map(width) | max)) as $w
    | . as $all
    | (map(.group) | reduce .[] as $g ([]; if index([$g]) then . else . + [$g] end)) as $gs
    | foreach .[] as $r ({prev: null};
        .out = (if $r.group != null and $r.group != .prev
                then ["-:\($gs | index([$r.group]))\t\u001b[1m── \($r.group) (\($all | map(select(.group == $r.group)) | length)) ──\u001b[0m\t"]
                else [] end)
               + ["\($r.id)\t\([$r.cols, $w] | transpose | to_entries | map(if .key == ($w | length) - 1 then .value[0] else .value[0] + (" " * ((.value[1] - (.value[0] | width)) | if . < 0 then 0 else . end)) end) | join("  "))\t\u001b[2m\($r.search)\u001b[0m"]
        | .prev = $r.group;
        .out[])' <<<"$out"
}

cmd_header() {
  local cfg st
  cfg=$(config) || exit 1
  st=$(state)
  jq -rn --argjson c "$cfg" --argjson s "$st" '
    ($c.sorts | map(.name)) as $sn | ($c.groups | map(.name)) as $gn
    | "sort: \($sn[$sn | index($s.sort // "") // 0])  group: \($gn[$gn | index($s.group // "") // 0])\n"
      + "\($c.keys.sort) sort  \($c.keys.group) group  \($c.keys.up)/\($c.keys.down) move  alt-1..9 prio  \($c.keys.clear) clear"'
}

# Enter on a group header steps to the next row; with no match there is nothing to do.
cmd_enter() {
  case "${1:-}" in
    "") ;;
    -:*) echo down ;;
    *) echo accept ;;
  esac
}

cmd_act() {
  local op=${1:-} cfg st r new
  cfg=$(config) || exit 1
  st=$(state)
  case "$op" in
    sort-next|group-next)
      local list=sorts k=sort
      [ "$op" = group-next ] && { list=groups; k=group; }
      new=$(jq -n --argjson c "$cfg" --argjson s "$st" --arg l "$list" --arg k "$k" '
        ($c[$l] | map(.name)) as $n
        | $s + {($k): $n[(($n | index($s[$k] // "")) // 0) + 1]}') || die "cannot update the state"
      ;;
    move)
      local dir=${2:-} id=${3:-}
      [ "$dir" = up ] || [ "$dir" = down ] || die "move needs up or down"
      [ -n "$id" ] || die "move needs a pane id"
      # The manual order is renumbered 0..n-1 (keeping its sequence, ties broken by list order) and
      # then only the two panes trade numbers, so the rest of it is kept whatever sort is on screen and
      # a saved position can never tie with a default one. The neighbor must be in the same group:
      # groups are rebuilt on every reload, so a swap across a header would snap back.
      r=$(rows) || exit 1
      new=$(jq -n --argjson s "$st" --arg dir "$dir" --arg id "$id" '
        [inputs] as $r
        | ($r | map(.id) | index($id)) as $i
        | (if $i == null then null elif $dir == "up" then $i - 1 else $i + 1 end) as $j
        | if $j == null or $j < 0 or $j >= ($r | length) or $r[$j].group != $r[$i].group then $s
          else ($r | sort_by([.manual, .idx]) | map(.key) | to_entries | map({key: .value, value: .key}) | from_entries) as $n
               | $s | .order = ($n + {($r[$i].key): $n[$r[$j].key], ($r[$j].key): $n[$r[$i].key]})
          end' <<<"$r") || die "cannot update the state"
      ;;
    prio|clear)
      local n id
      if [ "$op" = prio ]; then n=${2:-}; id=${3:-}; else n=; id=${2:-}; fi
      [ "$op" = clear ] || [[ "$n" =~ ^[1-9]$ ]] || die "prio needs 1-9"
      [ -n "$id" ] || die "$op needs a pane id"
      r=$(rows) || exit 1
      new=$(jq -n --argjson s "$st" --arg id "$id" --arg n "$n" '
        ([inputs] | map(select(.id == $id)) | first | .key) as $k
        | if $k == null then $s
          elif $n == "" then $s | .prio = ((.prio // {}) | del(.[$k]))
          else $s | .prio = ((.prio // {}) + {($k): ($n | tonumber)}) end' <<<"$r") || die "cannot update the state"
      ;;
    *) die "unknown act: $op" ;;
  esac
  # Every write drops priorities and positions of panes that are gone, so the state does not grow.
  r=$(rows) || exit 1
  new=$(jq -n --argjson s "$new" '
    [inputs | .key] as $live
    | def live: with_entries(select(.key as $x | $live | index([$x])));
    $s | .prio = ((.prio // {}) | live) | .order = ((.order // {}) | live)' <<<"$r") || die "cannot update the state"
  write_state "$new"
}

# Herdr has no focus-by-pane-id for plain terminals: agent panes go through `agent focus`
# (which also marks a done agent seen, as looking at it in Herdr does); others, and an agent that
# exited since the list was taken, focus their workspace and tab.
# shortcut: a plain terminal sharing a tab with other panes lands on the tab, not the pane;
# walk pane.layout with pane.focus_direction if that case shows up.
cmd_focus() {
  local id=${1:-} pane
  case "$id" in ""|-:*) exit 0 ;; esac
  pane=$(pane_list | jq -c --arg id "$id" 'first(.result.panes[]? | select(.pane_id == $id))') || die "cannot read the pane list"
  [ -n "$pane" ] || die "pane $id is gone"
  if [ "$(jq -r '.agent // empty' <<<"$pane")" != "" ] && "$HERDR" agent focus "$id" >/dev/null 2>&1; then
    return 0
  fi
  "$HERDR" workspace focus "$(jq -r .workspace_id <<<"$pane")" >/dev/null &&
    "$HERDR" tab focus "$(jq -r .tab_id <<<"$pane")" >/dev/null || die "cannot focus $id"
}

cmd_preview() {
  local id=${1:-}
  pane_list | jq -r --arg id "$id" '
    .result.panes[]? | select(.pane_id == $id)
    | (del(.tokens, .scroll, .revision) | to_entries) + (.tokens // {} | to_entries | map(.key |= "tokens.\(.)"))
    | .[] | select(.value != null) | [.key, .value] | map(tostring | gsub("[\u0000-\u001f\u007f]"; " ")) | join("\t")'
}

# True when version $1 (x.y[.z]) is at least $2 (x.y).
version_ge() {
  awk -v a="$1" -v b="$2" 'BEGIN { split(a, x, "."); split(b, y, "."); exit !(x[1] > y[1] || (x[1] == y[1] && x[2] + 0 >= y[2] + 0)) }'
}

# The popup closes as soon as this exits, so a failure is shown and waits for a key before exiting non-zero.
fail_in_popup() {
  printf 'pane-picker: %s\nPress Enter.\n' "$*"
  read -r _
  exit 1
}

cmd_ui() {
  command -v jq >/dev/null || fail_in_popup "needs jq"
  command -v fzf >/dev/null || fail_in_popup "needs fzf $FZF_MIN+"
  local v
  v=$(fzf --version | awk '{print $1}')
  version_ge "$v" "$FZF_MIN" || fail_in_popup "needs fzf $FZF_MIN+ (found $v)"

  local cfg
  cfg=$(config 2>&1) || fail_in_popup "${cfg#pane-picker: }"

  if [ -z "${PANE_PICKER_LIST:-}" ]; then
    # Global, not local: the EXIT trap runs after cmd_ui has returned.
    SNAP=$(mktemp) || fail_in_popup "cannot create a temporary file"
    trap 'rm -f "$SNAP"' EXIT
    "$HERDR" pane list > "$SNAP" || fail_in_popup "herdr pane list failed"
    export PANE_PICKER_LIST=$SNAP
  fi

  local lines
  lines=$(cmd_lines 2>&1) || { lines=${lines##*$'\n'}; fail_in_popup "${lines#pane-picker: }"; }

  local k_sort k_group k_up k_down k_clear
  k_sort=$(jq -r .keys.sort <<<"$cfg"); k_group=$(jq -r .keys.group <<<"$cfg")
  k_up=$(jq -r .keys.up <<<"$cfg"); k_down=$(jq -r .keys.down <<<"$cfg"); k_clear=$(jq -r .keys.clear <<<"$cfg")
  local reload="reload-sync(bash '$SELF' lines)+transform-header(bash '$SELF' header)"
  local binds=(
    --bind "$k_sort:execute-silent(bash '$SELF' act sort-next)+$reload"
    --bind "$k_group:execute-silent(bash '$SELF' act group-next)+$reload"
    --bind "$k_up:execute-silent(bash '$SELF' act move up {1})+$reload"
    --bind "$k_down:execute-silent(bash '$SELF' act move down {1})+$reload"
    --bind "$k_clear:execute-silent(bash '$SELF' act clear {1})+$reload"
    --bind "enter:transform(bash '$SELF' enter {1})"
  )
  local n
  for n in 1 2 3 4 5 6 7 8 9; do
    binds+=(--bind "alt-$n:execute-silent(bash '$SELF' act prio $n {1})+$reload")
  done

  # --nth=2 searches the hidden search text (field 2 after --with-nth), so a query never lands on
  # a group header, whose search text is empty.
  local picked rc
  picked=$(if [ -n "$lines" ]; then printf '%s\n' "$lines"; fi | fzf --ansi --delimiter='\t' --with-nth=2.. --nth=2 --accept-nth=1 \
    --id-nth=1 --track --no-sort --layout=reverse --header-first \
    --header "$(cmd_header)" \
    --preview "bash '$SELF' preview {1}" --preview-window=right,40%,wrap \
    "${binds[@]}")
  rc=$?
  local msg
  case $rc in
    # Inside $( ), die in cmd_focus ends only the subshell, so the message reaches the popup.
    0) msg=$(cmd_focus "$picked" 2>&1) || { msg=${msg##*$'\n'}; fail_in_popup "${msg#pane-picker: }"; } ;;
    1|130) exit 0 ;;
    *) fail_in_popup "fzf exited with $rc" ;;
  esac
}

cmd_open() {
  "$HERDR" plugin pane open --plugin "${HERDR_PLUGIN_ID:-takashiaihara.pane-picker}" --entrypoint picker >/dev/null
}

sub=${1:-}
shift || true
case "$sub" in
  open) cmd_open ;;
  ui) cmd_ui ;;
  lines) cmd_lines ;;
  header) cmd_header ;;
  enter) cmd_enter "$@" ;;
  act) cmd_act "$@" ;;
  focus) cmd_focus "$@" ;;
  preview) cmd_preview "$@" ;;
  *) die "usage: picker.sh open|ui|lines|header|enter|act|focus|preview" ;;
esac
