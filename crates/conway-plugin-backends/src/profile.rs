//! Declarative provider profiles: the per-provider wire-behavior and
//! baseline-capability data that used to be five `matches!(dialect, ...)`
//! arms scattered across `openai_compat/dialect.rs`, `openai_compat/wire.rs`,
//! and `tool_calls/mod.rs`.
//!
//! The whole surface those arms encoded turned out to be small and uniform:
//! a chat-completions path (provider-invariant today, kept as a field for
//! future flexibility), six wire-behavior booleans/enum, and the six
//! baseline `Capabilities` fields [`crate::capabilities::DialectDefaults`]
//! already grouped. [`Profile`] is that struct. The five dialects this
//! crate shipped before this item (`openai`, `ollama`, `vllm_hermes`,
//! `lm_studio`, `llama_cpp_server`) are now [`Profile`] values embedded as
//! data (`BUILT_IN_PROFILES`) rather than Rust match arms, plus a sixth,
//! `kimi` (the Moonshot platform API — distinct from the already-shipped
//! Kimi Code Anthropic-compatible path, see `docs/providers.md`).
//! `crate::config::Dialect` is kept as a small, `Copy`, five-variant
//! convenience enum resolving to one of these built-ins ([`Dialect::profile`])
//! for source compatibility with every existing call site; it is not
//! deprecated, but it can no longer name a provider this crate doesn't
//! already ship code for — a *new* provider (Kimi, llama.cpp, or anything a
//! user points `openai-compat` at next) is added as a [`Profile`] value,
//! never as a new `Dialect` variant.
//!
//! # Why every field is `#[serde(default)]`
//!
//! A hand-authored or embedded profile file is untrusted, evolving input:
//! one written against today's field set must still load
//! unchanged after a later conway version adds a new field (the missing
//! field takes its documented, conservative default), and a typo in a field
//! name must be a loud, typed error naming the field rather than a silently
//!-defaulted behavior change. Those two requirements point at different
//! knobs — `#[serde(default)]` on every field solves the first; rejecting
//! *unrecognized* fields solves the second. `ProfileRaw` (the wire shape
//! [`Profile`] deserializes through, mirroring `AnthropicConfigRaw`'s
//! `TryFrom` pattern in `config.rs`) sets `#[serde(deny_unknown_fields)]` for
//! exactly this reason: a field this binary doesn't recognize is far more
//! likely to be a typo (`"sends_paralel_tool_calls"`) than a
//! forward-compatible field from a newer schema, since profiles are small,
//! hand-authored, and one flipped boolean silently changes what gets sent
//! over the wire to a real provider. The trade-off this accepts: a profile
//! file authored for a *future* conway version that adds a new field will
//! fail to load on this build rather than degrading gracefully. Given the
//! severity of a silently-wrong wire behavior versus a loud load failure a
//! user can act on immediately, the loud failure is the better default.
//! Every field still has a conservative default, so an *old* profile
//! (fewer fields than this build recognizes) always loads unchanged.
//!
//! Every default is deliberately the most conservative behavior a
//! completely undescribed provider could have: no `stream_options`, a
//! flattened (not array) multi-block user message, no
//! `parallel_tool_calls`, `max_tokens` (not `max_completion_tokens`), no
//! `reasoning_effort`, the tolerant delta parser with no Hermes text
//! scanning, non-streaming-only tool support, no structured output, and
//! `Unknown` reliability — mirroring the existing dialects' own documented
//! "conservative when unverified" convention.
//!
//! # S4b: storage/selection/merge-order/error-reporting moved to `crate::profile_store`
//!
//! `ProfileStore`/`ProfileOrigin`/`LoadedProfile` used to be defined in this
//! module, hardcoded to hold `Profile` values. They are now `crate::
//! profile_store::{ProfileStore<T>, ProfileOrigin, LoadedProfile<T>}` —
//! generic, kind-agnostic, shared with `"anthropic"`'s own profile support
//! (`crate::factory::AnthropicBackendFactory`, via `crate::profile_store::
//! ProfileBundle`) — see that module's own doc for why one generic facility
//! replaces what would otherwise become a second per-kind store. `Profile`
//! itself is unchanged: every field, every default, `ProfileRaw`'s
//! `deny_unknown_fields`, `TryFrom<ProfileRaw>`'s empty-id rejection — all
//! identical to before this item. What changed is only that `Profile` now
//! implements `crate::profile_store::Profiled` (this module's own answer to
//! "how do I parse a `[[profile]]` source string, and what is my selection
//! key") instead of a `ProfileStore` hardcoded to it, and `ProfileStore` here
//! is a type alias (plus one inherent-impl `built_ins()` constructor) over
//! the generic type, so every existing call site (`ProfileStore::built_ins()`,
//! `.get(id) -> Option<&Profile>`, `.merge_file(path)`, `.list()`, `.len()`,
//! `.is_empty()`) compiles unchanged.

use conway_core::capabilities::{
    CacheMode, CacheTtl, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use serde::Deserialize;

use crate::capabilities::DialectDefaults;
use crate::config::Dialect;
use crate::model_metadata::{StructuredOutputSpec, ToolCallSupportSpec};
use crate::profile_store::Profiled;
use crate::tool_calls::ToolCallStyle;

fn default_chat_path() -> String {
    "/chat/completions".to_string()
}

fn default_flatten_multiblock_user() -> bool {
    true
}

/// The `max_context_tokens` every built-in profile ships (and what an
/// undescribed model of ANY dialect ultimately falls back to once neither a
/// `ModelMetadata` entry — bundled `DEFAULTS`, a `metadata_path` file, or a
/// live `probe_on_startup` hint — nor a `models.json`/`ModelOverrides` entry
/// says anything about it). This is a genuinely LAST-RESORT floor, not a
/// routine per-model answer: a context window is a property of a *model*,
/// not of a wire dialect, and Ollama alone serves windows from 4K to 1M
/// tokens under the identical `"ollama"` profile — one number cannot be
/// right for more than one model here except by coincidence.
///
/// Kept deliberately conservative (not raised) rather than picking a larger
/// number that would be equally unsourced for whatever unfamiliar server
/// actually answers `"ollama"`/`"lm_studio"`/etc: the real fix for an
/// under-declared model is a `ModelMetadata`/`models.json` entry for it (see
/// `model_metadata.rs`'s bundled `DEFAULTS`, which is where `glm-5.2`'s
/// correct window now lives — this constant is exactly the
/// number that was silently governing it before that entry existed), never
/// a bigger guess at this single per-dialect layer. What changed instead:
/// `crate::capabilities::{ContextTokensSource, max_context_tokens_source}`
/// makes it possible to tell, for any given resolution, whether this floor
/// governed or a real declaration did — see that type's doc for the
/// incident (an operator refused at 36,288 tokens against a model that, on
/// the same endpoint, was independently recorded accepting 61,667) that
/// made "kept but silent" no longer acceptable.
///
/// **2026-08-30 addendum, board item (context-window declaration honesty,
/// num_ctx):** the operator ruled explicitly that this number must never be
/// swapped for a different invented one (200,000 was proposed and rejected
/// for exactly this reason — "we don't want to pick an arbitrary number").
/// This constant is UNCHANGED (`32_768`) and is now understood, precisely,
/// as what it always actually was: an internal admission-safety clamp, not
/// a claim about any real model's window. What DID change is that this
/// value's [`ContextTokensSource`](crate::capabilities::ContextTokensSource)
/// is now [`Unverified`](crate::capabilities::ContextTokensSource::Unverified)
/// for every profile whose [`Profile::context_window_verified`] is `false`
/// (every built-in profile here except `openai`) rather than the same
/// [`DialectDefaultFloor`](crate::capabilities::ContextTokensSource::DialectDefaultFloor)
/// a genuinely curated per-provider figure gets — a caller/operator-facing
/// surface can now tell "this cap is real" apart from "this cap is a
/// last-resort clamp conway made up" even though both currently resolve to
/// the identical `u32`. The operator's own preferred remedy for the
/// `Unverified` case is establishing a real value BEFORE this floor is ever
/// reached — see `docs/providers.md`'s "context window" section for the
/// setup-time discover-or-ask flow this floor exists to be a safety net
/// under, not a replacement for.
fn default_max_context_tokens() -> u32 {
    32_768
}

fn default_reliability_tier() -> ReliabilityTier {
    ReliabilityTier::Unknown
}

fn default_tool_calling() -> ToolCallSupportSpec {
    ToolCallSupportSpec::NonStreaming
}

fn default_structured_output() -> StructuredOutputSpec {
    StructuredOutputSpec::None
}

/// Hand-authorable wire vocabulary for [`Profile::cache`]:
/// `{ kind = "implicit_prefix", min_prefix_tokens = 1024 }`. An
/// internally-tagged mirror of `conway_core::capabilities::CacheMode`, which
/// is *externally* tagged (`cache = { implicit_prefix = { min_prefix_tokens
/// = 1024 } }` in TOML) — awkward to hand-write compared to this flatter
/// `kind`-tagged shape.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum CacheSpec {
    ImplicitPrefix {
        #[serde(default)]
        min_prefix_tokens: u32,
    },
    ExplicitBreakpoints {
        #[serde(default = "default_max_breakpoints")]
        max_breakpoints: u8,
        #[serde(default = "default_ttls")]
        ttls: Vec<CacheTtl>,
    },
    SlotKv,
    #[default]
    None,
}

fn default_max_breakpoints() -> u8 {
    4
}

fn default_ttls() -> Vec<CacheTtl> {
    vec![CacheTtl::FiveMinutes, CacheTtl::OneHour]
}

impl CacheSpec {
    fn to_cache_mode(&self) -> CacheMode {
        match self {
            CacheSpec::ImplicitPrefix { min_prefix_tokens } => CacheMode::ImplicitPrefix {
                min_prefix_tokens: *min_prefix_tokens,
            },
            CacheSpec::ExplicitBreakpoints {
                max_breakpoints,
                ttls,
            } => CacheMode::ExplicitBreakpoints {
                max_breakpoints: *max_breakpoints,
                ttls: ttls.clone(),
            },
            CacheSpec::SlotKv => CacheMode::SlotKv,
            CacheSpec::None => CacheMode::None,
        }
    }
}

/// The wire shape [`Profile`] deserializes through — see the module doc's
/// "Why every field is `#[serde(default)]`" section for the
/// `deny_unknown_fields` reasoning.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileRaw {
    id: String,
    #[serde(default = "default_chat_path")]
    chat_path: String,
    #[serde(default)]
    supports_stream_options: bool,
    #[serde(default = "default_flatten_multiblock_user")]
    flatten_multiblock_user: bool,
    #[serde(default)]
    sends_parallel_tool_calls: bool,
    #[serde(default)]
    uses_max_completion_tokens: bool,
    #[serde(default)]
    sends_reasoning_effort: bool,
    #[serde(default)]
    tool_call_style: ToolCallStyle,
    #[serde(default)]
    cache: CacheSpec,
    #[serde(default = "default_tool_calling")]
    tool_calling: ToolCallSupportSpec,
    #[serde(default = "default_max_context_tokens")]
    max_context_tokens: u32,
    /// See [`Profile::context_window_verified`]. `#[serde(default)]` (never
    /// a `default = "fn"` override): the safe default for a profile that
    /// does not mention this field at all is `false` -- an unfamiliar
    /// provider's `max_context_tokens` is a placeholder until something
    /// says otherwise, never treated as sourced fact by omission.
    #[serde(default)]
    context_window_verified: bool,
    /// See [`Profile::sends_num_ctx`]. `#[serde(default)]`: `false` unless a
    /// profile explicitly opts in.
    #[serde(default)]
    sends_num_ctx: bool,
    #[serde(default = "default_structured_output")]
    structured_output: StructuredOutputSpec,
    #[serde(default)]
    parallel_tool_calls: bool,
    #[serde(default = "default_reliability_tier")]
    reliability_tier: ReliabilityTier,
}

impl TryFrom<ProfileRaw> for Profile {
    type Error = String;

    fn try_from(raw: ProfileRaw) -> Result<Self, String> {
        if raw.id.trim().is_empty() {
            return Err("id must not be empty".to_string());
        }
        Ok(Profile {
            id: raw.id,
            chat_path: raw.chat_path,
            supports_stream_options: raw.supports_stream_options,
            flatten_multiblock_user: raw.flatten_multiblock_user,
            sends_parallel_tool_calls: raw.sends_parallel_tool_calls,
            uses_max_completion_tokens: raw.uses_max_completion_tokens,
            sends_reasoning_effort: raw.sends_reasoning_effort,
            tool_call_style: raw.tool_call_style,
            cache: raw.cache.to_cache_mode(),
            tool_calling: raw.tool_calling.to_capability(),
            max_context_tokens: raw.max_context_tokens,
            context_window_verified: raw.context_window_verified,
            sends_num_ctx: raw.sends_num_ctx,
            structured_output: raw.structured_output.to_capability(),
            parallel_tool_calls: raw.parallel_tool_calls,
            reliability_tier: raw.reliability_tier,
        })
    }
}

/// One provider's declarative wire behavior and baseline capabilities —
/// what used to be a `Dialect` match arm in `dialect.rs`/`wire.rs`/
/// `tool_calls/mod.rs` plus a `capabilities.rs` `*_defaults()` function.
///
/// Constructed only via `ProfileRaw`'s `TryFrom` (`#[serde(try_from =
/// "ProfileRaw")]`), so a `Profile` value can never carry an empty `id`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "ProfileRaw")]
pub struct Profile {
    /// The name a backend config or `Dialect` selects this profile by
    /// (`"openai"`, `"kimi"`, ...). Never empty.
    pub id: String,
    /// The chat-completions endpoint path, relative to `base_url`. Every
    /// built-in profile shares `/chat/completions` (provider-invariant
    /// today); kept as a field rather than a hardcoded constant so a future
    /// provider that genuinely differs does not need a code change.
    pub chat_path: String,
    /// Whether to send `"stream_options":{"include_usage":true}` on a
    /// streamed request.
    pub supports_stream_options: bool,
    /// Whether a multi-text-block `User` segment is flattened to one
    /// `\n\n`-joined string (`true`) or kept as an OpenAI-shaped
    /// `[{"type":"text","text":...}]` array (`false`).
    pub flatten_multiblock_user: bool,
    /// Whether to send `"parallel_tool_calls"` on a tools-bearing request
    /// when the resolved model capability allows it. Distinct from
    /// [`Profile::parallel_tool_calls`] below (the *capability default*,
    /// i.e. whether an undescribed model of this provider is assumed to
    /// support multiple simultaneous tool calls at all): a provider can in
    /// principle support multiple calls per turn without accepting the
    /// OpenAI-specific request hint that asks for them.
    pub sends_parallel_tool_calls: bool,
    /// Whether the max-output-tokens request field is named
    /// `"max_completion_tokens"` (`true`, OpenAI's current field) or
    /// `"max_tokens"` (`false`, the OpenAI-compatible-server-wide legacy
    /// name every other provider in this crate's history has accepted).
    pub uses_max_completion_tokens: bool,
    /// Whether to emit a caller-supplied `"reasoning_effort"` request
    /// field.
    pub sends_reasoning_effort: bool,
    /// Which streamed tool-call parsing strategy this provider needs — see
    /// [`ToolCallStyle`].
    pub tool_call_style: ToolCallStyle,
    /// Baseline prompt-caching behavior (informational: never gates wire
    /// output in this crate — see `openai_compat::wire`'s module doc).
    pub cache: CacheMode,
    /// Baseline tool-calling support level.
    pub tool_calling: ToolCallSupport,
    /// Baseline context window, in tokens.
    pub max_context_tokens: u32,
    /// Whether [`Profile::max_context_tokens`] is a real, sourced fact about
    /// this PROVIDER as a whole (`openai`'s `128_000`, OpenAI's own
    /// documented floor across its mainstream chat-completions models) --
    /// board item (context-window declaration honesty, num_ctx): `true`
    /// makes `crate::capabilities::max_context_tokens_source` report
    /// [`crate::capabilities::ContextTokensSource::DialectDefaultFloor`]
    /// when this layer governs; `false` (every built-in profile except
    /// `openai`) makes it report
    /// [`crate::capabilities::ContextTokensSource::Unverified`] instead --
    /// `max_context_tokens`'s NUMBER is unchanged either way (still `32768`
    /// for `ollama`/`vllm_hermes`/`lm_studio`/`llama_cpp_server`/`kimi`, kept
    /// only as an internal admission-safety clamp, see
    /// `default_max_context_tokens`'s own doc for why a real per-model
    /// figure cannot exist at this layer for those five) -- what changes is
    /// only whether a caller is told this number is a fact or a guess. A
    /// hand-authored profile that omits this field gets the conservative
    /// `false` (`#[serde(default)]`, no named default fn): an unfamiliar
    /// provider is never assumed verified by silence.
    pub context_window_verified: bool,
    /// Whether this profile expresses a resolved context window on the wire
    /// as Ollama's native `options.num_ctx` (`true`, `ollama` only today) or
    /// has no equivalent to send (`false`, every other built-in profile) --
    /// board item (context-window declaration honesty, num_ctx). Empirically
    /// confirmed 2026-08-30 against a live local Ollama 0.32.13: Ollama's
    /// OpenAI-compatible `/v1/chat/completions` endpoint silently IGNORES a
    /// passed `options`/`num_ctx` field (verified via `GET /api/ps`
    /// reporting the server's own unrequested default after the call, not
    /// the value that was sent) -- only the NATIVE `/api/chat` endpoint
    /// honours it. `sends_num_ctx = true` is therefore also the switch
    /// `OpenAiCompatBackend` reads to route a request through the native
    /// endpoint instead of the OpenAI-compatible one when (and only when) a
    /// real context window was actually resolved to request -- see
    /// `openai_compat/ollama_native.rs`'s module doc for the full
    /// dialect-split rationale and its cost.
    pub sends_num_ctx: bool,
    /// Baseline structured-output support.
    pub structured_output: StructuredOutput,
    /// Baseline "does an undescribed model of this provider support
    /// multiple tool calls in one turn" capability default — see
    /// [`Profile::sends_parallel_tool_calls`] for how this differs from the
    /// wire-level gate.
    pub parallel_tool_calls: bool,
    /// Baseline reliability tier.
    pub reliability_tier: ReliabilityTier,
}

impl Profile {
    /// Projects this profile's six baseline-capability fields into a
    /// [`DialectDefaults`], the shape `crate::capabilities::build_capabilities`
    /// consumes. The single conversion point that makes every built-in
    /// dialect's `capabilities.rs` `*_defaults()` function profile-derived.
    pub fn dialect_defaults(&self) -> DialectDefaults {
        DialectDefaults {
            cache: self.cache.clone(),
            tool_calling: self.tool_calling,
            max_context_tokens: self.max_context_tokens,
            context_window_verified: self.context_window_verified,
            structured_output: self.structured_output,
            parallel_tool_calls: self.parallel_tool_calls,
            reliability_tier: self.reliability_tier,
        }
    }
}

/// Wire shape of a profile file: an array-of-tables, `[[profile]]` —
/// mirrors `model_metadata.rs`'s `[[model]]` file shape.
#[derive(Debug, Deserialize)]
struct ProfileFile {
    #[serde(default)]
    profile: Vec<Profile>,
}

/// S4b: `Profile`'s own answer to `crate::profile_store::Profiled` —
/// unchanged parsing (`ProfileFile`/`toml::from_str`, the exact
/// `#[serde(try_from = "ProfileRaw")]` deserialization this module already
/// ran before this item), now behind the trait the kind-agnostic facility
/// dispatches through rather than a hardcoded `ProfileStore::parse`.
impl Profiled for Profile {
    fn id(&self) -> &str {
        &self.id
    }

    fn parse_source(source: &str) -> Result<Vec<Self>, String> {
        let file: ProfileFile = toml::from_str(source).map_err(|err| err.to_string())?;
        Ok(file.profile)
    }
}

/// The five dialects this crate shipped before declarative provider
/// profiles, plus `kimi` (the Moonshot platform API — distinct from the
/// already-shipped Kimi Code Anthropic-compatible path; see
/// `docs/providers.md`), embedded at compile time. Every value
/// here must reproduce this crate's pre-existing per-dialect behavior
/// exactly — the regression net for that is `tests/dialect_conformance.rs`
/// and `openai_compat/wire.rs`'s/`capabilities.rs`'s own unit tests, which
/// now assert against these profile-derived answers rather than literal
/// `matches!` arms.
const BUILT_IN_PROFILES: &str = r#"
[[profile]]
id = "openai"
chat_path = "/chat/completions"
supports_stream_options = true
flatten_multiblock_user = false
sends_parallel_tool_calls = true
uses_max_completion_tokens = true
sends_reasoning_effort = true
tool_call_style = "structured"
tool_calling = "streaming_validated"
max_context_tokens = 128000
# OpenAI's own documented context window across its current mainstream
# chat-completions models (gpt-4o/gpt-4o-mini/gpt-4-turbo family) -- a real,
# sourced figure, not a last-resort placeholder. See
# `default_max_context_tokens`'s doc for the distinction this flag makes
# discoverable (board item: context-window declaration honesty, num_ctx).
context_window_verified = true
structured_output = "json_schema"
parallel_tool_calls = true
reliability_tier = "verified"

[profile.cache]
kind = "implicit_prefix"
min_prefix_tokens = 1024

[[profile]]
id = "ollama"
chat_path = "/chat/completions"
supports_stream_options = true
flatten_multiblock_user = true
sends_parallel_tool_calls = false
uses_max_completion_tokens = false
sends_reasoning_effort = false
tool_call_style = "tolerant"
tool_calling = "non_streaming"
max_context_tokens = 32768
# 32768 here is NOT a sourced fact about any real Ollama model -- Ollama
# alone serves windows from 4K to 1M+ tokens under this identical profile;
# `context_window_verified` (omitted -- defaults false) reports this
# resolution as `Unverified`, not `DialectDefaultFloor`, when it governs.
# `sends_num_ctx = true`: Ollama's NATIVE `/api/chat` (never its
# OpenAI-compatible `/v1/chat/completions`, confirmed 2026-08-30 to
# silently ignore an `options`/`num_ctx` field) honours a requested context
# window via `options.num_ctx` -- see `openai_compat/ollama_native.rs`.
sends_num_ctx = true
structured_output = "json_schema"
parallel_tool_calls = false
reliability_tier = "unknown"

[profile.cache]
kind = "implicit_prefix"
min_prefix_tokens = 0

[[profile]]
id = "vllm_hermes"
chat_path = "/chat/completions"
supports_stream_options = false
flatten_multiblock_user = true
sends_parallel_tool_calls = false
uses_max_completion_tokens = false
sends_reasoning_effort = false
tool_call_style = "hermes_text_fallback"
tool_calling = "non_streaming"
max_context_tokens = 32768
structured_output = "json_schema"
parallel_tool_calls = true
reliability_tier = "community"

[profile.cache]
kind = "implicit_prefix"
min_prefix_tokens = 0

[[profile]]
id = "lm_studio"
chat_path = "/chat/completions"
supports_stream_options = false
flatten_multiblock_user = true
sends_parallel_tool_calls = false
uses_max_completion_tokens = false
sends_reasoning_effort = false
tool_call_style = "tolerant"
tool_calling = "non_streaming"
max_context_tokens = 32768
structured_output = "none"
parallel_tool_calls = false
reliability_tier = "unknown"

[profile.cache]
kind = "none"

[[profile]]
id = "llama_cpp_server"
chat_path = "/chat/completions"
supports_stream_options = false
flatten_multiblock_user = true
sends_parallel_tool_calls = false
uses_max_completion_tokens = false
sends_reasoning_effort = false
tool_call_style = "tolerant"
tool_calling = "non_streaming"
max_context_tokens = 32768
structured_output = "grammar"
parallel_tool_calls = false
reliability_tier = "community"

[profile.cache]
kind = "implicit_prefix"
min_prefix_tokens = 0

# Moonshot's platform API (`https://api.moonshot.ai/v1`) -- distinct from
# the already-shipped Kimi Code Anthropic-compatible path (`kind =
# "anthropic"`, `api.kimi.com/coding/`, v0.3.0). Structured (not
# text-fallback) tool calls, array (not flattened) multi-block user
# content, and no `parallel_tool_calls` request field -- undocumented on
# Kimi's platform API, so conway never sends it rather than guessing.
# `min_prefix_tokens = 256` documents Kimi's own automatic prompt-caching
# threshold (informational only -- see `Profile::cache`'s doc); conway
# implements no cache-hint workaround for it (decided: pass values through,
# let the server's own behavior/errors be loud). See
# `docs/providers.md` for the temperature-range and
# `tool_choice: "required"` quirks this profile deliberately does not
# encode -- conway passes values through rather than rewriting them.
[[profile]]
id = "kimi"
chat_path = "/chat/completions"
supports_stream_options = true
flatten_multiblock_user = false
sends_parallel_tool_calls = false
uses_max_completion_tokens = false
sends_reasoning_effort = false
tool_call_style = "structured"
tool_calling = "streaming_validated"
max_context_tokens = 32768
structured_output = "json_schema"
parallel_tool_calls = false
reliability_tier = "community"

[profile.cache]
kind = "implicit_prefix"
min_prefix_tokens = 256
"#;

/// S4b: a resolved set of [`Profile`]s — the compile-time
/// `BUILT_IN_PROFILES` plus zero or more user-supplied files layered over
/// them, each entry tracking its origin ([`ProfileOrigin`], re-exported
/// below) — the "what is loaded" inspection surface (`list`). A type ALIAS
/// over `crate::profile_store::ProfileStore<Profile>`, the ONE generic
/// profile facility, rather than a second store definition: see
/// `crate::profile_store`'s own module doc for why. Every method this
/// module's callers already use (`built_ins`, `get`, `merge_file`, `list`,
/// `len`, `is_empty`) still resolves — `built_ins` via the inherent impl
/// immediately below (specialized to `Profile`, which orphan rules permit
/// since both the generic type and `Profile` are local to this crate); the
/// rest are the generic type's own methods, unchanged in signature.
pub type ProfileStore = crate::profile_store::ProfileStore<Profile>;
pub use crate::profile_store::{LoadedProfile, ProfileOrigin};

impl crate::profile_store::ProfileStore<Profile> {
    /// The compile-time-embedded built-in profiles (see
    /// `BUILT_IN_PROFILES`), every entry's origin `ProfileOrigin::BuiltIn`.
    ///
    /// Panics only if the embedded TOML itself fails to parse — a
    /// compile-time-fixed invariant this module's own tests cover, never a
    /// possible runtime/user-input state (mirrors
    /// `ModelMetadataStore::defaults`'s identical `.expect()`).
    pub fn built_ins() -> Self {
        Self::from_source(BUILT_IN_PROFILES).expect("BUILT_IN_PROFILES must parse and validate")
    }
}

impl Dialect {
    /// The built-in profile id this fixed variant names — the single
    /// mapping every one of `Dialect`'s own predicate methods below
    /// resolves through.
    pub fn profile_id(self) -> &'static str {
        match self {
            Dialect::OpenAi => "openai",
            Dialect::Ollama => "ollama",
            Dialect::VllmHermes => "vllm_hermes",
            Dialect::LmStudio => "lm_studio",
            Dialect::LlamaCppServer => "llama_cpp_server",
        }
    }

    /// The resolved built-in [`Profile`] this dialect names. Every one of
    /// this crate's five original dialects always resolves (see
    /// `tests::every_dialect_resolves_to_a_built_in_profile` below); this
    /// method is the sole channel `Dialect`'s own predicate methods use, so
    /// they can never diverge from the profile data.
    pub fn profile(self) -> Profile {
        ProfileStore::built_ins()
            .get(self.profile_id())
            .cloned()
            .unwrap_or_else(|| panic!("built-in profile '{}' missing", self.profile_id()))
    }

    /// This dialect's baseline `DialectDefaults`, profile-derived.
    pub fn defaults(self) -> DialectDefaults {
        self.profile().dialect_defaults()
    }

    /// The chat-completions endpoint path, relative to `base_url`.
    pub fn chat_path(self) -> &'static str {
        // Every built-in profile shares this path today (see `Profile::chat_path`'s
        // doc); `'static` is safe here because `Dialect` is limited to the
        // five compile-time-fixed built-ins, never a user-supplied profile.
        "/chat/completions"
    }

    /// Whether to send `"stream_options":{"include_usage":true}` on a
    /// streamed request. Profile-derived.
    pub fn supports_stream_options(self) -> bool {
        self.profile().supports_stream_options
    }

    /// Whether a multi-text-block `User` segment is flattened to one
    /// `\n\n`-joined string (`true`) or kept as an OpenAI-shaped array
    /// (`false`). Profile-derived.
    pub fn flatten_multiblock_user(self) -> bool {
        self.profile().flatten_multiblock_user
    }

    /// Whether `dialect` sends `"parallel_tool_calls"` on a tools-bearing
    /// request. Profile-derived.
    pub fn sends_parallel_tool_calls(self) -> bool {
        self.profile().sends_parallel_tool_calls
    }

    /// Whether `dialect` uses the Hermes inline `<tool_call>...</tool_call>`
    /// text fallback. Profile-derived.
    pub fn uses_hermes_text_fallback(self) -> bool {
        matches!(
            self.profile().tool_call_style,
            ToolCallStyle::HermesTextFallback
        )
    }

    /// This dialect's [`ToolCallStyle`] — the parameter
    /// `tool_calls::ToolCallAccumulator::new` now takes in place of a raw
    /// `Dialect`.
    pub fn tool_call_style(self) -> ToolCallStyle {
        self.profile().tool_call_style
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigError;

    #[test]
    fn built_ins_parse_and_cover_every_fixed_dialect_and_kimi() {
        let store = ProfileStore::built_ins();
        for id in [
            "openai",
            "ollama",
            "vllm_hermes",
            "lm_studio",
            "llama_cpp_server",
            "kimi",
        ] {
            assert!(store.get(id).is_some(), "missing built-in profile {id}");
        }
        assert_eq!(store.len(), 6);
    }

    #[test]
    fn every_dialect_resolves_to_a_built_in_profile() {
        for dialect in [
            Dialect::OpenAi,
            Dialect::Ollama,
            Dialect::VllmHermes,
            Dialect::LmStudio,
            Dialect::LlamaCppServer,
        ] {
            let profile = dialect.profile();
            assert_eq!(profile.id, dialect.profile_id());
        }
    }

    #[test]
    fn kimi_profile_matches_the_documented_quirks() {
        let kimi = ProfileStore::built_ins().get("kimi").unwrap().clone();
        assert!(kimi.supports_stream_options);
        assert!(
            !kimi.flatten_multiblock_user,
            "array content, not flattened"
        );
        assert!(!kimi.sends_parallel_tool_calls);
        assert_eq!(kimi.tool_call_style, ToolCallStyle::Structured);
        assert_eq!(
            kimi.cache,
            CacheMode::ImplicitPrefix {
                min_prefix_tokens: 256
            },
            "documents Kimi's automatic 256-token caching threshold"
        );
    }

    /// Board item (context-window declaration honesty, num_ctx): `openai`
    /// is the one built-in profile whose `max_context_tokens` is a real,
    /// sourced figure; every other built-in is a placeholder clamp.
    #[test]
    fn only_openai_declares_its_context_window_verified() {
        assert!(Dialect::OpenAi.profile().context_window_verified);
        for dialect in [
            Dialect::Ollama,
            Dialect::VllmHermes,
            Dialect::LmStudio,
            Dialect::LlamaCppServer,
        ] {
            assert!(
                !dialect.profile().context_window_verified,
                "{dialect:?} has no sourced context-window figure at this layer"
            );
        }
        assert!(
            !ProfileStore::built_ins()
                .get("kimi")
                .unwrap()
                .context_window_verified,
            "kimi's 32768 is the same inherited placeholder as the other four"
        );
    }

    /// Only `ollama` has a confirmed native request field for a resolved
    /// context window (`options.num_ctx` via `/api/chat`) -- see
    /// `openai_compat/ollama_native.rs`.
    #[test]
    fn only_ollama_sends_num_ctx() {
        assert!(Dialect::Ollama.profile().sends_num_ctx);
        for dialect in [
            Dialect::OpenAi,
            Dialect::VllmHermes,
            Dialect::LmStudio,
            Dialect::LlamaCppServer,
        ] {
            assert!(!dialect.profile().sends_num_ctx, "{dialect:?}");
        }
    }

    #[test]
    fn merge_file_records_a_shadow_when_an_id_already_exists() {
        let dir = std::env::temp_dir().join(format!(
            "conway-profile-shadow-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profiles.toml");
        std::fs::write(
            &path,
            r#"
            [[profile]]
            id = "openai"
            chat_path = "/v2/chat/completions"
            "#,
        )
        .unwrap();

        let store = ProfileStore::built_ins().merge_file(&path).unwrap();
        let loaded = store
            .list()
            .into_iter()
            .find(|l| l.profile.id == "openai")
            .unwrap();
        assert_eq!(loaded.origin, ProfileOrigin::File(path.clone()));
        assert_eq!(loaded.shadows, Some(ProfileOrigin::BuiltIn));
        assert_eq!(loaded.profile.chat_path, "/v2/chat/completions");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn merge_file_missing_path_is_a_no_op_not_an_error() {
        let path = std::env::temp_dir().join("conway-profile-does-not-exist.toml");
        let _ = std::fs::remove_file(&path);
        let before = ProfileStore::built_ins();
        let before_len = before.len();
        let after = before.merge_file(&path).unwrap();
        assert_eq!(after.len(), before_len);
    }

    #[test]
    fn an_unrecognized_field_is_a_typed_error_naming_it_not_a_panic() {
        let dir = std::env::temp_dir().join(format!(
            "conway-profile-typo-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profiles.toml");
        std::fs::write(
            &path,
            r#"
            [[profile]]
            id = "typo-provider"
            sends_paralel_tool_calls = true
            "#,
        )
        .unwrap();

        let err = ProfileStore::built_ins()
            .merge_file(&path)
            .expect_err("an unrecognized field must be a typed error, not silently ignored");
        match err {
            ConfigError::Profile { detail, .. } => {
                assert!(
                    detail.contains("sends_paralel_tool_calls"),
                    "error must name the offending field: {detail}"
                );
            }
            other => panic!("expected ConfigError::Profile, got {other:?}"),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_id_is_a_typed_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!(
            "conway-profile-empty-id-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profiles.toml");
        std::fs::write(
            &path,
            r#"
            [[profile]]
            id = ""
            "#,
        )
        .unwrap();

        let err = ProfileStore::built_ins()
            .merge_file(&path)
            .expect_err("an empty id must be rejected");
        assert!(matches!(err, ConfigError::Profile { .. }));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A profile with only `id` set (every other field defaulted) must
    /// still parse -- the additive-safety/forward-compat guarantee the
    /// module doc describes.
    #[test]
    fn a_minimal_profile_with_only_id_set_parses_with_conservative_defaults() {
        let dir = std::env::temp_dir().join(format!(
            "conway-profile-minimal-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profiles.toml");
        std::fs::write(
            &path,
            r#"
            [[profile]]
            id = "minimal"
            "#,
        )
        .unwrap();

        let store = ProfileStore::built_ins().merge_file(&path).unwrap();
        let minimal = store.get("minimal").unwrap();
        assert_eq!(minimal.chat_path, "/chat/completions");
        assert!(!minimal.supports_stream_options);
        assert!(minimal.flatten_multiblock_user);
        assert!(!minimal.sends_parallel_tool_calls);
        assert!(!minimal.uses_max_completion_tokens);
        assert_eq!(minimal.tool_call_style, ToolCallStyle::Tolerant);
        assert_eq!(minimal.cache, CacheMode::None);
        assert_eq!(minimal.tool_calling, ToolCallSupport::NonStreamingOnly);
        assert_eq!(minimal.max_context_tokens, 32_768);
        assert!(
            !minimal.context_window_verified,
            "an unfamiliar provider's max_context_tokens is never assumed verified by omission"
        );
        assert!(
            !minimal.sends_num_ctx,
            "an unfamiliar provider has no known context-window request field by default"
        );
        assert_eq!(minimal.structured_output, StructuredOutput::None);
        assert!(!minimal.parallel_tool_calls);
        assert_eq!(minimal.reliability_tier, ReliabilityTier::Unknown);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_surfaces_every_loaded_profile_with_its_origin() {
        let store = ProfileStore::built_ins();
        let listed = store.list();
        assert_eq!(listed.len(), store.len());
        assert!(listed.iter().all(|l| l.origin == ProfileOrigin::BuiltIn));
        assert!(listed.iter().all(|l| l.shadows.is_none()));
    }

    // --- `Dialect`'s per-predicate tests, preserved from the pre-profile
    // `openai_compat/dialect.rs` ( earlier work/ earlier work): every assertion below is
    // unchanged from before this item, now exercising the profile-derived
    // methods above rather than literal `matches!` arms — the regression
    // net proving byte-identical behavior.

    #[test]
    fn chat_path_is_the_same_for_every_dialect() {
        for dialect in [
            Dialect::OpenAi,
            Dialect::Ollama,
            Dialect::VllmHermes,
            Dialect::LmStudio,
            Dialect::LlamaCppServer,
        ] {
            assert_eq!(dialect.chat_path(), "/chat/completions");
        }
    }

    #[test]
    fn only_openai_and_ollama_support_stream_options() {
        assert!(Dialect::OpenAi.supports_stream_options());
        assert!(Dialect::Ollama.supports_stream_options());
        assert!(!Dialect::VllmHermes.supports_stream_options());
        assert!(!Dialect::LmStudio.supports_stream_options());
        assert!(!Dialect::LlamaCppServer.supports_stream_options());
    }

    #[test]
    fn only_openai_keeps_the_multiblock_user_array() {
        assert!(!Dialect::OpenAi.flatten_multiblock_user());
        assert!(Dialect::Ollama.flatten_multiblock_user());
        assert!(Dialect::VllmHermes.flatten_multiblock_user());
    }

    #[test]
    fn only_openai_sends_parallel_tool_calls() {
        assert!(Dialect::OpenAi.sends_parallel_tool_calls());
        for dialect in [
            Dialect::Ollama,
            Dialect::VllmHermes,
            Dialect::LmStudio,
            Dialect::LlamaCppServer,
        ] {
            assert!(!dialect.sends_parallel_tool_calls(), "{dialect:?}");
        }
    }

    #[test]
    fn only_vllm_hermes_uses_the_hermes_text_fallback() {
        assert!(Dialect::VllmHermes.uses_hermes_text_fallback());
        for dialect in [
            Dialect::OpenAi,
            Dialect::Ollama,
            Dialect::LmStudio,
            Dialect::LlamaCppServer,
        ] {
            assert!(!dialect.uses_hermes_text_fallback(), "{dialect:?}");
        }
    }

    #[test]
    fn defaults_matches_capabilities_dialect_defaults() {
        assert_eq!(
            Dialect::Ollama.defaults(),
            crate::capabilities::dialect_defaults(Dialect::Ollama)
        );
        assert_eq!(
            Dialect::OpenAi.defaults(),
            crate::capabilities::dialect_defaults(Dialect::OpenAi)
        );
    }
}
