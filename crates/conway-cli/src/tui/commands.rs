//! TUI slash-command parsing and dispatch (WI-115).
//!
//! Two responsibilities, kept apart on purpose so [`parse`] stays a pure,
//! state-free function while resolution and dispatch get to see the live
//! session: [`parse`] turns one line of raw input into a [`SlashCommand`]
//! with every argument still a raw `String` (no agent-id resolution, no
//! facade call); [`execute`] resolves agent-id arguments against the
//! caller's [`AppState`] tree and performs the one facade call each command
//! maps to (module notes' table), through the [`Host`] seam so dispatch
//! logic is testable without a live `Runtime`.
//!
//! None of this reaches past `SessionHandle`/`Conway` -- [`Host`] is a
//! thin abstraction over exactly those two types' methods, and [`LiveHost`]
//! is a pure delegation to them.

use std::collections::HashMap;

use conway::{
    AgentId, AgentTreeSnapshot, ContextReport, Conway, Event, ForkSpec, Provenance, RoutingReason,
    SessionHandle, SessionId, SpawnSpec, ToolSelector, Usage,
};

use super::state::{AppState, Entry};

/// One parsed slash command. Agent/session identifiers are still raw
/// strings here -- prefix resolution against the live tree happens in
/// [`execute`], the only place with a tree to resolve against.
#[derive(Debug, Clone, PartialEq)]
pub enum SlashCommand {
    Steer {
        target: String,
        text: String,
    },
    Tree,
    Context {
        agent: String,
    },
    Why,
    /// `agent` is `None` for a BARE `/fork`/`/fork <directive>` (WI "bare
    /// /spawn & /fork open an interactive session"): the child is created
    /// as a fresh, interactive KEEP-ALIVE session forked from the FOCUSED
    /// agent -- see [`execute`]'s own `Fork` arm and [`parse_fork`] for the
    /// exact forms this covers. `agent` is `Some` only for the explicit-
    /// target form `/fork @<agent> <directive>` (this item's generalization
    /// of the pre-existing `/fork <agent> <directive>`, unchanged in
    /// substance: an autonomous, non-keep-alive fork of that SPECIFIC live
    /// agent). `directive` is `None` when the caller supplies no first
    /// message -- the interactive child then idles until prompted
    /// (`Effect::FocusNewSession`'s own doc); for the explicit-target form
    /// `directive` is always `Some` (required, exactly as it always was).
    Fork {
        agent: Option<String>,
        directive: Option<String>,
    },
    /// `agent_def` is `None` when the caller omits it (`/spawn <prompt>`) --
    /// the spawned child then inherits the parent session's role/model (see
    /// [`parse`]'s `/spawn` branch and `conway::SpawnSpec`'s own doc).
    /// `prompt` is `None` for a BARE `/spawn`/`/spawn @<agent_def>` (this
    /// item): the child is created as a fresh, interactive KEEP-ALIVE
    /// session with no first message -- it idles until prompted.
    Spawn {
        agent_def: Option<String>,
        prompt: Option<String>,
    },
    Resume {
        sid: String,
    },
    Help,
    Quit,
}

/// A malformed slash command. `Display` always names the expected form, so
/// it can be surfaced verbatim as a transcript [`Entry::Notice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// Parses one line of raw input (leading `/` included, e.g. `"/steer a7
/// hold on"`) into a [`SlashCommand`].
///
/// Parsing rule (module notes): split on the first whitespace for the
/// command word; a command with a trailing free-text argument consumes the
/// remainder after its first argument verbatim -- no re-tokenization, no
/// quote handling.
pub fn parse(input: &str) -> Result<SlashCommand, ParseError> {
    let (word, rest) = split_command(input);
    match word {
        "/steer" => {
            let (target, text) = parse_two_arg(rest, "/steer <agent> <text>")?;
            Ok(SlashCommand::Steer { target, text })
        }
        "/tree" => {
            parse_no_arg(rest, "/tree")?;
            Ok(SlashCommand::Tree)
        }
        "/context" => {
            let agent = parse_one_arg(rest, "/context <agent>")?;
            Ok(SlashCommand::Context { agent })
        }
        "/why" => {
            parse_no_arg(rest, "/why")?;
            Ok(SlashCommand::Why)
        }
        "/fork" => {
            let (agent, directive) = parse_fork(rest, "/fork [@<agent> <directive>] | [<text>]")?;
            Ok(SlashCommand::Fork { agent, directive })
        }
        "/spawn" => {
            let (agent_def, prompt) = parse_spawn(rest, "/spawn [@<agent_def>] [<prompt>]")?;
            Ok(SlashCommand::Spawn { agent_def, prompt })
        }
        "/resume" => {
            let sid = parse_one_arg(rest, "/resume <session-id>")?;
            Ok(SlashCommand::Resume { sid })
        }
        "/help" => {
            parse_no_arg(rest, "/help")?;
            Ok(SlashCommand::Help)
        }
        "/quit" | "/exit" => {
            parse_no_arg(rest, word)?;
            Ok(SlashCommand::Quit)
        }
        other => Err(ParseError(format!(
            "unknown command `{other}` -- try /help"
        ))),
    }
}

/// Splits `input` into its command word and the (left-trimmed) remainder,
/// on the first whitespace run.
fn split_command(input: &str) -> (&str, &str) {
    match input.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim_start()),
        None => (input, ""),
    }
}

fn parse_no_arg(rest: &str, form: &str) -> Result<(), ParseError> {
    if rest.trim().is_empty() {
        Ok(())
    } else {
        Err(ParseError(format!("usage: {form} (no arguments)")))
    }
}

fn parse_one_arg(rest: &str, form: &str) -> Result<String, ParseError> {
    let value = rest.trim();
    if value.is_empty() {
        Err(ParseError(format!("usage: {form}")))
    } else {
        Ok(value.to_string())
    }
}

/// Splits `rest` into its first whitespace-delimited token and everything
/// after the single separating whitespace char, verbatim (module notes:
/// "consume the remainder verbatim, no re-tokenization"). Errors when
/// either half is missing -- covers both "no arguments at all" and "first
/// argument but no free-text second argument" (e.g. `/fork a7` with no
/// directive) under the same message.
fn parse_two_arg(rest: &str, form: &str) -> Result<(String, String), ParseError> {
    match rest.split_once(char::is_whitespace) {
        Some((first, text)) if !text.trim().is_empty() => Ok((first.to_string(), text.to_string())),
        _ => Err(ParseError(format!("usage: {form}"))),
    }
}

/// Parses `/spawn`'s argument list, where naming an `agent_def` is optional
/// (module notes / this item's own doc: no `agent_def` means the spawned
/// child inherits the parent session's role/model) AND -- since the "bare
/// /spawn & /fork open an interactive session" item -- the prompt itself is
/// now ALSO optional (a bare `/spawn`/`/spawn @<agent_def>` creates a fresh,
/// interactive keep-alive session with no first message; `execute` supplies
/// the first message later, via `Effect::FocusNewSession`, only if one was
/// given here). The unambiguous syntax for naming an `agent_def` is a
/// leading `@<agent_def>` token -- distinguishable from the prompt by the
/// `@` sigil with no positional guessing:
///
/// - `/spawn` -- no agent_def, no prompt: bare interactive spawn.
/// - `/spawn <prompt>` -- no agent_def, `prompt` is the first message.
/// - `/spawn @<agent_def>` -- names an agent_def, no first message.
/// - `/spawn @<agent_def> <prompt>` -- names an agent_def AND a first
///   message.
/// - `/spawn @@<prompt>` -- escape hatch: a prompt that must begin with a
///   literal `@` (no agent_def). Without this, a prompt like `@channel ...`
///   would be silently mis-split into an agent_def + a truncated prompt.
fn parse_spawn(rest: &str, _form: &str) -> Result<(Option<String>, Option<String>), ParseError> {
    if let Some(after_at_at) = rest.strip_prefix("@@") {
        // Literal-`@` prompt, no agent_def: re-attach the single `@` the
        // escape consumed and treat the whole thing as the prompt.
        let after = after_at_at.trim();
        let prompt = if after.is_empty() {
            None
        } else {
            Some(format!("@{after}"))
        };
        return Ok((None, prompt));
    }
    match rest.strip_prefix('@') {
        Some(after_at) => {
            let after_at = after_at.trim_start();
            match after_at.split_once(char::is_whitespace) {
                Some((agent_def, prompt)) if !agent_def.is_empty() => {
                    let prompt = prompt.trim();
                    let prompt = if prompt.is_empty() {
                        None
                    } else {
                        Some(prompt.to_string())
                    };
                    Ok((Some(agent_def.to_string()), prompt))
                }
                // No whitespace at all (or an empty leading token): the
                // entire remainder is the agent_def name, no prompt.
                _ if !after_at.is_empty() => Ok((Some(after_at.to_string()), None)),
                _ => Ok((None, None)),
            }
        }
        None => {
            let prompt = rest.trim();
            if prompt.is_empty() {
                Ok((None, None))
            } else {
                Ok((None, Some(prompt.to_string())))
            }
        }
    }
}

/// Parses `/fork`'s argument list. Generalizes the pre-existing explicit
/// two-argument form (`/fork <agent> <directive>`, forking a NAMED live
/// agent autonomously) to a leading `@<agent>` sigil -- mirroring
/// [`parse_spawn`]'s own `@` convention for the same reason (unambiguously
/// distinguishing "name a target" from free text, no positional guessing)
/// -- and adds the bare/optional-text forms the "bare /spawn & /fork open
/// an interactive session" item introduces:
///
/// - `/fork` -- no target, no directive: a bare interactive fork of the
///   FOCUSED agent (`execute` resolves it via `AppState::focused_agent`),
///   idling until prompted.
/// - `/fork <text>` -- no target; `text` (verbatim, however many words)
///   becomes the interactive child's first message.
/// - `/fork @<agent> <directive>` -- explicit target: forks that SPECIFIC
///   live agent with `directive` (both required, exactly like the
///   pre-this-item two-argument form did) -- `execute` keeps this
///   autonomous, NOT keep-alive.
/// - `/fork @@<text>` -- escape hatch: a first message that must begin with
///   a literal `@`, no explicit target.
fn parse_fork(rest: &str, form: &str) -> Result<(Option<String>, Option<String>), ParseError> {
    if rest.trim().is_empty() {
        return Ok((None, None));
    }
    if let Some(after_at_at) = rest.strip_prefix("@@") {
        let directive = parse_one_arg(&format!("@{after_at_at}"), form)?;
        return Ok((None, Some(directive)));
    }
    match rest.strip_prefix('@') {
        Some(after_at) => {
            let (agent, directive) = parse_two_arg(after_at, form)?;
            if agent.is_empty() {
                return Err(ParseError(format!("usage: {form}")));
            }
            Ok((Some(agent), Some(directive)))
        }
        None => {
            let directive = parse_one_arg(rest, form)?;
            Ok((None, Some(directive)))
        }
    }
}

/// The effect an executed command has on the caller's [`App`](super::app::App)
/// loop, beyond the [`AppState`] mutation `execute` already performed
/// directly.
pub enum Effect {
    /// Nothing further to do.
    None,
    /// `/quit` -- the app loop should exit.
    Quit,
    /// `/resume` succeeded -- the caller's active `SessionHandle` must be
    /// swapped for this one and its event stream resubscribed (`execute`
    /// cannot do either itself: both live in the app loop, not here).
    Resumed(SessionHandle),
    /// A bare/implicit `/spawn` or `/fork` succeeded (WI "bare /spawn &
    /// /fork open an interactive session"): `child` was created as a fresh,
    /// interactive KEEP-ALIVE session and must be auto-focused by the app
    /// loop (`app.rs` reuses the existing `Action::FocusAgent` path --
    /// `AppState::focus_agent` + re-subscribing `handle.agent_events(child)`
    /// -- neither of which `execute` can do itself: focus-switching needs
    /// the live facade, which only `app.rs` holds). `first_message`, when
    /// `Some` (the caller supplied `/spawn <text>`/`/fork <text>`), must
    /// then be delivered to `child` via `SessionHandle::prompt_agent` --
    /// again something only `app.rs` can do, since `execute` has no live
    /// handle either. Deliberately NOT baked into the `SpawnSpec`/
    /// `ForkSpec` that created `child`: those are always built with an
    /// EMPTY prompt/directive (`execute`'s own `Spawn`/`Fork` arms), so the
    /// child starts genuinely idle (`conway_runtime::subagent`'s own doc on
    /// `keep_alive` + an empty prompt) and `first_message` becomes the
    /// child's own first `UserTurn`, indistinguishable from any later
    /// message the user types once focused on it.
    FocusNewSession {
        child: AgentId,
        /// The agent `child` was spawned/forked under (root for `/spawn`, the
        /// focused agent for `/fork`). The app loop seeds `child`'s `/agents`
        /// tree node under this parent immediately (`AppState::
        /// ensure_agent_tracked`), rather than waiting for `child`'s
        /// `AgentSpawned` event -- which never arrives on the stream the app
        /// switches to. The app swaps its event subscription to
        /// `agent_events(child)` the SAME turn, and that stream's replay is
        /// `child`'s own records only (never its own spawn lifecycle event),
        /// while the live half subscribed only AFTER the spawn already fired.
        /// Without this seed the freshly created session is missing from the
        /// panel until some LATER tree event happens to redraw it.
        parent: AgentId,
        first_message: Option<String>,
    },
}

/// The facade surface commands dispatch through -- abstracted behind a
/// trait so dispatch logic is unit-testable against a fake, with no live
/// `Runtime` (module notes: "headless, fake `SessionHandle` seam").
#[async_trait::async_trait]
pub trait Host {
    fn root(&self) -> AgentId;
    fn tree(&self) -> AgentTreeSnapshot;
    async fn context_report(&self, agent: AgentId) -> conway::Result<ContextReport>;
    /// The focused agent's cumulative token spend (board item
    /// 01KYAGP11FF9YC3G60TWHHKKST): a thin passthrough to
    /// `SessionHandle::session_usage`, reached through this trait -- like
    /// every other method here -- so `app.rs`'s status-line refresh logic
    /// stays unit-testable against a fake, with no live `Runtime`.
    async fn session_usage(&self, agent: AgentId) -> conway::Result<Usage>;
    async fn fork(&self, from: AgentId, spec: ForkSpec) -> conway::Result<AgentId>;
    async fn spawn(&self, from: AgentId, spec: SpawnSpec) -> conway::Result<AgentId>;
    async fn steer(&self, target: AgentId, text: String) -> conway::Result<()>;
    async fn resume(&self, sid: SessionId) -> conway::Result<SessionHandle>;
}

/// The live [`Host`]: pure delegation to a `SessionHandle` + `Conway` pair
/// -- no logic of its own, per this item's own objective ("none of them may
/// reach past `SessionHandle`/`Conway`").
pub struct LiveHost<'a> {
    pub handle: &'a SessionHandle,
    pub conway: &'a Conway,
}

#[async_trait::async_trait]
impl Host for LiveHost<'_> {
    fn root(&self) -> AgentId {
        self.handle.root()
    }

    fn tree(&self) -> AgentTreeSnapshot {
        self.handle.tree()
    }

    async fn context_report(&self, agent: AgentId) -> conway::Result<ContextReport> {
        self.handle.context_report(agent).await
    }

    async fn session_usage(&self, agent: AgentId) -> conway::Result<Usage> {
        self.handle.session_usage(agent).await
    }

    async fn fork(&self, from: AgentId, spec: ForkSpec) -> conway::Result<AgentId> {
        self.handle.fork(from, spec).await
    }

    async fn spawn(&self, from: AgentId, spec: SpawnSpec) -> conway::Result<AgentId> {
        self.handle.spawn(from, spec).await
    }

    async fn steer(&self, target: AgentId, text: String) -> conway::Result<()> {
        self.handle.steer(target, text).await
    }

    async fn resume(&self, sid: SessionId) -> conway::Result<SessionHandle> {
        self.conway.resume(sid).await
    }
}

/// The tool profile a bare `/fork`/`/spawn`'s fresh, interactive keep-alive
/// child gets (decision 01KYB0BWY27DWB69NCNK85D56J): the same "pure and
/// light" exclusion `App::new` gives the TUI root -- excludes `report`, since
/// an interactive keep-alive child (like the root) has no parent to report an
/// `AgentResult` to, and would otherwise hit the permission gate on a tool
/// call nothing downstream ever unblocks. `conway_subagent` and every other
/// builtin tool stay available. Deliberately NOT applied to the
/// explicit-target `/fork @<agent> <directive>` arm above -- that fork stays
/// autonomous (non-keep-alive) and keeps the default toolset, `report`
/// included, exactly as an autonomous `conway_subagent`-spawned child does.
fn interactive_keep_alive_tools() -> ToolSelector {
    ToolSelector::Except(vec!["report".into()])
}

/// Executes one parsed command against `host`, mutating `state` directly
/// (transcript entries, and -- for `/resume` -- a full state reset) and
/// returning whatever [`Effect`] the caller's app loop must additionally
/// carry out. Every command maps to exactly one `host` call except `/why`
/// (reads `state.last_model_decision`, no facade call at all) and `/help`/
/// `/tree`'s rendering, which read `host.tree()`/constants only.
///
/// Never panics and never propagates a facade error: a failing command
/// becomes a `Notice` entry with the error's `Display` (module notes: "A
/// failing slash command must never terminate the TUI").
pub async fn execute<H: Host>(cmd: SlashCommand, state: &mut AppState, host: &H) -> Effect {
    match cmd {
        SlashCommand::Steer { target, text } => {
            match resolve_agent(state, &target) {
                Ok(agent) => match host.steer(agent, text).await {
                    Ok(()) => notice(state, format!("steer queued for {agent}")),
                    Err(e) => notice(state, e.to_string()),
                },
                Err(e) => notice(state, e),
            }
            Effect::None
        }
        SlashCommand::Tree => {
            let snapshot = host.tree();
            render_tree_snapshot(&snapshot, state);
            Effect::None
        }
        SlashCommand::Context { agent } => {
            match resolve_agent(state, &agent) {
                Ok(agent_id) => match host.context_report(agent_id).await {
                    Ok(report) => render_context_report(&report, state),
                    Err(e) => notice(state, e.to_string()),
                },
                Err(e) => notice(state, e),
            }
            Effect::None
        }
        SlashCommand::Why => {
            render_why(state);
            Effect::None
        }
        SlashCommand::Fork { agent, directive } => match agent {
            // Explicit target (`/fork @<agent> <directive>`): the
            // pre-existing autonomous, non-keep-alive fork-of-a-named-agent
            // behavior, unchanged in substance -- `parse_fork` guarantees
            // `directive` is `Some` whenever `agent` is.
            Some(token) => {
                let directive_text = directive.unwrap_or_default();
                match resolve_agent(state, &token) {
                    Ok(agent_id) => {
                        match host.fork(agent_id, ForkSpec::new(directive_text)).await {
                            Ok(child) => notice(state, format!("forked {agent_id} -> {child}")),
                            Err(e) => notice(state, e.to_string()),
                        }
                    }
                    Err(e) => notice(state, e),
                }
                Effect::None
            }
            // Bare/implicit (`/fork`, `/fork <text>`): a fresh, interactive
            // keep-alive fork of the FOCUSED agent -- empty directive (the
            // child inherits context at head and idles); `directive`, if
            // given, becomes its first message via `Effect::
            // FocusNewSession`, not baked into the `ForkSpec` itself (see
            // that variant's own doc).
            None => {
                let focused = state.focused_agent;
                match host
                    .fork(
                        focused,
                        ForkSpec::new("")
                            .keep_alive(true)
                            .tools(interactive_keep_alive_tools()),
                    )
                    .await
                {
                    Ok(child) => Effect::FocusNewSession {
                        child,
                        parent: focused,
                        first_message: directive,
                    },
                    Err(e) => {
                        notice(state, e.to_string());
                        Effect::None
                    }
                }
            }
        },
        SlashCommand::Spawn { agent_def, prompt } => {
            // Always a fresh, interactive keep-alive session (this item):
            // empty prompt (the child idles until `prompt`, if given, is
            // delivered separately by the app loop -- see `Effect::
            // FocusNewSession`'s own doc), attached under `host.root()`
            // exactly as every spawn always has been (spawn never named a
            // "from" agent).
            let root = host.root();
            let mut spec = SpawnSpec::new("")
                .keep_alive(true)
                .tools(interactive_keep_alive_tools());
            if let Some(def) = &agent_def {
                spec = spec.agent_def(def.clone());
            }
            match host.spawn(root, spec).await {
                Ok(child) => Effect::FocusNewSession {
                    child,
                    parent: root,
                    first_message: prompt,
                },
                Err(e) => {
                    notice(state, e.to_string());
                    Effect::None
                }
            }
        }
        SlashCommand::Resume { sid } => match sid.parse::<SessionId>() {
            Ok(id) => match host.resume(id).await {
                Ok(handle) => {
                    // Module notes: "replace the active handle, resubscribe
                    // events, reset AppState from handle.transcript(root)".
                    // The full LogRecord -> Entry backfill is left out here
                    // (disclosed): no LogRecord -> Entry mapping exists
                    // anywhere in this crate today, and no criterion of
                    // this item exercises it -- `conway::SessionHandle`'s
                    // own `record_to_event` doc names the analogous
                    // LogRecord -> Event gap as unresolved for the same
                    // reason (mismatched cardinality). `state` is reset to
                    // a clean `AppState` scoped to the new root instead, so
                    // resumed browsing starts from a known-empty transcript
                    // rather than a stale one from the old session.
                    *state = AppState::new(handle.root());
                    notice(state, format!("resumed session {sid}"));
                    Effect::Resumed(handle)
                }
                Err(e) => {
                    notice(state, e.to_string());
                    Effect::None
                }
            },
            Err(e) => {
                notice(state, format!("invalid session id `{sid}`: {e}"));
                Effect::None
            }
        },
        SlashCommand::Help => {
            render_help(state);
            Effect::None
        }
        SlashCommand::Quit => Effect::Quit,
    }
}

fn notice(state: &mut AppState, text: impl Into<String>) {
    state.transcript.push(Entry::Notice { text: text.into() });
}

/// Resolves `token` to a live agent id: a full ULID is accepted outright
/// (no membership check -- the facade call itself rejects an agent outside
/// this session), otherwise `token` is matched as a unique prefix against
/// `state.tree`'s known agent ids (module notes: "an ambiguous prefix is a
/// `ParseError` listing the candidates").
fn resolve_agent(state: &AppState, token: &str) -> Result<AgentId, String> {
    if let Ok(id) = token.parse::<AgentId>() {
        return Ok(id);
    }
    let matches: Vec<AgentId> = state
        .tree
        .nodes
        .iter()
        .map(|n| n.agent_id)
        .filter(|id| id.to_string().starts_with(token))
        .collect();
    match matches.as_slice() {
        [] => Err(format!("no agent matches `{token}`")),
        [id] => Ok(*id),
        _ => Err(format!(
            "ambiguous agent prefix `{token}`; candidates: {}",
            matches
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Renders `snapshot` into the transcript as one `Notice` line per agent,
/// indented by ancestor depth -- mirrors `view.rs`'s own left-pane
/// `ancestor_depth` approach (insertion order plus a computed depth, not a
/// recursive tree walk) for the same reason: `AgentTreeSnapshot.nodes` is a
/// flat `Vec` with `parent` links, not a nested tree.
///
/// The depth walk is a closure (not a named helper fn) so its captured
/// `by_id` map never needs `conway_core::agent::AgentNode` spelled out in a
/// signature -- that type is not part of this crate's curated `conway`
/// re-export list, and widening it is out of this item's file scope.
fn render_tree_snapshot(snapshot: &AgentTreeSnapshot, state: &mut AppState) {
    let by_id: HashMap<_, _> = snapshot.nodes.iter().map(|n| (n.agent_id, n)).collect();
    let depth_of = |agent: AgentId| -> usize {
        let mut depth = 0;
        let mut cursor = agent;
        while let Some(node) = by_id.get(&cursor) {
            match node.parent {
                Some(p) => {
                    depth += 1;
                    cursor = p;
                }
                None => break,
            }
        }
        depth
    };
    let mut lines = Vec::with_capacity(snapshot.nodes.len());
    for node in &snapshot.nodes {
        let depth = depth_of(node.agent_id);
        let indent = "  ".repeat(depth);
        let label = node
            .agent_def
            .clone()
            .unwrap_or_else(|| "agent".to_string());
        lines.push(format!(
            "{indent}{} {label} [{:?}]",
            node.agent_id, node.status
        ));
    }
    for line in lines {
        notice(state, line);
    }
}

fn render_context_report(report: &ContextReport, state: &mut AppState) {
    if report.segments.is_empty() {
        notice(state, "empty context");
        return;
    }
    for entry in &report.segments {
        notice(
            state,
            format!(
                "{} {} {}tok",
                entry.segment,
                provenance_label(&entry.provenance),
                entry.tokens_est
            ),
        );
    }
}

fn provenance_label(p: &Provenance) -> String {
    match p {
        Provenance::UserPrompt => "user prompt".to_string(),
        Provenance::AgentDef { name } => format!("agent def `{name}`"),
        Provenance::Skill { name } => format!("skill `{name}`"),
        Provenance::ToolRegistry { hash } => format!("tool registry {hash}"),
        Provenance::Inherited { from, seq_range } => {
            format!("inherited from {from} ({seq_range:?})")
        }
        Provenance::ForkDirective { by } => format!("fork directive by {by}"),
        Provenance::ParentSteer { from, parent_seq } => {
            format!("parent steer from {from} @{parent_seq:?}")
        }
        Provenance::ToolResult { call_id, tool } => format!("tool result {tool} ({call_id})"),
        Provenance::SystemNote { reason } => format!("system note: {reason}"),
        _ => "unknown provenance".to_string(),
    }
}

/// `/why`: renders `state.last_model_decision` (populated by `app.rs` on
/// `Event::ModelDecision` -- this module never writes it). No facade call
/// at all (module notes: "reads cached state with no facade call").
fn render_why(state: &mut AppState) {
    let Some(env) = state.last_model_decision.clone() else {
        notice(state, "no routing decision yet");
        return;
    };
    let Event::ModelDecision {
        role,
        chosen,
        reason,
        attempt,
    } = env.event
    else {
        // `last_model_decision` is only ever assigned an `Event::ModelDecision`
        // envelope (app.rs's own invariant) -- this arm exists so a future
        // widening of that invariant degrades to the same "nothing to show
        // yet" message rather than panicking.
        notice(state, "no routing decision yet");
        return;
    };
    notice(state, format!("role: {role}"));
    notice(state, format!("model: {chosen}"));
    notice(state, format!("reason: {}", render_routing_reason(&reason)));
    notice(state, format!("attempt: {attempt}"));
}

fn render_routing_reason(reason: &RoutingReason) -> String {
    match reason {
        RoutingReason::PinnedByApi => "pinned by API".to_string(),
        RoutingReason::PinnedByAgentDef => "pinned by agent definition".to_string(),
        RoutingReason::AliasPrimary { alias } => format!("primary for role `{alias}`"),
        RoutingReason::Fallback { position, after } => format!(
            "fallback #{position} after: {}",
            after
                .iter()
                .map(|f| format!("{} ({})", f.model, f.error))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        RoutingReason::CapabilitySkip { skipped, missing } => {
            format!("skipped `{skipped}`: missing {}", missing.join(", "))
        }
        RoutingReason::HealthSkip { skipped, breaker } => {
            format!("skipped `{skipped}`: {breaker:?} breaker open")
        }
        _ => "unknown routing reason".to_string(),
    }
}

const HELP_LINES: &[&str] = &[
    "/steer <agent> <text>       -- send a steer message to a running agent",
    "/tree                       -- show the whole agent tree",
    "/context <agent>            -- show an agent's assembled context",
    "/why                        -- show the last routing decision",
    "/fork [<text>]              -- open an interactive fork of the focused agent (optional first message)",
    "/fork @<agent> <directive>  -- fork a specific live agent with a directive",
    "/spawn [@<agent_def>] [<prompt>] -- open an interactive spawned agent (inherits role/model if no @agent_def)",
    "/resume <session-id>        -- resume a prior session",
    "/help                       -- show this help",
    "/quit                       -- exit",
    "/exit                       -- alias for /quit",
];

fn render_help(state: &mut AppState) {
    for line in HELP_LINES {
        notice(state, *line);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use conway::{AgentId, ConwayError, SessionId, SubagentMode};
    // Test-only: `ContextReportEntry`/`SegmentId` are not part of this
    // crate's curated `conway` re-export list -- constructing
    // `ContextReport` fixtures reaches into `conway-core` directly, exactly
    // as `exit.rs`/`oneshot.rs`/`render/*.rs`'s existing tests already do
    // (see this crate's `Cargo.toml` `[dev-dependencies]` comment).
    use conway_core::ids::SegmentId;
    use conway_core::provenance::ContextReportEntry;

    use super::*;

    /// Wide enough that a rendered status line's `focused: <ulid>` suffix
    /// (a 26-char ULID, after every other status segment) is never itself
    /// clipped by the terminal width -- see the render/state tests near the
    /// bottom of this module.
    const RENDER_WIDTH: u16 = 200;

    // ---------------------------------------------------------------
    // parse()
    // ---------------------------------------------------------------

    #[test]
    fn steer_parses_and_preserves_internal_whitespace() {
        assert_eq!(
            parse("/steer a7 hold on"),
            Ok(SlashCommand::Steer {
                target: "a7".to_string(),
                text: "hold on".to_string(),
            })
        );
    }

    #[test]
    fn steer_missing_text_is_a_parse_error_naming_the_form() {
        let err = parse("/steer a7").unwrap_err();
        assert!(err.to_string().contains("/steer <agent> <text>"));
    }

    #[test]
    fn tree_parses() {
        assert_eq!(parse("/tree"), Ok(SlashCommand::Tree));
    }

    #[test]
    fn tree_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/tree foo").unwrap_err();
        assert!(err.to_string().contains("/tree"));
    }

    #[test]
    fn context_parses() {
        assert_eq!(
            parse("/context a7"),
            Ok(SlashCommand::Context {
                agent: "a7".to_string(),
            })
        );
    }

    #[test]
    fn context_missing_agent_is_a_parse_error_naming_the_form() {
        let err = parse("/context").unwrap_err();
        assert!(err.to_string().contains("/context <agent>"));
    }

    #[test]
    fn why_parses() {
        assert_eq!(parse("/why"), Ok(SlashCommand::Why));
    }

    #[test]
    fn why_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/why now").unwrap_err();
        assert!(err.to_string().contains("/why"));
    }

    #[test]
    fn fork_at_agent_splits_agent_and_directive() {
        // Explicit target via `@` (this item's generalization of the old
        // `/fork <agent> <directive>` two-arg form).
        assert_eq!(
            parse("/fork @a7 review the diff"),
            Ok(SlashCommand::Fork {
                agent: Some("a7".to_string()),
                directive: Some("review the diff".to_string()),
            })
        );
    }

    #[test]
    fn fork_at_agent_missing_directive_is_a_parse_error_naming_the_form() {
        let err = parse("/fork @a7").unwrap_err();
        assert!(err.to_string().contains("/fork"));
    }

    #[test]
    fn bare_fork_parses_with_no_agent_and_no_directive() {
        // Bare `/fork` (this item): a fresh, interactive keep-alive fork of
        // the FOCUSED agent, idling until prompted.
        assert_eq!(
            parse("/fork"),
            Ok(SlashCommand::Fork {
                agent: None,
                directive: None,
            })
        );
    }

    #[test]
    fn fork_with_text_and_no_at_sigil_is_a_bare_fork_with_a_first_message() {
        // No `@` sigil -- the entire remainder (however many words) is the
        // interactive child's first message, not an explicit target.
        assert_eq!(
            parse("/fork please review this"),
            Ok(SlashCommand::Fork {
                agent: None,
                directive: Some("please review this".to_string()),
            })
        );
    }

    #[test]
    fn spawn_with_no_agent_def_treats_the_whole_remainder_as_the_prompt() {
        // No `@<agent_def>` token -- the entire remainder is the prompt and
        // `agent_def` is `None` (the spawned child inherits the parent's
        // role/model).
        assert_eq!(
            parse("/spawn reviewer review the diff"),
            Ok(SlashCommand::Spawn {
                agent_def: None,
                prompt: Some("reviewer review the diff".to_string()),
            })
        );
    }

    #[test]
    fn spawn_with_at_agent_def_splits_agent_def_and_prompt() {
        assert_eq!(
            parse("/spawn @reviewer review the diff"),
            Ok(SlashCommand::Spawn {
                agent_def: Some("reviewer".to_string()),
                prompt: Some("review the diff".to_string()),
            })
        );
    }

    #[test]
    fn bare_spawn_parses_with_no_agent_def_and_no_prompt() {
        // Bare `/spawn` (this item): a fresh, interactive keep-alive spawn,
        // idling until prompted -- no longer a parse error.
        assert_eq!(
            parse("/spawn"),
            Ok(SlashCommand::Spawn {
                agent_def: None,
                prompt: None,
            })
        );
    }

    #[test]
    fn spawn_at_agent_def_with_no_prompt_parses_with_prompt_none() {
        // `/spawn @<agent_def>` (this item): names an agent_def with no
        // first message -- no longer a parse error.
        assert_eq!(
            parse("/spawn @reviewer"),
            Ok(SlashCommand::Spawn {
                agent_def: Some("reviewer".to_string()),
                prompt: None,
            })
        );
    }

    #[test]
    fn spawn_double_at_escapes_a_literal_at_prompt_with_no_agent_def() {
        // `@@` is the escape hatch for a prompt that must begin with `@`;
        // without it, `@channel ...` would be mis-split into an agent_def.
        assert_eq!(
            parse("/spawn @@channel please refactor the parser"),
            Ok(SlashCommand::Spawn {
                agent_def: None,
                prompt: Some("@channel please refactor the parser".to_string()),
            })
        );
    }

    #[test]
    fn resume_parses() {
        assert_eq!(
            parse("/resume 01ARZ3NDEKTSV4RRFFQ69G5FAV"),
            Ok(SlashCommand::Resume {
                sid: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            })
        );
    }

    #[test]
    fn resume_missing_sid_is_a_parse_error_naming_the_form() {
        let err = parse("/resume").unwrap_err();
        assert!(err.to_string().contains("/resume <session-id>"));
    }

    #[test]
    fn help_parses() {
        assert_eq!(parse("/help"), Ok(SlashCommand::Help));
    }

    #[test]
    fn help_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/help me").unwrap_err();
        assert!(err.to_string().contains("/help"));
    }

    #[test]
    fn quit_parses() {
        assert_eq!(parse("/quit"), Ok(SlashCommand::Quit));
    }

    #[test]
    fn quit_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/quit now").unwrap_err();
        assert!(err.to_string().contains("/quit"));
    }

    #[test]
    fn exit_parses_as_an_alias_for_quit() {
        assert_eq!(parse("/exit"), Ok(SlashCommand::Quit));
    }

    #[test]
    fn exit_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/exit now").unwrap_err();
        assert!(err.to_string().contains("/exit"));
    }

    #[test]
    fn bareword_exit_and_quit_do_not_parse_as_slash_commands() {
        // No leading `/` -- these must stay normal prompts sent to the
        // model, never intercepted as a slash command.
        assert!(parse("exit").is_err());
        assert!(parse("quit").is_err());
    }

    #[test]
    fn unknown_command_is_a_parse_error() {
        let err = parse("/nope").unwrap_err();
        assert!(err.to_string().contains("/nope"));
    }

    // ---------------------------------------------------------------
    // execute() -- dispatch, via a fake Host
    // ---------------------------------------------------------------

    struct FakeHost {
        calls: Mutex<Vec<&'static str>>,
        root: AgentId,
        tree: AgentTreeSnapshot,
        context: Option<ContextReport>,
        /// When `Some`, `fork`/`spawn` succeed with this child id instead of
        /// the default `fake_error()` -- lets a test exercise the
        /// `Effect::FocusNewSession` success path.
        fork_child: Option<AgentId>,
        spawn_child: Option<AgentId>,
        /// The most recent `ForkSpec`/`SpawnSpec` `execute` actually passed
        /// -- lets a test assert the bare/implicit paths build a
        /// `keep_alive(true)`, empty-prompt spec (module notes: never baked
        /// into the spec itself, see `Effect::FocusNewSession`'s own doc).
        last_fork_spec: Mutex<Option<ForkSpec>>,
        last_spawn_spec: Mutex<Option<SpawnSpec>>,
    }

    impl FakeHost {
        fn new(root: AgentId) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                root,
                tree: AgentTreeSnapshot {
                    root,
                    nodes: Vec::new(),
                    at: chrono::Utc::now(),
                },
                context: None,
                fork_child: None,
                spawn_child: None,
                last_fork_spec: Mutex::new(None),
                last_spawn_spec: Mutex::new(None),
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn fake_error() -> ConwayError {
        ConwayError::Config {
            path: None,
            message: "fake error".to_string(),
        }
    }

    #[async_trait::async_trait]
    impl Host for FakeHost {
        fn root(&self) -> AgentId {
            self.root
        }

        fn tree(&self) -> AgentTreeSnapshot {
            self.calls.lock().unwrap().push("tree");
            self.tree.clone()
        }

        async fn context_report(&self, _agent: AgentId) -> conway::Result<ContextReport> {
            self.calls.lock().unwrap().push("context_report");
            self.context.clone().ok_or_else(fake_error)
        }

        async fn session_usage(&self, _agent: AgentId) -> conway::Result<Usage> {
            self.calls.lock().unwrap().push("session_usage");
            Err(fake_error())
        }

        async fn fork(&self, _from: AgentId, spec: ForkSpec) -> conway::Result<AgentId> {
            self.calls.lock().unwrap().push("fork");
            *self.last_fork_spec.lock().unwrap() = Some(spec);
            self.fork_child.ok_or_else(fake_error)
        }

        async fn spawn(&self, _from: AgentId, spec: SpawnSpec) -> conway::Result<AgentId> {
            self.calls.lock().unwrap().push("spawn");
            *self.last_spawn_spec.lock().unwrap() = Some(spec);
            if let Some(child) = self.spawn_child {
                return Ok(child);
            }
            Err(fake_error())
        }

        async fn steer(&self, _target: AgentId, _text: String) -> conway::Result<()> {
            self.calls.lock().unwrap().push("steer");
            Err(fake_error())
        }

        async fn resume(&self, _sid: SessionId) -> conway::Result<SessionHandle> {
            self.calls.lock().unwrap().push("resume");
            // A live `SessionHandle` has no public constructor reachable
            // from this crate (`conway::SessionHandle::new` is
            // `pub(crate)` to the facade) -- this fake can only ever
            // exercise the call-count and error-propagation half of the
            // `/resume` criterion from outside `conway`, disclosed here
            // rather than silently skipped.
            Err(fake_error())
        }
    }

    #[tokio::test]
    async fn steer_maps_to_exactly_one_steer_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let effect = execute(
            SlashCommand::Steer {
                target: root.to_string(),
                text: "hold on".to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(matches!(effect, Effect::None));
        assert_eq!(host.calls(), vec!["steer"]);
    }

    #[tokio::test]
    async fn tree_maps_to_exactly_one_tree_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(SlashCommand::Tree, &mut state, &host).await;

        assert_eq!(host.calls(), vec!["tree"]);
    }

    #[tokio::test]
    async fn context_maps_to_exactly_one_context_report_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(
            SlashCommand::Context {
                agent: root.to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["context_report"]);
    }

    #[tokio::test]
    async fn fork_at_agent_maps_to_exactly_one_fork_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(
            SlashCommand::Fork {
                agent: Some(root.to_string()),
                directive: Some("review the diff".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["fork"]);
        // The explicit-target `/fork @<agent> <directive>` arm is the
        // pre-existing AUTONOMOUS (non-keep-alive) fork -- unlike the bare
        // fork/spawn arms, it must keep the default toolset (`report`
        // included), exactly like a `conway_subagent`-spawned child does.
        let spec = host
            .last_fork_spec
            .lock()
            .unwrap()
            .clone()
            .expect("fork should have been called");
        assert_eq!(
            spec.tools, None,
            "an explicit-target autonomous fork must keep the default toolset"
        );
    }

    #[tokio::test]
    async fn spawn_maps_to_exactly_one_spawn_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(
            SlashCommand::Spawn {
                agent_def: Some("reviewer".to_string()),
                prompt: Some("review the diff".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["spawn"]);
    }

    #[tokio::test]
    async fn spawn_without_agent_def_still_maps_to_exactly_one_spawn_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: Some("review the diff".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["spawn"]);
    }

    #[tokio::test]
    async fn bare_spawn_builds_a_keep_alive_empty_prompt_spec_and_returns_focus_new_session() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.spawn_child = Some(child);

        let effect = execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: None,
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["spawn"]);
        match effect {
            Effect::FocusNewSession {
                child: focused,
                parent,
                first_message,
            } => {
                assert_eq!(focused, child);
                assert_eq!(parent, root, "a bare spawn attaches the child under root");
                assert_eq!(first_message, None);
            }
            _ => panic!("expected Effect::FocusNewSession, got a different effect"),
        }
        let spec = host
            .last_spawn_spec
            .lock()
            .unwrap()
            .clone()
            .expect("spawn should have been called");
        assert!(spec.keep_alive, "a bare spawn must be keep_alive");
        assert_eq!(spec.prompt, "", "the SpawnSpec's own prompt must be empty");
        assert_eq!(
            spec.tools,
            Some(ToolSelector::Except(vec!["report".into()])),
            "a bare, interactive keep-alive spawn must exclude `report`"
        );
    }

    #[tokio::test]
    async fn spawn_with_text_carries_the_text_as_the_effects_first_message_not_the_spec_prompt() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.spawn_child = Some(child);

        let effect = execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: Some("hello there".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        match effect {
            Effect::FocusNewSession {
                child: focused,
                parent,
                first_message,
            } => {
                assert_eq!(focused, child);
                assert_eq!(parent, root, "a bare spawn attaches the child under root");
                assert_eq!(first_message, Some("hello there".to_string()));
            }
            _ => panic!("expected Effect::FocusNewSession, got a different effect"),
        }
        let spec = host
            .last_spawn_spec
            .lock()
            .unwrap()
            .clone()
            .expect("spawn should have been called");
        assert_eq!(
            spec.prompt, "",
            "the first message must not be baked into the SpawnSpec"
        );
    }

    #[tokio::test]
    async fn bare_fork_builds_a_keep_alive_empty_directive_spec_targeting_the_focused_agent() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child_focus = AgentId::new();
        state.focus_agent(child_focus);
        let mut host = FakeHost::new(root);
        let grandchild = AgentId::new();
        host.fork_child = Some(grandchild);

        let effect = execute(
            SlashCommand::Fork {
                agent: None,
                directive: None,
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["fork"]);
        match effect {
            Effect::FocusNewSession {
                child,
                parent,
                first_message,
            } => {
                assert_eq!(child, grandchild);
                assert_eq!(
                    parent, child_focus,
                    "a bare fork attaches the child under the focused agent"
                );
                assert_eq!(first_message, None);
            }
            _ => panic!("expected Effect::FocusNewSession, got a different effect"),
        }
        let spec = host
            .last_fork_spec
            .lock()
            .unwrap()
            .clone()
            .expect("fork should have been called");
        assert!(spec.keep_alive, "a bare fork must be keep_alive");
        assert_eq!(
            spec.directive, "",
            "the ForkSpec's own directive must be empty"
        );
        assert_eq!(
            spec.tools,
            Some(ToolSelector::Except(vec!["report".into()])),
            "a bare, interactive keep-alive fork must exclude `report`"
        );
    }

    #[tokio::test]
    async fn fork_with_text_carries_the_text_as_the_effects_first_message() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.fork_child = Some(child);

        let effect = execute(
            SlashCommand::Fork {
                agent: None,
                directive: Some("please review".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        match effect {
            Effect::FocusNewSession {
                child: focused,
                parent,
                first_message,
            } => {
                assert_eq!(focused, child);
                assert_eq!(
                    parent, root,
                    "the focused agent (root here) is the fork parent"
                );
                assert_eq!(first_message, Some("please review".to_string()));
            }
            _ => panic!("expected Effect::FocusNewSession, got a different effect"),
        }
    }

    #[tokio::test]
    async fn resume_maps_to_exactly_one_resume_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let effect = execute(
            SlashCommand::Resume {
                sid: SessionId::new().to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(matches!(effect, Effect::None), "the fake resume errors");
        assert_eq!(host.calls(), vec!["resume"]);
    }

    #[tokio::test]
    async fn why_makes_no_facade_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(SlashCommand::Why, &mut state, &host).await;

        assert!(host.calls().is_empty(), "expected no facade call for /why");
    }

    #[tokio::test]
    async fn why_before_any_model_decision_renders_no_routing_decision_yet() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(SlashCommand::Why, &mut state, &host).await;

        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text == "no routing decision yet"
        ));
    }

    #[tokio::test]
    async fn why_after_a_model_decision_renders_its_fields() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let chosen = conway::ModelRef {
            backend: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
        };
        state.last_model_decision = Some(conway::Envelope {
            seq: 1,
            ts: chrono::Utc::now(),
            session: SessionId::new(),
            agent: root,
            event: Event::ModelDecision {
                role: "planner".into(),
                chosen: chosen.clone(),
                reason: RoutingReason::PinnedByApi,
                attempt: 1,
            },
        });

        execute(SlashCommand::Why, &mut state, &host).await;

        let texts: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Notice { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t.contains("planner")));
        assert!(texts.iter().any(|t| t.contains(&chosen.to_string())));
        assert!(texts.iter().any(|t| t.contains("pinned by API")));
        assert!(texts.iter().any(|t| t.contains('1')));
    }

    #[tokio::test]
    async fn unknown_slash_command_never_reaches_the_model() {
        // The app-level guarantee this criterion is about lives in
        // `app.rs::submit` (parse fails before `execute` is ever called) --
        // this test locks down the piece owned here: `parse` rejects an
        // unknown command rather than silently accepting it as a `SlashCommand`
        // some `execute` arm would forward as a prompt.
        assert!(parse("/nope").is_err());
    }

    #[tokio::test]
    async fn context_renders_one_line_per_segment() {
        let root = AgentId::new();
        let seg0 = SegmentId::new();
        let seg1 = SegmentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.context = Some(ContextReport {
            agent_id: root,
            turn: 1,
            tokenizer: "heuristic-chars4".to_string(),
            segments: vec![
                ContextReportEntry {
                    segment: seg0,
                    provenance: Provenance::UserPrompt,
                    tokens_est: 12,
                    estimated: true,
                },
                ContextReportEntry {
                    segment: seg1,
                    provenance: Provenance::AgentDef {
                        name: "reviewer".to_string(),
                    },
                    tokens_est: 40,
                    estimated: true,
                },
            ],
            total_tokens_est: 52,
        });

        execute(
            SlashCommand::Context {
                agent: root.to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        let lines: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Notice { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 2, "expected one line per segment");
        // Each line must carry the segment id, a provenance label, and the
        // token estimate -- not just be present (cycle-1 review M1).
        assert!(
            lines[0].contains(&seg0.to_string())
                && lines[0].contains("user prompt")
                && lines[0].contains("12tok"),
            "line 0 missing id/provenance/tokens: {:?}",
            lines[0]
        );
        assert!(
            lines[1].contains(&seg1.to_string())
                && lines[1].contains("agent def `reviewer`")
                && lines[1].contains("40tok"),
            "line 1 missing id/provenance/tokens: {:?}",
            lines[1]
        );
    }

    #[tokio::test]
    async fn context_with_zero_segments_renders_an_explicit_empty_line() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.context = Some(ContextReport {
            agent_id: root,
            turn: 1,
            tokenizer: "heuristic-chars4".to_string(),
            segments: Vec::new(),
            total_tokens_est: 0,
        });

        execute(
            SlashCommand::Context {
                agent: root.to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text == "empty context"
        ));
    }

    #[tokio::test]
    async fn ambiguous_agent_prefix_is_reported_and_does_not_call_the_facade() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let session = SessionId::new();

        // `b` is `a`'s own string with only its last character swapped for a
        // different valid Crockford char -- guarantees a 25-char shared
        // prefix between two distinct, valid agent ids deterministically
        // (no dependence on `AgentId::new()`'s timing-derived value).
        let a = AgentId::new();
        let a_str = a.to_string();
        let last = a_str.chars().next_back().expect("ULID string is non-empty");
        let alt = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
            .chars()
            .find(|&c| c != last)
            .expect("Crockford alphabet has more than one symbol");
        let mut b_str = a_str.clone();
        b_str.pop();
        b_str.push(alt);
        let b: AgentId = b_str
            .parse()
            .expect("swapping the last char keeps it a valid ULID");
        let shared_prefix = &a_str[..a_str.len() - 1];

        // Populate `state.tree` the same way `state.rs`'s own tests do
        // (`AppState::apply`) -- this module has no access to
        // `AgentTreeView`'s private `insert` and is out of scope to widen
        // `state.rs` for one.
        for child in [a, b] {
            state.apply(&conway::Envelope {
                seq: 1,
                ts: chrono::Utc::now(),
                session,
                agent: child,
                event: Event::AgentSpawned {
                    kind: SubagentMode::Spawn,
                    parent: Some(root),
                    agent_def: None,
                    inherited_upto: None,
                },
            });
        }
        let host = FakeHost::new(root);

        execute(
            SlashCommand::Steer {
                target: shared_prefix.to_string(),
                text: "hi".to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(
            host.calls().is_empty(),
            "ambiguous prefix must not reach the facade"
        );
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text.contains("ambiguous")
        ));
    }

    // ---------------------------------------------------------------
    // Render/state: after the `Effect::FocusNewSession` an app loop would
    // handle by focusing `child` (`app.rs::try_focus_agent`, thin over
    // `AppState::focus_agent` -- reused, not duplicated, here since this
    // module has no live facade to drive the REAL `agent_events` resubscribe
    // that `try_focus_agent` also performs), the focused agent really is the
    // new child, through the REAL render pass (`crate::tui::test_support`).
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn after_a_bare_spawns_focus_new_session_effect_the_focused_agent_is_the_new_child() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.spawn_child = Some(child);

        let effect = execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: None,
            },
            &mut state,
            &host,
        )
        .await;
        let Effect::FocusNewSession {
            child: focused_child,
            parent,
            ..
        } = effect
        else {
            panic!("expected Effect::FocusNewSession");
        };
        assert_eq!(focused_child, child);
        assert_eq!(parent, root, "a bare spawn's parent is root");

        // Mirrors `App::run`'s own handling of this effect: seed the tree
        // node, THEN focus + resubscribe. The seed is the fix under test --
        // without it the freshly spawned child is absent from the `/agents`
        // panel, since its own `AgentSpawned` reaches neither the child
        // stream's replay (own records only) nor its live half (subscribed
        // after the spawn already fired).
        assert_ne!(
            state.focused_agent, child,
            "must not already be focused on the not-yet-focused child"
        );
        assert!(
            !state.tree.nodes.iter().any(|n| n.agent_id == child),
            "precondition: the child is not in the tree until seeded"
        );
        state.ensure_agent_tracked(child, parent);
        assert!(
            state
                .tree
                .nodes
                .iter()
                .any(|n| n.agent_id == child && n.parent == Some(root)),
            "the /agents tree must list the newly spawned child under root: {:?}",
            state.tree.nodes
        );
        state.focus_agent(child);

        assert_eq!(state.focused_agent, child);
        // Through the REAL render pass, not a hand-rolled assertion on
        // `AppState` alone: the status line names the newly focused child
        // (mirrors `view::status`'s own `status_line_names_the_focused_
        // agent_once_switched_off_root` test). Rendered wide enough
        // (`RENDER_WIDTH`) that the status line's `focused: <ulid>` suffix
        // is not itself clipped by the terminal width -- a ULID is 26
        // chars, wider than `DEFAULT_SIZE`'s 80-column status line leaves
        // room for once every other status segment is in front of it.
        let rendered = crate::tui::test_support::render(&state, RENDER_WIDTH, 24);
        assert!(
            rendered.iter().any(|row| row.contains(&child.to_string())),
            "the rendered status line must name the newly focused child: {rendered:?}"
        );
    }

    #[tokio::test]
    async fn after_a_bare_forks_focus_new_session_effect_the_focused_agent_is_the_new_child() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.fork_child = Some(child);

        let effect = execute(
            SlashCommand::Fork {
                agent: None,
                directive: Some("go".to_string()),
            },
            &mut state,
            &host,
        )
        .await;
        let Effect::FocusNewSession {
            child: focused_child,
            parent,
            first_message,
        } = effect
        else {
            panic!("expected Effect::FocusNewSession");
        };
        assert_eq!(focused_child, child);
        assert_eq!(
            parent, root,
            "a bare fork's parent is the focused agent (root here)"
        );
        assert_eq!(first_message, Some("go".to_string()));

        // Same regression as the spawn case: seed the tree node (the fix)
        // before focusing, and confirm the child now appears in the panel.
        state.ensure_agent_tracked(child, parent);
        assert!(
            state.tree.nodes.iter().any(|n| n.agent_id == child),
            "the /agents tree must list the newly forked child: {:?}",
            state.tree.nodes
        );
        state.focus_agent(child);
        assert_eq!(state.focused_agent, child);
        let rendered = crate::tui::test_support::render(&state, RENDER_WIDTH, 24);
        assert!(
            rendered.iter().any(|row| row.contains(&child.to_string())),
            "the rendered status line must name the newly focused child: {rendered:?}"
        );
    }
}
