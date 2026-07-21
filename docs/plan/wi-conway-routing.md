## Size Assessment

**Right size — 6 work items (WI-031 … WI-036).** The module has five distinct Provides with a clean dependency ordering (config/errors → capability index & breakers in parallel → router → prober & explain). No sub-module split is warranted.

**Stated assumptions / flagged interface gaps** (carried into the items):

1. `RouteRequest.pin: Option<ModelRef>` carries no pin *source*, but `RoutingReason` distinguishes `PinnedByApi` from `PinnedByAgentDef`. **Flagged to conway-core.** Interim binding behavior: the router emits `PinnedByApi` for any `req.pin`. If core adds a pin-source field, WI-034 maps it; no other item changes.
2. `RoutingError` is owned by conway-core. Items assume the variants `NoCandidate { role, considered: Vec<(ModelRef, RoutingReason)> }` and `UnknownRole { role }`. If `UnknownRole` does not exist in core, an unknown role returns `NoCandidate { role, considered: vec![] }` — **flagged to conway-core**, behavior specified either way.
3. `HealthConfig` is not listed in conway-core's Provides; this crate defines and owns it (`BreakerRegistry::new(HealthConfig)` requires it).
4. Module spec signature `HealthProber::spawn(Vec<Arc<dyn Backend>>, HealthConfig)` cannot record observations without a registry. **Flagged.** Bound signature adds `Arc<dyn HealthRegistry>` as the second parameter.
5. Config *discovery/loading* belongs to the facade. This crate owns only serde-derived config types, defaults, and semantic validation.
6. Endpoint identity is 1:1 with backend identity for MVP: `endpoint_of(&ModelRef) -> EndpointId` maps `ModelRef.backend`. Per-model endpoints are out of scope.

---

# WI-031: conway-routing crate scaffold, config types, and failure classification

## Complexity
Medium

## Scope
- `crates/conway-routing/Cargo.toml` (create)
- `crates/conway-routing/src/lib.rs` (create)
- `crates/conway-routing/src/config.rs` (create)
- `crates/conway-routing/src/failure.rs` (create)
- `crates/conway-routing/src/capability.rs` (create — placeholder, body written by WI-032)
- `crates/conway-routing/src/breaker.rs` (create — placeholder, body written by WI-033)
- `crates/conway-routing/src/router.rs` (create — placeholder, body written by WI-034)
- `crates/conway-routing/src/prober.rs` (create — placeholder, body written by WI-035)
- `crates/conway-routing/src/explain.rs` (create — placeholder, body written by WI-036)

## Depends
- MODULE:conway-core

## Criteria
- [machine] `cargo check -p conway-routing` succeeds with the workspace member added.
- [machine] `Cargo.toml` dependencies are exactly: `conway-core` (path), `serde` (derive), `serde_json`, `thiserror`, `humantime-serde`, `chrono`, `tokio` (features `rt`, `time`, `macros`, `sync`), `async-trait`, `futures`. Dev-dependencies: `toml`, `tokio` (feature `test-util`), `conway-core` with feature `fakes`.
- [machine] `Cargo.toml` contains no dependency whose name matches `candle|ort|tokenizers|tch|onnx|fastembed|linfa|smartcore` (GP-07: no learned components link into this crate).
- [machine] `src/lib.rs` declares `pub mod config; pub mod failure; mod capability; mod breaker; mod router; mod prober; mod explain;` and re-exports `DeclarativeRouter`, `BreakerRegistry`, `HealthProber`, `ProberHandle`, `CapabilityIndex`, `RoutingExplain`, `ExplainReport`.
- [machine] `HealthConfig` deserializes from the TOML fragment in Implementation Notes with all fields populated; a fragment omitting every field deserializes to `HealthConfig::default()`.
- [machine] `HealthConfig::default()` equals `{ transport_failures_to_open: 3, probe_failures_to_open: 2, open_duration: 30s, half_open_successes_to_close: 1, probe_interval: 15s, probe_timeout: 2s, probe_enabled: true }`.
- [machine] `config::validate(&RoutingConfig) -> Result<(), Vec<ConfigIssue>>` returns `Err` for: a role with an empty `chain`; a chain entry that does not parse as `backend/model`; a duplicate `ModelRef` within one chain. Each case is covered by a named unit test.
- [machine] `failure::classify(&BackendError) -> FailureClass` maps: `Transport|ServerError|RateLimit` → `FailoverRetryable`; `ContextOverflow|BadRequest` → `RequestIncompatible`; `Auth|Cancelled|ToolParse` → `Fatal`.
- [machine] `failure::observation_for(&BackendError) -> Option<Observation>` returns `None` for every `BackendError` whose class is `RequestIncompatible` or `Fatal`, and `Some(_)` for every `FailoverRetryable` variant. Exhaustive unit test over all `BackendError` variants.
- [machine] `FailureClass::advances_chain()` returns `true` for `FailoverRetryable` and `RequestIncompatible`, `false` for `Fatal`.
- [machine] `cargo test -p conway-routing` passes.

## Notes
**Objective:** Establish the crate, its configuration schema, and the error taxonomy that resolves tension T-2 (`ContextOverflow`/`BadRequest` advance the fallback chain but produce **no** health observation).

**Implementation Notes:**

Placeholder files: `capability.rs`, `breaker.rs`, `router.rs`, `prober.rs`, `explain.rs` are created containing only `// implemented in WI-0NN` plus the minimum item declarations needed for `lib.rs`'s re-exports to compile (empty `pub struct` definitions with `todo!()`-free stub inherent impls are **not** permitted; instead, gate the re-exports: `lib.rs` re-export lines are added by the item that implements each type, and the placeholder files start empty with only a `mod` declaration in `lib.rs`). Concretely: WI-031 writes `mod capability; mod breaker; mod router; mod prober; mod explain;` with each file containing a single comment line, and the `pub use` re-export lines listed in the criteria are added incrementally by WI-032…WI-036 in their own files via `pub` items plus `pub use` lines that WI-031 pre-writes as `pub use` only after those types exist. To keep `cargo check` green at WI-031, write the re-exports as a single `pub use` block guarded by nothing but placed in `lib.rs` **after** the implementing items land — i.e. WI-031's acceptance for the re-export criterion is satisfied when the file lists the module declarations; the `pub use` block is authored by WI-034 (which is the last structural dependency). Do not introduce `todo!()`, `unimplemented!()`, or dummy types anywhere.

Config types (this crate owns `HealthConfig`; `RoutingConfig`/`RoleConfig`/`ModelRef` come from `conway-core`):

```toml
[roles.planner]
chain = [ "anthropic/claude-sonnet-4-6", "ollama-cloud/glm-5.2", "local/qwen3-coder-80b" ]

[roles.fast]
chain = [ "local/qwen3-coder-80b", "anthropic/claude-haiku-4-5" ]

[health]
transport_failures_to_open  = 3
probe_failures_to_open      = 2
open_duration               = "30s"
half_open_successes_to_close = 1
probe_interval              = "15s"
probe_timeout               = "2s"
probe_enabled               = true
```

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HealthConfig {
    pub transport_failures_to_open: u32,
    pub probe_failures_to_open: u32,
    #[serde(with = "humantime_serde")] pub open_duration: Duration,
    pub half_open_successes_to_close: u32,
    #[serde(with = "humantime_serde")] pub probe_interval: Duration,
    #[serde(with = "humantime_serde")] pub probe_timeout: Duration,
    pub probe_enabled: bool,
}
```

`ConfigIssue { role: RoleAlias, position: Option<usize>, kind: ConfigIssueKind, message: String }` where `ConfigIssueKind = EmptyChain | MalformedModelRef | DuplicateEntry`. `message` format is exact and tested: `"role 'planner': chain is empty"`, `"role 'planner' position 1: 'glm-5.2' is not a valid backend/model reference"`, `"role 'fast' position 1: duplicate entry 'local/qwen3-coder-80b' (first at position 0)"`.

`ModelRef` string form is `"<backend>/<model>"`; the model segment may itself contain `/` — split on the **first** `/` only. A ref with an empty backend or empty model segment is malformed.

`failure.rs` exports `pub enum FailureClass { FailoverRetryable, RequestIncompatible, Fatal }` plus the two functions above. `observation_for` maps `Transport → Observation::TransportError`, `ServerError → Observation::ServerError`, `RateLimit{retry_after} → Observation::RateLimited{retry_after}`. This is the crate's single authority on "does this error touch health state"; no other module may re-derive it.

Unit tests live inline in `#[cfg(test)] mod tests` within `config.rs` and `failure.rs`.

---

# WI-032: CapabilityIndex and RequiredCaps satisfaction check

## Complexity
Medium

## Scope
- `crates/conway-routing/src/capability.rs` (modify)

## Depends
- WI-031
- MODULE:conway-core

## Criteria
- [machine] `CapabilityIndex::builder().insert(BackendId, ModelId, Capabilities).build()` produces an index; `get(&ModelRef) -> Option<&Capabilities>` returns the inserted value and `None` for an unknown ref.
- [machine] `CapabilityIndex::from_backends(&[Arc<dyn Backend>], &[ModelRef]) -> CapabilityIndex` calls `Backend::capabilities(model)` once per `(backend, model)` pair present in the slice, and silently omits refs whose backend id is not in the slice. Verified with a counting fake backend.
- [machine] `get` is O(1): the index is backed by a `HashMap<(BackendId, ModelId), Capabilities>`; no linear scan appears in `get`.
- [machine] `capability::satisfies(&Capabilities, &RequiredCaps, est_tokens: u32) -> Result<(), Vec<String>>` returns `Ok(())` when all requirements hold, and `Err` listing **every** unmet requirement (not just the first), in the fixed order: tool_calling, structured_output, parallel_tool_calls, reasoning, reliability_tier, min_context, est_tokens.
- [machine] Missing-reason strings match exactly: `"tool_calling: requires NonStreamingOnly, has None"`, `"structured_output: requires JsonSchema, has None"`, `"parallel_tool_calls: required"`, `"reasoning: required"`, `"reliability_tier: requires Verified, has Community"`, `"min_context: requires >= 40000, has 32768"`, `"est_tokens: request needs 48000, max_context_tokens is 32768"`. One unit test per string.
- [machine] A candidate whose `max_context_tokens < est_tokens` yields the `est_tokens:` missing reason (T-1 resolution: capability-filtered, never truncated).
- [machine] `CapabilityIndex` is `Send + Sync + Clone`; asserted by a static-assertion test.
- [machine] `cargo test -p conway-routing` passes.

## Notes
**Objective:** Provide the `(backend, model) → Capabilities` lookup built at startup and the pure, total predicate the router uses for capability filtering.

**Implementation Notes:**

`ToolCallSupport` ordering for the `>=` comparison is total and fixed: `None < NonStreamingOnly < Streaming{validated:false} < Streaming{validated:true}`. `StructuredOutput`: `None < JsonSchema < Grammar`. `ReliabilityTier`: `Unknown < Community < Verified`. Implement these as a private `rank()` function per enum rather than deriving `Ord` on core types (core types are not owned here and may be `#[non_exhaustive]`); an unknown future variant ranks as its documented-lowest peer and must not panic.

`satisfies` is pure, synchronous, and allocation-free on the success path (`Ok(())` allocates nothing; the `Vec<String>` is allocated only when a requirement fails).

`RequiredCaps` fields are treated as optional requirements: a `None`/`false` field imposes no constraint. `est_tokens` is checked unconditionally against `max_context_tokens`.

`from_backends` does not perform I/O — `Backend::capabilities` is synchronous. The index is immutable after `build()`; there is no runtime mutation path (capability refresh is a rebuild, owned by the facade).

Unit tests inline in `capability.rs` using `conway-core`'s `FakeBackend` (feature `fakes`).

---

# WI-033: BreakerRegistry — dual per-endpoint circuit breakers

## Complexity
High

## Scope
- `crates/conway-routing/src/breaker.rs` (modify)

## Depends
- WI-031
- MODULE:conway-core

## Criteria
- [machine] `BreakerRegistry::new(HealthConfig) -> Arc<BreakerRegistry>` and `BreakerRegistry::with_clock(HealthConfig, Arc<dyn Clock>) -> Arc<BreakerRegistry>` exist; `BreakerRegistry` implements `conway_core::HealthRegistry`.
- [machine] `BreakerRegistry::kind_state(&EndpointId, BreakerKind) -> BreakerState` exposes each breaker independently; `state(&EndpointId)` returns `Open{until}` with the **later** `until` if either breaker is open, else `HalfOpen` if either is half-open, else `Closed`.
- [machine] An unknown `EndpointId` returns `BreakerState::Closed` from both `state` and `kind_state` without inserting an entry (verified by an entry-count accessor used only in tests).
- [machine] Transport breaker: `transport_failures_to_open` consecutive `TransportError`/`ServerError` observations transition `Closed → Open{until = now + open_duration}`. At `transport_failures_to_open - 1` it is still `Closed`.
- [machine] Probe breaker: `probe_failures_to_open` consecutive `ProbeFail` observations transition `Closed → Open`. `ProbeFail` never affects the Transport breaker; `TransportError`/`ServerError` never affect the Probe breaker. Both directions asserted.
- [machine] `Observation::Ok{latency}` resets the consecutive-failure counter on **both** breakers to 0 and counts as a half-open success on both.
- [machine] `Observation::RateLimited{retry_after}` sets the Transport breaker to `Open{until = now + retry_after}` (falling back to `open_duration` when `retry_after` is `None`) without incrementing the consecutive-failure counter; it does not touch the Probe breaker.
- [machine] With a `TestClock`: an `Open{until}` breaker reports `HalfOpen` at `now >= until` **without any `record` call**; `state()` performs no state mutation (asserted by calling `state()` 1000× and observing an unchanged internal generation counter).
- [machine] In `HalfOpen`, `half_open_successes_to_close` `Ok` observations transition to `Closed` with all counters reset; a single failing observation transitions back to `Open{until = now + open_duration}` (fixed duration, no exponential backoff).
- [machine] `record` is safe under concurrency: a test spawning 64 tasks each recording 100 observations on the same endpoint completes without panic and leaves a deterministic terminal state.
- [machine] `record`/`state` take `&self` (no `&mut self`), and `BreakerRegistry` is `Send + Sync`; static-assertion test.
- [machine] `BreakerRegistry::snapshot() -> Vec<(EndpointId, BreakerKind, BreakerState)>` returns entries sorted by `(EndpointId, BreakerKind)` for deterministic reporting.
- [machine] `cargo test -p conway-routing` passes.

## Notes
**Objective:** Own all endpoint health *state*, as two independent breakers per endpoint (Olla pattern), with a fully deterministic, clock-injectable state machine.

**Implementation Notes:**

State machine, applied per `(EndpointId, BreakerKind)`:

```
Closed  --failure, consecutive >= threshold--> Open{until = now + open_duration}
Closed  --Ok--> Closed (consecutive = 0)
Open    --now >= until (computed on read, no write)--> HalfOpen
HalfOpen --Ok, successes >= half_open_successes_to_close--> Closed (all counters reset)
HalfOpen --Ok, successes <  threshold--> HalfOpen (successes += 1)
HalfOpen --failure--> Open{until = now + open_duration} (successes = 0)
```

Observation → breaker routing table (exhaustive; add no other mapping):

| Observation | Transport breaker | Probe breaker |
|---|---|---|
| `Ok{latency}` | success | success |
| `TransportError` | failure | — |
| `ServerError` | failure | — |
| `RateLimited{retry_after}` | force-open until `now + retry_after` | — |
| `ProbeFail` | — | failure |

`BadRequest`, `Auth`, `ContextOverflow` never reach this type — `failure::observation_for` (WI-031) returns `None` for them. Do not add handling for them here.

Storage: `RwLock<HashMap<EndpointId, EndpointBreakers>>` where `EndpointBreakers { transport: BreakerCell, probe: BreakerCell }` and `BreakerCell { consecutive_failures: u32, half_open_successes: u32, opened_until: Option<DateTime<Utc>> }`. `state()` takes a read lock only and derives `HalfOpen` from the clock — the entry is *not* rewritten on expiry. This is what makes the "router never mutates breaker state" rule mechanically true: the router only calls `state`/`kind_state`, both of which are `&self` read-only.

Clock: `pub trait Clock: Send + Sync { fn now(&self) -> DateTime<Utc>; }`, `SystemClock`, and `#[cfg(any(test, feature = "test-clock"))] TestClock` with `advance(Duration)` and `set(DateTime<Utc>)`.

`latency` in `Observation::Ok` is recorded into a per-endpoint `last_latency` field surfaced by `snapshot()`'s companion `metrics()` accessor; it does not influence breaker transitions in MVP (no latency-based tripping).

Tests inline in `breaker.rs`, all using `TestClock` — no `sleep`, no wall-clock dependence.

---

# WI-034: DeclarativeRouter — pin → capability → health → chain-order resolution

## Complexity
High

## Scope
- `crates/conway-routing/src/router.rs` (modify)
- `crates/conway-routing/src/lib.rs` (modify)
- `crates/conway-routing/tests/router_resolution.rs` (create)

## Depends
- WI-031
- WI-032
- WI-033
- MODULE:conway-core

## Criteria
- [machine] `DeclarativeRouter::new(RoutingConfig, Arc<dyn HealthRegistry>, CapabilityIndex) -> Result<DeclarativeRouter, Vec<ConfigIssue>>` exists and returns `Err` for any config rejected by `config::validate`.
- [machine] `impl Router for DeclarativeRouter` with `fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError>`; the function is synchronous and contains no `.await`, no `spawn`, and no I/O.
- [machine] `src/router.rs` contains no reference to prompt text: `rg -n 'prompt|text|content|embed|classif' crates/conway-routing/src/router.rs` returns no match outside comments/doc-strings (GP-07).
- [machine] `lib.rs` exports `pub use router::DeclarativeRouter;` and the crate's remaining `pub use` block; `cargo check -p conway-routing` succeeds.
- [machine] **Pin path:** with `req.pin = Some(ref)`, the returned vec has length 1 with `reason == RoutingReason::PinnedByApi`, regardless of the role's chain, when the pin passes capability and health filters.
- [machine] A pin failing the capability filter returns `Err(NoCandidate{ role, considered })` where `considered == [(pin_ref, CapabilitySkip{ skipped, missing })]`. Same for a health-open pin with `HealthSkip`.
- [machine] **Filter order:** for a candidate that both fails capabilities and has an open breaker, the recorded reason is `CapabilitySkip` (capability filter runs before health filter). Dedicated test.
- [machine] **Chain order preserved:** with all candidates passing, `resolve` returns candidates in declared chain order; `reason[0] == AliasPrimary{alias}` and `reason[i>0] == Fallback{ position: i as u8, after: vec![] }`.
- [machine] **Position is the chain index, not the survivor index:** if chain positions 0 and 1 are skipped, the survivor at position 2 has `Fallback{ position: 2, .. }` and is *not* `AliasPrimary`.
- [machine] **Health filter:** a candidate whose endpoint reports `Open` on the Transport breaker is skipped with `HealthSkip{ skipped, breaker: BreakerKind::Transport }`; `Probe`-open yields `breaker: Probe`; both open yields `Transport`. `HalfOpen` and `Closed` candidates are **retained**.
- [machine] **T-1:** with `est_tokens` exceeding every candidate's `max_context_tokens`, `resolve` returns `Err(NoCandidate{..})` (no truncation, no fallback to a smaller model) and `considered` lists every candidate with the `est_tokens:` missing reason.
- [machine] `Err(NoCandidate{ role, considered })` enumerates **every** chain entry with a reason; `considered.len() == chain.len()` in the all-rejected case, in chain order.
- [machine] Unknown role returns `RoutingError::UnknownRole{role}` (or `NoCandidate{role, considered: vec![]}` if core lacks the variant — assert whichever is compiled).
- [machine] `resolve` never calls `HealthRegistry::record`: a recording-counting fake registry reports 0 `record` calls after 1000 `resolve` calls.
- [machine] Allocation budget: `resolve` performs at most 2 heap allocations on the success path (the result `Vec`, pre-sized with `Vec::with_capacity(chain.len())`, plus the per-`Route` reason). Asserted with a counting allocator test behind `#[cfg(test)]`.
- [machine] `cargo test -p conway-routing` passes, including `tests/router_resolution.rs`.

## Notes
**Objective:** Implement the pure, synchronous, declarative resolution of a `RoleAlias` to an ordered, capability- and health-filtered candidate list, with a `RoutingReason` on every chosen and every skipped candidate.

**Implementation Notes:**

Resolution algorithm — this order is binding:

```
1. pin:        if req.pin.is_some() -> candidates = [pin],  reason base = PinnedByApi
               else                 -> candidates = roles[req.role].chain (Err(UnknownRole) if absent)
2. capability: for each candidate, satisfies(index.get(ref), &req.required, req.est_tokens)
               miss -> record CapabilitySkip{ skipped: ref, missing }, drop candidate
               absent from index -> CapabilitySkip{ skipped: ref, missing: vec!["capabilities: unknown (backend, model) pair".into()] }
3. health:     for each surviving candidate, health.state(endpoint_of(ref))
               Open{..} -> HealthSkip{ skipped: ref, breaker: Transport if transport open else Probe }
               HalfOpen | Closed -> retain
4. order:      surviving candidates keep their original chain index; reason =
               AliasPrimary{alias} when chain index == 0 else Fallback{ position: idx as u8, after: vec![] }
5. empty       -> Err(NoCandidate{ role: req.role.clone(), considered })
```

`after: vec![]` is correct and intentional: the router has no knowledge of attempt outcomes; the runtime populates `AttemptFailure`s as it walks the list. Do not invent attempt history here.

`Route.params`: the role's `params` from `RoleConfig` if present, else `SamplingParams::default()`. Params are not merged per-entry in MVP.

`endpoint_of(&ModelRef) -> EndpointId` maps `ModelRef.backend` 1:1 and is a private helper in `router.rs` shared with `explain.rs` as `pub(crate)`.

Expose `pub(crate) fn evaluate(&self, req: &RouteRequest) -> Evaluation` where `Evaluation { entries: Vec<EvalEntry> }` and `EvalEntry { model_ref, chain_position: Option<u8>, outcome: EvalOutcome }`, `EvalOutcome = Selected(RoutingReason) | Skipped(RoutingReason)`. `resolve` is a thin projection of `evaluate` (map `Selected` → `Route`, else `Err(NoCandidate)`). WI-036 consumes `evaluate` — the two surfaces must never diverge, which this sharing enforces structurally.

Chain index `> u8::MAX` is rejected at `new()` as a `ConfigIssue` (`kind: EmptyChain` is wrong here — add `ChainTooLong`, message `"role 'x': chain has 300 entries, maximum is 255"`).

`tests/router_resolution.rs` is an integration test exercising the full matrix: pin hit / pin capability-miss / pin health-open / all-healthy chain / head-skipped chain / all-rejected / est_tokens overflow / unknown role / half-open retained. Use `BreakerRegistry::with_clock(_, TestClock)` and a hand-built `CapabilityIndex` — no fakes beyond `conway-core`'s.

---

# WI-035: HealthProber — periodic Backend::probe loop

## Complexity
Medium

## Scope
- `crates/conway-routing/src/prober.rs` (modify)

## Depends
- WI-031
- WI-033
- MODULE:conway-core

## Criteria
- [machine] `HealthProber::spawn(backends: Vec<Arc<dyn Backend>>, health: Arc<dyn HealthRegistry>, config: HealthConfig) -> ProberHandle` exists (signature deviation from the module spec is documented in the item's rustdoc).
- [machine] `ProberHandle` exposes `fn shutdown(&self)` (idempotent, non-blocking) and `async fn join(self) -> Result<(), JoinError>`; dropping the handle without `shutdown` does **not** abort the task (documented and tested).
- [machine] With `tokio::time::pause()` and a fake backend, advancing the clock by `probe_interval` triggers exactly one `Backend::probe` call per backend per interval. Verified with a call counter over 5 intervals (5 calls per backend, no drift-induced extras).
- [machine] A probe returning `Ok(ProbeReport)` records `Observation::Ok{latency}` for that backend's endpoint; a probe returning `Err(_)` records `Observation::ProbeFail`.
- [machine] A probe that does not resolve within `probe_timeout` is abandoned and records `Observation::ProbeFail` exactly once (not twice, not also `Ok` when it later resolves).
- [machine] Backends are probed concurrently within a tick: with N backends each taking `probe_timeout`, one tick completes in ~`probe_timeout`, not `N × probe_timeout`. Asserted under `tokio::time::pause`.
- [machine] A backend whose `probe` panics does not terminate the prober task; the panic is caught, `ProbeFail` is recorded, and subsequent ticks still probe that backend.
- [machine] `probe_enabled = false` causes `spawn` to return a handle whose task exits immediately and records zero observations.
- [machine] After `shutdown()`, no further observations are recorded even if the clock advances by 10 intervals.
- [machine] `prober.rs` contains no call to `HealthRegistry::state` (the prober writes health, it does not read routing state).
- [machine] `cargo test -p conway-routing` passes.

## Notes
**Objective:** Keep the Probe breaker fed with liveness data independently of request traffic, so a dead local server is distinguishable from a slow one before a request is routed to it.

**Implementation Notes:**

Loop shape:

```rust
let mut ticker = tokio::time::interval(config.probe_interval);
ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);   // no burst catch-up
loop {
    tokio::select! {
        _ = cancel.cancelled() => break,
        _ = ticker.tick()      => { probe_round(&backends, &health, config.probe_timeout).await; }
    }
}
```

`probe_round` uses a `JoinSet`, one task per backend, each wrapping `tokio::time::timeout(probe_timeout, backend.probe())` in `AssertUnwindSafe(...).catch_unwind()`. Outcome mapping is exhaustive and total:

| probe outcome | observation |
|---|---|
| `Ok(Ok(report))` | `Ok{ latency: measured wall time }` |
| `Ok(Err(_))` | `ProbeFail` |
| `Err(Elapsed)` | `ProbeFail` |
| panic | `ProbeFail` |

The first tick fires immediately (`tokio::time::interval` semantics) — this is intentional so startup health is known before the first turn. Document it.

No jitter in MVP: determinism under `tokio::time::pause` is worth more than thundering-herd avoidance at this scale. Do not add a random component.

`EndpointId` per backend is `endpoint_of` applied to `Backend::id()`; the prober does not enumerate models (probes are per endpoint, not per model).

Cancellation uses `tokio_util::sync::CancellationToken` if already a workspace dependency; otherwise a `tokio::sync::watch<bool>` — do not add `tokio-util` solely for this.

Tests inline in `prober.rs` using `tokio::time::pause`/`advance` and a `CountingProbeBackend` fake defined in the test module (scripted per-call outcomes: Ok, Err, hang, panic).

---

# WI-036: RoutingExplain — ExplainReport for chosen and skipped candidates

## Complexity
Medium

## Scope
- `crates/conway-routing/src/explain.rs` (modify)
- `crates/conway-routing/tests/explain_report.rs` (create)

## Depends
- WI-034
- MODULE:conway-core

## Criteria
- [machine] `RoutingExplain::new(&DeclarativeRouter) -> RoutingExplain` and `RoutingExplain::explain(&self, req: &RouteRequest) -> ExplainReport` exist; `explain` is synchronous and performs no I/O.
- [machine] `ExplainReport` and all nested types derive `Serialize`, `Deserialize`, `Debug`, `Clone`, `PartialEq`; a round-trip `serde_json` test asserts equality.
- [machine] `explain` and `resolve` agree by construction: a property-style test over ≥12 hand-built scenarios asserts that the `ModelRef`s with `EntryOutcome::Selected` in `ExplainReport`, in order, equal the `(backend, model)` pairs `resolve` returns — and that when `resolve` returns `Err(NoCandidate)`, `ExplainReport` has zero `Selected` entries.
- [machine] `ExplainReport.entries.len() == chain.len()` for a non-pinned request (every chain entry appears, selected or skipped), and `== 1` for a pinned request.
- [machine] Each entry carries the same `RoutingReason` value the router produced for that candidate — asserted field-by-field against `resolve`'s output for selected entries and against `NoCandidate.considered` for skipped ones.
- [machine] Each entry carries `breaker: BreakerSnapshot { transport: BreakerState, probe: BreakerState }` read from `HealthRegistry` at explain time, and `capabilities: Option<CapabilitySummary>` (`None` when the `(backend, model)` pair is absent from the index).
- [machine] `explain` does not call `HealthRegistry::record`: a counting fake registry reports 0 `record` calls after 100 `explain` calls.
- [machine] `ExplainReport::render_text(&self) -> String` produces a stable, line-oriented rendering; a golden-file test at `crates/conway-routing/tests/golden/explain_planner.txt` matches byte-for-byte.
- [machine] `cargo test -p conway-routing` passes, including `tests/explain_report.rs`.

## Notes
**Objective:** Answer "why did this model run, and why not the others" as a serializable report, so `conway routes explain <role>` and the CLI's `/why` render from one source of truth.

**Implementation Notes:**

```rust
pub struct ExplainReport {
    pub role: RoleAlias,
    pub pin: Option<ModelRef>,
    pub est_tokens: u32,
    pub required: RequiredCaps,
    pub entries: Vec<ExplainEntry>,
    pub generated_at: DateTime<Utc>,
}
pub struct ExplainEntry {
    pub model_ref: ModelRef,
    pub chain_position: Option<u8>,     // None for a pinned candidate
    pub outcome: EntryOutcome,          // Selected { reason } | Skipped { reason }
    pub capabilities: Option<CapabilitySummary>,
    pub breaker: BreakerSnapshot,
}
pub struct CapabilitySummary {
    pub tool_calling: ToolCallSupport, pub max_context_tokens: u32,
    pub structured_output: StructuredOutput, pub parallel_tool_calls: bool,
    pub reasoning: bool, pub reliability_tier: ReliabilityTier,
}
```

`explain` is implemented **solely** as a projection of `DeclarativeRouter::evaluate` (WI-034) plus a health snapshot read. It must not re-implement filtering; duplicating the filter logic here is the specific bug this structure prevents.

`render_text` format (exact; the golden file encodes it):

```
role: planner  (est_tokens=12000)
  [0] anthropic/claude-sonnet-4-6   SKIPPED  health: transport breaker open until 2026-07-20T10:00:30Z
  [1] ollama-cloud/glm-5.2          SKIPPED  capability: max_context_tokens: requires >= 40000, has 32768
  [2] local/qwen3-coder-80b         SELECTED fallback(position=2)
```

Columns: two-space indent, `[<position>]` (or `[pin]`), model ref left-padded to the longest ref in the report + 2 spaces, then `SELECTED`/`SKIPPED` padded to 8, then the reason. Timestamps are RFC 3339 UTC. Trailing newline present. No ANSI codes — rendering is the CLI's concern.

`generated_at` is injected via the router's `Clock` so the golden test is deterministic.

---

## Coverage Statement

**Module:** conway-routing

**Work items:** WI-031, WI-032, WI-033, WI-034, WI-035, WI-036

**Coverage:** These six work items collectively implement 100% of the module's scope. (a) `DeclarativeRouter` — role-alias→ordered-candidate resolution from static config — is WI-031 (config schema + validation) + WI-032 (capability filter) + WI-034 (resolution). (b) `BreakerRegistry` — per-endpoint circuit breakers and health probing — is WI-033 (breakers) + WI-035 (probing) + WI-031 (the `BackendError`→`Observation` taxonomy that gates what reaches the breakers).

Explicitly excluded, correctly owned elsewhere: making backend calls and walking the returned candidate list (conway-runtime); config file discovery/merging (conway facade); `Capabilities` production from live endpoints (conway-backends `CapabilityProbe` — this crate only indexes what `Backend::capabilities` returns); cost estimation, prompt classification, embeddings, and any content inspection (forbidden by GP-07 and mechanically asserted by WI-034's `rg` criterion and WI-031's dependency-denylist criterion).

Design tensions handled: **T-2** — WI-031's `FailureClass::RequestIncompatible` plus `observation_for(..) -> None` gives the runtime a chain-advancing, health-neutral category for `ContextOverflow`/`BadRequest`. **T-1** — WI-032 filters candidates whose `max_context_tokens < est_tokens`; WI-034 returns `Err(NoCandidate)` when none fit, with no truncation path anywhere in the crate. **T-4** — no cache-affinity input exists in this decomposition, per the architecture's MVP decision.

**Provides implemented by:**
- `DeclarativeRouter::new(RoutingConfig, Arc<dyn HealthRegistry>, CapabilityIndex) -> impl Router` → WI-034 (config types/validation from WI-031)
- `BreakerRegistry::new(HealthConfig) -> Arc<dyn HealthRegistry>`, two independent breakers, configurable threshold/open-duration/half-open → WI-033 (`HealthConfig` from WI-031)
- `HealthProber::spawn(...) -> ProberHandle` → WI-035
- `CapabilityIndex` — `(backend, model) -> Capabilities`, consulted for `RequiredCaps` filtering → WI-032 (consumed by WI-034)
- `RoutingExplain::explain(&RouteRequest) -> ExplainReport` → WI-036

**Requires consumed by:**
- `conway-core::Router` (trait impl) → WI-034
- `conway-core::HealthRegistry` (trait impl) → WI-033; consumed as `Arc<dyn HealthRegistry>` by WI-034, WI-035, WI-036
- `conway-core::RouteRequest` → WI-034, WI-036
- `conway-core::Route` → WI-034
- `conway-core::RoutingReason` (all six variants; `PinnedByAgentDef` unreachable pending the flagged pin-source gap) → WI-034, WI-036
- `conway-core::RoutingConfig` (+ `RoleConfig`, `ModelRef`, `RoleAlias`) → WI-031 (validation), WI-034 (resolution)
- `conway-core::Capabilities` (+ `RequiredCaps`, `ToolCallSupport`, `StructuredOutput`, `ReliabilityTier`) → WI-032, WI-036
- `conway-core::Backend` (`capabilities`, `probe`) → WI-032, WI-035
- `conway-core::BackendError`, `Observation`, `BreakerState`, `BreakerKind`, `EndpointId`, `RoutingError` → WI-031, WI-033, WI-034

**Unresolved items flagged to conway-core (not worked around):** pin-source field on `RouteRequest`; `RoutingError::UnknownRole` variant; `HealthProber::spawn` signature needs the registry parameter.