# conway-core

`conway-core` is the foundation of the [ports-and-adapters workspace](/ARCHITECTURE.md):
domain types and port traits, nothing else. Every other crate either
implements one of its ports (an adapter) or wires adapter implementations
together (a consumer). See [`/ARCHITECTURE.md`](/ARCHITECTURE.md) for the
whole-system picture; this doc covers what lives in this crate specifically.

## Responsibility and boundary

`conway-core` performs **no I/O**. It has no `tokio`, no `reqwest`, no
filesystem or network calls anywhere in its non-test code — the crate is
pure computation: type definitions, trait signatures, and (behind
`feature = "fakes"`) in-memory test doubles for every port. This is
mechanically enforceable: `cargo tree -p conway-core -e normal` contains no
line matching `reqwest|tokio|hyper`.

Every public type is `Serialize + Deserialize`. This is not incidental — it
is what lets the event stream, the JSONL session log, and a future
subprocess/RPC plugin transport all be built on the same vocabulary without
a translation layer. The one deliberate exception is `ToolCtx`
(`ports::plugin`), which holds trait objects (`Arc<dyn EventSink>`,
`Arc<dyn SubagentHost>`) and is therefore `Clone` but not `Serialize`.

`conway-core` depends on nothing else in the workspace. Everything else
depends on it, directly or transitively.

## Module layout

```
ids           ULID- and String-backed identifier newtypes
error         the full error taxonomy (BackendError, ToolError, StoreError,
              RoutingError, RuntimeError, PluginError, ConwayError)
content       Role, ContentBlock, Message, ToolCall/ToolResult/ToolSpec,
              Usage, StopReason, SamplingParams
log           LogRecord, SessionMeta — the append-only log's vocabulary
provenance    Provenance, ContextReport
segment       PromptSegment, CacheHint, CacheTtl, SegmentKind
capabilities  Capabilities, RequiredCaps (incl. reasoning headroom),
              ToolCallSupport, CacheMode, ReliabilityTier
routing       RouteRequest, Route, RoutingReason, BreakerState,
              Observation, ExplainReport, RoutingConfig, HealthConfig
agent         AgentResult, Budget, SubagentSpec, AgentTreeSnapshot,
              PermissionRequest/Decision
config        AgentDef, SkillDef, ConwayConfig (types only — no loading)
event         Event, Envelope — the flat, agent-tagged event stream
ports         every port trait (backend, plugin, permission, session,
              routing, subagent, events)
fakes         (feature = "fakes") in-memory test doubles for every port
```

## The port traits

These nine traits are the binding contract every implementation crate
compiles against — `conway-runtime`'s `RuntimeDeps` holds each of them as a
trait object (`Arc<dyn Backend>`, `Arc<dyn Router>`, ...), and a unit test in
`ports::mod` (`_assert_object_safe`) mechanically proves every one is
dyn-compatible.

- **`Backend`** (`ports::backend`) — `id`, `capabilities(&ModelId)`,
  `generate`, `stream`, `probe`. One implementation per LLM provider
  dialect; see [`conway-backends`](conway-backends.md). `GenerateRequest`
  carries an ordered `Vec<PromptSegment>` that adapters must not reorder,
  merge, or drop — order is load-bearing for implicit-prefix caching.
- **`Plugin` / `Tool`** (`ports::plugin`) — a `Plugin` declares the `Tool`s
  it provides (there is deliberately no init hook: setup belongs in the
  plugin's own constructor, before `with_plugin`, where a failure reaches
  the embedder — see the trait's own doc); a `Tool` declares its `ToolSpec`
  (JSON schema, `ToolCategory`, `PermissionClass`), an async `invoke`, and
  `render(&self, args: &Value) -> String`, a human-readable one-liner for a
  proposed call — the text behind the permission prompt,
  `Event::PermissionRequested`, and pattern-grant prefix matching (see
  [`conway-cli`](conway-cli.md)'s "Permission modes and pattern grants"
  section). `render` has a default (a generic `name(args)` one-liner,
  correct for a tool with no natural single-command shape) so existing
  third-party `Tool` implementations keep compiling unmodified; a tool whose
  call genuinely IS a single command (`bash`) overrides it. `args` is
  model-supplied and therefore untrusted — an implementation must not panic
  on any shape. This is the **only** extension mechanism — the built-in
  filesystem/shell/subagent/report tools in `conway-tools` implement the
  exact same traits a third party would. See
  [`conway-tools`](conway-tools.md) for the full extensibility story.
- **`PermissionGate`** (`ports::permission`) — `async fn check(&self, req:
  PermissionRequest) -> PermissionDecision`. Always implemented by the
  consumer; conway ships no privileged bypass.
- **`SessionStore`** (`ports::session`) — `create`, `append`, `read`,
  `head`, `fork`, `meta`, `children`, `list`. The MVP implementation is
  `JsonlSessionStore` in [`conway-session`](conway-session.md); `fork`
  writes exactly one header line and copies zero records, which is what
  makes fork O(1) regardless of parent transcript size.
- **`Router` / `HealthRegistry`** (`ports::routing`) — deliberately split:
  `Router::resolve` owns *policy* (a pure function of `RouteRequest`, which
  by construction cannot carry prompt text — `RouteRequest`'s field set is
  exactly `{role, pin, required, est_tokens, agent_id}`), `HealthRegistry`
  owns *state* (per-endpoint breaker status). See
  [`conway-routing`](conway-routing.md).
- **`SubagentHost`** (`ports::subagent`) — `start`, `steer`, `await_result`,
  `cancel`, `tree`. The cycle-breaker that lets a tool living in
  `conway-tools` drive fork/spawn without depending on `conway-runtime`:
  the runtime implements this port and hands the trait object down through
  `ToolCtx`. The developer API (`SessionHandle::fork`/`spawn`) and the
  built-in `conway_subagent` tool call the exact same trait, so the tool
  has no privileged access the public API lacks.
- **`EventSink`** (`ports::events`) — one method, `emit(&self, event:
  Event)`, contractually synchronous and non-blocking; a slow consumer is
  dropped from delivery and sees `Event::Lagged { skipped }` on its next
  receive rather than stalling the runtime.
- **`ContextHook`** (`ports::plugin`, 0.2.0) — see below.

## `ContextHook`: pluggable context and tool curation (0.2.0)

`ContextHook` unifies what used to be several separate curation ideas
(masking, tool-announcement narrowing, overflow-time trimming) into one
port trait, invoked once per assembled request before it is routed:

```rust
#[async_trait]
pub trait ContextHook: Send + Sync + 'static {
    async fn before_request(&self, ctx: &ContextHookCtx, payload: ContextPayload)
        -> ContextPayload;

    async fn on_overflow(&self, ctx: &ContextHookCtx, payload: ContextPayload,
                          overflow: OverflowInfo) -> Option<ContextPayload> {
        None // default: fall through to a hard ContextTooLarge
    }
}
```

`ContextPayload { segments: Vec<PromptSegment>, tools: Vec<ToolSpec> }`
bundles the assembled prompt segments together with the tool set announced
to the model for this turn, because the runtime treats them as one outgoing
request: a hook can edit or drop a segment *and* narrow the announced
`tools` in the same call. `before_request` is async specifically so an
inference-driven hook (one that issues its own LLM call to decide what to
keep) is a first-class case, not a special one.

`on_overflow` is a distinct, optional second method — it fires only when
the *already-hooked* payload still doesn't fit the routed model's window
(the runtime's T-1 context gate). Its default (`None`) is treated identically
to no hook being registered: a hard `RoutingError::ContextTooLarge` /
`RuntimeError::ForkContextOverflow`. This keeps "no hook registered → prior
behavior exactly" a guarantee per method, not just per trait — a consumer
that implements curation logic in `before_request` does not thereby also
suppress the hard overflow error unless it opts in.

**No hook is registered by default, and `conway-core` ships no
implementation.** The runtime holds `Option<Arc<dyn ContextHook>>`; with
`None` (the default), nothing is invoked, not even a no-op pass-through —
behavior is byte-for-byte what it was before the hook existed. See
[`conway-runtime`](conway-runtime.md) for where the hook is invoked in the
turn loop, and [`conway`](conway.md) for how a consumer registers one via
`ConwayBuilder`.

## The no-I/O rule

This is a structural constraint, not a style preference: `conway-core`'s
`Cargo.toml` may only depend on crates drawn from `[workspace.dependencies]`
that are themselves I/O-free (`serde`, `serde_json`, `thiserror`,
`async-trait`, `futures-core`, `chrono`, `blake3`, `schemars`, `ulid`). It
exists so:

- an embedder can depend on `conway` alone without transitively pulling in
  every backend's HTTP client or a TUI renderer;
- a third-party plugin author can depend on `conway-core` — a small,
  slow-moving crate with strict semver discipline — without depending on
  the whole harness;
- every port has a fake/test-double available behind `feature = "fakes"`
  (see `fakes.rs`), so `conway-runtime` and `conway-tools` are testable
  end-to-end with zero network.

## Key types and invariants

- **Content-block / `ToolResultBlock` model.** `ContentBlock` is the
  smallest unit of message content, and it is recursive:

  ```rust
  #[non_exhaustive]
  pub enum ContentBlock {
      Text { text: String },
      Thinking { text: String, signature: Option<String> },
      ToolUse { call_id: String, name: ToolName, arguments: serde_json::Value },
      ToolResultBlock { call_id: String, blocks: Vec<ContentBlock>, is_error: bool },
      Image { media_type: String, data_base64: String },
  }
  ```

  `ToolResultBlock` carries its own nested `blocks`, which is what lets a
  tool result be multi-part (e.g. text plus an image) and lets the same
  `ContentBlock` vocabulary represent both what the model said and what a
  tool returned. `Thinking` carries an optional `signature` for backends
  (Anthropic) that require a round-tripped cryptographic signature on
  extended-thinking blocks across a multi-turn tool-call loop — see
  [`conway-backends`](conway-backends.md) for how that's enforced at the
  wire layer. It is `conway-backends`' dialect adapters that turn
  `ToolUse`/`ToolResultBlock` into the wire-specific tool-call and
  tool-result message shapes each provider expects, and 0.2.0 fixed a bug
  where tool results were being assembled but not actually serialized into
  outgoing requests on some dialects (see the runtime and backends docs).

- **`Provenance`** (`provenance.rs`) is the crate's most semantically
  load-bearing type: nine variants (`UserPrompt`, `AgentDef`, `Skill`,
  `ToolRegistry`, `Inherited`, `ForkDirective`, `ParentSteer`, `ToolResult`,
  `SystemNote`) tagging *where a segment came from*. `Provenance::tier()`
  maps each variant to `Static | Inherited | Volatile`, which is the
  ordering `ContextBuilder` (in `conway-runtime`) sorts by to produce the
  fixed segment order (static content, then inherited prefix, then
  volatile) that implicit-prefix-caching backends depend on for cache hits.
  `PromptSegment` has no `Default` impl and a non-`Option` `provenance`
  field by construction — a segment cannot be built without declaring where
  it came from.

- **`Event::UserTurn`** (`event.rs`) is the typed counterpart of a user's own
  turn text on the flat event stream — `text: String, prov: Provenance`,
  mirroring `LogRecord::UserTurn`'s own fields byte-for-byte. Before this
  variant existed, a user turn had no `Event` representation at all: replay
  fell back to `Event::AgentProgress { note: format!("user turn: {text}") }`,
  so a consumer could only recognize one by matching that literal string
  prefix — fragile (a genuine free-text notice could start with it too) and
  a violation of GP-10 ("context provenance is visible"/inspectable without
  string-sniffing). `Event` is `#[non_exhaustive]`, but its variant count is
  pinned by a test (`event.rs`'s `every_variant_constructs_and_round_trips_
  with_exact_tag`) precisely so a new variant is never added without that
  test being updated deliberately, as this one was. `ForkDirective`/
  `ParentSteer` remain on the `AgentProgress` fallback — a deliberate,
  disclosed scope decision (not an oversight): closing them needs the same
  attach-ordering care `UserTurn`'s live emission required (see
  [`conway-runtime`](conway-runtime.md)) plus auditing a differently-shaped
  call site (the parent-steer mailbox drain), which a later item can pick up
  without revisiting this enum's shape again.

- **Reasoning headroom** (`capabilities.rs`, 0.2.0). `Capabilities::
  max_context_tokens` is the *total* window — prompt plus generated output
  plus reasoning tokens — so gating only on assembled-prompt size would
  admit requests that overflow mid-generation. `RequiredCaps` carries a
  non-`Option` `headroom_tokens` field (default `DEFAULT_HEADROOM_TOKENS =
  8192`) reserved for output/reasoning, and
  `RequiredCaps::satisfied_by(&Capabilities, est_tokens)` enforces
  `est_tokens + headroom_tokens <= max_context_tokens` with saturating
  arithmetic. `RoutingConfig`/`RoleConfig` carry a
  `default_headroom_tokens` / per-role `headroom_tokens` override chain
  (`RoutingConfig::headroom_for`), and `ModelOverrides::
  min_headroom_tokens` is a per-model *floor* applied via `max()`, never a
  reduction. The two T-1 error variants —
  `RoutingError::ContextTooLarge` and `RuntimeError::ForkContextOverflow`
  — carry `est_tokens`, `headroom_tokens`, `required_tokens`, and
  `shortfall_tokens` together so their `Display` message names the whole
  accounting rather than leaving a user to wonder why a 24k-token prompt
  was rejected against a 32k-token model.

- **The T-1 rule: no truncation, no silent escalation.** When an assembled
  context (plus reserved headroom) exceeds the resolved model's window, the
  request is rejected outright — `RoutingError::ContextTooLarge` at
  route-resolution time, or `RuntimeError::ForkContextOverflow` at the fork
  boundary. Neither error type has any field capable of expressing a
  truncation or escalation outcome; this is enforced by construction, not
  by convention. `ContextHook::on_overflow` (above) is the one sanctioned
  way a consumer can intervene before that hard error fires.

- **Fork vs. spawn.** `SubagentSpec::mode: SubagentMode` (`Fork | Spawn`) is
  the only distinction between conway's two ways to create a child agent —
  see [`/ARCHITECTURE.md §3.1`](/ARCHITECTURE.md) for the full semantics.
  `SubagentSpec::validate` no longer rejects `Spawn` without an `agent_def`
  (a recorded design decision relaxed the earlier "clean slate needs a
  system prompt from somewhere" rule): a spawn with `agent_def: None` is
  valid, and `conway_runtime`'s `SubagentHost::start` resolves its role the
  same way a roleless fork does — inherit the spawning session's role (and,
  transitively, its model routing) — rather than inventing a placeholder
  def. `cache_hint` defaults to `true` for `Fork` and is ignored for
  `Spawn`.

- **`AgentResult`** bounds a subagent's terminal report: `summary: String`
  (not `Option`) is truncated to `DEFAULT_SUMMARY_LIMIT` (2000 chars, on a
  char boundary) by `AgentResult::new`, so a runaway subagent cannot flood
  a parent's context with an unbounded result. `ResultStatus::Rejected {
  missing }` and `BudgetExceeded` are first-class terminal states alongside
  `Completed`/`Failed`/`Cancelled`.

- **The append-only `LogRecord` union** (`log.rs`) is what `conway-session`
  persists as JSONL, one record per line, `#[serde(tag = "kind")]`. It is
  the source of truth conway-runtime's `ContextBuilder` replays into
  `PromptSegment`s; see [`conway-session`](conway-session.md) and
  [`conway-runtime`](conway-runtime.md).

## How it fits the whole

`conway-core` is the one crate every other workspace member depends on; see
the dependency diagram in [`/ARCHITECTURE.md §2`](/ARCHITECTURE.md).
Sibling docs:

- [`conway-backends`](conway-backends.md) implements `Backend`.
- [`conway-routing`](conway-routing.md) implements `Router` and
  `HealthRegistry`.
- [`conway-session`](conway-session.md) implements `SessionStore`.
- [`conway-tools`](conway-tools.md) implements `Plugin`/`Tool` for the
  built-ins and defines the `PermissionGate` model third parties build
  against.
- [`conway-runtime`](conway-runtime.md) implements `SubagentHost`, drives
  `ContextHook`, and owns the agent loop that ties every port together.
- [`conway`](conway.md) is the facade that assembles concrete adapters
  behind these ports into `ConwayBuilder`/`Conway`.
- [`conway-cli`](conway-cli.md) is a consumer of the facade, not of
  `conway-core` directly.
