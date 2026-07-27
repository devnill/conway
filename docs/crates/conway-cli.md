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
