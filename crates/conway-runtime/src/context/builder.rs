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
//! `conway_core::provenance::Provenance` is exhaustively ten variants
//! (enforced by that crate's own tests; the original §5.3 nine plus
//! `MergedAsk`, added by B4) and none of them represents "the
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
use conway_core::content::{ContentBlock, Role, ToolResult, ToolSpec};
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
    /// The IMMEDIATE ancestor this bundle was inherited from — "who handed
    /// me this context" — NOT the original author of every record in
    /// `records`. At fork depth >= 2, `records` is the WHOLE effective
    /// transcript up to the fork point (GP-02: root's own records, then
    /// every intermediate ancestor's own records in turn, through the
    /// immediate parent's), per `conway_session::TranscriptResolver`'s
    /// "inherited prefix always flows through in full" contract — but
    /// every one of those records, however deep its true origin, is
    /// stamped with this single `from` when `ContextBuilder` turns
    /// `records` into `Provenance::Inherited` segments (see this crate's
    /// `subagent.rs` module doc, "`InheritedPrefix::from` at fork depth >=
    /// 2", for the full rationale). Recovering true per-record authorship
    /// at arbitrary depth would require per-record session tracking that
    /// does not exist upstream (in `conway_core::log::LogRecord` or in
    /// `conway_session`'s resolver) — out of this item's scope; queued as a
    /// refinement question rather than attempted here (coordinator ruling,
    /// WI-084 rework).
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

        let report = build_report(input.agent_id, input.turn, &segments);

        Ok((segments, report))
    }
}

/// Re-derives a `ContextReport` from a (possibly `ContextHook`-transformed)
/// segment list: recomputes every segment's `tokens_est` and rebuilds the
/// report entries in the given order. WI-126: a hook may add, edit, or drop
/// segments after `ContextBuilder::build` -- content is the only thing that
/// can have changed, so re-estimating every segment (not just ones with
/// `tokens_est: None`) is the only correct way to keep `tokens_est`/
/// `total_tokens_est` honest, including for a segment whose `content` a hook
/// edited in place without clearing its stale estimate. Does NOT touch
/// `segment.id` (`derive_segment_id` is a pure function of the ORIGINAL
/// assembly's `(agent_id, ordinal, provenance, content)`; a hook-added
/// segment simply keeps whatever id `PromptSegment::new` gave it -- this
/// builder makes no determinism claim about a hook's own output) or cache
/// hints (a hook that cares about cache-breakpoint placement is responsible
/// for its own `cache_hint`).
pub(crate) fn retotal(
    agent_id: AgentId,
    turn: u32,
    segments: &mut [PromptSegment],
) -> ContextReport {
    for segment in segments.iter_mut() {
        segment.tokens_est = Some(estimate_tokens(&segment.content));
    }
    build_report(agent_id, turn, segments)
}

fn build_report(agent_id: AgentId, turn: u32, segments: &[PromptSegment]) -> ContextReport {
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

    ContextReport {
        agent_id,
        turn,
        tokenizer: TOKEN_ESTIMATOR.to_string(),
        segments: entries,
        total_tokens_est,
    }
}

fn text_block(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: text.to_string(),
    }]
}

/// Wraps a recorded `ToolResult` as the single `ContentBlock::ToolResultBlock`
/// a `Role::ToolResult` segment must carry (WI-137). Both wire adapters
/// (`openai_compat::tool_result_messages` and `anthropic::tool_result_blocks`)
/// serialize a tool result ONLY from a `ToolResultBlock` -- it is the block
/// that carries the `call_id` tying the result back to its `tool_use` /
/// `tool_call`. The tool runner produces raw `Text` blocks, so before this
/// wrapping the segment held bare `Text`, matched neither wire adapter, and the
/// tool result was silently dropped from every request: the model saw its own
/// tool call (WI-122) but never the output, so it confabulated an answer.
fn tool_result_block(result: &ToolResult) -> Vec<ContentBlock> {
    vec![ContentBlock::ToolResultBlock {
        call_id: result.call_id.clone(),
        blocks: result.blocks.clone(),
        is_error: result.is_error,
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
            Some((Role::ToolResult, tool_result_block(result)))
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
        LogRecord::UserTurn { text, prov, .. } => {
            // B4: honor the STORED provenance rather than deriving it from
            // record kind alone — a `Conway::pull_in`-merged `/ask` question
            // is a `UserTurn` whose `prov` is `Provenance::MergedAsk { from
            // }`, and kind-derivation mislabeled it `UserPrompt` in
            // `/context`, erasing the merge origin (P-2/GP-10). Records
            // written before `MergedAsk` existed all carry
            // `Provenance::UserPrompt` here (the field is mandatory on the
            // wire), so old-record behavior is preserved by construction
            // (C-04).
            Some((Role::User, text_block(text), prov.clone()))
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
            tool_result_block(result),
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

/// Fixed per-block overhead (in tokens) added to every content block's own
/// char-count estimate, standing in for the wire-format framing (role/type
/// tags) a real tokenizer spends a handful of tokens on that a pure
/// character count would otherwise miss entirely. Deliberately small next to
/// a typical JSON-serialized block's own structural overhead (field names,
/// quoting, escaping) -- see the module doc's WI-126 note on why this
/// estimator no longer serializes the whole block to JSON first.
const PER_BLOCK_OVERHEAD_TOKENS: u32 = 4;

/// Heuristic token estimate (T-9: explicitly approximate, never presented as
/// an exact count) over a segment's actual text/content payload, NOT its
/// JSON serialization. WI-126: the prior formula (`json.len() / 4` over
/// `serde_json::to_string(content)`) counted every content block's field
/// names, `{}`/`[]`/`,` punctuation, and string-escaping once per block --
/// structural overhead a real tokenizer never spends tokens on -- which
/// inflated the estimate most for payloads with many small blocks (exactly
/// the "structurally-heavy" case this heuristic most needs to get right,
/// since that is what an overflow/curation hook is judging). This version
/// sums each block's own meaningful payload length (`ceil(chars / 4)`) plus
/// a small fixed per-block overhead, still a heuristic (no tokenizer
/// dependency), just one that scales with content rather than with JSON
/// framing.
fn estimate_tokens(content: &[ContentBlock]) -> u32 {
    content.iter().map(estimate_block_tokens).sum()
}

fn estimate_block_tokens(block: &ContentBlock) -> u32 {
    (block_payload_chars(block) as u32).div_ceil(4) + PER_BLOCK_OVERHEAD_TOKENS
}

/// The block's own meaningful character payload -- prose for `Text`/
/// `Thinking`, name + compactly-serialized arguments for `ToolUse` (still
/// JSON, but the tool's actual structured payload, not incidental
/// content-block framing around it), recursively summed nested blocks for
/// `ToolResultBlock`, and the encoded bytes for `Image`.
fn block_payload_chars(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::Thinking { text, signature } => {
            text.len() + signature.as_deref().map_or(0, str::len)
        }
        ContentBlock::ToolUse {
            name, arguments, ..
        } => {
            name.as_str().len()
                + serde_json::to_string(arguments)
                    .map(|s| s.len())
                    .unwrap_or(0)
        }
        ContentBlock::ToolResultBlock { blocks, .. } => {
            blocks.iter().map(block_payload_chars).sum()
        }
        ContentBlock::Image { data_base64, .. } => data_base64.len(),
        // `ContentBlock` is `#[non_exhaustive]`: an unrecognized future
        // variant falls back to its full JSON length rather than silently
        // costing 0 -- an overestimate is the safe direction here.
        other => serde_json::to_string(other).map(|s| s.len()).unwrap_or(0),
    }
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
        Provenance::MergedAsk { .. } => "merged_ask",
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

/// Re-derives the A/B breakpoint indices from segment PROVENANCE alone,
/// rather than from positions tracked during assembly: A is the last
/// `Provenance::ToolRegistry` segment, B is the last `Provenance::Inherited`
/// segment — the same rule `build` applies inline while it still has the
/// positions on hand (see `breakpoint_indices_tests` for direct coverage of
/// this function against `build`'s own output).
///
/// This is what makes the model-aware cache-hint post-pass in
/// `attempt.rs` (run AFTER routing resolves a concrete model, and after any
/// `ContextHook::before_request` has had a chance to add, drop, or reorder
/// segments — WI-126) correct even when the hook has changed segment
/// positions since `build` ran: re-deriving from provenance on the FINAL
/// segment list is safe against staleness in a way that threading `build`-
/// time indices through would not be. `a_index` is `None` only if a hook
/// dropped the (normally unconditional) `ToolSchemas` segment — in that
/// case there is no A to breakpoint on, so the caller attaches no hints at
/// all rather than guessing.
pub(crate) fn breakpoint_indices(segments: &[PromptSegment]) -> (Option<usize>, Option<usize>) {
    let a_index = segments
        .iter()
        .rposition(|segment| matches!(segment.provenance, Provenance::ToolRegistry { .. }));
    let b_index = segments
        .iter()
        .rposition(|segment| matches!(segment.provenance, Provenance::Inherited { .. }));
    (a_index, b_index)
}

/// Attach cache hints per architecture §5.3. `ExplicitBreakpoints` and
/// `SlotKv` get hints (trimmed to `max_breakpoints` for the former, on
/// priority order B > A); `ImplicitPrefix` and `None` get none, since
/// ordering alone produces hits for those backends.
///
/// `pub(crate)`: also called from `attempt.rs`'s post-routing cache-hint
/// post-pass, keyed on the ACTUALLY resolved model's capability (see that
/// module's doc) — this builder's own call, inside [`ContextBuilder::build`],
/// only ever sees a placeholder pre-routing `CacheMode` (`CacheMode::None`
/// at every production call site today) and so never marks anything itself
/// in practice; the post-pass is where a live turn's hints actually get
/// attached.
pub(crate) fn attach_cache_hints(
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

#[cfg(test)]
mod estimator_tests {
    use super::*;
    use conway_core::content::Role;
    use conway_core::provenance::Provenance;

    /// The formula this heuristic replaced (WI-126): the whole content
    /// array's JSON serialization, divided by 4. Reproduced here (not
    /// exported) purely so the "more accurate" claim has a concrete
    /// baseline to compare against.
    fn old_json_div4_estimate(content: &[ContentBlock]) -> u32 {
        let json = serde_json::to_string(content).expect("content always serializes");
        (json.len() as u32).div_ceil(4)
    }

    /// Criterion (d): for a structurally-heavy payload (many small blocks,
    /// where JSON framing -- `{"type":"text","text":...}` repeated per
    /// block -- dominates the actual prose), the new estimate must come out
    /// lower than the old json-length/4 formula, since it no longer counts
    /// that framing at all.
    #[test]
    fn new_estimate_is_lower_than_old_json_div4_for_many_small_blocks() {
        let content: Vec<ContentBlock> = (0..50)
            .map(|i| ContentBlock::Text {
                text: format!("x{i}"),
            })
            .collect();

        let old = old_json_div4_estimate(&content);
        let new = estimate_tokens(&content);

        assert!(
            new < old,
            "expected new estimate ({new}) < old json/4 estimate ({old}) for a \
             structurally-heavy (many small blocks) payload"
        );
    }

    #[test]
    fn estimate_scales_with_actual_text_length_not_json_framing() {
        let short = vec![ContentBlock::Text {
            text: "hi".to_string(),
        }];
        let long = vec![ContentBlock::Text {
            text: "a".repeat(400),
        }];
        assert!(estimate_tokens(&long) > estimate_tokens(&short));
        // 400 chars / 4 = 100 tokens, plus the fixed per-block overhead --
        // not inflated by any JSON quoting/escaping of the block itself.
        assert_eq!(estimate_tokens(&long), 100 + PER_BLOCK_OVERHEAD_TOKENS);
    }

    #[test]
    fn tool_use_counts_name_and_compact_arguments_not_raw_content_json() {
        let block = ContentBlock::ToolUse {
            call_id: "call_1".to_string(),
            name: conway_core::ids::ToolName::new("read"),
            arguments: serde_json::json!({"path": "/tmp/x"}),
        };
        // "read" (4) + compact-serialized {"path":"/tmp/x"} (18 chars).
        let expected_chars = "read".len()
            + serde_json::to_string(&serde_json::json!({"path": "/tmp/x"}))
                .unwrap()
                .len();
        assert_eq!(
            estimate_block_tokens(&block),
            (expected_chars as u32).div_ceil(4) + PER_BLOCK_OVERHEAD_TOKENS
        );
    }

    fn segment(text: &str) -> PromptSegment {
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            Provenance::UserPrompt,
        )
    }

    #[test]
    fn retotal_recomputes_every_segment_even_with_a_stale_estimate() {
        let agent_id = AgentId::new();
        let mut segments = vec![segment("hello")];
        // Simulate a hook editing content in place without clearing the
        // estimate `build` had already set.
        segments[0].tokens_est = Some(9_999);
        segments[0].content = vec![ContentBlock::Text {
            text: "a".repeat(40),
        }];

        let report = retotal(agent_id, 3, &mut segments);

        let expected = estimate_tokens(&segments[0].content);
        assert_eq!(segments[0].tokens_est, Some(expected));
        assert_eq!(report.total_tokens_est, expected);
        assert_eq!(report.turn, 3);
        assert_eq!(report.agent_id, agent_id);
    }

    #[test]
    fn retotal_reflects_a_hook_dropping_a_segment() {
        let agent_id = AgentId::new();
        let mut segments = vec![segment("keep"), segment("drop")];
        segments.retain(|s| match &s.content[0] {
            ContentBlock::Text { text } => text == "keep",
            _ => true,
        });

        let report = retotal(agent_id, 0, &mut segments);
        assert_eq!(report.segments.len(), 1);
    }
}

#[cfg(test)]
mod own_segment_provenance_tests {
    use super::*;
    use chrono::Utc;
    use conway_core::ids::LogSeq;

    fn user_turn(prov: Provenance) -> LogRecord {
        LogRecord::UserTurn {
            seq: LogSeq(0),
            ts: Utc::now(),
            text: "the merged question".into(),
            prov,
        }
    }

    /// B4: a `Conway::pull_in`-merged `/ask` question lands in the parent's
    /// log as a `UserTurn` stamped `Provenance::MergedAsk { from }` — and
    /// `own_segment` must surface THAT stored provenance (so `/context`'s
    /// report shows `merged_ask`, naming the purged child session), not the
    /// kind-derived `UserPrompt` it used to fabricate.
    #[test]
    fn own_segment_honors_stored_provenance_on_a_merged_user_turn() {
        let from = SessionId::new();
        let (role, _content, prov) =
            own_segment(&user_turn(Provenance::MergedAsk { from })).expect("a UserTurn maps");
        assert_eq!(role, Role::User);
        assert_eq!(prov, Provenance::MergedAsk { from });
    }

    /// C-04 back-compat: records written before `MergedAsk` existed all
    /// carry `Provenance::UserPrompt` in their (mandatory) `prov` field, so
    /// honoring the stored provenance reproduces the old kind-derived
    /// behavior for them exactly.
    #[test]
    fn own_segment_keeps_user_prompt_for_pre_merge_records() {
        let (role, _content, prov) =
            own_segment(&user_turn(Provenance::UserPrompt)).expect("a UserTurn maps");
        assert_eq!(role, Role::User);
        assert_eq!(prov, Provenance::UserPrompt);
    }

    /// The same property end-to-end through `ContextBuilder::build`: a
    /// merged turn in `own` produces a segment (and a context-report entry)
    /// whose provenance is `MergedAsk`, not `UserPrompt`.
    #[test]
    fn build_labels_a_merged_turn_with_its_stored_provenance() {
        let from = SessionId::new();
        let input = ContextInput {
            agent_id: AgentId::new(),
            turn: 1,
            model: ModelId::new("echo-model"),
            cache_mode: CacheMode::None,
            system_prompt: None,
            skills: vec![],
            tools: vec![],
            inherited: None,
            head: HeadSegment::Prompt {
                text: "parent prompt".into(),
            },
            own: Arc::from(vec![user_turn(Provenance::MergedAsk { from })]),
            cache_ttl: CacheTtl::FiveMinutes,
        };

        let (segments, report) = ContextBuilder::new().build(&input).unwrap();
        let merged = segments
            .iter()
            .find(|s| s.provenance == Provenance::MergedAsk { from })
            .expect("the merged turn must appear with its stored MergedAsk provenance");
        assert!(
            report
                .segments
                .iter()
                .any(|entry| entry.segment == merged.id
                    && entry.provenance == Provenance::MergedAsk { from }),
            "the context report must carry the MergedAsk provenance too"
        );
        assert!(
            segments
                .iter()
                .all(|s| s.provenance != Provenance::UserPrompt
                    || s.content
                        == merged_prompt_content(&input)),
            "no segment may mislabel the merged turn as UserPrompt"
        );
    }

    fn merged_prompt_content(input: &ContextInput) -> Vec<ContentBlock> {
        match &input.head {
            HeadSegment::Prompt { text } => text_block(text),
            _ => unreachable!("this test's head is always a Prompt"),
        }
    }
}

#[cfg(test)]
mod breakpoint_indices_tests {
    use super::*;
    use chrono::Utc;
    use conway_core::ids::{LogSeq, SessionId};

    fn input_with_inherited() -> ContextInput {
        let from = SessionId::new();
        ContextInput {
            agent_id: AgentId::new(),
            turn: 0,
            model: ModelId::new("m"),
            cache_mode: CacheMode::None,
            system_prompt: None,
            skills: vec![],
            tools: vec![],
            inherited: Some(InheritedPrefix {
                from,
                seq_range: SeqRange::new(LogSeq(0), Some(LogSeq(0).succ())),
                records: Arc::from(vec![LogRecord::UserTurn {
                    seq: LogSeq(0),
                    ts: Utc::now(),
                    text: "inherited turn".into(),
                    prov: Provenance::UserPrompt,
                }]),
            }),
            head: HeadSegment::Prompt {
                text: "head".into(),
            },
            own: Arc::from(vec![]),
            cache_ttl: CacheTtl::FiveMinutes,
        }
    }

    /// A = the sole `ToolSchemas` segment (index 0: no system prompt/skills
    /// in this fixture), B = the sole inherited segment (index 1) — exactly
    /// what `build` itself tracks inline while assembling this same input.
    #[test]
    fn finds_a_and_b_when_an_inherited_prefix_exists() {
        let input = input_with_inherited();
        let (segments, _) = ContextBuilder::new().build(&input).unwrap();

        let (a, b) = breakpoint_indices(&segments);
        assert_eq!(a, Some(0));
        assert!(matches!(
            segments[0].provenance,
            Provenance::ToolRegistry { .. }
        ));
        assert_eq!(b, Some(1));
        assert!(matches!(
            segments[1].provenance,
            Provenance::Inherited { .. }
        ));
    }

    /// No `InheritedPrefix` at all -> B is `None`, A still resolves to the
    /// unconditional `ToolSchemas` segment.
    #[test]
    fn b_is_none_without_an_inherited_prefix() {
        let mut input = input_with_inherited();
        input.inherited = None;
        let (segments, _) = ContextBuilder::new().build(&input).unwrap();

        let (a, b) = breakpoint_indices(&segments);
        assert_eq!(a, Some(0));
        assert!(b.is_none());
    }

    /// A hook (WI-126) can drop the `ToolSchemas` segment entirely from the
    /// FINAL list this function is actually run against in `attempt.rs`'s
    /// post-pass -- in that case there is no A, so this returns `None`
    /// rather than a stale/wrong index.
    #[test]
    fn a_is_none_when_the_tool_schemas_segment_is_absent() {
        let input = input_with_inherited();
        let (mut segments, _) = ContextBuilder::new().build(&input).unwrap();
        segments.retain(|s| !matches!(s.provenance, Provenance::ToolRegistry { .. }));

        let (a, _b) = breakpoint_indices(&segments);
        assert!(a.is_none());
    }
}
