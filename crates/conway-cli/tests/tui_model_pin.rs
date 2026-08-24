//! `--model`/`--session`/`--resume`/
//! `--fork-from` were all accepted by the CLI parser and never read by the
//! interactive TUI's own session construction (`tui::app::App::new` built
//! its `SessionSpec` from `role`/`keep_alive`/`tools` only) -- a
//! renderer-only gap: the identical `--model` flag is genuinely wired
//! in one-shot mode (`oneshot::resolve_session`, covered by
//! `tests/oneshot.rs`'s `model_flag_pins_and_overrides_role_chain`).
//!
//! This suite drives the TUI's OWN construction path directly --
//! [`conway_cli::tui::app::App::session_spec`], the exact associated
//! function `App::new` calls to build the `SessionSpec` it passes to
//! `Conway::new_session` -- rather than `oneshot::resolve_session` (which
//! is private to `oneshot.rs`, and is a different code path besides).
//! `model_flag_pins_the_session_spec` below was confirmed to fail
//! before this item's fix: `App::new`'s inline `SessionSpec { .. }`
//! construction never set `model` at all, so `spec.model` was always
//! `None` regardless of `--model`.
//!
//! `App::new` itself is not driven here (only `App::session_spec`): it also
//! needs a live `Conway` and, for the positive/negative `--model` cases,
//! that adds real routing/config setup with nothing left to prove --
//! `App::session_spec` is the exact code in question, and is unit-testable
//! without any of that. The `--session`/`--resume`/`--fork-from` rejection
//! tests do go through the full `App::new` (see
//! `session_flags_are_rejected_by_app_new_not_silently_ignored` below), to
//! prove the rejection actually reaches a caller starting the TUI for real,
//! not just the extracted helper.

mod common;

use std::str::FromStr;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{Conway, ConwayBuilder, ModelRef, PermissionGate};
use conway_cli::cli::{Cli, OutputFormat, PermissionMode};
use conway_cli::exit::ExitCode;
use conway_cli::tui::app::App;
use conway_core::agent::PermissionDecision;
use conway_core::ids::{BackendId, ModelId};
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use std::collections::BTreeMap;
use std::sync::Arc;

use common::mock_backend::{MockBackend, Script};
use common::{run_conway, write_fixture};

fn minimal_cli() -> Cli {
    Cli {
        print: None,
        output_format: OutputFormat::Text,
        allowed_tools: Vec::new(),
        deny_tools: Vec::new(),
        permission_mode: PermissionMode::Allowlist,
        role_override: None,
        model: None,
        agent: None,
        system_prompt: None,
        append_system_prompt: None,
        max_turns: None,
        max_tokens: None,
        max_seconds: None,
        output_schema: None,
        session: None,
        resume: None,
        fork_from: None,
        config: None,
        cwd: None,
        root: None,
        verbose: 0,
        command: None,
    }
}

/// `--model backend/model` must reach `SessionSpec.model` -- the exact
/// field `crates/conway/src/session_handle.rs`'s `SessionSpec` has carried
/// since then, which `App::new`'s own stale doc comment used to (wrongly)
/// claim did not exist.
#[test]
fn model_flag_pins_the_session_spec() {
    let mut cli = minimal_cli();
    cli.model = Some("anthropic/claude-x".to_string());

    let spec = App::session_spec(&cli).expect("a well-formed --model must build a spec");

    assert_eq!(
        spec.model,
        Some(ModelRef::from_str("anthropic/claude-x").expect("valid model ref")),
        "the TUI's own SessionSpec must carry the --model pin, not silently drop it"
    );
}

/// The flag-free default: `SessionSpec.model` stays `None`, exactly as
/// before this item -- `--model`'s absence must not be confused with a pin.
#[test]
fn no_model_flag_leaves_the_pin_unset() {
    let spec = App::session_spec(&minimal_cli()).expect("no --model must still build a spec");
    assert_eq!(spec.model, None);
}

/// A malformed `--model` must fail the SAME way in both modes -- this item's
/// own binding requirement, verified by comparing the TUI's in-process
/// error (`App::session_spec`) against one-shot's real, compiled-binary
/// error (mirroring `tests/oneshot.rs`'s `exit_2_bad_model_ref`). Both call
/// through `crate::model_pin::parse_model_pin`, the single parser this item
/// introduced specifically so the two could never drift apart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_model_fails_identically_in_both_modes() {
    let mut cli = minimal_cli();
    cli.model = Some("not-a-valid-ref".to_string());
    let tui_err = App::session_spec(&cli)
        .expect_err("a malformed --model must be a usage error, not build a spec")
        .to_string();
    assert_eq!(
        ExitCode::from_error(&conway::FacadeError::Config {
            path: None,
            message: tui_err.clone(),
        }),
        ExitCode::Usage,
        "a malformed --model must classify as a usage error (exit 2) in the TUI too"
    );

    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    let out = run_conway(&["-p", "hi", "--model", "not-a-valid-ref"], &fixture);
    assert_eq!(
        out.status.code(),
        Some(2),
        "one-shot's own exit code for a malformed --model"
    );
    let oneshot_err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        mock.requests().is_empty(),
        "a malformed --model must fail before ever dialing a backend, in either mode"
    );

    let needle = "--model not-a-valid-ref:";
    assert!(
        tui_err.contains(needle),
        "TUI error must name the malformed flag/value the same way one-shot's does, got: \
         {tui_err:?}"
    );
    assert!(
        oneshot_err.contains(needle),
        "one-shot's stderr must name the malformed flag/value, got: {oneshot_err:?}"
    );
}

/// `--session`/`--resume`/`--fork-from` are a decided non-goal for the TUI
/// (see `App::session_spec`'s own doc comment for the reasoning): rather
/// than silently accept and ignore them (this item's whole point), the TUI
/// refuses to start with a usage error naming the alternative. Driven
/// through the extracted helper for all three flags...
fn assert_flag_is_rejected(label: &str, cli: &Cli) {
    let err = match App::session_spec(cli) {
        Ok(_) => panic!("{label} must be refused by the TUI, not silently ignored"),
        Err(e) => e,
    };
    assert_eq!(
        ExitCode::from_error(&err),
        ExitCode::Usage,
        "{label}: rejection must classify as a usage error (exit 2)"
    );
    let text = err.to_string();
    assert!(
        text.contains("--session") && text.contains("--resume") && text.contains("--fork-from"),
        "{label}: the refusal must name all three continuity flags, got: {text:?}"
    );
    assert!(
        text.contains("/resume"),
        "{label}: the refusal must point at the in-TUI alternative, got: {text:?}"
    );
}

#[test]
fn session_continuity_flags_are_rejected_not_silently_ignored() {
    let mut cli = minimal_cli();
    cli.session = Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string());
    assert_flag_is_rejected("--session", &cli);

    let mut cli = minimal_cli();
    cli.resume = Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string());
    assert_flag_is_rejected("--resume", &cli);

    let mut cli = minimal_cli();
    cli.fork_from = Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string());
    assert_flag_is_rejected("--fork-from", &cli);
}

/// ...and again through the full `App::new` -- not just the extracted
/// helper -- so the rejection is proven to reach the real path a caller
/// starting the TUI actually takes.
#[tokio::test]
async fn session_flags_are_rejected_by_app_new_not_silently_ignored() {
    let conway = build_conway_with_echo_backend();
    let mut cli = minimal_cli();
    cli.resume = Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string());

    let err = match App::new(&cli, &conway, &[]).await {
        Ok(_) => panic!("App::new must refuse --resume, not start an interactive session anyway"),
        Err(e) => e,
    };
    assert_eq!(ExitCode::from_error(&err), ExitCode::Usage);
}

fn base_config() -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    ConwayConfig {
        default_role: conway::RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// Mirrors `tui::app`'s own in-crate test helper of the same name (private
/// to that module, so duplicated here rather than shared -- same pattern
/// `routes_explain_injected_router.rs`'s own `config_with_role` doc already
/// explains).
fn build_conway_with_echo_backend() -> Conway {
    let backend: Arc<dyn conway::Backend> = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let router: Arc<dyn conway::Router> = Arc::new(FakeRouter::single(conway::ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(gate)
        .with_router(router)
        .build()
        .expect("build should succeed with every port injected")
}
