//! `EditTool`: the `edit` tool — literal, byte-exact substring replacement.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::content::{PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{PathArgs, RenderKind, Tool, ToolCtx, ToolOutput};

use crate::common::{check_cancel, error_text, parse_args, resolve_path, text_output};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

/// Replaces `old_string` with `new_string` in a file. Matching is literal
/// byte-exact substring matching — never regex, never whitespace-normalized.
/// `old_string` must match exactly once unless `replace_all` is set.
#[derive(Debug, Default)]
pub struct EditTool;

impl EditTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for EditTool {
    /// `EditArgs::path` is the only path argument -- `old_string`/
    /// `new_string` are file content and `replace_all` is a flag.
    fn path_args(&self) -> PathArgs {
        PathArgs::Named(&["path"])
    }

    /// `edit` never overrides `render`, so its rendering is always the
    /// trait's own default JSON dump -- never a shell command.
    ///.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("edit"),
            description: "Replace an exact substring in a file".into(),
            schema: schemars::schema_for!(EditArgs),
            category: ToolCategory::Edit,
            permission: PermissionClass::RequiresApproval,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: EditArgs = parse_args(&call)?;
        let path = resolve_path(&ctx, &args.path)?;

        if args.old_string == args.new_string {
            return Ok(error_text(format!(
                "old_string and new_string are identical in {}",
                path.display()
            )));
        }

        // `[S1.5]`/(retirement): `edit` gained
        // NO harness-level root check before this item, at any point --
        // `check_root` was only ever wired into `read`/`write`/`cd`. Under
        // the (now-retired) harness pre-gate this was masked: `edit` still
        // declares `PathArgs::Named(&["path"])`, so `PermissionBroker::
        // check_root` confined it anyway, from OUTSIDE this file, before
        // `invoke` ever ran. Retiring that pre-gate would have made `edit`
        // silently unconfined were this call not added here -- see this
        // item's own report for the full "what did the harness cover that
        // conway.fs did not" accounting. `edit`'s write half is confined the
        // same way, at its own `atomic_write`/`beneath::write_file_atomic`
        // call site below.
        let bytes = match crate::fs::beneath::read_file(&ctx, &path).await? {
            crate::fs::beneath::ReadOutcome::Bytes(bytes) => bytes,
            crate::fs::beneath::ReadOutcome::NotFound => {
                return Ok(error_text(format!("file not found: {}", path.display())));
            }
        };

        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                return Ok(error_text(format!(
                    "file is not valid UTF-8: {}",
                    path.display()
                )));
            }
        };

        check_cancel(&ctx)?;

        let count = content.matches(args.old_string.as_str()).count();
        if count == 0 {
            return Ok(error_text(format!(
                "old_string not found in {}",
                path.display()
            )));
        }
        if count > 1 && !args.replace_all {
            return Ok(error_text(format!(
                "found {count} occurrences of old_string in {}; add surrounding context to make it unique, or set replace_all: true",
                path.display()
            )));
        }

        let (new_content, replacements) = if args.replace_all {
            (content.replace(&args.old_string, &args.new_string), count)
        } else {
            (content.replacen(&args.old_string, &args.new_string, 1), 1)
        };

        crate::fs::beneath::write_file_atomic(&ctx, &path, &new_content).await?;

        Ok(text_output(
            format!("edited {}: {replacements} replacement(s)", path.display()),
            TruncationPolicy::None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_ctx;
    use tempfile::TempDir;

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "tc_1".into(),
            name: ToolName::new("edit"),
            arguments,
        }
    }

    #[test]
    fn spec_has_expected_name_category_permission() {
        let spec = EditTool::new().spec();
        assert_eq!(spec.name.as_str(), "edit");
        assert_eq!(spec.category, ToolCategory::Edit);
        assert_eq!(spec.permission, PermissionClass::RequiresApproval);
    }

    #[test]
    fn schema_required_and_properties() {
        let spec = EditTool::new().spec();
        let json = serde_json::to_value(&spec.schema).unwrap();
        let mut required: Vec<&str> = json["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        required.sort();
        assert_eq!(required, vec!["new_string", "old_string", "path"]);
        assert_eq!(json["additionalProperties"], false);
    }

    #[tokio::test]
    async fn invoke_unique_occurrence_replaces_and_preserves_rest() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "one two three").unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let out = EditTool::new()
            .invoke(
                call(
                    serde_json::json!({"path": "f.txt", "old_string": "two", "new_string": "TWO"}),
                ),
                ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "one TWO three"
        );
    }

    #[tokio::test]
    async fn invoke_zero_occurrences_is_recoverable_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "one two three").unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let out = EditTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "old_string": "missing", "new_string": "x"})),
                ctx,
            )
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn invoke_multiple_without_replace_all_is_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "aa bb aa").unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let out = EditTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "old_string": "aa", "new_string": "x"})),
                ctx,
            )
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn invoke_replace_all_replaces_all() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "aa bb aa").unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let out = EditTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "old_string": "aa", "new_string": "x", "replace_all": true})),
                ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "x bb x"
        );
    }

    #[tokio::test]
    async fn invoke_identical_strings_is_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "same").unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let out = EditTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "old_string": "same", "new_string": "same"})),
                ctx,
            )
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn invoke_pre_cancelled_returns_cancelled_without_touching_fs() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "one two three").unwrap();
        let (ctx, handles) = test_ctx(dir.path().to_path_buf());
        handles.cancel.cancel();
        let err = EditTool::new()
            .invoke(
                call(
                    serde_json::json!({"path": "f.txt", "old_string": "two", "new_string": "TWO"}),
                ),
                ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Cancelled));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "one two three"
        );
    }

    /// Driven through this tool's
    /// production `invoke` entry point, not `resolve_path` in isolation.
    #[tokio::test]
    async fn invoke_rejects_nul_byte_in_path() {
        let dir = TempDir::new().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let err = EditTool::new()
            .invoke(
                call(serde_json::json!({"path": "a\0b", "old_string": "x", "new_string": "y"})),
                ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    /// (retirement): pins the gap this
    /// item's own report names -- before this item, `edit` was confined
    /// ONLY by the harness-level pre-gate check (`edit` performed no root
    /// check of its own at all). This proves `edit` is confined by
    /// `conway.fs` itself now, independent of any harness-level mechanism.
    #[tokio::test]
    async fn invoke_denies_a_target_outside_the_configured_root() {
        use std::sync::Arc;

        let tmp = TempDir::new().unwrap();
        let root_dir = tmp.path().join("root");
        let outside_dir = tmp.path().join("outside");
        std::fs::create_dir(&root_dir).unwrap();
        std::fs::create_dir(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("f.txt"), "one two three").unwrap();

        let (mut ctx, _h) = test_ctx(root_dir.clone());
        let mut values = serde_json::Map::new();
        values.insert(
            "conway.fs.root".to_string(),
            serde_json::json!(root_dir.display().to_string()),
        );
        ctx.config = Arc::new(conway_core::ports::PluginConfig { values });

        let err = EditTool::new()
            .invoke(
                call(serde_json::json!({
                    "path": outside_dir.join("f.txt").display().to_string(),
                    "old_string": "two",
                    "new_string": "TWO",
                })),
                ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied { .. }));
        assert_eq!(
            std::fs::read_to_string(outside_dir.join("f.txt")).unwrap(),
            "one two three",
            "the out-of-root file must be untouched"
        );
    }
}
