//! End-to-end CLI tests for board item `01M2TWC242P96Z3JDXWC9F3R5E`'s two
//! post-kill answers, driven through the real compiled `conway` binary and
//! the REAL shipped `conway-plugin-checkpoint` crate -- the same "no stub
//! anywhere" standard `checkout_and_mask_plugin.rs` holds
//! `conway-plugin-history` to, and the file this one is modelled on.
//!
//! # The question these tests are the answer to
//!
//! *"After killing a worker mid-edit, could you tell what it had done to
//! your tree?"* conway writes one session file per AGENT, so the object
//! matching "that worker" already existed -- it was unaddressable from a
//! shell, not absent. Two surfaces close that:
//!
//! 1. `conway sessions show <id> --diff` -- what a session's root agent
//!    edited or wrote, reconstructed from its own log.
//! 2. `conway conway.checkpoint.list --session <id>` -- that ONE session's
//!    snapshot store, rather than the empty store of a session conway
//!    minted for the duration of the command.
//!
//! Both were unit-tested and neither had ever been EXECUTED when the
//! implementing lane finished (`crates/conway-cli/tests/**` was outside its
//! fence). That is what this file exists for: every test here runs the
//! shipped binary as a separate OS process, exactly as an operator would
//! type it, and asserts on its real stdout/exit code.
//!
//! # Why every test starts by really writing a file
//!
//! The interesting assertions are all about a session that ALREADY did
//! something, discovered from a LATER, separate process -- the post-kill
//! shape. So the tests that need one script the mock backend to call the
//! real `write` tool (`--allowed-tools write`, one-shot's own fail-closed
//! gate naming the one call it permits), then find the session id the way
//! `checkout_and_mask_plugin.rs` finds one: off disk, since `-p` prints
//! only the assistant's reply.
//!
//! # What this file deliberately does NOT cover
//!
//! `sessions show --diff`'s `## unfinished` heading -- an `edit`/`write`
//! `ToolUse` with no matching `ToolResultRecord`, the defining shape of a
//! mid-edit kill. There is no deterministic way to produce that shape from
//! the shell: the runtime records a result for every call it dispatches
//! (including a denied or failed one), so the only producer is a real kill
//! landing inside the window between an agent's proposal and its result,
//! which is a race, not a test. It is covered at the unit level instead, in
//! `commands::sessions`'s own `tests` module, against hand-built
//! `LogRecord`s. The honest EMPTY state IS reachable from the shell and is
//! covered here, by
//! `a_session_that_touched_nothing_says_so_rather_than_printing_nothing`
//! (a plain code span, not a doc link: rustdoc does not document a test
//! target, so a link here would be one the doc gate cannot resolve).

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, Fixture};

/// The written file's path, as the scripted model names it in the `write`
/// call's own arguments -- relative, so it lands in the fixture's temp dir
/// (every process in this file runs with that dir as its cwd,
/// `common::command`). `sessions show --diff` reports the recorded ARGUMENT
/// verbatim as its `## <path>` heading, so this exact string is what the
/// diff assertions look for.
const WRITTEN_PATH: &str = "worker-notes.txt";

/// Two lines, so the reconstructed diff has more than one `+` row to show.
/// A `write` that CREATED a path reconstructs exactly (`crate::diff::
/// derive_baseline` un-applies a `Write` to the empty string unconditionally
/// -- it does not have to guess at displaced bytes), which is why this file
/// scripts a `write` to a fresh path rather than clobbering an existing one.
const WRITTEN_CONTENT: &str = "worker line one\nworker line two\n";

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

/// Every PER-SESSION `.jsonl` file in the fixture's sessions dir --
/// deliberately excludes `index.jsonl` (`conway-session`'s own
/// `SessionIndex`, written to the SAME directory), which is not a session
/// file. Lifted from `checkout_and_mask_plugin.rs`, which needs the
/// identical discovery for the identical reason.
fn session_files(fixture: &Fixture) -> Vec<std::path::PathBuf> {
    let sessions_dir = common::session_dir(fixture);
    std::fs::read_dir(&sessions_dir)
        .expect("read sessions dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|ext| ext == "jsonl")
                && p.file_stem().and_then(|s| s.to_str()) != Some("index")
        })
        .collect()
}

/// The id of the one session written so far, read off disk -- `-p` prints
/// only the assistant's reply, never an identifying session id, so this is
/// how a test learns which session the previous process actually wrote
/// (`checkout_and_mask_plugin.rs`'s own approach, and `continuity.rs`'s
/// before it).
fn sole_session_id(fixture: &Fixture) -> String {
    let entries = session_files(fixture);
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one session file so far, found {entries:?}"
    );
    entries[0]
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("a session file has a stem")
        .to_string()
}

fn jsonl_lines(stdout: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8(stdout.to_vec())
        .expect("stdout is utf8")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad jsonl line {l}: {e}")))
        .collect()
}

/// The `tool_call_finished.preview` for the call naming `tool_name` in
/// `event.tool_call_proposed`, correlated by `call_id` -- mirrors
/// `tests/durable_memory.rs`'s identical helper.
fn tool_call_finished_preview(lines: &[serde_json::Value], tool_name: &str) -> Option<String> {
    let call_id = lines.iter().find_map(|line| {
        if line["event"] == "tool_call_proposed" && line["tool"] == tool_name {
            line["call_id"].as_str().map(str::to_string)
        } else {
            None
        }
    })?;
    lines.iter().find_map(|line| {
        if line["event"] == "tool_call_finished" && line["call_id"] == call_id {
            line["preview"].as_str().map(str::to_string)
        } else {
            None
        }
    })
}

/// One real `conway -p` process that calls the real `write` tool once, the
/// way a delegated worker would -- the state every "and now, from a later
/// shell, what did it do?" assertion below starts from. Returns the id of
/// the session it wrote.
///
/// `plugins` is passed straight to [`add_plugins_install`] before the run,
/// so a caller that needs `conway.checkpoint`'s observer to have been live
/// during the write asks for it here rather than after the fact (installing
/// it later would leave the store genuinely empty, and the test would be
/// asserting on the wrong thing).
fn a_worker_that_wrote_a_file(fixture: &Fixture, plugins: &[&str]) -> String {
    add_plugins_install(fixture, plugins);

    let out = run_conway(
        &[
            "-p",
            "write the notes file",
            "--allowed-tools",
            "write",
            "--output-format",
            "jsonl",
        ],
        fixture,
    );
    assert!(
        out.status.success(),
        "the scripted write turn must succeed -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let preview = tool_call_finished_preview(&jsonl_lines(&out.stdout), "write")
        .expect("the run must have actually called the real `write` tool");
    assert!(
        preview.starts_with("wrote "),
        "the real WriteTool::invoke must have produced this reply, got: {preview}"
    );
    sole_session_id(fixture)
}

/// The mock script both halves of a `write` turn need: the tool call, then
/// the follow-up text turn the model takes once it sees the result.
fn write_script() -> Script {
    Script(vec![
        vec![
            Chunk::ToolCall {
                name: "write",
                args: serde_json::json!({ "path": WRITTEN_PATH, "content": WRITTEN_CONTENT }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("wrote it"), Chunk::Finish("stop")],
    ])
}

// -----------------------------------------------------------------------
// `conway sessions show <id> --diff`
// -----------------------------------------------------------------------

/// **The first half of the post-kill answer, end to end.** A separate,
/// later process asks what a finished session did to the tree, by id, and
/// gets the path and the real content back -- reconstructed from that
/// session's own log, with no `git` anywhere in the pipeline (which is the
/// point: `--diff` describes what conway did, not what the tree looks like,
/// and those diverge exactly when the operator has uncommitted edits of
/// their own).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_show_diff_names_what_an_earlier_process_wrote() {
    let mock = MockBackend::start(write_script()).await;
    let fixture = common::write_fixture(&mock, 10);
    let sid = a_worker_that_wrote_a_file(&fixture, &["conway.checkpoint"]);

    let out = run_conway(&["sessions", "show", &sid, "--diff"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains(&format!("## {WRITTEN_PATH}")),
        "the diff must be headed by the path the session actually wrote: {stdout}"
    );
    assert!(
        stdout.contains("+worker line one") && stdout.contains("+worker line two"),
        "a created file's whole content must show as added lines: {stdout}"
    );
    assert!(
        !stdout.contains("no files edited or written yet"),
        "the empty-state line must NOT appear for a session that wrote a file: {stdout}"
    );
}

/// The honest empty state, which is a real answer and not an absence of
/// one: a session that took a turn but touched no file says so, rather than
/// printing nothing and leaving the operator to guess whether the command
/// worked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_that_touched_nothing_says_so_rather_than_printing_nothing() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("nothing to do"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = common::write_fixture(&mock, 10);

    let first = run_conway(&["-p", "say hello"], &fixture);
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let sid = sole_session_id(&fixture);

    let out = run_conway(&["sessions", "show", &sid, "--diff"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no files edited or written yet"),
        "a touch-free session must print the empty-state line: {stdout}"
    );
}

// -----------------------------------------------------------------------
// `conway conway.checkpoint.list --session <id>`
// -----------------------------------------------------------------------

/// **The second half, and the one the change exists for.** The same
/// invocation, run two ways against the same fixture: targeted at the
/// session that did the work, it reads THAT session's snapshot store;
/// untargeted, it mints a session for the duration of the command and
/// truthfully reports that session's emptiness -- naming the id it is
/// answering for, so "no snapshots" can no longer be misread as "there were
/// no snapshots".
///
/// Both halves live in one test on purpose: the contrast between them is
/// the property, and splitting it across two tests would let either half
/// pass while the pair said nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_list_reads_the_named_sessions_store_and_says_whose_emptiness_it_reports() {
    let mock = MockBackend::start(write_script()).await;
    let fixture = common::write_fixture(&mock, 10);
    let sid = a_worker_that_wrote_a_file(&fixture, &["conway.checkpoint"]);

    // Targeted: the snapshot the observer recorded during the write turn,
    // read back from a wholly separate process.
    let targeted = run_conway(&["conway.checkpoint.list", "--session", &sid], &fixture);
    assert!(
        targeted.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&targeted.stderr)
    );
    let targeted_stdout = String::from_utf8_lossy(&targeted.stdout);
    assert!(
        targeted_stdout.contains(WRITTEN_PATH),
        "the targeted listing must name the path that session wrote: {targeted_stdout}"
    );
    assert!(
        targeted_stdout.contains("Write"),
        "the targeted listing must name the kind of touch it recorded: {targeted_stdout}"
    );
    assert!(
        !targeted_stdout.contains("no snapshots recorded yet"),
        "the targeted listing must not be the empty one: {targeted_stdout}"
    );

    // Untargeted: the pre-flag behavior, now self-describing rather than
    // merely true.
    let bare = run_conway(&["conway.checkpoint.list"], &fixture);
    assert!(
        bare.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&bare.stderr)
    );
    let bare_stdout = String::from_utf8_lossy(&bare.stdout);
    assert!(
        bare_stdout.contains("no snapshots recorded yet for session "),
        "the untargeted listing must name the session it is answering for: {bare_stdout}"
    );
    assert!(
        !bare_stdout.contains(&sid),
        "the untargeted listing answers for a session conway minted, NOT the one that did the \
         work ({sid}): {bare_stdout}"
    );
    assert!(
        bare_stdout.contains("--session"),
        "the untargeted listing must name the flag that addresses a different session: \
         {bare_stdout}"
    );
}

/// **Never creates.** An id nothing answers to is a usage error naming the
/// value, not a silently-minted empty session reported as that id's own
/// record -- the whole failure this flag exists to end, and the one place
/// its semantics deliberately differ from the ROOT `--session` flag's
/// create-if-new behavior.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unresolvable_session_is_a_usage_error_not_a_fresh_empty_one() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.checkpoint"]);

    let missing = conway::SessionId::new().to_string();
    let out = run_conway(&["conway.checkpoint.list", "--session", &missing], &fixture);

    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&missing),
        "the error must name the value that did not resolve: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("no snapshots recorded yet"),
        "an unresolvable target must be refused outright, never answered for by a session conway \
         minted to stand in for it: {stdout}"
    );
}

/// The position rule, at the shell rather than at the unit level. A plugin
/// command's arguments are free text, so `--session` anywhere but the front
/// is a named error -- never silent free text a command would then misread
/// (`/conway.checkpoint.rollback 42 --session <id>` would take `--session`
/// as a path to restore).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_session_flag_is_refused_by_name() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.checkpoint"]);

    let out = run_conway(
        &["conway.checkpoint.list", "42", "--session", "whatever"],
        &fixture,
    );

    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("must come FIRST"),
        "the error must state the rule it is enforcing: {stderr}"
    );
}

// -----------------------------------------------------------------------
// The ROOT `--session` flag, threaded to this dispatch
// -----------------------------------------------------------------------

/// **The trap that existed at the shell, closed.** clap accepts `--session`
/// before ANY subcommand word, so `conway --session <id> <plugin>.<cmd>`
/// has always parsed -- and until `main.rs`'s `Command::External` arm
/// forwarded `cli.session`, that value stopped there: the command ran
/// against a freshly-minted empty session while the operator was looking at
/// the id they had just typed. Two spellings, one of them silently wrong.
///
/// Asserts byte equality with the post-subcommand spelling rather than
/// merely "both mention the path": these are the same flag, so the same
/// invocation must produce the same listing, and a weaker assertion would
/// pass even if the root spelling resolved to some other non-empty session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_root_session_flag_targets_the_same_session_as_the_post_subcommand_one() {
    let mock = MockBackend::start(write_script()).await;
    let fixture = common::write_fixture(&mock, 10);
    let sid = a_worker_that_wrote_a_file(&fixture, &["conway.checkpoint"]);

    let post = run_conway(&["conway.checkpoint.list", "--session", &sid], &fixture);
    assert!(
        post.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&post.stderr)
    );

    let root = run_conway(&["--session", &sid, "conway.checkpoint.list"], &fixture);
    assert!(
        root.status.success(),
        "the root --session spelling must reach the plugin dispatch -- stderr: {}",
        String::from_utf8_lossy(&root.stderr)
    );

    let root_stdout = String::from_utf8_lossy(&root.stdout);
    assert!(
        root_stdout.contains(WRITTEN_PATH),
        "the root spelling must read the NAMED session's store, not a minted empty one: \
         {root_stdout}"
    );
    assert!(
        !root_stdout.contains("no snapshots recorded yet"),
        "the root spelling must not fall through to a minted empty session: {root_stdout}"
    );
    assert_eq!(
        String::from_utf8_lossy(&post.stdout),
        root_stdout,
        "the two spellings are the same flag and must produce the same listing"
    );
}

/// Both spellings at once is a usage error naming both values, not a
/// precedence rule. They are equals; silently preferring either would act
/// on a session the operator also named and did not get -- on a surface
/// that reaches `/conway.checkpoint.rollback`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn giving_both_session_spellings_at_once_is_a_usage_error() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.checkpoint"]);

    let before = conway::SessionId::new().to_string();
    let after = conway::SessionId::new().to_string();
    let out = run_conway(
        &[
            "--session",
            &before,
            "conway.checkpoint.list",
            "--session",
            &after,
        ],
        &fixture,
    );

    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("given twice"),
        "the error must say the flag was given twice: {stderr}"
    );
    assert!(
        stderr.contains(&before) && stderr.contains(&after),
        "the error must quote BOTH values so the operator can see what conway refused to choose \
         between: {stderr}"
    );
}
