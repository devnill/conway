//! The terminal `AgentResult` contract (with the MAST mitigations: a bounded
//! summary, typed facts, artifacts, a `transcript_ref`, and a
//! `Rejected{missing}` status), the two-mode `SubagentSpec` (fork vs spawn),
//! the flat agent tree snapshot, the parent<->child message enum, and the
//! permission request/decision types.
//!
//! This crate performs no I/O: `SubagentSpec::validate` only checks internal
//! consistency, and the `fork`/`spawn` constructors only set field defaults.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::content::{Artifact, ToolCategory, Usage};
use crate::error::ConwayError;
use crate::ids::{AgentId, LogSeq, RoleAlias, SessionId, ToolName};

/// Fork vs spawn: re-exported from `log` (the canonical definition lives
/// there because [`crate::log::ForkOrigin`] persists it). Do not redefine.
pub use crate::log::SubagentMode;

/// The maximum number of `char`s an [`AgentResult::summary`] may contain.
/// [`AgentResult::new`] truncates on a `char` boundary, never a byte offset.
pub const DEFAULT_SUMMARY_LIMIT: usize = 2000;

/// The terminal outcome of one agent's run: the only thing a parent (or the
/// CLI/IDE) ever sees of a finished child, by design (MAST: bound what
/// crosses the trust boundary).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentResult {
    pub agent_id: AgentId,
    pub status: ResultStatus,
    /// Always a bounded `String` (never `Option`): a result without a
    /// summary is still required to say so explicitly (an empty string),
    /// not omit the field.
    pub summary: String,
    pub facts: Vec<Fact>,
    pub artifacts: Vec<Artifact>,
    pub structured: Option<serde_json::Value>,
    pub transcript_ref: SessionId,
    pub usage: Usage,
    pub steps_taken: u32,
}

impl AgentResult {
    /// Builds a result, truncating `summary` to at most
    /// [`DEFAULT_SUMMARY_LIMIT`] `char`s. The truncation point is found via
    /// `char_indices` so it always lands on a UTF-8 character boundary,
    /// never a raw byte offset (which would panic on multi-byte input).
    pub fn new(
        agent_id: AgentId,
        transcript_ref: SessionId,
        status: ResultStatus,
        summary: impl Into<String>,
    ) -> Self {
        let summary = truncate_to_char_limit(summary.into(), DEFAULT_SUMMARY_LIMIT);
        Self {
            agent_id,
            status,
            summary,
            facts: Vec::new(),
            artifacts: Vec::new(),
            structured: None,
            transcript_ref,
            usage: Usage::default(),
            steps_taken: 0,
        }
    }

    /// `true` only for [`ResultStatus::Completed`].
    pub fn is_terminal_success(&self) -> bool {
        matches!(self.status, ResultStatus::Completed)
    }
}

/// Truncate `s` to at most `max_chars` `char`s, cutting only on a character
/// boundary. No-op if `s` already has `max_chars` or fewer `char`s.
fn truncate_to_char_limit(mut s: String, max_chars: usize) -> String {
    if let Some((byte_idx, _)) = s.char_indices().nth(max_chars) {
        s.truncate(byte_idx);
    }
    s
}

/// How an agent's run ended.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResultStatus {
    Completed,
    Failed {
        error: String,
    },
    Cancelled {
        reason: String,
    },
    /// The agent hit its [`Budget`] before finishing.
    BudgetExceeded {
        limit: String,
    },
    /// The agent's request was rejected outright (e.g. a fork whose inherited
    /// context plus directive already exceeds the model's window, T-1) —
    /// never truncated or escalated.
    Rejected {
        missing: Vec<String>,
    },
}

/// One typed, attributable fact extracted from an agent's run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub key: String,
    pub value: serde_json::Value,
    pub source: Option<String>,
}

/// A hard resource ceiling on a subagent's run. `max_steps` is deliberately
/// not optional: §6.4 requires every child to have a step budget so a
/// parent's pending tool call can never hang.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub max_steps: u32,
    pub deadline: Option<DateTime<Utc>>,
    pub max_tokens: Option<u32>,
    pub max_tool_calls: Option<u32>,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_steps: 40,
            deadline: None,
            max_tokens: None,
            max_tool_calls: None,
        }
    }
}

/// The complete specification for spawning or forking a subagent. `fork` and
/// `spawn` are the only two subagent modes (GP-02); `mode` is
/// [`SubagentMode`], re-exported above.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubagentSpec {
    pub mode: SubagentMode,
    pub prompt: String,
    pub agent_def: Option<AgentDefRef>,
    pub role: Option<RoleAlias>,
    pub tools: Option<ToolSelector>,
    pub budget: Budget,
    /// Never correctness-bearing (GP-06). Meaningful only for `Fork`, where
    /// it defaults to `true`; ignored for `Spawn`, where the `fork`/`spawn`
    /// constructors force it `false`.
    pub cache_hint: bool,
    pub result_contract: Option<schemars::schema::RootSchema>,
    /// `false` enables fan-out: the caller does not block on this child's
    /// result (§"conway-tools").
    pub await_result: bool,
}

impl SubagentSpec {
    /// `Err(ConwayError::Config{..})` when `mode == Spawn && agent_def.is_none()`
    /// (§5.2: `agent_def` is required for `Spawn`).
    pub fn validate(&self) -> Result<(), ConwayError> {
        if matches!(self.mode, SubagentMode::Spawn) && self.agent_def.is_none() {
            return Err(ConwayError::Config {
                detail: "SubagentSpec: `agent_def` is required when mode == Spawn".into(),
            });
        }
        Ok(())
    }

    /// Builds a `Fork` spec. `cache_hint` defaults to `true` for forks.
    pub fn fork(prompt: impl Into<String>, budget: Budget) -> Self {
        Self {
            mode: SubagentMode::Fork,
            prompt: prompt.into(),
            agent_def: None,
            role: None,
            tools: None,
            budget,
            cache_hint: true,
            result_contract: None,
            await_result: true,
        }
    }

    /// Builds a `Spawn` spec. `cache_hint` is ignored for spawns and is
    /// forced to `false`.
    pub fn spawn(prompt: impl Into<String>, agent_def: AgentDefRef, budget: Budget) -> Self {
        Self {
            mode: SubagentMode::Spawn,
            prompt: prompt.into(),
            agent_def: Some(agent_def),
            role: None,
            tools: None,
            budget,
            cache_hint: false,
            result_contract: None,
            await_result: true,
        }
    }
}

/// A named reference to an `AgentDef` (by name, resolved by the facade).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentDefRef(pub String);

/// Which tools a subagent may use.
///
/// Matching is case-sensitive; an entry ending in `*` is a prefix match on
/// the tool name, otherwise it is exact equality. `All` selects everything;
/// `Except` selects everything not matched by its list.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSelector {
    All,
    Only(Vec<String>),
    Except(Vec<String>),
}

impl ToolSelector {
    pub fn selects(&self, tool: &ToolName) -> bool {
        match self {
            ToolSelector::All => true,
            ToolSelector::Only(patterns) => patterns.iter().any(|p| pattern_matches(p, tool)),
            ToolSelector::Except(patterns) => !patterns.iter().any(|p| pattern_matches(p, tool)),
        }
    }
}

fn pattern_matches(pattern: &str, tool: &ToolName) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => tool.as_str().starts_with(prefix),
        None => pattern == tool.as_str(),
    }
}

/// A flat, point-in-time snapshot of the whole agent tree. A `Vec` with
/// `parent` links, not a nested tree — this matches the flat, `agent`-tagged
/// event stream and keeps the snapshot trivially serializable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentTreeSnapshot {
    pub root: AgentId,
    pub nodes: Vec<AgentNode>,
    pub at: DateTime<Utc>,
}

/// One agent's entry in an [`AgentTreeSnapshot`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentNode {
    pub agent_id: AgentId,
    pub session: SessionId,
    pub parent: Option<AgentId>,
    pub mode: Option<SubagentMode>,
    pub agent_def: Option<String>,
    pub role: Option<RoleAlias>,
    pub status: AgentStatus,
    pub steps_taken: u32,
    pub budget: Budget,
}

/// An agent's lifecycle state within the tree.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Starting,
    Running,
    AwaitingPermission,
    AwaitingChildren,
    Finished,
    Failed,
    Cancelled,
}

/// A message exchanged between a parent and a child agent.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentMessage {
    Steer {
        from: AgentId,
        text: String,
        at_parent_seq: LogSeq,
    },
    Cancel {
        from: AgentId,
        reason: String,
        hard: bool,
    },
    Progress {
        from: AgentId,
        note: String,
    },
    Result {
        from: AgentId,
        result: AgentResult,
    },
}

/// The event-stream-facing projection of [`AgentMessage`] (`Event::MessageSent`).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Steer,
    Cancel,
    Progress,
    Result,
}

impl From<&AgentMessage> for MessageKind {
    fn from(msg: &AgentMessage) -> Self {
        match msg {
            AgentMessage::Steer { .. } => MessageKind::Steer,
            AgentMessage::Cancel { .. } => MessageKind::Cancel,
            AgentMessage::Progress { .. } => MessageKind::Progress,
            AgentMessage::Result { .. } => MessageKind::Result,
        }
    }
}

/// A request for permission to run one tool call (architecture §4.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub agent_id: AgentId,
    pub agent_path: Vec<AgentId>,
    pub tool: ToolName,
    pub category: ToolCategory,
    pub arguments: serde_json::Value,
    pub rendered: String,
    pub call_id: String,
}

/// The human's (or policy's) answer to a [`PermissionRequest`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PermissionDecision {
    AllowOnce,
    AllowAlways { scope: PermissionScope },
    Deny { reason: String },
    DenyWithFeedback { message: String },
}

/// How broadly an `AllowAlways` decision applies.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    Session,
    Agent,
    AgentSubtree,
}

/// The event-stream-facing projection of [`PermissionDecision`]
/// (`Event::PermissionResolved`). `Cached` has no corresponding
/// `PermissionDecision` variant: it is reached when a prior `AllowAlways`
/// decision resolves a later request without prompting again.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecisionKind {
    AllowOnce,
    AllowAlways,
    Denied,
    DeniedWithFeedback,
    Cached,
}

impl From<&PermissionDecision> for PermissionDecisionKind {
    fn from(decision: &PermissionDecision) -> Self {
        match decision {
            PermissionDecision::AllowOnce => PermissionDecisionKind::AllowOnce,
            PermissionDecision::AllowAlways { .. } => PermissionDecisionKind::AllowAlways,
            PermissionDecision::Deny { .. } => PermissionDecisionKind::Denied,
            PermissionDecision::DenyWithFeedback { .. } => {
                PermissionDecisionKind::DeniedWithFeedback
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_status_five_variants_round_trip() {
        let cases = vec![
            ResultStatus::Completed,
            ResultStatus::Failed {
                error: "boom".into(),
            },
            ResultStatus::Cancelled {
                reason: "user abort".into(),
            },
            ResultStatus::BudgetExceeded {
                limit: "max_steps=40".into(),
            },
            ResultStatus::Rejected {
                missing: vec!["tool_calling".into()],
            },
        ];
        assert_eq!(cases.len(), 5, "exactly five ResultStatus variants");
        for status in cases {
            let json = serde_json::to_string(&status).unwrap();
            let back: ResultStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn agent_result_new_truncates_summary_on_char_boundary() {
        // A 5000-char summary of 3-byte-in-UTF-8 characters: byte-offset
        // truncation at 2000 would either panic or split a character.
        let summary: String = std::iter::repeat('あ').take(5000).collect();
        let result = AgentResult::new(
            AgentId::new(),
            SessionId::new(),
            ResultStatus::Completed,
            summary,
        );
        assert_eq!(result.summary.chars().count(), DEFAULT_SUMMARY_LIMIT);
    }

    #[test]
    fn agent_result_new_leaves_short_summary_untouched() {
        let result = AgentResult::new(
            AgentId::new(),
            SessionId::new(),
            ResultStatus::Completed,
            "short summary",
        );
        assert_eq!(result.summary, "short summary");
        assert!(result.is_terminal_success());
    }

    #[test]
    fn is_terminal_success_only_for_completed() {
        let ok = AgentResult::new(
            AgentId::new(),
            SessionId::new(),
            ResultStatus::Completed,
            "",
        );
        assert!(ok.is_terminal_success());
        let failed = AgentResult::new(
            AgentId::new(),
            SessionId::new(),
            ResultStatus::Failed { error: "x".into() },
            "",
        );
        assert!(!failed.is_terminal_success());
    }

    #[test]
    fn budget_default_max_steps_is_40() {
        let b = Budget::default();
        assert_eq!(b.max_steps, 40);
        assert!(b.deadline.is_none());
        assert!(b.max_tokens.is_none());
        assert!(b.max_tool_calls.is_none());
    }

    #[test]
    fn tool_selector_all_selects_everything() {
        assert!(ToolSelector::All.selects(&ToolName::new("anything")));
    }

    #[test]
    fn tool_selector_only_matches_exact_and_prefix() {
        let sel = ToolSelector::Only(vec!["read".into(), "edit_*".into()]);
        assert!(sel.selects(&ToolName::new("read")));
        assert!(sel.selects(&ToolName::new("edit_file")));
        assert!(!sel.selects(&ToolName::new("delete")));
        assert!(!sel.selects(&ToolName::new("edit"))); // no `_` suffix content
    }

    #[test]
    fn tool_selector_except_excludes_exact_and_prefix() {
        let sel = ToolSelector::Except(vec!["delete".into(), "exec_*".into()]);
        assert!(!sel.selects(&ToolName::new("delete")));
        assert!(!sel.selects(&ToolName::new("exec_shell")));
        assert!(sel.selects(&ToolName::new("read")));
    }

    #[test]
    fn subagent_spec_validate_rejects_spawn_without_agent_def() {
        let spec = SubagentSpec {
            mode: SubagentMode::Spawn,
            prompt: "do it".into(),
            agent_def: None,
            role: None,
            tools: None,
            budget: Budget::default(),
            cache_hint: false,
            result_contract: None,
            await_result: true,
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn subagent_spec_validate_ok_for_fork_without_agent_def() {
        let spec = SubagentSpec::fork("do it", Budget::default());
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn subagent_spec_validate_ok_for_spawn_with_agent_def() {
        let spec = SubagentSpec::spawn("do it", AgentDefRef("reviewer".into()), Budget::default());
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn fork_constructor_defaults_cache_hint_true() {
        let spec = SubagentSpec::fork("x", Budget::default());
        assert_eq!(spec.mode, SubagentMode::Fork);
        assert!(spec.cache_hint);
    }

    #[test]
    fn spawn_constructor_forces_cache_hint_false() {
        let spec = SubagentSpec::spawn("x", AgentDefRef("r".into()), Budget::default());
        assert_eq!(spec.mode, SubagentMode::Spawn);
        assert!(!spec.cache_hint);
    }

    #[test]
    fn message_kind_from_agent_message() {
        let msg = AgentMessage::Progress {
            from: AgentId::new(),
            note: "n".into(),
        };
        assert_eq!(MessageKind::from(&msg), MessageKind::Progress);
        let msg = AgentMessage::Cancel {
            from: AgentId::new(),
            reason: "r".into(),
            hard: true,
        };
        assert_eq!(MessageKind::from(&msg), MessageKind::Cancel);
    }

    #[test]
    fn permission_decision_kind_from_decision() {
        assert_eq!(
            PermissionDecisionKind::from(&PermissionDecision::AllowOnce),
            PermissionDecisionKind::AllowOnce
        );
        assert_eq!(
            PermissionDecisionKind::from(&PermissionDecision::AllowAlways {
                scope: PermissionScope::Session
            }),
            PermissionDecisionKind::AllowAlways
        );
        assert_eq!(
            PermissionDecisionKind::from(&PermissionDecision::Deny {
                reason: "no".into()
            }),
            PermissionDecisionKind::Denied
        );
        assert_eq!(
            PermissionDecisionKind::from(&PermissionDecision::DenyWithFeedback {
                message: "try again".into()
            }),
            PermissionDecisionKind::DeniedWithFeedback
        );
    }

    #[test]
    fn agent_tree_snapshot_round_trips() {
        let node = AgentNode {
            agent_id: AgentId::new(),
            session: SessionId::new(),
            parent: None,
            mode: None,
            agent_def: Some("reviewer".into()),
            role: Some(RoleAlias::new("coder")),
            status: AgentStatus::Running,
            steps_taken: 3,
            budget: Budget::default(),
        };
        let snapshot = AgentTreeSnapshot {
            root: node.agent_id,
            nodes: vec![node],
            at: "2026-07-20T00:00:00Z".parse().unwrap(),
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: AgentTreeSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snapshot, back);
    }
}
