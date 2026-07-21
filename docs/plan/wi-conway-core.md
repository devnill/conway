## Size Assessment

**Right size.** `conway-core` is broad but shallow: it is a declarative crate (types + trait signatures + fakes) with no I/O and no algorithms. Decomposition axis is **type-dependency layers** (ids/errors → content/log → provenance/segments → capabilities/routing → agent/result/config → events → ports → fakes). Eight work items, dependency depth 5, three parallel branches after WI-001. Workspace root scaffolding is folded into WI-001 because conway-core is Group 0 and nothing else can create it.

Ambiguity noted: the module's Requires list does not include a ULID crate, but Internal Design Notes mandate ULID ids. Assumption: `ulid` (with `serde` feature) is added as an external dependency — it is pure computation, no I/O, and does not violate the boundary rules. Stated explicitly in WI-001.

---

# WI-001: Create the cargo workspace and the conway-core crate skeleton with ID newtypes and the error taxonomy

- **id**: WI-001
- **title**: Create the cargo workspace and the conway-core crate skeleton with ID newtypes and the error taxonomy
- **complexity**: medium
- **scope**:
  - `Cargo.toml` (create)
  - `.gitignore` (create)
  - `rust-toolchain.toml` (create)
  - `crates/conway-core/Cargo.toml` (create)
  - `crates/conway-core/src/lib.rs` (create)
  - `crates/conway-core/src/ids.rs` (create)
  - `crates/conway-core/src/error.rs` (create)
- **depends**: none
- **criteria**:
  - [ ] Root `Cargo.toml` declares `[workspace]` with `resolver = "2"`, `members = ["crates/*"]`, and a `[workspace.package]` block setting `version = "0.1.0"`, `edition = "2021"`, `license = "Apache-2.0"`, `rust-version` matching `rust-toolchain.toml`. [machine]
  - [ ] Root `Cargo.toml` has a `[workspace.dependencies]` table pinning `serde`, `serde_json`, `thiserror`, `async-trait`, `futures-core`, `chrono`, `blake3`, `schemars`, `ulid`. [machine]
  - [ ] `crates/conway-core/Cargo.toml` lists **only** dependencies drawn from `workspace.dependencies` and contains no `reqwest`, `tokio`, `hyper`, `std::process`-wrapping, or workspace-path dependency. [machine]
  - [ ] `crates/conway-core/Cargo.toml` declares `[features] default = []` and `fakes = []`. [machine]
  - [ ] `cargo build -p conway-core` succeeds; `cargo build -p conway-core --features fakes` succeeds. [machine]
  - [ ] `cargo tree -p conway-core -e normal` output contains no line matching `reqwest|tokio|hyper|rusqlite|std-process`. [machine]
  - [ ] `ids.rs` exports `SessionId`, `AgentId`, `SegmentId`, `ModelId`, `BackendId`, `EndpointId`, `RoleAlias`, `ToolName`, `PrefixKey`, `ModelRef`, `LogSeq`, `SeqRange`; each implements `Clone + Debug + PartialEq + Eq + Hash + Serialize + Deserialize + Display + FromStr`. [machine]
  - [ ] `error.rs` exports `ConwayError`, `BackendError`, `ToolError`, `StoreError`, `RoutingError`, `RuntimeError`, `PluginError`, each `#[non_exhaustive]`, each deriving `thiserror::Error + Debug + Serialize + Deserialize`. [machine]
  - [ ] A unit test asserts `RoutingError::ContextTooLarge` and `RuntimeError::ForkContextOverflow` exist and round-trip through `serde_json`. [machine]
  - [ ] `cargo test -p conway-core` passes. [machine]
- **notes**:

  **Objective:** Establish the cargo workspace root for the 8-crate conway project and the `conway-core` crate skeleton, containing the identifier newtypes every other type depends on and the complete error taxonomy including the typed error that resolves design tension T-1 (an assembled context exceeding the resolved model's max context is rejected, never truncated or escalated).

  **Implementation Notes:**

  `rust-toolchain.toml`: `[toolchain] channel = "stable"`, `components = ["rustfmt", "clippy"]`.

  `crates/conway-core/src/lib.rs` declares the full module list up front so subsequent work items only add files (`pub mod ids; pub mod error; pub mod content; pub mod log; pub mod provenance; pub mod segment; pub mod capabilities; pub mod routing; pub mod agent; pub mod config; pub mod event; pub mod ports;` and `#[cfg(feature = "fakes")] pub mod fakes;`). Modules not yet created by this item must be commented out with a `// WI-NNN` marker so the crate compiles; each later item uncomments its own line. Also add `#![deny(missing_debug_implementations)]` and a crate-level `pub use` prelude module `pub mod prelude { pub use crate::{ids::*, error::*}; }` extended by later items.

  `ids.rs`. ULID-backed ids: `SessionId`, `AgentId`, `SegmentId` are `#[derive(...)] pub struct X(pub ulid::Ulid)` with an inherent `pub fn new() -> Self` generating a fresh ULID and `impl Default`. String-backed ids: `ModelId(String)`, `BackendId(String)`, `EndpointId(String)`, `RoleAlias(String)`, `ToolName(String)`, `PrefixKey(String)` — `PrefixKey` additionally exposes `pub fn from_blake3(hash: blake3::Hash) -> Self` producing the lowercase hex string. `ModelRef { pub backend: BackendId, pub model: ModelId }` with `Display` rendering `"{backend}/{model}"` and `FromStr` parsing on the first `/` (error: `ConwayError::Parse`). `LogSeq(pub u64)` with `Ord`, `succ()`, and `ZERO` const. `SeqRange { pub start: LogSeq, pub end: Option<LogSeq> }` where `end` is exclusive and `None` means open-ended; provide `contains(&LogSeq) -> bool`.

  Serde representation for all newtypes is the transparent inner value (`#[serde(transparent)]`), so a `SessionId` serializes as a bare JSON string and `LogSeq` as a bare number. This is load-bearing for the JSONL log format in §5.1.

  `error.rs` variants (exact):
  - `BackendError`: `Transport { detail: String }`, `RateLimit { retry_after_secs: Option<u64> }`, `Auth { detail: String }`, `BadRequest { detail: String }`, `ServerError { status: u16, detail: String }`, `ContextOverflow { required_tokens: u32, max_context_tokens: u32 }`, `ToolParse { detail: String }`, `Cancelled`. Add `pub fn is_failover_worthy(&self) -> bool` returning `true` for `Transport | RateLimit | ServerError | ContextOverflow`, `false` otherwise, and `pub fn is_health_signal(&self) -> bool` returning `true` for `Transport | ServerError | RateLimit`, `false` for `Auth | BadRequest | ContextOverflow | ToolParse | Cancelled` (per §8: `BadRequest`/`Auth`/`ContextOverflow` are not endpoint-health signals).
  - `ToolError`: `InvalidArguments { detail: String }`, `Denied { reason: String }`, `Cancelled`, `Timeout { after_secs: u64 }`, `Io { detail: String }`, `Internal { detail: String }`.
  - `StoreError`: `NotFound { session: SessionId }`, `Corrupt { session: SessionId, line: u64, detail: String }`, `Io { detail: String }`, `SeqOutOfRange { requested: LogSeq, head: LogSeq }`, `AlreadyExists { session: SessionId }`.
  - `RoutingError`: `NoCandidate { role: RoleAlias, considered: Vec<(ModelRef, String)> }` (the `String` is the rendered `RoutingReason`; a typed `RoutingReason` cannot be used here because it is defined in WI-004 which depends on this file — keep the rendered form to avoid a cycle), `UnknownRole { role: RoleAlias }`, `UnknownModelRef { reference: String }`, and **T-1**: `ContextTooLarge { role: RoleAlias, model: ModelRef, required_tokens: u32, max_context_tokens: u32, shortfall_tokens: u32 }`.
  - `RuntimeError`: `AgentNotFound { agent: AgentId }`, `BudgetExceeded { agent: AgentId }`, `Cancelled { agent: AgentId, reason: String }`, `Backend(BackendError)`, `Routing(RoutingError)`, `Store(StoreError)`, `Tool(ToolError)`, and **T-1 at the fork boundary**: `ForkContextOverflow { parent: AgentId, model: ModelRef, required_tokens: u32, max_context_tokens: u32, shortfall_tokens: u32 }`. The `Display` message for both T-1 variants MUST name the shortfall, e.g. `"assembled context requires {required_tokens} tokens but {model} accepts at most {max_context_tokens} (short by {shortfall_tokens}); request rejected"`. No truncation or escalation path exists in the type.
  - `PluginError`: `Init { plugin: String, detail: String }`, `MissingHostCapability { plugin: String, capability: String }`, `DuplicateTool { tool: ToolName }`.
  - `ConwayError`: `#[non_exhaustive]` umbrella with `Backend`, `Tool`, `Store`, `Routing`, `Runtime`, `Plugin`, `Config { detail: String }`, `Parse { detail: String }` variants and `#[from]` conversions for each nested error type.

  All error enums must be serde round-trippable; use `#[serde(tag = "kind")]`-style externally-tagged default (no custom representation) and store only owned data (`String`, not `&str`, no `Box<dyn Error>`).

---

# WI-002: Define message content, tool call, and log record types

- **id**: WI-002
- **title**: Define message content, tool call, and log record types
- **complexity**: medium
- **scope**:
  - `crates/conway-core/src/content.rs` (create)
  - `crates/conway-core/src/log.rs` (create)
  - `crates/conway-core/src/lib.rs` (modify)
- **depends**: WI-001
- **criteria**:
  - [ ] `content.rs` exports `Role`, `ContentBlock`, `Message`, `ToolCall`, `ToolResult`, `ToolSpec`, `ToolCategory`, `PermissionClass`, `TruncationPolicy`, `Artifact`, `Usage`, `StopReason`, `SamplingParams`. [machine]
  - [ ] `log.rs` exports `LogRecord` and `SessionMeta`, `ForkOrigin`, `SessionFilter`, `SessionStatus`. [machine]
  - [ ] Every exported type derives `Clone + Debug + Serialize + Deserialize`; every enum listed in Implementation Notes as forward-compatible carries `#[non_exhaustive]`. [machine]
  - [ ] `LogRecord` serializes with an internal tag field named `kind` whose values are exactly `header|user_turn|assistant|tool_call|tool_result|fork_directive|parent_steer|system_note|agent_result|context_report`. A unit test asserts the tag string for each variant. [machine]
  - [ ] A unit test deserializes the five example JSONL lines from architecture §5.1 into `LogRecord`/`SessionMeta` values without error. [machine]
  - [ ] `ToolSpec` contains a `schema: schemars::schema::RootSchema` field and round-trips through `serde_json`. [machine]
  - [ ] `cargo test -p conway-core` passes and `cargo clippy -p conway-core -- -D warnings` is clean. [machine]
- **notes**:

  **Objective:** Define the conversational substrate — content blocks, messages, tool calls/results/specs — and the append-only `LogRecord` union that `conway-session` persists as JSONL and every other crate reads.

  **Implementation Notes:**

  `content.rs`:
  ```rust
  #[non_exhaustive] pub enum Role { System, User, Assistant, ToolResult }

  #[non_exhaustive]
  #[serde(tag = "type", rename_all = "snake_case")]
  pub enum ContentBlock {
      Text { text: String },
      Thinking { text: String, signature: Option<String> },
      ToolUse { call_id: String, name: ToolName, arguments: serde_json::Value },
      ToolResultBlock { call_id: String, blocks: Vec<ContentBlock>, is_error: bool },
      Image { media_type: String, data_base64: String },
  }

  pub struct Message { pub role: Role, pub content: Vec<ContentBlock> }

  pub struct ToolCall { pub call_id: String, pub name: ToolName, pub arguments: serde_json::Value }
  pub struct ToolResult { pub call_id: String, pub tool: ToolName,
                          pub blocks: Vec<ContentBlock>, pub is_error: bool,
                          pub truncated: Option<TruncationRecord> }
  pub struct TruncationRecord { pub policy: TruncationPolicy, pub original_bytes: u64, pub kept_bytes: u64 }

  #[non_exhaustive] pub enum TruncationPolicy {
      None, Head { max_bytes: u64 }, Tail { max_bytes: u64 },
      HeadTail { head_bytes: u64, tail_bytes: u64 }, Artifact,   // spill to Artifact, keep a pointer
  }

  pub struct ToolSpec { pub name: ToolName, pub description: String,
                        pub schema: schemars::schema::RootSchema,
                        pub category: ToolCategory, pub permission: PermissionClass }

  #[non_exhaustive] pub enum ToolCategory { Read, Edit, Delete, Move, Search, Execute, Think, Fetch, Delegate }
  #[non_exhaustive] pub enum PermissionClass { Safe, RequiresApproval, Dangerous }

  pub struct Artifact { pub id: String, pub kind: ArtifactKind, pub path: Option<PathBuf>,
                        pub media_type: Option<String>, pub bytes: Option<u64>, pub label: String }
  #[non_exhaustive] pub enum ArtifactKind { File, Diff, Value, Log }

  pub struct Usage { pub input_tokens: u32, pub output_tokens: u32,
                     pub cache_read_tokens: u32, pub cache_write_tokens: u32,
                     pub reasoning_tokens: u32 }   // impl Add/AddAssign for aggregation

  #[non_exhaustive] pub enum StopReason { EndTurn, ToolUse, MaxTokens, StopSequence, Refusal }

  pub struct SamplingParams { pub temperature: Option<f32>, pub top_p: Option<f32>,
                              pub max_tokens: Option<u32>, pub stop: Vec<String>,
                              pub seed: Option<u64>, pub extra: serde_json::Map<String, serde_json::Value> }
  ```
  `SamplingParams` implements `Default` (all `None`, empty collections). `PathBuf` use here is `std::path::PathBuf` only — no filesystem calls.

  `log.rs`. `LogRecord` is `#[non_exhaustive]`, `#[serde(tag = "kind", rename_all = "snake_case")]`:
  - `Header(SessionMeta)` — note the tag rename must yield `"header"`; use `#[serde(rename = "header")]` with a flattened `SessionMeta`.
  - `UserTurn { seq: LogSeq, ts: DateTime<Utc>, text: String, prov: Provenance }` — the `prov` field type is added in WI-003; for this item declare the field as `provenance::Provenance` and gate compilation by ordering (WI-003 depends on WI-002, so instead **omit the typed field here and add it in WI-003**). Concretely: WI-002 defines each variant *without* the `prov` field; WI-003 modifies `log.rs` to add the `prov: Provenance` field to every applicable variant. `log.rs` therefore appears in both items, dependency-ordered.
  - `Assistant { seq, ts, content: Vec<ContentBlock>, model: ModelRef, route_reason: serde_json::Value, usage: Usage, stop: StopReason }` — `route_reason` is typed as `serde_json::Value` here and retyped to `RoutingReason` in WI-004 (which modifies `log.rs`). To avoid three items touching one file, **keep `route_reason: serde_json::Value` permanently**: the log format in §5.1 stores the reason as data, `conway-session` does not interpret record semantics, and typed access is available via `Event::ModelDecision`. Do not retype it later.
  - `ToolCallRecord { seq, ts, call: ToolCall }` (tag `tool_call`)
  - `ToolResultRecord { seq, ts, result: ToolResult }` (tag `tool_result`)
  - `ForkDirective { seq, ts, text: String, by: AgentId }`
  - `ParentSteer { seq, ts, text: String, from: AgentId, parent_seq: LogSeq }`
  - `SystemNote { seq, ts, text: String, reason: String }`
  - `AgentResultRecord { seq, ts, result: serde_json::Value }` — retyped to `AgentResult` in WI-005, which modifies `log.rs`. Prefer: **defer this variant entirely to WI-005** so `log.rs` is created here and extended once by WI-003 and once by WI-005, both dependency-ordered.
  - `ContextReportRecord { seq, ts, report: serde_json::Value }` — same deferral to WI-003 (which owns `ContextReport`).

  Provide `impl LogRecord { pub fn seq(&self) -> Option<LogSeq>; pub fn kind_str(&self) -> &'static str; }`. Headers return `None` for `seq`.

  `SessionMeta { pub id: SessionId, pub agent_id: AgentId, pub origin: Option<ForkOrigin>, pub agent_def: Option<String>, pub role: Option<RoleAlias>, pub created: DateTime<Utc>, pub cwd: PathBuf, pub labels: Vec<String>, pub status: SessionStatus }`.
  `ForkOrigin { pub parent: SessionId, pub at_seq: LogSeq, pub mode: SubagentMode }` — `SubagentMode` is defined in WI-005; to keep this file's dependencies clean, define `SubagentMode { Fork, Spawn }` **here** in `log.rs` and re-export it from `agent.rs` in WI-005 rather than redefining it.
  `SessionStatus { Active, Completed, Failed, Cancelled }` (`#[non_exhaustive]`).
  `SessionFilter { pub agent_def: Option<String>, pub label: Option<String>, pub status: Option<SessionStatus>, pub parent: Option<SessionId>, pub limit: Option<usize> }` with `Default`.

  `chrono::DateTime<Utc>` serializes as RFC3339 (chrono's default serde impl); do not add a custom serializer.

---

# WI-003: Define Provenance, PromptSegment, cache hints, and ContextReport

- **id**: WI-003
- **title**: Define Provenance, PromptSegment, cache hints, and ContextReport
- **complexity**: medium
- **scope**:
  - `crates/conway-core/src/provenance.rs` (create)
  - `crates/conway-core/src/segment.rs` (create)
  - `crates/conway-core/src/log.rs` (modify)
- **depends**: WI-002
- **criteria**:
  - [ ] `provenance.rs` exports `Provenance` with exactly the nine variants named in the module spec, and `ContextReport`. [machine]
  - [ ] `segment.rs` exports `PromptSegment`, `CacheHint`, `CacheTtl`, `SegmentKind`. [machine]
  - [ ] `PromptSegment` has no `Default` impl and its `provenance` field is non-`Option`, so a segment cannot be constructed without provenance. A compile-fail test (`trybuild` or a `#[test]` asserting `PromptSegment: !Default` via a negative-impl helper trait) documents this. [machine]
  - [ ] `Provenance` serializes internally-tagged with field `type` and snake_case tag values `user_prompt|agent_def|skill|tool_registry|inherited|fork_directive|parent_steer|tool_result|system_note`; a unit test asserts each tag string. [machine]
  - [ ] A unit test deserializes `{"type":"inherited","from":"01J...","seq_range":{"start":0,"end":142}}` and `{"type":"tool_result","call_id":"tc_1","tool":"read"}` into the correct `Provenance` variants. [machine]
  - [ ] `log.rs` gains a `prov: Provenance` field on `UserTurn`, `ForkDirective`, `ParentSteer`, `SystemNote`, and a `ContextReportRecord { seq, ts, report: ContextReport }` variant with tag `context_report`; existing §5.1 example lines still deserialize. [machine]
  - [ ] `cargo test -p conway-core` passes. [machine]
- **notes**:

  **Objective:** Implement GP-10's typed provenance enum, the `PromptSegment` type that carries it, and the cache-hint types that make caching an economics-only concern. This is the crate's most semantically load-bearing item: `ContextBuilder` in `conway-runtime` and the IDE's provenance tree both render directly off these types.

  **Implementation Notes:**

  ```rust
  #[non_exhaustive]
  #[serde(tag = "type", rename_all = "snake_case")]
  pub enum Provenance {
      UserPrompt,
      AgentDef   { name: String },
      Skill      { name: String },
      ToolRegistry { hash: String },              // blake3 hex of the sorted ToolSpec set
      Inherited  { from: SessionId, seq_range: SeqRange },
      ForkDirective { by: AgentId },
      ParentSteer   { from: AgentId, parent_seq: LogSeq },
      ToolResult    { call_id: String, tool: ToolName },
      SystemNote    { reason: String },
  }
  ```
  Add `impl Provenance { pub fn is_static(&self) -> bool }` returning `true` for `AgentDef | Skill | ToolRegistry` — this encodes the §5.3 static/inherited/volatile tiering that the fixed segment order depends on, and `pub fn tier(&self) -> SegmentTier` returning `Static | Inherited | Volatile` (`Inherited` for `Inherited`, `Static` for the three above, `Volatile` for the rest). `SegmentTier` derives `Ord` such that `Static < Inherited < Volatile`; `ContextBuilder` sorts by it.

  ```rust
  pub struct PromptSegment {
      pub id: SegmentId,
      pub role: Role,
      pub content: Vec<ContentBlock>,
      pub provenance: Provenance,          // required, no Default
      pub cache_hint: Option<CacheHint>,
      pub tokens_est: Option<u32>,
  }
  pub struct CacheHint { pub breakpoint: bool, pub ttl: CacheTtl, pub prefix_key: PrefixKey }
  #[non_exhaustive] pub enum CacheTtl { FiveMinutes, OneHour }
  ```
  `PromptSegment` gets `pub fn new(role: Role, content: Vec<ContentBlock>, provenance: Provenance) -> Self` (generating a fresh `SegmentId`, `cache_hint: None`, `tokens_est: None`) and builder-style `with_cache_hint`/`with_tokens_est`. Do **not** derive or implement `Default`. Add a doc comment on `cache_hint` restating the invariant: *stripping every `cache_hint` from a `Vec<PromptSegment>` must not change the assembled request content bytes.* Provide `pub fn strip_cache_hints(segments: &mut [PromptSegment])` in `segment.rs` so tests in downstream crates can assert that invariant mechanically.

  `SegmentKind` is a lightweight classification used by the CLI `/context` renderer: `#[non_exhaustive] enum SegmentKind { SystemPrompt, SkillFragment, ToolSchemas, InheritedPrefix, Directive, Turn }`, with `impl From<&Provenance> for SegmentKind`.

  `ContextReport { pub agent_id: AgentId, pub turn: u32, pub tokenizer: String, pub segments: Vec<ContextReportEntry>, pub total_tokens_est: u32 }` where `ContextReportEntry { pub segment: SegmentId, pub provenance: Provenance, pub tokens_est: u32, pub estimated: bool }`. Per T-9 the `estimated` flag and the `tokenizer` name are mandatory — the report must never present a count as exact without naming the tokenizer that produced it.

  `log.rs` modification: add `prov: Provenance` to the four variants named in the criteria, and add the `ContextReportRecord` variant. Keep field ordering so the existing §5.1 fixtures still parse (serde is order-insensitive for structs, so this is a compatibility note, not a constraint).

---

# WI-004: Define capability, routing, and health types

- **id**: WI-004
- **title**: Define capability, routing, and health types
- **complexity**: medium
- **scope**:
  - `crates/conway-core/src/capabilities.rs` (create)
  - `crates/conway-core/src/routing.rs` (create)
- **depends**: WI-001
- **criteria**:
  - [ ] `capabilities.rs` exports `Capabilities`, `ToolCallSupport`, `CacheMode`, `StructuredOutput`, `ReliabilityTier`, `RequiredCaps`, `ProbeReport`. [machine]
  - [ ] `routing.rs` exports `RouteRequest`, `Route`, `RoutingReason`, `Observation`, `BreakerState`, `BreakerKind`, `ExplainReport`, `RoutingConfig`, `RoleConfig`, `HealthConfig`, `BackendConfig`. [machine]
  - [ ] `RouteRequest` has no field of type `String`, `Vec<ContentBlock>`, `PromptSegment`, or `Message` that could carry prompt text; a unit test asserts the field set is exactly `{role, pin, required, est_tokens, agent_id}`. [machine]
  - [ ] `RequiredCaps::satisfied_by(&Capabilities) -> Result<(), Vec<String>>` exists and a unit test verifies: a `RequiredCaps { min_context: 200_000, .. }` against `Capabilities { max_context_tokens: 32_768, .. }` returns `Err` containing a string naming both numbers. [machine]
  - [ ] `RoutingConfig` deserializes from the exact TOML snippet in architecture §"conway-routing / Internal Design Notes" (roles.planner chain, roles.fast chain, health block) via `toml`-shaped `serde_json` equivalent; a unit test round-trips it through `serde_json`. [machine]
  - [ ] All enums are `#[non_exhaustive]`; all types are `Serialize + Deserialize + Clone + Debug`. [machine]
  - [ ] `cargo test -p conway-core` passes. [machine]
- **notes**:

  **Objective:** Define the capability description model that makes backends comparable per `(backend, model)`, the content-free routing request/response contract that makes GP-07 a compile-time guarantee, and the health/breaker state vocabulary shared by `conway-routing` and `conway-runtime`.

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
  Deviation from §4.1 noted and intended: `ttls` is `Vec<CacheTtl>` rather than `&'static [CacheTtl]` because every public type must be `Deserialize` (boundary rule) and a `'static` slice is not.

  `ToolCallSupport` gets `pub fn rank(&self) -> u8` (`None`=0, `NonStreamingOnly`=1, `Streaming{validated:false}`=2, `Streaming{validated:true}`=3) and `PartialOrd` derived from it, so `RequiredCaps` can express "tool_calling >= NonStreamingOnly".

  `RequiredCaps { pub tool_calling: Option<ToolCallSupport>, pub min_context: Option<u32>, pub structured_output: Option<StructuredOutput>, pub reasoning: Option<bool>, pub parallel_tool_calls: Option<bool>, pub min_reliability: Option<ReliabilityTier> }` with `Default` (all `None`). `satisfied_by` returns `Err(Vec<String>)` where each string is a human-readable missing-capability description used verbatim in `RoutingReason::CapabilitySkip { missing }` and in `RoutingError::NoCandidate`. The min-context message format is fixed: `"min_context: requires {required} tokens, model provides {available}"`.

  `ProbeReport { pub ok: bool, pub latency_ms: u32, pub models: Vec<ModelId>, pub detail: Option<String>, pub at: DateTime<Utc> }`.

  `routing.rs`:
  ```rust
  pub struct RouteRequest { pub role: RoleAlias, pub pin: Option<ModelRef>,
                            pub required: RequiredCaps, pub est_tokens: u32, pub agent_id: AgentId }
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
                             pub breaker_states: Vec<(EndpointId, BreakerState)> }
  ```
  Note in a doc comment on `Observation`: `BadRequest`, `Auth`, and `ContextOverflow` deliberately have no `Observation` representation (§8) — they are request problems, not endpoint-health signals.

  Config types (these are *types only*; loading lives in the `conway` facade):
  ```rust
  pub struct RoutingConfig { pub roles: BTreeMap<String, RoleConfig>, pub health: HealthConfig }
  pub struct RoleConfig { pub chain: Vec<ModelRef>, pub required: RequiredCaps,
                          pub params: SamplingParams }
  pub struct HealthConfig { pub transport_failures_to_open: u32, pub open_duration_secs: u64,
                            pub probe_interval_secs: u64, pub probe_timeout_secs: u64,
                            pub probe_failures_to_open: u32 }
  pub struct BackendConfig { pub id: BackendId, pub kind: BackendKind, pub base_url: Option<String>,
                             pub api_key_env: Option<String>, pub dialect: Option<String>,
                             pub models: BTreeMap<String, ModelOverrides>, pub extra: serde_json::Map<String, serde_json::Value> }
  #[non_exhaustive] pub enum BackendKind { Anthropic, OpenAiCompat }
  pub struct ModelOverrides { pub stream_tools: Option<bool>, pub max_context_tokens: Option<u32>,
                              pub reliability_tier: Option<ReliabilityTier> }
  ```
  `HealthConfig` and `RoleConfig` implement `Default` matching the §"conway-routing" TOML defaults (`transport_failures_to_open = 3`, `open_duration_secs = 30`, `probe_interval_secs = 15`, `probe_timeout_secs = 2`, `probe_failures_to_open = 3`). Durations are plain integer-second fields, not `humantime` strings — string duration parsing belongs to the facade's config layer. Use `BTreeMap` (not `HashMap`) so serialized config is deterministically ordered.

---

# WI-005: Define agent result, budget, subagent spec, and agent/skill definition types

- **id**: WI-005
- **title**: Define agent result, budget, subagent spec, and agent/skill definition types
- **complexity**: medium
- **scope**:
  - `crates/conway-core/src/agent.rs` (create)
  - `crates/conway-core/src/config.rs` (create)
  - `crates/conway-core/src/log.rs` (modify)
- **depends**: WI-002, WI-004
- **criteria**:
  - [ ] `agent.rs` exports `AgentResult`, `ResultStatus`, `Fact`, `Budget`, `SubagentSpec`, `SubagentMode` (re-export from `log`), `AgentDefRef`, `ToolSelector`, `AgentTreeSnapshot`, `AgentNode`, `AgentMessage`, `MessageKind`, `PermissionRequest`, `PermissionDecision`, `PermissionDecisionKind`, `PermissionScope`. [machine]
  - [ ] `config.rs` exports `AgentDef`, `SkillDef`, `ConwayConfig`. [machine]
  - [ ] `ResultStatus` includes a `Rejected { missing: Vec<String> }` variant and a `BudgetExceeded` variant; a unit test asserts all five variants round-trip through `serde_json`. [machine]
  - [ ] `AgentResult::summary` is `String` (not `Option<String>`) and `AgentResult::new` enforces `summary.chars().count() <= DEFAULT_SUMMARY_LIMIT` (2000) by truncating on a char boundary; a unit test with a 5000-char summary asserts the result is exactly 2000 chars and does not panic on multi-byte input. [machine]
  - [ ] `Budget` has non-`Option` `max_steps` and `Default` yielding `max_steps: 40`; a unit test asserts `Budget::default().max_steps == 40`. [machine]
  - [ ] `ToolSelector::selects(&ToolName) -> bool` exists; unit tests cover `All`, `Only`, `Except`, and a pattern with a `*` suffix. [machine]
  - [ ] `log.rs` gains `AgentResultRecord { seq, ts, result: AgentResult }` with tag `agent_result`. [machine]
  - [ ] `cargo test -p conway-core` passes. [machine]
- **notes**:

  **Objective:** Define the terminal `AgentResult` contract that carries the MAST mitigations (bounded summary, typed facts, artifacts, `transcript_ref`, `Rejected{missing}`), the subagent spec that makes fork and spawn exactly two modes, the parent↔child message enum, the permission request/decision types, and the serde-only agent/skill definition types.

  **Implementation Notes:**

  ```rust
  pub const DEFAULT_SUMMARY_LIMIT: usize = 2000;

  pub struct AgentResult {
      pub agent_id: AgentId,
      pub status: ResultStatus,
      pub summary: String,
      pub facts: Vec<Fact>,
      pub artifacts: Vec<Artifact>,
      pub structured: Option<serde_json::Value>,
      pub transcript_ref: SessionId,
      pub usage: Usage,
      pub steps_taken: u32,
  }
  #[non_exhaustive] pub enum ResultStatus {
      Completed,
      Failed { error: String },
      Cancelled { reason: String },
      BudgetExceeded { limit: String },
      Rejected { missing: Vec<String> },
  }
  pub struct Fact { pub key: String, pub value: serde_json::Value, pub source: Option<String> }
  ```
  `AgentResult::new(agent_id, transcript_ref, status, summary) -> Self` truncates the summary as specified (use `char_indices` to find the boundary; never slice by byte offset). Add `pub fn is_terminal_success(&self) -> bool` (`matches!(status, Completed)`).

  ```rust
  pub struct Budget { pub max_steps: u32, pub deadline: Option<DateTime<Utc>>,
                      pub max_tokens: Option<u32>, pub max_tool_calls: Option<u32> }
  ```
  `Default`: `max_steps: 40`, rest `None`. `max_steps` is deliberately not optional — §6.4 requires every child to have a step budget so a parent's pending tool call can never hang.

  ```rust
  pub struct SubagentSpec {
      pub mode: SubagentMode,               // re-exported from log.rs; do NOT redefine
      pub prompt: String,
      pub agent_def: Option<AgentDefRef>,
      pub role: Option<RoleAlias>,
      pub tools: Option<ToolSelector>,
      pub budget: Budget,
      pub cache_hint: bool,
      pub result_contract: Option<schemars::schema::RootSchema>,
      pub await_result: bool,               // §"conway-tools": await:false enables fan-out
  }
  pub struct AgentDefRef(pub String);
  #[non_exhaustive] pub enum ToolSelector { All, Only(Vec<String>), Except(Vec<String>) }
  ```
  `SubagentSpec::validate(&self) -> Result<(), ConwayError>` returns `Err(ConwayError::Config{..})` when `mode == Spawn && agent_def.is_none()` (§5.2: `agent_def` is required for Spawn). `cache_hint` defaults to `true` for `Fork` and is ignored for `Spawn`; encode this in `SubagentSpec::fork(...)`/`SubagentSpec::spawn(...)` constructors.

  `ToolSelector::selects` matches entries case-sensitively; an entry ending in `*` is a prefix match on the tool name, otherwise exact equality. `All` selects everything; `Except` selects everything not matched.

  ```rust
  pub struct AgentTreeSnapshot { pub root: AgentId, pub nodes: Vec<AgentNode>, pub at: DateTime<Utc> }
  pub struct AgentNode { pub agent_id: AgentId, pub session: SessionId, pub parent: Option<AgentId>,
                         pub mode: Option<SubagentMode>, pub agent_def: Option<String>,
                         pub role: Option<RoleAlias>, pub status: AgentStatus,
                         pub steps_taken: u32, pub budget: Budget }
  #[non_exhaustive] pub enum AgentStatus { Starting, Running, AwaitingPermission, AwaitingChildren, Finished, Failed, Cancelled }
  ```
  The snapshot is a flat `Vec` with `parent` links, not a nested tree — this matches the flat, `agent`-tagged event stream and keeps it trivially serializable.

  ```rust
  #[non_exhaustive] pub enum AgentMessage {
      Steer   { from: AgentId, text: String, at_parent_seq: LogSeq },
      Cancel  { from: AgentId, reason: String, hard: bool },
      Progress{ from: AgentId, note: String },
      Result  { from: AgentId, result: AgentResult },
  }
  #[non_exhaustive] pub enum MessageKind { Steer, Cancel, Progress, Result }
  ```
  with `impl From<&AgentMessage> for MessageKind`.

  Permission types (§4.3), all in `agent.rs`:
  ```rust
  pub struct PermissionRequest { pub agent_id: AgentId, pub agent_path: Vec<AgentId>,
                                 pub tool: ToolName, pub category: ToolCategory,
                                 pub arguments: serde_json::Value, pub rendered: String,
                                 pub call_id: String }
  #[non_exhaustive] pub enum PermissionDecision {
      AllowOnce, AllowAlways { scope: PermissionScope },
      Deny { reason: String }, DenyWithFeedback { message: String },
  }
  #[non_exhaustive] pub enum PermissionScope { Session, Agent, AgentSubtree }
  #[non_exhaustive] pub enum PermissionDecisionKind { AllowOnce, AllowAlways, Denied, DeniedWithFeedback, Cached }
  ```
  `PermissionDecisionKind` is the event-stream-facing projection (`Event::PermissionResolved`); provide `impl From<&PermissionDecision> for PermissionDecisionKind`.

  `config.rs`:
  ```rust
  pub struct AgentDef { pub name: String, pub description: Option<String>,
                        pub system_prompt: String,          // markdown body
                        pub role: Option<RoleAlias>, pub model: Option<ModelRef>,
                        pub tools: ToolSelector, pub skills: Vec<String>,
                        pub max_steps: Option<u32>,
                        pub result_contract: Option<schemars::schema::RootSchema> }
  pub struct SkillDef { pub name: String, pub description: Option<String>, pub body: String,
                        pub always_include: bool }
  pub struct ConwayConfig { pub backends: Vec<BackendConfig>, pub routing: RoutingConfig,
                            pub default_role: RoleAlias, pub max_parallel_tools: usize,
                            pub fsync: FsyncPolicy, pub session_root: PathBuf,
                            pub default_budget: Budget, pub cache_ttl: CacheTtl }
  #[non_exhaustive] pub enum FsyncPolicy { Always, Interval { millis: u64 }, Never }
  ```
  `FsyncPolicy::default()` is `Interval { millis: 200 }`; `max_parallel_tools` defaults to `4`. These are *types* only: no file discovery, no path resolution, no environment reads anywhere in this crate.

---

# WI-006: Define the Event and Envelope stream types

- **id**: WI-006
- **title**: Define the Event and Envelope stream types
- **complexity**: low
- **scope**:
  - `crates/conway-core/src/event.rs` (create)
- **depends**: WI-003, WI-004, WI-005
- **criteria**:
  - [ ] `event.rs` exports `Event` and `Envelope`. [machine]
  - [ ] `Event` contains every variant listed in architecture §6.5 plus `Lagged { skipped: u64 }`; a unit test enumerates and constructs one value of each variant. [machine]
  - [ ] `Event` is `#[non_exhaustive]`, `#[serde(tag = "event", rename_all = "snake_case")]`; `Envelope` serializes to a single-line JSON object containing keys `seq`, `ts`, `session`, `agent`, and the flattened event. A unit test asserts `serde_json::to_string(&envelope)` contains no `\n`. [machine]
  - [ ] `Event::is_fatal(&self) -> bool` exists returning `true` only for `Error { fatal: true, .. }`. [machine]
  - [ ] `Event::agent_lifecycle_kind(&self) -> Option<LifecyclePhase>` exists mapping `AgentSpawned -> Start`, `AgentFinished -> End`, everything else `None`. [machine]
  - [ ] `cargo test -p conway-core` passes. [machine]
- **notes**:

  **Objective:** Define the flat, `agent`-tagged event enum that is the IDE's render surface and the CLI's `jsonl` output format, plus the `Envelope` that adds sequencing and identity.

  **Implementation Notes:**

  Transcribe §6.5 exactly, with these concrete field types:
  ```rust
  pub struct Envelope { pub seq: u64, pub ts: DateTime<Utc>,
                        pub session: SessionId, pub agent: AgentId,
                        #[serde(flatten)] pub event: Event }

  #[non_exhaustive]
  #[serde(tag = "event", rename_all = "snake_case")]
  pub enum Event {
      AgentSpawned { kind: SubagentMode, parent: Option<AgentId>,
                     agent_def: Option<String>, inherited_upto: Option<LogSeq> },
      AgentProgress { note: String },
      AgentFinished { result: AgentResult },

      TurnStarted { turn: u32 },
      ModelDecision { role: RoleAlias, chosen: ModelRef, reason: RoutingReason, attempt: u8 },
      TextDelta { text: String },
      ThinkingDelta { text: String },
      TurnFinished { usage: Usage, stop: StopReason },

      ToolCallProposed { call_id: String, tool: ToolName, args: serde_json::Value },
      PermissionRequested { call_id: String, rendered: String },
      PermissionResolved { call_id: String, decision: PermissionDecisionKind },
      ToolCallStarted { call_id: String },
      ToolProgress { call_id: String, note: String },
      ToolCallFinished { call_id: String, is_error: bool, preview: String },

      ContextSegmentAdded { segment: SegmentId, provenance: Provenance, tokens_est: u32 },
      MessageSent { to: AgentId, kind: MessageKind },
      SteerQueued { target: AgentId, queued_since: DateTime<Utc> },
      SteerDropped { target: AgentId, reason: String },
      RepeatedStep { tool: ToolName, prior_seq: LogSeq },
      BackendDegraded { endpoint: EndpointId, breaker: BreakerKind, until: DateTime<Utc> },
      Lagged { skipped: u64 },
      Error { error: ConwayError, fatal: bool },
  }
  ```
  `SteerDropped` and `SteerQueued { queued_since }` are additions justified by §6.2 (mailbox overflow emits `Event::SteerDropped`) and §6.3 (`SteerQueued{ target, queued_since }`); both are named in the architecture prose but absent from the §6.5 listing. `Lagged` is required by the §8 broadcast guarantee.

  `#[serde(flatten)]` on `Envelope::event` combined with `#[serde(tag = "event")]` on `Event` produces exactly one flat JSON object per line for `--output-format jsonl`. Do not nest the event under an `"event"` object key.

  Add a doc comment on `Envelope` restating the three §8 guarantees (monotonic `seq` per session across all agents in the tree; `AgentSpawned` precedes every event bearing that agent id; every `AgentSpawned` is eventually followed by exactly one `AgentFinished`) so downstream implementers see them at the definition site.

  `LifecyclePhase { Start, End }` is a small local enum defined in this file.

---

# WI-007: Define all port traits

- **id**: WI-007
- **title**: Define all port traits
- **complexity**: medium
- **scope**:
  - `crates/conway-core/src/ports/mod.rs` (create)
  - `crates/conway-core/src/ports/backend.rs` (create)
  - `crates/conway-core/src/ports/plugin.rs` (create)
  - `crates/conway-core/src/ports/permission.rs` (create)
  - `crates/conway-core/src/ports/session.rs` (create)
  - `crates/conway-core/src/ports/routing.rs` (create)
  - `crates/conway-core/src/ports/subagent.rs` (create)
  - `crates/conway-core/src/ports/events.rs` (create)
- **depends**: WI-003, WI-005, WI-006
- **criteria**:
  - [ ] Traits `Backend`, `Plugin`, `Tool`, `PermissionGate`, `SessionStore`, `Router`, `HealthRegistry`, `SubagentHost`, `EventSink` exist with the exact method signatures given in the Implementation Notes. [machine]
  - [ ] `Backend`, `Plugin`, `Tool`, `PermissionGate`, `SessionStore`, `SubagentHost` are `Send + Sync + 'static`; `Router` and `HealthRegistry` are `Send + Sync`. A unit test contains `fn _assert_object_safe(_: &dyn Backend, _: &dyn Plugin, _: &dyn Tool, _: &dyn PermissionGate, _: &dyn SessionStore, _: &dyn Router, _: &dyn HealthRegistry, _: &dyn SubagentHost, _: &dyn EventSink) {}`, proving every port is object-safe. [machine]
  - [ ] `Router::resolve` is a synchronous `fn` (not `async`) taking `&RouteRequest` and returning `Result<Vec<Route>, RoutingError>`. [machine]
  - [ ] `backend.rs` defines `GenerateRequest`, `GenerateResponse`, `StreamChunk`, all `Serialize + Deserialize`. [machine]
  - [ ] `plugin.rs` defines `PluginManifest`, `PluginInitCtx`, `PluginConfig`, `ToolCtx`, `ToolOutput`, `CancellationToken`. [machine]
  - [ ] `ToolCtx` contains fields `agent_id`, `session_id`, `cwd`, `cancel`, `events`, `subagents`, `config` with the types given below. [machine]
  - [ ] `cargo build -p conway-core` succeeds and `cargo tree -p conway-core -e normal` still shows no `tokio` or `reqwest`. [machine]
  - [ ] `cargo test -p conway-core` passes. [machine]
- **notes**:

  **Objective:** Define every port trait the rest of the workspace implements. These signatures are the binding contract; downstream crates compile against them and nothing else. No default method may perform I/O.

  **Implementation Notes:**

  `ports/mod.rs` declares the submodules and `pub use`s every trait and every associated type at `crate::ports::*`, then `lib.rs` re-exports `pub use ports::*` into the prelude.

  `backend.rs`:
  ```rust
  pub type BoxStream<'a, T> = core::pin::Pin<Box<dyn futures_core::Stream<Item = T> + Send + 'a>>;

  #[async_trait::async_trait]
  pub trait Backend: Send + Sync + 'static {
      fn id(&self) -> BackendId;
      fn capabilities(&self, model: &ModelId) -> Capabilities;
      async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError>;
      async fn stream(&self, req: GenerateRequest)
          -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError>;
      async fn probe(&self) -> Result<ProbeReport, BackendError>;
  }

  pub struct GenerateRequest { pub model: ModelId, pub segments: Vec<PromptSegment>,
                               pub tools: Vec<ToolSpec>, pub params: SamplingParams,
                               pub prefix_key: Option<PrefixKey> }
  pub struct GenerateResponse { pub content: Vec<ContentBlock>, pub tool_calls: Vec<ToolCall>,
                                pub stop: StopReason, pub usage: Usage }
  #[non_exhaustive] pub enum StreamChunk {
      TextDelta(String), ThinkingDelta(String),
      ToolCallDelta { index: u32, raw: String }, Done(GenerateResponse),
  }
  ```
  `BoxStream` is defined locally over `futures_core::Stream` — do not add `futures` or `futures-util` as a dependency. Doc-comment `GenerateRequest::segments`: *order is load-bearing for implicit-prefix caching; adapters MUST NOT reorder, merge, or drop segments.* Doc-comment `prefix_key`: reserved for `CacheMode::SlotKv`; adapters that do not support slots ignore it.

  `plugin.rs`:
  ```rust
  pub trait Plugin: Send + Sync + 'static {
      fn manifest(&self) -> PluginManifest;
      fn tools(&self) -> Vec<Arc<dyn Tool>>;
      fn on_init(&self, _ctx: &PluginInitCtx) -> Result<(), PluginError> { Ok(()) }
  }
  #[async_trait::async_trait]
  pub trait Tool: Send + Sync + 'static {
      fn spec(&self) -> ToolSpec;
      async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError>;
  }
  pub struct PluginManifest { pub id: String, pub version: String,
                              pub tools: Vec<ToolName>, pub required_host_caps: Vec<String> }
  pub struct PluginInitCtx { pub config: Arc<PluginConfig>, pub cwd: PathBuf }
  pub struct PluginConfig { pub values: serde_json::Map<String, serde_json::Value> }
  pub struct ToolCtx {
      pub agent_id: AgentId,
      pub session_id: SessionId,
      pub cwd: PathBuf,
      pub cancel: CancellationToken,
      pub events: EventSinkHandle,          // Arc<dyn EventSink>
      pub subagents: Arc<dyn SubagentHost>,
      pub config: Arc<PluginConfig>,
  }
  pub struct ToolOutput { pub blocks: Vec<ContentBlock>, pub is_error: bool,
                          pub truncation: TruncationPolicy, pub artifacts: Vec<Artifact> }
  ```
  `ToolCtx` is `Clone` (all fields are `Arc`/`Copy`/cheap). It is **not** `Serialize` — it holds trait objects; this is the known T-8 limitation and should be doc-commented as such (`ToolCall` and `ToolOutput` are fully serializable so a future subprocess plugin transport only needs an RPC form of `ToolCtx`).

  `CancellationToken` must be defined here without a tokio dependency: `#[derive(Clone, Debug, Default)] pub struct CancellationToken(Arc<AtomicBool>)` with `pub fn cancel(&self)`, `pub fn is_cancelled(&self) -> bool`, and `pub fn child(&self) -> CancellationToken` (a new token whose `is_cancelled` also returns true when the parent is cancelled — implement by holding `Option<Arc<AtomicBool>>` parent handle). Downstream crates that need async cancellation await can bridge to `tokio_util::sync::CancellationToken` themselves; `conway-core` must not depend on tokio. Doc-comment this design choice.

  `ports/events.rs`:
  ```rust
  pub trait EventSink: Send + Sync + 'static { fn emit(&self, event: Event); }
  pub type EventSinkHandle = Arc<dyn EventSink>;
  ```
  `emit` is synchronous and non-blocking by contract (a broadcast send); doc-comment that an implementation must never block the caller and drops with `Event::Lagged` instead.

  `permission.rs`:
  ```rust
  #[async_trait::async_trait]
  pub trait PermissionGate: Send + Sync + 'static {
      async fn check(&self, req: PermissionRequest) -> PermissionDecision;
  }
  ```
  Doc-comment the §8 contract: the gate may block indefinitely; gate cancellation surfaces as `Deny { reason: "cancelled" }`.

  `session.rs`: transcribe §4.4 verbatim (`create`, `append`, `read`, `head`, `fork`, `meta`, `children`, `list`), `#[async_trait]`, all returning `Result<_, StoreError>`. Doc-comment `fork`: *writes exactly one header line; copies zero records; O(1) in parent transcript size.*

  `routing.rs`:
  ```rust
  pub trait Router: Send + Sync {
      fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError>;
  }
  pub trait HealthRegistry: Send + Sync {
      fn state(&self, ep: &EndpointId) -> BreakerState;
      fn record(&self, ep: &EndpointId, obs: Observation);
  }
  ```
  Doc-comment `resolve`: *MUST be pure with respect to request content; `RouteRequest` carries no prompt text by construction. MUST NOT mutate breaker state. On success the returned Vec is non-empty and every element carries a `RoutingReason`. On failure `RoutingError::NoCandidate` enumerates every rejection.* Also note the T-1 contract: if no candidate's `max_context_tokens` covers `req.est_tokens`, `resolve` returns `RoutingError::ContextTooLarge` naming the shortfall — it must never return a candidate that cannot fit the request.

  `subagent.rs`: transcribe §4.6 verbatim, `#[async_trait]`, all `Result<_, RuntimeError>`, plus `fn tree(&self) -> AgentTreeSnapshot`. Doc-comment the invariant: *`await_result` always terminates — the supervisor synthesizes a result on budget exhaustion, cancellation, or task panic.*

---

# WI-008: Implement feature-gated test fakes

- **id**: WI-008
- **title**: Implement feature-gated test fakes
- **complexity**: medium
- **scope**:
  - `crates/conway-core/src/fakes.rs` (create)
  - `crates/conway-core/tests/fakes_conformance.rs` (create)
- **depends**: WI-007
- **criteria**:
  - [ ] `fakes.rs` is gated by `#[cfg(feature = "fakes")]` and exports `FakeBackend`, `ScriptedBackend`, `FakeStore`, `FakeGate`, `FakeRouter`, `FakeHealth`, `FakeSubagentHost`, `CollectingEventSink`. [machine]
  - [ ] `cargo build -p conway-core` (no features) compiles and `cargo tree` shows no additional dependencies vs. WI-001. [machine]
  - [ ] `cargo test -p conway-core --features fakes` passes; `cargo test -p conway-core` (no features) also passes with the conformance test skipped via `#![cfg(feature = "fakes")]` at the top of the test file. [machine]
  - [ ] `ScriptedBackend::new(Vec<ScriptedTurn>)` returns responses in order and returns `BackendError::BadRequest` when the script is exhausted; a test asserts both. [machine]
  - [ ] `ScriptedBackend::stream` emits the same content as `generate` decomposed into `TextDelta` chunks followed by exactly one `Done(GenerateResponse)`; a test asserts the concatenated deltas equal the `generate` text. [machine]
  - [ ] `FakeStore` implements the full `SessionStore` trait in memory, and a test asserts `fork` copies zero records: after forking a 100-record parent, `FakeStore` internal record count increases by 0 and the child's `meta().origin` equals `Some(ForkOrigin{parent, at_seq, mode})`. [machine]
  - [ ] `FakeGate::new(PermissionDecision)` returns a fixed decision, and `FakeGate::recording()` captures every `PermissionRequest` for assertion; a test asserts `agent_path` is preserved. [machine]
  - [ ] `CollectingEventSink::events() -> Vec<Event>` returns emitted events in order; a test asserts ordering. [machine]
  - [ ] A test asserts the cache-hint invariant helper: for a `Vec<PromptSegment>` with hints, `strip_cache_hints` leaves `content` and `provenance` of every segment byte-identical (compared via `serde_json::to_string`). [machine]
  - [ ] `cargo clippy -p conway-core --all-features -- -D warnings` is clean. [machine]
- **notes**:

  **Objective:** Provide in-crate test doubles for every port so `conway-runtime` (Group 1 track E) can be developed and tested end-to-end with zero network and zero filesystem access, per GP-04. These are the only implementations `conway-core` is permitted to contain.

  **Implementation Notes:**

  All fakes use `std::sync::Mutex`/`RwLock` — no tokio, no async runtime primitives. `#[async_trait]` methods do their work synchronously inside the async fn body.

  `FakeBackend { id: BackendId, caps: Capabilities, response: GenerateResponse }` — returns the same `GenerateResponse` for every call, `probe` returns `ProbeReport { ok: true, latency_ms: 1, .. }`. Constructor `FakeBackend::echo(id)` returns a backend whose response is a single `ContentBlock::Text` echoing the concatenated text of the last `User`-role segment, with `stop: EndTurn` and zeroed `Usage`. Add `FakeBackend::with_capabilities(caps)` and `FakeBackend::failing(BackendError)` (every call returns the given error — needed to test the runtime's fallback loop and health recording).

  `ScriptedBackend { script: Mutex<VecDeque<ScriptedTurn>>, id, caps, calls: Mutex<Vec<GenerateRequest>> }` where
  ```rust
  pub enum ScriptedTurn { Respond(GenerateResponse), Fail(BackendError) }
  ```
  Expose `pub fn calls(&self) -> Vec<GenerateRequest>` so tests can assert segment ordering (§5.3) and cache-hint placement. Exhausted script ⇒ `BackendError::BadRequest { detail: "scripted backend exhausted" }`.

  `FakeStore { sessions: RwLock<BTreeMap<SessionId, FakeSession>> }` with `FakeSession { meta: SessionMeta, records: Vec<LogRecord> }`. `append` assigns the next `LogSeq` and returns it. `fork` inserts a new `FakeSession` with `records: vec![]` and `meta.origin = Some(ForkOrigin{ parent, at_seq: at, mode })` — it must not clone the parent's `records` vector; the zero-copy criterion is asserted by counting total records across all sessions before and after. `read` honors `SeqRange` and returns `StoreError::SeqOutOfRange` when `range.start > head`. `children` scans for sessions whose `origin.parent` matches. Add `pub fn total_record_count(&self) -> usize` for the fork test.

  `FakeGate` — two constructors as in the criteria; `recording()` stores requests in a `Mutex<Vec<PermissionRequest>>` exposed via `pub fn requests(&self)`, and returns `PermissionDecision::AllowOnce` by default. Add `FakeGate::deny_all(reason)`.

  `FakeRouter { routes: Vec<Route>, err: Option<RoutingError> }` — `resolve` returns the configured routes (cloned) or the configured error. Add `FakeRouter::single(ModelRef)` producing a one-element chain with `RoutingReason::AliasPrimary`. Add `FakeRouter::context_too_large(role, model, required, max)` producing the T-1 error, so the runtime's rejection path is testable without a real router.

  `FakeHealth { states: RwLock<BTreeMap<EndpointId, BreakerState>>, observations: Mutex<Vec<(EndpointId, Observation)>> }` — `state` returns `Closed` for unknown endpoints; `record` appends. Expose `pub fn observations(&self)` so tests can assert that `BadRequest`/`Auth`/`ContextOverflow` produced **no** observation.

  `FakeSubagentHost { started: Mutex<Vec<(AgentId, SubagentSpec)>>, results: Mutex<BTreeMap<AgentId, AgentResult>>, tree: RwLock<AgentTreeSnapshot> }` — `start` records the spec and returns a fresh `AgentId`; `await_result` returns the preconfigured result or, if none, a synthesized `AgentResult` with `ResultStatus::Completed` and `summary: "fake"` (never blocks, honoring the always-terminates invariant); `steer`/`cancel` record and return `Ok(())`.

  `CollectingEventSink { events: Mutex<Vec<Event>> }` implementing `EventSink::emit` by pushing. Add `pub fn clear(&self)` and `pub fn find<F: Fn(&Event) -> bool>(&self, f: F) -> Option<Event>`.

  `tests/fakes_conformance.rs` starts with `#![cfg(feature = "fakes")]` and contains the assertions named in the criteria. It also contains a `fn _ports_are_usable_as_trait_objects()` compile-check constructing `Arc<dyn Backend>`, `Arc<dyn SessionStore>`, `Arc<dyn PermissionGate>`, `Arc<dyn Router>`, `Arc<dyn HealthRegistry>`, `Arc<dyn SubagentHost>`, `Arc<dyn EventSink>` from the fakes — this is the mechanical proof that every port is usable exactly as `RuntimeDeps` requires.

---

## Coverage Statement

**Module:** conway-core
**Work items:** WI-001, WI-002, WI-003, WI-004, WI-005, WI-006, WI-007, WI-008

**Coverage:** These eight work items collectively implement 100% of the conway-core module scope — the domain model, every port trait, and the feature-gated test fakes — plus the cargo workspace root scaffolding assigned to this module as the Group 0 foundation. Nothing in the module spec is excluded. Two deliberate deviations from the literal architecture text are recorded in-item and are not scope reductions: (a) `Capabilities::cache::ExplicitBreakpoints::ttls` is `Vec<CacheTtl>` rather than `&'static [CacheTtl]`, because the boundary rule "every public type is Serialize + Deserialize" is incompatible with a `'static` slice; (b) `LogRecord::Assistant::route_reason` stays `serde_json::Value` rather than a typed `RoutingReason`, because the session store must not interpret record semantics and typed access is available on `Event::ModelDecision`. One dependency beyond the module's stated Requires list is added: `ulid`, mandated by the Internal Design Notes ("IDs are ULIDs") and compliant with the no-I/O boundary rule.

**Provides implemented by:**

| Provides entry | Work item(s) |
|---|---|
| `Message`, `ContentBlock`, `Role`, `ToolCall`, `ToolResult` | WI-002 |
| `LogRecord` | WI-002 (created), WI-003 (provenance fields, `ContextReportRecord`), WI-005 (`AgentResultRecord`) |
| `LogSeq`, `SeqRange` | WI-001 |
| `SessionId`, `AgentId`, `SegmentId`, `ModelId`, `BackendId`, `EndpointId`, `RoleAlias` | WI-001 |
| `Provenance` (all nine variants) | WI-003 |
| `PromptSegment`, `CacheHint`, `PrefixKey`, `TruncationPolicy` | WI-003 (`PromptSegment`, `CacheHint`), WI-001 (`PrefixKey`), WI-002 (`TruncationPolicy`) |
| Ports: `Backend`, `Plugin`, `Tool`, `PermissionGate`, `SessionStore`, `Router`, `HealthRegistry`, `SubagentHost`, `EventSink` | WI-007 |
| `Capabilities`, `ToolCallSupport`, `CacheMode`, `ReliabilityTier`, `RequiredCaps` | WI-004 |
| `Event`, `Envelope` | WI-006 |
| `AgentResult`, `ResultStatus`, `Fact`, `Budget` | WI-005 |
| `Artifact`, `Usage` | WI-002 |
| `AgentDef`, `SkillDef`, `ToolSelector` | WI-005 |
| `RoutingConfig`, `BackendConfig` | WI-004 |
| `ConwayError` + `BackendError`, `ToolError`, `StoreError`, `RoutingError`, `RuntimeError` (+ `PluginError`) | WI-001 |
| T-1 typed rejection: `RoutingError::ContextTooLarge`, `RuntimeError::ForkContextOverflow` | WI-001 (defined), WI-007 (contract doc-commented on `Router::resolve`), WI-008 (`FakeRouter::context_too_large` makes the path testable) |
| `#[cfg(feature="fakes")] FakeBackend`, `FakeStore`, `FakeGate`, `ScriptedBackend` | WI-008 |
| Supporting types crossing boundaries (`ToolSpec`, `ToolCategory`, `ToolCtx`, `ToolOutput`, `GenerateRequest/Response`, `StreamChunk`, `SessionMeta`, `ForkOrigin`, `SubagentSpec`, `SubagentMode`, `AgentMessage`, `PermissionRequest/Decision`, `RouteRequest`, `Route`, `RoutingReason`, `Observation`, `BreakerState/Kind`, `AgentTreeSnapshot`, `ContextReport`) | WI-002, WI-003, WI-004, WI-005, WI-007 as itemized above |
| Workspace root scaffolding (root `Cargo.toml`, toolchain, crate manifest, feature flags, `cargo tree` boundary check) | WI-001 |

**Requires consumed by:** conway-core requires nothing from the workspace. External crate usage: `serde`/`serde_json` — every item; `thiserror` — WI-001; `chrono` — WI-002, WI-003, WI-004, WI-005, WI-006; `schemars` — WI-002, WI-005; `blake3` — WI-001 (`PrefixKey::from_blake3`); `async-trait` — WI-007, WI-008; `futures-core` — WI-007 (`BoxStream`), WI-008; `ulid` — WI-001.

**Dependency graph (DAG, depth 5):**
```
WI-001 ─┬─> WI-002 ─> WI-003 ─┐
        │      └──────────────┼─> WI-005 ─┐
        └─> WI-004 ───────────┴───────────┴─> WI-006 ─> WI-007 ─> WI-008
```
WI-002 and WI-004 are parallel after WI-001. No file appears in two items except `lib.rs` (WI-001 create, WI-002 modify — dependency-ordered) and `log.rs` (WI-002 create, WI-003 modify, WI-005 modify — all dependency-ordered through WI-002).