//! `WriteTool`: the `write` tool — atomic whole-file replacement via a
//! sibling temp file plus rename.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use conway_core::content::{PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{PathArgs, RenderKind, Tool, ToolCtx, ToolOutput};

use crate::common::{check_cancel, parse_args, resolve_path, text_output};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}

/// Writes `content` to `path`, creating parent directories as needed and
/// replacing any existing content, atomically (temp file + rename).
#[derive(Debug, Default)]
pub struct WriteTool;

impl WriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WriteTool {
    /// `WriteArgs::path` is the only path argument (`content` is file data,
    /// not a path). Note this is the case that forbids a plain
    /// `fs::canonicalize` containment check: a write target legitimately
    /// does not exist yet.
    fn path_args(&self) -> PathArgs {
        PathArgs::Named(&["path"])
    }

    /// `write` never overrides `render`, so its rendering is always the
    /// trait's own default JSON dump -- never a shell command.
    ///.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("write"),
            description: "Write a file's contents, replacing it if it exists".into(),
            schema: schemars::schema_for!(WriteArgs),
            category: ToolCategory::Edit,
            permission: PermissionClass::RequiresApproval,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: WriteArgs = parse_args(&call)?;
        let path = resolve_path(&ctx, &args.path)?;

        let bytes = atomic_write(&path, &args.content).await?;

        Ok(text_output(
            format!("wrote {bytes} bytes to {}", path.display()),
            TruncationPolicy::None,
        ))
    }
}

/// Atomically replaces `path`'s contents with `content`: creates parent
/// directories, writes to a sibling `.{filename}.conway.tmp`, `flush`s and
/// `sync_all`s it, then renames it over `path`. On any failure after the
/// temp file is created, the temp file is removed (best effort) before the
/// error is returned. Shared by `write` and `edit` (both need atomic
/// whole-file replacement). Returns the number of bytes written.
pub(crate) async fn atomic_write(path: &Path, content: &str) -> Result<u64, ToolError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|err| ToolError::Io {
            detail: format!(
                "failed to create parent directories for {}: {err}",
                path.display()
            ),
        })?;

    let tmp_path = tmp_sibling(path);

    let write_result: Result<u64, std::io::Error> = async {
        let mut file = tokio::fs::File::create(&tmp_path).await?;
        file.write_all(content.as_bytes()).await?;
        file.flush().await?;
        file.sync_all().await?;
        Ok(content.len() as u64)
    }
    .await;

    let bytes = match write_result {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(ToolError::Io {
                detail: format!("failed to write {}: {err}", path.display()),
            });
        }
    };

    if let Err(err) = tokio::fs::rename(&tmp_path, path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(ToolError::Io {
            detail: format!("failed to finalize write to {}: {err}", path.display()),
        });
    }

    Ok(bytes)
}

/// The atomic-write temp sibling for `path`:
/// `<parent>/.<filename>.<pid>.<n>.conway.tmp`.
///
/// The pid + per-process counter suffix makes concurrent writers to the
/// same target path use distinct temp inodes, so neither can truncate the
/// other's in-flight content — the last `rename` wins whole-file
/// (incremental review S1, cycle 1). Same-directory placement keeps the
/// final `rename` on one filesystem (atomic, no cross-device copy).
fn tmp_sibling(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    parent.join(format!(
        ".{filename}.{}.{}.conway.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_ctx;
    use tempfile::TempDir;

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "tc_1".into(),
            name: ToolName::new("write"),
            arguments,
        }
    }

    #[test]
    fn spec_has_expected_name_category_permission() {
        let spec = WriteTool::new().spec();
        assert_eq!(spec.name.as_str(), "write");
        assert_eq!(spec.category, ToolCategory::Edit);
        assert_eq!(spec.permission, PermissionClass::RequiresApproval);
    }

    #[test]
    fn schema_required_and_properties() {
        let spec = WriteTool::new().spec();
        let json = serde_json::to_value(&spec.schema).unwrap();
        let mut required: Vec<&str> = json["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        required.sort();
        assert_eq!(required, vec!["content", "path"]);
        assert_eq!(json["additionalProperties"], false);
    }

    #[tokio::test]
    async fn invoke_creates_parents_and_writes() {
        let dir = TempDir::new().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let out = WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "dir/does/not/exist/f.txt", "content": "hello"})),
                ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        let target = dir.path().join("dir/does/not/exist/f.txt");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
    }

    #[tokio::test]
    async fn invoke_second_write_replaces_content() {
        let dir = TempDir::new().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "content": "first"})),
                ctx.clone(),
            )
            .await
            .unwrap();
        WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "content": "second, longer"})),
                ctx,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "second, longer"
        );
    }

    #[tokio::test]
    async fn invoke_no_tmp_sibling_remains_after_success() {
        let dir = TempDir::new().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "content": "hi"})),
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

    #[tokio::test]
    async fn invoke_pre_cancelled_returns_cancelled_without_touching_fs() {
        let dir = TempDir::new().unwrap();
        let (ctx, handles) = test_ctx(dir.path().to_path_buf());
        handles.cancel.cancel();
        let err = WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "content": "hi"})),
                ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Cancelled));
        assert!(!dir.path().join("f.txt").exists());
    }

    /// Driven through this tool's
    /// production `invoke` entry point, not `resolve_path` in isolation.
    #[tokio::test]
    async fn invoke_rejects_nul_byte_in_path() {
        let dir = TempDir::new().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let err = WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "a\0b", "content": "hi"})),
                ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }
}
