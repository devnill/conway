//! Board item 01KZ803DJW8Y1H4FXTM8D3PYMY: `Conway::warnings()` is a real,
//! populated mechanism (`config::merge::validate` pushes a
//! `WarningCode::HeadroomExceedsContext` when a role's effective headroom is
//! `>=` the smallest context window reachable through its chain) that,
//! before this item, had **zero callers workspace-wide** -- a user with a
//! misconfigured headroom was told nothing.
//!
//! This suite asserts the OBSERVABLE OUTCOME (GP-14's test-design corollary):
//! the rendered stderr text a real misconfigured fixture produces when run
//! through the real compiled `conway` binary, not the return value of
//! `warnings()` itself -- a test that only checked the vec would pass
//! against the broken (unsurfaced) behavior just as easily as the fixed one.
//! `crates/conway/tests/config_headroom.rs` already pins the mechanism
//! (`config::load` returns the right `ConfigWarning`); this file pins that
//! it actually reaches a human.
//!
//! Reuses the WI-113 harness (`tests/common/mod.rs`) unchanged, the same way
//! `subcommands.rs` does.

#[allow(dead_code)]
mod common;

use common::{command, Fixture};

/// `common::write_fixture_with`'s model (`test-model`) is declared with
/// `max_context_tokens: 128_000` in the `.conway/models.json` it writes
/// (see that function's own doc) -- this fixture keeps that model
/// declaration untouched and instead configures role `coder`'s
/// `headroom_tokens` at `200_000`, comfortably `>=` that window, so
/// `config::merge::validate`'s check 7 fires deterministically. `coder`'s
/// chain is pointed at the same `mock/test-model` pair the template already
/// declares a backend for (`"mock"`, `kind: "openai-compat"`), so `build()`
/// never needs a real network dial -- these tests only ever run read-only
/// subcommands, never a prompt.
///
/// `permissions.mode` is set to `"deny"` -- the same override
/// `subcommands.rs::allow_build_without_prompt_handler` applies, for the
/// identical reason (documented there): `sessions`/`routes` never pass a
/// `PermissionGate` override (`main.rs`'s own comment: only `tui` and
/// one-shot `-p` build their own), so the default `"prompt"` mode would
/// otherwise fail `ConwayBuilder::build()` outright with "requires a prompt
/// handler to be supplied" before the CLI ever reaches the warning-printing
/// code under test.
fn write_headroom_warning_fixture() -> Fixture {
    let fixture = common::write_fixture_with("http://127.0.0.1:1/v1", "test-model", 10);
    let text = std::fs::read_to_string(&fixture.config_path).expect("read fixture config");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("parse fixture config");
    value["permissions"] = serde_json::json!({ "mode": "deny" });
    value["roles"]["coder"] = serde_json::json!({
        "chain": ["mock/test-model"],
        "headroom_tokens": 200_000,
    });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize fixture config"),
    )
    .expect("rewrite fixture config");
    fixture
}

/// VERIFICATION ANCHOR: "run `conway` with a headroom exceeding a model's
/// context window and observe the warning on stderr." `sessions list`
/// stands in for "any non-interactive dispatch target" -- the warning is
/// printed once, in `main`, before `dispatch` picks a target
/// (board item's own report requirement: every CLI target shares the one
/// choke point, not a per-command carve-out).
#[test]
fn misconfigured_headroom_is_visible_on_stderr_for_a_cli_subcommand() {
    let fixture = write_headroom_warning_fixture();
    let out = command(&["sessions", "list"], &fixture)
        .output()
        .expect("run conway binary");

    assert!(
        out.status.success(),
        "sessions list should still succeed (a headroom warning is non-fatal); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway: warning:"),
        "expected the standard diag::warn prefix on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("headroom for role 'coder'"),
        "expected the warning to name the misconfigured role, got: {stderr:?}"
    );
    assert!(
        stderr.contains("200000"),
        "expected the warning to name the configured headroom, got: {stderr:?}"
    );
    assert!(
        stderr.contains("mock/test-model"),
        "expected the warning to name the offending chain entry, got: {stderr:?}"
    );
    assert!(
        stderr.contains("128000"),
        "expected the warning to name the model's max context window, got: {stderr:?}"
    );
}

/// One-shot `-p` mode is a SEPARATE dispatch target from `sessions`/`routes`
/// (its own gate-building branch in `main.rs`, see that file's own
/// comment). This proves the warning print is not accidentally scoped to
/// the `Some(Command)` arm alone -- it needs a live scripted mock backend
/// (unlike the read-only subcommand above) since one-shot mode actually
/// dials the chain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn misconfigured_headroom_is_visible_on_stderr_for_one_shot_print() {
    use common::mock_backend::{Chunk, MockBackend, Script};

    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = common::write_fixture(&mock, 10);
    let text = std::fs::read_to_string(&fixture.config_path).expect("read fixture config");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("parse fixture config");
    // One-shot `-p` builds its own gate (`oneshot::build_gate`) regardless
    // of `permissions.mode` (main.rs's own comment: `-p` supplies its own
    // gate override) -- `"deny"` here just needs to pass `merge::validate`'s
    // "allowlist requires non-empty allowed_tools" check trivially, exactly
    // like `write_headroom_warning_fixture` above.
    value["permissions"] = serde_json::json!({ "mode": "deny" });
    value["roles"]["default"] = serde_json::json!({
        "chain": [format!("mock/{}", mock.model)],
        "headroom_tokens": 200_000,
    });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize fixture config"),
    )
    .expect("rewrite fixture config");

    let out = command(&["-p", "hi"], &fixture)
        .output()
        .expect("run conway binary");

    // Unlike the read-only `sessions list` case above, one-shot `-p`
    // actually dials the chain -- a headroom this badly misconfigured
    // means the turn itself is later refused by the SAME context-window
    // gate the warning named in advance (`routing error: context
    // rejected: ...`), so this run is expected to exit non-zero. The
    // warning must still be on stderr regardless: build-time diagnostics
    // are not conditioned on whether the run later succeeds.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway: warning:") && stderr.contains("200000"),
        "expected the headroom warning on stderr for one-shot -p too, got: {stderr:?}"
    );
    assert!(
        stderr.contains("headroom for role 'default'"),
        "expected the warning to name the misconfigured role, got: {stderr:?}"
    );
}

/// BREAK-THE-GUARD: with the misconfiguration removed (headroom well under
/// the model's window), the same fixture must NOT print a warning --
/// otherwise the assertions above could be trivially satisfied by printing
/// unconditional noise on every run rather than an actual, computed
/// `ConfigWarning`.
#[test]
fn a_healthy_headroom_prints_no_warning() {
    let fixture = common::write_fixture_with("http://127.0.0.1:1/v1", "test-model", 10);
    let text = std::fs::read_to_string(&fixture.config_path).expect("read fixture config");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("parse fixture config");
    value["permissions"] = serde_json::json!({ "mode": "deny" });
    // Well under the fixture's 128_000-token model window -- no warning.
    value["roles"]["coder"] = serde_json::json!({
        "chain": ["mock/test-model"],
        "headroom_tokens": 4_096,
    });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize fixture config"),
    )
    .expect("rewrite fixture config");

    let out = command(&["sessions", "list"], &fixture)
        .output()
        .expect("run conway binary");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("conway: warning:"),
        "a healthy headroom must print no warning at all, got: {stderr:?}"
    );
}
