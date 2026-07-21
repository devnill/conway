//! `ReportTool`: the `report` tool — explicit terminal-result declaration.
//!
//! This tool emits a canonical, versioned JSON envelope (`conway_report`).
//! It performs no delegation, no session logging, and no result
//! finalization itself: the runtime recognizes the `report` tool by name
//! and lifts the envelope's payload into the agent's terminal result. That
//! lift is deliberately kept out of this crate (architecture boundary:
//! conway-tools must not depend on conway-runtime or conway-session).

use std::path::PathBuf;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use conway_core::agent::Fact;
use conway_core::content::{
    Artifact, ArtifactKind, PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy,
};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{Tool, ToolCtx, ToolOutput};

use crate::common::{check_cancel, error_text, parse_args, text_output};

const MAX_SUMMARY_CHARS: usize = 2000;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FactArg {
    key: String,
    value: String,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArtifactArg {
    kind: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReportArgs {
    #[schemars(length(max = 2000))]
    summary: String,
    #[serde(default)]
    facts: Vec<FactArg>,
    #[serde(default)]
    artifacts: Vec<ArtifactArg>,
    #[serde(default)]
    structured: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct ReportEnvelope {
    conway_report: ReportBody,
}

#[derive(Debug, Serialize)]
struct ReportBody {
    version: u32,
    summary: String,
    facts: Vec<Fact>,
    artifacts: Vec<Artifact>,
    structured: Option<serde_json::Value>,
}

/// Gives an agent a tool to explicitly declare its terminal result instead
/// of the runtime inferring one from trailing text.
#[derive(Debug, Default)]
pub struct ReportTool;

impl ReportTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ReportTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("report"),
            description: "Declare this agent's terminal result: a bounded summary, optional \
                          typed facts, optional artifacts, and optional structured output"
                .into(),
            schema: schemars::schema_for!(ReportArgs),
            category: ToolCategory::Think,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: ReportArgs = parse_args(&call)?;

        if args.summary.chars().count() > MAX_SUMMARY_CHARS {
            return Ok(error_text("summary exceeds 2000 characters".into()));
        }

        let facts: Vec<Fact> = args.facts.into_iter().map(to_fact).collect();
        let artifacts: Vec<Artifact> = args
            .artifacts
            .into_iter()
            .enumerate()
            .map(|(index, arg)| to_artifact(&call.call_id, index, arg))
            .collect::<Result<_, _>>()?;

        let envelope = ReportEnvelope {
            conway_report: ReportBody {
                version: 1,
                summary: args.summary,
                facts,
                artifacts,
                structured: args.structured,
            },
        };
        let text =
            serde_json::to_string(&envelope).expect("report envelope is always serializable");

        let mut output = text_output(text, TruncationPolicy::None);
        output.artifacts = envelope.conway_report.artifacts;
        Ok(output)
    }
}

fn to_fact(arg: FactArg) -> Fact {
    Fact {
        key: arg.key,
        value: serde_json::Value::String(arg.value),
        source: arg.source,
    }
}

fn to_artifact(call_id: &str, index: usize, arg: ArtifactArg) -> Result<Artifact, ToolError> {
    let kind = parse_artifact_kind(&arg.kind)?;
    Ok(Artifact {
        id: format!("{call_id}-artifact-{index}"),
        kind,
        path: arg.path.map(PathBuf::from),
        media_type: None,
        bytes: None,
        label: arg.value.unwrap_or_default(),
    })
}

fn parse_artifact_kind(kind: &str) -> Result<ArtifactKind, ToolError> {
    match kind {
        "file" => Ok(ArtifactKind::File),
        "diff" => Ok(ArtifactKind::Diff),
        "value" => Ok(ArtifactKind::Value),
        "log" => Ok(ArtifactKind::Log),
        other => Err(ToolError::InvalidArguments {
            detail: format!(
                "unknown artifact kind {other:?}; expected one of file, diff, value, log"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_ctx;

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "tc_1".into(),
            name: ToolName::new("report"),
            arguments,
        }
    }

    #[test]
    fn spec_has_expected_name_category_permission() {
        let spec = ReportTool::new().spec();
        assert_eq!(spec.name.as_str(), "report");
        assert_eq!(spec.category, ToolCategory::Think);
        assert_eq!(spec.permission, PermissionClass::Safe);
    }

    #[test]
    fn schema_required_and_additional_properties() {
        let spec = ReportTool::new().spec();
        let json = serde_json::to_value(&spec.schema).unwrap();
        let required: Vec<&str> = json["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["summary"]);
        assert_eq!(json["additionalProperties"], false);
    }

    #[tokio::test]
    async fn valid_call_emits_versioned_envelope() {
        let (ctx, _h) = test_ctx(PathBuf::from("/tmp/x"));
        let out = ReportTool::new()
            .invoke(
                call(serde_json::json!({
                    "summary": "did the thing",
                    "facts": [{"key": "k", "value": "v"}],
                    "artifacts": [{"kind": "file", "path": "a.txt", "value": "label"}],
                    "structured": {"ok": true}
                })),
                ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.truncation, TruncationPolicy::None);

        let text = match &out.blocks[0] {
            conway_core::content::ContentBlock::Text { text } => text.clone(),
            other => panic!("expected a text block, got {other:?}"),
        };
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let report = &json["conway_report"];
        assert_eq!(report["version"], 1);
        assert_eq!(report["summary"], "did the thing");

        let facts: Vec<Fact> = serde_json::from_value(report["facts"].clone()).unwrap();
        assert_eq!(facts.len(), 1);
        let artifacts: Vec<Artifact> = serde_json::from_value(report["artifacts"].clone()).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(out.artifacts.len(), 1);
    }

    #[tokio::test]
    async fn omitted_fields_default_to_empty_and_null() {
        let (ctx, _h) = test_ctx(PathBuf::from("/tmp/x"));
        let out = ReportTool::new()
            .invoke(call(serde_json::json!({"summary": "s"})), ctx)
            .await
            .unwrap();
        let text = match &out.blocks[0] {
            conway_core::content::ContentBlock::Text { text } => text.clone(),
            other => panic!("expected a text block, got {other:?}"),
        };
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let report = &json["conway_report"];
        assert_eq!(report["facts"], serde_json::json!([]));
        assert_eq!(report["artifacts"], serde_json::json!([]));
        assert_eq!(report["structured"], serde_json::Value::Null);
        assert!(out.artifacts.is_empty());
    }

    #[tokio::test]
    async fn summary_over_limit_is_error_with_no_artifacts() {
        let (ctx, _h) = test_ctx(PathBuf::from("/tmp/x"));
        let long_summary = "a".repeat(2001);
        let out = ReportTool::new()
            .invoke(
                call(serde_json::json!({
                    "summary": long_summary,
                    "artifacts": [{"kind": "file", "path": "a.txt"}]
                })),
                ctx,
            )
            .await
            .unwrap();
        assert!(out.is_error);
        let text = match &out.blocks[0] {
            conway_core::content::ContentBlock::Text { text } => text.clone(),
            other => panic!("expected a text block, got {other:?}"),
        };
        assert!(text.contains("summary exceeds 2000 characters"));
        assert!(out.artifacts.is_empty());
    }

    #[tokio::test]
    async fn unknown_artifact_kind_is_invalid_arguments() {
        let (ctx, _h) = test_ctx(PathBuf::from("/tmp/x"));
        let err = ReportTool::new()
            .invoke(
                call(serde_json::json!({
                    "summary": "s",
                    "artifacts": [{"kind": "bogus"}]
                })),
                ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn invoke_pre_cancelled_returns_cancelled() {
        let (ctx, handles) = test_ctx(PathBuf::from("/tmp/x"));
        handles.cancel.cancel();
        let err = ReportTool::new()
            .invoke(call(serde_json::json!({"summary": "s"})), ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Cancelled));
    }
}
