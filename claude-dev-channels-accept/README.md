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
2. the visible screen contains `WARNING: Loading development channels`
3. the last three non-empty lines on screen are the dialog's own: `❯ 1. I am using this for local development`, `2. Exit`, `Enter to confirm · Esc to cancel`

Only then does it send `Enter`, which picks the highlighted first option. A tool-permission prompt or question puts its own options on the bottom lines, so it is left for you even when the dialog's words are still visible higher up (for example after Claude printed this README).

`Enter` is sent once per pane and foreground process group, so a repeated status event for the same dialog does not answer the next prompt too. When `claude` is the pane's foreground job (started from the shell, or `exec`'d by a launcher), each start is a new group and is answered again.

## Limits

- It only works inside Herdr. A `claude` started in a plain terminal still shows the dialog.
- A launcher that keeps running and starts `claude` as its child more than once in the same job keeps one process group, so within a day only the first dialog is answered.
- Every failure (a missing state directory, an unexpected screen, a pane too narrow for the dialog's lines to fit unwrapped) leaves the dialog for you. A failed `send-keys` is retried on the next status event, if one comes. Failures exit non-zero and show up in `herdr plugin log list --plugin takashiaihara.claude-dev-channels-accept`.
- It matches the dialog's English text and the event layout of Herdr 0.9.3. If either changes, the plugin stops answering and the dialog waits for you, as it would without the plugin.

## Test

```bash
bash claude-dev-channels-accept/tests/hook_test.sh
```
