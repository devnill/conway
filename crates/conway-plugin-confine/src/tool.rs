//! `ConfinedBashTool`: the `confined_bash` tool -- `conway.confine`'s own
//! bash-equivalent, every call launched through an OS containment primitive
//! instead of a bare `/bin/bash -c`.
//!
//! **Why a distinct `Tool` type, not a bare `conway::plugin::BashTool`
//! instance.** [`BashTool::with_launcher`]'s own doc explains the split:
//! `spec`/`path_args`/`render_kind`/`confined_by_tool` describe facts about
//! THIS type's own name, description, and structural claims -- a fresh
//! `sandbox-exec`/`bwrap`-wrapped `BashTool` still answers `"bash"`/
//! `Unconfinable`/`ShellCommand`/`false` for those, because `BashTool`
//! itself never changes what it claims about itself based on which launcher
//! built it (see that constructor's own doc for why that is deliberate).
//! `ConfinedBashTool` answers all four independently and truthfully for
//! ITSELF (a distinct name so the two tools never collide if both are
//! installed; `confined_by_tool() == true`, the structural flag the
//! root+unconfinable-shell-tool warning consults) while `invoke` alone
//! delegates to a freshly-built `BashTool::with_launcher`, so the
//! streaming/cancellation/timeout run loop stays ONE implementation.
//!
//! **Root resolution happens per call, not once at construction.** This
//! agent's `conway.fs` root (`crate::root::resolve_root`) can differ agent
//! to agent within one process (narrowed down a fork/spawn tree -- see
//! `conway_core::ports::Plugin::narrowable_keys`'s own doc), so the
//! [`conway::plugin::Launcher`] closure -- and therefore the `BashTool` it
//! launches -- is built fresh inside `invoke`, capturing THIS call's own
//! resolved root, never a root cached at plugin-construction time.

use std::path::PathBuf;

use async_trait::async_trait;
use conway::plugin::{
    BashTool, PathArgs, PermissionClass, RenderKind, Tool, ToolCall, ToolCategory, ToolCtx,
    ToolError, ToolOutput, ToolSpec,
};

use crate::root::resolve_root;
use crate::TOOL_NAME;

/// The SAME schema `conway.shell`'s own `bash` tool declares -- WHAT TO
/// BUILD point 1: "same argument schema as `BashArgs`". Duplicated rather
/// than imported: `conway_tools::shell::bash::BashArgs` is a private type
/// (`conway-tools` is not a dependency of this crate at all -- the plugin
/// tier depends on `conway::plugin` only), so this is a second, field-
/// identical declaration for SCHEMA/ANNOUNCEMENT purposes only. The actual
/// runtime parsing of a call's `arguments` never uses this type: `invoke`
/// (below) hands the untouched `ToolCall` to a freshly-built
/// `BashTool::with_launcher`, which parses it with `conway-tools`' own,
/// real `BashArgs` -- so a schema drift here could only ever make the
/// ANNOUNCED schema wrong, never change what a call actually does (P-14:
/// this type has no role in confinement, or in argument handling at all).
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConfinedBashArgs {
    /// Shell command executed with bash -c, confined to --root
    #[allow(dead_code)]
    command: String,
    /// Kill the command if it hasn't finished after this many milliseconds
    #[allow(dead_code)]
    #[serde(default)]
    timeout_ms: Option<u64>,
    /// Working directory; default the agent cwd
    #[allow(dead_code)]
    cwd: Option<String>,
}

/// `conway.confine`'s bash-equivalent tool -- see this module's own doc.
pub struct ConfinedBashTool {
    primitive: PathBuf,
}

impl ConfinedBashTool {
    /// Constructs this tool directly against `primitive`, with NO
    /// existence check -- unlike [`crate::ConfinePlugin::new`]/
    /// [`crate::ConfinePlugin::with_primitive_path`], which refuse to
    /// construct the PLUGIN at all when `primitive` does not exist (WHAT TO
    /// BUILD point 2's construction-time check). This constructor is the
    /// lower-level seam `ConfinePlugin` builds on, exposed directly so this
    /// crate's own tests can construct a tool against a real primitive path
    /// without going through the plugin, and so an embedder that already
    /// knows its primitive is present may skip the redundant `is_file`
    /// check. `Tool::invoke`'s own per-call re-check (this module's own
    /// doc) still applies regardless of how the tool was constructed.
    pub fn new(primitive: PathBuf) -> Self {
        Self { primitive }
    }
}

#[async_trait]
impl Tool for ConfinedBashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: conway::plugin::ToolName::new(TOOL_NAME),
            description: "Execute a shell command with bash -c, the same as the bash tool, but \
                every call runs inside this operating system's own containment primitive \
                (sandbox-exec on macOS, bwrap on Linux), which refuses any filesystem WRITE \
                outside --root. Reads and network access are NOT confined -- only writes. \
                Requires a confinement root (--root); a call with none configured is refused."
                .into(),
            schema: schemars::schema_for!(ConfinedBashArgs),
            category: ToolCategory::Execute,
            permission: PermissionClass::Dangerous,
        }
    }

    /// **Identical to `bash`'s own answer, and for the identical reason**
    /// (see `conway_tools::shell::bash::BashTool::path_args`'s own doc,
    /// restated here because this is a separate `Tool` implementation, not
    /// a reuse of that one): `command` reaches any path via redirection,
    /// substitution, `cd`, or a subprocess, so it is `Unconfinable` in the
    /// STATIC path-argument sense the permission broker's own root check
    /// uses -- that check has nothing to do with whether the OS containment
    /// primitive enforces a boundary at RUN time, which is a separate,
    /// later mechanism this tool's `invoke` applies regardless of what the
    /// broker decided. `cwd` stays checkable for the identical reason
    /// `bash`'s own does.
    fn path_args(&self) -> PathArgs {
        PathArgs::Unconfinable {
            checkable: &["cwd"],
        }
    }

    /// Identical to `bash`'s own answer: this tool's `render` output IS a
    /// shell command, so `PatternRule`'s metacharacter gate must still
    /// apply to it -- confinement is a RUN-time OS guarantee, not a reason
    /// to relax the operator's own pattern-grant legibility check.
    fn render_kind(&self) -> RenderKind {
        RenderKind::ShellCommand
    }

    /// The structural flag the root+unconfinable-shell-tool warning
    /// consults (`Tool::confined_by_tool`'s own doc) -- `true` because this
    /// tool's own `invoke`, below, genuinely enforces `--root` through an OS
    /// primitive rather than merely declaring that it does.
    fn confined_by_tool(&self) -> bool {
        true
    }

    /// The bare shell command -- byte-for-byte `bash`'s own override (see
    /// that tool's doc for why `PatternRule` needs the literal command
    /// text, not a JSON dump). Duplicated for the identical private-type
    /// reason [`ConfinedBashArgs`] is: there is no shared, importable
    /// implementation to call instead.
    fn render(&self, args: &serde_json::Value) -> String {
        match args.get("command").and_then(serde_json::Value::as_str) {
            Some(command) => command.to_string(),
            None => format!("{}({})", self.spec().name, args),
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        // WHAT TO BUILD point 2: a root missing at CALL time is an error
        // naming `--root`, never an unconfined run.
        let root = resolve_root(&ctx)?;

        // WHAT TO BUILD point 2: the primitive missing at CALL time (it was
        // already checked once, at plugin construction, by
        // `ConfinePlugin::with_primitive_path`) is ALSO an error, never a
        // silent fall-through to running unconfined -- defense against the
        // primitive being removed out from under an already-running
        // process, not merely a construction-time nicety.
        if !self.primitive.is_file() {
            return Err(ToolError::Denied {
                reason: format!(
                    "conway.confine's OS containment primitive ({}) is no longer present -- \
                     refusing to run this command unconfined",
                    self.primitive.display()
                ),
            });
        }

        let launcher = build_launcher(self.primitive.clone(), PathBuf::from(root))?;
        BashTool::with_launcher(launcher).invoke(call, ctx).await
    }
}

/// Builds this call's own [`conway::plugin::Launcher`] from the resolved
/// primitive/root -- one OS-specific launcher, chosen at compile time. A
/// build for any OTHER target compiles (this crate is not `#[cfg(unix)]`
/// gated at the crate level) but every call refuses with a named "no
/// containment primitive on this OS" error -- see WHAT NOT TO BUILD: no
/// Landlock/`unshare`, and no fallback to an unconfined run on a platform
/// this crate does not implement a primitive for.
#[cfg(target_os = "macos")]
fn build_launcher(
    primitive: PathBuf,
    root: PathBuf,
) -> Result<conway::plugin::Launcher, ToolError> {
    Ok(crate::launcher::sandbox_exec_launcher(primitive, root))
}

#[cfg(target_os = "linux")]
fn build_launcher(
    primitive: PathBuf,
    root: PathBuf,
) -> Result<conway::plugin::Launcher, ToolError> {
    Ok(crate::launcher::bwrap_launcher(primitive, root))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn build_launcher(
    _primitive: PathBuf,
    _root: PathBuf,
) -> Result<conway::plugin::Launcher, ToolError> {
    Err(ToolError::Denied {
        reason: "conway.confine implements no OS containment primitive on this platform \
                 (only macOS's sandbox-exec and Linux's bwrap are implemented) -- refusing to \
                 run this command unconfined"
            .into(),
    })
}
