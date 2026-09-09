//! `/model` bare's own model-listing helpers -- pure, state-free functions
//! factored out of `commands.rs` so "which `\"backend/model\"` strings does
//! the picker offer" and "does a typed filter match one of them" are each
//! unit-testable in isolation, with no `AppState`/`Host` in sight.
//!
//! # Where the picker's candidate list comes from -- and one source it does
//! NOT cover
//!
//! [`candidate_models`] unions every `"backend/model"` pair an
//! operator-configured role's own `chain` lists (`AppState::
//! configured_models`, already computed by `App::refresh_default_entries`)
//! with every model recorded in the local model-metadata file
//! (`AppState::model_max_context`'s own keys -- `.conway/models.json`,
//! already loaded once by `ConwayBuilder::build` and carried on `AppState`,
//! so this reads no file itself). It does **not** additionally enumerate
//! "whatever each configured backend declares" on top of those two:
//! `conway-plugin-backends` carries no static per-backend model catalogue
//! today (only runtime capability *resolution* for a model already named
//! some other way), and reaching for one would mean either a live provider
//! roster fetch (explicitly out of scope here) or a new catalogue this
//! change does not add.
//!
//! # Why this module has no `AppState`-mutating function at all
//!
//! A picker where typing narrows the list live and one more key promotes
//! the highlighted entry to the persistent default needs a new render
//! surface (`view/`), a new key handler (`input.rs`), and either a new
//! `Mode` variant or a new field on `AppState` (`state.rs`) to remember
//! which entry is highlighted and what's been typed so far between
//! keystrokes -- this change touches none of those three files. What ships
//! here is the data layer those three files would need to wire the rest
//! in, plus the two places `commands.rs` reuses it today without touching
//! any of them: bare `/model` (every reachable model, no `conway.ui`
//! plugin required -- see `commands::execute`'s `Model { model: None }`
//! arm) and `/model <text>` with no exact match (opens the SAME listing
//! pre-filtered by `<text>` instead of erroring -- see that arm's `Model {
//! model: Some(_) }` sibling).

use std::collections::BTreeSet;

/// Every model `/model` bare's picker should offer: the union of
/// `chain_models` (every `"backend/model"` pair an operator-configured
/// role's own `chain` names) and `metadata_models` (every key the local
/// model-metadata file records) -- sorted and deduped, a `BTreeSet` giving
/// both for free, the identical trick `AppState::configured_models`'s own
/// construction already uses. See this module's own doc for the third
/// source this deliberately does not cover.
pub fn candidate_models(
    chain_models: &[String],
    metadata_models: impl Iterator<Item = String>,
) -> Vec<String> {
    let mut set: BTreeSet<String> = chain_models.iter().cloned().collect();
    set.extend(metadata_models);
    set.into_iter().collect()
}

/// Case-insensitive substring filter over an already-built candidate list
/// (typically [`candidate_models`]'s own output) -- what `/model <text>`
/// opens the picker pre-filtered by when `<text>` is not itself a
/// syntactically valid `"backend/model"` pair (`commands::execute`'s
/// `Model { model: Some(_) }` arm decides that part; this function only
/// answers "does this candidate's name contain the query").  Preserves
/// `models`' own relative (sorted) order. An empty `query` matches
/// everything, unfiltered -- bare `/model`'s own case, expressed as a
/// filter with nothing typed yet rather than a second code path.
pub fn filter_models(models: &[String], query: &str) -> Vec<String> {
    if query.is_empty() {
        return models.to_vec();
    }
    let needle = query.to_lowercase();
    models
        .iter()
        .filter(|m| m.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_models_unions_chain_and_metadata_sorted_and_deduped() {
        let chain = vec![
            "anthropic/claude-haiku".to_string(),
            "openai/gpt-5".to_string(),
        ];
        // "openai/gpt-5" also appears in the metadata map (models.json
        // records context windows for chain members too) -- must appear
        // exactly once in the result, not twice.
        let metadata = vec![
            "openai/gpt-5".to_string(),
            "local/qwen3.8:27b-mlx".to_string(),
        ];

        let got = candidate_models(&chain, metadata.into_iter());

        assert_eq!(
            got,
            vec![
                "anthropic/claude-haiku".to_string(),
                "local/qwen3.8:27b-mlx".to_string(),
                "openai/gpt-5".to_string(),
            ],
            "must union both sources, sorted, with the shared entry deduped"
        );
    }

    #[test]
    fn candidate_models_with_no_metadata_returns_the_chain_alone_sorted() {
        let chain = vec![
            "openai/gpt-5".to_string(),
            "anthropic/claude-haiku".to_string(),
        ];

        let got = candidate_models(&chain, std::iter::empty());

        assert_eq!(
            got,
            vec![
                "anthropic/claude-haiku".to_string(),
                "openai/gpt-5".to_string()
            ]
        );
    }

    #[test]
    fn candidate_models_with_neither_source_is_empty() {
        assert!(candidate_models(&[], std::iter::empty()).is_empty());
    }

    #[test]
    fn filter_models_matches_a_case_insensitive_substring() {
        let models = vec![
            "anthropic/claude-haiku".to_string(),
            "anthropic/claude-sonnet-4-6".to_string(),
            "openai/gpt-5".to_string(),
        ];

        let got = filter_models(&models, "CLAUDE");

        assert_eq!(
            got,
            vec![
                "anthropic/claude-haiku".to_string(),
                "anthropic/claude-sonnet-4-6".to_string(),
            ],
            "a wrong-case query must still match, and the non-matching entry \
             must be excluded"
        );
    }

    #[test]
    fn filter_models_with_an_empty_query_returns_every_model_unfiltered() {
        let models = vec![
            "anthropic/claude-haiku".to_string(),
            "openai/gpt-5".to_string(),
        ];

        assert_eq!(filter_models(&models, ""), models);
    }

    #[test]
    fn filter_models_with_no_match_returns_empty_not_everything() {
        let models = vec!["anthropic/claude-haiku".to_string()];

        assert!(
            filter_models(&models, "zzz-nonexistent").is_empty(),
            "a query with no hits must never silently fall back to the full list"
        );
    }
}
