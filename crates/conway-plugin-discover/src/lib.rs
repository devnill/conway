//! `conway.discover`: the tool a model calls to find a session or record it
//! does not already hold a reference to -- the half of cherry-pick
//! (`01M0KZ6J0DF6XR1TVSDH2KDPRX`) that composing alone could not close
//! (board item `01M0PS8J3AK7Z7253Z3E3RD3GY`).
//!
//! # The gap this closes
//!
//! `compose_context_path` (`conway-plugin-path`) takes resolved
//! `(session, seq)` pairs -- correct per decision `01M0K4QT6MBXPD6PXMBBBD2P7B`
//! ("the operator states intent, a model resolves it"), but a model can
//! only resolve intent into a reference it ALREADY holds: its own session,
//! or a completed subagent's `transcript_ref`. "Bring in what we worked out
//! about the retry logic yesterday" names neither -- there is no way to
//! answer "which session is that" without this tool.
//!
//! # Tool, not host (argued, not assumed)
//!
//! `compose_context_path` is a tool because composition is a DECISION that
//! needs a model's interpretation before a mechanical call can run
//! (`conway_core::ports::ContextPathHost`'s own module doc). Discovery
//! needs the identical interpretation step -- turning "yesterday's retry
//! logic conversation" into a `text` substring and a `scope` is exactly the
//! kind of natural-language resolution `CurateCtx` cannot perform (it
//! carries no callable model, only `model: Option<ModelId>` as a sizing
//! identifier -- decision `01M0K4QT6MBXPD6PXMBBBD2P7B`'s own finding). So
//! this is a tool for the SAME reason, not by symmetry alone: an automatic,
//! every-turn host-side search would either interpret nothing (a fixed
//! keyword scheme no operator asked for) or cost tokens silently on a path
//! that has no model in the loop to decide whether the search was worth
//! it -- the identical re-entrancy/hidden-cost argument that kept curation
//! out of `CurateCtx` in the first place.
//!
//! # Search surface (argued): metadata always, content only when asked,
//! bounded either way
//!
//! `search_sessions` never reads a record body unless the caller supplies
//! `text` -- see `SearchSessionsArgs::text`'s own doc. Bare listing
//! (`ULID`s and timestamps) is not what an operator asks for in ordinary
//! language, but full free-text search over every session ever logged is
//! exactly the "quietly reads a thousand records" cost this project's
//! standing rule forbids. `max_sessions` is the caller-visible bound
//! BEFORE the call runs; `report_text` states what was actually scanned
//! and what it cost AFTER -- "knowable in advance, visible afterward," the
//! same pair `compose_context_path`'s own `CostEstimate` reporting
//! establishes for composition.
//!
//! # Reach (settled, not reopened here)
//!
//! `SessionSearchScope` is `conway_core::ports::discovery`'s own type, and
//! that module's doc argues the reach question already: central-root
//! `AllProjects` vs. project-local `CurrentProject`, no crawler, no
//! registry. This crate contributes no reach decision of its own -- it is
//! the surface a model reaches the port THROUGH, nothing more.
//!
//! # Composes with `compose_context_path`, does not replace it
//!
//! This tool only FINDS: a [`conway::SessionMatch`]'s `matched_records`
//! names `(session, seq)` pairs a model hands straight to
//! `compose_context_path`'s `include` list. Nothing here calls
//! `set_head`/`derive_with` -- see this crate's own end-to-end test
//! (`tests/search_then_compose_end_to_end.rs`) for the full round trip,
//! search finding a record neither started this turn nor spawned, through
//! composition, surviving a later turn.

use std::sync::Arc;

use conway::plugin::{
    async_trait, ContentBlock, PathArgs, PermissionClass, Plugin, PluginDescription,
    PluginManifest, RenderKind, Tool, ToolCall, ToolCategory, ToolCtx, ToolError, ToolOutput,
    ToolSpec, TruncationPolicy,
};
use conway::{SessionSearchQuery, SessionSearchResult, SessionSearchScope, ToolName};

/// This plugin's published manifest id.
pub const PLUGIN_ID: &str = "conway.discover";

/// The bare name `SearchSessionsTool` registers under.
pub const SEARCH_TOOL_NAME: &str = "search_sessions";

/// The bare name of this plugin's one [`conway::plugin::InstructionFragment`].
pub const INSTRUCTION_NAME: &str = "conway.discover.when_to_search";

/// The `conway.discover` plugin: contributes one tool (`SearchSessionsTool`)
/// and the paragraph telling a model when to reach for it. See this
/// crate's own module doc for the full design argument.
pub struct DiscoverPlugin;

impl Plugin for DiscoverPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![ToolName::new(SEARCH_TOOL_NAME)],
            required_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "lets the model find a session or record it does not already hold a \
                       reference to"
                .to_string(),
            you_get: format!(
                "1 tool ({SEARCH_TOOL_NAME}) and an instruction telling the model when to use \
                 it -- the model can search this project's (or, opted in, every project's) \
                 sessions and hand what it finds to compose_context_path"
            ),
            you_lose: "nothing else -- nothing is searched unless the model calls this tool"
                .to_string(),
            costs: format!(
                "none beyond the {SEARCH_TOOL_NAME} calls the model makes; each call states \
                 what it scanned and what it cost"
            ),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(SearchSessionsTool)]
    }

    fn instructions(&self) -> Vec<conway::plugin::InstructionFragment> {
        vec![conway::plugin::InstructionFragment {
            name: INSTRUCTION_NAME.to_string(),
            text: include_str!("../fragments/when_to_search.md").to_string(),
            tool_ids: vec![ToolName::new(SEARCH_TOOL_NAME)],
        }]
    }
}

/// Which sessions to search -- the wire spelling of
/// [`conway::SessionSearchScope`] (a model names a string, not a Rust
/// variant).
#[derive(Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ScopeArg {
    /// This project's own other sessions. The default: almost always what
    /// "yesterday" or "the other session" means (this crate's own module
    /// doc, "Search surface").
    #[default]
    CurrentProject,
    /// Every project under the central sessions root -- an explicit
    /// widening, never the default (`conway_core::ports::
    /// SessionSearchScope`'s own doc argues why).
    AllProjects,
}

impl From<ScopeArg> for SessionSearchScope {
    fn from(arg: ScopeArg) -> Self {
        match arg {
            ScopeArg::CurrentProject => SessionSearchScope::CurrentProject,
            ScopeArg::AllProjects => SessionSearchScope::AllProjects,
        }
    }
}

fn default_max_sessions() -> usize {
    20
}

/// Args for `SearchSessionsTool`.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SearchSessionsArgs {
    /// Which sessions to consider -- see [`ScopeArg`]'s own doc. Defaults
    /// to this project's own other sessions.
    #[serde(default)]
    scope: ScopeArg,
    /// Exact match against a session's own label, if it has one.
    #[serde(default)]
    label: Option<String>,
    /// Exact match against a session's own agent definition name.
    #[serde(default)]
    agent_def: Option<String>,
    /// A plain, case-insensitive substring to look for in each candidate
    /// session's own logged records. Omit this to search METADATA ONLY --
    /// which sessions exist, when, labeled how -- with zero record content
    /// ever read, regardless of `max_sessions`. Supplying it turns this
    /// into a real, bounded content scan (this crate's own module doc,
    /// "Search surface").
    #[serde(default)]
    text: Option<String>,
    /// The most sessions this call will ever open and read (metadata or
    /// content, whichever `text` selects), most-recent-first. The reply
    /// states whether more existed beyond this bound (`truncated`) --
    /// raise this and re-ask if so. Clamped into a sane range by the host
    /// regardless of what is asked for.
    #[serde(default = "default_max_sessions")]
    max_sessions: usize,
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
struct SearchSessionsTool;

#[async_trait]
impl Tool for SearchSessionsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(SEARCH_TOOL_NAME),
            description: "Find a session or record you do not already hold a reference to \
                          (not your own current session, not a completed subagent's \
                          transcript_ref). Metadata-only by default (which sessions exist); \
                          supplying `text` searches record content too, bounded by \
                          `max_sessions`. Hand a match's (session, seq) pairs to \
                          compose_context_path to actually bring one onto your context path."
                .to_string(),
            schema: schemars::schema_for!(SearchSessionsArgs),
            category: ToolCategory::Search,
            // Read-only: never writes a record, never changes a context
            // path. Unlike `compose_context_path` (RequiresApproval --
            // it mutates the session's head), nothing here has an effect
            // to approve.
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: SearchSessionsArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;

        let query = SessionSearchQuery {
            scope: args.scope.into(),
            label: args.label,
            agent_def: args.agent_def,
            text: args.text,
            max_sessions: args.max_sessions,
        };

        match ctx.session_discovery.search(query).await {
            Ok(result) => Ok(ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: report_text(&result),
                }],
                is_error: false,
                truncation: TruncationPolicy::None,
                artifacts: Vec::new(),
            }),
            Err(e) => Ok(error_output(format!("session search failed: {e}"))),
        }
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }
}

/// Renders a [`SessionSearchResult`] as the model reads it: what was
/// found, then what was searched and what it cost (this crate's own
/// module doc, "Search surface" -- knowable in advance via `max_sessions`,
/// visible afterward here).
fn report_text(result: &SessionSearchResult) -> String {
    let mut out = String::new();
    if result.matches.is_empty() {
        out.push_str("no matching sessions.\n");
    } else {
        for m in &result.matches {
            out.push_str(&format!(
                "session {} (project \"{}\", cwd {}, created {}",
                m.session,
                m.project_key,
                m.cwd.display(),
                m.created.to_rfc3339(),
            ));
            if let Some(agent_def) = &m.agent_def {
                out.push_str(&format!(", agent_def \"{agent_def}\""));
            }
            if !m.labels.is_empty() {
                out.push_str(&format!(", labels {:?}", m.labels));
            }
            out.push_str(")\n");
            for r in &m.matched_records {
                out.push_str(&format!("  seq {}: {}\n", r.seq, r.snippet));
            }
        }
    }
    out.push_str(&format!(
        "searched {} project(s), {} session(s) considered, {} session(s) content-scanned, \
         {} record(s) read.{}",
        result.projects_scanned,
        result.sessions_considered,
        result.sessions_content_scanned,
        result.records_scanned,
        if result.truncated {
            " More candidates existed beyond max_sessions -- raise it and re-ask to see them."
        } else {
            ""
        }
    ));
    out
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    /// The plugin browser's own read surface (board item
    /// `01M0KARX71A64NTSYTDBVANVPF`): a real description, never the
    /// trait's empty default.
    #[test]
    fn description_is_non_empty() {
        let description = DiscoverPlugin.description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
    }

    #[test]
    fn report_text_of_no_matches_says_so_and_still_states_cost() {
        let result = SessionSearchResult {
            projects_scanned: 1,
            sessions_considered: 3,
            ..Default::default()
        };
        let text = report_text(&result);
        assert!(text.contains("no matching sessions"));
        assert!(text.contains("1 project(s)"));
        assert!(text.contains("3 session(s) considered"));
    }

    #[test]
    fn report_text_names_truncation_when_the_bound_was_hit() {
        let result = SessionSearchResult {
            truncated: true,
            ..Default::default()
        };
        assert!(report_text(&result).contains("raise it and re-ask"));
    }
}
