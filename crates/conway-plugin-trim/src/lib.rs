//! `conway.trim`: a [`Curator`] that omits tool call/result round-trips
//! older than a configurable turn window (board item
//! `01M0EMAC4CCDQ8QJYM21RXPKRY`).
//!
//! # Why this exists
//!
//! ~5,500 lines of context-path and curation machinery existed with no
//! production consumer, as of 2026-08-19: no `Curator` implementation outside
//! `conway-core`'s own test doubles, `FsPathStore` never constructed,
//! `derive_with` with no caller. This crate is the smallest honest one —
//! *"drop tool results older than K turns"* — built through the ordinary
//! `Plugin::curators` surface, the same one a third party gets.
//!
//! Two of those three have since been closed by the work this crate's own
//! findings unblocked, and the sentence above is kept as the *motivation*
//! rather than as a claim about the tree today: `ConwayBuilder::build` now
//! constructs an `FsPathStore` by default, and `resolve_default_path` is the
//! production path constructor on every turn. `derive_with` still has no
//! production caller — this curator reaches `derive`, not `derive_with`,
//! because dropping a call and its result together never moves anything.
//!
//! # The op it actually performs
//!
//! [`PathOp::Omit`] works at record granularity: a whole [`LogRecord`], not
//! a single [`ContentBlock`] inside one. That forces a real design choice.
//! Omitting a `ToolResultRecord` alone orphans the `ContentBlock::ToolUse`
//! call that issued it — `derive` refuses this (`PathError::WouldOrphan`,
//! rule 1) — so "drop the result" is only expressible as "drop the call
//! *and* the result together". An `Assistant` record and every
//! `ToolResultRecord` answering one of its calls belong to the same "turn"
//! (`CurateCtx::turn`, `AgentLoop`'s own `state.turn`, bumped once per whole
//! loop iteration — AFTER that iteration's results, not right after the
//! `Assistant` record itself), so dropping both halves together whenever
//! either is old enough never orphans anything. Getting the turn boundary
//! right took a real session to catch: a naive "bump on every `Assistant`
//! record, immediately" first cut put a call one turn younger than its own
//! result, because a `ContextReportRecord` the harness writes between a call
//! and its answering result — see `tests/real_session.rs` for the exact
//! `seq`s — isn't an `Assistant` record either, but does land between them.
//! `derive` refused the resulting orphan rather than building it, which is
//! exactly the coherence guard doing its job; no retry against
//! `PathError::WouldOrphan`'s offers is needed once the boundary is right.
//!
//! One real cost this forces: a model that interleaves prose with a tool
//! call in the same response (common — not a corner case) loses that prose
//! too when its round-trip ages out, because the seam cannot address the
//! `Text`/`Thinking` block separately from the `ToolUse` block it shares a
//! record with.
//!
//! # What it does NOT do
//!
//! Never reorders. [`ValidatedPath::derive`] (not `derive_reordered`) is the
//! only constructor this curator calls — omission was sufficient for this
//! shape, so reordering (INTENT.md §5b's strictly more expensive operation)
//! was never reached for.
//!
//! # Installing it
//!
//! ```json
//! { "plugins": { "install": ["conway.trim"] } }
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use conway::plugin::{
    ContentBlock, CurateCtx, CurateOutcome, Curator, Plugin, PluginManifest, Tool,
};
use conway::{LogRecord, PathOp, ValidatedPath};

/// The install id an operator names in `plugins.install`.
pub const PLUGIN_ID: &str = "conway.trim";

/// The default window: keep the last 8 turns' tool round-trips, drop older
/// ones. Arbitrary but small enough to matter on a session worth curating at
/// all.
pub const DEFAULT_KEEP_TURNS: u32 = 8;

/// Drops tool call/result round-trips whose turn is more than `keep_turns`
/// behind [`CurateCtx::turn`]. See the module doc for why a round-trip,
/// never a lone result, is the unit this curator omits.
#[derive(Debug, Clone, Copy)]
pub struct TrimOldToolResults {
    pub keep_turns: u32,
}

impl Default for TrimOldToolResults {
    fn default() -> Self {
        Self {
            keep_turns: DEFAULT_KEEP_TURNS,
        }
    }
}

impl TrimOldToolResults {
    pub fn new(keep_turns: u32) -> Self {
        Self { keep_turns }
    }
}

#[async_trait]
impl Curator for TrimOldToolResults {
    async fn curate(&self, ctx: &CurateCtx, base: &ValidatedPath) -> CurateOutcome {
        let threshold = ctx.turn.saturating_sub(self.keep_turns);
        let mut turn: u32 = 0;
        // Bumped lazily -- only once a SECOND `Assistant` record is actually
        // seen -- rather than the instant the first one is. `state.turn`
        // (`conway_runtime::agent_loop`) increments once per whole loop
        // iteration, i.e. AFTER that iteration's `ToolResultRecord`s (and any
        // `ContextReportRecord`/`SystemNote` the harness interleaves between
        // a call and its own answering result) are already appended, not
        // right after the `Assistant` record itself. Bumping eagerly put a
        // call and its own result one turn apart on a real session (a
        // `ContextReportRecord` sits between them) and `derive` correctly
        // refused the resulting orphan -- this flag is that fix.
        let mut round_open = false;
        let mut ops = Vec::new();
        for (node, record) in base.nodes() {
            if matches!(record.as_ref(), LogRecord::Assistant { .. }) {
                if round_open {
                    turn = turn.saturating_add(1);
                }
                round_open = true;
            }
            let owning_turn = turn;
            if owning_turn >= threshold {
                continue;
            }
            let drop = match record.as_ref() {
                LogRecord::ToolResultRecord { .. } => true,
                LogRecord::Assistant { content, .. } => content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolUse { .. })),
                _ => false,
            };
            if drop {
                ops.push(PathOp::Omit { node: node.record });
            }
        }
        if ops.is_empty() {
            return CurateOutcome::Unchanged;
        }
        match base.derive(&ops) {
            Ok(derivation) => CurateOutcome::Derived(derivation),
            Err(err) => CurateOutcome::Failed {
                reason: format!("conway.trim: derive refused: {err}"),
            },
        }
    }
}

/// The plugin wrapper. Contributes no tools — one curator is the whole of
/// it.
#[derive(Debug)]
pub struct TrimPlugin(Arc<TrimOldToolResults>);

impl Default for TrimPlugin {
    fn default() -> Self {
        Self(Arc::new(TrimOldToolResults::default()))
    }
}

impl TrimPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_keep_turns(keep_turns: u32) -> Self {
        Self(Arc::new(TrimOldToolResults::new(keep_turns)))
    }
}

impl Plugin for TrimPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn curators(&self) -> Vec<Arc<dyn Curator>> {
        vec![self.0.clone() as Arc<dyn Curator>]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::plugin::TranscriptResolver;
    use conway::{AgentId, SessionId, SessionStore};

    /// Installs the SAME way an operator's `plugins.install` does: through
    /// `Plugin::curators`, never a constructor a third party couldn't reach.
    #[test]
    fn installs_exactly_one_curator_through_the_ordinary_plugin_surface() {
        let plugin = TrimPlugin::new();
        assert_eq!(plugin.manifest().id, PLUGIN_ID);
        assert_eq!(plugin.curators().len(), 1);
        assert!(plugin.tools().is_empty());
    }

    #[tokio::test]
    async fn an_empty_path_is_unchanged() {
        let curator = TrimOldToolResults::default();
        let store: Arc<dyn SessionStore> = Arc::new(conway_testkit::FakeStore::new());
        let ctx = CurateCtx {
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            turn: 0,
            model: None,
            store,
            resolver: Arc::new(TranscriptResolver::new(8)),
        };
        let base = ValidatedPath::default_path(Vec::new());
        let outcome = curator.curate(&ctx, &base).await;
        assert!(matches!(outcome, CurateOutcome::Unchanged));
    }
}
