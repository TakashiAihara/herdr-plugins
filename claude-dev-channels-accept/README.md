# claude-dev-channels-accept

Answers Claude Code's development-channels warning for you, so a session started with `--dangerously-load-development-channels` comes up without a wrapper (`expect` and the like) sitting between the shell and `claude`.

## Install

```bash
herdr plugin install TakashiAihara/herdr-plugins/claude-dev-channels-accept
```

Requires Herdr 0.9.3 or later and `bash`.

## What it does

Claude Code shows this dialog every time it starts with development channels:

```text
WARNING: Loading development channels
...
❯ 1. I am using this for local development
  2. Exit
```

Herdr marks the pane `blocked` while the dialog is up. On every `pane.agent_status_changed` event the plugin checks, in order:

1. the pane's agent is `claude` and its status is `blocked`
2. the visible screen contains both `WARNING: Loading development channels` and `I am using this for local development`

Only then does it send `Enter`, which picks the highlighted first option. Any other blocked prompt (tool permissions, questions) is left for you.

## Limits

- It only works inside Herdr. A `claude` started in a plain terminal still shows the dialog.
- It matches the dialog's English text. If Claude Code changes the wording, the plugin stops answering and the dialog waits for you, as it would without the plugin.

## Test

```bash
bash claude-dev-channels-accept/tests/hook_test.sh
```
