# agent-search

A popup for jumping to an agent, built for sessions with many of them. It lists the panes Herdr's Agents view lists, searches every field Herdr reports for them (title, cwd, agent, status, and every metadata token, shown or not), and lets you sort, group, prioritize and reorder the list. The sort, grouping and manual order stay between openings.

## Requirements

- Herdr 0.9.5+
- Linux or macOS (x86_64 or arm64). Installing downloads a prebuilt binary; nothing else is needed at runtime. On another platform, or when the download fails, it builds from source and needs a Rust toolchain.

## Install

```bash
herdr plugin install TakashiAihara/herdr-plugins/agent-search
```

Herdr does not let a plugin bind keys, so add one to your Herdr config:

```toml
[[keys.command]]
key = "prefix+s"
type = "plugin_action"
command = "takashiaihara.agent-search.open"
description = "search agents"
```

## Keys in the popup

| Key | Action |
|---|---|
| type | fuzzy filter on the `search` fields |
| `Up` / `Down`, `Ctrl-P` / `Ctrl-N` | move |
| `Enter` | jump to the agent |
| `Esc`, `Ctrl-C` | close |
| `ctrl-s` | next sort |
| `ctrl-g` | next grouping |
| `alt-up` / `alt-down` | move the agent up or down (manual order) |
| `alt-1` … `alt-9` | set the agent's priority |
| `alt-0` | clear the agent's priority |

The right side shows every field of the selected pane, so you can see what is searchable and what to put in the config.

## Priority

A pane's priority is a metadata token on the pane, `prio` by default (`priority_token` in the config). Anything can set it, for example a status line:

```bash
herdr pane report-metadata "$HERDR_PANE_ID" --source my-statusline --token prio=3
```

`alt-1` … `alt-9` in the popup set the same token, so other tools and your sidebar rows (`$prio`) see it too. Herdr drops tokens when its server restarts; the plugin keeps a copy of the priorities it set and sends them again from its startup hook and whenever Herdr detects an agent. A token only shows up in the sidebar when your `[ui.sidebar.agents] rows` names it, so you can also report tokens that exist only to be searched.

## Configuration

Optional. Write `config.json` in the directory printed by:

```bash
herdr plugin config-dir takashiaihara.agent-search
```

Lists (`columns`, `search`, `sorts`, `groups`) replace the default list whole; `keys` is merged key by key, so `{"keys": {"sort": "ctrl-x"}}` changes only that key. Fields are paths into one entry of `herdr pane list` (`title`, `agent_status`, `tokens.repo`). `tokens.*` means every token value. Suffix a field with `:basename` to keep the last path segment, and a sort field with `:desc` to reverse it (`cwd:basename:desc`). Two virtual fields exist: `prio` (the pane's priority; usable in `sorts` and `columns`) and `manual` (the order you set with `alt-up` / `alt-down`; usable in `sorts`).

The defaults:

```json
{
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
  "priority_token": "prio",
  "keys": {"sort": "ctrl-s", "group": "ctrl-g", "up": "alt-up", "down": "alt-down", "clear": "alt-0"}
}
```

- `sorts` and `groups` are cycled in the order listed; the first one is used until you switch. A sort's `by` is a list of tie-breakers. A group's `by` is a fallback chain: the first field with a value names the group, and agents with none go to `(none)`. Groups are ordered by their highest-ranked agent, so a group holding a high-priority agent comes first.
- Example: show the branch and group by workspace first.

```json
{
  "columns": ["prio", "agent_status", "tokens.branch", "title"],
  "groups": [{"name": "workspace", "by": ["workspace_id"]}, {"name": "none", "by": []}]
}
```

A config that is not a JSON object, or has a key of the wrong shape (an empty list, duplicate sort or group names, an empty key name, an invalid token name), stops the popup with a message instead of falling back to the defaults.

## State

`state.json` in the plugin state directory holds the current sort and grouping, the manual order, and the priorities the popup set. An agent is keyed by its agent session id when it has one, so its manual order survives a Herdr restart that renumbers panes; other panes are keyed by pane id. Entries of panes that are gone are dropped on every change.

## Limits

- Only panes with an agent are listed, and only those of the Herdr server the popup runs on. `herdr pane list` covers one server; agents on other machines are not merged in.
- Herdr can focus a pane by id only through `herdr agent focus`, which also marks a done agent as seen, as looking at it in Herdr does. When that fails (the agent exited after the list was taken), the popup focuses the pane's workspace and tab instead.
- `alt-up` / `alt-down` trade the manual positions of the selected agent and its neighbor in the same group; the rest of the manual order stays. Under a sort where `manual` is only a tie-breaker, the swap shows only between agents that tie on the earlier keys.
- The `alt-*` keys need the terminal to send Option/Alt as Meta (macOS Terminal.app: Settings → Profiles → Keyboard → "Use Option as Meta key"), or move `up` / `down` / `clear` to other keys in `keys` (`alt-1` … `alt-9` are fixed).
- Panes that report the same agent session id: the first in the list keeps the session-keyed order; the others are keyed by pane id while the duplicate lasts.
- The popup works on one snapshot of the pane list taken when it opens. Agents started while it is up appear the next time.

## Development

```bash
cargo build --release
cargo test
bash tests/search_test.sh   # the subcommands against a canned pane list and a fake herdr
bash tests/fetch_test.sh    # scripts/fetch-or-build.sh against a local release
```

`.github/workflows/agent-search-binaries.yml` builds a binary per platform for every `agent-search/` tree that reaches main and publishes it to the `agent-search-binaries` release, named by the tree hash. `scripts/fetch-or-build.sh`, the plugin's build step, downloads the one for the installed tree and checks its SHA-256.
