# conway-cli

`conway-cli` is the `conway` binary: one-shot (`-p`/`--print`) mode and the
interactive TUI, both built purely on the [`conway`](conway.md) facade —
this crate never reaches into `conway-core`/`conway-runtime` directly (a
`no_forbidden_deps` test enforces the dependency restriction). See
[`/ARCHITECTURE.md §1`](/ARCHITECTURE.md) for how the TUI and one-shot mode
are equal-status consumers of the same facade API, not a "more native"
mode versus a lesser one.

## Responsibility and boundary

```
cli.rs           the complete clap command surface (Cli, Command, OutputFormat)
main.rs          entry point: matches every fallible step explicitly, no `?`
exit.rs          ExitCode: the one vocabulary every command path reports through
diag.rs          stderr-only diagnostic output (error/warn/info)
oneshot.rs       -p/--print mode: prompt -> streamed render -> ExitCode
signal.rs        SIGINT handling for one-shot mode
session_ref.rs   --session/--resume/--fork-from value parsing
render/          streaming renderers: text, json, jsonl
commands/        subcommands: sessions, routes, fmt
tui/             the interactive terminal shell
```

`main` never uses `?`: every fallible step is matched explicitly and
converted to an `ExitCode` via `ExitCode::from_error`, so there is exactly
one place — the bottom of `main.rs` — that turns an `ExitCode` into a
process exit status. `diag.rs` is the only place any code in this crate
may write a diagnostic: every function there writes to `stderr` only and
none takes a stdout handle, which is the actual mechanism (not just a
convention) that enforces "stdout carries only program output" — a
renderer or command handler that wants to tell the user something can only
reach for `diag::{error,warn,info}`, never a stray `println!`.

## One-shot `-p`/`--print` mode

`oneshot::run` (`oneshot.rs`) reads the prompt (from argv or stdin), builds
the session through the facade, streams the resulting `EventStream`
through a `render::Renderer` selected by `--output-format`
(`text`/`json`/`jsonl`), and maps termination to an `ExitCode`. All three
renderers (`render/text.rs`, `render/json.rs`, `render/jsonl.rs`) write
through the same `Box<dyn Write + Send>` — always a `BufWriter<Stdout>` in
practice — and each flushes itself after every event, never buffering
across events, so a consumer piping the output sees it promptly rather
than in bursts.

**Fails closed by default.** One-shot mode cannot prompt an operator for a
permission decision, so its `PermissionGate` (`build_gate`, wired from
`--permission-mode`/`--allowed-tools`/`--deny-tools` onto `conway`'s
`AllowListGate`) denies every tool when `--allowed-tools` is empty and no
broader `--permission-mode` is set — a bare `conway -p "do something"`
with no explicit tool grant is intentionally answer-only, no side effects,
rather than silently permissive.

**SIGINT handling** (`signal.rs`) is two-tier: `install` spawns a
background task watching for Ctrl-C. The *first* delivery only records
itself — `oneshot::run`'s own render loop reacts to it (cancelling the
session, starting a grace window), since only that loop holds the live
`SessionHandle`. The *second* (and every later) delivery aborts the
process immediately and unconditionally from the background task alone,
independent of whether the render loop is itself stuck (for example,
blocked on a backend that never responds) — "a second Ctrl-C forces an
immediate exit" cannot depend on the main loop noticing anything.

**Exit codes** (`exit.rs`) are the single vocabulary every command path
reports through — no code in this crate calls `std::process::exit`
directly for anything reachable from a live run; everything resolves to an
`ExitCode` value first. The discriminants are the contract (`code()` casts
`self` directly to `i32`):

| code | variant | meaning |
|---|---|---|
| 0 | `Completed` | `ResultStatus::Completed` |
| 1 | `AgentFailed` | `Failed`/`Rejected`/`Cancelled` (no SIGINT observed), or an unclassified `ConwayError` |
| 2 | `Usage` | config/agent-def/build/unsupported-feature error |
| 3 | `PermissionDenied` | reserved — see below |
| 4 | `NoHealthyBackend` | `RoutingError::NoCandidate`, reached either directly or wrapped in `RuntimeError::Routing` (fallback-chain exhaustion) |
| 5 | `BudgetExceeded` | `ResultStatus::BudgetExceeded` |
| 130 | `Interrupted` | `ResultStatus::Cancelled` **and** a SIGINT was observed (`from_result_with_sigint`) |

**Disclosed gap, not silently worked around:** code 3 (`PermissionDenied`)
is unreachable from any live code path in the currently-committed runtime.
`conway-runtime`'s `PermissionBroker` collapses both `PermissionDecision::
Deny` and `::DenyWithFeedback` into a model-visible tool-result error
(`tools/runner.rs`), never a terminal `ConwayError` — so a permission
denial never reaches `ExitCode::from_error`. `ToolError::Denied` is
declared in `conway-core` but constructed nowhere in the workspace.
Producing `PermissionDenied` in practice would require the runtime to add
a terminal permission-escalation path; `exit.rs`'s module doc flags this
for the module owner rather than approximating it with a speculative
match. Similarly, the 4-code path (`NoHealthyBackend`) is detected by a
`Display`-substring match (`"no candidate for role"`) rather than a type
match, because the facade does not re-export `conway_core::error::
{RoutingError, RuntimeError}` and this crate is mechanically forbidden
from depending on `conway-core` directly (`no_forbidden_deps`).

**Session continuity.** `--session`/`--resume` take a bare `SessionId`;
`--fork-from` takes `<session-id>[@<seq>]` (parsed by `session_ref.rs`,
which depends only on `conway`'s own re-exported `SessionId`/`LogSeq`
types, honoring the same facade-only dependency restriction as the rest of
the crate).

## Subcommands

`commands/` implements `conway sessions` (list/inspect persisted sessions,
`commands/sessions.rs`), `conway routes explain <role>` (renders
`Conway::explain_routing`'s `ExplainReport` — see
[`conway-routing`](conway-routing.md) — so an operator can see exactly
which candidate was chosen for a role and why, and which were skipped and
why, `commands/routes.rs`), and `conway fmt` (`commands/fmt.rs`).

## The TUI

`tui::run` (`tui/mod.rs`) owns terminal lifecycle: it enters raw mode and
the alternate screen, installs a panic hook that restores the terminal
before re-raising (so a panic mid-session never leaves the user's shell in
raw mode), and restores the terminal on every exit path, not just the
success one.

### Architecture: three tasks, one render model

`tui/app.rs`'s `run` joins three sources through channels into one
`AppState` (`tui/state.rs`), redrawing at a capped rate: the session's own
`EventStream`, the permission gate's pending-prompt channel, and
crossterm's key/resize event stream. `AppState::apply` is the single
mutation entry point, fed one `Envelope` at a time — this is what makes
the render model unit-testable with no terminal at all: construct an
`AppState`, feed it a sequence of `Envelope`s, assert on the resulting
transcript/tree. `run` itself is not unit-tested directly (it owns a real
terminal and a live `SessionHandle`); every piece it composes
(`state::apply`, `input::handle_key`, `view::draw`, `gate::TuiGate`) is
unit-tested independently instead. `App::submit` is the one exception worth
calling out narrowly: it needs only a live `SessionHandle`, no terminal, so
its own test builds a fully in-memory `Conway` and drives it directly (see
the `Entry::User` note just below).

**`Entry::User` is built from the event stream, not pushed locally
(P-8 fix).** `submit`/`deliver_first_message` used to push `Entry::User`
into the transcript themselves, synchronously, before ever calling the
facade — so the TUI showed a user's prompt but a library embedder watching
the bare `EventStream` for the same session never did, exactly the kind of
mode divergence P-8 calls a renderer bug. `conway-runtime` now emits a
typed `Event::UserTurn` live for every prompt (`Runtime::prompt`, reached
by both call sites), and `AppState::apply` builds `Entry::User` from that
envelope — the one path the TUI and any other `EventStream` consumer now
share. `submit`/`deliver_first_message` push nothing locally anymore; doing
so would double the prompt, which is exactly what `tui/app.rs`'s own
`submit_renders_the_prompt_exactly_once_not_zero_not_twice` test guards
against (built the same way `App::submit` needs — an in-memory `Conway`,
not the state-only `test_support` harness, since `Action::Submit` needs a
live facade call that harness deliberately does not make). Replaying a
session (a focus switch, or a fresh `agent_events`/`events_from` subscribe)
reconstructs the same `Entry::User` from `session_handle.rs`'s
`record_to_event`, which now maps `LogRecord::UserTurn` to `Event::UserTurn`
faithfully instead of a stringly-typed `AgentProgress` fallback — see
[`conway-core`](conway-core.md) and [`conway`](conway.md) for the full
`Event`/`EventStream` side of this change.

**`TuiGate`** (`tui/gate.rs`) is the TUI's in-process `PermissionGate`: its
`check` never decides anything itself — it forwards every
`PermissionRequest` over an `mpsc` channel as a `PendingPrompt` and awaits
the app loop's `oneshot` reply. This is what lets the runtime's tool-call
task block (per `PermissionGate`'s own contract — the gate may block
indefinitely) while the ratatui app loop renders a prompt and waits for a
keypress on its own task, without either side needing to poll the other.

**Key handling** (`tui/input.rs`) translates a `crossterm::event::KeyEvent`
into an `Action` the app loop carries out. It is pure with respect to the
input line itself (`AppState::input`/`AppState::cursor` are mutated
directly, as local editing state with no async effect) but never calls
`SessionHandle`/`Conway` directly — every side-effecting action is
returned to the caller instead, keeping key handling testable without a
live session.

### Single-column layout (0.2.0 redesign)

`tui/view/` implements the TUI's render pass as a pure function from
`&AppState` to a `ratatui::Frame` — no `AppState` mutation, no I/O, so it
runs under `ratatui::backend::TestBackend` with no real terminal. 0.2.0
replaced the previous always-on two-pane layout (a left agent-tree pane
alongside right transcript/input columns) with a **single column**:
conversation stream on top, an optional on-demand agent panel, an input
box, and a bottom status line.

- **`transcript.rs`** — the conversation stream: a plain, borderless
  `Paragraph`, deliberately rendered with no `.block(..)` at all (no
  border, no title, no box-drawing glyph anywhere in the area). Every cell
  painted there comes straight from a rendered `Span`'s text content, so
  selecting/copying this region copies exactly that plain text — this is
  the "clean-copy" guarantee, and it is why the input box (below) is
  allowed a border while this area never is.
- **`input_box.rs`** — the input line, one of the two bordered elements
  (with the on-demand agent panel/palette); the clean-copy guarantee is
  specific to the transcript, not the whole screen.
- **`status.rs`** — a single, always-visible plain status line (no border)
  summarizing mode and agent count, and naming the two on-demand
  affordances: `/` for the command palette, `/agents` for the agent-tree
  panel.
- **`agents.rs`** — the below-chat agent-tree panel, shown on demand
  (toggled by `/agents`) rather than as an always-on side pane. `/agents`
  is the canonical agent surface: every row carries the agent's status
  marker and label plus its *recipe label* — how the agent was spawned —
  composed from the spawn event's provenance: `fork @seq N` for a fork
  (with its inherited-up-to fork point), `@<agent_def>` for a spawn with a
  named agent definition, `(inherit)` for a spawn that inherited the
  parent's role/model, and an `(ephemeral)` marker on `/ask`-style
  ephemeral forks (which are full tree citizens, shown with their
  provenance attached). While the panel is open, `v` cycles a draw-time
  visibility filter — **all** (the default, V5), **finished-only**,
  **active-only** (terminal-status agents hidden) — without ever mutating
  the tree itself; the header names the current mode. Ordinary subagent
  lifecycle is *also* surfaced inline in the conversation stream itself;
  this panel is for browsing the whole tree at a glance, not the only place
  activity shows.

  **V5** flipped the default from active-only to all: hiding a node the
  instant it finishes read, in practice, as "the agents screen doesn't
  always list the same agents" — the panel reshaping itself with no visible
  cause. The status marker (`v`/`x`/`-` vs `*`/`o`/`?`) already reads
  status at a glance per row, so nothing about "what's still running" is
  lost by defaulting to a stable, unfiltered list; active-only remains one
  `v` press away.

  `/tree` still parses but is demoted to a **hidden alias** (dropped from
  the palette): it renders the same content as the panel —
  the same `state.tree` nodes, the same recipe labels — as plain-text
  transcript notices, one line per agent, indented by depth, with the full
  agent id kept on each line so it can be copied into `/steer` /
  `/context`. Unlike the panel it ignores the visibility filter and shows
  *all* nodes, terminal ones included, since a transcript dump is a
  provenance artifact.

### Theme: `[tui.theme]`

`tui/view/theme.rs` is the TUI's central color/style system. A single
`Theme` struct holds one named `ratatui::style::Style` per concern, and
`view::draw` plus each per-view `draw` fn takes `&Theme` and reads
`theme.<name>` instead of building a `Style` inline. The `Theme` is
constructed once at startup (`App::new`) from the loaded `[tui.theme]`
config table and passed by reference into every render pass — it is
injected, not re-fetched via a call-site accessor or a global (decision
D-T1). An unconfigured TUI renders identically to a pre-theme build: the
`Theme::default()` values are the exact `(Color, Modifier)` pairs the view
files used to hand-roll inline, captured faithfully (visual parity is an
acceptance criterion for the refactor).

`[tui.theme]` is a `[tui]` subsection of `settings.json` (schema:
`conway::config::schema::ThemeConfig`). Each named style is an optional
sub-table with `fg`, `bg`, and `modifiers`; a field you omit uses the
built-in default for that slot. The TUI resolves `fg`/`bg` as ratatui
color names and `modifiers` as ratatui modifier names; an unparseable or
out-of-range value falls back to the default for that slot — never a
panic (P-10: config is untrusted input). Example:

```toml
[tui.theme.notice]
fg = "yellow"
modifiers = ["bold"]

[tui.theme.tool_running]
fg = "#ff8800"
```

**Accepted color spellings** (case-insensitive, `snake_case` or
`kebab-case`): `black`, `red`, `green`, `yellow`, `blue`, `magenta`,
`cyan`, `gray`/`grey`, `dark_gray`/`dark-gray`, `light_red`/`light-red`,
`light_green`/`light-green`, `light_yellow`/`light-yellow`,
`light_blue`/`light-blue`, `light_magenta`/`light-magenta`,
`light_cyan`/`light-cyan`, `white`, `reset`, and `#rrggbb` / `#rgb` hex.

**Accepted modifier spellings**: `bold`, `dim`, `italic`, `underlined`,
`reversed`, `slow_blink`/`slow-blink`, `rapid_blink`/`rapid-blink`,
`hidden`, `crossed_out`/`crossed-out`.

#### Palette rationale (V7): what each color MEANS

T1 built the named-style table; every default in it is a **named ANSI
color** (`Color::Yellow`, never `Color::Rgb`), deliberately — a named color
resolves through the user's own terminal palette, so Conway sits inside
whatever scheme they already run rather than fighting it. `Color::Rgb` (hex)
parsing exists only for a user's own `[tui.theme]` override, never a
built-in default.

V7 audited that table for the failure mode a "more polish" request usually
produces — adding color — and found the opposite problem in a few spots:
color spent on things that don't mean anything, and one color-worthy meaning
(AUTO-ALLOW) that had none. The rule set it settled on, so the next slot
added here has something to follow instead of a nearest-neighbor guess:

- **Red is failure or active danger, never decoration.** `error`,
  `tool_failed`, `agent_failed`, `border_danger` are all red. `fatal_error`
  (red + bold) is the palette's one *highest*-alert accent — V7 gave it a
  real call site: the status line's `AUTO-ALLOW` indicator (every tool call
  auto-approved with no prompt is a genuine safety-relevant state, previously
  rendered with no color at all via `theme.emphasized`). It stays reserved
  for a genuinely fatal runtime error too, once `Event::Error { fatal: true }`
  is wired to carry a style through `Entry::Notice` (not done by V7 — see
  Deferred, below).
- **Yellow is in-progress.** `tool_running`, `agent_running`, `spinner`,
  `border_warning` (the `/ask` modal is "waiting on you," in-progress from
  the operator's side).
- **Magenta is blocked-on-you.** `tool_awaiting`, `agent_awaiting` — distinct
  from yellow (moving on its own) and red (something went wrong): this is
  "stopped, needs a decision."
- **Green is success, and only success.** `tool_done`, `agent_finished`. V7
  stopped reusing green for `help_key` (a plain layout column, no status at
  all — see "chrome," below) so a green glyph anywhere in the TUI means
  exactly one thing.
- **Gray/dim is secondary, and is never a fixed dark color.**
  `tool_proposed`/`agent_starting` (pending — Gray, a lighter, more legible
  gray on both dark and light backgrounds than DarkGray) mark "not yet
  meaningful." Everything that means "secondary annotation" —
  `dim`, `timestamp`, `agent_cancelled`, `reasoning`, `status_dim`,
  `scroll_footer` — renders via `Modifier::DIM` rather than a fixed
  `Color::DarkGray`. This is a V7 correction: `Color::DarkGray` is a
  *fixed* dark color, and a dark-background terminal's own "bright black"
  frequently renders it nearly indistinguishable from the background —
  `timestamp`, `reasoning`, and `agent_cancelled` all defaulted to
  `Color::DarkGray` before this item and are the ones V7 checked and moved.
  `Modifier::DIM` instead asks the terminal to dim *its own* foreground
  color relative to normal — legible by construction on both a dark and a
  light scheme, since it is never an absolute color guess.
- **Conversation text is never colored.** `user` and `assistant` stay
  unstyled — the reading experience is plain text, the way `claude code`/
  `opencode`-quality TUIs keep the actual conversation quiet so the
  status/tool chrome around it can carry meaning without competing.
  `assistant_marker` (magenta + bold) is the one deliberate exception: it
  names *which model* answered, provenance metadata sitting next to the
  text rather than the text itself, not an emphasis choice.
- **Chrome that carries no state is bold or dim, never colored.** `focused`,
  `emphasized`, and (as of V7) `help_key` are bold-only. `help_key` used to
  be green + bold, distinguishing the `/help` overlay's key/chord column
  from its plain description column — a real need, but green already means
  "success" elsewhere, and reusing it for a column split blurred that
  meaning for zero benefit (bold alone still reads as a distinct column).
- **Modal borders are colored by how urgent the decision behind them is**:
  `border_danger` (red — approve/refuse a tool call), `border_warning`
  (yellow — the `/ask` modal), `border_accent` (cyan — the NL intent-confirm
  card), `help_border` (blue — `/help`, the one modal with no decision at
  all, deliberately the coolest, least urgent hue of the four).

**Removed: `agent_marker`.** It had no call site anywhere in `view/*.rs`
from the day T4 defined it through V7's audit (grep-verified) — a config key
a user could set that would silently do nothing, the same failure mode V6
already ruled out for `spinner_b`/`spinner_c`. `[tui.theme.agent_marker]` is
now an unrecognized key (`deny_unknown_fields`) rather than a no-op; if you
had it set, remove it. No alias: an override that never had any visible
effect cannot regress by being rejected instead.

**Considered and NOT done: collapsing `tool_*`/`agent_*` into one semantic
set.** The ten tag slots share five default colors two-for-two (`tool_
proposed`/`agent_starting` both Gray, `tool_running`/`agent_running` both
Yellow, `tool_awaiting`/`agent_awaiting` both Magenta, `tool_done`/`agent_
finished` both Green, `tool_failed`/`agent_failed` both Red — `agent_
cancelled` has no tool-side counterpart, so it stays its own slot regardless).
That duplication is real, but it is a maintenance/documentation fact, not a
rendered-UI problem: the two families draw in different places (tool-call
tags vs. agent-lifecycle tags), never side by side as a pair a user could
notice disagreeing, and a user overriding one axis independently of the
other (recoloring agent status without touching tool status, say, for
scanability when both appear interleaved in one stream) is a real,
if uncommon, use this rationale doc is exactly what makes safe: the meaning
each slot carries is now written down, so an override that diverges from
this default is a legible, deliberate choice, not drift. Collapsing would
have required either breaking any config that already sets one of the ten
names (schema `deny_unknown_fields`) or a genuine aliasing layer with real
precedence-ordering risk — for a problem that is presently invisible on
screen. Left as-is; this doc section is the fix for the "ten slots, five
meanings" finding instead of a schema change.

**Deferred: a fatal `Entry::Notice` still renders in `theme.notice`
(cyan).** `Event::Error { error, fatal }` already distinguishes a fatal
error in its *text* (`"fatal error: …"` vs. `"error: …"`) but both map to
the same `Entry::Notice { text }` variant, which always renders cyan — the
same color as an ordinary informational notice like `"backend degraded"`.
`theme.fatal_error` (red + bold) exists and is now wired up (see the AUTO-
ALLOW note above) but this second call site is not: giving `Entry::Notice`
a `fatal: bool` so the two render distinctly touches roughly four dozen
existing construction/match sites in `tui/state.rs` and its tests, which is
real state-machine work, not a color-table pass — filed as a follow-up
rather than done under this item's scope.

**Named styles** (each defaults to the pre-theme inline style; new accent
styles have no pre-theme call site and are defined for later v0.3.0 items
to consume):

| Name | Default | Used by |
| --- | --- | --- |
| `user` | bold, no fg | transcript `you>` prefix |
| `assistant` | unstyled | transcript assistant text body |
| `assistant_marker` | magenta + bold | (T4) assistant `[modelname]>` speaker marker |
| `reasoning` | dim + italic | (T4) `Entry::Reasoning` trace body + `thinking>` prefix (V7: was dark_gray) |
| `timestamp` | dim | (T4) `HH:MM ` per-entry timestamp prefix (`/settings` -- "show timestamps") (V7: was dark_gray) |
| `tool_proposed` | gray | tool-call tag, `Proposed` |
| `tool_awaiting` | magenta | tool-call tag, `AwaitingPermission` |
| `tool_running` | yellow | tool-call tag, `Running` |
| `tool_done` | green | tool-call tag, `Finished` (ok) |
| `tool_failed` | red | tool-call tag, `Finished` (error) |
| `agent_starting` | gray | agent-tree/transcript marker, `Starting` |
| `agent_running` | yellow | agent-tree/transcript marker, `Running` |
| `agent_awaiting` | magenta | agent-tree/transcript marker, `AwaitingPermission` |
| `agent_finished` | green | agent-tree/transcript marker, `Finished` |
| `agent_failed` | red | agent-tree/transcript marker, `Failed` |
| `agent_cancelled` | dim | agent-tree/transcript marker, `Cancelled` (V7: was dark_gray) |
| `notice` | cyan | transcript `Entry::Notice` |
| `error` | red | `/ask` modal error line |
| `fatal_error` | red + bold | the status line's `AUTO-ALLOW` indicator (V7; reserved before that) |
| `dim` | dim modifier | agent-tree recipe labels, input-box placeholder |
| `focused` | bold modifier | agent-tree `(focused)` tag |
| `selected` | reversed modifier | agent-tree arrow-selected row highlight |
| `emphasized` | bold modifier | modal-overlay body lines (command, `you asked:`, `recipe:`), and `plan` mode in the status line |
| `border_normal` | unstyled | input-box / agent-panel block borders |
| `border_warning` | yellow + bold | `/ask` modal border |
| `border_danger` | red + bold | permission-prompt modal border |
| `border_accent` | cyan + bold | NL intent confirmation card border |
| `status_mode` | reversed modifier | the bottom status line |
| `status_dim` | dim modifier | status-line dim accent (T2: the `elapsed · +tokens` tail of the working indicator) |
| `spinner` | yellow | activity spinner accent (steady; V6 removed T2's pulse palette) |
| `header` | reversed modifier | (T6, corrected) sticky prompt overlay above the transcript while the current turn's prompt is scrolled out of view |
| `scroll_footer` | dim modifier | (T6) floating "jump to bottom" footer pill |
| `help_border` | blue + bold | (T7) `/help` overlay border, and the `/settings` menu border |
| `help_key` | bold modifier | (T7) `/help` overlay's key/chord column (V7: was green + bold) |

A unit test (`tui::view::theme::tests::no_inline_style_default_fg_color_remains_in_view_files`)
guards the refactor's central invariant: no `Style::default().fg(Color::…)`
literal remains in any `view/*.rs` other than `theme.rs` (the one place
the defaults live). `Theme::default()`'s exact pairs are pinned by
`default_*_match_pre_t1` tests so a future change cannot silently drift
the colors.

### Status line: `[tui.status_line]` (T3)

The bottom status line is a single, always-visible plain line (no border)
that summarizes the focused agent's turn at a glance. It is an **ordered,
configurable set of fields** driven by the `[tui.status_line]` table in
`settings.json` (schema: `conway::config::schema::StatusLineConfig`).
Each field renders only when it is both listed in the configured `fields`
order AND has data to show (e.g. `git` is omitted when not in a repo,
`model` is omitted before the first `Event::ModelDecision`).

The whole line uses `theme.status_mode` (reversed) as its base style; the
`activity` field overlays its spinner style and dim
elapsed/tokens tail, and the `hint` field overlays `theme.status_dim`.

**Default Lean line:** `session | lineage | mode | model | ctx | tokens |
activity | hint` (`lineage` is omitted while root-focused and `model` is
omitted until the first turn routes, so a brand-new root-focused session
shows `session | mode | ctx | tokens | activity | hint`).

`session` and `lineage` were added by the item that corrected a requirement
miss in T6's scroll-triggered sticky overlay: T6 put this same content on a
header that appeared and disappeared with scroll position, which is the
tell that it was misfiled — session/model/ctx/lineage are application
chrome, not scroll-position-dependent information, so they belong in the
persistent status line. See "Sticky prompt overlay..." below for the full
story.

**Configuration** — `[tui.status_line]` has one key, `fields`: an ordered
list of field names to render. A field absent from the list is hidden; the
list order is the render order. Unknown names are dropped at render time
(P-10: config is untrusted input, never a panic). An empty/empty-after-
validation list falls back to the default Lean order rather than rendering
a blank line. `CONWAY_TUI__STATUS_LINE__FIELDS=session,lineage,mode,model,ctx,tokens,activity,hint`
overrides the order via env (comma-split). **A `fields` list pinned before
`session`/`lineage` existed keeps working unchanged, but will not gain
either new field** — an omitted name is never an error, so an older config
simply renders without them.

```toml
[tui.status_line]
fields = ["mode", "model", "ctx", "tokens", "git", "activity", "hint"]
```

**Available fields** (all orderable):

| Field | Renders | Source |
| --- | --- | --- |
| `session` | `session <id>` (the session's own root agent's short id) — unconditional, always renders | `AppState::root_agent` |
| `lineage` | off-root only: `agent <id>[ via root → fork @seq N → @def]` — see "Sticky prompt overlay..." below for the full breadcrumb/degrade behavior | `AppState::tree` (`TreeNode::parent`/`kind`/`inherited_upto`) |
| `mode` | `ready` / `awaiting permission` / `ask` / `intent` | `AppState::mode` |
| `model` | the serving model display name, e.g. `anthropic/claude-sonnet-4-6`; omitted before the first turn routes | `Event::ModelDecision { chosen }` |
| `ctx` | `ctx 42%` when the focused model's max context is known, else `ctx 12.3k` (raw tokens, compact-suffixed; capped at `ctx 100%`) | cumulative `Event::ContextSegmentAdded { tokens_est }` ÷ the focused model's `max_context_tokens` from `[models.metadata_path]` |
| `tokens` | `<total> tok (<n%> cached)` when cache data is present, else `<total> tok` | cumulative `Event::TurnFinished { usage }` (`Usage`'s input + output + both cache dimensions + reasoning); `n%` = `cache_read / (input + cache_read + cache_write)`, omitted when the denominator is 0 or `cache_read` is 0 |
| `activity` | the working indicator: `⠋ thinking… 12s · +45 tok` while active, `idle` while idle (steady `theme.spinner` style; `+{n} tok` is session-deduped new-segment tokens added this turn) | `AppState::activity` + T2 counters |
| `hint` | a persistent keybinding hint, dim: `Enter submit · Ctrl-E expand · /help · /agents to {view\|hide}`, plus `focused: <id>` when the transcript is focused on a non-root agent AND `lineage` is not part of the resolved field list (reconciled against `lineage` naming the same thing — see below). V6 trimmed the slash-command enumeration — `/help` is the single pointer to the rest | static + `AppState::agent_view_open`/`focused_agent` |
| `git` | the current branch (e.g. `main`); omitted when not a git repo, git is absent, or the command fails | one-shot `git rev-parse --abbrev-ref HEAD` at startup, no polling |
| `cwd` | the session's working directory; omitted when unset | `Cli --cwd` or `config.cwd` |

**Width-aware degradation (review finding).** Adding `session`/`lineage`
grew the default line's full length to ~106 characters — wide enough that,
on anything narrower, a plain "render every field's full text and let the
terminal clip the overflow" approach silently ate `hint` (the line's only
pointer to `/help` and the `/agents` toggle), and below ~40 columns ate it
*entirely*, with no visible sign anything had been cut. The status line now
treats itself as one width budget: each field has its own small ladder of
shorter-but-still-complete phrasings (the same "shorter complete form,
never a mid-word clip" rule the floating scroll footer and `lineage`'s own
Full → Compact → Bare degrade already used), and when the full line does
not fit the render width, fields give up space one at a time in a fixed
priority order — never a mid-character clip.

Give-up order, weakest claim on the line first:

1. `cwd`, `git` — ambient chrome, already conditionally omitted.
2. `model`, `ctx`, `tokens` — point-in-time telemetry, reconstructable from
   the transcript/turn-end summary if briefly absent.
3. `session`, then `lineage` (degrades Full → Compact → Bare first, then
   drops entirely) — orientation ("which session/agent am I in").
4. `activity` — degrades to spinner + phrase (drops the elapsed/token
   tail), then is omitted entirely; this happens before `hint`/`mode` give
   up anything, so a tiny terminal sacrifices "is it working" rather than
   have it compete with discoverability/safety for the last few columns.
5. `hint` — discoverability. Degrades full → `/help · /agents to
   {view|hide}` → bare `/help`, dropped entirely only as the very last
   resort before `mode`.
6. `mode` — **never dropped.** Its one degrade step removes the
   `ready`/`awaiting permission` UI word and keeps the non-default
   permission-mode label alone, so `AUTO-ALLOW` — a genuine safety signal;
   an operator who forgets they're in it is the exact failure this guards
   against — is the one thing on the line guaranteed to survive as long as
   *anything* does, right down to the narrowest terminal that shows
   anything at all.

**This guarantee only protects `mode` while it is actually in the resolved
`fields` list — a subsequent adversarial review found and closed a gap
here.** `fields` accepted a list that simply never named `mode` verbatim,
which silently disabled `AUTO-ALLOW` at every width via config alone (a
hand-pinned `settings.json`, or `CONWAY_TUI__STATUS_LINE__FIELDS` set
without it) — not a width accident, and not something the width-degradation
machinery above could ever protect against, since it only runs on fields
already in the list. Fixed at the render layer, uniformly across every
config source: while the active permission mode is non-default
(`plan`/`AutoAllow`), `mode` is forced into the resolved field list even
when the configured `fields` omits it. This is **not user-disableable** —
it depends only on the live permission mode, not on anything in `config` —
and it stays out while `Prompt` (the default) is active, so an
ordinary/older `fields` list that genuinely doesn't want `mode` keeps
rendering exactly as configured: the field only appears once it has
something non-default to say.

**Width accounting is in terminal columns, not `chars().count()`, and a
pathological width truncates explicitly instead of silently — two more
review findings closed alongside the one above.** A field's text is not
ASCII-only (`lineage`'s `@{agent_def}` hop names are arbitrary user-chosen
text), and a CJK character or emoji is one `char` but renders as two
terminal columns; the width-fit arithmetic used to undercount those by up
to 2x, which could make the assembly believe a line fit when it actually
overflowed onto whichever field ended up last in the assembled text — every
width calculation now goes through ratatui's own `Span::width()`/
`Line::width()` display-width helpers instead. Separately, the give-up loop
above can legitimately exhaust every field's own floor and still not fit an
arbitrarily narrow terminal (the bare `AUTO-ALLOW` label alone is 10
columns and has nowhere shorter to go) — that case now clamps the assembled
line explicitly, at a character boundary, with a trailing `…` marker,
rather than being handed over-length to the renderer and silently clipped
mid-word wherever the terminal's own boundary happened to fall. Below ~12
columns the mode label may render as e.g. `AUTO-ALL…` instead of the full
`AUTO-ALLOW` — a deliberate, marked degradation at a genuinely pathological
width, not an unmarked accident.

**`tokens (n% cached)` format.** The `tokens` field is a single combined
field: the cumulative token total followed by a cache-hit-rate
parenthetical. `total` is the sum of every `Usage` field
(`input_tokens + output_tokens + cache_read_tokens + cache_write_tokens +
reasoning_tokens`) — all of them are tokens the model actually processed
for this agent's own turns. The parenthetical `(<n%> cached)` is the cache
hit rate `cache_read_tokens / (input_tokens + cache_read_tokens +
cache_write_tokens)`, shown as a whole-number percentage. The
parenthetical is omitted entirely (rendering just `<total> tok`) when the
denominator is 0 (no input or cache activity yet) or when `cache_read` is
0 (no cache hits to report a rate from) — divide-by-zero is guarded, never
a panic.

**`ctx%` computation.** The numerator is `AppState::focused_ctx_tokens`:
the segment-id-deduped sum of every `Event::ContextSegmentAdded { tokens_est }`
observed on the focused agent's own stream since the focus began. Each
segment id is counted at most once per focused session
(`AppState::focused_seen_segments`), so a non-keep-alive child whose fresh
`AgentLoop` re-emits its existing context on the first turn of a new run
does not double-count. The denominator is the focused model's
`max_context_tokens` from the local model-metadata file
(`[models.metadata_path]`), looked up by the model's `"backend/model"`
string at the time a `ModelDecision` arrives. When the metadata file has
no entry for the chosen model (or no metadata file exists), the field
falls back to the raw token count, compact-suffixed (`ctx 12.3k` for
12,345 tokens; `ctx 750` for sub-thousand counts). The percentage is
capped at `ctx 100%` — a deliberate lossy clamp: a context estimate that
exceeds the declared max (headroom, rounding, an under-declaring metadata
file) is shown as `ctx 100%` rather than `ctx 137%` so the line never
looks like a bug; this CAN hide a genuine overshoot, accepted as a
tradeoff (the authoritative total lands via the turn-end summary, and a
proper runtime re-fetch on focus is a separate follow-up).

A freshly focused agent shows `ctx 0%` / no model until its own next LIVE
`ContextSegmentAdded`/`ModelDecision` arrives — replay does NOT synthesize
these events (`record_to_event` maps a replayed `Assistant` record to
`TextDelta`, never to `ContextSegmentAdded` or `ModelDecision`). A proper
re-fetch of the true context total from the runtime on focus is tracked as
a separate follow-up board item.

**Git branch read.** `App::new` runs `git rev-parse --abbrev-ref HEAD`
once via `std::process::Command` on the blocking pool (best-effort: `None`
on non-repo / git-absent / non-zero exit / non-UTF8 output / spawn error;
never panics, never blocks startup on a hung `git`). No polling — the
branch is read once and stored on `AppState::git_branch`. C-04: no new
deps.

**Model display name.** `AppState::apply`'s `ModelDecision` arm (which
the spec called out as previously dropped by the wildcard arm) captures
`chosen.to_string()` into `AppState::focused_model` and looks up the max
context in `AppState::model_max_context` (populated once at `App::new`
from `[models.metadata_path]`). Both reset on `focus_agent`; the new
focus's own first LIVE `ModelDecision` repopulates them (replay does NOT
synthesize `ModelDecision` — see the `ctx%` computation note above).

### Activity spinner + animation tick (T2)

The status line's activity slot is the TUI's primary "is it working?"
signal. While the focused agent's `Activity` is anything other than
`Idle`, the slot renders a braille spinner glyph plus the activity word
plus live elapsed seconds plus the new context tokens added this turn,
e.g. `⠋ thinking… 12s · +45 tok`. The spinner glyph and the activity
word share one steady `theme.spinner` style.

T2 originally cycled that style through a three-color palette on every
tick. V6 removed it: in practice it read as strobing in the corner of the
eye rather than as a liveness signal. The advancing braille frame already
carries that signal, and adding color motion on top only competed with
it. The `spinner_b`/`spinner_c` theme slots and their `[tui.theme]` config
keys were removed with it — a config key that silently does nothing is
worse than no key at all.

The 125ms tick is additive to the existing 16ms redraw cap
(`REDRAW_TICK`); it advances `AppState::spinner_frame` (modulo the
10-glyph braille sequence `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) and marks the frame dirty **only while
`should_animate(activity)` is true**. An idle terminal never pays for
animation: the tick arm is gated, so the counters don't advance and no
redraw is forced (the 16ms redraw tick still runs but is itself
dirty-gated).

The elapsed clock starts at `Event::TurnStarted` for the focused agent
(`AppState::turn_started_at`) and stops when `activity` returns to
`Idle` (`TurnFinished`/`AgentFinished` for the focused agent, or a
focus switch). The `+{n} tok` figure
(`AppState::turn_running_tokens`) is the sum of
`Event::ContextSegmentAdded { tokens_est }` deltas on the focused
agent's own stream between `TurnStarted` and `TurnFinished`. The
runtime emits `ContextSegmentAdded` only for segments NEW to a
session-scoped `seen_segments` set that is deliberately never reset
across turns, so this is a session-deduped segment-delta count — NOT
total context occupancy and NOT the authoritative turn-end token
total. On turn 1 it reads ~full context size (every segment is new);
on turn 2+ only genuinely new segments fire, so for the same
conversation it is large on turn 1 then small on turn 2. The leading
`+` signals "added this turn" and visually distinguishes it from the
cumulative `tokens` field (`<total> tok (<n%> cached)`, formalized by T3
— see the `[tui.status_line]` section below), so both figures are visible
while active. The authoritative turn-end token total lands via the
turn-end summary (T4).

A second T2 flourish: while `activity == Responding`, the live,
in-progress `Entry::Assistant` line in the transcript gets a block `▌`
cursor appended at **render time only** — never baked into the stored
`Entry::Assistant` text or into `entry_lines` output for settled
entries (the clean-copy invariant is relaxed only for the actively-
streaming line, per decision D-clean-copy). When the turn settles the
cursor disappears from the next render because the
`activity == Responding` gate stops firing.

### Tool output folding + expand (T5)

A settled tool entry (`Entry::Tool` with a non-empty `preview`) renders
its preview **folded** by default: only the first
`[tui.tool_preview_lines]` physical lines (default 3) render, followed
by a dim `… (+M lines, Ctrl-E to expand)` affordance naming how many
lines are hidden. `Ctrl-E` flips `expanded` on **every** tool entry in
the transcript at once (MVP — there is no transcript-cursor/selection
state, so expand/collapse is all-at-once); an expanded entry renders
its full preview with no affordance. The stored `preview` is never
truncated — the cap is render-time only, so toggling never loses data.

The toggle is pure state mutation: `Ctrl-E` does NOT touch `scroll` or
`follow_tail`. The next render's existing clamp
(`state.scroll.min(max_scroll)`) re-clamps to the nearest valid
position without snapping the viewport — a toggle that shrinks the
content height clamps an overscrolled `scroll` down to the new
`max_scroll`; a toggle that grows it back restores the original
`scroll` since it was never overwritten.

`Ctrl-E` (a control key, not a bare `e` — the always-on input box must
keep `e` as ordinary text input) is bound in `input.rs::handle_normal_key`
and calls `AppState::toggle_all_tool_entries_expanded` directly,
mirroring the `v` visibility-filter key's direct-mutation pattern (no
`Action` variant, since the toggle has no facade side effect). The
status-line `hint` field advertises `Ctrl-E expand` (reconciling the
earlier `Ctrl-E submit` hint — unmodified Enter was and remains the actual
submit key; T8 adds `Alt-Enter`/`Shift-Enter` for inserting a literal
newline instead, a distinct binding from submit itself — see "Input
ergonomics" below).

**Configuration** — `[tui.tool_preview_lines]` is an optional integer
(default 3, clamped to `1..=200` with a fallback to 3 on a
missing/out-of-range/bad value — P-10: config is untrusted input, never
a panic). `CONWAY_TUI__TOOL_PREVIEW_LINES=10` overrides via env.

```toml
[tui]
tool_preview_lines = 5
```

**Clean-copy invariant.** Settled tool output uses no box-drawing and no
`Block`: the entry ends with a blank line followed by a dim plain `-`
rule (a single dash, styled with `theme.dim`) as a non-box separator.
The `entry_lines_never_contain_box_drawing_glyphs` test covers this —
no `│`/`─`/`Block` appears in collapsed or expanded tool output.

**T4 reuse.** The `Entry::Tool::expanded` flag and the
`tool_lines` collapsed/expanded render branch are intentionally generic:
T4's tool-args preview is the same shape (a one-line-truncated args
preview is the collapsed branch with `cap = 1` and a different content
string), so T4 can reuse the mechanism without changing the `expanded`
field, the affordance format, or the `Ctrl-E` action.

### Transcript provenance (T4)

The transcript entries (`Entry` in `tui/state.rs`) carry provenance fields
that the renderer (`tui/view/transcript.rs::entry_lines`) surfaces:

- **Reasoning traces** — `Event::ThinkingDelta` now creates/appends an
  `Entry::Reasoning { text, model, summary, ts }` (previously only
  `activity` was flipped to `Thinking`; the delta itself was dropped).
  Rendered dim+italic with a `thinking> ` prefix, **expanded by default**
  (`AppState::show_reasoning` defaults `true`). The `/settings` menu's
  "show reasoning traces" row (V4; originally the standalone `/thinking`
  slash command, removed) toggles `show_reasoning`; when off, `build_lines`
  skips `Entry::Reasoning` entirely (the entries are still stored, so
  toggling back on restores them without replay).
- **Speaker markers** — `Entry::Assistant` gains `model: Option<String>`
  (stamped from `AppState::focused_model` at creation time). The renderer
  prepends `[modelname]> ` (plain `[`/`]`/`>`, no box-drawing) styled with
  `theme.assistant_marker` when `model` is `Some`; omitted when `None`
  (replay — `record_to_event` maps a stored `Assistant` record to a bare
  `TextDelta` carrying no model, so a replayed bubble renders as it
  originally streamed).
- **Tool args + progress** — `Entry::Tool` gains `args: String` (from
  `Event::ToolCallProposed::args`, stored compact) and `progress: String`
  (accumulated `Event::ToolProgress { call_id, note }` notes, previously
  dropped by `apply`'s wildcard arm, appended to the matching in-flight
  tool entry by `call_id`). Args render as a one-line truncated `args: …`
  preview while collapsed and an `args:` label + pretty-printed JSON while
  expanded; both args and output expand/collapse together via the single
  `expanded` flag (T5's `Ctrl-E` toggle). Progress notes render as dim
  `-> {note}` lines between the args line and the output block.
- **Per-entry timestamps** — `Entry::Assistant`/`Reasoning`/`Tool` gain
  `ts: Option<DateTime<Utc>>`, stamped from the envelope's `ts` at apply
  time. The `/settings` menu's "show timestamps" row (V4; originally the
  standalone `/timestamps` slash command, removed) toggles
  `AppState::show_timestamps` (default off); when on, `entry_lines`
  prepends an `HH:MM ` prefix (styled with `theme.timestamp`) to the
  entry's first rendered line.
- **Turn-end summary** — `Event::TurnFinished` stamps a
  `summary: Option<String>` onto the last `Entry::Assistant` or
  `Entry::Reasoning` (whichever was the last block under the turn),
  formatted `{elapsed} · {tokens} ({n%} cached)` from the turn's `Usage`
  and `turn_started_at` elapsed (e.g. `1m 6s · 1.4k tok (88% cached)`).
  Rendered as a final dim line on the block. A turn with no
  assistant/reasoning block (only tool calls) gets no summary.

The streaming cursor (T2's `▌`) extends to the live reasoning line: while
`activity == Thinking`, the cursor attaches to the last `Entry::Reasoning`'s
last line (same render-time carve-out T2 uses for the assistant streaming
line; the clean-copy invariant is preserved for settled output).

`/thinking` and `/timestamps` originally were intercepted in
`app.rs::submit` (mirroring `/agents`'s pattern) — state-only toggles that
never reached `commands::parse` and were never sent to the model. **V4
removes both entirely** (not aliased) in favor of a single `/settings`
menu — see "The `/settings` menu (V4)" below.

### Sticky prompt overlay, jump keys, and the scrolled-back indicator (T6, corrected)

Scrolling back through a long conversation used to lose you: nothing said
where you were, and nothing but paging brought you home. T6 added three
affordances, all keyboard-only; a later item corrected what the first of
the three actually showed.

**T6's original mistake, and the fix.** T6's own problem statement was
scroll-shaped ("you scroll and lose track of where you are"), but its
binding decision put `session <id> · agent <id>[ via lineage] · model ·
ctx%` on a header reserved above the transcript **only while it overflowed
the viewport**. That gating was the tell: nobody gates session/model/ctx on
scroll position if they actually mean it as persistent chrome — chrome that
flickers with scroll position is noise, not information. The user's own
correction: *"the sticky header isn't a full app header. its just for
scrolling, it was a requirement miss... We want a sticky header showing the
prompt (or a preview of the prompt)."*

So the overlay now shows **exactly one thing**: the current turn's own
prompt, and only while it has scrolled out of view. `session`/`model`/
`ctx%` were never removed — they always belonged in the persistent status
line and stay there (see the `[tui.status_line]` section above). The
lineage breadcrumb (V5) is the one field that genuinely needed a new home,
since T6 misfiled it onto the scroll overlay in the first place: it moved
to the status line's new `lineage` field, taking its width-degrade
machinery and its fork/spawn-content trap with it unchanged (see below).

**The sticky prompt overlay** is a single plain row pinned to the TOP of
the transcript pane, shown only while the CURRENT TURN'S OWN prompt has
scrolled above the viewport. The trigger is neither "the transcript
overflows" (T6's original, wrong test) nor `!follow_tail` (the floating
footer's own trigger, also wrong here — a short turn scrolled back only
slightly still has its own prompt genuinely on screen, and the overlay must
stay away in exactly that case). The actual rule: find the entry governing
the viewport's topmost visible row; walk backward from it to the nearest
`Entry::User`; if that IS the governing entry (some part of the prompt is
already on screen), show nothing; otherwise show that prompt.

This also decides *which* prompt to show when several turns are on-screen
at once: the **nearest** `Entry::User` at or before the viewport's top —
"sticky" in the editor sense — never simply the most recent prompt anywhere
in the transcript, which would name an unrelated question several turns
back the moment the reader scrolls up into an earlier one.

Mapping a scroll offset (counted in WRAPPED rows) to a transcript entry
needs a wrapped-row breakdown per entry, not just per-entry logical line
counts — `view/transcript.rs::entry_row_starts` provides it, reusing the
exact same `Paragraph::line_count` wrapping `wrapped_line_count`/`draw`
already use (computed per entry rather than for the whole transcript at
once, since ratatui's `Wrap` reflows each stored `Line` independently and
never merges across entry boundaries) rather than a second, hand-rolled
wrap model that could silently put the overlay one turn off.

A focused agent with no `Entry::User` of its own yet (a freshly focused
spawn child — `focus_agent` clears the transcript down to that agent's OWN
log with no lineage content mixed in) draws nothing. It never falls back to
an ancestor's prompt: a spawn child inherited nothing from its parent, so
showing parent content would display context the agent itself never saw.

**Overlay text handling.** A pasted prompt can be multi-line (`Alt`/
`Shift-Enter` insert literal `\n`s) — flattened to one line before
measuring width. It can carry raw control bytes (bracketed paste can
include an actual ANSI escape) — every control character is stripped
before the text ever reaches the rendered `Span`, so a pasted escape can
never inject styling. And unlike every other degrading string in this
crate (the floating footer below, the lineage breadcrumb's width-fit
search), a prompt has no shorter *complete* rewrite to fall back through —
so once it doesn't fit, the overlay truncates it for real, by character
(never by byte, which can split a multi-byte UTF-8 sequence and render as
garbage), with a trailing `…`.

**V5 — lineage breadcrumb**, now a `[tui.status_line]` field
(`lineage`) rather than part of the scroll overlay. Off-root, `agent <id>`
grows a `via` clause naming how the focused agent came to be: `agent <id>
via root → fork @seq 3 → @reviewer`. This answers the "clicking into a
subagent doesn't show the parents" report: `focus_agent` clears the
transcript down to the focused agent's *own* log (unchanged), but the tree
itself — `parent`/`kind`/`inherited_upto` on each `TreeNode` — is always
available, so the breadcrumb reads it fresh on every render rather than
seeding anything into the transcript. Each hop's text is built by
`view/agents.rs::hop_label`, which reuses the exact same `recipe_parts`
provenance string the `/agents` panel row already shows for that node —
`fork @seq N` for a fork, `@def`/`(inherit)` for a spawn — so the breadcrumb
and the panel can never disagree about how a given agent was created. A
node with no recorded `kind` (the root, or one seeded out-of-band via
`ensure_agent_tracked`, which never saw a spawn event) falls back to its
own short id rather than being mislabeled as a fork or a spawn it never
was.

This is **metadata only** — never the ancestor's actual transcript
content — and that is deliberate, not a shortcut. A fork child's inherited
context is a literal, immutable prefix of the parent's log, so showing it
would be accurate; a spawn child inherits nothing, so showing it parent
content would display information the agent never actually saw. Rather
than build two different rendering paths (one that shows fork content,
one that carefully never shows spawn content) with the attendant risk of
getting the distinction wrong, the breadcrumb stays at metadata for both —
which is trivially safe for a spawn child, and still legitimately useful
for a fork child (it names the fork point; the fork's own transcript, once
focused, contains the inherited content itself).

The breadcrumb degrades through shorter *complete* forms rather than being
clipped mid-word — the same shape `footer_text` (below) already uses for
the floating footer. A long chain first drops to `agent <id> via root →
…(N) → <last hop>` (keeping the endpoints and the count of what was
omitted), and finally to the plain `agent <id>` with no lineage at all if
even that does not fit. The ancestry walk itself
(`view/agents.rs::ancestor_chain`) is bounded to 64 hops and stops the
instant it revisits an id already in the chain, so a cycle in `parent`
(should be impossible) ends the walk rather than hanging.

**Reconciling with `hint`'s own note.** Before `lineage` existed, the
status line's `hint` field appended `focused: <id>` off-root as its only
way to name the focused agent. Now that `lineage` says the same thing (and
more), `hint` suppresses its own note whenever `lineage` is part of the
resolved field list — it survives only as a fallback for an older pinned
`[tui.status_line] fields` config that predates `lineage` and so never
gained it.

**End and Home** jump the transcript when the input box is empty: `End`
snaps to the tail and re-engages auto-follow, `Home` jumps to the top and
disengages it. When the input box has text, both keep their ordinary
cursor-movement meaning — the transcript jump never steals a key you were
using to edit. (No `G`/`gg`: Conway is always in input-typing mode, so a
bare printable key can never be a binding.)

**The floating footer** appears over the transcript's bottom row whenever
you have scrolled away from the tail: `↓ N lines above tail — End to jump
to bottom`, with a live count. It vanishes the moment auto-follow
re-engages. On a narrow terminal it degrades to a shorter *complete* form
(`↓ 8 above — End to jump`, then `↓ 8 — End`) rather than being clipped
mid-word — a hard truncation would cut off the `End` hint first, which is
the half that tells you what to do about it.

**Neither the sticky prompt overlay nor the footer reserves a layout row,
or is part of the transcript's own `Paragraph`.** Both are `Clear` +
`Paragraph` overlays drawn straight onto the frame after
`transcript::draw` — the overlay over the transcript's own top row, the
footer over its bottom row — the same pattern the permission and `/ask`
modals already use. This is a correction from T6's original shape, which
reserved the header a real `Constraint::Length` row whenever the
transcript overflowed; an overlay needs no such reservation, and removing
it also ends the reflow-under-the-reader problem an appearing/disappearing
reserved row caused. The clean-copy guarantee is untouched — `entry_lines`
never emits either overlay, so selecting and copying the transcript still
yields exactly the conversation text.

**Mouse capture stays off, but the wheel still scrolls.** Conway does not
enable crossterm mouse capture: a captured terminal routes every mouse
event to the application instead of the emulator, which disables the
terminal's own click-drag text selection — the mechanism the clean-copy
guarantee exists to protect.

That does not mean the wheel is inert. Terminals implement *alternate
scroll* (DECSET 1007): while the alternate screen is active, they
translate wheel events into `Up`/`Down` cursor-key presses. So a
two-finger scroll reaches Conway as arrow keys, and bare `Up`/`Down`
scroll the transcript one line — which is what makes the wheel work.

This is why input history lives on `Ctrl-P`/`Ctrl-N` rather than on the
arrows. Conway cannot tell a wheel-driven arrow from a typed one (the
information that would distinguish them is exactly what mouse capture
would provide), so the arrows go to the more frequent interaction.
`PageUp`/`PageDown` scroll a full page; `Home`/`End` jump to top/tail.

Theme slots: `header` (reversed) — now the sticky PROMPT overlay's style,
carried over unchanged from T6's original header slot — and `scroll_footer`
(dim), both configurable under `[tui.theme]`.

### Input ergonomics: multi-line, history, paste, wrap-fix (T8)

The input box (`view/input_box.rs`) used to be single-line-only (`Enter`
always submitted; a literal `\n` could never land in it), had no memory of
what you'd typed before, silently mangled a pasted block into a flood of
individual keystrokes, and clamped a long line's cursor to the box's own
width instead of scrolling — the cursor visually froze at the right edge
while the text it was supposedly pointing at kept extending off-screen,
invisibly. T8 fixes all four.

**Multi-line: `Alt-Enter` *and* `Shift-Enter`.** Either inserts a literal
`\n` at the cursor; unmodified `Enter` still submits. Both are bound,
deliberately — some terminals encode Shift-Enter indistinguishably from a
plain Enter, so relying on Shift alone would silently lose multi-line entry
on those terminals. The input box's own height grows with the draft: one
row per `\n`-separated line plus the two border rows, capped at
`area.height / 3` so a long paste or draft can never crowd the
transcript/status out entirely. `view/mod.rs::layout`'s constraint list
reads this same grown height, so the input box and the transcript area can
never disagree about how much room each actually has.

**History: `Up`/`Down`, persisted.** Every submitted line (a prompt or a
slash command) is pushed onto a bounded FIFO
(`[tui.history_size]`, default 500 — see below), oldest entries evicted
once the cap is exceeded. `Up` recalls the previous (older) entry into the
input line; `Down` recalls the next (newer) one; `Down` past the newest
entry restores whatever unsent draft you had been composing before the
first `Up`, rather than leaving the input blank. A recalled entry is
ordinary `input`/`cursor` state — it is immediately editable inline, with
no separate "recalled" mode to escape first, and re-submitting it (`Enter`)
pushes the edited text as a brand-new history entry.

`Up`/`Down` are contended keys, resolved in a fixed priority order in
`input.rs::handle_normal_key`:

1. the `/` command palette, if it is showing (arrow-select a candidate
   command);
2. the on-demand `/agents` panel, if it is open (move the row selection);
3. a multi-line draft's own interior lines — `Up` moves the cursor to the
   line above (unless already on the first line), `Down` to the line below
   (unless already on the last line) — so a recalled or freshly-typed
   multi-line entry stays reachable line-by-line;
4. history recall (the lowest-priority fallback).

Because 1–3 all return before ever reaching step 4, history recall cannot
fire while the palette or the panel owns the key, and `Up` on the first
line / `Down` on the last line of a single- or multi-line draft always
falls through to history — by construction, not a separate guard flag.

History is **persisted** to `~/.conway/history` (or
`$XDG_CONFIG_HOME/conway/history` when that's set — the same directory
`settings.json` itself resolves to, just a different filename), loaded once
at startup and appended to after every submit. It deliberately lives
alongside the *global* config, not the project's own `.conway/` directory —
history follows the user across every project, not the checkout. Entries
are stored one JSON-string per line (not bare newline-delimited text),
since a submitted entry can itself contain embedded `\n` from T8's own
multi-line input — a bare-newline format could not round-trip that.
Writes go through a `.tmp`-sibling-then-`rename` (the same atomic-write
shape `conway-session`'s session index uses), so a crash mid-write can
never leave a half-written history file in place. The history file is
untrusted input exactly like `settings.json` (P-10): a missing, unreadable,
or corrupt file degrades to an empty history — never a panic, never a
startup failure — and a corrupt individual line is skipped rather than
discarding the whole file; a failed history *write* is swallowed and never
fails the submit that triggered it.

**Configuration** — `[tui.history_size]` is an optional integer (default
500, clamped to `1..=100_000` with a fallback to 500 on a
missing/out-of-range/bad value — P-10). `CONWAY_TUI__HISTORY_SIZE=1000`
overrides via env.

```toml
[tui]
history_size = 1000
```

**Bracketed paste.** Crossterm only emits `Event::Paste` when bracketed
paste mode is enabled on the terminal — `tui/mod.rs`'s terminal setup now
does that (`EnableBracketedPaste`, alongside entering the alternate
screen), and `restore_terminal` disables it again (`DisableBracketedPaste`)
on every exit path, the same best-effort discipline already used for raw
mode and the alternate screen. `app.rs`'s run loop inserts the whole pasted
string as **one edit** at the cursor (`input::handle_paste`) — not a loop
that re-enters the key handler once per character, which is what used to
happen before bracketed paste was enabled at all (the terminal fell back to
sending a paste as an ordinary flood of individual keystrokes, each one
separately mutating `AppState`/the palette/etc.).

**The cursor-clamp fix.** The input box's rendered cursor column used to be
clamped to `area.width - 2` regardless of the draft's true length — for a
line longer than the box, the cursor visually froze at the right edge while
the actual text kept extending invisibly off-screen. Now the box's cursor
line scrolls horizontally: whichever line currently holds the cursor is
rendered as a window ending at the cursor's true position, so the cursor is
always genuinely at the character it claims to be at. A multi-line draft
taller than the box similarly scrolls vertically to keep the cursor's own
line on screen. Every OTHER line (not the cursor's) renders from its own
start and is simply clipped if it overflows the width — only the line
you're actively editing needs to track the cursor.

**Out of scope** (explicitly, per the item spec): fuzzy autocomplete,
arg-parameter hints, undo/redo, and vim/emacs editing modes.

### Permission modes and pattern grants (V2)

Approving every command individually does not scale — a real session can
produce hundreds of prompts. Conway now has three modes and a pattern
grant, and the mode is always visible in the status line.

**Modes.** `prompt` (the default, and Conway's original behavior) asks
about every distinct call. `plan` allows only non-mutating tools and
denies the rest outright. `AUTO-ALLOW` allows everything without asking.
Switch modes from `/settings`; that is also the escape hatch out of an
over-broad mode, mid-session and without a restart.

The status line names `plan` and `AUTO-ALLOW` but deliberately stays quiet
about `prompt`. Labelling the ordinary case every frame trains the eye to
skip the field, which is the wrong reflex for the one case that matters.

**Plan mode is defined on the tool's declared category, not on the command
text.** `read`, `grep`, and `glob` declare themselves `Read`/`Search`, so
they run; `bash` declares `Execute` no matter what it is handed, so
`bash cat file` is blocked even though it only reads. That is deliberate:
deciding otherwise would mean parsing shell, and a parser that is wrong
once is a hole. A category Conway does not have yet is blocked, not
allowed.

**Pattern grants** let one approval cover a family of commands:

```
bash:git status   → "git status", "git status --short"
bash:git          → any git subcommand
read:*            → any read
```

A pattern is a **prefix**, matched on whole arguments — not a regex. Regex
was rejected on purpose: `bash:git .*` reads as tight, but `.` matches `;`,
so it would authorize `git status; <anything>`. A prefix is predictable by
reading, and matching token-wise means `git status` covers
`git status --short` without covering `git statusfoo`.

**The rule that makes prefixes safe.** `git status && <anything>` starts
with `git status`. So a pattern grant applies **only when the command
contains no shell metacharacters** — `;`, `&`, `|`, backtick, `$`, `<`,
`>`, parentheses, braces, or a newline. A chained or substituted command
always re-prompts, no matter what patterns exist. This is checked before
any prefix comparison, so there is no path from a pattern to an allow that
skips it.

The gate is deliberately over-eager: a command with a harmless pipe still
re-prompts. An unnecessary prompt costs a keystroke; a missed one costs
arbitrary execution.

**Grants inherit to subagents** via the existing `AgentSubtree` scope — a
grant made by a parent covers its descendants, and does not leak sideways
to an unrelated agent.

**Persistence.** Rules live in `.conway/permissions.json`, resolved
project-first then global, as a flat list of wire-form strings so the file
can be read and diffed by a human:

```json
{ "allow": ["bash:git status", "read:*"] }
```

A corrupt or unreadable file **fails closed** — it authorizes nothing, and
the worst outcome is extra prompting. A malformed individual entry is
dropped rather than guessed at; the rest of the file still loads.

**Granting from the prompt.** When a command is proposed, the permission
prompt offers `[p]` alongside allow-once and deny — and states in words
what accepting would grant, before you press anything:

```
[y] once  [a] always  [p] pattern  [n] deny  [Esc] deny w/ feedback
  [p] grants: `bash` commands starting with `git status`
```

The offered prefix is **two tokens** — `git status`, not `git`. That is
deliberate: `git` alone would silently include `git push --force`, and an
operator skimming a prompt could accept the broad grant believing they got
the narrow one. A single-token command (`pwd`) offers just that token.

`[p]` does not appear at all for a command carrying shell metacharacters,
since the gate would refuse to honor such a grant anyway and offering one
would be confusing.

Want something broader? Edit `permissions.json` by hand. That asymmetry is
the point — granting more should take deliberate effort, granting less
should be the default. You can always grant again; you cannot un-authorize
what already ran.

**Loading and scope.** Rules load at startup from both files and **merge**.
They answer different questions: a global rule is "I always allow this,
everywhere" (`read:*`), a project rule is "this checkout's build command is
fine" (`bash:cargo test`). Having the project file silently discard a global
grant would surprise an operator who set one deliberately. New grants are
written to the project file by default, so they can be reviewed in a diff
alongside the code they authorize.

Review the active grants, switch modes, and revoke everything from
`/settings`. Per-rule revocation is not implemented yet — revoke-all is
the current floor.

### The `/` command palette, with arrow-select

`tui/commands.rs` owns the authoritative slash-command parser
(`parse: &str -> SlashCommand`, pure and state-free) and `execute`
(resolves agent-id arguments against the live `AppState` tree and performs
the one facade call each command maps to, through a `Host` seam so
dispatch is testable without a live `Runtime`). `tui/view/palette.rs`
holds a second, independent command table specifically so the palette can
also list `/ask` and `/agents` (handled directly in `app.rs`, never
reaching `commands.rs`'s parser) — a disclosed, intentional duplication
rather than an oversight.

Typing `/` shows every command; each further character narrows the list
live, since the palette's `matches` filter runs fresh on every render
against the live `AppState::input` — there is no separate "palette is
open" flag that could fall out of sync with what's actually typed.

**Arrow-select** (0.2.0): `AppState` tracks an optional palette-selection
index — `None` means "not navigating yet." The first `Down` press lands on
the first match and autofills the input with it; the first `Up` lands on
the last match; further presses wrap. Autofilling the whole matched
command on each arrow press does not shrink the candidate list to that one
entry — the palette keeps showing every match consistent with what was
typed *before* navigation started, so arrowing through candidates and
retyping stay independent. Arrow keys are contextual: they drive palette
navigation when the palette is open, and otherwise scroll the agent panel
when *it* is open — palette navigation takes priority when both could
apply. The selection highlight itself is a plain reversed-style row (no
box-drawing), consistent with the rest of the single-column redesign, and
is covered by dedicated render-layer tests asserting the highlighted row
is reachable and correctly styled.

### The `/help` keybinding overlay (T7)

Before this item, `/help` dumped a static command list
(`commands.rs::HELP_LINES`, now removed entirely) into the transcript as a
pile of `Entry::Notice` lines — spamming the conversation with content that
already lived in the `/` command palette above, and there was no keybinding
reference anywhere. `/help` now opens a read-only overlay
(`tui/view/help.rs`) instead and pushes **zero** transcript entries;
`AppState::open_help` is a pure flag flip (`AppState::help_open`), nothing
more.

**Keybindings only.** Every genuine slash *command* (`/steer`, `/fork`,
`/spawn`, `/ask`, `/agents`, `/settings`, `/resume`, `/quit`, ...) stays
exclusively in the `/` palette — the overlay never lists one, so the two
surfaces can never drift into duplicating each other. `/thinking` and
`/timestamps` used to be the one exception (syntactically commands,
functionally keyboard-driven view toggles); V4 removed both in favor of
`/settings`, a genuine command like any other, so the "keybindings only"
rule now holds with no carve-out.

The overlay groups every binding the TUI actually has: input & editing
(`Enter`, `Alt-Enter`/`Shift-Enter`, `Left`/`Right`, `Backspace`, `Ctrl-W`,
`Ctrl-D`, `Ctrl-C`), history & navigation (`Up`/`Down`, `Home`/`End`,
`PageUp`/`PageDown`), tools & display (`Ctrl-E`), the settings menu's own
keys (`Up`/`Down`/`Enter`/`Left`/`Right`/`Esc`, live only while `/settings`
is open — see below), the modal-only keys for the `/ask` modal /
intent-confirm card / permission prompt (each only live while that surface
is up), and the agent panel's `v`/`Esc`. It closes a trailing note
explaining that **mouse
wheel scrolling is deliberately not a Conway binding** — Conway never calls
`EnableMouseCapture` and has no `MouseEventKind` handler anywhere in this
crate, so the wheel scrolling you see is your terminal emulator's own
scrollback, not something Conway captures (capturing it would disable the
terminal's native click-drag text selection, the very mechanism the
transcript's clean-copy guarantee exists to protect — see the T6 section
above). `PageUp`/`PageDown` and `Home`/`End` are the in-app equivalents. The
note is deliberately prose, never a keybinding *row*, so a future
well-meaning "mouse: scroll" row can't sneak back in as if it were a real
binding — a dedicated test scans the overlay's row data directly to guard
this.

**No hotkey opens it.** Conway is always in input-typing mode, so a bare
printable key (`?`, `F1`, ...) can never be a binding — `/help` is the only
way in, and `Esc` is the only way out.

**Shape and stacking.** The overlay follows the permission/`/ask`/
intent-confirm overlays' shape exactly: `Clear` + a bordered `Block` drawn
over the transcript area, exempt from the transcript's own clean-copy
guarantee (it is a modal, not conversation text). Unlike those three it is
**not** a `Mode` variant — `AppState::help_open` is a plain flag, since the
overlay is a passive reference with no decision the user owes an answer to,
unlike a blocked tool call, an unfated `/ask`, or an unconfirmed classified
intent. `view::draw` gates the overlay on `help_open && mode == Normal`, and
`input::handle_key` gates its own key-swallowing (everything but `Esc`/
`Ctrl-C`/`Ctrl-D`) the same way. This gives "never stacks on an active
decision" for free: `offer_prompt`/`offer_ask_modal`/`offer_intent_confirm`
all move `mode` away from `Normal` the instant one of those three surfaces
arrives, regardless of `help_open` — the overlay simply stops being
drawn/reachable the moment that happens, with no separate park/promote path
needed, and it reappears on its own once `mode` returns to `Normal` (nothing
ever resets `help_open` on the other three surfaces' account). A `/help`
submission can only ever reach `open_help` while `mode` is already `Normal`
in the first place, since the input line is inert while any of the other
three surfaces owns `mode`.

New theme slots: `help_border` (blue, bold) and `help_key` (bold, no
color — V7 dropped the green; see the palette rationale above), both
configurable under `[tui.theme]`.

### The shared modal primitive: bottom-anchored, content-sized, capped (V1)

Before this item, the permission prompt, the `/ask` modal, the NL
intent-confirm card, and `/help` each hand-rolled their own `Rect`/border/
`Clear`/footer-split logic — four independent copies of roughly the same
shape, and the permission overlay's own doc read *"claim nearly the whole
transcript area,"* which is exactly what made it feel wrong: a modal that
always ate the screen regardless of how little it had to say, and never
scrolled a long command past its own edges.

`tui/view/modal.rs` is now the ONE place that decides a modal's `Rect`:
**bottom-anchored, sized to its own content, capped at a maximum**, so a
short modal renders short and a long one grows only to the cap and then
**scrolls** rather than either truncating or filling the screen. All four
surfaces above call `modal::draw_modal_frame` for their border/`Clear`
treatment and `modal::body_max_scroll`/`modal::clamp_scroll` for their
scroll math.

**The cap** is `transcript_area.height / cap_denominator`, a per-caller
parameter rather than one global number — `modal::DEFAULT_CAP_DENOMINATOR`
(`2`) is what the three DECISION-owed surfaces (permission prompt, `/ask`
modal, intent-confirm card) use; `/help`, being informational rather than a
decision the user owes an answer to, passes its own larger cap (`1`, i.e.
up to the whole `transcript_area`) since its binding list is genuinely long
and the user opened it on purpose to read. (The first cap tried here
reused `/agents`' own `area.height / 3` — the user had named that panel as
the feel they liked — but `/agents`' fraction is measured against the WHOLE
frame while a modal's is measured against the already-shrunk
`transcript_area`, and reusing the same fraction there proved too tight in
practice: it left as few as two body rows for a modal on an ordinary 80x24
terminal.)

**Scrolling replaces the old `permission_scroll` field.** `AppState` now
has one shared `modal_scroll: u16`, since at most one of the four
modal-bearing surfaces is ever showing at a time (the three `Mode` variants
are mutually exclusive, and `/help` never stacks on them either — see the
T7 section above). `PageUp`/`PageDown` drive it in all four
`input::handle_*_key` fns, and it resets to `0` every time a NEW surface
becomes the active one (`AppState::offer_prompt`,
`AppState::promote_next_surface`, `AppState::offer_ask_modal`,
`AppState::offer_intent_confirm`, `AppState::open_help`) so a leftover
scroll position from a previous, unrelated surface's content never carries
over.

**`/agents` stays a panel, not a modal.** The item that introduced this
primitive asked for a justified answer either way. `/agents` is meant to be
browsed *while still composing* — it shares the screen with a live input
line, which a modal (drawn *over* the transcript, per this primitive's own
shape) cannot do without contradicting itself. A modal is for a decision or
a temporary read-only reference; `/agents` is an ambient, side-by-side
view, which this primitive is not shaped for. What carries over is the
*feel*, not the literal conversion: bottom-anchored, bordered, never eating
the whole screen.

**A tree/menu navigation primitive** (`tui/view/menu.rs`) is layered on top
of the modal: `MenuNode::Leaf`/`MenuNode::Group` for nested, collapsible
sections, and `MenuState` for arrow navigation (`move_selection`),
expand/collapse (`toggle_group_at_selection`), and resolving the current
selection down to an opaque leaf `id` (`selected_leaf_id`) through the
visible, flattened row list — mirroring how `/agents`' own filtered-row
lookup already works. It landed unwired but fully exercised by its own
tests (including nested-group navigation), and V4's `/settings` menu (below)
is its first real caller.

No new theme slots were needed for the primitive itself — it takes a
caller-supplied `Style` for its border (each ported surface keeps its own
`theme.border_danger`/`border_warning`/`border_accent`/`help_border`) and
reuses the existing `theme.selected`/`theme.emphasized`/`theme.dim` slots
for the menu primitive's highlighted/group/leaf rows, the same way
`view/agents.rs` already does.

### The `/settings` menu (V4)

`/thinking` and `/timestamps` used to be two standalone slash commands,
each owning exactly one boolean. That doesn't scale — every future display
preference would mean another slash command competing for footer/palette
space. `/settings` replaces both with one menu, built on the modal + menu
primitives above.

**Content.** Three settings, surveyed from `AppState`/`TuiSection` for
genuinely user-facing display preferences — not swept from "every bool that
happens to live on `AppState`" (internal bookkeeping like scroll offsets or
in-flight flags isn't a setting, and settings that already have a
dedicated, better-fitting binding — `v` for `/agents`' visibility filter,
`Ctrl-E` for tool-output expand — stay there rather than gaining a second,
redundant home here):

- **show reasoning traces** (was `/thinking`) — `AppState::show_reasoning`.
- **show timestamps** (was `/timestamps`) — `AppState::show_timestamps`.
- **tool preview lines** — `AppState::tool_preview_lines` (T5's fold cap,
  previously config-only; this menu is the first place it's reachable at
  runtime at all).

**Grouping.** Two `MenuNode::Group`s — "display" (the two booleans) and
"tool output" (the one numeric setting) — the shape a third settings
category would extend later, not nesting invented just to exercise the
primitive.

**The numeric setting.** `tool_preview_lines` is the one non-boolean, so it
needs its own interaction: `Left`/`Right` step it by ±1 rather than cycling
a fixed preset list. Every value in `1..=200` is an equally plausible
answer to "how many lines before folding" — there's no natural
"meaningfully different" preset set the way a theme picker would have — so
a continuous stepper lets the user land on exactly the number they want.
Stepping floors/caps at the boundary (`AppState::adjust_tool_preview_lines`)
rather than reusing `clamp_tool_preview_lines`'s config-validation fallback
directly: that function's job is "malformed config value → fall back to the
default (3)", and applying that same fallback to an interactive stepper
would make pressing `Left` at the floor (1) bounce **up** to 3 instead of
simply stopping — confusing for a live control. Both functions still share
one range constant (`TOOL_PREVIEW_LINES_RANGE`) so the bound itself can't
drift between them; only the out-of-range *behavior* differs, matched to
what each caller needs.

**Session-only, disclosed in the UI.** Conway's config load
(`conway::config::merge::load`) is a five-source layered read with no
writer anywhere outside test fixtures — persisting a runtime toggle would
mean inventing one, and "which layer gets written" (default/XDG/
project/env/CLI) has no good default answer; that question is out of this
item's scope. The menu changes `AppState` at runtime only, exactly as the
two removed slash commands already did. A footer note
("changes apply to this session only") says so on every render, and the
one leaf with a real backing config key names it inline: "tool preview
lines — 3 (Left/Right to adjust; persists via `[tui.tool_preview_lines]`)".
The two booleans have no `[tui.*]` config-key equivalent today, so they
carry no such annotation — the disclosure only claims what's true.

**Interaction.** `Enter` toggles a boolean leaf or expands/collapses a
group (mirrors `/agents`' own Enter-to-activate shape); the numeric leaf
ignores `Enter` (nothing to activate — it has its own `Left`/`Right`
stepper). `Esc` closes.

**Fresh tree every call.** `view/settings.rs::build_tree` rebuilds a fresh
`MenuState` — with each leaf's label baked from the CURRENT `AppState`
values — on every render *and* every keypress, rather than mutating one
long-lived tree in place: a stored tree would go stale the instant a toggle
changed the very value its own label displays, and `menu.rs` deliberately
exposes no "relabel this one leaf" mutator (it doesn't know what a leaf's
opaque `id` means). Only the cursor (`AppState::settings_selected`) and
which groups are collapsed (`AppState::settings_collapsed_groups`, keyed by
label) persist across calls — `MenuState::set_selected` (a V4 addition to
`menu.rs`) restores the cursor onto each freshly built tree.

**Shape and stacking.** Informational, gated exactly the way `/help` is: a
plain `AppState::settings_open` flag checked ahead of the `Mode` match in
`input::handle_key`, never a `Mode` variant, so it can never stack on an
active permission prompt / `/ask` modal / intent-confirm card. New for V4:
`/settings` and `/help` are ALSO mutually exclusive with each other
(`AppState::open_settings`/`open_help` each close the other) — both are
gated the identical way, so without this, whichever one this crate's fixed
check order saw first would silently be the only one reachable while the
other sat open in the background. While `/settings` is open it owns
`Up`/`Down`/`Enter`/`Left`/`Right` completely (never falling through to
V3's palette/agent-panel/wheel-scroll priority chain) and releases them the
instant it closes — the same trade the agent panel already makes for its
own arrows.

### The `/ask` single-turn modal

`/ask <prompt>` asks an ephemeral fork of the current session a side
question: the child attaches as a proper fork child (visible in `/agents`
with an `(ephemeral)` marker while it runs), inherits the session's full
context and tool set, and runs exactly one turn. When the answer is ready,
a **modal overlay** opens over the transcript showing the question and the
child's reply — the 0.2.0 rendering of `/ask` as a dimmed aside inline in
the transcript is gone; the answer is no longer part of the copyable
conversation until the user says so.

Closing the modal **forces exactly one fate** — there is no fourth way out:

- `[f]` **fork** — `Conway::promote`: the child becomes a persistent
  session in its own right (its `/agents` node stays and loses the
  `(ephemeral)` marker via the `AgentPromoted` event).
- `[p]` **pull in** — `Conway::pull_in`: the question and answer merge
  into the parent's own transcript (the question re-stamped
  `Provenance::MergedAsk`), and the child is purged.
- `[esc]` **discard** — `Conway::purge`: the child is deleted outright,
  merging nothing.

A fate that fails (e.g. a refused pull-in) keeps the modal open with the
error shown in-modal — the user still must choose; a failed fate never
silently falls through to another one. Quitting with the modal open
(`Ctrl-D`, double `Ctrl-C`) **is** the discard fate: the child is purged
before the process exits. While the modal is open the input line is inert
and `/agents` is neither visible nor available (the mode swallows the
panel toggle) — a panel that was open returns, unchanged, once a fate
closes the modal. One ask at a time: a second `/ask` while one is in
flight is refused with a notice.

Because a crashed or killed TUI leaves no modal that will ever show the
answer, the TUI runs a **crash-residue sweep** once at startup
(`Conway::sweep_stale_modal_asks`): leftover ephemeral sessions created by
this modal path are purged (nothing is live yet at startup). `conway_ask`
*tool* children are never swept — a new `ask_origin` tag on the session
header (`modal_ask` vs `tool_ask`) tells the two ephemeral-ask paths
apart, and a tool child's transcript is referenced by an
`EphemeralSessionRef` artifact that would dangle. See
[`conway`](conway.md)'s `/ask` section for the facade ops themselves.

### NL intent on `/fork` and `/spawn` with a confirmation card

`/fork <free text>` and `/spawn <free text>` (free text that does NOT
start with explicit `@<agent_def>` syntax) are run through the facade's
NL intent classifier (`Conway::classify_agent_intent`, C1) BEFORE any
agent is created, and the classified result is shown in a
**confirmation card** — a modal overlay over the transcript — so
inference can never silently choose (P-10: classified output is untrusted
until the user confirms). The card shows the classified `recipe`
(`fork`/`spawn`), the `agent_def` (or `(inherit)` when `None`), and the
`prompt` the classifier produced (or the user's raw text on the verbatim
passthrough), and forces exactly one choice:

- `[enter]` **confirm** — run the classified recipe as-is: fork or spawn
  with `intent.agent_def` (for spawn; a fork ignores a classifier-returned
  def — `ForkSpec` has no agent_def field) and `intent.prompt` as the
  first message. The recipe may have been cross-classified (user typed
  `/fork`, classifier said `spawn`).
- `[e]` **edit** — drop the classified `prompt` (not the raw text) into
  the input line and close the card; the user edits and submits normally.
- `[esc]` **manual** — fall back to today's pre-classification flow with
  the original raw text (untouched) under the original command's default
  recipe.

`Ctrl-C`/`Ctrl-D` pass through while the card is open — quitting with the
card open IS the manual fallback (no agent has been created yet, so
there is nothing to purge, unlike the `/ask` modal). While the card is
open the input line is inert and `/agents` is neither visible nor
available, exactly like the `/ask` modal. One card at a time: a card
arriving while a permission prompt or an `/ask` modal is showing parks
behind it and opens once that surface clears.

The verbatim **passthrough** path (unconfigured `[roles.intent]` role,
unparseable reply, invalid recipe, or empty prompt) is NOT an error —
the card still opens with the raw text as the prompt and no agent def,
and the user confirms it as-is. A real backend failure propagates as
`ConwayError::IntentClassification`; the card does NOT appear for a hard
error, and the command falls back to today's manual flow with a notice.

**Configuration requirement:** the classifier routes through the
declarative `intent` role alias, which must be configured in
`settings.json` under `roles.intent` (see the snippet in
[`conway`](conway.md)'s `intent` module doc / `crates/conway/src/intent.rs`).
With no `[roles.intent]` entry, classification degrades to the verbatim
passthrough described above — no session is ever created for it.

**Unchanged paths:** explicit `@<agent_def>` syntax
(`/fork @<agent> <directive>`, `/spawn @<agent_def> <prompt>`) and bare
invocations (`/fork`, `/spawn` with no text) skip inference entirely and
behave exactly as before — the card never appears for them. Oneshot
(`-p`) NL intent classification is deferred (out of this epic); the
`-p` `/fork`/`/spawn` paths are unchanged.

## How it fits the whole

`conway-cli` depends on [`conway`](conway.md) and nothing else in the
workspace — it has no dependency on `conway-core`, `conway-runtime`, or
any backend/tool crate directly, mechanically enforced by a
`no_forbidden_deps` test. It is the outermost consumer in the dependency
graph; see [`/ARCHITECTURE.md §2`](/ARCHITECTURE.md) for the full
dependency diagram.
