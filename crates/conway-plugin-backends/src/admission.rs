//! The one estimator both shipped dialects call to turn their OWN
//! already-built wire body into `est_tokens`.
//!
//! `estimate_wire_tokens` is deliberately dialect-agnostic *as a function*
//! while still yielding a genuinely different answer per dialect for
//! identical logical content, because an `AnthropicBackend` request body and
//! an `OpenAiCompatBackend` request body for the same segments/tools are
//! different byte sequences (different field names, message envelope,
//! system-prompt handling, tool-schema shape). Each adapter builds ITS OWN
//! wire body (`wire::build_request_body`) before calling this — that is the
//! "tokenizer as the injected seam" the routing contract asks for. This
//! function is only the
//! "count the bytes" tail end of that seam, shared because there is nothing
//! dialect-specific left to say once the body is already built.
//!
//! The headroom arithmetic and fit comparison this estimate feeds is a
//! SEPARATE, single implementation — [`conway_core::ports::check_admission`] —
//! that both adapters also call. This module never performs that comparison
//! itself (one implementation: grep for `max_context_tokens` in this file and
//! find nothing).
//!
//! Same heuristic shape `conway-runtime`'s `ContextBuilder` documents as
//! `"heuristic-chars4"` (`docs/routing.md`): roughly `ceil(chars / 4)`.
//! Calibrating this against a response's reported `input_tokens` is the job
//! of a measured baseline (the headroom amendment's own text), not this one's.
//!
//! **Fidelity, declared, not left to be inferred (board item
//! 01M0AP4ADTGJWF3GFMCFWFF1ZQ):** both `AnthropicBackend::token_fidelity` and
//! `OpenAiCompatBackend::token_fidelity` override
//! `conway_core::ports::Backend::token_fidelity`'s default to state
//! `TokenCountFidelity::Heuristic` explicitly, with each override's own doc
//! explaining why (no vendored tokenizer, no measured calibration factor
//! available in this crate or this build/review environment). This module
//! staying `chars.div_ceil(4)` is therefore not an oversight left for a
//! later item to quietly improve — it is the honest ceiling of what this
//! crate can claim today, and the declaration makes that a visible fact
//! rather than something a reader has to infer from the absence of a
//! tokenizer dependency.

pub(crate) fn estimate_wire_tokens(body: &serde_json::Value) -> u32 {
    let rendered = body.to_string();
    let chars = u32::try_from(rendered.chars().count()).unwrap_or(u32::MAX);
    chars.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn estimate_scales_with_body_size() {
        let small = estimate_wire_tokens(&json!({"a": "hi"}));
        let big = estimate_wire_tokens(&json!({"a": "hi".repeat(1000)}));
        assert!(big > small, "a larger wire body must estimate more tokens");
    }

    #[test]
    fn estimate_is_deterministic() {
        let body = json!({"messages": [{"role": "user", "content": "hello"}]});
        assert_eq!(estimate_wire_tokens(&body), estimate_wire_tokens(&body));
    }

    #[test]
    fn empty_body_estimates_almost_nothing() {
        assert_eq!(estimate_wire_tokens(&json!({})), 1); // "{}" -> ceil(2/4)
    }
}
