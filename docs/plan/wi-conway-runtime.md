Size assessment below, then 12 work items (WI-076…WI-087).

**Module size assessment:** `conway-runtime` decomposes into **12 work items** — above the nominal 2–8 band. It contains three separable sub-systems (S1 single-agent engine: WI-076…WI-082; S2 multi-agent: WI-083…WI-086; S3 provenance inspection: WI-087). I have kept it as one module because S2 mutates S1's files (`agent_loop.rs`, `runtime.rs`) rather than sitting behind an internal interface, so splitting would create false file-ownership boundaries. Sub-module attribution is noted per item.

**Assumption stated up front:** `conway-core` provides `Event::Lagged { skipped: u64 }`, `Event::SteerDropped { target: AgentId }`, and `RuntimeError` variants for `ContextTooLarge`, `NoCandidate`, `AgentNotFound`, `BudgetExceeded`. If any is absent, the consuming item raises it against `MODULE:conway-core` rather than defining a local duplicate.

---

# WI-076: Crate scaffold, module skeleton, and EventBus

**complexity:** Medium
**sub-system:** S1

**scope:**
- `crates/conway-runtime/Cargo.toml` (create)
- `crates/conway-runtime/src/lib.rs` (create)
- `crates/conway-runtime/src/error.rs` (create)
- `crates/conway-runtime/src/events.rs` (create)
- `crates/conway-runtime/src/context/mod.rs` (create — stub)
- `crates/conway-runtime/src/context/builder.rs` (create — stub)
- `crates/conway-runtime/src/context/prefix.rs` (create — stub)
- `crates/conway-runtime/src/context/report.rs` (create — stub)
- `crates/conway-runtime/src/permission.rs` (create — stub)
- `crates/conway-runtime/src/tools/mod.rs` (create — stub)
- `crates/conway-runtime/src/tools/registry.rs` (create — stub)
- `crates/conway-runtime/src/tools/runner.rs` (create — stub)
- `crates/conway-runtime/src/attempt.rs` (create — stub)
- `crates/conway-runtime/src/agent_loop.rs` (create — stub)
- `crates/conway-runtime/src/runtime.rs` (create — stub)
- `crates/conway-runtime/src/tree.rs` (create — stub)
- `crates/conway-runtime/src/supervisor.rs` (create — stub)
- `crates/conway-runtime/src/subagent.rs` (create — stub)
- `crates/conway-runtime/src/mailbox.rs` (create — stub)
- `crates/conway-runtime/src/step_digest.rs` (create — stub)
- `crates/conway-runtime/src/result.rs` (create — stub)
- `crates/conway-runtime/tests/events_ordering.rs` (create)

**depends:** MODULE:conway-core

**criteria:**
- [machine] `cargo build -p conway-runtime` succeeds; `Cargo.toml` declares deps `conway-core`, `conway-routing`, `conway-session`, `tokio` (features `rt-multi-thread,sync,macros,time`), `tokio-util` (CancellationToken), `tokio-stream`, `futures`, `async-trait`, `blake3`, `serde`, `serde_json`, `jsonschema`, `thiserror`, `tracing`; dev-deps include `conway-core` with feature `fakes`.
- [machine] `cargo tree -p conway-runtime -i conway-backends` returns no path, and `cargo tree -p conway-runtime -i conway-tools` returns no path (runtime names no concrete adapter or plugin crate).
- [machine] `lib.rs` declares every module listed in scope; each stub file compiles as an empty or placeholder module with no public items other than those introduced by later work items.
- [machine] `EventBus::new(capacity: usize) -> Arc<EventBus>` exists; `EventBus::emit(&self, session: SessionId, agent: AgentId, event: Event) -> u64` returns the assigned `seq`.
- [machine] Test: 8 tokio tasks each emit 500 events for the same `SessionId`; the collected `Envelope.seq` values are exactly `0..4000` with no duplicates and no gaps.
- [machine] Test: two different `SessionId`s maintain independent monotonic `seq` counters starting at 0.
- [machine] Test: a subscriber that stops polling until the broadcast buffer overflows receives an `Envelope` carrying `Event::Lagged { skipped }` with `skipped > 0`, and the runtime-side `emit` call never blocks or errors.
- [machine] `BusSink` implements `conway_core::EventSink`; a `BusSink` bound to `(session, agent)` stamps every emitted envelope with those ids.

**notes:**

Objective: Create the crate, fix the module layout so that no two later work items must edit `lib.rs`, and implement the event bus that every other component emits through.

Implementation Notes:
`error.rs` holds `RuntimeResult<T> = Result<T, conway_core::RuntimeError>` plus crate-internal error conversions only; do not define a second public error enum.

`EventBus`:
```rust
pub struct EventBus {
    tx: tokio::sync::broadcast::Sender<Envelope>,
    seqs: std::sync::Mutex<HashMap<SessionId, u64>>,
}
```
`emit` takes the mutex, increments/reads the per-session counter, releases the mutex, constructs `Envelope { seq, ts: Utc::now(), session, agent, event }`, then calls `tx.send` ignoring `SendError` (no subscribers is normal). The mutex is never held across an `await` — `emit` is a synchronous `fn`.

`subscribe(&self) -> EventStream` where `EventStream = Pin<Box<dyn Stream<Item = Envelope> + Send>>`, built from `tokio_stream::wrappers::BroadcastStream` mapping `Err(Lagged(n))` into a synthesized `Envelope` with `Event::Lagged { skipped: n }` and `seq = u64::MAX` (sentinel; the notice is out-of-band). Default capacity 1024.

`BusSink { bus: Arc<EventBus>, session: SessionId, agent: AgentId }` implements `EventSink` so tools report progress without seeing the bus.

Stub files: each contains only `//! placeholder — implemented by WI-0NN`. This item exists so later items never contend on `lib.rs`.

---

# WI-077: ContextBuilder — fixed segment order, provenance, cache hints, PrefixKey

**complexity:** High
**sub-system:** S1

**scope:**
- `crates/conway-runtime/src/context/mod.rs` (modify)
- `crates/conway-runtime/src/context/builder.rs` (modify)
- `crates/conway-runtime/src/context/prefix.rs` (modify)
- `crates/conway-runtime/tests/context_golden.rs` (create)
- `crates/conway-runtime/tests/golden/context_root_simple.json` (create)
- `crates/conway-runtime/tests/golden/context_fork_inherited.json` (create)
- `crates/conway-runtime/tests/golden/context_spawn_clean.json` (create)
- `crates/conway-runtime/tests/golden/context_with_steer_and_toolresults.json` (create)

**depends:** WI-076, MODULE:conway-core

**criteria:**
- [machine] `ContextBuilder::build(&self, input: &ContextInput) -> Result<(Vec<PromptSegment>, ContextReport), RuntimeError>` exists and is pure (no I/O, no `async`).
- [machine] Golden test `context_root_simple`: segments appear in exactly the order `SystemPrompt, SkillFragments*, ToolSchemas, Prompt, volatile*`; serialized output byte-equals the golden file.
- [machine] Golden test `context_fork_inherited`: order is `SystemPrompt, SkillFragments*, ToolSchemas, InheritedPrefix*, ForkDirective, volatile*`; every inherited segment carries `Provenance::Inherited { from, seq_range }` with `seq_range` covering exactly `0..at_seq`.
- [machine] Golden test `context_spawn_clean`: no segment carries `Provenance::Inherited`; segment `[3]` is the `Prompt` with `Provenance::UserPrompt`.
- [machine] Test: every returned `PromptSegment` has a `provenance` field; a compile-fail test (`trybuild` or a doc-test asserting no `Default` impl) shows `PromptSegment` cannot be constructed without one.
- [machine] Cache-hint placement test: with `CacheMode::ExplicitBreakpoints { max_breakpoints: 4, .. }`, exactly two segments have `cache_hint.breakpoint == true` — the last `ToolSchemas` segment (A) and the last `InheritedPrefix` segment (B); with no inherited prefix, exactly one (A).
- [machine] Breakpoint-trim test: with `max_breakpoints: 1`, only breakpoint B survives (priority B > A > none).
- [machine] Cache-neutrality test: for each golden case, `strip_cache_hints(build(input))` and `build(input_with_cache_disabled)` produce identical `(id, role, content, provenance)` tuples — hints change nothing else.
- [machine] `PrefixKey` test: `prefix_key(model, &segments)` == `blake3(model_id ‖ canonical_bytes(segments[0..=B]))`; two sibling inputs differing only in their post-B segments produce the same `PrefixKey`; a differing `model_id` produces a different one.
- [machine] `ContextReport` test: report entry count == segment count, and each entry is `(SegmentId, Provenance, tokens_est)` matching the corresponding segment; `report.estimator == "heuristic-chars4"`.

**notes:**

Objective: Implement the single component that assembles an agent's request context in the fixed §5.3 order with complete provenance and non-correctness-bearing cache hints. This is the highest-risk component in the module (GP-01/02/06/10 converge here) and is golden-file tested from its first commit.

Implementation Notes:
Keep the builder **pure over already-resolved records** so it needs no session dependency and is trivially testable:
```rust
pub struct ContextInput {
    pub agent_id: AgentId,
    pub model: ModelId,
    pub cache_mode: CacheMode,
    pub system_prompt: Option<SystemPromptSpec>,   // text + agent_def name
    pub skills: Vec<SkillFragment>,                // name + text, caller-ordered, stable
    pub tools: Vec<ToolSpec>,
    pub inherited: Option<InheritedPrefix>,        // { from: SessionId, seq_range: SeqRange, records: Arc<[LogRecord]> }
    pub head: HeadSegment,                         // ForkDirective { text, by } | Prompt { text }
    pub own: Arc<[LogRecord]>,                     // volatile: assistant turns, tool results, parent steers
    pub cache_ttl: CacheTtl,
}
```
Ancestry resolution (`TranscriptResolver`) is the caller's job (WI-084); the builder never touches a store.

Segment mapping, in emission order:
1. `SystemPrompt` → one `Role::System` segment, `Provenance::AgentDef { name }`.
2. Each skill → one `Role::System` segment, `Provenance::Skill { name }`.
3. `ToolSchemas` → one `Role::System` segment whose content is the canonical JSON of the sorted tool specs, `Provenance::ToolRegistry { hash: blake3(canonical_json) }`. **Breakpoint A** attaches here.
4. Inherited records → one segment per record, preserving record order, each `Provenance::Inherited { from, seq_range }` where `seq_range` is the record's own single-seq range; **breakpoint B** attaches to the last one.
5. Head segment → `Role::User`, `Provenance::ForkDirective { by }` or `Provenance::UserPrompt`.
6. Own records → one segment each, provenance derived from record kind: assistant → (no breakpoint) `Role::Assistant`; tool_result → `Provenance::ToolResult { call_id, tool }`; parent_steer → `Provenance::ParentSteer { from, parent_seq }`; system note → `Provenance::SystemNote { reason }`.

`SegmentId` is deterministic: `blake3(agent_id ‖ ordinal ‖ provenance_discriminant ‖ content_hash)` truncated — determinism is required for golden files.

Cache-hint rules: emit hints only when `cache_mode` is `ExplicitBreakpoints` or `SlotKv`; for `ImplicitPrefix` and `None`, emit `cache_hint: None` on every segment (ordering alone produces hits). When the count of desired breakpoints exceeds `max_breakpoints`, drop in the order: everything else → A → B (B is last to go).

Token estimate: `tokens_est = ceil(utf8_len / 4)`, recorded with `estimator: "heuristic-chars4"` on the `ContextReport` (T-9: explicitly approximate, never presented as exact).

Golden files are serialized with `serde_json::to_string_pretty` of a stable projection struct (`GoldenSegment { ordinal, role, provenance, cache_hint, content_sha }`) so unrelated content churn does not invalidate them. Test harness supports `UPDATE_GOLDEN=1` regeneration.

---

# WI-078: PermissionBroker — decision cache over the consumer's PermissionGate

**complexity:** Medium
**sub-system:** S1

**scope:**
- `crates/conway-runtime/src/permission.rs` (modify)
- `crates/conway-runtime/tests/permission_broker.rs` (create)

**depends:** WI-076, MODULE:conway-core

**criteria:**
- [machine] `PermissionBroker::new(gate: Arc<dyn PermissionGate>, bus: Arc<EventBus>) -> PermissionBroker` exists.
- [machine] `async fn decide(&self, ctx: &PermissionCtx, call: &ToolCall) -> PermissionOutcome` exists, where `PermissionOutcome ∈ { Allow, Deny { rendered_error: String } }`.
- [machine] Test: two identical calls under `AllowAlways { scope: Session }` invoke the underlying gate exactly once (gate call counter == 1).
- [machine] Test: `AllowAlways { scope: Agent }` caches for the granting agent only; the same tool+args from a sibling agent invokes the gate again.
- [machine] Test: `AllowAlways { scope: AgentSubtree }` is honored for a descendant agent (`agent_path` prefix match) and not for a non-descendant.
- [machine] Test: `AllowOnce` is never cached — two identical calls invoke the gate twice.
- [machine] Test: `Deny { reason }` produces `PermissionOutcome::Deny` whose `rendered_error` contains the reason, and is not cached.
- [machine] Test: `DenyWithFeedback { message }` produces `Deny` with `rendered_error == message`, marked so the caller converts it into a model-visible tool error rather than an abort.
- [machine] Event ordering test: for each decision exactly one `PermissionRequested { call_id, rendered }` precedes exactly one `PermissionResolved { call_id, decision }` in bus `seq` order.
- [machine] Test: a gate future that is dropped/cancelled yields `Deny { reason: "cancelled" }` and still emits `PermissionResolved`.

**notes:**

Objective: Layer a per-session decision cache above the embedder's `PermissionGate` and normalize gate outcomes into an allow/deny result the tool runner can act on, with complete event emission.

Implementation Notes:
Cache key: `CacheKey { tool: ToolName, args_digest: blake3(canonical_json(arguments)) }`. Canonicalization sorts object keys recursively and drops insignificant whitespace, so semantically identical argument objects hit the same entry.

Cache storage: `RwLock<HashMap<CacheKey, Vec<GrantScope>>>` where `GrantScope ∈ { Session, Agent(AgentId), Subtree(AgentId) }`. Lookup order: `Session` grant → hit; `Agent(a)` where `a == ctx.agent_id` → hit; `Subtree(a)` where `ctx.agent_path` contains `a` → hit. The `RwLock` is released before awaiting the gate; never held across `.await`.

`PermissionCtx { agent_id, agent_path: Vec<AgentId>, session: SessionId, cwd }`. The broker builds `PermissionRequest` including the full root→requester `agent_path` (§8 precondition) and `rendered`, which the caller supplies from the tool's own renderer.

Emission sequence per call, strictly: `PermissionRequested` → await gate → insert cache entry if `AllowAlways` → `PermissionResolved`. The gate may block indefinitely; the broker must not impose a timeout (§8: runtime holds the call pending).

Deny is not cached — an embedder may legitimately answer differently next time.

---

# WI-079: PluginRegistry and ToolRunner — schema validation, bounded concurrency, truncation

**complexity:** High
**sub-system:** S1

**scope:**
- `crates/conway-runtime/src/tools/mod.rs` (modify)
- `crates/conway-runtime/src/tools/registry.rs` (modify)
- `crates/conway-runtime/src/tools/runner.rs` (modify)
- `crates/conway-runtime/tests/tool_runner.rs` (create)

**depends:** WI-076, WI-078, MODULE:conway-core

**criteria:**
- [machine] `PluginRegistry::from_plugins(Vec<Arc<dyn Plugin>>) -> Result<PluginRegistry, RuntimeError>` exists; duplicate tool names across plugins return an error naming both plugin ids and the colliding tool name.
- [machine] `PluginRegistry::specs(&self, selector: Option<&ToolSelector>) -> Vec<ToolSpec>` returns specs in a deterministic order (lexicographic by tool name) so `ToolRegistry` provenance hashes are stable.
- [machine] `ToolRunner::run_batch(&self, ctx: &ToolBatchCtx, calls: Vec<ToolCall>) -> Vec<ToolOutcome>` exists; results are returned in input `call_id` order regardless of completion order.
- [machine] Test: arguments failing the tool's JSON Schema produce a `ToolOutcome` with `is_error == true` and a message naming the failing JSON pointer; the tool's `invoke` is never called.
- [machine] Test: with `max_parallel_tools = 2` and 5 concurrently-runnable tools, observed peak in-flight count is exactly 2.
- [machine] Test: triggering the batch `CancellationToken` causes every in-flight tool to return `ToolError::Cancelled` and `run_batch` to return within 100 ms.
- [machine] Test: a tool whose `invoke` panics yields a `ToolOutcome { is_error: true }` naming the tool; `run_batch` returns normally and the process does not abort.
- [machine] Truncation test: a `ToolOutput` exceeding the configured limit with `TruncationPolicy::HeadTail` is truncated to head+tail, and the emitted `LogRecord`-bound metadata records `{ policy, orig_bytes }`.
- [machine] Event ordering test: per call, `ToolCallProposed` → (`PermissionRequested`/`PermissionResolved`) → `ToolCallStarted` → `ToolProgress`* → `ToolCallFinished` appear in that bus `seq` order.
- [machine] Test: a denied call emits no `ToolCallStarted` and produces a `ToolOutcome { is_error: true }` carrying the denial text.

**notes:**

Objective: Own tool dispatch: name resolution, argument validation, permission gating, bounded concurrent execution, cancellation, truncation enforcement, and per-call event emission.

Implementation Notes:
`PluginRegistry` stores `HashMap<ToolName, (PluginId, Arc<dyn Tool>)>` plus a lazily compiled `jsonschema::JSONSchema` per tool (compile once at registry construction; a spec whose schema fails to compile is a construction-time error).

`ToolBatchCtx { agent_id, agent_path, session_id, cwd, cancel: CancellationToken, subagents: Arc<dyn SubagentHost>, plugin_config: Arc<PluginConfig>, max_parallel_tools: usize }`. The runner builds a per-call `ToolCtx` from it, giving each call a **child** `CancellationToken` (`cancel.child_token()`) so a single tool can be cancelled without tearing down the batch.

Execution: `tokio::task::JoinSet` with a `tokio::sync::Semaphore` of `max_parallel_tools` permits. Each spawned future is wrapped in `AssertUnwindSafe(...).catch_unwind()` so a panicking tool becomes an error outcome (never an aborted batch). Results are collected into a `HashMap<call_id, ToolOutcome>` then re-ordered to the input sequence.

Order of operations per call: resolve tool → validate args → emit `ToolCallProposed` → `PermissionBroker::decide` → on `Allow` emit `ToolCallStarted`, invoke → apply truncation → emit `ToolCallFinished { is_error, preview }` (preview = first 200 chars of the first text block).

Truncation: `apply_truncation(&mut ToolOutput, limit_bytes) -> Option<TruncationRecord>`. `HeadTail` keeps `limit/2` from each end with an elided marker line naming the omitted byte count. The `TruncationRecord { policy, orig_bytes }` is returned to the caller for persistence — the runner never writes to the session log itself.

`ToolOutcome { call_id, tool, blocks, is_error, truncation: Option<TruncationRecord>, artifacts }`.

---

# WI-080: Attempt engine — strategy resolution, fallback chain, health recording

**complexity:** High
**sub-system:** S1

**scope:**
- `crates/conway-runtime/src/attempt.rs` (modify)
- `crates/conway-runtime/tests/attempt_fallback.rs` (create)

**depends:** WI-076, WI-077, MODULE:conway-core, MODULE:conway-routing

**criteria:**
- [machine] `AttemptEngine::execute(&self, req: AttemptRequest) -> Result<AttemptOutcome, RuntimeError>` exists, taking an ordered `Vec<Route>` and the assembled segments.
- [machine] Strategy test (table-driven, one case per row of the §runtime strategy table): `Streaming{validated:true}` + tools → `stream()`; `Streaming{validated:false}` + tools → `generate()`; `NonStreamingOnly` + tools → `generate()`; any capability + no tools → `stream()`.
- [machine] Test: a `generate()` path still emits at least one `Event::TextDelta` carrying the full text — the caller-facing stream contract is identical in both paths.
- [machine] Test: `BackendError::ToolParse` on a streamed attempt triggers exactly **one** non-streaming retry of the identical `GenerateRequest` against the **same** route; a second `ToolParse` advances to the next route.
- [machine] Test: `Event::ModelDecision { role, chosen, reason, attempt }` is emitted before every backend call, including each fallback and the non-streaming retry; `attempt` increments monotonically from 0 across the whole chain.
- [machine] Health test: `Transport` and `ServerError` failures call `HealthRegistry::record` with `Observation::TransportError`/`ServerError`; `RateLimit` records `RateLimited { retry_after }`; success records `Ok { latency }`.
- [machine] T-2 test: `ContextOverflow`, `BadRequest`, and `Auth` produce **zero** `HealthRegistry::record` calls; `ContextOverflow` and `BadRequest` advance the chain (`RequestIncompatible` class), `Auth` aborts the chain.
- [machine] Test: when a `record` call transitions a breaker from `Closed` to `Open{until}`, exactly one `Event::BackendDegraded { endpoint, breaker, until }` is emitted; a second failure while already `Open` emits none.
- [machine] T-1 test: if every candidate route's `Capabilities.max_context_tokens < est_tokens`, `execute` returns `RuntimeError::ContextTooLarge { est_tokens, best_candidate, max_context, shortfall }` without calling any backend; no truncation and no escalation occurs.
- [machine] Test: exhausting all routes returns `RuntimeError::NoCandidate` enumerating every route and its terminal failure.

**notes:**

Objective: Turn an ordered candidate list plus assembled segments into one `GenerateResponse`, choosing streaming vs non-streaming from declared capabilities, sequencing fallback, and recording health observations with the correct classification.

Implementation Notes:
```rust
pub struct AttemptRequest<'a> {
    pub agent_id: AgentId, pub session: SessionId, pub role: RoleAlias,
    pub routes: Vec<Route>,
    pub segments: &'a [PromptSegment], pub tools: &'a [ToolSpec],
    pub prefix_key: Option<PrefixKey>, pub est_tokens: u32,
    pub cancel: CancellationToken,
}
pub struct AttemptOutcome { pub response: GenerateResponse, pub route: Route, pub attempts: u8, pub latency: Duration }
```
Backends are looked up from an injected `HashMap<BackendId, Arc<dyn Backend>>` — the engine never constructs one.

Pre-flight (T-1): before the loop, filter routes whose `capabilities(model).max_context_tokens < est_tokens`. If the filtered list is empty, return `ContextTooLarge` naming the largest candidate window and the shortfall in tokens. Do not truncate, do not escalate to an unlisted model.

Failure classification (single function, exhaustively matched so a new `BackendError` variant is a compile error):
| error | health obs | chain action |
|---|---|---|
| `Transport` | `TransportError` | advance |
| `ServerError` | `ServerError` | advance |
| `RateLimit{retry_after}` | `RateLimited{retry_after}` | advance |
| `ContextOverflow` | none | advance (RequestIncompatible) |
| `BadRequest` | none | advance (RequestIncompatible) |
| `ToolParse` | none | retry non-streaming once, then advance |
| `Auth` | none | abort chain, return error |
| `Cancelled` | none | abort chain, return `RuntimeError::Cancelled` |

Breaker-transition detection: capture `health.state(ep)` before `record` and after; emit `BackendDegraded` only on a `Closed → Open` edge. This keeps the event non-repeating without adding a callback to the `HealthRegistry` port.

Streaming consumption: drive the `BoxStream`, mapping `TextDelta`/`ThinkingDelta` chunks to bus events immediately (this is user-visible latency), accumulating into the final `Done(GenerateResponse)`. Select against `cancel.cancelled()` so a hard cancel drops the stream promptly.

Tests use `conway_core::fakes::ScriptedBackend` with a per-attempt script and a fake `Router`/`HealthRegistry` recording invocations.

---

# WI-081: AgentLoop — single-agent turn state machine

**complexity:** High
**sub-system:** S1

**scope:**
- `crates/conway-runtime/src/agent_loop.rs` (modify)
- `crates/conway-runtime/tests/agent_loop_e2e.rs` (create)

**depends:** WI-077, WI-078, WI-079, WI-080, MODULE:conway-core, MODULE:conway-session

**criteria:**
- [machine] `AgentLoop::run(self) -> AgentResult` exists and is infallible in return type — every failure path produces an `AgentResult` with a non-`Completed` status.
- [machine] End-to-end test against `ScriptedBackend`: script `[text-only response]` → loop performs one turn, emits `TurnStarted`, `TextDelta`, `TurnFinished`, `AgentFinished`, returns `ResultStatus::Completed`.
- [machine] End-to-end test: script `[tool_call read, then text]` → two turns; the tool result is appended as `LogRecord::ToolResult` with `Provenance::ToolResult { call_id, tool }`; the second turn's context includes it.
- [machine] Persist-before-act test: with a `FakeStore` recording call order, the assistant `LogRecord` append completes before any tool `invoke` begins, and the `UserTurn` append completes before the first backend call.
- [machine] Budget test: `Budget { max_steps: 2 }` against a backend that always returns a tool call yields `ResultStatus::BudgetExceeded` after exactly 2 turns; likewise `deadline` and `max_tokens` each have a test producing `BudgetExceeded`.
- [machine] Test: a denied tool call is appended as an error `ToolResult` and the loop continues to the next turn (denial feeds back to the model, not an abort).
- [machine] Test: `RuntimeError::NoCandidate` from the attempt engine yields `ResultStatus::Failed { err }` and one `Event::Error { fatal: true }`.
- [machine] Event ordering test: within a turn, bus `seq` order is `TurnStarted < ModelDecision < TextDelta* < TurnFinished < ToolCallProposed*`… asserted by a sequence-matcher over the collected envelopes.
- [machine] Test: triggering the agent's `CancellationToken` mid-tool-batch yields `ResultStatus::Cancelled` and the loop returns within 100 ms.

**notes:**

Objective: Implement the per-agent turn loop exactly as specified in §7 (`conway-runtime` internal design notes), wiring ContextBuilder → Router → AttemptEngine → PermissionBroker → ToolRunner → SessionStore, with budgets and terminal-result construction. No subagent code exists in this item.

Implementation Notes:
```rust
pub struct AgentLoop {
    pub agent_id: AgentId, pub session: SessionId, pub parent: Option<AgentId>,
    pub deps: Arc<LoopDeps>,          // store, router, health, backends, registry, broker, bus, builder
    pub spec: AgentSpec,              // system prompt, skills, tool selector, role, budget, result_contract
    pub cancel: CancellationToken,
}
```
State machine, one iteration = one turn:
1. `drain_inbox()` — a no-op hook in this item (`fn drain_inbox(&mut self) -> Vec<LogRecord> { Vec::new() }`), implemented by WI-085. Placing the call site now guarantees steering lands at a turn boundary by construction.
2. `budget.check(state)` → on exhaustion, `finish(BudgetExceeded)`.
3. Resolve the effective transcript. For a root agent this is `store.read(sid, ..)`; the `InheritedPrefix` path stays `None` here (WI-084 supplies it via an injected `TranscriptSource`).
4. `builder.build(&input)` → `(segments, report)`. Emit one `ContextSegmentAdded` per **newly added** segment (diff against the previous turn's report by `SegmentId`) so the event stream is not quadratic.
5. `router.resolve(RouteRequest { role, pin, required, est_tokens, agent_id })`.
6. `attempt.execute(...)` → `AttemptOutcome`.
7. Append assistant `LogRecord` (with `model`, `route_reason`, `usage`) **before** acting on its tool calls.
8. If `tool_calls.is_empty()` → `finish(Completed)`; summary derived from trailing assistant text (WI-086 replaces this with `report`-tool-aware construction).
9. `tool_runner.run_batch(...)`, append all results, emit `TurnFinished`, loop.

Budget accounting: `LoopState { turn: u32, tokens: u64, started: Instant }`; `max_tokens` accumulates `usage.input + usage.output` across turns; `deadline` is checked at the top of each turn **and** raced via `tokio::select!` against the attempt future so a long generation cannot overrun it unboundedly.

`finish(status)` constructs the `AgentResult`, appends it as a `LogRecord` with `fsync=always` semantics (store policy), then emits `AgentFinished`. Terminal-result construction is deliberately routed through one function so WI-083's supervisor can synthesize an equivalent one.

Never hold a lock across an `await`; `AgentLoop` owns its state exclusively (single task) so no interior mutability is needed for loop state.

---

# WI-082: Runtime facade — RuntimeDeps, start_root, prompt, cancel, subscribe, live context_report

**complexity:** Medium
**sub-system:** S1

**scope:**
- `crates/conway-runtime/src/runtime.rs` (modify)
- `crates/conway-runtime/tests/runtime_api.rs` (create)

**depends:** WI-081

**criteria:**
- [machine] `Runtime::new(deps: RuntimeDeps) -> Arc<Runtime>` exists, with `RuntimeDeps { store, router, health, backends, plugins, gate, agent_defs, event_bus }` — every field a port type (`Arc<dyn _>` or a map thereof).
- [machine] Compile-check test: `RuntimeDeps` is constructible entirely from `conway-core` fakes (`FakeStore`, `ScriptedBackend`, `FakeGate`) plus a fake router; the test crate does not depend on `conway-backends` or `conway-tools`.
- [machine] `Runtime::start_root(&self, spec: RootSpec) -> Result<AgentId, RuntimeError>` creates a session, appends the header, spawns one tokio task, and returns before the first turn completes.
- [machine] `Runtime::prompt(&self, agent: AgentId, text: String) -> Result<(), RuntimeError>` appends `LogRecord::UserTurn` with `Provenance::UserPrompt` **before** returning, and returns `RuntimeError::AgentNotFound` for an unknown agent.
- [machine] `Runtime::cancel(&self, agent: AgentId, reason: String)` trips that agent's `CancellationToken` and the agent's task terminates with `ResultStatus::Cancelled`.
- [machine] `Runtime::subscribe(&self) -> EventStream` yields envelopes emitted after subscription; two concurrent subscribers observe identical `seq` sequences.
- [machine] `Runtime::context_report(&self, agent: AgentId) -> Result<ContextReport, RuntimeError>` returns the most recent turn's report for a live agent.
- [machine] `Runtime::tree(&self) -> AgentTreeSnapshot` returns a snapshot containing exactly the started root agents (single-level; children added in WI-083).
- [machine] Test: two roots started concurrently run in independent tasks and produce interleaved, per-session-monotonic event sequences.

**notes:**

Objective: Expose the runtime's public surface over the agent loop and own agent-task lifecycle for root agents.

Implementation Notes:
```rust
pub struct Runtime {
    deps: RuntimeDeps,
    registry: PluginRegistry,          // built once from deps.plugins
    broker: PermissionBroker,
    agents: RwLock<HashMap<AgentId, AgentHandle>>,
    builder: ContextBuilder,
}
pub struct AgentHandle {
    pub session: SessionId, pub parent: Option<AgentId>,
    pub cancel: CancellationToken,
    pub inbox: mpsc::Sender<AgentMessage>,      // wired in WI-085; created here, unused
    pub result: watch::Receiver<Option<AgentResult>>,
    pub last_report: Arc<Mutex<Option<ContextReport>>>,
    pub join: Arc<Mutex<Option<JoinHandle<()>>>>,
}
```
Lock discipline: `agents` is an `RwLock<HashMap<..>>` holding only cheap clonable handles. Acquire, clone the handle, release — never await while holding it. This is the invariant that makes "no shared mutable agent state" true in practice.

`prompt` writes the `UserTurn` record via the store, then notifies the agent task (via the inbox channel or a dedicated `Notify`) — persist-before-act, so a crash between persist and notify loses no user intent.

`RootSpec { session: Option<SessionId>, agent_def: Option<AgentDefRef>, role: Option<RoleAlias>, tools: Option<ToolSelector>, budget: Budget, cwd: PathBuf, prompt: Option<String> }`.

`context_report` reads `last_report` (updated by the loop each turn). Historical/post-restart reads are added in WI-087; return `RuntimeError::AgentNotFound` for an unknown id here rather than falling back to disk.

The `Runtime` constructs nothing concrete: backends arrive as `HashMap<BackendId, Arc<dyn Backend>>`, plugins as `Vec<Arc<dyn Plugin>>`, gate/store/router/health as trait objects.

---

# WI-083: AgentTree and supervisor — budgets, panic containment, guaranteed terminal results

**complexity:** High
**sub-system:** S2

**scope:**
- `crates/conway-runtime/src/tree.rs` (modify)
- `crates/conway-runtime/src/supervisor.rs` (modify)
- `crates/conway-runtime/src/runtime.rs` (modify)
- `crates/conway-runtime/tests/supervisor.rs` (create)

**depends:** WI-082

**criteria:**
- [machine] `AgentTree::attach(&self, node: AgentNode) -> Result<(), RuntimeError>` and `AgentTree::snapshot(&self) -> AgentTreeSnapshot` exist; the snapshot includes each agent's id, parent, session, status, budget, and steps taken.
- [machine] Test: `AgentTree::path(agent) -> Vec<AgentId>` returns the root→agent chain and is used to populate `PermissionRequest::agent_path`.
- [machine] Test: attaching a node whose parent is unknown returns `RuntimeError::AgentNotFound`; attaching a duplicate id errors.
- [machine] **Panic test:** a child agent task whose loop panics still resolves an awaiting parent — `await_result(child)` returns `AgentResult { status: Failed { .. } }` mentioning the panic, within 1 s, and the parent task is unaffected.
- [machine] **Budget test:** a child whose `deadline` elapses while blocked in a tool call resolves with `ResultStatus::BudgetExceeded` synthesized by the supervisor; `await_result` returns.
- [machine] **Cancel test:** `cancel(child, reason)` with `hard: true` resolves `await_result` with `ResultStatus::Cancelled`.
- [machine] Test: `AgentSpawned` is emitted before any other envelope bearing that `agent_id`, and exactly one `AgentFinished` follows per spawned agent — asserted by a stream invariant checker used across the multi-agent test suite.
- [machine] Test: exactly one `AgentResult` is ever published per agent (a normal completion racing a supervisor synthesis yields one value; the `watch` channel is set-once).
- [machine] Test: cancelling a parent cancels its entire subtree (each descendant's token is tripped) and every descendant produces a terminal result.

**notes:**

Objective: Own the agent tree and the supervision guarantee that `await_result` always terminates. This is the MAST "failure to recognize termination" mitigation and the hardest correctness property in the module.

Implementation Notes:
```rust
pub struct AgentTree { nodes: RwLock<HashMap<AgentId, AgentNode>> }
pub struct AgentNode {
    pub id: AgentId, pub parent: Option<AgentId>, pub session: SessionId,
    pub kind: Option<SubagentMode>, pub budget: Budget,
    pub cancel: CancellationToken,
    pub result_tx: watch::Sender<Option<AgentResult>>,   // set-once
    pub status: AgentStatus,                              // Running | Finished
}
```
Result publication uses a `tokio::sync::watch::Sender<Option<AgentResult>>` plus an `AtomicBool` `resolved` flag. `publish_result` does `compare_exchange` on the flag; the loser discards its value. Every awaiter (`watch::Receiver`) sees the same single value, and awaiters that subscribe *after* resolution still read it immediately — this is why `watch` is used rather than `oneshot`.

Supervisor: for each spawned agent the runtime spawns the loop task and a supervising wrapper:
```rust
let jh = tokio::spawn(async move { loop_.run().await });
tokio::spawn(async move {
    let outcome = tokio::select! {
        r = jh                          => match r { Ok(res) => res, Err(e) if e.is_panic() => synth(Failed{ panic }), Err(_) => synth(Cancelled) },
        _ = sleep_until(deadline)       => { cancel.cancel(); synth(BudgetExceeded) }
        _ = cancel.cancelled()          => synth(Cancelled),
    };
    tree.publish_result(agent, outcome);
});
```
The `JoinError::is_panic()` branch is the panic-containment guarantee; without it a panicking child leaves the parent's tool call pending forever.

Deadline arm: after tripping `cancel`, give the loop a bounded grace window (`min(2s, remaining)`) to publish its own result before synthesizing — the loop's own result is preferred when available because it carries real `usage` and `steps_taken`.

Cancel propagation: `CancellationToken::child_token()` for each child of a parent, so subtree cancellation is structural rather than a manual walk.

`runtime.rs` is modified here only to replace the WI-082 single-level `tree()` stub with `tree.snapshot()` and to route `Runtime::cancel` through the tree.

---

# WI-084: SubagentHost implementation — fork/spawn, inherited context, session forking

**complexity:** High
**sub-system:** S2

**scope:**
- `crates/conway-runtime/src/subagent.rs` (modify)
- `crates/conway-runtime/src/agent_loop.rs` (modify)
- `crates/conway-runtime/tests/subagent_fork_spawn.rs` (create)

**depends:** WI-083, WI-077, MODULE:conway-session

**criteria:**
- [machine] `impl SubagentHost for Runtime` exists with exactly the §4.6 signatures — `start`, `steer`, `await_result`, `cancel`, `tree` — and no additional public methods.
- [machine] Fork test: `start(parent, SubagentSpec { mode: Fork, .. })` calls `SessionStore::fork(parent_sid, parent.head, meta)` exactly once, copies zero records, and the child header carries `ForkOrigin { parent, at_seq, mode: Fork }`.
- [machine] Fork context test: the child's first assembled context contains `InheritedPrefix` segments covering exactly the parent's `0..at_seq`, in order, verbatim, followed by a `ForkDirective` segment with `Provenance::ForkDirective { by: parent }` (golden-file assertion reusing WI-077's harness).
- [machine] Spawn test: a spawned child's context contains **no** `Provenance::Inherited` segment; its system prompt comes from the required `agent_def`; `start` returns `RuntimeError::InvalidSpec` when `mode == Spawn` and `agent_def` is `None`.
- [machine] Test: `Event::AgentSpawned { kind, parent, agent_def, inherited_upto }` is emitted with `inherited_upto == Some(at_seq)` for Fork and `None` for Spawn, and precedes all other events for that agent.
- [machine] Immutability test: the parent appends 5 records after the fork; re-resolving the child's context still yields exactly `0..at_seq` inherited segments — the child sees a snapshot, not a live view.
- [machine] Sibling-sharing test: 3 children forked at the same `(sid, at_seq)` share one `Arc<[LogRecord]>` allocation (pointer equality assertion via `Arc::ptr_eq`) and produce the same `PrefixKey`.
- [machine] Budget test: `start` returns `RuntimeError::InvalidSpec` if `spec.budget` is absent or has neither `max_steps` nor `deadline` set — every child has a budget, by construction.
- [machine] Test: `await_result` on an unknown agent returns `RuntimeError::AgentNotFound`; on a finished agent returns its recorded result immediately.
- [machine] Test: `ToolCtx::subagents` handed to tools is the `Runtime` itself (`Arc<dyn SubagentHost>`); no other privileged path to fork/spawn exists.

**notes:**

Objective: Implement the cycle-breaking `SubagentHost` port on `Runtime`, making fork/spawn a case of context assembly plus O(1) session forking, and wire the inherited-prefix path into the agent loop.

Implementation Notes:
`start(parent, spec)` sequence, in order:
1. Validate spec (mode/agent_def pairing, budget presence, tool selector resolvable).
2. `let at = store.head(parent_sid)` — freeze point.
3. Fork mode: `store.fork(parent_sid, at, meta)`. Spawn mode: `store.create(meta)` with `origin: None`… **correction:** spawn also records `ForkOrigin { parent, at_seq: at, mode: Spawn }` in meta so the tree is reconstructible from headers alone, but the *context* ignores it. This distinction (parent link vs. context inheritance) is the whole Fork/Spawn difference and must be commented at the call site.
4. `tree.attach(AgentNode { .. cancel: parent_cancel.child_token(), .. })`, create the `mpsc::channel(64)` inbox pair.
5. Append the `fork_directive` record (Fork) or the prompt record (Spawn) as seq 0 of the child log.
6. Emit `AgentSpawned` **before** spawning the task — this is what makes the §8 ordering guarantee hold.
7. Spawn the loop task and its supervisor (WI-083).

`agent_loop.rs` changes: add a `TranscriptSource` field resolving the effective transcript. For a child, `conway_session::TranscriptResolver::resolve(&store, sid)` returns the memoized `Arc<[LogRecord]>`; the loop splits it at `origin.at_seq` into `ContextInput::inherited` and `ContextInput::own`. The memoization in `conway-session` is what gives sibling `Arc` sharing — do not add a second cache here.

`steer`/`cancel` here only deliver to the inbox / trip the token; drain semantics land in WI-085. `steer` on an agent with a full inbox must not block (see WI-085).

`await_result` clones the node's `watch::Receiver` and awaits a `Some` value; it holds no tree lock while awaiting.

---

# WI-085: Mailboxes and steering — bounded inbox, turn-boundary drain, overflow policy

**complexity:** Medium
**sub-system:** S2

**scope:**
- `crates/conway-runtime/src/mailbox.rs` (modify)
- `crates/conway-runtime/src/agent_loop.rs` (modify)
- `crates/conway-runtime/tests/steering.rs` (create)

**depends:** WI-084

**criteria:**
- [machine] `Mailbox::new(capacity: usize) -> (MailboxSender, MailboxReceiver)` exists with capacity 64 used by the runtime.
- [machine] **Turn-boundary test:** a steer sent while the agent is mid-tool-call is not present in the context of the in-flight turn, and *is* present as the first user-role segment of the next turn — asserted via the assembled segment list of both turns.
- [machine] Test: a drained steer is appended as `LogRecord::parent_steer` with `Provenance::ParentSteer { from, parent_seq }` and appears in the context with that provenance.
- [machine] Test: no code path injects into a context outside `drain_inbox()`; asserted by a source-level test that `ContextInput::own` is only populated from store records (grep-style assertion or an API shape that makes it structural — `drain_inbox` returns records, it does not mutate segments).
- [machine] Overflow test: sending 70 `Steer` messages into a 64-slot inbox never blocks the sender; the 6 oldest are dropped and exactly 6 `Event::SteerDropped` envelopes are emitted.
- [machine] Test: `Cancel { hard: false }` takes effect at the next turn boundary (the in-flight tool completes); `Cancel { hard: true }` trips the token immediately, aborts in-flight tool futures, and still yields `AgentResult { status: Cancelled }`.
- [machine] Test: `Progress` messages never appear in the parent's assembled context and are emitted as `Event::AgentProgress`.
- [machine] Test: `Event::SteerQueued { target }` is emitted at enqueue time; the envelope carries the queue timestamp so a consumer can render "steer pending".
- [machine] Test: `Result` messages resolve the parent's pending `conway_subagent` tool call exactly once.

**notes:**

Objective: Implement parent↔child messaging with the decision-3 guarantee that steering lands only at turn boundaries, and with an overflow policy that can never deadlock a parent behind a stuck child.

Implementation Notes:
Inbox is `tokio::sync::mpsc::channel::<AgentMessage>(64)`. Because `mpsc` blocks on a full channel and §6.2 forbids blocking the sender, wrap the sender:
```rust
pub struct MailboxSender { tx: mpsc::Sender<AgentMessage>, bus: Arc<EventBus>, target: AgentId }
impl MailboxSender {
    pub fn send(&self, msg: AgentMessage) -> Result<(), RuntimeError> {
        match self.tx.try_send(msg) { Ok(()) => ..., Err(TrySendError::Full(m)) => self.evict_oldest_then_send(m), ... }
    }
}
```
Oldest-drop with an `mpsc` requires either a side buffer or a `VecDeque` under a `Mutex` plus a `Notify`. Use the latter — it is simpler and makes "oldest-dropped" exact:
```rust
struct Inbox { q: Mutex<VecDeque<AgentMessage>>, notify: Notify, cap: usize }
```
`push`: lock, if `len == cap` pop_front and emit `SteerDropped`, push_back, unlock, `notify_one`. `drain`: lock, `std::mem::take` the deque, unlock. The mutex is a `std::sync::Mutex` held only for pointer moves — never across an `await`.

`drain_inbox()` in `agent_loop.rs` replaces the WI-081 no-op. It classifies drained messages:
- `Steer` → append `LogRecord::parent_steer` (persist-before-act: appended before entering the context), returned as a record for `ContextInput::own`.
- `Cancel { hard: false }` → set a `pending_cancel` flag consumed by the top-of-loop budget/cancel check.
- `Cancel { hard: true }` → handled at enqueue time by the sender tripping the token directly, not at drain (drain would be too late by definition).
- `Progress` → emit `Event::AgentProgress`; never a record, never a segment (context-clash prevention).
- `Result` → resolve the pending subagent tool call future.

Pending-subagent bookkeeping: `HashMap<AgentId, oneshot::Sender<AgentResult>>` on the parent's loop state, keyed by child id, populated when a blocking `conway_subagent` call starts.

---

# WI-086: AgentResult construction, result contract validation, and MAST mitigations

**complexity:** High
**sub-system:** S2

**scope:**
- `crates/conway-runtime/src/result.rs` (modify)
- `crates/conway-runtime/src/step_digest.rs` (modify)
- `crates/conway-runtime/src/agent_loop.rs` (modify)
- `crates/conway-runtime/tests/result_contract.rs` (create)
- `crates/conway-runtime/tests/step_digest.rs` (create)

**depends:** WI-085

**criteria:**
- [machine] `ResultBuilder::from_report_tool(...)` and `ResultBuilder::from_trailing_text(...)` exist; a `report` tool call in the final turn takes precedence over inferred trailing text.
- [machine] Test: `summary` is always non-empty and is truncated to 2000 chars (configurable) with an elision marker; an agent producing no text yields a summary naming the terminal status rather than an empty string.
- [machine] Test: every terminal path (`Completed`, `Failed`, `Cancelled`, `BudgetExceeded`, `Rejected`) produces an `AgentResult` with `transcript_ref` set to the agent's own `SessionId`, populated `usage`, and correct `steps_taken`.
- [machine] Contract test: with `result_contract: Some(schema)`, a child whose `structured` output fails validation is retried **exactly once** with the validation error injected as a system note; a second failure yields `ResultStatus::Rejected { missing }` enumerating the failing schema paths.
- [machine] Contract test: valid `structured` output on the first attempt causes zero retries.
- [machine] Test: `transcript_ref` content is never auto-injected into the parent's context — the parent's `ToolResult` for `conway_subagent` contains only `summary`, `facts`, `artifacts`, `structured`, `usage`, `status`, and the `transcript_ref` id.
- [machine] StepDigest test: `blake3(tool_name ‖ canonical_normalized_args)`; the 3rd identical digest emits exactly one `Event::RepeatedStep { tool, prior_seq }` and appends one `LogRecord` system note with `Provenance::SystemNote { reason: "repeated_step" }` citing the prior result's `seq`.
- [machine] StepDigest test: the 4th and 5th identical calls emit no further `RepeatedStep` for that digest (one notice per digest, not per call); two different digests are tracked independently.
- [machine] StepDigest test: argument normalization means `{"a":1,"b":2}` and `{"b":2,"a":1}` yield the same digest; `{"a":2}` yields a different one.
- [machine] Test: the digest ring is bounded (default 64 entries) and evicts least-recently-used entries without unbounded growth over 10 000 calls.

**notes:**

Objective: Finalize the `AgentResult` contract and implement the three MAST mitigations the runtime owns: repeated-step detection, schema-enforced result contracts with a single corrective retry, and `Rejected{missing}` as a first-class terminal status.

Implementation Notes:
`ResultBuilder` collects, over the agent's lifetime: accumulated `Usage`, `steps_taken`, artifacts emitted by tools, and the last `report` tool invocation (if any). At `finish(status)` the loop calls `builder.build(status)`.

Precedence for `summary`/`facts`/`artifacts`/`structured`: (1) the `report` tool's arguments if the agent called it; (2) trailing assistant text for `summary`, empty `facts`, tool-collected `artifacts`. Rationale is in `conway-tools`' `ReportPlugin` scope — the runtime must not *require* `report`, only prefer it.

Result-contract enforcement lives at the `finish` boundary, not in the tool layer:
```rust
match validate(&structured, &contract) {
  Ok(_) => finish,
  Err(errs) if !retried => { append system note with errs; retried = true; continue loop }  // one more turn
  Err(errs) => finish(Rejected { missing: errs.paths() }),
}
```
The retry is one additional *turn*, not one additional backend call — the model gets a chance to call `report` again with corrected output. The system note carries `Provenance::SystemNote { reason: "result_contract_violation" }`.

`StepDigest`:
```rust
pub struct StepDigest { ring: LruCache<[u8;32], DigestEntry> }   // cap 64
struct DigestEntry { count: u8, first_seq: LogSeq, noticed: bool }
```
`observe(&mut self, tool: &ToolName, args: &Value, seq: LogSeq) -> Option<RepeatedStep>` returns `Some` only when `count` reaches 3 and `noticed == false`, then sets `noticed = true`. Normalization: recursively sort object keys, drop nulls, and render with `serde_json::to_vec` — same canonicalization function as the permission cache (share it, do not duplicate).

The injected system note lists the prior result's `seq` so the model can reference the earlier tool result instead of re-running it.

---

# WI-087: ContextReport persistence and provenance inspection API

**complexity:** Medium
**sub-system:** S3

**scope:**
- `crates/conway-runtime/src/context/report.rs` (modify)
- `crates/conway-runtime/src/runtime.rs` (modify)
- `crates/conway-runtime/tests/context_report_persistence.rs` (create)

**depends:** WI-086, MODULE:conway-session

**criteria:**
- [machine] Every turn persists its `ContextReport` via `SessionStore::append` as a dedicated `LogRecord` kind, written **after** the assistant record for that turn.
- [machine] Test: `Runtime::context_report(agent)` returns the live report for a running agent and the last persisted report for an agent whose task has finished.
- [machine] Restart test: a fresh `Runtime` over the same store (no in-memory state) returns a `ContextReport` for a completed agent that byte-equals the one observed live.
- [machine] `Runtime::context_report_at(&self, agent: AgentId, turn: u32) -> Result<ContextReport, RuntimeError>` returns the report for a historical turn; an out-of-range turn returns a typed error naming the valid range.
- [machine] Completeness test: for every turn of a fork-and-steer scenario, the union of report provenance variants includes `AgentDef`, `ToolRegistry`, `Inherited`, `ForkDirective`, `ParentSteer`, and `ToolResult`, and the report entry count equals the assembled segment count for that turn.
- [machine] Test: each report carries `estimator: "heuristic-chars4"` and per-entry `tokens_est`; the report's total equals the sum of entries.
- [machine] Test: a truncated tool result appears in the report with its `TruncationRecord` visible (a truncation is a context-affecting event and must be inspectable).
- [machine] Test: `context_report` on an unknown agent returns `RuntimeError::AgentNotFound`; on a known-but-never-run agent returns an empty report rather than an error.

**notes:**

Objective: Make GP-10 answerable for historical turns and across process restarts, not just for the live turn, by persisting `ContextReport` alongside each turn and reading it back.

Implementation Notes:
`ContextReport` is defined by `conway-session` (`provenance::ContextReport`) per that module's Provides; this item consumes that type and does not redefine it. If the shape there lacks `estimator` or per-entry truncation metadata, raise it against `MODULE:conway-session` rather than defining a runtime-local variant.

Write path: `agent_loop` already produces `(segments, report)`; this item adds the append after the assistant record so a report is never durable for a turn that did not happen.

Read path in `runtime.rs`:
1. Live agent → `AgentHandle::last_report` (in-memory, cheap).
2. Otherwise → scan the agent's session records in reverse for the newest report record; `context_report_at(turn)` filters by the report's recorded turn index.
3. Ancestry is not walked — a child's report already contains its `Inherited` entries, so a single-session read is complete by construction.

Reverse scan cost is acceptable at MVP scale; if it becomes hot, the fix belongs in `conway-session`'s index, not here.

---

## Coverage Statement

**Module:** conway-runtime
**Work items:** WI-076, WI-077, WI-078, WI-079, WI-080, WI-081, WI-082, WI-083, WI-084, WI-085, WI-086, WI-087

**Coverage:** These 12 work items collectively implement 100% of the `conway-runtime` scope as specified in §7: agent loop (WI-081), agent tree + supervisor (WI-083), mailboxes (WI-085), context assembly + provenance (WI-077, WI-087), plugin registry (WI-079), permission brokering (WI-078), tool execution (WI-079), backend attempt/fallback sequencing (WI-080), event bus (WI-076), budgets (WI-081, WI-083), and the `SubagentHost` implementation (WI-084). Nothing in the module scope is excluded. Explicitly *not* covered here because they belong to other modules: config discovery/parsing, CLI/TUI, wire formats, routing policy, storage format.

Design tensions resolved by directive and implemented: **T-1** as strict rejection (WI-080, `ContextTooLarge`, no truncation/escalation); **T-2** as a `RequestIncompatible` class that advances the chain without a health observation (WI-080); **T-3** as one non-streaming retry (WI-080); **T-9** as an explicitly-marked heuristic estimator (WI-077, WI-087). **T-4** (cache affinity) and **T-5** (`steer_urgent`) are deliberately not implemented — no `RouteRequest::prefer_affinity`, no urgent-steer path.

**Provides implemented by:**
| Provides | Work item(s) |
|---|---|
| `Runtime::new(RuntimeDeps)` / injectable ports | WI-082 |
| `Runtime::start_root(RootSpec)` | WI-082 |
| `impl SubagentHost for Runtime` | WI-084 (`start`/`await_result`), WI-085 (`steer`/`cancel` delivery), WI-083 (`tree`) |
| `Runtime::prompt` / `::steer` / `::cancel` | WI-082, WI-085 |
| `Runtime::tree() -> AgentTreeSnapshot` | WI-083 (stub in WI-082) |
| `Runtime::context_report(agent)` | WI-082 (live), WI-087 (persisted/historical) |
| `Runtime::subscribe() -> EventStream` | WI-076 (bus), WI-082 (surface) |
| `ContextBuilder` (crate-internal, golden-tested) | WI-077 |
| `PermissionBroker` | WI-078 |
| `AgentTree` + supervisor + budgets | WI-083 |
| Mailboxes / steering | WI-085 |
| `ToolRunner` + `PluginRegistry` | WI-079 |
| `attempt_with_fallback` + health recording | WI-080 |
| `StepDigest` + `AgentResult` contract + MAST mitigations | WI-086 |
| `EventBus` + event ordering guarantees | WI-076, plus invariant checker used in WI-083 |

**Requires consumed by:**
| Requires | Work item(s) |
|---|---|
| `conway-core` — all ports/types, `Provenance`, `Event`, fakes | WI-076 (types, `EventSink`), WI-077 (`PromptSegment`, `Provenance`, `CacheMode`), WI-078 (`PermissionGate`), WI-079 (`Plugin`/`Tool`/`ToolCtx`), WI-080 (`Backend`, `BackendError`), WI-081 (`LogRecord`, `Budget`, `ScriptedBackend`), WI-084 (`SubagentHost`, `SubagentSpec`), WI-086 (`AgentResult`, `ResultStatus`) |
| `conway-routing` — `Router`, `HealthRegistry` | WI-080 (`resolve`, `record`, breaker state), WI-081 (route request construction) |
| `conway-session` — `SessionStore`, `TranscriptResolver`, `ContextReport` | WI-081 (append/read, persist-before-act), WI-084 (`fork`, ancestry resolution, sibling `Arc` sharing), WI-087 (`ContextReport` persistence) |
| `conway-backends` — only as injected `Arc<dyn Backend>` | none — no item requires the real adapter; all backend tests use `ScriptedBackend`. No compile-time dependency exists (asserted by WI-076's `cargo tree` criterion). |
| `conway-tools` — only as injected `Arc<dyn Plugin>` | none — no compile-time dependency (asserted by WI-076); `SubagentPlugin` consumes `SubagentHost` from the other side. |

**DAG check:** WI-076 → {077, 078, 080}; {076,078} → 079; {077,078,079,080} → 081 → 082 → 083 → 084 → 085 → 086 → 087. Acyclic. Maximum parallel width 3 (077, 078, 080 concurrently after 076). No file is listed by two items that are not connected by a dependency edge; `lib.rs` is created once (WI-076) and never modified again; `agent_loop.rs` and `runtime.rs` are modified only along the linear chain 081→082→083→084→085→086→087.