//! `GrepTool`: the `grep` tool — regex content search over a directory
//! tree, gitignore-aware.

use async_trait::async_trait;
use regex::RegexBuilder;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::content::{PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{PathArgs, RenderKind, Tool, ToolCtx, ToolOutput};

use crate::common::{check_cancel, error_text, parse_args, resolve_path, text_output};
use crate::fs::walk_files;

/// The first N bytes inspected for a NUL byte to decide whether a file is
/// binary (matches `read.rs`'s sniff window).
const SNIFF_BYTES: usize = 8192;

/// Matches returned when the caller doesn't supply `max_results`.
const DEFAULT_MAX_RESULTS: u32 = 200;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GrepArgs {
    /// Rust regex
    pattern: String,
    /// Search root; default cwd
    path: Option<String>,
    /// Only search files matching this glob
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    #[schemars(range(min = 1))]
    max_results: Option<u32>,
}

/// Searches file contents under a search root for lines matching a regex.
/// Walks gitignore-aware (via `crate::fs::walk_files`); results are
/// grouped by path in walk order, one line per match.
#[derive(Debug, Default)]
pub struct GrepTool;

impl GrepTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GrepTool {
    /// `GrepArgs::path` is the search root (optional; defaults to the agent
    /// cwd). `pattern` (a regex) and `glob` (a filter expression) are NOT
    /// paths, so declaring them would give a root check strings it cannot
    /// canonicalize.
    fn path_args(&self) -> PathArgs {
        PathArgs::Named(&["path"])
    }

    /// `grep` never overrides `render`, so its rendering is always the
    /// trait's own default JSON dump -- never a shell command. Board item
    /// 01KYT3NSWRHMPEAXVXRJ73KDYR.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("grep"),
            description: "Search file contents for a regex pattern, gitignore-aware".into(),
            schema: schemars::schema_for!(GrepArgs),
            category: ToolCategory::Search,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: GrepArgs = parse_args(&call)?;
        let root = match &args.path {
            Some(p) => resolve_path(&ctx, p)?,
            None => ctx.cwd.clone(),
        };
        let max_results = args.max_results.unwrap_or(DEFAULT_MAX_RESULTS) as usize;

        let regex = match RegexBuilder::new(&args.pattern)
            .case_insensitive(args.case_insensitive)
            .build()
        {
            Ok(regex) => regex,
            Err(err) => {
                return Ok(error_text(format!(
                    "invalid pattern {}: {err}",
                    args.pattern
                )));
            }
        };

        let glob_matcher = match &args.glob {
            Some(pattern) => match globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
            {
                Ok(glob) => Some(glob.compile_matcher()),
                Err(err) => {
                    return Ok(error_text(format!("invalid glob {pattern}: {err}")));
                }
            },
            None => None,
        };

        let cancel = ctx.cancel.clone();
        let walk_root = root.clone();
        let entries = tokio::task::spawn_blocking(move || walk_files(&walk_root, &cancel))
            .await
            .map_err(|err| ToolError::Io {
                detail: format!("grep walk task panicked: {err}"),
            })??;

        check_cancel(&ctx)?;

        let mut lines_out: Vec<String> = Vec::new();
        let mut match_count = 0usize;
        let mut limit_reached = false;

        'entries: for entry in &entries {
            if let Some(matcher) = &glob_matcher {
                if !matcher.is_match(&entry.relative) {
                    continue;
                }
            }

            let bytes = match tokio::fs::read(&entry.absolute).await {
                Ok(bytes) => bytes,
                // Best-effort: a file that vanished or became unreadable
                // between the walk and the read is skipped, not fatal.
                Err(_) => continue,
            };

            let sniff_len = bytes.len().min(SNIFF_BYTES);
            if bytes[..sniff_len].contains(&0u8) {
                continue;
            }

            let content = String::from_utf8_lossy(&bytes);
            for (idx, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    lines_out.push(format!("{}:{}:{line}", entry.relative.display(), idx + 1));
                    match_count += 1;
                    if match_count >= max_results {
                        limit_reached = true;
                        break 'entries;
                    }
                }
            }
        }

        if lines_out.is_empty() {
            return Ok(error_text(format!("no matches for {}", args.pattern)));
        }

        let mut text = lines_out.join("\n");
        if limit_reached {
            text.push_str(&format!("\n… (result limit {max_results} reached)"));
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
            name: ToolName::new("grep"),
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
        let spec = GrepTool::new().spec();
        assert_eq!(spec.name.as_str(), "grep");
        assert_eq!(spec.category, ToolCategory::Search);
        assert_eq!(spec.permission, PermissionClass::Safe);
    }

    #[test]
    fn schema_required_and_properties() {
        let spec = GrepTool::new().spec();
        let json = serde_json::to_value(&spec.schema).unwrap();
        let required: Vec<&str> = json["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["pattern"]);
        let props = json["properties"].as_object().unwrap();
        for key in ["pattern", "path", "glob", "case_insensitive", "max_results"] {
            assert!(props.contains_key(key), "missing {key}");
        }
        assert_eq!(json["additionalProperties"], false);
    }

    #[tokio::test]
    async fn invoke_zero_matches_is_recoverable_error() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let out = GrepTool::new()
            .invoke(call(serde_json::json!({"pattern": "fn foo"})), ctx)
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(text_of(&out).contains("no matches for fn foo"));
    }

    #[tokio::test]
    async fn invoke_invalid_regex_is_recoverable_error_containing_parse_error() {
        let (ctx, _h) = test_ctx(PathBuf::from("/tmp"));
        let out = GrepTool::new()
            .invoke(call(serde_json::json!({"pattern": "("})), ctx)
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(text_of(&out).contains("unclosed group"));
    }

    #[tokio::test]
    async fn invoke_pre_cancelled_returns_cancelled() {
        let (ctx, handles) = test_ctx(PathBuf::from("/tmp"));
        handles.cancel.cancel();
        let err = GrepTool::new()
            .invoke(call(serde_json::json!({"pattern": "fn"})), ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Cancelled));
    }
}
