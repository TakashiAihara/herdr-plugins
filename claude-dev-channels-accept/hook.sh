#!/usr/bin/env bash
# Herdr reports a pane whose Claude Code shows the development-channels warning as
# blocked. Answer it with Enter only when the screen shows both lines of that dialog,
# so any other blocked prompt (permissions, questions) is left for the person.
set -u

json="${HERDR_PLUGIN_EVENT_JSON:-}"
herdr="${HERDR_BIN_PATH:-herdr}"

re_pane='"pane_id":"([^"]+)"'
re_status='"agent_status":"([^"]+)"'
re_agent='"agent":"([^"]+)"'
[[ "$json" =~ $re_pane ]] || exit 0
pane="${BASH_REMATCH[1]}"
[[ "$json" =~ $re_status ]] || exit 0
[[ "${BASH_REMATCH[1]}" == blocked ]] || exit 0
[[ "$json" =~ $re_agent ]] || exit 0
[[ "${BASH_REMATCH[1]}" == claude ]] || exit 0

screen="$("$herdr" pane read "$pane" --source visible --lines 40 2>/dev/null)" || exit 0
[[ "$screen" == *"WARNING: Loading development channels"* ]] || exit 0
[[ "$screen" == *"I am using this for local development"* ]] || exit 0

exec "$herdr" pane send-keys "$pane" Enter
