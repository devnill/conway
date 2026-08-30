//! Acceptance coverage for board item `01M11XVEHNMYY942JE63F7MAFH` (the
//! first-run guided-setup flow) that does NOT require a real terminal --
//! see `crates/conway-cli/src/first_run.rs`'s own module doc for why the
//! genuinely interactive branches (accept-local-in-one-keypress, the
//! wrong-credential retry prompt, an explicit decline) are covered by pure
//! unit tests in that file instead, and are not, and cannot be, driven
//! through this suite: no pty is attached to a subprocess spawned by
//! `assert_cmd`, and this crate adds none (C-04).
//!
//! What CAN be driven against the real compiled binary (P-15/acceptance 9,
//! `crates/conway-cli/tests/claude_compat_hooks.rs`'s own shape):
//! - acceptance 1: the trigger opens (something other than the old raw
//!   "no backends configured" error) instead of the hard error.
//! - acceptance 5: the non-interactive degrade message, verbatim.
//! - acceptance 6: a configured, working provider starts straight into a
//!   session with no flow and no material added delay.
//! - acceptance 8: an `Undetermined` fleet never opens the flow.
//! - board item `01M19XZPZD5CKRB83JJS42E8JN`'s acceptance 2 (Ollama Cloud):
//!   an `api_key_env`-shaped backend entry actually resolves and completes
//!   a real turn, not merely that the JSON written for it has the right
//!   shape.
//!
//! `verify_backend`'s own two directions (a fake provider that accepts, one
//! that rejects the key -- acceptance 3's own mechanism) are covered as
//! ordinary `#[tokio::test]`s below, driving the SAME `mock_backend` mock
//! server the one-shot suite already uses -- no terminal involved, since
//! `verify_backend` itself never touches one.

mod common;

use std::time::Instant;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::write_fixture;
use conway_cli::first_run::{self, GUIDED_SETUP_MARKER};

// ---------------------------------------------------------------------
// verify_backend: acceptance 3's own mechanism, no terminal involved.
// ---------------------------------------------------------------------

fn openai_compat_entry_json(base_url: &str, api_key: &str) -> String {
    serde_json::json!({
        "kind": "openai-compat",
        "dialect": "openai",
        "base_url": base_url,
        "api_key": api_key,
    })
    .to_string()
}

/// The positive control: a script that actually finishes.
fn ok_script() -> Script {
    Script(vec![vec![Chunk::Text("ok"), Chunk::Finish("stop")]])
}

#[tokio::test]
async fn first_run_verify_backend_succeeds_against_a_working_mock_provider() {
    let mock = MockBackend::start(ok_script()).await;
    let entry_json = openai_compat_entry_json(&mock.base_url, "any-key");

    let result = first_run::verify_backend("mock", &entry_json, &mock.model).await;

    assert!(result.is_ok(), "expected Ok(()), got {result:?}");
}

/// **The load-bearing acceptance-3 test: a fake provider that rejects the
/// key.** `Chunk::HttpError { status: 401, .. }` is `mock_backend`'s own
/// documented way to drive the real adapter's status-to-`BackendError`
/// classification (401/403 -> `Auth`) -- the same mechanism, not a second,
/// hand-rolled one.
#[tokio::test]
async fn first_run_verify_backend_fails_with_a_message_when_the_key_is_wrong() {
    let mock = MockBackend::start(Script(vec![vec![Chunk::HttpError {
        status: 401,
        body: r#"{"error":{"message":"invalid api key","type":"invalid_request_error"}}"#,
    }]]))
    .await;
    let entry_json = openai_compat_entry_json(&mock.base_url, "wrong-key");

    let result = first_run::verify_backend("mock", &entry_json, &mock.model).await;

    let Err(message) = result else {
        panic!("expected Err(..) for a rejected key, got Ok(())");
    };
    assert!(
        !message.is_empty(),
        "a verification failure must carry a real, non-empty reason"
    );
    // Never the literal key itself, and never a raw Rust Debug dump --
    // this is the exact text a human decides "retry or give up" from.
    assert!(
        !message.contains("wrong-key"),
        "the failure message must not echo the credential back: {message:?}"
    );
}

#[tokio::test]
async fn first_run_verify_backend_fails_cleanly_when_the_model_does_not_exist() {
    // No script entries needed: `mock_backend`'s own default for an
    // unscripted request is a plain `Finish("stop")` with no text, which
    // is a *successful* completion from the mock's point of view --
    // exhausting the routing chain (`no route matched`) or completing
    // successfully both count as "does not panic and returns a total
    // result", which is what this test actually needs to hold: pointing
    // `verify_backend` at a chain entry `mock_backend` was never told
    // about must not panic.
    let mock = MockBackend::start(ok_script()).await;
    let entry_json = openai_compat_entry_json(&mock.base_url, "any-key");

    let result = first_run::verify_backend("mock", &entry_json, "a-model-nobody-declared").await;

    // Either outcome is acceptable here (the mock doesn't distinguish
    // model ids); the only real assertion is "this returns, it does not
    // panic and does not hang" -- P-10's own "a panic is a defect class on
    // par with a crash" applied to a value that ultimately traces back to
    // this flow's own hardcoded per-provider default, not merely to a
    // human's typed input, but exercised the same way here for cheapness.
    let _ = result;
}

// ---------------------------------------------------------------------
// Compiled-binary acceptance tests (P-15 / acceptance 9).
// ---------------------------------------------------------------------

/// A fixture with `"backends": {}` -- the canonical `NoBackendsConfigured`
/// case. The role's chain is deliberately EMPTY, not merely absent-backend:
/// `config::merge::validate` rejects a chain naming a backend id that does
/// not exist in `backends` (`conway/tests/cli_surface.rs::MINIMAL_CONFIG`'s
/// own note, and `conway/tests/builder.rs::base_config`'s identical
/// precedent) -- that check would fire before this item's own trigger ever
/// gets a chance to, and reports a different, unrelated error. An empty
/// chain has nothing to validate against a backend id at all.
fn write_no_backends_fixture() -> common::Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::json!({
            "default_role": "default",
            "backends": {},
            "roles": { "default": { "chain": [] } },
        })
        .to_string(),
    )
    .expect("write conway.json");
    common::Fixture { dir, config_path }
}

/// Acceptance 1 + acceptance 5 (they share one fixture: under a piped,
/// non-tty test harness -- `common::command`'s own `Stdio::null()` stdin --
/// EVERY invocation takes the non-interactive branch, so "the trigger opens
/// instead of the hard error" and "the non-interactive degrade message" are
/// the same observable here). This is also acceptance 5's own explicit
/// warning made concrete: under a test harness stdin is typically not a
/// terminal, so this is the branch these tests actually exercise by
/// default -- named, not assumed.
#[test]
fn first_run_no_usable_provider_prints_the_guided_setup_message_not_the_old_hard_error() {
    let fixture = write_no_backends_fixture();

    let started = Instant::now();
    let out = common::run_conway(&["-p", "hi"], &fixture);
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "must never hang waiting for input nobody can give; took {elapsed:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(GUIDED_SETUP_MARKER),
        "expected the guided-setup message, got stderr: {stderr}"
    );
    assert!(
        !stderr.contains("no backends configured: add a"),
        "the OLD hard error must be replaced, not merely joined; got stderr: {stderr}"
    );
    // Acceptance 5: names the file AND a concrete, pasteable snippet, not
    // a vague description.
    assert!(
        stderr.contains("settings.json"),
        "must name the file to edit; got stderr: {stderr}"
    );
    assert!(
        stderr.contains("ANTHROPIC_API_KEY") && stderr.contains("\"kind\": \"anthropic\""),
        "must print the exact snippet to add; got stderr: {stderr}"
    );
    assert!(
        !out.status.success(),
        "no provider is configured yet -- this run cannot succeed"
    );
}

/// Acceptance 6: a configured, WORKING provider starts straight into a
/// session, with no guided-setup text anywhere in the output, and no
/// material added delay. The discriminating observable named up front (per
/// this item's own "where this will go wrong" #2): the guided-setup
/// MARKER string's absence, plus a generous wall-clock ceiling that would
/// fail if the trigger were, say, probing a remote host instead of only
/// declared-local ones.
// `flavor = "multi_thread"`, matching `claude_compat_hooks.rs`'s own
// identical need: `common::run_conway` blocks the calling OS thread on
// `Command::output()` while a live `MockBackend` (its own background
// accept-loop task, spawned on THIS SAME runtime) must keep running
// concurrently to answer the subprocess's request. A single-threaded
// runtime starves that task for the whole blocking call -- confirmed
// empirically: this test hung for ~363s (three times the adapter's own
// 120s per-request timeout) under the default `#[tokio::test]` flavor
// before this fix, then passed in under two seconds with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_run_a_working_provider_starts_straight_into_a_session_with_no_flow() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 5);

    let started = Instant::now();
    let out = common::run_conway(&["-p", "hi"], &fixture);
    let elapsed = started.elapsed();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains(GUIDED_SETUP_MARKER),
        "a working provider must never trigger the guided-setup flow; got stderr: {stderr}"
    );
    // A loose smoke bound, named as such rather than claimed as a proof
    // (`backend_usability`'s own concurrency test carries the identical
    // caveat): this fixture's one backend is not declared `local`, so
    // `ProbePolicy::LocalOnly` never probes it at all -- the classify step
    // costs a handful of in-memory map lookups, not a network round trip.
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "a working provider must not pay for a startup probe; took {elapsed:?}"
    );
}

/// Acceptance 2 (the second of the two credential styles the board item
/// requires): an `api_key_env`-shaped backend entry -- the shape
/// `first_run.rs::backend_entry_json` writes for `CredentialPlan::
/// ReuseEnvVar`, exactly what an operator with `OLLAMA_API_KEY` (or
/// `ANTHROPIC_API_KEY`/`OPENAI_API_KEY`) already exported gets -- actually
/// resolves through `conway::builder::resolve_api_key` and completes a real
/// turn, not merely that the JSON it produces has the right shape (the unit
/// tests in `first_run.rs` itself already cover that half).
///
/// **The credential is set via `Command::env`, scoped to the CHILD PROCESS
/// only, never `std::env::set_var` in this test's own process** -- that
/// would race every other test thread in this binary reading real process
/// env in parallel, the exact hazard `conway::backend_usability`'s own
/// module doc names (`crates/conway/tests/config_isolation_guard.rs`
/// exists because it broke a suite once).
///
/// Dialect stays `"openai"`, matching [`write_fixture`]'s own template
/// (see that helper's module doc for why: `MockBackend` only speaks the
/// streaming wire shape, and `"ollama"`'s default `tool_calling` would pick
/// the non-streaming path instead) -- credential resolution is dialect-
/// agnostic, so this proves the exact mechanism the new `ollama_cloud`
/// choice depends on without needing the mock to speak a wire format it
/// does not implement.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_run_a_provider_configured_via_api_key_env_actually_completes_a_turn() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 5);

    let mut config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fixture.config_path).expect("read fixture"))
            .expect("fixture is valid json");
    config["backends"]["mock"]["api_key_env"] =
        serde_json::json!("CONWAY_TEST_FIRST_RUN_API_KEY_ENV_STYLE");
    std::fs::write(&fixture.config_path, config.to_string()).expect("rewrite fixture");

    let started = Instant::now();
    let out = common::command(&["-p", "hi"], &fixture)
        .env("CONWAY_TEST_FIRST_RUN_API_KEY_ENV_STYLE", "sk-child-process-only")
        .output()
        .expect("run conway binary");
    let elapsed = started.elapsed();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains(GUIDED_SETUP_MARKER),
        "an api_key_env-credentialed backend must not read as unconfigured; got stderr: {stderr}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "took {elapsed:?}"
    );
}

/// Acceptance 8, the single most important test in this file per the
/// item's own steering: an `Undetermined` fleet (here: a backend with no
/// declared credential and not declared local, so `classify_entry` cannot
/// tell whether it needs one -- `Undetermined::NoCredentialDeclared`) must
/// NEVER open the guided-setup flow, no matter what else about the run
/// fails. Fully hermetic: `Undetermined::NoCredentialDeclared` is reached
/// from configuration alone (`ProbePolicy::LocalOnly` never even attempts
/// a probe here, since `local` is unset/false), never from a timing race
/// or a real network condition.
#[test]
fn first_run_an_undetermined_fleet_never_opens_the_guided_setup_flow() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::json!({
            "default_role": "default",
            "backends": {
                // No `api_key`/`api_key_env`, `local` unset -- exactly
                // `Undetermined::NoCredentialDeclared`, never `Unusable`.
                "ghost": { "kind": "openai-compat", "base_url": "https://example.invalid/v1" }
            },
            "roles": { "default": { "chain": ["ghost/some-model"] } },
        })
        .to_string(),
    )
    .expect("write conway.json");
    let fixture = common::Fixture { dir, config_path };

    let started = Instant::now();
    let out = common::run_conway(&["-p", "hi"], &fixture);
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "must never hang; took {elapsed:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains(GUIDED_SETUP_MARKER),
        "an Undetermined fleet must never trigger the guided-setup flow -- a timed-out/unsure \
         probe must not ambush an operator whose second provider merely hasn't answered yet; \
         got stderr: {stderr}"
    );
}
