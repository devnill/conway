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
