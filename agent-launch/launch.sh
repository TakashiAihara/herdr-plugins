#!/usr/bin/env bash
# open: remember the focused pane's directory and show the prompt popup.
# ui:   (inside the popup) read the task, create a workspace there, run the command in it.
set -u

herdr="${HERDR_BIN_PATH:-herdr}"
# In the popup a message printed just before exit would vanish with it, so wait for Enter there.
die() {
  echo "agent-launch: $*" >&2
  [[ "${1:-}" != usage* && "${MODE:-}" == ui ]] && read -r -p "press Enter to close" _
  exit 1
}

# Single quotes are the one quoting both sh and zsh read the same way, whatever the text holds.
quote() { local s=${1//\'/\'\\\'\'}; printf "'%s'" "$s"; }

# The command template: {prompt} and {repo} are replaced by what you type, each as one quoted word.
template() {
  local f="${HERDR_PLUGIN_CONFIG_DIR:-}/command" t="" def='claude {prompt}'
  [[ -f "$f" ]] && IFS= read -r t < "$f"
  printf '%s' "${t:-$def}"
}

# One left-to-right pass over the template: text that came from a value is never scanned again,
# and no ${var//pat/repl} is used, whose replacement Bash 5.2 rewrites ('&' is the match).
render() {
  local t=$1 out="" a b
  while :; do
    a=${t%%\{prompt\}*}; b=${t%%\{repo\}*}
    if (( ${#a} < ${#t} && ${#a} <= ${#b} )); then
      out+="$a$(quote "$prompt")"; t=${t:${#a}+8}
    elif (( ${#b} < ${#t} )); then
      out+=$b; [[ -n "$repo" ]] && out+=$(quote "$repo"); t=${t:${#b}+6}
    else
      printf '%s' "$out$t"; return
    fi
  done
}

MODE="${1:-}"
case "$MODE" in
  open)
    # The pane you were in decides where the new workspace starts, so a repo-less command
    # (claude, or ccxn without a repo) runs in that repo. Fields of one pane are flat until
    # agent_session/scroll/tokens, and cwd is immediately followed by focused (herdr 0.9.5).
    list="$("$herdr" pane list)" || die "herdr pane list failed"
    # A cwd with a JSON escape (\" or \\) does not match and stops here rather than being misread.
    re='"cwd":"([^"\\]*)","focused":true'
    [[ "$list" =~ $re ]] || die "no focused pane in herdr pane list, or its cwd has \" or \\"
    # Herdr 0.9.5 marks one pane in all workspaces; if that ever changes, the first could be wrong.
    [[ "${list#*\"focused\":true}" != *'"focused":true'* ]] || die "more than one focused pane"
    exec "$herdr" plugin pane open --plugin "${HERDR_PLUGIN_ID:-takashiaihara.agent-launch}" \
      --entrypoint prompt --env "AGENT_LAUNCH_CWD=${BASH_REMATCH[1]}" >/dev/null
    ;;
  ui)
    # open always sets it; without it the agent would start in some other directory.
    cwd="${AGENT_LAUNCH_CWD:-}"
    [[ -n "$cwd" ]] || die "AGENT_LAUNCH_CWD is not set (open the popup with the plugin action)"
    t="$(template)"
    echo "in: $cwd"
    echo "run: $t"
    repo=""
    if [[ "$t" == *"{repo}"* ]]; then
      read -r -e -p "repo (empty = this directory)> " repo || exit 0
    fi
    read -r -e -p "prompt> " prompt || exit 0
    # read already drops surrounding blanks, so a blank line is empty here.
    [[ -n "$prompt" ]] || exit 0
    cmd="$(render "$t")"

    [[ -n "${cmd//[[:space:]]/}" ]] || die "the command template renders to nothing: $t"

    # Focus moves last: this popup may close once it loses focus, and the command must be sent by then.
    out="$("$herdr" workspace create --cwd "$cwd" --label "${prompt:0:24}" --no-focus)"
    # The reply has one pane (pane_id) and one workspace id; a quote inside a value is escaped,
    # so these keys cannot be matched inside the label or cwd, whatever braces those hold.
    re='"workspace_id":"([^"]+)"'
    [[ "$out" =~ $re ]] || die "herdr workspace create returned no workspace: $out"
    ws="${BASH_REMATCH[1]}"
    re='"pane_id":"([^"]+)"'
    [[ "$out" =~ $re ]] || { "$herdr" workspace close "$ws" >/dev/null; die "herdr workspace create returned no pane: $out"; }
    "$herdr" pane run "${BASH_REMATCH[1]}" "$cmd" >/dev/null \
      || { "$herdr" workspace close "$ws" >/dev/null; die "herdr pane run failed"; }
    "$herdr" workspace focus "$ws" >/dev/null || die "herdr workspace focus failed"
    ;;
  *) die "usage: launch.sh open|ui" ;;
esac
