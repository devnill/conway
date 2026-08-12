//! `ResultBuilder`: accumulates the cross-turn state needed to construct an
//! agent's terminal `AgentResult` (WI-086) -- every artifact any dispatched
//! tool emitted over the run, and the most recent successful `report` tool
//! invocation, if the agent made one. `AgentLoop::finish` resolves
//! precedence between the two exactly once, at the finish boundary.
//!
//! Also home to the result-contract validation this item's MAST mitigation
//! needs (`validate_result_contract`): schema-checking a `structured` value
//! against a `SubagentSpec::result_contract`, classified into `Ok` /
//! `Retry` / `Rejected` so the turn loop can drive the spec's "one
//! corrective retry, then `Rejected{missing}`" rule. Enforcement lives at
//! the finish boundary, not in the tool layer (module notes, WI-086) --
//! this function is the boundary's stateless decision procedure; the loop
//! itself tracks whether a given failure is the first or second attempt.
//!
//! `ResultBuilder` and `StepDigest` (`step_digest.rs`) are both
//! turn-loop-local state (`AgentLoop::run_inner`'s stack), not fields on
//! `AgentLoop`/`AgentSpec` -- see this crate's lib doc / WI-086 self-check
//! for why: both structs are constructed via field literals in files
//! outside this item's scope (`runtime.rs`, `subagent.rs`, and existing
//! tests), so adding a struct field there would force edits this item is
//! not chartered to make.

use conway_core::agent::{Fact, ResultStatus};
use conway_core::content::{Artifact, ContentBlock};
use conway_core::ids::ToolName;
use serde::Deserialize;

use crate::tools::ToolOutcome;

/// The tool name the runtime recognizes as the explicit-finalization call
/// (`conway-tools`' `ReportPlugin`, WI-065's `ReportTool`). Matched by name
/// only -- `conway-runtime` must not depend on `conway-tools`
/// (`report_tool.rs`'s own module doc names this exact boundary).
const REPORT_TOOL_NAME: &str = "report";

/// The four fields whose source precedence [`ResultBuilder::resolve`]
/// arbitrates, per the module doc: (1) the `report` tool's own arguments if
/// the agent called it, else (2) trailing assistant text for `summary`,
/// empty `facts`, and tool-collected `artifacts`.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultParts {
    pub summary: String,
    pub facts: Vec<Fact>,
    pub artifacts: Vec<Artifact>,
    pub structured: Option<serde_json::Value>,
}

/// A structural mirror of `conway-tools`' `ReportBody` envelope
/// (`{"conway_report": {version, summary, facts, artifacts, structured}}`).
/// Duplicated here rather than imported -- see the module doc's dependency
/// boundary note. `version` is parsed (so a malformed envelope with a
/// non-numeric version is rejected like any other malformed one) but never
/// read back; nothing in this item's criteria is version-conditional.
#[derive(Debug, Deserialize)]
struct ReportEnvelope {
    conway_report: ReportBody,
}

#[derive(Debug, Deserialize)]
struct ReportBody {
    #[allow(dead_code)]
    #[serde(default)]
    version: u32,
    summary: String,
    #[serde(default)]
    facts: Vec<Fact>,
    #[serde(default)]
    artifacts: Vec<Artifact>,
    #[serde(default)]
    structured: Option<serde_json::Value>,
}

/// Accumulates, over an agent's whole run, the two inputs [`Self::resolve`]
/// needs at the finish boundary: every artifact any dispatched tool
/// emitted, and the most recent successful `report` call's parsed envelope.
/// A later `report` call (e.g. one made after a result-contract retry)
/// replaces an earlier one -- "last call wins".
#[derive(Default)]
pub struct ResultBuilder {
    tool_artifacts: Vec<Artifact>,
    last_report: Option<ResultParts>,
}

impl ResultBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one dispatched tool call's outcome into the builder. Every
    /// artifact the call emitted is accumulated regardless of tool name; if
    /// the call was a non-error invocation of `report` whose output parses
    /// as a `conway_report` envelope, it becomes (replacing any earlier
    /// remembered report) this run's `last_report`. A `report` call that
    /// errored, or whose output does not parse as the envelope, is treated
    /// as though `report` had not been called for this purpose -- it falls
    /// through to trailing text unless an earlier, valid `report` call
    /// already set `last_report`.
    pub fn observe_tool_outcome(&mut self, tool: &ToolName, outcome: &ToolOutcome) {
        self.tool_artifacts
            .extend(outcome.artifacts.iter().cloned());
        if tool.as_str() == REPORT_TOOL_NAME && !outcome.is_error {
            if let Some(parts) = Self::from_report_tool(&outcome.blocks) {
                self.last_report = Some(parts);
            }
        }
    }

    /// Builds `ResultParts` from a `report` tool call's text output --
    /// precedence source (1) (module doc). `None` if the output is not a
    /// well-formed `conway_report` envelope.
    pub fn from_report_tool(blocks: &[ContentBlock]) -> Option<ResultParts> {
        let text = blocks.iter().find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })?;
        let envelope: ReportEnvelope = serde_json::from_str(text).ok()?;
        let body = envelope.conway_report;
        Some(ResultParts {
            summary: body.summary,
            facts: body.facts,
            artifacts: body.artifacts,
            structured: body.structured,
        })
    }

    /// Builds `ResultParts` from trailing assistant text -- precedence
    /// source (2) (module doc): empty `facts`, tool-collected `artifacts`,
    /// no `structured` output. `status` names the terminal outcome so an
    /// agent that produced no text at all (and never called `report`)
    /// still gets a non-empty, meaningful summary rather than `""`.
    pub fn from_trailing_text(
        trailing: &str,
        tool_artifacts: Vec<Artifact>,
        status: &ResultStatus,
    ) -> ResultParts {
        let summary = if trailing.trim().is_empty() {
            format!("(no output; terminal status: {})", status_label(status))
        } else {
            trailing.to_string()
        };
        ResultParts {
            summary,
            facts: Vec::new(),
            artifacts: tool_artifacts,
            structured: None,
        }
    }

    /// Resolves precedence for the finish boundary: the last successful
    /// `report` call if the agent made one, else trailing text. Takes
    /// `&self` (does not consume the builder) so `AgentLoop::finish` -- a
    /// `&self` method -- can call it directly.
    pub fn resolve(&self, trailing: &str, status: &ResultStatus) -> ResultParts {
        match &self.last_report {
            Some(parts) => parts.clone(),
            None => Self::from_trailing_text(trailing, self.tool_artifacts.clone(), status),
        }
    }
}

/// A short, human-readable name for a `ResultStatus`. Originally used only
/// to build the non-empty fallback summary in
/// [`ResultBuilder::from_trailing_text`]; `pub(crate)` since
/// `context::builder`'s `own_segment` also needs it to render a
/// `LogRecord::ChildResultRecord`'s status into the segment text a parent's
/// next turn actually sees. `ResultStatus` is `#[non_exhaustive]`; the
/// wildcard arm is forward compatibility, not a modeled case.
pub(crate) fn status_label(status: &ResultStatus) -> &'static str {
    match status {
        ResultStatus::Completed => "completed",
        ResultStatus::Failed { .. } => "failed",
        ResultStatus::Cancelled { .. } => "cancelled",
        ResultStatus::BudgetExceeded { .. } => "budget_exceeded",
        ResultStatus::Rejected { .. } => "rejected",
        _ => "unknown",
    }
}

/// The outcome of validating an agent's `structured` output against a
/// `result_contract` schema at the finish boundary.
#[derive(Debug, PartialEq)]
pub enum ContractOutcome {
    /// `structured` satisfies the schema (including the vacuous case: there
    /// is no contract to satisfy).
    Ok,
    /// `structured` fails the schema and this is the first failure this run
    /// -- the loop should inject `errors` as a system note and give the
    /// agent one more turn.
    Retry { errors: Vec<String> },
    /// `structured` fails the schema and a retry has already been spent --
    /// terminal: the loop should finish with `ResultStatus::Rejected {
    /// missing }`.
    Rejected { missing: Vec<String> },
}

/// Validates `structured` against `contract`, classifying a failure as
/// `Retry` or `Rejected` depending on `already_retried` -- the spec's
/// "retried exactly once" rule. This function is stateless; the caller
/// (`AgentLoop::run_inner`) is the one tracking whether a prior retry has
/// already been spent this run.
pub fn validate_result_contract(
    structured: Option<&serde_json::Value>,
    contract: &schemars::schema::RootSchema,
    already_retried: bool,
) -> ContractOutcome {
    match schema_errors(structured, contract) {
        Ok(()) => ContractOutcome::Ok,
        Err(errors) if !already_retried => ContractOutcome::Retry { errors },
        Err(errors) => ContractOutcome::Rejected { missing: errors },
    }
}

/// Runs `contract` against `structured` (`null` if absent -- a contract
/// with no `structured` output at all fails it, unless the schema itself
/// accepts `null`), returning every failing instance path plus its message,
/// or `Ok(())` if none. A schema that itself fails to serialize/compile is
/// reported the same way as a validation failure (closest-fit convention:
/// there is no separate "schema is broken" status in this item's
/// `ContractOutcome`), so a malformed `result_contract` still surfaces as an
/// actionable message rather than a panic.
fn schema_errors(
    structured: Option<&serde_json::Value>,
    contract: &schemars::schema::RootSchema,
) -> Result<(), Vec<String>> {
    let schema_value = serde_json::to_value(contract).map_err(|err| {
        vec![format!(
            "result_contract schema is not serializable to JSON: {err}"
        )]
    })?;
    let validator = jsonschema::validator_for(&schema_value)
        .map_err(|err| vec![format!("result_contract schema failed to compile: {err}")])?;
    let instance = structured.cloned().unwrap_or(serde_json::Value::Null);
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|err| format!("{}: {err}", err.instance_path()))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use conway_core::content::ArtifactKind;

    use super::*;

    fn report_blocks(json: serde_json::Value) -> Vec<ContentBlock> {
        vec![ContentBlock::Text {
            text: json.to_string(),
        }]
    }

    fn envelope(summary: &str) -> serde_json::Value {
        serde_json::json!({
            "conway_report": {
                "version": 1,
                "summary": summary,
                "facts": [{"key": "k", "value": "v", "source": null}],
                "artifacts": [],
                "structured": {"ok": true},
            }
        })
    }

    #[test]
    fn from_report_tool_parses_a_well_formed_envelope() {
        let parts = ResultBuilder::from_report_tool(&report_blocks(envelope("did the thing")))
            .expect("well-formed envelope must parse");
        assert_eq!(parts.summary, "did the thing");
        assert_eq!(parts.facts.len(), 1);
        assert_eq!(parts.structured, Some(serde_json::json!({"ok": true})));
    }

    #[test]
    fn from_report_tool_rejects_malformed_json() {
        let blocks = vec![ContentBlock::Text {
            text: "not json at all".into(),
        }];
        assert!(ResultBuilder::from_report_tool(&blocks).is_none());
    }

    #[test]
    fn from_report_tool_rejects_missing_envelope_key() {
        let blocks = report_blocks(serde_json::json!({"something_else": {}}));
        assert!(ResultBuilder::from_report_tool(&blocks).is_none());
    }

    #[test]
    fn from_trailing_text_uses_trailing_text_when_present() {
        let parts = ResultBuilder::from_trailing_text("hello", vec![], &ResultStatus::Completed);
        assert_eq!(parts.summary, "hello");
        assert!(parts.facts.is_empty());
        assert!(parts.structured.is_none());
    }

    #[test]
    fn from_trailing_text_names_the_status_when_empty() {
        let parts = ResultBuilder::from_trailing_text("", vec![], &ResultStatus::Completed);
        assert!(!parts.summary.is_empty());
        assert!(parts.summary.contains("completed"));

        let parts = ResultBuilder::from_trailing_text(
            "   ",
            vec![],
            &ResultStatus::BudgetExceeded {
                limit: "max_steps=40".into(),
            },
        );
        assert!(!parts.summary.is_empty());
        assert!(parts.summary.contains("budget_exceeded"));
    }

    #[test]
    fn resolve_prefers_report_over_trailing_text() {
        let mut builder = ResultBuilder::new();
        let outcome = ToolOutcome {
            call_id: "tc_1".into(),
            tool: ToolName::new(REPORT_TOOL_NAME),
            blocks: report_blocks(envelope("from report")),
            is_error: false,
            truncation: None,
            artifacts: vec![],
        };
        builder.observe_tool_outcome(&ToolName::new(REPORT_TOOL_NAME), &outcome);

        let parts = builder.resolve("trailing text should lose", &ResultStatus::Completed);
        assert_eq!(parts.summary, "from report");
    }

    #[test]
    fn resolve_falls_back_to_trailing_text_when_report_never_called() {
        let builder = ResultBuilder::new();
        let parts = builder.resolve("the trailing text", &ResultStatus::Completed);
        assert_eq!(parts.summary, "the trailing text");
    }

    #[test]
    fn last_report_call_wins_over_an_earlier_one() {
        let mut builder = ResultBuilder::new();
        let first = ToolOutcome {
            call_id: "tc_1".into(),
            tool: ToolName::new(REPORT_TOOL_NAME),
            blocks: report_blocks(envelope("first attempt")),
            is_error: false,
            truncation: None,
            artifacts: vec![],
        };
        let second = ToolOutcome {
            call_id: "tc_2".into(),
            tool: ToolName::new(REPORT_TOOL_NAME),
            blocks: report_blocks(envelope("second attempt")),
            is_error: false,
            truncation: None,
            artifacts: vec![],
        };
        builder.observe_tool_outcome(&ToolName::new(REPORT_TOOL_NAME), &first);
        builder.observe_tool_outcome(&ToolName::new(REPORT_TOOL_NAME), &second);

        let parts = builder.resolve("", &ResultStatus::Completed);
        assert_eq!(parts.summary, "second attempt");
    }

    #[test]
    fn a_failed_report_call_does_not_override_an_earlier_valid_one() {
        let mut builder = ResultBuilder::new();
        let good = ToolOutcome {
            call_id: "tc_1".into(),
            tool: ToolName::new(REPORT_TOOL_NAME),
            blocks: report_blocks(envelope("good report")),
            is_error: false,
            truncation: None,
            artifacts: vec![],
        };
        let bad = ToolOutcome {
            call_id: "tc_2".into(),
            tool: ToolName::new(REPORT_TOOL_NAME),
            blocks: vec![ContentBlock::Text {
                text: "summary exceeds 2000 characters".into(),
            }],
            is_error: true,
            truncation: None,
            artifacts: vec![],
        };
        builder.observe_tool_outcome(&ToolName::new(REPORT_TOOL_NAME), &good);
        builder.observe_tool_outcome(&ToolName::new(REPORT_TOOL_NAME), &bad);

        let parts = builder.resolve("", &ResultStatus::Completed);
        assert_eq!(parts.summary, "good report");
    }

    #[test]
    fn tool_artifacts_accumulate_across_multiple_non_report_calls() {
        let mut builder = ResultBuilder::new();
        let artifact = |id: &str| Artifact {
            id: id.into(),
            kind: ArtifactKind::File,
            path: None,
            media_type: None,
            bytes: None,
            label: id.into(),
        };
        let outcome_a = ToolOutcome {
            call_id: "tc_1".into(),
            tool: ToolName::new("write"),
            blocks: vec![],
            is_error: false,
            truncation: None,
            artifacts: vec![artifact("a")],
        };
        let outcome_b = ToolOutcome {
            call_id: "tc_2".into(),
            tool: ToolName::new("write"),
            blocks: vec![],
            is_error: false,
            truncation: None,
            artifacts: vec![artifact("b")],
        };
        builder.observe_tool_outcome(&ToolName::new("write"), &outcome_a);
        builder.observe_tool_outcome(&ToolName::new("write"), &outcome_b);

        let parts = builder.resolve("done", &ResultStatus::Completed);
        assert_eq!(
            parts.artifacts.iter().map(|a| &a.id).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    fn schema_requiring(prop: &str) -> schemars::schema::RootSchema {
        serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": { prop: { "type": "string" } },
            "required": [prop],
        }))
        .unwrap()
    }

    #[test]
    fn validate_result_contract_ok_for_matching_structured() {
        let contract = schema_requiring("summary");
        let structured = serde_json::json!({"summary": "ok"});
        let outcome = validate_result_contract(Some(&structured), &contract, false);
        assert_eq!(outcome, ContractOutcome::Ok);
    }

    #[test]
    fn validate_result_contract_retries_once_then_rejects() {
        let contract = schema_requiring("summary");
        let outcome = validate_result_contract(None, &contract, false);
        match outcome {
            ContractOutcome::Retry { errors } => assert!(!errors.is_empty()),
            other => panic!("expected Retry, got {other:?}"),
        }

        let outcome = validate_result_contract(None, &contract, true);
        match outcome {
            ContractOutcome::Rejected { missing } => assert!(!missing.is_empty()),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn validate_result_contract_treats_missing_structured_as_null() {
        let contract = schema_requiring("summary");
        let outcome = validate_result_contract(None, &contract, false);
        assert_ne!(outcome, ContractOutcome::Ok);
    }

    #[test]
    fn status_label_covers_every_variant_used_by_from_trailing_text() {
        assert_eq!(status_label(&ResultStatus::Completed), "completed");
        assert_eq!(
            status_label(&ResultStatus::Failed { error: "x".into() }),
            "failed"
        );
        assert_eq!(
            status_label(&ResultStatus::Cancelled { reason: "x".into() }),
            "cancelled"
        );
        assert_eq!(
            status_label(&ResultStatus::Rejected { missing: vec![] }),
            "rejected"
        );
    }
}
