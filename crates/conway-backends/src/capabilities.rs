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
//! WI-123 (unifying the two capability systems that previously diverged —
//! see `conway_routing::capability::CapabilityIndex::from_backends`'s doc):
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
//! WI-123's file scope) — flagged there as a scope-boundary follow-up, not
//! solved here.

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

/// `Dialect::OpenAi` defaults.
pub fn openai_defaults() -> DialectDefaults {
    DialectDefaults {
        cache: CacheMode::ImplicitPrefix {
            min_prefix_tokens: 1024,
        },
        tool_calling: ToolCallSupport::Streaming { validated: true },
        max_context_tokens: 128_000,
        structured_output: StructuredOutput::JsonSchema,
        parallel_tool_calls: true,
        reliability_tier: ReliabilityTier::Verified,
    }
}

/// `Dialect::Ollama` defaults. `NonStreamingOnly` is deliberate — see the
/// module-level research-backends note on ollama#12557.
pub fn ollama_defaults() -> DialectDefaults {
    DialectDefaults {
        cache: CacheMode::ImplicitPrefix {
            min_prefix_tokens: 0,
        },
        tool_calling: ToolCallSupport::NonStreamingOnly,
        max_context_tokens: 32_768,
        structured_output: StructuredOutput::JsonSchema,
        parallel_tool_calls: false,
        reliability_tier: ReliabilityTier::Unknown,
    }
}

/// `Dialect::VllmHermes` defaults. `NonStreamingOnly` is deliberate — see
/// the module-level research-backends note on vllm#31871.
pub fn vllm_hermes_defaults() -> DialectDefaults {
    DialectDefaults {
        cache: CacheMode::ImplicitPrefix {
            min_prefix_tokens: 0,
        },
        tool_calling: ToolCallSupport::NonStreamingOnly,
        max_context_tokens: 32_768,
        structured_output: StructuredOutput::JsonSchema,
        parallel_tool_calls: true,
        reliability_tier: ReliabilityTier::Community,
    }
}

/// `Dialect::LmStudio` defaults. `NonStreamingOnly` is deliberate — see the
/// module-level research-backends note on codex#7517.
pub fn lm_studio_defaults() -> DialectDefaults {
    DialectDefaults {
        cache: CacheMode::None,
        tool_calling: ToolCallSupport::NonStreamingOnly,
        max_context_tokens: 32_768,
        structured_output: StructuredOutput::None,
        parallel_tool_calls: false,
        reliability_tier: ReliabilityTier::Unknown,
    }
}

/// `Dialect::LlamaCppServer` defaults.
pub fn llama_cpp_server_defaults() -> DialectDefaults {
    DialectDefaults {
        cache: CacheMode::ImplicitPrefix {
            min_prefix_tokens: 0,
        },
        tool_calling: ToolCallSupport::NonStreamingOnly,
        max_context_tokens: 32_768,
        structured_output: StructuredOutput::Grammar,
        parallel_tool_calls: false,
        reliability_tier: ReliabilityTier::Community,
    }
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

/// Dispatches to the matching `*_defaults()` function for one of the five
/// `openai-compat` dialects.
pub fn dialect_defaults(dialect: Dialect) -> DialectDefaults {
    match dialect {
        Dialect::OpenAi => openai_defaults(),
        Dialect::Ollama => ollama_defaults(),
        Dialect::VllmHermes => vllm_hermes_defaults(),
        Dialect::LmStudio => lm_studio_defaults(),
        Dialect::LlamaCppServer => llama_cpp_server_defaults(),
    }
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

/// Composes `inputs` into a `Capabilities` value. Pure — equal inputs
/// always produce equal outputs, and this function performs no I/O.
///
/// `max_context_tokens`, `parallel_tool_calls`, and `reliability_tier` are
/// the three fields `ModelOverrides` can set, so those three follow the
/// full `overrides > metadata > dialect_defaults` chain (with
/// `reliability_tier` additionally falling back to
/// [`quantization_tier_hint`] between `metadata`'s explicit tier and the
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
