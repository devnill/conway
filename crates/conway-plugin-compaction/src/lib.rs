//! `conway.compaction`: an ephemeral, every-turn fold of old `ToolResult`
//! segments into one labeled summary segment (board item
//! `01M1YVMTDJYEFC5PKSQHDRASJX`).
//!
//! # Why this exists, and why it is not a core opinion
//!
//! `docs/vision/CATALOGUE.md`'s entry "Ship the ephemeral compaction hook as
//! an installable plugin" packages `docs/plugins/cookbook.md` example 2's
//! `CompactOldToolResultsHook` -- code that already existed and already ran,
//! against a scratch crate outside this workspace, before this crate did --
//! as an installable, **off by default** `conway::plugin::Plugin`. This
//! crate is that packaging: the hook itself (below, as
//! [`CompactOldToolResults`]) is the cookbook's own logic, carried over with
//! one change (a configurable `fold_after_turns` in place of the cookbook's
//! hardcoded `keep_last: 1`), not a reimplementation.
//!
//! **The caveat, copied verbatim from `docs/vision/CATALOGUE.md`, because it
//! is load-bearing, not decoration:**
//!
//! > **The caveat, stated as plainly as the cookbook states it.** This is
//! > **explicitly not** what most people mean by "compaction": it recomputes
//! > the fold every turn (nothing persists it — `LogRecord::ContextMask` has
//! > no producer anywhere in the tree, and the item that would have built one
//! > was filed and then cancelled per `docs/plugins/hooks.md` point 9's
//! > citation), and `INTENT.md` §3 calls compaction "the enemy, not a
//! > feature" for good reason: it is a lossy, unauditable summary applied to
//! > material someone was reasoning over. Ship it anyway, off by default,
//! > labeled honestly as the weaker ephemeral form — because *some* users
//! > will want it regardless of the house opinion, and "you install it, you
//! > chose it" is the whole point of `PHILOSOPHY.md` §5's default-set test.
//! > Do not let this entry read as "conway now has compaction" on a
//! > feature-comparison chart; it has the weakest version of the feature
//! > everyone else means by that word.
//!
//! # What it does, precisely
//!
//! [`ContextHook::before_request`] scans the just-assembled
//! [`conway::plugin::ContextPayload`]'s segments for every one whose
//! [`conway::plugin::Provenance`] is `ToolResult`, keeps the
//! [`CompactOldToolResults::fold_after_turns`] most recent ones verbatim, and
//! replaces every OLDER one with a single summary segment: a short,
//! mechanically truncated (80 characters) excerpt of each folded result's
//! own text, joined under a `[conway.compaction folded N earlier tool
//! result(s) ...]` header. The summary segment's own
//! [`conway::plugin::Provenance`] is `SystemNote` with a `reason` naming
//! `conway.compaction` and the exact count folded -- the "distinct
//! provenance naming the plugin and how many results it folded" `/context`
//! (`conway-cli`'s `provenance_label`) reads and renders, so an operator
//! sees `system note: conway.compaction: folded N earlier tool result(s)`
//! rather than an anonymous note indistinguishable from any other one. A
//! session with `fold_after_turns` or fewer `ToolResult` segments is
//! returned completely unchanged -- there is nothing yet to fold.
//!
//! **Named `fold_after_turns`, counted in `ToolResult` segments.** A
//! `ContextHook` sees the assembled `Vec<PromptSegment>` for one request --
//! not the underlying `LogRecord`s a [`conway::plugin::Curator`] like
//! `conway-plugin-trim`'s own `TrimOldToolResults` walks, which is how that
//! crate's `keep_turns` can genuinely count session turns (it tracks
//! `Assistant` record boundaries while it walks). A `PromptSegment` carries
//! no turn number of its own, so this hook orders by POSITION in the
//! assembled payload instead -- the newest `ToolResult` segments are always
//! last, because `ContextBuilder` appends records in the order the log
//! wrote them. In ordinary use (at most one tool call/result pair per turn)
//! that position IS the turn boundary, so `fold_after_turns = N` means "keep
//! the `ToolResult`s from roughly the last `N` turns verbatim" in the
//! overwhelmingly common case; a turn that issues several tool calls at once
//! makes this an approximation (several `ToolResult`s from ONE turn can
//! straddle the keep/fold boundary), which is disclosed here rather than
//! discovered.
//!
//! # What it does NOT do
//!
//! - **No persistence.** Nothing here appends a `LogRecord::ContextMask` or
//!   any other durable record -- `ContextHook::before_request`'s only
//!   channel back to the runtime is the `ContextPayload` it returns for the
//!   CURRENT request, which is why the fold is recomputed, identically,
//!   from the full unfolded segment list, on every single turn.
//! - **No summarizing of user/assistant text.** Only segments whose
//!   `Provenance` is `ToolResult` are ever folded; a `UserPrompt`, `Skill`,
//!   `AgentDef`, or `Assistant` segment is never touched, matched, or
//!   counted by this hook.
//! - **No model call.** The summary is a mechanical, truncated excerpt of
//!   each folded result's own first content block, computed locally --
//!   never an LLM-written digest, and never a network or disk round trip.
//! - **No change to the admission gate.** This plugin's only seam is
//!   `ContextHook::before_request`; an overflowing request this hook cannot
//!   fold small enough is still refused loudly by the runtime's own T-1
//!   admission gate, exactly as it would be with no compaction hook
//!   installed at all.
//!
//! # Installing it
//!
//! Off by default -- NOT a member of `conway-cli`'s `DEFAULT_OPINION_SET`
//! (`crates/conway-cli/src/first_party_plugins.rs`), and not installed by
//! any other mechanism either. An operator who wants it opts in explicitly:
//!
//! ```json
//! { "plugins": { "install": ["conway.compaction"] } }
//! ```
//!
//! # Configuring the fold: `[plugins.config.conway.compaction]`
//!
//! `DEFAULT_FOLD_AFTER_TURNS` (1, the cookbook's own `keep_last: 1`) is only
//! the default -- set `fold_after_turns` directly to change it:
//!
//! ```json
//! { "plugins": {
//!     "install": ["conway.compaction"],
//!     "config": { "conway.compaction": { "fold_after_turns": 3 } }
//! } }
//! ```
//!
//! `fold_after_turns` must be a JSON integer `>= 0`. Unlike
//! `conway-plugin-trim`'s `keep_turns`, `0` is a legitimate value here --
//! "fold every `ToolResult` segment, keep none verbatim" is a real
//! (aggressive) policy choice, not a value that silently means something
//! else. Any OTHER key under `conway.compaction`'s own table is refused BY
//! NAME ([`conway::plugin::PluginConfigureError::UnknownKey`]), never
//! silently dropped -- the same discipline `conway-plugin-trim::TrimPlugin::
//! configure` established as this mechanism's first implementor, applied
//! here by [`CompactionPlugin`]'s own [`Plugin::configure`] as its second
//! one. An operator's `[plugins.config.conway.compaction]` table is
//! validated even if `"conway.compaction"` is absent from
//! `[plugins].install` -- the same "validate every candidate this table
//! names, whether or not it ends up selected" posture every other
//! first-party plugin's config takes.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use conway::plugin::{
    ContentBlock, ContextHook, ContextHookCtx, ContextPayload, Plugin, PluginConfigureError,
    PluginDescription, PluginManifest, PromptSegment, Provenance, Role, Tool,
};

/// The install id an operator names in `plugins.install`.
pub const PLUGIN_ID: &str = "conway.compaction";

/// The default fold threshold: keep the single most recent `ToolResult`
/// segment verbatim, fold everything older. Taken directly from the
/// cookbook's own worked example (`CompactOldToolResultsHook { keep_last: 1
/// }`, `docs/plugins/cookbook.md`) -- not independently measured, the same
/// "picked, not measured" honesty `conway-plugin-trim::DEFAULT_KEEP_TURNS`
/// states for its own constant.
pub const DEFAULT_FOLD_AFTER_TURNS: usize = 1;

/// Folds every `ToolResult`-provenance segment except the
/// [`Self::fold_after_turns`] most recent ones into a single summary
/// segment, every time [`ContextHook::before_request`] runs. See the module
/// doc for the full "what it does/does not do" contract and the exact
/// meaning of `fold_after_turns`.
#[derive(Debug, Clone, Copy)]
pub struct CompactOldToolResults {
    pub fold_after_turns: usize,
}

impl Default for CompactOldToolResults {
    fn default() -> Self {
        Self {
            fold_after_turns: DEFAULT_FOLD_AFTER_TURNS,
        }
    }
}

impl CompactOldToolResults {
    pub fn new(fold_after_turns: usize) -> Self {
        Self { fold_after_turns }
    }
}

#[async_trait]
impl ContextHook for CompactOldToolResults {
    async fn before_request(
        &self,
        _ctx: &ContextHookCtx,
        payload: ContextPayload,
    ) -> ContextPayload {
        let ContextPayload { segments, tools } = payload;

        let result_idxs: Vec<usize> = segments
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.provenance, Provenance::ToolResult { .. }))
            .map(|(i, _)| i)
            .collect();

        if result_idxs.len() <= self.fold_after_turns {
            return ContextPayload { segments, tools };
        }

        let fold_count = result_idxs.len() - self.fold_after_turns;
        let fold_set: HashSet<usize> = result_idxs[..fold_count].iter().copied().collect();

        let mut summary_lines = Vec::new();
        for &i in &result_idxs[..fold_count] {
            if let Some(ContentBlock::Text { text }) = segments[i].content.first() {
                let head: String = text.chars().take(80).collect();
                summary_lines.push(format!("- {head}"));
            }
        }

        let summary = PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: format!(
                    "[conway.compaction folded {} earlier tool result(s) -- ephemeral, \
                     recomputed every request; the session log is untouched]\n{}",
                    fold_set.len(),
                    summary_lines.join("\n")
                ),
            }],
            // Names the plugin AND the count, so `/context`'s rendering of
            // this segment's provenance (`system note: <reason>`) never
            // reads as an anonymous or ambiguous note -- see the module
            // doc's "What it does, precisely" section.
            Provenance::SystemNote {
                reason: format!(
                    "conway.compaction: folded {} earlier tool result(s)",
                    fold_set.len()
                ),
            },
        );

        let mut new_segments = Vec::with_capacity(segments.len() - fold_set.len() + 1);
        let mut inserted = false;
        for (i, seg) in segments.into_iter().enumerate() {
            if fold_set.contains(&i) {
                if !inserted {
                    new_segments.push(summary.clone());
                    inserted = true;
                }
                continue;
            }
            new_segments.push(seg);
        }
        ContextPayload {
            segments: new_segments,
            tools,
        }
    }
}

/// The plugin wrapper. Contributes no tool -- one `ContextHook` is the whole
/// of it, exactly like `conway-plugin-memory`/`conway-plugin-toolindex`'s
/// own hook-only shape.
#[derive(Debug)]
pub struct CompactionPlugin(Arc<CompactOldToolResults>);

impl Default for CompactionPlugin {
    fn default() -> Self {
        Self(Arc::new(CompactOldToolResults::default()))
    }
}

impl CompactionPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_fold_after_turns(fold_after_turns: usize) -> Self {
        Self(Arc::new(CompactOldToolResults::new(fold_after_turns)))
    }
}

impl Plugin for CompactionPlugin {
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

    /// An honest "what does flipping this on/off change" for an operator
    /// (the plugin browser's own read surface) -- `you_get`/`you_lose` state
    /// exactly the module doc's "what it does"/"what it does NOT do"
    /// sections in the operator's own words, never the trait's empty
    /// default.
    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "folds old tool results into one ephemeral summary segment -- the weakest, \
                      non-persistent form of 'compaction'"
                .to_string(),
            you_get: format!(
                "every ToolResult segment except the {} most recent is folded, every turn, into \
                 one labeled summary segment carrying its own provenance (conway.compaction and \
                 how many results it folded, visible in /context)",
                self.0.fold_after_turns
            ),
            you_lose: "no persistence -- the fold is recomputed from scratch on every single \
                       turn, nothing is written to the session log; no summarizing of \
                       user/assistant text, only ToolResult segments ever fold; no model call, \
                       the summary is a mechanically truncated excerpt, never an LLM-written \
                       digest; and the model can no longer see a folded result's own content \
                       beyond its first 80 characters"
                .to_string(),
            costs: "one linear pass over the assembled segments per turn; no network or disk \
                    I/O of its own"
                .to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn context_hooks(&self) -> Vec<Arc<dyn ContextHook>> {
        vec![self.0.clone() as Arc<dyn ContextHook>]
    }

    /// `value` must be a JSON object; every key is validated before
    /// anything is applied (`&mut self` is only mutated once validation of
    /// the WHOLE object has succeeded, at the bottom of this method), the
    /// same all-or-nothing discipline `conway_plugin_trim::TrimPlugin::
    /// configure` established as this mechanism's first implementor.
    ///
    /// - `"fold_after_turns"`: must be a JSON integer `>= 0` and `<=
    ///   usize::MAX`. `0` is accepted (fold everything, keep nothing
    ///   verbatim) -- deliberately NOT refused the way `conway.trim`
    ///   refuses `keep_turns = 0`, because "fold every tool result" is a
    ///   coherent, if aggressive, policy for this plugin, unlike
    ///   `conway.trim`'s "keep nothing" (see this crate's own module doc,
    ///   "Configuring the fold").
    /// - Any other key is refused BY NAME
    ///   (`PluginConfigureError::UnknownKey`), never silently dropped.
    fn configure(&mut self, value: &serde_json::Value) -> Result<(), PluginConfigureError> {
        let object = value
            .as_object()
            .ok_or_else(|| PluginConfigureError::NotAnObject {
                actual: json_value_kind(value).to_string(),
            })?;
        let mut fold_after_turns = self.0.fold_after_turns;
        for (key, raw) in object {
            match key.as_str() {
                "fold_after_turns" => {
                    let n = raw
                        .as_u64()
                        .ok_or_else(|| PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: "must be a JSON integer".to_string(),
                        })?;
                    fold_after_turns =
                        usize::try_from(n).map_err(|_| PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: format!("must fit in a usize, got {n}"),
                        })?;
                }
                other => {
                    return Err(PluginConfigureError::UnknownKey {
                        key: other.to_string(),
                    });
                }
            }
        }
        self.0 = Arc::new(CompactOldToolResults::new(fold_after_turns));
        Ok(())
    }
}

/// The JSON type-name `PluginConfigureError::NotAnObject` reports -- mirrors
/// `conway_plugin_trim`'s own private helper of the same name and shape
/// exactly, since `serde_json::Value` has no built-in `Display` for "which
/// variant is this".
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
    use conway::{AgentId, SessionId};

    fn artifacts_handle() -> conway::plugin::ArtifactWriteHandle {
        conway::plugin::ArtifactWriteHandle::noop(AgentId::new())
    }

    fn hook_ctx() -> ContextHookCtx {
        let agent_id = AgentId::new();
        ContextHookCtx {
            agent_id,
            agent_path: vec![agent_id],
            session_id: SessionId::new(),
            turn: 3,
            model: None,
            estimated_tokens: 100,
            artifacts: artifacts_handle(),
            tag: None,
        }
    }

    fn user_prompt(text: &str) -> PromptSegment {
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            Provenance::UserPrompt,
        )
    }

    fn tool_result(call_id: &str, tool: &str, text: &str) -> PromptSegment {
        PromptSegment::new(
            Role::ToolResult,
            vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            Provenance::ToolResult {
                call_id: call_id.to_string(),
                tool: conway::ToolName::new(tool),
            },
        )
    }

    /// Installs the SAME way an operator's `plugins.install` does: through
    /// `Plugin::context_hooks`, never a constructor a third party couldn't
    /// reach -- mirrors `conway-plugin-trim`'s own
    /// `installs_exactly_one_curator_through_the_ordinary_plugin_surface`.
    #[test]
    fn installs_exactly_one_context_hook_through_the_ordinary_plugin_surface() {
        let plugin = CompactionPlugin::new();
        assert_eq!(plugin.manifest().id, PLUGIN_ID);
        assert_eq!(plugin.context_hooks().len(), 1);
        assert!(plugin.tools().is_empty());
    }

    #[test]
    fn description_is_non_empty() {
        let description = CompactionPlugin::new().description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
    }

    /// Ported from `docs/plugins/cookbook.md` example 2's
    /// `older_tool_results_fold_into_one_attributed_summary` (the cookbook's
    /// own scratch-crate test, elided there for presentation): three tool
    /// results behind one user prompt, `fold_after_turns = 1` keeps only the
    /// newest verbatim and folds the other two into one summary segment.
    ///
    /// **Also the required "folded segment carries the plugin's own
    /// provenance" proof:** the assertion on `Provenance::SystemNote`
    /// below is what makes `/context` (`conway-cli`'s `provenance_label`,
    /// `format!("system note: {reason}")`) name `conway.compaction` and the
    /// fold count rather than rendering an anonymous note.
    #[tokio::test]
    async fn older_tool_results_fold_into_one_attributed_summary() {
        let payload = ContextPayload {
            segments: vec![
                user_prompt("please read three files"),
                tool_result("tc_1", "read_file", "contents of the first, oldest file"),
                tool_result("tc_2", "read_file", "contents of the second file"),
                tool_result("tc_3", "read_file", "contents of the third, most recent file"),
            ],
            tools: vec![],
        };
        let hook = CompactOldToolResults { fold_after_turns: 1 };

        let out = hook.before_request(&hook_ctx(), payload).await;

        // UserPrompt + one folded summary + the kept-verbatim last ToolResult.
        assert_eq!(out.segments.len(), 3);
        let Provenance::SystemNote { reason } = &out.segments[1].provenance else {
            panic!("expected the folded summary at index 1 to carry Provenance::SystemNote")
        };
        assert!(
            reason.contains("conway.compaction"),
            "provenance reason must name conway.compaction so /context attributes it, got: \
             {reason}"
        );
        assert!(
            reason.contains("folded 2 earlier tool result"),
            "provenance reason must name how many results were folded, got: {reason}"
        );
        let ContentBlock::Text { text } = &out.segments[1].content[0] else {
            panic!("expected the folded summary's content to be text")
        };
        assert!(text.contains("folded 2 earlier tool result"));
        assert!(text.contains("contents of the first, oldest file"));
        assert!(text.contains("contents of the second file"));
        // The kept-verbatim third result's own text never appears in the
        // SUMMARY segment (it still appears, untouched, in its own segment).
        assert!(!text.contains("contents of the third, most recent file"));
        let Provenance::ToolResult { call_id, .. } = &out.segments[2].provenance else {
            panic!("expected the newest ToolResult to survive verbatim at index 2")
        };
        assert_eq!(call_id, "tc_3");
    }

    /// A session with `fold_after_turns` or fewer `ToolResult` segments is
    /// returned completely unchanged -- the module doc's "nothing yet to
    /// fold" case, and the cookbook's own pass-through branch.
    #[tokio::test]
    async fn too_few_tool_results_pass_through_unchanged() {
        let payload = ContextPayload {
            segments: vec![
                user_prompt("one question"),
                tool_result("tc_1", "read_file", "the only result"),
            ],
            tools: vec![],
        };
        let hook = CompactOldToolResults::default(); // fold_after_turns = 1
        let out = hook.before_request(&hook_ctx(), payload.clone()).await;
        assert_eq!(out.segments, payload.segments);
    }

    /// Config-parse test: `fold_after_turns` actually reaches the
    /// installed hook and changes what it folds, not merely parses --
    /// mirrors `conway-plugin-trim`'s own
    /// `configuring_keep_turns_changes_what_conway_trim_actually_omits`.
    #[tokio::test]
    async fn configuring_fold_after_turns_changes_what_the_hook_actually_folds() {
        let payload = ContextPayload {
            segments: vec![
                tool_result("tc_1", "read_file", "oldest"),
                tool_result("tc_2", "read_file", "middle"),
                tool_result("tc_3", "read_file", "newest"),
            ],
            tools: vec![],
        };

        let mut plugin = CompactionPlugin::new();
        plugin
            .configure(&serde_json::json!({ "fold_after_turns": 2 }))
            .expect("fold_after_turns = 2 is a valid config value");
        let hook = plugin
            .context_hooks()
            .into_iter()
            .next()
            .expect("CompactionPlugin always contributes exactly one context hook");
        let out = hook.before_request(&hook_ctx(), payload).await;

        // fold_after_turns = 2 keeps the two newest verbatim and folds only
        // the single oldest one -- a DIFFERENT split than the default (1
        // kept, 2 folded) exercised above, proving the configured value
        // reached the installed hook rather than being parsed and dropped.
        assert_eq!(out.segments.len(), 3);
        let Provenance::SystemNote { reason } = &out.segments[0].provenance else {
            panic!("expected the folded summary first, naming the single oldest result")
        };
        assert!(reason.contains("folded 1 earlier tool result"));
        for (idx, expected_call_id) in [(1, "tc_2"), (2, "tc_3")] {
            let Provenance::ToolResult { call_id, .. } = &out.segments[idx].provenance else {
                panic!("expected a surviving ToolResult at index {idx}")
            };
            assert_eq!(call_id, expected_call_id);
        }
    }

    /// Required pair, half 2: an unknown key under
    /// `[plugins.config.conway.compaction]` must fail `configure`, naming
    /// the offending key -- never silently ignored, mirroring
    /// `conway-plugin-trim`'s own `configure_refuses_an_unknown_key_by_name`.
    #[test]
    fn configure_refuses_an_unknown_key_by_name() {
        let mut plugin = CompactionPlugin::new();
        let err = plugin
            .configure(&serde_json::json!({ "fold_after_trns": 2 }))
            .expect_err("a typo'd/unrecognized key must be refused, not silently ignored");
        match err {
            PluginConfigureError::UnknownKey { key } => {
                assert_eq!(key, "fold_after_trns", "the error must name the offending key");
            }
            other => panic!("expected UnknownKey naming 'fold_after_trns', got {other:?}"),
        }
    }

    /// Deliberately the OPPOSITE of `conway-plugin-trim`'s
    /// `configure_refuses_a_keep_turns_of_zero`: `0` means "fold everything,
    /// keep nothing verbatim" here, a coherent policy rather than a value
    /// that silently means something else -- see this crate's module doc,
    /// "Configuring the fold", for why the two plugins differ on this point.
    #[test]
    fn configure_accepts_a_fold_after_turns_of_zero() {
        let mut plugin = CompactionPlugin::new();
        plugin
            .configure(&serde_json::json!({ "fold_after_turns": 0 }))
            .expect("fold_after_turns = 0 is a valid, if aggressive, config value");
    }

    /// `configure` must reject a non-object value by name, the same
    /// `NotAnObject` contract every other first-party plugin's `configure`
    /// honours.
    #[test]
    fn configure_refuses_a_non_object_value() {
        let mut plugin = CompactionPlugin::new();
        let err = plugin
            .configure(&serde_json::json!(3))
            .expect_err("a bare scalar must be refused, not interpreted as a key/value map");
        assert!(matches!(err, PluginConfigureError::NotAnObject { ref actual } if actual == "number"));
    }

    /// `Plugin::description().you_get` already interpolates
    /// `self.0.fold_after_turns` -- this pins that a `configure`d value is
    /// what that text reports, not the constructor default, mirroring
    /// `conway-plugin-trim`'s own
    /// `description_reflects_a_configured_window_not_the_default`.
    #[test]
    fn description_reflects_a_configured_fold_not_the_default() {
        let mut plugin = CompactionPlugin::new();
        plugin
            .configure(&serde_json::json!({ "fold_after_turns": 4 }))
            .expect("fold_after_turns = 4 is a valid config value");
        let you_get = plugin.description().you_get;
        assert!(
            you_get.contains('4'),
            "description().you_get must reflect the configured fold (4), got: {you_get}"
        );
    }
}
