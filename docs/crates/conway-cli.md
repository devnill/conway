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
unit-tested independently instead.

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
  visibility filter — **active-only** (the default: terminal-status agents
  hidden), **all**, **finished-only** — without ever mutating the tree
  itself; the header names the current mode. Ordinary subagent lifecycle
  is *also* surfaced inline in the conversation stream itself; this panel
  is for browsing the whole tree at a glance, not the only place activity
  shows.

  `/tree` still parses but is demoted to a **hidden alias** (dropped from
  `/help` and the palette): it renders the same content as the panel —
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

**Named styles** (each defaults to the pre-theme inline style; new accent
styles have no pre-theme call site and are defined for later v0.3.0 items
to consume):

| Name | Default | Used by |
| --- | --- | --- |
| `user` | bold, no fg | transcript `you>` prefix |
| `assistant` | unstyled | transcript assistant text body |
| `assistant_marker` | magenta + bold | (T4) assistant `[modelname]>` speaker marker |
| `reasoning` | dark_gray + italic | (T4) `Entry::Reasoning` trace body + `thinking>` prefix |
| `timestamp` | dark_gray | (T4) `HH:MM ` per-entry timestamp prefix (`/timestamps`) |
| `tool_proposed` | gray | tool-call tag, `Proposed` |
| `tool_awaiting` | magenta | tool-call tag, `AwaitingPermission` |
| `tool_running` | yellow | tool-call tag, `Running` |
| `tool_done` | green | tool-call tag, `Finished` (ok) |
| `tool_failed` | red | tool-call tag, `Finished` (error) |
| `agent_marker` | blue + bold | *(NEW, T4 will consume)* generic agent marker |
| `agent_starting` | gray | agent-tree/transcript marker, `Starting` |
| `agent_running` | yellow | agent-tree/transcript marker, `Running` |
| `agent_awaiting` | magenta | agent-tree/transcript marker, `AwaitingPermission` |
| `agent_finished` | green | agent-tree/transcript marker, `Finished` |
| `agent_failed` | red | agent-tree/transcript marker, `Failed` |
| `agent_cancelled` | dark_gray | agent-tree/transcript marker, `Cancelled` |
| `notice` | cyan | transcript `Entry::Notice` |
| `error` | red | `/ask` modal error line |
| `fatal_error` | red + bold | *(NEW, no pre-theme call site)* fatal-error accent |
| `dim` | dim modifier | agent-tree recipe labels, input-box placeholder |
| `focused` | bold modifier | agent-tree `(focused)` tag |
| `selected` | reversed modifier | agent-tree arrow-selected row highlight |
| `emphasized` | bold modifier | modal-overlay body lines (command, `you asked:`, `recipe:`) |
| `border_normal` | unstyled | input-box / agent-panel block borders |
| `border_warning` | yellow + bold | `/ask` modal border |
| `border_danger` | red + bold | permission-prompt modal border |
| `border_accent` | cyan + bold | NL intent confirmation card border |
| `status_mode` | reversed modifier | the bottom status line |
| `status_dim` | dim modifier | status-line dim accent (T2: the `elapsed · +tokens` tail of the working indicator) |
| `spinner` | yellow | activity spinner accent (T2: first color of the pulse palette) |
| `spinner_b` | light_yellow | *(NEW, T2)* second color of the spinner pulse palette |
| `spinner_c` | white | *(NEW, T2)* third color of the spinner pulse palette |

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
`activity` field overlays its T2 spinner pulse color and dim
elapsed/tokens tail, and the `hint` field overlays `theme.status_dim`.

**Default Lean line:** `mode | model | ctx | tokens | activity | hint`
(`model` is omitted until the first turn routes, so a brand-new session
shows `mode | ctx | tokens | activity | hint`).

**Configuration** — `[tui.status_line]` has one key, `fields`: an ordered
list of field names to render. A field absent from the list is hidden; the
list order is the render order. Unknown names are dropped at render time
(P-10: config is untrusted input, never a panic). An empty/empty-after-
validation list falls back to the default Lean order rather than rendering
a blank line. `CONWAY_TUI__STATUS_LINE__FIELDS=mode,model,ctx,tokens,activity,hint`
overrides the order via env (comma-split).

```toml
[tui.status_line]
fields = ["mode", "model", "ctx", "tokens", "git", "activity", "hint"]
```

**Available fields** (all orderable):

| Field | Renders | Source |
| --- | --- | --- |
| `mode` | `ready` / `awaiting permission` / `ask` / `intent` | `AppState::mode` |
| `model` | the serving model display name, e.g. `anthropic/claude-sonnet-4-6`; omitted before the first turn routes | `Event::ModelDecision { chosen }` |
| `ctx` | `ctx 42%` when the focused model's max context is known, else `ctx 12.3k` (raw tokens, compact-suffixed; capped at `ctx 100%`) | cumulative `Event::ContextSegmentAdded { tokens_est }` ÷ the focused model's `max_context_tokens` from `[models.metadata_path]` |
| `tokens` | `<total> tok (<n%> cached)` when cache data is present, else `<total> tok` | cumulative `Event::TurnFinished { usage }` (`Usage`'s input + output + both cache dimensions + reasoning); `n%` = `cache_read / (input + cache_read + cache_write)`, omitted when the denominator is 0 or `cache_read` is 0 |
| `activity` | T2's working indicator: `⠋ thinking… 12s · +45 tok` while active, `idle` while idle (spinner + phrase pulse via `Theme::spinner_palette`; `+{n} tok` is session-deduped new-segment tokens added this turn) | `AppState::activity` + T2 counters |
| `hint` | a persistent keybinding/affordance hint, dim: `Enter submit · Ctrl-E expand · ↑↓ history · PgUp/PgDn · /help · /thinking · /timestamps · /agents to {view\|hide}`, plus `focused: <id>` when the transcript is focused on a non-root agent | static + `AppState::agent_view_open`/`focused_agent` |
| `git` | the current branch (e.g. `main`); omitted when not a git repo, git is absent, or the command fails | one-shot `git rev-parse --abbrev-ref HEAD` at startup, no polling |
| `cwd` | the session's working directory; omitted when unset | `Cli --cwd` or `config.cwd` |

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
word share one pulse color per frame, cycling through a small theme
palette (`spinner`/`spinner_b`/`spinner_c` via `Theme::spinner_palette`)
on each 125ms (8 TPS) animation tick — a subtle element-level contrast
shift, not per-character `TextShimmer` (out of scope).

The 125ms tick is additive to the existing 16ms redraw cap
(`REDRAW_TICK`); it advances `AppState::spinner_frame` (modulo the
10-glyph braille sequence `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) and `AppState::spinner_color_idx`
(modulo the palette length) and marks the frame dirty **only while
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
earlier `Ctrl-E submit` hint — Enter was and remains the actual submit
key; T8 will move submit to Alt/Shift-Enter).

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
  (`AppState::show_reasoning` defaults `true`). The `/thinking` slash
  command toggles `show_reasoning`; when off, `build_lines` skips
  `Entry::Reasoning` entirely (the entries are still stored, so toggling
  back on restores them without replay). The keybinding is advertised in
  the status-line `hint` and `/help`.
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
  time. The `/timestamps` slash command toggles
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

`/thinking` and `/timestamps` are intercepted in `app.rs::submit`
(mirroring `/agents`'s pattern) — state-only toggles that never reach
`commands::parse` and are never sent to the model. Both are listed in the
`/help` overlay (`commands.rs::HELP_LINES`), the command palette
(`view/palette.rs::COMMANDS`), and the status-line hint.

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
