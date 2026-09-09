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

A few flags change the session the TUI starts:

| Flag | Effect |
| --- | --- |
| `--role-override <role>` | Use this role instead of `default_role` for the session. |
| `--model <backend/model>` | Pin a specific model instead of routing through a role's chain. |
| `--resume <id\|name>` | Reattach to a persisted session and open the TUI on it, with its full history drawn into the transcript. Accepts an operator-chosen name (`conway sessions name`) anywhere it accepts an id. |
| `--fork-from <id\|name>[@seq]` | Open the TUI on a **new** session branched from an existing one's log, at its current head or an explicit earlier point. Omit `--cwd` when using this flag: the forked session always inherits the parent's cwd. |
| `--continue` / `-c` | Open the TUI on the most recently WRITTEN-TO session for this project — no id to look up first. Ranked by each session's own last-write time, not when it was first created. A usage error naming `conway sessions list` if this project has no sessions yet. |

`--resume`/`--fork-from`/`--continue` are mutually exclusive with each
other and with `--session` (still **one-shot (`-p`) only** — the TUI
refuses to start if you pass it, with a usage error naming the
alternative: one-shot mode, or `--resume`/`--fork-from`/`--continue`).
Budget/prompt flags one-shot has no TUI equivalent for
(`--system-prompt`/`--append-system-prompt`, `--max-turns`/`--max-tokens`/
`--max-seconds`, `--output-schema`, `--agent`) are refused, not silently
ignored, alongside any of the three continuity flags — for the same reason
the flags themselves were: an accepted-and-ignored flag is a defect, not a
convenience.

Once the TUI is already running, [`/resume <id|name>`](#slash-commands)
does the same thing `--resume` does at startup — switches the running TUI
onto that session, history drawn, without a restart. There is no
`/fork-from` or `/continue` slash-command equivalent; those two stay
startup-only flags.

**bash is off by default.** `fs`/`subagent`/`report` are registered
automatically; bash (arbitrary shell command execution) is not, and needs a
deliberate opt-in — add `"conway.shell"` to `tools.builtin_plugins` in
`settings.json`:

```json
{ "tools": { "builtin_plugins": ["conway.fs", "conway.subagent", "conway.report", "conway.shell"] } }
```

See [`getting-started.md`](getting-started.md#enabling-bash-shell-commands)
for the full explanation.

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
prompt, `/ask`, `/trust permissions`'s preview card, `/settings`, `/help`)
are bordered; the transcript is the one thing that deliberately isn't.

## Composing input

Type your message and press `Enter` to submit. A few other keys matter
while you're composing:

| Key | Effect |
| --- | --- |
| `Enter` | Submit. |
| `Alt-Enter` or `Shift-Enter` | Insert a literal newline instead of submitting (both are bound, since some terminals don't distinguish Shift-Enter from plain Enter). |
| `Up` / `Down` | Move the cursor within a multi-line draft; once the cursor is already on the first/last line, scroll the transcript one line instead (bare arrows are also what a two-finger scroll arrives as — see "Why `Up`/`Down` scroll, not recall history" below). |
| `Ctrl-P` / `Ctrl-N` | Recall older/newer entries from your input history, unconditionally — the readline pairing, and conway's one way to reach history from the keyboard. |
| `Ctrl-W` | Delete the previous word. |
| `Ctrl-G` | Edit the current input in `$VISUAL`/`$EDITOR` (falling back to `vi`) — see "Keybindings" below. |
| `Home` / `End` | With the input box empty, jump the transcript to the top/tail instead of moving the cursor. |
| `PageUp` / `PageDown` | Scroll the transcript a page at a time. |
| `Ctrl-C` | Interrupt the current turn (or, pressed with nothing running, does nothing destructive on its own). Also abandons an in-flight `/ask`, if one is running — see below. |
| `Ctrl-D` | Quit, when the input box is empty. |

Your input history persists across sessions (`~/.conway/history`, or under
`$CONWAY_CONFIG_DIR/conway` if set) — it follows you across every project,
not just the current checkout. A pasted block is inserted as one edit, not
replayed as a flood of keystrokes.

Every key above (except `Enter`/`Alt-Enter`/`Shift-Enter`/`Left`/`Right`/
`Backspace`/`Home`/`End`/`Ctrl-C`/`Ctrl-D`) is rebindable — see
"Keybindings" below for the file, the full action vocabulary, and exactly
which keys stay fixed.

### Why `Up`/`Down` scroll, not recall history

**This is deliberate, checked against the convergence test, and kept as a
documented divergence rather than left looking accidental.**

Conway runs its TUI in the terminal's alternate screen (`EnterAlternateScreen`)
and deliberately never enables mouse capture, so the terminal's own
click-drag text selection keeps working on the transcript (the "clean-copy"
guarantee — see `view/transcript.rs`'s own doc). The cost of that choice: with
mouse capture off, a terminal's *alternate scroll* mode (DECSET 1007)
translates two-finger scroll-wheel events into bare `Up`/`Down` keypresses
while the alternate screen is active — indistinguishable from a real
keystroke. An earlier revision bound history recall to bare `Up`/`Down` and a
two-finger scroll silently recalled history instead of scrolling; moving
recall to `Ctrl-P`/`Ctrl-N` fixed it, and this section records that as
deliberate rather than as an unexplained rebinding.

The harness-convergence check this decision was re-run against (2026-08-30),
per `docs/vision/DESIGN-surface-coherence.md` §8's "several independent
harnesses, not one" test:

- **Claude Code** — bare `Up`/`Down` (or `Ctrl-P`/`Ctrl-N`) move the cursor
  within a multi-line draft first; once the cursor is on the first/last
  visual row, the SAME keys recall history next.
- **OpenCode** — ships both `input_move_up`/`input_move_down` (cursor) and
  `history_previous`/`history_next` bound to bare `up`/`down` by default
  (`tui.json`'s own defaults), the identical "cursor first, history at the
  edge" shape.
- **Pi** (`pi.dev`) — `tui.editor.cursorUp`/`cursorDown` default to bare
  `up`/`down`, documented in Pi's own reference as *"move cursor up, browsing
  older history at the top."*
- **Hermes** — its own keybinding reference does not list `Up`/`Down` at all
  for the main composer, consistent with the ordinary readline-style default
  (history recall) rather than a scroll override.

Three of four converge cleanly on bare `Up`/`Down` recalling history once the
cursor is at an edge. **The convergence is on the key, not on the mechanism
that makes it safe.** OpenCode and Pi both run a genuine alternate-screen TUI
the same way conway does, and both resolve the identical wheel-vs-keystroke
ambiguity DECSET 1007 creates by enabling mouse capture — Pi's own docs
describe implementing click-drag text selection *itself* once capture is on,
replacing the terminal's native selection rather than preserving it; Claude
Code's classic (non-fullscreen) renderer avoids the ambiguity a different
way, by not taking over the alternate screen at all, so a wheel scroll is the
terminal's own native scrollback and never reaches the application as a
keystroke in the first place. Conway's own bottom-anchored transcript
(`view/transcript.rs`'s clean-copy guarantee) chose neither: it keeps the
terminal's native, zero-implementation-cost selection and stays out of the
alternate-screen mouse-capture business entirely, which is exactly what
makes the wheel arrive as `Up`/`Down` keystrokes with no way to tell it apart
from a real one.

Matching the converged key binding without adopting the mechanism underneath
it would not be a neutral change: it would reintroduce the exact regression
an earlier revision already shipped and had to revert (a two-finger scroll
silently recalling history instead of scrolling the transcript). Building
conway's own mouse-capture-plus-selection layer to close that gap is real,
separable work with its own cost (see the follow-up note in this item's own
report), not a rebinding this page can settle on its own — so `Up`/`Down`
stays bound to scrolling, `Ctrl-P`/`Ctrl-N` stays the one way to reach
history from the keyboard, and this is recorded here as the deliberate
reason, not an accident.

## Watching a turn

While the agent is working, the status line's `activity` field is your
"is it working?" signal: a spinner plus a phrase (`⠋ thinking…`, `⠙
responding…`), live elapsed seconds, and new context tokens added this
turn. It reads `idle` between turns.

### Runway notices

The status line's `ctx%` field is for *you*. Before this, the model itself
found out its context window or a budget was running out only when it was
already cut off — the harness computed how close it was every turn, and
told only the human. conway now tells the model too: once per session it
crosses 50%, 75%, or 90% of the routed model's context window, and once
per turn it comes within 20% of a configured `[limits]` ceiling
(`max_steps`, `max_tool_calls`, `max_tokens`, or a deadline), the harness
appends a note the model reads on its next turn, e.g.:

```
runway: context window 76% full (12.4k of 16k tokens est., anthropic/claude). Fork the
remaining exploration to a child and keep only its distillate; do not accumulate large
tool results inline.
```

or

```
runway: 4 of 5 max_steps used this turn (max_steps=5). Wrap up or report now.
```

It shows up in your transcript as a notice line, the same way conway
already shows other system notes (e.g. `conway.stepguard`'s repeated-call
warning) — you see exactly what the model was told, not a summary of it.
No note is ever sent for a window whose size conway does not actually
know; the harness says nothing rather than guess.

### When a turn is cut off

`max_steps` and `max_tool_calls` are runaway-tool-loop guards *for the
current turn*, not a cap on the whole conversation: the root session is
always **keep-alive**, so tripping one of them ends only the turn that
tripped it, never the session. You'll see a notice like:

```
turn ended: max_steps=40 reached; type to continue
```

and the model sees the same thing, in its own words, as a system note it
reads before its next reply — so it knows to wrap up with whatever it has
rather than silently stopping mid-thought. Nothing else changes: the
conversation is exactly as alive as it was before the trip, and your next
prompt starts a fresh turn with a fresh `max_steps`/`max_tool_calls`
allowance. This is what "each turn independently bounded" means in
practice — a single runaway tool loop can no longer take the whole session
down with it.

### When the root session itself ends

Two budget dimensions are **session-lifetime**, not per-turn: `max_tokens`
(total tokens spent across every turn) and a `deadline` (a wall-clock
cutoff). Unlike `max_steps`/`max_tool_calls` above, tripping one of these —
or a cancel — really does end the session for good, so nothing else in the
transcript tells you when it stops responding — it would otherwise look
exactly like a hang. When that happens, a `session ended: …` notice
appears, naming the terminal reason, e.g.:

```
session ended: budget exceeded (max_tokens=100000; steps_taken=81
(this session), steps_this_turn=6 (this turn))
```

`steps_taken` (this session's WHOLE lifetime step count, every turn since
it started) and `steps_this_turn` (steps since the last turn boundary) are
shown side by side, each labelled with the scope it counts, so neither
number is ever mistaken for the other.

Tool calls appear inline in the transcript as they're proposed, run, and
finish, each tagged with its state (`proposed`, `awaiting permission`,
`running`, `done`, `failed`). A settled tool call's output is folded to its first few
lines by default, with a dim `… (+N lines, Ctrl-E to expand)` affordance;
`Ctrl-E` expands or collapses every tool entry in the transcript at once.
Reasoning traces (when the model streams them) and per-entry timestamps
are shown according to your `/settings` preferences (below).

## The permission prompt

Unless your permission mode is `plan` or `AUTO-ALLOW`, every distinct tool
call pauses for your decision. The example below is a `bash` call — see
"Starting a session" above if bash isn't enabled yet; every other built-in
tool prompts the same way:

```
┌ PERMISSION REQUIRED ────────────────────────────────────────────┐
│echo pong                                                        │
│[y] allow once  [a] allow always  [n] deny  [Esc] deny w/ feedback│
└───────────────────────────────────────────────────────────────────┘
```

The first line is the command as it would actually run (below it, not
shown above, the box also names the tool, its category, and the agent
path proposing the call). Note there is no `[p]` here: a `bash` call never
offers a pattern grant at all, at any scope — see `permissions.md`'s Limits
section for why. A structured tool (`read`, `write`, `grep`, …) offers
`[p]` instead:

```
┌ PERMISSION REQUIRED ────────────────────────────────────────────┐
│read({"path":"/etc/hosts"})                                       │
│[y] once  [a] always  [p] pattern  [n] deny  [Esc] deny w/ feedback│
│  [p] grants: any `read` call                                    │
└───────────────────────────────────────────────────────────────────┘
```

Your options:

| Key | Effect |
| --- | --- |
| `y` | Allow this one call. |
| `a` | Allow this call, and remember the decision for the rest of the session. |
| `p` | Opens a field editor over the call's structured arguments — every field starts wildcard; `space` pins the selected field to its exact value, `↑`/`↓`/`tab` move, `s` cycles the grant scope, `Enter` installs an allow rule covering future calls whose pinned fields match (unpinned fields stay wildcard) and allows this call, `Esc` cancels back to this prompt. Granting with nothing pinned is the broadest offer — any call to that tool. Never offered for a `bash` call. |
| `n` | Deny this call. |
| `Esc` | Deny this call, and tell the model to try a different approach. |
| `PageUp` / `PageDown` | Scroll a long command's own display. |

`[p]`'s field editor, project-file trust, and how grants persist and get
revoked are covered in full in [`permissions.md`](permissions.md) — this
prompt is the one place you'll meet them, but that page is where the
depth lives. One thing worth knowing here rather than only there: at
session scope, a `[p]` grant is also appended to the project's
`permissions.json` (its structured `rules` array, not the flat `allow`
list a plain pattern grant uses) — so it survives a restart the same way
any other session-scope grant does, once you `/trust permissions` if the
file wasn't already trusted. A per-agent or per-subtree grant is never
written to a file, at any scope.

### Diffs, not raw JSON

A pending `edit` or `write` call doesn't show its raw arguments as JSON —
it shows a colored unified diff of the target file: current bytes on one
side, what the call would actually produce on the other, `-`/`+` lines the
same way `diff -u`/every code-review tool shows one:

```
┌ PERMISSION REQUIRED ────────────────────────────────────────────┐
│--- src/lib.rs                                                    │
│+++ src/lib.rs                                                    │
│@@ -12,3 +12,3 @@                                                 │
│ fn greet() {                                                     │
│-    println!("hello");                                           │
│+    println!("hello, world");                                    │
│ }                                                                 │
│[y] once  [a] always  [p] pattern  [n] deny  [Esc] deny w/ feedback│
└───────────────────────────────────────────────────────────────────┘
```

The raw arguments stay reachable underneath the diff (scroll with
`PageUp`/`PageDown`, same as any other long command); a call whose diff
can't be computed (an `edit` whose `old_string` isn't in the file right
now, or an unreadable target) falls back to the raw JSON dump the same way
every other tool's prompt already renders.

Once you approve it, the settled transcript entry shows the identical
diff, folded under the same line cap and `Ctrl-E` expand toggle every
other tool's output already uses (see "Watching a turn" above and the
`/settings` menu's `tool_preview_lines` stepper below). It is computed
exactly once, right when the call settles — never recomputed against
whatever the file looks like by the time you scroll back to it.

`/diff` (above) shows the CUMULATIVE version: every file this session's
agents have touched, all in one place, each against the bytes it had the
first time this session touched it — not just the single most recent
change. `conway sessions show <id> --diff` prints the identical cumulative
diff for a completed session, headlessly (see
[`sessions.md`](sessions.md)).

The diff colors are configurable, like every other themed element:
`[tui.theme.diff_add]`/`[tui.theme.diff_del]` in `settings.json`, each an
`{fg, bg, modifiers}` table exactly like every other `[tui.theme.<name>]`
entry (default: plain green/red).

## Slash commands

Type `/` to open the command palette; it narrows live as you keep typing,
and `Up`/`Down` arrow through matches (autofilling the input, without
shrinking the candidate list).

| Command | Usage | Effect |
| --- | --- | --- |
| `/ask` | `/ask <text>` | Ask an ephemeral fork a side question — it doesn't affect the live session. While it's in flight, the `activity` status field shows `⠋ asking… Ns`, same as an ordinary turn; `Ctrl-C` abandons it (cancels the child and, if it's stuck waiting on a tool permission decision, discards that prompt too — nothing is left running). Once it answers, the reply opens in its own modal; choose to fork it into a real session, pull the Q&A into your transcript, or discard it. Pulling it in appends the question and answer to your transcript immediately, live — no restart or `/resume` needed — with a marker line naming the ask it came from, so a merged exchange is never mistaken for one you typed yourself. |
| `/agents` | `/agents` | Toggle the below-chat agent-tree panel. |
| `/settings` | `/settings` | Open the settings menu (display preferences, permission mode, and grant management). |
| `/plugin` | `/plugin` | List every plugin conway can run today — compiled-in, subprocess, and MCP — each row naming where it came from and what it contributes. |
| `/steer` | `/steer <agent> <text>` | Send a steering message to a running agent. |
| `/cancel` | `/cancel <agent> [<reason>]` | Cancel a running agent immediately — stops it and its whole subtree, but never the session itself: cancelling any OTHER agent leaves the parent session working, and cancelling the session's own root agent is refused (use `/quit` to end the session instead). The cancelled agent's row in `/agents`/`/tree` flips to `Cancelled`. |
| `/await` | `/await <agent>` | Ask to be told when a running agent finishes — the operator's counterpart to the model's `conway_await` tool. Posts an immediate notice ("awaiting `<agent>`; a keep_alive agent ends only on `/cancel`"), then keeps working — input never blocks — and posts a second notice once the agent reaches a terminal state, naming its status, summary, and how many facts/artifacts it produced. Awaiting the session's own root agent is refused (use `/quit`); a second `/await` on an agent already being awaited is refused too — one waiter per agent from this surface. |
| `/context` | `/context [<agent>]` | Show an agent's assembled context, including its preamble (see below). With no argument, shows the focused agent's context; see [the agent panel](#the-agent-panel-agents) for where to find another agent's id. |
| `/why` | `/why` | Show the last routing decision — and, after a `/model`/`/role` switch, what changed. |
| `/fork` | `/fork [--role <alias>\|--model <backend/model>] [<text>]` or `/fork [--role <alias>\|--model <backend/model>] @<agent> <directive>` | Open an interactive fork of the focused agent (inherits its context, frozen at the fork point), or fork a specific agent explicitly. `--role`/`--model` (mutually exclusive) pick the child's routing explicitly instead of inheriting the focused agent's; giving either skips the free-text classification below entirely (there is nothing left to infer once routing is explicit). Otherwise, free text is classified into a fork/spawn recipe and confirmed before anything is created. |
| `/spawn` | `/spawn [--role <alias>\|--model <backend/model>] [@<agent_def>] [<prompt>]` | Open an interactive spawned agent — a clean slate, optionally from a named agent definition; inherits the parent's role/model if none is given. `--role`/`--model` (mutually exclusive) pick the child's routing explicitly — see `/fork`'s own row for the identical mutual-exclusion and classification-skip rules. |
| `/resume` | `/resume [<id\|name>]` | With an id or a name given via `conway sessions name`, resume that prior session directly -- switches this running TUI onto it, with its full history drawn into the transcript. An id or name that resolves to nothing errors clearly, naming `conway sessions list`. Bare, opens an interactive picker over this project's own sessions instead (see below). |
| `/model` | `/model [<backend/model>]` | With an exact `backend/model` argument, switch the focused agent to a pinned model, mid-conversation. Bare, or with any other text, opens an interactive picker over every reachable model instead — no plugin required (see below). |
| `/role` | `/role <alias>` | Switch the focused agent to a different role, mid-conversation. |
| `/trust permissions` | `/trust permissions` | Opens a preview card showing the project's `.conway/permissions.json` at its current content; `[y]`/`Enter` confirms (trusting it and installing its `allow` rules for this session), `[n]`/`Esc` cancels having written nothing. See [`permissions.md`](permissions.md). |
| `/tree` | `/tree` | Print the same agent tree the `/agents` panel shows, as plain transcript lines you can scroll back to or copy — with each agent's **full** id rather than the panel's short one, since a printed line may be pasted elsewhere long after the row set on screen has changed. |
| `/diff` | `/diff` | Print the cumulative diff of every file this session's agents have edited or written so far, one `## <path>` section per file, against the bytes each file had the first time this session touched it. See "The permission prompt" above and "Diffs, not raw JSON" below. |
| `/help` | `/help` | Open a read-only keybinding reference overlay. |
| `/quit` or `/exit` | `/quit` | Exit conway. |

A message that doesn't start with `/` is sent to the model as an ordinary
prompt. An unrecognized `/command` is reported as an error rather than
sent to the model. Every command in the table above is discoverable by
typing `/` — the `/` palette is generated from the same command table this
page's own list is, so the two cannot drift apart the way they once did
(board item `01M0RW29F2ATVGCV0R8H0GQEYH`: `/trust` and `/tree` used to work
while being absent from the palette).

### `/context`: the summary header and the preamble section

`/context <agent>` (or bare `/context`, for the focused agent) lists every
segment in that agent's assembled context. A large session can carry
hundreds of segments, so the per-segment listing is preceded by a summary
header rather than making you count lines by hand (after any preamble
section, described below, when one applies):

```
context: 175,212 tok est across 183 segments
  tool result: read  48 segments  89,624tok  51%
  tool result: bash  31 segments  38,155tok  22%
  agent def `reviewer`  1 segment  1,200tok  1%
  ...
largest:
  tool result read (tc_9f2)  60,412tok
  tool result bash (tc_7a1)  22,003tok
  ...
```

The first line is the total estimate and segment count. The table below it
groups every segment by kind — a tool result is further split by which
*tool* produced it (`tool result: read` and `tool result: bash` are
separate rows, never merged into one `tool result` row), every other kind
groups on its own provenance variant alone (a `SystemNote`'s specific
reason, for instance, is never a separate row — it stays one `system note`
row regardless of which reason produced which segment). Rows are ordered by
token count, largest first. The `largest:` block that follows names the
five single biggest segments in the whole context, so a runaway file read
or an oversized tool result is visible immediately rather than found by
scanning 183 lines one at a time. If the context has no segments at all,
`/context` still shows the plain `empty context` notice it always has —
there is nothing to summarize.

If any installed plugin declares an instruction fragment (a paragraph of
guidance shipped alongside its tools, rather than a system prompt or a
directory-loaded skill), those fragments appear first, in a **preamble**
section:

```
preamble: 2 plugin-declared fragments · 700tok
  conway.trim.when-to-compose  400tok  <- conway.trim
  conway.memory.recalling      300tok  <- conway.memory
```

The source column is the point: it makes visible which plugin a paragraph
of instruction came from, so it's obvious that uninstalling that plugin
removes it too. If a fragment names a tool that isn't actually installed
for this session, its text is never sent to the model — instead the line
says so:

```
  conway.trim.when-to-compose  400tok  ⚠ names compose_path -- not installed
```

If no installed plugin declares an instruction fragment, `/context` shows
no preamble section at all — the ordinary per-segment listing (system
prompt, skills, path) is unaffected either way.

**Subagents do not get instruction fragments yet.** A forked or spawned
child agent receives none, even when it holds a tool whose plugin declares
one — the same limitation directory-loaded skills already have for child
agents. So `/context <child-agent>` shows no preamble section, and that
looks identical to a session where no plugin declares one at all. If a
subagent is mishandling a tool that its parent uses correctly, a missing
instruction fragment is a likely cause and is worth ruling out first.

### `/model` and `/role`: changing model mid-session

A cheaper model for a mechanical stretch, a larger window when the work
gets big, a different provider when one is degraded — switching is
ordinary, not exceptional, and it works while the conversation is still
running: no quitting, no `--resume`.

```
/model anthropic/claude-haiku
/role planner
```

Under the hood this forks the focused agent: the new agent inherits the
*entire* prior conversation (by reference, frozen at the switch point) and
becomes the one you're now talking to — the same interactive-fork idiom a
bare `/fork` uses, just with the child's model pinned (`/model`) or its role
changed (`/role`) instead of a directive. Nothing about *which* records are
selected changes; only the model rendering them from here does. A notice
records the switch immediately; `/why` reports the resulting routing
decision — and keeps a short session HISTORY of routing decisions, not only
the latest one, so a run of several switches (or several ordinary
fallbacks) stays reviewable, each showing what it changed (`role: planner
-> fast`, `model: X -> Y`) against the one before it.

**`/agents` does not grow one row per switch.** A run of `/model`/`/role`
switches off the same agent collapses to a single row in the `/agents`
panel — its own live tip — rather than piling up a fresh row per switch;
every fork underneath is still real and independently logged (nothing about
the session's log semantics changes), the panel just shows the switch chain
as what it is: one lineage, not several unrelated agents.

If the newly-pinned model (or the new role's own chain) cannot take the
conversation's current size, you'll see the same loud refusal an ordinary
turn's admission gate gives — naming what didn't fit — the next time you
send a message. Nothing silently falls back to the old model, and nothing
is silently trimmed to make it fit.

### `--role`/`--model` on `/spawn` and `/fork`: choosing a NEW child's routing

`/model`/`/role` above switch the *focused* agent onto a different
model/role mid-session. `--role <alias>`/`--model <backend/model>` on
`/spawn`/`/fork` are the sibling case: choosing a **new** child's routing at
the moment it is created, rather than switching an existing one afterward.

```
/spawn --role fast do these three renames
/fork --model anthropic/claude-haiku review this diff
```

The two flags are mutually exclusive — `--role` names a routing role alias
(the child's turns route through that role's own configured fallback chain,
so the operator's routing config — fallbacks, capability floors, `/why`
reasons — still applies); `--model` pins a specific `backend/model` pair
outright, bypassing routing entirely. This is the exact operator-side
parity surface for the model-invoked `conway_fork`/`conway_spawn` tools' own
`role` argument (see [`agents.md`](agents.md#a-model-tool-call)): anything a
model can do to the session's own agents through those tools, the operator
can do too, from one typed command — and, with `--model`, more, since
`--model` has no model-invoked equivalent at all (a model may only name a
role, never a raw model — see that same section for why).

An unconfigured `--role <alias>` is rejected loud, naming the alias, before
any child is created — never a silent fallback to the parent's own role. A
malformed `--model` value is reported the same way `/model <value>`'s own
malformed-value case is: as a notice, before any child is created.

#### `/model` with no argument: an interactive picker, no plugin required

Typing `/model` with nothing after it opens an interactive picker over
every reachable model — every `backend/model` pair named in any role's
`chain`, plus every model recorded in the local model-metadata file
(`.conway/models.json`, even one that no role's `chain` names) — rather than
erroring or reaching out to a provider for a live roster. `Up`/`Down` move
the highlighted option, `Enter` switches to it (exactly like typing
`/model <that pair>` yourself), `Esc` cancels with no switch at all. The
prompt line names the focused agent's own current model, so comparison
doesn't require memorizing which line was already active before you opened
the picker.

**`d` makes the highlighted model the persistent default, without leaving
the picker.** This writes the highlighted model to the *head* of the
default role's own `chain` in your global `settings.json` — the exact same
reorder the `/settings → defaults` promotion row (below) performs, through
the same writer, so there is still exactly one source of truth for "what is
the persistent default." Every other configured fallback survives, in its
previous relative order, just no longer first. The resulting chain is
echoed back in a notice. Unlike `Enter`, pressing `d` does **not** switch
this session's own running model and does **not** close the picker — the
two are independent: `d` changes what a *future* session starts on, `Enter`
changes what *this* session is running right now, and you can press either
one, or both, without reopening `/model`.

This reuses the exact same modal `ask_question` (a model-called tool)
opens, and **needs no plugin installed at all** — `conway.ui`
(`plugins.install`, opt-in and absent by default — see
[`plugins/trust-and-security.md`](plugins/trust-and-security.md)) is
unrelated to whether this picker is available; installing it changes
nothing about `/model`.

**`/model <text>` with `<text>` not itself a valid `backend/model` pair**
opens the identical picker, pre-filtered to entries whose name contains
`<text>` (case-insensitive), instead of erroring — so `/model claude`
narrows straight to every configured Claude model without first having to
see the whole list. If nothing matches, `/model` says so by name rather
than opening an unusable empty picker. A syntactically well-formed
`backend/model` pair (`/model anthropic/claude-haiku`) still switches
directly, exactly as before — filtering only kicks in for text that isn't
already a complete pair.

If nothing is configured yet (no `[backends]`, no role with a non-empty
`chain`, and no `.conway/models.json`), `/model` says so by name rather
than opening an empty picker.

**Not yet built: a live filter box inside the open picker.** Today,
narrowing the list further means retyping `/model <narrower text>` (closing
the picker and reopening it pre-filtered) — the `d` key above covers
promoting a highlighted entry to the persistent default without leaving the
picker; only in-picker live filtering remains a separate, larger piece.

#### `/resume` with no argument: an interactive picker over this project's sessions

Typing `/resume` with nothing after it opens an interactive picker over
this project's own sessions — the same set `conway sessions list` shows —
rather than erroring. Each row shows the session's name (or, for an
unnamed session, its derived auto-title — the same one-line summary
`conway sessions list`'s `NAME` column shows), its first prompt, when it
was last active, how many records its transcript holds, and any labels
attached via `conway sessions label`. `Up`/`Down` move the highlighted
row, `Enter` resumes it — the exact same mechanism a hand-typed `/resume
<id>` uses, so choosing a row from the picker and typing its id yourself
land you in an identical place — and `Esc` cancels with nothing resumed.

If this project has no sessions yet, `/resume` says so by name rather than
opening an empty picker.

This reuses the exact same modal `/model`'s own bare-argument picker
(above) does, and, like that picker, needs no plugin installed.

**Not yet built: a live filter box inside the open picker, and a key that
widens the listing to sessions under a different project's own session-root
key.** Unlike `/model`, retyping `/resume <narrower text>` does not
re-open the picker pre-filtered either — an id or a name that does not
resolve stays a plain, immediate error naming `conway sessions list` (see
the table above), so a typed `/resume` argument keeps meaning exactly one
thing: "reattach to precisely this session." A session sitting under an
old subdirectory key (see
[`sessions.md`](sessions.md#if-you-already-have-subdirectory-keyed-sessions))
is reachable only by typing its id or name directly; the picker itself
lists only this project's own session-root key.

### Plugin-declared commands

An installed plugin can contribute its own slash command — an operator-facing
capability distinct from a tool (which the *model* calls; a command is
something *you* type). A plugin command's name is always namespaced with its
declaring plugin's own id, `/<plugin id>.<command name>` (e.g.
`/conway.plugin_skeleton.ping`, the shipped worked example — install it with
`"conway.plugin_skeleton"` in `plugins.install`, see
[`embedding.md`](embedding.md)) — never a bare name, so an installed plugin
can never shadow a built-in command. A plugin command shows up in the `/`
palette exactly like a built-in one, alongside its one-line description; type
it and press `Enter` like any other command. Its argument text (everything
after the command word) is passed to the plugin verbatim.

A plugin command runs with your full privileges, the same trust posture as
every other part of an installed plugin — see
[`docs/plugins/trust-and-security.md`](plugins/trust-and-security.md#tui-slash-commands-no-permission-gate-at-all-by-design)
for exactly what that does and does not mean, including why (unlike a tool
call) there is no permission prompt: you typed it yourself. See
[`docs/plugins/hooks.md`](plugins/hooks.md) point 15 for the full contract an
author implements against, including what a command may and may not do —
it can ask the host to fork the session it was invoked from at an explicit,
already-known sequence number (`CommandOutcome::ForkSession`; this is how
`conway.history`'s `/conway.history.rewind <seq>` works, see below), and it
can ask the host to submit text as a new turn — as if you had typed it
yourself (`CommandOutcome::SubmitPrompt`; this is how a file-backed
"prompt-template" command works, e.g. `conway-plugin-skeleton`'s
`FilePromptCommand`, which reads a markdown file once and submits its body
verbatim every time you type the command) — but it can never resume,
steer, or otherwise drive a session by name, and it cannot read your
transcript to resolve free text into a sequence or a prompt itself — and
the guarantee that a slow or hanging one degrades to "no reply yet," never
a frozen terminal. A submitted prompt is never confused with something you
typed: it is attributed in the durable log as coming from the command that
produced it, and `/context`'s own provenance rendering shows the
difference.

`conway.history` (`crates/conway-plugin-history`, install it with
`"conway.history"` in `plugins.install`) ships exactly one command,
`/conway.history.rewind <seq>`: starts a new agent from that persisted
sequence number and switches you to drive it, leaving the original agent's
own log untouched. `<seq>` must be an
explicit number you already know — see the status line's `session` field
below for where to read the current one — never free text like "before the
bad edit": nothing a plugin command receives lets it read your transcript
to resolve that on your behalf (the same narrowing this paragraph just
described).

## The agent panel (`/agents`)

`/agents` opens a panel below the transcript listing every agent in the
session's tree: a status marker, the agent's **short id** (its id's first
8 characters — the same truncation the status line's `session`/`lineage`
fields already use), a label, and how it was created (`fork @seq N`,
`@agent_def`, `(inherit)`, with `(ephemeral)` for an in-flight `/ask`). A
child created with `/spawn --role <alias>`/`/fork --role <alias>` shows
`role: <alias>`; one created with `--model <backend/model>` shows `model:
<backend/model>` instead, right alongside the recipe label. This only
covers what YOU typed at this command: a child another agent created via
the model-invoked `conway_spawn`/`conway_fork` tools' own `role` argument
shows no such marker here (its row still shows the ordinary fork/spawn
recipe). The currently focused agent's row is tagged `(focused)`. The short id is the
one thing this panel shows that `/context`/`/steer`/`/cancel`/`/await`/`/fork
@<agent>` actually accept as an argument — a plain label is not unique (several
agents can share one, and any agent spawned with no `agent_def` renders
the same literal `agent`) and is never matched against those commands'
own `<agent>` argument. A short id is not guaranteed unique either — two
agents created within about a second of each other would otherwise
share one, so the panel lengthens it until it is unambiguous — and naming one
that turns out to be ambiguous is reported as an error listing every
candidate, never silently resolved to the wrong agent. `v` cycles which
agents are shown (all, finished-only, active-only); `Esc` (or `/agents`
again) closes it. A row you don't want to keep watching is a good `/await`
target — its short id copied from here into `/await <id>` gets you a
transcript notice the moment that agent finishes, without staying focused
on it. The status markers:

| Marker | Status |
| --- | --- |
| `o` | Starting |
| `*` | Running |
| `?` | Awaiting permission |
| `v` | Finished |
| `x` | Failed |
| `-` | Cancelled |

## The `/settings` menu

`/settings` opens a menu of six groups: **defaults** (the default role and
the default model — see below), **display** (show reasoning traces, show
timestamps), **tool output** (how many lines a folded tool call shows
before `Ctrl-E` is needed), **permissions** (cycle the permission mode;
review or revoke individual grants under **allow** — flat and structured
alike; read-only **deny** and **prompt** sections listing every rule —
flat or structured — that any permissions file, trusted or not, has put in
force, each with the file it came from; and **hooks**, a fourth, revocable
review list), **providers** (add or remove a `backends.<id>` entry — see
[`providers.md`](providers.md#managing-providers-from-the-tui)), and
**plugins** — a single shortcut row that opens `/plugin` (below); this
menu itself no longer lists plugins directly. `Up`/`Down` navigate,
`Enter` toggles a boolean, cycles the default role, expands/collapses a
group, revokes a selected grant/hook row, or opens `/plugin`,
`Left`/`Right` step the numeric tool-preview setting, `Esc` closes. The
two display toggles, the permission-mode cycle, and every revoke action
apply to this session only; the tool-preview line count persists to
`[tui.tool_preview_lines]` in `settings.json` when you step it.
Permission-mode and grant details are covered in
[`permissions.md`](permissions.md).

**Defaults, not session state — `/model` and `/role` stay top-level
commands for exactly that reason.** The `default role -- <role> (default)`
row is settable: `Enter` cycles it through every role your `[roles]`
config declares, wrapping, and writes `default_role` in your global
`settings.json` immediately — this is the role a *new* session starts on,
not the current one (change that with `/role` instead, which never
touches a file). The `default model -- <model> (default; ...)` row right
below it is read-only: it shows the head of the default role's own
`chain` — see [`routing.md`](routing.md#roles-and-fallback-chains) — and
there is no separate "default model" setting to change independently;
changing which model a fresh session starts on means changing the default
role above, or that role's `chain` in `settings.json` by hand.

**Making a session's model the persistent default.** `/model` (above)
only ever changes what *this* session is running — a fresh session still
starts on the default role's chain head, and nothing tells you the two
have diverged unless you go looking. Pressing `d` inside `/model`'s own
picker (above) promotes the *highlighted* candidate to the persistent
default directly, without leaving the picker or switching this session.
This section's own row covers the same promotion for the model *this
session is already running*, from `/settings` instead: if they have
diverged, a third row appears right under "default model": `this session is
running <model> — Enter to make it the persistent default`. Pressing
`Enter` writes that model to the
*head* of the default role's own `chain` (moving it there if it was
already a fallback further down, inserting it if it wasn't in the chain at
all) — every other configured fallback survives, in its previous order,
just no longer first. This row is a REORDER of the same `chain` the
"default model" row above already reads from, not a second, independent
setting — and it only appears at all when the session's model and the
persistent default actually differ; once they match, it's gone, because
there is nothing left to promote. (This closes the gap where switching
models mid-session with `/model` felt permanent but silently wasn't —
restarting conway would put you back on the old default with no warning
that anything had reverted.)

## The `/plugin` command

`/plugin` lists **every kind of plugin conway can run today**, in one
place, each row naming where it came from (its **origin**) and what it
honestly contributes. This is the one place to check whether an
operator-configured MCP server or subprocess plugin is actually running —
before this command existed, `/settings`' own plugins section showed only
compiled-in plugins, so a configured MCP server had no listing anywhere in
the interface.

The compiled-in half of this same table — list, on/off, `you get`/`you
lose`/`costs` — is also reachable headless: `conway plugin list`, with no
TUI in sight. See [`scripting.md`](scripting.md#conway-plugin) for the full
`list`/`install`/`remove` reference (subprocess/MCP/claude-compat rows stay
TUI-only, unchanged by that command).

Three origins exist today, grouped under their own header row (row count
included):

- **compiled-in** — a first-party plugin built into this binary, selected
  via `[plugins].install`. The only origin with a real ON/OFF switch:
  each row is a checkbox-style `[x]`/`[ ]` box, its id, and a one-line
  summary; pressing `Enter` flips it. Selecting the row opens a detail
  panel below the list with that plugin's own status plus three rows in
  the operator's own framing — **you get** (what turning it ON adds),
  **you lose** (what's different with it OFF), and **costs** (its
  ongoing cost, if any). A flip writes `~/.conway/settings.json`'s
  `plugins.install` array directly (or `$CONWAY_CONFIG_DIR/settings.json`
  when that's set) — the SAME writer, and the SAME restart-to-apply
  contract, `/settings`' own plugins section used before this command
  existed: the change applies on your NEXT restart, not immediately, and
  the footer says so.
- **subprocess** — a `[plugins].subprocess[]` entry: an operator-named
  command speaking conway's own wire protocol. Every configured entry is
  spawned unconditionally — there is no candidate set to toggle, so the
  row is read-only and says so directly on the row (`(read-only: ...)`),
  naming exactly what to edit (`settings.json`) instead. Its contribution
  is stated as the closed set of wire points a subprocess plugin may
  bridge: tools, permission policy, observation, and status.
- **mcp** — a `[plugins].mcp[]` entry: an operator-named command speaking
  MCP (JSON-RPC 2.0) as a client. Also installed unconditionally, also
  read-only here for the same reason. Its contribution is stated plainly
  as **tools only** — an MCP server can never contribute a command, a
  permission policy, or anything else its transport doesn't carry.

Neither the subprocess nor the MCP row is padded to look like a
compiled-in one: nothing is spawned just to ask it for more, and each
row's own contribution line names exactly what its own transport can
carry, no more.

This is a listing surface, not a config editor: `Up`/`Down` move,
`Enter` toggles a compiled-in row (the only kind that responds to it),
`Esc` closes. There is deliberately no way to add, remove, or reconfigure
a subprocess/MCP entry from here — edit `settings.json` by hand for
that.

The **hooks** section lists every configured `hooks.rules[]` entry whose
event can currently deny something — `pre_tool_use` (narrows a tool call)
and `prompt_submitted` (narrows a submitted prompt) — each row naming its
`id`, its event, its tool matcher (`match`, or "every call" when unset),
and where it was configured. A rule still appears here even if its
script is broken or missing: that is exactly the moment you most need to
see and revoke it, since a broken script denies everything it matches
until you do (fail-closed). Selecting a row and pressing `Enter` revokes
it for the rest of this session only — the same session-only rule every
other `/settings` toggle follows, since there is no `settings.json`
writer for hooks either. Every other hook event (`post_tool_use`,
`session_starting`, `child_spawned`, `request_assembled`,
`context_overflow`, `child_reported`) does not appear in this list: none of
them can deny a call — `request_assembled`/`context_overflow` can edit the
assembled context (append/exclude, append-only), but editing is not
denying, so there is nothing here for them to silently keep authorizing by staying
enabled — to turn one off, edit its `enabled` field in `settings.json`.

## The status line

A single line pinned to the bottom of the screen, fields separated by
`|`. Which fields render, and in what order, is configurable
(`[tui.status_line].fields` in `settings.json`); the default is:

```
session | lineage | mode | model | ctx | tokens | activity | hint
```

| Field | Shows | Notes |
| --- | --- | --- |
| `session` | `session <id>@<seq>` | The session's root agent's short id, plus its own persisted log's current head sequence once known (`@<seq>` is omitted before the first authoritative read). Always renders. The `<seq>` is what `/conway.history.rewind <seq>` (`conway-plugin-history`, if installed) takes. |
| `lineage` | `agent <id> via root → fork @seq 3 → @reviewer` | How the focused agent was created. Omitted while you're focused on the session's own root. |
| `mode` | `ready`, `awaiting permission`, `ask`, or `intent` | The TUI's current top-level state. When your permission mode isn't the default, this field also names it: `ready · plan` or `ready · AUTO-ALLOW`. `AUTO-ALLOW` is the one thing on this line guaranteed to keep showing even on a very narrow terminal — it's a genuine safety signal, and the field most likely to matter if you've forgotten you're in it. |
| `model` | `anthropic/claude-sonnet-4-6` | The focused agent's serving model. Omitted until its first turn has routed. |
| `ctx` | `ctx 42%`, or `ctx 12.3k` when the model's context window isn't known | Cumulative context-window occupancy for the focused agent, from the same resolved `(backend, model)` capability index [`conway routes explain`](routing.md#asking-why-a-route-was-chosen) reads. When the window itself is only a `floor (assumed)` — the model's own dialect declares no sourced figure, so this is not a fact about this specific model — the figure carries that same marker: `ctx 31% floor (assumed)`. A `verified` (compiled-in table, or a dialect's own documented per-provider figure), `models.json` (an operator-editable override), or `probed` window never carries it. |
| `tokens` | `1.4k tok (88% cached)`, or `1.4k tok (cache: not reported by ollama)` | The focused agent's cumulative token spend; the cached-percentage parenthetical is the prompt-cache hit rate — `cache_read / (input + cache_read + cache_write)`. **Declaration honesty**: the percentage shows whenever the backend actually reported cache figures for at least one cache-relevant token — including a genuine `0% cached` — and is omitted only when the denominator itself is 0 (no cache-relevant tokens processed yet). When the backend's wire format carries no cache field at all (e.g. Ollama's native `/api/chat` path, see [providers.md](providers.md)), the field instead shows `cache: not reported by <backend>` — a `0%` here would claim an observation the backend never made. |
| `activity` | `⠋ thinking… 12s · +45 tok` while active, `⠋ asking… 12s` while an `/ask` is in flight, `idle` otherwise | The working indicator: elapsed time and new context tokens added this turn. An in-flight `/ask` takes this field over outright (its own clock, no token figure — it's a different agent than the one this field otherwise tracks). |
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

## Keybindings

Every key conway recognizes routes through ONE dispatch table (board item
`01M1YVJ4RA5V7FF95MFRQMTQW3`), built once at startup by merging built-in
defaults with `$CONWAY_CONFIG_DIR/keybindings.json` (or
`~/.conway/keybindings.json` when that env var is unset — the same
directory `settings.json`/`history` already live in), if it exists.
`/help` always shows the EFFECTIVE bindings — defaults as overridden by
your file, never the bare defaults — and this section documents the exact
same table (`crate::tui::keybindings::ACTIONS`, in `conway-cli`).

### The keymap file

`$CONWAY_CONFIG_DIR/keybindings.json`, shaped:

```json
{
  "transcript": {
    "toggle_tool_output": ["Ctrl-O"]
  }
}
```

Top level: context name → `{ action: [key, key, ...] }`. An entry you
supply REPLACES that action's default keys wholesale — rebinding
`toggle_tool_output` off `Ctrl-E` really turns `Ctrl-E` off, not "off by
default but still there." Binding an action to `[]` disables it with no
replacement. The same key bound in two DIFFERENT contexts is fine (only
one context is ever active at a time); the same key bound twice WITHIN one
context is a load error.

Key strings: an optional `Ctrl-`/`Alt-`/`Shift-` prefix (combinable, e.g.
`Ctrl-Shift-X`), then a single character (`g`, `v`, `y`, ...) or a named
key (`Enter`, `Esc`, `Tab`, `BackTab`, `Backspace`, `Left`, `Right`, `Up`,
`Down`, `Home`, `End`, `PageUp`, `PageDown`, `Delete`, `Insert`, `Space`,
`F1`-`F12`). `Shift-Tab` and `BackTab` are the same physical key (different
terminals encode it differently) and are treated as one binding either way
you spell it.

**A load error refuses to guess.** An unknown context, an unknown action, a
key string that doesn't parse, or a same-context collision fails to load
and names the exact `context.action` entry at fault — never a silent drop
of the bad entry, and never a fallback to "whatever half of the file
parsed." A malformed file is surfaced as a visible notice at the start of
the session (and the session still runs, on plain built-in defaults).

### `Ctrl-G`: edit the prompt in your own editor

Writes the current input to a temp file, suspends the TUI (leaves the
alternate screen and raw mode, the same teardown a clean exit uses), runs
`$VISUAL` (falling back to `$EDITOR`, then `vi`), and replaces the input
with whatever the editor left behind when it exits 0. An editor that exits
non-zero, a missing temp file, or an editor that can't even be started
leaves your input UNCHANGED and shows a notice — never a silent clear. An
EMPTY result (you deleted everything and saved) also leaves the input
unchanged, rather than clearing it. Works while a turn is running — the
edit is entirely local until you submit.

### Action vocabulary

Every action below is grouped by context; `/help` (press it any time) shows
the same list with your OWN effective bindings, not these defaults.

#### `prompt`

- `prompt.open_editor` — default `Ctrl-G` — open the current input in
  `$VISUAL`/`$EDITOR` (see above).
- `prompt.delete_word_back` — default `Ctrl-W` — delete the previous word.
- `prompt.history_prev` — default `Ctrl-P` — recall the previous
  input-history entry.
- `prompt.history_next` — default `Ctrl-N` — recall the next input-history
  entry.

#### `transcript`

- `transcript.toggle_tool_output` — default `Ctrl-E` — expand/collapse all
  tool output.
- `transcript.scroll_page_up` — default `PageUp` — scroll the transcript up
  one page.
- `transcript.scroll_page_down` — default `PageDown` — scroll the
  transcript down one page.
- `transcript.cycle_permission_mode` — default `Shift-Tab` — cycle the
  permission mode: prompt → plan → auto-allow.

#### `palette`

- `palette.navigate_up` — default `Up` — move the `/` command-palette
  selection up.
- `palette.navigate_down` — default `Down` — move the `/` command-palette
  selection down.

#### `agents_panel`

- `agents_panel.scroll_up` — default `Up` — move the `/agents` panel
  selection up.
- `agents_panel.scroll_down` — default `Down` — move the `/agents` panel
  selection down.
- `agents_panel.cycle_visibility` — default `v` — cycle the panel's
  visibility filter (active / all / finished).

#### `permission_prompt`

- `permission_prompt.allow_once` — default `y` — allow this call once.
- `permission_prompt.allow_always` — default `a` — allow always, at the
  current grant scope.
- `permission_prompt.cycle_grant_scope` — default `s` — cycle the
  remembered-grant scope: session → agent → subtree.
- `permission_prompt.edit_pattern` — default `p` — narrow the grant to
  specific argument fields before allowing.
- `permission_prompt.deny` — default `n` — deny this call.
- `permission_prompt.deny_with_feedback` — default `Esc` — deny this call,
  with a typed reason.
- `permission_prompt.scroll_up` — default `PageUp` — scroll the shown
  command up.
- `permission_prompt.scroll_down` — default `PageDown` — scroll the shown
  command down.

#### `settings`

- `settings.move_up` — default `Up` — move the selection up.
- `settings.move_down` — default `Down` — move the selection down.
- `settings.activate` — default `Enter` — toggle a display setting, or
  expand/collapse a group.
- `settings.step_left` — default `Left` — step the numeric setting down.
- `settings.step_right` — default `Right` — step the numeric setting up.
- `settings.close` — default `Esc` — close the settings menu.

### Fixed, not remappable

Text-editing primitives (typing, `Backspace`, `Left`/`Right`/`Home`/`End`
cursor movement, `Enter`/`Alt-Enter`/`Shift-Enter`, bare-arrow transcript
scroll) and the two safety chords `Ctrl-C` (interrupt) and `Ctrl-D` (quit
on empty input) stay fixed. So do the ask-modal/intent-confirm/
trust-preview decision keys and the agent panel's own `Esc` (close panel,
then return to root) — none of those live in the six rebindable contexts
above. No vim mode and no chords/leader keys either — a separate item owns
vim mode; this is scoped to single-key rebinding only.

## Ending a session

`/quit` or `/exit` end the session cleanly. `Ctrl-D` does the same when
the input box is empty. Two consecutive `Ctrl-C` presses force an
immediate exit even if a turn is stuck. Quitting with an `/ask` modal open
discards that ephemeral fork first; quitting with an `/ask` still in
flight (no answer yet) abandons it the same way `Ctrl-C` does — the
child is cancelled and any pending permission prompt for it is discarded,
without waiting for it to actually finish, since the process is exiting
either way; any residue is swept up automatically on the next startup.
Quitting with a fork/spawn confirmation card open falls back to the
manual (unclassified) flow; quitting with `/trust permissions`'s preview
card open is the same as pressing `[n]` — nothing was ever trusted or
written, so there is nothing to undo. None of these leave anything
half-created behind.
