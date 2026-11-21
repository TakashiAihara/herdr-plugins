# herdr-plugins

[![test](https://github.com/TakashiAihara/herdr-plugins/actions/workflows/test.yml/badge.svg)](https://github.com/TakashiAihara/herdr-plugins/actions/workflows/test.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Plugins for [Herdr](https://herdr.dev), the terminal multiplexer for coding agents. Each directory is one self-contained plugin; install only the ones you need.

They come from running many Claude Code sessions side by side in Herdr: each plugin removes one manual step that would otherwise interrupt every session, and does it from Herdr's own hooks, actions and panes instead of a wrapper around `claude`.

## Plugins

| Plugin | What it does | Herdr | Platforms |
|---|---|---|---|
| [claude-dev-channels-accept](claude-dev-channels-accept/) | Answers Claude Code's development-channels warning so `claude --dangerously-load-development-channels` starts without a wrapper (`expect` and the like) | 0.9.3+ | Linux, macOS |
| [agent-sort](agent-sort/) | Switches the sort of Herdr's Agents view between presets from a config file, mixing your own pane tokens with Herdr's built-in fields, from a key or a popup (prebuilt binary, no runtime dependencies) | 0.9.5+ | Linux, macOS |
| [agent-launch](agent-launch/) | Type a task in a popup and start an agent on it in a new workspace, in the directory of the pane you were in, with a command template of your own | 0.9.5+ | Linux, macOS |
| [agent-search](agent-search/) | A popup that jumps to an agent, searching every pane field and token, and sorting, grouping, prioritizing and reordering the list (prebuilt binary, no runtime dependencies) | 0.9.5+ | Linux, macOS |

Each plugin's README covers what it matches, when it acts, and where it deliberately leaves things to you.

## Install

```bash
herdr plugin install TakashiAihara/herdr-plugins/<plugin>
```

Pin a version with `--ref <tag-or-commit>`. Manage installed plugins with the usual Herdr commands:

```bash
herdr plugin list
herdr plugin update
herdr plugin uninstall <plugin-id>
herdr plugin log list --plugin <plugin-id>
```

## Design

- One problem per plugin. No shared runtime, no daemon, no dependency between plugins.
- Nothing to install besides the plugin. Hooks are plain `bash`; a plugin with an interactive UI ships a prebuilt single binary (Rust, ratatui, like Herdr itself) and builds from source only when no binary matches.
- Fail closed. When the screen or the event does not look exactly as expected, the plugin does nothing and leaves the pane as it would be without it. Failures exit non-zero so they show up in `herdr plugin log`.

## Layout

```text
<plugin>/
├── herdr-plugin.toml   # id, min_herdr_version, platforms, entrypoints
├── *.sh or src/        # the hook scripts, or the Rust crate of a plugin with a UI
├── README.md           # behavior and limits
└── tests/*_test.sh     # runs without Herdr
```

## Development

Try a working copy without publishing it:

```bash
herdr plugin link ./<plugin>
```

Run every plugin's tests, as CI does on Ubuntu and macOS (build the Rust plugins first):

```bash
cargo build --release --manifest-path agent-search/Cargo.toml
cargo build --release --manifest-path agent-sort/Cargo.toml
for t in */tests/*_test.sh; do bash "$t"; done
```

## License

[MIT](LICENSE)
