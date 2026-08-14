//! Integration coverage for `BashTool` and the `ShellPlugin` assembly
//! (criteria).
//!
//! Requires the `test-fakes` feature (for `conway_tools::testing::test_ctx`).
//! Declared with `required-features = ["test-fakes"]` in Cargo.toml, so a
//! plain `cargo test -p conway-tools` skips (not fails) this file.
//!
//! The non-unix `invoke` path (`bash tool requires a unix host`) is not
//! exercised here: this suite only ever runs on a unix host (macOS/Linux
//! CI), and `#[cfg(not(unix))]` code is stripped before type-checking on
//! such a host, so it cannot be driven from a test in this file. Its only
//! verifiable property on this host is that the crate compiles
//! (`cargo check -p conway-tools`).

#![cfg(feature = "test-fakes")]

use std::path::PathBuf;
use std::time::Duration;

use conway_core::content::{ContentBlock, ToolCall, ToolCategory, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::event::Event;
use conway_core::ids::ToolName;
use conway_core::ports::{Plugin, Tool, ToolOutput};
use conway_tools::shell::{BashTool, ShellPlugin};
use conway_tools::testing::test_ctx;
use tempfile::TempDir;

fn call(arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "tc_1".into(),
        name: ToolName::new("bash"),
        arguments,
    }
}

fn text_of(out: &ToolOutput) -> &str {
    match &out.blocks[0] {
        ContentBlock::Text { text } => text,
        other => panic!("expected a text block, got {other:?}"),
    }
}

/// The exact `stdout:` body extracted from a `BashTool` output, per the
/// fixed `stdout:\n..\n\nstderr:\n..\n\nexit code: N` rendering.
fn stdout_section(text: &str) -> &str {
    let after = text.strip_prefix("stdout:\n").expect("stdout section");
    let end = after.find("\n\nstderr:").expect("stderr section");
    &after[..end]
}

/// Finds the first `ToolProgress` note that parses as an integer — used by
/// the process-group tests, which prefix their command with `echo $$` to
/// capture bash's own pid (== pgid, since the child is its own group
/// leader) for a post-invoke liveness check.
fn find_pgid(events: &[Event]) -> i32 {
    events
        .iter()
        .find_map(|e| match e {
            Event::ToolProgress { note, .. } => note.trim().parse::<i32>().ok(),
            _ => None,
        })
        .expect("expected a numeric pgid line among ToolProgress events")
}

/// Asserts `kill(-pgid, 0)` cannot reach our spawned group any more. This is
/// `ESRCH` in the overwhelming common case (the pgid is simply gone). Under
/// the PID churn `cargo test`'s default parallelism creates, the kernel can
/// occasionally recycle the freed pgid for an unrelated process before this
/// check runs, which surfaces as `EPERM` instead — still proof our group is
/// dead (had it still been alive, we own it and `kill` would return `Ok`).
///
/// POLLS rather than checking once, because group teardown is ASYNCHRONOUS
/// and this assertion is not. `kill_group` signals the whole group and then
/// `child.wait()`s, but that reaps only the direct child — the bash shell.
/// A backgrounded `sleep 300 &` is a GRANDchild: it receives the same
/// group-directed SIGKILL, but when its parent shell dies it is reparented
/// to init and stays a ZOMBIE until init reaps it. **A zombie still answers
/// `kill(pid, 0)` with success**, so a single immediate check can observe
/// `Ok` for a group whose members are already dead — which is not what this
/// test means to catch.
///
/// That is not hypothetical: this checked once and passed on macOS while
/// failing deterministically on Linux CI (two consecutive runs, GitHub
/// Actions run 31129229843), because the reparent-and-reap timing differs.
/// It was the first defect the workspace `cargo test` CI job ever caught,
/// and it had never run on Linux before that job existed.
///
/// The poll is what makes the assertion honest, not a way to make a red test
/// green: if the group genuinely SURVIVED — a real orphaned-process bug —
/// polling cannot rescue it, because a live `sleep 300` outlives this window
/// by minutes and every attempt returns `Ok`. Finding
///.
#[cfg(unix)]
fn assert_group_dead(pgid: i32) {
    // Generous relative to reaping (milliseconds) and tiny relative to the
    // 300s sleeps this test spawns, so it cannot mask a surviving group.
    const DEADLINE: Duration = Duration::from_secs(2);
    const POLL: Duration = Duration::from_millis(20);

    let start = std::time::Instant::now();
    loop {
        let result = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(-pgid),
            None::<nix::sys::signal::Signal>,
        );
        if result.is_err() {
            return;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "process group {pgid} should be dead, but kill(-pgid, 0) still \
             returned Ok after {DEADLINE:?} of polling — the group survived \
             cancellation rather than merely awaiting reaping"
        );
        std::thread::sleep(POLL);
    }
}

// ----------------------------------------------------------- identity ---

#[test]
fn shell_plugin_manifest_and_tools() {
    let plugin = ShellPlugin::new();
    let manifest = plugin.manifest();
    assert_eq!(manifest.id, "conway.shell");

    let names: Vec<String> = plugin
        .tools()
        .iter()
        .map(|t| t.spec().name.as_str().to_string())
        .collect();
    assert_eq!(names, vec!["bash"]);
}

#[test]
fn bash_tool_name_and_category() {
    let spec = BashTool::new().spec();
    assert_eq!(spec.name.as_str(), "bash");
    assert_eq!(spec.category, ToolCategory::Execute);
}

// -------------------------------------------------------------- basic ---

#[tokio::test]
async fn echo_succeeds_with_exit_code_zero() {
    let (ctx, _h) = test_ctx(PathBuf::from("/tmp"));
    let out = BashTool::new()
        .invoke(call(serde_json::json!({"command": "echo hi"})), ctx)
        .await
        .unwrap();
    assert!(!out.is_error);
    let text = text_of(&out);
    assert!(text.contains("hi"), "text was {text:?}");
    assert!(text.contains("exit code: 0"), "text was {text:?}");
}

#[tokio::test]
async fn mixed_stdout_stderr_nonzero_exit_is_sectioned_and_is_error() {
    let (ctx, _h) = test_ctx(PathBuf::from("/tmp"));
    let out = BashTool::new()
        .invoke(
            call(serde_json::json!({"command": "echo out; echo err 1>&2; exit 3"})),
            ctx,
        )
        .await
        .unwrap();
    assert!(out.is_error);
    let text = text_of(&out);
    assert!(text.contains("exit code: 3"), "text was {text:?}");

    let stdout_idx = text.find("stdout:").unwrap();
    let stderr_idx = text.find("stderr:").unwrap();
    let out_idx = text.find("out").unwrap();
    let err_idx = text.find("err").unwrap();
    assert!(stdout_idx < out_idx && out_idx < stderr_idx);
    assert!(stderr_idx < err_idx);
}

#[tokio::test]
async fn truncation_policy_is_head_tail_thirty_thousand_bytes_total() {
    let (ctx, _h) = test_ctx(PathBuf::from("/tmp"));
    let out = BashTool::new()
        .invoke(call(serde_json::json!({"command": "echo hi"})), ctx)
        .await
        .unwrap();
    assert_eq!(
        out.truncation,
        TruncationPolicy::HeadTail {
            head_bytes: 15_000,
            tail_bytes: 15_000,
        }
    );
}

// ----------------------------------------------------------- streaming ---

#[tokio::test]
async fn streams_each_line_as_tool_progress_in_order() {
    let (ctx, handles) = test_ctx(PathBuf::from("/tmp"));
    let call_id = "tc_stream";
    let out = BashTool::new()
        .invoke(
            ToolCall {
                call_id: call_id.into(),
                name: ToolName::new("bash"),
                arguments: serde_json::json!({"command": r"printf 'a\nb\nc\n'"}),
            },
            ctx,
        )
        .await
        .unwrap();
    assert!(!out.is_error);

    let notes: Vec<String> = handles
        .events
        .events()
        .into_iter()
        .filter_map(|e| match e {
            Event::ToolProgress { call_id: cid, note } if cid == call_id => Some(note),
            _ => None,
        })
        .collect();
    assert!(notes.len() >= 3, "notes were {notes:?}");
    assert_eq!(
        &notes[..3],
        &["a".to_string(), "b".to_string(), "c".to_string()]
    );
}

// ------------------------------------------------------------ cwd ---

#[tokio::test]
async fn cwd_argument_overrides_ctx_cwd() {
    let dir = TempDir::new().unwrap();
    let expected = dir.path().canonicalize().unwrap();
    let (ctx, _h) = test_ctx(PathBuf::from("/tmp"));
    let out = BashTool::new()
        .invoke(
            call(serde_json::json!({"command": "pwd", "cwd": dir.path().to_str().unwrap()})),
            ctx,
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    let text = text_of(&out);
    let pwd_out = PathBuf::from(stdout_section(text).trim())
        .canonicalize()
        .unwrap();
    assert_eq!(pwd_out, expected);
}

#[tokio::test]
async fn absent_cwd_runs_in_ctx_cwd() {
    let dir = TempDir::new().unwrap();
    let expected = dir.path().canonicalize().unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());
    let out = BashTool::new()
        .invoke(call(serde_json::json!({"command": "pwd"})), ctx)
        .await
        .unwrap();
    assert!(!out.is_error);
    let text = text_of(&out);
    let pwd_out = PathBuf::from(stdout_section(text).trim())
        .canonicalize()
        .unwrap();
    assert_eq!(pwd_out, expected);
}

// ------------------------------------------------------ cancel + group ---

#[cfg(unix)]
#[tokio::test]
async fn cancel_kills_the_whole_process_group_including_backgrounded_child() {
    let (ctx, handles) = test_ctx(PathBuf::from("/tmp"));
    let cancel = handles.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
    });

    let result = tokio::time::timeout(
        Duration::from_secs(3),
        BashTool::new().invoke(
            call(serde_json::json!({"command": "echo $$; sleep 300 & sleep 300"})),
            ctx,
        ),
    )
    .await
    .expect("invoke should return within 3s");

    assert!(matches!(result, Err(ToolError::Cancelled)));

    let pgid = find_pgid(&handles.events.events());
    assert_group_dead(pgid);
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_the_process_group_and_reports_is_error() {
    let (ctx, handles) = test_ctx(PathBuf::from("/tmp"));

    let result = tokio::time::timeout(
        Duration::from_secs(3),
        BashTool::new().invoke(
            call(serde_json::json!({"command": "echo $$; sleep 5", "timeout_ms": 300})),
            ctx,
        ),
    )
    .await
    .expect("invoke should return within 3s");

    let out = result.expect("timeout is a recoverable Ok(is_error: true), not an Err");
    assert!(out.is_error);
    let text = text_of(&out);
    assert!(text.contains("timed out after 300ms"), "text was {text:?}");

    let pgid = find_pgid(&handles.events.events());
    assert_group_dead(pgid);
}
