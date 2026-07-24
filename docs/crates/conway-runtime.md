# conway-runtime

`conway-runtime` is the agent engine: it wires every port defined in
[`conway-core`](conway-core.md) together into a running agent loop. See
[`/ARCHITECTURE.md §4`](/ARCHITECTURE.md) for the turn data-flow this crate
implements end to end.

## Responsibility and boundary

This crate owns the agent loop, the agent tree and its supervisor,
mailboxes and steering, context assembly, the plugin/tool registry and
dispatch, permission brokering, backend attempt/fallback sequencing, the
event bus, budgets, and the `SubagentHost` implementation. It is the one
crate that holds a concrete instance of every port trait (`Backend`,
`Router`, `HealthRegistry`, `Plugin`, `PermissionGate`, `SessionStore`,
`EventSink`, `ContextHook`) as an injected dependency
(`runtime::RuntimeDeps`) and drives them through one turn.

```
agent_loop     AgentLoop: the per-agent turn state machine
attempt        AttemptEngine: candidate list -> one GenerateResponse
context/       ContextBuilder, prefix resolution, ContextReport persistence
mailbox        bounded per-agent inbox, oldest-drop, turn-boundary drain
permission     PermissionBroker: per-session decision cache over PermissionGate
result         ResultBuilder: accumulates state for the terminal AgentResult
runtime        Runtime: the facade over one agent tree (RuntimeDeps, start_root, ...)
step_digest    StepDigest: repeated-tool-call detection (MAST mitigation)
subagent       impl SubagentHost for Runtime — the fork/spawn entry point
supervisor     guarantees await_result always terminates
tools/         PluginRegistry + ToolRunner: dispatch, permission gating, truncation
tree           AgentTree: multi-agent tree, agent_path, cancellation propagation
events         EventBus: the single fan-out point for Event
```

## The agent loop

`AgentLoop` (`agent_loop.rs`) is the per-agent turn state machine: it wires
`ContextBuilder -> Router -> AttemptEngine -> ToolRunner -> SessionStore`
into one turn, with budget enforcement and terminal-result construction.
One iteration is, in order: assemble context, resolve a route, attempt
generation across the fallback chain, run any proposed tool calls through
the permission gate, append the results to the session log, and loop until
the model stops proposing tool calls or a budget trips. See
[`/ARCHITECTURE.md §4`](/ARCHITECTURE.md) for the full data-flow diagram.

`AttemptEngine` (`attempt.rs`) turns an ordered candidate list plus
assembled segments into one `GenerateResponse`: it chooses streaming vs.
non-streaming per the candidate's declared tool-calling capability,
sequences the fallback chain, and — as a backstop specifically covering
the pinned-model path, which bypasses `conway-routing`'s own headroom
gate — enforces the headroom-aware T-1 context check itself, and records
health observations using `conway-routing`'s failure classification. See
[`conway-routing`](conway-routing.md) for that classification and the
breaker model this engine reports into.

## Context assembly: `ContextBuilder`

`ContextBuilder` (`context/builder.rs`) assembles one agent's request
context in the fixed architecture order — static content, then the
inherited prefix, then volatile turn records — from already-resolved
`LogRecord`s. It is deliberately synchronous and I/O-free: ancestry
resolution ([`conway-session`](conway-session.md)'s `TranscriptResolver`)
is the caller's job, so this builder is pure over its input and trivially
testable with golden fixtures.

A structural subtlety worth naming: `PromptSegment::new` assigns a fresh
random `SegmentId`, which can't simultaneously satisfy golden-file
byte-equality across repeated test runs *and* the cache-neutrality
property (`strip_cache_hints(build(x)) == build(x_with_cache_disabled)` on
the `(id, role, content, provenance)` tuple, which needs the *same* id
from two independent `build` calls for the same logical segment). The
builder resolves this by **overwriting every segment's id** with a value
deterministically derived from `blake3(agent_id ‖ ordinal ‖ ...)` rather
than trusting the constructor's random one.

### The 0.2.0 tool-result fix

`ContextBuilder` is where the 0.2.0 "tool results reach the model" bug was
actually fixed (see [`conway-backends`](conway-backends.md) for the wire
side of the story). The tool runner produces `ToolResult.blocks =
[ContentBlock::Text{..}]`, and prior to the fix the context builder's
`Role::ToolResult` segment carried those raw `Text` blocks straight
through. But both wire adapters serialize a tool result *only* from a
`ContentBlock::ToolResultBlock` — the variant carrying the `call_id` the
wire format keys on — so a `Text`-only tool-result segment matched neither
adapter and was silently dropped from every follow-up request: the model
saw its own tool call but never the result, and would confabulate an
answer instead of grounding on the real output. The fix wraps a recorded
`ToolResult` into a single `ContentBlock::ToolResultBlock { call_id,
blocks, is_error }` before it becomes a segment, in both the
own-record and inherited-record mapping paths, so the segment carries the
canonical shape both wire adapters already expect.

### `ContextHook` invocation (0.2.0)

The turn loop gives a registered [`ContextHook`](conway-core.md) (from
`conway-core`) two chances to act, both inside `AgentLoop`:

1. **`before_request`**, once per turn, immediately after
   `ContextBuilder::build` produces the assembled `segments` and
   `ContextReport`: the loop bundles `segments` and the announced
   `Vec<ToolSpec>` into one `ContextPayload`, calls the hook, and
   re-derives everything downstream (the live report slot,
   `Event::ContextSegmentAdded`, routing, the attempt request, the
   persisted `ContextReportRecord`) from whatever the hook returns — never
   from the pre-hook payload, so no consumer can see a stale view.
2. **`on_overflow`**, only from `route_and_attempt`, and only when the
   already-hooked payload still fails the T-1 context gate for the
   specific model routing chose. The engine gives the hook one bounded
   re-assembly retry before giving up and returning the hard
   `ContextTooLarge`/`ForkContextOverflow` error.

With **no hook registered**, neither code path runs at all — not a
no-op call, an absent one — so the rest of the turn is byte-identical to
pre-`ContextHook` behavior. `Runtime::set_context_hook` (see below) is
where a consumer registers or clears the hook.

## Fork/spawn orchestration: `SubagentHost`

`impl SubagentHost for Runtime` (`subagent.rs`) is the cycle-breaking
fork/spawn entry point every tool call and every developer-API call
(`SessionHandle::fork`/`spawn`) goes through — mechanically the same
method, so the built-in `conway_subagent` tool has no privileged path the
public API lacks. Fork and spawn are, mechanically, the same four steps —
create a child session, resolve its starting context, attach it to the
tree, launch its `AgentLoop` — differing only in how the starting context
is resolved: a fork's `InheritedPrefix` is the parent's own effective
transcript up to the fork point in full (never a truncated slice); a
spawn's context has no inherited prefix at all.

`SubagentHost::await_result` always terminates by construction: the
**supervisor** (`supervisor.rs`) wraps every agent task so a panic, a
blown deadline, or an external cancellation each resolve to a synthesized
terminal `AgentResult`, published through `AgentTree`'s set-once
publication guarantee (`tree.rs`) — a parent's pending
`conway_subagent`/`conway_await` tool call can never hang. `AgentTree`
additionally owns structural `agent_path` resolution (the precondition
`PermissionRequest::agent_path` needs) and cancellation propagation down
the tree.

## Mailboxes and steering

`mailbox.rs` implements each agent's bounded inbox as a plain
`VecDeque`-backed ring behind a `std::sync::Mutex` (not
`tokio::sync::mpsc`, whose blocking `send`/imprecise `try_send` semantics
can't guarantee exact oldest-drop behavior without a side buffer) — a
stuck child must never be able to deadlock its parent by not draining
messages, so the sender never blocks; overflow drops the oldest queued
message instead. `AgentLoop::drain_inbox` drains and classifies
(`mailbox::classify`) this inbox at every turn boundary, which is the
mechanism that gives steer messages their turn-boundary landing guarantee
"by construction" — no code path injects a steer mid-generation.

`AgentMessage::Cancel` carries a `hard: bool` distinguishing two genuinely
different urgencies: `hard: false` is classified like every other message,
at the next drain (turn boundary) — the in-flight tool call is allowed to
finish. `hard: true` trips the agent's `CancellationToken` **synchronously
inside `MailboxSender::send`**, before the call returns, not at the next
drain — waiting for a drain would be too late by definition, since the
whole point of a hard cancel is to interrupt a turn already in flight,
which will not reach the top of the loop for an arbitrary amount of time.
Overflow eviction only produces `Event::SteerDropped` when the evicted
message is itself a `Steer`; eviction of any other kind (including a
queued soft `Cancel`) is logged via `tracing::warn` naming the evicted
kind rather than evented, since reporting a steer as dropped when a
different kind of message was actually evicted would mislead a consumer
rendering that event.

## Keep-alive (0.2.0)

By default, a session's root agent task terminates once its first turn
reaches a natural `Completed` state. `AgentSpec::keep_alive` /
`RootSpec::keep_alive` opts a session out of that: the root agent idles at
the end of a turn, awaiting its next prompt, instead of returning — this is
what lets a long-lived host (the TUI) run a second chat message against the
same running task. A `keep_alive` agent's step-budget accounting is
deliberately turn-scoped rather than run-scoped (`check_budget` gates
`max_steps` against `state.turn_steps`, reset each turn, rather than a
lifetime counter) so a long keep-alive session's budget doesn't exhaust
itself across unrelated prompts; non-`keep_alive` behavior (`state.turn`
accounting) is byte-for-byte unchanged. A `keep_alive` session emits
exactly one `Event::AgentFinished` for its whole lifetime — on cancel,
deadline, or budget exhaustion — never one per turn.

## Permission brokering

`PermissionBroker` (`permission.rs`) sits between `ToolRunner` and the
consumer's [`PermissionGate`](conway-tools.md): it normalizes whatever the
gate decides into a `PermissionOutcome` the runner acts on directly, and it
owns the `AllowAlways` decision cache (keyed by `PermissionScope`) so a
consumer who answers "allow always" is asked at most once per scope for
the rest of the session/agent/subtree. It never imposes its own timeout on
the gate — a pending call is held open for as long as the gate takes to
decide.

## Tool dispatch

`tools::registry::PluginRegistry` compiles the injected `Arc<dyn Plugin>`
set exactly once, at construction — every tool's JSON Schema is compiled
up front so `ToolRunner` never re-parses one per call, and a schema
compile failure or a duplicate tool name across plugins is a
construction-time error rather than something a running agent discovers
mid-batch. `tools::runner::ToolRunner` owns per-call name resolution,
argument validation, permission gating (through `PermissionBroker`),
bounded concurrent execution, cancellation, truncation-policy enforcement,
and per-call event emission (`ToolCallProposed` → `PermissionRequested` /
`PermissionResolved` → `ToolCallStarted` → `ToolProgress`* →
`ToolCallFinished`). It is also where the `report` tool's envelope is
lifted into the agent's terminal `AgentResult` — deliberately kept out of
[`conway-tools`](conway-tools.md), which must not depend on this crate.

## MAST mitigations this crate owns

- **`StepDigest`** (`step_digest.rs`) — repeated-tool-call detection: a
  per-agent, in-memory, bounded ring of the last-seen `(tool,
  canonicalized-args)` digest. The third identical call is noticed exactly
  once, so the model can be told it's looping — detection, not
  enforcement; the runtime never refuses to run the call itself.
- **`ResultBuilder`** (`result.rs`) — accumulates every artifact any
  dispatched tool emitted over the run plus the most recent successful
  `report` invocation, resolving precedence between the two exactly once
  at the finish boundary. Also owns result-contract validation
  (`validate_result_contract`, classified `Ok`/`Retry`/`Rejected`) driving
  the "one corrective retry, then `Rejected{missing}`" rule for a
  `SubagentSpec::result_contract`.
- **The supervisor's always-terminates guarantee** — see "Fork/spawn
  orchestration" above.

## `Runtime`: the facade over one agent tree

`Runtime` (`runtime.rs`) owns dependency injection (`RuntimeDeps`,
constructed once with every port implementation the embedding consumer
supplies) and the public surface: `start_root`, `prompt`, `cancel`,
`subscribe`, `context_report`, `tree`. `Runtime::set_context_hook`
registers or clears the `Option<Arc<dyn ContextHook>>` every `AgentLoop`
reads. This is the type the [`conway`](conway.md) facade wraps to produce
`Conway`/`SessionHandle` — `conway-runtime` itself has no CLI, no config
loading, and no knowledge of `-p`/TUI mode.

## How it fits the whole

`conway-runtime` depends on [`conway-core`](conway-core.md),
[`conway-session`](conway-session.md) (`SessionStore`, `TranscriptResolver`),
[`conway-routing`](conway-routing.md) (`Router`, `HealthRegistry`), and
[`conway-backends`](conway-backends.md) (`Backend`) — and it is a *consumer*
of [`conway-tools`](conway-tools.md)'s `Plugin`/`Tool` implementations
without depending on the crate at compile time beyond the trait objects
handed to it. [`conway`](conway.md) is `conway-runtime`'s sole downstream
consumer, wrapping `Runtime` behind `ConwayBuilder`/`Conway`. See
[`/ARCHITECTURE.md`](/ARCHITECTURE.md) for how these pieces compose across
one turn.
