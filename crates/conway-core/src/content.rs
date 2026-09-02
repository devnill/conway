//! The conversational substrate: content blocks, messages, tool calls,
//! tool results, tool specs, usage accounting, and sampling parameters.

use std::ops::{Add, AddAssign};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::ToolName;

/// The role a message plays in a conversation.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    ToolResult,
}

/// One block of message content.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
    ToolUse {
        call_id: String,
        name: ToolName,
        arguments: serde_json::Value,
    },
    ToolResultBlock {
        call_id: String,
        blocks: Vec<ContentBlock>,
        is_error: bool,
    },
    Image {
        media_type: String,
        data_base64: String,
    },
}

/// Every [`ContentBlock::Text`] block's text, concatenated in order.
///
/// The ONE narrowing from a block sequence to the plain text a transcript
/// shows. It lives here, in the crate that owns [`ContentBlock`], because
/// two crates need it and neither can call the other: `conway`'s
/// `record_to_event` maps a persisted record to an event when REPLAYING a
/// resumed session, and `conway-runtime`'s `pull_in` maps the identical
/// record to the identical event LIVE when merging a pulled-in `/ask`. The
/// two must agree exactly -- a resumed transcript and a live one showing
/// different text for the same record is the defect -- and the way to make
/// two things agree is one implementation, not two and a comment asking
/// the next person to keep them in step.
///
/// Non-text blocks are dropped: thinking, tool use, tool results and
/// images have their own rendering paths and are not part of the text a
/// transcript line carries.
pub fn assistant_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A role-tagged sequence of content blocks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

/// A tool invocation proposed by the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    pub name: ToolName,
    pub arguments: serde_json::Value,
}

/// The outcome of a tool invocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub tool: ToolName,
    pub blocks: Vec<ContentBlock>,
    pub is_error: bool,
    pub truncated: Option<TruncationRecord>,
}

/// A record that truncation was applied to a tool output.
///
/// The policy's `policy` tag flattens onto the record, so the wire shape is
/// `{"policy":"head_tail","head_bytes":...,"original_bytes":...,"kept_bytes":...}`
/// (architecture §5.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TruncationRecord {
    #[serde(flatten)]
    pub policy: TruncationPolicy,
    pub original_bytes: u64,
    pub kept_bytes: u64,
}

/// How oversized tool output is truncated. A truncation is a context-affecting
/// event: the runtime records it in the log, so where it came from stays answerable.
///
/// **There is deliberately no spill-to-file variant.** An earlier `Artifact`
/// variant promised to spill the full output to an [`Artifact`] and keep a
/// pointer in context, but nothing ever constructed it and the runtime
/// handled it identically to `None` -- the inverse of the promise. It was removed rather than implemented:
/// where to spill, when, the retention/cleanup policy, and whether the
/// preview is head/tail/summary are workload-specific opinions, and policy of that kind
/// puts opinions like that in a hook or plugin, not in this enum.
/// `ToolOutput::artifacts` and [`Artifact`] already give a plugin the type
/// surface to report a spilled file; the seam a spill plugin still needs is
/// a participant point that can *narrow* another tool's output before it
/// reaches context, which does not exist yet
/// (the extension design tracks the gap).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum TruncationPolicy {
    None,
    Head { max_bytes: u64 },
    Tail { max_bytes: u64 },
    HeadTail { head_bytes: u64, tail_bytes: u64 },
}

/// A tool's registration record: name, description, JSON Schema, category,
/// and permission class.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: ToolName,
    pub description: String,
    pub schema: schemars::schema::RootSchema,
    pub category: ToolCategory,
    pub permission: PermissionClass,
}

/// Tool categorization, aligned with ACP's tool-call categories.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Delegate,
}

/// How dangerous a tool is, as declared by the tool itself. The permission
/// broker and the consumer's gate decide what to do with it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionClass {
    Safe,
    RequiresApproval,
    Dangerous,
}

/// A non-prose product of an agent or tool: a file, diff, value, or log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub kind: ArtifactKind,
    pub path: Option<PathBuf>,
    pub media_type: Option<String>,
    pub bytes: Option<u64>,
    pub label: String,
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    File,
    Diff,
    Value,
    Log,
    /// A reference to an ephemeral child session created by `SubagentHost::ask`
    /// (provenance): the artifact points at the ephemeral child's
    /// `SessionId` so the orchestrator's `ToolResultRecord` can name it.
    EphemeralSessionRef,
}

/// Whether a [`Usage`]'s cache figures (`cache_read_tokens`/
/// `cache_write_tokens`) came from a wire response that actually reported
/// caching, or are a zero-filled placeholder because the backend's wire
/// format carries no cache field at all.
///
/// This is a per-response fact, not a backend capability declaration (that
/// is [`crate::capabilities::CacheMode`]): the SAME backend profile can
/// speak two different wire dialects with different cache-reporting
/// honesty (e.g. Ollama's OpenAI-compatible endpoint vs. its native
/// `/api/chat` endpoint), so this lives on `Usage` itself, set by whichever
/// decoder actually read the response.
///
/// Without this distinction, "the provider reported zero cache hits" and
/// "the provider's wire format has no cache field" are indistinguishable:
/// both render as `cache_read_tokens: 0`, which either looks like caching
/// genuinely isn't happening (worth investigating) or silently hides that
/// caching can't be observed at all (nothing to investigate, but the
/// operator has no way to know that).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheAccounting {
    /// The wire response carried a cache field (present, possibly zero).
    #[default]
    Reported,
    /// The wire response carried no cache field at all; `cache_read_tokens`/
    /// `cache_write_tokens` are zero-filled placeholders, not observations.
    NotReported,
}

impl CacheAccounting {
    /// Aggregation rule for [`Add`]/[`AddAssign`]: `NotReported` is sticky.
    /// Once any summand's cache figures are unobservable, the aggregate's
    /// are too -- a mix of `Reported`+`NotReported` cannot honestly claim
    /// `Reported` (the reported half's percentage would understate the
    /// true cache-hit rate by folding in tokens the other half's provider
    /// never accounted for).
    fn combine(self, rhs: Self) -> Self {
        if self == CacheAccounting::NotReported || rhs == CacheAccounting::NotReported {
            CacheAccounting::NotReported
        } else {
            CacheAccounting::Reported
        }
    }
}

/// Token usage accounting. Addable for aggregation across turns and agents.
///
/// `cache_accounting` records whether `cache_read_tokens`/
/// `cache_write_tokens` are real observations (`Reported`, the default --
/// old logs without this field decode as `Reported`, which for pre-existing
/// zero-filled Ollama-native records renders as `0% cached`; see
/// CHANGELOG) or zero-filled placeholders because the backend's wire format
/// has no cache field (`NotReported`). See [`CacheAccounting`]'s own doc.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub reasoning_tokens: u32,
    #[serde(default)]
    pub cache_accounting: CacheAccounting,
}

impl Add for Usage {
    type Output = Usage;

    fn add(self, rhs: Usage) -> Usage {
        Usage {
            input_tokens: self.input_tokens.saturating_add(rhs.input_tokens),
            output_tokens: self.output_tokens.saturating_add(rhs.output_tokens),
            cache_read_tokens: self.cache_read_tokens.saturating_add(rhs.cache_read_tokens),
            cache_write_tokens: self
                .cache_write_tokens
                .saturating_add(rhs.cache_write_tokens),
            reasoning_tokens: self.reasoning_tokens.saturating_add(rhs.reasoning_tokens),
            cache_accounting: self.cache_accounting.combine(rhs.cache_accounting),
        }
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, rhs: Usage) {
        *self = *self + rhs;
    }
}

/// Why the model stopped generating.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Refusal,
}

/// Sampling parameters passed through to the backend.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SamplingParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
    pub seed: Option<u64>,
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_block_tags() {
        let text = ContentBlock::Text { text: "hi".into() };
        let json = serde_json::to_value(&text).unwrap();
        assert_eq!(json["type"], "text");
        let tool = ContentBlock::ToolUse {
            call_id: "tc_1".into(),
            name: ToolName::new("read"),
            arguments: serde_json::json!({"path": "a.txt"}),
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["type"], "tool_use");
    }

    #[test]
    fn tool_spec_roundtrips_with_schema() {
        let schema = schemars::schema_for!(std::collections::BTreeMap<String, String>);
        let spec = ToolSpec {
            name: ToolName::new("read"),
            description: "Read a file".into(),
            schema,
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: ToolSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn usage_aggregates() {
        let a = Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        };
        let b = Usage {
            input_tokens: 1,
            reasoning_tokens: 7,
            ..Default::default()
        };
        let mut c = a;
        c += b;
        assert_eq!(c.input_tokens, 11);
        assert_eq!(c.output_tokens, 5);
        assert_eq!(c.reasoning_tokens, 7);
    }

    /// Old logs (and any wire decoder that never learned about this field)
    /// decode as `Reported` -- `#[serde(default)]` plus `CacheAccounting`'s
    /// own `#[default]` variant. Stated in CHANGELOG: for pre-existing
    /// zero-filled Ollama-native records this renders as `0% cached`, not
    /// `not reported` -- honest enough for a field that did not exist yet.
    #[test]
    fn usage_without_cache_accounting_field_decodes_as_reported() {
        let json = serde_json::json!({
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "reasoning_tokens": 0,
        });
        let usage: Usage = serde_json::from_value(json).unwrap();
        assert_eq!(usage.cache_accounting, CacheAccounting::Reported);
    }

    /// `NotReported` is sticky under aggregation: a session that mixes a
    /// cache-reporting turn with a non-reporting one (e.g. a model switch
    /// mid-session) cannot honestly claim its aggregate cache percentage
    /// is real -- the non-reporting turn's true cache usage, if any, is
    /// unknown and would silently understate the aggregate rate.
    #[test]
    fn cache_accounting_not_reported_is_sticky_under_add() {
        let reported = Usage {
            cache_accounting: CacheAccounting::Reported,
            ..Default::default()
        };
        let not_reported = Usage {
            cache_accounting: CacheAccounting::NotReported,
            ..Default::default()
        };
        assert_eq!(
            (reported + not_reported).cache_accounting,
            CacheAccounting::NotReported
        );
        assert_eq!(
            (not_reported + reported).cache_accounting,
            CacheAccounting::NotReported
        );
        assert_eq!(
            (reported + reported).cache_accounting,
            CacheAccounting::Reported
        );
        assert_eq!(
            (not_reported + not_reported).cache_accounting,
            CacheAccounting::NotReported
        );
    }

    #[test]
    fn sampling_params_default_is_empty() {
        let p = SamplingParams::default();
        assert!(p.temperature.is_none() && p.stop.is_empty() && p.extra.is_empty());
    }
}
