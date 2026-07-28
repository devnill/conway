# Changelog

All notable changes to **conway** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Kimi coding-plan support, and Anthropic-compatible endpoints
  generally.** Kimi's coding plan is served over an Anthropic-shaped
  `/v1/messages`, so it needs no dedicated adapter — point `base_url` at
  `https://api.kimi.com/coding/` with `kind = "anthropic"`. Endpoints
  under a path prefix now have that prefix preserved (`.../coding/` →
  `.../coding/v1/messages`), pinned by a test so a future refactor cannot
  silently drop it.

  `AnthropicConfig` gained an optional `id`, defaulting to `"anthropic"`
  so existing configs are unaffected. Previously `AnthropicBackend::id()`
  was hardcoded, which forced any Anthropic-kind backend to occupy the
  config key `"anthropic"` — you could not name a backend `kimi`, and you
  could not configure Kimi and Anthropic at the same time. Both now work;
  the build-time key check that enforced the old constraint is removed.

  Bundled model metadata gains `k3-256k` (262,144 tokens) and `k3[1m]`
  (1,048,576 tokens), so the status line's `ctx%` is accurate rather than
  falling back to raw counts. The literal brackets in `k3[1m]` are part of
  the provider's model id; a test pins that they survive TOML parsing.

  See `docs/crates/conway-backends.md` for a copy-pasteable config, which
  is itself pinned by a test that loads it through the real config loader.

### Changed

- **Config no longer inspects the shape of an API key.** The
  `sk-ant-oat*` prefix rejection is removed from all three layers that
  enforced it (`AnthropicConfig::validate`, `config::merge::validate`, and
  `ConwayBuilder`'s `api_key_env` resolution), along with the
  `ConfigError::SubscriptionTokenRejected` variant. Any non-empty key is
  now passed through to the configured `base_url` as-is.

  Policing which credentials look legitimate is an opinion that does not
  belong in the core, and it blocked a real use case: an
  Anthropic-compatible third-party endpoint (a coding-plan subscription, a
  self-hosted shim) could not be configured, and the resulting error
  misdirected the user to `console.anthropic.com` — the wrong vendor
  entirely. Whether a key works is the provider's answer to give, and its
  auth error is more accurate than any prefix match Conway could perform.

  **Unchanged:** an empty or whitespace-only `api_key` is still rejected
  (`ConfigError::MissingApiKey`), and an `api_key_env` naming an unset
  variable is still a hard error that names the variable. Those describe a
  missing credential, which Conway can identify precisely, rather than
  judging one it has.

### Added

- **TUI: tool output folding + expand (T5).** A settled tool entry's
  preview in the transcript now renders **folded** by default: the first
  `[tui.tool_preview_lines]` physical lines (default 3) plus a dim
  `… (+M lines, Ctrl-E to expand)` affordance naming how many lines are
  hidden, instead of spilling the entire preview inline with no bound.
  `Ctrl-E` flips `expanded` on **every** tool entry at once (MVP — no
  transcript-cursor/selection state, so expand/collapse is all-at-once);
  an expanded entry renders its full preview. The stored `preview` is
  never truncated — the cap is render-time only, so toggling never loses
  data. The toggle is pure state mutation: `Ctrl-E` does NOT touch
  `scroll` or `follow_tail`; the next render's existing clamp
  (`state.scroll.min(max_scroll)`) re-clamps to the nearest valid
  position without snapping the viewport. `Ctrl-E` is a control key
  (not a bare `e`, which stays ordinary text input for the always-on
  input box), bound directly to
  `AppState::toggle_all_tool_entries_expanded` (mirroring the `v`
  visibility-filter key's direct-mutation pattern — no `Action` variant,
  no facade side effect). Settled tool output honors the clean-copy
  invariant: no box-drawing, no `Block` — the entry ends with a blank
  line + a dim plain `-` rule as a non-box separator. New config key
  `[tui.tool_preview_lines]` (optional integer, default 3, clamped to
  `1..=200` with a fallback to 3 on bad input — P-10, never a panic);
  `CONWAY_TUI__TOOL_PREVIEW_LINES=10` overrides via env. The
  `Entry::Tool::expanded` flag and the `tool_lines` collapsed/expanded
  render branch are generic for T4's tool-args reuse. The status-line
  `hint` field now advertises `Enter submit · Ctrl-E expand` (reconciling
  the earlier `Ctrl-E submit` hint — Enter was always the actual submit
  key; T8 will move submit to Alt/Shift-Enter). See
  `docs/crates/conway-cli.md`'s "Tool output folding + expand (T5)"
  section.

- **TUI: transcript provenance — speaker markers, reasoning variant,
  timestamps, tool args/progress (T4).** The transcript now surfaces
  per-entry provenance: reasoning traces (`Event::ThinkingDelta` ->
  `Entry::Reasoning`, dim+italic with a `thinking> ` prefix, **expanded
  by default**; `/thinking` toggles `show_reasoning` and hides them
  entirely while off, but entries are still stored so toggling back on
  restores them without replay); assistant speaker markers
  (`Entry::Assistant` carries the serving model name, rendered as a
  `[modelname]> ` prefix in `theme.assistant_marker`; omitted on replay
  where no model provenance is available); tool args + progress
  (`Entry::Tool` stores `Event::ToolCallProposed::args` as a compact
  JSON string, rendered as a one-line truncated `args: …` preview while
  collapsed and pretty-printed while expanded — both args and output
  expand/collapse together via T5's `Ctrl-E` toggle; accumulated
  `Event::ToolProgress { call_id, note }` notes — previously dropped —
  append to the matching in-flight tool entry by `call_id` and render as
  dim `-> {note}` lines); per-entry timestamps (`/timestamps` toggles
  `show_timestamps`, default off, prepending an `HH:MM ` prefix styled
  with `theme.timestamp` to each entry's first rendered line); and a
  turn-end summary (`Event::TurnFinished` stamps `{elapsed} · {tokens}
  ({n%} cached)` — e.g. `1m 6s · 1.4k tok (88% cached)` — onto the last
  assistant/reasoning block, rendered as a final dim line). The
  streaming cursor (T2) extends to the live reasoning line while
  `activity == Thinking`. New theme slots `assistant_marker`,
  `reasoning`, and `timestamp` (defaults: magenta+bold, dark_gray+italic,
  dark_gray) are configurable via `[tui.theme]`. `/thinking` and
  `/timestamps` are intercepted in `app.rs::submit` (state-only toggles,
  never sent to the model), listed in `/help`, the command palette, and
  the status-line hint. Settled `entry_lines` output honors the
  clean-copy invariant (no box-drawing glyphs). See
  `docs/crates/conway-cli.md`'s "Transcript provenance (T4)" section.

### Changed

- **TUI: status line rework — model + ctx% + cwd + git + field config
  (T3).** The bottom status line is now an ordered, configurable set of
  fields driven by a new `[tui.status_line]` `settings.json` section
  (schema: `conway::config::schema::StatusLineConfig`). The default Lean
  line is `mode | model | ctx | tokens | activity | hint`; `git` and
  `cwd` are also available as orderable fields. Each field renders only
  when listed in the configured `fields` order AND has data to show
  (`model` is omitted before the first turn routes; `git` is omitted
  outside a repo; etc.). Unknown field names are dropped at render time
  — never a panic (P-10). New fields: `model` (the focused agent's
  serving model display name from `Event::ModelDecision`); `ctx`
  (context-window occupancy — `ctx 42%` when the focused model's max
  context is known from `[models.metadata_path]`, else the raw
  cumulative `Event::ContextSegmentAdded` token estimate, compact-
  suffixed as `ctx 12.3k`; capped at `ctx 100%`); `tokens` formalizes
  the cumulative spend slot as `<total> tok (<n%> cached)`, where
  `total` is every `Usage` field summed and `n%` is the cache hit rate
  `cache_read / (input + cache_read + cache_write)` (the parenthetical
  is omitted when the denominator is 0 — divide-by-zero guarded); `git`
  (the current `git rev-parse --abbrev-ref HEAD` branch, read once at
  startup, best-effort, no polling, no new deps); `cwd` (from `--cwd` or
  `config.cwd`). The `activity` field IS T2's working indicator
  (spinner + pulse + elapsed + `+{n} tok`), unchanged; the `hint` field
  is a persistent keybinding/affordance hint (`Ctrl-E submit · ↑↓
  history · PgUp/PgDn · /help · /agents to {view|hide}`, plus
  `focused: <id>` off-root). `AppState::apply`'s previously-dropped
  `ModelDecision` arm now captures the focused model + max context;
  `ContextSegmentAdded` now also accumulates a session-wide cumulative
  context-token estimate (distinct from T2's per-turn
  `turn_running_tokens`). See `docs/crates/conway-cli.md`'s
  `[tui.status_line]` section for the full field table, the
  `tokens (n% cached)` format, and reordering/hiding instructions.

- **TUI: activity spinner + animation tick (T2).** The status line's
  "is it working?" slot now renders a braille spinner glyph plus the
  activity word plus live elapsed seconds plus the new context tokens
  added this turn (`⠋ thinking… 12s · +45 tok`) while the focused agent
  is working. The spinner glyph and the activity word pulse together
  through a small theme palette (`spinner`/`spinner_b`/`spinner_c`,
  defaulting to yellow/light_yellow/white) on a new 125ms (8 TPS)
  animation tick, additive to the existing 16ms redraw cap. The tick is
  gated by `should_animate(activity)` so an idle terminal never pays for
  animation — the counters don't advance and no redraw is forced while
  idle. The elapsed clock starts at `Event::TurnStarted`; the `+{n} tok`
  figure accumulates from `Event::ContextSegmentAdded` deltas —
  session-deduped new-segment tokens added this turn (NOT total context
  occupancy: the runtime emits `ContextSegmentAdded` only for segments
  new to a never-reset `seen_segments` set, so the figure is large on
  turn 1 then small on turn 2+ for the same conversation). The leading
  `+` signals "added this turn" and distinguishes it from the
  cumulative `| {tokens} tok |` slot; the authoritative turn-end token
  total lands via the turn-end summary (T4). New theme slots
  `spinner_b` and `spinner_c` join the existing `spinner` slot to form
  the pulse palette. While `activity == Responding`, the live,
  in-progress assistant line in the transcript also gets a block `▌`
  streaming cursor appended at render time only — never baked into the
  stored `Entry::Assistant` text or into `entry_lines` output for
  settled entries (clean-copy invariant relaxed only for the
  actively-streaming line). See `docs/crates/conway-cli.md`'s "Activity
  spinner + animation tick" section for the full mechanism.

- **TUI: central theme module + named styles (T1).** The TUI's render
  pass now reads colors/styles from a single `Theme` struct threaded
  through `view::draw` and each per-view `draw` fn as `&Theme`, replacing
  the per-call-site `Style::default().fg(Color::…)` the five view files
  used to hand-roll inline. The theme is configurable from the start via
  a new `[tui.theme]` `settings.json` section (per-named-style `fg`/`bg`/
  `modifiers` overrides; defaults match the pre-T1 exact
  `(Color, Modifier)` pairs, so an unconfigured TUI renders identically).
  Malformed overrides fall back to the affected slot's default — never a
  panic (P-10). New accent styles `assistant_marker`, `reasoning`,
  `agent_marker`, `fatal_error`, `status_dim`, and `spinner` are defined
  for later v0.3.0 polish items to consume. See
  `docs/crates/conway-cli.md`'s `[tui.theme]` section for the full named-
  style table and accepted color/modifier spellings.
- **`/ask` is now a single-turn modal with three forced fates.** Asking
  forks an ephemeral child (visible in `/agents` marked `(ephemeral)`),
  runs one turn, and opens a modal over the child's answer. Closing the
  modal forces exactly one choice: `[f]` fork (promote the child to a
  persistent session), `[p]` pull in (merge the question and answer into
  the parent's own transcript, then purge the child), or `[esc]` discard
  (purge outright). Quitting with the modal open discards. A failed fate
  keeps the modal open with the error shown. The 0.2.0 rendering — a
  dimmed aside inline in the transcript — is gone. On startup the TUI
  sweeps modal-`/ask` residue left behind by a crashed process; a new
  `ask_origin` tag on the session header distinguishes these from
  `conway_ask` tool children, which are never swept.
- **TUI `/agents` panel is now the single agent surface.** Every row shows
  the agent's recipe label — `fork @seq N` for forks (with the inherited
  fork point), `@<agent_def>` for spawns with a named agent definition,
  `(inherit)` for spawns that inherited the parent's role/model — and
  ephemeral `/ask` forks are now visible in the tree with an `(ephemeral)`
  marker instead of being omitted. While the panel is open, `v` cycles row
  visibility (active-only by default, all, finished-only) as a draw-time
  filter that never mutates the tree. `/tree` is demoted to a hidden
  alias: it still parses and renders, but its output is derived from the
  same panel tree — the same nodes and recipe labels, shown unfiltered as
  plain-text transcript lines — and it no longer appears in `/help` or the
  command palette.

### Added

- **Facade lifecycle ops for ephemeral `/ask` children** —
  `Conway::promote` (the one-way ephemeral→persistent flip: durable header
  rewrite, live-tree flip, and an `Event::AgentPromoted` for UIs, in that
  failure-ordered sequence), `Conway::pull_in` (merge the child's question
  and answer into the parent's log — the question re-stamped
  `Provenance::MergedAsk`, assistant records verbatim — then purge the
  child), `Conway::purge` (discard a terminal ephemeral child), and
  `Conway::sweep_stale_modal_asks` (crash-residue reaper). Ephemeral `/ask`
  children now attach as proper fork children of the asker, so they appear
  in `/agents` marked `(ephemeral)` while running.
- **`conway_ask` model-facing tool**: runs a prompt in an ephemeral fork of
  the calling agent and returns the child's full reply text (not a truncated
  summary), so the model can compose it into a `conway_subagent` spawn and
  keep curation/context-drafting inference out of the orchestrator's context
  window. Fork-only (`prompt` + optional `budget`); the child is marked
  ephemeral (shown in the TUI `/agents` panel with an `(ephemeral)` marker
  while running, and under the `v`-cycled all/finished views once done;
  excluded from default session listings; still attached to the live agent
  tree for provenance). Composes
  `conway_subagent` per the "exactly two subagent primitives" principle —
  `ask` is fork+await-text, not a third primitive.
- **`conway_ask` gains an optional `tools` arg**: narrows the ephemeral fork
  child's tool set to the named tools (`ToolSelector::Only`, the same
  selector `conway_subagent`'s `tools` arg produces) — e.g.
  `{"prompt": "summarize the diff", "tools": ["read"]}` restricts the child
  to read-only inspection. Narrowing-only: it can restrict, never widen, the
  tool set the child would otherwise inherit.
- **NL intent on `/fork` and `/spawn` with a mandatory confirmation card.**
  Free text after `/fork` or `/spawn` that does NOT start with explicit
  `@<agent_def>` syntax is classified by the facade's `intent` role
  (`Conway::classify_agent_intent`, C1) BEFORE any agent is created, and
  the classified result is shown in a confirmation card
  (`[enter]` confirm / `[e]` edit / `[esc]` manual) so inference can never
  silently choose (P-10). `[enter]` runs the classified recipe as-is
  (possibly cross-classified); `[e]` drops the classified prompt into the
  input line for editing; `[esc]` falls back to today's pre-classification
  manual flow with the raw text untouched. The verbatim passthrough
  (unconfigured `[roles.intent]` role, unparseable reply, invalid recipe,
  empty prompt) still shows the card with the raw text; a hard
  `ConwayError::IntentClassification` does NOT show the card and falls back
  to the manual flow with a notice. Explicit `@<agent_def>` syntax and bare
  invocations are unchanged. Oneshot (`-p`) `/fork`/`/spawn` paths are
  unchanged (deferred).

## [0.2.0] — 2026-07-23

### Changed

- **License: relicensed from Apache-2.0 to AGPL-3.0-only.** conway is now
  covered by the GNU Affero General Public License v3.0 — running a modified
  conway as a network service requires making the modified source available to
  its users. This is a deliberate choice for an agent harness and means conway
  is not intended for use as a permissively-licensed library dependency inside
  closed-source software. See [LICENSE](LICENSE).
- **Unified the two model-capability systems** into a single source of truth:
  the router's context-fit gate and `Backend::capabilities()` now resolve
  through the same path, so a `models.json` value has one predictable routing
  effect instead of silently diverging.
- **Redesigned the TUI** to a single-column, copy-paste-friendly layout
  (conversation stream, input box, status line) with a live `/`-command
  palette and an on-demand agent-tree panel, replacing the always-on paned
  layout that dragged UI chrome into the clipboard.

### Added

- **`ContextHook`** — a pluggable per-call context/tool-curation port: mask
  records, edit the system prompt, filter the announced tool set, or react to
  context overflow. No built-in curation policy and no automatic compaction;
  with no hook registered, behavior is unchanged.
- **Out-of-context record mask** — mark log records to exclude from LLM calls
  while keeping them in the append-only log (reversible).
- **Reasoning support at the wire layer** — extended-thinking budget /
  reasoning-effort request params per dialect, Anthropic thinking-block
  signature round-trip across tool loops, and `redacted_thinking` handling.
- **TUI keyboard navigation** — arrow-select + autofill in the `/`-command
  palette, and arrow-scroll + Esc-to-close in the agent panel.
- **`/ask`** — an ephemeral forked question rendered as a dimmed aside; it
  inherits the session's context but never pollutes the transcript.
- **Failure observability** — backend and routing errors are surfaced to
  stderr (including the reasons a candidate was rejected).
- **OSS front door** — a README and a runnable offline example
  (`cargo run -p conway --example minimal_session`).

### Fixed

- **Multi-turn tool use no longer loops.** Assistant records now persist the
  tool calls they made, so a follow-up turn sees the tool result instead of
  re-calling the tool indefinitely; tool-call-only assistant turns serialize an
  empty string rather than `null` (which some OpenAI-compatible servers, e.g.
  Ollama Cloud, reject).
- **Dialect-aware health probe** — the probe now uses a liveness endpoint the
  target dialect actually serves, and an unsupported liveness path is no longer
  counted as a health failure that opens the circuit breaker.
- **`--model`** is now wired to a facade pin (previously accepted by the CLI
  parser but inert — a 0.1.0 known limitation).

## [0.1.0] — 2026-07-22

First release. conway is a Rust agent harness for agentic coding, built around
one library that serves three consumption modes equally, first-class
hierarchical forking, and a strict ports-and-adapters architecture where every
capability is a plugin.

### Architecture

- **8-crate Cargo workspace** with strictly downward dependencies
  (ports-and-adapters):
  - `conway-core` — domain types and port traits only; no I/O.
  - `conway-backends` — provider adapters (Anthropic native, OpenAI-compatible).
  - `conway-routing` — declarative role→model routing, health, and failover.
  - `conway-session` — append-only session persistence and transcript resolution.
  - `conway-tools` — the built-in tool/plugin implementations.
  - `conway-runtime` — the agent loop, supervision, and orchestration.
  - `conway` — the public facade (the single supported embedding surface).
  - `conway-cli` — the `conway` binary; depends only on the `conway` facade.
- The `SubagentHost` port breaks the tools↔runtime cycle so tools can spawn
  sub-agents without an upward dependency.

### Three consumption modes (one library)

- **Embeddable Rust library** — the primary surface. A `Conway` builder plus
  `SessionHandle` API: fully async, event-streamed, designed to be driven from a
  host application (e.g. a Tauri IDE).
- **Interactive TUI** — a terminal shell with live token streaming, an agent-tree
  pane, in-UI permission prompts, an editable input line, and slash commands
  (`/steer`, `/tree`, `/context`, `/why`, `/fork`, `/spawn`, `/resume`, `/help`,
  `/quit`).
- **`-p` / `--print` one-shot** — a clean, scriptable non-interactive mode:
  prompt from argv or stdin, streamed output, `--output-format text|json|jsonl`,
  strict stdout purity (only model output on stdout; all diagnostics on stderr),
  stable exit codes, and SIGINT handling.

### Hierarchical forking and spawning (distinct primitives)

- **Fork** — a child inherits the forker's *entire* effective context at the fork
  point as a literal, immutable, cache-friendly prefix, plus an added directive.
  Storage is O(1): one header line, zero records copied. Siblings forked at the
  same point share a single memoized prefix allocation.
- **Spawn** — a clean-slate child that requires an agent definition; it inherits
  no parent context. Fork and spawn are genuinely separate primitives — there is
  no partial-inheritance knob.
- **Copy-on-fork snapshot semantics** — after a fork the two sessions are
  independent append-only logs. Prompting the parent never reaches the child, and
  prompting the child never reaches the parent; the inherited prefix is bounded
  at the fork sequence and is byte-identical to a snapshot.
- **Bidirectional messaging** — parents steer children (applied at turn
  boundaries) and can soft- or hard-cancel them; children report progress and
  terminal results back. This is explicit, addressed messaging — separate from
  context inheritance.
- **Aggregate** — a parent can fork, spawn N differently-prompted children, and
  collect their results. A parent's `await` on a child can never hang: the
  supervisor synthesizes a terminal result on panic, budget exhaustion, or
  cancellation.

### Session persistence and continuity

- Append-only JSONL session store, one file per session, with crash-tolerant
  reads.
- **Resume** a persisted session and continue it — from both the library and the
  CLI (`--resume <id>`).
- **Fork-from** a persisted session at any sequence — from both the library and
  the CLI (`--fork-from <id>[@<seq>]`); the child genuinely inherits the parent's
  context, transitively across multi-level fork chains.
- Caller-chosen session ids (`--session <id>`), with honest usage errors on
  collision (directs the user to `--resume`).
- `Runtime::resume_root` re-registers a persisted agent as live, gated so it idles
  until its first prompt rather than racing a spurious turn against the old
  transcript.
- Full session inspection surface: `conway sessions list | show | tree | export`.

### Provider routing and backends

- **Backends**:
  - Anthropic native (Messages API), with cache-breakpoint mapping.
    API-key authentication only.
  - OpenAI-compatible, with per-dialect adapters for **Ollama**, **vLLM / Hermes**,
    **LM Studio**, and **llama.cpp server**.
- **Declarative routing** — per-role model aliases with explicit fallback chains;
  every response can be traced to which model served it and why
  (`conway routes explain`). No content inspection, no learned classifiers.
- **Health and failover** — dual circuit breakers per endpoint (transport and
  probe), a background prober, and an attempt/fallback loop that records health
  observations and fails over on transport, server, and rate-limit errors.
- **Capability-aware** — per-model tool-calling reliability, streaming behavior,
  and prompt-caching support are first-class. Prompt caching is used
  opportunistically but is never correctness-bearing (verified by byte-identity
  tests).

### Tools and extensibility

- Built-in plugins, all implemented on the *same* public Plugin/Tool API that
  third parties use (no privileged core tools):
  - Filesystem: read, write, edit, glob, grep.
  - Shell: bash execution with process-group termination on cancel.
  - Fork/subagent tools.
  - Explicit report/finalization tools.
- **Permission gate model** — allow-list, deny-all, and interactive-prompt gates,
  with a callback surface for the embedder. One-shot mode defaults fail-closed
  (an empty allow-list denies every tool) because it cannot prompt an operator.
- Sandboxing and worktree isolation are left to an agent's own tools rather than
  imposed by the harness.

### Reliability (multi-agent failure-mode mitigations)

- Full context provenance: every context segment records where it came from
  (a 9-variant provenance model).
- Literal prefix inheritance — no lossy summarization of inherited context.
- Repeated-step detection.
- Mandatory budgets (token and deadline), enforced as hard ceilings.
- Result-contract schema validation with a single retry, then an explicit
  refusal rather than a silent bad result.

### Security

- Anthropic OAuth subscription tokens (`sk-ant-oat…`) are rejected at three
  layers — conway is metered-API-key only, by design.
- Cross-session agent access is rejected (`AgentNotInSession`); an agent handle
  cannot drive a session it does not belong to.

### Known limitations (deliberate for 0.1.0)

- No Claude Pro/Max subscription authentication — metered API keys only.
- `--model` is accepted by the CLI parser but not yet wired to a facade pin field.
- Cross-*backend-kind* failover has unit coverage but no end-to-end integration
  test yet.
- No bundled example third-party plugin, and OSS-release docs (README,
  plugin-author guide) are not yet written.

<!-- No published remote yet; add a compare/tag link here once the repository is
     hosted, e.g. [0.1.0]: https://<host>/<owner>/conway/releases/tag/v0.1.0 -->

