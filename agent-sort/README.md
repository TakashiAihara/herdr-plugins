# agent-sort

Switch the sort of Herdr's built-in Agents view between named presets. A preset is a list of sort keys — Herdr's built-in fields like `attention`, or metadata tokens reported on a pane, like `prio` — plus an optional filter, all from a small TOML config. `next` cycles the presets, the `open` action opens a popup picker, and the last preset is re-applied when Herdr starts.

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
key = "prefix+o"
type = "plugin_action"
command = "takashiaihara.agent-sort.next"
description = "next agent sort"

[[keys.command]]
key = "prefix+O"
type = "plugin_action"
command = "takashiaihara.agent-sort.open"
description = "pick an agent sort"
```

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
- Token values compare as strings, so a single digit needs no padding, but zero-pad two-digit ranges (`prio=03` … `prio=12`) when numeric order matters.
- `filter` is the filter object of the socket API's `agent.view.set`, converted from TOML to JSON untouched; it must be a table.
- Preset names must be non-empty, unique, and at most 32 characters (Herdr's label limit); `sort` must be non-empty with at most 8 entries (Herdr's sort-field limit); `default` must name a preset.
- A `[[sort_presets]]` list replaces the built-ins whole. Without a config file the built-ins are `attention` (attention, then state_change_seq, both desc), `recent` (state_change_seq desc), `workspace` (workspace_order, tab_order, pane_order, all asc).

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

## State

`state.json` in the plugin state directory holds only `{"preset": "<name>"}` or `{"preset": null}`. It is written atomically, and only after Herdr accepted the view.

## Limits

- A view lasts until the Herdr server exits; the plugin's startup hook (`restore`) re-applies the saved preset.
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
