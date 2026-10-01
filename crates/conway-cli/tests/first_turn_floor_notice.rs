//! Board item `01M2N2GJ9K7QEABGZD7R9GVT3Y`: a model that reaches a resolved
//! config WITHOUT ever passing through guided setup's own confirm surface
//! (`first_run::handle_context_window_at_setup`, reached only from
//! `run_backend_setup`/`retry_credential_and_finish`) must still surface an
//! assumed, unsourced context-window floor to the operator before it costs
//! them a real turn.
//!
//! This is P-15's own load-bearing case: a hand-written `settings.json`
//! naming `ollama_cloud/glm-5.3` -- the literal 2026-09-09 reproduction --
//! never through [`crate::first_run::HOSTED_CHOICES`]'s guided-setup
//! shortlist at all (that shortlist only ever offers `glm-5.2`, see this
//! item's own board-item context). Before this item, nothing in
//! `conway-cli` ever inspected a resolved model's `ContextTokensSource`
//! outside `handle_context_window_at_setup` -- a fixture built the way
//! [`write_glm53_fixture`] builds one never reaches that function at all
//! (no guided setup ever runs), so this suite's own anchor test is expected
//! to fail against HEAD.
//!
//! Reuses the harness (`tests/common/mod.rs`), but builds its own
//! `conway.json` directly (`serde_json::json!`), the same way
//! `config_warnings.rs::write_fixture_with_deadline` does -- unlike
//! `common::write_fixture_with`, which always writes a `.conway/
//! models.json` entry for its own `mock`/`test-model` pair; that would
//! defeat the entire point here, since this suite's fixtures must have NO
//! metadata entry for the model under test (P-15's own premise: "the
//! operator learns the window is a guess", which presupposes nothing has
//! recorded one yet).
//!
//! `base_url` points at `http://127.0.0.1:1/v1` (nothing listening --
//! `config_warnings.rs`'s own pattern for a fast, deterministic connection
//! failure) rather than a live mock server: the notice this suite asserts
//! on is printed in `main.rs`, BEFORE `dispatch` ever reaches
//! `oneshot::run` -- so it must appear on stderr regardless of whether the
//! turn that follows succeeds, times out, or (as here) fails fast on a
//! refused connection, exactly the same "build-time diagnostics are not
//! conditioned on whether the run later succeeds" rule
//! `config_warnings.rs`'s own headroom test already established.
//!
//! No `[plugins]` section in any fixture here, deliberately:
//! `conway-plugin-routing`'s `ROUTER_ID` (`"conway.routing"`) is not one of
//! the seven ids in guided setup's own `first_party_plugins::
//! DEFAULT_OPINION_SET`, so an ordinary hand-written `settings.json` (this
//! suite's whole premise) runs on `conway_core::routing::MinimalRouter` --
//! no capability-index gate, no `models.json` entry required for the chain
//! to route at all. See `first_run::resolve_first_turn_floor_notice`'s own
//! doc for why that is exactly the scenario this item's own investigation
//! found reachable in production.
//!
//! **2026-09-30 correction, board item `01M3T76R6RGWK83ERATQATBM1H`:** this
//! suite's own original anchor test used `ollama_cloud/glm-5.3` as its
//! "fires" reproduction. That premise was itself wrong: `glm-5.3` (like
//! `glm-5.2`) is one of the two ids `conway_plugin_backends::
//! model_metadata`'s bundled `DEFAULTS` table declares a real
//! `1_048_576`-token window for, so a real turn against it never actually
//! runs on the floor -- dogfooding caught the notice claiming otherwise
//! anyway. [`write_fixture_naming`] now takes the model id as a parameter;
//! the "fires" anchor below uses a model this crate truly has no bundled
//! opinion about, and [`glm_5_2_and_5_3_print_no_notice_with_no_models_json_
//! entry_at_all`] is this item's own required regression coverage (the
//! real config shape: `ollama_cloud` openai-compat/`ollama`-dialect
//! backend, `default_role` headed by the pair in question, `MinimalRouter`,
//! no `models.json` entry for that key at all -- see this item's own
//! `first_run.rs` doc for why "a models.json entry... WITHOUT a context
//! window" is represented as "no entry at all": `ModelMetadataEntry::
//! max_context_tokens` is a required `u32`, so an entry that names
//! `tool_calling`/`reasoning` but omits a window cannot be constructed in
//! the first place; "no entry" is the realistic shape that actually
//! reproduces this defect, and is the shape `HOSTED_CHOICES`' own
//! guided-setup output always leaves behind).

#[allow(dead_code)]
mod common;

use common::{command, Fixture};

/// Builds a fixture naming `ollama_cloud/{model}` directly in `backends`/
/// `roles` -- never through `first_run::run_guided_setup`/`HOSTED_CHOICES`
/// at all, P-15's own required shape. `api_key` is a literal (never
/// `api_key_env`) so `FleetUsability::should_offer_guided_setup` sees a
/// configured, credentialed backend and does not intercept this run with
/// its own guided-setup message before `build()` ever runs -- this suite is
/// about what happens ONCE a model is configured this way, not about the
/// guided-setup trigger itself. Deliberately NO `.conway/models.json` at
/// all -- this is P-15's whole point: nothing has ever confirmed a window
/// for this pair.
fn write_fixture_naming(model: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = serde_json::json!({
        "default_role": "default",
        "backends": {
            "ollama_cloud": {
                "kind": "openai-compat",
                "dialect": "ollama",
                "base_url": "http://127.0.0.1:1/v1",
                "api_key": "test-key-not-real"
            }
        },
        "roles": {
            "default": { "chain": [format!("ollama_cloud/{model}")] }
        }
    });
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config).expect("serialize conway.json"),
    )
    .expect("write conway.json");
    Fixture { dir, config_path }
}

/// VERIFICATION ANCHOR (P-15): a hand-written `settings.json` naming a
/// model this crate has no bundled opinion about -- NOT through guided
/// setup's own shortlist -- must surface the assumed-floor guess on stderr
/// before the turn that follows can cost the operator a run.
#[test]
fn a_hand_written_config_naming_an_unconfirmed_model_warns_before_the_first_turn() {
    let fixture = write_fixture_naming("mystery-model-9000b");
    let out = command(&["-p", "hi"], &fixture)
        .output()
        .expect("run conway binary");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway: warning:"),
        "expected the standard diag::warn prefix on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("ollama_cloud/mystery-model-9000b"),
        "must name the exact pair about to run unconfirmed: {stderr:?}"
    );
    assert!(
        stderr.contains("floor (assumed)"),
        "must use the SAME provenance vocabulary `conway routes explain` already uses, not a \
         fresh label: {stderr:?}"
    );
    assert!(
        stderr.contains("models.json"),
        "must name the remedy: {stderr:?}"
    );
}

/// **THE REGRESSION this item exists to close** (board item
/// `01M3T76R6RGWK83ERATQATBM1H`): the real config shape described in this
/// item's own board spec -- `ollama_cloud` openai-compat/`ollama`-dialect
/// backend, `default_role` headed by `ollama_cloud/glm-5.2` (or `glm-5.3`),
/// `MinimalRouter`, NO `.conway/models.json` entry for that key at all (the
/// exact output `HOSTED_CHOICES`' own guided setup leaves behind). Before
/// this item's fix, this printed the identical "no confirmed context
/// window ... 32768-token limit" notice the anchor test above asserts FOR
/// an actually-unknown model -- a false positive, since
/// `conway_plugin_backends::model_metadata`'s bundled `DEFAULTS` table
/// already declares `1_048_576` for both ids (proved against the real
/// admission path, not just this notice, by `crates/conway-plugin-backends/
/// tests/glm_context_window.rs`).
#[test]
fn glm_5_2_and_5_3_print_no_notice_with_no_models_json_entry_at_all() {
    for model in ["glm-5.2", "glm-5.3"] {
        let fixture = write_fixture_naming(model);
        let out = command(&["-p", "hi"], &fixture)
            .output()
            .expect("run conway binary");

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("floor (assumed)"),
            "a model the bundled defaults table already declares a window for must not be \
             reported as running on an unconfirmed floor: model={model} stderr={stderr:?}"
        );
        assert!(
            !stderr.contains("no confirmed context window"),
            "must not claim no window is confirmed when the bundled table already supplies one: \
             model={model} stderr={stderr:?}"
        );
    }
}

/// BREAK-THE-GUARD for the regression test immediately above: the identical
/// fixture shape, but naming the SAME genuinely-undescribed model the anchor
/// test above uses, proving the suite's harness itself still reports a
/// notice when the bundled table has nothing to say -- the silence above is
/// not an artifact of this harness going quiet for every model.
#[test]
fn an_undescribed_model_on_the_identical_fixture_shape_still_warns() {
    let fixture = write_fixture_naming("mystery-model-9000b");
    let out = command(&["-p", "hi"], &fixture)
        .output()
        .expect("run conway binary");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("floor (assumed)"),
        "the harness itself must still warn for a model with no bundled declaration: {stderr:?}"
    );
}

/// BREAK-THE-GUARD: the identical fixture, but with a `.conway/
/// models.json` entry already recording a window for this exact pair --
/// "no re-asking about a window already confirmed" (the parent item's own
/// ruling, carried over verbatim by this item's own spec). Proves the
/// assertions above are not trivially satisfied by unconditional noise on
/// every `-p` run.
#[test]
fn an_already_recorded_window_prints_no_notice() {
    let fixture = write_fixture_naming("mystery-model-9000b");
    let models_dir = fixture.dir.path().join(".conway");
    std::fs::create_dir_all(&models_dir).expect("create .conway dir");
    let models_json = serde_json::json!({
        "models": {
            "ollama_cloud/mystery-model-9000b": {
                "max_context_tokens": 1_048_576,
                "tool_calling": "streaming",
                "reasoning": false,
                "reliability_tier": "community"
            }
        }
    });
    std::fs::write(
        models_dir.join("models.json"),
        serde_json::to_vec(&models_json).expect("serialize models.json"),
    )
    .expect("write models.json");

    let out = command(&["-p", "hi"], &fixture)
        .output()
        .expect("run conway binary");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("floor (assumed)"),
        "an already-recorded window must not be re-litigated on every run: {stderr:?}"
    );
}

/// BREAK-THE-GUARD: `sessions list` never attempts a turn
/// (`main.rs::command_needs_provider`), so it must print no notice at all
/// even against the identical unconfirmed fixture -- checking what a turn
/// would run against for a read-only inspection would be noise, not a
/// warning (the same test `conway.warnings()` itself is already held to,
/// `config_warnings.rs`'s own suite).
#[test]
fn a_read_only_subcommand_prints_no_notice() {
    let fixture = write_fixture_naming("mystery-model-9000b");
    let out = command(&["sessions", "list"], &fixture)
        .output()
        .expect("run conway binary");

    assert!(
        out.status.success(),
        "sessions list must still succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("floor (assumed)"),
        "a read-only subcommand never attempts a turn and must print no notice: {stderr:?}"
    );
}
