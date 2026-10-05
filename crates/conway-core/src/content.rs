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

/// A single image an operator attaches to their own turn (terminal image
/// attachment): the input-side counterpart of [`ContentBlock::Image`].
/// Distinct from that type because an attachment also carries decoded
/// pixel dimensions for the `[image #N · WxH · size]` chip a caller
/// renders -- a fact [`ContentBlock::Image`] itself has no field for,
/// since the wire format a provider actually reads never needs it.
/// `conway_runtime::runtime::Runtime::prompt_with_images` is the one place
/// this becomes both a `crate::log::LogRecord::UserImage` (for replay/
/// resume) and, via the context builder, a plain [`ContentBlock::Image`]
/// for the outgoing request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachedImage {
    pub media_type: String,
    pub data_base64: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
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

/// Per-model price, USD-per-million-tokens, by token kind -- the
/// `price` sub-object an operator may add to one entry of `models.json`
/// (`conway::config::model_metadata::ModelMetadataEntry::price`). **No
/// bundled defaults ship with conway, for any model**: a price is only ever
/// what an operator configured, never a guess this crate makes on their
/// behalf -- see [`turn_cost`]'s own doc for what an absent price means for
/// a given [`Usage`].
///
/// `cache_read_per_mtok`/`cache_write_per_mtok` are optional even when
/// `input_per_mtok`/`output_per_mtok` are set: a provider that reports cache
/// tokens at all usually prices a cache READ below, and a cache WRITE above,
/// its ordinary input rate, but an operator who has not looked up those two
/// numbers yet can still price the ordinary dimensions -- see [`turn_cost`]
/// for what happens to a cache-relevant token when its own rate is unknown.
///
/// **Only `"USD"` is understood today** (board item `01M1YVRS0K284H9QB32ZZW6D5G`
/// scoped this to one currency, deliberately -- no currency conversion, no
/// budget cap, see that item's own "NOT" list). [`turn_cost`] and
/// [`Price::estimate_input_cost`] both return `None`/produce no figure for
/// any other `currency` value rather than rendering a `$` sign in front of a
/// number that is not actually dollars.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
    #[serde(default = "default_price_currency")]
    pub currency: String,
}

fn default_price_currency() -> String {
    "USD".to_string()
}

/// The only currency [`turn_cost`]/[`Price::estimate_input_cost`] will ever
/// compute a figure for today -- see [`Price`]'s own doc.
pub const SUPPORTED_CURRENCY: &str = "USD";

/// One computed cost figure: an amount in [`Price::currency`]
/// (always `"USD"` today -- see [`Price`]'s own doc), and whether it is
/// exact or an honest over-estimate.
///
/// `approximate` is `true` whenever this figure could not apply a real
/// discount it had reason to believe exists -- see [`turn_cost`]'s own doc
/// for the two cases that set it. It is never `true` merely because the
/// NUMBER is small; a tiny but fully-priced turn is `approximate: false`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub amount: f64,
    pub approximate: bool,
}

impl Cost {
    /// Formats this figure as a `$`-prefixed USD string, prefixed with `≈`
    /// when [`Self::approximate`] is set.
    ///
    /// **Precision rule (stated, not incidental): `>= $0.01` renders 3
    /// decimal places (`$0.012`); below that, 4 decimal places
    /// (`$0.0012`).** A flat 2-decimal rule (ordinary retail-price
    /// formatting) would round a genuinely sub-cent per-turn cost to
    /// `$0.00`, which is indistinguishable from "free" and from "unknown" --
    /// exactly the two things a cost figure exists to tell apart. The extra
    /// decimal below one cent keeps a small-but-real figure visibly nonzero
    /// without carrying that same precision up into the common case, where
    /// a fourth decimal digit is mostly noise.
    pub fn format(&self) -> String {
        let body = if self.amount >= 0.01 {
            format!("${:.3}", self.amount)
        } else {
            format!("${:.4}", self.amount)
        };
        if self.approximate {
            format!("≈{body}")
        } else {
            body
        }
    }
}

impl Price {
    /// Estimates the cost of a NEXT request's input alone (`tokens` priced
    /// at [`Self::input_per_mtok`]) -- the `/context` header's "what would
    /// the next turn cost" figure, computed before any request is sent and
    /// therefore always approximate: it is a prediction over the CURRENT
    /// context size, not an observation of what a backend actually billed.
    /// Always [`Cost::approximate`], and `None` for any `currency` other
    /// than [`SUPPORTED_CURRENCY`] -- see [`Price`]'s own doc.
    pub fn estimate_input_cost(&self, tokens: u64) -> Option<Cost> {
        if self.currency != SUPPORTED_CURRENCY {
            return None;
        }
        Some(Cost {
            amount: (tokens as f64) * self.input_per_mtok / 1_000_000.0,
            approximate: true,
        })
    }
}

/// **The one pricing computation** (board item `01M1YVRS0K284H9QB32ZZW6D5G`
/// -- a single, shared cost-arithmetic implementation): the TUI turn
/// summary, `conway sessions show --cost`, and one-shot `--output-format
/// json` all call this function rather than each re-deriving token-rate
/// arithmetic of their own. `None` when `price`'s `currency` is not
/// [`SUPPORTED_CURRENCY`] -- never a `$`-prefixed figure for a currency this
/// crate does not actually understand.
///
/// `output_per_mtok` prices both `usage.output_tokens` and
/// `usage.reasoning_tokens`: every backend this crate talks to bills a
/// reasoning/thinking token as an output token, and `Price` carries no
/// separate reasoning rate.
///
/// **The decision on cache tokens whose exact rate is unknown, stated here
/// rather than left implicit (two cases, same resolution): cost is shown
/// only when a price is configured and a real discount can be applied,
/// otherwise the figure is marked approximate rather than withheld or
/// guessed.**
///
/// 1. `usage.cache_accounting` is [`CacheAccounting::NotReported`]. The
///    backend's wire format carries no cache field at all, so
///    `cache_read_tokens`/`cache_write_tokens` are zero-filled placeholders,
///    not observations (see [`CacheAccounting`]'s own doc) -- there is
///    nothing to discount in the arithmetic below. But the backend may still
///    have applied a REAL cache discount to what it actually billed, this
///    computation simply has no way to know: charging every input token at
///    the full rate could overstate the true cost. The figure is still
///    computed (never withheld), marked [`Cost::approximate`].
/// 2. `usage.cache_accounting` is [`CacheAccounting::Reported`], genuine
///    cache tokens were processed, but `price` sets no
///    `cache_read_per_mtok`/`cache_write_per_mtok` of its own. Those tokens
///    are priced at the ordinary `input_per_mtok` rate instead (a
///    conservative, never-lower-than-reality substitute: a cache rate is
///    never priced ABOVE the ordinary input rate it discounts -- see
///    [`Price`]'s own doc), and the figure is marked approximate.
///
/// Every other combination -- a price with no cache rates at all and a
/// `Usage` with no cache-relevant tokens, or a price that names both cache
/// rates and a `Usage` that reports them -- produces an EXACT figure.
pub fn turn_cost(usage: &Usage, price: &Price) -> Option<Cost> {
    if price.currency != SUPPORTED_CURRENCY {
        return None;
    }
    const MTOK: f64 = 1_000_000.0;
    let mut approximate = matches!(usage.cache_accounting, CacheAccounting::NotReported);
    let output_like_tokens = f64::from(usage.output_tokens) + f64::from(usage.reasoning_tokens);
    let mut amount = f64::from(usage.input_tokens) * price.input_per_mtok / MTOK
        + output_like_tokens * price.output_per_mtok / MTOK;
    if usage.cache_read_tokens > 0 {
        let rate = match price.cache_read_per_mtok {
            Some(rate) => rate,
            None => {
                approximate = true;
                price.input_per_mtok
            }
        };
        amount += f64::from(usage.cache_read_tokens) * rate / MTOK;
    }
    if usage.cache_write_tokens > 0 {
        let rate = match price.cache_write_per_mtok {
            Some(rate) => rate,
            None => {
                approximate = true;
                price.input_per_mtok
            }
        };
        amount += f64::from(usage.cache_write_tokens) * rate / MTOK;
    }
    Some(Cost { amount, approximate })
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

    fn priced(
        input: f64,
        output: f64,
        cache_read: Option<f64>,
        cache_write: Option<f64>,
    ) -> Price {
        Price {
            input_per_mtok: input,
            output_per_mtok: output,
            cache_read_per_mtok: cache_read,
            cache_write_per_mtok: cache_write,
            currency: SUPPORTED_CURRENCY.to_string(),
        }
    }

    /// The exact case: every dimension `usage` reports has a configured
    /// rate, including the cache-read discount -- the figure must be exact
    /// arithmetic, not approximate.
    #[test]
    fn turn_cost_applies_the_cache_read_discount_when_reported_and_priced() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_read_tokens: 2_000_000,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cache_accounting: CacheAccounting::Reported,
        };
        let price = priced(3.0, 15.0, Some(0.3), Some(3.75));
        let cost = turn_cost(&usage, &price).expect("priced model must compute a cost");
        // 1M in @ $3/Mtok = $3.00; 500k out @ $15/Mtok = $7.50;
        // 2M cache-read @ $0.30/Mtok = $0.60. Total $11.10.
        assert!(
            (cost.amount - 11.10).abs() < 1e-9,
            "expected 11.10, got {}",
            cost.amount
        );
        assert!(!cost.approximate, "every dimension was priced: must be exact");
    }

    /// Reasoning tokens are billed at the OUTPUT rate (no separate price
    /// field for them).
    #[test]
    fn turn_cost_bills_reasoning_tokens_at_the_output_rate() {
        let usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 1_000_000,
            cache_accounting: CacheAccounting::Reported,
        };
        let price = priced(1.0, 20.0, None, None);
        let cost = turn_cost(&usage, &price).unwrap();
        assert!((cost.amount - 20.0).abs() < 1e-9);
        assert!(!cost.approximate);
    }

    /// Case 1 of the cache-discount decision on `turn_cost`'s own doc:
    /// `CacheAccounting::NotReported` zero-fills the cache
    /// fields, so there is nothing to discount in the arithmetic -- but the
    /// figure must still be marked approximate, since a real discount this
    /// computation cannot see may already be baked into what the backend
    /// actually billed.
    #[test]
    fn turn_cost_marks_not_reported_cache_accounting_as_approximate() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cache_accounting: CacheAccounting::NotReported,
        };
        let price = priced(3.0, 15.0, Some(0.3), Some(3.75));
        let cost = turn_cost(&usage, &price).unwrap();
        assert!((cost.amount - 3.0).abs() < 1e-9);
        assert!(
            cost.approximate,
            "NotReported must mark the figure approximate even with nothing to discount"
        );
    }

    /// Case 2 of that same decision: `Reported` cache tokens with NO configured cache
    /// rate fall back to the ordinary input rate, and the figure is marked
    /// approximate (the backend's real cache discount is unknown).
    #[test]
    fn turn_cost_falls_back_to_input_rate_for_unpriced_reported_cache_tokens() {
        let usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cache_accounting: CacheAccounting::Reported,
        };
        let price = priced(3.0, 15.0, None, None);
        let cost = turn_cost(&usage, &price).unwrap();
        assert!((cost.amount - 3.0).abs() < 1e-9, "priced at the input rate");
        assert!(cost.approximate);
    }

    #[test]
    fn turn_cost_is_none_for_an_unsupported_currency() {
        let usage = Usage {
            input_tokens: 100,
            ..Default::default()
        };
        let mut price = priced(3.0, 15.0, None, None);
        price.currency = "EUR".to_string();
        assert!(turn_cost(&usage, &price).is_none());
    }

    #[test]
    fn cost_format_uses_three_decimals_at_or_above_one_cent_and_four_below() {
        assert_eq!(
            Cost {
                amount: 0.012,
                approximate: false
            }
            .format(),
            "$0.012"
        );
        assert_eq!(
            Cost {
                amount: 0.0012,
                approximate: false
            }
            .format(),
            "$0.0012"
        );
        assert_eq!(
            Cost {
                amount: 0.012,
                approximate: true
            }
            .format(),
            "≈$0.012"
        );
    }

    #[test]
    fn price_estimate_input_cost_is_always_approximate() {
        let price = priced(3.0, 15.0, None, None);
        let cost = price.estimate_input_cost(2_000_000).unwrap();
        assert!((cost.amount - 6.0).abs() < 1e-9);
        assert!(cost.approximate);
    }

    #[test]
    fn price_estimate_input_cost_is_none_for_an_unsupported_currency() {
        let mut price = priced(3.0, 15.0, None, None);
        price.currency = "EUR".to_string();
        assert!(price.estimate_input_cost(1_000).is_none());
    }
}
