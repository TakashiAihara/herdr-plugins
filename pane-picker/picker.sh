#!/usr/bin/env bash
# Pane picker for Herdr. Subcommands:
#   open                 action entry: open the popup pane
#   ui                   popup entry: run fzf over the pane list
#   lines                print the fzf input (pane_id<TAB>display<TAB>search text)
#   header               print the fzf header (current sort / group, keys)
#   act <op> [args]      change state: sort-next | group-next | move up|down <id> | prio <n> <id> | clear <id>
#   focus <pane_id>      jump to a pane
#   preview <pane_id>    print every field of a pane
# PANE_PICKER_LIST (a file with `herdr pane list` output) replaces the live list; the tests use it.
set -uo pipefail

HERDR=${HERDR_BIN_PATH:-herdr}
SELF=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/picker.sh
CONFIG_DIR=${HERDR_PLUGIN_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/herdr-pane-picker}
STATE_DIR=${HERDR_PLUGIN_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/herdr-pane-picker}
CONFIG=$CONFIG_DIR/config.json
STATE=$STATE_DIR/state.json

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

# User values replace defaults key by key; arrays are replaced, not merged.
config() {
  if [ -f "$CONFIG" ]; then
    jq -e 'type == "object"' "$CONFIG" >/dev/null 2>&1 || die "$CONFIG is not a JSON object"
    jq -n --argjson d "$DEFAULTS" --slurpfile u "$CONFIG" '$d * $u[0]'
  else
    printf '%s' "$DEFAULTS"
  fi
}

state() {
  if [ -f "$STATE" ] && jq -e 'type == "object"' "$STATE" >/dev/null 2>&1; then
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
# Row: {id, key, group, cols, search}. A pane's key is its agent session id when it has one, so
# manual order and priority survive a pane id change; otherwise the pane id.
JQ_ROWS='
def field($p; $pane):
  ($p | split(":")) as $parts
  | ($parts[0] | split(".")) as $path
  | (if $path[-1] == "*" then ($pane | getpath($path[:-1]) // {} | [.[]?] | map(tostring) | join(" "))
     else ($pane | getpath($path)) end) as $v
  | if $parts[1] == "basename" and $v != null then ($v | tostring | split("/") | map(select(. != "")) | last)
    else $v end;

def blank: . == null or . == "";

($cfg[0]) as $c | ($st[0]) as $s
| (.result.panes // []) | to_entries
| map(.key as $i | .value as $p
    | ($p.agent_session.value // $p.pane_id) as $k
    | (($s.prio // {})[$k]
        // (if $c.priority.source != "manual" then ([field($c.priority.source; $p) | tonumber?] | first) else null end)) as $prio
    | {pane: $p, id: $p.pane_id, key: $k, prio: $prio,
       manual: (($s.order // {})[$k] // (1000000 + $i))})
| ($c.sorts | map(.name) | index($s.sort // "") // 0) as $si
| ($c.groups | map(.name) | index($s.group // "") // 0) as $gi
| $c.sorts[$si].by as $by
| $c.groups[$gi].by as $gby
| sort_by(. as $r | $by | map(
    (split(":")) as [$f, $dir]
    | (if $f == "prio" then $r.prio elif $f == "manual" then $r.manual else field($f; $r.pane) end) as $v
    | [($v | blank),
       (if $dir == "desc" then (if ($v | type) == "number" then -$v else ($v // "" | tostring | explode | map(-.)) end)
        else (if ($v | type) == "number" then $v else ($v // "" | tostring) end) end)]))
| map(. as $r
    | .group = ([$gby[] | field(.; $r.pane) | select(blank | not) | tostring] | first // (if ($gby | length) > 0 then "(none)" else null end))
    | .cols = [$c.columns[] | if . == "prio" then (if $r.prio == null then "  " else "★\($r.prio)" end)
                              else (field(.; $r.pane) // "" | tostring) end]
    | .search = ([$c.search[] | field(.; $r.pane) | select(blank | not) | tostring] | join(" ")))
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

cmd_lines() {
  rows | jq -rs '
    (map(.cols) | transpose | map(map(length) | max)) as $w
    | . as $all
    | foreach .[] as $r ({prev: null};
        .out = (if $r.group != null and $r.group != .prev
                then ["-\t\u001b[1m── \($r.group) (\($all | map(select(.group == $r.group)) | length)) ──\u001b[0m\t"]
                else [] end)
               + ["\($r.id)\t\([$r.cols, $w] | transpose | to_entries | map(if .key == ($w | length) - 1 then .value[0] else .value[0] + (" " * ((.value[1] - (.value[0] | length)) | if . < 0 then 0 else . end)) end) | join("  "))\t\u001b[2m\($r.search)\u001b[0m"]
        | .prev = $r.group;
        .out[])'
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

cmd_act() {
  local op=${1:-} cfg st
  cfg=$(config) || exit 1
  st=$(state)
  local new
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
      new=$(rows | jq -n --argjson s "$st" --arg dir "$dir" --arg id "$id" '
        [inputs] as $r | ($r | map(.key)) as $keys
        | ($r | map(.id) | index($id)) as $i
        | (if $i == null then null elif $dir == "up" then $i - 1 else $i + 1 end) as $j
        | if $j == null or $j < 0 or $j >= ($keys | length) then $s
          else ($keys | .[$i] = $keys[$j] | .[$j] = $keys[$i]) as $new
               | $s + {order: ($new | to_entries | map({key: .value, value: .key}) | from_entries)}
          end') || die "cannot update the state"
      ;;
    prio|clear)
      local n id
      if [ "$op" = prio ]; then n=${2:-}; id=${3:-}; else n=; id=${2:-}; fi
      [ "$op" = clear ] || [[ "$n" =~ ^[1-9]$ ]] || die "prio needs 1-9"
      [ -n "$id" ] || die "$op needs a pane id"
      # Keys of closed panes are dropped on every write so the state does not grow forever.
      new=$(rows | jq -n --argjson s "$st" --arg id "$id" --arg n "$n" '
        [inputs] as $r | ($r | map(.key)) as $live
        | ($r | map(select(.id == $id)) | first | .key) as $k
        | if $k == null then $s
          else $s | .prio = ((.prio // {}) | with_entries(select(.key as $x | $live | index([$x])))
                              | if $n == "" then del(.[$k]) else . + {($k): ($n | tonumber)} end)
          end') || die "cannot update the state"
      ;;
    *) die "unknown act: $op" ;;
  esac
  write_state "$new"
}

# Herdr has no focus-by-pane-id for plain terminals: agent panes go through `agent focus`
# (which also marks a done agent seen); others focus their workspace and tab.
# shortcut: a plain terminal sharing a tab with other panes lands on the tab, not the pane;
# walk pane.layout with pane.focus_direction if that case shows up.
cmd_focus() {
  local id=${1:-} pane
  [ -n "$id" ] && [ "$id" != - ] || exit 0
  pane=$(pane_list | jq -c --arg id "$id" '.result.panes[]? | select(.pane_id == $id)') || die "cannot read the pane list"
  [ -n "$pane" ] || die "pane $id is gone"
  if [ "$(jq -r '.agent // empty' <<<"$pane")" != "" ]; then
    "$HERDR" agent focus "$id" >/dev/null
  else
    "$HERDR" workspace focus "$(jq -r .workspace_id <<<"$pane")" >/dev/null &&
      "$HERDR" tab focus "$(jq -r .tab_id <<<"$pane")" >/dev/null
  fi
}

cmd_preview() {
  local id=${1:-}
  [ -n "$id" ] && [ "$id" != - ] || exit 0
  pane_list | jq -r --arg id "$id" '
    .result.panes[]? | select(.pane_id == $id)
    | (del(.tokens, .scroll, .revision) | to_entries) + (.tokens // {} | to_entries | map(.key |= "tokens.\(.)"))
    | .[] | select(.value != null) | "\(.key)\t\(.value | if type == "object" then tojson else tostring end)"'
}

cmd_ui() {
  command -v fzf >/dev/null || { echo "pane-picker needs fzf (0.71+). Press Enter."; read -r _; exit 1; }
  command -v jq >/dev/null || { echo "pane-picker needs jq. Press Enter."; read -r _; exit 1; }
  local cfg keys
  cfg=$(config) || { echo "Press Enter."; read -r _; exit 1; }
  keys=$(jq -r '.keys | [.sort, .group, .up, .down, .clear] | @tsv' <<<"$cfg")
  local k_sort k_group k_up k_down k_clear
  IFS=$'\t' read -r k_sort k_group k_up k_down k_clear <<<"$keys"
  local reload="reload-sync(bash '$SELF' lines)+transform-header(bash '$SELF' header)"
  local binds=(
    --bind "$k_sort:execute-silent(bash '$SELF' act sort-next)+$reload"
    --bind "$k_group:execute-silent(bash '$SELF' act group-next)+$reload"
    --bind "$k_up:execute-silent(bash '$SELF' act move up {1})+$reload"
    --bind "$k_down:execute-silent(bash '$SELF' act move down {1})+$reload"
    --bind "$k_clear:execute-silent(bash '$SELF' act clear {1})+$reload"
    --bind "enter:transform:[ {1} = - ] && echo down || echo accept"
  )
  local n
  for n in 1 2 3 4 5 6 7 8 9; do
    binds+=(--bind "alt-$n:execute-silent(bash '$SELF' act prio $n {1})+$reload")
  done
  local picked
  picked=$(cmd_lines | fzf --ansi --delimiter='\t' --with-nth=2.. --accept-nth=1 \
    --id-nth=1 --track --no-sort --layout=reverse --header-first \
    --header "$(cmd_header)" \
    --preview "bash '$SELF' preview {1}" --preview-window=right,40%,wrap \
    "${binds[@]}") || exit 0
  cmd_focus "$picked"
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
  act) cmd_act "$@" ;;
  focus) cmd_focus "$@" ;;
  preview) cmd_preview "$@" ;;
  *) die "usage: picker.sh open|ui|lines|header|act|focus|preview" ;;
esac
