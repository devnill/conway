//! `conway-plugin-skeleton`: a worked example of the first-party plugin
//! tier — see `PHILOSOPHY.md`'s
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
//! private door. As of, that
//! includes declaring and firing a custom event
//! ([`PONG_DISPATCHED_EVENT`]): `SkeletonPlugin::events` declares it and
//! `SkeletonPingTool::invoke` fires it on every call, unconditionally --
//! the open-vocabulary half of `PHILOSOPHY.md` §5's hooks claim, proven
//! end to end against a real configured hook in
//! `tests/skeleton_end_to_end.rs` rather than asserted in prose.
//!
//! **What this crate is not.** Dynamic routing is built
//! (`conway-plugin-routing`); context compaction, memory, skills, and MCP
//! support are not, each separate, later work with no yet as of
//! 2026-08-13 (`scripts/board-claims.md`'s `UNFILED` entry records the gap)
//! — this crate performs no real work of its own and is not a template any
//! of them must literally follow, only a proof that the tier's install
//! mechanism holds together end to end.
//!
//! **As of board item `01M0WWPA70E8YAAN981EK10D3D`, this crate also proves
//! Edge B (plugin -> plugin capability calls,
//! `docs/vision/DESIGN-plugin-dependencies.md` §2).**
//! [`SkeletonAskTool`]'s `skeleton_ask` is this tier's first CONSUMER of
//! another plugin's capability, and the first in-tree caller of
//! [`conway::plugin::CapabilityCallHandle::call_versioned`] -- that
//! method's own doc named this crate's board item as its intended first
//! consumer before this tool existed. It calls into `conway-plugin-ui`'s
//! `ui.form` capability BY BARE NAME AND HAND-BUILT JSON, with no compile-
//! time dependency on that crate from `src/` (see [`SkeletonAskTool`]'s own
//! doc) -- proving, not merely asserting, that Edge B needs no shared type
//! between a provider and a consumer.
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
    PathArgs, PermissionClass, Plugin, PluginDescription, PluginManifest, RenderKind, Tool,
    ToolCall, ToolCategory, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};

/// This plugin's manifest id: the string an operator names in
/// `[plugins].install` (`settings.json`) or a caller matches by hand
/// before calling `ConwayBuilder::with_plugin`.
pub const PLUGIN_ID: &str = "conway.plugin_skeleton";

/// The one tool this plugin provides.
pub const TOOL_NAME: &str = "skeleton_ping";

/// This plugin's own custom event (:
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
        // The worked example's event half (//): fires this plugin's OWN declared
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

/// The bare name `SkeletonAskTool` registers under.
pub const ASK_TOOL_NAME: &str = "skeleton_ask";

/// `conway.ui`'s published capability, named BARE -- this crate depends on
/// no type from `conway-plugin-ui` in `src/` (only in `[dev-dependencies]`,
/// for this crate's own tests -- see this crate's own Cargo.toml comment).
/// A consumer that needed a compile-time dependency on its provider's crate
/// would defeat the entire argument `docs/vision/
/// DESIGN-plugin-dependencies.md` §2 makes for Edge B being dynamic,
/// serialisable JSON rather than a typed Rust trait per capability: an
/// out-of-process caller (a subprocess plugin) could never depend on
/// `conway-plugin-ui`'s Rust types at all, and this in-process caller does
/// not either, on purpose.
const UI_FORM_CAPABILITY: &str = "ui.form";

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AskArgs {
    /// The question to pose. Optional: a bare call asks a fixed default
    /// question, mirroring `PingArgs::message`'s own "optional, defaulted"
    /// shape above.
    #[serde(default)]
    prompt: Option<String>,
}

/// `skeleton_ask`: this tier's first real CONSUMER of Edge B
/// (`ctx.capabilities`, `docs/vision/DESIGN-plugin-dependencies.md` §2) --
/// [`SkeletonPingTool`] above proves a plugin can give the model a tool;
/// this proves a plugin can call ANOTHER plugin's capability from inside
/// one. Poses one fixed yes/no question through `conway.ui`'s `ui.form`
/// capability (`^1`, decision `01M189XS6Z9VKYENAHNY1B54CM`) and reports
/// whatever comes back.
///
/// **Never fails the call, either way** (`ToolOutput::is_error` stays
/// `false` in both branches below) -- `conway.ui` not installed at all,
/// installed but declaring an incompatible version, or installed with no
/// drawing surface wired in to answer through (`conway_plugin_ui`'s own
/// module doc: every host today) are all the SAME, ordinary outcome from
/// this tool's point of view: no answer was collected, so it says so and
/// moves on. This is the MAIN-LINE degrade path board item
/// `01M0WWPA70E8YAAN981EK10D3D`'s acceptance 3 names, not a fallback for an
/// edge case -- asking a question nobody could answer is not a tool error.
struct SkeletonAskTool;

#[async_trait]
impl Tool for SkeletonAskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(ASK_TOOL_NAME),
            description: "First-party plugin tier: poses one fixed yes/no question through \
                          conway.ui's ui.form capability (Edge B) and reports the answer, or a \
                          plain degrade message if none could be collected. Not registered by \
                          default -- installed via `[plugins].install` or \
                          `ConwayBuilder::with_plugin`."
                .to_string(),
            schema: schemars::schema_for!(AskArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: AskArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;
        let prompt = args.prompt.unwrap_or_else(|| "proceed?".to_string());
        let payload = serde_json::json!({
            "prompt": prompt,
            "options": ["yes", "no"],
        });
        // A hand-written literal, never operator- or model-supplied --
        // `.expect()` here is the same posture `CapabilityRegistration::new`
        // itself takes for a hard-coded version string (that constructor's
        // own doc): a malformed literal would be a programmer error caught
        // the first time this code runs at all, not untrusted input P-10
        // governs.
        let required =
            semver::VersionReq::parse("^1").expect("\"^1\" is a literal and always parses");
        let text = match ctx
            .capabilities
            .call_versioned(UI_FORM_CAPABILITY, &required, payload)
            .await
        {
            Ok(answer) => {
                let selected = answer
                    .get("selected")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(the answer carried no \"selected\" field)")
                    .to_string();
                format!("skeleton ask: answered '{selected}'")
            }
            Err(e) => format!("skeleton ask: no answer available ({e}); proceeding without one"),
        };
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

/// The bare name `SkeletonPingCommand` registers under -- reachable in the
/// TUI as `/{PLUGIN_ID}.{COMMAND_NAME}` (`conway_cli::tui::commands::
/// CommandRegistry::build` prefixes it with this plugin's own manifest id;
/// see that function's own doc for why an author never picks their own
/// namespace).
pub const COMMAND_NAME: &str = "ping";

/// `/{PLUGIN_ID}.ping`: the worked example's command half , proving a plugin can give the OPERATOR
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

/// A command whose entire behavior is "read this file once, submit its
/// body as a new turn" -- the smallest possible instance of the capability
/// board item `01M0VSMF71S6VXX81YRAAF5S8Q` ships
/// (`conway::plugin::CommandOutcome::SubmitPrompt`), and this tier's own
/// proof that a markdown file becomes a typeable command with no Rust
/// beyond the handful of lines below.
///
/// **Deliberately NOT wired into [`SkeletonPlugin::commands`].** This type
/// is fallible to construct (the named file may not exist) and reads a
/// caller-supplied path -- unlike `SkeletonPingCommand`, it cannot be a
/// zero-argument, always-installed member of the skeleton plugin's fixed
/// command list without inventing a fake path nobody asked for. A caller
/// (this crate's own `tests/file_prompt_command.rs`, or a library embedder
/// following the same shape) constructs one explicitly, via
/// [`Self::from_file`], and installs it on whatever `Plugin` it likes --
/// exactly the same "construct it yourself, install it yourself" contract
/// every other `Command` in this workspace already has.
///
/// **v1, deliberately: no interpolation of any kind.** [`CommandCtx::args`]
/// is read and ignored -- the submitted text is always the file's own
/// verbatim body, read once at construction (never re-read per
/// invocation, so a file edited after construction takes effect only on
/// the next process start). This mirrors `Plugin::instructions`'s own
/// `include_str!` convention's accepted tradeoff (see that method's own
/// doc, "Convention, not enforcement"), chosen here for the identical
/// reason: a v1 whose entire job is proving the capability end to end
/// needs no live-reload story, and P-10 (range-check untrusted input at
/// the boundary) prefers the smaller slice -- no template language exists
/// anywhere in this type for a file's own content, or an operator's typed
/// arguments, to be parsed through.
pub struct FilePromptCommand {
    name: String,
    summary: String,
    body: String,
}

impl FilePromptCommand {
    /// Reads `path`'s content once, at construction -- fallible (the file
    /// may not exist, or may not be valid UTF-8), surfacing the error to
    /// the caller's own construction site rather than deferring it to a
    /// later `invoke` no caller could react to sensibly (`Plugin`'s own
    /// module doc: "an implementer needing setup does it in its own
    /// constructor... where errors surface to the embedder directly").
    /// `name` is this command's bare name (no leading `/`, no plugin-id
    /// prefix -- the host prefixes it, exactly like every other
    /// [`CommandSpec::name`]).
    pub fn from_file(name: impl Into<String>, path: &std::path::Path) -> std::io::Result<Self> {
        let body = std::fs::read_to_string(path)?;
        let name = name.into();
        Ok(Self {
            summary: format!("submits {path:?}'s body as a new turn"),
            name,
            body,
        })
    }
}

#[async_trait]
impl Command for FilePromptCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: self.name.clone(),
            summary: self.summary.clone(),
        }
    }

    async fn invoke(&self, _ctx: CommandCtx) -> CommandOutcome {
        CommandOutcome::SubmitPrompt {
            text: self.body.clone(),
        }
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
            // Versioned WITH the workspace --
            // see this crate's own Cargo.toml doc comment.
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![ToolName::new(TOOL_NAME), ToolName::new(ASK_TOOL_NAME)],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            // `skeleton_ask` calls into `conway.ui`'s `ui.form` capability
            // (board item `01M0WWPA70E8YAAN981EK10D3D`) but degrades
            // cleanly when it is absent -- `docs/vision/
            // DESIGN-plugin-dependencies.md` §4a's own test for `optional`
            // rather than `requires`: this plugin's stated function (a
            // tool that answers, or honestly says it could not) survives
            // fully intact either way. Declaring it here means uninstalling
            // `conway.ui` while this plugin is enabled is announced
            // (`WarningCode::OptionalPluginDependencyMissing` plus a
            // `tracing::warn!`), never silent.
            optional: vec!["conway.ui".to_string()],
        }
    }

    /// Honest about what this crate is: a worked example of the install
    /// mechanism, not a real capability -- see this crate's own module doc.
    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "a worked example proving the plugin install mechanism".to_string(),
            you_get: format!(
                "2 tools ({TOOL_NAME} echoes an argument back; {ASK_TOOL_NAME} poses a fixed \
                 question through conway.ui's ui.form capability, if installed) and 1 command \
                 that echoes an argument back -- proof the install mechanism, and now the \
                 plugin-to-plugin capability channel, both work; no real capability of its own"
            ),
            you_lose: "nothing -- it does no real work of its own".to_string(),
            costs: "none".to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(SkeletonPingTool), Arc::new(SkeletonAskTool)]
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
mod plugin_tests {
    use super::*;

    /// The plugin browser's own read surface (board item
    /// `01M0KARX71A64NTSYTDBVANVPF`): a real description, never the
    /// trait's empty default.
    #[test]
    fn description_is_non_empty() {
        let description = SkeletonPlugin.description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
    }

    /// Board item `01M0WWPA70E8YAAN981EK10D3D`: `skeleton_ask` joins
    /// `skeleton_ping` as a real, dispatchable tool -- the manifest's own
    /// `tools` list (checked at `ConwayBuilder::build`, independent of
    /// `Plugin::tools`'s own return) must name both.
    #[test]
    fn manifest_names_both_tools() {
        let manifest = SkeletonPlugin.manifest();
        assert_eq!(
            manifest.tools,
            vec![ToolName::new(TOOL_NAME), ToolName::new(ASK_TOOL_NAME)]
        );
    }

    /// `skeleton_ask` degrades rather than requires -- `docs/vision/
    /// DESIGN-plugin-dependencies.md` §4a's own test: this plugin's stated
    /// function survives fully without `conway.ui` installed, so the edge
    /// belongs in `optional`, never `requires`.
    #[test]
    fn conway_ui_is_declared_optional_not_required() {
        let manifest = SkeletonPlugin.manifest();
        assert_eq!(manifest.optional, vec!["conway.ui".to_string()]);
        assert!(manifest.requires.is_empty());
    }

    #[test]
    fn plugin_declares_both_tools_and_the_one_ping_command() {
        let plugin = SkeletonPlugin;
        let tool_names: Vec<_> = plugin.tools().iter().map(|t| t.spec().name).collect();
        assert_eq!(
            tool_names,
            vec![ToolName::new(TOOL_NAME), ToolName::new(ASK_TOOL_NAME)]
        );
        assert_eq!(plugin.commands().len(), 1);
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;

    fn ctx(args: &str) -> CommandCtx {
        CommandCtx {
            focused_agent: conway::AgentId::new(),
            root_agent: conway::AgentId::new(),
            session_id: conway::SessionId::new(),
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
