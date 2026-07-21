//! `ContextBuilder`: assembles one agent's request context in the fixed
//! architecture §5.3 order, with complete provenance and non-correctness-
//! bearing cache hints. Pure over already-resolved records — no I/O, no
//! `async`; ancestry resolution is the caller's job (WI-084).
//!
//! ## `SegmentId` determinism
//!
//! [`conway_core::segment::PromptSegment::new`] assigns a fresh random
//! `SegmentId` (`ulid::Ulid::new()`), which cannot satisfy two binding
//! criteria at once: golden-file byte-equality across test runs, and the
//! cache-neutrality property (`strip_cache_hints(build(input))` must equal
//! `build(input_with_cache_disabled)` on the `(id, role, content,
//! provenance)` tuple, which requires the *same* `id` from two independent
//! `build` calls). This builder therefore **overwrites every segment's id**
//! with a value deterministically derived from `blake3(agent_id ‖ ordinal
//! ‖ provenance_discriminant ‖ content_hash)`, reinterpreted as a
//! `ulid::Ulid` (see [`derive_segment_id`]). The hash input never includes
//! `cache_mode`, so cache-neutrality holds by construction, not by
//! coincidence.
//!
//! Golden files additionally project segments through a `GoldenSegment`
//! struct (in `tests/context_golden.rs`) that omits `id` entirely, so
//! byte-equality does not depend on the derivation formula staying fixed
//! forever — only on it staying deterministic.
//!
//! ## A documented interpretation gap: assistant-turn provenance
//!
//! `conway_core::provenance::Provenance` is exhaustively nine variants
//! (enforced by that crate's own tests) and none of them represents "the
//! model's own prior turn" — `LogRecord::Assistant` does not even carry a
//! `prov` field. Architecture §5.3's own-records mapping table names a
//! provenance for `tool_result`, `parent_steer`, and system notes, but only
//! a role (`Role::Assistant`) for assistant turns, not a provenance. Since
//! `PromptSegment::provenance` is mandatory and this crate cannot add a
//! tenth `Provenance` variant (out of `conway-runtime`'s scope), assistant
//! turns are mapped to `Provenance::SystemNote { reason: "assistant_turn"
//! }` — the closest existing volatile-tier variant, using a `reason`
//! sentinel that collides with no other component's matching (WI-086 only
//! matches `"repeated_step"` and `"result_contract_violation"`). This is a
//! placeholder, not a design decision: it should be raised against
//! `MODULE:conway-core` as a request for a dedicated `Provenance::Assistant`
//! (or similar) variant.

use std::sync::Arc;

use conway_core::capabilities::CacheMode;
use conway_core::content::{ContentBlock, Role, ToolSpec};
use conway_core::error::RuntimeError;
use conway_core::ids::{AgentId, ModelId, PrefixKey, SegmentId, SeqRange, SessionId};
use conway_core::log::LogRecord;
use conway_core::provenance::{ContextReport, ContextReportEntry, Provenance};
use conway_core::segment::{CacheHint, CacheTtl, PromptSegment};

use super::prefix::{self, canonical_json_bytes};

/// Name of the estimator recorded on every [`ContextReport`] this builder
/// produces (T-9: never present a token estimate as exact without naming
/// what produced it).
///
/// `conway_core::provenance::ContextReport` has no dedicated `estimator`
/// field (only `tokenizer: String`); the criterion `report.estimator ==
/// "heuristic-chars4"` is satisfied via that field instead, since adding a
/// field is out of this crate's scope. Raise against `MODULE:conway-core`
/// if a dedicated field is wanted.
pub const TOKEN_ESTIMATOR: &str = "heuristic-chars4";

/// The system prompt sourced from an `AgentDef`.
#[derive(Clone, Debug)]
pub struct SystemPromptSpec {
    pub agent_def: String,
    pub text: String,
}

/// One skill fragment injected into context, in caller-supplied (stable)
/// order.
#[derive(Clone, Debug)]
pub struct SkillFragment {
    pub name: String,
    pub text: String,
}

/// A verbatim prefix inherited from a parent session at fork time
/// (architecture §5.1, §5.2).
#[derive(Clone, Debug)]
pub struct InheritedPrefix {
    pub from: SessionId,
    pub seq_range: SeqRange,
    pub records: Arc<[LogRecord]>,
}

/// The segment that follows the static/inherited prefix: either a fork
/// directive (Fork mode) or the whole prompt (Spawn mode, or a root
/// agent's first turn).
#[derive(Clone, Debug)]
pub enum HeadSegment {
    ForkDirective { text: String, by: AgentId },
    Prompt { text: String },
}

/// Pure input to [`ContextBuilder::build`] — already-resolved records, no
/// store dependency. Ancestry resolution (`TranscriptResolver`) is the
/// caller's job (WI-084); this builder never touches a store.
///
/// `turn` is not part of the WI-077 spec's illustrative struct but is
/// required to populate `ContextReport::turn`; added here since
/// `ContextReport` is otherwise unconstructable.
#[derive(Clone, Debug)]
pub struct ContextInput {
    pub agent_id: AgentId,
    pub turn: u32,
    pub model: ModelId,
    pub cache_mode: CacheMode,
    pub system_prompt: Option<SystemPromptSpec>,
    pub skills: Vec<SkillFragment>,
    pub tools: Vec<ToolSpec>,
    pub inherited: Option<InheritedPrefix>,
    pub head: HeadSegment,
    pub own: Arc<[LogRecord]>,
    pub cache_ttl: CacheTtl,
}

/// Assembles an agent's request context in the fixed architecture §5.3
/// order, with complete provenance and cache hints. Stateless; safe to
/// share across agents.
///
/// `PromptSegment` cannot be constructed without stating its
/// [`Provenance`] — there is no `Default` impl to fall back on (GP-10;
/// enforced in `conway-core` by a `static_assertions` guard on
/// `PromptSegment`). The following fails to compile:
///
/// ```compile_fail
/// let _segment: conway_core::segment::PromptSegment = Default::default();
/// ```
#[derive(Debug, Default)]
pub struct ContextBuilder;

impl ContextBuilder {
    pub fn new() -> Self {
        Self
    }

    /// Assemble `input` into `(segments, report)`. Pure: no I/O, no
    /// `async`. The `Result` is part of the binding signature (matching
    /// the attempt/route/tool layers this feeds); nothing in the current
    /// implementation can fail, since every input type here always
    /// serializes successfully.
    #[allow(clippy::unnecessary_wraps)]
    pub fn build(
        &self,
        input: &ContextInput,
    ) -> Result<(Vec<PromptSegment>, ContextReport), RuntimeError> {
        let mut segments: Vec<PromptSegment> = Vec::new();

        // [0] SystemPrompt
        if let Some(system_prompt) = &input.system_prompt {
            segments.push(PromptSegment::new(
                Role::System,
                text_block(&system_prompt.text),
                Provenance::AgentDef {
                    name: system_prompt.agent_def.clone(),
                },
            ));
        }

        // [1] SkillFragments*
        for skill in &input.skills {
            segments.push(PromptSegment::new(
                Role::System,
                text_block(&skill.text),
                Provenance::Skill {
                    name: skill.name.clone(),
                },
            ));
        }

        // [2] ToolSchemas — unconditional; breakpoint A attaches here.
        let mut sorted_tools = input.tools.clone();
        sorted_tools.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        let tools_value = serde_json::to_value(&sorted_tools).expect("ToolSpec always serializes");
        let canonical = canonical_json_bytes(&tools_value);
        let hash = blake3::hash(&canonical).to_hex().to_string();
        let tool_schemas_text =
            String::from_utf8(canonical).expect("canonical JSON is always valid UTF-8");
        segments.push(PromptSegment::new(
            Role::System,
            text_block(&tool_schemas_text),
            Provenance::ToolRegistry { hash },
        ));
        let a_index = segments.len() - 1;

        // [3] InheritedPrefix* — one segment per record, order preserved;
        // breakpoint B attaches to the last one, if any exist.
        let mut b_index = None;
        if let Some(inherited) = &input.inherited {
            for record in inherited.records.iter() {
                let Some((role, content)) = record_role_and_content(record) else {
                    continue;
                };
                let seq = record
                    .seq()
                    .expect("inherited records always carry a seq (Header never appears here)");
                segments.push(PromptSegment::new(
                    role,
                    content,
                    Provenance::Inherited {
                        from: inherited.from,
                        seq_range: SeqRange::new(seq, Some(seq.succ())),
                    },
                ));
            }
            if segments.len() > a_index + 1 {
                b_index = Some(segments.len() - 1);
            }
        }

        // [4] ForkDirective | Prompt
        match &input.head {
            HeadSegment::ForkDirective { text, by } => segments.push(PromptSegment::new(
                Role::User,
                text_block(text),
                Provenance::ForkDirective { by: *by },
            )),
            HeadSegment::Prompt { text } => segments.push(PromptSegment::new(
                Role::User,
                text_block(text),
                Provenance::UserPrompt,
            )),
        }

        // [5..] own records — volatile.
        for record in input.own.iter() {
            if let Some((role, content, provenance)) = own_segment(record) {
                segments.push(PromptSegment::new(role, content, provenance));
            }
        }

        // Deterministic ids + token estimates. Runs before cache-hint
        // attachment so the hash inputs never include a cache_hint.
        for (ordinal, segment) in segments.iter_mut().enumerate() {
            segment.id = derive_segment_id(
                input.agent_id,
                ordinal,
                &segment.provenance,
                &segment.content,
            );
            segment.tokens_est = Some(estimate_tokens(&segment.content));
        }

        // Cache hints — never correctness-bearing (GP-06).
        let key = prefix::prefix_key(&input.model, &segments);
        attach_cache_hints(
            &mut segments,
            &input.cache_mode,
            input.cache_ttl,
            a_index,
            b_index,
            &key,
        );

        let entries: Vec<ContextReportEntry> = segments
            .iter()
            .map(|segment| ContextReportEntry {
                segment: segment.id,
                provenance: segment.provenance.clone(),
                tokens_est: segment.tokens_est.unwrap_or(0),
                estimated: true,
            })
            .collect();
        let total_tokens_est = entries.iter().map(|entry| entry.tokens_est).sum();

        let report = ContextReport {
            agent_id: input.agent_id,
            turn: input.turn,
            tokenizer: TOKEN_ESTIMATOR.to_string(),
            segments: entries,
            total_tokens_est,
        };

        Ok((segments, report))
    }
}

fn text_block(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: text.to_string(),
    }]
}

/// Generic `(role, content)` extraction used for inherited records, whose
/// original provenance is discarded and replaced with
/// `Provenance::Inherited` regardless of record kind.
fn record_role_and_content(record: &LogRecord) -> Option<(Role, Vec<ContentBlock>)> {
    match record {
        LogRecord::UserTurn { text, .. } => Some((Role::User, text_block(text))),
        LogRecord::Assistant { content, .. } => Some((Role::Assistant, content.clone())),
        LogRecord::ToolResultRecord { result, .. } => {
            Some((Role::ToolResult, result.blocks.clone()))
        }
        LogRecord::ForkDirective { text, .. } => Some((Role::User, text_block(text))),
        LogRecord::ParentSteer { text, .. } => Some((Role::User, text_block(text))),
        LogRecord::SystemNote { text, .. } => Some((Role::System, text_block(text))),
        LogRecord::Header(_)
        | LogRecord::ToolCallRecord { .. }
        | LogRecord::AgentResultRecord { .. }
        | LogRecord::ContextReportRecord { .. } => None,
        // `LogRecord` is `#[non_exhaustive]`; an unrecognized future kind
        // is dropped rather than fed into context.
        _ => None,
    }
}

/// Own (volatile) record mapping, per architecture §5.3: provenance
/// derived from record kind. See the module doc for the `Assistant` gap.
fn own_segment(record: &LogRecord) -> Option<(Role, Vec<ContentBlock>, Provenance)> {
    match record {
        LogRecord::UserTurn { text, .. } => {
            Some((Role::User, text_block(text), Provenance::UserPrompt))
        }
        LogRecord::Assistant { content, .. } => Some((
            Role::Assistant,
            content.clone(),
            Provenance::SystemNote {
                reason: "assistant_turn".to_string(),
            },
        )),
        LogRecord::ToolResultRecord { result, .. } => Some((
            Role::ToolResult,
            result.blocks.clone(),
            Provenance::ToolResult {
                call_id: result.call_id.clone(),
                tool: result.tool.clone(),
            },
        )),
        LogRecord::ForkDirective { text, by, .. } => Some((
            Role::User,
            text_block(text),
            Provenance::ForkDirective { by: *by },
        )),
        LogRecord::ParentSteer {
            text,
            from,
            parent_seq,
            ..
        } => Some((
            Role::User,
            text_block(text),
            Provenance::ParentSteer {
                from: *from,
                parent_seq: *parent_seq,
            },
        )),
        LogRecord::SystemNote { text, reason, .. } => Some((
            Role::System,
            text_block(text),
            Provenance::SystemNote {
                reason: reason.clone(),
            },
        )),
        LogRecord::Header(_)
        | LogRecord::ToolCallRecord { .. }
        | LogRecord::AgentResultRecord { .. }
        | LogRecord::ContextReportRecord { .. } => None,
        _ => None,
    }
}

/// `ceil(utf8_len / 4)` over the segment's content, serialized to JSON.
/// Explicitly approximate (T-9); never presented as an exact count.
fn estimate_tokens(content: &[ContentBlock]) -> u32 {
    let json = serde_json::to_string(content).expect("content always serializes");
    (json.len() as u32).div_ceil(4)
}

/// `blake3(agent_id ‖ ordinal ‖ provenance_discriminant ‖ content_hash)`,
/// reinterpreted as a `ulid::Ulid` — see the module doc.
/// NOTE (cycle-1 review S2): unlike `SessionId`/`AgentId` (fresh ULIDs,
/// chronologically sortable), a derived `SegmentId` is a blake3-based
/// deterministic id reinterpreted as a Ulid — it does NOT sort by creation
/// time. Never order segments by id; order is the Vec's order.
fn derive_segment_id(
    agent_id: AgentId,
    ordinal: usize,
    provenance: &Provenance,
    content: &[ContentBlock],
) -> SegmentId {
    let content_value = serde_json::to_value(content).expect("content always serializes");
    let content_hash = blake3::hash(&canonical_json_bytes(&content_value));

    let mut hasher = blake3::Hasher::new();
    hasher.update(agent_id.to_string().as_bytes());
    hasher.update(&(ordinal as u64).to_le_bytes());
    hasher.update(provenance_discriminant(provenance).as_bytes());
    hasher.update(content_hash.as_bytes());
    let digest = hasher.finalize();

    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    SegmentId(ulid::Ulid::from(u128::from_be_bytes(bytes)))
}

fn provenance_discriminant(provenance: &Provenance) -> &'static str {
    match provenance {
        Provenance::UserPrompt => "user_prompt",
        Provenance::AgentDef { .. } => "agent_def",
        Provenance::Skill { .. } => "skill",
        Provenance::ToolRegistry { .. } => "tool_registry",
        Provenance::Inherited { .. } => "inherited",
        Provenance::ForkDirective { .. } => "fork_directive",
        Provenance::ParentSteer { .. } => "parent_steer",
        Provenance::ToolResult { .. } => "tool_result",
        Provenance::SystemNote { .. } => "system_note",
        _ => "unknown",
    }
}

/// Breakpoint indices in trim priority order: `B` (last `InheritedPrefix`,
/// if any) before `A` (last `ToolSchemas`) — architecture §5.3: "priority
/// order when trimming is B > A > everything else".
fn desired_breakpoints(a_index: usize, b_index: Option<usize>) -> Vec<usize> {
    match b_index {
        Some(b) => vec![b, a_index],
        None => vec![a_index],
    }
}

/// Attach cache hints per architecture §5.3. `ExplicitBreakpoints` and
/// `SlotKv` get hints (trimmed to `max_breakpoints` for the former, on
/// priority order B > A); `ImplicitPrefix` and `None` get none, since
/// ordering alone produces hits for those backends.
fn attach_cache_hints(
    segments: &mut [PromptSegment],
    cache_mode: &CacheMode,
    ttl: CacheTtl,
    a_index: usize,
    b_index: Option<usize>,
    key: &PrefixKey,
) {
    let max_breakpoints = match cache_mode {
        CacheMode::ExplicitBreakpoints {
            max_breakpoints, ..
        } => Some(*max_breakpoints as usize),
        CacheMode::SlotKv => None,
        CacheMode::ImplicitPrefix { .. } | CacheMode::None => return,
        // `CacheMode` is `#[non_exhaustive]`: an unrecognized future mode
        // gets no hints rather than a guess.
        _ => return,
    };

    let desired = desired_breakpoints(a_index, b_index);
    let keep: Vec<usize> = match max_breakpoints {
        Some(max) => desired.into_iter().take(max).collect(),
        None => desired,
    };

    for index in keep {
        segments[index].cache_hint = Some(CacheHint {
            breakpoint: true,
            ttl,
            prefix_key: key.clone(),
        });
    }
}
