//! `GlobTool`: the `glob` tool — gitignore-aware glob pattern matching over
//! a directory tree, results ordered mtime-descending.

use std::path::PathBuf;
use std::time::SystemTime;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::content::{PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{PathArgs, RenderKind, Tool, ToolCtx, ToolOutput};

use crate::common::{check_cancel, error_text, parse_args, resolve_path, text_output};
use crate::fs::walk_files;

/// Matches returned when the caller doesn't supply `limit`.
const DEFAULT_LIMIT: u32 = 1000;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GlobArgs {
    /// Glob pattern, e.g. **/*.rs
    pattern: String,
    /// Search root; default cwd
    path: Option<String>,
    /// Max matches to return; default 1000
    #[schemars(range(min = 1))]
    limit: Option<u32>,
}

/// Finds files under a search root matching a glob pattern. Walks
/// gitignore-aware (via `crate::fs::walk_files`); results are ordered by
/// file mtime descending, ties broken lexicographically by relative path.
#[derive(Debug, Default)]
pub struct GlobTool;

impl GlobTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GlobTool {
    /// `GlobArgs::path` is the search root (optional; defaults to the agent
    /// cwd). `pattern` is deliberately NOT declared: it is a glob expression
    /// matched *within* `path`, not a path itself, and declaring it would
    /// hand a root check a string it cannot meaningfully canonicalize.
    fn path_args(&self) -> PathArgs {
        PathArgs::Named(&["path"])
    }

    /// `glob` never overrides `render`, so its rendering is always the
    /// trait's own default JSON dump -- never a shell command. Board item
    /// 01KYT3NSWRHMPEAXVXRJ73KDYR.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("glob"),
            description: "Find files matching a glob pattern, gitignore-aware".into(),
            schema: schemars::schema_for!(GlobArgs),
            category: ToolCategory::Search,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: GlobArgs = parse_args(&call)?;
        let root = match &args.path {
            Some(p) => resolve_path(&ctx, p)?,
            None => ctx.cwd.clone(),
        };
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT) as usize;

        let matcher = match globset::GlobBuilder::new(&args.pattern)
            .literal_separator(true)
            .build()
        {
            Ok(glob) => glob.compile_matcher(),
            Err(err) => {
                return Ok(error_text(format!(
                    "invalid pattern {}: {err}",
                    args.pattern
                )));
            }
        };

        let cancel = ctx.cancel.clone();
        let walk_root = root.clone();
        let entries = tokio::task::spawn_blocking(move || walk_files(&walk_root, &cancel))
            .await
            .map_err(|err| ToolError::Io {
                detail: format!("glob walk task panicked: {err}"),
            })??;

        check_cancel(&ctx)?;

        let mut matches: Vec<(PathBuf, SystemTime)> = Vec::new();
        for entry in &entries {
            if matcher.is_match(&entry.relative) {
                let mtime = std::fs::metadata(&entry.absolute)
                    .and_then(|meta| meta.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                matches.push((entry.relative.clone(), mtime));
            }
        }

        if matches.is_empty() {
            return Ok(error_text(format!("no files matched {}", args.pattern)));
        }

        // mtime descending, ties broken lexicographically ascending.
        matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let total = matches.len();
        let lines: Vec<String> = matches
            .iter()
            .take(limit)
            .map(|(path, _)| path.display().to_string())
            .collect();
        let mut text = lines.join("\n");
        if total > limit {
            text.push_str(&format!("\n… ({} more matches)", total - limit));
        }

        Ok(text_output(
            text,
            TruncationPolicy::Head { max_bytes: 32_768 },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_ctx;
    use conway_core::content::ContentBlock;
    use std::path::PathBuf;

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "tc_1".into(),
            name: ToolName::new("glob"),
            arguments,
        }
    }

    fn text_of(out: &ToolOutput) -> &str {
        match &out.blocks[0] {
            ContentBlock::Text { text } => text,
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    fn spec_has_expected_name_category_permission() {
        let spec = GlobTool::new().spec();
        assert_eq!(spec.name.as_str(), "glob");
        assert_eq!(spec.category, ToolCategory::Search);
        assert_eq!(spec.permission, PermissionClass::Safe);
    }

    #[test]
    fn schema_required_and_properties() {
        let spec = GlobTool::new().spec();
        let json = serde_json::to_value(&spec.schema).unwrap();
        let required: Vec<&str> = json["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["pattern"]);
        let props = json["properties"].as_object().unwrap();
        assert!(props.contains_key("pattern"));
        assert!(props.contains_key("path"));
        assert!(props.contains_key("limit"));
        assert_eq!(json["additionalProperties"], false);
    }

    #[tokio::test]
    async fn invoke_zero_matches_is_recoverable_error() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let out = GlobTool::new()
            .invoke(call(serde_json::json!({"pattern": "*.rs"})), ctx)
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(text_of(&out).contains("no files matched *.rs"));
    }

    #[tokio::test]
    async fn invoke_invalid_pattern_is_recoverable_error() {
        let (ctx, _h) = test_ctx(PathBuf::from("/tmp"));
        let out = GlobTool::new()
            .invoke(call(serde_json::json!({"pattern": "["})), ctx)
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn invoke_pre_cancelled_returns_cancelled() {
        let (ctx, handles) = test_ctx(PathBuf::from("/tmp"));
        handles.cancel.cancel();
        let err = GlobTool::new()
            .invoke(call(serde_json::json!({"pattern": "*.rs"})), ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Cancelled));
    }
}
