//! Applies a configured script hook's `ContextDelta` answers
//! (`conway_core::hook::ContextDelta`, dispatched through
//! `crate::hook_dispatch::HookDispatcher::dispatch_context`) to an assembled
//! `ContextPayload` -- append-only, per decision `01KZRZAFD8T3GX407MZC8P1W1E`,
//! and composed under the no-chaining rule (decision
//! `01KYTQVYPJW0PAAXRBEMAKZY0V`): every hook in the slice this module reads
//! was evaluated independently against the SAME pre-edit payload (its
//! caller's job, `crate::agent_loop`, never this module's), exclusions union,
//! and appends concatenate in configured order.
//!
//! # Why this is append-only at the TYPE level, not by convention
//!
//! [`apply_script_deltas`] takes ownership of a `ContextPayload` and returns
//! an [`AppliedContextEdit`]. Its ENTIRE vocabulary of mutation is two
//! operations on the segment `Vec`:
//!
//! - `Vec::retain` -- drops a whole segment, chosen by its stringified
//!   [`conway_core::ids::SegmentId`] appearing in some hook's
//!   `ContextDelta::excludes`. A segment survives or it does not; nothing
//!   about a surviving segment's `content`, `role`, `provenance`, or
//!   position relative to its neighbors is ever touched.
//! - `Vec::push` -- appends a brand-new [`PromptSegment`] built from a
//!   `ContextDelta::appends` item, always at the END of whatever survived
//!   `retain`.
//!
//! There is no third operation, and there cannot structurally be one: the
//! input this module reads, `ContextDelta { appends: Vec<serde_json::Value>,
//! excludes: Vec<String> }` (`conway_core::hook`), has no field pairing an
//! append with a target position or an existing segment's identity -- a
//! script cannot ask to replace segment X with Y, only to exclude X (by id)
//! and separately append Y (which lands at the end, never at X's former
//! position). `conway_core::hook`'s own test
//! `an_unknown_replace_shaped_key_is_ignored_not_interpreted_as_a_replacement`
//! proves a `"replace"`-shaped key on the wire is inert; this module's own
//! `appending_a_segment_never_lands_at_an_excluded_segments_former_position`
//! test proves the SAME thing at the point where a delta is actually
//! applied, not merely deserialized.
//!
//! # Reconstructability -- the OTHER half of the same decision
//!
//! "The prior state must remain reconstructable at a point in time" is not
//! satisfied by "the model never saw it" -- an excluded segment's CONTENT
//! must survive somewhere, or there is no prior state left to reconstruct.
//! [`AppliedContextEdit`] carries every excluded segment alongside the
//! index it occupied in the pre-edit list, so [`AppliedContextEdit::
//! reconstruct_pre_edit`] can put every one of them back exactly where it
//! was and drop exactly the appended tail -- proven directly, by
//! reconstructing and comparing, in this module's own
//! `reconstruct_pre_edit_recovers_the_exact_pre_edit_payload` test.
//!
//! # Relationship to `LogRecord::ContextMask`
//!
//! `conway_core::log::LogRecord::ContextMask` models the identical idea --
//! "fold something away by appending a record saying treat these as hidden,
//! reversible by writing a second record" -- durably, in the session log.
//! It has no producer anywhere in the tree, and board item
//! `01KZRZZP6A4A27R3EN0HQAENBS` (this module's own) is explicit that
//! becoming its first producer is a SETTLED DEFERRAL, not this item's open
//! question (see that item's own text, and `docs/plugins/hooks.md` point
//! 4's "Two designed extensions to this point, neither built"). This
//! module is the EPHEMERAL, per-request analogue `ContextMask`'s own
//! vocabulary anticipates: `ContextDelta::excludes` is the in-memory
//! counterpart of a mask's `excluded` set, [`AppliedContextEdit`]'s own
//! (private) `excluded` field is the in-memory counterpart of "what was
//! folded staying visible", and
//! [`reconstruct_pre_edit`](AppliedContextEdit::reconstruct_pre_edit) is
//! the in-memory counterpart of applying a SECOND
//! record that undoes the first. Nothing here writes to the session log;
//! the fold vanishes the instant `AppliedContextEdit` is dropped at the end
//! of the turn that produced it. Converging the two into one vocabulary
//! (an in-memory delta that can ALSO be persisted as a `ContextMask`
//! record) is future work for whichever item actually builds a `ContextMask`
//! producer, not this one.

use std::collections::HashSet;

use conway_core::content::{ContentBlock, Role};
use conway_core::ports::ContextPayload;
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;

use crate::hook_dispatch::ContextHookAnswer;

/// One malformed `ContextDelta::appends` item this hook's answer otherwise
/// applied cleanly -- the append is skipped (never partially applied, never
/// interpreted as anything else) and this is returned so the caller can log
/// it, matching `HookDispatcher::dispatch_context`'s own "visible, not
/// silent" posture for a whole-hook failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedAppend {
    pub hook_id: String,
    pub reason: String,
}

/// What [`apply_script_deltas`] produces: the edited payload (what actually
/// goes downstream) plus enough to reconstruct the payload exactly as it was
/// before this edit ran -- see this module's own doc, "Reconstructability".
#[derive(Clone, Debug)]
pub struct AppliedContextEdit {
    pub payload: ContextPayload,
    /// Every segment [`apply_script_deltas`] excluded, paired with the
    /// index it occupied in the PRE-edit segment list. Never read by
    /// anything downstream of this edit (an excluded segment must not
    /// reach the model) -- read only by [`Self::reconstruct_pre_edit`].
    excluded: Vec<(usize, PromptSegment)>,
    /// How many trailing entries of `payload.segments` are appends --
    /// always exactly this many, always at the end, since
    /// [`apply_script_deltas`] never `insert`s.
    appended: usize,
    /// Every append skipped for being malformed -- see [`SkippedAppend`].
    pub skipped: Vec<SkippedAppend>,
}

impl AppliedContextEdit {
    /// Recovers the payload exactly as it was before [`apply_script_deltas`]
    /// ran: drops the appended tail, then splices every excluded segment
    /// back at its original index (ascending order, so an earlier
    /// reinsertion never shifts a later one's target).
    pub fn reconstruct_pre_edit(&self) -> ContextPayload {
        let mut segments = self.payload.segments.clone();
        let keep = segments.len().saturating_sub(self.appended);
        segments.truncate(keep);

        let mut excluded = self.excluded.clone();
        excluded.sort_by_key(|(index, _)| *index);
        for (index, segment) in excluded {
            let at = index.min(segments.len());
            segments.insert(at, segment);
        }
        ContextPayload {
            segments,
            tools: self.payload.tools.clone(),
        }
    }
}

/// The wire shape one `ContextDelta::appends` item must match: a NEW
/// segment's role and text. Deliberately minimal -- see `ContextDelta::
/// appends`'s own doc on `conway_core::hook` for why the per-item shape is
/// this module's decision to make, not `conway-core`'s.
#[derive(Debug, serde::Deserialize)]
struct AppendItem {
    role: Role,
    text: String,
}

/// Applies every answer in `answers` to `payload`, append-only (module doc).
/// `answers` must already be the FULL set evaluated against `payload` as
/// their shared pre-edit input (the no-chaining rule) -- this function does
/// not fetch them itself.
///
/// Composition, per decision `01KYTQVYPJW0PAAXRBEMAKZY0V`: every answer's
/// `excludes` unions into one set (`{X} ∪ {X} = {X}`, so two hooks excluding
/// the same segment is not a conflict); every answer's `appends` concatenate
/// in `answers`' own order, each stamped with ITS OWN hook id (the
/// ACCEPTANCE'S provenance requirement) -- there is no "same-target replace"
/// case to resolve because this vocabulary cannot express a target at all
/// (module doc).
pub fn apply_script_deltas(
    payload: ContextPayload,
    answers: &[ContextHookAnswer],
) -> AppliedContextEdit {
    let mut excluded_ids: HashSet<String> = HashSet::new();
    for answer in answers {
        excluded_ids.extend(answer.delta.excludes.iter().cloned());
    }

    let mut excluded: Vec<(usize, PromptSegment)> = Vec::new();
    let mut surviving: Vec<PromptSegment> = Vec::with_capacity(payload.segments.len());
    for (index, segment) in payload.segments.into_iter().enumerate() {
        if excluded_ids.contains(&segment.id.to_string()) {
            excluded.push((index, segment));
        } else {
            surviving.push(segment);
        }
    }

    let mut skipped = Vec::new();
    let mut appended = 0usize;
    for answer in answers {
        for item in &answer.delta.appends {
            match serde_json::from_value::<AppendItem>(item.clone()) {
                Ok(parsed) => {
                    surviving.push(PromptSegment::new(
                        parsed.role,
                        vec![ContentBlock::Text { text: parsed.text }],
                        // Names the configured hook id verbatim -- the
                        // ACCEPTANCE'S "every segment a script appends
                        // carries provenance naming the hook's configured
                        // id", satisfied with the SAME `SystemNote`
                        // provenance the cookbook's own `ContextHook`
                        // compaction example already uses to name itself
                        // as a compaction artifact rather than pretending
                        // to be a real tool output
                        // (`docs/plugins/cookbook.md`).
                        Provenance::SystemNote {
                            reason: format!("script_hook:{}", answer.hook_id),
                        },
                    ));
                    appended += 1;
                }
                Err(err) => {
                    tracing::warn!(
                        hook = answer.hook_id.as_str(),
                        "context hook append skipped -- malformed append item: {err}"
                    );
                    skipped.push(SkippedAppend {
                        hook_id: answer.hook_id.clone(),
                        reason: err.to_string(),
                    });
                }
            }
        }
    }

    AppliedContextEdit {
        payload: ContextPayload {
            segments: surviving,
            tools: payload.tools,
        },
        excluded,
        appended,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::content::ToolSpec;
    use conway_core::ids::ModelId;
    use conway_core::provenance::Provenance as Prov;

    fn seg(text: &str, provenance: Prov) -> PromptSegment {
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text { text: text.into() }],
            provenance,
        )
    }

    fn payload(segments: Vec<PromptSegment>) -> ContextPayload {
        ContextPayload {
            segments,
            tools: Vec::<ToolSpec>::new(),
        }
    }

    fn answer(
        hook_id: &str,
        appends: Vec<serde_json::Value>,
        excludes: Vec<String>,
    ) -> ContextHookAnswer {
        ContextHookAnswer {
            hook_id: hook_id.to_string(),
            delta: conway_core::hook::ContextDelta::new(appends, excludes),
        }
    }

    // -------------------------------------------------------------- append --

    #[test]
    fn an_append_lands_at_the_end_stamped_with_its_hooks_id() {
        let a = seg("a", Prov::UserPrompt);
        let b = seg("b", Prov::UserPrompt);
        let input = payload(vec![a.clone(), b.clone()]);

        let edit = apply_script_deltas(
            input,
            &[answer(
                "annotator",
                vec![serde_json::json!({"role": "system", "text": "note"})],
                vec![],
            )],
        );

        assert_eq!(edit.payload.segments.len(), 3);
        assert_eq!(edit.payload.segments[0], a);
        assert_eq!(edit.payload.segments[1], b);
        let appended = &edit.payload.segments[2];
        assert_eq!(appended.role, Role::System);
        match &appended.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "note"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(
            appended.provenance,
            Prov::SystemNote {
                reason: "script_hook:annotator".to_string()
            },
            "provenance must name the configured hook id verbatim"
        );
    }

    #[test]
    fn two_hooks_appends_concatenate_in_answer_order() {
        let input = payload(vec![]);
        let edit = apply_script_deltas(
            input,
            &[
                answer(
                    "first",
                    vec![serde_json::json!({"role": "system", "text": "one"})],
                    vec![],
                ),
                answer(
                    "second",
                    vec![serde_json::json!({"role": "system", "text": "two"})],
                    vec![],
                ),
            ],
        );
        let texts: Vec<&str> = edit
            .payload
            .segments
            .iter()
            .map(|s| match &s.content[0] {
                ContentBlock::Text { text } => text.as_str(),
                _ => panic!("expected Text"),
            })
            .collect();
        assert_eq!(texts, vec!["one", "two"]);
    }

    /// A malformed append item is skipped, visibly (`edit.skipped`), and
    /// does not prevent the REST of the same hook's valid work, or any
    /// other hook's, from applying.
    #[test]
    fn a_malformed_append_item_is_skipped_visibly_without_losing_the_rest() {
        let input = payload(vec![]);
        let edit = apply_script_deltas(
            input,
            &[answer(
                "flaky",
                vec![
                    serde_json::json!({"role": "system", "text": "good"}),
                    serde_json::json!({"not": "a valid append shape"}),
                ],
                vec![],
            )],
        );
        assert_eq!(edit.payload.segments.len(), 1);
        assert_eq!(edit.skipped.len(), 1);
        assert_eq!(edit.skipped[0].hook_id, "flaky");
    }

    // ------------------------------------------------------------- exclude --

    #[test]
    fn an_exclude_removes_the_named_segment_by_id_only() {
        let a = seg("keep", Prov::UserPrompt);
        let b = seg("drop", Prov::UserPrompt);
        let b_id = b.id.to_string();
        let input = payload(vec![a.clone(), b]);

        let edit = apply_script_deltas(input, &[answer("censor", vec![], vec![b_id])]);

        assert_eq!(edit.payload.segments, vec![a]);
    }

    #[test]
    fn two_hooks_excluding_the_same_segment_is_not_a_conflict() {
        let a = seg("keep", Prov::UserPrompt);
        let b = seg("drop", Prov::UserPrompt);
        let b_id = b.id.to_string();
        let input = payload(vec![a.clone(), b]);

        let edit = apply_script_deltas(
            input,
            &[
                answer("censor-1", vec![], vec![b_id.clone()]),
                answer("censor-2", vec![], vec![b_id]),
            ],
        );

        assert_eq!(edit.payload.segments, vec![a]);
    }

    /// A survivor is untouched: same `PartialEq` value, same position
    /// relative to other survivors -- `apply_script_deltas` never reaches
    /// into a segment it keeps.
    #[test]
    fn a_surviving_segment_is_byte_for_byte_identical_and_keeps_its_relative_order() {
        let a = seg("a", Prov::UserPrompt);
        let b = seg("b", Prov::UserPrompt);
        let c = seg("c", Prov::UserPrompt);
        let b_id = b.id.to_string();
        let input = payload(vec![a.clone(), b, c.clone()]);

        let edit = apply_script_deltas(input, &[answer("censor", vec![], vec![b_id])]);

        assert_eq!(edit.payload.segments, vec![a, c]);
    }

    // ----------------------------------------------- type-level append-only --

    /// ACCEPTANCE, type-level: an append can NEVER land at an excluded
    /// segment's former position -- it always lands at the end, regardless
    /// of where the exclusion happened. This is what "no replace primitive"
    /// means concretely: excluding index 0 of a 2-element list and
    /// appending one item produces `[survivor, new]`, never `[new,
    /// survivor]` -- there is no field anywhere in `ContextDelta` a script
    /// could set to ask for the latter.
    #[test]
    fn appending_a_segment_never_lands_at_an_excluded_segments_former_position() {
        let first = seg("first", Prov::UserPrompt);
        let second = seg("second", Prov::UserPrompt);
        let first_id = first.id.to_string();
        let input = payload(vec![first, second.clone()]);

        let edit = apply_script_deltas(
            input,
            &[answer(
                "replacer-attempt",
                vec![serde_json::json!({"role": "user", "text": "new"})],
                vec![first_id],
            )],
        );

        // If this vocabulary could express replacement, a script excluding
        // index 0 and appending one item could put the new segment back at
        // index 0. It cannot: the survivor stays first, the append is
        // always last.
        assert_eq!(edit.payload.segments.len(), 2);
        assert_eq!(edit.payload.segments[0], second);
        match &edit.payload.segments[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "new"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    // ------------------------------------------------------ reconstruction --

    /// The OTHER half of the decision this module implements: the pre-edit
    /// payload is recoverable from what `apply_script_deltas` returned, by
    /// reconstructing and comparing -- not by trusting a flag.
    #[test]
    fn reconstruct_pre_edit_recovers_the_exact_pre_edit_payload() {
        let a = seg("a", Prov::UserPrompt);
        let b = seg(
            "b",
            Prov::ToolResult {
                call_id: "c1".into(),
                tool: conway_core::ids::ToolName::new("read"),
            },
        );
        let c = seg("c", Prov::UserPrompt);
        let original = payload(vec![a.clone(), b.clone(), c.clone()]);
        let b_id = b.id.to_string();

        let edit = apply_script_deltas(
            original.clone(),
            &[answer(
                "editor",
                vec![serde_json::json!({"role": "system", "text": "new note"})],
                vec![b_id],
            )],
        );

        // Sent-downstream shape: b is gone, a new segment is appended at
        // the end.
        assert_eq!(edit.payload.segments.len(), 3);
        assert_eq!(edit.payload.segments[0], a);
        assert_eq!(edit.payload.segments[1], c);
        match &edit.payload.segments[2].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "new note"),
            other => panic!("expected Text, got {other:?}"),
        }

        let reconstructed = edit.reconstruct_pre_edit();
        assert_eq!(
            reconstructed.segments, original.segments,
            "the pre-hook payload must be recoverable exactly, byte for byte"
        );
    }

    /// Multiple exclusions at different original indices all land back in
    /// their own original slots, not just the right SET of segments in any
    /// order.
    #[test]
    fn reconstruct_pre_edit_restores_multiple_exclusions_at_their_original_indices() {
        let segs: Vec<PromptSegment> = (0..5)
            .map(|i| seg(&i.to_string(), Prov::UserPrompt))
            .collect();
        let original = payload(segs.clone());
        let excludes = vec![segs[1].id.to_string(), segs[3].id.to_string()];

        let edit = apply_script_deltas(original.clone(), &[answer("editor", vec![], excludes)]);
        assert_eq!(
            edit.payload.segments,
            vec![segs[0].clone(), segs[2].clone(), segs[4].clone()]
        );

        assert_eq!(edit.reconstruct_pre_edit().segments, original.segments);
    }

    // -------------------------------------------------------- prefix cache --

    /// THE cache proof the item's own ACCEPTANCE asks for: an append-only
    /// edit that touches only the VOLATILE tail (never the static/inherited
    /// prefix) leaves `crate::context::prefix_key` byte-identical --
    /// asserted on the observable `PrefixKey`, computed the SAME way the
    /// real attempt path computes it (`crate::context::prefix_key`), not on
    /// a flag that claims the property holds.
    #[test]
    fn appending_and_excluding_only_volatile_segments_leaves_the_prefix_key_unchanged() {
        use crate::context::prefix::prefix_key;

        let model = ModelId::new("m");
        let static_segment = seg(
            "system prompt",
            Prov::AgentDef {
                name: "assistant".into(),
            },
        );
        let tool_result = seg(
            "result",
            Prov::ToolResult {
                call_id: "c1".into(),
                tool: conway_core::ids::ToolName::new("read"),
            },
        );
        let tool_result_id = tool_result.id.to_string();
        let user_turn = seg("hi", Prov::UserPrompt);
        let original = payload(vec![static_segment, tool_result, user_turn]);

        let before = prefix_key(&model, &original.segments);

        let edit = apply_script_deltas(
            original,
            &[answer(
                "annotator",
                vec![serde_json::json!({"role": "system", "text": "a volatile note"})],
                vec![tool_result_id],
            )],
        );
        let after = prefix_key(&model, &edit.payload.segments);

        assert_eq!(
            before, after,
            "excluding/appending only VOLATILE-tier segments must not change the cache prefix key"
        );
    }

    /// The negative case, so the positive one above is not vacuous: editing
    /// (here, excluding) something IN the static/inherited prefix DOES
    /// change the key -- the guarantee is about the surviving bytes ahead
    /// of an edit point, not "editing is free no matter what it touches."
    #[test]
    fn excluding_a_static_segment_does_change_the_prefix_key() {
        use crate::context::prefix::prefix_key;

        let model = ModelId::new("m");
        let static_segment = seg(
            "system prompt",
            Prov::AgentDef {
                name: "assistant".into(),
            },
        );
        let static_id = static_segment.id.to_string();
        let user_turn = seg("hi", Prov::UserPrompt);
        let original = payload(vec![static_segment, user_turn]);

        let before = prefix_key(&model, &original.segments);
        let edit = apply_script_deltas(original, &[answer("editor", vec![], vec![static_id])]);
        let after = prefix_key(&model, &edit.payload.segments);

        assert_ne!(
            before, after,
            "excluding a STATIC-tier segment is a legitimate, deliberate cache-affecting edit"
        );
    }
}
