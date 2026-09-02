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

use conway::{CacheAccounting, Usage};

/// The cache-accounting suffix for a token-count line: `" (N% cached)"`
/// when `usage.cache_accounting` is `Reported` and at least one
/// cache-relevant token was processed (`input_tokens` + both cache
/// dimensions > 0 -- else there is nothing to compute a rate over, so no
/// suffix at all, matching the pre-existing "no usage yet" case);
/// `" (cache: not reported by <backend id>)"` when `NotReported` and
/// `focused_model` is `Some("backend/model")`-shaped, else the bare
/// `" (cache: not reported)"`. Returns `""` when neither applies. The
/// leading space lets every call site simply append the result after its
/// own `"{total} tok"` text.
pub(crate) fn cache_suffix(usage: &Usage, focused_model: Option<&str>) -> String {
    match usage.cache_accounting {
        CacheAccounting::NotReported => match focused_model.and_then(backend_id) {
            Some(backend_id) => format!(" (cache: not reported by {backend_id})"),
            None => " (cache: not reported)".to_string(),
        },
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
        assert_eq!(cache_suffix(&u, None), " (0% cached)");
    }

    #[test]
    fn reported_renders_nonzero_percent() {
        let u = usage(800, CacheAccounting::Reported);
        // 800 / (100 + 800 + 100) = 80%.
        assert_eq!(cache_suffix(&u, None), " (80% cached)");
    }

    #[test]
    fn reported_with_no_usage_at_all_renders_nothing() {
        let u = Usage {
            cache_accounting: CacheAccounting::Reported,
            ..Usage::default()
        };
        assert_eq!(cache_suffix(&u, None), "");
    }

    #[test]
    fn not_reported_names_the_backend_when_a_focused_model_is_known() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(
            cache_suffix(&u, Some("ollama/gemma4:e4b")),
            " (cache: not reported by ollama)"
        );
    }

    #[test]
    fn not_reported_falls_back_to_the_generic_form_without_a_focused_model() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(cache_suffix(&u, None), " (cache: not reported)");
    }

    #[test]
    fn not_reported_falls_back_on_a_malformed_focused_model_string() {
        let u = usage(0, CacheAccounting::NotReported);
        assert_eq!(
            cache_suffix(&u, Some("/no-backend")),
            " (cache: not reported)"
        );
    }
}
