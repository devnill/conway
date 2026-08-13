//! The pure predicate the router uses for capability filtering (WI-032,
//! amended: headroom-aware context gate; further amended, board item
//! 01KZFBZHTWDF11TH7G0H613ERE, to express the size half of that gate through
//! `conway_core::ports::Admission` and to move `CapabilityIndex` to
//! `conway-core`'s `ports` module — see that module's own doc).
//!
//! Reconciliation with `conway-core` (documented choice): core's
//! `RequiredCaps::satisfied_by(&Capabilities, est_tokens)` already enforces
//! the same admission rule using the `headroom_tokens` field carried *inside*
//! `RequiredCaps`, with core's own message wording. Routing's skip reasons
//! are bound to the amendment's exact strings, and the router resolves the
//! effective headroom per role and passes it explicitly, so [`satisfies`]
//! here is a self-contained rendering of the same rule over four scalars.
//! The two formulations are pinned together by
//! `satisfies_agrees_with_core_on_accept_reject` below: for identical
//! inputs they must agree on ACCEPT vs REJECT (messages may differ; the
//! decision may not). As of 01KZFBZHTWDF11TH7G0H613ERE, the SIZE half of
//! both formulations is no longer two independent restatements of
//! `est_tokens + headroom_tokens <= max_context_tokens`: both
//! [`size_missing`] here and `RequiredCaps::satisfied_by`'s own context
//! check build a `conway_core::ports::Admission` and read its
//! `fits`/`required_tokens`/`shortfall_tokens` -- one implementation, never
//! restated here. What still
//! legitimately differs between the two -- and is why a single surviving
//! function did not replace both -- is per-field MESSAGE rendering (routing
//! is bound to the amendment's exact CLI-facing strings; core's own public
//! `satisfied_by` has its own, differently-ordered, differently-worded
//! contract, exercised directly by `conway-core`'s own tests) and, for the
//! six non-size fields, independently-written rank/equality comparisons
//! that were never the same arithmetic to begin with.

use conway_core::capabilities::{
    Capabilities, ReliabilityTier, RequiredCaps, StructuredOutput, ToolCallSupport,
};
use conway_core::ports::Admission;

/// Rank helpers over core's `#[non_exhaustive]` enums. Unknown future
/// variants rank as their documented-lowest peer; never panic.
fn structured_rank(s: &StructuredOutput) -> u8 {
    match s {
        StructuredOutput::None => 0,
        StructuredOutput::JsonSchema => 1,
        StructuredOutput::Grammar => 2,
        _ => 0,
    }
}

fn reliability_rank(t: &ReliabilityTier) -> u8 {
    match t {
        ReliabilityTier::Unknown => 0,
        ReliabilityTier::Community => 1,
        ReliabilityTier::Verified => 2,
        _ => 0,
    }
}

fn tool_support_name(t: &ToolCallSupport) -> &'static str {
    match t {
        ToolCallSupport::None => "None",
        ToolCallSupport::NonStreamingOnly => "NonStreamingOnly",
        ToolCallSupport::Streaming { validated: false } => "Streaming",
        ToolCallSupport::Streaming { validated: true } => "Streaming(validated)",
        _ => "Unknown",
    }
}

fn structured_name(s: &StructuredOutput) -> &'static str {
    match s {
        StructuredOutput::None => "None",
        StructuredOutput::JsonSchema => "JsonSchema",
        StructuredOutput::Grammar => "Grammar",
        _ => "Unknown",
    }
}

fn reliability_name(t: &ReliabilityTier) -> &'static str {
    match t {
        ReliabilityTier::Unknown => "Unknown",
        ReliabilityTier::Community => "Community",
        ReliabilityTier::Verified => "Verified",
        _ => "Unknown",
    }
}

/// Combines a role's configured capability floor (`RoutingConfig.roles.
/// <alias>.required`) with the caller-supplied `req.required` into the
/// single strictest requirement a candidate must clear: each field
/// independently takes whichever of the two is more demanding (higher
/// rank / larger minimum / `true` over `false`/`None`). Neither side can
/// ever WEAKEN the other -- a role floor can only add restrictions on top
/// of what a caller already asked for, and vice versa.
///
/// Mirrors `DeclarativeRouter::effective_headroom`'s own "config default,
/// resolved once per role" shape, generalized from one scalar to the whole
/// `RequiredCaps` struct. `headroom_tokens` is deliberately left
/// untouched (copied from `request`) -- headroom is resolved and passed to
/// `satisfies` as its own explicit parameter (see this module's own doc
/// comment), never read from either side's `required.headroom_tokens`
/// field, so merging it here would be dead code.
pub(crate) fn strictest(config_floor: &RequiredCaps, request: &RequiredCaps) -> RequiredCaps {
    RequiredCaps {
        tool_calling: strictest_by_rank(config_floor.tool_calling, request.tool_calling, |t| {
            t.rank()
        }),
        structured_output: strictest_by_rank(
            config_floor.structured_output,
            request.structured_output,
            |s| structured_rank(&s),
        ),
        parallel_tool_calls: strictest_bool(
            config_floor.parallel_tool_calls,
            request.parallel_tool_calls,
        ),
        reasoning: strictest_bool(config_floor.reasoning, request.reasoning),
        min_reliability: strictest_by_rank(
            config_floor.min_reliability,
            request.min_reliability,
            |t| reliability_rank(&t),
        ),
        min_context: strictest_min(config_floor.min_context, request.min_context),
        headroom_tokens: request.headroom_tokens,
    }
}

/// `Some` wins over `None`; two `Some`s keep whichever ranks higher (a tie
/// keeps `a`, arbitrarily -- the two are equally strict by definition).
fn strictest_by_rank<T: Copy>(a: Option<T>, b: Option<T>, rank: impl Fn(T) -> u8) -> Option<T> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(if rank(x) >= rank(y) { x } else { y }),
    }
}

/// `Some(true)` from either side wins (a requirement, once asserted by
/// either the config floor or the caller, cannot be un-asserted by the
/// other); otherwise whichever side is `Some(_)`, else `None`.
fn strictest_bool(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    if a == Some(true) || b == Some(true) {
        Some(true)
    } else {
        a.or(b)
    }
}

/// The larger of the two minimums; `Some` wins over `None`.
fn strictest_min(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(x.max(y)),
    }
}

/// The non-size half of the capability predicate: every requirement in
/// `required` EXCEPT the context/headroom gate, in the fixed order
/// tool_calling, structured_output, parallel_tool_calls, reasoning,
/// reliability_tier, min_context. Returns every unmet requirement (never
/// short-circuits on the first) as a human-readable string.
///
/// Split out (board item 01KZFBZHTWDF11TH7G0H613ERE) so `router.rs`'s
/// `check_candidate` can ask "did anything OTHER than size fail?"
/// structurally -- `non_size_missing(..).is_empty()` -- rather than by
/// counting strings in `satisfies`'s combined `Vec`.
pub(crate) fn non_size_missing(caps: &Capabilities, required: &RequiredCaps) -> Vec<String> {
    let mut missing: Vec<String> = Vec::new();

    if let Some(required_tc) = &required.tool_calling {
        if caps.tool_calling.rank() < required_tc.rank() {
            missing.push(format!(
                "tool_calling: requires {}, has {}",
                tool_support_name(required_tc),
                tool_support_name(&caps.tool_calling)
            ));
        }
    }

    if let Some(required_so) = &required.structured_output {
        if structured_rank(&caps.structured_output) < structured_rank(required_so) {
            missing.push(format!(
                "structured_output: requires {}, has {}",
                structured_name(required_so),
                structured_name(&caps.structured_output)
            ));
        }
    }

    if required.parallel_tool_calls == Some(true) && !caps.parallel_tool_calls {
        missing.push("parallel_tool_calls: required".to_string());
    }

    if required.reasoning == Some(true) && !caps.reasoning {
        missing.push("reasoning: required".to_string());
    }

    if let Some(required_tier) = &required.min_reliability {
        if reliability_rank(&caps.reliability_tier) < reliability_rank(required_tier) {
            missing.push(format!(
                "reliability_tier: requires {}, has {}",
                reliability_name(required_tier),
                reliability_name(&caps.reliability_tier)
            ));
        }
    }

    if let Some(min_context) = required.min_context {
        if caps.max_context_tokens < min_context {
            missing.push(format!(
                "min_context: requires >= {min_context}, has {}",
                caps.max_context_tokens
            ));
        }
    }

    missing
}

/// The size half of the capability predicate, expressed through
/// `conway_core::ports::Admission` (the headroom arithmetic itself, in one place --
/// `fits`/`required_tokens`/`shortfall_tokens` -- lives in exactly one
/// place, not restated here). `Some(entry)` is the CONTRACT golden string
/// (amendment ordering: `"context: needs {est_tokens} input +
/// {headroom_tokens} headroom = {needed}, model max_context_tokens is
/// {max}"`) when the request does not fit; `None` when it does. `router.rs`'s
/// `check_candidate` calls this directly for its structural headroom-only
/// discrimination; [`satisfies`] below calls it to build its own combined
/// `missing` list.
///
/// Saturating: `u32::MAX` inputs produce a rejection, never a wrap or panic.
/// Inclusive bound: `est_tokens + headroom_tokens == max_context_tokens`
/// fits (`None`).
pub(crate) fn size_missing(
    caps: &Capabilities,
    est_tokens: u32,
    headroom_tokens: u32,
) -> Option<String> {
    let admission = Admission {
        est_tokens,
        headroom_tokens,
        max_context_tokens: caps.max_context_tokens,
    };
    if admission.fits() {
        None
    } else {
        Some(format!(
            "context: needs {est_tokens} input + {headroom_tokens} headroom = {}, \
             model max_context_tokens is {}",
            admission.required_tokens(),
            caps.max_context_tokens
        ))
    }
}

/// Pure, synchronous admission predicate over four scalars: no `&self`, no
/// clock, no registry. Returns `Ok(())` (allocation-free) when every
/// requirement holds, else `Err` listing **every** unmet requirement in the
/// fixed order: tool_calling, structured_output, parallel_tool_calls,
/// reasoning, reliability_tier, min_context, context (headroom gate last) --
/// a thin composition of `non_size_missing` and `size_missing`, appended
/// in that same fixed order (CONTRACT: the context entry is always last,
/// with the exact string `size_missing` documents).
///
/// The caller (the router, WI-034) resolves the effective headroom once per
/// role and passes it in — headroom policy lives in exactly one place.
pub fn satisfies(
    caps: &Capabilities,
    required: &RequiredCaps,
    est_tokens: u32,
    headroom_tokens: u32,
) -> Result<(), Vec<String>> {
    let mut missing = non_size_missing(caps, required);
    missing.extend(size_missing(caps, est_tokens, headroom_tokens));

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::capabilities::CacheMode;

    /// STRUCTURAL discrimination (replaces the old pin that counted
    /// `missing`'s entries, board item 01KZFBZHTWDF11TH7G0H613ERE): a
    /// headroom-only failure is exactly `non_size_missing(..).is_empty() &&
    /// size_missing(..).is_some()` -- never a string count. `router.rs`'s
    /// `check_candidate` consumes these two functions directly, not
    /// `satisfies`'s combined `Vec<String>`.
    ///
    /// Without this, the router's classification rests on an unstated
    /// property of these functions. If someone splits the context entry
    /// into two strings, this test fails here rather than silently turning
    /// every headroom-only rejection back into a `NoCandidate` (board item
    /// `01KYXNAHN64YMADZPQDQC0CPTJ`).
    #[test]
    fn headroom_only_failure_is_non_size_empty_and_size_some() {
        let fits_everything_but_context = caps(40_000);
        let required = RequiredCaps::default();

        assert!(
            non_size_missing(&fits_everything_but_context, &required).is_empty(),
            "no non-size requirement is set"
        );
        assert!(
            size_missing(&fits_everything_but_context, 34_000, 16_000).is_some(),
            "50000 needed against a 40000 window must not fit"
        );

        // `satisfies`'s combined view agrees: exactly one entry, the
        // context one.
        let missing = satisfies(&fits_everything_but_context, &required, 34_000, 16_000)
            .expect_err("50000 needed against a 40000 window must be rejected");
        assert_eq!(missing.len(), 1);
        assert!(missing[0].starts_with("context:"));
    }

    /// The complement: a non-size failure alone -- `non_size_missing(..)`
    /// non-empty, `size_missing(..)` `None` -- is never classified
    /// headroom-only.
    #[test]
    fn non_context_failure_alone_is_never_classified_as_headroom_only() {
        let roomy_but_weak = weak_caps();
        let required = RequiredCaps {
            reasoning: Some(true),
            ..RequiredCaps::default()
        };

        assert!(!non_size_missing(&roomy_but_weak, &required).is_empty());
        assert!(
            size_missing(&roomy_but_weak, 10, 10).is_none(),
            "the window is ample; nothing to report on size"
        );

        let missing = satisfies(&roomy_but_weak, &required, 10, 10)
            .expect_err("weak_caps has reasoning=false, so this must be rejected");
        assert!(!missing.iter().any(|m| m.starts_with("context:")));
    }

    /// The MIXED case: both halves fail at once -- `non_size_missing(..)`
    /// non-empty AND `size_missing(..)` `Some` -- which `check_candidate`
    /// must NOT classify as headroom-only -- a mixed rejection is reported as
    /// such, since core surfaces a refusal rather than routing around it.
    #[test]
    fn mixed_failure_is_neither_non_size_empty_nor_size_none() {
        let weak_and_small = Capabilities {
            tool_calling: ToolCallSupport::None,
            ..caps(40_000)
        };
        let required = RequiredCaps {
            tool_calling: Some(ToolCallSupport::NonStreamingOnly),
            ..RequiredCaps::default()
        };

        assert!(!non_size_missing(&weak_and_small, &required).is_empty());
        assert!(size_missing(&weak_and_small, 34_000, 16_000).is_some());
    }

    fn caps(max_context_tokens: u32) -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::Streaming { validated: true },
            cache: CacheMode::None,
            parallel_tool_calls: true,
            structured_output: StructuredOutput::Grammar,
            max_context_tokens,
            reasoning: true,
            reliability_tier: ReliabilityTier::Verified,
        }
    }

    fn weak_caps() -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::None,
            cache: CacheMode::None,
            parallel_tool_calls: false,
            structured_output: StructuredOutput::None,
            max_context_tokens: 32_768,
            reasoning: false,
            reliability_tier: ReliabilityTier::Community,
        }
    }

    fn demanding() -> RequiredCaps {
        RequiredCaps {
            tool_calling: Some(ToolCallSupport::NonStreamingOnly),
            structured_output: Some(StructuredOutput::JsonSchema),
            parallel_tool_calls: Some(true),
            reasoning: Some(true),
            min_reliability: Some(ReliabilityTier::Verified),
            min_context: Some(40_000),
            ..RequiredCaps::default()
        }
    }

    #[test]
    fn ordering_lists_every_unmet_requirement_context_last() {
        let err = satisfies(&weak_caps(), &demanding(), 34_000, 16_000).unwrap_err();
        let prefixes: Vec<&str> = err.iter().map(|s| s.split(':').next().unwrap()).collect();
        assert_eq!(
            prefixes,
            vec![
                "tool_calling",
                "structured_output",
                "parallel_tool_calls",
                "reasoning",
                "reliability_tier",
                "min_context",
                "context"
            ]
        );
    }

    #[test]
    fn golden_strings_match_exactly() {
        let err = satisfies(&weak_caps(), &demanding(), 34_000, 16_000).unwrap_err();
        assert_eq!(err[0], "tool_calling: requires NonStreamingOnly, has None");
        assert_eq!(err[1], "structured_output: requires JsonSchema, has None");
        assert_eq!(err[2], "parallel_tool_calls: required");
        assert_eq!(err[3], "reasoning: required");
        assert_eq!(err[4], "reliability_tier: requires Verified, has Community");
        assert_eq!(err[5], "min_context: requires >= 40000, has 32768");
        assert_eq!(
            err[6],
            "context: needs 34000 input + 16000 headroom = 50000, \
             model max_context_tokens is 32768"
        );
    }

    #[test]
    fn golden_context_string_verbatim_per_amendment() {
        // The amendment's exact fixture: max 40000, est 34000, headroom 16000.
        let err = satisfies(&caps(40_000), &RequiredCaps::default(), 34_000, 16_000).unwrap_err();
        assert_eq!(
            err,
            vec!["context: needs 34000 input + 16000 headroom = 50000, \
                 model max_context_tokens is 40000"
                .to_string()]
        );
    }

    #[test]
    fn headroom_rejects_candidate_that_fits_raw_input() {
        // Fits the raw input (34000 <= 40000) but not input + headroom.
        assert!(satisfies(&caps(40_000), &RequiredCaps::default(), 34_000, 16_000).is_err());
        assert!(satisfies(&caps(40_000), &RequiredCaps::default(), 34_000, 0).is_ok());
    }

    #[test]
    fn boundary_exact_fit_passes_one_over_fails() {
        assert!(satisfies(&caps(50_000), &RequiredCaps::default(), 34_000, 16_000).is_ok());
        assert!(satisfies(&caps(49_999), &RequiredCaps::default(), 34_000, 16_000).is_err());
    }

    #[test]
    fn saturating_arithmetic_never_panics() {
        // Saturation, not wrap: at a u32::MAX window the saturated sum
        // equals the window, so the gate admits — the point is no panic
        // and no wrap-to-small-number false admission.
        let ok = satisfies(&caps(u32::MAX), &RequiredCaps::default(), u32::MAX, 16_000);
        assert!(ok.is_ok(), "saturated sum == window is admissible");
        let err = satisfies(&caps(100), &RequiredCaps::default(), u32::MAX, u32::MAX);
        assert!(err.is_err(), "saturated sum still rejects a real window");
    }

    #[test]
    fn min_context_and_headroom_gate_are_independent() {
        let required = RequiredCaps {
            min_context: Some(200_000),
            ..RequiredCaps::default()
        };
        let err = satisfies(&caps(50_000), &required, 40_000, 16_000).unwrap_err();
        assert_eq!(err.len(), 2);
        assert!(err[0].starts_with("min_context:"));
        assert!(err[1].starts_with("context:"));
    }

    /// Reconciliation pin (re-aimed, board item 01KZFBZHTWDF11TH7G0H613ERE):
    /// routing's `satisfies` and core's `RequiredCaps::satisfied_by` --
    /// TWO surviving renderings of ONE admission decision, not two
    /// independent implementations of it -- must agree on ACCEPT vs REJECT
    /// for identical inputs (messages may differ; the decision may not).
    /// This is what "the surviving single source" actually is here: the SIZE
    /// arithmetic both call is now the SAME code, `conway_core::ports::
    /// Admission` (see `size_missing`'s own doc and `RequiredCaps::
    /// satisfied_by`'s context-check block) -- deleting either renderer
    /// outright is not possible without breaking a real, distinct contract
    /// (`satisfies`'s CLI-facing golden strings here; `satisfied_by`'s own
    /// public API and tests in `conway-core`), so this test is what keeps
    /// the two renderings from silently drifting on the one thing that must
    /// never differ between them: the verdict.
    ///
    /// Core reads headroom from `required.headroom_tokens`, so the pin sets
    /// that field to the value passed to `satisfies`. `structured_output` is
    /// held to `None` here: core uses equality where routing uses rank
    /// ordering (a documented, deliberate difference in requirement
    /// semantics, not in admission arithmetic).
    #[test]
    fn satisfies_agrees_with_core_on_accept_reject() {
        let windows = [10_000u32, 32_768, 50_000, 200_000];
        let inputs = [
            (0u32, 0u32),
            (30_000, 8_192),
            (34_000, 16_000),
            (200_000, 0),
        ];
        for window in windows {
            for (est, headroom) in inputs {
                let required = RequiredCaps {
                    headroom_tokens: headroom,
                    ..RequiredCaps::default()
                };
                let candidate = caps(window);
                let routing_ok = satisfies(&candidate, &required, est, headroom).is_ok();
                let core_ok = required.satisfied_by(&candidate, est).is_ok();
                assert_eq!(
                    routing_ok, core_ok,
                    "decision drift at window={window} est={est} headroom={headroom}"
                );
            }
        }
    }
}
