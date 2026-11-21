# agent-search

A popup for jumping to an agent, built for sessions with many of them. It lists the panes Herdr's Agents view lists, searches every field Herdr reports for them (title, cwd, agent, status, the agent session id, and every metadata token, shown or not), and lets you sort, group, prioritize and reorder the list. The sort, grouping, match mode, preview and wrap choices and the manual order stay between openings.

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
| type | filter on the `search` fields (fuzzy or substring) |
| `Up` / `Down`, `Ctrl-P` / `Ctrl-N` | move |
| `Enter` | jump to the agent |
| `Esc`, `Ctrl-C` | close |
| `ctrl-s` | next sort |
| `ctrl-g` | next grouping |
| `ctrl-r` | toggle match mode: fuzzy ↔ substring |
| `ctrl-o` | hide or show the preview |
| `ctrl-w` | toggle preview wrapping: cut long lines ↔ wrap them with a hanging indent |
| `alt-up` / `alt-down` | move the agent up or down (manual order) |
| `alt-1` … `alt-5` | set the agent's priority (up to `priority_max`) |
| `alt-0` | clear the agent's priority |

The popup is 90% of the screen wide and 85% high, set in the plugin manifest; Herdr has no per-user override for a plugin's popup size.

The right side previews the selected pane as an outline in up to three sections, in this order: `[ids]` holds every field whose name ends in `_id` (`pane_id`, `workspace_id`, `tab_id`, `terminal_id`, and any other identifier Herdr reports) under its Herdr name; `[metadata]` every other field but `tokens` (`agent`, `agent_status`, `cwd`, `title`, `state_labels`, `agent_session`, ...); `[tokens]` each token on its own line, named without the `tokens.` prefix. A scalar field is one `key  value` line; a field holding a JSON object or array (`agent_session`, `state_labels`, a token that is itself an object) is its name on one line with each entry indented two spaces below it, recursively, an array's index as the key. Every value starts in one column, and a section with nothing in it is left out. For example:

```text
[ids]
pane_id        w2:p1
workspace_id   w2
tab_id         w2:t1
[metadata]
agent          devin
agent_status   working
agent_session
  value        s3
title          write docs
cwd            /src/alpha
[tokens]
repo           alpha
task           urgent kaneo
```

`ctrl-o` hides the preview so the list takes the full width; the choice is saved in the state like the sort and match mode, so it wins over the configured `preview` until toggled again. `ctrl-w` toggles wrapping, saved in the state the same way and winning over `preview_wrap` until toggled again. With wrapping off (the default), a line wider than the pane is cut at its right edge. With wrapping on, a long `key  value` line continues on the next lines starting at its value column, and a long outline parent (a key with no value of its own) continues at its own indent; wrapping counts terminal cells, so a wide character is never split down the middle. `preview_hide` is a list of field paths left out of the preview — a nested entry by its path (`tokens.flags`, `agent_session.kind`) or a whole object (`state_labels`) — and only affects the preview, never the search or the list. For example, to hide the statusline helper tokens:

```json
{"preview_hide": ["tokens.last", "tokens.mid", "tokens.flags", "tokens.prtitle"]}
```

This is also how you see what is searchable and what to put in the config.

## Search

The filter runs with `nucleo-matcher`, the same kind of matching as fzf, in one of two modes shown in the header. In `fuzzy` mode (the default), every space-separated word of the query must appear in the row's search text with its letters in order, not necessarily next to each other, so `agsr` matches `agent-search`. In `substring` mode every word must appear as a contiguous substring, so `agsr` does not match `agent-search` but `agent` does. Matching is smart case in both modes: an all-lowercase word ignores case, a capital letter makes it case-sensitive. The fzf pattern syntax works in both modes: `'exact` for a literal substring, `^prefix`, `suffix$`, and `!not` to exclude. Matches keep the current sort order; searching never reorders by score.

`ctrl-r` toggles the mode, and the toggled mode is saved in the state like the sort and grouping: it wins over the configured `match` until toggled again.

`search` and `columns` are independent lists: a field can be searchable without being shown in the list. The default `search` includes `agent_session.value`, the agent's stored native session id, so a session id finds its pane in any environment even when no token carries it.

## Priority

A pane's priority is a metadata token on the pane, `prio` by default (`priority_token` in the config), a number from 1 to `priority_max` (5 by default). `alt-1` … `alt-<priority_max>` in the popup set it, and any agent can set its own the same way:

```bash
herdr pane report-metadata "$HERDR_PANE_ID" --source takashiaihara.agent-search --token prio=3
```

The token is the source of truth: when the popup opens and whenever a CLI act reads the live pane list, a token that is an integer from 1 to `priority_max` is copied into the saved state (under the state lock) when it differs from the saved value, and a token of `0` removes the saved value and counts as no priority — the pane shows an empty priority and sorts with the panes that have none — `--token prio=0` is how an agent clears its own priority. Any other token value is shown but not copied, and a pane without the token keeps its saved value: a token cleared from outside cannot be told apart from one Herdr dropped at a restart, so it is not removed from the state and comes back on the next restore. Acts run inside the popup never copy tokens: its pane list can be minutes old, and a stale token would overwrite a newer priority. Other tools and your sidebar rows (`$prio`) see it too, and a token only shows up in the sidebar when your `[ui.sidebar.agents] rows` names it, so you can also report tokens that exist only to be searched.

Herdr drops tokens when its server restarts. The state keeps the copy taken above, and `restore` (run by the startup hook and whenever Herdr detects an agent) writes the saved value back to every pane that lost it, so a priority an agent set itself survives a restart once the popup or a CLI act has run. `restore` only writes to panes that are missing the token; it never copies a pane's token into the state.

## Configuration

Optional. Write `config.json` in the directory printed by:

```bash
herdr plugin config-dir takashiaihara.agent-search
```

The file is read every time the popup opens, so a change shows up on the next open.

Lists (`columns`, `search`, `sorts`, `groups`) replace the default list whole; `keys` is merged key by key, so `{"keys": {"sort": "ctrl-x"}}` changes only that key. Fields are paths into one entry of `herdr pane list` (`title`, `agent_status`, `tokens.repo`). `tokens.*` means every token value. Suffix a field with `:basename` to keep the last path segment, and a sort field with `:desc` to reverse it (`cwd:basename:desc`). Two virtual fields exist: `prio` (the pane's priority; usable in `sorts` and `columns`) and `manual` (the order you set with `alt-up` / `alt-down`; usable in `sorts`).

The defaults:

```json
{
  "columns": ["prio", "agent_status", "title"],
  "search": ["title", "cwd", "agent", "agent_status", "terminal_title_stripped", "agent_session.value", "tokens.*"],
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
  "priority_max": 5,
  "match": "fuzzy",
  "preview": true,
  "preview_wrap": false,
  "preview_hide": [],
  "keys": {"sort": "ctrl-s", "group": "ctrl-g", "match": "ctrl-r", "preview": "ctrl-o", "wrap": "ctrl-w", "up": "alt-up", "down": "alt-down", "clear": "alt-0"}
}
```

- `sorts` and `groups` are cycled in the order listed; the first one is used until you switch. A sort's `by` is a list of tie-breakers. A group's `by` is a fallback chain: the first field with a value names the group, and agents with none go to `(none)`. Groups are ordered by their highest-ranked agent, so a group holding a high-priority agent comes first.
- `priority_max` is an integer from 1 to 9 (default 5): the popup binds `alt-1` through `alt-<priority_max>` and the higher digits do nothing, though `alt-1` … `alt-9` stay reserved for priorities in `keys` validation either way. A value outside 1..9 or not an integer is a config error naming `priority_max`.
- `match` is `"fuzzy"` (the default) or `"substring"`, the mode the popup's filter starts in; anything else is a config error naming `match`.
- `preview` is `true` (the default) or `false`, whether the preview pane starts shown; anything else is a config error naming `preview`.
- `preview_wrap` is `true` or `false` (default `false`), whether preview lines wider than the pane wrap with a hanging indent instead of being cut at the right edge; anything else is a config error naming `preview_wrap`. `ctrl-w` toggles it and the saved choice wins over this default until toggled again.
- `preview_hide` is a list of field paths left out of the preview; every entry must be a non-empty string or it is a config error naming `preview_hide`.
- For nested headers a group gives `levels` instead of `by`: a list of fallback chains, one per level, no chain empty. A group needs exactly one of `by` or `levels` (`by: []` means no grouping). Headers nest two spaces per level, count the agents under them, and a header with no matching agents is hidden. For example, repo over branch:

```json
{"name": "repo/branch", "levels": [["tokens.repo", "cwd:basename"], ["tokens.branch"]]}
```

```text
▾ herdr-plugins (3)
  ▾ feat/agent-search (2)
```

- Sorting compares strings as strings, so a token sort puts `10` before `9`; the `prio` virtual field compares as a number. To sort by a token value with priority as the tie-breaker:

```json
{
  "sorts": [{"name": "repo", "by": ["tokens.repo", "prio:desc"]}]
}
```
- Example: show the branch and group by workspace first.

```json
{
  "columns": ["prio", "agent_status", "tokens.branch", "title"],
  "groups": [{"name": "workspace", "by": ["workspace_id"]}, {"name": "none", "by": []}]
}
```

A config that is not a JSON object, or has a key of the wrong shape (an empty list, duplicate sort or group names, a group without exactly one of `by` and `levels` or with an empty `levels` chain, an empty key name, an invalid token name, a `priority_max` that is not an integer from 1 to 9, a `match` that is not `"fuzzy"` or `"substring"`, a `preview` or `preview_wrap` that is not `true` or `false`, a `preview_hide` that is not a list of non-empty strings), stops the popup with a message instead of falling back to the defaults.

## State

Each Herdr session has its own state file in the plugin state directory: `state-<tag>.json`, where `<tag>` is the first 12 hex characters of the SHA-256 of the session's `HERDR_SOCKET_PATH`, so a write, prune or lock in one session never touches another's; without `HERDR_SOCKET_PATH` the file is `state.json`. It holds the current sort, grouping, match mode, preview and wrap choices, the manual order, and the saved priorities: the ones the popup set plus every in-range priority token the popup or a CLI act saw on a live pane. An agent is keyed by its agent session id when it has one, so its manual order survives a Herdr restart that renumbers panes; other panes are keyed by pane id. Entries of panes that are gone from `herdr pane list` are dropped on every change made outside the popup, where gone means no pane in the list at all, agent or not, has that key. An empty pane list never prunes, since no panes at all is not evidence that every saved pane is gone. The popup works on a snapshot, so it never drops anything. Concurrent writers of one file are serialized by an exclusive lock on its `.lock` sibling, which the operating system releases when its holder exits.

## Limits

- Only panes with an agent are listed, and only those of the Herdr server the popup runs on. `herdr pane list` covers one server; agents on other machines are not merged in.
- Herdr can focus a pane by id only through `herdr agent focus`, which also marks a done agent as seen, as looking at it in Herdr does. When that fails (the agent exited after the list was taken), the popup focuses the pane's workspace and tab instead.
- `alt-up` / `alt-down` trade the manual positions of the selected agent and its neighbor in the same group; the rest of the manual order stays. Under a sort where `manual` is only a tie-breaker, the swap shows only between agents that tie on the earlier keys.
- The `alt-*` keys need the terminal to send Option/Alt as Meta (macOS Terminal.app: Settings → Profiles → Keyboard → "Use Option as Meta key"), or move `up` / `down` / `clear` to other keys in `keys` (`alt-1` … `alt-9` are fixed).
- A priority token cleared from outside the plugin cannot be told apart from one Herdr dropped at a restart, so the saved priority stays and the token is set again on the next restore. To really clear a priority, use `alt-0` or `act clear` in the plugin, or report `prio=0` from the pane itself.
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
