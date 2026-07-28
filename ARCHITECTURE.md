# Architecture: conway

This is the living design overview for conway. It describes the system as
committed at **0.3.0** — for the release-by-release history of what changed
and why, see [CHANGELOG.md](CHANGELOG.md); for the inception-era planning
rationale that predates the current shape (why decisions were made, not
just what they are), see this project's ideate decision/journal records —
the design content of the original planning set has been folded into this
file and `docs/crates/*.md`, and the planning documents themselves have
been retired.

For a narrower, implementation-level treatment of each crate, see
[`docs/README.md`](docs/README.md) and `docs/crates/*.md`.

---

## 1. What conway is

conway is an embeddable Rust agent harness for agentic coding: it runs
LLM-driven agents that call tools, fork and spawn child agents, and route
across multiple model backends, with explicit permissions, an append-only
session log, and no hidden context manipulation.

It ships as **one library** (`conway`, the facade crate) that serves **three
consumption modes equally** — there is no mode that is "more native" than the
others, because all three are written against the same public API:

- **Embeddable Rust library.** A host application (for example an IDE)
  depends on the `conway` crate directly: `ConwayBuilder` assembles a
  `Conway`, which hands out `SessionHandle`s. Fully async, event-streamed, no
  process boundary.
- **Interactive TUI.** `conway-cli`'s terminal shell — a single-column,
  copy-paste-friendly conversation view, a live `/`-command palette, and an
  on-demand agent-tree panel. It is a thin consumer of the same facade API
  the library embedder uses.
- **One-shot `-p` / `--print`.** A scriptable, non-interactive mode: prompt
  from argv or stdin, streamed output, `--output-format text|json|jsonl`,
  strict stdout purity (only model output on stdout, diagnostics on stderr),
  stable exit codes. Because one-shot mode cannot prompt an operator for
  permission, it fails closed: tools are denied unless explicitly named via
  `--allowed-tools`.

## 2. The workspace: 8 crates, ports-and-adapters

conway is a Cargo workspace of eight crates laid out as ports-and-adapters
(hexagonal architecture). `conway-core` defines the domain types and every
port trait; every other crate is either an adapter implementing those ports,
or a consumer wiring adapters together.

```
conway-core        domain types + port traits. No I/O, no tokio-net.
conway-backends    Backend adapters: Anthropic native, OpenAI-compatible
                    dialects (OpenAI, Ollama, vLLM/Hermes, LM Studio,
                    llama.cpp server).
conway-routing     Capability-based routing, circuit breakers, health probes.
conway-session     The append-only session log; transcript/prefix resolution.
conway-tools       The tool/plugin registry and built-in tools.
conway-runtime     The agent loop, context assembly, fork/spawn orchestration.
conway             The public facade: ConwayBuilder, Conway, SessionHandle.
conway-cli         The `conway` binary: one-shot mode and the TUI.
```

Dependency direction is **strictly downward**; `conway-core` depends on
nothing else in the workspace:

```
conway-cli ──> conway ──> conway-runtime ──┬─> conway-routing ─┐
                  │           │            ├─> conway-session ─┤
                  │           │            ├─> conway-tools ───┤─> conway-core
                  └───────────┴────────────┴─> conway-backends ┘
```

There are no cycles. The one place a cycle would naturally appear — the
fork/spawn *tool*, which lives in `conway-tools`, needing to drive the
runtime that lives above it — is broken by the **`SubagentHost` port**:
defined in `conway-core`, implemented by `conway-runtime`, and handed to
tools through `ToolCtx`. A tool never depends on `conway-runtime` directly;
it depends only on the trait object it's handed.

This shape exists so:

- An embedder can depend on `conway` alone and never pull in `clap`, a TUI
  renderer, or every backend's HTTP client.
- A third-party plugin author depends on `conway-core` — a small, slow-moving
  crate with strict semver discipline — not on the whole harness.
- Backend adapters are feature-gated per crate consumer (`anthropic`,
  `openai-compat`), which only works cleanly with a dedicated crate.
- Every port trait has a fake/test-double implementation available behind a
  feature flag, so the runtime and tools are testable end-to-end with zero
  network.

## 3. Core primitives

### 3.1 Fork vs. spawn

conway has exactly two ways to create a child agent, and they are genuinely
distinct primitives — there is no partial-inheritance mode.

- **Fork** — a child inherits the parent's *entire* effective context at the
  fork point: a literal, immutable, ordered prefix, frozen at that sequence
  number, plus an appended directive. This is O(1) on disk: forking writes
  one header line and copies zero records. Siblings forked at the same point
  share one memoized in-memory prefix allocation.
- **Spawn** — a clean-slate child. It inherits no parent context. Naming an
  agent definition (a system prompt / skill) is optional, not required:
  omitting one means the child inherits the spawning session's own role, and
  transitively its model routing, the same way a roleless fork inherits its
  forker's role.

After a fork, the two sessions are independent append-only logs: prompting
the parent never reaches the child and vice versa. Cross-agent communication
after that point is explicit — steer messages (applied at turn boundaries)
and terminal results, never implicit context bleed.

The ephemeral **`/ask`** primitive (`SessionHandle::ask`, §3.6) is built
directly on fork: it forks the current session at its head, drives one turn
on the child, and returns a handle to that child — the parent's own
transcript is never touched.

### 3.2 The append-only session log and context assembly

Every turn, tool call, and tool result is written to an append-only session
log (`conway-session`, `JsonlSessionStore`) — one JSONL file per session,
never mutated or deleted in place, crash-tolerant on read. The log is the
source of truth; in-memory agent state is a cache over it.

Context for a turn is assembled **deterministically** from the log by
`conway-runtime`'s `ContextBuilder`, in three conceptual layers:

- **Records** — the raw, persisted `LogRecord`s (user turns, assistant turns,
  tool results, fork directives, parent steers, ...), each carrying a typed
  `Provenance` describing where it came from.
- **Segments** — records resolved into an ordered `Vec<PromptSegment>` in a
  fixed order (static content first: system prompt, tool schemas; then the
  inherited prefix for a fork child; then the turn's own volatile records).
  This fixed ordering is what makes cache hits possible for
  implicit-prefix-caching backends — ordering is an economics mechanism, not
  a correctness one.
- **Wire messages** — segments translated into the concrete request shape a
  given `Backend` adapter expects, with cache hints attached where the
  backend's `CacheMode` supports them.

Fork inheritance and context assembly compose: a fork child's "inherited"
prefix is resolved once at fork time by `conway-session`'s
`TranscriptResolver` (walking the ancestry chain, bounded at the fork
sequence) and is then simply replayed into every one of the child's turns
unchanged.

### 3.3 Capability-based routing

`conway-routing` resolves a role alias (`"planner"`, `"coder"`, `"fast"`, ...)
to an ordered list of candidate `(backend, model)` routes, filtered by each
candidate's declared `Capabilities` (context window, tool-calling support,
reasoning) against the turn's requirements. Routing never inspects prompt
content — there is no code path in `conway-routing` that can see request
text.

That is a crate-level guarantee, not a claim that conway can never route on
content. It makes the router's decision a pure function of role,
capabilities, and health. Content-aware policy lives one layer up, in role
*selection*: the caller picks the role, whether that is the host, an agent
definition, or a plugin spawning with a role it chose. `ContextHook` cannot
do this today — `ContextPayload` carries only `segments` and `tools`, and
`AgentSpec::role` is fixed before `Router::resolve` runs — so a hook reads
the routing outcome (`ContextHookCtx::model`) rather than steering it.
Widening that surface is future work.

Two independent circuit breakers are tracked per endpoint — **transport**
(connection failures) and **probe** (a background liveness/readiness check) —
because a slow-but-alive local server and a genuinely dead one are different
states that should be handled differently. When the primary candidate for a
role is unavailable, the runtime walks the ordered fallback chain, recording
health observations as it goes; `conway routes explain` surfaces exactly
which candidate was chosen and why, and which were skipped and why.

### 3.4 Tools as plugins, behind a permission gate

Every tool — built-in or third-party — implements the same `Plugin`/`Tool`
traits from `conway-core`; nothing about the built-ins (filesystem, shell,
subagent, report) is privileged. A `Plugin` declares the `Tool`s it provides;
each `Tool` declares its JSON-schema `ToolSpec`, a `ToolCategory`, and a
`PermissionClass`.

Tool **announcement** — what the model is told exists and may be called — is
a distinct concern from tool **execution** — what is actually allowed to
run. Every proposed tool call passes through an explicit `PermissionGate`
(`AllowOnce`, `AllowAlways{scope}`, `Deny`, or `DenyWithFeedback` — the last
of which lets the model see and adapt to a denial reason rather than just
failing silently). The gate is always implemented by the consumer; conway
ships an allow-list gate (used by `-p`), a deny-all gate, and an interactive
prompting gate (the TUI).

### 3.5 `ContextHook` — pluggable context and tool curation

`ContextHook` is a port trait (`conway-core`) invoked once per assembled
request, before it is routed: `before_request` receives the segments and the
announced tool set together as one `ContextPayload` and may edit or drop
segments, narrow the announced tools, or leave the payload unchanged. An
optional `on_overflow` method fires only when the already-hooked payload
still doesn't fit the routed model's context window, giving the hook one more
chance to shrink it before the runtime raises a hard error.

**No hook is registered by default, and conway-core ships no curation
policy.** With `Option<Arc<dyn ContextHook>>` set to `None` — the default —
the runtime never invokes anything, not even a no-op pass-through; behavior
is byte-for-byte what it was before the hook existed. There is no automatic
compaction anywhere in the harness. A consumer that wants masking,
system-prompt instrumentation, tool-announcement narrowing, or
overflow-time summarization supplies its own hook — which may be a pure
script or may itself issue an LLM call, since `before_request` is async.

A related, independent mechanism is the **out-of-context record mask**:
individual log records can be marked to exclude from future LLM calls while
remaining in the append-only log — reversible, and orthogonal to
`ContextHook`.

### 3.6 Keep-alive sessions and `/ask`

By default, a session's root agent task terminates once its first turn
reaches a natural `Completed` state; a second `SessionHandle::prompt` call on
the same handle would silently run no turn. **Keep-alive** (`SessionSpec::
keep_alive = true`) opts a session out of that: the root agent idles,
awaiting its next prompt, instead of terminating — this is what lets the TUI
run a second chat message in the same process. A keep-alive session emits
exactly one `AgentFinished` for its whole lifetime (on cancel, deadline, or
budget exhaustion), not one per turn.

**`/ask`** (`SessionHandle::ask`) is the ephemeral side-question primitive
built on fork (§3.1): it forks the current session at its head into a child
marked `SessionMeta::ephemeral = true`, then drives one turn on that child
with the given text. The child inherits the full context and tool set, so
tool calls it makes are real, but its transcript never touches the parent's
session and is excluded from default session listings — a throwaway
question never pollutes the live conversation.

### 3.7 Reasoning support at the wire layer

conway carries reasoning/extended-thinking as a first-class, per-dialect
wire concern in `conway-backends`: extended-thinking budget and
reasoning-effort request parameters are mapped per backend dialect, and for
Anthropic specifically, thinking-block **signature round-trip** is preserved
across multi-turn tool-call loops (including `redacted_thinking` blocks),
since a dropped or malformed signature invalidates the request. This is
wire-layer plumbing, not policy — whether/how much a model reasons is a
routing/config decision, not something the harness imposes.

### 3.8 The extension model

The same shape recurs at every extension point: `conway-core` defines a
port, the core ships mechanism behind it, and the policy is the consumer's
to write.

- **Tools** (§3.4) are plugins. The filesystem, shell, subagent, and report
  tools implement the same `Plugin`/`Tool` traits a third party would, so
  there is nothing a built-in can do that a third-party plugin cannot.
- **Permissions** (§3.4) are a `PermissionGate` the consumer supplies.
  conway ships an allow-list, a deny-all, and an interactive gate, and
  privileges none of them.
- **Context curation** (§3.5) is a `ContextHook`. None is registered by
  default and there is no compaction anywhere in the harness, so a consumer
  that wants condensing, masking, or tool narrowing writes that policy
  itself.
- **Routing** (§3.3) resolves roles; selecting the role belongs to the
  caller, which is where content-aware policy lives.

`conway-core` is deliberately small and slow-moving under strict semver
discipline (§2), because it is the surface third-party plugins depend on.
Keeping opinions out of it is what keeps that surface stable: a policy baked
into the core becomes a behavior every extension has to accommodate or work
around, and those accumulate.

The reach of these ports is not uniform yet, and §3.3 records one current
limit: a `ContextHook` can read the routing outcome but cannot steer it.
Widening port surfaces where they are narrower than the model implies is
ongoing work, and the tool-facing types are serialization-ready, which
leaves cheaper plugin hosts (subprocess, WASM) as a layered addition rather
than a redesign.

## 4. Data flow of one turn

```
prompt
  │
  ▼
persist            Runtime appends LogRecord::UserTurn to the session log
  │                 (persist-before-act: the log is truth, memory is cache)
  ▼
context build       ContextBuilder resolves records -> ordered, provenance-
                     tagged PromptSegments (fixed order: static, then
                     inherited prefix, then volatile)
  │
  ▼
route                Router::resolve(role, required capabilities, est_tokens)
                     -> an ordered candidate list, filtered by breaker state
  │
  ▼
attempt / backend call   Walk the candidate list; call Backend::generate or
                     ::stream (choice depends on the model's declared
                     tool-calling support); record health observations
  │
  ▼
tool run             Proposed tool calls pass PermissionGate; permitted
                     calls run concurrently, bounded, cancellable
  │
  ▼
append records       Assistant turn + tool results are appended to the log
  │
  ▼
next turn            Loop back to context build until the model returns no
                     tool calls, or a budget (steps/deadline/tokens) trips
```

Steer messages from a parent agent are only ever applied at a **turn
boundary** — the mailbox is drained after a turn's tool batch completes and
before the next turn's context is assembled. No code path injects into a
context mid-generation.

## 5. Where to go next

- [`docs/README.md`](docs/README.md) indexes the per-crate design docs.
- [CHANGELOG.md](CHANGELOG.md) is the authoritative feature history,
  release by release.
- This project's ideate decision/journal records hold the inception-era
  planning rationale — the "why" behind choices this file and
  `docs/crates/*.md` describe as the current "what" and "how".
