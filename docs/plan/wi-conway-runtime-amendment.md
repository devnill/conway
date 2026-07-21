Addendum folded in. Two items change (WI-080, WI-081) plus one new `RuntimeDeps` field in WI-082. Revised sections below replace the corresponding sections in my previous output; all other items, dependencies, and the DAG are unchanged.

---

# WI-080 (REVISED): Attempt engine — strategy resolution, fallback chain, headroom-aware context gate, health recording

**complexity:** High
**sub-system:** S1

**scope:**
- `crates/conway-runtime/src/attempt.rs` (modify)
- `crates/conway-runtime/src/headroom.rs` (create)
- `crates/conway-runtime/tests/attempt_fallback.rs` (create)
- `crates/conway-runtime/tests/headroom_gate.rs` (create)

**depends:** WI-076, WI-077, MODULE:conway-core, MODULE:conway-routing

**criteria:** (unchanged criteria from the prior version retained; the T-1 criterion is replaced and four are added)
- [machine] `AttemptEngine::execute(&self, req: AttemptRequest) -> Result<AttemptOutcome, RuntimeError>` exists, taking an ordered `Vec<Route>` and the assembled segments.
- [machine] Strategy test (table-driven, one case per row of the §runtime strategy table): `Streaming{validated:true}` + tools → `stream()`; `Streaming{validated:false}` + tools → `generate()`; `NonStreamingOnly` + tools → `generate()`; any capability + no tools → `stream()`.
- [machine] Test: a `generate()` path still emits at least one `Event::TextDelta` carrying the full text.
- [machine] Test: `BackendError::ToolParse` on a streamed attempt triggers exactly one non-streaming retry of the identical `GenerateRequest` against the same route; a second `ToolParse` advances to the next route.
- [machine] Test: `Event::ModelDecision { role, chosen, reason, attempt }` is emitted before every backend call, including each fallback and the non-streaming retry; `attempt` increments monotonically from 0.
- [machine] Health test: `Transport`/`ServerError` record `Observation::TransportError`/`ServerError`; `RateLimit` records `RateLimited { retry_after }`; success records `Ok { latency }`.
- [machine] T-2 test: `ContextOverflow`, `BadRequest`, `Auth` produce zero `HealthRegistry::record` calls; `ContextOverflow`/`BadRequest` advance the chain, `Auth` aborts.
- [machine] Test: a `Closed → Open` breaker transition emits exactly one `Event::BackendDegraded`; a further failure while `Open` emits none.
- [machine] **T-1 headroom gate test (replaces prior T-1 criterion):** a candidate is admissible iff `est_tokens + headroom <= capabilities.max_context_tokens`. With `est_tokens = 30_000`, `headroom = 4_000`, and candidate windows `[32_768, 32_000]`, both are rejected (30_000 + 4_000 > 32_768) and `execute` returns `RuntimeError::ContextTooLarge { input_tokens: 30_000, headroom: 4_000, required: 34_000, largest_candidate: ModelRef, largest_window: 32_768, shortfall: 1_232 }` without calling any backend. No truncation and no escalation to an unlisted model occurs.
- [machine] **Boundary test:** `est_tokens + headroom == max_context_tokens` is **admissible** (inclusive bound); `est_tokens + headroom == max_context_tokens + 1` is rejected.
- [machine] **Mixed-candidate test:** with windows `[32_768, 200_000]`, the 32k candidate is skipped by the gate and the 200k candidate is attempted; the skip emits `Event::ModelDecision` for the attempted route only, and the skipped candidate appears in the returned `AttemptOutcome::skipped` with reason `CapabilitySkip { skipped, missing: ["min_context"] }`. The gate records **no** health observation for a skipped candidate.
- [machine] **Error-message test:** the `Display` output of `ContextTooLarge` names, in text, the input token count, the headroom value, the required total, and the largest candidate's window and model ref (assert all five substrings present).
- [machine] **`max_tokens` default test:** the `GenerateRequest.params.max_tokens` sent to the backend equals the resolved headroom when `AttemptRequest.max_tokens_override` is `None`, and equals the override when `Some(n)` — asserted by capturing the request on `ScriptedBackend`. The override is passed through unchanged even if it exceeds headroom (an explicit setting is not silently clamped; the gate arithmetic still uses headroom).

**notes:**

Objective: Turn an ordered candidate list plus assembled segments into one `GenerateResponse`, choosing streaming vs non-streaming from declared capabilities, sequencing fallback, enforcing the headroom-aware context gate, and recording health observations with the correct classification.

Implementation Notes:
```rust
pub struct AttemptRequest<'a> {
    pub agent_id: AgentId, pub session: SessionId, pub role: RoleAlias,
    pub routes: Vec<Route>,
    pub segments: &'a [PromptSegment], pub tools: &'a [ToolSpec],
    pub prefix_key: Option<PrefixKey>,
    pub est_tokens: u32,
    pub headroom: u32,                       // resolved by the caller (WI-081) — the engine never reads config
    pub max_tokens_override: Option<u32>,
    pub cancel: CancellationToken,
}
```
`headroom.rs` owns the policy type and its resolution, used by WI-081:
```rust
pub struct HeadroomPolicy { pub default_tokens: u32, pub per_role: HashMap<RoleAlias, u32> }
impl HeadroomPolicy { pub fn resolve(&self, role: &RoleAlias) -> u32 { *self.per_role.get(role).unwrap_or(&self.default_tokens) } }
```
Default `default_tokens = 4096`. If `RoutingConfig`/`ConwayConfig` in `conway-core` has no field to carry this, raise it against `MODULE:conway-core` (`headroom_tokens` global + `roles.<name>.headroom_tokens`) rather than inventing a runtime-local config file; the runtime receives an already-parsed `HeadroomPolicy` through `RuntimeDeps`.

Pre-flight gate (T-1), before the attempt loop:
```rust
let required = est_tokens.saturating_add(headroom);
let (admissible, skipped): (Vec<_>, Vec<_>) = routes.into_iter()
    .partition(|r| backends[&r.backend].capabilities(&r.model).max_context_tokens >= required);
if admissible.is_empty() {
    let (mref, window) = largest_candidate(&skipped);
    return Err(RuntimeError::ContextTooLarge {
        input_tokens: est_tokens, headroom, required,
        largest_candidate: mref, largest_window: window,
        shortfall: required - window,
    });
}
```
The bound is inclusive (`>=`), so a request that exactly fills the window with its headroom is allowed. `shortfall` is computed against the largest candidate so the message answers "how much smaller must this context be to run at all."

Skipped candidates are surfaced as `RoutingReason::CapabilitySkip { skipped, missing: vec!["min_context"] }` on `AttemptOutcome`, so `conway routes explain` and `/why` can show the headroom-driven skip. They are **not** health observations — the endpoint is fine, the pairing is wrong (same principle as T-2).

`max_tokens` on the wire: `params.max_tokens = max_tokens_override.unwrap_or(headroom)`. Headroom is the reservation and therefore the natural output cap; an explicit override wins and is never clamped, because silently shrinking a caller's explicit setting is the kind of invisible degradation GP-06 forbids. Note in a code comment that an override exceeding headroom can overflow the window at generation time and will surface as `BackendError::ContextOverflow` → chain advance, which is the correct honest failure.

Failure classification table, `Display` requirements, breaker-edge detection, and streaming consumption are unchanged from the prior version of this item.

---

# WI-081 (REVISED): AgentLoop — single-agent turn state machine, headroom-aware RouteRequest

**complexity:** High
**sub-system:** S1

**scope:**
- `crates/conway-runtime/src/agent_loop.rs` (modify)
- `crates/conway-runtime/tests/agent_loop_e2e.rs` (create)

**depends:** WI-077, WI-078, WI-079, WI-080, MODULE:conway-core, MODULE:conway-session

**criteria:** (prior criteria retained; three added)
- [machine] `AgentLoop::run(self) -> AgentResult` exists and is infallible in return type — every failure path produces an `AgentResult` with a non-`Completed` status.
- [machine] End-to-end test against `ScriptedBackend`: text-only response → one turn, emits `TurnStarted`, `TextDelta`, `TurnFinished`, `AgentFinished`, returns `Completed`.
- [machine] End-to-end test: `[tool_call read, then text]` → two turns; the tool result is appended with `Provenance::ToolResult { call_id, tool }` and appears in the second turn's context.
- [machine] Persist-before-act test: assistant `LogRecord` append completes before any tool `invoke`; `UserTurn` append completes before the first backend call.
- [machine] Budget tests: `max_steps`, `deadline`, and `max_tokens` each produce `ResultStatus::BudgetExceeded`.
- [machine] Test: a denied tool call is appended as an error `ToolResult` and the loop continues.
- [machine] Test: `RuntimeError::NoCandidate` yields `ResultStatus::Failed { err }` and one `Event::Error { fatal: true }`.
- [machine] Event ordering test: within a turn, `TurnStarted < ModelDecision < TextDelta* < TurnFinished < ToolCallProposed*` in bus `seq` order.
- [machine] Test: tripping the agent's `CancellationToken` mid-tool-batch yields `Cancelled` within 100 ms.
- [machine] **Headroom propagation test:** the loop resolves `headroom = policy.resolve(&agent_role)` and constructs `RouteRequest` with `est_tokens` from the `ContextReport` total and `required.min_context = est_tokens + headroom`; asserted by capturing the `RouteRequest` on a fake `Router`. A per-role override in `HeadroomPolicy` is reflected in that value; absent an override, the global default is used.
- [machine] **Consistency test:** the `min_context` value in `RouteRequest` and the `required` value used by the attempt engine's gate are derived from the same `(est_tokens, headroom)` pair — a single test drives one turn and asserts the captured `RouteRequest.required.min_context == captured AttemptRequest.est_tokens + captured AttemptRequest.headroom`. The router-side filter and the engine-side gate can never disagree.
- [machine] **Rejection surfacing test:** when the attempt engine returns `ContextTooLarge`, the loop terminates with `ResultStatus::Failed { err }` whose message contains the input tokens, headroom, and largest window; exactly one `Event::Error { fatal: true }` is emitted; **no** retry, no context truncation, and no additional turn occur.

**notes:**

Objective: Implement the per-agent turn loop per §7, wiring ContextBuilder → Router → AttemptEngine → PermissionBroker → ToolRunner → SessionStore, with budgets, headroom-aware route requests, and terminal-result construction. No subagent code exists in this item.

Implementation Notes:
`AgentLoop` gains `headroom: Arc<HeadroomPolicy>` (from `RuntimeDeps`, see WI-082) alongside the fields listed previously. `AgentSpec` gains `headroom_override: Option<u32>` so an agent definition may pin its own reservation; resolution order is `spec.headroom_override` → `policy.per_role[role]` → `policy.default_tokens`. Resolve **once per turn**, into a local, and use that single value for both the `RouteRequest` and the `AttemptRequest` — this is what the consistency test above locks in. Do not resolve it twice.

Step 5 of the turn state machine becomes:
```rust
let est = report.total_tokens_est();
let headroom = resolve_headroom(&self.spec, &self.headroom, &role);
let req = RouteRequest {
    role: role.clone(), pin: self.spec.pin.clone(),
    required: RequiredCaps { min_context: est + headroom, tool_calling: needs_tools, ..Default::default() },
    est_tokens: est, agent_id: self.agent_id,
};
```
Passing `min_context = est + headroom` means the router performs the same admissibility filter declaratively and the attempt engine's gate becomes a backstop for the pin path (a pinned route bypasses chain filtering but must still satisfy the gate). Both must use the identical arithmetic; the shared helper `fn required_context(est: u32, headroom: u32) -> u32 { est.saturating_add(headroom) }` lives in `headroom.rs` and is called from both sites.

`ContextTooLarge` is terminal, not retryable: it maps straight to `finish(Failed { err })`. The loop must not attempt to shrink the context, drop inherited segments, or re-route — decision 1 and the T-1 resolution both forbid it, and a silent shrink would destroy the golden-file guarantees from WI-077.

All other implementation notes (state machine steps 1–9, budget accounting, `finish()`, lock discipline, `drain_inbox` no-op hook) are unchanged from the prior version of this item.

---

# WI-082 (DELTA): one added `RuntimeDeps` field

No scope, dependency, or complexity change. `RuntimeDeps` gains `headroom: Arc<HeadroomPolicy>`, supplied by the facade from parsed config. One added criterion:

- [machine] `RuntimeDeps` includes `headroom: Arc<HeadroomPolicy>`; `Runtime::new` with an empty `per_role` map and `default_tokens: 4096` produces agents whose `RouteRequest.required.min_context == est_tokens + 4096`.

---

## Coverage Statement (amended)

Unchanged except as follows.

**Provides mapping addendum:** `attempt_with_fallback` (WI-080) now also owns the headroom-aware T-1 context gate and the `max_tokens` default; `HeadroomPolicy` resolution is owned jointly by WI-081 (per-turn resolution and `RouteRequest` construction) and WI-080 (policy type, shared `required_context` helper, gate enforcement).

**Requires mapping addendum:** `conway-core` — if `RoutingConfig`/`ConwayConfig` lacks `headroom_tokens` (global) and `roles.<name>.headroom_tokens` (per-role) fields, WI-080 raises this against `MODULE:conway-core` as a required config-type addition. The runtime consumes only the already-parsed `HeadroomPolicy`; parsing and precedence remain the facade's responsibility (`MODULE:conway` — no config-loading code enters this module).

**T-1 resolution restated:** a candidate is admissible iff `est_tokens + headroom <= max_context_tokens` (inclusive). When no candidate is admissible, the runtime rejects with `RuntimeError::ContextTooLarge { input_tokens, headroom, required, largest_candidate, largest_window, shortfall }`. No truncation, no escalation, no retry. Enforced at two consistent points: declaratively via `RouteRequest.required.min_context` (WI-081) and as a backstop gate covering the pin path (WI-080), both computing through one shared helper.

**DAG check:** unchanged — the addendum adds no dependency edges and no new file contention (`headroom.rs` is created by WI-080 and only read by WI-081).