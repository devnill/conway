# Changelog

All notable changes to **conway** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

