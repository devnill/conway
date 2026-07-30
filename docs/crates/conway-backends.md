# conway-backends

`conway-backends` implements the [`Backend` port](conway-core.md) from
`conway-core`: one adapter per LLM provider dialect. See
[`/ARCHITECTURE.md`](/ARCHITECTURE.md) for how this crate fits the
ports-and-adapters workspace.

## Responsibility and boundary

This crate owns wire-format translation, tool-call parsing, cache-hint
mapping, and capability declaration for two feature-gated adapters:

- **`AnthropicBackend`** (`feature = "anthropic"`) — the native Anthropic
  Messages API.
- **`OpenAiCompatBackend`** (`feature = "openai-compat"`) — one adapter that
  speaks to any OpenAI-compatible provider: **OpenAI**, **Ollama**,
  **vLLM/Hermes**, **LM Studio**, **llama.cpp server**, **Kimi** (Moonshot
  platform API), and any provider a user describes as a
  [declarative provider profile](#declarative-provider-profiles). These are
  not separate backend types; `conway_backends::profile::Profile` is a
  small data value that parameterizes a single adapter's request/response
  shaping, because every OpenAI-compatible provider speaks the same
  `/chat/completions`-shaped protocol with small, well-characterized
  deviations — see "Declarative provider profiles" below for the full
  story and how a new provider is added with no recompile.

It performs **no routing or policy decisions** and **no cross-backend
retry**: a single `generate`/`stream` call targets one endpoint, and the
bounded transport-retry policy in `http` retries at most twice against that
*same* endpoint — falling over to a different candidate is
[`conway-routing`](conway-routing.md)'s job, not this crate's.

`config` and `error` are feature-independent (they compile under
`--no-default-features`, with no HTTP client); `tool_calls` is likewise
feature-independent and shared, unmodified, by both adapters' streaming
delta-accumulation paths. The `http` transport wrapper — the only module
here that depends on `reqwest` — is compiled only when at least one adapter
feature is enabled.

## Anthropic-compatible third-party endpoints

`AnthropicBackend` is not hardwired to `api.anthropic.com`. Any provider
serving an Anthropic-shaped `/v1/messages` works by pointing `base_url` at
it, including endpoints that live under a path prefix rather than at the
host root: the prefix is preserved, so `https://host/coding/` resolves to
`https://host/coding/v1/messages` (with or without the trailing slash).

Conway does not inspect the shape of an API key. Whether a credential is a
metered API key, a coding-plan subscription key, or something a self-hosted
shim issues is the provider's business, not Conway's — an unusable key
surfaces as that provider's own auth error, which says more than any
prefix check here could. The only key-related failures Conway raises are
the ones it can describe precisely: an empty key, or an `api_key_env`
naming a variable that is not set.

### Kimi coding plan

Kimi's coding plan is served over an Anthropic-compatible endpoint, so it
needs no dedicated adapter:

```json
{
  "backends": {
    "kimi": {
      "kind": "anthropic",
      "base_url": "https://api.kimi.com/coding/",
      "api_key_env": "KIMI_API_KEY"
    }
  },
  "roles": {
    "coder": { "chain": ["kimi/k3-256k"] }
  }
}
```

The backend is named `kimi`, not `anthropic`. `AnthropicConfig` carries an
`id` taken from the config key, so an Anthropic-compatible provider is
named for what it actually is and can sit alongside a real `anthropic`
backend in the same config — route to either by name.

`api_key_env` names the variable holding the key rather than the key
itself, so the credential never lands in a config file. Get the key from
the Kimi Code console and export it as `KIMI_API_KEY`. If that variable is
unset at startup, Conway fails with an error naming it.

Two context variants ship in the bundled model metadata. The window is
selected by the model id itself, which is why they are separate entries:

| Model id | Context window |
|---|---|
| `k3-256k` | 262,144 tokens |
| `k3[1m]` | 1,048,576 tokens |

The `[1m]` suffix is literal — part of the id the provider expects. Both
declare streaming validated tool-calling and `reasoning = true` (K3 thinks
by default). Override either in a `[models.metadata_path]` file if the
provider's limits change.

A caveat worth stating plainly: third-party Anthropic-compatible shims do
not always implement everything Conway sends. Tool-use and thinking
behaviors in particular differ between providers. That is a provider-side
limitation — Conway reports what it gets back rather than papering over
the difference.

## Segment to wire message mapping

Both adapters implement the same job — translate an ordered
`Vec<PromptSegment>` (`conway-core`) plus a tool set into the provider's
wire request shape — in a `wire.rs` module private to the adapter
(`anthropic::wire`, `openai_compat::wire`). The mapping walks each
segment's `Role` and `ContentBlock`s:

- `Role::System` segments become the system message (Anthropic: a top-level
  `system` field; OpenAI-compatible: a `role: "system"` message).
- `Role::User`/`Role::Assistant` segments become `user`/`assistant`
  messages; `ContentBlock::Text` concatenates into the message's text.
- `ContentBlock::ToolUse` blocks on an assistant segment become the
  dialect's tool-call representation (Anthropic: a `tool_use` content
  block; OpenAI-compatible: the `tool_calls` array on the assistant
  message, with `arguments` JSON-encoded to a string).
- `ContentBlock::ToolResultBlock` blocks become the dialect's tool-result
  representation (Anthropic: a `tool_result` content block keyed by
  `tool_use_id`; OpenAI-compatible: a separate `role: "tool"` message keyed
  by `tool_call_id`).

Segment order is preserved verbatim — per the `Backend` port's contract,
adapters must not reorder, merge, or drop segments, since order is what
makes implicit-prefix caching hit.

## The 0.2.0 tool-call *and* tool-result fix

0.2.0 closed two related, sequential bugs in getting a tool round-trip to
actually reach the model, both diagnosed via `-vv` wire captures:

- **Tool calls reaching the model.** Earlier behavior produced a
  tool-call-only assistant turn with `content: null`, which OpenAI accepts
  but which Ollama Cloud / glm-5.2 rejects outright (`bad request: invalid
  message content type: <nil>`), failing every tool-continuation request.
  The fix (`openai_compat::wire::assistant_message`) always sends an empty
  **string** (`""`) for a tool-call-only assistant turn — accepted by every
  dialect, OpenAI included — never `null`.
- **Tool results reaching the model.** Both wire adapters serialize a tool
  result *only* from a `ContentBlock::ToolResultBlock` — it is the variant
  that carries the `call_id` the wire format keys on
  (`openai_compat::wire::tool_result_messages`,
  `anthropic::wire::tool_result_blocks`). Until 0.2.0's fix, the segment
  built by `conway-runtime`'s `ContextBuilder` for a `ToolResultRecord`
  carried a raw `ContentBlock::Text` instead, which matches neither
  adapter's tool-result mapping — the result was silently dropped from
  every request, and the model would see its own tool call but never the
  result, confabulating an answer instead of grounding on the real output.
  The fix lives in `conway-runtime` (`context/builder.rs`, wrapping the
  result into a canonical `ToolResultBlock`), but the root cause was
  precisely this crate's mapping functions requiring that specific block
  shape — see [`conway-runtime`](conway-runtime.md) for the fix site and
  [`conway-core`](conway-core.md) for the `ContentBlock` model both rely
  on.

Both fixes mean: a tool-call/tool-result turn now round-trips correctly
across every supported dialect, and this crate's own golden wire-format
tests fixture their input as a `ToolResultBlock` (matching the
representation the fixed context builder actually produces, not the
`Text`-only shape that previously slipped through untested).

## Reasoning / extended-thinking handling

Reasoning is wire-layer plumbing in this crate, not policy — whether or how
much a model reasons is a routing/config decision made above this crate.

- **Anthropic.** `Capabilities::reasoning` maps to a `thinking` request
  parameter with a token budget (`reasoning_budget_tokens`). Thinking
  blocks round-trip through `ContentBlock::Thinking { text, signature }`:
  a signed `thinking` block is emitted back verbatim on the next turn
  (`assistant_content_blocks`), and an *unsigned* thinking block is
  deliberately **omitted**, never sent unsigned — Anthropic requires the
  signature for the block to be valid on a later turn. `redacted_thinking`
  blocks (an opaque, encrypted payload with no plaintext reasoning) are
  carried through the same `ContentBlock::Thinking` variant, keyed by an
  empty `text` and the redacted payload in `signature`, and are round-tripped
  verbatim, never inspected.
- **OpenAI-compatible dialects.** A `reasoning_effort` request parameter is
  emitted only for the `OpenAi` dialect when the caller sets one
  (`reasoning_effort`). Reasoning-model dialects (DeepSeek-R1-style,
  served via vLLM/Ollama/LM Studio) return their reasoning trace in a
  `reasoning_content` response field (`reasoning` accepted as an alias);
  the non-streaming path surfaces it as a `ContentBlock::Thinking`, and
  `stream.rs` surfaces the streamed equivalent as `Event::ThinkingDelta`.

## Capability declaration

`Backend::capabilities(&ModelId) -> Capabilities` is resolved per
`(backend, model)`, never per-backend alone — quantization and chat
template change tool-call reliability independent of the server. Precedence
is fixed and the same for every field: **config `ModelOverrides` >
`ModelMetadata` entry (`model_metadata.rs`) > probed server value ("dialect
defaults" plus live discovery) > `DialectDefaults`** (`capabilities.rs`,
`dialect_defaults(Dialect) -> DialectDefaults`, profile-derived — see
"Declarative provider profiles" below). `build_capabilities` and
`resolve_model` in `capabilities.rs` implement that merge.

`CapabilityProbe` (`probe.rs`, `openai-compat`-gated) does best-effort
startup discovery against an OpenAI-compatible endpoint's model list and
server properties: a 5-second timeout, zero retries, and any failure
(transport error, non-2xx, malformed body) degrades to "this step found
nothing" rather than propagating — discovery is never a hard dependency,
and `discover` always returns `Ok`, falling back to `ModelMetadata` and
config overrides alone if every endpoint is unreachable. Discovery may only
*narrow* `max_context_tokens` (never raise it above the metadata/dialect
value) and never raises `tool_calling` or `reliability_tier` above their
configured/metadata values — it can only downgrade `reliability_tier` to
`Unknown` for the `llama_cpp_server` built-in profile when a chat template
is missing. The handful of extra discovery/probe steps (Ollama's
`/api/tags`/`/api/version` fallback, vLLM's `max_model_len`, llama.cpp's
`/props`) are matched by profile id, not by a declarative field — endpoint
selection for discovery is a different concern from the wire-behavior
fields a `Profile` declares, so a new or user-supplied profile simply gets
the generic `/models`-only probe every other built-in profile without a
named extra step already has. This probe is the profile-aware building
block `conway-routing`'s health prober composes into breaker-state
observations — see [`conway-routing`](conway-routing.md).

## Declarative provider profiles

`OpenAiCompatBackend` covers every OpenAI-compatible provider through one
adapter parameterized by a `conway_backends::profile::Profile` — a plain
data value, not a fixed Rust match arm. This is what lets a new provider be
added (and a user's own local server variant be described) without a code
change or recompile.

### The `Profile` type

```rust
pub struct Profile {
    pub id: String,
    pub chat_path: String,                  // default "/chat/completions"
    pub supports_stream_options: bool,       // send stream_options on a streamed request
    pub flatten_multiblock_user: bool,       // default true (conservative)
    pub sends_parallel_tool_calls: bool,     // emit the parallel_tool_calls request field
    pub uses_max_completion_tokens: bool,    // "max_completion_tokens" vs "max_tokens"
    pub sends_reasoning_effort: bool,        // emit the reasoning_effort request field
    pub tool_call_style: ToolCallStyle,      // Structured | Tolerant | HermesTextFallback
    pub cache: CacheMode,                    // baseline caching behavior (informational)
    pub tool_calling: ToolCallSupport,       // baseline capability
    pub max_context_tokens: u32,             // baseline capability
    pub structured_output: StructuredOutput, // baseline capability
    pub parallel_tool_calls: bool,           // baseline capability default
    pub reliability_tier: ReliabilityTier,   // baseline capability
}
```

`ToolCallStyle` is an enum, not a boolean, because "does this provider use
the Hermes inline-text fallback" is really "which parsing strategy does
this provider need" — a question with more than two answers once a third
provider needs a third strategy:

- `Structured` — canonical OpenAI-shaped `delta.tool_calls`, no inline-text
  scanning. Also the right choice for a well-behaved but otherwise
  undocumented provider.
- `Tolerant` — a superset parser that additionally accepts a complete
  JSON-object `arguments` value instead of only a string fragment
  (ollama#12557, codex#7517). The conservative default for an unfamiliar
  server.
- `HermesTextFallback` — structured parsing (as above) *plus* scanning
  `delta.content` for an inline `<tool_call>...</tool_call>` block some
  vLLM/Hermes servers emit instead of a structured delta (vllm#31871).

Every field is `#[serde(default)]`, and every default is the most
conservative behavior a completely undescribed provider could have: no
`stream_options`, flattened (not array) multi-block content, no
`parallel_tool_calls`, `max_tokens` (not `max_completion_tokens`), no
`reasoning_effort`, the `Tolerant` parser, `NonStreamingOnly` tool support,
no structured output, `Unknown` reliability. A profile with only `id` set
still loads and behaves this way. `ProfileRaw` (the wire shape `Profile`
deserializes through) sets `deny_unknown_fields`: an unrecognized field in
a hand-authored profile is far more likely to be a typo than a
forward-compatible new field, and a typo that silently keeps its default
changes what conway sends to a real provider — a loud, named parse error is
the safer failure mode. This does mean a profile written for a *future*
conway version's new field will fail to load on an older build rather than
degrading gracefully; a profile written today, with fewer fields than a
later version recognizes, always loads unchanged (every missing field
takes its documented default).

A malformed profile — a bad TOML file, an unrecognized field, an empty
`id` — is always a typed `ConfigError::Profile { path, detail }` naming the
offending field, never a panic.

### Built-in profiles

Six profiles are embedded at compile time
(`conway_backends::profile::BUILT_IN_PROFILES`), reproducing this crate's
five original dialects' behavior exactly, plus Kimi:

| id | stream_options | multi-block | parallel_tool_calls (wire) | max tokens field | tool-call style | reliability |
|---|---|---|---|---|---|---|
| `openai` | yes | array | yes | `max_completion_tokens` | Structured | verified |
| `ollama` | yes | flattened | no | `max_tokens` | Tolerant | unknown |
| `vllm_hermes` | no | flattened | no | `max_tokens` | HermesTextFallback | community |
| `lm_studio` | no | flattened | no | `max_tokens` | Tolerant | unknown |
| `llama_cpp_server` | no | flattened | no | `max_tokens` | Tolerant | community |
| `kimi` | yes | array | no | `max_tokens` | Structured | community |

`crate::config::Dialect` — the five-variant enum this crate shipped before
declarative provider profiles — is **kept, not deprecated**, as a small
`Copy` convenience name for the first five rows: `Dialect::Ollama.profile()`
resolves it to the equivalent `Profile` value, which is the actual source
of truth every one of `Dialect`'s own predicate methods (`chat_path()`,
`supports_stream_options()`, ...) now reads. Every existing call site that
names one of these five dialects by Rust identifier continues to compile
and behave identically. A **new** provider, however, is never added as a
sixth `Dialect` variant — see the next section.

### Adding a provider: `.conway/profiles.toml`

A user-supplied provider needs no recompile. Add a `[[profile]]` table to
`.conway/profiles.toml` (project-scoped, next to `.conway/settings.json`)
or the global `~/.conway/profiles.toml` (or `$XDG_CONFIG_HOME/conway/`) —
the same project-then-global discovery layering
`conway::config::discovery::permission_file_paths` already establishes for
permission rules, reused here as
`conway::config::discovery::provider_profile_file_paths`:

```toml
[[profile]]
id = "my-local-server"
supports_stream_options = true
tool_call_style = "tolerant"
max_context_tokens = 65536
reliability_tier = "community"

[profile.cache]
kind = "implicit_prefix"
min_prefix_tokens = 0
```

A backend entry selects a profile the same way it already selects one of
the five built-in dialects — `backends.<id>.dialect` names it by id:

```json
{
  "backends": {
    "local": {
      "kind": "openai-compat",
      "base_url": "http://localhost:8000/v1",
      "dialect": "my-local-server"
    }
  }
}
```

A project or global file's profile **shadows** a built-in (or another
file's) profile with the same id — this is a deliberate override
mechanism, not a collision error, but it is never silent:
`ProfileStore::merge_file` records the previous origin in
`LoadedProfile::shadows`, and `ProfileStore::list()` is the "what is
loaded" inspection surface — every loaded profile, its origin
(`ProfileOrigin::BuiltIn` or `ProfileOrigin::File(path)`), and what it
shadowed, if anything. The same principle `conway_runtime::permission`'s
`active_patterns()` establishes for permission rules applies here: a rule
set nobody can inspect is a trap.

### Kimi (Moonshot platform API)

`kimi` names Moonshot's platform API (`https://api.moonshot.ai/v1`) — a
different product from the already-shipped **Kimi Code** path (`kind =
"anthropic"`, `base_url = "https://api.kimi.com/coding/"`, see "Kimi coding
plan" above). Configure it as an `openai-compat` backend:

```json
{
  "backends": {
    "kimi": {
      "kind": "openai-compat",
      "base_url": "https://api.moonshot.ai/v1",
      "dialect": "kimi",
      "api_key_env": "MOONSHOT_API_KEY"
    }
  }
}
```

The `kimi` profile sends structured tool calls (not the Hermes text
fallback), keeps multi-block user content as an array (does not flatten
it), and never sends `parallel_tool_calls` — undocumented on Kimi's
platform API, so conway assumes an unrecognized-field `400` rather than
guessing. `max_context_tokens` defaults conservatively to 32,768; override
per model via `models.json`/`metadata_path` for a specific Moonshot model
known to support more.

Three additional Kimi quirks are documented here, deliberately
**undocumented as capability flags and not worked around** (a standing
decision for this crate: pass values through and let the provider's own
error, or behavior, be loud rather than conway silently rewriting a
request):

- **Temperature range is `[0, 1]`, not the `[0, 2]` most OpenAI-compatible
  servers accept.** A caller-supplied `temperature` outside that range is
  sent as-is; Kimi's own validation error is what a caller sees.
- **`tool_choice: "required"` is rejected by Kimi's k2.5/k2.6 models but
  accepted by k3.** This is per-model, not per-provider, so it cannot be a
  `Profile` field — the provider profile is the same regardless of which
  Kimi model a request targets.
- **Prompt caching is automatic** and only engages above a 256-token
  prompt — `kimi`'s `Profile::cache` documents this
  (`ImplicitPrefix { min_prefix_tokens: 256 }`), informational only (cache
  hints are never wire-gating in this crate; see "Segment to wire message
  mapping" below).

### llama.cpp server

The `llama_cpp_server` built-in profile predates this item (it already
existed as `Dialect::LlamaCppServer`); this item's job included verifying
its settings are actually right for a real `llama-server`, not just
assumed. They are: `llama-server`'s OpenAI-compatible endpoint does not
reliably emit `stream_options`-driven usage on every build/template
combination (`supports_stream_options = false`), does not document
`parallel_tool_calls` support, and its structured-output support is
grammar-constrained (GBNF) rather than JSON-schema in the OpenAI sense
(`structured_output = "grammar"`). Its tool-call streaming shape varies
across chat templates in ways the tolerant parser already accommodates
(`tool_call_style = "tolerant"`), and `reliability_tier = "community"` with
`NonStreamingOnly` tool support matches the same "unverified until proven"
posture every other self-hosted/community server in this crate gets. This
is a **no-op**: the existing settings were already correct, and this
section exists to document that judgment rather than leave it unstated.

### `cached_tokens`: reading either wire shape

Kimi's Moonshot platform API reports `usage.cached_tokens` at the **top
level**, while OpenAI nests it under
`usage.prompt_tokens_details.cached_tokens`. `openai_compat::wire::map_usage`
reads either shape unconditionally — nested wins if a (hypothetical)
server sends both, otherwise whichever is present, else `0` — rather than
gating this on a `Profile` field. This is deliberately *not* per-provider
knowledge: accepting an optional top-level field is strictly more
permissive (a server that only ever sends the nested shape, like today's
OpenAI, is completely unaffected), so there is no provider-specific
behavior to declare and no reason to make the profile schema carry it.

## Streaming tool calls

`tool_calls::ToolCallAccumulator` (`tool_calls/mod.rs`) is a
`ToolCallStyle`-parameterized state machine that reconstructs complete
tool calls from streamed deltas — isolated here specifically because it is
the most bug-prone surface of the OpenAI-compatible streaming format.
Providers stream a tool call as a sequence of partial deltas (an
`index`/`id` seen once or repeatedly, `arguments` arriving as JSON
fragments only valid once concatenated), and real-world servers deviate
from the OpenAI-canonical shape in observed, reproducible ways. The
accumulator holds one `Slot` per in-flight call keyed by `resolve_key`,
with style-specific quirk handling for Hermes-style tool calls
(`tool_calls/hermes.rs`), the tolerant parser (`tool_calls/ollama.rs`), and
canonical structured deltas (`tool_calls/openai.rs`), plus shared
argument-JSON validation (`tool_calls/validate.rs`).

## How it fits the whole

`conway-backends` is a leaf adapter crate: it depends only on
[`conway-core`](conway-core.md) and has no workspace dependents that are
themselves adapters. [`conway-routing`](conway-routing.md) consumes
`Backend::capabilities`/`probe` to build its capability index and health
observations; [`conway-runtime`](conway-runtime.md) calls
`Backend::generate`/`stream` to actually execute a routed turn. See
[`/ARCHITECTURE.md`](/ARCHITECTURE.md) for the full data flow of one turn.
