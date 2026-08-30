//! Compiled-binary regression test: `conway routes explain <unknown-role>`
//! must not name the merge floor's baked-in `default` role among "configured
//! roles".
//!
//! `conway::config::merge::default_document` bakes an empty role named
//! `default` into the LOWEST merge layer so an unconfigured `default_role`
//! still validates. Every surface that presents `config().roles` to a person
//! therefore has to filter it, or it names a role the operator never wrote --
//! and one whose empty chain cannot route if it is ever selected.
//!
//! `/settings`' default-role cycle list was fixed for this first; this
//! command was the SECOND such surface and was missed, which is why this
//! test exists at the binary level rather than beside that fix.
//!
//! Deliberately a compiled-binary test rather than a unit test over a
//! hand-built `ConwayConfig`: `routes_explain_injected_router.rs` constructs
//! its `ConwayConfig` as a literal and so never runs the merge pipeline at
//! all, which is exactly why nothing caught this. A test that skips the code
//! path under test cannot fail for the right reason.

mod common;

use common::{command, write_fixture_with};

/// The discriminating observable: the error lists the two roles the fixture
/// declares and NOTHING else. Asserting merely that the command errors, or
/// that the list contains `coder`, would pass just as happily with the
/// phantom `default` still in it.
#[test]
fn unknown_role_error_lists_only_operator_declared_roles() {
    let fixture = write_fixture_with("http://127.0.0.1:1/v1", "mock-model", 5);

    let text = std::fs::read_to_string(&fixture.config_path).expect("read fixture");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("parse fixture");
    value["default_role"] = serde_json::json!("coder");
    value["roles"] = serde_json::json!({
        "coder": { "chain": ["mock/mock-model"] },
        "reviewer": { "chain": ["mock/mock-model"] },
    });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec_pretty(&value).expect("serialize fixture"),
    )
    .expect("rewrite fixture");

    let output = command(&["routes", "explain", "revewier"], &fixture)
        .output()
        .expect("run conway");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("configured roles: coder, reviewer"),
        "expected exactly the two declared roles; got: {stderr}"
    );
    assert!(
        !stderr.contains("default"),
        "the merge floor's baked-in `default` role must never be offered as \
         a configured role; got: {stderr}"
    );
}
