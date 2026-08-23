//! `conway.path`: the tool a model calls to compose a session's context
//! path -- the caller `ContextPathHost`/`write_head`/`ValidatedPath::
//! derive_with` were built for and, until this crate, had none (board item
//! `01M0PEFMG96SVBBD5D2E06H34A`, decision `01M0K4QT6MBXPD6PXMBBBD2P7B`).
//! `ContextPathHost` itself is not exported here or through `conway::
//! plugin` -- this crate reaches it only via `ToolCtx::context_path`'s
//! method calls, mirroring `SubagentHandle`'s own "used by method dispatch,
//! never named" precedent.
//!
//! # Why a tool, not a `Curator` (decision `01M0K4QT6MBXPD6PXMBBBD2P7B`)
//!
//! `CurateCtx` carries `model: Option<ModelId>` as a sizing IDENTIFIER, not
//! a callable backend, and a `Curator` runs per-turn before routing, so
//! inference there would be re-entrant. An operator's stated intent needs a
//! MODEL to interpret it first -- so this is a tool the model calls once
//! that interpreting is already done, at a boundary where inference is
//! already in flight, costing tokens once, visibly, because the operator
//! asked. `CurateCtx` is not widened; this crate never touches it.
//!
//! # Arguments: resolved record references, not natural language
//! (determine-before-building #1)
//!
//! The operator states intent in prose; the model resolves it. By the time
//! THIS tool is called, that resolution has already happened, so
//! `ComposeContextPathArgs` takes concrete `(session, seq)` pairs, never
//! a string for this crate to re-interpret. A model's two ordinary sources
//! for a `RecordRef` today: its OWN current session (every record it has
//! already seen carries an implicit seq it can name for `exclude`), and a
//! completed subagent's `transcript_ref` (`conway::AgentResult::
//! transcript_ref` -- the same field `conway_ask`'s own outcome carries) --
//! a real `SessionId` a model already
//! holds the moment `conway_fork`/`conway_spawn`/`conway_ask` resolves,
//! with seq 0 being that child's own first turn. Browsing an ARBITRARY
//! session's records by content (a search/listing capability) is
//! deliberately not built here -- see this crate's own completion report
//! for why that is a disclosed follow-up, not a silent gap.
//!
//! # Which plugin ships it (determine-before-building #2)
//!
//! A NEW plugin, `conway.path` -- not folded into `conway-plugin-trim`
//! (that crate is a `Curator`, the port this decision explicitly does NOT
//! use) or `conway-plugin-memory` (a different capability, freeform text
//! with no backing record, §11.7's "memory needs no path" case this
//! decision does not disturb). Installed the same way every other
//! first-party plugin is: a candidate in `conway-cli`'s
//! `first_party_plugins::bundle()`, opt-in via `[plugins].install` --
//! `PHILOSOPHY.md`'s own "First-party plugins, and why they are not
//! defaults" section states this is deliberate for the WHOLE tier, not a
//! gap this item reopens. What THIS item fixes is `conway-plugin-trim`'s
//! actual defect: not being a `conway-cli` dependency AT ALL, so no
//! `[plugins].install` entry could ever reach it. `conway.path` IS a
//! `conway-cli` dependency (`crates/conway-cli/src/first_party_plugins.rs`)
//! and IS a candidate `[plugins].install` can name -- reachable by a person
//! on the day it lands, via one line in `settings.json`, exactly like
//! `conway.memory`/`conway.skills` already are.
//!
//! # What the operator sees afterwards (determine-before-building #3)
//!
//! `ComposeContextPathTool::invoke`'s success reply states what was
//! brought in (`CostEstimate::appended_nodes` -- genuinely foreign records,
//! never counting a base node re-included at its original position),
//! whether the composition landed inside the frozen/cacheable tier
//! (`CostEstimate::divergence_inside_frozen_tier`), and the resulting head's
//! own log position. It reports STRUCTURE, never a token guess --
//! `CostEstimate` itself carries no token field by operator ruling
//! (`conway_core::path::CostEstimate`'s own doc): the backend's admission
//! gate is the one place a token cost is ever computed, and restating a
//! second, worse guess here would be exactly the drift risk that ruling
//! retired.
//!
//! # Coherence: refuse, never silently patch (determine-before-building #4)
//!
//! `ValidatedPath::derive_with` already refuses (`PathError::WouldOrphan`)
//! a composition that would leave a tool call or result stranded, and this
//! tool does NOT catch that refusal and quietly widen the request to bring
//! the missing half along. It surfaces the refusal verbatim (`PathError`'s
//! own `Display`, which names the orphan and both candidate repairs) as an
//! `is_error` tool result and persists NOTHING -- `set_head` is never
//! called on a refused derivation. The model reads the refusal, decides
//! (include the missing half too, or drop the one that orphans it), and
//! retries. The harness offers; it never picks (DESIGN §4.1's own rule,
//! unchanged by this tool).
//!
//! # The `covers_upto` trap (see the board item's own citation of finding
//! `01M0P50E04EY3BHQJHZX74HSSC`): resolved by ALWAYS composing from the
//! live default path, never by changing `write_head`
//!
//! `ContextPathHost::default_path` (`conway_runtime::context::path::
//! resolve_default_path`, called through the host) already returns this
//! session's own tail as part of the base -- the records from the current
//! head's `covers_upto` onward, or the whole log if there is no head yet.
//! `ComposeContextPathTool::invoke` derives from that base with
//! `PathOp::Omit` only for what the model explicitly names
//! (`ComposeContextPathArgs::exclude`) -- an op list that never mentions the
//! own tail simply never removes it, so it survives in the derived path
//! exactly as `derive`'s "omit only what an op names" contract already
//! guarantees. `write_head`'s own `covers_upto_for` then finds those
//! surviving own nodes and keeps `covers_upto` where it already was.
//!
//! **`covers_upto` can never fall to `LogSeq::ZERO` through this tool at
//! all** -- not by accident and not by request. Whichever argument asks for
//! it, the anchor rule below keeps one own node on the path, so the zero
//! state is unreachable from here.
//!
//! **Whenever this call would omit EVERY own node, the newest one is kept
//! as an anchor.** A second trap, found empirically while
//! testing `drop_own_tail`: omitting literally every own-attributed node
//! makes `covers_upto_for` fall back to `LogSeq::ZERO`, and zero does not
//! mean "no own tail" -- DESIGN §2.5's own semantic is "own tail FROM
//! `covers_upto`", so zero means "own tail = my ENTIRE own log, read live".
//! The very next own append (this call's own tool result, the model's own
//! follow-up reply) would then resurface every earlier own record on the
//! NEXT turn -- an amnesia flag that undoes itself the instant anything else
//! is said, which is the identical failure shape the board item's own
//! finding describes, just triggered by this tool's OWN deliberate branch
//! instead of an accident. Keeping the newest own node anchors
//! `covers_upto` just past it, so every EARLIER own record is genuinely,
//! durably left off the path. This is the honest cost of composing through
//! `write_head`'s EXISTING contract rather than widening it -- see the next
//! section for why widening was rejected.
//!
//! **What the anchor actually retains, stated precisely, because an earlier
//! revision of this doc understated it.** In practice the newest own node is
//! the record carrying this very tool call -- but that is a WHOLE
//! `Assistant` log record, not a minimal marker. `agent_loop` appends the
//! entire assistant turn (its narrative text and every one of its parallel
//! tool calls) as ONE record before the batch runs, so if the turn that
//! called this tool also said something substantial or invoked other tools,
//! all of that content is what survives. Path nodes are whole-record
//! granularity; there is no sub-record unit to keep instead. Do not describe
//! `drop_own_tail` as leaving nothing behind.
//!
//! **The anchor rule is checked over the COMBINED omit set, not inside the
//! `drop_own_tail` branch.** That distinction is the whole of it: an anchor
//! that only defends one argument's path is not an anchor. See the check
//! itself in `ComposeContextPathTool::invoke` for the two ways the narrower
//! version was defeated.
//!
//! **This is remedy 2 from the board item's own list ("make the caller
//! state the own-tail intent explicitly, so a reset is a choice rather than
//! a default"), not remedy 1 or 3.** Argued against the other two:
//!
//! - Remedy 1 (carry the prior head's `covers_upto` forward as a floor
//!   inside `write_head` itself) changes what a head MEANS -- it stops
//!   being self-describing (derivable from its own selection alone) and
//!   starts depending on history, a real cost against an append-only model
//!   whose entire read rule is latest-seq-wins. It also touches a SEAM
//!   (`write_head`) for a problem this tool can solve entirely at its own
//!   call site, which is where the board item's own text says the fix
//!   belongs if a seam change is not the answer.
//! - Remedy 3 (have the tool ALWAYS include the own tail, unconditionally)
//!   removes a real, legitimate capability: an operator who deliberately
//!   wants "forget everything I've said this session, use only that other
//!   conversation" cannot express it, and the tool would silently override
//!   a stated intent -- exactly the "operator's curation was forgotten by
//!   an arithmetic that had no reason to look for it" failure the finding
//!   describes, just moved one layer up and dressed as a safety feature.
//! - Remedy 2 keeps the default SAFE (the trap's trigger -- "compose a
//!   selection carrying none of the session's own records" -- cannot
//!   happen by accident, because the tool always starts from a base that
//!   already has the tail) while keeping the escape hatch REAL
//!   (`drop_own_tail: true` is one field a model sets when it means it).
//!   `write_head` is untouched; the invariant it enforces
//!   (`covers_upto == selection_last_seq + 1`) stays exactly where it was
//!   enforced before this crate existed.
//!
//! This crate's own end-to-end test suite pins both halves: composing with
//! only a foreign `include` and no `drop_own_tail` keeps the own tail (and
//! `covers_upto` sticky) across a LATER, independently-scripted turn; the
//! identical call with `drop_own_tail: true` genuinely and durably drops
//! every own record present at compose time (bar the compose call's own,
//! unavoidable, minimal-content record), without that content resurfacing
//! on a LATER turn either.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;

use conway::plugin::{
    async_trait, ContentBlock, PathArgs, PermissionClass, Plugin, PluginDescription,
    PluginManifest, RenderKind, Tool, ToolCall, ToolCategory, ToolCtx, ToolError, ToolOutput,
    ToolSpec, TruncationPolicy,
};
use conway::{LogSeq, PathError, PathOp, RecordRef, Selector, SessionId, ToolName};

/// This plugin's published manifest id -- a config author (or a first-party
/// bundle's own linking module) resolves `[plugins].install` entries
/// against this constant.
pub const PLUGIN_ID: &str = "conway.path";

/// The bare name `ComposeContextPathTool` registers under.
pub const COMPOSE_TOOL_NAME: &str = "compose_context_path";

/// The bare name of this plugin's one [`conway::plugin::InstructionFragment`].
pub const INSTRUCTION_NAME: &str = "conway.path.when_to_compose";

/// The `conway.path` plugin: contributes one tool
/// (`ComposeContextPathTool`) and the paragraph telling a model when to
/// reach for it. See this crate's own module doc for the full design
/// argument.
pub struct PathPlugin;

impl Plugin for PathPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![ToolName::new(COMPOSE_TOOL_NAME)],
            required_host_caps: vec![],
        }
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "lets the model bring in or drop context on its own".to_string(),
            you_get: format!(
                "1 tool ({COMPOSE_TOOL_NAME}) and an instruction telling the model when to \
                 use it -- the model can compose a session's next context path on request"
            ),
            you_lose: "nothing else -- context composition stays whatever the default path \
                       already includes"
                .to_string(),
            costs: format!("none beyond the {COMPOSE_TOOL_NAME} calls the model makes"),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(ComposeContextPathTool)]
    }

    fn instructions(&self) -> Vec<conway::plugin::InstructionFragment> {
        vec![conway::plugin::InstructionFragment {
            name: INSTRUCTION_NAME.to_string(),
            text: include_str!("../fragments/when_to_compose.md").to_string(),
            tool_ids: vec![ToolName::new(COMPOSE_TOOL_NAME)],
        }]
    }
}

/// One `(session, seq)` pair a model already resolved -- never a free-text
/// name. See this crate's own module doc, "Arguments: resolved record
/// references, not natural language".
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RecordRefArg {
    /// The id of the session that logged this record -- e.g. the
    /// `transcript_ref`/`session_id` a completed `conway_fork`/
    /// `conway_spawn`/`conway_ask` call already returned, or this tool
    /// call's own session.
    session: String,
    /// The record's sequence number within that session's own log.
    seq: u64,
}

/// Args for `ComposeContextPathTool`.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ComposeContextPathArgs {
    /// Records to bring onto this session's context path, from any
    /// session -- each one a `(session, seq)` pair the caller already
    /// resolved. Landing at the tail of the composed path, in the order
    /// given.
    #[serde(default)]
    include: Vec<RecordRefArg>,
    /// Sequence numbers of THIS session's own records to leave off the
    /// composed path (e.g. an exploratory dead end). Always interpreted
    /// against this call's own session -- there is no way to exclude
    /// another session's record (it was never automatically on the path to
    /// begin with).
    #[serde(default)]
    exclude: Vec<u64>,
    /// When true, durably drop this session's own earlier history from the
    /// composed path. What survives: everything in `include`, plus the
    /// NEWEST own record, which is always kept as an anchor -- dropping
    /// every own record would reset the tail to "read my whole log live"
    /// and resurrect all of it on the next turn. That newest record is
    /// normally the whole assistant turn that issued this call, including
    /// any other text or tool calls it contained, so it is not necessarily
    /// small. Defaults to false: composing a context path never drops this
    /// session's own ongoing conversation as a side effect (see this
    /// crate's module doc, "The `covers_upto` trap").
    #[serde(default)]
    drop_own_tail: bool,
}

fn error_output(text: impl Into<String>) -> ToolOutput {
    ToolOutput {
        blocks: vec![ContentBlock::Text { text: text.into() }],
        is_error: true,
        truncation: TruncationPolicy::None,
        artifacts: Vec::new(),
    }
}

/// The one tool this plugin ships -- see this crate's own module doc for
/// the full design argument this `invoke` implements.
struct ComposeContextPathTool;

#[async_trait]
impl Tool for ComposeContextPathTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(COMPOSE_TOOL_NAME),
            description: "Compose this session's context path: bring specific records from \
                          another session onto it, leave specific records of this session's \
                          own history off it, or both. This session's own ongoing turns stay \
                          on the path unless you explicitly ask to drop them (drop_own_tail). \
                          Reports what was brought in and whether the change falls inside the \
                          cached portion of context."
                .to_string(),
            schema: schemars::schema_for!(ComposeContextPathArgs),
            category: ToolCategory::Edit,
            permission: PermissionClass::RequiresApproval,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: ComposeContextPathArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;

        // 1. The base: this session's CURRENT default path. Already carries
        // the own tail -- see the module doc's "covers_upto trap" section.
        let base = match ctx.context_path.default_path().await {
            Ok(p) => p,
            Err(e) => {
                return Ok(error_output(format!(
                    "could not read this session's current context path: {e}"
                )))
            }
        };

        // 2. Parse `include` into RecordRefs; refuse on a malformed session id
        // before touching the store at all.
        let mut include_refs: Vec<RecordRef> = Vec::with_capacity(args.include.len());
        for r in &args.include {
            let session = match SessionId::from_str(&r.session) {
                Ok(s) => s,
                Err(_) => {
                    return Ok(error_output(format!(
                        "`{}` is not a valid session id",
                        r.session
                    )))
                }
            };
            include_refs.push(RecordRef {
                session,
                seq: LogSeq(r.seq),
            });
        }

        // 3. Resolve every `include` ref through the honest, masked read
        // surface -- a ref naming a masked or unknown record fails HERE,
        // with a clear reason, rather than surfacing later as a confusing
        // `derive_with` refusal.
        let foreign: BTreeMap<RecordRef, Arc<conway::LogRecord>> = if include_refs.is_empty() {
            BTreeMap::new()
        } else {
            match ctx.context_path.resolve_records(&include_refs).await {
                Ok(m) => m,
                Err(e) => {
                    return Ok(error_output(format!(
                        "could not resolve one or more `include` records: {e}"
                    )))
                }
            }
        };

        // 4. Build the op list. Nothing here removes the own tail unless
        // the model explicitly asked (`exclude`/`drop_own_tail`) -- see the
        // module doc.
        let mut ops: Vec<PathOp> = Vec::new();

        // Every OWN node currently on the path. The anchor below is chosen
        // from this set, so it must be gathered before any op is emitted.
        let own_seqs_in_base: BTreeSet<LogSeq> = base
            .nodes()
            .filter(|(n, _)| n.record.session == ctx.session_id)
            .map(|(n, _)| n.record.seq)
            .collect();

        // The own seqs this call would omit, gathered from BOTH sources
        // before any is turned into an op. Combining them first is the
        // whole point: the anchor rule below has to reason about what
        // will ACTUALLY survive, and neither source alone can tell it
        // that. `exclude` entries naming a seq that is not on the path
        // are still emitted, so `derive_with` reports them exactly as it
        // did before rather than this tool silently swallowing them.
        let mut omit_own: BTreeSet<LogSeq> = args.exclude.iter().copied().map(LogSeq).collect();
        if args.drop_own_tail {
            omit_own.extend(own_seqs_in_base.iter().copied());
        }

        // THE ANCHOR: never omit EVERY own node, whichever argument asked.
        //
        // `write_head`'s `covers_upto_for` derives `covers_upto` ONLY from
        // own-attributed nodes still present in the FROZEN selection
        // (`conway_runtime::context::path`'s own doc). Omitting literally
        // all of them falls back to `LogSeq::ZERO`, which does NOT mean
        // "no own tail" -- it means "own tail = my ENTIRE own log, read
        // live", the documented no-head-equivalent default. The very next
        // own append (this call's own tool result, the model's follow-up
        // reply) would then resurface EVERY earlier own record that was
        // just dropped, on the very next turn: an amnesia flag that un-does
        // itself the instant anything else is said. That is board finding
        // `01M0P50E04EY3BHQJHZX74HSSC`.
        //
        // Keeping the newest own node anchors `covers_upto` just past it,
        // so every EARLIER own record is genuinely and durably off the
        // path.
        //
        // This check is deliberately over the COMBINED set rather than
        // inside the `drop_own_tail` branch. An earlier revision anchored
        // only within that branch, which left two ways to reach the
        // zero state anyway and reopen the very trap this tool was filed
        // to close: an `exclude` list that also names the anchor seq, and
        // an `exclude` list that enumerates the whole own tail with
        // `drop_own_tail` never set. An invariant that holds only on the
        // path its author was thinking about is not an invariant.
        let mut anchor_kept: Option<LogSeq> = None;
        if !own_seqs_in_base.is_empty() && own_seqs_in_base.is_subset(&omit_own) {
            let newest = *own_seqs_in_base
                .iter()
                .next_back()
                .expect("non-empty checked above");
            omit_own.remove(&newest);
            anchor_kept = Some(newest);
        }

        for seq in omit_own {
            ops.push(PathOp::Omit {
                node: RecordRef {
                    session: ctx.session_id,
                    seq,
                },
            });
        }
        for r in &include_refs {
            ops.push(PathOp::Include { node: *r });
        }

        if ops.is_empty() {
            return Ok(ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: "nothing to compose: no include/exclude/drop_own_tail given, the \
                           context path is unchanged"
                        .to_string(),
                }],
                is_error: false,
                truncation: TruncationPolicy::None,
                artifacts: Vec::new(),
            });
        }

        // 5. Derive. Pure, refuses (never silently patches) on
        // `PathError::WouldOrphan` -- see the module doc's "Coherence"
        // section. `Selector::Operator` per decision `01M0K4QT6MBXPD6PXMBBBD2P7B`:
        // this IS its real producer.
        let derivation = match base.derive_with(&ops, &foreign, Selector::Operator) {
            Ok(d) => d,
            Err(e) => return Ok(error_output(refusal_text(&e))),
        };

        // 6. Persist. Only reached on a coherent derivation -- nothing
        // partial is ever written.
        let cost = derivation.cost.clone();
        let node_count = derivation.path.nodes().count();
        let head_seq = match ctx.context_path.set_head(derivation.path).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(error_output(format!(
                    "derived a coherent context path but could not freeze it as the new head: {e}"
                )))
            }
        };

        // 7. Report what was brought in and what it cost -- structure, not
        // a token guess (module doc, "What the operator sees afterwards").
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: report_text(&cost, node_count, head_seq, anchor_kept),
            }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: Vec::new(),
        })
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }
}

/// `PathError::WouldOrphan`'s own `Display` already names the orphan and
/// both candidate repairs (DESIGN §4.1); every other `PathError` variant
/// gets a short prefix so a caller with no orphan can still tell what
/// happened. One function, so this tool's refusal wording never drifts
/// from the type's own.
fn refusal_text(e: &PathError) -> String {
    match e {
        PathError::WouldOrphan { .. } => format!("refused: {e}"),
        other => format!("could not compose this context path: {other}"),
    }
}

/// Renders what `ComposeContextPathTool::invoke` did, in structural terms
/// only (module doc: "What the operator sees afterwards").
fn report_text(
    cost: &conway::CostEstimate,
    node_count: usize,
    head_seq: LogSeq,
    anchor_kept: Option<LogSeq>,
) -> String {
    let cache_note = if cost.divergence_inside_frozen_tier {
        "inside the cached portion of context -- earlier turns downstream may lose their cache"
    } else {
        "outside the cached portion of context -- no cache impact"
    };
    // Say it when a record was kept that the caller asked to drop. Keeping
    // the anchor silently would be the same class of defect as the reset it
    // prevents: the caller would believe the whole tail is gone.
    let anchor_note = match anchor_kept {
        Some(seq) => format!(
            " This call asked to drop every one of this session's own records; record {seq} was \
             KEPT anyway, as the anchor that makes the drop durable -- dropping literally all of \
             them resets the tail to \"read my whole log live\" and would resurrect everything on \
             the next turn. Every earlier own record is genuinely off the path."
        ),
        None => String::new(),
    };
    format!(
        "context path composed: {appended} record(s) brought in from elsewhere, {total} \
         record(s) now on the path, head written at seq {head_seq}. Change falls \
         {cache_note}.{anchor_note}",
        appended = cost.appended_nodes,
        total = node_count,
    )
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    /// The plugin browser's own read surface (board item
    /// `01M0KARX71A64NTSYTDBVANVPF`): a real description, never the
    /// trait's empty default.
    #[test]
    fn description_is_non_empty() {
        let description = PathPlugin.description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
    }
}
