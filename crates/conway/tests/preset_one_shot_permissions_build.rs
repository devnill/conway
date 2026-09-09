//! Board item 01M01EM4QSB204FZSANJB3XH78: `presets::
//! default_permissions_for_one_shot()` returned exactly the allow-list-mode
//! / empty `allowed_tools` combination that `config::merge::validate`'s
//! since-removed check 3 hard-rejected, so the preset could never survive
//! `ConwayBuilder::build()` -- see `crates/conway/examples/
//! bare_inference.rs`'s own `config_with_tools` comment, which hit exactly
//! this wall and worked around it with `GateMode::Deny` instead.
//!
//! P-15 ("a check is not established until it has been shown to fail"): a
//! test that only calls the preset and asserts its shape (as
//! `tests/gates.rs::presets_default_permissions_for_one_shot_is_empty_
//! allowlist` already does) would pass whether or not the preset can ever
//! actually build -- nothing there drives it through `ConwayBuilder::build`.
//! This test instead builds a real `Conway` from the preset, unmodified, and
//! drives one turn through it end to end, fully offline against
//! `conway_testkit` (no API key, no live provider) -- the same shape as
//! `examples/bare_inference.rs`.
//!
//! Board item 01M1YVP3FDPHY4WZ72SXMWAN2D: the preset now returns a
//! `conway::gates::GateConfig`, supplied via `ConwayBuilder::
//! with_gate_config` rather than `config.permissions` (which no longer
//! carries a gate selection at all).

use std::sync::Arc;

use conway::config::schema::{ConwayConfig, ToolsConfig};
use conway::test_support::base_config;
use conway::{ConwayBuilder, SessionSpec};
use conway_core::ids::{BackendId, ModelId, ModelRef};
use conway_core::ports::SessionStore;
use conway_testkit::{FakeBackend, FakeRouter, FakeStore};

/// [`base_config`], except `tools` is emptied so this test drives a turn
/// with no tool wiring in the way. The one-shot preset itself is supplied
/// separately, via `ConwayBuilder::with_gate_config` at the call site
/// below -- it is no longer a `ConwayConfig` field.
fn config_with_one_shot_preset() -> ConwayConfig {
    let mut config = base_config();
    // full literal: `ToolsConfig` has exactly one field, emptied here so
    // this test drives a turn with no tool wiring in the way.
    config.tools = ToolsConfig {
        builtin_plugins: Vec::new(),
    };
    config
}

/// ACCEPTANCE (this item's own verification anchor): a `Conway` built from
/// `presets::default_permissions_for_one_shot()`, unmodified, actually
/// builds -- and once built, a turn can be driven through it. Failed, at
/// the time this test was written, against the since-removed check 3 with
/// a `FacadeError::Config` naming `GateMode::Allowlist` / `allowed_tools`,
/// because `ConwayBuilder::build()` re-validates via `config::merge::
/// apply_cli` (its own step 1) even for a config assembled through
/// `from_parts`.
#[tokio::test]
async fn one_shot_preset_permissions_build_and_drive_a_turn() {
    let backend = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let route = ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    };
    let store = Arc::new(FakeStore::new());

    let conway = ConwayBuilder::from_parts(config_with_one_shot_preset())
        .with_backend(backend)
        .with_session_store(store.clone())
        .with_router(Arc::new(FakeRouter::single(route)))
        .with_gate_config(conway::presets::default_permissions_for_one_shot())
        .build()
        .expect(
            "a Conway built from presets::default_permissions_for_one_shot(), unmodified, must \
             succeed -- if this fails with a GateMode::Allowlist / allowed_tools \
             error, the preset and the validator still disagree",
        );

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session must succeed");
    let turn = session
        .prompt("hello from the one-shot preset")
        .await
        .expect("prompt must be accepted");
    let text = turn.text().await.expect("turn must produce text");
    assert!(
        !text.is_empty(),
        "the driven turn must produce a non-empty response"
    );
    let _ = turn.result().await.expect("turn must complete");

    let head = store.head(&session.id()).await.expect("head read");
    assert!(
        head > conway_core::ids::LogSeq::ZERO,
        "the session log must have advanced past the single driven turn, got {head:?}"
    );
}
