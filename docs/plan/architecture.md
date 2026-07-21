I've read all four research reports. Here is the architecture.

---

# Architecture: conway

**Version:** 0.1 (MVP design)
**Sources:** research-backends.md, research-prior-art.md, research-routing.md, research-acp.md; guiding principles GP-01…GP-10; constraints C-01…C-05.

---

## 1. Workspace Layout (and why)

A **cargo workspace with 8 crates**, structured as ports-and-adapters (hexagonal). Justification:

| Reason | Consequence |
|---|---|
| C-03 library-first, C-04 OSS-ready | An embedder (the Tauri IDE) must be able to depend on the harness without pulling in TUI, clap, or every backend's HTTP client. Crate boundaries are the only real dependency boundary in Rust; modules in one crate cannot be excluded from a build. |
| GP-03 everything is a plugin | Third-party plugin authors must depend on a small, slow-moving crate (`conway-core`) — not on the runtime. A single-crate design would force plugin authors to depend on the whole harness. |
| GP-06 capability-aware backends | Backend adapters are feature-gated (`anthropic`, `openai-compat`, later `llamacpp-native`). Feature gating is per-crate-consumer and works cleanly only with a dedicated crate. |
| GP-04 thin testable slices | Every port trait lives in `conway-core` with a fake implementation in-crate behind `cfg(feature="test-fakes")`. The runtime is testable end-to-end against fakes with zero network. |
| Compile times | The runtime + CLI iterate fastest when adapters and stores are separate compilation units. |

```
conway/
├── Cargo.toml                  # workspace
├── crates/
│   ├── conway-core/            # domain types + ALL port traits. No I/O, no tokio-net.
│   ├── conway-backends/        # Backend impls: anthropic, openai-compat (feature-gated)
│   ├── conway-routing/         # Router (declarative policy) + Health (circuit breakers)
│   ├── conway-session/         # JsonlSessionStore: append-only log, fork refs, provenance
│   ├── conway-tools/           # built-in plugins: fs, bash, subagent (fork/spawn), report
│   ├── conway-runtime/         # agent loop, agent tree, mailboxes, context assembly, event bus
│   ├── conway/                 # facade: config loading, builder, public API surface
│   └── conway-cli/             # bin: interactive REPL + `-p` one-shot streaming
└── docs/
```

Dependency direction is strictly downward; `conway-core` depends on nothing in the workspace.

```
conway-cli ──> conway ──> conway-runtime ──┬─> conway-routing ─┐
                  │           │            ├─> conway-session ─┤
                  │           │            ├─> conway-tools ───┤─> conway-core
                  └───────────┴────────────┴─> conway-backends ┘
```

There are **no cycles**. The one place a cycle would naturally appear — the fork/spawn *tool* (in `conway-tools`) needing to drive the runtime — is broken by the `SubagentHost` port trait, defined in `conway-core` and implemented by `conway-runtime`, handed to tools through `ToolCtx`.

---

## 2. Component Map

```
┌──────────────────────────────────────────────────────────────────────────┐
│ Consumers (thin)                                                          │
│   Tauri IDE (Rust lib)   │   conway-cli interactive   │  conway -p        │
└───────────────┬───────────────────────┬────────────────────────┬─────────┘
                └───────────────────────┴────────────────────────┘
                                        │  SessionHandle + Event stream
┌───────────────────────────────────────▼──────────────────────────────────┐
│ conway (facade)                                                           │
│   ConwayBuilder · config load/merge · SessionHandle · EventStream         │
└───────────────────────────────────────┬──────────────────────────────────┘
┌───────────────────────────────────────▼──────────────────────────────────┐
│ conway-runtime                                                            │
│  ┌────────────┐ ┌──────────────┐ ┌──────────────┐ ┌───────────────────┐  │
│  │ AgentTree  │ │  AgentLoop   │ │ContextBuilder│ │  EventBus         │  │
│  │ supervisor │ │ (per agent)  │ │ + Provenance │ │ (broadcast, seq'd)│  │
│  └────────────┘ └──────┬───────┘ └──────────────┘ └───────────────────┘  │
│  ┌────────────┐ ┌──────▼───────┐ ┌──────────────┐ ┌───────────────────┐  │
│  │  Mailbox   │ │ ToolRunner + │ │ SubagentHost │ │ PermissionBroker  │  │
│  │  (steer)   │ │ PluginRegistry│ │  (impl)     │ │ (cache + gate)    │  │
│  └────────────┘ └──────────────┘ └──────────────┘ └───────────────────┘  │
└───┬───────────────┬──────────────────┬──────────────────┬────────────────┘
    │               │                  │                  │
┌───▼─────┐  ┌──────▼──────┐   ┌───────▼──────┐   ┌───────▼─────────┐
│ routing │  │  backends   │   │   session    │   │     tools       │
│ Router  │  │ Anthropic   │   │ JsonlStore   │   │ fs/bash/subagent│
│ Health  │  │ OpenAICompat│   │ fork refs    │   │ (all plugins)   │
└─────────┘  └─────────────┘   └──────────────┘   └─────────────────┘
                       all implement ports from conway-core
                                    ▲
                          ┌─────────┴──────────┐
                          │    conway-core     │
                          │ types + port traits│
                          └────────────────────┘
```

Ports defined in `conway-core`, implemented outside it: `Backend`, `Tool`/`Plugin`, `PermissionGate`, `SessionStore`, `Router`, `HealthRegistry`, `SubagentHost`.

---

## 3. Data Flow

### 3.1 Primary path — a prompt through a root agent

1. **Entry.** Consumer calls `SessionHandle::prompt(text)` (library), or CLI reads a line / stdin. All three consumption modes converge here — there is exactly one entry function.
2. **Persist.** `Runtime` appends `LogRecord::UserTurn{ provenance: Provenance::UserPrompt }` to the session log via `SessionStore::append`. Persist-before-act: the log is the source of truth, in-memory state is a cache.
3. **Assemble.** `ContextBuilder` materializes the agent's effective transcript (see §5) into an ordered `Vec<PromptSegment>`, each tagged with a `Provenance` and an optional `CacheHint`.
4. **Route.** `Router::resolve(RouteRequest{ role, required_caps, est_tokens })` returns an ordered `Vec<Route>`, filtered by `HealthRegistry` breaker state. A `Event::ModelDecision{ alias, chosen, reason }` is emitted **before** the call — this is what makes GP-07 ("which model ran this and why") answerable at all times.
5. **Generate.** `ToolCallStrategy` is selected from `Capabilities` (§4.1). If the turn may produce tool calls and the backend's tool-calling is `NonStreamingOnly`, the runtime calls `Backend::generate`; otherwise `Backend::stream`. Text deltas are emitted as `Event::TextDelta` in both cases (non-streaming emits one delta) — **the caller-facing stream contract never changes**.
6. **Parse.** Tool calls are validated against the registered `ToolSpec` JSON schema. Validation failure on a streamed response triggers exactly one non-streaming retry of the identical request (see Tension T-3).
7. **Permit.** For each tool call, `PermissionBroker` consults its per-session decision cache, then the embedder's `PermissionGate::check`. Emits `PermissionRequested` / `PermissionResolved`.
8. **Execute.** `ToolRunner` invokes tools concurrently (`JoinSet`, bounded by `max_parallel_tools`), each with a `ToolCtx` carrying a `CancellationToken`.
9. **Append + loop.** Tool results are appended as `LogRecord::ToolResult{ provenance: ToolResult{call_id, tool} }`. At the **turn boundary**, the mailbox is drained (steer messages become user turns). Loop to 3 until the model returns no tool calls, or a budget (`max_steps`, `deadline`, `max_tokens`) trips.
10. **Exit.** Text streams out through `EventStream`. One-shot mode maps terminal state to an exit code. `AgentResult` is appended and emitted.

### 3.2 Fork/spawn path

```
parent AgentLoop
  └─ model emits tool_call: conway_subagent{ mode:"fork"|"spawn", prompt, agent_def?, role? }
       │  (or: embedder calls SessionHandle::fork(agent_id, spec) directly — API-first, decision 2)
       ▼
  ToolCtx.subagents: SubagentHost::start(SubagentSpec)
       ├─ SessionStore::fork(parent_sid, at_seq=parent.head, mode) -> child_sid   [O(1)]
       ├─ AgentTree::attach(child) ; Mailbox pair created (parent↔child)
       ├─ Event::AgentSpawned{ kind, parent, inherited_upto: seq }
       └─ child AgentLoop runs §3.1 with ContextBuilder inheriting per mode
             ├─ Fork:  segments = [child SystemPrompt?] ++ INHERITED[parent 0..seq] ++ ForkDirective ++ own
             └─ Spawn: segments = [child SystemPrompt]  ++ Prompt ++ own
       ▼
  child terminates -> AgentResult -> parent mailbox
       └─ parent's pending tool_call resolves with the *structured result contract* (§6.4)
```

The `ToolCall` that spawned the child stays pending in the parent's log; a `LogRecord::ToolResult` carrying the serialized `AgentResult` closes it. If the child is orphaned/timed out, the supervisor synthesizes a `Failed` result — the parent's loop can never hang on a missing child (MAST "failure to recognize termination").

### 3.3 Egress

| Sink | Content |
|---|---|
| `EventStream` (broadcast) | Every `Event`, flat, `seq`-ordered per session, tagged with `agent_id`. |
| Session log (JSONL on disk) | Every `LogRecord`, one file per session under `.conway/sessions/<sid>.jsonl`. |
| stdout (one-shot) | `--output-format text` → assistant text only; `json` → single terminal `AgentResult`; `jsonl` → the `Event` stream verbatim. |
| exit code (one-shot) | See §6.7. |

---

## 4. Key Port Traits

All in `conway-core`. Signatures are the binding contract.

### 4.1 Backend

```rust
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    fn id(&self) -> BackendId;

    /// Capabilities are per (backend, model): quantization and chat template
    /// change tool-call reliability independent of the server. [research-backends]
    fn capabilities(&self, model: &ModelId) -> Capabilities;

    async fn generate(&self, req: GenerateRequest)
        -> Result<GenerateResponse, BackendError>;

    async fn stream(&self, req: GenerateRequest)
        -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError>;

    /// Cheap liveness/readiness probe. Distinct from transport errors.
    async fn probe(&self) -> Result<ProbeReport, BackendError>;
}

pub struct Capabilities {
    pub tool_calling: ToolCallSupport,
    pub cache: CacheMode,
    pub parallel_tool_calls: bool,
    pub structured_output: StructuredOutput,   // None | JsonSchema | Grammar
    pub max_context_tokens: u32,
    pub reasoning: bool,
    pub reliability_tier: ReliabilityTier,     // Verified | Community | Unknown
}

pub enum ToolCallSupport { None, NonStreamingOnly, Streaming { validated: bool } }

pub enum CacheMode {
    /// Anthropic: explicit addressable breakpoints.
    ExplicitBreakpoints { max_breakpoints: u8, ttls: &'static [CacheTtl] },
    /// OpenAI / vLLM / Ollama: passive prefix matching. Hints become ORDERING guarantees only.
    ImplicitPrefix { min_prefix_tokens: u32 },
    /// llama.cpp native slot save/restore (post-MVP adapter).
    SlotKv,
    None,
}

pub struct GenerateRequest {
    pub model: ModelId,
    pub segments: Vec<PromptSegment>,   // ORDERED; static-first is enforced by ContextBuilder
    pub tools: Vec<ToolSpec>,
    pub params: SamplingParams,
    pub prefix_key: Option<PrefixKey>,  // stable id of the shared prefix, for SlotKv backends
}

pub struct PromptSegment {
    pub id: SegmentId,
    pub role: Role,                  // System | User | Assistant | ToolResult
    pub content: Vec<ContentBlock>,
    pub provenance: Provenance,      // GP-10: every segment knows where it came from
    pub cache_hint: Option<CacheHint>,
}

pub struct CacheHint { pub breakpoint: bool, pub ttl: CacheTtl, pub prefix_key: PrefixKey }
```

**Cache hint contract (GP-06, never correctness-bearing):** an adapter MAY ignore `cache_hint` entirely. Mapping:

| CacheMode | Adapter behavior |
|---|---|
| `ExplicitBreakpoints` | Emit `cache_control:{type:"ephemeral", ttl}` on the last content block of each segment with `breakpoint:true`, capped at `max_breakpoints` (drop the earliest-value breakpoints first). |
| `ImplicitPrefix` | No-op on the wire. The *ordering* invariant from `ContextBuilder` (static→inherited→volatile) is what produces hits. |
| `SlotKv` | Use `prefix_key` as the slot lookup key; `/slots/<id>/restore` on hit, `save` after generation. Post-MVP; the trait already carries the field. |
| `None` | No-op. |

If a hint is dropped, output must be byte-for-byte the same request content. **Cache is an economics feature, never a semantics feature.**

### 4.2 Plugin / Tool

```rust
pub trait Plugin: Send + Sync + 'static {
    fn manifest(&self) -> PluginManifest;   // id, semver, provided tools, required host caps
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    fn on_init(&self, ctx: &PluginInitCtx) -> Result<(), PluginError> { Ok(()) }
}

#[async_trait]
pub trait Tool: Send + Sync + 'static {
    fn spec(&self) -> ToolSpec;      // name, description, JSON Schema, ToolCategory, PermissionClass
    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError>;
}

pub struct ToolCtx {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    pub events: EventSink,                       // progress reporting
    pub subagents: Arc<dyn SubagentHost>,        // cycle-breaker for the fork/spawn tool
    pub config: Arc<PluginConfig>,
}

pub struct ToolOutput {
    pub blocks: Vec<ContentBlock>,
    pub is_error: bool,
    /// Tool declares how it wants oversized output handled; runtime enforces and records it.
    pub truncation: TruncationPolicy,
    pub artifacts: Vec<Artifact>,
}

pub enum ToolCategory { Read, Edit, Delete, Move, Search, Execute, Think, Fetch, Delegate }
```

`ToolCategory` is intentionally aligned with ACP's tool-call categories — free future compatibility, zero present cost (research-acp).

**There is exactly one extension mechanism (GP-03).** Built-in read/write/edit/bash and the subagent tool are `Plugin` implementations registered by default in `ConwayBuilder`; nothing about them is privileged. MVP plugins are in-process `Arc<dyn Plugin>` (see Tension T-8).

### 4.3 PermissionGate

```rust
#[async_trait]
pub trait PermissionGate: Send + Sync + 'static {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision;
}

pub struct PermissionRequest {
    pub agent_id: AgentId,
    pub agent_path: Vec<AgentId>,      // root→…→requester; the IDE renders which subagent asked
    pub tool: ToolName,
    pub category: ToolCategory,
    pub arguments: serde_json::Value,
    pub rendered: String,              // human-readable one-liner from the tool
}

pub enum PermissionDecision {
    AllowOnce,
    AllowAlways { scope: PermissionScope },   // Session | Agent | AgentSubtree
    Deny { reason: String },
    /// Denied, but the reason is fed back to the model as a tool error so it can adapt.
    DenyWithFeedback { message: String },
}
```

The gate is **always** async and always implemented by the consumer. Built-in gates shipped in `conway`: `AllowListGate` (declarative, used by `-p`), `DenyAllGate`, `PromptingGate` (CLI). GP-08: no worktree/sandbox logic anywhere in the harness — agents get isolation by calling `bash` with `git worktree`.

### 4.4 SessionStore

```rust
#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    async fn create(&self, meta: SessionMeta) -> Result<SessionId, StoreError>;
    async fn append(&self, sid: &SessionId, rec: LogRecord) -> Result<LogSeq, StoreError>;
    async fn read(&self, sid: &SessionId, range: SeqRange) -> Result<Vec<LogRecord>, StoreError>;
    async fn head(&self, sid: &SessionId) -> Result<LogSeq, StoreError>;

    /// O(1): writes a child header referencing (parent, at). Copies NO records.
    async fn fork(&self, parent: &SessionId, at: LogSeq, meta: SessionMeta)
        -> Result<SessionId, StoreError>;

    async fn meta(&self, sid: &SessionId) -> Result<SessionMeta, StoreError>;
    async fn children(&self, sid: &SessionId) -> Result<Vec<SessionId>, StoreError>;
    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>, StoreError>;
}
```

MVP impl: `JsonlSessionStore` — one `.jsonl` per session, first line is the header. Debuggable with `jq`, greppable, diffable, and trivially inspectable by a human, per decision 9. An index file (`index.jsonl`) accelerates `list`/`children`; it is a derived cache and is rebuildable by scanning headers.

### 4.5 Router + Health

```rust
pub trait Router: Send + Sync {
    /// Ordered candidates. Never empty on success. Never consults request *content*. (GP-07)
    fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError>;
}

pub struct RouteRequest {
    pub role: RoleAlias,               // "planner" | "coder" | "fast" | user-defined
    pub pin: Option<ModelRef>,         // agent-def or API override; short-circuits the chain
    pub required: RequiredCaps,        // e.g. tool_calling >= NonStreamingOnly, min_context
    pub est_tokens: u32,
    pub agent_id: AgentId,
}

pub struct Route { pub backend: BackendId, pub model: ModelId,
                   pub params: SamplingParams, pub reason: RoutingReason }

pub enum RoutingReason {
    PinnedByApi, PinnedByAgentDef, AliasPrimary { alias: RoleAlias },
    Fallback { position: u8, after: Vec<AttemptFailure> },
    CapabilitySkip { skipped: ModelRef, missing: Vec<String> },
    HealthSkip { skipped: ModelRef, breaker: BreakerKind },
}

pub trait HealthRegistry: Send + Sync {
    fn state(&self, ep: &EndpointId) -> BreakerState;      // Closed | Open{until} | HalfOpen
    fn record(&self, ep: &EndpointId, obs: Observation);   // TransportError | Http5xx | Ok | ProbeFail
}
pub enum BreakerKind { Transport, Probe }   // two independent breakers, per Olla [research-routing]
```

Strict separation (decision 6): `Router` owns *policy* (which model should serve this role), `HealthRegistry` owns *state* (is this endpoint usable now). The router only reads breaker state as a filter. No classifiers, no embeddings, no request-content inspection — there is no code path in `conway-routing` that can see prompt text.

### 4.6 SubagentHost (the cycle-breaker)

```rust
#[async_trait]
pub trait SubagentHost: Send + Sync + 'static {
    async fn start(&self, parent: AgentId, spec: SubagentSpec) -> Result<AgentId, RuntimeError>;
    async fn steer(&self, target: AgentId, text: String) -> Result<(), RuntimeError>;
    async fn await_result(&self, target: AgentId) -> Result<AgentResult, RuntimeError>;
    async fn cancel(&self, target: AgentId, reason: String) -> Result<(), RuntimeError>;
    fn tree(&self) -> AgentTreeSnapshot;
}

pub struct SubagentSpec {
    pub mode: SubagentMode,               // Fork | Spawn  — decision 1, exactly two
    pub prompt: String,
    pub agent_def: Option<AgentDefRef>,   // system prompt / skill; required for Spawn
    pub role: Option<RoleAlias>,
    pub tools: Option<ToolSelector>,
    pub budget: Budget,                   // max_steps, deadline, max_tokens
    pub cache_hint: bool,                 // default true for Fork; best-effort only
    pub result_contract: Option<JsonSchema>,
}
pub enum SubagentMode { Fork, Spawn }
```

The developer API (`SessionHandle::fork/spawn`) and the `conway_subagent` tool both call this same trait. That is decision 2, mechanically enforced: the tool is a ~60-line wrapper with no privileged access.

---

## 5. Fork Mechanics (precise)

### 5.1 Log structure

```jsonc
// .conway/sessions/01J....jsonl  — line 0 is the header
{"kind":"header","session":"01J-child","agent":"a7","created":"...",
 "origin":{"parent":"01J-parent","at_seq":142,"mode":"fork"},
 "agent_def":"reviewer","role":"coder","budget":{"max_steps":40}}
{"seq":0,"kind":"fork_directive","text":"Now review the diff for races",
 "prov":{"type":"fork_directive","by":"a3"},"ts":"..."}
{"seq":1,"kind":"assistant","content":[...],"model":{"backend":"anthropic","model":"..."},
 "route_reason":{"AliasPrimary":{"alias":"coder"}},"usage":{...}}
{"seq":2,"kind":"tool_result","call_id":"tc_1","prov":{"type":"tool_result","tool":"read"},
 "truncated":{"policy":"head_tail","orig_bytes":918233}}
{"seq":3,"kind":"parent_steer","text":"skip the tests dir","prov":{"type":"parent_steer","from":"a3","parent_seq":150}}
```

- **Nothing is copied.** A fork writes exactly one line (the header). `SessionStore::fork` is O(1) regardless of parent transcript size — this is what makes tournament patterns (one fork → N spawned children) affordable.
- **Effective transcript** of an agent = `resolve(origin.parent, 0..origin.at_seq)` ++ own records, computed recursively up the ancestry chain. Cached in memory as `Arc<[LogRecord]>` per (session, at_seq) so N siblings forked at the same point share one allocation.
- **Immutability invariant:** records at `seq < at_seq` in a parent are frozen from the child's perspective. The parent continues appending at `seq >= at_seq`; those appends are invisible to the child. A fork is a snapshot, not a live view. Parent→child updates travel exclusively over the mailbox (§6).

### 5.2 What Fork vs Spawn inherits

| | Fork | Spawn |
|---|---|---|
| Ancestor transcript prefix | **Entire**, literal, verbatim, ordered first | none |
| System prompt | forker's, unless `agent_def` overrides | child's `agent_def` (required) |
| Tool schemas | forker's set ∩ `ToolSelector` | child's `agent_def` set |
| Additional prompt | `fork_directive`, appended after the prefix | the whole prompt |
| cwd, permission profile | inherited | inherited (isolation is a tool concern, GP-08) |
| Parent link (tree, mailbox, provenance) | yes | yes |
| Cache hint at boundary | on by default | n/a (no shared prefix) |

There is no partial-inheritance mode and none will be added. Context economy comes from tree shape: a lean parent forks cheaply; a specific task spawns clean (GP-01, decision 1).

### 5.3 Segment ordering and where cache hints attach

`ContextBuilder` emits segments in a **fixed order**, which is the entire mechanism by which implicit-prefix backends (vLLM/Ollama/OpenAI) get hits:

```
[0] SystemPrompt        prov=AgentDef{name}            ─┐
[1] SkillFragments      prov=Skill{name}                │ static; identical across siblings
[2] ToolSchemas         prov=ToolRegistry{hash}        ─┘  ◄── cache breakpoint A
[3] InheritedPrefix…    prov=Inherited{from,seq_range} ────  ◄── cache breakpoint B (fork boundary)
[4] ForkDirective|Prompt prov=ForkDirective|UserPrompt
[5..] own turns / tool results / parent steers          ─── volatile
```

Breakpoints go at the **end of [2]** and the **end of [3]**. Anthropic gets at most 4 `cache_control` markers, so the priority order when trimming is: B > A > everything else. `PrefixKey = blake3(model_id ‖ segments[0..=B])` and is stable across siblings — it is the slot key for a future llama.cpp adapter and the dedup key for the in-memory segment cache.

**Invariant:** if every cache hint is stripped, the assembled request bytes are identical. Caching changes cost and latency, never output (GP-06).

---

## 6. Parent–Child Messaging

### 6.1 Message types

```rust
pub enum AgentMessage {
    Steer   { from: AgentId, text: String, at_parent_seq: LogSeq },
    Cancel  { from: AgentId, reason: String, hard: bool },
    Progress{ from: AgentId, note: String },              // child→parent, EVENT only
    Result  { from: AgentId, result: AgentResult },       // child→parent, terminal
}
```

### 6.2 Queueing and delivery

- Each agent owns one bounded `mpsc` inbox (`capacity 64`). Overflow on `Steer` = oldest-dropped + `Event::SteerDropped` (never blocks the sender; a stuck child must not deadlock its parent).
- **`Steer` lands at the turn boundary only** (decision 3 — not mid-generation): the loop drains the inbox after the tool batch for turn N completes and before assembling turn N+1. Each drained steer becomes a `LogRecord::parent_steer` with provenance `ParentSteer{from, parent_seq}` and enters the context as a user-role segment.
- **`Cancel{hard:false}`** is also turn-boundary; **`hard:true`** trips the `CancellationToken` immediately, aborting in-flight tool futures and the backend request. A hard-cancelled child still emits `AgentResult{status: Cancelled}` — the parent's pending tool call always resolves.
- **`Progress`** never enters the parent's context. It is emitted as `Event::AgentProgress` for the IDE to render. This is deliberate: unsolicited child chatter in a parent's context is the "context clash" failure mode.
- **`Result`** resolves the parent's pending `conway_subagent` tool call. Exactly one per child, guaranteed by the supervisor.

### 6.3 Latency caveat (surfaced, not hidden)

Because steering is turn-boundary-bound, a child running a 10-minute `bash` call will not see a steer for 10 minutes. The runtime emits `Event::SteerQueued{ target, queued_since }` so the IDE can show "steer pending — child is in tool call." This is Tension T-5.

### 6.4 The result contract (MAST mitigations)

```rust
pub struct AgentResult {
    pub agent_id: AgentId,
    pub status: ResultStatus,        // Completed | Failed{err} | Cancelled | BudgetExceeded
                                     //   | Rejected{ missing: Vec<String> }
    pub summary: String,             // REQUIRED, bounded (default 2000 chars)
    pub facts: Vec<Fact>,            // typed k/v assertions the parent may re-inject verbatim
    pub artifacts: Vec<Artifact>,    // file paths, diffs, values — not prose
    pub structured: Option<serde_json::Value>,  // validated against result_contract if set
    pub transcript_ref: SessionId,   // full child log; inspectable, NEVER auto-injected
    pub usage: Usage,
    pub steps_taken: u32,
}
```

Targeted MAST failure modes:

| MAST failure | Mitigation |
|---|---|
| Losing conversation history | Fork inherits the literal prefix; nothing is summarized on the way down. Every context segment carries `Provenance` and is persisted. `transcript_ref` means a child's full history is never destroyed, only excluded from the parent's window. |
| Repeating steps | `StepDigest` ring per agent: `blake3(tool_name ‖ normalized_args)`. On the 3rd identical digest, emit `Event::RepeatedStep` and inject a system note listing the prior result's seq. Additionally, `facts` in a child's result give the parent re-injectable data instead of re-delegating. |
| Failing to recognize termination | Every child has a mandatory `Budget{max_steps, deadline}`. Supervisor synthesizes `BudgetExceeded` results. A parent tool call cannot hang. |
| Disobeying task/role spec | `result_contract: Option<JsonSchema>`; a child returning non-conforming `structured` output gets one retry with the validation error, then `Rejected{missing}`. |
| Inter-agent misalignment | `Rejected{missing}` is a first-class status: a child may refuse and enumerate what it needed but wasn't given, instead of hallucinating forward. |

### 6.5 Event stream (the IDE's render surface)

```rust
pub struct Envelope { pub seq: u64, pub ts: DateTime<Utc>,
                      pub session: SessionId, pub agent: AgentId, pub event: Event }

pub enum Event {
    AgentSpawned { kind: SubagentMode, parent: Option<AgentId>, agent_def: Option<String>,
                   inherited_upto: Option<LogSeq> },
    AgentProgress { note: String },
    AgentFinished { result: AgentResult },

    TurnStarted { turn: u32 },
    ModelDecision { role: RoleAlias, chosen: ModelRef, reason: RoutingReason, attempt: u8 },
    TextDelta { text: String },
    ThinkingDelta { text: String },
    TurnFinished { usage: Usage, stop: StopReason },

    ToolCallProposed { call_id: String, tool: ToolName, args: serde_json::Value },
    PermissionRequested { call_id: String, rendered: String },
    PermissionResolved  { call_id: String, decision: PermissionDecisionKind },
    ToolCallStarted { call_id: String },
    ToolProgress { call_id: String, note: String },
    ToolCallFinished { call_id: String, is_error: bool, preview: String },

    ContextSegmentAdded { segment: SegmentId, provenance: Provenance, tokens_est: u32 },
    MessageSent { to: AgentId, kind: MessageKind },
    SteerQueued { target: AgentId },
    RepeatedStep { tool: ToolName, prior_seq: LogSeq },
    BackendDegraded { endpoint: EndpointId, breaker: BreakerKind, until: DateTime<Utc> },
    Error { error: ConwayError, fatal: bool },
}
```

**Flat and `agent`-tagged, not nested.** A future ACP shim (research-acp: ACP's session model is flat, `session/fork` is an unmerged draft) filters `agent == root` for one ACP session, maps `TextDelta`→`session/update`, `PermissionRequested`→`session/request_permission`, `ToolCall*`→ACP tool-call updates, and surfaces each child as an independent ACP session with no linkage. Nothing in this enum precludes that. No ACP code is written now.

---

## 7. Module Specifications

---

## Module: conway-core

### Scope
The domain model and every port trait. Owns all types that cross a module boundary: messages, content blocks, tool specs, events, provenance, IDs, errors, and configuration *types* (not loading).

**Not responsible for:** any I/O, any HTTP client, config file discovery/parsing, any concrete implementation of the traits it defines (except `#[cfg(feature="fakes")]` test doubles).

### Provides
- `Message`, `ContentBlock`, `Role`, `ToolCall`, `ToolResult`, `LogRecord`, `LogSeq`, `SeqRange`
- `SessionId`, `AgentId`, `SegmentId`, `ModelId`, `BackendId`, `EndpointId`, `RoleAlias`
- `Provenance` — the GP-10 enum: `UserPrompt | AgentDef{name} | Skill{name} | ToolRegistry{hash} | Inherited{from: SessionId, seq_range} | ForkDirective{by} | ParentSteer{from, parent_seq} | ToolResult{call_id, tool} | SystemNote{reason}`
- `PromptSegment`, `CacheHint`, `PrefixKey`, `TruncationPolicy`
- **Ports:** `Backend`, `Plugin`, `Tool`, `PermissionGate`, `SessionStore`, `Router`, `HealthRegistry`, `SubagentHost`, `EventSink`
- `Capabilities`, `ToolCallSupport`, `CacheMode`, `ReliabilityTier`, `RequiredCaps`
- `Event`, `Envelope`, `AgentResult`, `ResultStatus`, `Fact`, `Artifact`, `Usage`, `Budget`
- `AgentDef`, `SkillDef`, `RoutingConfig`, `BackendConfig`, `ToolSelector` (serde types)
- `ConwayError` + per-port error enums (`BackendError`, `ToolError`, `StoreError`, `RoutingError`, `RuntimeError`)
- `#[cfg(feature="fakes")] FakeBackend`, `FakeStore`, `FakeGate`, `ScriptedBackend`

### Requires
Nothing from the workspace. External only: `serde`, `serde_json`, `thiserror`, `async-trait`, `futures-core`, `chrono`, `blake3`, `schemars`.

### Boundary Rules
- MUST NOT depend on any other workspace crate. Enforced by a CI check on `cargo tree`.
- MUST NOT depend on `reqwest`, `tokio::net`, `tokio::fs`, or any process-spawning crate.
- Every public type is `serde::Serialize + Deserialize` (needed for JSONL persistence and `--output-format jsonl`).
- Every public type is `#[non_exhaustive]` where forward-compatibility matters (C-04 OSS: enum growth must not be a breaking change).
- Semver discipline is strictest here — this is the plugin-author-facing crate.

### Internal Design Notes
IDs are ULIDs (sortable, timestamped, human-pasteable). `LogSeq` is a `u64` monotonic per session. `Provenance` is deliberately an enum, not a string map — the IDE renders a typed provenance tree, and adding a variant is a deliberate act.

---

## Module: conway-backends

### Scope
Concrete `Backend` implementations: `AnthropicBackend` (native Messages API, API key only) and `OpenAiCompatBackend` (one adapter covering Ollama local+cloud, llama.cpp server, LM Studio, vLLM, OpenAI). Owns wire-format translation, tool-call parsing (streaming and non-streaming), cache-hint mapping, and capability declaration.

**Not responsible for:** deciding *which* backend to use (routing), retry policy across backends (runtime), breaker state (routing), or the agent loop.

### Provides
- `AnthropicBackend::new(AnthropicConfig) -> impl Backend` — feature `anthropic`
- `OpenAiCompatBackend::new(OpenAiCompatConfig) -> impl Backend` — feature `openai-compat`
- `CapabilityProbe` — startup-time capability discovery (query `/v1/models`, `/props`, model-metadata file) producing `Capabilities` per model
- `ToolCallAccumulator` — backend-specific streamed-delta accumulation with validation, exposed for testing
- `ModelMetadata` loader — local file, optionally models.dev-derived; never a hard network dependency

### Requires
- `conway-core` — `Backend`, `GenerateRequest/Response`, `Capabilities`, `CacheHint`, `BackendError`

### Boundary Rules
- MUST declare `Capabilities` per `(backend, model)`, never per backend alone. Tool-call correctness varies by model+template+quantization, not server (research-backends).
- MUST NOT silently degrade: if a `cache_hint` cannot be honored, drop it and emit nothing — but the request content bytes must be unchanged.
- MUST NOT retry across backends. A single `generate`/`stream` call targets one endpoint. Transport-level retry (connection reset, 429 with `Retry-After`) is permitted at most twice with jitter; anything else surfaces as `BackendError` for the runtime/health layer to interpret.
- MUST classify errors into `BackendError::{Transport, RateLimit{retry_after}, Auth, BadRequest, ServerError, ContextOverflow, ToolParse, Cancelled}` — the health layer distinguishes `Transport`/`ServerError` (breaker-tripping) from `BadRequest`/`Auth` (not breaker-tripping; the endpoint is fine, the request isn't).
- MUST NOT implement Anthropic subscription OAuth. `sk-ant-oat*` tokens are rejected at config-parse time with an explanatory error (C-02, GP-09, research-routing: contractually prohibited and technically blocked since Feb 2026).
- The adapter trait MUST accommodate a future llama.cpp native slot adapter without signature change — satisfied by `prefix_key` on the request and `CacheMode::SlotKv`.

### Internal Design Notes
`OpenAiCompatBackend` carries a `dialect: Dialect` field (`OpenAi | Ollama | VllmHermes | LmStudio | LlamaCppServer`) selecting the `ToolCallAccumulator` and known-quirk workarounds. This keeps it one adapter, not five, while honoring "not a lowest common denominator."

Streaming tool-call parse is the known-buggy path across Ollama (#12557), vLLM hermes (#31871), and LM Studio-adjacent clients (#7517). The accumulator validates on `finish_reason`; a failure returns `BackendError::ToolParse` and the runtime falls back to non-streaming (decision 10). Per-model config `stream_tools = false` skips the streaming attempt entirely.

---

## Module: conway-routing

### Scope
Two cleanly separated concerns in one crate: (a) `DeclarativeRouter` — resolves a `RoleAlias` to an ordered candidate list from static config; (b) `BreakerRegistry` — per-endpoint circuit breakers and health probing.

**Not responsible for:** making the backend call, classifying prompts, cost estimation-based selection, or any content inspection.

### Provides
- `DeclarativeRouter::new(RoutingConfig, Arc<dyn HealthRegistry>, CapabilityIndex) -> impl Router`
- `BreakerRegistry::new(HealthConfig) -> Arc<dyn HealthRegistry>` with two independent breakers per endpoint (`Transport`, `Probe`), configurable threshold/open-duration/half-open policy
- `HealthProber::spawn(Vec<Arc<dyn Backend>>, HealthConfig) -> ProberHandle` — periodic `Backend::probe`
- `CapabilityIndex` — `(backend, model) -> Capabilities`, built at startup, consulted for `RequiredCaps` filtering
- `RoutingExplain::explain(&RouteRequest) -> ExplainReport` — the "why did this model run" answer, including skipped candidates and reasons

### Requires
- `conway-core` — `Router`, `HealthRegistry`, `RouteRequest`, `Route`, `RoutingReason`, `RoutingConfig`, `Capabilities`

### Boundary Rules
- `resolve()` MUST be pure with respect to request *content*. `RouteRequest` deliberately has no field carrying prompt text (GP-07). This is a compile-time guarantee, not a convention.
- `resolve()` MUST be synchronous and allocation-cheap; it is called on every turn of every agent.
- MUST return `RoutingReason` for the chosen route AND for every skipped candidate. A route with no explanation is a bug.
- Router MUST NOT mutate breaker state. Only the runtime, after an actual attempt, calls `HealthRegistry::record`.
- Breaker MUST distinguish transport failures from probe failures (Olla pattern, research-routing) — a slow-but-alive local server and a dead one are different states.
- No classifier, embedding model, or learned component may be linked into this crate. MVP or ever, absent an explicit decision reversal.

### Internal Design Notes
Config shape:

```toml
[roles.planner]
chain = [ "anthropic/claude-sonnet-4-6", "ollama-cloud/glm-5.2", "local/qwen3-coder-80b" ]
[roles.fast]
chain = [ "local/qwen3-coder-80b", "anthropic/claude-haiku-4-5" ]
[health]
transport_failures_to_open = 3
open_duration = "30s"
probe_interval = "15s"
probe_timeout = "2s"
```

Fallback is exercised by the runtime: `for route in router.resolve(req)? { match attempt(route) { Ok => break, Err(e) if e.is_failover_worthy() => { health.record(...); continue }, Err(e) => return Err(e) } }`. `BadRequest`/`ContextOverflow` are *not* failover-worthy — retrying a too-long prompt on a smaller model is worse than failing (see Tension T-2).

---

## Module: conway-session

### Scope
Persistence. The `JsonlSessionStore` implementation, log record serialization, ancestry resolution, fork-by-reference, resume, and the session index.

**Not responsible for:** deciding *what* to persist (runtime), context assembly, or in-memory agent state.

### Provides
- `JsonlSessionStore::open(root: PathBuf) -> Result<impl SessionStore>`
- `TranscriptResolver::resolve(&store, sid) -> Result<Arc<[LogRecord]>>` — walks the ancestry chain, applies `origin.at_seq` truncation, returns a shared slice. Memoized per `(sid, at_seq)`.
- `SessionIndex` — derived, rebuildable, accelerates `list`/`children`/tree reconstruction
- `SessionMeta { id, agent_id, origin: Option<ForkOrigin>, agent_def, role, created, cwd, labels, status }`
- `ForkOrigin { parent: SessionId, at_seq: LogSeq, mode: SubagentMode }`
- `provenance::ContextReport { segments: Vec<(SegmentId, Provenance, tokens_est)> }` — persisted alongside each turn so provenance survives process restart (decision 9)

### Requires
- `conway-core` — `SessionStore`, `LogRecord`, `SessionMeta`, `Provenance`, `StoreError`

### Boundary Rules
- Append-only. No record is ever mutated or deleted in place. Compaction, if ever added, writes a new file.
- `fork()` MUST NOT copy records. It writes one header line. Cost is O(1) in parent transcript size.
- One file per session ⇒ no cross-session write contention; N siblings forked from one parent write to N distinct files with no lock.
- Every append MUST be durable before the corresponding side effect is externally visible (persist-before-act). Fsync policy is configurable (`always | interval | never`); default `interval(200ms)` with `always` for headers and `AgentResult`.
- A partially-written trailing line MUST be tolerated on read (truncate-and-warn), never a hard failure. Crash recovery matters more than strictness.
- The store MUST NOT interpret record semantics beyond `kind` and `seq`.

### Internal Design Notes
JSONL over SQLite (OpenCode uses SQLite; we deliberately don't): decision 9 explicitly favors debuggable file-based storage. The properties that matter here — human inspection with `jq`/`grep`, one-file-per-session concurrency, O(1) fork, and trivially diffable/attachable bug reports for an OSS project — outweigh query performance we don't need. The `SessionStore` trait is the seam; a SQLite store is a drop-in later if `list` over 10k sessions becomes painful.

Ancestry resolution is memoized by `(SessionId, LogSeq)` in a bounded LRU of `Arc<[LogRecord]>`. Ten siblings forked at the same point share one allocation — this is the in-memory counterpart to the on-disk O(1) fork.

---

## Module: conway-tools

### Scope
The built-in plugins, each implementing the same `Plugin`/`Tool` traits available to third parties: `fs` (read/write/edit/glob/grep), `shell` (bash), `subagent` (fork/spawn/steer/await), `report` (structured result emission).

**Not responsible for:** permission decisions (it declares a `PermissionClass`, the broker decides), sandboxing/isolation (GP-08 — an agent achieves isolation by calling `bash` with `git worktree`), or MCP (a future plugin, not core).

### Provides
- `FsPlugin` — `read`, `write`, `edit`, `glob`, `grep`. Categories `Read`/`Edit`.
- `ShellPlugin` — `bash`. Category `Execute`. Streaming output via `ToolCtx::events`; `TruncationPolicy::HeadTail` default.
- `SubagentPlugin` — `conway_subagent{mode, prompt, agent_def?, role?, budget?}`, `conway_steer{agent_id, text}`, `conway_await{agent_id}`, `conway_cancel{agent_id}`. Category `Delegate`. Pure wrapper over `ToolCtx::subagents: Arc<dyn SubagentHost>`.
- `ReportPlugin` — `report{summary, facts, artifacts, structured}` — lets an agent explicitly finalize its `AgentResult` rather than having the runtime infer it from trailing text.
- `builtin_plugins() -> Vec<Arc<dyn Plugin>>`

### Requires
- `conway-core` — `Plugin`, `Tool`, `ToolCtx`, `ToolOutput`, `SubagentHost`, `ToolSpec`

### Boundary Rules
- MUST NOT depend on `conway-runtime`. All runtime interaction goes through `ToolCtx` ports. This is the constraint that makes GP-03 real rather than aspirational.
- Built-in tools get **no** privileged API. If a built-in needs a capability, that capability is added to `ToolCtx` and is thereby available to every third-party plugin.
- Every tool MUST honor `ToolCtx::cancel` cooperatively; `bash` additionally kills its process group.
- Every tool MUST declare `TruncationPolicy` for its output. The runtime enforces it and records the truncation in the log — a truncated tool result is a context-affecting event and must be visible in provenance (GP-10).
- Tools MUST NOT write to the session log directly.
- `SubagentPlugin` MUST NOT contain fork/spawn logic. If it does, decision 2 (API-first) has been violated.

### Internal Design Notes
`conway_subagent` is blocking-by-default (returns the child's `AgentResult` as its tool result) with an `await: false` option that returns an `agent_id` immediately for fan-out patterns. Tournament/adversarial composites are then: one fork issues N `conway_subagent{mode:"spawn", await:false}` calls with differing prompts, then N `conway_await` calls, then aggregates into its own `report`. No new primitive (decision 1).

---

## Module: conway-runtime

### Scope
The engine. Agent loop, agent tree + supervisor, mailboxes, context assembly, provenance tracking, plugin registry, permission brokering, tool execution, backend attempt/fallback sequencing, event bus, budgets, and the `SubagentHost` implementation.

**Not responsible for:** config file discovery/parsing (facade), CLI/TUI, wire formats, routing policy, or storage format.

### Provides
- `Runtime::new(RuntimeDeps) -> Arc<Runtime>` where `RuntimeDeps { store, router, health, backends, plugins, gate, agent_defs, event_bus }` — all ports, all injectable, all fakeable
- `Runtime::start_root(RootSpec) -> Result<AgentId>`
- `impl SubagentHost for Runtime`
- `Runtime::prompt(agent, text)`, `::steer(agent, text)`, `::cancel(agent, reason)`
- `Runtime::tree() -> AgentTreeSnapshot`
- `Runtime::context_report(agent) -> ContextReport` — GP-10 inspection API: every segment, its provenance, its token estimate
- `Runtime::subscribe() -> EventStream` (broadcast, lossy-with-notice for slow consumers)
- `ContextBuilder` (crate-internal, unit-tested against golden files)
- `PermissionBroker` — decision cache layered above the consumer's `PermissionGate`

### Requires
- `conway-core` (all ports/types) · `conway-routing` (`DeclarativeRouter`, `BreakerRegistry`) · `conway-session` (`TranscriptResolver`) · `conway-backends` (only as `Arc<dyn Backend>` injected by the facade — the runtime does not name concrete adapters) · `conway-tools` (only as `Arc<dyn Plugin>` injected)

### Boundary Rules
- One `AgentLoop` per agent, one tokio task per agent. No shared mutable agent state; all cross-agent interaction is via mailbox or `AgentTree` (which holds only metadata + handles under an `RwLock`).
- Persist-before-act: a `LogRecord` is durable before its effect is externally observable.
- Steer messages land **only** at turn boundaries (decision 3). No code path may inject into a context mid-generation.
- Every backend attempt MUST emit `Event::ModelDecision` before the request, including on fallback retries (`attempt` increments). GP-07 is unconditional.
- Every context segment MUST carry a `Provenance`. Constructing a `PromptSegment` without one is impossible by type (`Provenance` is a non-`Default` required field).
- `ContextBuilder` MUST emit segments in the fixed order of §5.3. Ordering is the implicit-prefix cache mechanism and is load-bearing for cost, never for correctness.
- Every child agent MUST have a `Budget`. The supervisor MUST synthesize a terminal `AgentResult` on budget exhaustion, cancellation, or panic. A parent's pending subagent tool call must always resolve.
- The runtime MUST NOT construct concrete backends, stores, or plugins. Everything arrives via `RuntimeDeps`.

### Internal Design Notes

**Agent loop skeleton:**
```
loop {
  drain_mailbox_to_context();                       // steers become user turns
  if budget.exhausted() { finish(BudgetExceeded); break }
  let segments = context_builder.build(agent)?;     // fixed order, provenance-tagged
  let routes   = router.resolve(&route_req(agent, &segments))?;
  let resp = attempt_with_fallback(routes, segments).await?;   // records health observations
  append(assistant_record(&resp));
  if resp.tool_calls.is_empty() { finish(Completed{from_text_or_report}); break }
  let permitted = permission_broker.filter(resp.tool_calls).await;
  let results   = tool_runner.run_concurrent(permitted).await;  // JoinSet, bounded
  append_all(results);
  step_digest.observe(&results);                     // repeated-step detection
}
```

**Streaming/tool-call strategy resolution** (decision 10):

| `Capabilities.tool_calling` | tools registered? | strategy |
|---|---|---|
| `Streaming{validated:true}` | yes | `stream()`; on `ToolParse` error → one non-streaming retry |
| `Streaming{validated:false}` or `NonStreamingOnly` | yes | `generate()` directly; text emitted as one `TextDelta` |
| any | no | `stream()` always — pure text turns are safe everywhere |

The caller's `EventStream` contract is identical in all cases. Text streaming to the *user* is preserved for text-only turns, which is the majority of visible output.

**Provenance tracking** is not an afterthought: `ContextBuilder::build` returns `(Vec<PromptSegment>, ContextReport)` and the report is persisted with the turn. `Runtime::context_report` reads it back, so provenance survives restart and is answerable for historical turns, not just live ones.

---

## Module: conway (facade)

### Scope
The public library API and everything about *assembling* a runtime: config discovery/parsing/merging, agent & skill definition loading, backend construction from config, default plugin registration, and the ergonomic `SessionHandle` surface the Tauri IDE consumes.

**Not responsible for:** any agent logic. This crate is wiring plus ergonomics.

### Provides
```rust
pub struct ConwayBuilder { /* … */ }
impl ConwayBuilder {
    pub fn from_config(path: impl AsRef<Path>) -> Result<Self>;
    pub fn discover() -> Result<Self>;                 // cwd → .conway/ → XDG → env
    pub fn with_backend(self, b: Arc<dyn Backend>) -> Self;
    pub fn with_plugin(self, p: Arc<dyn Plugin>) -> Self;
    pub fn with_permission_gate(self, g: Arc<dyn PermissionGate>) -> Self;
    pub fn with_session_store(self, s: Arc<dyn SessionStore>) -> Self;
    pub fn with_router(self, r: Arc<dyn Router>) -> Self;
    pub fn build(self) -> Result<Conway>;
}

pub struct Conway { /* Arc<Runtime> */ }
impl Conway {
    pub async fn new_session(&self, spec: SessionSpec) -> Result<SessionHandle>;
    pub async fn resume(&self, sid: SessionId) -> Result<SessionHandle>;
    pub async fn sessions(&self, f: SessionFilter) -> Result<Vec<SessionMeta>>;
    pub fn explain_routing(&self, role: &RoleAlias) -> ExplainReport;
}

pub struct SessionHandle { /* … */ }
impl SessionHandle {
    pub fn id(&self) -> SessionId;
    pub fn root(&self) -> AgentId;
    pub async fn prompt(&self, text: impl Into<String>) -> Result<TurnHandle>;
    pub fn events(&self) -> EventStream;                      // broadcast, from-now or from-seq
    // subagent primitives — API-first (decision 2)
    pub async fn fork (&self, from: AgentId, spec: ForkSpec)  -> Result<AgentId>;
    pub async fn spawn(&self, from: AgentId, spec: SpawnSpec) -> Result<AgentId>;
    pub async fn steer(&self, target: AgentId, text: impl Into<String>) -> Result<()>;
    pub async fn await_agent(&self, target: AgentId) -> Result<AgentResult>;
    pub async fn cancel(&self, target: AgentId, reason: &str) -> Result<()>;
    // introspection — GP-10
    pub fn tree(&self) -> AgentTreeSnapshot;
    pub async fn context_report(&self, agent: AgentId) -> Result<ContextReport>;
    pub async fn transcript(&self, agent: AgentId) -> Result<Vec<LogRecord>>;
}
```
Also: `config::{ConwayConfig, load, merge}`, `agents::load_agent_defs` (markdown + YAML frontmatter, `.conway/agents/*.md`), `gates::{AllowListGate, DenyAllGate}`, `presets::builtin_plugins`.

### Requires
- `conway-runtime`, `conway-core`, `conway-backends` (feature-gated), `conway-session`, `conway-routing`, `conway-tools`

### Boundary Rules
- This crate defines the **only** stable public API. `conway-runtime` internals are not re-exported.
- Every capability reachable from the CLI MUST be reachable here first (C-03, GP-05). A CLI-only feature is a bug.
- Config loading MUST NOT require network access. Model metadata is a local file with an optional, explicitly-invoked refresh (models.dev is a convenience source, not a dependency — research-routing).
- MUST reject Anthropic OAuth tokens at config parse time with an explanatory error naming the reason (C-02, GP-09).
- Feature flags: `default = ["anthropic","openai-compat","builtin-tools","jsonl-store"]`. Each backend independently disableable so an embedder can ship a minimal binary.

### Internal Design Notes
Config precedence: CLI flags > env (`CONWAY_*`) > `./.conway/conway.toml` > `$XDG_CONFIG_HOME/conway/conway.toml` > defaults. Agent definitions are markdown-with-frontmatter (matching what Claude Code/OpenCode users already know) — frontmatter carries `name`, `role`, `tools`, `model`, `max_steps`, `result_contract`; the body is the system prompt (decision 5).

---

## Module: conway-cli

### Scope
The `conway` binary: interactive REPL/TUI mode and one-shot streaming mode (`-p`). A thin presentation layer over `SessionHandle`.

**Not responsible for:** anything not purely presentational. If the CLI needs a capability, it is added to `conway` first.

### Provides
- `conway` (interactive): REPL with agent-tree pane, streamed text, permission prompts, `/steer <agent> <text>`, `/tree`, `/context <agent>`, `/why` (last routing decision), `/fork`, `/spawn`, `/resume <sid>`
- `conway -p` (one-shot): prompt from argv or stdin; `--output-format text|json|jsonl`; `--allowed-tools`, `--deny-tools`, `--permission-mode {allowlist,deny}`; `--role-override`, `--model`; `--session`, `--resume`, `--fork-from`
- `conway sessions list|show|tree|export`
- `conway routes explain <role>` — prints the resolved chain, capability filters, and current breaker state

### Requires
- `conway` facade only. MUST NOT depend on `conway-runtime`, `conway-backends`, `conway-session`, or `conway-routing` directly.

### Boundary Rules
- `-p` MUST NOT prompt interactively. Permissions come from `AllowListGate` built from flags; an unlisted tool call yields `DenyWithFeedback` and is reported in the result. (Claude Code's `-p` has this same constraint; we make the denial *visible in structured output* rather than a silent failure.)
- `-p` MUST stream to stdout as tokens arrive under `text` and `jsonl` formats. Buffering the whole response is a bug (this is the explicit `claude -p` replacement requirement).
- **Exit codes** (stable contract, C-04):
  | code | meaning |
  |---|---|
  | 0 | agent completed |
  | 1 | agent failed (`ResultStatus::Failed`) |
  | 2 | usage/config error |
  | 3 | terminated by permission denial |
  | 4 | no healthy backend after exhausting the fallback chain |
  | 5 | budget exceeded (steps/deadline/tokens) |
  | 130 | interrupted (SIGINT) |
- stdout carries **only** program output. All diagnostics go to stderr. `jsonl` output is one `Envelope` per line, valid JSON, no ANSI.
- SIGINT: first = graceful cancel (soft, drains to a terminal `AgentResult`, exit 130); second = immediate abort.

### Internal Design Notes
Both modes construct the identical `SessionHandle` and consume the identical `EventStream`; they differ only in the renderer. Interactive mode uses `ratatui`. The one-shot renderer is ~200 lines. If a behavior differs between modes, the divergence is in the renderer and is a bug.

---

## 8. Interface Contracts (boundary crossings)

```rust
// conway-runtime -> conway-backends
// Every field is producer-owned; adapters may reorder nothing.
struct GenerateRequest {
    model: ModelId,
    segments: Vec<PromptSegment>,   // order is load-bearing for implicit prefix caching
    tools: Vec<ToolSpec>,
    params: SamplingParams,
    prefix_key: Option<PrefixKey>,
}
struct GenerateResponse {
    content: Vec<ContentBlock>,     // Text | Thinking | ToolUse
    tool_calls: Vec<ToolCall>,      // already validated against ToolSpec schemas
    stop: StopReason,               // EndTurn | ToolUse | MaxTokens | StopSequence | Refusal
    usage: Usage,                   // incl. cache_read_tokens / cache_write_tokens when reported
}
enum StreamChunk { TextDelta(String), ThinkingDelta(String),
                   ToolCallDelta{index:u32, raw:String}, Done(GenerateResponse) }
```

```rust
// conway-runtime -> conway-routing
fn resolve(&RouteRequest) -> Result<Vec<Route>, RoutingError>;
// PRE:  RouteRequest carries no prompt content.
// POST: candidates are health-filtered and capability-filtered, ordered, each with a RoutingReason.
// POST: Err(NoCandidate{ role, considered: Vec<(ModelRef, RoutingReason)> }) enumerates every rejection.

// conway-runtime -> conway-routing (state, after each attempt)
fn record(&self, ep: &EndpointId, obs: Observation);
// Observation::{Ok{latency}, TransportError, ServerError, ProbeFail, RateLimited{retry_after}}
// BadRequest / Auth / ContextOverflow are NOT reported — they are not endpoint health signals.
```

```rust
// conway-runtime -> conway-session
async fn fork(&self, parent: &SessionId, at: LogSeq, meta: SessionMeta) -> Result<SessionId>;
// POST: exactly one header line written; zero records copied; O(1) in parent transcript size.
// POST: meta.origin == Some(ForkOrigin{ parent, at_seq: at, mode })

fn resolve(&store, sid) -> Result<Arc<[LogRecord]>>;
// POST: == concat(resolve(parent)[0..at_seq], own_records) transitively; shared across siblings.
```

```rust
// conway-runtime -> conway-tools  (and any third-party plugin)
async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError>;
// PRE:  call.arguments validated against self.spec().schema
// PRE:  permission already granted for (agent, tool, arguments)
// POST: honors ctx.cancel; returns within deadline or Err(ToolError::Cancelled)
// POST: declares TruncationPolicy; runtime applies it and records the truncation in the log
```

```rust
// conway-tools -> conway-runtime  (inverted, via port — this is the cycle-breaker)
trait SubagentHost {
    async fn start(&self, parent: AgentId, spec: SubagentSpec) -> Result<AgentId>;
    async fn steer(&self, target: AgentId, text: String) -> Result<()>;
    async fn await_result(&self, target: AgentId) -> Result<AgentResult>;
    async fn cancel(&self, target: AgentId, reason: String) -> Result<()>;
    fn tree(&self) -> AgentTreeSnapshot;
}
// INVARIANT: await_result always terminates — supervisor synthesizes a result on
//            budget exhaustion, cancellation, or task panic.
```

```rust
// conway-runtime -> consumer (embedder / CLI)
async fn check(&self, req: PermissionRequest) -> PermissionDecision;
// PRE:  req.agent_path is the full root→requester chain (IDE renders which subagent asked)
// POST: gate may block indefinitely; runtime holds the tool call pending and emits
//       PermissionRequested. Gate cancellation surfaces as Deny{"cancelled"}.

fn subscribe() -> EventStream;   // Stream<Item = Envelope>
// GUARANTEE: seq is monotonic per session across ALL agents in that session's tree.
// GUARANTEE: an agent's AgentSpawned precedes every other event bearing that agent id.
// GUARANTEE: every AgentSpawned is eventually followed by exactly one AgentFinished.
// NOTE: broadcast channel; a slow consumer receives Event::Lagged{skipped} rather than
//       stalling the runtime. Full history is always recoverable from the session log.
```

---

## 9. Execution Order

C-05 requires the CLI to be runnable as early as possible so every slice is hand-testable. Slice 1 therefore prioritizes an end-to-end vertical over any horizontal completeness.

### Parallel Groups

**Group 0 — foundation (blocks everything)**
- `conway-core`: types, all port traits, `Provenance`, `Event`, fakes.
Small, mostly declarative, one focused effort. Everything downstream compiles against it, so it must land first and change rarely.

**Group 1 — parallel after Group 0**
| Track | Work |
|---|---|
| A | `conway-session`: `JsonlSessionStore`, `TranscriptResolver`, fork-by-reference, index |
| B | `conway-backends`: `OpenAiCompatBackend` (Ollama dialect first — free, local, fast to iterate), `CapabilityProbe` |
| C | `conway-routing`: `DeclarativeRouter` + `BreakerRegistry` (config-driven, fully unit-testable against fakes) |
| D | `conway-tools`: `FsPlugin` + `ShellPlugin` (no `SubagentPlugin` yet) |
| E | `conway-runtime` skeleton: single-agent loop, `ContextBuilder`, `EventBus`, `PermissionBroker` — developed against `conway-core` fakes, no real backend needed |

**Slice 1 milestone (integration of Group 1):** `conway -p "list the files"` works against local Ollama with fs+bash tools, streams to stdout, persists a JSONL session, exits with a correct code, and `conway sessions show` reads it back. **Zero subagent code exists yet.** This is the hand-testable spine required by C-05.

**Group 2 — parallel after Slice 1**
| Track | Work |
|---|---|
| F | `AnthropicBackend` + `CacheMode::ExplicitBreakpoints` mapping + cache-hint plumbing |
| G | Fork/spawn: `SubagentHost` impl, `AgentTree`, supervisor, budgets, `SubagentPlugin`, `SessionHandle::fork/spawn` |
| H | Interactive CLI (`ratatui`): tree pane, permission prompts, `/why`, `/context` |
| I | Agent/skill definitions: markdown+frontmatter loader, per-agent tool selectors, per-agent role pinning |

**Group 3 — after G**
| Track | Work |
|---|---|
| J | Mailboxes + steering + `AgentResult` contract + MAST mitigations (`StepDigest`, `Rejected{missing}`, budget-synthesized results) |
| K | Fork/resume: `conway --resume`, `--fork-from <sid>@<seq>`, tree reconstruction from the index |
| L | Provenance inspection API + `/context` rendering + `context_report` persistence |
| M | Fallback exercise loop: multi-attempt with health recording, `Event::BackendDegraded`, `conway routes explain` |

**Group 4 — hardening / OSS-readiness (C-04)**
- Golden-file tests for `ContextBuilder` (fork ordering, cache-hint placement, provenance completeness)
- Backend conformance suite runnable against any adapter (vLLM, LM Studio, llama.cpp, OpenAI) — the artifact that makes third-party adapters possible
- Plugin-author documentation + example third-party plugin crate
- Public API review, `#[non_exhaustive]` audit, semver policy, LICENSE/CONTRIBUTING

### Sequential Dependencies

| Must precede | Because |
|---|---|
| `conway-core` → everything | All ports and types originate there. |
| `conway-session` fork semantics → G (fork/spawn) | Fork mechanics are a storage property first; O(1) fork must exist before the runtime relies on it. |
| E (`ContextBuilder`) → G | Fork inheritance *is* context assembly with an `Inherited` segment; building it twice would diverge. |
| G (`SubagentHost` + tree) → J (messaging) | Mailboxes need a tree to route between. |
| G → K (fork/resume CLI) | Resume must reconstruct a tree that exists. |
| C (`Router`+`Health`) → M (fallback loop) | The attempt loop consumes an ordered candidate list. |
| B (one real backend) → Slice 1 | Fakes prove the loop; a real backend proves the design. |
| Everything → Group 4 | Conformance suites codify behavior that must first exist. |

### Critical Path

```
conway-core (ports+types)
  → conway-runtime skeleton + ContextBuilder
    → conway-session fork-by-reference
      → SubagentHost + AgentTree + supervisor        [G]
        → Mailboxes + steering + AgentResult contract [J]
          → Provenance inspection API                 [L]
            → hardening / OSS release                 [Group 4]
```

Everything else (Anthropic adapter, interactive TUI, routing fallback, agent definitions, resume) hangs off this chain and can proceed in parallel once Slice 1 lands. The core risk concentration is `ContextBuilder` + fork semantics: it is the single component that touches GP-01, GP-02, GP-06, and GP-10 simultaneously, and it should get golden-file tests from its first commit.

---

## 10. Design Tensions

Unresolved by the stated principles and constraints. Each needs a decision; none should be guessed at during implementation.

### T-1 — Forked prefix exceeds the child's context window
Fork inherits the *entire* parent context literally (decision 1). If a child is routed to a smaller-context model (a `fast` role on a local 32k model), the inherited prefix may not fit. Any auto-summarization would violate "literal prefix." Options:

| Option | Tradeoff |
|---|---|
| Reject the fork with a typed error naming the shortfall | Honest and predictable; but a valid delegation pattern (big planner → cheap executor) now fails at runtime, and the model must recover |
| Router filters candidates by `min_context >= est_tokens` and falls through the chain | Automatic and consistent with GP-07; but a `fast` role can silently resolve to an expensive large-context model — cost surprise |
| Add an explicit `ForkSpec::on_overflow: Reject \| Escalate \| Truncate{policy}` | Puts the decision at the call site where intent is known; adds API surface and a truncation policy that GP-01/decision-1 arguably forbid |

Relevant: GP-01 (lean context), decision 1 (no partial inheritance), GP-07 (predictable routing). **Not resolvable from the principles — they conflict.** My inclination is option 2 as the default with option 1 as the terminal state (fail loudly if no candidate fits), but this is a product decision about cost surprise.

### T-2 — Fallback on `ContextOverflow` is not a health signal but *is* a routing event
The health/routing split (decision 6) is clean for transport errors. `ContextOverflow` is neither: the endpoint is healthy, the request is fine, the *pairing* is wrong. Failing over to the next chain entry is often correct (it may have a bigger window), but recording it as a health observation would wrongly trip a breaker. Currently specced as "not failover-worthy," which may be wrong. Needs a third category: `RequestIncompatible` → advance the chain, do not record health.

### T-3 — Non-streaming retry after a streamed tool-call parse failure double-bills
Decision 10 mandates a reliable non-streaming path. Retrying the identical request after a streaming parse failure means paying for both generations, and on non-deterministic sampling the retry may produce a *different* tool call than the one that half-arrived. Options: (a) retry once, accept the cost, log it (specced default); (b) never stream when tools are registered, sacrificing token-by-token text on tool-capable turns; (c) per-model `stream_tools` config defaulting to `false` for unverified backends and `true` for Anthropic/OpenAI. (c) is probably right but requires maintaining a reliability table, which is exactly the maintenance burden research-backends warns about (correctness varies by quantization, not just by backend).

### T-4 — Cache locality vs. role-based routing
`PrefixKey` and cache economics want siblings on the same backend+model as the fork parent. Role aliases want a `fast` child on a cheap model. These conflict on every fork. The `Router` currently has no cache-affinity input, by design (GP-07: nothing content- or state-dependent). Adding `RouteRequest::prefer_affinity: Option<EndpointId>` would make routing depend on runtime state and weaken "predictable declarative routing." Leaving it out means forks to a different model always pay full prefix cost. Unresolved; leaning toward leaving it out for MVP and measuring, but flagging that this may be the single largest cost lever in the whole design.

### T-5 — Steering latency is unbounded
Turn-boundary-only steering (decision 3) means a steer can wait for an arbitrarily long tool call. `Event::SteerQueued` makes it visible but does not fix it. A `Cancel{hard:true}` + re-steer is the only escape, which loses the in-flight tool result. Research confirms this is unsolved industry-wide (Claude Code issues #30492, #64624, #36326) — so we are not behind, but a user *will* hit it. Do we need a `steer_urgent` that hard-cancels the current tool call?

### T-6 — Anthropic cache TTL policy owner
Research reports the default TTL may have dropped 1h→5min (community-sourced, unconfirmed). 1h costs ~2× on write vs ~1.25×. For a long-lived parent whose children fork over many minutes, 5min may miss entirely. Who decides `CacheTtl` — global config, per-role, or per-`ForkSpec`? Currently specced as `CacheHint.ttl` set by `ContextBuilder` from config. A heuristic (long TTL when the tree has >N pending children) would be adaptive but unpredictable, cutting against GP-07's spirit. **Also: this fact needs primary-source verification before any cost modeling.**

### T-7 — Result summary is model-generated and therefore lossy
`AgentResult.summary` is required and bounded, and in practice the model writes it. This is exactly the MAST "losing conversation history" mode, one level up: the parent sees the summary, not the transcript. `facts`/`artifacts`/`transcript_ref` mitigate it (structured data + retrievable full log), but nothing forces a model to populate `facts` well. Options: make `result_contract` mandatory for spawned children (schema-enforced, rejects free-form summaries); or add a `read_subagent_transcript` tool letting a parent pull specific ranges from `transcript_ref` on demand. The latter is attractive and cheap — it turns a child's transcript into a queryable resource rather than a lost one — but adds a tool not in the MVP list.

### T-8 — Plugin API: in-process `dyn Trait` only
MVP plugins are `Arc<dyn Plugin>` compiled into the host. This means third-party plugins require recompiling the embedding application — a real limitation for an OSS project where "everything is a plugin" (GP-03) implies an ecosystem. Rust has no stable ABI, so the alternatives are: WASM components (sandboxed, but async host calls and `bash`-style tools get awkward), or subprocess + JSON-RPC (which is essentially reinventing MCP, and MCP-as-a-plugin is already the stated post-MVP path). Decision needed on whether the MVP `Plugin` trait should be shaped *now* to be subprocess-representable (all args/returns JSON-serializable, no `Arc<dyn>` in tool-facing signatures) even though MVP is in-process. **Cheap to do now, expensive to retrofit.** I have specced `ToolOutput`/`ToolCall` as fully serializable for this reason, but `ToolCtx.subagents: Arc<dyn SubagentHost>` is not subprocess-representable and would need an RPC form.

### T-9 — `ContextReport` token estimates are backend-specific
Provenance (GP-10) promises the user can see what their context contains and how big each piece is. Token counts differ per tokenizer, and a forked context may be re-routed to a different model than the estimate assumed. Either we ship per-backend tokenizers (dependency weight, drift risk) or we report estimates with an explicit `Estimated{tokenizer}` marker and accept ±15% error. Specced as the latter; flagging that "how many tokens is my context" is a question users will treat as exact.

---

## 11. 100% Coverage Check

**1. Every aspect of the system falls inside some module.**

| MVP scope item | Owning module(s) |
|---|---|
| Fork + spawn subagents | `conway-runtime` (SubagentHost, AgentTree, ContextBuilder) + `conway-session` (O(1) fork) + `conway-tools` (SubagentPlugin) + `conway` (SessionHandle::fork/spawn) |
| Bidirectional messaging | `conway-runtime` (Mailbox, supervisor); types in `conway-core` |
| Minimal built-in tools as plugins | `conway-tools`; trait in `conway-core`; registry in `conway-runtime` |
| Agent/skill definitions | `conway` (loading) + `conway-core` (`AgentDef`/`SkillDef`) + `conway-runtime` (application to system prompt) |
| Declarative routing + failover | `conway-routing` (policy + breakers) + `conway-runtime` (attempt loop) |
| Anthropic + OpenAI-compatible backends | `conway-backends` |
| Session persistence, fork/resume | `conway-session` + `conway` (`resume`) + `conway-cli` (`--resume`, `--fork-from`) |
| Permission callback | `conway-core` (`PermissionGate`) + `conway-runtime` (`PermissionBroker`) + `conway` (built-in gates) |
| Three consumption modes | `conway` (library) + `conway-cli` (interactive + `-p`) |
| Context provenance | `conway-core` (`Provenance`) + `conway-runtime` (`ContextBuilder`, `context_report`) + `conway-session` (persistence) |
| Cache hints | `conway-core` (`CacheHint`/`CacheMode`) + `conway-runtime` (placement) + `conway-backends` (mapping) |
| Event stream for the IDE | `conway-core` (`Event`) + `conway-runtime` (`EventBus`) + `conway` (`EventStream`) |

**2. No two modules claim the same responsibility.** Checked pairs where overlap was plausible:
- Routing *policy* (`conway-routing`) vs. *attempt/fallback execution* (`conway-runtime`) — the router returns an ordered list; only the runtime makes calls and records health.
- *What* to persist (`conway-runtime`) vs. *how* (`conway-session`) — the store never interprets record semantics.
- Permission *decision* (consumer's `PermissionGate`) vs. *caching/plumbing* (`conway-runtime` broker) vs. *classification* (`conway-tools` declares `PermissionClass`).
- Config *types* (`conway-core`) vs. *loading* (`conway`).
- Fork *storage* (`conway-session`, O(1) reference) vs. fork *context semantics* (`conway-runtime` ContextBuilder) vs. fork *tool surface* (`conway-tools`).

**3. Union of module scopes = project scope.** Verified against MVP scope. Explicitly out of scope and correctly unowned: sibling comparison/merge, harness-owned isolation (GP-08 → tools), classifier routing (GP-07 → forbidden in `conway-routing`), ACP adapter (§6.5 keeps the event stream shim-able), llama.cpp slot adapter (`CacheMode::SlotKv` + `prefix_key` reserve the seam), TS SDK, MCP client (a future plugin on the existing `Plugin` trait).

**4. Every cross-module dependency has a matching Provides/Requires pair with a defined contract.** All eight boundary crossings in §8 are two-sided. The dependency graph is acyclic; the only would-be cycle (`conway-tools` → `conway-runtime`) is inverted through `SubagentHost` in `conway-core`.

**Result: PASS — no gaps, no overlaps.** Three caveats carried forward as tensions, not gaps:
- T-1 (fork overflow) has no owning policy yet — the *mechanism* is owned by `conway-runtime`, the *policy* is undecided.
- T-8 (plugin distribution) means `conway-tools`'s scope is complete for MVP but the extension *story* is not.
- T-4 (cache affinity vs. routing) is a deliberate non-feature; if adopted it lands in `conway-routing` and changes the `RouteRequest` contract.