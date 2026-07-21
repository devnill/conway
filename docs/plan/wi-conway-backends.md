# conway-backends — Decomposition

**Size assessment:** Right size. 7 work items (WI-016 … WI-030 range used: WI-016 … WI-022). The module has five distinct sub-surfaces (transport/config/errors, model metadata, tool-call accumulation, OpenAI-compat adapter, capability probing, Anthropic adapter, remaining dialects) with a clean dependency order. No sub-module split needed.

**Assumption stated:** `conway-core` is assumed to export exactly the types named in §4.1/§8 (`Backend`, `GenerateRequest`, `GenerateResponse`, `StreamChunk`, `PromptSegment`, `ContentBlock`, `Role`, `ToolSpec`, `ToolCall`, `SamplingParams`, `Usage`, `StopReason`, `Capabilities`, `ToolCallSupport`, `CacheMode`, `CacheTtl`, `ReliabilityTier`, `StructuredOutput`, `ProbeReport`, `BackendError`, `ModelId`, `BackendId`, `PrefixKey`). Where a helper type is needed that the module spec does not assign to `conway-core` (e.g. `Dialect`, `ModelMetadata`, `AnthropicConfig`, `OpenAiCompatConfig`), it is defined in `conway-backends`.

---

# WI-016: conway-backends crate skeleton, config types, error classification, and HTTP transport layer

## Objective
Create the `conway-backends` crate with independently gateable `anthropic` and `openai-compat` features, the configuration types for both adapters (including `sk-ant-oat*` rejection at parse time), the HTTP client wrapper with the bounded transport-retry policy, and the shared HTTP-status → `BackendError` classification used by every adapter.

## Complexity
Medium

## Scope
- `crates/conway-backends/Cargo.toml` (create)
- `crates/conway-backends/src/lib.rs` (create)
- `crates/conway-backends/src/config.rs` (create)
- `crates/conway-backends/src/error.rs` (create)
- `crates/conway-backends/src/http.rs` (create)
- `crates/conway-backends/tests/config_validation.rs` (create)
- `crates/conway-backends/tests/error_classification.rs` (create)

## Depends
- MODULE:conway-core

## Criteria
- [machine] `cargo build -p conway-backends --no-default-features` succeeds.
- [machine] `cargo build -p conway-backends --no-default-features --features anthropic` succeeds.
- [machine] `cargo build -p conway-backends --no-default-features --features openai-compat` succeeds.
- [machine] `cargo build -p conway-backends --all-features` succeeds; `cargo clippy -p conway-backends --all-features -- -D warnings` passes.
- [machine] Unit test: `AnthropicConfig` deserialized from TOML/JSON with `api_key = "sk-ant-oat01-abc"` returns `Err`, and the error `Display` string contains both the substring `sk-ant-oat` and the substring `subscription OAuth tokens are not supported`.
- [machine] Unit test: `AnthropicConfig` with `api_key = "sk-ant-api03-abc"` parses successfully.
- [machine] Unit test table over classification inputs produces exactly the mapping in Implementation Notes (one assertion per row).
- [machine] Wiremock test: an endpoint returning `503` twice then `200` yields `Ok` from `HttpClient::send_with_retry` after exactly 3 upstream requests; an endpoint returning `400` yields `Err(BackendError::BadRequest{..})` after exactly 1 upstream request.
- [machine] Wiremock test: an endpoint returning `429` with header `Retry-After: 7` surfaces `BackendError::RateLimit{ retry_after: Some(Duration::from_secs(7)) }` after the retry budget is exhausted (3 upstream requests total).

## Notes
**Objective:** Foundation layer that all adapters share. No wire-format translation here.

**Implementation Notes:**

Cargo features:
```toml
[features]
default = ["anthropic", "openai-compat"]
anthropic = ["dep:reqwest", "dep:eventsource-stream"]
openai-compat = ["dep:reqwest", "dep:eventsource-stream"]
```
`reqwest` with `rustls-tls`, `json`, `stream`; no `default-features`. Dev-deps: `wiremock`, `tokio` (`macros`, `rt-multi-thread`), `serde_json`, `futures`.

`src/lib.rs` declares:
```rust
pub mod config; pub mod error; pub(crate) mod http;
pub mod model_metadata;               // WI-017
#[cfg(feature = "anthropic")] pub mod anthropic;
#[cfg(feature = "openai-compat")] pub mod openai_compat;
```
Module declarations for later work items may be added as empty stubs here; later items own their file contents.

`config.rs`:
```rust
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicConfig {
    pub api_key: SecretString,           // String newtype, Debug prints "***"
    #[serde(default = "default_anthropic_base")] pub base_url: Url,   // https://api.anthropic.com
    #[serde(default = "default_anthropic_version")] pub anthropic_version: String, // "2023-06-01"
    #[serde(default)] pub timeout: Option<Duration>,                  // default 600s
    #[serde(default)] pub models: BTreeMap<String, ModelOverrides>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiCompatConfig {
    pub id: BackendId,
    pub base_url: Url,                   // e.g. http://localhost:11434/v1
    #[serde(default)] pub api_key: Option<SecretString>,
    pub dialect: Dialect,
    #[serde(default)] pub timeout: Option<Duration>,
    #[serde(default)] pub metadata_path: Option<PathBuf>,
    #[serde(default)] pub models: BTreeMap<String, ModelOverrides>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect { OpenAi, Ollama, VllmHermes, LmStudio, LlamaCppServer }

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelOverrides {
    pub stream_tools: Option<bool>,
    pub max_context_tokens: Option<u32>,
    pub reliability_tier: Option<ReliabilityTier>,
    pub parallel_tool_calls: Option<bool>,
}
```
Validation is performed in `AnthropicConfig::validate(&self) -> Result<(), ConfigError>` and invoked from a `#[serde(try_from = "AnthropicConfigRaw")]` shim so that *deserialization itself* fails — this is the "rejected at config-parse time" requirement (C-02, GP-09). Rejection rule: `api_key` trimmed, if it starts with `sk-ant-oat` → `ConfigError::SubscriptionTokenRejected` whose message is exactly:
`"Anthropic subscription OAuth tokens (sk-ant-oat*) are not supported: subscription OAuth tokens are not supported by conway; use a standard API key (sk-ant-api*) from console.anthropic.com"`.
Empty/whitespace-only key → `ConfigError::MissingApiKey`.

`error.rs` — `classify(status: StatusCode, body: &str, headers: &HeaderMap) -> BackendError`:

| Condition | `BackendError` |
|---|---|
| reqwest connect/timeout/body/IO error (no HTTP status) | `Transport { source }` |
| status 401, 403 | `Auth { message }` |
| status 429 | `RateLimit { retry_after }` — from `Retry-After` (secs or HTTP-date) else `None` |
| status 400/422 **and** body matches context-length regex (see below) | `ContextOverflow { limit, requested }` (fields `None` if unparsable) |
| status 400, 404, 405, 413, 422 (other) | `BadRequest { message }` |
| status 408 | `Transport { source }` |
| status 5xx (500,502,503,504, other) | `ServerError { status, message }` |
| status 2xx but body fails JSON deserialization | `BadRequest { message: "malformed response body: …" }` |
| request future dropped / `CancellationToken` fired | `Cancelled` |

Context-length regex (case-insensitive, matched against the raw body): `context[ _-]?length|maximum context|context window|too many tokens|prompt is too long|n_ctx`. `message` is the provider `error.message` field when the body is JSON with `{"error":{"message":...}}` (Anthropic and OpenAI both use this shape), else the first 512 bytes of the body.

`http.rs` — `HttpClient { inner: reqwest::Client, timeout: Duration }` with:
```rust
pub async fn send_with_retry(&self, make: impl Fn() -> RequestBuilder, cancel: &CancellationToken)
    -> Result<reqwest::Response, BackendError>;
```
Retry policy (module boundary rule: at most two retries, single endpoint, never cross-backend):
- Retryable: `Transport`, `ServerError`, `RateLimit`.
- Max 2 retries (3 total attempts).
- Backoff: base 250ms, doubling (250ms, 500ms), full jitter — `sleep(rand_range(0..=base * 2^attempt))`. Use a seedable RNG behind a trait or `rand::rng()`; tests must not depend on the sleep duration, only on request counts.
- For `RateLimit` with `retry_after` present, sleep `min(retry_after, 30s)` instead of the jitter backoff.
- On cancellation during a sleep or in-flight request, return `BackendError::Cancelled` immediately.
- After the retry budget, return the last classified error unchanged.
Streaming requests use the same helper for the *initial* response; mid-stream failures are never retried (a partially consumed stream is not idempotent).

---

# WI-017: ModelMetadata loader and per-(backend, model) capability construction

## Objective
Implement the `ModelMetadata` loader (local file, optional models.dev-derived data, never a hard network dependency) and the pure function that composes `ModelMetadata` + `ModelOverrides` + dialect defaults into a `Capabilities` value for a `(backend, model)` pair, including the `stream_tools` default derived from `ReliabilityTier`.

## Complexity
Medium

## Scope
- `crates/conway-backends/src/model_metadata.rs` (create)
- `crates/conway-backends/src/capabilities.rs` (create)
- `crates/conway-backends/tests/model_metadata.rs` (create)
- `crates/conway-backends/tests/fixtures/models.toml` (create)

## Depends
- WI-016
- MODULE:conway-core

## Criteria
- [machine] `ModelMetadataStore::load(&Path)` on `tests/fixtures/models.toml` returns a store containing every entry in the fixture; unit test asserts field-by-field equality for at least two entries.
- [machine] `ModelMetadataStore::load` on a nonexistent path returns `Ok(ModelMetadataStore::empty())`, not an error.
- [machine] `ModelMetadataStore::load` on a syntactically invalid file returns `Err(ConfigError::Metadata{..})`.
- [machine] Unit test: lookup order is exact `model_id` → normalized `model_id` (lowercased, `:` and `/` collapsed to `-`, trailing `-latest` stripped) → `None`; asserted with `"Qwen3-Coder:30b"` matching an entry keyed `qwen3-coder-30b`.
- [machine] Unit test: `build_capabilities` with no metadata entry and no overrides returns `reliability_tier: Unknown`, `tool_calling: NonStreamingOnly`, `max_context_tokens` equal to the dialect default, and `cache` equal to the dialect's `CacheMode`.
- [machine] Unit test: `stream_tools` default is `true` for `Verified`, `false` for `Community` and `Unknown`; an explicit `ModelOverrides::stream_tools = Some(true)` wins over the tier default for all three tiers.
- [machine] Unit test: `build_capabilities` is a pure function — calling it twice with equal inputs yields equal outputs; no filesystem or network access occurs (verified by the function taking only borrowed data, no `io` imports in `capabilities.rs`).
- [machine] `capabilities.rs` contains no `reqwest`/`std::fs`/`tokio::fs` reference (grep assertion in the test file or a `#![deny]`-style compile check).

## Notes
**Objective:** The single place where "capabilities are per (backend, model), never per backend" (module boundary rule) is realized.

**Implementation Notes:**

```rust
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ModelMetadata {
    pub id: String,
    pub max_context_tokens: Option<u32>,
    pub tool_calling: Option<ToolCallSupportSpec>,   // "none" | "non_streaming" | "streaming" | "streaming_validated"
    pub parallel_tool_calls: Option<bool>,
    pub structured_output: Option<StructuredOutputSpec>, // "none" | "json_schema" | "grammar"
    pub reasoning: Option<bool>,
    pub reliability_tier: Option<ReliabilityTier>,
    pub quantization: Option<String>,               // e.g. "Q4_K_M"; informational + tier heuristic
}

pub struct ModelMetadataStore { entries: BTreeMap<String, ModelMetadata> }
impl ModelMetadataStore {
    pub fn empty() -> Self;
    pub fn load(path: &Path) -> Result<Self, ConfigError>;   // missing file => empty()
    pub fn merge(self, other: Self) -> Self;                 // `other` wins on key collision
    pub fn get(&self, model: &ModelId) -> Option<&ModelMetadata>;
}
```
File format is TOML with an array-of-tables `[[model]]`. Bundled defaults live in `include_str!`-embedded `default_models.toml` content inside `model_metadata.rs` (a `DEFAULTS: &str` const) covering, at minimum: `claude-sonnet-4-6`, `claude-haiku-4-5` (`Verified`, `streaming_validated`, `parallel_tool_calls = true`), `gpt-4.1`/`gpt-5` class (`Verified`), `qwen3-coder-30b`, `qwen3-coder-80b`, `llama3.1-8b`, `glm-5.2` (`Community`, `non_streaming`). Load order: `DEFAULTS` → `config.metadata_path` file (if any) → per-model `ModelOverrides` from config. models.dev-derived data, if ever added, is a build-time-generated file merged at the `DEFAULTS` position; there is no runtime network fetch under any code path.

Quantization heuristic (applies only when `reliability_tier` is absent): if `quantization` matches `^(Q2|Q3|IQ)` → `Unknown`; `^(Q4)` → `Community`; `^(Q5|Q6|Q8|F16|BF16)` → `Community`. Never promotes to `Verified` — `Verified` is only ever set explicitly. Rationale: research-backends, sub-4-bit quants produce malformed tool-call arguments.

```rust
pub struct CapabilityInputs<'a> {
    pub dialect_defaults: DialectDefaults,   // or ANTHROPIC_DEFAULTS
    pub metadata: Option<&'a ModelMetadata>,
    pub overrides: Option<&'a ModelOverrides>,
}
pub fn build_capabilities(inputs: CapabilityInputs<'_>) -> Capabilities;
pub fn stream_tools_default(tier: ReliabilityTier) -> bool;  // Verified => true, else false
pub struct ResolvedModel { pub capabilities: Capabilities, pub stream_tools: bool }
```
Precedence for every field: `overrides` > `metadata` > `dialect_defaults`.

`DialectDefaults` per dialect (constants in `capabilities.rs`):

| Dialect | `cache` | `tool_calling` | `max_context_tokens` | `structured_output` | `parallel_tool_calls` | tier |
|---|---|---|---|---|---|---|
| `OpenAi` | `ImplicitPrefix{min_prefix_tokens:1024}` | `Streaming{validated:true}` | 128_000 | `JsonSchema` | true | `Verified` |
| `Ollama` | `ImplicitPrefix{min_prefix_tokens:0}` | `NonStreamingOnly` | 32_768 | `JsonSchema` | false | `Unknown` |
| `VllmHermes` | `ImplicitPrefix{min_prefix_tokens:0}` | `NonStreamingOnly` | 32_768 | `JsonSchema` | true | `Community` |
| `LmStudio` | `None` | `NonStreamingOnly` | 32_768 | `None` | false | `Unknown` |
| `LlamaCppServer` | `ImplicitPrefix{min_prefix_tokens:0}` | `NonStreamingOnly` | 32_768 | `Grammar` | false | `Community` |
| Anthropic | `ExplicitBreakpoints{max_breakpoints:4, ttls:&[CacheTtl::FiveMinutes, CacheTtl::OneHour]}` | `Streaming{validated:true}` | 200_000 | `JsonSchema` | true | `Verified` |

Ollama/vLLM/LM Studio defaults are `NonStreamingOnly` deliberately: research-backends documents active streaming tool-call bugs (ollama#12557, vllm#31871, codex#7517). A user opts into streaming tools per model via metadata or overrides.

---

# WI-018: ToolCallAccumulator — streaming tool-call delta accumulation and validation

## Objective
Implement `ToolCallAccumulator`, the dialect-parameterized state machine that accumulates streamed tool-call deltas into complete, schema-validated `ToolCall` values, covering the `OpenAi` and `Ollama` dialects. Failure to produce valid calls at stream end yields `BackendError::ToolParse`.

## Complexity
High

## Scope
- `crates/conway-backends/src/tool_calls/mod.rs` (create)
- `crates/conway-backends/src/tool_calls/openai.rs` (create)
- `crates/conway-backends/src/tool_calls/ollama.rs` (create)
- `crates/conway-backends/src/tool_calls/validate.rs` (create)
- `crates/conway-backends/tests/tool_call_accumulator.rs` (create)
- `crates/conway-backends/tests/fixtures/streams/` (create)

## Depends
- WI-016
- MODULE:conway-core

## Criteria
- [machine] `ToolCallAccumulator::new(Dialect, &[ToolSpec])` exists and is public.
- [machine] Unit test: OpenAI-shaped deltas `{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"pa"}}` then `{"index":0,"function":{"arguments":"th\":\"a.txt\"}"}}` accumulate to exactly one `ToolCall{ id:"call_1", name:"read", arguments: {"path":"a.txt"} }`.
- [machine] Unit test: two interleaved indices (`index:0` and `index:1` deltas alternating) produce two `ToolCall`s in ascending index order with correctly separated argument buffers.
- [machine] Unit test (regression for codex#7517): deltas that repeat `id` and `name` on every chunk produce ONE tool call, not N.
- [machine] Unit test (regression for ollama#12557): a chunk carrying a complete `arguments` object (not a fragment) followed by an empty-string `arguments` chunk produces one valid call; a stream in which `arguments` arrives as a JSON *object* rather than a string is accepted for the `Ollama` dialect.
- [machine] Unit test: `finish(StopReason::ToolUse)` with an unterminated JSON argument buffer returns `Err(BackendError::ToolParse{..})` whose message contains the tool name and the truncated buffer (bounded to 256 chars).
- [machine] Unit test: arguments that parse as JSON but fail the `ToolSpec` JSON Schema return `Err(BackendError::ToolParse{..})` naming the failing schema path.
- [machine] Unit test: a call naming a tool absent from the supplied `&[ToolSpec]` returns `Err(BackendError::ToolParse{..})` containing `unknown tool`.
- [machine] Unit test: `finish(StopReason::EndTurn)` with zero accumulated calls returns `Ok(vec![])`.
- [machine] Unit test: `arguments` accumulating to the empty string OR `"{}"` for a tool whose schema has no required properties yields `arguments: {}` (not an error).

## Notes
**Objective:** Isolate the single most bug-prone surface in the module so it is unit-testable without a server (module spec: "exposed for testing").

**Implementation Notes:**

```rust
pub struct ToolCallAccumulator { dialect: Dialect, specs: HashMap<ToolName, ToolSpec>,
                                 slots: BTreeMap<u32, Slot>, next_index: u32 }
struct Slot { id: Option<String>, name: Option<String>, args: String, args_value: Option<Value> }

impl ToolCallAccumulator {
    pub fn new(dialect: Dialect, specs: &[ToolSpec]) -> Self;
    /// Feed one raw provider delta object (the element of `choices[0].delta.tool_calls`).
    pub fn push_delta(&mut self, raw: &str) -> Result<(), BackendError>;
    /// Feed a fully-formed non-streaming tool call (shared path with `generate`).
    pub fn push_complete(&mut self, id: Option<String>, name: String, args: Value) -> Result<(), BackendError>;
    /// Validate and drain. Called on `finish_reason`/`stop_reason` arrival.
    pub fn finish(self, stop: StopReason) -> Result<Vec<ToolCall>, BackendError>;
    pub fn is_empty(&self) -> bool;
}
```

Accumulation rules (dialect-independent core in `mod.rs`):
- Slot key is the delta's `index` when present. When `index` is absent (some Ollama/LM Studio builds), key on `id` if present, else on `next_index` — a *new* slot is opened only when a non-empty `name` arrives while the current slot already has a name AND a syntactically complete argument buffer; otherwise the delta appends to the current slot. This is the codex#7517 mitigation.
- `id`/`name` are set on first non-empty occurrence; subsequent identical values are ignored; a subsequent *different* non-empty `name` for the same index is an error (`ToolParse`, "conflicting tool name for index N").
- If `id` is `None` at `finish`, synthesize `format!("call_{index}")`. Runtime correlates on this id, so it must be stable and unique within the response.
- `arguments` string fragments are concatenated verbatim, never trimmed and never re-encoded.
- Validation happens **only** in `finish`, never per-delta: partial JSON is expected mid-stream.
- `finish` order: for each slot in ascending key order — (1) resolve args: if `args_value` set use it, else if `args` is empty/whitespace use `Value::Object(empty)`, else `serde_json::from_str`; (2) look up spec by name, missing → `ToolParse`; (3) validate against `spec.schema` with `jsonschema`; (4) build `ToolCall`.
- `finish(stop)` with accumulated slots but `stop != ToolUse`: still validate and return the calls (some servers report `stop`/`length` alongside tool calls); do not error on stop-reason mismatch alone.

`openai.rs` — `parse_delta(raw) -> DeltaParts` for the canonical shape:
```json
{"index":0,"id":"call_abc","type":"function","function":{"name":"read","arguments":"…"}}
```
`ollama.rs` — same shape, plus these quirk tolerances: `index` may be missing; `function.arguments` may be a JSON **object** (set `args_value` directly rather than appending to `args`); `id` may be missing; a delta may be a bare `{"function":{...}}` with no wrapper. Anything unparseable as either shape → `ToolParse` with the raw delta (bounded to 256 chars) in the message.

`validate.rs` — thin wrapper over the `jsonschema` crate compiling each `ToolSpec.schema` once at `new()`; a schema that fails to compile is a `BadRequest` at `new()` time, not `ToolParse`.

`tests/fixtures/streams/` holds captured SSE bodies as `.txt` files (one per scenario named in the criteria) so the same fixtures are reusable by WI-019 and WI-022 integration tests.

---

# WI-019: OpenAiCompatBackend — wire mapping, generate, stream (OpenAi + Ollama dialects)

## Objective
Implement `OpenAiCompatBackend::new(OpenAiCompatConfig) -> impl Backend` with segment→messages translation, tool schema translation, non-streaming `generate`, SSE `stream`, and the `ImplicitPrefix` cache-hint no-op, wired to the `OpenAi` and `Ollama` dialects. This is the Group 1 / Slice 1 deliverable.

## Complexity
High

## Scope
- `crates/conway-backends/src/openai_compat/mod.rs` (create)
- `crates/conway-backends/src/openai_compat/wire.rs` (create)
- `crates/conway-backends/src/openai_compat/stream.rs` (create)
- `crates/conway-backends/src/openai_compat/dialect.rs` (create)
- `crates/conway-backends/tests/openai_compat_generate.rs` (create)
- `crates/conway-backends/tests/openai_compat_stream.rs` (create)

## Depends
- WI-016
- WI-017
- WI-018
- MODULE:conway-core

## Criteria
- [machine] `OpenAiCompatBackend::new(OpenAiCompatConfig) -> Result<Self, ConfigError>` exists; `impl Backend for OpenAiCompatBackend` compiles under `--features openai-compat --no-default-features`.
- [machine] Unit test: segment→message mapping produces the exact JSON in Implementation Notes for a 4-segment fixture (System, User, Assistant-with-tool_calls, ToolResult), asserted with `assert_eq!` against a golden `serde_json::Value`.
- [machine] Unit test (cache-hint invariant): serializing a `GenerateRequest` whose segments carry `Some(CacheHint{..})` produces a byte-identical body to the same request with all `cache_hint` fields set to `None`.
- [machine] Wiremock test: `generate` against a stubbed `/chat/completions` returning a text-only completion yields `GenerateResponse{ content: [Text(..)], tool_calls: [], stop: EndTurn, usage }` with `usage.input_tokens`/`output_tokens` from `usage.prompt_tokens`/`completion_tokens`.
- [machine] Wiremock test: `generate` returning `finish_reason:"tool_calls"` with one function call yields exactly one validated `ToolCall` and `stop: StopReason::ToolUse`.
- [machine] Wiremock test: `stream` against a stubbed SSE endpoint emits `StreamChunk::TextDelta` per content delta, in order, terminated by exactly one `StreamChunk::Done(GenerateResponse)` whose `content` equals the concatenation of the deltas.
- [machine] Wiremock test: `stream` with tool-call deltas emits `StreamChunk::ToolCallDelta{index, raw}` per delta and a final `Done` whose `tool_calls` are validated.
- [machine] Wiremock test: `stream` whose tool-call deltas are truncated (unterminated JSON at `[DONE]`) yields a stream item `Err(BackendError::ToolParse{..})`; the adapter performs NO non-streaming retry of its own (assert the mock received exactly 1 request).
- [machine] Wiremock test: `generate` against a `500` endpoint yields `BackendError::ServerError` after exactly 3 requests; against `401` yields `BackendError::Auth` after exactly 1.
- [machine] Unit test: `capabilities(&model)` returns the value produced by `build_capabilities` for that model — asserted for one model present in metadata and one absent.

## Notes
**Objective:** One adapter, dialect-selected behavior. Ollama is the first supported dialect because it is free, local, and Slice 1 depends on it.

**Implementation Notes:**

```rust
pub struct OpenAiCompatBackend {
    id: BackendId, base: Url, dialect: Dialect, http: HttpClient,
    auth: Option<SecretString>, models: ModelMetadataStore, overrides: BTreeMap<String, ModelOverrides>,
}
```
`new` merges `DEFAULTS` + `config.metadata_path` metadata (WI-017) and stores overrides. `id()` returns `config.id`. `capabilities(model)` calls `build_capabilities` (WI-017); it never performs I/O.

Endpoint: `POST {base_url}/chat/completions`. Headers: `Content-Type: application/json`; `Authorization: Bearer {api_key}` only when `api_key.is_some()` (local servers commonly need none).

`wire.rs` — segment → OpenAI message mapping:

| `PromptSegment.role` | emitted message |
|---|---|
| `System` | `{"role":"system","content":<concatenated text blocks>}` |
| `User` | `{"role":"user","content":<text>}` (multi-block: array of `{"type":"text","text":...}` for `OpenAi`; joined with `\n\n` into a single string for `Ollama`) |
| `Assistant` | `{"role":"assistant","content":<text or null>,"tool_calls":[{"id":..,"type":"function","function":{"name":..,"arguments":<stringified JSON>}}]}` |
| `ToolResult` | one message per result: `{"role":"tool","tool_call_id":<call_id>,"content":<text>}` |

Rules: segment order is preserved exactly (order is load-bearing for implicit prefix caching, §5.3 — the adapter reorders nothing, §8). Consecutive segments are NOT merged. `ContentBlock::Thinking` is omitted from the request for both dialects. `cache_hint` is read and discarded — `CacheMode::ImplicitPrefix` maps to a wire no-op per §4.1; there must be no code path where the presence of a hint changes a single byte of the request body.

Tools: `"tools":[{"type":"function","function":{"name":spec.name,"description":spec.description,"parameters":spec.schema}}]`. Omit the `tools` key entirely when `req.tools` is empty. `"tool_choice":"auto"` only when tools are present. `"parallel_tool_calls"` is emitted only when `capabilities.parallel_tool_calls` is true AND dialect is `OpenAi` (other servers 400 on the unknown field).

Params: `temperature`, `top_p`, `max_tokens` (`max_completion_tokens` for `OpenAi`), `stop`. Omit any `None`. `prefix_key` is ignored for all dialects in this item (it is the `SlotKv` seam, post-MVP).

Response mapping (`generate`): `choices[0].message.content` → `ContentBlock::Text` (omitted when null/empty); `choices[0].message.tool_calls` → fed to `ToolCallAccumulator::push_complete` then `finish`; `finish_reason` → `StopReason`: `stop`→`EndTurn`, `tool_calls`/`function_call`→`ToolUse`, `length`→`MaxTokens`, `content_filter`→`Refusal`, unknown/null→`EndTurn`. `usage` → `Usage` with `cache_read_tokens` from `usage.prompt_tokens_details.cached_tokens` when present, else `None`.

`stream.rs` — SSE over `eventsource-stream`. Body adds `"stream":true` and, for `OpenAi` and `Ollama`, `"stream_options":{"include_usage":true}`. Per event: `data: [DONE]` terminates; otherwise parse a chunk and, for each `choices[0].delta`:
- `content` non-empty → yield `StreamChunk::TextDelta`, append to a text buffer.
- `reasoning_content`/`reasoning` non-empty → yield `StreamChunk::ThinkingDelta` (not appended to the text buffer).
- `tool_calls[i]` → yield `StreamChunk::ToolCallDelta{ index: i_or_slot, raw: <the raw delta JSON> }` and call `accumulator.push_delta(raw)`.
- `finish_reason` non-null → record the stop reason.
- top-level `usage` object → record usage.
At `[DONE]` (or stream end): call `accumulator.finish(stop)`. On `Ok(calls)` yield exactly one `StreamChunk::Done(GenerateResponse{ content, tool_calls: calls, stop, usage })` and end the stream. On `Err(ToolParse)` yield that error as the final stream item and end the stream — **the adapter does not retry**; the non-streaming fallback is the runtime's job (decision 10, §7 internal notes). Mid-stream transport errors yield `BackendError::Transport` as a stream item; they are never retried.

`dialect.rs` — `impl Dialect { fn defaults(self) -> DialectDefaults; fn chat_path(self) -> &'static str; fn supports_stream_options(self) -> bool; fn flatten_multiblock_user(self) -> bool; }`. Only `OpenAi` and `Ollama` arms are exercised by this item's tests; the remaining three arms return the `defaults()` table from WI-017 and are covered by WI-022.

---

# WI-020: CapabilityProbe and `Backend::probe` for OpenAiCompatBackend

## Objective
Implement `CapabilityProbe` — startup-time discovery that queries the endpoint's model list and server properties and merges the result with `ModelMetadata` to produce `Capabilities` per model — plus the `Backend::probe` liveness implementation for `OpenAiCompatBackend`.

## Complexity
Medium

## Scope
- `crates/conway-backends/src/probe.rs` (create)
- `crates/conway-backends/src/openai_compat/probe_impl.rs` (create)
- `crates/conway-backends/tests/capability_probe.rs` (create)

## Depends
- WI-017
- WI-019
- MODULE:conway-core

## Criteria
- [machine] `CapabilityProbe::new(...)` and `async fn discover(&self) -> Result<BTreeMap<ModelId, Capabilities>, BackendError>` are public.
- [machine] Wiremock test: `/v1/models` returning `{"data":[{"id":"qwen3-coder:30b"},{"id":"llama3.1:8b"}]}` yields a map with exactly those two `ModelId` keys.
- [machine] Wiremock test (llama.cpp): `/props` returning `{"default_generation_settings":{"n_ctx":16384}}` results in `max_context_tokens == 16384` for discovered models when metadata does not specify one; an explicit metadata value wins over the probed value.
- [machine] Wiremock test (Ollama fallback): `/v1/models` returning `404` while `/api/tags` returns `{"models":[{"name":"qwen3-coder:30b"}]}` still yields the model (dialect `Ollama` only).
- [machine] Wiremock test: total network failure of every discovery endpoint yields `Ok` with the capabilities derived from `ModelMetadata` alone (never an `Err`); a `tracing::warn` is emitted (assert via `tracing-test` capture or a returned `discovery_degraded: true` flag asserted directly).
- [machine] Wiremock test: `probe()` against a `200` `/v1/models` returns `Ok(ProbeReport)` with `latency` populated and `healthy: true`.
- [machine] Wiremock test: `probe()` against a connection-refused address returns `Err(BackendError::Transport{..})` after exactly 1 request (probe does NOT use the retry budget).
- [machine] Wiremock test: `probe()` against `401` returns `Err(BackendError::Auth{..})`.
- [machine] Unit test: `discover` never returns a `Capabilities` for a model whose id was not observed from the endpoint or listed in configured `models` overrides.

## Notes
**Objective:** Make the "capabilities per (backend, model)" boundary rule operational at startup without making discovery a hard dependency.

**Implementation Notes:**

```rust
pub struct CapabilityProbe { http: HttpClient, base: Url, dialect: Dialect,
                             auth: Option<SecretString>, metadata: ModelMetadataStore,
                             overrides: BTreeMap<String, ModelOverrides> }
pub struct DiscoveryResult { pub capabilities: BTreeMap<ModelId, Capabilities>, pub degraded: bool }
```
Discovery sequence per dialect (all failures are non-fatal; each step is best-effort with a 5s timeout and zero retries):
1. `GET {base}/models` (OpenAI shape, all dialects) → `data[].id`.
2. Dialect `Ollama` and step 1 failed/empty: `GET {base_host}/api/tags` → `models[].name`.
3. Dialect `LlamaCppServer`: `GET {base_host}/props` → `default_generation_settings.n_ctx` (server-wide context bound) and `chat_template` presence (a missing/empty template downgrades `reliability_tier` to `Unknown` — research-backends flags broken GGUF templates as an independent tool-call failure source).
4. Dialect `VllmHermes`: `GET {base}/models` only; `max_model_len` on the model object, when present, supplies `max_context_tokens`.

Merge precedence for each discovered model: config `ModelOverrides` > `ModelMetadata` entry > probed server value > `DialectDefaults`. Discovery may only *narrow* `max_context_tokens` when no explicit value exists; it never raises `tool_calling` above the metadata/dialect value and never sets `reliability_tier` to `Verified`.

Models present in config `models` but absent from discovery are still included (a user may pin a model the list endpoint does not expose) with `degraded = false`.

`probe_impl.rs` implements `Backend::probe` for `OpenAiCompatBackend`: `GET {base}/models` (or `/api/tags` for `Ollama` when `/models` 404s), 2s timeout, **retries disabled** — the health layer's breaker semantics require a probe to be a single observation (§4.5, `BreakerKind::Probe` is independent of `BreakerKind::Transport`). Returns `ProbeReport { healthy: true, latency, detail: Some(model_count.to_string()) }`.

---

# WI-021: AnthropicBackend — Messages API adapter with explicit cache-breakpoint mapping

## Objective
Implement `AnthropicBackend::new(AnthropicConfig) -> impl Backend`: native Messages API request/response translation, SSE streaming with `tool_use` accumulation, `CacheMode::ExplicitBreakpoints` cache-hint mapping with breakpoint capping, capabilities, and `probe`.

## Complexity
High

## Scope
- `crates/conway-backends/src/anthropic/mod.rs` (create)
- `crates/conway-backends/src/anthropic/wire.rs` (create)
- `crates/conway-backends/src/anthropic/stream.rs` (create)
- `crates/conway-backends/src/anthropic/cache.rs` (create)
- `crates/conway-backends/tests/anthropic_generate.rs` (create)
- `crates/conway-backends/tests/anthropic_stream.rs` (create)
- `crates/conway-backends/tests/anthropic_cache_mapping.rs` (create)

## Depends
- WI-016
- WI-017
- WI-018
- MODULE:conway-core

## Criteria
- [machine] `AnthropicBackend::new(AnthropicConfig) -> Result<Self, ConfigError>` exists; `impl Backend for AnthropicBackend` compiles under `--features anthropic --no-default-features`.
- [machine] Unit test: a 4-segment fixture maps to the golden request body — `system` as a top-level array, `messages` with alternating roles, `tool_result` blocks nested in a `user` message.
- [machine] Unit test: consecutive `ToolResult` segments are merged into ONE `user` message containing multiple `tool_result` blocks (API requires this).
- [machine] Unit test (cap): 6 segments with `cache_hint.breakpoint == true` produce exactly 4 `cache_control` markers, retained on the LAST 4 breakpointed segments in segment order (trim priority per Implementation Notes).
- [machine] Unit test (byte-identity invariant): the request body with all `cache_hint`s stripped equals the body with hints present after removing every `"cache_control"` key — asserted by recursive JSON key deletion and `assert_eq!`.
- [machine] Unit test: `CacheTtl::OneHour` emits `{"type":"ephemeral","ttl":"1h"}`; `CacheTtl::FiveMinutes` emits `{"type":"ephemeral"}` with no `ttl` key.
- [machine] Wiremock test: `generate` on a stubbed `/v1/messages` text response yields `GenerateResponse` with `stop: EndTurn` and `usage` populated from `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`.
- [machine] Wiremock test: `generate` returning a `tool_use` block yields exactly one validated `ToolCall` and `stop: ToolUse`.
- [machine] Wiremock test: `stream` over the canonical SSE event sequence (`message_start`, `content_block_start`, `content_block_delta`×N, `content_block_stop`, `message_delta`, `message_stop`) emits ordered `TextDelta`s and exactly one `Done`.
- [machine] Wiremock test: a streamed `tool_use` block whose `input_json_delta` fragments do not form valid JSON yields a final `Err(BackendError::ToolParse{..})` with exactly 1 upstream request (no adapter-level retry).
- [machine] Wiremock test: `429` with `retry-after: 3` yields `BackendError::RateLimit{retry_after: Some(3s)}`; `529` yields `ServerError`; `400` with `"prompt is too long"` yields `ContextOverflow`.
- [machine] Wiremock test: `thinking_delta` events emit `StreamChunk::ThinkingDelta` and produce a `ContentBlock::Thinking` in `Done.content`.
- [machine] Unit test: `capabilities(&ModelId("claude-sonnet-4-6"))` returns `cache: ExplicitBreakpoints{max_breakpoints:4, ..}` and `tool_calling: Streaming{validated:true}`.

## Notes
**Objective:** The Group 2 / Track F deliverable. API key only — no OAuth path exists anywhere in this file set.

**Implementation Notes:**

Endpoint `POST {base_url}/v1/messages`. Headers: `x-api-key: {api_key}`, `anthropic-version: {config.anthropic_version}`, `content-type: application/json`. No `Authorization: Bearer` header is ever constructed (GP-09/C-02); a grep for `Bearer` in `src/anthropic/` must find nothing.

`wire.rs` mapping:
- `Role::System` segments → top-level `"system": [{"type":"text","text":...}]`, in segment order, concatenated as separate array entries (separate entries are required so per-segment `cache_control` can attach).
- `Role::User` → `{"role":"user","content":[{"type":"text","text":...}]}`.
- `Role::Assistant` → `{"role":"assistant","content":[ text blocks…, {"type":"tool_use","id":..,"name":..,"input":<JSON object>} ]}`.
- `Role::ToolResult` → `{"role":"user","content":[{"type":"tool_result","tool_use_id":<call_id>,"content":<text>,"is_error":<bool>}]}`. **Consecutive** `ToolResult` segments merge into one `user` message.
- `tools` → `[{"name":..,"description":..,"input_schema": spec.schema}]`; omit the key when empty.
- `max_tokens` is **required** by the API: use `params.max_tokens` or a default of 8192.
- `ContentBlock::Thinking` in a prior assistant turn is emitted as `{"type":"thinking",...}` only when the block carries a signature; otherwise omitted.

`cache.rs` — `apply_cache_hints(body: &mut Value, segments: &[PromptSegment], max_breakpoints: u8)`:
- Collect the indices of segments with `cache_hint.breakpoint == true`.
- If the count exceeds `max_breakpoints`, drop the **earliest** ones (§5.3: trim priority is B > A > everything else; B, the fork boundary, is later in segment order than A, the tool-schema boundary, so "keep the last N in segment order" implements the stated priority).
- For each retained breakpoint, attach `"cache_control": {"type":"ephemeral"}` (plus `"ttl":"1h"` for `CacheTtl::OneHour`) to the **last content block** of that segment's emitted message/system entry.
- This function is the ONLY place that writes `cache_control`. It must be a strictly additive post-pass over the body produced by `wire.rs`, which guarantees the byte-identity invariant (§4.1: "if a hint is dropped, output must be byte-for-byte the same request content").
- `CacheTtl` values outside `capabilities.cache.ttls` are downgraded to the nearest supported value; no error.

`stream.rs` — SSE state machine over Anthropic event types:

| event | action |
|---|---|
| `message_start` | seed `usage` from `message.usage`; record model |
| `content_block_start` (`type:"text"`) | open a text block |
| `content_block_start` (`type:"tool_use"`) | open slot: `accumulator.push_delta` seeded with `id`+`name`, empty args |
| `content_block_start` (`type:"thinking"`) | open a thinking block |
| `content_block_delta` / `text_delta` | yield `TextDelta`, append to buffer |
| `content_block_delta` / `thinking_delta` | yield `ThinkingDelta` |
| `content_block_delta` / `input_json_delta` | yield `ToolCallDelta{index, raw: partial_json}`, append to that slot's args |
| `content_block_stop` | close the current block |
| `message_delta` | record `delta.stop_reason` and `usage.output_tokens` |
| `message_stop` | `accumulator.finish(stop)` → one `Done` or one `Err(ToolParse)` |
| `ping` | ignore |
| `error` | classify to `BackendError` and yield as the final item |

Stop reason mapping: `end_turn`→`EndTurn`, `tool_use`→`ToolUse`, `max_tokens`→`MaxTokens`, `stop_sequence`→`StopSequence`, `refusal`→`Refusal`.

`ToolCallAccumulator` is reused with a `Dialect`-independent constructor path: add `ToolCallAccumulator::new_anthropic(&[ToolSpec])` in this item's `anthropic/mod.rs` by calling the existing `push_complete`/`push_delta` API with `Dialect::OpenAi` semantics for the JSON-fragment path. Do not modify `src/tool_calls/*` — those files are owned by WI-018/WI-022.

`probe()`: `POST /v1/messages` is not free, so probe issues `GET {base_url}/v1/models` with the same headers, 2s timeout, no retries. `529` and 5xx → `ServerError`; connection failure → `Transport`; `401`/`403` → `Auth`.

Anthropic error-body shape `{"type":"error","error":{"type":"overloaded_error","message":"…"}}` is fed to `error::classify` from WI-016; additionally map `error.type == "overloaded_error"` → `ServerError` and `error.type == "rate_limit_error"` → `RateLimit` regardless of status.

---

# WI-022: Remaining OpenAI-compat dialects — VllmHermes, LmStudio, LlamaCppServer

## Objective
Complete the five-dialect matrix by adding the `VllmHermes`, `LmStudio`, and `LlamaCppServer` accumulator variants and request quirks to the existing `OpenAiCompatBackend`, with wiremock-based tests covering each dialect's documented failure modes.

## Complexity
Medium

## Scope
- `crates/conway-backends/src/tool_calls/hermes.rs` (create)
- `crates/conway-backends/src/tool_calls/mod.rs` (modify)
- `crates/conway-backends/src/openai_compat/dialect.rs` (modify)
- `crates/conway-backends/src/openai_compat/wire.rs` (modify)
- `crates/conway-backends/tests/dialect_conformance.rs` (create)

## Depends
- WI-018
- WI-019
- WI-020

## Criteria
- [machine] `Dialect::VllmHermes`, `Dialect::LmStudio`, `Dialect::LlamaCppServer` each return a distinct `DialectDefaults` matching the WI-017 table (one assertion per dialect).
- [machine] Unit test (vllm#31871 regression): a `VllmHermes` stream in which a tool call arrives as raw text `<tool_call>{"name":"read","arguments":{"path":"a.txt"}}</tool_call>` inside `delta.content` — with no `tool_calls` field at all — produces one validated `ToolCall` and `stop: ToolUse`, and those content deltas are NOT emitted as `TextDelta`.
- [machine] Unit test: a `VllmHermes` stream with well-formed `delta.tool_calls` uses the standard OpenAI path (no text scanning), asserted by identical output to the `OpenAi` dialect on the same fixture.
- [machine] Unit test: an unterminated `<tool_call>` block at stream end for `VllmHermes` yields `BackendError::ToolParse`.
- [machine] Unit test (codex#7517 regression, `LmStudio`): a stream repeating full `id`+`name`+complete `arguments` on every chunk produces ONE tool call.
- [machine] Wiremock test: `LlamaCppServer` `generate` with `tools` present emits `"tool_choice":"auto"` and does NOT emit `parallel_tool_calls` or `stream_options`; asserted by inspecting the captured request body.
- [machine] Wiremock test: `LmStudio` request body omits `stream_options` and omits `parallel_tool_calls`.
- [machine] Wiremock test: each of the three dialects completes a text-only `generate` and a text-only `stream` against an OpenAI-shaped stub (6 tests total).
- [machine] `cargo clippy -p conway-backends --all-features -- -D warnings` passes; no `todo!()`/`unimplemented!()` remains in `src/openai_compat/` or `src/tool_calls/`.

## Notes
**Objective:** "One adapter, five dialects" — completes the matrix without a second adapter. Sequenced last per §9 (Slice 1 needs only Ollama).

**Implementation Notes:**

`hermes.rs` — a text-scanning secondary accumulator used ONLY when `dialect == VllmHermes` and the stream has produced zero `delta.tool_calls` entries. Behavior:
- Buffer all `delta.content` text.
- Scan for `<tool_call>` … `</tool_call>` pairs (also accept `<tool_call>` followed by end-of-stream as an unterminated block → `ToolParse`).
- Each block's inner JSON is `{"name": …, "arguments": {…}}`; feed it to `ToolCallAccumulator::push_complete` with a synthesized id `call_{n}`.
- Text OUTSIDE the tags is emitted as `TextDelta` normally; text inside is suppressed. Because tag detection is retroactive, suppression works by buffering content deltas until either `<` cannot begin a `<tool_call>` prefix (flush) or a full tag is matched (suppress). Flush any residual buffer at stream end.
- If at least one hermes-text block is parsed, override `stop` to `StopReason::ToolUse` even when `finish_reason == "stop"`.

`tool_calls/mod.rs` modifications: add `Dialect::VllmHermes | Dialect::LmStudio | Dialect::LlamaCppServer` arms to `push_delta` dispatch. `LmStudio` and `LlamaCppServer` use the `ollama.rs` tolerant parser (missing `index`, object-valued `arguments`, repeated `id`/`name`) — no new parser file. `VllmHermes` uses the `openai.rs` parser for the structured path plus `hermes.rs` for the text fallback.

`dialect.rs` modifications: fill in the remaining arms —
- `supports_stream_options()`: true only for `OpenAi`, `Ollama`.
- `flatten_multiblock_user()`: true for `Ollama`, `LmStudio`, `LlamaCppServer`.
- `sends_parallel_tool_calls()`: true only for `OpenAi`.
- new `uses_hermes_text_fallback()`: true only for `VllmHermes`.
- `chat_path()`: `"/chat/completions"` for all five.

`wire.rs` modifications are limited to consulting the three new dialect predicates; no mapping rule from WI-019 changes for `OpenAi`/`Ollama` (regression-guarded by the WI-019 golden tests, which must continue to pass unmodified).

`LlamaCppServer` note: this item deliberately does NOT use the native `/completion`, `/slots/save|restore`, or GBNF grammar endpoints. Those belong to the post-MVP `CacheMode::SlotKv` native adapter; the `prefix_key` field on `GenerateRequest` reserves that seam and remains ignored here.

---

## Coverage Statement

**Module:** conway-backends
**Work items:** WI-016, WI-017, WI-018, WI-019, WI-020, WI-021, WI-022

**Coverage:** These seven work items collectively implement 100% of the conway-backends module scope: wire-format translation for both adapters (WI-019, WI-021, WI-022), tool-call parsing in both streaming and non-streaming modes (WI-018, WI-019, WI-021, WI-022), cache-hint mapping for `ExplicitBreakpoints` (WI-021) and `ImplicitPrefix` (WI-019), and capability declaration per (backend, model) (WI-017, WI-020). Every module boundary rule is bound to at least one machine-verifiable criterion: per-(backend, model) capabilities (WI-017, WI-019, WI-020); byte-identical requests when hints are dropped (WI-019, WI-021); no cross-backend retry and ≤2 transport retries with jitter (WI-016, WI-019, WI-021); the eight-variant `BackendError` classification (WI-016, plus per-adapter assertions in WI-019/WI-021); `sk-ant-oat*` rejection at config parse (WI-016); `prefix_key`/`CacheMode::SlotKv` seam preserved without signature change (WI-019, WI-022).

**Intentionally excluded:** (a) the llama.cpp native slot adapter — explicitly post-MVP per §4.1 and §7; the seam is reserved and asserted, not implemented. (b) MLX/`mlx_lm.server` — not named in the module Provides; reachable today via `Dialect::OpenAi` or `Dialect::LmStudio`. (c) The non-streaming fallback after a streaming `ToolParse` failure — owned by conway-runtime per decision 10; WI-019 and WI-021 assert the adapter does *not* retry. (d) The cross-adapter backend conformance suite — assigned to Group 4 hardening in §9.

**Provides implemented by:**
- `AnthropicBackend::new(AnthropicConfig) -> impl Backend` (feature `anthropic`) → WI-021 (config type: WI-016; capabilities: WI-017)
- `OpenAiCompatBackend::new(OpenAiCompatConfig) -> impl Backend` (feature `openai-compat`) → WI-019 (dialects `OpenAi`/`Ollama`), WI-022 (dialects `VllmHermes`/`LmStudio`/`LlamaCppServer`); config type: WI-016; `probe`: WI-020
- `CapabilityProbe` → WI-020 (capability composition function: WI-017)
- `ToolCallAccumulator` → WI-018 (`OpenAi`/`Ollama`), WI-022 (`VllmHermes`/`LmStudio`/`LlamaCppServer`), WI-021 (Anthropic construction path)
- `ModelMetadata` loader → WI-017

**Requires consumed by:**
- `conway-core::Backend` → WI-019, WI-020, WI-021 (impl blocks)
- `conway-core::{GenerateRequest, GenerateResponse, StreamChunk, PromptSegment, ContentBlock, ToolSpec, ToolCall, SamplingParams, Usage, StopReason}` → WI-018, WI-019, WI-021, WI-022
- `conway-core::{Capabilities, ToolCallSupport, CacheMode, ReliabilityTier, StructuredOutput}` → WI-017, WI-020
- `conway-core::CacheHint` → WI-019 (no-op path), WI-021 (`cache_control` mapping)
- `conway-core::BackendError` → WI-016 (classification), consumed by WI-018, WI-019, WI-020, WI-021, WI-022
- `conway-core::{ModelId, BackendId, PrefixKey, ProbeReport}` → WI-017, WI-019, WI-020

**DAG check:** WI-016 → {WI-017, WI-018, WI-019, WI-021}; WI-017 → {WI-019, WI-020, WI-021}; WI-018 → {WI-019, WI-021, WI-022}; WI-019 → {WI-020, WI-022}; WI-020 → WI-022. Acyclic. Max depth 5 (016→017→019→020→022). WI-021 (Anthropic) is parallel with WI-020/WI-022 after WI-018, matching the §9 Group 1 / Group 2 split.

**File-scope check:** The only files appearing in more than one item are `src/tool_calls/mod.rs`, `src/openai_compat/dialect.rs`, and `src/openai_compat/wire.rs` — each created in WI-018/WI-019 and modified in WI-022, which declares a dependency on both. No two concurrent items share a file.