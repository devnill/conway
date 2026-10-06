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

## Color themes

Every piece of chrome the TUI draws -- tool-status tags, agent-tree
markers, modal borders, the status line -- reads one named style out of a
single table (`fg`/`bg`/modifiers), never a color literal scattered at the
call site. By default that table is `system`: the plain 16 ANSI color
names, so the terminal emulator's own palette decides whether red/yellow/
green/etc. read correctly against a light or dark background -- conway
never guesses.

`tui.theme` in `settings.json` picks a different one, by name:

```json
{ "tui": { "theme": "dark" } }
```

Six built-in names: `system` (the default, above), `dark`, `light`, and
three well-known third-party palettes -- `solarized_dark`, `gruvbox`,
`nord`. `dark`/`light` pick concrete colors calibrated against a literal
dark/light background (unlike `system`, which can't -- an ANSI name's
actual RGB is whatever the terminal maps it to).

`tui.theme` still accepts the table it always has, for overriding
individual named styles -- `{"notice": {"fg": "magenta"}}`, one entry per
style, `fg`/`bg`/`modifiers` each optional. This is unchanged: an existing
`settings.json` with a `[tui.theme]` table keeps working exactly as
before. To override a few styles on top of a NAMED preset, use the
sibling key `tui.theme_overrides` instead, in the same per-style shape:

```json
{ "tui": { "theme": "light", "theme_overrides": { "notice": {"fg": "magenta"} } } }
```

`CONWAY_TUI__THEME=dark` overrides `tui.theme` from the environment, the
same `CONWAY_TUI__*` pattern every other `[tui]` setting already follows.

**Custom themes.** A file at `<config dir>/themes/<name>.json` (same
directory as `settings.json` -- `~/.conway/themes/`, or
`$CONWAY_CONFIG_DIR/themes/` if set), in the same per-style table shape, is
selectable by its own name: `"tui": {"theme": "my-theme"}` tries the six
built-in names first, then this file. An unresolvable name (no built-in,
no matching file) falls back to `system` with a startup notice naming it
-- never a refusal to start.

`/settings` → "display" → `theme` cycles through all six built-in presets
plus any custom theme file found in `<config dir>/themes/` (alphabetical,
after the six built-ins), wrapping -- the same session-only posture the
"display" group's other rows (`editor mode`, `busy input`, ...) already
have: it changes what the REST of this session renders, never
`settings.json`. Under `NO_COLOR`/`"tui": {"color": false}`, cycling still
changes the active name shown on the row, but the screen stays
colorless -- color, once disabled, stays disabled for the rest of the
session regardless of which theme is active.

**No color.** `NO_COLOR` (set at all, to anything, including an empty value
-- see [no-color.org](https://no-color.org): presence alone is the signal)
or `"tui": {"color": false}` renders
every slot without `fg`/`bg`, keeping modifiers that carry meaning on
their own: the selection highlight and the bottom status line's reverse-
video both stay exactly as visible, and `AUTO-ALLOW`'s own warning (see
"The permission prompt," below) gains an underline so it still reads as
distinct from the `plan` mode's own plain bold, now that color can no
longer carry that distinction.

## Composing input

Type your message and press `Enter` to submit. A few other keys matter
while you're composing:

| Key | Effect |
| --- | --- |
| `Enter` | Submit. |
| `Alt-Enter` or `Shift-Enter` | Insert a literal newline instead of submitting (both are bound, since some terminals don't distinguish Shift-Enter from plain Enter). |
| `Up` / `Down` | Move the cursor within a multi-line draft; once the cursor is already on the first/last line, scroll the transcript one line instead (bare arrows are also what a two-finger scroll arrives as — see "Why `Up`/`Down` scroll, not recall history" below). |
| `Ctrl-P` / `Ctrl-N` | Recall older/newer entries from your input history, unconditionally — the readline pairing, and conway's one way to reach history from the keyboard. |
| `Ctrl-A` / `Ctrl-E` | Move the cursor to the start/end of the current line. |
| `Ctrl-U` / `Ctrl-K` | Delete from the cursor to the start/end of the current line. |
| `Ctrl-W` | Delete the previous word. |
| `Ctrl-G` | Edit the current input in `$VISUAL`/`$EDITOR` (falling back to `vi`) — see "Keybindings" below. |
| `Home` / `End` | With the input box empty, jump the transcript to the top/tail instead of moving the cursor. |
| `PageUp` / `PageDown` | Scroll the transcript a page at a time. |
| `Ctrl-C` | Abort the session's current reply, in place — the session stays live, and it accepts and answers your next message normally. Pressed with nothing running, it does nothing destructive at all (just a quiet notice that a second press is what quits). Also abandons an in-flight `/ask` or `/distill`, if one is running, and denies a pending permission prompt, if one is showing — see below, and "The permission prompt" below. |
| `Ctrl-D` | Quit, when the input box is empty. |
| `@` + a few letters | Open a file-mention completion list — see "Mentioning files," below. |
| `Tab` | Inside an open mention list, insert the highlighted candidate; otherwise, complete the path-shaped word under the cursor. |

Your input history persists across sessions (`~/.conway/history`, or under
`$CONWAY_CONFIG_DIR/conway` if set) — it follows you across every project,
not just the current checkout. A pasted block is inserted as one edit, not
replayed as a flood of keystrokes.

Every key above (except `Enter`/`Alt-Enter`/`Shift-Enter`/`Left`/`Right`/
`Backspace`/`Home`/`End`/`Ctrl-C`/`Ctrl-D`) is rebindable — see
"Keybindings" below for the file, the full action vocabulary, and exactly
which keys stay fixed.

A Ctrl or Alt chord that is not bound to anything is ignored rather than
typed as its bare letter — pressing an unbound `Ctrl-X`, for instance,
does nothing, instead of inserting an `x`.

### Vim editing mode

`tui.editor_mode = "vim"` in `settings.json` (default `"emacs"`, today's
readline-shaped keys above, unchanged) turns on a modal editor for the
input box only — the transcript, the agent panel, every modal, and
`/settings` itself never gain vim keys, regardless of this setting. It is
also a `/settings` → "display" toggle (`Enter` flips it for the rest of the
session). The input box's own border title always names the current
submode (`input [INSERT]`, `input [NORMAL]`, `input [VISUAL]`, `input
[VISUAL LINE]`, or `input [/pattern]` while composing a search) so it is
never ambiguous which one you're in.

**Every Ctrl-bound action above (`Ctrl-A`/`Ctrl-E`/`Ctrl-U`/`Ctrl-K`,
`Ctrl-P`/`Ctrl-N` history, `Ctrl-O`, `Ctrl-G`, `Tab`, `F2`) keeps working
exactly as it does in `emacs` mode, in BOTH vim submodes.** Vim mode is a
modal layer on top of the input box, never a second, competing keymap —
see "Keybindings" below. The one new chord it adds is `Ctrl-R` (redo),
since nothing above already claims it.

Starts in INSERT (ordinary typing, exactly like `emacs` mode) so turning
this on mid-session never interrupts what you were typing. The supported
subset, stated in full — nothing beyond this list is implemented:

- `Esc` leaves INSERT for NORMAL. **`Esc` in INSERT never submits and
  never interrupts the agent** — `Ctrl-C` remains the only interrupt key,
  in both submodes.
- `i` `a` `I` `A` `o` `O` enter INSERT (before/after the cursor, at the
  start/end of the line, on a new line below/above).
- Motions, with counts (`3w`, `2j`, ...): `h j k l` (left/down/up/right —
  `j`/`k` move between lines of a multi-line draft, same as `Up`/`Down`
  already do), `w b e` (word forward/backward/to word-end), `0 ^ $`
  (start/first-non-blank/end of the current line), `gg G` (first/last
  line, or `{count}gg`/`{count}G` for an exact line number — the count
  here is an absolute target, never a multiplier), `f F t T` + a
  character (find/till, forward/backward, on the current line only). Any
  count — a bare count or either half of an operator+motion pair like
  `9999d9999w` — is capped at **9999**; digits typed past that ceiling are
  dropped rather than accumulated, so an implausibly large count (a typo,
  or a stray paste) can never allocate or loop far beyond what the input
  box could ever show.
- Operators `d` (delete) `c` (change) `y` (yank) compose with any motion
  above (`dw`, `c$`, `y0`, ...), plus the linewise doubled forms `dd cc
  yy` and the to-end-of-line forms `D C`. `cw` is vim's own special case:
  with the cursor on a non-blank it changes through the end of the
  current word (like `ce`), leaving trailing whitespace alone, rather
  than consuming it the way `dw`/`yw` do; on a blank it behaves like a
  bare `w`.
- Text objects, ONLY `iw aw i" a" i( a(` — reachable right after an
  operator (`ciw`, `daw`, `yi"`, `di(`, ...), never as a bare motion and
  never inside Visual mode.
- `x X` (delete the char under/before the cursor), `p P` (put the one
  unnamed register after/before the cursor or line).
- `u` / `Ctrl-R` (undo/redo).
- `.` repeats the last text-changing command, replayed against wherever
  the cursor is NOW — not a saved range.
- `v` / `V` (characterwise/linewise Visual) + `d y c`.
- `/pattern` + `Enter`: a literal (non-regex) forward search WITHIN the
  input text, wrapping around; `Esc` cancels.

**Not built, on purpose:** no `:` command line, no named registers (the
one unnamed register only — no macros either), no other text objects
(sentences, paragraphs, `ib`/`it`, ...), no `;`/`,` find-repeat. This is a
documented subset, not an emulation of vim itself.

### Typing while the agent works

Submitting a message while an agent's turn is still running has always
reached the model eventually — a durable message is appended and that
agent's own next context build picks it up — but nothing on screen used to
say so, so you could not tell whether your words were queued, lost, or sent.
Several things make that visible and configurable.

**Every queued message is tagged with the agent it was typed for, not with
whichever agent you happen to be looking at when it is finally sent.**
Switching focus (`/agents`, `/spawn`, `/fork`, `/model`, or any focus-nav
key) between queuing a message and its delivery never redirects it to a
different agent, and never strands it either — delivery is driven by polling
each agent that actually has something queued, independent of focus.

**A queued strip.** While a turn is running, a message you submit for the
FOCUSED agent appears in the input box's own border title (`"input — 2
queued: fix the bug..."`) and, at the same time, in the transcript in a dim
`queued>` style distinct from an ordinary sent message — until it is
actually delivered. A message held under `busy_input = steer` instead reads
`steer>` (and the strip says `"— 1 steer: ..."`) — it already left for the
model the instant you sent it, so it deserves a different word than a
message that is still waiting here, client-side, to be sent at all. A
message queued for a DIFFERENT, non-focused agent shows only as a bare count
(`"input — 1 queued (other agents)"`) — never its text — since you are not
looking at that conversation; switch focus to that agent to see it in full.
The strip also keeps showing while a permission prompt is on screen (the
title reads `"input (paused) — 1 queued: ..."`) — a message typed mid-turn
never reads as lost just because a DIFFERENT surface is asking you something
else right now. The count survives a focus change even though the
transcript row does not (switching away clears the whole transcript, the
same way it always has, and switching back rebuilds it from the persisted
log only — a message that was never sent was never persisted either, so it
is not reconstructed, though the count on the strip is correct the instant
you refocus that agent).

**`Up` on an empty input recalls it.** With the input box empty and at least
one message queued FOR THE AGENT YOU ARE CURRENTLY FOCUSED ON, `Up` pulls the
MOST RECENTLY queued one back into the editor for re-editing or
resubmission, removing it from the queue — a recalled message is never also
delivered. `Up` never reaches into a different agent's own queue. With
nothing queued for the focused agent, `Up` keeps its ordinary meaning (see
"Why `Up`/`Down` scroll, not recall history," below) — this is a narrow
addition to that behavior, not a replacement of it. To discard a queued
message instead of resending it, recall it with `Up` and then clear the
input the ordinary way (`Ctrl-U`, or backspacing) — `Esc` does NOT discard a
queued message (a reflex key is not a safe place for a destructive action)
and keeps its existing meanings unconditionally.

**`tui.busy_input` picks what "submitted while busy" means**, settable in
`settings.json` and adjustable for the rest of the session from `/settings`'s
"display" group (`Enter` cycles it, the same way the permission mode row
cycles its own three states):

- `steer` (the default) — delivers the message at the running turn's own
  next tool-loop step, via the same steer primitive `/steer <agent> <text>`
  already uses for a child agent (steer is bidirectional, Claude-Code-style,
  so steering the very agent you are talking to works the same way).
  **Steer never lands mid-generation** — it is folded into context no sooner
  than the model's next inference step within the current turn's tool loop,
  never while a model call is actually in flight. A message steered AFTER
  the agent's turn has already ended — it finished answering and is idling,
  waiting for you — is not left sitting unread until some later, unrelated
  message happens to remind it: it starts a fresh turn of its own,
  automatically, carrying exactly that text. (`queue` was the default
  through an earlier dogfood round; a message held under it had no visible
  difference from one that was simply lost until it was actually sent, which
  is the failure `steer` as the default closes.)
- `queue` — withholds the message entirely until the agent it was typed for
  is no longer generating a reply, then sends it, exactly like every other
  prompt. An agent that finishes outright (rather than simply going idle)
  before a queued message can be sent never receives it — a notice lists
  every message that could not be delivered, and the newest is restored to
  the input box if you are still looking at that same, now-finished agent
  and have not started typing something else.
- `interrupt` — aborts the focused agent's current turn (killing any tool it
  was mid-way through running, process group and all) and sends your new
  message into the SAME agent the instant it reports idle — the "cancel this
  reply, keep talking" mode. The session is never ended by this: it is the
  same non-terminal abort `Ctrl-C` uses, scoped to the focused agent rather
  than the session as a whole. `prompt.send_now` (default `F2`, see
  "Keybindings" below) is the per-message, one-off version of this — press it
  on any message, in any `busy_input` mode, to abort-and-send just that one
  message without changing your configured mode. Unlike a bare `Ctrl-C`, F2
  (and `busy_input = interrupt`) always resends your message once the abort
  completes, so the notice it leaves says so plainly ("sent now — the reply
  in progress was stopped") rather than the generic "type to continue" a
  budget- or operator-triggered abort with nothing queued behind it still
  uses.

A `!`/`!>` shell command or a `/`-prefixed slash command is never affected by
`busy_input` — neither is "a prompt" in the sense this setting governs, and
both still run (or open) immediately regardless of what the focused agent is
doing.

**Quitting with a non-empty queue** (`/quit` or `Ctrl-D`) is refused the
first time, with a notice naming how many messages would be discarded —
quitting again within a couple of seconds confirms it, the same two-press
shape double-`Ctrl-C` already uses to force an exit. (Double-`Ctrl-C` itself
is NOT additionally gated this way — it is already its own deliberate,
universally-understood "I mean it" signal.) The FIRST `Ctrl-C` is not
silent about it either, though: if any agent has a non-empty queue when you
press it, a notice names how many messages would be lost if you press
`Ctrl-C` again, so the most common escape gesture never discards a queue
with no warning at all.

### Mentioning files

Typing `@` at a word boundary (input start, or right after whitespace —
typing it mid-word, as in an email address, never triggers this) opens a
completion list of paths under your current working directory (or, with
`--root` set, confined to that directory — the list never names a path
outside it). Keep typing to filter it (a fuzzy, not-necessarily-contiguous
match against the relative path, best match first); `Up`/`Down` move the
highlight; `Tab` or `Enter` inserts the highlighted path (with a trailing
space, so you can keep typing — a path containing a space is quoted,
`@"release notes.md"`, and read back whole when you submit); `Esc` closes
the list without inserting anything, and typing further inside the SAME
`@`-token will not immediately reopen it.

The walk never blocks the keyboard: it runs in the background and reports
back when it finishes, so typing and scrolling keep working while it does
(a `git`-backed walk that somehow gets stuck — a contended `.git/index.lock`,
say — is itself wall-clock bounded and killed rather than left to hang).
The list shows "scanning..." for the moment that takes, then fills in once
the answer lands; if nothing has happened yet, `Tab`/`Enter`/arrows simply
have nothing to act on until it does. The walk is cached for about 15
seconds per directory, so typing several `@`-mentions in one message does
not re-walk the tree for each one. In a git repository it shells out to
`git ls-files --cached --others --exclude-standard` (the same listing `git
status` is built on, so `.gitignore` is honored for free); outside one, a
plain bounded directory walk skips `.git`, `target`, and `node_modules`.
Either way the walk is capped by candidate count (and, for the non-git
walk, by wall-clock time too) so a huge tree cannot make it take long; a
capped listing says so in its own title rather than silently looking
complete.

While composing `/fork`/`/spawn`, the identical `@` trigger instead
completes against this session's own LIVE agent ids (and names, if you've
set any with `conway.names`) — `/fork`/`/spawn @<agent>`'s existing
addressing convention, reusing the same list widget rather than a second
one (agent lookups are in-memory, so they never go through the background
walk above).

**A pasted `@`-shaped block does not let a bare `Enter` accept.** Pasting
text that happens to start with `@` at a word boundary (`@property` copied
from a stylesheet, say) still opens the completion list — but a bare
`Enter` right after submits your pasted text, rather than silently
replacing it with whatever candidate happened to fuzzy-match. Once you
arrow-navigate the list or keep typing into it, `Enter` accepts normally
again; `Tab` always accepts, paste or not, since a paste never delivers a
literal `Tab` keypress on its own.

**What the model actually receives.** An `@`-mention is sent as plain text,
exactly as typed — the model reads the file with its own tools, the same as
any other path you type by hand. Alongside it, conway appends a clearly
delimited line naming every `@`-mentioned path, so the model reads the
mention as a reference rather than ordinary prose. (There is no facade
primitive today for attaching that hint as separate turn metadata rather
than inline text, so it shows up at the end of your own message, visibly,
rather than hidden.) `@`-mentions never cause conway itself to read or send
file contents — only what the model's own tool calls fetch, same as always.

### Attaching an image

The TUI's own clipboard/drag-drop attach route is not wired yet — the
building blocks (clipboard reading, size/format validation, the
`[image #N · WxH · size]` chip) live in `conway-cli`'s `image_attach`
module, ready for the composer to call, but no key submits an attachment
into a live turn in this release. **One-shot mode already attaches images
end to end:** `conway -p "..." --image shot.png` — see
[scripting.md](scripting.md#attaching-an-image---image) for the flag and
[providers.md](providers.md#vision-attaching-an-image) for which models
actually look at it. An attached image you resume into, or review with
`conway sessions show`, renders as that same one-line chip, never the raw
bytes.

### Running a command yourself

Half the time you want to run `git status` or `ls`, you do not want to
spend tokens asking the model to do it for you — you want to type it and
see it. A line beginning with `!` runs the rest as a shell command,
without ever going through the model:

```
!git status
```

This runs `git status` with your own shell (`$SHELL -c`, falling back to
`/bin/bash -c` if `$SHELL` is unset or empty — so your aliases and profile
behave the way they would in an ordinary terminal) in the session's cwd,
and shows its combined output and exit code right in the transcript, in
its own styling (a `!` prefix on the command line, a dim output body, and
a green/red exit line) — distinct from both your own messages and the
model's. `Ctrl-C` while it runs kills the whole command (and anything it
spawned) without touching conway itself; the command and its output are
always capped to a bounded size, with a note if anything was cut, and any
raw terminal control sequence a command's own output carries (from `curl`,
or `cat` of a downloaded file, say) is neutralized before it is ever
rendered, so it cannot repaint your terminal or rewrite its title. Quitting
(`/quit`, `Ctrl-D`, or the double-`Ctrl-C` exit) while a `!` command is
still running kills it too, the same way — nothing is left behind as an
orphaned background process.

**You see it coming before you press Enter.** The moment your draft starts
with `!`, the input box's own title changes to `shell` (or `shell → model`
for the `!>` form below) so you always know you are about to run a
command, not send a message — never a silent switch sprung on you at
submit time. If you genuinely want to send the model a message that
happens to start with `!` (`!important`, say), prefix it with a backslash:
`\!important` sends the literal text `!important` — the one backslash is
stripped, nothing is executed, and the input box shows its ordinary
`input` title the whole time (the backslash form is never mistaken for a
command).

**Zero token cost, by default.** The command and its output are recorded
in this session's history (durably — it happened), but they are **never**
sent to the model, and never count toward `/context`. If you want the
model to see what you just ran — "run the tests and fix what fails" in one
line — use `!>` instead of `!`:

```
!> cargo test -p conway-core
```

This runs the command exactly the same way, and adds its output to the
model's context as an ordinary message, as if you had typed `cargo test -p
conway-core`'s output yourself. (It does cost tokens, same as any other
message — that is the whole point of the `>`.) **It does not, by itself,
spend a model turn.** The output becomes part of the conversation
immediately — it is there, durable, the instant the command finishes — but
the agent only actually reads and answers it the next time it runs a real
turn, which is your own next message (or steer). Running a command and
folding its result into context is not, on its own, something you asked the
model to respond to right now.

A bare `!` (or `!>`) with nothing after it runs nothing. `!!` repeats the
most recently run `!`/`!>` command, in whichever form it ran.

**A `!` command runs in the session's cwd at the time it STARTED** — not a
live, `cd`-tracked one. If the model has since moved its own working
directory with its `cd` tool mid-session, that move is not reflected here
yet; `!` runs where the status line's `cwd` field says the session is.

**The same `deny` rules a model-issued `bash` call would hit still refuse
a `!` command, and name the rule that refused it.** A `permissions.json`
`deny` entry targeting `bash` (`"bash:rm -rf"`, say) blocks `!rm -rf x`
exactly as it would block the model trying the same thing — deny rules are
unconditional, regardless of who is asking. `prompt` rules, the current
permission mode, and `pre_tool_use` hooks do **not** apply to `!`: all
three exist to put a human in the loop before the MODEL runs something —
and typing `!` already IS that human, so there is nothing left for them to
insert. `--root` does not confine a `!` command's string either, for the
identical reason it does not confine a model-issued `bash` call's string
(see [the permission prompt](#the-permission-prompt) and
[`permissions.md`](permissions.md)): a shell command can reach any path it
likes via redirection, `cd`, or a subprocess, so there is no finite scan
that could confine it.

`!` always runs unconfined today, even when `conway.confine`'s confined
shell is installed and you have opted into it for the model's own `bash`
tool — routing `!` through that same confinement is a disclosed
follow-up, not yet built.

There is no persistent shell session: each `!`/`!>` is one fresh process,
so `cd`ing inside one `!` command has no effect on the next. A command
that needs a real interactive terminal (a full-screen editor, a pager
without `--no-pager`, anything that reads from a tty) will hang or behave
oddly — this is not detected or refused, just not supported; redirect or
pass a non-interactive flag instead.

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

Separately from all of that, the *model's own* per-call output budget can
run out mid-turn — its `stop` came back `max_tokens` on the wire, unrelated
to this session's own `[limits] max_tokens` above. If it had already said
something, you'll see a notice that the response was cut off before it
finished (retry, or raise the model's own `max_tokens` cap). If the cap was
hit before it produced any visible output at all — no text, no tool call,
its whole budget spent on reasoning you can still read above in the
transcript — you'll see a notice that says exactly that, instead of the
transcript going quiet with no explanation: retry the turn, raise the cap,
or ask something narrower that needs less reasoning to answer.

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
lines by default, with a dim `… (+N lines, Ctrl-O to expand)` affordance
(the marker always names your EFFECTIVE key, including a rebind);
`Ctrl-O` expands or collapses every tool entry in the transcript at once.
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
│[y] once  [a] always  [p] prefix  [n] deny  [Esc] deny w/ feedback│
│  [p] proposes: "echo pong" (session only, never saved)          │
└───────────────────────────────────────────────────────────────────┘
```

The first line is the command as it would actually run (below it, not
shown above, the box also names the tool, its category, and the agent
path proposing the call). `bash` (and every other `ShellCommand` tool)
never offers the DURABLE pattern grant a structured tool's `[p]` installs
— see `permissions.md`'s Limits section for why that refusal is permanent
— but it does offer something narrower and temporary of its own: a
**session-scoped shell-prefix grant** (`[p]`), covered below. A structured
tool (`read`, `write`, `grep`, …) still offers the durable field editor
exactly as before:

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
| `a` | Arms a confirmation — see "`a` needs a deliberate second keystroke," below. |
| `p` (structured tool) | Opens a field editor over the call's structured arguments — every field starts wildcard; `space` pins the selected field to its exact value, `↑`/`↓`/`tab` move, `s` cycles the grant scope, `Enter` installs an allow rule covering future calls whose pinned fields match (unpinned fields stay wildcard) and allows this call, `Esc` cancels back to this prompt. Granting with nothing pinned is the broadest offer — any call to that tool. |
| `p` (shell command) | Opens a free-text editor seeded with a narrow default — ordinarily two tokens (e.g. `git status` from `git status --short`, never the bare `git`), governed by three invariants for a recognized interpreter/launcher or wrapper head (never ends on a flag or a bare selector when a target follows; a wrapper like `sudo`/`env` always proposes the whole command; any flag this function would otherwise have to guess the arity of also falls back to the whole command) — see "The shell-prefix grant" below for the full rule — type to widen or narrow it, `Ctrl-S` cycles the grant scope, `Enter` grants a **session-scoped, in-memory-only** prefix covering future shell commands sharing it and allows this call, `Esc` cancels back to this prompt. |
| `n` | Deny this call. |
| `Esc` | Deny this call, and tell the model to try a different approach. |
| `PageUp` / `PageDown` | Scroll a long command's own display. |
| `Ctrl-C` | Deny this call AND abort the session's current reply — see "`Ctrl-C` stops this too," below. |

**`Ctrl-C` stops this too.** Every other key above only ever decides the ONE
call on screen; `Ctrl-C` is conway's universal "stop this" gesture, and a
permission prompt sitting on screen no longer blocks it. Pressing it here
denies the pending call (the prompt closes, and the tool never runs — the
model sees a plain denial, the same as `n`) and also aborts the session's
current reply, exactly as it would with no prompt open at all (the session
stays live; see the keyboard table above). Any OTHER prompt still queued
behind this one for the SAME call's agent is denied too, rather than left to
surface next for a turn that no longer exists — a queued prompt from a
DIFFERENT agent is unaffected. The second `Ctrl-C` within the usual window
still quits, same as always.

**`a` needs a deliberate second keystroke.** The first `[a]` does not grant
anything yet — it arms a confirmation, and the prompt's own hint line
changes to say so plainly (`CONFIRM allow always? [a] or [Enter] confirms
[Esc] cancels`). Press `a` again, or `Enter`, to actually commit the
broader-than-once grant at whichever scope the prompt shows; `Esc` backs out
to the ordinary prompt with no decision made at all, as if `a` had never
been pressed. Any OTHER key disarms it too and takes its own normal
meaning — a stray `a` immediately followed by a deliberate `n` still denies.
No single keystroke can ever grant more than "allow once" by itself.

**Typed input never answers a permission prompt.** A prompt interrupting you
mid-sentence used to risk a keystroke you were still typing landing as a
decision instead — `a` from "also tell me…" granting "always" session-wide
being the sharpest version of this. A key arriving within about a second of
the prompt appearing, or while the input box already held a draft at that
instant, is treated as ongoing typing and goes into the draft instead —
never into a decision — for a window that widens automatically if you were
already mid-draft when the prompt interrupted you. `y`/`n`/`a`/`p`/`s` all
answer normally again, as single keys, once that window closes. `Ctrl-C` is
never subject to this window at all — it is never typeahead, by
construction, so it denies the pending call (see above) the instant it is
pressed, mid-sentence or not.

The guard is vim-aware: with `tui.editor_mode = "vim"` (see "Vim editing
mode," above) in NORMAL mode, a key inside the window runs as its own vim
command against the draft — an operator like `d`, a motion, or a mode
switch like `i` — rather than being inserted as a literal character, and a
pending operator (`d` waiting on its motion) survives across the window
exactly as it would mid-sentence in `emacs` mode.

A bracketed paste follows the identical rule, but with no window at all: it
lands in the draft, never as an answer, at any time the prompt is showing
— a paste is a deliberate block of text, never itself one of the single-key
decisions, so there is nothing for it to collide with.

`[p]`'s field editor, project-file trust, and how grants persist and get
revoked are covered in full in [`permissions.md`](permissions.md) — this
prompt is the one place you'll meet them, but that page is where the
depth lives. One thing worth knowing here rather than only there: at
session scope, a structured `[p]` grant is also appended to the project's
`permissions.json` (its structured `rules` array, not the flat `allow`
list a plain pattern grant uses) — so it survives a restart the same way
any other session-scope grant does, once you `/trust permissions` if the
file wasn't already trusted. A per-agent or per-subtree grant is never
written to a file, at any scope.

### The shell-prefix grant

A shell command's `[p]` is a different, narrower mechanism from a
structured tool's — not a smaller version of the same thing. A durable
prefix-pattern grant is refused outright for any `ShellCommand` tool,
permanently, regardless of what the prefix says (`permissions.md`'s Limits
section): judging a shell command means predicting what a shell will make
of a string, and conway does not do that. What `[p]` offers here instead
is a grant that only ever lives in memory, for the rest of THIS session,
and is offered so you never have to re-type the same handful of prefixes
(`git status`, `cargo test`, …) dozens of times in one long-running task.

Accepting it (`Enter`) authorizes later shell commands whose text starts
with the SAME whitespace-aligned tokens — `git status` covers `git status
--short` but not `git push` or `git statusfoo` — at whichever scope you
chose (including a spawned subagent under an `AgentSubtree` grant). It:

- **Is never persisted, at any scope.** It does not touch
  `permissions.json`; a session-scope shell-prefix grant behaves exactly
  like an agent/subtree structured grant in that one respect, even though
  its default scope reads `Session` the same way the other grants' does.
- **Is gone the moment the process exits.** A fresh `conway` run — even
  against the identical project, the identical command — starts with none
  of it; there is nothing to `/trust` and nothing to revoke on disk.
- **Is reviewable and revocable for the rest of THIS session**, board item
  `01M350FR4SM6QT0EM6M35EY5AZ`: `/settings` → permissions → "shell
  prefixes" lists every grant exactly as you granted it, with its scope.
  `Enter` on a row revokes just that one; a "revoke all shell-prefix
  grants" row clears every one. A revoked grant cannot come back on its
  own — the next matching command prompts again.
- **Is exactly as narrow as what you see on screen.** The editor never
  installs anything other than the text you left in the editor when you
  pressed `Enter` — narrow it further, or widen it, before accepting. The
  seeded default is ordinarily two tokens; board item
  `01M44PK0HNKWBXK86PWTHD6N7C` governs the exceptions with three
  invariants, replacing an earlier shape-by-shape table a follow-up review
  found holes in (a flag mixed into `env`'s own assignments still matched
  ANY later command; a flag right after `run` in `uv run --with pandas`/`go
  run -race ./cmd` got swept into the proposal instead of blocking it; an
  interpreter flag before `-m` in `python3 -u -m pytest` produced the exact
  wildcard this item exists to close):
  - **The proposal never ends on a flag, or on a launcher's own selector
    (`-m` for python, `run` for `uv`/`go`/`cargo`/`bun`/`deno`, `dlx`/`exec`
    for `pnpm`) when a target follows it.** It either extends past the
    selector to the token that actually names the target (`python3 -m
    pytest`, `uv run pytest`, `go run ./cmd`), or it does not extend at
    all.
  - **A WRAPPER head that re-executes some OTHER command verbatim without
    naming anything itself** — `sudo`, `doas`, `env` carrying any flag of
    its own, `time`, `nice`, `nohup`, `timeout`, `stdbuf`, `xargs`,
    `command`, `exec`, `caffeinate` — **always proposes the WHOLE command.**
    A plain `env NAME=VALUE ...` run (no flag of `env`'s own) is the one
    exception: the assignments are skipped when judging the command they
    precede, but stay verbatim at the front of the proposal.
  - **Any flag this function would otherwise have to guess the arity of
    also falls back to the WHOLE command**, rather than a fixed-width
    guess: an interpreter flag before the selector (`python3 -u -m
    pytest`), a flag right after a subcommand selector (`uv run --with
    pandas`, `go run -race ./cmd`, `cargo run --bin x`), or a
    script-literal (`bash -c`, `sh -c`, `node -e`) — the payload after one
    of those is an entire embedded script or arbitrary argument, not a
    nameable target.

  Every command that is neither a recognized launcher nor a wrapper keeps
  the ordinary two-token default, unchanged — so does a launcher whose
  second token already names the real target with nothing to skip past,
  like `npx eslint` or `node script.js`. **A WHOLE-command proposal is not
  "only this one invocation," precisely stated:** accepting it still
  authorizes a LATER command that starts with these same tokens and
  appends more, exactly like any shorter proposal — it is simply the
  narrowest proposal left once no shorter prefix still distinguishes this
  command from an unrelated one sharing its first tokens.
- **Never covers a compound command.** `git status` covers `git status
  --short`, but never `git status && rm -rf /`, `git status | sh`, `git
  status; rm -rf /`, a trailing `git status &`, an embedded newline, `git
  status $(rm -rf /)`, backticks, or `<(...)`/`>(...)` — any of those still
  prompt normally, exactly as they would with no grant at all. The same
  rule applies to what you type into the editor: a prefix containing one
  of those constructs is refused, with the reason shown right there,
  rather than installed.

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
diff, folded under the same line cap and `Ctrl-O` expand toggle every
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
shrinking the candidate list). Matching is ranked, not a plain prefix: an
exact prefix on the full command name ranks first, then a prefix on just
the final, namespaced segment of a plugin command's name (so `/rewind`
finds `/conway.history.rewind` without you typing `conway.history.`
first), then a fuzzy subsequence match against the command's name for
anything else.

| Command | Usage | Effect |
| --- | --- | --- |
| `/ask` | `/ask <text>` | Ask an ephemeral fork a side question — it doesn't affect the live session. While it's in flight, the `activity` status field shows `⠋ asking… Ns`, same as an ordinary turn; `Ctrl-C` abandons it (cancels the child and, if it's stuck waiting on a tool permission decision, discards that prompt too — nothing is left running). Once it answers, the reply opens in its own modal; choose to fork it into a real session, pull the Q&A into your transcript, or discard it. Pulling it in appends the question and answer to your transcript immediately, live — no restart or `/resume` needed — with a marker line naming the ask it came from, so a merged exchange is never mistaken for one you typed yourself. |
| `/new` | `/new` | End this session cleanly and start a fresh one in its place — see "`/new` and `/distill`: shrinking a conversation, out loud" below. |
| `/distill` | `/distill [<instructions>]` | Fork the focused agent, distill a briefing for a fresh agent, and show it before spawning one — see "`/new` and `/distill`: shrinking a conversation, out loud" below. |
| `/agents` | `/agents` | Toggle the below-chat agent-tree panel. |
| `/settings` | `/settings` | Open the settings menu (display preferences, permission mode, and grant management). |
| `/plugin` | `/plugin` | List every plugin conway can run today — compiled-in, subprocess, and MCP — each row naming where it came from and what it contributes. |
| `/steer` | `/steer <agent> <text>` | Send a steering message to a running agent. |
| `/cancel` | `/cancel <agent> [<reason>]` | Cancel a running agent immediately — stops it and its whole subtree, but never the session itself: cancelling any OTHER agent leaves the parent session working, and cancelling the session's own root agent is refused (use `/quit` to end the session instead). The cancelled agent's row in `/agents`/`/tree` flips to `Cancelled`. |
| `/await` | `/await <agent>` | Ask to be told when a running agent finishes — the operator's counterpart to the model's `conway_await` tool. Posts an immediate notice ("awaiting `<agent>`; a keep_alive agent ends only on `/cancel`"), then keeps working — input never blocks — and posts a second notice once the agent reaches a terminal state, naming its status, summary, and how many facts/artifacts it produced. Awaiting the session's own root agent is refused (use `/quit`); a second `/await` on an agent already being awaited is refused too — one waiter per agent from this surface. |
| `/context` | `/context [<agent>]` | Show an agent's assembled context, including its preamble (see below). With no argument, shows the focused agent's context; see [the agent panel](#the-agent-panel-agents) for where to find another agent's id. |
| `/goal` | `/goal [<text>\|clear]` | Set, show, or clear the focused agent's standing goal — see "`/goal`: a reminder that survives twenty turns" below. |
| `/why` | `/why` | Show the last routing decision — and, after a `/model`/`/role` switch, what changed. |
| `/fork` | `/fork [--role <alias>\|--model <backend/model>] [<text>]` or `/fork [--role <alias>\|--model <backend/model>] @<agent> <directive>` | Open an interactive fork of the focused agent (inherits its context, frozen at the fork point), or fork a specific agent explicitly. `--role`/`--model` (mutually exclusive) pick the child's routing explicitly instead of inheriting the focused agent's; giving either skips the free-text classification below entirely (there is nothing left to infer once routing is explicit). Otherwise, free text is classified into a fork/spawn recipe and confirmed before anything is created. |
| `/spawn` | `/spawn [--role <alias>\|--model <backend/model>] [@<agent_def>] [<prompt>]` | Open an interactive spawned agent — a clean slate, optionally from a named agent definition; inherits the parent's role/model if none is given. `--role`/`--model` (mutually exclusive) pick the child's routing explicitly — see `/fork`'s own row for the identical mutual-exclusion and classification-skip rules. |
| `/resume` | `/resume [<id\|name>]` | With an id or a name given via `conway sessions name`, resume that prior session directly -- switches this running TUI onto it, with its full history drawn into the transcript. An id or name that resolves to nothing errors clearly, naming `conway sessions list`. Bare, opens an interactive picker over this project's own sessions instead (see below). |
| `/model` | `/model [<backend/model>]` | With an exact `backend/model` argument, switch the focused agent to a pinned model, mid-conversation. Bare, or with any other text, opens an interactive picker over every reachable model instead — no plugin required (see below). |
| `/role` | `/role <alias>` | Switch the focused agent to a different role, mid-conversation. |
| `/trust permissions` | `/trust permissions` | Opens a preview card showing the project's `.conway/permissions.json` at its current content; `[y]`/`Enter` confirms (trusting it and installing its `allow` rules for this session), `[n]`/`Esc` cancels having written nothing. See [`permissions.md`](permissions.md). |
| `/tree` | `/tree` | Print the same agent tree the `/agents` panel shows, as plain transcript lines you can scroll back to or copy — with each agent's **full** id rather than the panel's short one, since a printed line may be pasted elsewhere long after the row set on screen has changed. |
| `/diff` | `/diff` | Print the cumulative diff of every file this session's agents have edited or written so far, one `## <path>` section per file, against the bytes each file had the first time this session touched it. See "The permission prompt" above and "Diffs, not raw JSON" below. |
| `/export` | `/export [<path>]` | Write this session's transcript to a Markdown file — the same rendering `conway sessions export --format markdown` produces (see [`sessions.md`](sessions.md#markdown-export)). With no argument, writes to `conway-<short-id>.md` in the current directory; with `<path>`, writes there instead. A leading `~` or `~/` in `<path>` expands to your home directory first, the same bare-tilde/`~/`-prefix rule every fs/shell tool argument already follows — `~bob/...`-style forms are not expanded and are kept literal. Never overwrites a file that already exists at the resolved path — if one does, prints a notice naming it instead of writing, so re-running `/export` with a different path is always the next step, never a silent clobber. |
| `/help` | `/help` | Open a read-only reference overlay: keybindings AND the full command list (built-ins plus every installed plugin's commands), in one scrollable view — the command section reads the identical list the `/` palette does, so the two can never disagree. |
| `/quit` | `/quit` | Exit conway. |
| `/exit` | `/exit` | A retired alias for `/quit` — still exits (so muscle memory from another tool is not punished), but prints a one-line "use `/quit`" notice first. |

A message that doesn't start with `/` is sent to the model as an ordinary
prompt. An unrecognized `/command` is reported as an error rather than
sent to the model. Every command in the table above is discoverable by
typing `/` — the `/` palette is generated from the same command table this
page's own list is, so the two cannot drift apart the way they once did
(board item `01M0RW29F2ATVGCV0R8H0GQEYH`: `/trust` and `/tree` used to work
while being absent from the palette).

### `/goal`: a reminder that survives twenty turns

A long run drifts: after enough turns of exploring, trying things, and
backing out of dead ends, neither the model nor you can see the objective
it started with from the transcript alone. `/goal <text>` sets a single
standing-goal sentence for the focused agent; the model is reminded of it
near the end of its context on every turn from then on, as a line reading
`Standing goal: <text>`. Bare `/goal` shows the current one (reading the
focused agent's own history directly — no fork, no model call); `/goal
clear` removes it. A goal over 300 characters is refused, naming the
limit, rather than silently cut short — shorten it and try again.

The goal is stored as an ordinary record on the agent's own log, so it
survives a `--resume` and shows up in `conway sessions show`; the status
line also carries a short `goal: <first words>` field once one is set.
Setting a new goal replaces the old one outright — it does not accumulate,
no matter how many times you change it, and clearing persists its own
marker so a later resume does not quietly bring an old goal back.

A forked child inherits whatever goal was set on its parent, because it
inherits the parent's whole log; a spawned child starts clean, with no
goal at all, since nothing of the parent's log carries over to a spawn.
`/goal` sets or clears nothing on its own but intent — it never continues
a conversation automatically and carries no token budget; it is a
reminder, not an enforcement mechanism.

This command's own behavior — the context line and the status-line field —
lives in the small, first-party `conway.goal` plugin (installed by
default; see [`docs/plugins/goal.md`](plugins/goal.md)). Uninstalling it
turns `/goal` into a command that says so, rather than one that silently
stores a goal nothing ever reads back.

### `/new` and `/distill`: shrinking a conversation, out loud

conway has no `/compact` and never will — nothing in a session is summarized
or rewritten behind your back (see "What conway deliberately will not do" in
[`GUIDE.md`](../GUIDE.md)). PHILOSOPHY.md's own §3 names the move it
recommends instead for a conversation that has grown heavy: "fork → distill
the part that matters → spawn a clean child with that briefing." That chain
is partial inheritance, built rather than configured — the reduction ends up
as an artifact you can read before you commit to it. `/new` and `/distill`
are that move, made into two typed verbs, the same liberty `/ask` already
took over the identical primitives for usability.

**`/new`** is the plain one: it ends the current session and starts a fresh
one in its place, same cwd, same role/model. Nothing is deleted — the old
session stays exactly as it was, still listed by `conway sessions list` and
still resumable with `/resume <id>`, and a notice names its id the moment
`/new` finishes. A queued message (`busy_input = queue`), a `!` shell command,
a `/distill`, an `/ask`, or a skill proposal still in flight refuses `/new`
outright, naming what's in the way, rather than silently discarding or
orphaning it; an agent turn in flight is aborted first (the same non-terminal
abort `Ctrl-C`'s first press uses — see "Typing while the agent works,"
above), not refused, since stopping the reply is the whole point of starting
over. What resets is the conversation — transcript, queues, focus, and any
permission you granted for that session only. Your configuration carries
over: keybindings, input history, `busy_input`, the status line, display
toggles, permission mode and rules, and the "project config ignored" marker.
`/resume` keeps the same set.

**`/distill [<instructions>]`** is the fork-then-brief move. It forks the
*focused* agent (ephemeral, exactly like `/ask`), and runs one directed turn
asking that fork to write a briefing covering the task, the decisions already
made and why, the open questions, and the files that matter — enough that a
fresh agent with none of this conversation's history needs nothing else to
start. `<instructions>`, if given, steers what the briefing should focus on,
the same way free text steers a `/fork`. The fork is bounded (20 steps, 180
seconds) — a reflection that goes sideways cannot spend an unbounded number
of tokens before the briefing even appears. `Ctrl-C` abandons an in-flight
`/distill` — the same way it already abandons an in-flight `/ask` — and if
the fork's briefing still arrives afterward, it is silently dropped rather
than popping its modal over a `/distill` you already gave up on.

Once the briefing is ready, it opens in its own modal with three ways out:

- **`Enter`** spawns a brand-new agent and delivers the briefing as its
  opening prompt, then switches this TUI onto it — a notice reports the cost
  of the move: the old agent's context size versus the new agent's, in
  tokens. `/context` on the fresh agent shows exactly what it started with:
  the briefing, and nothing of the old transcript.
- **`e`** opens the briefing in `$EDITOR` first (the same editor path
  `Ctrl-G` and the skill-proposal modal already use) — the edited text is
  what `Enter` then delivers.
- **`Esc`** discards the briefing. Nothing is spawned; the fork (already an
  ephemeral, throwaway child by this point) leaves no residue.

The old agent and its session are untouched either way — still forkable,
still resumable — `/distill` only ever adds a new one.

Neither verb ever runs on its own: there is no "at 90% we distill for you,"
and no automatic trigger of either command. You type `/new` or `/distill`,
or you don't — nothing about either chain happens behind your back.

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

The first line is the total estimate and segment count, plus — when the
focused model's `models.json` entry carries a `price` (see
[providers.md](providers.md#per-model-pricing)) — an estimated cost for the
NEXT request at that total, always marked `≈` since it is a prediction, not
an observation of a request that actually happened: `context: 175,212 tok
est across 183 segments · ≈$0.526 next request`. Omitted entirely when no
price is configured — never a guessed figure.

The table below it
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

**Subagents get instruction fragments too** (board item
`01M0VSKA76NSEHDSH25XJGJ2J5`). A forked or spawned child resolves fragments
the same way a root agent does — the same `resolve_instructions`/
`resolve_skills` functions are called for fork, spawn, and root alike, so a
child holding a tool whose plugin declares a fragment sees that fragment,
gated per-turn by the same `tool_ids` reachability check root goes through.
Directory-loaded skills reach a child the same way, through its own
resolved `agent_def.skills`. So `/context <child-agent>` shows a preamble
section whenever the child's own tool set reaches an installed plugin's
fragment — the same shape `/context` shows for the root, not a separate,
weaker case.

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

**Typing inside the open picker narrows the list.** Once the picker is
showing, any ordinary character narrows the visible rows to the ones whose
line (title, first prompt, activity, labels, id) contains what you typed,
case-insensitively; `Backspace` widens it back out one character at a time.
`Up`/`Down` and `Enter` only ever move to or answer with a row you can
currently see — a filter that matches nothing leaves the list empty rather
than letting `Enter` resume a row that is no longer shown. This is
**unlike `/model`**, which has no equivalent: `/model <narrower text>`
still means "close this picker and open a new one over the results," not
"filter the currently-open one" — the two commands are not required to
behave alike (see the next paragraph for why).

**A key that widens the listing to sessions under a different project's own
session-root key.** `Tab`, while the picker is open, additionally fetches
and merges in sessions found under every OTHER project directory this
machine knows about (the same central-root scan `conway.discover`'s
cross-project search already performs) — closing the one gap the base
picker above still has: a session sitting under an old subdirectory key
(see
[`sessions.md`](sessions.md#if-you-already-have-subdirectory-keyed-sessions))
is otherwise reachable only by typing its id or name directly, never listed
alongside this project's own sessions. Merged rows are metadata only (no
first prompt, no last-activity figure — reading every candidate session's
own transcript across every project would be unbounded I/O this toggle
deliberately does not pay for) and are marked with the project key they
came from, so they read as clearly distinct from this project's own rows.

**`/resume <text>` typed at the command line keeps its exact existing
contract — neither affordance above changes it.** An id or a name that does
not resolve stays a plain, immediate error naming `conway sessions list`
(see the table above): a typed `/resume` argument still means exactly one
thing, "reattach to precisely this session," never "open the picker
pre-filtered by this text." That is a deliberate difference from `/model`,
not an oversight: landing in the WRONG session is costlier than landing on
the wrong model, so a mistyped `/resume` argument gets a fast, specific
refusal instead of a silent guess at which session you meant. Filtering by
typed text is available only once you can already see the candidates —
inside the open picker, as described above.

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
timestamps, `busy input` — `queue`/`steer`/`interrupt`, see "Typing
while the agent works," above — and a read-only `attention` row showing
the current terminal-attention config, below), **tool output** (how many
lines a folded tool call shows before `Ctrl-O` is needed), **permissions**
(cycle the
permission mode; review or revoke individual grants under **allow** — flat
and structured alike; read-only **deny** and **prompt** sections listing
every rule — flat or structured — that any permissions file, trusted or
not, has put in force, each with the file it came from; and **hooks**, a
fourth, revocable review list), **providers** (add or remove a
`backends.<id>` entry — see
[`providers.md`](providers.md#managing-providers-from-the-tui)), and
**plugins** — a single shortcut row that opens `/plugin` (below); this
menu itself no longer lists plugins directly. `Up`/`Down` navigate,
`Enter` toggles a boolean, cycles the default role or the busy-input mode,
expands/collapses a group, revokes a selected grant/hook row, or opens
`/plugin`, `Left`/`Right` step the numeric tool-preview setting, `Esc`
closes. The display settings (including busy input), the permission-mode
cycle, and every revoke action apply to this session only — `busy input`
starts from `[tui.busy_input]` in `settings.json` (default `steer`) but,
like the other two display rows, cycling it here never writes that file
back; the tool-preview line count persists to `[tui.tool_preview_lines]`
when you step it. Permission-mode and grant details are covered in
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
  panel below the list with that plugin's own status plus four rows in
  the operator's own framing — **you get** (what turning it ON adds),
  **you lose** (what's different with it OFF), **costs** (its ongoing
  cost, if any), and **config** (this plugin's EFFECTIVE `[plugins.config.
  "<id>"]` table, if it has one that applied: the accepted keys and
  values, and the table they came from — or `defaults` when no table
  named this id). The `config` row is the same value/source pair `conway
  plugin list --verbose` prints for this id, never a second,
  independently-worded rendering of it. A flip writes `~/.conway/
  settings.json`'s `plugins.install` array directly (or
  `$CONWAY_CONFIG_DIR/settings.json` when that's set) — the SAME writer,
  and the SAME restart-to-apply contract, `/settings`' own plugins section
  used before this command existed: the change applies on your NEXT
  restart, not immediately, and the footer says so.
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

**A broken `[plugins.config."<id>"]` table only stops conway from starting
when `<id>` is actually installed.** A bad block for a plugin you have
since turned off, or a typo'd id that never matched one at all, instead
starts conway normally and shows a one-line warning naming the plugin,
the offending key, and why it was ignored — the same channel a
misconfigured headroom or a failed MCP handshake already use. Only a bad
block for an INSTALLED plugin still refuses to start, exactly as before:
that one WOULD have governed this session.

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
| `mode` | `ready`, `running`, `awaiting permission`, `ask`, or `intent` | The TUI's current top-level state. `ready` and `running` are the same underlying "no modal card is open" state, split by whether a turn is actually working — `Mode::Normal` used to always render `ready`, which told a watcher (human or automated) nothing was happening for the entire span of a running turn; it now reads `running` while `activity` is animating and `ready` only once it genuinely is idle. When your permission mode isn't the default, this field also names it: `running · plan` or `ready · AUTO-ALLOW`. `AUTO-ALLOW` is the one thing on this line guaranteed to keep showing even on a very narrow terminal — it's a genuine safety signal, and the field most likely to matter if you've forgotten you're in it. |
| `model` | `anthropic/claude-sonnet-4-6` | The focused agent's serving model. Omitted until its first turn has routed. |
| `ctx` | `ctx 42%`, or `ctx 12.3k` when the model's context window isn't known | Cumulative context-window occupancy for the focused agent, from the same resolved `(backend, model)` capability index [`conway routes explain`](routing.md#asking-why-a-route-was-chosen) reads. When the window itself is only a `floor (assumed)` — the model's own dialect declares no sourced figure, so this is not a fact about this specific model — the figure carries that same marker: `ctx 31% floor (assumed)`. A `verified` (compiled-in table, or a dialect's own documented per-provider figure), `models.json` (an operator-editable override), or `probed` window never carries it. |
| `tokens` | `1.4k tok (88% cached)`, or `1.4k tok (cache: not reported by ollama)` | The focused agent's cumulative token spend; the cached-percentage parenthetical is the prompt-cache hit rate — `cache_read / (input + cache_read + cache_write)`. **Declaration honesty**: the percentage shows whenever the backend actually reported cache figures for at least one cache-relevant token — including a genuine `0% cached` — and is omitted only when the denominator itself is 0 (no cache-relevant tokens processed yet). When the backend's wire format carries no cache field at all (e.g. Ollama's native `/api/chat` path, see [providers.md](providers.md)), the field instead shows `cache: not reported by <backend>` — a `0%` here would claim an observation the backend never made. |
| `cost` | `$0.145` | The focused agent's cumulative session cost, priced from the same cumulative usage `tokens` reads — not part of the default Lean line; add it to your own `fields` list. Omitted entirely — never a placeholder, never a guessed figure — unless the focused model's `models.json` entry carries a `price` (see [providers.md](providers.md#per-model-pricing)). A figure prefixed `≈` means the backend did not report whether prompt caching applied this session (`tokens`' own `cache: not reported`/`not supported` case above): the true cost may be lower than shown. |
| `activity` | `⠋ thinking… 12s · +45 tok` while active, `⠋ asking… 12s` while an `/ask` is in flight, `idle` otherwise | The working indicator: elapsed time and new context tokens added this turn. An in-flight `/ask` takes this field over outright (its own clock, no token figure — it's a different agent than the one this field otherwise tracks). |
| `hint` | `Enter submit · Ctrl-O expand · /help · /agents to view` | A persistent reminder of the essentials. The `expand` fragment always names `transcript.toggle_tool_output`'s effective key. Also names the focused agent when you're off-root and `lineage` isn't part of your configured fields. |
| `git` | the current branch name | Read once at startup; omitted outside a git repo. |
| `cwd` | the session's working directory | Omitted when unset. |

On a narrow terminal, fields give up space in a fixed order rather than
being clipped mid-word: ambient chrome (`cwd`, `git`) first, then
point-in-time telemetry (`model`, `ctx`, `tokens`/`cost`), then orientation
(`session`, `lineage`), then `activity`, then `hint`. `mode` is never
dropped — its own single degrade step removes the `ready`/`awaiting
permission` word and keeps only the permission-mode label, so `AUTO-ALLOW`
is the last thing standing on even the narrowest terminal that shows
anything at all.

## Terminal attention notifications

A coding agent works while you look elsewhere; the moment it finishes or
stops to ask permission is the moment you want to know. `[tui.attention]`
in `settings.json` controls a short, out-of-band signal written straight
to your terminal — never into the transcript — when that happens:

```json
{
  "tui": {
    "attention": {
      "method": "bell",
      "when": "unfocused",
      "events": ["turn_finished", "permission_pending"]
    }
  }
}
```

This is also the default — you don't need to write any of it yourself to
get it. `method` is one of:

| Method | What it does |
| --- | --- |
| `bell` (default) | The plain terminal bell (`BEL`, `\x07`) — a flash, a dock-icon bounce, or an audible beep, depending on your terminal/OS settings. The lowest common denominator: every terminal this project has had to consider renders *something* for it, with zero configuration, inside or outside tmux, even one that predates the other two methods entirely. |
| `osc9` | `OSC 9` — a real desktop notification carrying the event's own short text, on terminals that implement it. |
| `osc777` | `OSC 777` (`notify`) — the richer, two-field desktop-notification variant (title `conway`, body the event's own text) some terminals prefer. |
| `off` | No signal at all. |

`when` is `unfocused` (default — only notify while this terminal window
does *not* have focus) or `always` (notify unconditionally, even while
you're looking right at it). A terminal that never reports focus changes
at all is treated as unfocused under the default — you still get notified,
rather than the feature silently never firing because the one signal that
would have proven it unfocused never arrived.

The same rule applies at startup to a terminal that *does* report focus:
terminals report focus changes, not the current state, so until you
switch away from conway and back once, conway cannot tell that you're
looking at it, and the first notification may ring while the window has
focus. After that first change, `unfocused` behaves as described.

`events` (default `["turn_finished", "permission_pending"]`) is the list
of occurrences that count: `turn_finished` (the FOCUSED agent's own turn
ending) and `permission_pending` (a permission prompt becoming visible —
including one promoted from the queue once a previous prompt/modal
closes) are both wired to a real trigger today. `child_reported` and
`error` are accepted by the schema and reserved for a future item; naming
either in `events` loads cleanly but produces no notification yet.

### Per-terminal support

| Terminal | `bell` | `osc9` | `osc777` |
| --- | --- | --- | --- |
| iTerm2 | yes | yes (desktop notification) | yes |
| kitty | yes | yes | yes |
| WezTerm | yes | yes | yes |
| Ghostty | yes | yes | yes |
| foot | yes | yes | no |
| Windows Terminal | yes | yes | no |
| GNOME Terminal / VTE-based | yes (audible/visual bell setting) | no | no |
| A plain `xterm` | yes (bell only; no desktop integration) | no | no |

When in doubt, `bell` always does *something*; `osc9`/`osc777` are
worth opting into once you've confirmed your own terminal supports one
(most terminal projects document this on their own "escape sequences" or
"notifications" page).

### Inside tmux

An `OSC` sequence (`osc9`/`osc777`) written to a pane running inside tmux
is swallowed by tmux itself unless wrapped in tmux's own DCS passthrough
envelope — conway does this wrapping for you automatically whenever
`$TMUX` is set, so `osc9`/`osc777` work inside tmux without any change to
this config. **The one thing conway cannot do for you: tmux's own
`allow-passthrough` option defaults to off** (tmux 3.3+). Add this to your
`tmux.conf` once to let the wrapped sequence actually reach your terminal:

```
set -g allow-passthrough on
```

`bell` needs none of this — tmux already forwards a plain terminal bell
as its own bell action.

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
    "toggle_tool_output": ["Ctrl-Z"]
  }
}
```

Top level: context name → `{ action: [key, key, ...] }`. An entry you
supply REPLACES that action's default keys wholesale — rebinding
`toggle_tool_output` off its default `Ctrl-O` really turns `Ctrl-O` off,
not "off by default but still there." Binding an action to `[]` disables
it with no replacement. The same key bound in two DIFFERENT contexts is fine (only
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
- `prompt.line_start` — default `Ctrl-A` — move the cursor to the start of
  the current line.
- `prompt.line_end` — default `Ctrl-E` — move the cursor to the end of the
  current line.
- `prompt.kill_to_start` — default `Ctrl-U` — delete from the cursor to the
  start of the current line.
- `prompt.kill_to_end` — default `Ctrl-K` — delete from the cursor to the
  end of the current line.
- `prompt.delete_word_back` — default `Ctrl-W` — delete the previous word.
- `prompt.history_prev` — default `Ctrl-P` — recall the previous
  input-history entry.
- `prompt.history_next` — default `Ctrl-N` — recall the next input-history
  entry.
- `prompt.complete_path` — default `Tab` — complete the path-shaped word
  under the cursor (see "Mentioning files," below). Only reached while no
  `@`-mention list is open — `mentions.accept` (below) owns `Tab` while one
  is.
- `prompt.send_now` — default `F2` — abort the focused agent's current turn
  (a no-op if it is already idle) and send this message now, regardless of
  the session's own `busy_input` mode — the per-message, one-off twin of
  `busy_input = interrupt` (see "Typing while the agent works," above).

On a multi-line draft (`Alt-Enter`/`Shift-Enter`), all four of
`line_start`/`line_end`/`kill_to_start`/`kill_to_end` act on the cursor's
CURRENT line only, never the whole buffer. One deliberate difference from
readline: `Ctrl-K` with the cursor already at the end of a line does
nothing, rather than joining the next line onto this one.

#### `transcript`

- `transcript.toggle_tool_output` — default `Ctrl-O` — expand/collapse all
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

#### `mentions`

- `mentions.navigate_up` — default `Up` — move the mention-completion
  selection up.
- `mentions.navigate_down` — default `Down` — move the mention-completion
  selection down.
- `mentions.accept` — default `Tab`, `Enter` — insert the highlighted
  mention candidate.
- `mentions.close` — default `Esc` — close the mention-completion list
  without inserting anything.

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
above. No chords/leader keys here either: this table is single-key
rebinding only. `tui.editor_mode = "vim"` (see "Vim editing mode," above)
is a SEPARATE modal layer over the input box, not an entry in this table —
it never rebinds any action above, and every Ctrl-bound action here keeps
its exact meaning in both vim submodes.

## Ending a session

`/quit` ends the session cleanly (`/exit` does too, as a retired alias —
see the slash command table above). `Ctrl-D` does the same when
the input box is empty.

**Quitting stops whatever the agent is doing, including a tool it is
mid-way through running — without ending the session itself.** If a turn
is in flight — the model streaming a reply, or a tool call actually
executing — quitting aborts it before the process exits, the SAME
non-terminal abort a single `Ctrl-C` press already uses (see "Typing while
the agent works" above): the agent is left idle, ready to resume, never
given a terminal result. A `keep_alive` session stays fully resumable
afterward (`--resume`/`/resume`), exactly as if you had pressed `Ctrl-C`
once and then quit, never as if the session itself had ended in error.
This matters most for a long-running tool: a `bash` call the model started
(`sleep 999999 &`, a build, anything that outlives the call that launched
it) is killed, whole process group and all, rather than left running with
no supervisor once conway exits — previously, nothing on any quit path
stopped it, since killing a tool's process group has always depended on
the call actually being aborted, and no quit path aborted anything. A
forked or spawned subagent with its own turn running gets the identical
treatment, not just the root. An MCP server or other plugin-managed
subprocess needs no separate handling here: those are torn down whenever
the process holding them exits, regardless of how.

A bare, unprefixed `exit`, `quit`, `q`, `:q` or `:wq` — typed as an
ordinary message, no leading `/` — is intercepted too, with a one-line
hint ("to leave, use `/quit` (or `Ctrl-D` on an empty line) — press Enter
again to send the word to the model") rather than being sent to the model
as a prompt. These are another tool's quit commands, not conway's, and
without this a cheerful, useless reply is all that word would ever get.
Pressing Enter again with the exact same word sends it through as an
ordinary prompt — nothing else gets caught by this: "please exit" or
"quit my job" are ordinary prompts and reach the model unchanged.

Two consecutive `Ctrl-C` presses force an
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

### Closing the terminal, or a process manager stopping conway

Closing the terminal window you launched conway from sends it `SIGHUP`; a
process manager (systemd, a supervisor script, a parent shell's own job
control) asking it to stop sends `SIGTERM`. Both get the same cleanup
every other way of ending a session already gets: the agent's own current
turn is aborted (never the session itself — it stays resumable), just like
an ordinary `/quit` (see above) — an in-flight `!` command, or a
model-issued tool call such as `bash`, is killed, whole
process group and all (bound-awaited, not a bare signal with no
confirmation it actually landed), every other kind of in-flight residue
(an `/ask`/`/distill` fork, a parked confirmation card) is discarded
exactly as it would be on an ordinary quit, and the terminal itself is
left sane — never stuck in raw mode or the alternate screen. conway then
exits with the same signal-specific CODE
[`scripting.md`'s exit-code table](scripting.md#exit-codes) documents for
one-shot mode: **143** for `SIGTERM`, **129** for `SIGHUP` — the code
only, not that table's "terminal status is `Cancelled`" wording, which
describes one-shot mode's own outcome and does not apply here: as the
paragraph above already says, the session stays resumable with no
terminal record at all. A second
`SIGTERM`/`SIGHUP` while that cleanup is still running forces an
immediate, unconditional exit — the identical "second signal always wins"
safety valve two consecutive `Ctrl-C` presses already give you above — and,
unlike the first signal, does **not** wait for the terminal to be restored
first: that immediate exit exists specifically so a wedged app loop (which
could itself be the very thing blocking an orderly restore) can never
prevent getting out at all. A panic is handled differently again: the
terminal is always restored (the same panic hook that has always done
this), but an in-flight `!` command's child is not deliberately killed —
dropping the whole process on an unwinding panic already kills at least
that command's own leader process (`kill_on_drop`), though a backgrounded
grandchild it spawned could in principle outlive it; closing that
remaining gap is tracked as a follow-up, not implemented here.
