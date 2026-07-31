# Driving the TUI

This page is the reference for conway's interactive terminal UI: starting a
session, the screen layout, composing input, watching a turn, the
permission prompt, every slash command, and the status line. For
installing conway and configuring a provider, see
[`getting-started.md`](getting-started.md).

## Starting a session

Run `conway` with no `-p`/`--print` flag to get the TUI:

```console
conway
```

A few flags change which session you land in:

| Flag | Effect |
| --- | --- |
| `--session <id>` | Use (creating if new) a specific session id. |
| `--resume <id>` | Reattach to a persisted session and continue its transcript. |
| `--fork-from <id>[@seq]` | Start a new session branched from another one, optionally at a specific point in its log. |
| `--role-override <role>` | Use this role instead of `default_role` for the session. |
| `--model <backend/model>` | Pin a specific model instead of routing through a role's chain. |

`--session`, `--resume`, and `--fork-from` are mutually exclusive; with
none of them, conway starts a fresh session.

## `--cwd` and `--root`

These two flags are easy to conflate, and mixing them up is the mistake
most likely to cost you real damage — read this before you set either one.

- **`--cwd <DIR>`** sets the process's (and the root agent's own) working
  directory: where the agent *works*, and where a relative tool argument
  starts from. It is **not** a security boundary. It never limits what a
  tool call can reach — an agent given `--cwd /home/alice/project` can
  still read or write `/etc/passwd` if a tool call names that absolute
  path.
- **`--root <DIR>`** confines the root agent — and, by inheritance, every
  subagent it forks or spawns — to that directory: any tool call whose
  path argument resolves outside it is denied before your permission gate
  is ever consulted. This **is** the security boundary. A subagent can
  only narrow its inherited root further, never widen it.

Omit `--root` and the agent is **unconfined**: it can reach anywhere your
user account can reach, exactly like every invocation before this flag
existed. Set `--root` whenever you want a hard guarantee that conway
cannot touch anything outside a directory tree, regardless of what a tool
call asks for or what permission you grant it.

**When you set `--root`, also pass `--cwd` as an absolute path.** conway
must be able to verify the agent's own working directory sits inside the
root before it will start; a relative `--cwd` (or no `--cwd` at all, which
leaves the working directory at its default) can't be checked against the
root and conway refuses to start rather than guess:

```console
conway --cwd /home/alice/project --root /home/alice/project
```

## The screen

The TUI is a single column: the conversation transcript on top, an
optional on-demand agent panel below it, an input box, and a status line
pinned to the bottom. The transcript itself has no border — it renders as
plain text with no box-drawing glyphs, so selecting and copying it with
your terminal's own mouse selection copies exactly the conversation, never
chrome. The input box, the agent panel, and every modal (the permission
prompt, `/ask`, `/settings`, `/help`) are bordered; the transcript is the
one thing that deliberately isn't.

## Composing input

Type your message and press `Enter` to submit. A few other keys matter
while you're composing:

| Key | Effect |
| --- | --- |
| `Enter` | Submit. |
| `Alt-Enter` or `Shift-Enter` | Insert a literal newline instead of submitting (both are bound, since some terminals don't distinguish Shift-Enter from plain Enter). |
| `Up` / `Down` | Recall older/newer entries from your input history, when the cursor is already on the first/last line of the draft. |
| `Ctrl-P` / `Ctrl-N` | Recall input history unconditionally (works the same as `Up`/`Down` recall, without the line-position condition). |
| `Ctrl-W` | Delete the previous word. |
| `Home` / `End` | With the input box empty, jump the transcript to the top/tail instead of moving the cursor. |
| `PageUp` / `PageDown` | Scroll the transcript a page at a time. |
| `Ctrl-C` | Interrupt the current turn (or, pressed with nothing running, does nothing destructive on its own). |
| `Ctrl-D` | Quit, when the input box is empty. |

Your input history persists across sessions (`~/.conway/history`, or under
`$XDG_CONFIG_HOME/conway` if set) — it follows you across every project,
not just the current checkout. A pasted block is inserted as one edit, not
replayed as a flood of keystrokes.

## Watching a turn

While the agent is working, the status line's `activity` field is your
"is it working?" signal: a spinner plus a phrase (`⠋ thinking…`, `⠙
responding…`), live elapsed seconds, and new context tokens added this
turn. It reads `idle` between turns.

Tool calls appear inline in the transcript as they're proposed, run, and
finish, each tagged with its state (`proposed`, `awaiting permission`,
`running`, `done`, `failed`). A settled tool call's output is folded to its first few
lines by default, with a dim `… (+N lines, Ctrl-E to expand)` affordance;
`Ctrl-E` expands or collapses every tool entry in the transcript at once.
Reasoning traces (when the model streams them) and per-entry timestamps
are shown according to your `/settings` preferences (below).

## The permission prompt

Unless your permission mode is `plan` or `AUTO-ALLOW`, every distinct tool
call pauses for your decision:

```
┌ PERMISSION REQUIRED ────────────────────────────────────────────┐
│echo pong                                                        │
│[y] once  [a] always  [p] pattern  [n] deny  [Esc] deny w/ feedback│
│  [p] grants: `bash` commands starting with `echo pong`          │
└───────────────────────────────────────────────────────────────────┘
```

The first line is the command as it would actually run (below it, not
shown above, the box also names the tool, its category, and the agent
path proposing the call). Your options:

| Key | Effect |
| --- | --- |
| `y` | Allow this one call. |
| `a` | Allow this call, and remember the decision for the rest of the session. |
| `p` | Grant a reusable pattern (a prefix match — the prompt states exactly what it would cover, before you press anything). Not offered when the command isn't safe to prefix-match (a shell command containing `;`, `&&`, a pipe, or similar). |
| `n` | Deny this call. |
| `Esc` | Deny this call, and tell the model to try a different approach. |
| `PageUp` / `PageDown` | Scroll a long command's own display. |

`[p]`'s pattern grant, project-file trust, and how grants persist and get
revoked are covered in full in [`permissions.md`](permissions.md) — this
prompt is the one place you'll meet them, but that page is where the
depth lives.

## Slash commands

Type `/` to open the command palette; it narrows live as you keep typing,
and `Up`/`Down` arrow through matches (autofilling the input, without
shrinking the candidate list).

| Command | Usage | Effect |
| --- | --- | --- |
| `/ask` | `/ask <text>` | Ask an ephemeral fork a side question — it doesn't affect the live session. Answer opens in its own modal; choose to fork it into a real session, pull the Q&A into your transcript, or discard it. |
| `/agents` | `/agents` | Toggle the below-chat agent-tree panel. |
| `/settings` | `/settings` | Open the settings menu (display preferences, permission mode, and grant management). |
| `/steer` | `/steer <agent> <text>` | Send a steering message to a running agent. |
| `/context` | `/context <agent>` | Show an agent's assembled context. |
| `/why` | `/why` | Show the last routing decision. |
| `/fork` | `/fork [<text>]` or `/fork @<agent> <directive>` | Open an interactive fork of the focused agent (inherits its context, frozen at the fork point), or fork a specific agent explicitly. Free text is classified into a fork/spawn recipe and confirmed before anything is created. |
| `/spawn` | `/spawn [@<agent_def>] [<prompt>]` | Open an interactive spawned agent — a clean slate, optionally from a named agent definition; inherits the parent's role/model if none is given. |
| `/resume` | `/resume <session-id>` | Resume a prior session. |
| `/trust permissions` | `/trust permissions` | Trust the project's `.conway/permissions.json` at its current content, installing its `allow` rules for this session. See [`permissions.md`](permissions.md). |
| `/help` | `/help` | Open a read-only keybinding reference overlay. |
| `/quit` or `/exit` | `/quit` | Exit conway. |

A message that doesn't start with `/` is sent to the model as an ordinary
prompt. An unrecognized `/command` is reported as an error rather than
sent to the model. `/trust permissions` doesn't appear in the `/` palette
list above (typing it in full still works) — every other command does.

## The agent panel (`/agents`)

`/agents` opens a panel below the transcript listing every agent in the
session's tree: a status marker, a label, and how it was created (`fork
@seq N`, `@agent_def`, `(inherit)`, with `(ephemeral)` for an in-flight
`/ask`). `v` cycles which agents are shown (all, finished-only,
active-only); `Esc` (or `/agents` again) closes it. The status markers:

| Marker | Status |
| --- | --- |
| `o` | Starting |
| `*` | Running |
| `?` | Awaiting permission |
| `v` | Finished |
| `x` | Failed |
| `-` | Cancelled |

## The `/settings` menu

`/settings` opens a menu of three groups: **display** (show reasoning
traces, show timestamps), **tool output** (how many lines a folded tool
call shows before `Ctrl-E` is needed), and **permissions** (cycle the
permission mode, and review or revoke individual grants). `Up`/`Down`
navigate, `Enter` toggles a boolean or expands/collapses a group,
`Left`/`Right` step the numeric tool-preview setting, `Esc` closes. The
two display toggles and the permission-mode cycle apply to this session
only; the tool-preview line count persists to `[tui.tool_preview_lines]`
in `settings.json` when you step it. Permission-mode and grant details are
covered in [`permissions.md`](permissions.md).

## The status line

A single line pinned to the bottom of the screen, fields separated by
`|`. Which fields render, and in what order, is configurable
(`[tui.status_line].fields` in `settings.json`); the default is:

```
session | lineage | mode | model | ctx | tokens | activity | hint
```

| Field | Shows | Notes |
| --- | --- | --- |
| `session` | `session <id>` | The session's root agent's short id. Always renders. |
| `lineage` | `agent <id> via root → fork @seq 3 → @reviewer` | How the focused agent was created. Omitted while you're focused on the session's own root. |
| `mode` | `ready`, `awaiting permission`, `ask`, or `intent` | The TUI's current top-level state. When your permission mode isn't the default, this field also names it: `ready · plan` or `ready · AUTO-ALLOW`. `AUTO-ALLOW` is the one thing on this line guaranteed to keep showing even on a very narrow terminal — it's a genuine safety signal, and the field most likely to matter if you've forgotten you're in it. |
| `model` | `anthropic/claude-sonnet-4-6` | The focused agent's serving model. Omitted until its first turn has routed. |
| `ctx` | `ctx 42%`, or `ctx 12.3k` when the model's context window isn't known | Cumulative context-window occupancy for the focused agent, from `[models.metadata_path]`. |
| `tokens` | `1.4k tok (88% cached)` | The focused agent's cumulative token spend; the cached-percentage parenthetical is the prompt-cache hit rate — `cache_read / (input + cache_read + cache_write)` — and is omitted when there's no cache activity yet. |
| `activity` | `⠋ thinking… 12s · +45 tok` while active, `idle` otherwise | The working indicator: elapsed time and new context tokens added this turn. |
| `hint` | `Enter submit · Ctrl-E expand · /help · /agents to view` | A persistent reminder of the essentials. Also names the focused agent when you're off-root and `lineage` isn't part of your configured fields. |
| `git` | the current branch name | Read once at startup; omitted outside a git repo. |
| `cwd` | the session's working directory | Omitted when unset. |

On a narrow terminal, fields give up space in a fixed order rather than
being clipped mid-word: ambient chrome (`cwd`, `git`) first, then
point-in-time telemetry (`model`, `ctx`, `tokens`), then orientation
(`session`, `lineage`), then `activity`, then `hint`. `mode` is never
dropped — its own single degrade step removes the `ready`/`awaiting
permission` word and keeps only the permission-mode label, so `AUTO-ALLOW`
is the last thing standing on even the narrowest terminal that shows
anything at all.

## Ending a session

`/quit` or `/exit` end the session cleanly. `Ctrl-D` does the same when
the input box is empty. Two consecutive `Ctrl-C` presses force an
immediate exit even if a turn is stuck. Quitting with an `/ask` modal open
discards that ephemeral fork first; quitting with a fork/spawn
confirmation card open falls back to the manual (unclassified) flow —
neither leaves anything half-created behind.
