Addendum folded in. The affected item is **WI-004** (defines `RequiredCaps`, `RouteRequest`, `RoutingConfig`); two consequential amendments to **WI-001** (the T-1 error must name the headroom) and **WI-008** (fake must exercise it). Revised items below replace the originals; all other items are unchanged.

---

# WI-004 (REVISED): Define capability, routing, and health types, including reasoning headroom

- **id**: WI-004
- **title**: Define capability, routing, and health types, including reasoning headroom
- **complexity**: medium
- **scope**:
  - `crates/conway-core/src/capabilities.rs` (create)
  - `crates/conway-core/src/routing.rs` (create)
- **depends**: WI-001
- **criteria**:
  - [ ] `capabilities.rs` exports `Capabilities`, `ToolCallSupport`, `CacheMode`, `StructuredOutput`, `ReliabilityTier`, `RequiredCaps`, `ProbeReport`. [machine]
  - [ ] `routing.rs` exports `RouteRequest`, `Route`, `RoutingReason`, `Observation`, `BreakerState`, `BreakerKind`, `ExplainReport`, `RoutingConfig`, `RoleConfig`, `HealthConfig`, `BackendConfig`. [machine]
  - [ ] **`RequiredCaps` has a field `headroom_tokens: u32` (non-`Option`, defaulting to `DEFAULT_HEADROOM_TOKENS`), and it round-trips through `serde_json` — a unit test asserts `serde_json::from_str::<RequiredCaps>(r#"{"headroom_tokens":8192}"#).unwrap().headroom_tokens == 8192` and that serializing a default `RequiredCaps` emits the `headroom_tokens` key.** [machine]
  - [ ] **`RoutingConfig` has a global `default_headroom_tokens: u32` field and `RoleConfig` has a `headroom_tokens: Option<u32>` per-role override; both round-trip through `serde_json`. A unit test asserts `RoutingConfig::headroom_for(&RoleAlias) -> u32` returns the per-role value when set and the global default otherwise.** [machine]
  - [ ] **`RequiredCaps::satisfied_by(&Capabilities, est_tokens: u32) -> Result<(), Vec<String>>` enforces `est_tokens.saturating_add(self.headroom_tokens) <= caps.max_context_tokens`. Unit tests cover: (a) `est=30_000, headroom=8_192, max=32_768` → `Err` whose message names est, headroom, max, and the shortfall `5_424`; (b) `est=20_000, headroom=8_192, max=32_768` → `Ok`; (c) `est=u32::MAX, headroom=8_192` does not panic (saturating arithmetic).** [machine]
  - [ ] `RouteRequest` has no field of type `String`, `Vec<ContentBlock>`, `PromptSegment`, or `Message` that could carry prompt text; a unit test asserts the field set is exactly `{role, pin, required, est_tokens, agent_id}`. [machine]
  - [ ] `RoutingConfig` deserializes from the TOML-equivalent JSON of the architecture §"conway-routing / Internal Design Notes" snippet (roles.planner chain, roles.fast chain, health block) **extended with a `default_headroom_tokens` key and a per-role `headroom_tokens` key**; a unit test round-trips it. [machine]
  - [ ] All enums are `#[non_exhaustive]`; all types are `Serialize + Deserialize + Clone + Debug`. [machine]
  - [ ] `cargo test -p conway-core` passes. [machine]
- **notes**:

  **Objective:** Define the capability description model that makes backends comparable per `(backend, model)`, the content-free routing request/response contract that makes GP-07 a compile-time guarantee, the health/breaker state vocabulary, and the **reasoning-headroom** budget that reserves output/reasoning tokens so context-window gating measures the whole turn, not just the prompt.

  **Implementation Notes:**

  `capabilities.rs` — transcribe §4.1 exactly:
  ```rust
  pub struct Capabilities {
      pub tool_calling: ToolCallSupport,
      pub cache: CacheMode,
      pub parallel_tool_calls: bool,
      pub structured_output: StructuredOutput,
      pub max_context_tokens: u32,
      pub reasoning: bool,
      pub reliability_tier: ReliabilityTier,
  }
  #[non_exhaustive] pub enum ToolCallSupport { None, NonStreamingOnly, Streaming { validated: bool } }
  #[non_exhaustive] pub enum CacheMode {
      ExplicitBreakpoints { max_breakpoints: u8, ttls: Vec<CacheTtl> },
      ImplicitPrefix { min_prefix_tokens: u32 },
      SlotKv,
      None,
  }
  #[non_exhaustive] pub enum StructuredOutput { None, JsonSchema, Grammar }
  #[non_exhaustive] pub enum ReliabilityTier { Verified, Community, Unknown }
  ```
  Deviation from §4.1 noted and intended: `ttls` is `Vec<CacheTtl>` rather than `&'static [CacheTtl]` because every public type must be `Deserialize` and a `'static` slice is not.

  `ToolCallSupport` gets `pub fn rank(&self) -> u8` (`None`=0, `NonStreamingOnly`=1, `Streaming{validated:false}`=2, `Streaming{validated:true}`=3) and `PartialOrd` derived from it.

  **Headroom.** `max_context_tokens` in every backend's `Capabilities` is the *total* window: prompt plus generated output plus reasoning tokens. Gating only on assembled prompt size therefore admits requests that overflow mid-generation, which surfaces as a `BackendError::ContextOverflow` after the tokens are already paid for. `headroom_tokens` is the reserved remainder:

  ```rust
  pub const DEFAULT_HEADROOM_TOKENS: u32 = 8_192;

  pub struct RequiredCaps {
      pub tool_calling: Option<ToolCallSupport>,
      pub min_context: Option<u32>,
      pub structured_output: Option<StructuredOutput>,
      pub reasoning: Option<bool>,
      pub parallel_tool_calls: Option<bool>,
      pub min_reliability: Option<ReliabilityTier>,
      /// Tokens reserved for model output and reasoning. A candidate is compatible only if
      /// `est_tokens + headroom_tokens <= caps.max_context_tokens`. Never `Option`: a request
      /// with no reserved output space is always a configuration error, so the field carries
      /// `DEFAULT_HEADROOM_TOKENS` rather than "unspecified".
      #[serde(default = "default_headroom_tokens")]
      pub headroom_tokens: u32,
  }
  fn default_headroom_tokens() -> u32 { DEFAULT_HEADROOM_TOKENS }
  ```
  `RequiredCaps::default()` sets every `Option` to `None` and `headroom_tokens` to `DEFAULT_HEADROOM_TOKENS`. Do **not** derive `Default` — write it by hand so the headroom default is explicit and does not silently become `0` if the derive is regenerated. The `#[serde(default = ...)]` attribute is required so configs written before this field existed deserialize to the default rather than failing.

  `satisfied_by` signature changes from the pre-addendum form: it now takes the estimated prompt size, because the context check is no longer expressible from `Capabilities` alone.
  ```rust
  impl RequiredCaps {
      pub fn satisfied_by(&self, caps: &Capabilities, est_tokens: u32) -> Result<(), Vec<String>>;
      /// Total window the request will occupy. Saturating.
      pub fn total_required(&self, est_tokens: u32) -> u32 { est_tokens.saturating_add(self.headroom_tokens) }
      pub fn shortfall(&self, caps: &Capabilities, est_tokens: u32) -> u32 {
          self.total_required(est_tokens).saturating_sub(caps.max_context_tokens)
      }
  }
  ```
  `satisfied_by` accumulates one `String` per unmet requirement, in this fixed check order: tool_calling, context, structured_output, reasoning, parallel_tool_calls, min_reliability. Each string is used verbatim in `RoutingReason::CapabilitySkip { missing }` and in `RoutingError::NoCandidate`, so the formats are fixed:
  - context (headroom-aware, this is the load-bearing one): `"context: needs {est_tokens} prompt + {headroom_tokens} headroom = {total} tokens, model provides {max_context_tokens} (short by {shortfall})"`
  - explicit floor: `"min_context: requires {required} tokens, model provides {available}"` — emitted only when `min_context` is set and exceeds `max_context_tokens`; this is an independent, coarser check that a role may set to exclude small models regardless of the current request size.
  - tool_calling: `"tool_calling: requires {required:?}, model provides {available:?}"`
  - remaining checks follow the same `"{field}: requires {required}, model provides {available}"` shape.

  All arithmetic is `saturating_add`/`saturating_sub`. No `u32` overflow path may panic, including a pathological `est_tokens: u32::MAX`.

  `ProbeReport { pub ok: bool, pub latency_ms: u32, pub models: Vec<ModelId>, pub detail: Option<String>, pub at: DateTime<Utc> }`.

  `routing.rs`:
  ```rust
  pub struct RouteRequest { pub role: RoleAlias, pub pin: Option<ModelRef>,
                            pub required: RequiredCaps, pub est_tokens: u32, pub agent_id: AgentId }
  ```
  Headroom rides on `RouteRequest` via `required.headroom_tokens` — it is *not* a separate top-level field. Rationale: it is a capability-filter input, it must travel with the other filter inputs into `RequiredCaps::satisfied_by`, and adding a sixth top-level field weakens the "field set is exactly these five" test that mechanically guarantees `RouteRequest` cannot carry prompt content. Add `impl RouteRequest { pub fn total_required(&self) -> u32 { self.required.total_required(self.est_tokens) } }` so callers never recompute the sum by hand.

  ```rust
  pub struct Route { pub backend: BackendId, pub model: ModelId,
                     pub params: SamplingParams, pub reason: RoutingReason }
  #[non_exhaustive] pub enum RoutingReason {
      PinnedByApi, PinnedByAgentDef, AliasPrimary { alias: RoleAlias },
      Fallback { position: u8, after: Vec<AttemptFailure> },
      CapabilitySkip { skipped: ModelRef, missing: Vec<String> },
      HealthSkip { skipped: ModelRef, breaker: BreakerKind },
  }
  pub struct AttemptFailure { pub model: ModelRef, pub error: String, pub at: DateTime<Utc> }
  #[non_exhaustive] pub enum BreakerKind { Transport, Probe }
  #[non_exhaustive] pub enum BreakerState { Closed, Open { until: DateTime<Utc>, kind: BreakerKind }, HalfOpen }
  #[non_exhaustive] pub enum Observation { Ok { latency_ms: u32 }, TransportError, ServerError,
                                           ProbeFail, RateLimited { retry_after_secs: Option<u64> } }
  pub struct ExplainReport { pub role: RoleAlias, pub chain: Vec<ModelRef>,
                             pub chosen: Option<Route>,
                             pub considered: Vec<(ModelRef, RoutingReason)>,
                             pub breaker_states: Vec<(EndpointId, BreakerState)>,
                             pub headroom_tokens: u32 }
  ```
  `ExplainReport` carries the effective `headroom_tokens` so `conway routes explain <role>` can show why a model was excluded on size — a headroom-caused exclusion is otherwise invisible and looks like an arbitrary skip.

  Doc-comment on `Observation`: `BadRequest`, `Auth`, and `ContextOverflow` deliberately have no `Observation` representation (§8) — they are request problems, not endpoint-health signals. Headroom exists specifically to convert most would-be `ContextOverflow` failures into pre-flight `CapabilitySkip`/`ContextTooLarge` decisions.

  Config types (types only; loading lives in the `conway` facade):
  ```rust
  pub struct RoutingConfig {
      pub roles: BTreeMap<String, RoleConfig>,
      pub health: HealthConfig,
      /// Global default reserved output/reasoning tokens, applied to any role without an override.
      #[serde(default = "default_headroom_tokens")]
      pub default_headroom_tokens: u32,
  }
  pub struct RoleConfig {
      pub chain: Vec<ModelRef>,
      pub required: RequiredCaps,
      pub params: SamplingParams,
      /// Per-role override of `RoutingConfig::default_headroom_tokens`.
      #[serde(default)]
      pub headroom_tokens: Option<u32>,
  }
  ```
  ```rust
  impl RoutingConfig {
      /// Per-role override if present, else the global default. Unknown roles get the global default.
      pub fn headroom_for(&self, role: &RoleAlias) -> u32 {
          self.roles.get(role.as_str())
              .and_then(|r| r.headroom_tokens)
              .unwrap_or(self.default_headroom_tokens)
      }
      /// Builds the filter input for a request: the role's `required` caps with
      /// `headroom_tokens` resolved from the override/default chain.
      pub fn required_caps_for(&self, role: &RoleAlias) -> RequiredCaps;
  }
  ```
  `required_caps_for` clones the role's `RequiredCaps` (or `RequiredCaps::default()` for an unknown role) and overwrites `headroom_tokens` with `headroom_for(role)`. Precedence is fixed and total: **per-role `headroom_tokens` > `default_headroom_tokens` > `DEFAULT_HEADROOM_TOKENS`**. A caller-supplied `RouteRequest.required.headroom_tokens` set by the runtime (e.g. from an agent def) is above all of these — `conway-routing` uses the value on the request as given and only consults config when constructing the request. Document this precedence chain on `headroom_for`.

  Reference config, which the round-trip test uses:
  ```toml
  default_headroom_tokens = 8192

  [roles.planner]
  chain = [ "anthropic/claude-sonnet-4-6", "ollama-cloud/glm-5.2", "local/qwen3-coder-80b" ]
  headroom_tokens = 32768          # reasoning model: reserve much more

  [roles.fast]
  chain = [ "local/qwen3-coder-80b", "anthropic/claude-haiku-4-5" ]
  # no override -> 8192

  [health]
  transport_failures_to_open = 3
  open_duration = "30s"
  probe_interval = "15s"
  probe_timeout = "2s"
  ```

  ```rust
  pub struct HealthConfig { pub transport_failures_to_open: u32, pub open_duration_secs: u64,
                            pub probe_interval_secs: u64, pub probe_timeout_secs: u64,
                            pub probe_failures_to_open: u32 }
  pub struct BackendConfig { pub id: BackendId, pub kind: BackendKind, pub base_url: Option<String>,
                             pub api_key_env: Option<String>, pub dialect: Option<String>,
                             pub models: BTreeMap<String, ModelOverrides>,
                             pub extra: serde_json::Map<String, serde_json::Value> }
  #[non_exhaustive] pub enum BackendKind { Anthropic, OpenAiCompat }
  pub struct ModelOverrides { pub stream_tools: Option<bool>, pub max_context_tokens: Option<u32>,
                              pub reliability_tier: Option<ReliabilityTier>,
                              /// Per-model headroom floor, applied after the role chain resolves.
                              /// Consumed by conway-routing's CapabilityIndex; max() with the role value.
                              pub min_headroom_tokens: Option<u32> }
  ```
  `min_headroom_tokens` is a floor, not an override: a model that reasons heavily can insist on more reserved space than a role requests, but cannot reduce it. `conway-routing` applies `effective = max(request.headroom_tokens, model.min_headroom_tokens.unwrap_or(0))`. Document that rule here; the implementation lives in `conway-routing`.

  `HealthConfig` and `RoleConfig` implement `Default` matching the §"conway-routing" TOML defaults (`transport_failures_to_open = 3`, `open_duration_secs = 30`, `probe_interval_secs = 15`, `probe_timeout_secs = 2`, `probe_failures_to_open = 3`). Durations are plain integer-second fields; string duration parsing belongs to the facade. Use `BTreeMap`, not `HashMap`, so serialized config is deterministically ordered.

---

# WI-001 (AMENDED): Create the cargo workspace and the conway-core crate skeleton with ID newtypes and the error taxonomy

Only the T-1 error shapes change. Full item as previously specified, with these two amendments:

- **Added criterion**: **`RoutingError::ContextTooLarge` and `RuntimeError::ForkContextOverflow` each carry `est_tokens: u32` and `headroom_tokens: u32` alongside `max_context_tokens` and `shortfall_tokens`; a unit test asserts the `Display` output names all four numbers.** [machine]
- **Amended Implementation Notes** (replacing the two T-1 variant definitions):

  ```rust
  // RoutingError
  ContextTooLarge {
      role: RoleAlias, model: ModelRef,
      est_tokens: u32,            // assembled prompt estimate
      headroom_tokens: u32,       // reserved output/reasoning budget
      required_tokens: u32,       // est_tokens + headroom_tokens, saturating
      max_context_tokens: u32,
      shortfall_tokens: u32,      // required_tokens - max_context_tokens, saturating
  }

  // RuntimeError
  ForkContextOverflow {
      parent: AgentId, model: ModelRef,
      est_tokens: u32, headroom_tokens: u32,
      required_tokens: u32, max_context_tokens: u32, shortfall_tokens: u32,
  }
  ```
  Fixed `Display` format for both, so the CLI and IDE render an identical, diagnosable message:
  `"context rejected: {est_tokens} prompt + {headroom_tokens} reserved output = {required_tokens} tokens, but {model} accepts at most {max_context_tokens} (short by {shortfall_tokens}); no truncation or escalation is performed"`

  The explicit mention of reserved output tokens is required: without it a user sees a rejection at 24k tokens on a 32k model and reasonably concludes the harness is miscounting. Both variants keep the T-1 property that no field can express a truncation or escalation outcome — the error is terminal by construction.

---

# WI-008 (AMENDED): Implement feature-gated test fakes

Full item as previously specified, with these two amendments:

- **Added criterion**: **`FakeRouter::context_too_large(...)` constructs the headroom-aware `RoutingError::ContextTooLarge`, and a test asserts the runtime-facing rejection path is reachable with a fake: given `est_tokens = 30_000`, `headroom_tokens = 8_192`, `max_context_tokens = 32_768`, the error's `shortfall_tokens == 5_424` and its `Display` contains the substring `"reserved output"`.** [machine]
- **Added criterion**: **`FakeBackend::with_capabilities` accepts a `max_context_tokens` small enough to exercise headroom gating, and a test asserts `RequiredCaps::default().satisfied_by(&caps, 30_000)` returns `Err` for `max_context_tokens = 32_768` (30_000 + 8_192 default headroom > 32_768) — i.e. the default headroom is actually enforced, not merely stored.** [machine]

---

## Coverage Statement (REVISED)

**Module:** conway-core
**Work items:** WI-001, WI-002, WI-003, WI-004, WI-005, WI-006, WI-007, WI-008

**Coverage:** Unchanged from the prior statement — these eight items implement 100% of the conway-core module scope plus the workspace root scaffolding. The addendum adds no new work item and no new file: reasoning headroom is a field-level and semantics-level extension of types already owned by WI-004 (`RequiredCaps`, `RoutingConfig`, `RoleConfig`, `ModelOverrides`, `ExplainReport`) and WI-001 (the two T-1 error variants), with test coverage extended in WI-008. The dependency graph is unchanged (WI-004 still depends only on WI-001; WI-008 still depends only on WI-007).

**Provides mapping delta:**

| Provides entry | Work item(s) | Addendum effect |
|---|---|---|
| `RequiredCaps` | WI-004 | gains `headroom_tokens: u32` (non-`Option`, serde-defaulted); `satisfied_by` signature becomes `(&Capabilities, est_tokens: u32)` |
| `RoutingConfig` | WI-004 | gains `default_headroom_tokens: u32` and `headroom_for`/`required_caps_for` resolvers |
| `BackendConfig` | WI-004 | `ModelOverrides` gains `min_headroom_tokens: Option<u32>` (a floor, not an override) |
| `RoutingError::ContextTooLarge`, `RuntimeError::ForkContextOverflow` | WI-001 | gain `est_tokens` and `headroom_tokens`; `Display` names all four numbers |
| `FakeRouter`, `FakeBackend` | WI-008 | gain headroom-gating test coverage |

**Downstream contract note for the plan skill:** the headroom rule `est_tokens + headroom_tokens <= max_context_tokens` is *declared* in conway-core (WI-004) and *enforced* in `conway-routing`'s `DeclarativeRouter`/`CapabilityIndex`, which must call `RequiredCaps::satisfied_by(caps, req.est_tokens)` rather than comparing `est_tokens` to `max_context_tokens` directly, and must apply the `min_headroom_tokens` floor via `max()`. `conway-runtime` must populate `RouteRequest.required` using `RoutingConfig::required_caps_for(role)` so the config precedence chain is honored. Flag these as acceptance criteria on the corresponding conway-routing and conway-runtime work items — they are outside my module's range.