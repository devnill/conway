//! CLI-level acceptance test for declining a shipped backend dialect (board
//! item), against the REAL compiled `conway`
//! binary -- the compiled-binary sibling of `conway/tests/builder.rs`'s
//! `declined_backend_kind_error_is_distinct_from_unknown_backend_kind_error`
//! (the library-API half of the same property).
//!
//! `[plugins].default_backends` (`conway::config::schema::PluginsConfig`,
//! default `["anthropic", "openai-compat"]`) already lets an operator
//! decline a shipped dialect by editing `settings.json` -- that field
//! existed before this item. This
//! item's job was making the decline OBSERVABLE: `crates/conway-cli/src/
//! first_party_plugins.rs`'s `install` now computes every published
//! backend-factory id this binary links that `wanted` (`[plugins].install`
//! unioned with `[plugins].default_backends`) does not name, and hands that
//! list to `ConwayBuilder::with_declined_backend_kinds` before `build()` --
//! so a `[backends.<id>]` entry still naming a declined kind fails `build()`
//! with a message that says so, distinguishable from the pre-existing
//! unknown-kind message a kind this binary has genuinely never heard of
//! still gets.
//!
//! Two properties, both process-level observables (exit code + stderr text,
//! never an internal flag):
//! 1. Declining `"openai-compat"` (`plugins.default_backends = ["anthropic"]`)
//!    while `fixtures/conway.json.tmpl`'s own `backends.mock.kind` still
//!    names it fails the run, and stderr reads as DECLINED, not unknown
//!    (`declining_a_named_dialect_fails_the_run_with_a_declined_kind_message`).
//! 2. The same fixture, `default_backends` left at its default, but with
//!    `backends.mock.kind` rewritten to a kind this binary has genuinely
//!    never linked at all fails the run too, with stderr that reads as
//!    UNKNOWN, not declined
//!    (`a_kind_this_binary_never_linked_fails_with_an_unknown_kind_message`).
//!
//! Both stderr strings are asserted against each other directly, in the
//! third test below, to pin that the two are genuinely different text, not
//! a shared template an operator could not tell apart.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture};

/// Overwrites `[plugins].default_backends` in the rendered fixture config.
fn set_default_backends(fixture: &common::Fixture, ids: &[&str]) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    value["plugins"] = serde_json::json!({ "default_backends": ids });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with [plugins].default_backends");
}

/// Overwrites `backends.mock.kind` in the rendered fixture config --
/// `fixtures/conway.json.tmpl` always renders it as `"openai-compat"`.
fn set_mock_backend_kind(fixture: &common::Fixture, kind: &str) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    value["backends"]["mock"]["kind"] = serde_json::json!(kind);
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with a rewritten backends.mock.kind");
}

/// Property 1: declining `"openai-compat"` while a `[backends.<id>]` entry
/// still names it is a hard failure, and the message reads as a DECLINED
/// kind, not an unrecognised one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declining_a_named_dialect_fails_the_run_with_a_declined_kind_message() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("unreachable"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    // `fixtures/conway.json.tmpl`'s only backend entry names kind
    // "openai-compat" -- declining it here, and nothing else, is what makes
    // that pre-existing entry the thing that now fails to resolve.
    set_default_backends(&fixture, &["anthropic"]);

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        !out.status.success(),
        "a [backends.<id>] entry still naming a declined kind must fail the run, not silently \
         drop that backend and proceed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("openai-compat"),
        "the error must name the declined kind, got stderr: {stderr}"
    );
    assert!(
        stderr.contains("declined"),
        "the error must read as DECLINED, not merely unresolved, got stderr: {stderr}"
    );
    assert!(
        !stderr.contains("unknown kind"),
        "a declined kind is a different diagnosis than an unknown one -- it must not also read \
         as unknown, got stderr: {stderr}"
    );
}

/// Property 2: the sibling failure a kind this binary has genuinely never
/// linked at all still gets -- unchanged by this item, and the point of
/// comparison property 3 needs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kind_this_binary_never_linked_fails_with_an_unknown_kind_message() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("unreachable"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    // `[plugins].default_backends` is left at its own default (both
    // dialects attach); only the entry's own `kind` is rewritten to
    // something this binary never published a factory for.
    set_mock_backend_kind(&fixture, "totally-bogus-kind");

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        !out.status.success(),
        "a kind naming no registered factory at all must fail the run"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("totally-bogus-kind"),
        "the error must name the offending kind, got stderr: {stderr}"
    );
    assert!(
        stderr.contains("unknown kind"),
        "the error must read as UNKNOWN, got stderr: {stderr}"
    );
    assert!(
        !stderr.contains("declined"),
        "an unrecognised kind is a different diagnosis than a declined one -- it must not also \
         read as declined, got stderr: {stderr}"
    );
}

/// Property 3, the binding requirement itself (this item's own board spec:
/// "the message a user reads must distinguish 'conway has never heard of
/// this kind' from 'you turned this kind off'"): the two stderr strings
/// properties 1 and 2 each produce are genuinely different text, driven
/// through the real compiled binary, not the same template printed twice
/// with only the kind name swapped in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declined_and_unknown_kind_messages_are_verbatim_distinct() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("unreachable"),
        Chunk::Finish("stop"),
    ]]))
    .await;

    let declined_fixture = write_fixture(&mock, 10);
    set_default_backends(&declined_fixture, &["anthropic"]);
    let declined_out = run_conway(&["-p", "hi"], &declined_fixture);
    let declined_stderr = String::from_utf8_lossy(&declined_out.stderr).to_string();

    let unknown_fixture = write_fixture(&mock, 10);
    set_mock_backend_kind(&unknown_fixture, "openai-compat-but-misspelled");
    let unknown_out = run_conway(&["-p", "hi"], &unknown_fixture);
    let unknown_stderr = String::from_utf8_lossy(&unknown_out.stderr).to_string();

    assert!(!declined_out.status.success());
    assert!(!unknown_out.status.success());
    assert_ne!(
        declined_stderr, unknown_stderr,
        "declining a shipped dialect and naming a kind this binary never heard of must produce \
         genuinely different stderr text, not the same message twice"
    );
}
