//! Integration coverage for `FsPlugin`'s core file tools (`read`, `write`,
//! `edit`) against a real `tempfile::TempDir` (criteria).
//!
//! Requires the `test-fakes` feature (for `conway_tools::testing::test_ctx`).
//! Declared with `required-features = ["test-fakes"]` in Cargo.toml, so a
//! plain `cargo test -p conway-tools` skips (not fails) this file.

#![cfg(feature = "test-fakes")]

use conway_core::content::{ContentBlock, ToolCall, ToolCategory, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::Tool;
use conway_tools::fs::{EditTool, ReadTool, WriteTool};
use conway_tools::testing::test_ctx;
use tempfile::TempDir;

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "tc_1".into(),
        name: ToolName::new(name),
        arguments,
    }
}

fn text_of(out: &conway_core::ports::ToolOutput) -> &str {
    match &out.blocks[0] {
        ContentBlock::Text { text } => text,
        other => panic!("expected a text block, got {other:?}"),
    }
}

fn required_and_props(spec: &conway_core::content::ToolSpec) -> (Vec<String>, Vec<String>) {
    let json = serde_json::to_value(&spec.schema).unwrap();
    let mut required: Vec<String> = json["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    required.sort();
    let mut props: Vec<String> = json["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    props.sort();
    (required, props)
}

// ---------------------------------------------------------------- schemas --

#[test]
fn schemas_have_documented_required_and_properties() {
    let (read_required, read_props) = required_and_props(&ReadTool::new().spec());
    assert_eq!(read_required, vec!["path"]);
    assert_eq!(read_props, vec!["limit", "offset", "path"]);

    let (write_required, write_props) = required_and_props(&WriteTool::new().spec());
    assert_eq!(write_required, vec!["content", "path"]);
    assert_eq!(write_props, vec!["content", "path"]);

    let (edit_required, edit_props) = required_and_props(&EditTool::new().spec());
    assert_eq!(edit_required, vec!["new_string", "old_string", "path"]);
    assert_eq!(
        edit_props,
        vec!["new_string", "old_string", "path", "replace_all"]
    );
}

#[test]
fn tool_names_and_categories_are_as_specified() {
    let read_spec = ReadTool::new().spec();
    assert_eq!(read_spec.name.as_str(), "read");
    assert_eq!(read_spec.category, ToolCategory::Read);

    let write_spec = WriteTool::new().spec();
    assert_eq!(write_spec.name.as_str(), "write");
    assert_eq!(write_spec.category, ToolCategory::Edit);

    let edit_spec = EditTool::new().spec();
    assert_eq!(edit_spec.name.as_str(), "edit");
    assert_eq!(edit_spec.category, ToolCategory::Edit);
}

// ---------------------------------------------------------------------- read

/// S2 regression (cycle 1): host-level read failures are Err(ToolError::Io),
/// not model-recoverable is_error output — matching edit.rs's discipline.
#[cfg(unix)]
#[tokio::test]
async fn read_permission_denied_is_a_host_error() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locked.txt");
    std::fs::write(&path, "secret").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let (ctx, _handles) = test_ctx(dir.path().to_path_buf());
    let result = ReadTool
        .invoke(call("read", serde_json::json!({"path": path})), ctx)
        .await;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        matches!(result, Err(conway_core::error::ToolError::Io { .. })),
        "expected Err(Io), got {result:?}"
    );
}

/// S1 regression (cycle 1): concurrent writers to the same target use
/// distinct temp inodes — the surviving file is one writer's payload in
/// full, never interleaved bytes.
#[tokio::test]
async fn concurrent_writes_to_one_target_never_interleave() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("contended.txt");
    let payload_a = "A".repeat(64 * 1024);
    let payload_b = "B".repeat(64 * 1024);

    for _ in 0..20 {
        let (ctx_a, _ha) = test_ctx(dir.path().to_path_buf());
        let (ctx_b, _hb) = test_ctx(dir.path().to_path_buf());
        let (a, b) = tokio::join!(
            WriteTool.invoke(
                call(
                    "write",
                    serde_json::json!({"path": target, "content": payload_a})
                ),
                ctx_a
            ),
            WriteTool.invoke(
                call(
                    "write",
                    serde_json::json!({"path": target, "content": payload_b})
                ),
                ctx_b
            ),
        );
        assert!(a.is_ok() && b.is_ok());
        let survivor = std::fs::read_to_string(&target).unwrap();
        assert!(
            survivor == payload_a || survivor == payload_b,
            "interleaved content detected ({} bytes, first char {:?})",
            survivor.len(),
            survivor.chars().next()
        );
    }
    // No temp siblings linger.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".conway.tmp"))
        .collect();
    assert!(leftovers.is_empty(), "lingering temp files: {leftovers:?}");
}

#[tokio::test]
async fn read_five_line_file_is_cat_n_formatted() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\nb\nc\nd\ne\n").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = ReadTool::new()
        .invoke(call("read", serde_json::json!({"path": "f.txt"})), ctx)
        .await
        .unwrap();

    assert!(!out.is_error);
    assert_eq!(
        text_of(&out),
        "     1\ta\n     2\tb\n     3\tc\n     4\td\n     5\te"
    );
    assert_eq!(out.truncation, TruncationPolicy::Head { max_bytes: 65_536 });
}

#[tokio::test]
async fn read_offset_and_limit_yield_only_the_requested_window() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\nb\nc\nd\ne\n").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = ReadTool::new()
        .invoke(
            call(
                "read",
                serde_json::json!({"path": "f.txt", "offset": 3, "limit": 2}),
            ),
            ctx,
        )
        .await
        .unwrap();

    assert!(!out.is_error);
    assert!(text_of(&out).contains("     3\tc"));
    assert!(text_of(&out).contains("     4\td"));
    assert!(!text_of(&out).contains("\te"));
    assert!(!text_of(&out).contains("     1\ta"));
}

#[tokio::test]
async fn read_nonexistent_path_is_a_recoverable_error_containing_the_path() {
    let dir = TempDir::new().unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = ReadTool::new()
        .invoke(
            call("read", serde_json::json!({"path": "missing.txt"})),
            ctx,
        )
        .await
        .unwrap();

    assert!(out.is_error);
    assert!(text_of(&out).contains("missing.txt"));
}

#[tokio::test]
async fn read_binary_file_is_rejected() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("bin.dat"), [b'a', 0u8, b'b']).unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = ReadTool::new()
        .invoke(call("read", serde_json::json!({"path": "bin.dat"})), ctx)
        .await
        .unwrap();

    assert!(out.is_error);
    assert_eq!(text_of(&out), "binary file; not read");
}

// --------------------------------------------------------------------- write

#[tokio::test]
async fn write_creates_parent_dirs_and_reports_bytes_written() {
    let dir = TempDir::new().unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = WriteTool::new()
        .invoke(
            call(
                "write",
                serde_json::json!({"path": "dir/does/not/exist/f.txt", "content": "hello"}),
            ),
            ctx,
        )
        .await
        .unwrap();

    assert!(!out.is_error);
    let target = dir.path().join("dir/does/not/exist/f.txt");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
    let text = text_of(&out);
    assert!(text.starts_with("wrote 5 bytes to "));
    assert!(text.contains(&target.display().to_string()));
}

#[tokio::test]
async fn write_second_call_replaces_content() {
    let dir = TempDir::new().unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    WriteTool::new()
        .invoke(
            call(
                "write",
                serde_json::json!({"path": "f.txt", "content": "first"}),
            ),
            ctx.clone(),
        )
        .await
        .unwrap();
    WriteTool::new()
        .invoke(
            call(
                "write",
                serde_json::json!({"path": "f.txt", "content": "second"}),
            ),
            ctx,
        )
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "second"
    );
}

#[tokio::test]
async fn write_leaves_no_tmp_sibling_after_success() {
    let dir = TempDir::new().unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    WriteTool::new()
        .invoke(
            call(
                "write",
                serde_json::json!({"path": "f.txt", "content": "hi"}),
            ),
            ctx,
        )
        .await
        .unwrap();

    let leftover: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".conway.tmp"))
        .collect();
    assert!(leftover.is_empty(), "leftover tmp files: {leftover:?}");
}

// ---------------------------------------------------------------------- edit

#[tokio::test]
async fn edit_unique_occurrence_replaces_and_preserves_the_rest_of_the_file() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("f.txt"), "before ONE after").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = EditTool::new()
        .invoke(
            call(
                "edit",
                serde_json::json!({"path": "f.txt", "old_string": "ONE", "new_string": "1"}),
            ),
            ctx,
        )
        .await
        .unwrap();

    assert!(!out.is_error);
    assert_eq!(
        std::fs::read(dir.path().join("f.txt")).unwrap(),
        b"before 1 after"
    );
}

#[tokio::test]
async fn edit_zero_occurrences_is_a_recoverable_error() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("f.txt"), "hello world").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = EditTool::new()
        .invoke(
            call(
                "edit",
                serde_json::json!({"path": "f.txt", "old_string": "missing", "new_string": "x"}),
            ),
            ctx,
        )
        .await
        .unwrap();

    assert!(out.is_error);
    assert!(text_of(&out).contains("old_string not found"));
}

#[tokio::test]
async fn edit_multiple_occurrences_without_replace_all_is_an_error() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("f.txt"), "dup dup rest").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = EditTool::new()
        .invoke(
            call(
                "edit",
                serde_json::json!({"path": "f.txt", "old_string": "dup", "new_string": "x"}),
            ),
            ctx,
        )
        .await
        .unwrap();

    assert!(out.is_error);
    assert!(text_of(&out).contains("found 2 occurrences"));
}

#[tokio::test]
async fn edit_replace_all_replaces_every_occurrence() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("f.txt"), "dup dup rest").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = EditTool::new()
        .invoke(
            call(
                "edit",
                serde_json::json!({
                    "path": "f.txt",
                    "old_string": "dup",
                    "new_string": "x",
                    "replace_all": true
                }),
            ),
            ctx,
        )
        .await
        .unwrap();

    assert!(!out.is_error);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "x x rest"
    );
    let target = dir.path().join("f.txt");
    assert_eq!(
        text_of(&out),
        format!("edited {}: 2 replacement(s)", target.display())
    );
}

#[tokio::test]
async fn edit_identical_old_and_new_is_an_error() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("f.txt"), "same").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = EditTool::new()
        .invoke(
            call(
                "edit",
                serde_json::json!({"path": "f.txt", "old_string": "same", "new_string": "same"}),
            ),
            ctx,
        )
        .await
        .unwrap();

    assert!(out.is_error);
    assert!(text_of(&out).contains("old_string and new_string are identical"));
}

// ------------------------------------------------------------- cancellation

#[tokio::test]
async fn pre_cancelled_ctx_short_circuits_every_tool_without_touching_the_filesystem() {
    let dir = TempDir::new().unwrap();

    let (ctx, handles) = test_ctx(dir.path().to_path_buf());
    handles.cancel.cancel();
    let err = ReadTool::new()
        .invoke(call("read", serde_json::json!({"path": "f.txt"})), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Cancelled));
    assert!(!dir.path().join("f.txt").exists());

    let (ctx, handles) = test_ctx(dir.path().to_path_buf());
    handles.cancel.cancel();
    let err = WriteTool::new()
        .invoke(
            call(
                "write",
                serde_json::json!({"path": "f.txt", "content": "hi"}),
            ),
            ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Cancelled));
    assert!(!dir.path().join("f.txt").exists());

    std::fs::write(dir.path().join("existing.txt"), "one two").unwrap();
    let (ctx, handles) = test_ctx(dir.path().to_path_buf());
    handles.cancel.cancel();
    let err = EditTool::new()
        .invoke(
            call(
                "edit",
                serde_json::json!({"path": "existing.txt", "old_string": "one", "new_string": "1"}),
            ),
            ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Cancelled));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("existing.txt")).unwrap(),
        "one two"
    );
}
