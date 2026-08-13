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
//! private door.
//!
//! **What this crate is not.** Dynamic routing, context compaction,
//! memory, skills, and MCP support are each a separate, later board item
//! (`.design/philosophy-debt.md` entry 2's own sequencing note) — this
//! crate performs no real work of its own and is not a template any of
//! them must literally follow, only a proof that the tier's install
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
    async_trait, ContentBlock, PathArgs, PermissionClass, Plugin, PluginManifest, RenderKind, Tool,
    ToolCall, ToolCategory, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};

/// This plugin's manifest id: the string an operator names in
/// `[plugins].install` (`settings.json`) or a caller matches by hand
/// before calling `ConwayBuilder::with_plugin`.
pub const PLUGIN_ID: &str = "conway.plugin_skeleton";

/// The one tool this plugin provides.
pub const TOOL_NAME: &str = "skeleton_ping";

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
}
