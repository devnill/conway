//! End-to-end CLI tests for board item `01M3SJBNF2KZRWA5P5SSF9B868`, driven
//! through the real compiled `conway` binary and the REAL shipped
//! `conway-plugin-checkpoint` crate -- the same "no stub anywhere" standard
//! `postkill_session_targeting.rs` holds itself to, and the file this one is
//! modelled on.
//!
//! Three observables, each one line of the dogfood evidence this item
//! fixes:
//!
//! 1. **`git status` cleanliness.** A snapshot must add nothing to the
//!    project's own `git status` -- not an untracked `.conway/` entry, in
//!    whatever directory the operator happened to launch from.
//! 2. **Subdirectory parity.** A launch from a project subdirectory must
//!    find the SAME checkpoint store a launch from the repository root
//!    would, and vice versa -- the store is keyed by the enclosing git
//!    root, not the bare launch-time cwd.
//! 3. **A refused rollback exits non-zero.** `conway conway.checkpoint.
//!    rollback` against a path with a surviving hand edit must fail the
//!    process, not report a success a script would believe.
//!
//! The diff/notice-wording halves of this item are covered at the unit
//! level in `conway-plugin-checkpoint`'s own `lib.rs` tests -- nothing here
//! duplicates those; this file is only for what requires a real, separate
//! OS process (and, for (1)/(2), a real git repository) to observe at all.

mod common;

use std::path::Path;
use std::process::Command as StdCommand;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{command, run_conway, write_fixture, Fixture};

const WRITTEN_PATH: &str = "worker-notes.txt";
const WRITTEN_CONTENT: &str = "worker line one\nworker line two\n";

/// One real `write` tool call, then the model's follow-up text -- the two
/// requests one `-p` write turn makes against the mock. `count` copies of
/// this pair queue up enough scripted responses for that many SEPARATE
/// one-shot invocations against the SAME long-lived mock server (a request
/// beyond the script's length gets a silent, toolless default -- `Script`'s
/// own doc -- so under-provisioning this would make a later write turn
/// silently do nothing).
fn write_script(count: usize) -> Script {
    let mut entries = Vec::new();
    for _ in 0..count {
        entries.push(vec![
            Chunk::ToolCall {
                name: "write",
                args: serde_json::json!({ "path": WRITTEN_PATH, "content": WRITTEN_CONTENT }),
            },
            Chunk::Finish("tool_calls"),
        ]);
        entries.push(vec![Chunk::Text("wrote it"), Chunk::Finish("stop")]);
    }
    Script(entries)
}

fn add_plugins_install(fixture: &Fixture, ids: &[&str]) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    value["plugins"] = serde_json::json!({ "install": ids });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with [plugins].install");
}

/// Pins `[models].metadata_path` to an ABSOLUTE path -- the one the fixture
/// already populated at `<fixture.dir>/.conway/models.json`
/// (`common::write_fixture`'s own doc) -- so a test that runs the binary
/// with a cwd OTHER than `fixture.dir.path()` (the subdirectory-launch test
/// below) still finds it. Left at its relative default, this would resolve
/// against whatever cwd that particular invocation used instead, which is
/// exactly the variable this file's "subdirectory parity" test needs to
/// hold the model-routing side of the fixture steady while it varies.
fn pin_absolute_models_path(fixture: &Fixture) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    let absolute = fixture.dir.path().join(".conway").join("models.json");
    value["models"] = serde_json::json!({ "metadata_path": absolute.to_string_lossy() });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with an absolute [models].metadata_path");
}

fn git_init(dir: &Path) {
    let status = StdCommand::new("git")
        .current_dir(dir)
        .args(["init", "-q"])
        .status()
        .expect("run `git init` -- is git on PATH?");
    assert!(status.success(), "git init failed in {}", dir.display());
}

fn git_status_porcelain(dir: &Path) -> String {
    let out = StdCommand::new("git")
        .current_dir(dir)
        .args(["status", "--porcelain"])
        .output()
        .expect("run `git status` -- is git on PATH?");
    assert!(
        out.status.success(),
        "git status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Every PER-SESSION `.jsonl` file in the fixture's sessions dir --
/// deliberately excludes `index.jsonl`, which is not a session file.
/// Lifted from `postkill_session_targeting.rs`, which needs the identical
/// discovery for the identical reason (each `tests/*.rs` file is its own
/// independent crate, so this is a second, deliberate copy rather than a
/// shared one). Unlike that file's own copy, THIS one tolerates a sessions
/// dir that does not exist YET (empty, not a panic): this file calls it
/// BEFORE the first session of a test ever runs too, to compute a baseline
/// to diff a later call against -- a directory `conway` has not had a
/// reason to create yet is a real, ordinary state here, not a test bug.
fn session_files(fixture: &Fixture) -> Vec<std::path::PathBuf> {
    let sessions_dir = common::session_dir(fixture);
    match std::fs::read_dir(&sessions_dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|ext| ext == "jsonl")
                    && p.file_stem().and_then(|s| s.to_str()) != Some("index")
            })
            .collect(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => panic!("read sessions dir {sessions_dir:?}: {err}"),
    }
}

fn session_ids(fixture: &Fixture) -> std::collections::HashSet<String> {
    session_files(fixture)
        .into_iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .collect()
}

/// The first `seq N:` line `conway.checkpoint.list`'s own `ListCommand`
/// formatting produces, parsed back out -- what a real caller would read
/// off the real output to learn which seq to roll back to, never a seq this
/// test invents by guessing the observer's own counting.
fn first_listed_seq(stdout: &[u8]) -> u64 {
    let text = String::from_utf8_lossy(stdout);
    let line = text
        .lines()
        .find(|l| l.starts_with("seq "))
        .unwrap_or_else(|| panic!("no 'seq N:' line in listing: {text}"));
    let rest = line.strip_prefix("seq ").expect("checked above");
    let (num, _) = rest
        .split_once(':')
        .unwrap_or_else(|| panic!("malformed listing line: {line}"));
    num.trim()
        .parse()
        .unwrap_or_else(|e| panic!("seq {num:?} did not parse: {e}"))
}

// -----------------------------------------------------------------------
// (1) `git status` cleanliness
// -----------------------------------------------------------------------

/// **The headline fix.** Before this item, `conway.checkpoint`'s shadow
/// store lived at `<cwd>/.conway/checkpoints` -- inside whatever directory
/// the operator launched from, so every snapshot showed up as an untracked
/// `.conway/` entry (the dogfood evidence this item fixes: `git status
/// --short` reporting `?? .conway/` in the project). This test asserts on
/// the OBSERVABLE the evidence named directly: every NEW line `git status
/// --porcelain` gains between before and after a real snapshot-producing
/// write turn must be accounted for by the write itself (the turn's own
/// `worker-notes.txt`, the ordinary and expected product of a real `write`
/// tool call landing in a git-tracked project) -- NOTHING naming `.conway`
/// or `checkpoint`. (Not asserted as a byte-identical status: the fixture's
/// own `conway.json`/`.conway/models.json` are untracked test scaffolding,
/// already present in `before`, and the write turn itself legitimately adds
/// one new untracked file of its own -- the property under test is that the
/// CHECKPOINT PLUGIN adds nothing new, not that nothing in the turn does.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_checkpoint_snapshot_leaves_git_status_unchanged_in_the_project() {
    let mock = MockBackend::start(write_script(1)).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.checkpoint"]);
    git_init(fixture.dir.path());

    // `CONWAY_CONFIG_DIR` pointed at a directory that is NOT the project
    // itself -- the ordinary case this item fixes for (an operator's config
    // directory and a project's git root are two different places).
    // `common::command`'s own default points both at the SAME fixture
    // directory, which would hide exactly the defect this test exists to
    // catch.
    let config_dir = tempfile::tempdir().expect("tempdir");

    let before = git_status_porcelain(fixture.dir.path());
    let out = command(
        &["-p", "write the notes file", "--allowed-tools", "write"],
        &fixture,
    )
    .env("CONWAY_CONFIG_DIR", config_dir.path())
    .output()
    .expect("run conway binary");
    assert!(
        out.status.success(),
        "the scripted write turn must succeed -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = git_status_porcelain(fixture.dir.path());

    let before_lines: std::collections::HashSet<&str> = before.lines().collect();
    let new_lines: Vec<&str> = after
        .lines()
        .filter(|line| !before_lines.contains(line))
        .collect();
    assert_eq!(
        new_lines,
        vec![format!("?? {WRITTEN_PATH}").as_str()],
        "the ONLY new `git status` line a checkpoint-producing write turn may add is the \
         write itself -- the checkpoint plugin's own store must add nothing: before: \
         {before:?}, after: {after:?}"
    );
    assert!(
        !fixture
            .dir
            .path()
            .join(".conway")
            .join("checkpoints")
            .exists(),
        "the shadow store must not be created inside the project at all"
    );

    // Break-the-guard, read the other way: the plugin must have actually
    // captured something, under `CONWAY_CONFIG_DIR` -- a silently broken
    // observer would ALSO leave `git status` unchanged, for the wrong
    // reason.
    let mut env = std::collections::HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        config_dir.path().to_string_lossy().into_owned(),
    );
    let project_dir = fixture
        .dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| fixture.dir.path().to_path_buf());
    let store_root = conway::config::discovery::checkpoint_store_root(&project_dir, &env);
    assert!(
        store_root.join("sessions").exists(),
        "the plugin must have written a real session index at the relocated store: {store_root:?}"
    );
}

// -----------------------------------------------------------------------
// (2) Subdirectory parity
// -----------------------------------------------------------------------

/// A launch from a project subdirectory must find the SAME checkpoint store
/// a launch from the repository root would, and vice versa -- the DOGFOOD
/// evidence named launching from `repo/pkg/sub/` as producing an undo
/// history keyed to the launch directory rather than the project.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subdirectory_launch_finds_snapshots_taken_from_the_repo_root_and_vice_versa() {
    let mock = MockBackend::start(write_script(2)).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.checkpoint"]);
    pin_absolute_models_path(&fixture);
    git_init(fixture.dir.path());

    let sub = fixture.dir.path().join("pkg").join("sub");
    std::fs::create_dir_all(&sub).expect("create subdirectory");

    // Write #1, from the REPO ROOT.
    let before = session_ids(&fixture);
    let out_root = command(
        &["-p", "write the notes file", "--allowed-tools", "write"],
        &fixture,
    )
    .output()
    .expect("run conway binary");
    assert!(
        out_root.status.success(),
        "the root write turn must succeed -- stderr: {}",
        String::from_utf8_lossy(&out_root.stderr)
    );
    let after = session_ids(&fixture);
    let sid_from_root: Vec<&String> = after.difference(&before).collect();
    assert_eq!(sid_from_root.len(), 1, "expected exactly one new session");
    let sid_from_root = sid_from_root[0].clone();

    // Read it back from the SUBDIRECTORY.
    let listed_from_sub = command(
        &["conway.checkpoint.list", "--session", &sid_from_root],
        &fixture,
    )
    .current_dir(&sub)
    .output()
    .expect("run conway binary");
    assert!(
        listed_from_sub.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed_from_sub.stderr)
    );
    let listed_from_sub_stdout = String::from_utf8_lossy(&listed_from_sub.stdout);
    assert!(
        listed_from_sub_stdout.contains(WRITTEN_PATH),
        "a launch from a subdirectory must find a snapshot taken from the repo root: \
         {listed_from_sub_stdout}"
    );
    assert!(
        !listed_from_sub_stdout.contains("no snapshots recorded yet"),
        "{listed_from_sub_stdout}"
    );

    // Write #2, from the SUBDIRECTORY.
    let before = session_ids(&fixture);
    let out_sub = command(
        &["-p", "write the notes file", "--allowed-tools", "write"],
        &fixture,
    )
    .current_dir(&sub)
    .output()
    .expect("run conway binary");
    assert!(
        out_sub.status.success(),
        "the subdirectory write turn must succeed -- stderr: {}",
        String::from_utf8_lossy(&out_sub.stderr)
    );
    let after = session_ids(&fixture);
    let sid_from_sub: Vec<&String> = after.difference(&before).collect();
    assert_eq!(sid_from_sub.len(), 1, "expected exactly one new session");
    let sid_from_sub = sid_from_sub[0].clone();

    // Read it back from the REPO ROOT.
    let listed_from_root = run_conway(
        &["conway.checkpoint.list", "--session", &sid_from_sub],
        &fixture,
    );
    assert!(
        listed_from_root.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed_from_root.stderr)
    );
    let listed_from_root_stdout = String::from_utf8_lossy(&listed_from_root.stdout);
    assert!(
        listed_from_root_stdout.contains(WRITTEN_PATH),
        "a launch from the repo root must find a snapshot taken from a subdirectory: \
         {listed_from_root_stdout}"
    );
    assert!(
        !listed_from_root_stdout.contains("no snapshots recorded yet"),
        "{listed_from_root_stdout}"
    );
}

// -----------------------------------------------------------------------
// (3) A refused rollback exits non-zero headless
// -----------------------------------------------------------------------

/// The dogfood evidence this half fixes, verbatim: a hand edit present, a
/// plain `rollback <seq>` printed a refusal notice but exited 0, so a
/// script watching only the exit code believed the restore had succeeded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conflicted_rollback_exits_non_zero_headless() {
    let mock = MockBackend::start(write_script(1)).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.checkpoint"]);

    let before = session_ids(&fixture);
    let out = run_conway(
        &["-p", "write the notes file", "--allowed-tools", "write"],
        &fixture,
    );
    assert!(
        out.status.success(),
        "the scripted write turn must succeed -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = session_ids(&fixture);
    let sid: Vec<&String> = after.difference(&before).collect();
    assert_eq!(sid.len(), 1);
    let sid = sid[0].clone();

    let listed = run_conway(&["conway.checkpoint.list", "--session", &sid], &fixture);
    assert!(listed.status.success());
    let seq = first_listed_seq(&listed.stdout);

    // The operator's own hand edit, bypassing conway entirely.
    let written_path = fixture.dir.path().join(WRITTEN_PATH);
    std::fs::write(&written_path, "HAND-EDITED-BY-THE-OPERATOR").expect("hand edit the file");

    let rollback = run_conway(
        &[
            "conway.checkpoint.rollback",
            "--session",
            &sid,
            &seq.to_string(),
        ],
        &fixture,
    );
    assert_ne!(
        rollback.status.code(),
        Some(0),
        "a refused rollback must not exit 0 -- stdout: {}, stderr: {}",
        String::from_utf8_lossy(&rollback.stdout),
        String::from_utf8_lossy(&rollback.stderr)
    );
    assert_eq!(
        rollback.status.code(),
        Some(1),
        "a plugin command's own failure maps to exit 1 (`conway_cli::exit::ExitCode::\
         AgentFailed`), the same code every other `CommandOutcome::Error` from this dispatch \
         produces"
    );
    let stderr = String::from_utf8_lossy(&rollback.stderr);
    assert!(
        stderr.contains("refused"),
        "the refusal must be worded as one: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&written_path).unwrap(),
        "HAND-EDITED-BY-THE-OPERATOR",
        "a refused rollback must never touch the file"
    );
}
