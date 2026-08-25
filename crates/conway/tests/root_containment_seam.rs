//! Regression/acceptance tests for the broker's root-confinement check
//! ("S5: the broker root check") -- the SECURITY-CRITICAL slice: the only
//! one in this cycle where a mistake is a security bug rather than a
//! defect.
//!
//! Mirrors `permission_pattern_seam.rs` exactly, for the identical reason:
//! the absence of seam-spanning tests is what hid BOTH 0.5.0 permission
//! bugs (the inert pattern grants, and the plan-mode-below-the-cache
//! ordering bug). A hand-written `AuthorizedCall`/`PermissionRequest`
//! fixture proves nothing about whether the real pipeline -- real
//! `ReadTool`/`WriteTool`/`BashTool` (the `builtin-tools` feature, not a
//! test fixture) driven by a real agent turn through the real
//! `ToolRunner`/`PermissionBroker` -- actually enforces confinement. Every
//! test below drives that real stack end to end and asserts on what the
//! broker's `PermissionGate` actually received (a [`RecordingGate`]) and
//! what the tool's own persisted [`conway_core::content::ToolResult`]
//! actually says.
//!
//! A confined child is always produced via [`SessionHandle::spawn`] with
//! [`SpawnSpec::root`].
//!
//! **Root-agent confinement, added later.** `RootSpec` (a session's ROOT
//! agent -- the one an operator actually talks to) used to have no `root`
//! field at all, so every root agent started unconfined regardless of what
//! `PermissionBroker::check_root` could do, and `must_reach_gate` was
//! therefore always false for it. `ConwayBuilder::with_root` closes that gap
//! (see [`build_conway_with_root`] below); the tests in the final section of
//! this file ("root agent confinement") drive that surface end to end,
//! through the same real stack every other test here does, and additionally
//! cover the one composition this codebase could never previously exercise:
//! a spawned child's root narrowing against a parent that is ITSELF
//! confined (every earlier test here confines only the child, against an
//! always-`Unconfined` parent).
//!
//! **Retirement of the harness-level pre-gate check ("Retire the
//! harness-level confinement root once `conway.fs` enforces its own"):**
//! `PermissionBroker::check_root` no longer walks a `PathArgs::Named`
//! tool's own declared path arguments at all -- `conway.fs` does, INSIDE
//! the tool (`conway_tools::fs::beneath`, open-relative, TOCTOU-closed),
//! before its own I/O runs. Every test below that used to assert
//! `gate.requests().is_empty()` for a `PathArgs::Named` denial
//! (`read`/`cd`, never `bash`) now asserts `gate.requests().len() == 1`
//! instead: the operator's gate IS reached now (nothing before
//! `conway.fs`'s own check, which runs inside the tool, stops it), and the
//! call is STILL refused afterward -- the property that matters (the
//! read/cd never actually happens) is unchanged; only WHERE in the
//! pipeline the refusal happens moved. `bash`'s own `PathArgs::
//! Unconfinable { checkable }` tests (5, 6, 7, 8, 9, 9b, 12) are UNCHANGED:
//! `checkable` (e.g. `bash`'s `cwd`) is still walked by `PermissionBroker::
//! check_root` directly -- `bash` belongs to a different plugin than
//! `conway.fs` and has no plugin-level root check of its own to delegate
//! to, which is precisely the asymmetry GP-13 records ("a harness-level
//! root appears to cover every tool while actually covering only those
//! declaring path arguments").
//!
//! `--root`/`ConwayBuilder::with_root` and a spawned child's
//! `SubagentSpec::root` still confine ordinary tool calls end to end
//! (tests 2, 4, 11, 11b, 15 all still pass): `runtime::root::start_root`/
//! `subagent::SubagentHost::start` now derive a `conway.fs.root` per-agent
//! plugin-config entry from the SAME resolved, validated,
//! narrowing-checked `root` value they already computed for the
//! (unrelated, still-live) artifact-writer confinement path -- see
//! `conway_runtime::permission::derive_fs_root_config`'s own doc.
#![cfg(feature = "builtin-tools")]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{
    Conway, ConwayBuilder, PatternRule, PluginSelection, SessionHandle, SessionSpec, SpawnSpec,
};
use conway_core::agent::{PermissionDecision, PermissionRequest, PermissionScope};
use conway_core::content::{ContentBlock, StopReason, ToolCall, ToolResult, Usage};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, ToolName};
use conway_core::log::LogRecord;
use conway_core::permission_mode::PermissionMode;
use conway_core::ports::{Backend, GenerateResponse, PermissionGate};
use conway_testkit::{text_response, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use tempfile::TempDir;

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

fn tool_call_response(tool: &str, arguments: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: "call_1".to_string(),
            name: ToolName::new(tool),
            arguments,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

fn read_call(path: &str) -> GenerateResponse {
    tool_call_response("read", serde_json::json!({ "path": path }))
}

fn write_call(path: &str, content: &str) -> GenerateResponse {
    tool_call_response(
        "write",
        serde_json::json!({ "path": path, "content": content }),
    )
}

fn cd_call(path: &str) -> GenerateResponse {
    tool_call_response("cd", serde_json::json!({ "path": path }))
}

/// `cwd: None` omits the argument entirely (BashArgs::cwd is optional --
/// this is the shape that must NOT be treated as a violation, per the
/// item's own step-3 "absent is not an error" rule).
fn bash_call(command: &str, cwd: Option<&Path>) -> GenerateResponse {
    let mut args = serde_json::json!({ "command": command });
    if let Some(cwd) = cwd {
        args["cwd"] = serde_json::Value::String(cwd.display().to_string());
    }
    tool_call_response("bash", args)
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
        default_role: RoleAlias::new("default"),
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

/// Records every `PermissionRequest` it receives and always answers with a
/// fixed `decision` -- see `permission_pattern_seam.rs`'s identical fixture
/// for why this (rather than `conway_testkit::FakeGate` alone) is the
/// right shape: a test that expects the gate to be BYPASSED needs to see
/// zero requests, and a test that expects it to be REACHED needs to see the
/// decision actually take effect (never executed for real if it denies).
struct RecordingGate {
    decision: PermissionDecision,
    requests: Mutex<Vec<PermissionRequest>>,
}

impl RecordingGate {
    fn new(decision: PermissionDecision) -> Arc<Self> {
        Arc::new(Self {
            decision,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<PermissionRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl PermissionGate for RecordingGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        self.requests.lock().unwrap().push(req);
        self.decision.clone()
    }
}

fn build_conway(script: Vec<ScriptedTurn>, gate: Arc<dyn PermissionGate>) -> Conway {
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("fake")));
    let store = Arc::new(FakeStore::new());
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend as Arc<dyn Backend>)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        // (bash ships on by default and cannot be declined):
        // this file drives the REAL `bash` tool end to end, so it must now
        // opt in explicitly -- the facade's own default excludes it.
        .with_builtin_plugins(PluginSelection::All)
        .build()
        .expect("build should succeed with the real builtin fs/bash tools registered")
}

/// Identical to [`build_conway`], plus `ConwayBuilder::with_root(root)` --
/// the root-confinement item's own operator surface. Every
/// session this `Conway` starts (`conway.new_session`) is therefore a
/// CONFINED root agent, not only a spawned child.
fn build_conway_with_root(
    script: Vec<ScriptedTurn>,
    gate: Arc<dyn PermissionGate>,
    root: &Path,
) -> Conway {
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("fake")));
    let store = Arc::new(FakeStore::new());
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend as Arc<dyn Backend>)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .with_root(root)
        // (bash ships on by default and cannot be declined):
        // this file drives the REAL `bash` tool end to end, so it must now
        // opt in explicitly -- the facade's own default excludes it.
        .with_builtin_plugins(PluginSelection::All)
        .build()
        .expect("build should succeed with the real builtin fs/bash tools registered")
}

/// Runs `handle`'s root turn to completion -- used only by the
/// cache-priming test, to make an UNCONFINED call on the root agent before
/// spawning the confined child whose identical call must still be denied.
async fn run_root_turn(handle: &SessionHandle, prompt: &str) {
    let turn = handle.prompt(prompt).await.expect("root prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("root turn must not hang");
}

/// Spawns a child per `spec`, waits for it to finish, and returns its full
/// transcript -- the real production path end to end (`SessionHandle::spawn`
/// -> `Runtime::start` -> `SubagentHost::start` -> `AgentLoop::run` ->
/// `ToolRunner`/`PermissionBroker`).
async fn spawn_and_await(handle: &SessionHandle, spec: SpawnSpec) -> Vec<LogRecord> {
    let child = handle
        .spawn(handle.root(), spec)
        .await
        .expect("spawn should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(10), handle.await_agent(child))
        .await
        .expect("child turn must not hang")
        .expect("await_agent should resolve Ok");
    handle
        .transcript(child)
        .await
        .expect("transcript should resolve")
}

/// The LAST `ToolResultRecord` in a spawned child's transcript -- i.e. the
/// child's own, not an ancestor's. `SessionHandle::transcript`'s ancestry
/// walk deliberately still shows a spawned child's parent's prior records
/// too (the disclosed "transcript quirk": clean-slate describes the
/// child's *context*, not `transcript()`'s output -- see
/// `SessionHandle::spawn`'s own doc), so a test that also drives a tool
/// call on the parent (the cache-priming test) would find THAT record
/// first with a plain forward search. Every record after the child's own
/// head record is the child's, and its tool call is always this file's
/// last one dispatched -- taking the last `ToolResultRecord` is therefore
/// always the child's own, whether or not a parent call precedes it.
fn tool_result(records: &[LogRecord]) -> &ToolResult {
    records
        .iter()
        .rev()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } => Some(result),
            _ => None,
        })
        .expect("expected a ToolResultRecord in the child's transcript")
}

fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------
// 1. `read` inside the root -> allowed as normal.
// ---------------------------------------------------------------------
#[tokio::test]
async fn read_inside_root_is_allowed() {
    let root_dir = TempDir::new().unwrap();
    std::fs::write(root_dir.path().join("file.txt"), b"hello from inside").unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(read_call("file.txt")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("read the file")
        .root(root_dir.path())
        .cwd(root_dir.path());
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        !result.is_error,
        "an in-root read must succeed: {:?}",
        blocks_text(&result.blocks)
    );
    assert!(blocks_text(&result.blocks).contains("hello from inside"));
    assert_eq!(
        gate.requests().len(),
        1,
        "an in-root, fully-confinable call reaches the gate exactly as before this slice"
    );
}

// ---------------------------------------------------------------------
// 2. `read` outside the root -> denied. `conway.fs` enforces this now, not
//    a harness-level pre-gate check -- see this file's own module doc
//    (`[Retirement]`) for the full accounting. The operator's own gate
//    (`AllowOnce` here) IS reached exactly once first: nothing before
//    `conway.fs`'s own containment check, which now lives INSIDE the tool,
//    stops a `PathArgs::Named` call from reaching the gate any more. The
//    security property that matters -- the read never actually happens --
//    holds regardless: even though the gate says yes, `conway.fs` still
//    refuses.
// ---------------------------------------------------------------------
#[tokio::test]
async fn read_outside_root_is_denied() {
    let tmp = TempDir::new().unwrap();
    let root_dir = tmp.path().join("root");
    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir(&root_dir).unwrap();
    std::fs::create_dir(&outside_dir).unwrap();
    std::fs::write(outside_dir.join("secret.txt"), b"TOP SECRET").unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let secret_path = outside_dir.join("secret.txt");
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(read_call(&secret_path.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("read the secret")
        .root(&root_dir)
        .cwd(&root_dir);
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        result.is_error,
        "an out-of-root absolute path must be denied -- by `conway.fs` itself now, even though \
         the gate above already said AllowOnce: {:?}",
        blocks_text(&result.blocks)
    );
    assert_eq!(
        gate.requests().len(),
        1,
        "the gate IS reached once now (the pre-gate harness check that used to short-circuit \
         this is retired) -- the refusal comes from `conway.fs` afterward, not from the gate \
         never being asked: {:?}",
        gate.requests()
    );
}

// ---------------------------------------------------------------------
// 2a. (Retirement) a path argument carrying a NUL
// byte is denied at `conway_tools::common::resolve_path` -- the production
// callsite for `conway_core::containment::resolve_candidate` that now
// matters for this argument (the harness-level `PermissionBroker::
// check_root`'s OWN, separate call to the same shared function, ahead of
// the gate, is retired for `PathArgs::Named` tools -- see this file's own
// module doc). Driven end to end (real `ReadTool`, real broker, real gate
// double) rather than unit-testing the resolver in isolation -- exactly the
// discriminating shape this item's own acceptance criteria require: a test
// of the shared function alone proves nothing about whether a real call
// still reaches it.
// ---------------------------------------------------------------------
#[tokio::test]
async fn read_with_nul_byte_path_under_root_is_denied_not_bypassed() {
    let root_dir = TempDir::new().unwrap();
    std::fs::write(root_dir.path().join("file.txt"), b"hello from inside").unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        vec![
            // A NUL byte embedded in an otherwise ordinary relative path --
            // untrusted, model-influenced input. `resolve_path` (inside
            // `ReadTool::invoke` itself, BEFORE `conway.fs`'s own root
            // check ever runs) must refuse to resolve it.
            ScriptedTurn::Respond(read_call("file.txt\0/etc/passwd")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("read the file")
        .root(root_dir.path())
        .cwd(root_dir.path());
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        result.is_error,
        "a NUL-carrying path argument must be denied, not silently resolved: {:?}",
        blocks_text(&result.blocks)
    );
    // The persisted error text names `resolve_path`'s own distinctive
    // wording -- proving THIS is what denied it, not some other, unrelated
    // failure that happens to also set `is_error`.
    assert!(
        blocks_text(&result.blocks).contains("path contains a NUL byte"),
        "the denial must be `resolve_path`'s own typed NUL guard: {:?}",
        blocks_text(&result.blocks)
    );
    // Retirement: the gate IS reached now
    // (`PermissionBroker` no longer denies a `PathArgs::Named` call
    // pre-gate at all, for any reason) -- `resolve_path`'s NUL guard fires
    // INSIDE `ReadTool::invoke`, strictly after the gate already said
    // AllowOnce. The call is still refused; it is refused by the tool, not
    // by the gate never being asked.
    assert_eq!(
        gate.requests().len(),
        1,
        "the gate is reached once now, exactly like every other `PathArgs::Named` denial in \
         this file: {:?}",
        gate.requests()
    );
}

// ---------------------------------------------------------------------
// 2b. (Retirement) `cd` out of the root ->
// denied, now by `conway.fs` itself (`crate::fs::beneath::confined_
// metadata`, wired into `CdTool::invoke`) rather than by a harness-level
// generic `PathArgs::Named` walk that ran ahead of the gate. `CdTool`
// contains no root-specific SYMLINK/CONTAINMENT-ALGORITHM code of its own
// (it delegates to `beneath`, the same as `read`/`write`/`edit`) -- but it
// DOES now call into its own plugin's confinement, unlike before this item,
// when it called nothing at all and relied entirely on the harness.
//
// This test exists because the docs assert this guarantee. An asserted
// security property with no test is how both 0.5.0 fail-open bugs
// survived.
// ---------------------------------------------------------------------
#[tokio::test]
async fn cd_out_of_the_root_is_denied_by_the_generic_path_arg_check() {
    let tmp = TempDir::new().unwrap();
    let root_dir = tmp.path().join("root");
    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir(&root_dir).unwrap();
    std::fs::create_dir(&outside_dir).unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(cd_call(&outside_dir.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("leave the root")
        .root(&root_dir)
        .cwd(&root_dir);
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        result.is_error,
        "a `cd` to a directory outside the confinement root must be denied -- by `conway.fs` \
         itself now: {:?}",
        blocks_text(&result.blocks)
    );
    assert_eq!(
        gate.requests().len(),
        1,
        "the gate is reached once now (the harness-level pre-gate walk that used to deny this \
         before the gate is retired) -- `conway.fs` still refuses the `cd` afterward: {:?}",
        gate.requests()
    );
}

// ---------------------------------------------------------------------
// 2c. `cd` WITHIN the root -> allowed. cwd was never the security
//     boundary, so moving around inside the root is unremarkable and must
//     not be collateral damage of the check above.
// ---------------------------------------------------------------------
#[tokio::test]
async fn cd_within_the_root_is_allowed() {
    let root_dir = TempDir::new().unwrap();
    let sub = root_dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(cd_call(&sub.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("move within the root")
        .root(root_dir.path())
        .cwd(root_dir.path());
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        !result.is_error,
        "a `cd` inside the root must succeed; got {:?}",
        blocks_text(&result.blocks)
    );
}

// ---------------------------------------------------------------------
// 3. `write` to a non-existent target inside the root -> allowed (the
//    write-target case `CanonicalRoot` exists for).
// ---------------------------------------------------------------------
#[tokio::test]
async fn write_nonexistent_target_inside_root_is_allowed() {
    let root_dir = TempDir::new().unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(write_call("new/dir/file.txt", "hi there")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("write the file")
        .root(root_dir.path())
        .cwd(root_dir.path());
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        !result.is_error,
        "a write to a non-existent-but-in-root target must succeed: {:?}",
        blocks_text(&result.blocks)
    );
    let written = std::fs::read_to_string(root_dir.path().join("new/dir/file.txt"))
        .expect("the file must actually have been written");
    assert_eq!(written, "hi there");
    assert_eq!(gate.requests().len(), 1);
}

// ---------------------------------------------------------------------
// 4. (Retirement) Symlink escape (`root/link
//    -> ../outside`, read `root/link/secret`) -> denied, by `conway.fs`
//    itself now (open-relative, TOCTOU-closed -- see `conway_tools::fs::
//    beneath`'s own doc and `conway-tools/tests/fs_confinement.rs` for the
//    dedicated closure proof).
// ---------------------------------------------------------------------
#[tokio::test]
#[cfg(unix)]
async fn symlink_escape_is_denied() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir(&repo_dir).unwrap();
    std::fs::create_dir(&outside_dir).unwrap();
    std::fs::write(outside_dir.join("secret.txt"), b"TOP SECRET").unwrap();
    symlink(Path::new("../outside"), repo_dir.join("link")).unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(read_call("link/secret.txt")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("read through the link")
        .root(&repo_dir)
        .cwd(&repo_dir);
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        result.is_error,
        "a symlink that resolves outside the root must be denied: {:?}",
        blocks_text(&result.blocks)
    );
    assert_eq!(
        gate.requests().len(),
        1,
        "the gate is reached once now (the harness-level pre-gate check that used to deny this \
         before the gate is retired) -- `conway.fs`'s own open-relative check still refuses the \
         symlink escape afterward: {:?}",
        gate.requests()
    );
}

// ---------------------------------------------------------------------
// 5. A pattern grant cannot defeat root: install a matching grant, attempt
//    out-of-root (via `bash`'s checkable `cwd`) -- still denied.
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_pattern_grant_cannot_defeat_root() {
    let tmp = TempDir::new().unwrap();
    let root_dir = tmp.path().join("root");
    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir(&root_dir).unwrap();
    std::fs::create_dir(&outside_dir).unwrap();

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "must not be consulted".into(),
    });
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(bash_call("echo hi", Some(&outside_dir))),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    // A grant that would normally authorize this exact command without
    // ever troubling the gate.
    conway.grant_permission_pattern(
        PatternRule::parse("bash:echo hi").expect("valid rule"),
        PermissionScope::Session,
        AgentId::new(),
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("run the command")
        .root(&root_dir)
        .cwd(&root_dir);
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        result.is_error,
        "an out-of-root `cwd` must be denied even under a matching pattern grant"
    );
    assert!(
        gate.requests().is_empty(),
        "root must deny before the pattern grant (or the gate) is ever consulted: {:?}",
        gate.requests()
    );
}

// ---------------------------------------------------------------------
// 6. A cached `AllowAlways` cannot defeat root: grant it (via an unconfined
//    root call), attempt the byte-identical call from a confined child --
//    still denied, and the gate is never re-consulted.
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_cached_allow_always_cannot_defeat_root() {
    let tmp = TempDir::new().unwrap();
    let outside_dir = tmp.path().join("outside");
    let child_root_dir = tmp.path().join("child_root");
    std::fs::create_dir(&outside_dir).unwrap();
    std::fs::create_dir(&child_root_dir).unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowAlways {
        scope: PermissionScope::Session,
    });
    let conway = build_conway(
        vec![
            // The root's OWN call: unconfined, so it reaches the gate
            // normally and gets cached at Session scope.
            ScriptedTurn::Respond(bash_call("echo hi", Some(&outside_dir))),
            ScriptedTurn::Respond(text_response("done")),
            // The child's call: byte-identical `(tool, arguments)`, so it
            // hits the SAME cache key -- but the child is confined to a
            // root that does not contain `outside_dir`.
            ScriptedTurn::Respond(bash_call("echo hi", Some(&outside_dir))),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");

    run_root_turn(&handle, "prime the cache").await;
    assert_eq!(
        gate.requests().len(),
        1,
        "the root's own unconfined call must reach the gate and get cached"
    );

    let spec = SpawnSpec::new("run the identical command")
        .root(&child_root_dir)
        .cwd(&child_root_dir);
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        result.is_error,
        "the byte-identical, Session-scope-cached call must still be denied for the \
         confined child"
    );
    assert_eq!(
        gate.requests().len(),
        1,
        "the confined child's call must NOT re-consult the gate either -- root denies \
         before the cache lookup even happens: {:?}",
        gate.requests()
    );
}

// ---------------------------------------------------------------------
// 7. `AutoAllow` mode cannot defeat root -> still denied.
// ---------------------------------------------------------------------
#[tokio::test]
async fn auto_allow_mode_cannot_defeat_root() {
    let tmp = TempDir::new().unwrap();
    let root_dir = tmp.path().join("root");
    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir(&root_dir).unwrap();
    std::fs::create_dir(&outside_dir).unwrap();
    std::fs::write(outside_dir.join("secret.txt"), b"TOP SECRET").unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let secret_path = outside_dir.join("secret.txt");
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(read_call(&secret_path.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway.set_permission_mode(PermissionMode::AutoAllow);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("read the secret")
        .root(&root_dir)
        .cwd(&root_dir);
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        result.is_error,
        "AutoAllow must not defeat an out-of-root read"
    );
    assert!(
        gate.requests().is_empty(),
        "AutoAllow's own immediate-Allow path must never even be reached: {:?}",
        gate.requests()
    );
}

// ---------------------------------------------------------------------
// 8. A tool declaring `Unconfinable` (bash's own command) under a root
//    always reaches `gate.check` -- never auto-allowed, even under
//    `AutoAllow` mode with a matching pattern grant installed.
// ---------------------------------------------------------------------
#[tokio::test]
async fn unconfinable_bash_command_always_reaches_the_gate_under_a_root() {
    let root_dir = TempDir::new().unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(bash_call("echo hi", None)),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway.set_permission_mode(PermissionMode::AutoAllow);
    conway.grant_permission_pattern(
        PatternRule::parse("bash:echo hi").expect("valid rule"),
        PermissionScope::Session,
        AgentId::new(),
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("run a harmless command")
        .root(root_dir.path())
        .cwd(root_dir.path());
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        !result.is_error,
        "the gate's own AllowOnce must still let the call through: {:?}",
        blocks_text(&result.blocks)
    );
    assert_eq!(
        gate.requests().len(),
        1,
        "an Unconfinable call under a root must reach the gate even with AutoAllow mode \
         AND a matching pattern grant both in play: {:?}",
        gate.requests()
    );
}

// ---------------------------------------------------------------------
// 9. `bash` with a `cwd` argument outside the root -> denied (its own
//    `checkable`), with no pattern/AutoAllow involved -- the plain case.
// ---------------------------------------------------------------------
#[tokio::test]
async fn bash_cwd_outside_root_is_denied() {
    let tmp = TempDir::new().unwrap();
    let root_dir = tmp.path().join("root");
    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir(&root_dir).unwrap();
    std::fs::create_dir(&outside_dir).unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(bash_call("echo hi", Some(&outside_dir))),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("run the command")
        .root(&root_dir)
        .cwd(&root_dir);
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        result.is_error,
        "bash's own `cwd` argument, when outside the root, must be denied"
    );
    assert!(gate.requests().is_empty());
}

// ---------------------------------------------------------------------
// 9b. `bash` with a `cwd` argument INSIDE the root -> allowed as normal,
//     same as any other in-root, fully-confinable call. `cwd` was never
//     the security boundary here either -- this pins that the `checkable`
//     enforcement above (test 9) is not collateral-damaging an ordinary
//     in-root `cwd` (S6, part 1).
// ---------------------------------------------------------------------
#[tokio::test]
async fn bash_cwd_inside_root_is_allowed() {
    let root_dir = TempDir::new().unwrap();
    let sub = root_dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(bash_call("echo hi", Some(&sub))),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let spec = SpawnSpec::new("run the command")
        .root(root_dir.path())
        .cwd(root_dir.path());
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        !result.is_error,
        "bash's own `cwd` argument, when inside the root, must succeed exactly as it did \
         before root confinement existed: {:?}",
        blocks_text(&result.blocks)
    );
    assert!(blocks_text(&result.blocks).contains("hi"));
    assert_eq!(
        gate.requests().len(),
        1,
        "an in-root `cwd` still reaches the gate exactly once, as any Unconfinable call does"
    );
}

// ---------------------------------------------------------------------
// 10. No root set (`None`) -> byte-for-byte unchanged: a call reaching a
//     path outside the child's own `cwd` (but with no root confining it)
//     is allowed exactly as it always was.
// ---------------------------------------------------------------------
#[tokio::test]
async fn no_root_leaves_behavior_unchanged() {
    let tmp = TempDir::new().unwrap();
    let cwd_dir = tmp.path().join("a");
    let other_dir = tmp.path().join("b");
    std::fs::create_dir(&cwd_dir).unwrap();
    std::fs::create_dir(&other_dir).unwrap();
    std::fs::write(other_dir.join("other.txt"), b"unconfined").unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let other_path = other_dir.join("other.txt");
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(read_call(&other_path.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    // Deliberately no `.root(..)` call: this child inherits the root
    // session's `None`, exactly as every spawn did before this slice.
    let spec = SpawnSpec::new("read the other file").cwd(&cwd_dir);
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        !result.is_error,
        "with no root at all, a path outside cwd is still allowed exactly as before this \
         slice: {:?}",
        blocks_text(&result.blocks)
    );
    assert!(blocks_text(&result.blocks).contains("unconfined"));
    assert_eq!(
        gate.requests().len(),
        1,
        "the root check must be a complete no-op when the agent has no root"
    );
}

// =======================================================================
// Root agent confinement.
//
// Every test above confines only a SPAWNED CHILD -- the parent it spawns
// from is always `Unconfined` (`build_conway` never calls `with_root`).
// These tests instead confine the ROOT agent itself, via
// `build_conway_with_root`/`ConwayBuilder::with_root`, driven through the
// exact same real stack: `Conway::new_session` -> `Runtime::start_root` ->
// real `ReadTool`/`BashTool` -> real `ToolRunner`/`PermissionBroker`.
// =======================================================================

// ---------------------------------------------------------------------
// 11. A configured root confines the ROOT agent's own tool calls, not only
//     a spawned child's -- the central claim this item exists to make true.
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_configured_root_confines_the_root_agent_itself() {
    let tmp = TempDir::new().unwrap();
    let root_dir = tmp.path().join("root");
    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir(&root_dir).unwrap();
    std::fs::create_dir(&outside_dir).unwrap();
    std::fs::write(outside_dir.join("secret.txt"), b"TOP SECRET").unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let secret_path = outside_dir.join("secret.txt");
    let conway = build_conway_with_root(
        vec![
            ScriptedTurn::Respond(read_call(&secret_path.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
        &root_dir,
    );

    let handle = conway
        .new_session(SessionSpec {
            cwd: Some(root_dir.clone()),
            ..SessionSpec::default()
        })
        .await
        .expect("new_session");
    run_root_turn(&handle, "read the secret").await;

    let records = handle
        .transcript(handle.root())
        .await
        .expect("transcript should resolve");
    let result = tool_result(&records);
    assert!(
        result.is_error,
        "the ROOT agent's own out-of-root read must be denied, not just a spawned child's: {:?}",
        blocks_text(&result.blocks)
    );
    assert_eq!(
        gate.requests().len(),
        1,
        "(Retirement) the gate is \
         reached once now for the root agent too, exactly like a spawned child's identical call \
         (test 2) -- `conway.fs` still refuses afterward: {:?}",
        gate.requests()
    );
}

// ---------------------------------------------------------------------
// 11b. (Retirement) `cd` out of a
//      CONFINED ROOT AGENT's own root -> denied, by the same `conway.fs`
//      mechanism as test 2b -- explicitly confirmed for a root-agent root,
//      per this item's own "Interaction with `cd`" requirement, not merely
//      inferred from `read`'s behavior above.
// ---------------------------------------------------------------------
#[tokio::test]
async fn cd_out_of_a_confined_root_agents_own_root_is_denied() {
    let tmp = TempDir::new().unwrap();
    let root_dir = tmp.path().join("root");
    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir(&root_dir).unwrap();
    std::fs::create_dir(&outside_dir).unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway_with_root(
        vec![
            ScriptedTurn::Respond(cd_call(&outside_dir.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
        &root_dir,
    );

    let handle = conway
        .new_session(SessionSpec {
            cwd: Some(root_dir.clone()),
            ..SessionSpec::default()
        })
        .await
        .expect("new_session");
    run_root_turn(&handle, "leave the root").await;

    let records = handle
        .transcript(handle.root())
        .await
        .expect("transcript should resolve");
    let result = tool_result(&records);
    assert!(
        result.is_error,
        "a `cd` to a directory outside a CONFINED ROOT AGENT's own root must be denied -- by \
         `conway.fs` itself now: {:?}",
        blocks_text(&result.blocks)
    );
    assert_eq!(
        gate.requests().len(),
        1,
        "the gate is reached once now for the root agent too, exactly like test 2b's spawned \
         child: {:?}",
        gate.requests()
    );
}

// ---------------------------------------------------------------------
// 11c. `cd` WITHIN a confined root agent's own root -> allowed. Pairs with
//      11b the same way test 2c pairs with 2b: cwd was never the security
//      boundary, so moving around inside the root is unremarkable and must
//      not be collateral damage of 11b's check.
// ---------------------------------------------------------------------
#[tokio::test]
async fn cd_within_a_confined_root_agents_own_root_is_allowed() {
    let tmp = TempDir::new().unwrap();
    let root_dir = tmp.path().join("root");
    let sub = root_dir.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway_with_root(
        vec![
            ScriptedTurn::Respond(cd_call(&sub.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
        &root_dir,
    );

    let handle = conway
        .new_session(SessionSpec {
            cwd: Some(root_dir.clone()),
            ..SessionSpec::default()
        })
        .await
        .expect("new_session");
    run_root_turn(&handle, "move within the root").await;

    let records = handle
        .transcript(handle.root())
        .await
        .expect("transcript should resolve");
    let result = tool_result(&records);
    assert!(
        !result.is_error,
        "a `cd` inside a confined root agent's own root must succeed; got {:?}",
        blocks_text(&result.blocks)
    );
}

// ---------------------------------------------------------------------
// 12. `must_reach_gate` is reachable for the ROOT agent: bash's own
//     Unconfinable `command` always reaches the gate under a configured
//     root, even with AutoAllow mode AND a matching pattern grant both in
//     play -- the exact property the extension design depend on, and which was vacuous for a root agent before
//     this item (no `RootSpec::root` meant `AgentRoot::reconstruct` always
//     produced `Unconfined` for it, so `must_reach_gate` was always false).
// ---------------------------------------------------------------------
#[tokio::test]
async fn unconfinable_bash_command_always_reaches_the_gate_for_a_confined_root_agent() {
    let root_dir = TempDir::new().unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway_with_root(
        vec![
            ScriptedTurn::Respond(bash_call("echo hi", None)),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
        root_dir.path(),
    );
    conway.set_permission_mode(PermissionMode::AutoAllow);
    conway.grant_permission_pattern(
        PatternRule::parse("bash:echo hi").expect("valid rule"),
        PermissionScope::Session,
        AgentId::new(),
    );

    let handle = conway
        .new_session(SessionSpec {
            cwd: Some(root_dir.path().to_path_buf()),
            ..SessionSpec::default()
        })
        .await
        .expect("new_session");
    run_root_turn(&handle, "run a harmless command").await;

    let records = handle
        .transcript(handle.root())
        .await
        .expect("transcript should resolve");
    let result = tool_result(&records);
    assert!(
        !result.is_error,
        "the gate's own AllowOnce must still let the call through: {:?}",
        blocks_text(&result.blocks)
    );
    assert_eq!(
        gate.requests().len(),
        1,
        "an Unconfinable call from a CONFINED ROOT agent must reach the gate even with \
         AutoAllow mode AND a matching pattern grant both in play: {:?}",
        gate.requests()
    );
}

// ---------------------------------------------------------------------
// 13. No configured root (the default) -> the root agent stays unconfined,
//     byte-for-byte, exactly as every invocation before this item.
// ---------------------------------------------------------------------
#[tokio::test]
async fn no_configured_root_leaves_the_root_agent_unconfined() {
    let tmp = TempDir::new().unwrap();
    let cwd_dir = tmp.path().join("cwd");
    let other_dir = tmp.path().join("other");
    std::fs::create_dir(&cwd_dir).unwrap();
    std::fs::create_dir(&other_dir).unwrap();
    std::fs::write(other_dir.join("other.txt"), b"unconfined").unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let other_path = other_dir.join("other.txt");
    // Deliberately `build_conway`, not `build_conway_with_root`: no
    // `ConwayBuilder::with_root` call at all.
    let conway = build_conway(
        vec![
            ScriptedTurn::Respond(read_call(&other_path.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec {
            cwd: Some(cwd_dir.clone()),
            ..SessionSpec::default()
        })
        .await
        .expect("new_session");
    run_root_turn(&handle, "read the other file").await;

    let records = handle
        .transcript(handle.root())
        .await
        .expect("transcript should resolve");
    let result = tool_result(&records);
    assert!(
        !result.is_error,
        "with no configured root, the ROOT agent itself reads outside its own cwd exactly as \
         before this item: {:?}",
        blocks_text(&result.blocks)
    );
    assert!(blocks_text(&result.blocks).contains("unconfined"));
    assert_eq!(gate.requests().len(), 1);
}

// ---------------------------------------------------------------------
// 14. A spawned child's root can never WIDEN a confined root agent's own
//     root -- the composition this codebase could never previously
//     exercise (every earlier "narrow-only" test confines only the child,
//     against an always-`Unconfined` parent). The spawn itself must fail.
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_spawned_childs_root_cannot_widen_a_confined_root_agents_own_root() {
    let tmp = TempDir::new().unwrap();
    let parent_root = tmp.path().join("parent_root");
    let sideways_root = tmp.path().join("sideways");
    std::fs::create_dir(&parent_root).unwrap();
    std::fs::create_dir(&sideways_root).unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway_with_root(
        vec![],
        gate.clone() as Arc<dyn PermissionGate>,
        &parent_root,
    );

    let handle = conway
        .new_session(SessionSpec {
            cwd: Some(parent_root.clone()),
            ..SessionSpec::default()
        })
        .await
        .expect("new_session");

    let spec = SpawnSpec::new("try to widen the root")
        .root(&sideways_root)
        .cwd(&sideways_root);
    let err = handle
        .spawn(handle.root(), spec)
        .await
        .expect_err("a spawn root disjoint from the parent's own confined root must fail");
    let message = err.to_string();
    assert!(
        message.contains("root"),
        "the spawn failure should name the root mismatch: {message}"
    );
}

// ---------------------------------------------------------------------
// 15. A spawned child that sets NO root override inherits the confined root
//     agent's own root unchanged -- it does not start unconfined just
//     because the parent's confinement is new.
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_spawned_child_with_no_override_inherits_a_confined_root_agents_own_root() {
    let tmp = TempDir::new().unwrap();
    let parent_root = tmp.path().join("parent_root");
    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir(&parent_root).unwrap();
    std::fs::create_dir(&outside_dir).unwrap();
    std::fs::write(outside_dir.join("secret.txt"), b"TOP SECRET").unwrap();

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let secret_path = outside_dir.join("secret.txt");
    let conway = build_conway_with_root(
        vec![
            ScriptedTurn::Respond(read_call(&secret_path.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
        &parent_root,
    );

    let handle = conway
        .new_session(SessionSpec {
            cwd: Some(parent_root.clone()),
            ..SessionSpec::default()
        })
        .await
        .expect("new_session");

    // Deliberately no `.root(..)`/`.cwd(..)` override: the child inherits
    // the parent's cwd AND its now-confined root, unchanged.
    let spec = SpawnSpec::new("read the secret");
    let records = spawn_and_await(&handle, spec).await;

    let result = tool_result(&records);
    assert!(
        result.is_error,
        "a spawned child with no root override must inherit the parent's now-confined root, \
         not start unconfined: {:?}",
        blocks_text(&result.blocks)
    );
    assert_eq!(
        gate.requests().len(),
        1,
        "the gate is reached once now, exactly like every other `PathArgs::Named` denial in \
         this file -- the inherited root still denies afterward, via `conway.fs`'s own \
         (inherited, per (retirement)) `conway.fs.root` \
         plugin-config entry: {:?}",
        gate.requests()
    );
}
