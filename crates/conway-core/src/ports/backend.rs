//! The `Backend` port: one adapter per LLM provider dialect (architecture
//! §4.1).

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::capabilities::{Capabilities, ProbeReport};
use crate::content::{ContentBlock, SamplingParams, StopReason, ToolCall, ToolSpec, Usage};
use crate::error::{BackendError, ConwayError};
use crate::ids::{BackendId, ModelId, PrefixKey};
use crate::routing::ModelOverrides;
use crate::segment::PromptSegment;

/// The numbers behind an admission verdict (headroom-and-refusal amendment, decision
///):
/// `est_tokens` as measured by whichever `Backend::admit` implementation
/// produced this (its own dialect's local estimate -- never a network round
/// trip), the `headroom_tokens` the caller resolved from configuration, and
/// the model's declared `max_context_tokens`. `Backend::admit` returns
/// these numbers whether or not the request fits: on success as `Ok`, on
/// rejection folded into [`BackendError::ContextTooLarge`] (which carries
/// the same three numbers plus the two derived ones). Nothing about
/// admission is ever collapsed to a bare boolean.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Admission {
    pub est_tokens: u32,
    pub headroom_tokens: u32,
    pub max_context_tokens: u32,
}

impl Admission {
    /// `est_tokens + headroom_tokens`, saturating.
    pub fn required_tokens(&self) -> u32 {
        self.est_tokens.saturating_add(self.headroom_tokens)
    }

    /// Whether `required_tokens()` fits inside `max_context_tokens`.
    /// Inclusive bound: an exact fit (`required_tokens() ==
    /// max_context_tokens`) admits -- the same inclusive bound
    /// `conway-routing`'s pre-relocation restatement of this arithmetic
    /// used to document for itself, before
    /// retired that copy.
    pub fn fits(&self) -> bool {
        self.required_tokens() <= self.max_context_tokens
    }

    /// How far `required_tokens()` exceeds `max_context_tokens` (`0` when
    /// it fits). Saturating.
    pub fn shortfall_tokens(&self) -> u32 {
        self.required_tokens()
            .saturating_sub(self.max_context_tokens)
    }
}

/// THE headroom arithmetic and fit comparison -- one implementation, never
/// restated per caller, which is the "what stays shared" clause: every
/// `Backend::admit` implementation -- the default
/// below, and both shipped `conway-plugin-backends` dialects -- calls this rather
/// than restating `est_tokens + headroom_tokens <= max_context_tokens`
/// itself. Only the estimate feeding `est_tokens` may legitimately differ
/// per dialect (that is the "tokenizer as the injected seam" the routing rule
/// asks for);
/// the comparison itself must never be restated, since two backends
/// growing slightly different notions of "fits" -- one of which quietly
/// omits a check -- is precisely the drift a single implementation exists to
/// prevent.
pub fn check_admission(
    model: ModelId,
    est_tokens: u32,
    headroom_tokens: u32,
    max_context_tokens: u32,
) -> Result<Admission, BackendError> {
    let admission = Admission {
        est_tokens,
        headroom_tokens,
        max_context_tokens,
    };
    if admission.fits() {
        Ok(admission)
    } else {
        Err(BackendError::ContextTooLarge {
            model,
            est_tokens,
            headroom_tokens,
            required_tokens: admission.required_tokens(),
            max_context_tokens,
            shortfall_tokens: admission.shortfall_tokens(),
        })
    }
}

/// Dialect-neutral fallback estimator for [`Backend::admit`]'s default
/// implementation: `ceil(chars / 4)` over each segment's serialized content
/// (roughly `conway-runtime`'s `ContextBuilder::TOKEN_ESTIMATOR`,
/// `"heuristic-chars4"`, without that estimator's `+4`-per-block framing
/// term, folded into a per-segment `+4` here instead), plus the same over
/// the serialized tool schemas when any are present. A real dialect adapter
/// overrides `admit` entirely with its own wire-format-aware estimate and
/// never calls this -- see that method's own doc.
fn default_estimate_tokens(req: &GenerateRequest) -> u32 {
    let mut total: u32 = 0;
    for segment in &req.segments {
        let rendered = serde_json::to_value(&segment.content)
            .map(|value| value.to_string())
            .unwrap_or_default();
        let chars = u32::try_from(rendered.chars().count()).unwrap_or(u32::MAX);
        total = total.saturating_add(chars.div_ceil(4)).saturating_add(4);
    }
    if !req.tools.is_empty() {
        if let Ok(value) = serde_json::to_value(&req.tools) {
            let chars = u32::try_from(value.to_string().chars().count()).unwrap_or(u32::MAX);
            total = total.saturating_add(chars.div_ceil(4)).saturating_add(4);
        }
    }
    total
}

/// A boxed, `Send` stream of `T`.
///
/// Defined locally over `futures_core::Stream` so this crate does not need
/// `futures`/`futures-util` as a dependency — `conway-core` does no
/// stream-driving I/O and the combinator ecosystem those crates provide is
/// unneeded here. (The crate-wide "performs no I/O" claim has one live
/// exception, `containment`, labeled at the crate root; it is synchronous
/// `std::fs` and does not bear on this dependency choice either way.)
pub type BoxStream<'a, T> = core::pin::Pin<Box<dyn futures_core::Stream<Item = T> + Send + 'a>>;

/// One adapter for one LLM provider dialect (e.g. Anthropic, an
/// OpenAI-compatible endpoint). Implementations live in `conway-plugin-backends`.
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// This backend instance's configured identity.
    fn id(&self) -> BackendId;

    /// Capabilities are per `(backend, model)`, not per-backend: quantization
    /// and chat template change tool-call reliability independent of the
    /// server.
    fn capabilities(&self, model: &ModelId) -> Capabilities;

    /// Generate a complete (non-streamed) response.
    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError>;

    /// Generate a streamed response.
    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError>;

    /// Cheap liveness/readiness probe. Distinct from transport errors
    /// encountered during `generate`/`stream`.
    async fn probe(&self) -> Result<ProbeReport, BackendError>;

    /// Whether `req` fits inside its own `req.model`'s context window,
    /// reserving `headroom_tokens` for output/reasoning (headroom-and-refusal
    /// amendment
    ///). Headroom the NUMBER stays declarative
    /// configuration -- a default plus a per-role override -- resolved by the
    /// caller and passed in here; only who *reads* it moved.
    ///
    /// Synchronous and MUST NOT perform network I/O: an implementation that
    /// needs a round trip (e.g. a provider's own count-tokens endpoint) to
    /// answer this is using the wrong API. Estimate locally with your own
    /// dialect's tokenization, reconciled after the fact against reported
    /// usage if you choose to calibrate it (measure a baseline before optimising; not
    /// required here).
    ///
    /// `Ok(admission)` when it fits, `Err(BackendError::ContextTooLarge)`
    /// when it does not -- the numbers travel with the verdict either way,
    /// never a bare boolean. A caller that receives `Err` must relay the
    /// refusal rather than work around it: no trimming the request, no
    /// silently retrying against a larger model -- core refuses an oversized
    /// context rather than working around it, and that is its remaining
    /// commitment once the arithmetic itself moved here.
    ///
    /// The default estimates `req`'s size with a dialect-neutral heuristic
    /// over its assembled segments and tool schemas. A real dialect adapter
    /// SHOULD override this with its own tokenization (its wire format, its
    /// framing overhead) -- see `conway-plugin-backends`'s Anthropic and
    /// OpenAI-compatible adapters. Every override, and this default, MUST
    /// call [`check_admission`] for the arithmetic rather than restating it
    /// (ONE implementation of "fits", not one per dialect).
    ///
    /// **THE production admission path** -- `conway_runtime::attempt::AttemptEngine
    /// ::execute` builds each candidate route's real `GenerateRequest`
    /// first, then calls this method -- the authoritative answer to
    /// "does it fit?" -- before making any network call. A refusal skips
    /// that one candidate; when every candidate refuses this way, the
    /// engine aggregates into `RuntimeError::Routing(RoutingError::
    /// ContextTooLarge)`, sourcing every number from the refusing
    /// `BackendError`s directly. `conway-routing`'s own declared-window
    /// check (`DeclarativeRouter`'s `satisfies`) still runs first, as a
    /// cheap ADVISORY pre-filter over the router's chain -- see `docs/
    /// routing.md`'s "Advisory vs. authoritative" section for why the two
    /// are not required to agree.
    ///
    /// So the caller obligation stated above is live, not aspirational: a
    /// third-party `Backend` implementing this (or relying on the default)
    /// is on the real request path today.
    fn admit(
        &self,
        req: &GenerateRequest,
        headroom_tokens: u32,
    ) -> Result<Admission, BackendError> {
        let est_tokens = default_estimate_tokens(req);
        check_admission(
            req.model.clone(),
            est_tokens,
            headroom_tokens,
            self.capabilities(&req.model).max_context_tokens,
        )
    }
}

// ---------------------------------------------------------------------
// `BackendFactory`: names a
// provider-adapter KIND up front -- so a third party can ship one -- and
// defers actual construction to a later, fallible step. Mirrors
// `RouterFactory`/`RouterBuildContext`/`RouterBundle`
// (`crate::ports::routing`) one layer
// over, with one load-bearing asymmetry stated in `BackendFactory`'s own doc
// below: a build has exactly one router, so `RouterFactory::build` is
// invoked at most once; a build has a SET of backends, so a `BackendFactory`
// is not bounded that way.
// ---------------------------------------------------------------------

/// Everything a [`BackendFactory::build`] genuinely needs to construct one
/// backend instance, resolved by the caller before this is handed over --
/// the same "resolved pieces, not a raw entry" shape `RouterBuildContext`
/// already chose for `RoutingConfig`/`HeadroomPolicy`, and for the same
/// reason: `conway_plugin_backends::config::{AnthropicConfig, OpenAiCompatConfig}`
/// (`conway_plugin_backends::factory`'s `AnthropicBackendFactory::build`/
/// `OpenAiCompatBackendFactory::build` -- relocated from `crates/conway/src/
/// builder.rs`'s own `build_anthropic`/`build_openai_compat` by
///) are what this shape is read off of, and every
/// one of these six fields is something those two `build` methods resolve
/// BEFORE their adapter's own constructor ever runs.
///
/// **Why not the raw `[backends.<id>]` entry instead (the cheaper-looking
/// alternative)?** Two reasons, not one:
/// 1. `crate::config::schema::BackendEntry` (the facade's own type) cannot
///    appear here at all -- `conway-core` cannot depend on `conway` (that
///    edge runs the other way), so a "raw entry" option would mean
///    inventing a SECOND, `conway-core`-native struct shaped like
///    `BackendEntry` purely to duplicate it across the crate boundary.
/// 2. Even granting that duplication, handing over `api_key`/`api_key_env`
///    unresolved would mean every third-party kind reimplements "literal
///    key wins, else read `api_key_env` from the process environment, else
///    unset" -- and they would diverge, silently losing
///    [`crate::error::ConwayError`]'s specific "api_key_env '...' is not
///    set" message the facade's own `resolve_api_key` already produces.
///    Resolving it once, centrally, and handing over the result is what
///    keeps that one good error message the only one that exists.
///
/// **Why not both (the raw entry ALONGSIDE these resolved fields)?** Two
/// sources of the same value invites the question "which one wins when they
/// disagree" for no benefit any item to date has needed answering.
/// `backends.<id>.kind` is an OPEN name now (,
///), so a third-party kind is already
/// nameable; `BackendEntry`'s own six fields (`kind`, `api_key`,
/// `api_key_env`, `base_url`, `dialect`, `stream_tools`) still fully cover
/// via the resolved fields below (`stream_tools` has no analogue here: it is
/// not read by either shipped kind's `build` today), and [`Self::extra`]
/// is that same escape hatch for
/// whatever a `kind` puts beyond those six -- a raw entry duplicating the
/// six typed fields a second time alongside it would still buy nothing.
#[derive(Clone, Debug)]
pub struct BackendBuildContext {
    /// The instance identity this backend SHOULD report from its own
    /// `Backend::id()` -- ordinarily the `[backends.<id>]` JSON key.
    /// Advisory, not enforced: `build()` never inspects the returned
    /// `Backend::id()` against this field, the same way `RouterBuildContext`
    /// hands over data a `RouterFactory` is trusted to use, not data it is
    /// mechanically checked against.
    pub id: BackendId,
    /// `[backends.<id>].base_url`, unparsed. Raw rather than a parsed URL type:
    /// `conway-core` depends on nothing that could parse or validate one (no
    /// new dependency -- this tree stays release-ready), and the two shipped
    /// adapters do not even agree with each other on how to treat an empty
    /// value (Anthropic substitutes a hardcoded default; OpenAI-compatible
    /// requires one) -- a third kind is entitled to its own policy here too,
    /// not one this context would otherwise impose on it.
    pub base_url: String,
    /// The resolved secret: a literal `api_key`, or an `api_key_env`
    /// variable already read from the process environment, or `None` when
    /// neither was set -- see this struct's own doc for why resolving this
    /// centrally (rather than handing over the two raw, unresolved fields)
    /// is the whole point of choosing this shape.
    pub api_key: Option<String>,
    /// `[backends.<id>].dialect`, unresolved: this is the facade's
    /// `AnthropicConfig`/`OpenAiCompatConfig`-agnostic notion of "which
    /// wire shape" only in the sense that it is the SAME string
    /// `OpenAiCompatBackendFactory::build` feeds to its own internal
    /// profile resolution -- a third-party kind is free to give it an
    /// entirely different meaning
    /// (or ignore it) rather than being forced through the facade's own
    /// `conway_plugin_backends::profile::ProfileStore`, which this crate does not
    /// depend on either.
    pub dialect: Option<String>,
    /// The exact `BTreeMap<String, ModelOverrides>` shape
    /// `models_overrides_for(id, metadata)` projects out of `models.json`
    /// for this same `id` -- so a factory-built backend's
    /// `Backend::capabilities()` can honor an operator's `models.json`
    /// override the identical way a config-derived backend's own
    /// `ModelOverrides` table already does (the caller's single-source
    /// guarantee, extended to a third-party kind rather than left as a
    /// built-ins-only privilege).
    pub models: BTreeMap<String, ModelOverrides>,
    /// Declarative provider-profile file paths a `dialect`-selecting kind
    /// (e.g. `conway_plugin_backends`'s `"openai-compat"`) may resolve its
    /// own `dialect` string against, in precedence order (project-scoped
    /// first, then global) -- the exact list `crate::config::discovery::
    /// provider_profile_file_paths` (the facade's own XDG/project-precedence
    /// scanner) already resolves once per `build()` call and hands to EVERY
    /// [`BackendBuildContext`] unconditionally, the same way [`Self::models`]
    /// is handed to every kind whether or not it reads `models.json`
    /// overrides. Resolved (a path list), not raw (`cwd`/env) -- for the
    /// identical reason [`Self::api_key`] is resolved rather than handed
    /// over as two raw fields (this struct's own doc, above): duplicating
    /// the facade's own project/global discovery precedence inside a
    /// third-party kind would risk it drifting from `conway`'s one true
    /// implementation, where handing over the already-resolved list cannot.
    /// A path may not exist on disk -- a kind that cares (like
    /// `conway_plugin_backends`'s own `ProfileStore::merge_file`) treats a
    /// missing file as "contributes nothing," never an error. A kind with no
    /// "dialect"/"profile" concept at all (e.g. `"anthropic"`) simply never
    /// reads this field, the same way it is free to ignore
    /// [`Self::dialect`] itself.
    pub profile_file_paths: Vec<PathBuf>,
    /// Every key `[backends.<id>]`'s entry carried beyond `kind` and the
    /// five typed fields above -- the facade's own `BackendEntry::extra`
    /// (`crates/conway/src/config/schema.rs`), copied here verbatim by
    /// `build_backend_context` and from nowhere else. `BTreeMap<String,
    /// serde_json::Value>` on purpose: the identical map type
    /// `BackendEntry::extra` itself uses, so no conversion happens crossing
    /// this boundary.
    ///
    /// This is the field that makes `BackendEntry`'s own trade real rather
    /// than merely accepted: that struct dropped `#[serde(deny_unknown_
    /// fields)]` specifically so a third-party kind would have somewhere to
    /// put its own configuration, at the disclosed cost that a typo in one
    /// of the five typed field names (e.g. `base_ur1`) is silently captured
    /// here instead of rejected at load. Before this field existed, `extra`
    /// was captured at load time and then discarded before any
    /// [`BackendFactory::build`] ever saw it -- all of that cost, none of
    /// the benefit. A kind's `build()` is free to validate its own keys out
    /// of this map and reject its own typos there; the facade itself
    /// performs no per-kind validation.
    ///
    /// Empty (not merely absent) when the entry set no key beyond the five
    /// typed ones -- a kind that never reads this field behaves exactly as
    /// it did before this field was added. As of the item that made the
    /// shipped Anthropic kind consume its own configuration,
    /// `conway_plugin_backends`'s `"anthropic"` reads it
    /// (`AnthropicBackendFactory::resolve_extra`: `anthropic_version` and
    /// `headers`, with any other key a rejected, named build error --
    /// never silently ignored). `"openai-compat"` still does not read it;
    /// that kind's own per-provider configuration surface is `dialect` (a
    /// profile selector), not this field -- the two kinds do not share one
    /// `dialect`/`extra` vocabulary, see `conway_plugin_backends::factory`'s
    /// own module doc for the finding.
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Builds one [`Backend`] instance for a provider-adapter KIND named up
/// front by [`Self::id`] -- the provider-adapter analogue of
/// [`crate::ports::routing::RouterFactory`], one layer over: read that
/// trait's own doc first, the shape here mirrors it deliberately.
///
/// **A kind identity is NOT [`Backend::id()`], and this is a different
/// question than `RouterFactory` answering "why isn't a router id `Router
/// ::id()`" -- restated here in this port's own words, not merely cited,
/// because the two ports differ in a way that matters:** `Backend::id()` is
/// a CONFIGURED INSTANCE's identity, read off the `[backends.<id>]` JSON key
/// the instance was built from (`BackendBuildContext::id`, itself sourced
/// from that same key) -- it exists only once construction has already
/// happened, and it answers "which of MY backends is this." `BackendFactory
/// ::id()` exists BEFORE any instance does, is fixed for the life of the
/// factory, and answers "which ADAPTER IMPLEMENTATION would build this" --
/// e.g. "anthropic" the KIND versus "kimi" the INSTANCE, where "kimi" is an
/// Anthropic-compatible endpoint configured under a name that says what it
/// IS, not what built it (`crates/conway/src/builder.rs`'s own module doc,
/// "The backend map is keyed by..."). Collapsing the two would make it
/// impossible to configure two different instances of the same kind under
/// two different ids -- exactly the "kimi" scenario the shipped Anthropic
/// adapter already supports today.
///
/// **The asymmetry against `RouterFactory::build`, stated up front so an
/// implementor does not assume the stricter cardinality:** a build has
/// EXACTLY ONE router, so `RouterFactory::build` is invoked at most once.
/// A build has a SET of backends -- one installed kind can legitimately
/// produce many configured instances (as the "kimi" example above already
/// shows for a built-in kind) -- so nothing about this trait promises
/// `build` is called at most once over a `ConwayBuilder::build()` call.
/// `[backends.<id>].kind` is an open name (,
///), so `ConwayBuilder::build()`
/// routes each `[backends.<id>]` entry to whichever registered factory's
/// `BackendFactory::id()` matches that entry's own `kind` -- see
/// `ConwayBuilder::with_backend_factory`'s own doc for the exact resolution
/// order, and `crates/conway/src/builder.rs`'s `resolve_backend_factory` for
/// the implementation. `build` is therefore invoked once per matching
/// `[backends.<id>]` entry, never unconditionally once per `ConwayBuilder::
/// build()` call.
pub trait BackendFactory: Send + Sync {
    /// This factory's own identity -- the id an operator names to select it. A KIND, not a configured
    /// instance's identity -- see this trait's own doc for why the two are
    /// not the same question asked twice. Stable across every `Backend`
    /// this factory might construct.
    fn id(&self) -> &str;

    /// Builds one backend instance from `ctx`. Deferred (invoked only once
    /// `ctx` can actually be assembled) and fallible, returning
    /// [`ConwayError`] -- `conway-core`'s own existing crate-level error
    /// enum, reused rather than inventing a new one, the same choice
    /// [`crate::ports::routing::RouterFactory::build`] already makes for
    /// the identical reason: a factory's construction failure is exactly
    /// the shape `ConwayError::Config`/`ConwayError::Parse` already exist to
    /// describe.
    fn build(&self, ctx: BackendBuildContext) -> Result<std::sync::Arc<dyn Backend>, ConwayError>;

    /// Optional startup capability discovery -- a kind whose provider exposes a
    /// server-side model/capability listing (today, only
    /// `conway_plugin_backends`'s `"openai-compat"` kind does) may override
    /// this to report what it discovers for `ctx`'s configured instance.
    /// Called by `ConwayBuilder::build` at most once per matching
    /// `[backends.<id>]` entry, and ONLY when `[models].probe_on_startup` is
    /// set -- with the SAME [`BackendBuildContext`] [`Self::build`] itself
    /// received for that entry, never a separately-assembled one. The
    /// default returns an empty map: no discovery mechanism, `probe_on_
    /// startup` has no effect for this kind -- today's disclosed behavior
    /// for every kind but `"openai-compat"`, made structural rather than
    /// merely documented.
    ///
    /// **The RESTRICT policy this return value is subject to lives in the
    /// CALLER, not here, and every kind gets it identically.** `ConwayBuilder
    /// ::build` only ever applies an entry of the returned map to the
    /// router's `CapabilityIndex` when its key already appears in
    /// `ctx.models` -- i.e. was already declared for this backend in
    /// `models.json` -- silently dropping any other entry. A kind's own
    /// `probe_capabilities` implementation may therefore report everything
    /// it genuinely observed with no filtering of its own; it can never, by
    /// returning a map, make an undeclared `(backend, model)` pair routable
    /// (no opaque auto-selection in the core) -- keeping this
    /// enforcement in one place, applied to every kind (built-in or
    /// third-party) the same way, is the entire reason it is not delegated
    /// to each implementation to get right on its own.
    ///
    /// MUST NOT perform network I/O asynchronously relative to the caller --
    /// like [`Self::build`], this is a synchronous method; an implementation
    /// that needs a real round trip bridges it itself (e.g. a dedicated
    /// blocking `tokio` runtime, the same pattern `ConwayBuilder::build`'s own
    /// private `block_on` helper uses for its own async lower-crate calls),
    /// exactly as any third-party kind is free to do too -- nothing about this
    /// being a first-party kind reaches a private hook a third-party
    /// implementation could not also use -- a built-in gets no privileged API.
    fn probe_capabilities(&self, _ctx: &BackendBuildContext) -> BTreeMap<ModelId, Capabilities> {
        BTreeMap::new()
    }
}

/// A request to generate a response from one model.
///
/// Every field is producer-owned; adapters may reorder nothing (architecture
/// §8).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub model: ModelId,
    /// Order is load-bearing for implicit-prefix caching (architecture
    /// §5.3: static → inherited → volatile). Adapters MUST NOT reorder,
    /// merge, or drop segments.
    pub segments: Vec<PromptSegment>,
    pub tools: Vec<ToolSpec>,
    pub params: SamplingParams,
    /// Reserved for `CacheMode::SlotKv`; adapters that do not support slots
    /// ignore it.
    pub prefix_key: Option<PrefixKey>,
}

/// The result of a completed (non-streamed) generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerateResponse {
    pub content: Vec<ContentBlock>,
    /// Already validated against the requested `ToolSpec` schemas.
    pub tool_calls: Vec<ToolCall>,
    pub stop: StopReason,
    /// Includes `cache_read_tokens`/`cache_write_tokens` when the backend
    /// reports them.
    pub usage: Usage,
}

/// One chunk of a streamed generation.
///
/// Externally tagged (serde's default enum representation): an internal tag
/// (`#[serde(tag = "type")]`) cannot represent the newtype variants
/// (`TextDelta(String)`, `ThinkingDelta(String)`) here, since serde has no
/// way to merge a bare string payload into a tagged object.
#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamChunk {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCallDelta { index: u32, raw: String },
    Done(GenerateResponse),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{Role, StopReason};
    use crate::provenance::Provenance;

    fn sample_segment() -> PromptSegment {
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text { text: "hi".into() }],
            Provenance::UserPrompt,
        )
    }

    #[test]
    fn generate_request_round_trips_and_preserves_segment_order() {
        let req = GenerateRequest {
            model: ModelId::new("claude-sonnet-4-6"),
            segments: vec![sample_segment(), sample_segment()],
            tools: vec![],
            params: SamplingParams::default(),
            prefix_key: Some(PrefixKey::from_blake3(blake3::hash(b"x"))),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: GenerateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.segments.len(), req.segments.len());
        assert_eq!(
            back.segments.iter().map(|s| s.id).collect::<Vec<_>>(),
            req.segments.iter().map(|s| s.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn stream_chunk_variants_round_trip() {
        let done = StreamChunk::Done(GenerateResponse {
            content: vec![],
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage::default(),
        });
        let cases = vec![
            StreamChunk::TextDelta("hi".into()),
            StreamChunk::ThinkingDelta("hmm".into()),
            StreamChunk::ToolCallDelta {
                index: 0,
                raw: "{}".into(),
            },
            done,
        ];
        for chunk in cases {
            let json = serde_json::to_string(&chunk).unwrap();
            let _back: StreamChunk = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn box_stream_type_is_usable() {
        use futures_core::Stream;

        struct Empty;
        impl Stream for Empty {
            type Item = Result<StreamChunk, BackendError>;
            fn poll_next(
                self: core::pin::Pin<&mut Self>,
                _cx: &mut core::task::Context<'_>,
            ) -> core::task::Poll<Option<Self::Item>> {
                core::task::Poll::Ready(None)
            }
        }

        let _s: BoxStream<'static, Result<StreamChunk, BackendError>> = Box::pin(Empty);
    }

    // -----------------------------------------------------------------
    // `Admission` / `check_admission`
    // -----------------------------------------------------------------

    #[test]
    fn admission_required_tokens_is_saturating() {
        let admission = Admission {
            est_tokens: u32::MAX,
            headroom_tokens: 16_000,
            max_context_tokens: 100,
        };
        assert_eq!(admission.required_tokens(), u32::MAX);
        assert!(!admission.fits());
        assert_eq!(admission.shortfall_tokens(), u32::MAX - 100);
    }

    #[test]
    fn boundary_exact_fit_admits_one_over_rejects() {
        let ok = check_admission(ModelId::new("m"), 34_000, 16_000, 50_000);
        assert!(ok.is_ok(), "exact fit (34000 + 16000 == 50000) must admit");

        let err = check_admission(ModelId::new("m"), 34_000, 16_001, 50_000).unwrap_err();
        let BackendError::ContextTooLarge {
            est_tokens,
            headroom_tokens,
            required_tokens,
            max_context_tokens,
            shortfall_tokens,
            ..
        } = err
        else {
            panic!("expected ContextTooLarge, got a different variant");
        };
        assert_eq!(est_tokens, 34_000);
        assert_eq!(headroom_tokens, 16_001);
        assert_eq!(required_tokens, 50_001);
        assert_eq!(max_context_tokens, 50_000);
        assert_eq!(shortfall_tokens, 1);
    }

    #[test]
    fn check_admission_names_the_model_on_rejection() {
        let model = ModelId::new("claude-sonnet-4-6");
        let err = check_admission(model.clone(), 100_000, 8_192, 32_768).unwrap_err();
        let BackendError::ContextTooLarge { model: named, .. } = err else {
            panic!("expected ContextTooLarge");
        };
        assert_eq!(named, model);
    }

    /// A minimal `Backend` that relies entirely on `admit`'s default
    /// implementation -- exercises the dialect-neutral estimator plus
    /// `check_admission` together, the same path every fake in
    /// `conway-core::fakes` and every other test-local `Backend` in the
    /// workspace gets for free without writing a single line for it.
    struct DefaultAdmitBackend {
        max_context_tokens: u32,
    }

    #[async_trait]
    impl Backend for DefaultAdmitBackend {
        fn id(&self) -> BackendId {
            BackendId::new("default-admit")
        }
        fn capabilities(&self, _model: &ModelId) -> Capabilities {
            use crate::capabilities::{
                CacheMode, ReliabilityTier, StructuredOutput, ToolCallSupport,
            };
            Capabilities {
                tool_calling: ToolCallSupport::None,
                cache: CacheMode::None,
                parallel_tool_calls: false,
                structured_output: StructuredOutput::None,
                max_context_tokens: self.max_context_tokens,
                reasoning: false,
                reliability_tier: ReliabilityTier::Unknown,
            }
        }
        async fn generate(&self, _req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
            unimplemented!("not exercised by this test")
        }
        async fn stream(
            &self,
            _req: GenerateRequest,
        ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
            unimplemented!("not exercised by this test")
        }
        async fn probe(&self) -> Result<ProbeReport, BackendError> {
            unimplemented!("not exercised by this test")
        }
    }

    fn tiny_request(text: &str) -> GenerateRequest {
        use crate::provenance::Provenance;
        GenerateRequest {
            model: ModelId::new("m"),
            segments: vec![PromptSegment::new(
                crate::content::Role::User,
                vec![ContentBlock::Text { text: text.into() }],
                Provenance::UserPrompt,
            )],
            tools: vec![],
            params: SamplingParams::default(),
            prefix_key: None,
        }
    }

    #[test]
    fn default_admit_admits_a_small_request_against_a_roomy_window() {
        let backend = DefaultAdmitBackend {
            max_context_tokens: 1_000_000,
        };
        let admission = backend.admit(&tiny_request("hello"), 8_192).unwrap();
        assert!(admission.fits());
        assert_eq!(admission.max_context_tokens, 1_000_000);
    }

    #[test]
    fn default_admit_rejects_against_a_window_too_small_to_hold_headroom_alone() {
        let backend = DefaultAdmitBackend {
            max_context_tokens: 10,
        };
        let err = backend.admit(&tiny_request("hello"), 8_192).unwrap_err();
        assert!(matches!(err, BackendError::ContextTooLarge { .. }));
    }
}
