# pane-picker

A popup for jumping to a pane, built for sessions with many panes. It searches every field Herdr reports for a pane (title, cwd, agent, status, and every metadata token), and lets you sort, group, prioritize and reorder the list. The sort, grouping, priorities and order stay between openings.

## Requirements

- Herdr 0.9.5+
- `bash`, `jq`, and `fzf` 0.71+ (for `--id-nth`)

## Install

```bash
herdr plugin install TakashiAihara/herdr-plugins/pane-picker
```

Herdr does not let a plugin bind keys, so add one to your Herdr config:

```toml
[[keys.command]]
key = "prefix+g"
type = "plugin_action"
command = "takashiaihara.pane-picker.open"
description = "pick a pane"
```

## Keys in the popup

| Key | Action |
|---|---|
| type | filter on every field |
| `Enter` | jump to the pane |
| `Esc` | close |
| `ctrl-s` | next sort |
| `ctrl-g` | next grouping |
| `alt-up` / `alt-down` | move the pane up or down (manual order) |
| `alt-1` … `alt-9` | set the pane's priority |
| `alt-0` | clear the pane's priority |

The right side shows every field of the selected pane, so you can see what is searchable and what to put in the config.

## Configuration

Optional. Write `config.json` in the directory printed by:

```bash
herdr plugin config-dir takashiaihara.pane-picker
```

Lists (`columns`, `search`, `sorts`, `groups`) replace the default list whole; objects (`keys`, `priority`) are merged key by key, so `{"keys": {"sort": "ctrl-x"}}` changes only that key. Fields are paths into one entry of `herdr pane list` (`title`, `agent_status`, `tokens.repo`). `tokens.*` means every token value. Suffix a field with `:basename` to keep the last path segment, and a sort field with `:desc` to reverse it (`cwd:basename:desc`). Typing filters on the `search` fields only. Two virtual fields exist: `prio` (the pane's priority; usable in `sorts` and `columns`) and `manual` (the order you set with `alt-up` / `alt-down`; usable in `sorts`).

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
  "priority": {"source": "manual"},
  "keys": {"sort": "ctrl-s", "group": "ctrl-g", "up": "alt-up", "down": "alt-down", "clear": "alt-0"}
}
```

- `sorts` and `groups` are cycled in the order listed; the first one is used until you switch. A sort's `by` is a list of tie-breakers. A group's `by` is a fallback chain: the first field with a value names the group, and panes with none go to `(none)`. Groups are ordered by their highest-ranked pane, so a group holding a high-priority pane comes first.
- `priority.source` is `manual` (only `alt-1` … `alt-9`) or a field holding a number, for example a token your status line reports with `herdr pane report-metadata`. A priority set with `alt-1` … `alt-9` overrides the field for that pane.
- Example: show the branch and group by workspace first.

```json
{
  "columns": ["prio", "agent_status", "tokens.branch", "title"],
  "groups": [{"name": "workspace", "by": ["workspace_id"]}, {"name": "none", "by": []}]
}
```

A config that is not a JSON object, or has a key of the wrong shape (an empty list, duplicate sort or group names, an empty key name), stops the picker with a message instead of falling back to the defaults.

## State

`state.json` in the plugin state directory holds the current sort and grouping, the manual order, and priorities. A pane is keyed by its agent session id when it has one, so its order and priority survive a Herdr restart that renumbers panes; other panes are keyed by pane id. Priorities and positions of panes that are gone are dropped on every change.

## Limits

- Only panes of the Herdr server the popup runs on are listed. `herdr pane list` covers one server; panes on other machines are not merged in.
- Herdr can focus a pane by id only when an agent runs in it (`herdr agent focus`). Any other pane is reached by focusing its workspace and tab, which lands on the pane only when it is alone in its tab.
- `alt-up` / `alt-down` trade the manual positions of the selected pane and its neighbor in the same group; the rest of the manual order stays. Under a sort where `manual` is only a tie-breaker, the swap shows only between panes that tie on the earlier keys.
- Jumping to an agent pane goes through `herdr agent focus`, which marks a done agent as seen, as looking at it in Herdr does.
- The `alt-*` keys need the terminal to send Option/Alt as Meta (macOS Terminal.app: Settings → Profiles → Keyboard → "Use Option as Meta key"), or move `up` / `down` / `clear` to other keys in `keys` (`alt-1` … `alt-9` are fixed).
- Group headers are list rows; `Enter` on one moves to the next row.
- `FZF_DEFAULT_OPTS` and `FZF_DEFAULT_OPTS_FILE` are not applied to the picker, since options such as `--multi` or `--print-query` there would break the pick.
- Panes that report the same agent session id: the first in the list keeps the session-keyed order and priority; the others are keyed by pane id while the duplicate lasts. When it ends, the remaining pane takes the session's order and priority, and what was set on it by pane id is dropped.
- The popup works on one snapshot of the pane list taken when it opens. Panes opened while it is up appear the next time.

## Tests

```bash
bash tests/picker_test.sh
```

Runs the list building, state changes, focus and the popup wiring against a canned pane list, a fake `herdr` and a fake `fzf` that records its arguments. A real fzf session is not driven.
