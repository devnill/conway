//! Compiled-binary regression test: `conway routes explain <unknown-role>`
//! must not name the merge floor's baked-in `default` role among "configured
//! roles".
//!
//! `conway::config::merge::default_document` bakes an empty role named
//! `default` into the LOWEST merge layer so an unconfigured `default_role`
//! still validates. Every surface presenting `config().roles` to a person has
//! to filter it, or it names a role the operator never wrote -- and one whose
//! empty chain cannot route if it is ever selected. `/settings`' default-role
//! cycle list was fixed for this first; this command was the SECOND such
//! surface and was missed.
//!
//! Deliberately NOT reusing `tests/common/mod.rs`, for the same kind of
//! reason `config_isolation_binary.rs` states for itself: that harness's
//! `Fixture` comes bundled with `mock_backend`, and this test needs no mock
//! at all -- `routes explain` rejects an unknown role before any request is
//! made. Pulling the whole mock harness in to borrow a fixture writer makes
//! every one of its helpers look dead in THIS binary (each `tests/*.rs` is
//! its own crate), which would mean scattering `#[allow(dead_code)]` across
//! shared harness surface to satisfy one file.
//!
//! And deliberately a compiled-binary test rather than a unit test over a
//! hand-built `ConwayConfig`: `routes_explain_injected_router.rs` constructs
//! its config as a literal and so never runs the merge pipeline, which is
//! exactly why nothing caught this. A test that skips the code path under
//! test cannot fail for the right reason.

use std::process::Command;

/// A config declaring exactly two roles, neither named `default`. The merge
/// floor will still contribute its own `default` beneath this.
const CONFIG: &str = r#"{
  "default_role": "coder",
  "limits": { "max_steps": 5 },
  "backends": {
    "mock": { "kind": "openai-compat", "base_url": "http://127.0.0.1:1/v1", "dialect": "openai" }
  },
  "roles": {
    "coder": { "chain": ["mock/mock-model"] },
    "reviewer": { "chain": ["mock/mock-model"] }
  }
}"#;

/// The discriminating observable: the error names the two declared roles and
/// nothing else. Asserting merely that the command errors, or that the list
/// contains `coder`, would pass just as happily with the phantom still in it.
///
/// Measured against a deliberately broken guard: with the filter removed from
/// `commands::routes`, this fails with `configured roles: coder, default,
/// reviewer`.
#[test]
fn unknown_role_error_lists_only_operator_declared_roles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("conway.json");
    std::fs::write(&config_path, CONFIG).expect("write config");

    // Isolate the USER config layer, or this reads the developer's real
    // `~/.conway/settings.json`: the first run of this test reported
    // `coder, fast, local, planner, reviewer` -- three of those being roles
    // from this machine, which would make the assertion mean something
    // different in CI, on another machine, or after the operator edits their
    // own config. `--config` sets only the project layer; the user layer is
    // separate and is what `CONWAY_CONFIG_DIR` relocates.
    let isolated = tempfile::tempdir().expect("isolated config dir");
    let output = Command::new(assert_cmd::cargo::cargo_bin("conway"))
        .current_dir(dir.path())
        .env("CONWAY_CONFIG_DIR", isolated.path())
        .env("HOME", isolated.path())
        .env("USERPROFILE", isolated.path())
        .arg("--config")
        .arg(&config_path)
        .args(["routes", "explain", "revewier"])
        .output()
        .expect("run conway");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("configured roles: coder, reviewer"),
        "expected exactly the two declared roles; got: {stderr}"
    );
    assert!(
        !stderr.contains("default"),
        "the merge floor's baked-in `default` role must never be offered as a \
         configured role; got: {stderr}"
    );
}
