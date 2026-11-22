# agent-launch

Start an agent on a new task without making the pane yourself: press a key, pick a repo and type the task in a popup, and the plugin opens a new workspace in the directory of the pane you were in and runs your agent command there. Several agents can be configured and switched with `Tab`.

## Requirements

- Herdr 0.9.5+
- Linux or macOS (x86_64 or arm64). Installing downloads a prebuilt binary; nothing else is needed at runtime. On another platform, or when the download fails, it builds from source and needs a Rust toolchain.

## Install

```bash
herdr plugin install TakashiAihara/herdr-plugins/agent-launch
```

Herdr does not let a plugin bind keys, so add one to your Herdr config:

```toml
[[keys.command]]
key = "prefix+t"
type = "plugin_action"
command = "takashiaihara.agent-launch.open"
description = "launch an agent"
```

## What it does

1. The key remembers the directory of the focused pane and opens the popup.
2. Repo step: filter the repos your `repos` command prints and press `Enter` on one. The first row, `(this directory)`, means no repo. This step is skipped when no agent's command has `{repo}`.
3. Prompt step: type the task, press `Tab` to pick the agent, and press `Enter`.
4. The plugin creates a workspace in the remembered directory, labelled with the first 24 characters of the task, runs the agent's command in its pane, and then focuses it.

With no configuration the command is `claude -- {prompt}`: Claude Code starts with your task as its first prompt (`--` keeps a task such as `--help is wrong` from being read as an option).

## Keys in the popup

| Step | Key | Action |
|---|---|---|
| Repo | type | filter the candidates (fuzzy) |
| Repo | `Up` / `Down`, `Ctrl-P` / `Ctrl-N` | move |
| Repo | `Enter` | take the selected row. While you type, the list shows `(this directory)`, then `use "<what you typed>"` (unless it is exactly a candidate), then the fuzzy matches; the best match is selected, or the typed row when nothing matches |
| Repo | `Esc`, `Ctrl-C`, `Ctrl-D` | close |
| Prompt | type | the task |
| Prompt | `Tab` / `Shift-Tab` | next / previous agent |
| Prompt | `Enter` | launch (an empty task starts nothing and closes the popup) |
| Prompt | `Esc` | back to the repo step, or close when that step was skipped |
| Prompt | `Ctrl-C`, `Ctrl-D` | close |
| both | `Left` / `Right`, `Home` / `End`, `Ctrl-A` / `Ctrl-E`, `Ctrl-U` (delete to the start), `Backspace`, `Delete` | edit |

## Configuration

`config.json` in the plugin's config directory (`herdr plugin config-dir takashiaihara.agent-launch`):

```json
{
  "agents": [
    {"name": "claude", "command": "ccxn {repo} {prompt}"},
    {"name": "devin", "command": "ccxn --agent devin {repo} {prompt}"}
  ],
  "repos": "ghq list"
}
```

- `agents`: a non-empty list; the first is the default. `name` is shown in the popup and `command` is the template below. Without the key the single agent `claude` with `claude -- {prompt}` is used.
- `repos`: a shell command (run with `sh -c` in the remembered directory) that prints one repo per line. Its lines are the candidates exactly as printed; blank lines are dropped. It runs only when some agent's command has `{repo}`. When it fails, the popup shows the error under the list and you can still type a repo by hand.
- Without `config.json` the default above applies. A `config.json` that exists but is not valid (bad JSON, an empty `agents`, an agent without `name` or `command`, two agents with one `name`, an unknown key) stops the popup with an error naming the problem; it never falls back to the default.

A command template is typed into the new pane's shell, so shell functions and aliases from your shell's startup files work:

- `{prompt}` is replaced by the task as one single-quoted word, so quotes, `$` and backquotes in it reach the command as typed. Do not put quotes around the placeholder yourself.
- `{repo}` is replaced by the chosen repo as one single-quoted word, or by nothing when you chose `(this directory)`. Use it with a launcher that takes a repo, for example a shell function that clones it and starts the agent in the clone: `mylaunch --agent devin {repo} {prompt}`.
- Text that came from what you typed is never scanned again: a task that contains `{repo}` or `&` stays literal. A template that renders to nothing creates nothing.

### Migrating from 0.1

0.1 read a one-line template from a file named `command`. While that file is still there, the popup stops with an error naming it, so a launch never silently uses a different agent. Move it into `config.json` and delete it:

```bash
printf '{"agents":[{"name":"default","command":"%s"}]}\n' 'claude --model opus {prompt}' > "$(herdr plugin config-dir takashiaihara.agent-launch)/config.json"
```

(Edit the file by hand when the command contains a double quote or a backslash, which JSON needs escaped.)

## Limits

- The workspace starts in the focused pane's directory as Herdr reports it (`cwd`), not the foreground program's.
- Exactly one pane must be focused in `herdr pane list`; with none or several, the key's action fails and shows up in `herdr plugin log list --plugin takashiaihara.agent-launch`.
- A failure in the popup (an invalid config, `workspace create` returning nothing, `pane run` failing) is shown there until you press a key, and the popup exits non-zero. A failure before the command is sent closes the workspace already created for it. A failed `workspace focus` leaves the workspace and the running agent in place.
- The `repos` command runs once, when the popup opens, with no time limit; `Ctrl-C` stops it before the popup is drawn. The candidate list is not refreshed while the popup is up.
- A task is one line. The label is cut at 24 characters, not bytes.

## Development

```bash
cargo build --release
cargo test
bash tests/open_test.sh     # the binary against a fake herdr
bash tests/fetch_test.sh    # scripts/fetch-or-build.sh against a local release
bash tests/publish_test.sh  # the binaries workflow's publish step against a fake gh
```

`.github/workflows/agent-launch-binaries.yml` builds a binary per platform for every `agent-launch/` tree that reaches main and publishes it to the `agent-launch-binaries` release, named by the tree hash. `scripts/fetch-or-build.sh`, the plugin's build step, downloads the one for the installed tree and checks its SHA-256.
