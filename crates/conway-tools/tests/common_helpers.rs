//! Integration coverage for `common::{resolve_path, parse_args,
//! check_cancel}` against a real `tempfile::TempDir`.
//!
//! Requires the `testing` feature (for `conway_tools::testing::test_ctx`):
//! run as `cargo test -p conway-tools --features testing`. Declared with
//! `required-features = ["test-fakes"]` in Cargo.toml, so a plain `cargo test
//! -p conway-tools` skips (not fails) this file.

#![cfg(feature = "test-fakes")]

use std::path::PathBuf;

use conway_core::content::ToolCall;
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_tools::common::{check_cancel, parse_args, resolve_path};
use conway_tools::testing::test_ctx;
use tempfile::TempDir;

#[derive(Debug, serde::Deserialize)]
struct ReadArgs {
    path: String,
    #[allow(dead_code)]
    #[serde(default)]
    offset: Option<u32>,
}

fn call(arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "tc_1".into(),
        name: ToolName::new("test"),
        arguments,
    }
}

#[test]
fn resolve_path_against_real_tempdir() {
    let dir = TempDir::new().unwrap();
    let (ctx, _handles) = test_ctx(dir.path().to_path_buf());

    // Relative path joins onto the tempdir.
    let resolved = resolve_path(&ctx, "nested/file.txt").unwrap();
    assert_eq!(resolved, dir.path().join("nested/file.txt"));

    // Absolute path passes through unchanged even though it's outside the
    // tempdir (resolve_path performs no containment checks, GP-08).
    let abs = resolve_path(&ctx, "/etc/hosts").unwrap();
    assert_eq!(abs, PathBuf::from("/etc/hosts"));

    // NUL byte is rejected.
    let err = resolve_path(&ctx, "bad\0path").unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments { .. }));
}

#[test]
fn resolve_path_target_can_be_written_and_read_back() {
    let dir = TempDir::new().unwrap();
    let (ctx, _handles) = test_ctx(dir.path().to_path_buf());

    let target = resolve_path(&ctx, "sub/out.txt").unwrap();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, b"hello").unwrap();

    let read_back = std::fs::read_to_string(&target).unwrap();
    assert_eq!(read_back, "hello");
}

#[test]
fn parse_args_round_trips_a_well_formed_call() {
    let tool_call = call(serde_json::json!({"path": "a.txt", "offset": 3}));
    let args: ReadArgs = parse_args(&tool_call).unwrap();
    assert_eq!(args.path, "a.txt");
    assert_eq!(args.offset, Some(3));
}

#[test]
fn parse_args_reports_serde_error_text_on_malformed_call() {
    // `path` is required; omit it entirely.
    let tool_call = call(serde_json::json!({"offset": 3}));
    let err = parse_args::<ReadArgs>(&tool_call).unwrap_err();
    match err {
        ToolError::InvalidArguments { detail } => {
            assert!(detail.contains("path"), "detail was {detail:?}");
        }
        other => panic!("expected InvalidArguments, got {other:?}"),
    }
}

#[test]
fn check_cancel_reflects_live_cancellation_state() {
    let dir = TempDir::new().unwrap();
    let (ctx, handles) = test_ctx(dir.path().to_path_buf());

    assert!(check_cancel(&ctx).is_ok());

    handles.cancel.cancel();

    assert!(matches!(check_cancel(&ctx), Err(ToolError::Cancelled)));
}
