//! `conway.confine`: a bash-equivalent tool (`confined_bash`) whose every
//! command runs inside the operating system's own containment primitive --
//! `sandbox-exec` on macOS, `bwrap` on Linux -- so a session's `--root`
//! becomes a real write boundary an agent cannot reach past, rather than a
//! path-argument convention `bash` (`conway.shell`) has always been outside
//! of.
//!
//! # Why this exists (operator ruling 2026-09-01, decision
//! `01M1FQG08GDQ71984T0W0RJ019`)
//!
//! Approving every shell command by hand trains an operator to stop
//! reading, and approving them all at once (session `AutoAllow`) with the
//! ordinary `bash` tool means one bad command can write anywhere the
//! operator's own account can. This plugin is the guardrail that makes
//! blanket approval of a confined shell actually safe: the OS, not this
//! crate reading the command text, is what refuses a write outside `--root`.
//!
//! # Mechanism, not policy
//!
//! **This crate never reads a command to decide anything.** The whole
//! command string goes to `/bin/bash -c` verbatim, wrapped in the OS
//! primitive's own argv (`crate::launcher`) -- the containment guarantee is
//! the kernel's (Seatbelt's, or Linux namespaces'), not a Rust-side
//! allow/deny list, metacharacter scan, or path-extraction heuristic. See
//! `docs/plugins/confine.md` for the operator-facing statement of exactly
//! what this does and does not confine.
//!
//! # What is confined, and what is not
//!
//! **Writes only.** Both containment profiles this crate builds deny
//! filesystem writes everywhere except under the confinement root; neither
//! restricts reads or network reachability at all -- the ruling's own
//! scope, restated in `crate::launcher`'s own module doc and in
//! `docs/plugins/confine.md`.
//!
//! # No fallback to unconfined execution, ever
//!
//! A missing primitive at plugin construction ([`ConfinePlugin::new`]/
//! [`ConfinePlugin::with_primitive_path`]) is a `conway::FacadeError::Config`
//! naming the binary; a primitive that has gone missing by CALL time (e.g.
//! removed out from under an already-running process) is a per-call error,
//! never a silent unconfined run (`tool::ConfinedBashTool::invoke`'s own
//! re-check). No Landlock/`unshare` is implemented in this slice -- a
//! labeled absence, not a claim -- and no code path in this crate ever runs
//! a command bare when confinement was requested.

mod launcher;
mod root;
mod tool;

use std::path::PathBuf;
use std::sync::Arc;

use conway::plugin::{Plugin, PluginDescription, PluginManifest, Tool};
use conway::FacadeError;

pub use tool::ConfinedBashTool;

/// This plugin's manifest id.
pub const PLUGIN_ID: &str = "conway.confine";

/// This plugin's one tool. Named distinctly from `bash` (`conway.shell`'s
/// own tool) -- see `tool::ConfinedBashTool`'s own module doc for why this
/// crate does not simply reuse the name `"bash"`: two installed plugins
/// both claiming that name would collide, and the model needs a name that
/// tells the two apart (confined vs. not) without reading either
/// description.
pub const TOOL_NAME: &str = "confined_bash";

/// macOS's own Seatbelt front end -- present at this fixed path on every
/// macOS release conway supports (`man sandbox-exec`; it has shipped at
/// this exact path since Mac OS X 10.5).
#[cfg(target_os = "macos")]
pub const DEFAULT_PRIMITIVE_PATH: &str = "/usr/bin/sandbox-exec";

/// bubblewrap, resolved at the ordinary location a distro's package manager
/// installs it. Unlike `sandbox-exec`, there is no single fixed path across
/// every Linux distribution -- a caller whose `bwrap` lives elsewhere (a
/// Nix store path, `/usr/local/bin/bwrap`) uses
/// [`ConfinePlugin::with_primitive_path`] instead of [`ConfinePlugin::new`].
#[cfg(target_os = "linux")]
pub const DEFAULT_PRIMITIVE_PATH: &str = "/usr/bin/bwrap";

/// The human-readable name of the primitive binary this build expects, for
/// an error message -- kept as a separate constant from
/// [`DEFAULT_PRIMITIVE_PATH`] (a full path) so the message reads naturally
/// regardless of which absolute path this OS happens to use.
#[cfg(target_os = "macos")]
const PRIMITIVE_BINARY_NAME: &str = "sandbox-exec";
#[cfg(target_os = "linux")]
const PRIMITIVE_BINARY_NAME: &str = "bwrap";
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const PRIMITIVE_BINARY_NAME: &str = "sandbox-exec (macOS) or bwrap (Linux)";

/// [`DEFAULT_PRIMITIVE_PATH`], as a plain function rather than a
/// per-target-gated constant -- so a caller that only wants SOME path (e.g.
/// [`ConfinePlugin::unchecked`], for a browse-only listing that must
/// compile and run on every target regardless of which primitive, if any,
/// this OS implements) does not need its own `#[cfg(target_os = ...)]`
/// gate. On a target this crate implements no primitive for at all, this
/// returns a plainly-invalid placeholder path -- correct for `unchecked`'s
/// own "no existence check performed" contract, and never reached by
/// [`ConfinePlugin::new`]'s real, checked path on such a target (there is
/// no real path to try, so [`ConfinePlugin::with_primitive_path`] is the
/// only way to attempt real construction there, exactly like every other
/// unresolvable path).
pub fn default_primitive_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        PathBuf::from(DEFAULT_PRIMITIVE_PATH)
    }
    #[cfg(target_os = "linux")]
    {
        PathBuf::from(DEFAULT_PRIMITIVE_PATH)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        PathBuf::from("conway-confine-has-no-primitive-on-this-target")
    }
}

/// The `conway.confine` plugin: [`ConfinedBashTool`], the `confined_bash`
/// tool.
pub struct ConfinePlugin {
    tool: Arc<dyn Tool>,
}

impl ConfinePlugin {
    /// Constructs this plugin against the OS default primitive path
    /// ([`DEFAULT_PRIMITIVE_PATH`]). Fails with a `conway::FacadeError::Config`
    /// naming the binary if it does not exist at that path -- WHAT TO BUILD
    /// point 2: "Primitive missing at plugin construction →
    /// `ConwayBuilder::build` config error naming the binary." This crate
    /// never falls back to installing a tool that would run unconfined, so
    /// there is no other outcome for a missing primitive than refusing to
    /// construct at all.
    ///
    /// On a target this crate implements no primitive for at all (neither
    /// macOS nor Linux), [`default_primitive_path`] returns a placeholder
    /// that never exists, so this still fails the same honest way --
    /// naming that placeholder is not useful, so prefer
    /// [`Self::with_primitive_path`] directly on such a target.
    pub fn new() -> Result<Self, FacadeError> {
        Self::with_primitive_path(default_primitive_path())
    }

    /// Constructs this plugin against an explicit primitive binary path
    /// rather than the OS default -- the seam a caller uses to point at a
    /// non-default install location, and the seam this crate's own
    /// acceptance test for "missing primitive" uses to exercise the failure
    /// deterministically, without needing `sandbox-exec`/`bwrap` to be
    /// ABSENT on the machine running the test.
    ///
    /// Fails the identical way [`Self::new`] does when `primitive` does not
    /// name an existing file.
    pub fn with_primitive_path(primitive: PathBuf) -> Result<Self, FacadeError> {
        if !primitive.is_file() {
            return Err(FacadeError::Config {
                path: None,
                message: format!(
                    "conway.confine: the OS containment primitive '{}' does not exist. \
                     conway.confine never falls back to running a command unconfined, so it \
                     refuses to install without it -- install {PRIMITIVE_BINARY_NAME}, point \
                     ConfinePlugin::with_primitive_path at its real location, or remove \
                     \"conway.confine\" from plugins.install.",
                    primitive.display()
                ),
            });
        }
        Ok(Self {
            tool: Arc::new(ConfinedBashTool::new(primitive)),
        })
    }

    /// Constructs this plugin with NO existence check on the primitive path
    /// -- for display/browsing purposes only (a plugin browser's "every
    /// linked candidate, installed or not" scan, `conway-cli`'s own
    /// `first_party_plugins::all_bundle_plugins`), never for an instance a
    /// real turn dispatches a tool call through.
    ///
    /// **Why this exists at all**, given [`Self::new`]/[`Self::with_primitive_path`]'s
    /// whole point is refusing to construct without
    /// a real primitive: a read-only capability listing (manifest, tool
    /// name, [`Plugin::description`]) must still be able to name this
    /// plugin on a machine that happens to lack `sandbox-exec`/`bwrap` --
    /// the ordinary case for `conway.trim`/`conway.idiom`/every other
    /// bundle member that constructs unconditionally for browsing and is
    /// separately gated by `[plugins].install` for whether it actually
    /// attaches. `ConwayBuilder::build` (the REAL, dispatch-affecting path)
    /// never calls this -- it calls [`Self::new`]/[`Self::with_primitive_path`],
    /// so an actually-missing primitive still fails
    /// the real build exactly as WHAT TO BUILD point 2 requires; only the
    /// unfiltered display scan uses the unchecked path, on the identical
    /// footing `conway_plugin_skills::SkillsPlugin`'s own "malformed skill
    /// file falls back to an empty shape HERE" precedent already
    /// establishes (`crates/conway-cli/src/first_party_plugins.rs`'s
    /// `bundle`, "a genuinely broken skill never reaches a turn with a
    /// silently-empty plugin, it fails the build first").
    pub fn unchecked(primitive: PathBuf) -> Self {
        Self {
            tool: Arc::new(ConfinedBashTool::new(primitive)),
        }
    }
}

impl Plugin for ConfinePlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            tools: vec![self.tool.spec().name],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![self.tool.clone()]
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "a bash tool confined to --root by an OS sandbox, not a convention".into(),
            you_get: "one tool, confined_bash: every command runs under sandbox-exec (macOS) \
                      or bwrap (Linux); a write outside --root is refused by the OS itself. \
                      Reads and network reach exactly as far as they would unconfined."
                .into(),
            you_lose: "nothing conway.shell (bash) already gives you -- install both to let an \
                       operator/agent choose per call, or this one alone for a session where \
                       every write must stay inside the root."
                .into(),
            costs: "one OS containment primitive spawned per call, in addition to bash itself; \
                    a call with no --root configured for this agent is refused outright."
                .into(),
        }
    }
}
