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
  speaks to any of five OpenAI-compatible dialects: **OpenAI**, **Ollama**,
  **vLLM/Hermes**, **LM Studio**, and **llama.cpp server**. These are not
  five separate backend types; `Dialect` is a small enum that parameterizes
  a single adapter's request/response shaping (`dialect.rs`), because all
  five speak the same `/chat/completions`-shaped protocol with small,
  well-characterized deviations.

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
`dialect_defaults(Dialect) -> DialectDefaults`, one function per dialect:
`openai_defaults`, `ollama_defaults`, `vllm_hermes_defaults`,
`lm_studio_defaults`, `llama_cpp_server_defaults`, `anthropic_defaults`).
`build_capabilities` and `resolve_model` in `capabilities.rs` implement
that merge.

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
`Unknown` for `Dialect::LlamaCppServer` when a chat template is missing.
This probe is the dialect-aware building block `conway-routing`'s health
prober composes into breaker-state observations — see
[`conway-routing`](conway-routing.md).

## Streaming tool calls

`tool_calls::ToolCallAccumulator` (`tool_calls/mod.rs`) is a
dialect-parameterized state machine that reconstructs complete tool calls
from streamed deltas — isolated here specifically because it is the most
bug-prone surface of the OpenAI-compatible streaming format. Providers
stream a tool call as a sequence of partial deltas (an `index`/`id` seen
once or repeatedly, `arguments` arriving as JSON fragments only valid once
concatenated), and real-world servers deviate from the OpenAI-canonical
shape in observed, reproducible ways. The accumulator holds one `Slot` per
in-flight call keyed by `resolve_key`, with dialect-specific quirk handling
for Hermes-style tool calls (`tool_calls/hermes.rs`), Ollama
(`tool_calls/ollama.rs`), and canonical OpenAI (`tool_calls/openai.rs`),
plus shared argument-JSON validation (`tool_calls/validate.rs`).

## How it fits the whole

`conway-backends` is a leaf adapter crate: it depends only on
[`conway-core`](conway-core.md) and has no workspace dependents that are
themselves adapters. [`conway-routing`](conway-routing.md) consumes
`Backend::capabilities`/`probe` to build its capability index and health
observations; [`conway-runtime`](conway-runtime.md) calls
`Backend::generate`/`stream` to actually execute a routed turn. See
[`/ARCHITECTURE.md`](/ARCHITECTURE.md) for the full data flow of one turn.
