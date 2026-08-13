//! `conway-plugin-skeleton`: a worked example of the first-party plugin
//! tier (board item 01KZDC3JQ7W4DY1MG6MBCVB2DV) — see `PHILOSOPHY.md`'s
//! "First-party plugins, and why they are not defaults" and
//! `docs/embedding.md`'s "First-party plugin tier" section for what this
//! crate proves and what it deliberately does not.
//!
//! **What this crate proves.** That a plugin written and shipped in this
//! repository, but installed the same way a third party's would be, can
//! register a real tool and be called through all three consumption modes
//! (library, TUI, one-shot) — while staying completely invisible when
//! nobody asks for it. It is written entirely against `conway::plugin`,
//! the identical public surface a third-party plugin author gets -- a
//! first-party plugin gets no privileged API: if this crate ever needed to
//! reach past that surface, that would
//! be a defect in the plugin API, not a reason to give this crate a
//! private door. As of board item 01KZS03BFE720EQZG7Q2768N2H, that
//! includes declaring and firing a custom event
//! ([`PONG_DISPATCHED_EVENT`]): `SkeletonPlugin::events` declares it and
//! `SkeletonPingTool::invoke` fires it on every call, unconditionally --
//! the open-vocabulary half of `PHILOSOPHY.md` §5's hooks claim, proven
//! end to end against a real configured hook in
//! `tests/skeleton_end_to_end.rs` rather than asserted in prose.
//!
//! **What this crate is not.** Dynamic routing is built
//! (`conway-plugin-routing`); context compaction, memory, skills, and MCP
//! support are not, each separate, later work with no board item yet as of
//! 2026-08-13 (`scripts/board-claims.md`'s `UNFILED` entry records the gap)
//! — this crate performs no real work of its own and is not a template any
//! of them must literally follow, only a proof that the tier's install
//! mechanism holds together end to end.
//!
//! **`conway` (the facade) does not, and must never, depend on this
//! crate.** Doing so would put a first-party plugin back on the exact
//! footing the tier exists to avoid — a capability the core carries
//! whether or not anyone asked for it. The one place this crate IS linked
//! is `conway-cli` (`crates/conway-cli/src/first_party_plugins.rs`),
//! behind the `[plugins].install` config gate — never the library facade
//! itself. A library embedder who wants this plugin depends on this crate
//! directly and calls `ConwayBuilder::with_plugin`, exactly as a
//! third-party plugin's own consumer would.

use std::sync::Arc;

use conway::plugin::{
    async_trait, Command, CommandCtx, CommandOutcome, CommandSpec, ContentBlock, EventDecl,
    PathArgs, PermissionClass, Plugin, PluginManifest, RenderKind, Tool, ToolCall, ToolCategory,
    ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};

/// This plugin's manifest id: the string an operator names in
/// `[plugins].install` (`settings.json`) or a caller matches by hand
/// before calling `ConwayBuilder::with_plugin`.
pub const PLUGIN_ID: &str = "conway.plugin_skeleton";

/// The one tool this plugin provides.
pub const TOOL_NAME: &str = "skeleton_ping";

/// This plugin's own custom event (board item 01KZS03BFE720EQZG7Q2768N2H:
/// the open-vocabulary half of `PHILOSOPHY.md` §5's hooks claim, "A plugin
/// declares the events it emits"). BARE here -- reachable in an operator's
/// `[hooks].rules[].event` as `"{PLUGIN_ID}.{PONG_DISPATCHED_EVENT}"` once
/// `ConwayBuilder::build` namespaces it (`conway_runtime::hook_dispatch::
/// declared_plugin_events`), the identical division of labor
/// [`COMMAND_NAME`] already establishes for this plugin's command.
pub const PONG_DISPATCHED_EVENT: &str = "pong_dispatched";

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct PingArgs {
    /// Echoed back verbatim in the reply, so a caller can tell one
    /// invocation's result from another's. Optional: a bare call with no
    /// arguments is a valid ping.
    #[serde(default)]
    message: Option<String>,
}

/// `skeleton_ping`: the whole of this plugin's functionality. Replying with
/// a fixed sentence (plus the caller's optional `message`) proves nothing
/// more than that a call reached this plugin's own `Tool::invoke` through
/// the real runtime — the least a worked example needs to say to be
/// checkable end to end, and deliberately not more (this tier's own
/// members are separate, later work — see the module doc).
struct SkeletonPingTool;

#[async_trait]
impl Tool for SkeletonPingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(TOOL_NAME),
            description: "First-party plugin tier skeleton: replies with a fixed message. Not \
                          registered by default -- installed via `[plugins].install` or \
                          `ConwayBuilder::with_plugin`."
                .to_string(),
            schema: schemars::schema_for!(PingArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: PingArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;
        let text = match args.message {
            Some(message) => format!("skeleton pong: {message}"),
            None => "skeleton pong".to_string(),
        };
        // The worked example's event half (board item
        // 01KZS03BFE720EQZG7Q2768N2H): fires this plugin's OWN declared
        // event, through the SAME `ToolCtx` capability every third-party
        // plugin gets -- nothing privileged. `ctx.plugin_events` is bound
        // to this tool's own declaring plugin id (`PLUGIN_ID`), so this
        // call can only ever produce
        // `"{PLUGIN_ID}.{PONG_DISPATCHED_EVENT}"`, never another plugin's
        // namespace. Fires unconditionally, whether or not any operator
        // has wired a hook to it -- an event a plugin declares and never
        // fires is the same defect as a tool that does nothing
        // (`PHILOSOPHY.md` §5), so this proves the FIRING half, not only
        // the declaration.
        ctx.plugin_events
            .emit(PONG_DISPATCHED_EVENT, serde_json::json!({ "reply": text }))
            .await;
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text { text }],
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

/// The bare name [`SkeletonPingCommand`] registers under -- reachable in the
/// TUI as `/{PLUGIN_ID}.{COMMAND_NAME}` (`conway_cli::tui::commands::
/// CommandRegistry::build` prefixes it with this plugin's own manifest id;
/// see that function's own doc for why an author never picks their own
/// namespace).
pub const COMMAND_NAME: &str = "ping";

/// `/{PLUGIN_ID}.ping`: the worked example's command half (board item
/// 01KZYBFTK4QPB45AJT9M57P60W), proving a plugin can give the OPERATOR
/// something to type, not only the model something to call --
/// [`SkeletonPingTool`] above is this same worked example's tool half.
/// Deliberately the smallest useful thing: replies with a fixed sentence
/// plus whatever the operator typed after the command word, echoed back
/// verbatim -- proves a call reached this plugin's own `Command::invoke`
/// through the real TUI dispatch path (`commands::parse` ->
/// `commands::execute` -> `App::spawn_plugin_command`) and nothing more.
struct SkeletonPingCommand;

#[async_trait]
impl Command for SkeletonPingCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME.to_string(),
            summary: "First-party plugin tier skeleton: replies with a fixed message. Not \
                      registered by default -- installed via `[plugins].install` or \
                      `ConwayBuilder::with_plugin`."
                .to_string(),
        }
    }

    async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
        let text = if ctx.args.is_empty() {
            "skeleton pong".to_string()
        } else {
            format!("skeleton pong: {}", ctx.args)
        };
        CommandOutcome::Output(vec![text])
    }
}

/// The plugin itself. `Default` so a caller (this crate's own tests,
/// `conway-cli`'s first-party bundle) constructs it with no arguments,
/// matching every built-in's own zero-config construction.
#[derive(Default)]
pub struct SkeletonPlugin;

impl Plugin for SkeletonPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            // Versioned WITH the workspace (board item 01KZDC3JQ7W4DY1MG6MBCVB2DV) --
            // see this crate's own Cargo.toml doc comment.
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![ToolName::new(TOOL_NAME)],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(SkeletonPingTool)]
    }

    fn commands(&self) -> Vec<Arc<dyn Command>> {
        vec![Arc::new(SkeletonPingCommand)]
    }

    fn events(&self) -> Vec<EventDecl> {
        vec![EventDecl {
            name: PONG_DISPATCHED_EVENT.to_string(),
            summary: "fires once per skeleton_ping call, carrying the exact reply text \
                      the call produced; payload has no \"tool\" field."
                .to_string(),
            carries_tool_name: false,
        }]
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;

    fn ctx(args: &str) -> CommandCtx {
        CommandCtx {
            focused_agent: conway::AgentId::new(),
            root_agent: conway::AgentId::new(),
            args: args.to_string(),
        }
    }

    #[tokio::test]
    async fn ping_command_replies_with_a_fixed_message() {
        let command = SkeletonPingCommand;
        let outcome = command.invoke(ctx("")).await;
        assert_eq!(
            outcome,
            CommandOutcome::Output(vec!["skeleton pong".to_string()])
        );
    }

    #[tokio::test]
    async fn ping_command_echoes_its_argument() {
        let command = SkeletonPingCommand;
        let outcome = command.invoke(ctx("hello")).await;
        assert_eq!(
            outcome,
            CommandOutcome::Output(vec!["skeleton pong: hello".to_string()])
        );
    }

    #[test]
    fn plugin_declares_the_ping_command_under_its_bare_name() {
        let plugin = SkeletonPlugin;
        let commands = plugin.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].spec().name, COMMAND_NAME);
    }

    #[test]
    fn plugin_declares_the_pong_event_under_its_bare_name() {
        let plugin = SkeletonPlugin;
        let events = plugin.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, PONG_DISPATCHED_EVENT);
        assert!(!events[0].carries_tool_name);
    }
}
