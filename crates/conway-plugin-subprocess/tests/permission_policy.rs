//! The `permission.policy/1` wire point (board item
//! `01M03VKJG7JJ0JEKY265WA7MJ7`): proof that a persistent-transport plugin
//! that declares `permission.policy/1` at a SUPPORTED version has its
//! per-tool NARROWING policy exchanged once at session open (AFTER
//! `initialize/1`, BEFORE any `tool/1` call), stored on the session, and
//! surfaced via `SubprocessPlugin::permission_rules` as the
//! `PluginPermissionRule`s the `conway` facade installs as
//! `PatternOrigin::Plugin` deny/prompt rules; that a plugin declaring the
//! point at an UNSUPPORTED version is REFUSED at discover with a typed
//! `HandshakeRefused` naming the version mismatch (the participant rule);
//! that a malformed policy answer fails CLOSED (`HandshakeMalformed`, never
//! silently no-op); that a plugin that does NOT declare the point loads
//! normally and contributes no wire policy; and that the host now
//! ADVERTISES `permission.policy/1` in its `initialize/1` `points` array.
//!
//! Mirrors `tests/handshake.rs`'s own mock-plugin-process pattern (fixtures
//! in `tests/common/mod.rs`): every fixture here is a plain Python 3 script
//! this suite writes into a fresh temp dir at run time, authored outside
//! this workspace's dependency graph. The SUBORDINATION composition (a
//! plugin `prompt` forces the gate; a plugin `abstain` cannot widen past an
//! operator `deny`) lives in `crates/conway-runtime/src/permission.rs`'s
//! inline `plugin_permission_subordination_*` tests, where the real
//! `PermissionBroker::decide` enforces it -- this file proves the WIRE half
//! (the policy genuinely reaches the host over the persistent transport and
//! is stored in the shape the broker consumes).

mod common;

use conway::plugin::PluginPermissionVerdict;
use conway_plugin_subprocess::{SubprocessPlugin, SubprocessPluginError, SubprocessTransport};

// ---------------------------------------------------------------------
// Acceptance criterion 1 (wire half) -- a matching policy exchange stores
// the declared NARROWING rules and surfaces them via the Plugin trait.
// ---------------------------------------------------------------------

/// **Criterion 1 (wire half).** A persistent plugin that declares
/// `permission.policy/1` at version 1 and answers with a `greet`->`prompt`,
/// `bash`->`deny`, `read`->`abstain` policy must OPEN, store the rules, and
/// surface them via `SubprocessPlugin::permission_rules` (the `Plugin`
/// trait method) as the `PluginPermissionRule`s the `conway` facade installs
/// as `PatternOrigin::Plugin` deny/prompt rules. The `conway-runtime`
/// inline tests prove those installed rules actually narrow at the gate.
#[tokio::test]
async fn a_matching_policy_exchange_stores_and_surfaces_the_declared_rules() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "policy_ok.py",
        common::PERSISTENT_POLICY_OK_PLUGIN,
    )
    .await;
    assert_eq!(spec.transport, SubprocessTransport::Persistent);

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("a supported-version policy plugin loads");

    let rules = plugin.permission_rules();
    assert_eq!(rules.len(), 3, "all three declared rules are surfaced");

    let greet = rules
        .iter()
        .find(|r| r.tool == "greet")
        .expect("greet rule");
    assert_eq!(
        greet.verdict,
        PluginPermissionVerdict::Prompt,
        "greet -> prompt (force the operator's gate)"
    );
    assert_eq!(greet.reason, "greet should be approved");

    let bash = rules.iter().find(|r| r.tool == "bash").expect("bash rule");
    assert_eq!(
        bash.verdict,
        PluginPermissionVerdict::Deny,
        "bash -> deny (refuse outright)"
    );
    assert_eq!(bash.reason, "bash is refused by this plugin");

    let read = rules.iter().find(|r| r.tool == "read").expect("read rule");
    assert_eq!(
        read.verdict,
        PluginPermissionVerdict::Abstain,
        "read -> abstain (no opinion)"
    );
}

// ---------------------------------------------------------------------
// Acceptance criterion 2 -- an unsupported permission.policy/1 version
// refuses to load, naming the version mismatch (participant rule).
// ---------------------------------------------------------------------

/// **Criterion 2.** A plugin that declares `permission.policy/1` at version
/// 2 (the host speaks version 1) must be REFUSED at discover with a typed
/// `HandshakeRefused` whose detail names BOTH versions and the point --
/// the participant rule: an incompatible version is refused, never silently
/// never-run.
#[tokio::test]
async fn an_unsupported_permission_policy_version_refuses_to_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "policy_ver.py",
        common::PERSISTENT_POLICY_VERSION_MISMATCH_PLUGIN,
    )
    .await;

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("an unsupported permission.policy/1 version must refuse to load");
    match err {
        SubprocessPluginError::HandshakeRefused {
            condition, detail, ..
        } => {
            assert!(
                condition.contains("permission.policy/1"),
                "the condition names the point: {condition}"
            );
            assert!(
                condition.contains("version mismatch"),
                "the condition names the mismatch: {condition}"
            );
            assert!(
                detail.contains("version 1"),
                "the detail names the host's version: {detail}"
            );
            assert!(
                detail.contains("version 2"),
                "the detail names the plugin's version: {detail}"
            );
        }
        other => {
            panic!("an unsupported permission.policy/1 version is HandshakeRefused, got {other:?}")
        }
    }
}

// ---------------------------------------------------------------------
// Acceptance criterion 3 -- a malformed policy answer fails closed.
// ---------------------------------------------------------------------

/// **Criterion 3.** A plugin that declares `permission.policy/1` at a
/// supported version but answers with a structurally-malformed body
/// (`rules` is not an array) must fail CLOSED with a typed
/// `HandshakeMalformed` -- never silently no-op.
#[tokio::test]
async fn a_malformed_policy_answer_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "policy_bad.py",
        common::PERSISTENT_POLICY_MALFORMED_PLUGIN,
    )
    .await;

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("a malformed policy answer must fail closed");
    match err {
        SubprocessPluginError::HandshakeMalformed { detail, .. } => {
            assert!(
                detail.contains("rules"),
                "the malformed detail names the offending field: {detail}"
            );
        }
        other => panic!("a malformed policy answer is HandshakeMalformed, got {other:?}"),
    }
}

/// A plugin that DELIBERATELY declines to declare a policy (`ok:false` with
/// an error) surfaces as `HandshakeRefused` -- the categorical twin of
/// `initialize/1`'s own `ok:false`-with-error refusal. Sibling of criterion
/// 3: a refusal is not malformation, and the typed variant distinguishes them.
#[tokio::test]
async fn a_refused_policy_answer_surfaces_as_handshake_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "policy_no.py",
        common::PERSISTENT_POLICY_REFUSED_PLUGIN,
    )
    .await;

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("a refused policy answer must surface as HandshakeRefused");
    match err {
        SubprocessPluginError::HandshakeRefused { detail, .. } => {
            assert!(
                detail.contains("I decline to declare a policy"),
                "the refused detail carries the plugin's reason: {detail}"
            );
        }
        other => panic!("a refused policy answer is HandshakeRefused, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Presence-gating -- a plugin that does NOT declare permission.policy/1
// loads normally and contributes no wire policy (advertising != requiring).
// ---------------------------------------------------------------------

/// A plugin that declares ONLY `tool/1` (not `permission.policy/1`) in its
/// `initialize/1` answer must load NORMALLY -- the host does NOT send a
/// `permission.policy/1` request, and `permission_rules` is empty. This is
/// the "advertising a point means the host speaks it, not that the host
/// requires it" rule: the participant refusal is VERSION-gated (both speak
/// the point at incompatible versions), not presence-gated. Reuses the
/// handshake-ok fixture (which declares only `tool/1`).
#[tokio::test]
async fn a_plugin_not_declaring_the_point_loads_normally_with_no_policy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "handshake_ok.py",
        common::PERSISTENT_HANDSHAKE_OK_PLUGIN,
    )
    .await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("a plugin not declaring permission.policy/1 loads normally");
    assert!(
        plugin.permission_rules().is_empty(),
        "a plugin that did not declare the point contributes no wire policy"
    );
}
