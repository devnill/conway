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
//!
//! # Configuring the window: `[plugins.config.conway.trim]`
//!
//! `DEFAULT_KEEP_TURNS` (8) is only the DEFAULT now, not the only value
//! reachable from `settings.json` (board item `01M1YVM9CHFCJ6112XDYHCFS84`).
//! An operator who wants a smaller window (a small context budget) or a
//! larger one (a session worth keeping more of) sets `keep_turns` directly:
//!
//! ```json
//! { "plugins": {
//!     "install": ["conway.trim"],
//!     "config": { "conway.trim": { "keep_turns": 3 } }
//! } }
//! ```
//!
//! `keep_turns` must be a JSON integer `>= 1` -- `0` would mean "keep
//! nothing", which is not what this plugin does (it always keeps at least
//! the newest round-trip; see `curate`'s own doc). Any OTHER key under
//! `conway.trim`'s own table is refused BY NAME
//! (`conway_core::ports::PluginConfigureError::UnknownKey`), never silently
//! dropped -- see [`TrimPlugin::configure`]'s own doc for the exact
//! validation this performs, and `conway_core::ports::Plugin::configure`'s
//! own doc for the general seam this is the first real implementor of.
//!
//! **This is the general answer, not an exception argued for this one
//! constant.** Earlier revisions of this doc comment argued the opposite --
//! that `[plugins].install` deciding *whether* a first-party plugin runs,
//! never *how*, was settled policy, and that this specific constant was
//! additionally a poor candidate even if the mechanism existed (a curation
//! heuristic with no feedback loop, unlike an actual budget). That
//! blanket rule is what board item `01M1YVM9CHFCJ6112XDYHCFS84` changed:
//! `docs/plugins/authoring.md`'s "Configuration" section and `PHILOSOPHY.md`
//! §6 now describe the built `[plugins.config.<id>]`/`Plugin::configure`
//! seam this crate is the first real consumer of, and this crate's own
//! `TrimPlugin::configure` is the worked example a future plugin author
//! copies from -- see `docs/plugins/trim.md`.
//!
//! An embedder who wants a different window with no `settings.json` at all
//! still has the reachable path this crate always had: constructing
//! `TrimPlugin::with_keep_turns` directly in Rust, the same way
//! `conway_plugin_memory::MemoryPlugin::new` takes a caller-supplied
//! `MemoryConfig` today.

use std::sync::Arc;

use async_trait::async_trait;
use conway::plugin::{
    ContentBlock, CurateCtx, CurateOutcome, Curator, Plugin, PluginConfigureError,
    PluginDescription, PluginManifest, Tool,
};
use conway::{LogRecord, PathOp, ValidatedPath};

/// The install id an operator names in `plugins.install`.
pub const PLUGIN_ID: &str = "conway.trim";

/// The default window: keep the last 8 turns' tool round-trips, drop older
/// ones. Arbitrary but small enough to matter on a session worth curating at
/// all.
///
/// **Provenance: picked, not measured.** This crate's introducing commit
/// (`df5de41`) already called it "arbitrary" the day it was written -- no
/// session-cost benchmark, user study, or token-budget model ever compared
/// 8 against 6 or 12 or anything else. Nobody recorded why 8 specifically,
/// beyond "small enough to matter"; that absence is the honest note, not a
/// gap this doc comment is pretending to fill. See the module doc's "No
/// `settings.json` knob" section for why that is left as a constant rather
/// than exposed for an operator to pick their own guess instead.
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
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    /// The plugin browser's own read surface (board item
    /// `01M0KARX71A64NTSYTDBVANVPF`) -- an honest "what does flipping this
    /// on/off change" for an operator, not the trait's empty default. No
    /// tool/command/instruction to name (this plugin's whole contribution
    /// is the curator itself, see `curators` below), so `you_get` names the
    /// SHAPE of what it drops rather than a tool an operator would call.
    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "drops old tool call/result round-trips to save context room".to_string(),
            you_get: format!(
                "tool call/result round-trips older than {} turns behind the current one are \
                 omitted from what the model sees, never reordered",
                self.0.keep_turns
            ),
            you_lose: "the model can no longer see or reference a tool result once it ages out \
                       of the window -- including any prose the same response mixed in with the \
                       call, since omission works at whole-record granularity"
                .to_string(),
            costs: "one pass over the session's path per turn to decide what to drop; no \
                    network or disk I/O of its own"
                .to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn curators(&self) -> Vec<Arc<dyn Curator>> {
        vec![self.0.clone() as Arc<dyn Curator>]
    }

    /// The first real implementor of `conway_core::ports::Plugin::configure`
    /// (board item `01M1YVM9CHFCJ6112XDYHCFS84`) -- see the module doc's
    /// "Configuring the window" section for the `settings.json` shape this
    /// answers.
    ///
    /// `value` must be a JSON object; every key it carries is validated
    /// before anything is applied, so a value that is partly valid and
    /// partly not changes nothing (`&mut self` is only mutated once
    /// validation of the WHOLE object has already succeeded, at the bottom
    /// of this method -- never incrementally as each key is read).
    ///
    /// - `"keep_turns"`: must be a JSON integer `>= 1` and `<= u32::MAX`.
    ///   `0` is refused rather than silently clamped to `1` -- a value that
    ///   plainly means something other than what the operator intended
    ///   (this plugin never keeps *nothing*; see `curate`'s own doc) should
    ///   fail the load loudly, not be quietly reinterpreted.
    /// - Any other key -- a typo (`"keep_trns"`), a renamed field, a key
    ///   belonging to a DIFFERENT plugin pasted into the wrong table -- is
    ///   refused BY NAME (`PluginConfigureError::UnknownKey`), never
    ///   silently ignored. This is the check `tests::configure_refuses_an_
    ///   unknown_key_by_name` establishes never regresses to a no-op.
    fn configure(&mut self, value: &serde_json::Value) -> Result<(), PluginConfigureError> {
        let object = value
            .as_object()
            .ok_or_else(|| PluginConfigureError::NotAnObject {
                actual: json_value_kind(value).to_string(),
            })?;
        let mut keep_turns = self.0.keep_turns;
        for (key, raw) in object {
            match key.as_str() {
                "keep_turns" => {
                    let n = raw
                        .as_u64()
                        .ok_or_else(|| PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: "must be a JSON integer".to_string(),
                        })?;
                    if n == 0 {
                        return Err(PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: "must be >= 1".to_string(),
                        });
                    }
                    keep_turns =
                        u32::try_from(n).map_err(|_| PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: format!("must fit in a u32, got {n}"),
                        })?;
                }
                other => {
                    return Err(PluginConfigureError::UnknownKey {
                        key: other.to_string(),
                    });
                }
            }
        }
        self.0 = Arc::new(TrimOldToolResults::new(keep_turns));
        Ok(())
    }
}

/// The JSON type-name `PluginConfigureError::NotAnObject` reports --
/// `serde_json::Value` has no built-in `Display` for "which variant is
/// this", so this names the six wire kinds by hand rather than leaking a
/// `{...full value...}` dump into an error message a config file's author
/// has to read.
fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::plugin::TranscriptResolver;
    use conway::{
        AgentId, NodeProvenance, NodeStamp, PathNode, RecordRef, Selector, SessionId, SessionStore,
    };

    /// Installs the SAME way an operator's `plugins.install` does: through
    /// `Plugin::curators`, never a constructor a third party couldn't reach.
    #[test]
    fn installs_exactly_one_curator_through_the_ordinary_plugin_surface() {
        let plugin = TrimPlugin::new();
        assert_eq!(plugin.manifest().id, PLUGIN_ID);
        assert_eq!(plugin.curators().len(), 1);
        assert!(plugin.tools().is_empty());
    }

    /// The plugin browser's own read surface (board item
    /// `01M0KARX71A64NTSYTDBVANVPF`): a real description, never the
    /// trait's empty default -- matches the same standard every other
    /// first-party plugin's own `description_is_non_empty` test pins.
    #[test]
    fn description_is_non_empty() {
        let description = TrimPlugin::new().description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
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

    /// One synthetic tool call/result round-trip at turn `turn_idx`, as the
    /// two raw JSON records `LogRecord` decodes from (the same shape
    /// `tests/curates_a_synthetic_session.rs`'s own fixture uses) --
    /// `seq_base`/`seq_base + 1` are the assistant record's and its
    /// answering tool result's own sequence numbers.
    fn round_trip_turn(seq_base: u32, turn_idx: u32) -> Vec<serde_json::Value> {
        let call_id = format!("call_{turn_idx}");
        vec![
            serde_json::json!({
                "kind": "assistant",
                "seq": seq_base,
                "ts": "2026-09-08T00:00:00Z",
                "content": [
                    { "type": "tool_use", "call_id": call_id, "name": "read_file", "arguments": {} }
                ],
                "model": { "backend": "kimi", "model": "k3" },
                "route_reason": null,
                "usage": {
                    "input_tokens": 1, "output_tokens": 1, "cache_read_tokens": 0,
                    "cache_write_tokens": 0, "reasoning_tokens": 0
                },
                "stop": "tool_use"
            }),
            serde_json::json!({
                "kind": "tool_result",
                "seq": seq_base + 1,
                "ts": "2026-09-08T00:00:00Z",
                "call_id": call_id,
                "tool": "read_file",
                "blocks": [{ "type": "text", "text": "ok" }],
                "is_error": false,
                "truncated": null
            }),
        ]
    }

    /// A `ValidatedPath` over `turns` synthetic round-trips (turn 0 oldest,
    /// `turns - 1` newest), built directly -- no `SessionStore` round trip
    /// needed, since `TrimOldToolResults::curate` never reads `ctx.store`.
    fn synthetic_transcript(session: SessionId, turns: u32) -> ValidatedPath {
        let records: Vec<LogRecord> = (0..turns)
            .flat_map(|t| round_trip_turn(1 + t * 2, t))
            .map(|v| serde_json::from_value(v).expect("valid synthetic record"))
            .collect();
        let now = chrono::Utc::now();
        let nodes: Vec<(PathNode, Arc<LogRecord>)> = records
            .iter()
            .enumerate()
            .map(|(i, rec)| {
                let seq = rec.seq().expect("non-header record has a seq");
                let node = PathNode {
                    record: RecordRef { session, seq },
                    stamp: if i == 0 {
                        NodeStamp::Head
                    } else {
                        NodeStamp::Own
                    },
                    prov: NodeProvenance {
                        selected_by: Selector::DefaultRule,
                        at: now,
                    },
                };
                (node, Arc::new(rec.clone()))
            })
            .collect();
        ValidatedPath::default_path(nodes)
    }

    /// Every `call_id` a `ToolResultRecord` still present in `path` answers
    /// -- reads `Derivation::path`'s own nodes back, so this asserts on
    /// trim's ACTUAL output (what a next request would actually see), not
    /// merely on the `PathOp` list an implementation could get right while
    /// still building the wrong final path.
    fn surviving_result_call_ids(path: &ValidatedPath) -> std::collections::BTreeSet<String> {
        path.nodes()
            .filter_map(|(_, rec)| match rec.as_ref() {
                LogRecord::ToolResultRecord { result, .. } => Some(result.call_id.clone()),
                _ => None,
            })
            .collect()
    }

    /// Required pair, half 1 (acceptance 1): `[plugins.config.conway.trim]
    /// keep_turns = 3` must change what `conway.trim` actually omits from a
    /// real transcript, not merely parse. Six synthetic round-trips, current
    /// turn = 6: the unconfigured default (8) leaves the whole transcript
    /// untouched (nothing is old enough yet, `threshold =
    /// 6.saturating_sub(8) == 0`); configuring `keep_turns = 3` makes
    /// `threshold = 6 - 3 == 3`, so the THREE OLDEST round-trips (turns 0,
    /// 1, 2) age out and the three most recent (turns 3, 4, 5) survive --
    /// asserted by name, on the derived path's own surviving tool results,
    /// not on the op count alone.
    ///
    /// **What a wrong `configure` implementation this catches:** one that
    /// parses `keep_turns` but never actually stores it back onto the
    /// curator (e.g. validates and discards) would leave EVERY assertion in
    /// this test identical to `an_empty_path_is_unchanged`'s -- the
    /// configured run would stay `Unchanged` exactly like the default one,
    /// which the `CurateOutcome::Derived` match below refuses to accept.
    #[tokio::test]
    async fn configuring_keep_turns_changes_what_conway_trim_actually_omits() {
        let session = SessionId::new();
        let base = synthetic_transcript(session, 6);
        let store: Arc<dyn SessionStore> = Arc::new(conway_testkit::FakeStore::new());
        let ctx = CurateCtx {
            agent_id: AgentId::new(),
            session_id: session,
            turn: 6,
            model: None,
            store,
            resolver: Arc::new(TranscriptResolver::new(8)),
        };

        // Baseline: the unconfigured default (8) leaves a 6-turn transcript
        // entirely untouched.
        let default_curator = TrimPlugin::new()
            .curators()
            .into_iter()
            .next()
            .expect("TrimPlugin always contributes exactly one curator");
        let default_outcome = default_curator.curate(&ctx, &base).await;
        assert!(
            matches!(default_outcome, CurateOutcome::Unchanged),
            "default keep_turns=8 should leave a 6-turn transcript unchanged, got \
             {default_outcome:?}"
        );

        // Configured: keep_turns=3 ages the three oldest round-trips out.
        let mut plugin = TrimPlugin::new();
        plugin
            .configure(&serde_json::json!({ "keep_turns": 3 }))
            .expect("keep_turns=3 is a valid config value");
        let configured_curator = plugin
            .curators()
            .into_iter()
            .next()
            .expect("TrimPlugin always contributes exactly one curator");
        let configured_outcome = configured_curator.curate(&ctx, &base).await;
        let derivation = match configured_outcome {
            CurateOutcome::Derived(d) => d,
            other => panic!("keep_turns=3 must actually change a 6-turn transcript, got {other:?}"),
        };

        let surviving = surviving_result_call_ids(&derivation.path);
        for turn in 0..3u32 {
            let call_id = format!("call_{turn}");
            assert!(
                !surviving.contains(&call_id),
                "turn {turn} is one of the three oldest and must have aged out under \
                 keep_turns=3, but {call_id}'s tool result survived: {surviving:?}"
            );
        }
        for turn in 3..6u32 {
            let call_id = format!("call_{turn}");
            assert!(
                surviving.contains(&call_id),
                "turn {turn} is one of the three most recent and must survive \
                 keep_turns=3, but {call_id}'s tool result is missing: {surviving:?}"
            );
        }
    }

    /// Required pair, half 2 (acceptance 1): an unknown key under
    /// `[plugins.config.conway.trim]` must fail `configure`, naming the
    /// offending key -- never silently ignored.
    ///
    /// **What this catches that the other half of the pair does not:** the
    /// companion test above would pass unchanged even if `configure`
    /// accepted (and silently dropped) any key it did not recognize, so
    /// long as it still applied `keep_turns` correctly -- a plugin that
    /// merges only the keys it understands and ignores the rest is exactly
    /// the "typo parses as a harmless no-op" defect this method exists to
    /// refuse. This test would fail against that implementation (no `Err`
    /// at all), and would also fail against one that returns a generic
    /// error not naming the key (the `UnknownKey { key }` match below).
    #[test]
    fn configure_refuses_an_unknown_key_by_name() {
        let mut plugin = TrimPlugin::new();
        let err = plugin
            .configure(&serde_json::json!({ "keep_tuns": 3 }))
            .expect_err("a typo'd/unrecognized key must be refused, not silently ignored");
        match err {
            PluginConfigureError::UnknownKey { key } => {
                assert_eq!(key, "keep_tuns", "the error must name the offending key");
            }
            other => panic!("expected UnknownKey naming 'keep_tuns', got {other:?}"),
        }
    }

    /// `keep_turns = 0` is refused rather than silently clamped -- this
    /// plugin never keeps *nothing* (see `curate`'s own doc), so a
    /// configured `0` means the operator's intent could not be honoured
    /// honestly.
    #[test]
    fn configure_refuses_a_keep_turns_of_zero() {
        let mut plugin = TrimPlugin::new();
        let err = plugin
            .configure(&serde_json::json!({ "keep_turns": 0 }))
            .expect_err("keep_turns = 0 must be refused");
        assert!(
            matches!(err, PluginConfigureError::InvalidValue { ref key, .. } if key == "keep_turns"),
            "expected InvalidValue naming 'keep_turns', got {err:?}"
        );
    }

    /// The plugin browser's own read surface (`Plugin::description`)
    /// already interpolates `self.0.keep_turns` into `you_get` -- this pins
    /// that a `configure`d window is what that text reports, not the
    /// constructor default, so a browser/`/context`-style summary shows the
    /// ACTIVE window rather than a stale one once this method is called.
    #[test]
    fn description_reflects_a_configured_window_not_the_default() {
        let mut plugin = TrimPlugin::new();
        plugin
            .configure(&serde_json::json!({ "keep_turns": 3 }))
            .expect("keep_turns=3 is a valid config value");
        let you_get = plugin.description().you_get;
        assert!(
            you_get.contains('3'),
            "description().you_get must reflect the configured window (3), got: {you_get}"
        );
        assert!(
            !you_get.contains('8'),
            "description().you_get must not still report the unconfigured default (8), got: \
             {you_get}"
        );
    }
}
