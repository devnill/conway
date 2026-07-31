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

### Prompt caching: a post-routing, capability-keyed pass

`ContextBuilder::build` runs *before* routing resolves a concrete model
(`AgentLoop::run_inner` builds context, then calls `route_and_attempt`), so
`ContextInput::cache_mode` can only ever be a pre-routing placeholder —
every production caller (`runtime.rs`'s `start_root`/resume-root,
`subagent.rs`'s fork/spawn) sets it to `CacheMode::None`, deliberately, not
as a gap. Selecting a real `CacheMode` there would mean guessing at a model
that has not been chosen yet; for an unpinned agent (most of them) it
cannot be guessed at all.

The real cache-hint attachment happens one layer down, in `AttemptEngine::
execute`, once per candidate `Route` in the fallback chain — right where
`caps = backend.capabilities(&route.model)` is already resolved for the
T-1 headroom gate and the stream-vs-generate strategy choice.
`attach_route_cache_hints` (private to `attempt.rs`) re-derives the A/B
breakpoint indices from the FINAL segment list's *provenance* — the last
`Provenance::ToolRegistry` segment (A) and the last `Provenance::Inherited`
segment (B), via `context::builder::breakpoint_indices` — rather than
threading indices computed at `build` time, because a registered
`ContextHook::before_request` (WI-126) may have added, dropped, or
reordered segments since `build` ran; re-deriving from provenance on the
list actually being sent is what keeps this correct regardless. It then
calls `context::builder::attach_cache_hints` (the same function
`ContextBuilder::build` itself calls, now exposed `pub(crate)`) with
`caps.cache` — the ACTUALLY resolved model's capability, never the
placeholder — marking `PromptSegment::cache_hint` on a fresh per-route
clone of the segments before `build_request` turns them into a
`GenerateRequest`.

This is deliberately keyed on *capability*, not backend identity or a
per-agent opt-in setting: every Anthropic model declares
`CacheMode::ExplicitBreakpoints { max_breakpoints: 4, .. }`
(`conway-backends`'s `anthropic_defaults()`), so every turn against one —
root, fork, or spawn, pinned or routed — gets breakpoints attached with no
caller action, matching the "caching is a sane default" design decision.
Fork and spawn children get identical treatment to a root turn (same code
path); a fork child's inherited-prefix segment additionally lets it mark
breakpoint B, which is where caching compounds most (GP-02: a child
inherits the whole ancestry transcript, and every sibling forked at the
same point shares an identical B-bounded prefix). A route whose resolved
capability is `CacheMode::ImplicitPrefix` (e.g. Ollama, Kimi) or `None`
gets no hints at all — `attach_cache_hints`'s own match decides that, so
this pass is a true no-op for every non-explicit-breakpoint candidate, and
`PromptSegment::cache_hint` remains read by exactly one module in the
whole workspace ([`conway-backends`](conway-backends.md)'s
`anthropic::cache`). See that crate's doc for how a hint becomes
`cache_control` on the wire, and `conway-core`'s `segment.rs` for why a
hint is never correctness-bearing (GP-06): the request the model actually
sees is byte-identical whether or not any hint survives.

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

**`steer`/`await_result`/`cancel` are descendancy-checked at this trait
boundary, not only at the `conway_steer`/`conway_await`/`conway_cancel`
tool callsites** (P-1, mirroring `AskRequiresFork`'s fork-only-invariant
shape): each takes a `caller: AgentId` alongside `target`, and
`Runtime::ensure_own_subtree` (`subagent.rs`) rejects with
`RuntimeError::AgentNotInSubtree` unless `target` is `caller` itself or one
of its descendants (walking the live tree, the same `AgentTree::path`
lookup `start` uses to build a new child's `agent_path`) — an unknown
`target` still reports `AgentNotFound`, unchanged. Before this, all three
methods took only `target`, so any agent that had merely seen a sibling's
id (tool output, the event stream, `conway_subagent`'s own return value)
could cancel that sibling's work or inject a `steer` message; `steer` made
this worse by attributing the injected message to `target`'s own tree
parent rather than the real sender, so a forged steer was indistinguishable
from a genuine one on the receiving end. `steer`'s attribution
(`AgentMessage::Steer::from`, and the persisted `LogRecord::
ParentSteer::from`) now derives from `caller` directly. There is no
separate "operator" bypass: `conway::SessionHandle`'s `steer`/
`await_agent`/`cancel` (the TUI/embedder path) pass the session's own root
agent as `caller`, and a root's subtree already covers its whole session by
construction, so an operator-originated call is authorized by the exact
same check every other caller is held to. The model-invoked
`conway_steer`/`conway_await`/`conway_cancel` tools (`conway-tools`'
`subagent/control.rs`) always pass `ToolCtx::agent_id` — the
runtime-assigned identity of the agent actually dispatching the call, never
model-supplied — as `caller`.

**Live `Event::UserTurn`, and the one attach-ordering hazard it has to
respect.** `Runtime::prompt` (the target of `SessionHandle::prompt`/
`prompt_agent`, every plain TUI chat message) and `Runtime::start_root`
(a root created with an initial prompt) each emit `Event::UserTurn`
immediately after persisting the matching `LogRecord::UserTurn`, so a live
subscriber and a later replay of the same session see the identical
occurrence — closing the P-8 gap where only the TUI's own local transcript
push ever showed a prompt. Both call sites are ordering-safe by
construction: `prompt`'s target must already be a live, attached agent
(looked up in `Runtime.agents`) to reach the emit at all, and a root's
`kind: None` means `AgentTree::attach` never emits `Event::AgentSpawned`
for it in the first place, so the "`AgentSpawned` precedes every event for
its agent" guarantee is either already satisfied or vacuous.

The THIRD site is not so simple, and is the one genuinely reachable
pre-spawn-ordering hazard this item found: `subagent.rs::start`'s `Spawn`
branch appends its own head `LogRecord::UserTurn` (the model-invoked
`conway_subagent`/`conway_spawn` tool always supplies a real, non-empty
prompt this way) **before** `launch_agent` attaches the child to the tree.
Emitting the live event at that same append site — mirroring `Runtime::
prompt`'s placement — would broadcast `Event::UserTurn` for a child agent
id that has no `Event::AgentSpawned` yet, inverting the ordering
guarantee. `start` instead emits it right after `launch_agent` returns
(i.e. after `AgentTree::attach` has already emitted `AgentSpawned`), which
is the only ordering-safe point for that one code path. A `Fork`'s own head
record (`ForkDirective`) still has no `Event` counterpart at all — see
[`conway-core`](conway-core.md)'s note on why that stays a disclosed,
deliberate scope decision rather than being closed in the same change.

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

**The root-containment check runs first in `decide`, above every allow
path.** An agent with a confinement root (`AgentRoot`, reconstructed once
per agent from `SessionMeta.root`/`SubagentSpec.root` — see
`conway_core::containment::CanonicalRoot`) has each call's declared path
arguments (`Tool::path_args`) checked against it before the cache, pattern
grants, `AutoAllow` mode, or the gate itself are ever consulted, reading the
call's raw `arguments` (never the display-sanitized `rendered` string). A
path outside the root is denied outright; a tool whose call cannot be
statically confined (`PathArgs::Unconfinable`, e.g. `bash`'s command) is
never auto-allowed under a root — it always reaches the gate, though any of
its own `checkable` arguments are enforced the same as a `Named` path. No
root (`AgentRoot::Unconfined`, the default) makes this a complete no-op. A
root that fails to reconstruct fails closed (`AgentRoot::Broken`), denying
every root-relevant call for that agent's run. This is deliberately NOT
delegated to `PermissionGate`: three of the broker's four `Allow` paths
never reach a gate implementation at all, so a check living there would
fail open silently for each of them.

### The honest boundary: what a root does and does not confine

A confinement root confines **the path arguments of path-taking tools** —
the arguments each tool declares via `Tool::path_args`, checked against
`conway_core::containment::CanonicalRoot` before any allow path runs. It
does **not** confine what a shell command does. `bash`'s `cwd` argument is
root-checked like any other path, but its `command` string is **declared
unconfinable, not enforced** — the string goes to `/bin/bash -c` verbatim,
and the broker cannot parse shell to find the paths it touches. **An agent
holding `bash` is not confined by root alone.**

Do not "fix" this by extracting paths out of the command string and
allowing when none are found outside the root. `cd ..`, `$HOME/x`,
`$(echo /etc)/passwd`, `exec 3</etc/passwd`, a shell function, and a
heredoc all defeat any such scan — the input language is a full shell,
not a finite list of shapes. That is the metacharacter-gate lesson
repeated: a transformation of untrusted input whose *failure to find*
something becomes an authorization. See
[`conway-tools`](conway-tools.md)'s `ShellPlugin` section and
`BashTool::path_args`'s own doc comment (`crates/conway-tools/src/shell/
bash.rs`) for the same reasoning recorded at the source.

**The composition that IS a real guarantee.** A confined child is a
child spawned *without* `bash`. `SubagentSpec.tools` already scopes a
child's tool set — `subagent.rs`'s tool-selection block (`let tools =
spec.tools.clone().or_else(|| agent_def.map(|d| d.tools.clone()));`)
resolves the effective tool set once, at spawn, from the caller's request
or the named `AgentDef`; `ToolSelector::Only(vec![])` is already used this
way at `conway/src/intent.rs` (an intent classifier spawned with zero
tools at all). Pairing a confinement root with a `ToolSelector` that
excludes `bash` composes into an actual jail — the child can only touch
paths inside the root, through tools whose arguments the broker can
actually check. Root alone does not do this; root is a path-argument
check, not a capability boundary, and `bash` is the tool whose one
argument that matters most (the command) is outside what any
argument-level check can reach.

**The TOCTOU limit.** The root check happens once, in the permission
broker, before the call is dispatched; the file a tool actually opens is
opened later, inside the tool, across a task boundary (the gate may also
hold the call open indefinitely waiting on a human decision in between).
Anyone able to create a symlink inside the root between those two moments
— including the very agent whose call is being checked, if it also holds
`bash` — defeats the check: the path that resolved inside the root at
check time can resolve outside it by the time the tool opens it. Closing
that gap requires `openat`/`O_NOFOLLOW`-style resolution inside the tools
themselves, which is tool-layer sandboxing and is out of scope for this
design (GP-08: the harness's responsibility ends at the permission model,
not the filesystem). **Root protects against mistaken and
prompt-injected paths, not against an adversarial agent holding `bash`.**

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
