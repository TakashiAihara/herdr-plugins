# session-resume

Find a Claude Code session and pick it up again, including one whose pane is long gone: press a key, search your sessions in a popup, and press `Enter`. If the session is running in a Herdr pane the plugin jumps to that pane; otherwise it opens a new workspace in the directory the session started in and resumes it there.

## Requirements

- Herdr 0.9.5+
- Linux or macOS (x86_64 or arm64). Installing downloads a prebuilt binary; nothing else is needed at runtime. On another platform, or when the download fails, it builds from source and needs a Rust toolchain (1.89 or newer).
- Claude Code transcripts under `~/.claude/projects` (or `$CLAUDE_CONFIG_DIR/projects`)

## Install

```bash
herdr plugin install TakashiAihara/herdr-plugins/session-resume
```

Herdr does not let a plugin bind keys, so add one to your Herdr config:

```toml
[[keys.command]]
key = "prefix+r"
type = "plugin_action"
command = "takashiaihara.session-resume.open"
description = "resume a session"
```

## What it does

1. The key opens the popup. Sessions are listed at once, newest activity first, and stopped ones are included.
2. Type to narrow the list. A query is words separated by spaces; a session matches when every word appears (case does not matter, Japanese works) somewhere in what the current scope searches. Different words may match in different places.
3. `Tab` widens the scope and `Shift-Tab` narrows it. The row you are on shows what matched when the match was in the conversation, with the text around it.
4. `Enter` resumes the selected session (see below).

The list is always ordered by last activity; how well a session matches does not change its place.

## Scopes

| Scope | What a word can match |
|---|---|
| `meta` | the session's title, its first prompt, its directory, its git branch and its id |
| `+user` | all of the above, and the prompts you (or a script calling `claude -p`) gave Claude |
| `+assistant` | all of the above, and Claude's replies (its thinking is not searched) |
| `+tools` | all of the above, and the tool calls Claude made (commands and other inputs), their results, and text Claude Code inserted on its own (skill bodies, background task notifications, the summary a compaction leaves) |

- The title is the one you set with `/rename` if any, otherwise the one Claude Code generated; a session with neither shows its first prompt, then its id.
- Sessions that ran as a subagent of another session are not listed or searched.

## Resuming

- If Herdr shows a pane running that session (by the session id it detected), `Enter` focuses that pane. No workspace is created.
- Otherwise the plugin creates a workspace in the session's original directory (labelled with the first 24 characters of its title), types the `resume` command into its pane, and focuses it. The command is read from the config below; the default is `claude --resume {id}`.
- If the session's directory no longer exists, nothing is created and the popup says which directory is missing.
- The running check uses Herdr's own detection. A session open in a pane Herdr did not detect as Claude, or in a terminal outside Herdr, is not seen as running, and `Enter` starts it a second time.

## Keys in the popup

| Key | Action |
|---|---|
| type, paste | edit the query (a paste is one line: line breaks and other control characters become spaces) |
| `Left` / `Right`, `Home` / `End`, `Ctrl-A` / `Ctrl-E`, `Ctrl-U` (delete to the start), `Backspace`, `Delete` | edit |
| `Up` / `Down`, `Ctrl-P` / `Ctrl-N` | move one row |
| `PageUp` / `PageDown` | move ten rows |
| `Tab` / `Shift-Tab` | next / previous scope (it wraps) |
| `Ctrl-T` | cycle the marker filter `all`, `unmarked`, `marked` (does nothing without a `marker`) |
| `Enter` | resume the selected session |
| `Esc`, `Ctrl-C`, `Ctrl-D` | close |

A row shows, from the left: the marker (when configured), `●` when the session is running, how long ago it was last active (`5m`, `3h`, `2d`, `4w`), the last two components of its directory, and its title. The area below the list shows the id, directory, branch, last activity (in UTC), first prompt, and the match.

## Markers

A marker is a file whose existence means "this session is set aside" (for example, finished). Set `marker` to a path with `{id}` in it, and sessions whose file exists get a `✓` and can be filtered with `Ctrl-T` or started filtered with `show`. The plugin only reads the marker; it never creates or removes one.

## Configuration

`config.json` in the plugin's config directory (`herdr plugin config-dir takashiaihara.session-resume`). Every key is optional:

```json
{
  "resume": "claude --resume {id} --model opus",
  "projects": "~/.claude/projects",
  "marker": "~/.local/state/my-sessions/{id}/done",
  "show": "unmarked",
  "scope": "meta"
}
```

- `resume`: the command typed into the new pane's shell, so shell functions and aliases from your startup files work. Default `claude --resume {id}`.
- `projects`: where the transcripts are. Default `$CLAUDE_CONFIG_DIR/projects` when that variable is set, otherwise `~/.claude/projects`.
- `marker`: a path template that must contain `{id}`.
- `show`: the marker filter the popup opens with, `all` (default), `unmarked` or `marked`. Anything but `all` needs `marker`.
- `scope`: the scope the popup opens with, `meta` (default), `user`, `assistant` or `tools`.
- A leading `~/` in `projects` and `marker` is your home directory.
- Without `config.json` the defaults apply. A `config.json` that exists but is not valid (bad JSON, a wrong type, an unknown key, a bad value) stops the popup with an error naming the problem; it never falls back to the defaults.

In `resume`:

- `{id}` is replaced by the session id as one single-quoted word.
- `{cwd}` is replaced by the session's directory as one single-quoted word.
- Do not put quotes around either placeholder yourself.
- Text that came from a value is never scanned again. A command that renders to nothing creates nothing.

## The index

Searching the conversation does not read the transcripts on every popup: the plugin keeps the text it extracted in `index/` under its state directory (`$HERDR_PLUGIN_STATE_DIR`, which Herdr sets for the plugin; otherwise `$XDG_STATE_HOME/herdr-session-resume` or `~/.local/state/herdr-session-resume`), per session.

- The popup opens without waiting. Sessions whose index is current appear immediately; changed or new ones are read in the background, with `indexing 12/40` in the header, and join the list as they finish.
- Transcripts only grow, so only the bytes added since the last read are parsed. A line Claude is still writing is read the next time, once it is complete. A transcript found shorter than at the last read is read again from the start.
- The conversation text of a scope is loaded the first time you widen the scope to it, so `meta` never reads the large files.
- `session-resume index` brings the whole index up to date without a popup and prints how many sessions it lists, how many it had to read, and the time it took. Run it from a timer to keep the first popup fast.
- The index holds the text of your conversations, so its directory is readable only by you (mode `0700`, files `0600`). It is rebuilt when the plugin changes the format, and the files of a session whose transcript is gone are removed. Its size is a small fraction of the transcripts' (about 6% on one set of several hundred transcripts, whose first indexing took a few seconds).

## Limits

- Claude Code sessions on this machine only.
- Transcript lines are recognised by Claude Code's compact JSON (`"type":"user"` and so on); lines written another way are skipped.
- A match is shown in lowercase, because the index stores lowercase text.
- Times are shown in UTC.
- A popup and `session-resume index` running at the same time take turns writing the index (a lock file in it), so their writes do not mix. Reading takes no lock: a popup that is already open reads a session's conversation the first time a search needs it, and if that session was rebuilt in between, the popup may search a mix of its old and new text until it is opened again.
- A transcript that was cut and then grew past its old size before the index looked at it again is not noticed: the index keeps its old text and adds what it reads after the old end.
- Text that came from a transcript is stripped of control characters before it is drawn or used as a workspace label.
- A failure in the popup (an invalid config, a projects directory that is not there, `workspace create` returning nothing, `pane run` failing) is shown there until you press `Enter` or `Esc` (or `Ctrl-C`), and the popup exits non-zero. A failure before the command is sent closes the workspace already created for it. A `herdr pane list` that fails when the popup opens is shown under the list and nothing is marked as running; when it fails at `Enter`, nothing is resumed.

## Development

```bash
cargo build --release
cargo test
bash tests/open_test.sh     # the binary against a fake herdr and transcripts made on the spot
bash tests/fetch_test.sh    # scripts/fetch-or-build.sh against a local release
bash tests/publish_test.sh  # the binaries workflow's publish step against a fake gh
```

`.github/workflows/session-resume-binaries.yml` builds a binary per platform for every `session-resume/` tree that reaches main and publishes it to the `session-resume-binaries` release, named by the tree hash. `scripts/fetch-or-build.sh`, the plugin's build step, downloads the one for the installed tree and checks its SHA-256.
