#!/usr/bin/env bash
# Runs agent-sort against a fake Herdr socket (a Unix socket server that records each request) and
# a fake herdr binary that records its argv. The popup is tested in Rust; everything it does goes
# through the subcommands tested here.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
bin="${AGENT_SORT_BIN:-$here/../target/release/agent-sort}"
tmp="$(mktemp -d)"
server_pid=
trap '[ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null; rm -rf "$tmp"' EXIT

command -v python3 > /dev/null || { echo "FAIL python3 is needed for the fake socket"; exit 1; }

cat > "$tmp/herdr" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_CALLS"
exit 0
FAKE
chmod +x "$tmp/herdr"

# Appends every request line to $FAKE_REQS and answers with the line in $FAKE_REPLY_FILE (or an
# agent_view result when that file is missing), echoing the request id.
cat > "$tmp/server.py" <<'PY'
import json, os, socket, sys
path, reqs, reply_file = sys.argv[1], sys.argv[2], sys.argv[3]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(path)
s.listen(16)
while True:
    c, _ = s.accept()
    f = c.makefile("rb")
    line = f.readline()
    with open(reqs, "ab") as out:
        out.write(line if line.endswith(b"\n") else line + b"\n")
    try:
        rid = json.loads(line).get("id")
    except Exception:
        rid = None
    if os.path.exists(reply_file):
        body = json.loads(open(reply_file).read())
    else:
        body = {"result": {"type": "agent_view", "active": True}}
    body["id"] = rid
    c.sendall((json.dumps(body) + "\n").encode())
    c.close()
PY
python3 "$tmp/server.py" "$tmp/sock" "$tmp/reqs" "$tmp/reply.json" &
server_pid=$!
for _ in $(seq 50); do [ -S "$tmp/sock" ] && break; sleep 0.1; done
[ -S "$tmp/sock" ] || { echo "FAIL the fake socket did not start"; exit 1; }

export HERDR_SOCKET_PATH="$tmp/sock" HERDR_BIN_PATH="$tmp/herdr" HERDR_PLUGIN_ID=test.agent-sort
export HERDR_PLUGIN_CONFIG_DIR="$tmp/config" HERDR_PLUGIN_STATE_DIR="$tmp/state" FAKE_CALLS="$tmp/calls" FAKE_REQS="$tmp/reqs"
mkdir -p "$HERDR_PLUGIN_CONFIG_DIR" "$HERDR_PLUGIN_STATE_DIR"
cfg="$HERDR_PLUGIN_CONFIG_DIR/config.toml"
state="$HERDR_PLUGIN_STATE_DIR/state.json"

fails=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1"; echo "  want: $3"; echo "  got:  $2"; fails=$((fails + 1)); fi
}
# Fresh calls and requests for the next step; config and state stay unless a step resets them.
fresh() { : > "$tmp/calls"; : > "$tmp/reqs"; rm -f "$tmp/reply.json"; }
reset() { fresh; rm -f "$cfg" "$state"; }
run() { "$bin" "$@" > "$tmp/out" 2> "$tmp/err"; }
# One line per request: method, then params with keys sorted, so the comparison ignores key order.
reqs() {
  python3 -c '
import json, sys
for line in open(sys.argv[1]):
    if line.strip():
        r = json.loads(line)
        print(r["method"], json.dumps(r["params"], sort_keys=True, separators=(",", ":")))
' "$tmp/reqs"
}
preset_in_state() { python3 -c 'import json,sys; print(json.dumps(json.load(open(sys.argv[1])).get("preset", "missing")))' "$state" 2>/dev/null || echo "no state"; }

ATTENTION='"sort":[{"field":"attention","order":"desc"},{"field":"state_change_seq","order":"desc"}]'
RECENT='"sort":[{"field":"state_change_seq","order":"desc"}]'
WORKSPACE='"sort":[{"field":"workspace_order","order":"asc"},{"field":"tab_order","order":"asc"},{"field":"pane_order","order":"asc"}]'
SRC='"source":"plugin:test.agent-sort"'

reset
run probe
check "probe answers ok without touching Herdr" "$(cat "$tmp/out"; reqs; cat "$tmp/calls")" "ok"

reset
run presets
check "without a config the built-in presets are listed" "$(cat "$tmp/out")" "attention
recent
workspace"

reset
run next; rc=$?
check "next without state applies the first preset and saves it" \
  "$(echo "rc=$rc"; reqs; preset_in_state)" \
  "rc=0
agent.view.set {\"label\":\"attention\",$SRC,$ATTENTION}
\"attention\""
check "next tells the user which preset is active" "$(grep -c 'notification show.*attention' "$tmp/calls")" "1"

fresh; run next
check "next moves to the following preset" "$(reqs; preset_in_state)" "agent.view.set {\"label\":\"recent\",$SRC,$RECENT}
\"recent\""
fresh; run next; fresh; run next
check "next wraps around after the last preset" "$(reqs; preset_in_state)" "agent.view.set {\"label\":\"attention\",$SRC,$ATTENTION}
\"attention\""

fresh; run set workspace; rc=$?
check "set applies the named preset" "$(echo "rc=$rc"; reqs; preset_in_state)" "rc=0
agent.view.set {\"label\":\"workspace\",$SRC,$WORKSPACE}
\"workspace\""

fresh; run set nope; rc=$?
check "set with an unknown name fails, names it, sends nothing and keeps the state" \
  "$(echo "rc=$rc"; reqs; preset_in_state; grep -c nope "$tmp/err")" "rc=1
\"workspace\"
1"

fresh; run clear; rc=$?
check "clear removes the plugin's view and remembers that nothing is applied" \
  "$(echo "rc=$rc"; reqs; preset_in_state; grep -c 'notification show' "$tmp/calls")" "rc=0
agent.view.clear {$SRC}
null
1"

fresh; run restore; rc=$?
check "restore after clear sends nothing" "$(echo "rc=$rc"; reqs)" "rc=0"

fresh; run next
check "next after clear starts again from the first preset" "$(preset_in_state)" "\"attention\""

fresh; run set recent; fresh; run restore; rc=$?
check "restore applies the saved preset" "$(echo "rc=$rc"; reqs)" "rc=0
agent.view.set {\"label\":\"recent\",$SRC,$RECENT}"
check "restore does not notify on success" "$(grep -c 'notification show' "$tmp/calls")" "0"

reset; run restore; rc=$?
check "restore with no state and no default sends nothing" "$(echo "rc=$rc"; reqs)" "rc=0"

reset
cat > "$cfg" <<'TOML'
default = "prio"

[[sort_presets]]
name = "prio"
sort = [
  { token = "prio", order = "desc" },
  { field = "attention", order = "desc" },
  { field = "state_change_seq" },
]
filter = { op = "in", field = "status", values = ["blocked", "done", "working"] }

[[sort_presets]]
name = "repo"
sort = [{ token = "repo" }]
TOML
run presets
check "a config replaces the built-in presets" "$(cat "$tmp/out")" "prio
repo"
run restore; rc=$?
check "restore with no state applies the config default, mixing tokens and fields, order defaulting to asc, filter passed through" \
  "$(echo "rc=$rc"; reqs)" \
  "rc=0
agent.view.set {\"filter\":{\"field\":\"status\",\"op\":\"in\",\"values\":[\"blocked\",\"done\",\"working\"]},\"label\":\"prio\",$SRC,\"sort\":[{\"field\":{\"token\":\"prio\"},\"order\":\"desc\"},{\"field\":\"attention\",\"order\":\"desc\"},{\"field\":\"state_change_seq\",\"order\":\"asc\"}]}"
fresh; run next
check "next without state starts from the config default" "$(reqs)" "agent.view.set {\"label\":\"repo\",$SRC,\"sort\":[{\"field\":{\"token\":\"repo\"},\"order\":\"asc\"}]}"

fresh; echo '{"preset":"gone"}' > "$state"; run next
check "next from a preset the config no longer has starts from the first" "$(preset_in_state)" "\"prio\""

bad() { # bad <description> <config> <text the error must contain>
  reset; echo '{"preset":"x"}' > "$state"; printf '%s\n' "$2" > "$cfg"
  for cmd in next restore "set x"; do
    fresh
    # shellcheck disable=SC2086
    run $cmd; rc=$?
    check "$1 ($cmd)" \
      "$(echo "rc=$rc"; reqs; preset_in_state; grep -c -- "$3" "$tmp/err"; grep -c 'notification show' "$tmp/calls")" \
      "rc=1
\"x\"
1
1"
  done
}
bad "an unknown field is a config error" '[[sort_presets]]
name = "x"
sort = [{ field = "nope" }]' 'nope'
bad "an empty sort is a config error" '[[sort_presets]]
name = "x"
sort = []' 'sort'
bad "duplicate preset names are a config error" '[[sort_presets]]
name = "x"
sort = [{ field = "agent" }]
[[sort_presets]]
name = "x"
sort = [{ field = "seen" }]' 'duplicate'
bad "a default that names no preset is a config error" 'default = "y"
[[sort_presets]]
name = "x"
sort = [{ field = "agent" }]' '"y"'
bad "an invalid token name is a config error" '[[sort_presets]]
name = "x"
sort = [{ token = "a b" }]' 'a b'
bad "an order other than asc or desc is a config error" '[[sort_presets]]
name = "x"
sort = [{ field = "agent", order = "down" }]' 'down'
bad "an entry with both token and field is a config error" '[[sort_presets]]
name = "x"
sort = [{ field = "agent", token = "prio" }]' 'token'
bad "a file that is not TOML is a config error" 'this is = = not toml' 'config.toml'

reset
printf '[[sort_presets]]\nname = "x"\nsort = [{ field = "agent" }]\n[keys]\nedit = "ctrl-nonsense"\n' > "$cfg"
echo '{"preset":"x"}' > "$state"
run restore
check "restore ignores keys it does not use, even unusable ones" "$(reqs)" "agent.view.set {\"label\":\"x\",$SRC,\"sort\":[{\"field\":\"agent\",\"order\":\"asc\"}]}"
fresh; run set x; rc=$?
check "set ignores unusable keys too" "$(echo "rc=$rc"; reqs)" "rc=0
agent.view.set {\"label\":\"x\",$SRC,\"sort\":[{\"field\":\"agent\",\"order\":\"asc\"}]}"

reset
echo '{"preset":"recent"}' > "$state"
echo '{"error":{"code":"invalid_params","message":"boom from herdr"}}' > "$tmp/reply.json"
run next; rc=$?
check "an error from Herdr fails, shows its message and keeps the state" \
  "$(echo "rc=$rc"; preset_in_state; grep -c 'boom from herdr' "$tmp/err"; grep -c 'notification show' "$tmp/calls")" "rc=1
\"recent\"
1
1"

reset
HERDR_SOCKET_PATH="$tmp/no-such-sock" run next; rc=$?
check "an unreachable socket fails and saves nothing" "$(echo "rc=$rc"; preset_in_state)" "rc=1
no state"

reset
run open; rc=$?
check "open opens the picker popup" "$(echo "rc=$rc"; cat "$tmp/calls")" "rc=0
plugin pane open --plugin test.agent-sort --entrypoint picker"

reset
EDITOR=true VISUAL= run edit; rc=$?
check "edit writes a commented template when there is no config" \
  "$(echo "rc=$rc"; [ -s "$cfg" ] && echo written; grep -q '^#' "$cfg" && echo commented)" "rc=0
written
commented"
run presets
check "the template holds the built-in presets" "$(cat "$tmp/out")" "attention
recent
workspace"
cp "$cfg" "$tmp/template"
fresh; EDITOR=true VISUAL= run edit
check "edit leaves an existing config alone" "$(cmp -s "$cfg" "$tmp/template" && echo same)" "same"

reset
echo '{"preset":"attention"}' > "$state"
cat > "$tmp/editor" <<'SH'
#!/bin/sh
printf '[[sort_presets]]\nname = "attention"\nsort = [{ token = "prio", order = "desc" }]\n' > "$1"
SH
chmod +x "$tmp/editor"
VISUAL="$tmp/editor" EDITOR=false run edit; rc=$?
check "edit runs \$VISUAL over \$EDITOR, then reapplies the active preset from the new config" "$(echo "rc=$rc"; reqs)" "rc=0
agent.view.set {\"label\":\"attention\",$SRC,\"sort\":[{\"field\":{\"token\":\"prio\"},\"order\":\"desc\"}]}"

fresh
cat > "$tmp/editor" <<'SH'
#!/bin/sh
printf '[[sort_presets]]\nname = "other"\nsort = [{ field = "agent" }]\n' > "$1"
SH
VISUAL="$tmp/editor" run edit; rc=$?
check "edit that drops the active preset applies nothing and says so" \
  "$(echo "rc=$rc"; reqs; grep -c 'notification show' "$tmp/calls")" "rc=0
1"

fresh
printf '#!/bin/sh\necho "broken = = =" > "$1"\n' > "$tmp/editor"
VISUAL="$tmp/editor" run edit; rc=$?
check "edit that leaves a broken config fails without applying" "$(echo "rc=$rc"; reqs; grep -c 'config.toml' "$tmp/err")" "rc=1
1"

[ "$fails" -eq 0 ] && echo "all passed" || { echo "$fails failed"; exit 1; }
