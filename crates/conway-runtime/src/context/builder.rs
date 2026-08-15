//! `ContextBuilder`: assembles one agent's request context in the fixed
//! architecture §5.3 order, with complete provenance and non-correctness-
//! bearing cache hints. Pure over already-resolved records — no I/O, no
//! `async`; ancestry resolution is the caller's job.
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
//! `ulid::Ulid` (see `derive_segment_id`). The hash input never includes
//! `cache_mode`, so cache-neutrality holds by construction, not by
//! coincidence.
//!
//! Golden files additionally project segments through a `GoldenSegment`
//! struct (in `tests/context_golden.rs`) that omits `id` entirely, so
//! byte-equality does not depend on the derivation formula staying fixed
//! forever — only on it staying deterministic.
//!
//! ## Tool-call/result pairing
//!
//! A rendered context must never contain a tool call without its result:
//! every provider rejects that request outright rather than tolerating it
//! (`kimi/k3`: "an assistant message with 'tool_calls' must be followed by
//! tool messages responding to each 'tool_call_id'"). Nothing upstream
//! guarantees the pairing, because two ordinary situations break it:
//!
//! - **A fork snapshot cut mid-batch.** `conway_fork` runs as one call
//!   *inside* a batch, so `Conway::fork_from` takes the parent's head while
//!   that batch is still in flight: the assistant record carrying every
//!   `ToolUse` of the turn is already appended, and none of the answering
//!   `ToolResultRecord`s exist yet. The child inherits a prefix ending on
//!   unanswered calls. Eight parallel `conway_fork` calls produced exactly
//!   this -- eight children, all dead on their first request with zero steps
//!   taken.
//! - **An own log that ends mid-batch.** `agent_loop` re-reads the whole
//!   session every turn, so a session killed between the assistant append
//!   and its tool results is unresumable for the same reason.
//!
//! `drop_unanswered_tool_calls` (private to this module) enforces the
//! invariant on the final assembled list, covering both. It is lossy in one
//! direction, deliberately: the model no longer sees that it made those
//! calls, and may re-issue them. That is a recoverable turn, where the
//! alternative is a request no provider will accept at all. Synthesizing
//! placeholder results was rejected as the costlier trade: it would fabricate
//! `Provenance::Inherited` content that no agent ever produced.
//!
//! **The loss is recorded, not merely conceded.** Every dropped `call_id`
//! lands in `ContextReport::dropped`, which reaches both the live
//! `report_slot` the TUI's `/context` reads and the durable
//! `LogRecord::ContextReportRecord`. The harness does not curate context on
//! its own initiative; the one place it must intervene to produce a sendable
//! request at all is therefore in the record rather than behind it, and a
//! turn where the model re-issues a call it appears never to have made is
//! explicable from the log instead of mysterious.
//!
//! **This invariant is only guaranteed on `ContextBuilder::build`'s OWN
//! output.** A registered `ContextHook` runs after `build` returns and can
//! edit `segments` freely, including orphaning a tool call/result pair --
//! `drop_unanswered_tool_calls` never runs again to catch it. `crate::
//! context::hook_guard::ensure_hook_payload_coherent` is the seam that
//! re-derives coherence on a hook's output, using `check_tool_call_coherence`
//! (this module's check-only sibling of `drop_unanswered_tool_calls`,
//! covering BOTH orphan directions -- a hook's edit is a deliberate act, so
//! it is refused rather than repaired; see that module's own doc).
//!
//! ## A documented interpretation gap: assistant-turn provenance
//!
//! `conway_core::provenance::Provenance` is exhaustively eleven variants
//! (enforced by that crate's own tests; the original §5.3 nine, plus
//! `MergedAsk` added by B4, plus `ChildResult` added by
//!) and none of them represents "the
//! model's own prior turn" — `LogRecord::Assistant` does not even carry a
//! `prov` field. Architecture §5.3's own-records mapping table names a
//! provenance for `tool_result`, `parent_steer`, and system notes, but only
//! a role (`Role::Assistant`) for assistant turns, not a provenance. Since
//! `PromptSegment::provenance` is mandatory and this crate cannot add
//! another `Provenance` variant (out of `conway-runtime`'s scope), assistant
//! turns are mapped to `Provenance::SystemNote { reason: "assistant_turn"
//! }` — the closest existing volatile-tier variant, using a `reason`
//! sentinel that collides with no other component's matching (only
//! matches `"repeated_step"` and `"result_contract_violation"`). This is a
//! placeholder, not a design decision: it should be raised against
//! `MODULE:conway-core` as a request for a dedicated `Provenance::Assistant`
//! (or similar) variant.

use std::collections::HashSet;
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
    /// The IMMEDIATE ancestor this bundle was inherited from — "who handed me
    /// this context" — NOT the original author of every record in `records`. At
    /// fork depth >= 2, `records` is the WHOLE effective transcript up to the
    /// fork point (a fork inherits all of it: root's own records, then every
    /// intermediate ancestor's own records in turn, through the immediate
    /// parent's), per `conway_core::transcript::TranscriptResolver`'s "inherited prefix
    /// always flows through in full" contract — but every one of those records,
    /// however deep its true origin, is stamped with this single `from` when
    /// `ContextBuilder` turns `records` into `Provenance::Inherited` segments
    /// (see this crate's `subagent.rs` module doc, "`InheritedPrefix::from` at
    /// fork depth >= 2", for the full rationale). Recovering true per-record
    /// authorship at arbitrary depth would require per-record session tracking
    /// that does not exist upstream (in `conway_core::log::LogRecord` or in
    /// `conway_core::transcript`'s resolver) — out of this item's scope; queued as a
    /// refinement question rather than attempted here (coordinator ruling,
    /// rework).
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
/// caller's job; this builder never touches a store.
///
/// `turn` is not part of the spec's illustrative struct but is
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
/// [`Provenance`] — there is no `Default` impl to fall back on, so where a segment came
/// from is always answerable (
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
        //
        // NO SCHEMA TEXT LIVES IN THIS SEGMENT'S `content` (//). Every request already carries the full
        // tool-schema set once as the native `tools` array the provider
        // actually consumes (`conway-plugin-backends`'s `openai_compat`/
        // `anthropic` wire modules, built straight from `ContextInput.tools`
        // via `AttemptEngine::build_request`, independent of `segments`
        // entirely). A `Role::System` segment holding the SAME canonical JSON
        // used to be pushed here too, as a `ToolSpec`-serializing superset
        // (name, description, schema, **and** the harness-internal
        // `category`/`permission` fields the model never needed) — a full
        // second copy of every tool's schema, paid on every turn and, via fork
        // inheritance -- a fork takes the whole prefix -- at every fork depth
        // for every sibling. Measured against a representative 14-tool built-in
        // set (`conway-tools::builtin_plugins`), that duplicate text ran
        // ~13.7KB / ~3.4K estimated tokens — on the close order of, and often
        // larger than, everything else in a single turn; this was measured
        // rather than assumed.
        //
        // Investigated rather than assumed: Anthropic's Messages API DOES
        // support `cache_control` directly on the `tools` array — "Tool
        // definitions can be cached by placing `cache_control` on the last
        // tool in your `tools` array. All tools defined before and
        // including that tool are cached as a single prefix." (Anthropic
        // docs, "Prompt caching" > "Caching tool definitions",
        // https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching,
        // verified 2026-08-08). That is exactly what breakpoint A needs, so
        // this segment's TEXT is no longer sent — the native array is the
        // only copy — while the segment ITSELF is kept, with empty
        // `content`, for three reasons that would otherwise regress:
        //   1. It is still the anchor `desired_breakpoints`/
        //      `attach_cache_hints` mark for breakpoint A — unchanged, since
        //      that machinery keys on `Provenance::ToolRegistry`, never on
        //      content. `conway-plugin-backends::anthropic::wire`'s
        //      `BreakpointTarget::Tools` is what redirects the resulting
        //      `cache_hint` to `body["tools"]`'s last entry instead of a
        //      `system` entry (see that module).
        //   2. It is still the sole source of `Provenance::ToolRegistry {
        //      hash }`, which `prefix::boundary_index`/`prefix_key` rely on
        //      to be unconditionally present (an empty-content Static-tier
        //      segment satisfies that exactly as well as a populated one —
        //      neither this key's boundary-finding nor its canonical-bytes
        //      hashing cares whether `content` is empty, only that a
        //      `ToolRegistry` segment with a `hash` derived from the real
        //      tool set exists). `prefix_key` behaviour is therefore
        //      UNCHANGED in shape (still `blake3(model ‖
        //      canonical_bytes(segments[0..=B]))`) and still changes when
        //      the tool set changes (via `hash`), even though the bytes it
        //      hashes for this one segment shrank to nothing.
        //   3. It is still the segment `ContextReportEntry` attaches a
        //      `tokens_est` to for `/context`'s reporting — see
        //      `estimate_tool_schemas_tokens` below, which fixes the
        //      matching under-count: the generic per-segment estimator
        //      would now see empty `content` and report 0, so this value is
        //      computed directly from `input.tools` instead and assigned
        //      onto the segment after the generic pass runs.
        //
        // OpenAI-compatible dialects have no `cache_control` equivalent at
        // all (`CacheMode::ImplicitPrefix` attaches no hints — see
        // `attach_cache_hints`), so for them this is a pure win: one fewer
        // `system` message, no cache anchor lost because there was never
        // one to lose.
        let mut sorted_tools = input.tools.clone();
        sorted_tools.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        let tools_value = serde_json::to_value(&sorted_tools).expect("ToolSpec always serializes");
        let hash = blake3::hash(&canonical_json_bytes(&tools_value))
            .to_hex()
            .to_string();
        segments.push(PromptSegment::new(
            Role::System,
            Vec::new(),
            Provenance::ToolRegistry { hash },
        ));
        let a_index = segments.len() - 1;

        // [3] InheritedPrefix* — one segment per record, order preserved;
        // breakpoint B attaches to the last one, if any exist (resolved
        // below, AFTER `drop_unanswered_tool_calls` may have removed
        // segments, so the index can never be stale).
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

        // Every tool call this context renders must have its result in the
        // same context (see the module doc, "Tool-call/result pairing").
        // Runs on the WHOLE assembled list -- both an inherited prefix cut
        // mid-batch and an own log that ends mid-batch produce the same
        // invalid request -- and before ids/estimates/hints below, so all
        // three describe what is actually sent.
        let dropped = drop_unanswered_tool_calls(&mut segments);

        // Breakpoint B: the last surviving inherited segment, re-derived
        // from provenance now that the list is final. A cannot have moved:
        // the `ToolSchemas` segment carries no `ToolUse` block, so the pass
        // above can neither empty nor drop it, and nothing before it is
        // droppable either.
        let (_, b_index) = breakpoint_indices(&segments);

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
        // [2]'s `content` is deliberately empty (see its own comment
        // above), so the generic per-segment pass just set its estimate to
        // 0 -- overwrite it with a term for the native `tools` array that
        // segment no longer carries text for, so `ContextReport::
        // total_tokens_est` (and therefore `est_tokens` on every downstream
        // `RouteRequest`/`AttemptRequest`, `agent_loop.rs`'s
        // `route_and_attempt`) does not silently drop the whole tool-schema
        // cost the way it would if this were left at the generic 0.
        segments[a_index].tokens_est = Some(estimate_tool_schemas_tokens(&input.tools));

        // Cache hints — economics, never correctness-bearing.
        let key = prefix::prefix_key(&input.model, &segments);
        attach_cache_hints(
            &mut segments,
            &input.cache_mode,
            input.cache_ttl,
            a_index,
            b_index,
            &key,
        );

        let report = build_report(input.agent_id, input.turn, &segments, dropped);

        Ok((segments, report))
    }
}

/// Re-derives a `ContextReport` from a (possibly `ContextHook`-transformed)
/// segment list: recomputes every segment's `tokens_est` and rebuilds the
/// report entries in the given order. a hook may add, edit, or drop
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
///
/// `tools`: the same
/// `ContextPayload::tools` a hook returned alongside `segments` -- a
/// `ContextHook` can narrow/replace the announced tool set (the
/// `announced_tools`) as well as edit segments, and the `[2] ToolSchemas`
/// segment's `content` is deliberately always empty (see `ContextBuilder::
/// build`'s own comment), so the generic per-segment loop above cannot
/// recover its token cost the way it can for every other segment kind.
/// Both call sites (`agent_loop.rs`'s `on_overflow` and `before_request`
/// handling) already have the hook's returned tools at hand and pass the
/// same value here that flows into the eventual request.
/// `dropped` is the list from the report this one supersedes. It MUST be
/// carried forward rather than defaulted: `drop_unanswered_tool_calls` ran
/// during the original assembly and its removals are not recoverable from
/// `segments` afterwards, so re-deriving here would silently produce an
/// empty list -- replacing the silent drop this field exists to expose with
/// a second one, on every turn a `ContextHook` is registered.
pub(crate) fn retotal(
    agent_id: AgentId,
    turn: u32,
    segments: &mut [PromptSegment],
    tools: &[ToolSpec],
    dropped: Vec<String>,
) -> ContextReport {
    for segment in segments.iter_mut() {
        segment.tokens_est = Some(estimate_tokens(&segment.content));
    }
    if let Some(segment) = segments
        .iter_mut()
        .rev()
        .find(|segment| matches!(segment.provenance, Provenance::ToolRegistry { .. }))
    {
        segment.tokens_est = Some(estimate_tool_schemas_tokens(tools));
    }
    build_report(agent_id, turn, segments, dropped)
}

/// `dropped` is threaded in rather than recomputed: by the time a report is
/// built the unanswered calls are already gone from `segments`, so nothing
/// downstream can rediscover them. [`retotal`] therefore has to carry the
/// incoming report's list forward -- see its own doc.
fn build_report(
    agent_id: AgentId,
    turn: u32,
    segments: &[PromptSegment],
    dropped: Vec<String>,
) -> ContextReport {
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
        dropped,
    }
}

fn text_block(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: text.to_string(),
    }]
}

/// Wraps a recorded `ToolResult` as the single `ContentBlock::ToolResultBlock`
/// a `Role::ToolResult` segment must carry. Both wire adapters
/// (`openai_compat::tool_result_messages` and `anthropic::tool_result_blocks`)
/// serialize a tool result ONLY from a `ToolResultBlock` -- it is the block
/// that carries the `call_id` tying the result back to its `tool_use` /
/// `tool_call`. The tool runner produces raw `Text` blocks, so before this
/// wrapping the segment held bare `Text`, matched neither wire adapter, and the
/// tool result was silently dropped from every request: the model saw its own
/// tool call but never the output, so it confabulated an answer.
fn tool_result_block(result: &ToolResult) -> Vec<ContentBlock> {
    vec![ContentBlock::ToolResultBlock {
        call_id: result.call_id.clone(),
        blocks: result.blocks.clone(),
        is_error: result.is_error,
    }]
}

/// Drops every `ContentBlock::ToolUse` whose `call_id` has no answering
/// `ContentBlock::ToolResultBlock` anywhere in `segments`, then drops any
/// segment the filtering emptied. Returns the dropped `call_id`s, which is
/// the only record that the request was altered at all -- see the module doc.
///
/// A segment that held ONLY unanswered calls is removed rather than sent
/// empty: an assistant message with neither text nor tool calls is rejected
/// by the Anthropic dialect ("all messages must have non-empty content").
/// A segment that was ALREADY empty is untouched -- that is the `[2]
/// ToolSchemas` segment, which is deliberately contentless and load-bearing
/// (breakpoint A, `prefix_key`'s boundary, the tool-schema token estimate).
///
/// The unmatched direction (a result whose call is absent) is deliberately
/// NOT handled here: this pass only ever runs on `ContextBuilder::build`'s
/// OWN assembly output, where it genuinely cannot fire -- records are
/// ordered, an assistant record always precedes the results answering it,
/// and no prefix cut can therefore admit a result while excluding its call.
/// **That was true only as long as nothing edited the list after assembly.**
/// A `ContextHook` now can: it receives an already-coherent
/// `Vec<PromptSegment>` and is free to drop the `ToolResultBlock` half of a
/// pair as easily as the `ToolUse` half, which is exactly the direction this
/// function still does not handle. `check_tool_call_coherence` (below) is
/// the check-only sibling that reports BOTH directions, over the same
/// `Vec<PromptSegment>` type a hook returns -- see
/// `crate::context::hook_guard`, which runs it on a hook's output instead of
/// assuming assembly's own ordering guarantee still holds.
fn drop_unanswered_tool_calls(segments: &mut Vec<PromptSegment>) -> Vec<String> {
    let answered: HashSet<String> = tool_result_call_ids(segments).into_iter().collect();

    // The overwhelmingly common case -- a settled transcript, every call
    // answered -- returns here, having rebuilt nothing.
    let dropped: Vec<String> = tool_use_call_ids(segments)
        .into_iter()
        .filter(|call_id| !answered.contains(call_id))
        .collect();
    if dropped.is_empty() {
        return dropped;
    }

    let mut kept: Vec<PromptSegment> = Vec::with_capacity(segments.len());
    for mut segment in std::mem::take(segments) {
        let was_empty = segment.content.is_empty();
        segment.content.retain(|block| {
            !matches!(block, ContentBlock::ToolUse { call_id, .. } if !answered.contains(call_id))
        });
        if was_empty || !segment.content.is_empty() {
            kept.push(segment);
        }
    }
    *segments = kept;
    dropped
}

/// Every `call_id` on a `ContentBlock::ToolUse` block in `segments`, in
/// segment/block order (duplicates preserved -- a legitimate transcript
/// never repeats a `call_id`, but nothing here assumes that). Shared by
/// [`drop_unanswered_tool_calls`] (the mutating pass) and
/// [`check_tool_call_coherence`] (the check-only pass), so the two can never
/// drift on what counts as a "call".
fn tool_use_call_ids(segments: &[PromptSegment]) -> Vec<String> {
    segments
        .iter()
        .flat_map(|segment| segment.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect()
}

/// Every `call_id` on a `ContentBlock::ToolResultBlock` block in `segments`,
/// in segment/block order. See [`tool_use_call_ids`]'s own doc.
fn tool_result_call_ids(segments: &[PromptSegment]) -> Vec<String> {
    segments
        .iter()
        .flat_map(|segment| segment.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResultBlock { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect()
}

/// One direction of tool-call/result orphaning [`check_tool_call_coherence`]
/// can find. `drop_unanswered_tool_calls` (`ContextBuilder::build`'s own
/// mutating pass) only ever produces [`Self::UnansweredCall`] -- a prefix cut
/// can leave a call standing with no answering result, but ordering makes
/// the reverse impossible on assembly's own output (see that function's own
/// doc). A `ContextHook` edits an ALREADY-coherent list, so it can produce
/// either direction; this type names both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToolCallOrphan {
    /// A `ToolUse` block whose `call_id` has no answering `ToolResultBlock`
    /// anywhere in the checked segments.
    UnansweredCall { call_id: String },
    /// A `ToolResultBlock` whose `call_id` names no surviving `ToolUse`
    /// anywhere in the checked segments.
    OrphanedResult { call_id: String },
}

impl std::fmt::Display for ToolCallOrphan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolCallOrphan::UnansweredCall { call_id } => {
                write!(f, "call `{call_id}` has no answering result")
            }
            ToolCallOrphan::OrphanedResult { call_id } => {
                write!(f, "result `{call_id}` names no surviving call")
            }
        }
    }
}

/// Check-only counterpart of [`drop_unanswered_tool_calls`]: reports every
/// tool-call/result pairing violation in `segments`, in BOTH directions (see
/// [`ToolCallOrphan`]), without mutating anything. Built on the same
/// `call_id` bookkeeping (`tool_use_call_ids`/`tool_result_call_ids`) the
/// mutating pass uses, so "what counts as answered" can never diverge
/// between the two.
///
/// This is the seam `crate::context::hook_guard::ensure_hook_payload_coherent`
/// runs on a `ContextHook`'s returned `segments` before anything downstream
/// consumes them: `ContextBuilder::build` already guarantees the invariant
/// on its own output via the mutating pass above; a hook can undo that
/// guarantee on ITS output, and this is what re-derives whether it did.
pub(crate) fn check_tool_call_coherence(segments: &[PromptSegment]) -> Vec<ToolCallOrphan> {
    let calls = tool_use_call_ids(segments);
    let results = tool_result_call_ids(segments);
    let call_set: HashSet<&str> = calls.iter().map(String::as_str).collect();
    let result_set: HashSet<&str> = results.iter().map(String::as_str).collect();

    let mut orphans: Vec<ToolCallOrphan> = calls
        .iter()
        .filter(|call_id| !result_set.contains(call_id.as_str()))
        .map(|call_id| ToolCallOrphan::UnansweredCall {
            call_id: call_id.clone(),
        })
        .collect();
    orphans.extend(
        results
            .iter()
            .filter(|call_id| !call_set.contains(call_id.as_str()))
            .map(|call_id| ToolCallOrphan::OrphanedResult {
                call_id: call_id.clone(),
            }),
    );
    orphans
}

/// Renders a `LogRecord::ChildResultRecord`'s `result` into the plain-text
/// a parent's context actually carries -- shared by [`record_role_and_content`]
/// (inherited pass-through) and [`own_segment`] (this agent's own volatile
/// records) so the two never drift apart on wording.
fn child_result_text(result: &conway_core::agent::AgentResult) -> String {
    format!(
        "child agent {} finished ({}): {}",
        result.agent_id,
        crate::result::status_label(&result.status),
        result.summary
    )
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
        // A child's result, recorded into an ancestor's own log,
        // flows into a fork child's inherited
        // prefix exactly like any other own volatile record kind -- same
        // treatment `ParentSteer` already gets two arms above.
        LogRecord::ChildResultRecord { result, .. } => {
            Some((Role::System, text_block(&child_result_text(result))))
        }
        LogRecord::Header(_)
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
            // `/context`, erasing the merge origin, which must stay answerable. Records
            // written before `MergedAsk` existed all carry
            // `Provenance::UserPrompt` here (the field is mandatory on the
            // wire), so old-record behavior is preserved by construction
            // (kept readable for already-persisted records).
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
        // a child's terminal result,
        // drained into THIS agent's own mailbox and persisted by
        // `mailbox::classify`, becomes visible on the very next turn's
        // ordinary re-read -- exactly like `ParentSteer` above. `prov` is
        // the STORED `Provenance::ChildResult` (never re-derived, never
        // `SystemNote`), so the segment is unambiguously marked as having
        // come from a child, not this agent itself, and provenance says so.
        LogRecord::ChildResultRecord { result, prov, .. } => Some((
            Role::System,
            text_block(&child_result_text(result)),
            prov.clone(),
        )),
        LogRecord::Header(_)
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
/// quoting, escaping) -- see the module doc's earlier work note on why this
/// estimator no longer serializes the whole block to JSON first.
const PER_BLOCK_OVERHEAD_TOKENS: u32 = 4;

/// Heuristic token estimate (T-9: explicitly approximate, never presented as
/// an exact count) over a segment's actual text/content payload, NOT its
/// JSON serialization. the prior formula (`json.len() / 4` over
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

/// Estimated token cost of the native provider `tools` array -- the `[2] ToolSchemas` segment carries no
/// `content` to run [`estimate_tokens`] over anymore, so this is the
/// estimator's dedicated term for the copy that actually reaches the wire.
/// Same `heuristic-chars4` shape as every other estimate in this module
/// (`ceil(chars / 4)` plus a fixed per-payload overhead), over the full
/// (unstripped) `ToolSpec` list -- the same shape
/// `conway_core::ports::backend::default_estimate_tokens`'s own `req.tools`
/// term uses, so the informational `/context` report this feeds and the
/// authoritative admission gate that estimator feeds do not drift far
/// apart, even though (per that function's own doc, and `attempt.rs`'s
/// module doc) the two are never required to agree exactly -- a real
/// dialect's `Backend::admit` override measures the ACTUAL serialized wire
/// body instead, which is smaller (no `category`/`permission` fields, a
/// dialect-specific envelope) -- so this is deliberately an approximation
/// (T-9), not a claim of matching either the model's own tokenizer or any
/// specific dialect's wire bytes.
fn estimate_tool_schemas_tokens(tools: &[ToolSpec]) -> u32 {
    if tools.is_empty() {
        return 0;
    }
    let rendered = serde_json::to_value(tools)
        .map(|value| value.to_string())
        .unwrap_or_default();
    let chars = u32::try_from(rendered.chars().count()).unwrap_or(u32::MAX);
    chars.div_ceil(4) + PER_BLOCK_OVERHEAD_TOKENS
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
/// NOTE: unlike `SessionId`/`AgentId` (fresh ULIDs,
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
        Provenance::ChildResult { .. } => "child_result",
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
/// segments — earlier work) correct even when the hook has changed segment
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

    /// The formula this heuristic replaced: the whole content
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

        let report = retotal(agent_id, 3, &mut segments, &[], Vec::new());

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

        let report = retotal(agent_id, 0, &mut segments, &[], Vec::new());
        assert_eq!(report.segments.len(), 1);
    }

    fn sample_tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: conway_core::ids::ToolName::new(name),
            description: format!("{name} tool"),
            schema: schemars::schema_for!(std::collections::BTreeMap<String, String>),
            category: conway_core::content::ToolCategory::Read,
            permission: conway_core::content::PermissionClass::Safe,
        }
    }

    /// The `[2] ToolSchemas` segment
    /// no longer carries the schema text on the wire (the native `tools`
    /// array is the only copy), but the estimator must still account for
    /// it.
    #[test]
    fn tool_schemas_estimate_is_nonzero_and_grows_with_the_tool_set() {
        assert_eq!(estimate_tool_schemas_tokens(&[]), 0);

        let one = estimate_tool_schemas_tokens(&[sample_tool("read")]);
        assert!(one > 0, "a non-empty tool set must estimate > 0 tokens");

        let two = estimate_tool_schemas_tokens(&[sample_tool("read"), sample_tool("write")]);
        assert!(
            two > one,
            "a larger tool set must estimate more tokens ({two} vs {one})"
        );
    }

    /// `retotal` must recover the SAME estimate `ContextBuilder::build`
    /// itself would have set, using whatever `tools` a `ContextHook`
    /// returned — not fall back to the generic (now-zero, since `[2]`'s
    /// `content` is always empty) per-segment estimate.
    #[test]
    fn retotal_recovers_the_tool_schemas_estimate_from_the_given_tools() {
        let agent_id = AgentId::new();
        let mut segments = vec![PromptSegment::new(
            Role::System,
            Vec::new(),
            Provenance::ToolRegistry {
                hash: "deadbeef".into(),
            },
        )];
        let tools = vec![sample_tool("read"), sample_tool("write")];

        let report = retotal(agent_id, 0, &mut segments, &tools, Vec::new());

        let expected = estimate_tool_schemas_tokens(&tools);
        assert!(expected > 0);
        assert_eq!(segments[0].tokens_est, Some(expected));
        assert_eq!(report.total_tokens_est, expected);
    }

    /// End-to-end through `ContextBuilder::build`: the `[2] ToolSchemas`
    /// segment carries no content (nothing to duplicate the native `tools`
    /// array with) but still contributes a non-zero, tool-set-derived
    /// `tokens_est` to the report — the acceptance criterion "the token
    /// estimator accounts for the native tools array" made concrete.
    #[test]
    fn build_sends_no_tool_schema_text_but_still_estimates_its_cost() {
        let tools = vec![sample_tool("read"), sample_tool("write")];
        let input = ContextInput {
            agent_id: AgentId::new(),
            turn: 0,
            model: ModelId::new("m"),
            cache_mode: CacheMode::None,
            system_prompt: None,
            skills: vec![],
            tools: tools.clone(),
            inherited: None,
            head: HeadSegment::Prompt { text: "hi".into() },
            own: Arc::from(vec![]),
            cache_ttl: CacheTtl::FiveMinutes,
        };
        let (segments, report) = ContextBuilder::new().build(&input).unwrap();

        let tool_registry = segments
            .iter()
            .find(|s| matches!(s.provenance, Provenance::ToolRegistry { .. }))
            .expect("ToolSchemas segment is unconditional");
        assert!(
            tool_registry.content.is_empty(),
            "the native `tools` array is the only wire copy now"
        );
        let expected = estimate_tool_schemas_tokens(&tools);
        assert_eq!(tool_registry.tokens_est, Some(expected));
        assert!(
            report.total_tokens_est >= expected,
            "the report's total must include the tool-schema estimate"
        );
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

    /// Back-compat: records written before `MergedAsk` existed all
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
                    || s.content == merged_prompt_content(&input)),
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

    /// A hook can drop the `ToolSchemas` segment entirely from the
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

#[cfg(test)]
mod tool_call_pairing_tests {
    use super::*;
    use chrono::Utc;
    use conway_core::content::{StopReason, Usage};
    use conway_core::ids::{BackendId, LogSeq, ModelRef, SessionId, ToolName};

    fn tool_use(call_id: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            call_id: call_id.to_string(),
            name: ToolName::new("read"),
            arguments: serde_json::json!({ "path": call_id }),
        }
    }

    fn assistant(seq: u64, content: Vec<ContentBlock>) -> LogRecord {
        LogRecord::Assistant {
            seq: LogSeq(seq),
            ts: Utc::now(),
            content,
            model: ModelRef {
                backend: BackendId::new("b"),
                model: ModelId::new("m"),
            },
            route_reason: serde_json::json!({}),
            usage: Usage::default(),
            stop: StopReason::ToolUse,
        }
    }

    fn tool_result(seq: u64, call_id: &str) -> LogRecord {
        LogRecord::ToolResultRecord {
            seq: LogSeq(seq),
            ts: Utc::now(),
            result: ToolResult {
                call_id: call_id.to_string(),
                tool: ToolName::new("read"),
                blocks: vec![ContentBlock::Text {
                    text: format!("contents of {call_id}"),
                }],
                is_error: false,
                truncated: None,
            },
        }
    }

    /// Records arrive as the agent's OWN log (the resumable-mid-batch case).
    fn input_with(own: Vec<LogRecord>, cache_mode: CacheMode) -> ContextInput {
        ContextInput {
            agent_id: AgentId::new(),
            turn: 0,
            model: ModelId::new("m"),
            cache_mode,
            system_prompt: None,
            skills: vec![],
            tools: vec![],
            inherited: None,
            head: HeadSegment::Prompt {
                text: "head".into(),
            },
            own: Arc::from(own),
            cache_ttl: CacheTtl::FiveMinutes,
        }
    }

    /// The same records arriving as an INHERITED prefix (the mid-batch fork
    /// case) rather than as the agent's own log.
    fn input_inheriting(records: Vec<LogRecord>) -> ContextInput {
        let from = SessionId::new();
        let last = records.last().and_then(|r| r.seq()).unwrap_or(LogSeq(0));
        ContextInput {
            inherited: Some(InheritedPrefix {
                from,
                seq_range: SeqRange::new(LogSeq(0), Some(last.succ())),
                records: Arc::from(records),
            }),
            ..input_with(vec![], CacheMode::None)
        }
    }

    /// Every `ToolUse` `call_id` the assembled segments actually render.
    fn rendered_call_ids(segments: &[PromptSegment]) -> Vec<String> {
        segments
            .iter()
            .flat_map(|s| s.content.iter())
            .filter_map(|b| match b {
                ContentBlock::ToolUse { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect()
    }

    /// The settled case: nothing is dropped, and the pass is invisible.
    #[test]
    fn a_fully_answered_batch_is_left_exactly_as_it_was() {
        let input = input_with(
            vec![
                assistant(0, vec![tool_use("a"), tool_use("b")]),
                tool_result(1, "a"),
                tool_result(2, "b"),
            ],
            CacheMode::None,
        );
        let (segments, report) = ContextBuilder::new().build(&input).unwrap();

        let mut ids = rendered_call_ids(&segments);
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
        assert!(report.dropped.is_empty());
    }

    /// Only `b` may be dropped -- this pass removes unanswered calls, not
    /// answered ones, and not the results that answer them.
    #[test]
    fn an_unanswered_call_is_dropped_and_the_answered_one_survives() {
        let input = input_with(
            vec![
                assistant(0, vec![tool_use("a"), tool_use("b")]),
                tool_result(1, "a"),
            ],
            CacheMode::None,
        );
        let (segments, report) = ContextBuilder::new().build(&input).unwrap();

        assert_eq!(rendered_call_ids(&segments), vec!["a".to_string()]);
        assert_eq!(
            report.dropped,
            vec!["b".to_string()],
            "the drop is recorded, not just performed"
        );
    }

    /// The turn still has prose, so the segment stays -- stripped of the
    /// unanswered call, not thrown away with it.
    #[test]
    fn a_turn_with_text_keeps_its_text_after_its_calls_are_dropped() {
        let input = input_with(
            vec![assistant(
                0,
                vec![
                    ContentBlock::Text {
                        text: "spawning reviewers".into(),
                    },
                    tool_use("a"),
                ],
            )],
            CacheMode::None,
        );
        let (segments, report) = ContextBuilder::new().build(&input).unwrap();

        assert!(rendered_call_ids(&segments).is_empty());
        assert_eq!(report.dropped, vec!["a".to_string()]);
        assert!(
            segments
                .iter()
                .any(|s| s.content.iter().any(
                    |b| matches!(b, ContentBlock::Text { text } if text == "spawning reviewers")
                )),
            "the turn's own prose must survive its dropped calls"
        );
    }

    /// A turn of only unanswered calls is removed rather than sent empty (the
    /// Anthropic dialect rejects a message with neither text nor calls). The
    /// deliberately-contentless `ToolSchemas` segment must NOT be swept up
    /// with it: it is load-bearing for breakpoint A and the schema estimate.
    #[test]
    fn a_turn_of_only_unanswered_calls_is_dropped_but_the_empty_tool_registry_stays() {
        let input = input_with(vec![assistant(0, vec![tool_use("a")])], CacheMode::None);
        let (segments, report) = ContextBuilder::new().build(&input).unwrap();

        assert!(rendered_call_ids(&segments).is_empty());
        assert_eq!(report.dropped, vec!["a".to_string()]);
        assert!(
            segments
                .iter()
                .any(|s| matches!(s.provenance, Provenance::ToolRegistry { .. })
                    && s.content.is_empty()),
            "the empty ToolSchemas segment is load-bearing and must survive"
        );
    }

    /// Breakpoint B is resolved AFTER the drop pass. If it were captured
    /// during assembly it would point at a segment the pass then removed, and
    /// the cache hint would land on the wrong boundary.
    #[test]
    fn breakpoint_b_lands_on_the_last_surviving_inherited_segment() {
        let input = input_inheriting(vec![
            LogRecord::UserTurn {
                seq: LogSeq(0),
                ts: Utc::now(),
                text: "inherited turn".into(),
                prov: Provenance::UserPrompt,
            },
            assistant(1, vec![tool_use("orphan")]),
        ]);
        let (segments, report) = ContextBuilder::new().build(&input).unwrap();

        assert_eq!(report.dropped, vec!["orphan".to_string()]);
        let (_, b) = breakpoint_indices(&segments);
        let b = b.expect("an inherited segment survives");
        assert!(
            matches!(segments[b].provenance, Provenance::Inherited { .. }),
            "B must name a segment that is still present after the drop"
        );
        assert_eq!(
            b,
            segments
                .iter()
                .rposition(|s| matches!(s.provenance, Provenance::Inherited { .. }))
                .unwrap(),
            "B is the LAST surviving inherited segment"
        );
    }

    /// The module doc claims this pass runs on the WHOLE assembled list,
    /// because an inherited prefix cut mid-batch and an own log that ends
    /// mid-batch produce the same invalid request. Each half has its own
    /// coverage -- the fork tests in `subagent_fork_spawn.rs` for the
    /// inherited side, the cases above for the own side -- but a build
    /// carrying BOTH is the one that shows the pass is not scoped to
    /// either source. A single-source implementation passes every other
    /// test in this module and fails this one.
    #[test]
    fn orphans_are_dropped_from_the_inherited_prefix_and_the_own_log_alike() {
        let mut input = input_inheriting(vec![
            LogRecord::UserTurn {
                seq: LogSeq(0),
                ts: Utc::now(),
                text: "inherited turn".into(),
                prov: Provenance::UserPrompt,
            },
            assistant(1, vec![tool_use("inherited_orphan")]),
        ]);
        input.own = Arc::from(vec![
            assistant(2, vec![tool_use("own_orphan"), tool_use("own_answered")]),
            tool_result(3, "own_answered"),
        ]);

        let (segments, report) = ContextBuilder::new().build(&input).unwrap();

        let mut dropped = report.dropped.clone();
        dropped.sort();
        assert_eq!(
            dropped,
            vec!["inherited_orphan".to_string(), "own_orphan".to_string()],
            "an orphan is an orphan whichever side of the prefix boundary it \
             sits on"
        );
        assert_eq!(
            rendered_call_ids(&segments),
            vec!["own_answered".to_string()],
            "and the answered call survives, from either side"
        );
    }

    /// `retotal` runs after a `ContextHook` has edited the payload and
    /// rebuilds the report from scratch. It must carry the drop list forward:
    /// the removed calls are unrecoverable from `segments` by that point, so
    /// re-deriving would silently empty the field on every turn a hook is
    /// registered -- a second silent drop replacing the first.
    #[test]
    fn retotal_carries_the_drop_list_forward() {
        let input = input_with(
            vec![
                assistant(0, vec![tool_use("a"), tool_use("b")]),
                tool_result(1, "a"),
            ],
            CacheMode::None,
        );
        let (mut segments, report) = ContextBuilder::new().build(&input).unwrap();
        assert_eq!(report.dropped, vec!["b".to_string()]);

        let after = retotal(
            input.agent_id,
            input.turn,
            &mut segments,
            &[],
            report.dropped.clone(),
        );
        assert_eq!(after.dropped, vec!["b".to_string()]);
    }

    /// `check_tool_call_coherence` is a no-op report on a fully-answered
    /// list -- same "settled case is invisible" property `build` itself
    /// keeps for `drop_unanswered_tool_calls`.
    #[test]
    fn check_tool_call_coherence_finds_nothing_in_a_fully_answered_batch() {
        let input = input_with(
            vec![assistant(0, vec![tool_use("a")]), tool_result(1, "a")],
            CacheMode::None,
        );
        let (segments, _) = ContextBuilder::new().build(&input).unwrap();
        assert!(check_tool_call_coherence(&segments).is_empty());
    }

    /// The direction `drop_unanswered_tool_calls` also finds: a `ToolUse`
    /// with no answering `ToolResultBlock`. Constructed directly (not
    /// through `build`, which would have already dropped it) -- this is
    /// exactly the shape a `ContextHook` produces by deleting a result
    /// segment out of an otherwise-coherent list.
    #[test]
    fn check_tool_call_coherence_finds_an_unanswered_call() {
        let segments = vec![PromptSegment::new(
            Role::Assistant,
            vec![tool_use("orphan")],
            Provenance::SystemNote {
                reason: "assistant_turn".into(),
            },
        )];
        assert_eq!(
            check_tool_call_coherence(&segments),
            vec![ToolCallOrphan::UnansweredCall {
                call_id: "orphan".to_string()
            }]
        );
    }

    /// The direction `drop_unanswered_tool_calls` does NOT handle: a
    /// `ToolResultBlock` whose call is gone. `build` alone can never
    /// produce this (ordering forbids it -- see that function's own doc,
    /// corrected in this change); a hook that deletes the `ToolUse` segment
    /// out of an otherwise-coherent list can.
    #[test]
    fn check_tool_call_coherence_finds_an_orphaned_result() {
        let segments = vec![PromptSegment::new(
            Role::ToolResult,
            tool_result_block(&ToolResult {
                call_id: "gone".to_string(),
                tool: ToolName::new("read"),
                blocks: vec![ContentBlock::Text {
                    text: "contents".into(),
                }],
                is_error: false,
                truncated: None,
            }),
            Provenance::ToolResult {
                call_id: "gone".to_string(),
                tool: ToolName::new("read"),
            },
        )];
        assert_eq!(
            check_tool_call_coherence(&segments),
            vec![ToolCallOrphan::OrphanedResult {
                call_id: "gone".to_string()
            }]
        );
    }

    /// Both directions at once, exactly what a hook that swaps which half
    /// of two DIFFERENT pairs it keeps would produce.
    #[test]
    fn check_tool_call_coherence_reports_both_directions_together() {
        let unanswered = PromptSegment::new(
            Role::Assistant,
            vec![tool_use("unanswered")],
            Provenance::SystemNote {
                reason: "assistant_turn".into(),
            },
        );
        let orphaned_result = PromptSegment::new(
            Role::ToolResult,
            tool_result_block(&ToolResult {
                call_id: "orphaned".to_string(),
                tool: ToolName::new("read"),
                blocks: vec![ContentBlock::Text {
                    text: "contents".into(),
                }],
                is_error: false,
                truncated: None,
            }),
            Provenance::ToolResult {
                call_id: "orphaned".to_string(),
                tool: ToolName::new("read"),
            },
        );
        let mut orphans = check_tool_call_coherence(&[unanswered, orphaned_result]);
        orphans.sort_by_key(|o| match o {
            ToolCallOrphan::UnansweredCall { call_id }
            | ToolCallOrphan::OrphanedResult { call_id } => call_id.clone(),
        });
        assert_eq!(
            orphans,
            vec![
                ToolCallOrphan::OrphanedResult {
                    call_id: "orphaned".to_string()
                },
                ToolCallOrphan::UnansweredCall {
                    call_id: "unanswered".to_string()
                },
            ]
        );
    }
}
