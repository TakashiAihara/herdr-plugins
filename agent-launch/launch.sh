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

MODE="${1:-}"
case "$MODE" in
  open)
    # The pane you were in decides where the new workspace starts, so a repo-less command
    # (claude, or ccxn without a repo) runs in that repo. Fields of one pane are flat until
    # agent_session/scroll/tokens, and cwd is immediately followed by focused (herdr 0.9.5).
    list="$("$herdr" pane list)" || die "herdr pane list failed"
    re='"cwd":"([^"]*)","focused":true'
    [[ "$list" =~ $re ]] || die "no focused pane in herdr pane list"
    exec "$herdr" plugin pane open --plugin "${HERDR_PLUGIN_ID:-takashiaihara.agent-launch}" \
      --entrypoint prompt --env "AGENT_LAUNCH_CWD=${BASH_REMATCH[1]}" >/dev/null
    ;;
  ui)
    cwd="${AGENT_LAUNCH_CWD:-$HOME}"
    t="$(template)"
    echo "in: $cwd"
    echo "run: $t"
    repo=""
    if [[ "$t" == *"{repo}"* ]]; then
      read -r -e -p "repo (empty = this directory)> " repo || exit 0
    fi
    read -r -e -p "prompt> " prompt || exit 0
    [[ -n "$prompt" ]] || exit 0

    cmd="${t//\{prompt\}/$(quote "$prompt")}"
    if [[ -n "$repo" ]]; then cmd="${cmd//\{repo\}/$(quote "$repo")}"; else cmd="${cmd//\{repo\}/}"; fi

    # A failed create prints nothing, which the root-pane check below reports.
    out="$("$herdr" workspace create --cwd "$cwd" --label "${prompt:0:24}" --focus)"
    re='"root_pane":\{[^}]*"pane_id":"([^"]+)"'
    [[ "$out" =~ $re ]] || die "herdr workspace create returned no root pane: $out"
    "$herdr" pane run "${BASH_REMATCH[1]}" "$cmd" >/dev/null || die "herdr pane run failed"
    ;;
  *) die "usage: launch.sh open|ui" ;;
esac
