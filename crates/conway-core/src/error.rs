//! The complete error taxonomy for the conway workspace.
//!
//! All enums are `#[non_exhaustive]`, serde round-trippable (externally tagged,
//! owned data only), and carry `Display` messages via `thiserror`.
//!
//! The two T-1 variants (`RoutingError::ContextTooLarge`,
//! `RuntimeError::ForkContextOverflow`) are terminal by construction: no field
//! can express a truncation or escalation outcome.

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, LogSeq, ModelRef, RoleAlias, SessionId, ToolName};

/// Errors produced by a `Backend` implementation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum BackendError {
    #[error("transport error: {detail}")]
    Transport { detail: String },
    #[error("rate limited (retry after {retry_after_secs:?} seconds)")]
    RateLimit { retry_after_secs: Option<u64> },
    #[error("authentication failed: {detail}")]
    Auth { detail: String },
    #[error("bad request: {detail}")]
    BadRequest { detail: String },
    #[error("server error (status {status}): {detail}")]
    ServerError { status: u16, detail: String },
    #[error("context overflow: request requires {required_tokens} tokens, model accepts at most {max_context_tokens}")]
    ContextOverflow {
        required_tokens: u32,
        max_context_tokens: u32,
    },
    #[error("tool call parse failure: {detail}")]
    ToolParse { detail: String },
    #[error("request cancelled")]
    Cancelled,
}

impl BackendError {
    /// Whether the attempt loop should advance to the next candidate route.
    pub fn is_failover_worthy(&self) -> bool {
        matches!(
            self,
            BackendError::Transport { .. }
                | BackendError::RateLimit { .. }
                | BackendError::ServerError { .. }
                | BackendError::ContextOverflow { .. }
        )
    }

    /// Whether this error is a signal about endpoint health.
    ///
    /// `Auth`, `BadRequest`, `ContextOverflow`, `ToolParse`, and `Cancelled`
    /// are request problems, not endpoint-health signals (§8).
    pub fn is_health_signal(&self) -> bool {
        matches!(
            self,
            BackendError::Transport { .. }
                | BackendError::ServerError { .. }
                | BackendError::RateLimit { .. }
        )
    }
}

/// Errors produced by a `Tool` implementation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ToolError {
    #[error("invalid arguments: {detail}")]
    InvalidArguments { detail: String },
    #[error("permission denied: {reason}")]
    Denied { reason: String },
    #[error("tool cancelled")]
    Cancelled,
    #[error("tool timed out after {after_secs} seconds")]
    Timeout { after_secs: u64 },
    #[error("io error: {detail}")]
    Io { detail: String },
    #[error("internal tool error: {detail}")]
    Internal { detail: String },
}

/// Errors produced by a `SessionStore` implementation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum StoreError {
    #[error("session {session} not found")]
    NotFound { session: SessionId },
    #[error("session {session} corrupt at line {line}: {detail}")]
    Corrupt {
        session: SessionId,
        line: u64,
        detail: String,
    },
    #[error("store io error: {detail}")]
    Io { detail: String },
    #[error("sequence {requested} out of range (head is {head})")]
    SeqOutOfRange { requested: LogSeq, head: LogSeq },
    #[error("session {session} already exists")]
    AlreadyExists { session: SessionId },
}

/// Errors produced by the `Router`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum RoutingError {
    /// No candidate in the role's chain was admissible. The `String` in each
    /// pair is the rendered routing reason (a typed `RoutingReason` cannot be
    /// used here without a module cycle; keep the rendered form).
    #[error("no candidate for role {role} ({} considered)", considered.len())]
    NoCandidate {
        role: RoleAlias,
        considered: Vec<(ModelRef, String)>,
    },
    #[error("unknown role alias: {role}")]
    UnknownRole { role: RoleAlias },
    #[error("unknown model reference: {reference}")]
    UnknownModelRef { reference: String },
    /// T-1: the assembled context plus reserved headroom exceeds the model's
    /// window. Terminal — no truncation or escalation is performed.
    #[error("context rejected: {est_tokens} prompt + {headroom_tokens} reserved output = {required_tokens} tokens, but {model} accepts at most {max_context_tokens} (short by {shortfall_tokens}); no truncation or escalation is performed")]
    ContextTooLarge {
        role: RoleAlias,
        model: ModelRef,
        /// Assembled prompt estimate.
        est_tokens: u32,
        /// Reserved output/reasoning budget.
        headroom_tokens: u32,
        /// `est_tokens + headroom_tokens`, saturating.
        required_tokens: u32,
        max_context_tokens: u32,
        /// `required_tokens - max_context_tokens`, saturating.
        shortfall_tokens: u32,
    },
}

/// Errors produced by the runtime.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum RuntimeError {
    #[error("agent {agent} not found")]
    AgentNotFound { agent: AgentId },
    #[error("agent {agent} exceeded its budget")]
    BudgetExceeded { agent: AgentId },
    #[error("agent {agent} cancelled: {reason}")]
    Cancelled { agent: AgentId, reason: String },
    #[error("backend error: {0}")]
    Backend(#[from] BackendError),
    #[error("routing error: {0}")]
    Routing(#[from] RoutingError),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),
    /// T-1 at the fork boundary. Terminal — no truncation or escalation.
    #[error("context rejected: {est_tokens} prompt + {headroom_tokens} reserved output = {required_tokens} tokens, but {model} accepts at most {max_context_tokens} (short by {shortfall_tokens}); no truncation or escalation is performed")]
    ForkContextOverflow {
        parent: AgentId,
        model: ModelRef,
        est_tokens: u32,
        headroom_tokens: u32,
        required_tokens: u32,
        max_context_tokens: u32,
        shortfall_tokens: u32,
    },
}

/// Errors produced by plugin registration and initialization.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum PluginError {
    #[error("plugin {plugin} failed to initialize: {detail}")]
    Init { plugin: String, detail: String },
    #[error("plugin {plugin} requires missing host capability {capability}")]
    MissingHostCapability { plugin: String, capability: String },
    #[error("duplicate tool name: {tool}")]
    DuplicateTool { tool: ToolName },
}

/// The crate-level umbrella error.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ConwayError {
    #[error("backend error: {0}")]
    Backend(#[from] BackendError),
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("routing error: {0}")]
    Routing(#[from] RoutingError),
    #[error("runtime error: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("plugin error: {0}")]
    Plugin(#[from] PluginError),
    #[error("configuration error: {detail}")]
    Config { detail: String },
    #[error("parse error: {detail}")]
    Parse { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{BackendId, ModelId};

    fn model_ref() -> ModelRef {
        ModelRef {
            backend: BackendId::new("local"),
            model: ModelId::new("qwen3-coder:30b"),
        }
    }

    #[test]
    fn context_too_large_exists_and_roundtrips() {
        let err = RoutingError::ContextTooLarge {
            role: RoleAlias::new("planner"),
            model: model_ref(),
            est_tokens: 30_000,
            headroom_tokens: 4_000,
            required_tokens: 34_000,
            max_context_tokens: 32_768,
            shortfall_tokens: 1_232,
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: RoutingError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }

    #[test]
    fn fork_context_overflow_exists_and_roundtrips() {
        let err = RuntimeError::ForkContextOverflow {
            parent: AgentId::new(),
            model: model_ref(),
            est_tokens: 100_000,
            headroom_tokens: 16_000,
            required_tokens: 116_000,
            max_context_tokens: 32_768,
            shortfall_tokens: 83_232,
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: RuntimeError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }

    #[test]
    fn t1_display_names_all_four_numbers() {
        let routing = RoutingError::ContextTooLarge {
            role: RoleAlias::new("planner"),
            model: model_ref(),
            est_tokens: 30_000,
            headroom_tokens: 4_000,
            required_tokens: 34_000,
            max_context_tokens: 32_768,
            shortfall_tokens: 1_232,
        }
        .to_string();
        for needle in [
            "30000",
            "4000",
            "34000",
            "32768",
            "1232",
            "no truncation or escalation",
        ] {
            assert!(
                routing.contains(needle),
                "missing {needle:?} in {routing:?}"
            );
        }

        let runtime = RuntimeError::ForkContextOverflow {
            parent: AgentId::new(),
            model: model_ref(),
            est_tokens: 100_000,
            headroom_tokens: 16_000,
            required_tokens: 116_000,
            max_context_tokens: 32_768,
            shortfall_tokens: 83_232,
        }
        .to_string();
        for needle in ["100000", "16000", "116000", "32768", "83232"] {
            assert!(
                runtime.contains(needle),
                "missing {needle:?} in {runtime:?}"
            );
        }
    }

    #[test]
    fn backend_error_classification() {
        let cases: Vec<(BackendError, bool, bool)> = vec![
            (BackendError::Transport { detail: "x".into() }, true, true),
            (
                BackendError::RateLimit {
                    retry_after_secs: Some(7),
                },
                true,
                true,
            ),
            (
                BackendError::ServerError {
                    status: 503,
                    detail: "x".into(),
                },
                true,
                true,
            ),
            (
                BackendError::ContextOverflow {
                    required_tokens: 2,
                    max_context_tokens: 1,
                },
                true,
                false,
            ),
            (BackendError::Auth { detail: "x".into() }, false, false),
            (
                BackendError::BadRequest { detail: "x".into() },
                false,
                false,
            ),
            (BackendError::ToolParse { detail: "x".into() }, false, false),
            (BackendError::Cancelled, false, false),
        ];
        for (err, failover, health) in cases {
            assert_eq!(err.is_failover_worthy(), failover, "failover for {err:?}");
            assert_eq!(err.is_health_signal(), health, "health for {err:?}");
        }
    }

    #[test]
    fn umbrella_conversions() {
        let e: ConwayError = BackendError::Cancelled.into();
        assert!(matches!(e, ConwayError::Backend(_)));
        let e: ConwayError = RuntimeError::AgentNotFound {
            agent: AgentId::new(),
        }
        .into();
        assert!(matches!(e, ConwayError::Runtime(_)));
        let round: ConwayError = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, round);
    }
}
