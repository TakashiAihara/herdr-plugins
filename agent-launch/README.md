# agent-launch

Start an agent on a new task without making the pane yourself: press a key, type the task in a popup, and the plugin opens a new workspace in the directory of the pane you were in and runs your agent command there.

## Install

```bash
herdr plugin install TakashiAihara/herdr-plugins/agent-launch
```

Requires Herdr 0.9.5 or later and `bash`.

Herdr does not let a plugin bind keys, so add one to your Herdr config:

```toml
[[keys.command]]
key = "prefix+n"
type = "plugin_action"
command = "takashiaihara.agent-launch.open"
description = "launch an agent"
```

## What it does

1. The key remembers the directory of the focused pane and opens a small popup.
2. You type the task and press `Enter`. Before that, an empty line, `Ctrl-D` or `Ctrl-C` closes the popup without doing anything.
3. The plugin creates a workspace in that directory, labelled with the first 24 characters of the task, focuses it, and runs the command in its pane.

By default the command is `claude {prompt}`: Claude Code starts with your task as its first prompt.

## Command

Put a one-line template in `command` in the plugin's config directory (`herdr plugin config-dir takashiaihara.agent-launch`):

```bash
printf '%s\n' 'claude --model opus {prompt}' > "$(herdr plugin config-dir takashiaihara.agent-launch)/command"
```

- `{prompt}` is replaced by the task as one single-quoted word, so quotes, `$` and backquotes in it reach the command as typed.
- `{repo}` is optional. When the template has it, the popup asks for a repo first; what you type is passed as one quoted word, and an empty answer leaves the placeholder out. Use it with a launcher that takes a repo, for example a shell function that clones it and starts the agent in the clone: `mylaunch {repo} {prompt}`.
- The command is typed into the new pane's shell, so shell functions and aliases from your shell's startup files work.

## Limits

- A directory whose path Herdr has to escape in JSON (one with `"` or `\`) is refused with an error.
- The workspace starts in the focused pane's directory as Herdr reports it (`cwd`), not the foreground program's.
- A failure (no focused pane, `workspace create` returning nothing) stops before anything runs; in the popup the message stays until you press `Enter`, and it shows up in `herdr plugin log list --plugin takashiaihara.agent-launch`.
- It reads Herdr's JSON with a pattern for the 0.9.5 field order. If that changes, the plugin stops with an error rather than guessing.

## Test

```bash
bash agent-launch/tests/launch_test.sh
```
