//! Dialect-specific baseline capabilities and the pure composition of
//! `ModelMetadata` + `ModelOverrides` + dialect defaults into a
//! `conway_core::capabilities::Capabilities` value for one `(backend,
//! model)` pair.
//!
//! Per architecture §"Module: conway-backends": capabilities are declared
//! per `(backend, model)`, never per backend — this module is the single
//! place that rule is realized. [`build_capabilities`] is a pure function:
//! every input is borrowed data already resolved by a caller
//! ([`crate::model_metadata::ModelMetadataStore::load`], `crate::config`),
//! so this module has no filesystem or network dependency of its own and
//! must never gain an HTTP-client-crate or filesystem-module reference — a
//! test in `tests/model_metadata.rs` greps this file's source to enforce
//! that (see that test for the exact forbidden token list).
//!
//! earlier work (unifying the two capability systems that previously diverged —
//! see `conway_core::ports::CapabilityIndex::from_backends`'s doc):
//! [`build_capabilities`] is the *only* function that turns dialect
//! defaults + metadata + overrides into a `Capabilities` value anywhere in
//! the workspace. `Backend::capabilities()` (each adapter's own impl) calls
//! it directly; the facade's router-side `CapabilityIndex` is built by
//! asking backends for their `capabilities()` rather than recomputing this
//! composition from `models.json` a second time. `overrides` (a
//! `ModelOverrides`, `conway_core::routing`) is the only channel the
//! facade's `models.json` has into this function — and it carries just
//! `max_context_tokens`, `parallel_tool_calls`, and `reliability_tier`.
//! `tool_calling` and `reasoning` have no `ModelOverrides` field, so a
//! `models.json` entry's `tool_calling`/`reasoning` values currently reach
//! neither `Backend::capabilities()` nor the router's `CapabilityIndex` —
//! both read only dialect defaults / `ModelMetadataStore` metadata for
//! those two fields. Giving `models.json` real control over them requires
//! adding fields to `ModelOverrides` (owned by `conway-core`, out of
//! the file scope) — flagged there as a scope-boundary follow-up, not
//! solved here.
//!
//! [`ContextTokensSource`]/[`max_context_tokens_source`] are a later
//! addition (the context-window-declaration-honesty item): a pure query
//! over the same three [`CapabilityInputs`] that tells a caller which of
//! `build_capabilities`'s three `max_context_tokens` layers actually
//! supplied the resolved value, so "the profile's conservative floor
//! governs" is a fact a caller can check and log/surface rather than one
//! indistinguishable from a real, model-specific declaration. See that
//! type's own doc for the incident this is a response to.

use conway_core::capabilities::{
    CacheMode, CacheTtl, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};

use crate::config::{Dialect, ModelOverrides};
use crate::model_metadata::{quantization_tier_hint, ModelMetadata};

/// One dialect's (or Anthropic's) baseline capability values — the
/// lowest-precedence layer in [`build_capabilities`]. Built by
/// [`dialect_defaults`] (the five `openai-compat` dialects) or
/// [`anthropic_defaults`].
#[derive(Debug, Clone, PartialEq)]
pub struct DialectDefaults {
    pub cache: CacheMode,
    pub tool_calling: ToolCallSupport,
    pub max_context_tokens: u32,
    pub structured_output: StructuredOutput,
    pub parallel_tool_calls: bool,
    pub reliability_tier: ReliabilityTier,
}

/// `Dialect::OpenAi` defaults, profile-derived (declarative provider
/// profiles item: reads `Dialect::OpenAi.profile()`, the same `"openai"`
/// built-in `profile.rs` embeds — this function and `Dialect::defaults()`
/// can never diverge).
pub fn openai_defaults() -> DialectDefaults {
    Dialect::OpenAi.defaults()
}

/// `Dialect::Ollama` defaults, profile-derived. `NonStreamingOnly` is
/// deliberate — see the module-level research-backends note on
/// ollama#12557.
pub fn ollama_defaults() -> DialectDefaults {
    Dialect::Ollama.defaults()
}

/// `Dialect::VllmHermes` defaults, profile-derived. `NonStreamingOnly` is
/// deliberate — see the module-level research-backends note on vllm#31871.
pub fn vllm_hermes_defaults() -> DialectDefaults {
    Dialect::VllmHermes.defaults()
}

/// `Dialect::LmStudio` defaults, profile-derived. `NonStreamingOnly` is
/// deliberate — see the module-level research-backends note on codex#7517.
pub fn lm_studio_defaults() -> DialectDefaults {
    Dialect::LmStudio.defaults()
}

/// `Dialect::LlamaCppServer` defaults, profile-derived.
pub fn llama_cpp_server_defaults() -> DialectDefaults {
    Dialect::LlamaCppServer.defaults()
}

/// Anthropic's baseline defaults (not part of the [`Dialect`] enum — the
/// Anthropic adapter is dialect-selected only among `openai-compat`
/// servers).
pub fn anthropic_defaults() -> DialectDefaults {
    DialectDefaults {
        cache: CacheMode::ExplicitBreakpoints {
            max_breakpoints: 4,
            ttls: vec![CacheTtl::FiveMinutes, CacheTtl::OneHour],
        },
        tool_calling: ToolCallSupport::Streaming { validated: true },
        max_context_tokens: 200_000,
        structured_output: StructuredOutput::JsonSchema,
        parallel_tool_calls: true,
        reliability_tier: ReliabilityTier::Verified,
    }
}

/// `dialect`'s baseline `DialectDefaults`, profile-derived
/// (`dialect.defaults()`). Kept as a free function for source
/// compatibility with existing callers.
pub fn dialect_defaults(dialect: Dialect) -> DialectDefaults {
    dialect.defaults()
}

/// The three composable inputs to [`build_capabilities`] for a single
/// `(backend, model)` pair. Precedence, per field: `overrides` >
/// `metadata` > `dialect_defaults`.
#[derive(Debug, Clone)]
pub struct CapabilityInputs<'a> {
    pub dialect_defaults: DialectDefaults,
    pub metadata: Option<&'a ModelMetadata>,
    pub overrides: Option<&'a ModelOverrides>,
}

/// Where [`build_capabilities`]'s resolved `max_context_tokens` actually
/// came from — the discoverability seam this item adds. A context ceiling
/// is either something a real party declared about this specific model
/// (`Override`, a `models.json`/`ModelOverrides` entry; or `Metadata`, a
/// bundled/`metadata_path` [`ModelMetadata`] entry) or `DialectDefaultFloor`
/// — [`Profile::max_context_tokens`](crate::profile::Profile::max_context_tokens)'s
/// conservative per-*dialect* fallback, reached only when NEITHER of those
/// two sources says anything about this model at all.
///
/// The defect this exists for: a rejection or a routing decision citing a
/// context ceiling was, before this item, textually indistinguishable
/// whether that ceiling was a real, model-specific figure or the "no one
/// told conway anything" floor — one operator evening was spent chasing
/// the wrong fix (conversation compaction) because a 32,768-token refusal
/// looked exactly like a model's real limit rather than what it actually
/// was, an undescribed model silently falling through to
/// `default_max_context_tokens()`. [`max_context_tokens_source`] is the
/// pure primitive a caller anywhere in this crate (or a consumer of it)
/// can use to tell the two apart; `openai_compat::OpenAiCompatBackend`
/// logs a `tracing::debug!` when it resolves `DialectDefaultFloor` for
/// exactly this reason — see that module for the wiring. Surfacing this
/// distinction all the way into the operator-facing `ContextTooLarge`
/// message itself is `conway-core`'s `error.rs`/`routing.rs`, outside this
/// crate's file scope; this enum is the seam that work consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextTokensSource {
    /// A `models.json`/config-level `ModelOverrides::max_context_tokens`
    /// named this exact `(backend, model)` pair.
    Override,
    /// A [`ModelMetadata`] entry (bundled `DEFAULTS`, a `metadata_path`
    /// file, or a live `probe_on_startup` discovery hint folded into
    /// `dialect_defaults` before this call — see `probe.rs`) named this
    /// model.
    Metadata,
    /// Neither source above said anything about this model: the value is
    /// [`Profile::max_context_tokens`](crate::profile::Profile::max_context_tokens)'s
    /// per-dialect floor, not a fact about this specific model.
    DialectDefaultFloor,
}

/// Pure companion to [`build_capabilities`]: which of the three layers
/// [`build_capabilities`]'s `max_context_tokens` precedence chain actually
/// supplied the value, without recomputing or duplicating that chain's own
/// logic — see [`ContextTokensSource`]'s doc for why this exists.
pub fn max_context_tokens_source(inputs: &CapabilityInputs<'_>) -> ContextTokensSource {
    if inputs
        .overrides
        .and_then(|o| o.max_context_tokens)
        .is_some()
    {
        ContextTokensSource::Override
    } else if inputs.metadata.and_then(|m| m.max_context_tokens).is_some() {
        ContextTokensSource::Metadata
    } else {
        ContextTokensSource::DialectDefaultFloor
    }
}

/// Composes `inputs` into a `Capabilities` value. Pure — equal inputs
/// always produce equal outputs, and this function performs no I/O.
///
/// `max_context_tokens`, `parallel_tool_calls`, and `reliability_tier` are
/// the three fields `ModelOverrides` can set, so those three follow the
/// full `overrides > metadata > dialect_defaults` chain (with
/// `reliability_tier` additionally falling back to
/// `quantization_tier_hint` between `metadata`'s explicit tier and the
/// dialect default, per `ModelMetadata::quantization`'s documented
/// heuristic). `tool_calling`, `structured_output`, and `reasoning` have no
/// override field, so they resolve as `metadata > dialect_defaults` (with
/// `reasoning` defaulting to `false` when neither says otherwise — no
/// dialect declares a `reasoning` default). `cache` is always the dialect's
/// value: neither `metadata` nor `overrides` carries a cache hint.
pub fn build_capabilities(inputs: CapabilityInputs<'_>) -> Capabilities {
    let CapabilityInputs {
        dialect_defaults,
        metadata,
        overrides,
    } = inputs;

    let tool_calling = metadata
        .and_then(|m| m.tool_calling)
        .map(|spec| spec.to_capability())
        .unwrap_or(dialect_defaults.tool_calling);

    let structured_output = metadata
        .and_then(|m| m.structured_output)
        .map(|spec| spec.to_capability())
        .unwrap_or(dialect_defaults.structured_output);

    let reasoning = metadata.and_then(|m| m.reasoning).unwrap_or(false);

    let max_context_tokens = overrides
        .and_then(|o| o.max_context_tokens)
        .or_else(|| metadata.and_then(|m| m.max_context_tokens))
        .unwrap_or(dialect_defaults.max_context_tokens);

    let parallel_tool_calls = overrides
        .and_then(|o| o.parallel_tool_calls)
        .or_else(|| metadata.and_then(|m| m.parallel_tool_calls))
        .unwrap_or(dialect_defaults.parallel_tool_calls);

    let reliability_tier = overrides
        .and_then(|o| o.reliability_tier)
        .or_else(|| metadata.and_then(|m| m.reliability_tier))
        .or_else(|| {
            metadata
                .and_then(|m| m.quantization.as_deref())
                .and_then(quantization_tier_hint)
        })
        .unwrap_or(dialect_defaults.reliability_tier);

    Capabilities {
        tool_calling,
        parallel_tool_calls,
        structured_output,
        max_context_tokens,
        reasoning,
        reliability_tier,
        cache: dialect_defaults.cache,
    }
}

/// `stream_tools` default derived from a resolved `reliability_tier`:
/// `Verified` streams tool calls by default, `Community` and `Unknown` do
/// not (research-backends: active streaming tool-call bugs on non-`Verified`
/// backends — ollama#12557, vllm#31871, codex#7517). An explicit
/// `ModelOverrides::stream_tools` always wins over this default; see
/// [`resolve_model`].
pub fn stream_tools_default(tier: ReliabilityTier) -> bool {
    matches!(tier, ReliabilityTier::Verified)
}

/// A fully resolved `(backend, model)`: its composed [`Capabilities`] plus
/// whether tool calls should be requested in streaming mode.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModel {
    pub capabilities: Capabilities,
    pub stream_tools: bool,
}

/// Composes `inputs` into a [`ResolvedModel`]: `capabilities` from
/// [`build_capabilities`], and `stream_tools` from
/// `overrides.stream_tools` when set, else [`stream_tools_default`] of the
/// resolved `capabilities.reliability_tier`.
pub fn resolve_model(inputs: CapabilityInputs<'_>) -> ResolvedModel {
    let stream_tools_override = inputs.overrides.and_then(|o| o.stream_tools);
    let capabilities = build_capabilities(inputs);
    let stream_tools = stream_tools_override
        .unwrap_or_else(|| stream_tools_default(capabilities.reliability_tier));
    ResolvedModel {
        capabilities,
        stream_tools,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_metadata::{StructuredOutputSpec, ToolCallSupportSpec};

    fn no_overrides() -> Option<&'static ModelOverrides> {
        None
    }

    #[test]
    fn dialect_defaults_dispatches_to_matching_function() {
        assert_eq!(dialect_defaults(Dialect::OpenAi), openai_defaults());
        assert_eq!(dialect_defaults(Dialect::Ollama), ollama_defaults());
        assert_eq!(
            dialect_defaults(Dialect::VllmHermes),
            vllm_hermes_defaults()
        );
        assert_eq!(dialect_defaults(Dialect::LmStudio), lm_studio_defaults());
        assert_eq!(
            dialect_defaults(Dialect::LlamaCppServer),
            llama_cpp_server_defaults()
        );
    }

    #[test]
    fn build_capabilities_with_no_metadata_or_overrides_uses_dialect_defaults() {
        let inputs = CapabilityInputs {
            dialect_defaults: ollama_defaults(),
            metadata: None,
            overrides: no_overrides(),
        };
        let caps = build_capabilities(inputs);
        assert_eq!(caps.reliability_tier, ReliabilityTier::Unknown);
        assert_eq!(caps.tool_calling, ToolCallSupport::NonStreamingOnly);
        assert_eq!(caps.max_context_tokens, 32_768);
        assert_eq!(
            caps.cache,
            CacheMode::ImplicitPrefix {
                min_prefix_tokens: 0
            }
        );
    }

    #[test]
    fn stream_tools_default_true_only_for_verified() {
        assert!(stream_tools_default(ReliabilityTier::Verified));
        assert!(!stream_tools_default(ReliabilityTier::Community));
        assert!(!stream_tools_default(ReliabilityTier::Unknown));
    }

    #[test]
    fn explicit_stream_tools_override_wins_for_every_tier() {
        for tier in [
            ReliabilityTier::Verified,
            ReliabilityTier::Community,
            ReliabilityTier::Unknown,
        ] {
            let overrides = ModelOverrides {
                stream_tools: Some(true),
                max_context_tokens: None,
                reliability_tier: Some(tier),
                parallel_tool_calls: None,
                min_headroom_tokens: None,
            };
            let inputs = CapabilityInputs {
                dialect_defaults: ollama_defaults(),
                metadata: None,
                overrides: Some(&overrides),
            };
            let resolved = resolve_model(inputs);
            assert_eq!(resolved.capabilities.reliability_tier, tier);
            assert!(resolved.stream_tools, "tier {tier:?} override must win");
        }
    }

    #[test]
    fn stream_tools_default_applies_when_no_override_present() {
        // Community tier, no override: default (false) applies.
        let overrides = ModelOverrides {
            stream_tools: None,
            max_context_tokens: None,
            reliability_tier: Some(ReliabilityTier::Community),
            parallel_tool_calls: None,
            min_headroom_tokens: None,
        };
        let inputs = CapabilityInputs {
            dialect_defaults: ollama_defaults(),
            metadata: None,
            overrides: Some(&overrides),
        };
        let resolved = resolve_model(inputs);
        assert!(!resolved.stream_tools);

        // Verified tier, no override: default (true) applies.
        let overrides = ModelOverrides {
            stream_tools: None,
            max_context_tokens: None,
            reliability_tier: Some(ReliabilityTier::Verified),
            parallel_tool_calls: None,
            min_headroom_tokens: None,
        };
        let inputs = CapabilityInputs {
            dialect_defaults: ollama_defaults(),
            metadata: None,
            overrides: Some(&overrides),
        };
        let resolved = resolve_model(inputs);
        assert!(resolved.stream_tools);
    }

    #[test]
    fn build_capabilities_is_pure() {
        let metadata = ModelMetadata {
            id: "test-model".into(),
            max_context_tokens: Some(64_000),
            tool_calling: Some(ToolCallSupportSpec::Streaming),
            parallel_tool_calls: Some(true),
            structured_output: Some(StructuredOutputSpec::JsonSchema),
            reasoning: Some(true),
            reliability_tier: Some(ReliabilityTier::Community),
            quantization: None,
        };
        let inputs_a = CapabilityInputs {
            dialect_defaults: openai_defaults(),
            metadata: Some(&metadata),
            overrides: None,
        };
        let inputs_b = CapabilityInputs {
            dialect_defaults: openai_defaults(),
            metadata: Some(&metadata),
            overrides: None,
        };
        assert_eq!(build_capabilities(inputs_a), build_capabilities(inputs_b));
    }

    #[test]
    fn parallel_tool_calls_precedence_is_overrides_then_metadata_then_dialect() {
        let dialect = vllm_hermes_defaults(); // parallel_tool_calls: true

        let caps = build_capabilities(CapabilityInputs {
            dialect_defaults: dialect.clone(),
            metadata: None,
            overrides: None,
        });
        assert!(caps.parallel_tool_calls, "dialect default should apply");

        let metadata = ModelMetadata {
            parallel_tool_calls: Some(false),
            ..ModelMetadata::default()
        };
        let caps = build_capabilities(CapabilityInputs {
            dialect_defaults: dialect.clone(),
            metadata: Some(&metadata),
            overrides: None,
        });
        assert!(
            !caps.parallel_tool_calls,
            "metadata must override dialect default"
        );

        let overrides = ModelOverrides {
            stream_tools: None,
            max_context_tokens: None,
            reliability_tier: None,
            parallel_tool_calls: Some(true),
            min_headroom_tokens: None,
        };
        let caps = build_capabilities(CapabilityInputs {
            dialect_defaults: dialect,
            metadata: Some(&metadata),
            overrides: Some(&overrides),
        });
        assert!(caps.parallel_tool_calls, "overrides must win over metadata");
    }

    #[test]
    fn max_context_tokens_precedence_is_overrides_then_metadata_then_dialect() {
        let metadata = ModelMetadata {
            max_context_tokens: Some(64_000),
            ..ModelMetadata::default()
        };
        let overrides = ModelOverrides {
            stream_tools: None,
            max_context_tokens: Some(16_000),
            reliability_tier: None,
            parallel_tool_calls: None,
            min_headroom_tokens: None,
        };

        let caps = build_capabilities(CapabilityInputs {
            dialect_defaults: openai_defaults(),
            metadata: None,
            overrides: None,
        });
        assert_eq!(caps.max_context_tokens, 128_000);

        let caps = build_capabilities(CapabilityInputs {
            dialect_defaults: openai_defaults(),
            metadata: Some(&metadata),
            overrides: None,
        });
        assert_eq!(caps.max_context_tokens, 64_000);

        let caps = build_capabilities(CapabilityInputs {
            dialect_defaults: openai_defaults(),
            metadata: Some(&metadata),
            overrides: Some(&overrides),
        });
        assert_eq!(caps.max_context_tokens, 16_000);
    }

    /// Acceptance: "a model with no declared metadata still routes, and the
    /// fact that a fallback figure governs is discoverable rather than
    /// silent." The routing half is the pre-existing
    /// `build_capabilities_with_no_metadata_or_overrides_uses_dialect_defaults`
    /// test above (it still resolves a usable `Capabilities`, never an
    /// error); this test is the discoverability half, which had no
    /// assertion anywhere before this item — nothing previously
    /// distinguished "the profile's floor governs" from "a real source
    /// declared this number."
    #[test]
    fn max_context_tokens_source_reports_dialect_default_floor_when_nothing_is_declared() {
        let inputs = CapabilityInputs {
            dialect_defaults: ollama_defaults(),
            metadata: None,
            overrides: None,
        };
        assert_eq!(
            max_context_tokens_source(&inputs),
            ContextTokensSource::DialectDefaultFloor
        );
        // The model still resolves a real, usable capability -- silence
        // about the *source* is not the same as failing to route.
        assert_eq!(
            build_capabilities(inputs).max_context_tokens,
            ollama_defaults().max_context_tokens
        );
    }

    #[test]
    fn max_context_tokens_source_prefers_override_then_metadata_then_dialect_default_floor() {
        let metadata = ModelMetadata {
            max_context_tokens: Some(64_000),
            ..ModelMetadata::default()
        };

        // Metadata present, no override: `Metadata`.
        let inputs = CapabilityInputs {
            dialect_defaults: ollama_defaults(),
            metadata: Some(&metadata),
            overrides: None,
        };
        assert_eq!(
            max_context_tokens_source(&inputs),
            ContextTokensSource::Metadata
        );

        // Both present: `Override` wins, exactly matching
        // `build_capabilities`'s own precedence.
        let overrides = ModelOverrides {
            stream_tools: None,
            max_context_tokens: Some(16_000),
            reliability_tier: None,
            parallel_tool_calls: None,
            min_headroom_tokens: None,
        };
        let inputs = CapabilityInputs {
            dialect_defaults: ollama_defaults(),
            metadata: Some(&metadata),
            overrides: Some(&overrides),
        };
        assert_eq!(
            max_context_tokens_source(&inputs),
            ContextTokensSource::Override
        );
    }

    #[test]
    fn reliability_tier_falls_back_to_quantization_hint_before_dialect_default() {
        let metadata = ModelMetadata {
            reliability_tier: None,
            quantization: Some("Q4_K_M".to_string()),
            ..ModelMetadata::default()
        };
        let caps = build_capabilities(CapabilityInputs {
            dialect_defaults: openai_defaults(), // dialect default is Verified
            metadata: Some(&metadata),
            overrides: None,
        });
        assert_eq!(caps.reliability_tier, ReliabilityTier::Community);
    }
}
