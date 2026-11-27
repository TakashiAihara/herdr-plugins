# agent-sort

Switch the sort of Herdr's built-in Agents view between named presets. A preset is a list of sort keys — Herdr's built-in fields like `attention`, or metadata tokens reported on a pane, like `prio` — plus an optional filter, all from a small TOML config. Herdr's own two orders, `spaces` and `priority`, are always in the list next to the presets. `next` cycles the list, the `open` action opens a popup picker, and the last choice is re-applied when Herdr starts.

## Requirements

- Herdr 0.9.5+
- Linux or macOS (x86_64 or arm64). Installing downloads a prebuilt binary; nothing else is needed at runtime. On another platform, or when the download fails, it builds from source and needs a Rust toolchain.

## Install

```bash
herdr plugin install TakashiAihara/herdr-plugins/agent-sort
```

Herdr does not let a plugin bind keys, so add bindings to your Herdr config:

```toml
[[keys.command]]
key = "prefix+s"
type = "plugin_action"
command = "takashiaihara.agent-sort.next"
description = "next agent sort"

[[keys.command]]
key = "prefix+shift+s"
type = "plugin_action"
command = "takashiaihara.agent-sort.open"
description = "pick an agent sort"
```

`prefix+s` replaces Herdr's default `settings` binding. If your config sets `settings = "prefix+s"` explicitly (for example, a copy of the default config), Herdr keeps `settings` and disables this binding; `herdr config check` reports it. Move `settings` or pick another key for `next`.

## Built-in presets

The list always starts with Herdr's own two orders, the values of `ui.agent_panel_sort`; a config cannot remove or redefine them:

| Entry | Sort | What you see |
|---|---|---|
| `spaces` | `workspace_order` asc | Herdr's `"spaces"`: agents as workspaces, tabs and panes are arranged |
| `priority` | `attention` desc, then `state_change_seq` desc | Herdr's `"priority"`: agents that need you: blocked first, then done (finished, not looked at yet), then working, then idle you have already seen. Within each, the one whose state changed most recently |

Then come the presets. Without a config file they are these two, so `next` cycles `spaces` → `priority` → `recent` → `workspace` → `spaces`:

| Preset | Sort | What you see on top |
|---|---|---|
| `recent` | `state_change_seq` desc | The agent whose state changed most recently (started, finished, got blocked), whatever that state is |
| `workspace` | `workspace_order`, `tab_order`, `pane_order`, all asc | Herdr's own layout order: workspaces, then tabs, then panes as they are arranged |

`spaces` and `priority` are views the plugin applies, like any preset: they show Herdr's order (as Herdr 0.9.5 sorts it) whatever `ui.agent_panel_sort` is set to, and `restore` puts the chosen one back at every start. `0` in the popup (or `agent-sort clear`) drops the plugin's view altogether, and the value of `ui.agent_panel_sort` in Herdr's config applies again.

## Fields

What each built-in field sorts by (Herdr 0.9.5, `agent.view.set`):

| Field | Value | `desc` puts first |
|---|---|---|
| `attention` | 4 blocked, 3 done (idle, not seen yet), 2 working, 1 idle and seen, 0 unknown | blocked |
| `state_change_seq` | A counter Herdr bumps each time an agent changes state | the most recent change |
| `status` | `blocked`, `done`, `idle`, `unknown`, `working`, compared as text | `working` (alphabetical, not by urgency; use `attention` for urgency) |
| `seen` | whether you have looked at the agent since its last change | seen ones |
| `agent` | the agent's kind (`claude`, `devin`, ...), compared as text | reverse alphabetical |
| `workspace_order` / `tab_order` / `pane_order` | position in Herdr's layout | the last workspace / tab / pane (`asc` gives layout order) |
| `token = "<name>"` | a metadata token reported on the pane, compared as text | the largest value |

## Custom presets

Write your own presets when the built-ins are not the order you want: for example a priority token first, then urgency, then recency. Press `e` in the popup to open the config; on first use it is a commented template holding the two built-in presets, so you can copy one and change it. When you save and close the editor, the popup reloads the presets and re-applies the active one with its new definition.

## Configuration

Optional. Write `config.toml` in the directory printed by:

```bash
herdr plugin config-dir takashiaihara.agent-sort
```

`agent-sort edit` opens that file in `$VISUAL`, `$EDITOR`, or `vi`, writing a commented template first when it is missing.

```toml
# Optional: the preset used until you pick one, and on startup when nothing was picked.
default = "prio"

[keys]
up = ["up", "k"]
down = ["down", "j"]
select = ["enter"]
edit = ["e"]
clear = ["0"]
quit = ["esc", "q", "ctrl-c"]

[[sort_presets]]
name = "prio"
sort = [
  { token = "prio", order = "desc" },      # a metadata token reported on the pane
  { field = "attention", order = "desc" }, # a built-in Agents view field
  { field = "state_change_seq" },          # order defaults to "asc"
]
# Optional filter, passed to Herdr as-is:
filter = { op = "in", field = "status", values = ["blocked", "done"] }
```

- `sort` entries mix `token = "<name>"` and `field = "<name>"`; exactly one of the two per entry, `order` is `"asc"` (default) or `"desc"`.
- The built-in fields are `workspace_order`, `tab_order`, `pane_order`, `attention`, `status`, `agent`, `seen`, `state_change_seq`.
- A token name is 1–32 of `[A-Za-z0-9_-]`, the same rule Herdr uses for `pane report-metadata --token`.
- Token values compare as strings, so pad to the same number of digits whenever values can have different lengths (for 1–12 write `01`–`12`, or `"10"` sorts before `"3"`).
- `filter` is the filter object of the socket API's `agent.view.set`, converted from TOML to JSON untouched; it must be a table.
- Preset names must be non-empty, unique, and at most 32 characters (Herdr's label limit); `sort` must be non-empty with at most 8 entries (Herdr's sort-field limit); `default` must name a preset or one of Herdr's two (`spaces`, `priority`).
- `spaces` and `priority` are taken: a preset with one of those names is a config error; rename it.
- `attention` used to be a built-in preset with the same sort as `priority`. Where no preset of yours has that name, `default = "attention"` and `agent-sort set attention` mean `priority`. A saved `attention` is treated like any preset removed from the config: the next start says so once and applies the `default`, if there is one.
- A `[[sort_presets]]` list replaces the two built-in presets whole; copy the ones you still want from [Built-in presets](#built-in-presets). `spaces` and `priority` stay in front of your list.

## Keys in the popup

| Key | Action |
|---|---|
| `Up`, `k` | previous preset |
| `Down`, `j` | next preset |
| `Enter` | apply the highlighted preset and close |
| `0` | clear the plugin's sort and close |
| `e` | edit `config.toml`, then reload and re-apply |
| `Esc`, `q`, `Ctrl-C` | close |

Each action takes one key name or a list under `[keys]`. Any key error — a bad name, a bind duplicated across actions, or a custom bind clashing with another action's default (e.g. `edit = "q"`, already the default for quit) — makes the popup use all the default keys and show the error in its status line. The other subcommands do not read `[keys]`.

## Reporting your own metric

A `token` sort key orders panes by a metadata token, but the plugin never reports tokens — your agent, a status line, or a hook does:

```bash
herdr pane report-metadata "$HERDR_PANE_ID" --source my-metric --token prio=3
```

Herdr drops all tokens when its server restarts, so whatever reports them should report again from a hook or a periodic refresh, or the token sort goes flat.

## Tips

### Order agents by start time

Herdr 0.9.5 has no field for when a pane or agent was created, so report the start time as a token and sort by it. For Claude Code, the process start time is `startedAt` (epoch milliseconds) in `sessions/<pid>.json` under the config directory (`$CLAUDE_CONFIG_DIR`, or `~/.claude` when it is unset), and a hook runs a few processes below the `claude` process. A hook wired to `SessionStart` and `UserPromptSubmit` can report it:

```bash
#!/bin/bash
# Report the start time of the claude process above this hook as the `started` token.
# CLAUDE_CODE_ENTRYPOINT=cli skips `claude -p` runs that inherit the pane's environment.
[ -n "${HERDR_PANE_ID:-}" ] && [ "${CLAUDE_CODE_ENTRYPOINT:-}" = cli ] || exit 0
sessions="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/sessions"
pid=$PPID
while [ -n "$pid" ] && [ "$pid" -gt 1 ]; do
  # Files of exited sessions stay behind and pids are reused, so take only a process named claude,
  # and stop at the nearest one so an outer claude's time is never reported.
  if [ "$(basename -- "$(ps -o comm= -p "$pid")")" = claude ]; then
    [ -f "$sessions/$pid.json" ] || exit 0
    started=$(python3 -c 'import json, sys, time; ms = int(json.load(open(sys.argv[1]))["startedAt"]); print(time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(ms // 1000)) + ".%03dZ" % (ms % 1000))' "$sessions/$pid.json") || exit 0
    herdr pane report-metadata "$HERDR_PANE_ID" --source started-at --token "started=$started" >/dev/null 2>&1
    exit 0
  fi
  pid=$(ps -o ppid= -p "$pid" | tr -d ' ')
done
```

A token stays on the pane until it is replaced or cleared, so clear it from a `SessionEnd` hook. Otherwise another agent started later in the same pane sorts by the old time:

```bash
[ -n "${HERDR_PANE_ID:-}" ] && [ "${CLAUDE_CODE_ENTRYPOINT:-}" = cli ] || exit 0
herdr pane report-metadata "$HERDR_PANE_ID" --source started-at --clear-token started >/dev/null 2>&1
exit 0
```

Then add a preset that sorts by it, oldest first. A `[[sort_presets]]` list replaces the built-in presets whole (see [Configuration](#configuration)), so copy the built-ins you still want next to it:

```toml
[[sort_presets]]
name = "started"
sort = [{ token = "started" }, { field = "pane_order" }]
```

- The script needs `python3`, and assumes the native install, where the process is named `claude`. With the npm install the process is named `node`, the loop finds nothing, and no token is reported.
- The value is UTC in a fixed-width form with milliseconds, so comparing it as text gives the time order, even for agents started in the same second. Local time would break across a daylight-saving change.
- An interactive `claude` started inside another one in the same pane reports its own time over the outer one's, and clears it when it exits. The outer one gets it back on its next prompt.
- Clearing on `SessionEnd` is best effort. When Claude Code is killed or crashes, `SessionEnd` does not run and the old value stays on the pane until the next Claude Code session there reports a new one.
- Avoid `PreToolUse`: the hook runs synchronously before every tool call, and walking the process tree adds tens of milliseconds each time. A token that is not reported again keeps its value, so reporting once per prompt is enough.
- A session reopened with `claude --resume`, or restored after a Herdr restart, gets the time it was reopened.
- Herdr drops the token when its server restarts. Reporting from a hook that fires on every prompt puts it back on the next one.
- Agents without the token (other agent kinds, or a pane whose hook has not fired yet) sort after the ones that have it.

## State

`state.json` in the plugin state directory holds only `{"preset": "<name>"}` or `{"preset": null}`. It is written atomically, and only after Herdr accepted the view.

## Limits

- A view lasts until the Herdr server exits; the plugin's startup hook (`restore`) re-applies the saved preset, or the config `default` when the saved preset was removed from the config — it says so once and forgets the stale name.
- Agents without the token sort after agents that have it; equal entries keep Herdr's own order.
- The view is one projection for the whole Agents view, not per pane group or per workspace.
- Herdr has a single Agents view override shared by every source. If another plugin or a manual `agent.view.set` replaces it, agent-sort's state still names its preset: the popup's current mark, `next`, and `restore` at startup all follow the state.

## Development

```bash
cargo build --release
cargo test
bash tests/sort_test.sh   # the subcommands against a fake Herdr socket and a fake herdr
bash tests/fetch_test.sh  # scripts/fetch-or-build.sh against a local release
```

`.github/workflows/agent-sort-binaries.yml` builds a binary per platform for every `agent-sort/` tree that reaches main and publishes it to the `agent-sort-binaries` release, named by the tree hash. `scripts/fetch-or-build.sh`, the plugin's build step, downloads the one for the installed tree and checks its SHA-256.
