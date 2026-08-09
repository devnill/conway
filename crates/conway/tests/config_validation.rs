//! WI-097 rework (finding M1): coverage for `merge::validate` steps 4-6,
//! which were implemented correctly but had zero test coverage. This is a
//! hard-gate file three other work items build on, so each error path is
//! locked in explicitly.

#[path = "support/mod.rs"]
mod support;

use std::collections::HashMap;

use conway::config::{load, CliOverrides, LoadOptions};

#[test]
fn allowlist_mode_with_empty_allowed_tools_is_rejected() {
    let dir = support::unique_temp_dir("allowlist-empty");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("allowlist_empty.json")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("allowlist") && err.contains("allowed_tools"),
        "error must name the allowlist/allowed_tools requirement: {err}"
    );
}

#[test]
fn interval_fsync_with_zero_interval_ms_is_rejected() {
    let dir = support::unique_temp_dir("fsync-interval-zero");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("fsync_interval_zero.json")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("fsync") && err.contains("fsync_interval_ms"),
        "error must name the fsync/fsync_interval_ms requirement: {err}"
    );
}

#[test]
fn api_key_and_api_key_env_both_set_is_rejected() {
    let dir = support::unique_temp_dir("api-key-both-set");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("api_key_both_set.json")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("api_key")
            && err.contains("api_key_env")
            && err.contains("mutually exclusive"),
        "error must name the api_key/api_key_env mutual exclusivity: {err}"
    );
}

/// `merge::validate`'s check 2 (every routing chain entry names a backend
/// that exists in `[backends]`) still holds after board item
/// 01KZHF1E85MS1VF4YH8CDNCP9Z opened `kind` to an arbitrary name: this
/// check never inspected `kind` at all, so it is unaffected in principle --
/// pinned here because, until this test, nothing exercised it directly.
#[test]
fn chain_entry_naming_unknown_backend_is_rejected() {
    let dir = support::unique_temp_dir("chain-unknown-backend");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("chain_names_unknown_backend.json")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("ghost-backend") && err.contains("unknown backend"),
        "error must name the chain entry's unknown backend: {err}"
    );
}

/// Board item 01KZHF1E85MS1VF4YH8CDNCP9Z's harder half: `BackendEntry` drops
/// `#[serde(deny_unknown_fields)]` in favor of a flattened `extra` catch-all
/// (chosen over the two alternatives named in that item's spec -- see
/// `BackendEntry`'s own doc comment for the full reasoning and the
/// typo-detection cost this accepts). This pins the exact, chosen
/// consequence: a misspelled well-known key (`base_ur1`, not `base_url`) is
/// NOT rejected -- it loads successfully and is captured verbatim into
/// `extra`, while the typed `base_url` field it was meant to set stays at
/// its default (empty). Asserted, not assumed, per that item's own
/// acceptance criterion.
#[test]
fn misspelled_well_known_backend_key_is_accepted_and_passed_through() {
    let dir = support::unique_temp_dir("backend-typo-key");
    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("backend_typo_key.json")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("a misspelled backend key must not fail to load -- it is silently captured, not rejected");

    let anthropic = outcome
        .config
        .backends
        .get("anthropic")
        .expect("anthropic backend present");
    assert_eq!(
        anthropic.base_url, "",
        "the typo'd key must NOT have populated the typed base_url field"
    );
    assert_eq!(
        anthropic.extra.get("base_ur1"),
        Some(&serde_json::Value::String(
            "https://typo.invalid/not-picked-up".to_string()
        )),
        "the typo'd key must be captured verbatim in the catch-all `extra` map"
    );
}

/// The Kimi coding-plan config block published in
/// `docs/providers.md` must actually load. A copy-pasteable
/// example that does not parse is worse than no example, and this is the
/// documented "easy setup" path, so it is pinned against schema drift.
///
/// Note this exercises the post-`d27b5c0` behavior: a third-party
/// Anthropic-compatible endpoint configures without conway judging the
/// credential's shape.
#[test]
fn documented_kimi_coding_plan_config_loads() {
    let dir = support::unique_temp_dir("kimi-coding-plan");
    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("kimi_coding_plan.json")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("the documented Kimi config must load");

    let kimi = outcome
        .config
        .backends
        .get("kimi")
        .expect("kimi backend present");
    assert_eq!(kimi.base_url, "https://api.kimi.com/coding/");
    assert_eq!(kimi.api_key_env, "KIMI_API_KEY");
    assert!(
        kimi.api_key.is_empty(),
        "the key itself must never be in the config file -- only the env var name"
    );
}
