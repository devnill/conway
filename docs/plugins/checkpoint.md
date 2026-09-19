# `conway.checkpoint`: roll back the model's own file changes

Shipped by `crates/conway-plugin-checkpoint`, in the default opinion set. Depends
on [`concepts.md`](concepts.md) for vocabulary — this page covers only what is
specific to this one plugin: what it snapshots, what it does not, and exactly
how a rollback treats a hand edit you made yourself.

## What this is for

`conway.history`'s `/conway.history.rewind <seq>` can rewind the
CONVERSATION to any point without destroying anything — but nothing snapshots
the FILES a turn touched. Before this plugin, an operator whose model edited
the wrong file, or took a right one too far, had exactly one recovery: git,
which cannot tell which uncommitted edits were theirs and which were the
model's. `conway.checkpoint` snapshots a file's bytes around every
`write`/`edit` tool call, and gives you three commands to preview and undo
what changed.

**This reverses a written ruling, on purpose.** `docs/vision/CATALOGUE.md`
used to name filesystem checkpointing as something git already solves and
this project would not build. The operator revisited that on 2026-09-07,
applying this project's own convergence test to several independent coding
harnesses that all ship some form of file checkpoint — most of them ON TOP of
git, not instead of it — and ruled: build it, as a plugin, removable like
every other one. `docs/vision/CATALOGUE.md` records the reversal and its
cost alongside the amendment; it is not left as a doc that still argues
against a capability that ships.

## What it captures, including the first edit of a session

Every successful `write`/`edit` tool call is snapshotted, including the
FIRST time a session touches a given path: the plugin's observer reads the
target path's real bytes just before the write runs
(`ToolObserver::before_tool_call`, which fires after the permission decision
allows the call and before the tool executes) and again right after the
call lands, and stores both, content-addressed, in a shadow store under
`.conway/checkpoints`. It reads whatever the tool call's own arguments
name, without re-deriving the tool's output — so this works identically
whether `edit` replaced one line or `write` replaced the whole file.

From a path's second touch in a session onward, the "before" bytes are the
bytes this plugin already captured as that path's most recent earlier
"after" — a chain, needing no extra read. `/conway.checkpoint.list`/`.diff`
still report a baseline as "unavailable", never a guessed-empty one, for
the genuinely unrecoverable remainder: a path this plugin could not read at
all (e.g. a permissions error) when it tried to capture a baseline.

## `bash` is not captured

This plugin observes exactly two tools: `write` and `edit`. A file changed
through `bash` — `sed -i`, a redirect, anything else with shell access — is
invisible to it: no snapshot exists to roll back to. This is the same
disclosed limit Claude Code's own checkpoints carry, for the identical
reason: there is no seam here (or there) that watches what an unconstrained
shell subprocess actually touched. If a turn ran `bash` at all, treat this
plugin's coverage of that turn's file changes as partial, not complete.

## Three commands

- **`/conway.checkpoint.list`** — every snapshot this session has recorded,
  by seq and path. A rollback is itself listed here (see below), so you can
  see the full history of both edits and undos. An empty listing names the
  session id it answered for, because "no snapshots" is always a statement
  about one session — see [From a shell, after the session is
  gone](#from-a-shell-after-the-session-is-gone).
- **`/conway.checkpoint.diff <seq>`** — a unified diff, per touched path, of
  what `rollback <seq>` would change. Read-only; never writes anything.
- **`/conway.checkpoint.rollback <seq> [<path>] [--all] [--rewind]`** —
  restores every path touched at or after `<seq>` to the state it had
  immediately before that (or only `<path>`, when given).

`<seq>` is the same sequence-number space `/conway.history.rewind` and the
TUI status line's `session <id>@<seq>` field already use — an edit's own
seq, or a prior rollback's own seq (rollbacks get one too, see below).

## From a shell, after the session is gone

Every command above also works as a bare subcommand — `conway
conway.checkpoint.list`, no leading `/` — and that is the shape you reach
for after killing a worker. **On its own it will not find what you want.**
This store is keyed by session id, and a bare invocation has no session, so
conway mints an empty one for the duration of the command; the listing is
then a truthful report about a session that was born two milliseconds ago.

Name the session instead. `--session <id-or-name>` comes **first**,
immediately after the subcommand word, and accepts a session id or a
`conway sessions name` name:

```
conway sessions list
conway conway.checkpoint.list --session 01K7Z...
conway conway.checkpoint.diff --session 01K7Z... 42
conway conway.checkpoint.rollback --session 01K7Z... 42
```

A session id or name that resolves to nothing is an error (exit 2) — this
flag never creates a session, precisely because a silently-created empty
one is what made the un-targeted listing misleading in the first place.

**This is also how you address one agent.** conway writes one session per
agent, so a delegated worker's snapshots are already filed separately, under
the worker's own session id — `conway sessions list` shows it with an
`ORIGIN` of `spawn@<seq> <parent>`. There is no separate per-agent flag
because there is no per-agent store to point one at: the session id *is*
the agent.

Position is enforced rather than guessed: a plugin command's arguments are
free text, handed through verbatim, so conway can only tell its own flag
from the command's by where it sits. `--session` anywhere but the front is
an error naming this rule, never silently passed along. The root
`conway --session <id> ...` spelling reaches plugin subcommands too and
means the same thing; giving both spellings at once is a usage error
naming both values, rather than one silently winning.

### Which way round `diff` reads

`diff <seq>` previews the **rollback**, not the write that produced the
snapshot. The `-` side is the file as it stands right now; the `+` side is
what `rollback <seq>` would leave behind. Both headers say which is which,
so the direction is legible from the output alone:

```
/conway.checkpoint.diff 4
  --- /work/notes.txt (current)
  +++ /work/notes.txt (after rollback to seq 4)
  @@ -1,1 +1,1 @@
  -MODEL-WROTE-THIS
  +ORIGINAL-BYTES
```

Read that as "run the rollback and the `-` lines become the `+` lines" —
the same orientation `diff -u before after` has for any operation you are
about to perform. For a path the model *created*, the whole file shows as
`-` lines and nothing arrives: rolling back deletes it, and the output says
so in as many words.

## Rollback preserves a hand edit, by default

A rollback is not a naive overwrite. For every path it touches, it checks
three things: the snapshot it is about to restore, the bytes conway itself
last wrote there, and whatever is on disk right now. If the last two match,
nothing has happened to the file since conway's own last write, and the
restore proceeds. **If they differ — you edited the file yourself, by hand,
since conway's last write — that is a conflict**, reported per path, and the
file is left untouched. Nothing is silently overwritten. Pass `--all` to
force the restore anyway, discarding the hand edit.

**A rollback snapshots the CURRENT state before it writes anything, so a
rollback is itself undoable.** It is recorded as its own entry — visible in
`/conway.checkpoint.list`, and reachable by a later
`/conway.checkpoint.rollback <that rollback's own seq>`, which undoes it.

## Composing with `/conway.history.rewind`

Restoring files and rewinding the conversation are two different actions
until you ask for both. `/conway.checkpoint.rollback <seq> --rewind`
performs the ordinary rollback and then asks the host to fork the
conversation at the SAME seq — files and conversation land at the same
point together. Because a command can only ever answer one way, `--rewind`
does not show you the restore report; if you want to see that first, use
the two-command recipe instead:

```
/conway.checkpoint.rollback <seq>
/conway.history.rewind <seq>
```

## Two bounds, both with a visible notice

- **Per-file**: a file larger than 2 MiB (`DEFAULT_MAX_SNAPSHOT_BYTES`) is
  never snapshotted at all. The tool call itself still succeeds; only the
  checkpoint is skipped, with a note in the transcript naming the file and
  the bound.
- **Per-project**: the shadow store's total size is held to 256 MiB
  (`DEFAULT_MAX_PROJECT_BYTES`), shared across every session under the same
  `.conway/checkpoints` root. When a new snapshot would push the total over
  the bound, the OLDEST snapshots are evicted first, with a note naming how
  many. An evicted snapshot is never silently treated as available — a
  `diff`/`rollback` against it reports plainly that its content is gone.

Both are constructor parameters (`CheckpointPlugin::with_bounds`) for an
embedder that wants different numbers; the CLI's own default install uses
the two constants above.

## What this does not build

- **No git operations of any kind.** No stash, no commit, no write to
  `.git` — the shadow store is conway's own, entirely separate from your
  repository.
- **No automatic rollback, ever.** Nothing about a failed edit, a crashed
  session, or any other error triggers a restore on its own. Restoring is
  always the operator's own typed command.
- **No coverage of `bash`-driven changes** — see above.

## Installing it

```json
{ "plugins": { "install": ["conway.checkpoint"] } }
```

In `conway-cli`'s default opinion set — a fresh operator gets it
unprompted, and can remove it like any other opinion.
