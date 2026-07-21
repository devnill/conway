## Addendum Applied — Amended Work Items

Headroom folds into four items (WI-031 config schema, WI-032 filter gate, WI-034 resolution + `NoCandidate` shortfall, WI-036 report surface). WI-033 and WI-035 are unchanged. No new work item is warranted — headroom is a declarative config field and a term in an existing inequality, not a new subsystem.

**New flagged interface gap (7):** the per-role override `[roles.<alias>] headroom_tokens` lands inside a `RoleConfig` owned by conway-core. **Flagged to conway-core:** `RoleConfig` should gain `headroom_tokens: Option<u32>`. Interim binding behavior: conway-routing owns a `HeadroomPolicy` sidecar deserialized from the same TOML document; if/when core's `RoleConfig` carries the field, `DeclarativeRouter::new` prefers it and the sidecar is dropped. Either way the resolution happens once at construction and `resolve()` performs a lookup, never a computation.

---

# WI-031 (amended): conway-routing crate scaffold, config types, and failure classification

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
*(all criteria from the original WI-031 remain; the following are added)*

- [machine] `HeadroomPolicy` deserializes from the TOML fragment in Implementation Notes: `[routing] default_headroom_tokens = 4096` plus `[roles.planner] headroom_tokens = 16384`.
- [machine] `HeadroomPolicy::default()` equals `{ default_headroom_tokens: 4096, per_role: {} }`; the constant `config::DEFAULT_HEADROOM_TOKENS == 4096` is public.
- [machine] `HeadroomPolicy::resolve(&self, role: &RoleAlias) -> u32` returns the per-role value when present, else `default_headroom_tokens`. Three unit tests: per-role hit, per-role miss → global, empty policy → 4096.
- [machine] `HeadroomPolicy::resolve` is total — it never returns an error and never panics for any `RoleAlias`, including one absent from `roles`.
- [machine] `config::validate` returns `Err` with `ConfigIssueKind::HeadroomExceedsBudget` when a role's resolved headroom is `>= 1_000_000`, message exactly `"role 'planner': headroom_tokens 1000000 is implausibly large (maximum 999999)"`.
- [machine] `HeadroomPolicy` contains no method or field taking prompt text, token counts derived from content, or any `&str` request payload; `rg -n 'prompt|content|estimate_from' crates/conway-routing/src/config.rs` returns no match outside doc comments (GP-07: headroom is declarative, never computed).

## Notes
**Objective (amended):** …as before, plus own the declarative **headroom reservation** policy: a global default with per-role override, resolved entirely at config time.

**Implementation Notes (added):**

```toml
[routing]
default_headroom_tokens = 4096       # reserved for model output + reasoning

[roles.planner]
chain           = [ "anthropic/claude-sonnet-4-6", "ollama-cloud/glm-5.2", "local/qwen3-coder-80b" ]
headroom_tokens = 16384              # planner reasons long; reserve more

[roles.fast]
chain = [ "local/qwen3-coder-80b", "anthropic/claude-haiku-4-5" ]
                                     # no override -> inherits 4096
```

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HeadroomPolicy {
    pub default_headroom_tokens: u32,          // from [routing]
    #[serde(skip)] pub per_role: BTreeMap<RoleAlias, u32>,   // from [roles.<alias>].headroom_tokens
}
pub const DEFAULT_HEADROOM_TOKENS: u32 = 4096;
```

`per_role` is populated by `HeadroomPolicy::from_document(&toml::Value)` (or, once conway-core's `RoleConfig` carries `headroom_tokens`, by `HeadroomPolicy::from_routing_config(&RoutingConfig)` — write both, the latter delegating when the field exists). Precedence is exactly: per-role override > `[routing] default_headroom_tokens` > `DEFAULT_HEADROOM_TOKENS`.

Headroom is a reservation, not a prediction. There is no code path that derives it from request content, model behavior, or historical usage — that would be a learned/dynamic component and is forbidden by GP-07 in this crate.

---

# WI-032 (amended): CapabilityIndex and RequiredCaps satisfaction check with headroom gate

## Complexity
Medium

## Scope
- `crates/conway-routing/src/capability.rs` (modify)

## Depends
- WI-031
- MODULE:conway-core

## Criteria
*(criteria from the original WI-032 remain except the `est_tokens` string and T-1 criteria, which are superseded below)*

- [machine] Signature is `capability::satisfies(caps: &Capabilities, required: &RequiredCaps, est_tokens: u32, headroom_tokens: u32) -> Result<(), Vec<String>>`.
- [machine] **Gate:** a candidate passes the context check iff `est_tokens.saturating_add(headroom_tokens) <= caps.max_context_tokens`. Boundary tests: `est+headroom == max` passes; `est+headroom == max + 1` fails.
- [machine] **Headroom is load-bearing:** a candidate with `max_context_tokens = 40000`, `est_tokens = 34000`, `headroom_tokens = 16000` is rejected, while the same candidate with `headroom_tokens = 0` is accepted. Dedicated named unit test `headroom_rejects_candidate_that_fits_raw_input`.
- [machine] The context missing-reason string is exactly: `"context: needs 34000 input + 16000 headroom = 50000, model max_context_tokens is 40000"` (integers, no thousands separators, no units suffix). Golden-string unit test.
- [machine] `est_tokens + headroom_tokens` uses saturating arithmetic; `u32::MAX` inputs produce the rejection reason rather than a panic or wrap. Test asserts no overflow panic in debug builds.
- [machine] Missing-reason ordering is: tool_calling, structured_output, parallel_tool_calls, reasoning, reliability_tier, min_context, context (headroom gate last). Test asserts the full ordered vector for a candidate failing all seven.
- [machine] `min_context` (a `RequiredCaps` floor on the model's window) and the headroom gate are independent checks and can both appear in one `Err`.
- [machine] `satisfies` is pure and synchronous; it takes no `&self`, no clock, no registry.
- [machine] `cargo test -p conway-routing` passes.

## Notes
**Objective (amended):** …as before, plus enforce the declarative headroom reservation so a candidate is admitted only when the model's window holds the input **and** the reserved output/reasoning budget.

**Implementation Notes (added):**

The context check replaces the previous bare `est_tokens` check. Its shape is fixed:

```rust
let needed = est_tokens.saturating_add(headroom_tokens);
if needed > caps.max_context_tokens {
    missing.push(format!(
        "context: needs {est_tokens} input + {headroom_tokens} headroom = {needed}, \
         model max_context_tokens is {}", caps.max_context_tokens));
}
```

`satisfies` does not know about roles or config; the caller (WI-034) resolves the effective headroom once and passes it in. This keeps the function a pure predicate over four scalars and keeps headroom policy in exactly one place.

Note the interaction with T-1: with headroom > 0 the filter is *stricter* than before, so the "no candidate fits" path is reached more often. The resolution is unchanged — reject, never truncate.

---

# WI-034 (amended): DeclarativeRouter — pin → capability(+headroom) → health → chain-order resolution

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
*(criteria from the original WI-034 remain except the T-1 criterion, superseded below; the following are added)*

- [machine] `DeclarativeRouter::new(RoutingConfig, HeadroomPolicy, Arc<dyn HealthRegistry>, CapabilityIndex) -> Result<DeclarativeRouter, Vec<ConfigIssue>>` — the `HeadroomPolicy` parameter is second. `new` resolves each role's effective headroom **once** into the compiled chain table.
- [machine] `resolve` performs no headroom arithmetic beyond the single `est_tokens + headroom` sum inside `satisfies`: the effective headroom is read from the precomputed per-role table by index lookup. Asserted by a test that mutates nothing and by the allocation-budget criterion (unchanged at ≤2 allocations on the success path).
- [machine] **Headroom skip carries the right reason:** a candidate rejected solely by the headroom gate is skipped with `RoutingReason::CapabilitySkip{ skipped, missing }` where `missing == ["context: needs 34000 input + 16000 headroom = 50000, model max_context_tokens is 40000"]`. Named integration test `headroom_skip_reports_capability_skip_with_shortfall`.
- [machine] **Headroom changes the outcome, not just the message:** one fixture role with a 40000-token model and `est_tokens = 34000` resolves successfully with `headroom_tokens = 0` and returns `Err(NoCandidate)` with `headroom_tokens = 16000`, all else identical.
- [machine] **T-1 (amended):** when no candidate satisfies `est_tokens + headroom <= max_context_tokens`, `resolve` returns `Err(NoCandidate{ role, considered })` — no truncation, no headroom relaxation, no fallback to a smaller model. `considered` lists **every** chain entry, each with its shortfall string naming the headroom reservation.
- [machine] **Per-role override is honored end-to-end:** with `[routing] default_headroom_tokens = 4096` and `[roles.planner] headroom_tokens = 16384`, a `planner` request and a `fast` request against the same candidate model and same `est_tokens` produce different outcomes (planner rejected, fast selected). Integration test.
- [machine] A pinned request uses the headroom of `req.role` (the pin overrides the chain, not the policy); asserted by a test where a pin is rejected by the planner's larger headroom.
- [machine] `cargo test -p conway-routing` passes, including `tests/router_resolution.rs`.

## Notes
**Objective (amended):** …as before, with the capability filter now gating on `est_tokens + resolved_headroom`.

**Implementation Notes (amended step 2 of the binding algorithm):**

```
2. capability: headroom = self.effective_headroom[req.role]      // precomputed in new()
               for each candidate:
                   satisfies(index.get(ref), &req.required, req.est_tokens, headroom)
                   miss -> CapabilitySkip{ skipped: ref, missing }, drop candidate
```

`effective_headroom` is a `BTreeMap<RoleAlias, u32>` built in `new()` as `policy.resolve(role)` for every role in the chain table, plus a stored `fallback_headroom = policy.default_headroom_tokens` used for a pinned request whose `role` is absent from the table. No other headroom lookup path exists.

Filter order is unchanged and still binding: **pin → capability (incl. headroom) → health → chain order**. Headroom lives inside the capability stage; it does not become a fourth stage.

Add to `tests/router_resolution.rs`: headroom-only rejection; headroom flips selection across chain positions (position 0 rejected by headroom, position 1 selected as `Fallback{position:1}`); per-role override differentiation; global-default inheritance; all-rejected-by-headroom → `NoCandidate` with `considered.len() == chain.len()`; pin rejected by headroom.

---

# WI-036 (amended): RoutingExplain — ExplainReport for chosen and skipped candidates

## Complexity
Medium

## Scope
- `crates/conway-routing/src/explain.rs` (modify)
- `crates/conway-routing/tests/explain_report.rs` (create)

## Depends
- WI-034
- MODULE:conway-core

## Criteria
*(criteria from the original WI-036 remain; the following are added)*

- [machine] `ExplainReport` carries `pub headroom_tokens: u32` (the effective, role-resolved reservation used for this request) alongside `est_tokens`.
- [machine] `ExplainReport.headroom_tokens` equals `HeadroomPolicy::resolve(role)` for the request's role; asserted for both an overridden role and a role inheriting the global default.
- [machine] A headroom-skipped entry renders with its shortfall string; the golden file `crates/conway-routing/tests/golden/explain_planner.txt` includes a headroom-rejected candidate and matches byte-for-byte.
- [machine] `render_text` header line is exactly `role: planner  (est_tokens=34000, headroom_tokens=16384)`.
- [machine] `cargo test -p conway-routing` passes.

## Notes
**Implementation Notes (added):** the report must make the reservation visible — a user asking "why was my 40k model skipped for a 34k request" gets the answer from the report alone, without reading config. Golden fixture line for a headroom rejection:

```
  [1] ollama-cloud/glm-5.2          SKIPPED  capability: context: needs 34000 input + 16000 headroom = 50000, model max_context_tokens is 40000
```

`headroom_tokens` is read from the router's precomputed table via `evaluate` (extend `Evaluation` with the `headroom_tokens` field) — `explain.rs` must not call `HeadroomPolicy::resolve` itself, preserving the single-source-of-truth structure.

---

## Coverage Statement (amended)

**Module:** conway-routing

**Work items:** WI-031, WI-032, WI-033, WI-034, WI-035, WI-036

**Coverage:** Unchanged in structure — these six items implement 100% of the module's scope. The headroom addendum is absorbed as: declarative policy type, defaults, validation, and precedence (WI-031); the admission inequality `est_tokens + headroom <= max_context_tokens` and its shortfall string (WI-032); once-at-construction resolution, filter integration, and `NoCandidate` enumeration naming the reservation (WI-034); report/render surface (WI-036). WI-033 and WI-035 are untouched.

Exclusions unchanged. GP-07 compliance for headroom is mechanically asserted: headroom is a config scalar resolved at construction, with a WI-031 criterion forbidding any content-derived computation and the WI-034 allocation budget forbidding per-request policy work.

Design tensions: **T-1** — now stricter and still resolved the same way (capability-filter, then reject; no truncation, no headroom relaxation under pressure). **T-2**, **T-4** — unchanged.

**Provides implemented by:** unchanged mapping; `CapabilityIndex`'s `RequiredCaps` filtering (WI-032) now includes the headroom gate, and `DeclarativeRouter::new` (WI-034) takes `HeadroomPolicy` as its second parameter.

**Requires consumed by:** unchanged.

**Unresolved items flagged to conway-core (not worked around):** pin-source field on `RouteRequest`; `RoutingError::UnknownRole` variant; `HealthProber::spawn` signature needs the registry parameter; **`RoleConfig` should gain `headroom_tokens: Option<u32>`** (interim: routing-owned `HeadroomPolicy` sidecar, with a `from_routing_config` path already written to delegate once the field lands).