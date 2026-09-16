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
//! configured_models`, already computed by `App::refresh_default_entries`),
//! every model recorded in the local model-metadata file (`AppState::
//! model_max_context`'s own keys -- `.conway/models.json`, already loaded
//! once by `ConwayBuilder::build` and carried on `AppState`, so this reads
//! no file itself), and -- board item `01M24ZJ9ABPP0DGVAA2PS3XVDD` -- the
//! session's own `--model` pin (`AppState::model_pin`), when one was given.
//! The pin matters because a role chain cannot be built from the
//! environment at all (`conway::config::merge::env_to_value`'s own doc):
//! a launch with backends declared only through `CONWAY_BACKENDS__*` has an
//! empty `configured_models` by construction, and without the pin as a
//! third source the picker would have nothing to offer even though the
//! session is genuinely routing every turn to a real model.
//!
//! It does **not** additionally enumerate "whatever each configured
//! backend declares" on top of those three: `conway-plugin-backends`
//! carries no static per-backend model catalogue today (only runtime
//! capability *resolution* for a model already named some other way), and
//! reaching for one would mean either a live provider roster fetch
//! (explicitly out of scope here) or a new catalogue this change does not
//! add. A backend present in `AppState::configured_backend_ids` that no
//! candidate here names (no chain, no pin) is instead surfaced as a
//! `commands::execute`-pushed [`crate::tui::state::Entry::Notice`], one
//! layer up -- see [`backends_without_a_named_model`]'s own doc -- rather
//! than as an unparseable row in this list: every option this module
//! returns is round-trip-safe as a raw `ModelRef` string
//! (`open_model_picker`'s own doc, in `commands.rs`), and a bare backend id
//! is not one.
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
/// role's own `chain` names), `metadata_models` (every key the local
/// model-metadata file records), and -- board item
/// `01M24ZJ9ABPP0DGVAA2PS3XVDD` -- `pin` (`AppState::model_pin`'s own
/// `"backend/model"` string, when the session was launched with `--model`)
/// -- sorted and deduped, a `BTreeSet` giving both the union and the
/// dedup for free, the identical trick `AppState::configured_models`'s own
/// construction already uses. See this module's own doc for the fourth
/// source this deliberately does not cover.
pub fn candidate_models(
    chain_models: &[String],
    metadata_models: impl Iterator<Item = String>,
    pin: Option<&str>,
) -> Vec<String> {
    let mut set: BTreeSet<String> = chain_models.iter().cloned().collect();
    set.extend(metadata_models);
    if let Some(pin) = pin {
        set.insert(pin.to_string());
    }
    set.into_iter().collect()
}

/// Board item `01M24ZJ9ABPP0DGVAA2PS3XVDD`: every id in `backend_ids` that
/// no entry in `candidates` names -- i.e. no `"backend/model"` pair in
/// `candidates` has that id as its prefix before the first `/`. This is
/// what lets `commands::execute`'s `Model { model: None }` arm say, per
/// backend, "reachable but nothing routes to it yet" instead of silently
/// omitting it from an otherwise non-empty listing -- see this module's
/// own top doc for why such a backend cannot simply become a fourth row in
/// [`candidate_models`]'s own output (it names no specific model, so it is
/// not a round-trip-safe `ModelRef` string). Preserves `backend_ids`' own
/// order; an empty `candidates` list (nothing configured at all) makes
/// every backend id qualify, which is the correct degenerate case --
/// `commands::execute` only calls this once `candidates` is already known
/// to be non-empty, so that case in practice never reaches here, but the
/// function itself makes no such assumption.
pub fn backends_without_a_named_model(
    backend_ids: &[String],
    candidates: &[String],
) -> Vec<String> {
    backend_ids
        .iter()
        .filter(|id| {
            !candidates
                .iter()
                .any(|c| c.split('/').next() == Some(id.as_str()))
        })
        .cloned()
        .collect()
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

        let got = candidate_models(&chain, metadata.into_iter(), None);

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

        let got = candidate_models(&chain, std::iter::empty(), None);

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
        assert!(candidate_models(&[], std::iter::empty(), None).is_empty());
    }

    /// **VERIFICATION ANCHOR** (board item `01M24ZJ9ABPP0DGVAA2PS3XVDD`,
    /// test (a)): a `--model` pin is unioned in as a third source, even
    /// with no chain and no metadata at all -- the exact shape a launch
    /// with backends declared only through `CONWAY_BACKENDS__*` has (no
    /// role chain can be built from the environment, so `chain_models` is
    /// necessarily empty).
    #[test]
    fn candidate_models_includes_the_pin_even_with_no_other_source() {
        let got = candidate_models(&[], std::iter::empty(), Some("ollama_cloud/glm-5.2"));

        assert_eq!(got, vec!["ollama_cloud/glm-5.2".to_string()]);
    }

    /// The pin is deduped against the chain, not appended a second time.
    #[test]
    fn candidate_models_dedupes_a_pin_already_in_the_chain() {
        let chain = vec!["anthropic/claude-haiku".to_string()];

        let got = candidate_models(&chain, std::iter::empty(), Some("anthropic/claude-haiku"));

        assert_eq!(got, vec!["anthropic/claude-haiku".to_string()]);
    }

    #[test]
    fn backends_without_a_named_model_finds_the_unrouted_backend() {
        let backend_ids = vec!["ollama_cloud".to_string(), "anthropic".to_string()];
        let candidates = vec!["anthropic/claude-haiku".to_string()];

        let got = backends_without_a_named_model(&backend_ids, &candidates);

        assert_eq!(
            got,
            vec!["ollama_cloud".to_string()],
            "anthropic is named by a candidate; ollama_cloud is not"
        );
    }

    #[test]
    fn backends_without_a_named_model_is_empty_when_every_backend_is_named() {
        let backend_ids = vec!["ollama_cloud".to_string()];
        let candidates = vec!["ollama_cloud/glm-5.2".to_string()];

        assert!(backends_without_a_named_model(&backend_ids, &candidates).is_empty());
    }

    #[test]
    fn filter_models_matches_a_case_insensitive_substring() {
        let models = vec![
            "anthropic/claude-haiku".to_string(),
            "anthropic/claude-sonnet-5".to_string(),
            "openai/gpt-5".to_string(),
        ];

        let got = filter_models(&models, "CLAUDE");

        assert_eq!(
            got,
            vec![
                "anthropic/claude-haiku".to_string(),
                "anthropic/claude-sonnet-5".to_string(),
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
