//! One shared formatting rule for a `Usage`'s cache figures, called from
//! every TUI renderer that shows a token count alongside a cache
//! percentage (`tui::state::turn_summary`'s turn-end summary,
//! `tui::view::status`'s `tokens` status-line field) so the two surfaces
//! can never drift apart on the rule itself.
//!
//! Declaration honesty (board item: providers that report no cache
//! figures were indistinguishable from providers that report a genuine
//! zero): `CacheAccounting::Reported` always renders a percentage,
//! including `0% cached` -- the provider said zero, so `0%` is what it
//! said. `CacheAccounting::NotReported` never renders a percentage at all
//! -- `cache_read_tokens`/`cache_write_tokens` are zero-filled
//! placeholders, not observations, so a `0%` there would be a claim the
//! backend never made.
//!
//! **Board item `01M2NS0996E139VN5R8W4PGD8V`: "not reported" and "not
//! supported" are different facts.** `CacheAccounting::NotReported` alone
//! only says "this turn's response carried no cache field" -- it cannot
//! say WHY, and collapsing every why into one wording (the pre-existing
//! defect this item closes) makes a backend whose wire dialect
//! STRUCTURALLY NEVER carries a cache field (e.g. Ollama's native
//! `/api/chat`) look identical to a backend that is declared cache-capable
//! (`Backend::cache_reporting() == CacheReporting::Reported`, e.g. any
//! `dialect: "openai"` profile) but simply had a quiet turn. The former is
//! permanent and not actionable; the latter is exactly the signal an
//! operator watching this line is looking for. `cache_suffix` now also
//! takes the backend's declared `CacheReporting` (already resolved once at
//! `App::new` via `Conway::capability_index()`, never re-derived here) so
//! it can render three DISTINCT facts instead of two:
//!
//! - `cache_reporting == Some(CacheReporting::NotReported)`: structural,
//!   permanent -- `"not supported"`.
//! - `cache_reporting == Some(CacheReporting::Reported)`: this backend
//!   DOES have somewhere to report a cache hit; it just didn't this turn
//!   -- `"not reported"` (the pre-existing wording, kept for exactly this
//!   fact).
//! - `cache_reporting == None`: the capability itself is unknown to this
//!   caller (e.g. its backend id was never configured/injected, or state
//!   hasn't resolved a `ModelDecision` yet) -- a THIRD fact, per
//!   `conway routes explain`'s own `render_cache_reporting` precedent ("a
//!   candidate this report could not ask is not the same fact as one that
//!   answered 'no'"): `"reporting unknown"`, never folded into either of
//!   the other two. An unrecognized future `CacheReporting` variant
//!   (`#[non_exhaustive]` -- this crate does not own that type) renders the
//!   same "reporting unknown" wording, mirroring `render_cache_reporting`'s
//!   own `Some(_) => "unknown"` arm.

use conway::{CacheAccounting, CacheReporting, Usage};

/// The cache-accounting suffix for a token-count line: `" (N% cached)"`
/// when `usage.cache_accounting` is `Reported` and at least one
/// cache-relevant token was processed (`input_tokens` + both cache
/// dimensions > 0 -- else there is nothing to compute a rate over, so no
/// suffix at all, matching the pre-existing "no usage yet" case).
///
/// When `usage.cache_accounting` is `NotReported`, `cache_reporting` (the
/// focused backend's declared [`CacheReporting`], `None` when unknown to
/// the caller) decides which of three DISTINCT wordings renders -- see
/// this module's own doc for the full reasoning:
///
/// - `Some(CacheReporting::NotReported)`: `" (cache: not supported[ by
///   <backend id>])"`.
/// - `Some(CacheReporting::Reported)`: `" (cache: not reported[ by
///   <backend id>])"`.
/// - `None`: `" (cache: reporting unknown[ for <backend id>])"`.
///
/// Every branch's `[ ...<backend id>]` clause is present only when
/// `focused_model` is `Some("backend/model")`-shaped; a malformed or
/// missing `focused_model` falls back to the bare form. Returns `""` when
/// neither applies. The leading space lets every call site simply append
/// the result after its own `"{total} tok"` text.
pub(crate) fn cache_suffix(
    usage: &Usage,
    focused_model: Option<&str>,
    cache_reporting: Option<CacheReporting>,
) -> String {
    match usage.cache_accounting {
        CacheAccounting::NotReported => {
            let backend = focused_model.and_then(backend_id);
            match cache_reporting {
                Some(CacheReporting::NotReported) => match backend {
                    Some(backend_id) => format!(" (cache: not supported by {backend_id})"),
                    None => " (cache: not supported)".to_string(),
                },
                Some(CacheReporting::Reported) => match backend {
                    Some(backend_id) => format!(" (cache: not reported by {backend_id})"),
                    None => " (cache: not reported)".to_string(),
                },
                // `None` (unresolved by this caller) and an unrecognized
                // future `CacheReporting` variant (`#[non_exhaustive]` --
                // this crate does not own that type, see
                // `commands::routes::render_cache_reporting`'s identical
                // `Some(_) => "unknown"` precedent) both fold into the same
                // THIRD wording: neither is a claim this call site can tell
                // apart from the operator's point of view, and neither may
                // be silently read as either of the two named facts above.
                _ => match backend {
                    Some(backend_id) => format!(" (cache: reporting unknown for {backend_id})"),
                    None => " (cache: reporting unknown)".to_string(),
                },
            }
        }
        CacheAccounting::Reported => {
            let denom = u64::from(usage.input_tokens)
                + u64::from(usage.cache_read_tokens)
                + u64::from(usage.cache_write_tokens);
            if denom == 0 {
                String::new()
            } else {
                let pct = (u64::from(usage.cache_read_tokens) * 100) / denom;
                format!(" ({pct}% cached)")
            }
        }
    }
}

/// Extracts the backend id from a `"backend/model"`-shaped focused-model
/// string (`AppState::focused_model`'s own shape, `ModelRef::to_string()`).
/// `None` for an empty backend id (a malformed string with a leading `/`)
/// -- `"not reported by "` with nothing after it would be worse than the
/// generic fallback.
fn backend_id(focused_model: &str) -> Option<&str> {
    let id = focused_model.split('/').next()?;
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(cache_read: u32, accounting: CacheAccounting) -> Usage {
        Usage {
            input_tokens: 100,
            output_tokens: 400,
            cache_read_tokens: cache_read,
            cache_write_tokens: 100,
            reasoning_tokens: 0,
            cache_accounting: accounting,
        }
    }

    #[test]
    fn reported_renders_zero_percent_when_no_cache_hit() {
        let u = usage(0, CacheAccounting::Reported);
        assert_eq!(cache_suffix(&u, None, None), " (0% cached)");
    }

    #[test]
    fn reported_renders_nonzero_percent() {
        let u = usage(800, CacheAccounting::Reported);
        // 800 / (100 + 800 + 100) = 80%.
        assert_eq!(cache_suffix(&u, None, None), " (80% cached)");
    }

    #[test]
    fn reported_with_no_usage_at_all_renders_nothing() {
        let u = Usage {
            cache_accounting: CacheAccounting::Reported,
            ..Usage::default()
        };
        assert_eq!(cache_suffix(&u, None, None), "");
    }

    #[test]
    fn reported_percent_is_unaffected_by_cache_reporting_capability() {
        // The percent branch only ever looks at `usage.cache_accounting` --
        // whatever the caller passes for `cache_reporting` must not change
        // a turn that genuinely DID carry a cache field.
        let u = usage(800, CacheAccounting::Reported);
        assert_eq!(
            cache_suffix(&u, None, Some(CacheReporting::NotReported)),
            " (80% cached)"
        );
        assert_eq!(
            cache_suffix(&u, None, Some(CacheReporting::Reported)),
            " (80% cached)"
        );
    }

    // -- `CacheAccounting::NotReported`, `cache_reporting ==
    // Some(CacheReporting::Reported)`: this backend has somewhere to
    // report a cache hit, it just didn't this turn -- the pre-existing
    // "not reported" wording, kept for exactly this fact.

    #[test]
    fn not_reported_names_the_backend_when_capable_but_quiet_this_turn() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(
            cache_suffix(&u, Some("openai/gpt-5"), Some(CacheReporting::Reported)),
            " (cache: not reported by openai)"
        );
    }

    #[test]
    fn not_reported_falls_back_to_the_generic_form_without_a_focused_model() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(
            cache_suffix(&u, None, Some(CacheReporting::Reported)),
            " (cache: not reported)"
        );
    }

    #[test]
    fn not_reported_falls_back_on_a_malformed_focused_model_string() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(
            cache_suffix(&u, Some("/no-backend"), Some(CacheReporting::Reported)),
            " (cache: not reported)"
        );
    }

    // -- `CacheAccounting::NotReported`, `cache_reporting ==
    // Some(CacheReporting::NotReported)`: the backend's wire dialect
    // structurally never carries a cache field at all -- permanent, not
    // actionable, and must NOT read like the transient "not reported"
    // case above (board item `01M2NS0996E139VN5R8W4PGD8V`'s own defect).
    // Renders as "not supported", a distinct STRING from the "not
    // reported" wording above -- there is no separate `CacheReporting`
    // enum variant for it, only the two the type already has.

    #[test]
    fn not_supported_names_the_backend_when_structurally_incapable() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(
            cache_suffix(&u, Some("ollama/gemma4:e4b"), Some(CacheReporting::NotReported)),
            " (cache: not supported by ollama)"
        );
    }

    #[test]
    fn not_supported_falls_back_to_the_generic_form_without_a_focused_model() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(
            cache_suffix(&u, None, Some(CacheReporting::NotReported)),
            " (cache: not supported)"
        );
    }

    #[test]
    fn not_supported_falls_back_on_a_malformed_focused_model_string() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(
            cache_suffix(&u, Some("/no-backend"), Some(CacheReporting::NotReported)),
            " (cache: not supported)"
        );
    }

    // -- `CacheAccounting::NotReported`, `cache_reporting == None`: the
    // capability itself is unknown to this caller -- a THIRD fact, never
    // folded into either "not reported" or "not supported" above.

    #[test]
    fn unknown_capability_names_the_backend_when_a_focused_model_is_known() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(
            cache_suffix(&u, Some("kimi/moonshot-v1"), None),
            " (cache: reporting unknown for kimi)"
        );
    }

    #[test]
    fn unknown_capability_falls_back_to_the_generic_form_without_a_focused_model() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(cache_suffix(&u, None, None), " (cache: reporting unknown)");
    }

    #[test]
    fn unknown_capability_falls_back_on_a_malformed_focused_model_string() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(
            cache_suffix(&u, Some("/no-backend"), None),
            " (cache: reporting unknown)"
        );
    }

    #[test]
    fn the_three_not_reported_wordings_are_pairwise_distinct() {
        // The board item's own defect statement: "'not reported' and 'not
        // supported' are different wordings. They are different facts and
        // collapsing them is the defect." -- assert all three renderings
        // this module now distinguishes are pairwise different, with and
        // without a named backend, so no future edit can quietly re-merge
        // any two of them.
        let u = usage(0, CacheAccounting::NotReported);
        for focused_model in [None, Some("openai/gpt-5")] {
            let not_reported = cache_suffix(&u, focused_model, Some(CacheReporting::Reported));
            let not_supported = cache_suffix(&u, focused_model, Some(CacheReporting::NotReported));
            let unknown = cache_suffix(&u, focused_model, None);
            assert_ne!(not_reported, not_supported);
            assert_ne!(not_reported, unknown);
            assert_ne!(not_supported, unknown);
        }
    }
}
