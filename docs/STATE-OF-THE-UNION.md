# conway: State of the Union

*A whitepaper on purpose, value proposition, and design philosophy — as of 2026-07-26, post-0.2.0.*

---

## 1. Executive summary

conway is a **Rust agent harness for agentic coding**. It runs LLM-driven
agents that call tools, fork and spawn child agents, and route across multiple
model backends — behind an explicit permission system, with durable session
persistence and full context provenance. Its primary surface is an
**interactive terminal UI**; the same capabilities are reachable through an
embeddable Rust library and a scriptable one-shot mode for the cases where a
human in the loop doesn't make sense.

Its differentiator is **granular, first-class agent primitives**. Fork (a
child inherits the parent's context as a literal, cache-friendly prefix) and
spawn (a clean-slate child) are distinct, composable building blocks — the
foundation for fine-grained control over what context exists, where it lives,
and how it flows between agents.

Status: **0.2.0 is released** (2026-07-23). The inception project met all of
its success criteria; review cycles since have been clean. Current work is a
user-experience rework of the interactive agent surface (`/agents`, `/ask`,
`/fork`, `/spawn`). Open-source readiness (docs, example plugin, governance
files) is deliberately deferred but tracked.

---

## 2. Purpose: why conway exists

The problem conway addresses is that existing agentic-coding harnesses treat
**context as an afterthought and agents as flat**.

1. **Multi-agent systems fail in predictable, observable ways.** The MAST
   study — *"Why Do Multi-Agent LLM Systems Fail?"* (Cemri et al., UC
   Berkeley, 2025; [arXiv:2503.13657](https://arxiv.org/abs/2503.13657)) —
   annotated 1,600+ execution traces across 7 popular multi-agent frameworks
   and derived a taxonomy of 14 failure modes, with end-to-end failure rates
   of **41–87%** depending on framework and task. The two dominant categories
   — specification/system-design failures (41.8%: lost conversation history,
   step repetition, termination failures) and inter-agent misalignment
   (36.9%: context resets, information withholding, task derailment) — are
   exactly the failures that happen when context flow between agents is
   opaque: you cannot see what a child knew, where it came from, or why it
   drifted. conway's answer is structural: provenance-tagged context and a
   visible agent tree as first-class surfaces, not debug afterthoughts.

2. **Context economy is the central unsolved problem of agentic tooling.**
   Context windows are finite, attention degrades well before the nominal
   limit, and most harnesses answer with invisible truncation or automatic
   compaction. conway's stance is that context economy deserves **explicit,
   fine-grained mechanisms the user can see and steer** — today that means
   hierarchy (lean parent agents delegating detail downward), fork/spawn
   control over inheritance, and pluggable per-call curation hooks. The
   mechanisms are expected to evolve — more automated curation may well be
   warranted as the design space becomes clearer — but the goal is fixed:
   *spend context deliberately*.

3. **A better interactive agent experience.** The interactive UI is where
   conway aims to lead: improving on what opencode, Claude Code, and similar
   tools offer for driving agents from a terminal — a clean conversation
   surface, a visible and navigable agent tree, ephemeral side-questions, and
   explicit steering of running children.

4. **Real interfaces for non-interactive use.** When a human in the loop
   doesn't make sense — scripts, pipelines, other programs — the harness
   should still be fully drivable: a one-shot mode that behaves like a
   well-mannered Unix filter (model output on stdout, diagnostics on stderr,
   structured output formats, stable exit codes) and an embeddable Rust
   library for host applications that need inference and agent orchestration
   in-process.

5. **Provider routing that is actually operable.** Running agents across
   local servers (Ollama, llama.cpp, LM Studio, vLLM) and cloud APIs needs
   declarative role→model routing with health-aware failover — and every
   response traceable to *which model served it and why*.

---

## 3. Value proposition

**For the interactive user (the primary audience):** a terminal agent
experience designed to be best-in-class — a copy-paste-friendly single-column
conversation, a `/`-command palette, a live agent-tree panel, ephemeral
side-questions that never pollute the transcript, and explicit
fork/spawn/steer control over child agents.

**For scripts and automation:** a one-shot mode with streaming, `text|json|jsonl`
output, honest exit codes, and fail-closed tool permissions (nothing runs
unless explicitly allowed, since there is no operator to prompt).

**For host applications:** the same harness as a single Rust dependency —
sessions, hierarchical agents, routing, permissions, and a flat, ordered
event stream a UI can render directly. Fully async, no process boundary.

**For extenders:** a small, slow-moving plugin contract crate. Built-in
tools — filesystem, shell, subagents, reporting — are implemented on the
same public plugin API available to third parties. Nothing is privileged.

**For operators:** predictability. Declarative routing with explainability;
health handling that doesn't kill your only endpoint over a transient blip;
admission control that *rejects* requests that won't fit rather than silently
truncating or escalating cost; and per-session durable logs that survive
crashes and are inspectable with ordinary tools.

**Differentiators in one line each:**
- Fork and spawn as granular, first-class, composable agent primitives —
  the basis for fine-grained context curation.
- Full context provenance: for any agent, inspectable what its context
  contains and where every part came from.
- Declarative, explainable routing across local and cloud backends.
- Capability-aware backend adapters (tool-calling reliability, caching
  behavior, streaming) rather than a lowest-common-denominator interface.
- An interactive UI that treats the agent tree as a first-class citizen.

---

## 4. Design philosophy

These are the project's general objectives — the convictions the design keeps
returning to. They are stated as goals, not as frozen mechanisms: several of
them describe *what* must be true while deliberately leaving *how* open.

### 4.1 Context economy is the point

Everything else is in service of spending context deliberately. Today the
primary mechanism is structure — lean parent agents that delegate detail
downward, so deeper agents carry small, focused contexts (context rot is
empirically real long before nominal window limits). Over-inheritance is a
defect on par with under-inheritance. The mechanisms are expected to grow —
automated curation, smarter request-time shaping, and other machinery may be
adopted as the problem space becomes better understood — but the objective
doesn't move: context is the scarce resource, and conway treats it that way.

### 4.2 Granular primitives over monolithic features

Fork and spawn are the exemplar: two small, sharply-defined primitives that
compose into tournaments, adversarial panels, and parallel exploration —
rather than one blurry "subagent" feature with a partial-inheritance knob.
The same instinct applies across the system: ephemeral `/ask` is a
composition of fork, not a new primitive. When a design question arises,
prefer restructuring with existing primitives over adding a broader one.

### 4.3 Predictability over cleverness

- Routing is declarative: role→model chains with health-filtered fallback.
  *Which model served this request and why* is always answerable.
- Oversized requests are **rejected** by an admission gate that reserves
  headroom for output and reasoning — predictable failure over silent
  truncation or surprising cost.
- Every context segment carries typed provenance, and per-agent context
  composition is inspectable.
- Prompt-cache reuse is an economics optimization, never correctness-bearing:
  identical results whether caching is available, evicted, or disabled
  (verified by byte-identity tests).

### 4.4 The record is durable and inspectable

Sessions persist durably (today: an append-only JSONL log per session) and
in-memory state is a cache over the persisted record. Persist-before-act.
Curation of what goes to the model is explicit and reversible — the current
mechanism is an opt-in per-call hook plus a log-preserving record mask; no
automatic curation ships today, but this area is intentionally left open to
evolve (see 4.1).

### 4.5 Small core, extensible by construction

The core stays small; capabilities arrive as extensions. The Rust plugin API
is the first-class extension interface — stable, semver-disciplined, the same
surface the built-ins use. The harness's responsibility ends at the
permission gate: sandboxing, worktree management, and file-conflict
prevention belong to an agent's own tools, not the core. The extension story
is expected to broaden over time toward lower-barrier surfaces layered on top
of the stable core; the architecture is shaped so those layers remain
additions, not upheavals.

### 4.6 Interfaces matched to how the harness is actually driven

The interactive UI leads. The library and one-shot mode exist so that every
capability is also reachable when no human is in the loop — no feature should
be trapped in one surface. Each interface is allowed to be genuinely good at
its job rather than a lowest-common-denominator projection of the others.

---

## 5. Current state (the "state of the union")

### 5.1 Shipped

**0.1.0 (2026-07-22)** — the inception release. All success criteria met;
converged at 942 tests green, clippy/fmt clean. Delivered: an 8-crate
ports-and-adapters workspace; fork/spawn with bidirectional steering and
result aggregation; durable sessions with resume and fork-from (including
multi-level fork chains); Anthropic-native plus five OpenAI-compatible
dialect adapters; declarative routing with dual circuit breakers (transport
vs. probe) and `routes explain`; the plugin API with filesystem, shell,
subagent, and report built-ins; three permission-gate implementations;
reliability mitigations (provenance, literal prefix inheritance,
repeated-step detection, hard budgets, result-contract validation).

**0.2.0 (2026-07-23)** — driven by the first real-world interactive dogfood,
which surfaced blockers within minutes: undiagnosable routing failures, a
circuit breaker that killed the *only* configured endpoint over a single
rate-limit response, and an infinite tool-call loop. 0.2.0 fixed the
make-it-work blockers, unified two divergent model-capability systems into
one source of truth, added the per-call context-curation hook and a
log-preserving record mask, wired `--model`, added wire-layer reasoning
support (thinking budgets and signature round-trips), relicensed to
AGPL-3.0-only, redesigned the TUI to the current single-column layout,
shipped ephemeral `/ask`, and opened the OSS front door (README + a runnable
offline example).

**Unreleased:** `conway_ask` — a model-facing ephemeral-fork tool returning
the child's full reply text, so curation and context-drafting inference stay
out of the orchestrator's own context window.

### 5.2 In flight

The active work is a rework of the interactive agent surface, in four parts:

- **A unified `/agents` panel** absorbing the old `/tree` view; ephemeral
  (ask) agents appear in the panel with labels and visibility cycling.
- **A modal `/ask`**: a single-turn overlay with three explicit fates —
  promote the answer into the session, merge it in verbatim with provenance
  marking, or discard it entirely.
- **Natural-language intent on `/fork` and `/spawn`**: an intent-classifier
  pass feeding a confirmation card before the child is created.
- **Tool-set override for `conway_ask`** as a standalone capability.

### 5.3 Health of recent work

Review cycles since 0.1.0 have found zero critical defects; the single
significant finding (a stale changelog) was resolved. Known carry-forwards:
a builder→wire round-trip test for tool results (the exact seam a shipped
bug slipped through), live verification of the Anthropic tool-result path,
and a SECURITY.md ahead of any open-source release.

### 5.4 Deliberately deferred

- **Open-source readiness**: an example third-party plugin, a plugin-author
  guide, community/governance docs, and a cross-backend failover integration
  test.
- **An ACP (Agent Client Protocol) adapter**: deferred until the protocol
  matures; the event stream was kept compatible so the adapter stays thin.
- **A native llama.cpp slot-caching adapter** (the seam is reserved) and a
  **TypeScript client** (after the core API stabilizes).
- **Richer automated context curation** — an open design area (see §4.1),
  not a scheduled feature.

### 5.5 Open design questions

- Session continuity ergonomics (resuming and naming sessions) — partially
  built, not fully closed out.
- Cache affinity vs. role-based routing — whether routing should consider
  which endpoint holds a warm prefix cache.
- Urgent steering during long-running tool calls — today steering lands at
  turn boundaries only.
- Plugin distribution beyond in-process Rust (subprocess/WASM hosts; the
  tool-facing types are already serialization-ready to keep this cheap).
- Cost normalization across providers with different billing models.

---

## 6. What conway intends to be

1. **A best-in-class interactive agent harness for the terminal** — the
   tool you reach for to drive coding agents, with the agent tree and
   context flow visible and steerable.
2. **A dependable automation primitive** — the same harness behind clean
   one-shot scripting and an embeddable library, for every situation where
   interactive use doesn't fit.
3. **An extensible platform** — a stable Rust plugin core today, broadening
   toward more accessible extension surfaces over time.
4. **Editor and IDE reach** — via an ACP adapter once the protocol matures,
   without contorting the core to fit today's draft.
5. **Open source**, on AGPL terms, once the readiness checklist (docs,
   example plugin, governance, failover testing) is done.

The commitments expected to survive any realignment: granular composable
agent primitives; deliberate context economy; explainable routing; a durable,
inspectable record; a small extensible core; and interfaces that are each
genuinely good at their job.

---

## 7. Tensions worth recalibrating

Observed from the project's history, offered as agenda items for the
realignment session:

1. **Interactive-first is newly explicit.** The TUI is the primary surface,
   and the active board is entirely interactive-UX work — consistent with
   that. The open question is cadence: when do the automation surfaces
   (library ergonomics, one-shot polish) get their next dedicated attention?
2. **Dogfood-driven planning.** Both 0.2.0 and the current rework were
   triggered by live-use friction rather than a roadmap. That is a feature
   (real signal) and a risk (reactive scope) — the deferred OSS-readiness
   list has survived multiple cycles without being scheduled.
3. **Resilience vs. strict failure.** Making single-endpoint configs survive
   transient errors moved the failure philosophy slightly from "fail
   predictably" toward "retry hopefully" in that case. Worth confirming the
   stated philosophy matches the shipped behavior.
4. **Context curation is deliberately under-designed.** The current hook is
   minimal and opt-in by construction. If automated curation is coming, the
   realignment is the right place to sketch its direction before the current
   mechanism calcifies.
5. **Five design questions have been open since inception** (§5.5). The
   realignment should answer, schedule, or formally retire each.
6. **No active charter beyond the current board.** The inception project is
   complete; the board has a work frontier but the project has no stated
   next intent. Defining it is the point of the realignment.

---

*End of document. Generated 2026-07-26 from project records and repository
sources; intended as input to a realignment/refinement session.*
