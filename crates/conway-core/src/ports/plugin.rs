//! The `Plugin`/`Tool` ports (architecture §4.2) and the `CancellationToken`
//! used to interrupt an in-flight tool call.
//!
//! **There is exactly one extension mechanism: the plugin API.** Built-in
//! read/write/edit/bash and the subagent tool are `Plugin` implementations
//! registered by default in `ConwayBuilder`; nothing about them is
//! privileged. MVP plugins are in-process `Arc<dyn Plugin>` (Tension T-8).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::content::{Artifact, ContentBlock, ToolCall, ToolSpec, TruncationPolicy};
use crate::error::{CwdError, ToolError};
use crate::event_name::{validate_event_name, EVENT_NAMESPACE_SEPARATOR};
use crate::ids::{AgentId, LogSeq, ModelRef, SessionId, ToolName};
use crate::ports::{ArtifactWriteHandle, EventSinkHandle, SubagentHandle};
use crate::segment::PromptSegment;

/// A source of tools: a plugin declares its identity and the tools it
/// provides.
///
/// **There is no initialization hook, deliberately.** This trait carried an
/// `on_init(&PluginInitCtx)` method documented as "called once at startup"
/// that **nothing ever called** — no call site existed anywhere in the
/// workspace, and no implementor, built-in or otherwise, overrode it. A hook
/// that silently never runs is worse than an absent one: an absent hook is a
/// known limitation, an unwired one is a trap that costs an implementer a
/// debugging session to discover.
///
/// It is not merely unwired but hard to wire, which is why it was removed
/// rather than connected. `PluginRegistry::from_plugins` is synchronous and
/// eager — it calls `tools()` and compiles every schema at construction — and
/// `Runtime::new` builds it, so there is no natural point at which a fallible,
/// I/O-performing hook could run and have its failure mean anything useful.
/// A plugin needing setup does it in its own constructor, before
/// `ConwayBuilder::with_plugin`, where errors surface to the embedder
/// directly.
///
/// If a genuine lifecycle hook is wanted later, the out-of-process plugin
/// design specifies a real handshake with a
/// defined failure mode — that, not a resurrected no-op, is the shape to
/// build.
pub trait Plugin: Send + Sync + 'static {
    /// This plugin's static identity: id, semver, provided tools, required
    /// host capabilities.
    fn manifest(&self) -> PluginManifest;

    fn tools(&self) -> Vec<Arc<dyn Tool>>;

    /// Zero or more TUI slash commands this plugin contributes. The default returns none, so every
    /// existing `Plugin` implementor -- every built-in, every first-party
    /// plugin, every third party's -- keeps compiling unmodified.
    ///
    /// **Why this exists at all.** Before this method, `Plugin::manifest`/
    /// `Plugin::tools` was the WHOLE trait: a plugin could give the *model* a
    /// tool, but had no way to give the *operator* anything to type. That made
    /// the TUI's closed `SlashCommand` enum the last genuinely privileged
    /// surface in the harness (`PHILOSOPHY.md` §5's own membership test says
    /// built-ins are unprivileged "in the way that nothing in `/bin` is
    /// privileged over a program you wrote yourself" -- true for tools, true
    /// for backends, false for commands, until this). See
    /// `conway_cli::tui::commands`'s own module doc for the dispatch seam
    /// this feeds (`SlashCommand::Plugin`, resolved through the ordinary
    /// `parse`/`execute` path -- never a second, parser-bypassing surface).
    fn commands(&self) -> Vec<Arc<dyn Command>> {
        Vec::new()
    }

    /// Zero or more custom events this plugin may fire (///, `PHILOSOPHY.md` §5: "That list is open
    /// rather than fixed. A plugin declares the events it emits, so
    /// installing one brings hook points along with whatever else it
    /// provides... Those events sit at the same level as the ones conway
    /// emits"). The default returns none, so every existing `Plugin`
    /// implementor keeps compiling unmodified -- the SAME zero-cost-default
    /// precedent [`Self::commands`] established immediately above.
    ///
    /// **Follows that exact precedent, not a new pattern.** [`EventDecl`]
    /// is constructed the same way [`CommandSpec`] is: `name` is BARE, and
    /// the host -- not the plugin -- prefixes it with this plugin's own
    /// [`PluginManifest::id`] before it is ever reachable in an operator's
    /// `[hooks].rules[].event`, so a plugin can never pick its own
    /// namespace (mirrors [`Self::commands`]'s own "an author never picks
    /// their own namespace" rule). `conway_runtime::hook_dispatch::
    /// declared_plugin_events` performs that prefixing and validates the
    /// result with [`crate::event_name::validate_event_name`] -- the SAME
    /// shared validator [`Self::commands`]' registrar
    /// (`conway_cli::tui::commands::CommandRegistry::build`) already uses
    /// for command names (see that function's own doc, and
    /// `conway_core::event_name`'s private `validate_namespaced`, under
    /// "A third consumer, same rule, different vocabulary").
    ///
    /// **An event declared here and never fired is the same defect as a
    /// tool that does nothing** (`PHILOSOPHY.md` §5, verbatim) -- this
    /// method only ships the DECLARATION half; a plugin fires one of its
    /// own declared events from inside [`Tool::invoke`] via
    /// [`ToolCtx::plugin_events`] ([`PluginEventHandle::emit`]), the
    /// SAME `plugin_id.bare_name` shape this method's own `name` fields
    /// become.
    fn events(&self) -> Vec<EventDecl> {
        Vec::new()
    }

    /// Zero or more [`ToolObserver`](crate::ports::ToolObserver)s this plugin
    /// installs -- policy that watches finished tool calls and may add to the
    /// record. `PHILOSOPHY.md` §6 leaves loop intervention to the operator, so
    /// the harness ships the seam and a plugin supplies the judgment; see that
    /// port's own module doc for what an observer may and may not do.
    ///
    /// The default returns none, the same zero-cost-default precedent
    /// [`Self::commands`] and [`Self::events`] established above: every
    /// existing `Plugin` implementor keeps compiling unmodified, and a build
    /// with no observing plugin installed invokes nothing.
    ///
    /// Each observer is bound to THIS plugin's own [`PluginManifest::id`] when
    /// the runtime calls it, so the events it fires land under this plugin's
    /// namespace and no other -- an author never picks their own namespace,
    /// matching [`Self::commands`] and [`Self::events`] exactly.
    fn observers(&self) -> Vec<Arc<dyn crate::ports::ToolObserver>> {
        Vec::new()
    }
}

/// One custom event a plugin declares it may emit -- the event-vocabulary
/// sibling of [`CommandSpec`] (see [`Plugin::events`]'s own doc for why
/// this mirrors that type's exact shape rather than inventing a new one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventDecl {
    /// Bare name, e.g. `"candidate_chosen"` for a plugin whose manifest id
    /// is `"acme.routing"` -- reachable in `[hooks].rules[].event` as
    /// `"acme.routing.candidate_chosen"` once the host prefixes it (see
    /// [`Plugin::events`]'s own doc). Must be non-empty; enforced where the
    /// prefixing happens (`conway_runtime::hook_dispatch::
    /// declared_plugin_events`), not by this type itself, matching
    /// [`CommandSpec::name`]'s own identical division of labor.
    pub name: String,
    /// One line describing when this event fires and what its payload
    /// carries -- the answer to "how does an operator discover what is
    /// hookable given what they have installed" (`PHILOSOPHY.md` §5's own
    /// "an open vocabulary nobody can enumerate is worse than a closed
    /// one" concern): an embedder already holding the `&[Arc<dyn Plugin>]`
    /// it is about to pass to `ConwayBuilder` can call
    /// `conway_runtime::hook_dispatch::declared_plugin_events` itself and
    /// read this field for every event every installed plugin declares --
    /// no separate registry, no new port, the identical mechanism
    /// [`Plugin::commands`] already exposes for command discovery.
    pub summary: String,
    /// Whether this event's payload carries a `"tool"` string field --
    /// decides whether an operator's `[hooks].rules[]` entry may pair this
    /// event with `match` ('s rule,
    /// extended to plugin-declared events by this type). `false` here
    /// makes a rule's `match` on this event the SAME typed, build-time
    /// error `crates/conway/src/config/merge.rs`'s check 10 already gives
    /// a core event without a tool name -- see
    /// `crates/conway/src/builder.rs`'s own plugin-event validation pass
    /// for where that is enforced (this crate performs no config
    /// validation itself).
    pub carries_tool_name: bool,
}

/// One plugin-declared TUI slash command -- a plugin's own [`CommandSpec`] plus the
/// async handler `conway-cli` invokes when the operator types it.
///
/// **Deliberately narrow -- still true after
///.** [`CommandCtx`] carries read-only identity
/// and the raw argument text -- nothing that reaches a live
/// `Conway`/`SessionHandle`, and never will: `Plugin`/`Command` live in
/// `conway-core`, which cannot depend on `conway` (the facade, where
/// `Conway::fork_from` and session-swap capability live) without a cycle, so
/// a command can never hold a live handle onto its own session, let alone
/// another one.
///
/// **What DID change: [`CommandOutcome::ForkSession`].** A command that
/// wants to fork its own calling session (the capability `/rewind` needs,
/// per the owner's ruling that session-history features are plugins, not
/// core functionality) does not reach for a handle at all -- it RETURNS a
/// request, and the HOST (`conway-cli`'s `App`, which already holds a live
/// `Conway`) performs the fork with its own facade capability. This is the
/// declare/return-an-effect shape [`ContextHook`]'s own `before_request` and
/// `docs/plugins/hooks.md` point 12's "declare, then push; the render path
/// never blocks on a plugin" precedent both already establish -- applied
/// here to close the ONE gap
/// deliberately left open (see [`CommandOutcome::ForkSession`]'s own doc for
/// the full binding argument: nothing about this variant lets a command name
/// a session other than the one that invoked it -- there is no field to put
/// one in).
///
/// Everything else about the narrowing stands: a command still cannot
/// resume a DIFFERENT session, steer any agent, read/write a file through
/// conway's own mediation, or reach the permission broker. An extension
/// point earns a wider grant only once a real consumer needs it, not ahead
/// of one (YAGNI) -- `/rewind`, the item that asked this question first, is
/// that consumer for forking; nothing wider is built here.
#[async_trait]
pub trait Command: Send + Sync + 'static {
    /// This command's bare name (no leading `/`, no plugin-id prefix -- the
    /// host prefixes it with the declaring plugin's own
    /// [`PluginManifest::id`] before registering it, so an author never
    /// picks their own namespace) and a one-line summary for `/help`/the `/`
    /// palette.
    fn spec(&self) -> CommandSpec;

    /// Runs this command. **Must be safe to run on a spawned task**: the
    /// host never awaits this directly on its render/input loop (see this
    /// trait's own doc), so a slow implementation degrades to "the operator
    /// doesn't see the result yet," never to a frozen terminal -- but an
    /// implementation should still honor ordinary async hygiene (no
    /// unbounded blocking I/O without `spawn_blocking`) since a hung task is
    /// still a leaked task, even though it cannot hang the UI.
    async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome;
}

/// A plugin command's declared identity (module doc: [`Command::spec`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    /// Bare name, e.g. `"greet"` for a plugin whose manifest id is
    /// `"acme.tools"`, reachable as `/acme.tools.greet`. Must be non-empty
    /// and contain no whitespace -- the host rejects (named error, at
    /// registration) anything that would be unreachable through
    /// `commands::parse`'s own whitespace-splitting rule.
    pub name: String,
    /// One line, shown in `/help`'s pointer to the `/` palette and as the
    /// palette row's own description.
    pub summary: String,
}

/// Read-only invocation context handed to [`Command::invoke`] -- see
/// [`Command`]'s own doc for why this is deliberately narrow.
#[derive(Clone, Debug)]
pub struct CommandCtx {
    /// The agent the TUI's transcript is currently showing.
    pub focused_agent: AgentId,
    /// This session's root agent (`SessionHandle::root`).
    pub root_agent: AgentId,
    /// The CALLING session's own id --.
    /// Read-only identity, the same tier as [`Self::focused_agent`]/
    /// [`Self::root_agent`]: a command cannot use this to reach another
    /// session (there is no live handle on this type at all, for any
    /// session -- see [`Command`]'s own doc), but it is what the HOST
    /// captures, at invocation time, as the one session
    /// [`CommandOutcome::ForkSession`] is ever resolved against, regardless
    /// of which session the host happens to be driving by the time this
    /// command's async `invoke` actually completes (e.g. a `/resume` racing
    /// a slow plugin command). See that variant's own doc for the full
    /// binding argument.
    pub session_id: SessionId,
    /// Everything typed after the command word, left-trimmed, verbatim --
    /// the same "consume the remainder verbatim, no re-tokenization" rule
    /// `conway_cli::tui::commands::parse` applies to every other command's
    /// free-text argument. Empty when the operator supplied none.
    pub args: String,
}

/// What a [`Command::invoke`] call produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Zero or more lines appended to the transcript verbatim, each as its
    /// own entry -- the same shape a built-in command's own successful
    /// notice takes.
    Output(Vec<String>),
    /// The command failed; `message` is shown the same way any other slash
    /// command's failure is (`conway_cli::tui::commands`'s own rule: "a
    /// failing slash command must never terminate the TUI"). Never a panic
    /// bubbling out of `invoke` -- the host also isolates a genuine panic
    /// (see `conway_cli::tui::app`'s dispatch), but an implementation should
    /// still prefer returning this variant over panicking.
    Error(String),
    /// Asks the host to fork the CALLING session at `at_seq` and drive the
    /// resulting child in place of the parent -- the answer to "what must `conway-core`
    /// expose for `/rewind` to be a plugin at all", per the owner's ruling
    /// that session-history features are plugins, not core functionality.
    ///
    /// **Design choice, stated because a handle-based alternative was
    /// weighed and rejected.** The considered alternative was giving
    /// [`Command::invoke`] a `conway-core`-native handle that could fork and
    /// retarget directly (mirroring [`ToolCtx::subagents`]'s
    /// [`SubagentHandle`]). This variant instead asks the HOST to perform
    /// the fork with its own already-live `Conway`/`SessionHandle` -- the
    /// SAME declare/return-an-effect shape [`ContextHook::before_request`]
    /// and `docs/plugins/hooks.md` point 12's "declare, then push" precedent
    /// both already use, chosen because it keeps the plugin declarative,
    /// leaves the host in control of its own focus, and is a strictly
    /// smaller capability to hand out than a live handle would be (a
    /// request the host can refuse or reinterpret, not a capability the
    /// plugin exercises directly).
    ///
    /// **Bound to the invoking session, structurally, not by convention.**
    /// This variant carries NO session identifier of its own -- there is no
    /// field here through which a command could name a session other than
    /// the one it was invoked from. `conway_cli::tui::app::App` (the one
    /// production host) resolves `at_seq`/`directive` against the SAME
    /// [`CommandCtx::session_id`] it captured when it spawned this
    /// invocation's `Command::invoke` call, never against whatever session
    /// it happens to be driving by the time the reply arrives (the two can
    /// legitimately differ -- e.g. an operator typed `/resume` while a slow
    /// plugin command was still running). This is the "a command acts on
    /// its own session, never one it names" property
    ///'s [`SubagentHandle`] precedent
    /// established for tools, applied here to commands the only way it CAN
    /// be applied given this variant carries no live handle at all: by
    /// construction of the type, not by a runtime check that could be
    /// forgotten.
    ///
    /// **What the host actually does with this, disclosed here since this
    /// crate performs none of it:** `Conway::fork_from(session_id, at_seq,
    /// ForkSpec::new(directive))` -- zero-copy by reference
    /// (`SessionStore::fork`'s own O(1) contract), so the PARENT session's
    /// log is untouched; the host then swaps its own driven `SessionHandle`
    /// for the returned child and resubscribes its event stream, the SAME
    /// mechanism `SlashCommand::Resume`'s `Effect::Resumed` already uses for
    /// an unrelated reason (swapping which session the TUI drives).
    ForkSession {
        /// The sequence, within the CALLING session's own local log, to
        /// fork at -- `Conway::fork_from`'s own `at: LogSeq` parameter,
        /// which accepts any point up to and including the session's
        /// current head, not merely "fork at head." An `at_seq` past the
        /// session's actual head is a host-side error (`Conway::fork_from`'s
        /// own bounds check), never a panic here -- this crate performs no
        /// I/O and cannot know the session's head to validate against.
        at_seq: LogSeq,
        /// Becomes the child's `LogRecord::ForkDirective` text -- see
        /// `conway::ForkSpec::directive`'s own doc. Empty is legal (an
        /// undirected rewind: the child simply resumes from `at_seq` with no
        /// additional instruction).
        directive: String,
    },
}

/// One invocable tool: aligned with ACP's tool-call categories (`ToolCategory`
/// in `content.rs`) for free future compatibility, zero present cost.
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// This tool's name, description, JSON Schema, category, and permission
    /// class.
    fn spec(&self) -> ToolSpec;

    /// Invoke the tool.
    ///
    /// PRE: `call.arguments` has already been validated against
    /// `self.spec().schema`. PRE: permission has already been granted for
    /// `(agent, tool, arguments)`. POST: honors `ctx.cancel`; returns within
    /// the runtime's deadline or `Err(ToolError::Cancelled)`. POST: declares
    /// a `TruncationPolicy` on the returned `ToolOutput`; the runtime applies
    /// it and records the truncation in the log (architecture §8).
    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError>;

    /// Renders this proposed call as a single human-readable line: the text
    /// behind `PermissionRequest::rendered` (permission prompt display,
    /// `Event::PermissionRequested`, and any future audit log), and --
    /// for a tool whose rendering is a shell-command-shaped string -- the
    /// text `conway_core::permission_pattern::PatternRule` prefix-matches
    /// against.
    ///
    /// PRE: `args` has already been validated against `self.spec().schema`
    /// by the caller. It is nonetheless UNTRUSTED, model-supplied content:
    /// an implementation MUST NOT panic on any `serde_json::Value`
    /// shape (no `unwrap`/`expect`/indexing into `args`), since a caller
    /// that skips validation, or a future validator bug, must not turn a
    /// bad render into a crash. Callers additionally sanitize the returned
    /// string for control bytes before display -- see
    /// `conway_runtime::tools::runner`'s render seam -- so an implementation
    /// need not do that itself, only avoid panicking.
    ///
    /// The default reproduces this trait's original, pre-per-tool-render
    /// behavior: a generic `name(args)` one-liner. It is correct for any
    /// tool whose call has no natural single command-string representation
    /// (`read`, `edit`, the subagent tools, ...). A tool whose call IS
    /// meaningfully a shell command -- `bash` -- overrides this to return
    /// that bare command string instead, so `PatternRule`'s prefix matching
    /// (designed against a shell command, not a JSON debug dump) has
    /// something legible to operate on.
    fn render(&self, args: &serde_json::Value) -> String {
        format!("{}({})", self.spec().name, args)
    }

    /// Declares which top-level fields of a call's `arguments` (the same
    /// shape `self.spec().schema` validates) carry filesystem paths, for a
    /// later permission-broker root-containment check. **This slice ships
    /// only the declaration** -- no `PermissionCtx`/broker reads this yet.
    ///
    /// PRE (mirrors `render`'s own PRE): a caller MUST NOT rely on
    /// this to be internally consistent with untrusted `args` at runtime --
    /// it is static, call-independent metadata about the tool's *schema*,
    /// not a computation over one call's actual JSON. (A method that
    /// inspected `args` to compute paths would need an extra RPC round trip
    /// for an out-of-process plugin; a field-name list survives the wire
    /// intact — core ships mechanism plus declarative config, and policy
    /// attaches as a plugin.)
    ///
    /// The default is [`PathArgs::Unconfinable`], **not** "no declared
    /// paths": "no paths" defaulting to "nothing to check, therefore allow"
    /// would silently unconfine every tool that doesn't override this
    /// method, including every third-party [`Tool`] impl (a known hazard in
    /// this feature's inventory). `Unconfinable` does not mean "deny" --
    /// see the type's own doc for what it does mean, and why that keeps the
    /// default from being a brick wall.
    fn path_args(&self) -> PathArgs {
        PathArgs::default()
    }

    /// Declares whether [`Self::render`]'s OUTPUT can be interpreted by a
    /// shell, for `conway_core::permission_pattern::PatternRule`'s
    /// metacharacter gate.
    ///
    /// **Orthogonal to [`Self::path_args`].** `path_args` asks "which
    /// arguments are filesystem paths a root-containment check can
    /// confine"; this asks "is `render`'s OUTPUT itself something a shell
    /// would interpret". They diverge for real, shipped tools: `report`
    /// declares `PathArgs::Unconfinable` (its `artifacts[].path` is nested
    /// inside an array, which `PathArgs::Named`'s top-level-only vocabulary
    /// cannot express), but its `render` output is `report`'s own JSON
    /// dump, never handed to a shell -- reusing `path_args` as this gate's
    /// signal would leave `report:*` permanently inert for a reason that
    /// has nothing to do with why the gate exists. `bash` needs both
    /// answered independently too, just the other way: its `command` is
    /// unconfinable (a shell command reaches any path) AND
    /// shell-interpretable (it genuinely IS the string handed to a shell).
    ///
    /// **The default is [`RenderKind::ShellCommand`] -- the conservative
    /// choice, matching the metacharacter gate's behavior before this
    /// method existed** (every `rendered` string was gated, unconditionally,
    /// for every tool). A tool that does not override this method is
    /// exactly as gated as it was before this method existed: its pattern
    /// grants may stay inert if its `render` output happens to contain a
    /// shell metacharacter (as the trait's default JSON-dump `render` does,
    /// via `(`, `)`, `{`, `}`), but that is a missed convenience, never a
    /// missed prompt. Deliberately asymmetric with `path_args`'s
    /// `Unconfinable` default: there, "no declared paths" defaulting to
    /// "allow" was the hazard; here, the hazard runs the other way -- a
    /// tool that overrides [`Self::render`] to emit something
    /// shell-interpretable (as `bash` does) and does NOT also flip this to
    /// `ShellCommand` would silently defeat the chaining gate the moment it
    /// is pattern-matched. Declaring [`RenderKind::Structured`] is
    /// therefore an explicit, deliberate claim a tool author makes about
    /// their own `render` output -- see `conway_tools`' generic test
    /// (`render_kind_is_consistent_with_whether_render_is_overridden` in
    /// `conway-tools/tests/builtins.rs`) for the guard that makes this claim
    /// checkable, not merely aspirational: it can only ever be made
    /// truthfully by a tool that keeps this trait's own default `render`
    /// untouched.
    fn render_kind(&self) -> RenderKind {
        RenderKind::default()
    }
}

/// Declarative metadata: whether a [`Tool`]'s [`Tool::render`] output can be
/// interpreted by a shell, consumed by
/// `conway_core::permission_pattern::PatternRule`'s metacharacter gate to
/// decide whether that gate applies to this tool's pattern grants at all.
/// See [`Tool::render_kind`]'s own doc for the full rationale, including why
/// this is a SEPARATE declaration from [`PathArgs`] rather than a reuse of
/// it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderKind {
    /// `render`'s output is NOT interpreted by a shell -- a debug-shaped
    /// dump (the trait's own default `name(args)`), or any other rendering
    /// that is never handed to a shell for execution. Shell
    /// metacharacters appearing in it (JSON's `{`/`}`, a path's `(`, ...)
    /// are incidental syntax, not command-injection risk, so
    /// `PatternRule`'s metacharacter gate does not apply to this tool's
    /// pattern grants.
    Structured,
    /// `render`'s output IS (or could be) the string a shell interprets --
    /// shell metacharacters within it can genuinely extend the effective
    /// command past whatever prefix a pattern matched. `PatternRule`'s
    /// metacharacter gate MUST apply.
    ShellCommand,
}

impl Default for RenderKind {
    /// Fails closed: see [`Tool::render_kind`]'s own doc for why this is
    /// the conservative choice, not [`RenderKind::Structured`].
    fn default() -> Self {
        RenderKind::ShellCommand
    }
}

/// Declarative metadata: which of a [`Tool`]'s call argument names carry
/// filesystem paths. Consumed (in a later slice) by the permission broker to
/// decide whether a call can be auto-allowed under an operator-configured
/// root, or must always fall through to the gate.
///
/// **`Unconfinable` is the safe default, and it does NOT mean "deny."** It
/// means "this tool's arguments cannot be statically confined to a root, so
/// a root-containment check must always fall through to the operator's
/// gate" -- the same asymmetry `conway_core::permission_pattern`'s
/// metacharacter gate is built on (an unnecessary prompt costs a keystroke;
/// a missed one costs arbitrary execution). A tool that never overrides
/// [`Tool::path_args`] is exactly as gated as it is today -- nothing is
/// silently auto-allowed by adding this trait method.
///
/// `bash` needs BOTH of these facts about itself at once: its `command`
/// string is unconfinable (a shell command can reach any path via
/// redirection, substitution, `cd`, subprocess invocation, ...) AND its
/// optional `cwd` argument, when present, IS a path a root check could
/// usefully confine. `Unconfinable`'s `checkable` field carries that second
/// fact *alongside* the first, in the same variant, rather than needing a
/// second top-level enum variant or a struct-of-two-independent-flags
/// shape — so an enforcing call site never special-cases `bash`: it always
/// asks "is this call unconfinable, and if so, is there anything checkable
/// anyway", one match, no `if name == "bash"`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathArgs {
    /// This tool takes no path arguments at all.
    None,
    /// These top-level argument names (fields of `ToolCall::arguments`)
    /// carry filesystem paths a root-containment check can evaluate.
    Named(&'static [&'static str]),
    /// This tool's call cannot be statically confined to a root (e.g. a
    /// free-form shell command) -- a root-containment check must always
    /// fall through to the operator's gate for this call. `checkable`
    /// names any argument that nonetheless IS a checkable path (e.g.
    /// `bash`'s `cwd`); empty when nothing about the call is checkable.
    Unconfinable { checkable: &'static [&'static str] },
}

impl Default for PathArgs {
    /// Fails closed: see this type's own doc and [`Tool::path_args`]'s.
    fn default() -> Self {
        PathArgs::Unconfinable { checkable: &[] }
    }
}

/// A plugin's static identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub tools: Vec<ToolName>,
    pub required_host_caps: Vec<String>,
}

/// A plugin's untyped configuration values, as loaded and handed down by the
/// facade. This crate does no config loading itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginConfig {
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// A shared, mutable "current working directory" cell -- the capability
/// underlying a future `cd` tool (S1 ships only the capability itself; no
/// built-in tool calls [`Self::set`] yet).
///
/// **Modeled directly on Unix's `getcwd()`/`chdir()` split, not a single
/// mutable variable.** [`Self::current`] is `getcwd()`: a snapshot, never
/// fails, always returns *some* path. [`Self::set`] is `chdir()`: a
/// distinct, separately-fallible operation. [`ToolCtx::cwd`] stays exactly
/// the snapshot `PathBuf` it always was -- this handle is the mutable cell a
/// tool can `chdir()` through, cheaply cloned (an `Arc` refcount bump) into
/// every [`ToolCtx`] that shares it.
///
/// **No per-batch race, by construction of the call site, not this type.**
/// `conway_runtime::tools::runner::ToolRunner::run_batch` reads
/// [`Self::current`] exactly once, before spawning any concurrent tool
/// invocation for that batch, so every tool dispatched together observes the
/// identical snapshot regardless of completion order -- a `set` from
/// another tool in the same batch becomes visible starting the NEXT batch,
/// never the one it ran in. See that function's own doc for why this is
/// deliberate (Unix threads share one process `cwd` under the identical
/// constraint; this mirrors, rather than emulates around, that fact).
///
/// **Internal representation: `Arc<RwLock<PathBuf>>`, not a channel.** A
/// channel would still need a receiver task somewhere applying updates to a
/// value readers can see -- more moving parts for the same
/// single-writer(-at-a-time)/many-readers shape a lock expresses directly.
/// `set`'s critical section is one pointer-sized assignment with no `.await`
/// inside the guard, so the usual "never hold a `std` lock across an await
/// point" hazard does not apply here.
#[derive(Clone, Debug)]
pub struct CwdHandle {
    inner: Arc<RwLock<PathBuf>>,
}

impl CwdHandle {
    /// A fresh cell seeded with `initial`.
    pub fn new(initial: PathBuf) -> Self {
        Self {
            inner: Arc::new(RwLock::new(initial)),
        }
    }

    /// A snapshot of the cell's current value. Mirrors `getcwd()`: never
    /// fails. If some other clone of this handle poisoned the lock (a panic
    /// while holding [`Self::set`]'s write guard), the last successfully
    /// written value is recovered rather than the poison being propagated
    /// A read has no natural failure mode, and inventing one here
    /// would force every caller -- including the runtime's per-batch
    /// snapshot -- to handle an error this operation should never surface.
    pub fn current(&self) -> PathBuf {
        match self.inner.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Sets the cell to `path`, unconditionally. This slice performs no
    /// root/containment check on `path` (see the item's "out of scope":
    /// confinement does not exist yet, and cwd was never the boundary).
    ///
    /// `path` is model-influenced, so untrusted. This never panics on any input --
    /// `path` is stored, never inspected or parsed -- and the one failure
    /// mode this can hit (a poisoned lock) is reported as a typed error
    /// rather than propagated as a panic or silently swallowed.
    pub fn set(&self, path: PathBuf) -> Result<(), CwdError> {
        let mut guard = self.inner.write().map_err(|_| CwdError::Poisoned)?;
        *guard = path;
        Ok(())
    }
}

/// Accepts a plugin-declared event for dispatch to whatever hook an
/// operator has configured for it.
///
/// **A PORT, not a concrete type**, for the identical reason
/// [`crate::ports::HookRunner`] is one: a real implementation performs I/O
/// (spawning a hook's configured command), and this crate performs none.
/// It is the fan-out-layer sibling of `HookRunner` (which invokes ONE hook
/// command) -- `conway_runtime::hook_dispatch::HookDispatcher::dispatch`,
/// the SAME dispatch path every core observation event (`post_tool_use`,
/// `session_starting`, ...) already goes through, implements this trait
/// directly. **This is this item's own "one dispatch path" YAGNI, made
/// structural**: a plugin-declared event is dispatched exactly like
/// `post_tool_use` -- observation-only, fails open (a broken hook is
/// logged and skipped, never propagated) -- never through a second,
/// deny-capable tier this crate would have to invent and justify. Nothing
/// here lets a plugin's own control flow observe whether anything ran.
#[async_trait]
pub trait PluginEventEmitter: Send + Sync + 'static {
    /// Dispatches `name` (an ALREADY-namespaced, ALREADY-validated
    /// `plugin_id.event_name` -- [`PluginEventHandle::emit`] is the only
    /// caller and performs both steps before ever reaching this method)
    /// with `payload` to every hook subscribed to it. Returns nothing:
    /// mirrors `HookDispatcher::dispatch`'s own contract exactly, so a
    /// plugin's behavior never depends on whether an operator happened to
    /// wire a hook to this event.
    async fn emit(&self, name: &str, payload: serde_json::Value);
}

/// A [`PluginEventEmitter`] bound to ONE declaring plugin -- the
/// `ToolCtx`-facing capability a plugin's own [`Tool::invoke`] gets, in
/// place of a raw `Arc<dyn PluginEventEmitter>`. Mirrors the same
/// caller-baking narrowing [`SubagentHandle`] performs for
/// `SubagentHost` and [`CwdHandle`] performs for the cwd cell.
///
/// **Bakes `plugin_id` in structurally.** [`Self::emit`] takes only a BARE
/// event name -- there is no parameter through which a call could name a
/// DIFFERENT plugin's namespace, so one plugin cannot fire an event that
/// looks like it came from another. An operator's hook, once wired to
/// `"acme.routing.candidate_chosen"`, is trusting that name because only
/// the plugin whose manifest id is `acme.routing` can ever produce it --
/// unlike `SubagentHandle`'s `agent_id` (which guards against
/// MODEL-supplied forgery), `plugin_id` here is Rust-code-supplied by the
/// plugin author, never model-influenced; the guarantee this baking
/// provides is for the OPERATOR trusting a hook's provenance, not for
/// resisting a live attacker.
#[derive(Clone)]
pub struct PluginEventHandle {
    emitter: Arc<dyn PluginEventEmitter>,
    plugin_id: String,
}

impl std::fmt::Debug for PluginEventHandle {
    // Manual impl: `Arc<dyn PluginEventEmitter>` carries no `Debug` bound --
    // mirrors `SubagentHandle`'s own manual `Debug` exactly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginEventHandle")
            .field("plugin_id", &self.plugin_id)
            .field("emitter", &"<dyn PluginEventEmitter>")
            .finish()
    }
}

impl PluginEventHandle {
    /// Wraps `emitter`, baking `plugin_id` in as the one namespace every
    /// [`Self::emit`] call uses -- see this type's own doc for why nothing
    /// here lets that identity be overridden per call.
    pub fn new(emitter: Arc<dyn PluginEventEmitter>, plugin_id: impl Into<String>) -> Self {
        Self {
            emitter,
            plugin_id: plugin_id.into(),
        }
    }

    /// A handle that discards every event it is asked to fire -- for a
    /// tool whose `invoke` never calls [`Self::emit`] (every built-in, and
    /// most third-party tools) and every test fixture that constructs a
    /// [`ToolCtx`] without caring about this capability. Mirrors
    /// [`crate::ports::ArtifactWriteHandle::noop`]'s own precedent and
    /// rationale exactly: a no-op implementation performs no I/O either
    /// way, so it carries none of the risk that gates this crate's other
    /// test doubles behind the `fakes` feature, and is reachable from
    /// every crate in the workspace (`conway`'s facade does not forward
    /// `fakes` to its own dependents).
    pub fn noop(plugin_id: impl Into<String>) -> Self {
        Self::new(Arc::new(NoopPluginEventEmitter), plugin_id)
    }

    /// Fires `bare_name` (this plugin's OWN event, never another's -- see
    /// this type's own doc) with `payload`. Assembles the full
    /// `plugin_id.bare_name` and validates it with
    /// [`validate_event_name`] before dispatching -- the only way this can
    /// fail is an empty `bare_name` (`plugin_id` was already validated,
    /// once, at plugin registration: `conway_runtime::hook_dispatch::
    /// declared_plugin_events`), and an invalid full name can never be the
    /// key of any hook subscription an operator could have configured
    /// (the SAME validator gates the subscriber side too -- `crates/
    /// conway/src/config/merge.rs`), so dispatching it would reach no
    /// subscriber regardless. This method therefore drops it silently
    /// rather than panicking or returning an error a caller would have to
    /// remember to check -- matching every other observation-tier failure
    /// posture in this codebase (`HookDispatcher::dispatch`'s own doc).
    pub async fn emit(&self, bare_name: &str, payload: serde_json::Value) {
        let full_name = format!("{}{EVENT_NAMESPACE_SEPARATOR}{bare_name}", self.plugin_id);
        if validate_event_name(&full_name, Some(&self.plugin_id)).is_err() {
            return;
        }
        self.emitter.emit(&full_name, payload).await;
    }
}

/// The private implementation behind [`PluginEventHandle::noop`]. Not
/// itself exported -- mirrors `ports::artifact`'s own private
/// `NoopArtifactWriter` exactly, including why a name that exists purely
/// for tests stays behind a constructor rather than a second top-level
/// name.
struct NoopPluginEventEmitter;

#[async_trait]
impl PluginEventEmitter for NoopPluginEventEmitter {
    async fn emit(&self, _name: &str, _payload: serde_json::Value) {}
}

/// Per-invocation context handed to `Tool::invoke`.
///
/// `Clone` (every field is an `Arc`, `Copy`, or otherwise cheap to clone).
/// **Not** `Serialize` — it holds trait objects (`events`, `subagents`).
/// This is the known T-8 limitation: `ToolCall` and `ToolOutput` are fully
/// serializable, so a future subprocess/RPC plugin transport only needs an
/// RPC-shaped form of `ToolCtx`, not this one.
///
/// **Deliberately NOT `#[non_exhaustive]`, and that is a decision, not an
/// oversight.** Adding a field here is a breaking change for any external
/// struct-literal construction, so the question comes up every time one is
/// added (`plugin_events` is the most recent, `chdir` before it). The
/// reasons to leave it off:
///
/// - `#[non_exhaustive]` would not merely warn about literal construction,
///   it would *forbid* it outside this crate even with every field named,
///   which forces a constructor or builder. That is disproportionate for a
///   type built by hand in dozens of test fixtures.
/// - `ToolSpec` faced the same question and resolved it the same way; the
///   escape hatch there was a defaulted trait method (`Tool::path_args`),
///   which is not available for a capability that must arrive as a field.
/// - This type is already flagged above as due for a wholesale reshape for
///   the T-8 RPC-transport case. Hardening its literal-construction surface
///   now would be work spent on a shape that is expected to change.
///
/// If the project later wants that protection, the move is a builder plus
/// `#[non_exhaustive]` in one deliberate breaking change — not to add the
/// attribute alone.
#[derive(Clone)]
pub struct ToolCtx {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    /// A snapshot of the agent's working directory as of the top of the
    /// batch this call was dispatched in -- see [`CwdHandle`]'s doc for the
    /// snapshot-once-per-batch guarantee. Unchanged by this slice: every
    /// existing consumer (`read`/`write`/`edit`/`glob`/`grep`/`bash`, every
    /// third-party tool) keeps reading this exactly as before.
    pub cwd: PathBuf,
    /// S1: the `cd` capability. Calling `chdir.set(path)` changes the working
    /// directory the NEXT batch snapshots into `cwd` -- never this call's own
    /// `cwd`, and never any other call already dispatched in the same batch
    /// (see [`CwdHandle`]'s doc). No built-in tool calls this yet; it lands on
    /// every [`Tool`] implementation uniformly -- a built-in gets no privileged
    /// API -- so a future `cd` tool needs no privileged access this type
    /// doesn't already expose to every plugin.
    pub chdir: CwdHandle,
    pub cancel: CancellationToken,
    /// Progress reporting; see [`EventSinkHandle`].
    pub events: EventSinkHandle,
    /// The cycle-breaker for the fork/spawn tool: a [`SubagentHandle`] bound
    /// to [`Self::agent_id`] -- the same underlying host the developer API
    /// (`SessionHandle::fork`/`spawn`) calls, narrowed to a caller-bound
    /// handle with no way to act as a different agent (structural --
    /// see [`SubagentHandle`]'s own doc). Was `Arc<dyn SubagentHost>` until
    /// this narrowed it: the same widening `chdir` underwent for the cwd
    /// capability.
    pub subagents: SubagentHandle,
    /// Fires THIS call's own declaring plugin's custom events (///, [`Plugin::events`]) -- a
    /// [`PluginEventHandle`] bound to the plugin that registered the tool
    /// being invoked, so there is no parameter through which a call could
    /// fire under a DIFFERENT plugin's namespace (see that type's own
    /// doc). `conway_runtime::tools::runner` is the one production
    /// construction site and binds this to the resolved tool's own
    /// `plugin_id`; every other construction site (this crate's own
    /// tests, `conway-tools`' fixtures) that does not care about this
    /// capability uses [`PluginEventHandle::noop`].
    pub plugin_events: PluginEventHandle,
    pub config: Arc<PluginConfig>,
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCtx")
            .field("agent_id", &self.agent_id)
            .field("session_id", &self.session_id)
            .field("cwd", &self.cwd)
            .field("chdir", &self.chdir)
            .field("cancel", &self.cancel)
            .field("events", &"<dyn EventSink>")
            .field("subagents", &self.subagents)
            .field("plugin_events", &self.plugin_events)
            .field("config", &self.config)
            .finish()
    }
}

/// The outcome of a tool invocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub blocks: Vec<ContentBlock>,
    pub is_error: bool,
    /// The tool declares how it wants oversized output handled; the runtime
    /// enforces the policy and records the truncation in the log.
    pub truncation: TruncationPolicy,
    pub artifacts: Vec<Artifact>,
}

/// The outgoing request payload a [`ContextHook`] may transform: the
/// assembled prompt segments (in send order, including the `ToolRegistry`
/// segment) and the tool set announced to the model for this turn.
///
/// **Tool announcement vs. execution:** `tools` here is what the
/// model is TOLD it may call -- distinct from [`crate::ports::PermissionGate`], which
/// governs whether a call the model actually makes is allowed to run.
/// Narrowing `tools` hides a tool from the model entirely (it can never
/// propose calling it this turn); `PermissionGate` still gates every
/// proposed call regardless of what was announced. A tool a hook filters
/// out here was never a `PermissionGate` bypass -- it is simply never
/// offered.
#[derive(Clone, Debug, Default)]
pub struct ContextPayload {
    pub segments: Vec<PromptSegment>,
    pub tools: Vec<ToolSpec>,
}

/// Read-only identity/sizing context for one [`ContextHook`] invocation.
/// `estimated_tokens` reflects whatever payload is being transformed by
/// *this* call (the freshly built assembly for [`ContextHook::before_request`],
/// or the still-too-large one for [`ContextHook::on_overflow`]) -- a hook
/// does not need to recompute it itself.
#[derive(Clone, Debug)]
pub struct ContextHookCtx {
    pub agent_id: AgentId,
    /// The root->this-agent chain, including `agent_id` itself -- same
    /// ordering and self-inclusion as `crate::agent::PermissionRequest::
    /// agent_path` (§4.3), populated from the SAME `AgentLoop::agent_path`
    /// field that request is built from, so a consumer sharing one walk
    /// between both ports never observes a divergence. Required, not
    /// defaulted: unlike `PermissionRequest`, this struct has no wire format
    /// to stay backward-compatible with, so there is no serialization
    /// justification for a silent `vec![]` default -- and a hook's entire
    /// reason to want this field is telling a depth-four agent apart from a
    /// depth-one one, which an empty-by-default vector would defeat for any
    /// caller that forgets to plumb it. A root agent's path is
    /// `vec![agent_id]`.
    pub agent_path: Vec<AgentId>,
    pub session_id: SessionId,
    pub turn: u32,
    /// The model this request is routed toward, if known yet. `None` for
    /// `before_request` on an unpinned role (routing hasn't run); `Some` for
    /// `before_request` when `AgentSpec::pin` fixes the model regardless of
    /// routing, and always `Some` for `on_overflow` (a specific route was
    /// already chosen and found to overflow by the time that fires).
    pub model: Option<ModelRef>,
    pub estimated_tokens: u32,
    /// Where this hook may safely
    /// write an artifact file (e.g. a spill-to-file `TruncationPolicy::
    /// Artifact` implementation), if it wants to. See
    /// [`crate::ports::ArtifactWriteHandle`]'s own doc, and
    /// `crate::ports::artifact`'s module doc, for the full containment
    /// guarantee and why this is a write-capable accessor rather than a raw
    /// root or cwd value a hook would have to resolve against itself.
    pub artifacts: ArtifactWriteHandle,
    /// The opaque tag an embedder
    /// attached to this agent at creation time
    /// (`crate::agent::SubagentSpec::tag`), carried through unread by
    /// `conway_runtime`'s `AgentSpec::tag`. A hook may read it to correlate
    /// this call with the caller's own domain object; conway itself never
    /// branches on it -- see `SubagentSpec::tag`'s own doc for the full
    /// "never interpreted" guarantee this field is required to uphold.
    /// `None` for any agent whose spec did not set one (every root agent,
    /// and any fork/spawn child whose caller left `SubagentSpec::tag`
    /// unset).
    ///
    /// Required, not defaulting: same reasoning as [`Self::agent_path`]
    /// -- this struct derives no `Serialize`/
    /// `Deserialize` and has no wire format to stay backward-compatible
    /// with, so there is no serialization justification for a silent
    /// default, and a hook's whole reason to read this field is telling one
    /// agent's tag apart from another's (including apart from "no tag"),
    /// which a defaulted `None` would let a fixture satisfy for free
    /// whether or not the value was actually plumbed.
    pub tag: Option<String>,
}

/// Why [`ContextHook::on_overflow`] fired: the same shortfall accounting as
/// `conway_core::error::RoutingError::ContextTooLarge`, so a hook can decide
/// how much to trim without the runtime recomputing anything the T-1 gate
/// already worked out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverflowInfo {
    pub max_context_tokens: u32,
    pub headroom_tokens: u32,
    /// `estimated_tokens + headroom_tokens`, saturating.
    pub required_tokens: u32,
    /// `required_tokens - max_context_tokens`, saturating.
    pub shortfall_tokens: u32,
}

/// Pluggable per-call context/tool curation (architecture's
/// unifying hook primitive): invoked before every LLM request, with an
/// optional second invocation if the first invocation's output still
/// overflows the routed model's window.
///
/// **No hook registered is the whole contract for "default behavior
/// unchanged":** the runtime holds this as `Option<Arc<dyn ContextHook>>`
/// and never invokes anything when it is `None` -- not even a no-op
/// pass-through call. `conway-core` ships no implementation and no built-in
/// curation policy; every consumer (CLI/IDE/embedder) that wants masking,
/// system-prompt instrumentation, tool-announcement narrowing, or
/// overflow-time compaction supplies its own.
///
/// **One trait, three transforms:** `before_request`'s `ContextPayload`
/// bundles segments and announced tools together because the runtime treats
/// them as one outgoing request -- a hook can edit/drop a segment (e.g. the
/// `AgentDef`-provenance segment, to augment the system prompt; or any
/// segment, to apply an ad hoc exclusion mirroring the persisted
/// `ContextMask`) and/or narrow `tools` (announcement filtering) in the same
/// call. Async so an inference-driven hook can issue its own LLM call to
/// decide (criterion: "hooks may be pure scripts OR issue their own LLM
/// call").
///
/// **Overflow is a distinct, optional method, not a flag on
/// `before_request`:** `on_overflow` only fires when the *already-hooked*
/// payload still doesn't fit the routed model's window (the runtime's T-1
/// gate). Its default returns `None`, which the runtime treats identically
/// to no hook being registered at all: a hard `ContextTooLarge`. This
/// preserves "no hook registered -> today's behavior exactly" as a
/// per-method guarantee, not just a per-trait one -- a consumer can
/// implement curation (`before_request`) without accidentally also
/// suppressing the hard overflow error.
#[async_trait]
pub trait ContextHook: Send + Sync + 'static {
    /// Invoked once per assembled request, before it is routed/sent.
    /// Returning `payload` unchanged is always a valid implementation.
    async fn before_request(&self, ctx: &ContextHookCtx, payload: ContextPayload)
        -> ContextPayload;

    /// Invoked only when `before_request`'s output still exceeds the routed
    /// model's window. `Some(payload)` gives the runtime a smaller/edited
    /// payload to re-estimate and retry -- bounded by the runtime's own
    /// re-assembly loop, never by this trait. `None` (the default) falls
    /// through to the hard `ContextTooLarge` error.
    async fn on_overflow(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
        overflow: OverflowInfo,
    ) -> Option<ContextPayload> {
        let _ = (ctx, payload, overflow);
        None
    }
}

/// A minimal, serialization-free cancellation flag.
///
/// `conway-core` cannot depend on `tokio`, so this is a small
/// `Arc<AtomicBool>`-based token rather than `tokio_util::sync::
/// CancellationToken`. Downstream crates that need an async cancellation
/// *await* (rather than a poll of `is_cancelled`) bridge this token to
/// `tokio_util`'s token themselves — see `conway-runtime`.
///
/// `child()` produces a token that observes both its own cancellation and
/// every ancestor's, to arbitrary depth: internally each child holds a
/// shared handle to its parent (rather than the parent's raw flag alone), so
/// cancelling a root token cancels every descendant transitively.
#[derive(Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
    parent: Option<Arc<CancellationToken>>,
}

impl std::fmt::Debug for CancellationToken {
    // Manual impl: a derived Debug would walk (and print) the entire ancestor
    // chain, which is unbounded in a deep agent tree.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .field("has_parent", &self.parent.is_some())
            .finish()
    }
}

impl CancellationToken {
    /// A fresh, uncancelled, parentless token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks this token cancelled. Every token derived from it via
    /// [`Self::child`] (to any depth) observes this immediately.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// `true` if this token, or any ancestor it was derived from, has been
    /// cancelled. Iterative: walks the ancestor chain without recursion, so
    /// arbitrarily deep agent trees cannot overflow the stack.
    pub fn is_cancelled(&self) -> bool {
        let mut current = self;
        loop {
            if current.flag.load(Ordering::SeqCst) {
                return true;
            }
            match current.parent.as_deref() {
                Some(parent) => current = parent,
                None => return false,
            }
        }
    }

    /// A new token that is independently cancellable but also observes this
    /// token's (and its ancestors') cancellation.
    pub fn child(&self) -> CancellationToken {
        CancellationToken {
            flag: Arc::new(AtomicBool::new(false)),
            parent: Some(Arc::new(self.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancel_is_observed() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn child_observes_parent_cancellation() {
        let parent = CancellationToken::new();
        let child = parent.child();
        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn child_can_be_cancelled_independently_of_parent() {
        let parent = CancellationToken::new();
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn grandchild_observes_root_cancellation() {
        let root = CancellationToken::new();
        let child = root.child();
        let grandchild = child.child();
        assert!(!grandchild.is_cancelled());
        root.cancel();
        assert!(grandchild.is_cancelled());
    }

    #[test]
    fn plugin_manifest_round_trips() {
        let manifest = PluginManifest {
            id: "builtin.fs".into(),
            version: "0.1.0".into(),
            tools: vec![ToolName::new("read"), ToolName::new("write")],
            required_host_caps: vec![],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, back);
    }

    #[test]
    fn tool_output_round_trips() {
        let out = ToolOutput {
            blocks: vec![ContentBlock::Text { text: "ok".into() }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: vec![],
        };
        let json = serde_json::to_string(&out).unwrap();
        let back: ToolOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(out, back);
    }

    /// `ArtifactWriteHandle::noop`
    /// replaces what used to be a hand-rolled private `ArtifactWriter` double
    /// here -- this module's own fixtures are exactly the boilerplate that
    /// constructor exists to remove -- one implementation, reused rather than
    /// restated. The REAL containment guarantee is exercised by
    /// `conway-runtime`'s `artifact_store` tests, against a real
    /// `AgentArtifactWriter` and a real filesystem; this module's own fixtures
    /// exercise `before_request`/`on_overflow` transforms unrelated to artifact
    /// writing.
    fn artifacts_handle() -> crate::ports::ArtifactWriteHandle {
        crate::ports::ArtifactWriteHandle::noop(AgentId::new())
    }

    fn hook_ctx() -> ContextHookCtx {
        let agent_id = AgentId::new();
        ContextHookCtx {
            agent_id,
            agent_path: vec![agent_id],
            session_id: SessionId::new(),
            turn: 0,
            model: Some(ModelRef {
                backend: crate::ids::BackendId::new("anthropic"),
                model: crate::ids::ModelId::new("claude-sonnet-4-6"),
            }),
            estimated_tokens: 100,
            artifacts: artifacts_handle(),
            tag: None,
        }
    }

    fn segment(text: &str) -> PromptSegment {
        PromptSegment::new(
            crate::content::Role::User,
            vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            crate::provenance::Provenance::UserPrompt,
        )
    }

    /// A hook that drops every segment whose text contains "secret" and
    /// otherwise passes the payload through unchanged -- exercises the
    /// mask-like "drop a segment" transform (criterion 1a) without any
    /// dependency on the persisted `ContextMask`.
    struct DropSecretsHook;

    #[async_trait]
    impl ContextHook for DropSecretsHook {
        async fn before_request(
            &self,
            _ctx: &ContextHookCtx,
            mut payload: ContextPayload,
        ) -> ContextPayload {
            payload.segments.retain(|s| {
                !s.content.iter().any(|b| match b {
                    ContentBlock::Text { text } => text.contains("secret"),
                    _ => false,
                })
            });
            payload
        }
    }

    #[test]
    fn before_request_can_drop_a_segment() {
        let hook: Arc<dyn ContextHook> = Arc::new(DropSecretsHook);
        let payload = ContextPayload {
            segments: vec![segment("hello"), segment("the secret plan")],
            tools: vec![],
        };
        let out = block_on(hook.before_request(&hook_ctx(), payload));
        assert_eq!(out.segments.len(), 1);
    }

    /// The default `on_overflow` -- what every hook gets unless it opts in
    /// by overriding it -- must return `None`, which the runtime treats
    /// identically to no hook being registered (hard `ContextTooLarge`).
    struct BeforeRequestOnlyHook;

    #[async_trait]
    impl ContextHook for BeforeRequestOnlyHook {
        async fn before_request(
            &self,
            _ctx: &ContextHookCtx,
            payload: ContextPayload,
        ) -> ContextPayload {
            payload
        }
    }

    #[test]
    fn default_on_overflow_is_none() {
        let hook = BeforeRequestOnlyHook;
        let payload = ContextPayload {
            segments: vec![segment("hi")],
            tools: vec![],
        };
        let overflow = OverflowInfo {
            max_context_tokens: 100,
            headroom_tokens: 10,
            required_tokens: 200,
            shortfall_tokens: 100,
        };
        let out = block_on(hook.on_overflow(&hook_ctx(), payload, overflow));
        assert!(out.is_none());
    }

    /// Dependency-free async-test helper (`conway-core` has no `tokio`/
    /// `futures-executor` dependency, even in dev-deps): every hook exercised
    /// by this module's tests does no real `.await`ing internally, so a
    /// single poll with a no-op waker always resolves `Ready` -- this is not
    /// a general-purpose executor, just enough to drive `async_trait`'s
    /// synchronous-bodied futures to completion in a unit test.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn noop(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);

        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        loop {
            if let Poll::Ready(val) = fut.as_mut().poll(&mut cx) {
                return val;
            }
        }
    }

    /// Object-safety proof (mirrors this module's own `_assert_object_safe`
    /// pattern in `ports/mod.rs`): `RuntimeDeps` needs to hold this as a
    /// trait object.
    #[test]
    fn context_hook_is_object_safe() {
        fn assert_object_safe(_: &dyn ContextHook) {}
        let hook = BeforeRequestOnlyHook;
        assert_object_safe(&hook);
    }

    // ---- Tool::render's default implementation ----

    /// A tool that accepts the trait's default `render` untouched -- proves a
    /// third-party `Tool` implementor (`ConwayBuilder::with_plugin`, the one
    /// extension mechanism) keeps compiling without implementing the new method
    /// (the widening this trait underwent to fix the "pattern grants are inert"
    /// bug).
    struct DefaultRenderTool;

    #[async_trait]
    impl Tool for DefaultRenderTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: crate::ids::ToolName::new("probe"),
                description: "test".into(),
                schema: schemars::schema_for!(serde_json::Value),
                category: crate::content::ToolCategory::Read,
                permission: crate::content::PermissionClass::Safe,
            }
        }

        async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
            unreachable!("not exercised by this test")
        }
    }

    #[test]
    fn default_render_reproduces_the_pre_widening_name_args_shape() {
        let tool = DefaultRenderTool;
        let rendered = tool.render(&serde_json::json!({"a": 1}));
        assert_eq!(rendered, "probe({\"a\":1})");
    }

    /// `Tool` must remain object-safe: `PluginRegistry`/third-party plugin
    /// consumers hold it as `Arc<dyn Tool>`.
    #[test]
    fn tool_is_object_safe() {
        fn assert_object_safe(_: &dyn Tool) {}
        let tool = DefaultRenderTool;
        assert_object_safe(&tool);
    }

    // ---- Tool::path_args' default ----

    /// A third-party `Tool` implementor that accepts the trait's default
    /// `path_args` untouched (same proof shape as
    /// `default_render_reproduces_...` above): the default must be
    /// `Unconfinable` with nothing checkable, never `None` -- "no declared
    /// paths" silently read as "nothing to check, therefore allow" would
    /// unconfine every tool that doesn't override this method.
    #[test]
    fn default_path_args_is_unconfinable_with_nothing_checkable() {
        let tool = DefaultRenderTool;
        assert_eq!(tool.path_args(), PathArgs::Unconfinable { checkable: &[] });
    }

    // ---- Tool::render_kind's default ----

    /// A third-party `Tool` implementor that accepts the trait's default
    /// `render_kind` untouched must get `ShellCommand`, the CONSERVATIVE
    /// choice -- never `Structured`. `Structured` skips the metacharacter
    /// gate entirely; defaulting to it would let a third-party tool that
    /// overrides `render` to emit something shell-interpretable (mirroring
    /// `bash`) silently defeat the chaining gate the moment a pattern is
    /// matched against it, with no explicit opt-in from the tool author.
    #[test]
    fn default_render_kind_is_shell_command_not_structured() {
        let tool = DefaultRenderTool;
        assert_eq!(
            tool.render_kind(),
            RenderKind::ShellCommand,
            "the default must fail closed: an undeclared render_kind must never silently \
             skip the metacharacter gate"
        );
    }

    // ---- CwdHandle (S1: the `cd` capability) ----

    #[test]
    fn fresh_handle_reports_its_seed() {
        let handle = CwdHandle::new(PathBuf::from("/a/b"));
        assert_eq!(handle.current(), PathBuf::from("/a/b"));
    }

    #[test]
    fn set_then_current_round_trips() {
        let handle = CwdHandle::new(PathBuf::from("/a/b"));
        handle.set(PathBuf::from("/c/d")).unwrap();
        assert_eq!(handle.current(), PathBuf::from("/c/d"));
    }

    /// A clone of a `CwdHandle` shares the same underlying cell (`Arc`) --
    /// this is what lets `AgentLoop` construct the cell once and clone it
    /// into every turn's `ToolBatchCtx`/`ToolCtx` and still observe writes
    /// a spawned tool task made through its own clone.
    #[test]
    fn clones_share_the_same_cell() {
        let handle = CwdHandle::new(PathBuf::from("/a"));
        let clone = handle.clone();
        clone.set(PathBuf::from("/b")).unwrap();
        assert_eq!(handle.current(), PathBuf::from("/b"));
    }

    /// Adversarial case: a poisoned lock must not panic. Poisons the
    /// cell by panicking (via `catch_unwind`, so the test process itself
    /// survives) while holding the write guard directly -- reaching the
    /// private `inner` field is legitimate here since this test lives in
    /// the same module that defines it.
    fn poison(handle: &CwdHandle) {
        let inner = handle.inner.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = inner.write().unwrap();
            panic!("deliberately poisoning the lock");
        }));
        assert!(result.is_err(), "the injected panic should have unwound");
    }

    #[test]
    fn set_after_poisoning_returns_a_typed_error_not_a_panic() {
        let handle = CwdHandle::new(PathBuf::from("/a"));
        poison(&handle);
        let result = handle.set(PathBuf::from("/b"));
        assert_eq!(result, Err(CwdError::Poisoned));
    }

    /// `current` recovers rather than propagating the poison (see its own
    /// doc): it must keep returning the last successfully written value,
    /// never panic.
    #[test]
    fn current_after_poisoning_still_returns_the_last_value_without_panicking() {
        let handle = CwdHandle::new(PathBuf::from("/a"));
        handle.set(PathBuf::from("/b")).unwrap();
        poison(&handle);
        assert_eq!(handle.current(), PathBuf::from("/b"));
    }

    /// A weird-but-legal `PathBuf` (non-UTF8 on Unix) must not panic `set`
    /// -- it is stored, never parsed (untrusted: no `unwrap`/`expect` on the
    /// supplied path).
    #[test]
    #[cfg(unix)]
    fn set_accepts_non_utf8_paths_without_panicking() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let handle = CwdHandle::new(PathBuf::from("/a"));
        let weird = PathBuf::from(OsStr::from_bytes(b"/not/\xFFutf8"));
        handle.set(weird.clone()).unwrap();
        assert_eq!(handle.current(), weird);
    }

    // ---- Plugin::commands ----

    /// A third-party `Plugin` implementor that accepts the trait's default
    /// `commands` untouched -- same proof shape as `Tool`'s own default-method
    /// tests above -- must compile and return an empty list: adding this
    /// method must not break any existing `Plugin` implementor in or out of
    /// this workspace.
    struct NoCommandsPlugin;

    impl Plugin for NoCommandsPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test.no_commands".into(),
                version: "0.1.0".into(),
                tools: vec![],
                required_host_caps: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![]
        }
    }

    #[test]
    fn default_commands_is_empty() {
        let plugin = NoCommandsPlugin;
        assert!(plugin.commands().is_empty());
    }

    /// A greeting command whose `invoke` echoes its `args` back -- the
    /// smallest real implementation, used to prove `Command` is genuinely
    /// invocable and object-safe, not merely declarable.
    struct GreetCommand;

    #[async_trait]
    impl Command for GreetCommand {
        fn spec(&self) -> CommandSpec {
            CommandSpec {
                name: "greet".to_string(),
                summary: "echoes back its argument".to_string(),
            }
        }

        async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
            if ctx.args.is_empty() {
                CommandOutcome::Output(vec!["hello!".to_string()])
            } else {
                CommandOutcome::Output(vec![format!("hello, {}!", ctx.args)])
            }
        }
    }

    fn command_ctx(args: &str) -> CommandCtx {
        CommandCtx {
            focused_agent: AgentId::new(),
            root_agent: AgentId::new(),
            session_id: SessionId::new(),
            args: args.to_string(),
        }
    }

    #[test]
    fn command_invoke_reaches_the_implementation() {
        let command = GreetCommand;
        let outcome = block_on(command.invoke(command_ctx("world")));
        assert_eq!(
            outcome,
            CommandOutcome::Output(vec!["hello, world!".to_string()])
        );
    }

    #[test]
    fn command_invoke_handles_empty_args() {
        let command = GreetCommand;
        let outcome = block_on(command.invoke(command_ctx("")));
        assert_eq!(outcome, CommandOutcome::Output(vec!["hello!".to_string()]));
    }

    /// `Command` must remain object-safe: a plugin returns
    /// `Vec<Arc<dyn Command>>`, and the host stores/dispatches through the
    /// trait object.
    #[test]
    fn command_is_object_safe() {
        fn assert_object_safe(_: &dyn Command) {}
        let command = GreetCommand;
        assert_object_safe(&command);
    }

    // ---- CommandOutcome::ForkSession ----

    /// A `/rewind`-shaped fixture command: asks the host to fork at whatever
    /// `at_seq` the operator typed. Deliberately reads NOTHING from
    /// `CommandCtx` other than `args` to build the outcome -- there is no
    /// field on this type it COULD read to name a session other than
    /// `ctx.session_id` itself (see `command_outcome_fork_session_carries_no_
    /// session_field_at_all` below for the structural proof of that).
    struct RewindCommand;

    #[async_trait]
    impl Command for RewindCommand {
        fn spec(&self) -> CommandSpec {
            CommandSpec {
                name: "rewind".to_string(),
                summary: "forks the calling session at a sequence".to_string(),
            }
        }

        async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
            match ctx.args.parse::<u64>() {
                Ok(n) => CommandOutcome::ForkSession {
                    at_seq: LogSeq(n),
                    directive: String::new(),
                },
                Err(_) => CommandOutcome::Error(format!("not a sequence number: {}", ctx.args)),
            }
        }
    }

    #[test]
    fn fork_session_outcome_round_trips_through_invoke() {
        let command = RewindCommand;
        let outcome = block_on(command.invoke(command_ctx("3")));
        assert_eq!(
            outcome,
            CommandOutcome::ForkSession {
                at_seq: LogSeq(3),
                directive: String::new(),
            }
        );
    }

    /// **The discriminating observable this item exists to prove.**
    /// `CommandOutcome::ForkSession` carries no session identifier of its
    /// own -- checkable directly, by destructuring: this pattern binds only
    /// `at_seq`/`directive`, and would fail to COMPILE if a third field
    /// existed for a command to smuggle a foreign session id through. A
    /// command therefore structurally cannot express "act on a session I
    /// was not invoked from" -- there is nowhere in this type to write one
    /// down. The host resolves every `ForkSession` against
    /// [`CommandCtx::session_id`], captured once, at invocation time (see
    /// [`CommandOutcome::ForkSession`]'s own doc); `conway_cli::tui::app`'s
    /// own tests drive the live, end-to-end version of this property
    /// (including under a `/resume` race) against a real `Conway`.
    #[test]
    fn fork_session_outcome_carries_no_session_field_at_all() {
        let outcome = CommandOutcome::ForkSession {
            at_seq: LogSeq(1),
            directive: "hi".to_string(),
        };
        let CommandOutcome::ForkSession { at_seq, directive } = outcome else {
            panic!("constructed a ForkSession, so this arm must match");
        };
        assert_eq!(at_seq, LogSeq(1));
        assert_eq!(directive, "hi");
    }

    /// Two commands invoked with DIFFERENT `CommandCtx::session_id`s each
    /// produce a `ForkSession` outcome scoped only by whatever `ctx` THEY
    /// individually received -- there is no way for one invocation to
    /// influence what session another's outcome resolves against, since
    /// neither outcome carries a session id for either to share.
    #[test]
    fn independently_invoked_commands_each_see_only_their_own_ctx() {
        let command = RewindCommand;
        let ctx_a = CommandCtx {
            focused_agent: AgentId::new(),
            root_agent: AgentId::new(),
            session_id: SessionId::new(),
            args: "1".to_string(),
        };
        let ctx_b = CommandCtx {
            focused_agent: AgentId::new(),
            root_agent: AgentId::new(),
            session_id: SessionId::new(),
            args: "2".to_string(),
        };
        assert_ne!(ctx_a.session_id, ctx_b.session_id);

        let outcome_a = block_on(command.invoke(ctx_a.clone()));
        let outcome_b = block_on(command.invoke(ctx_b.clone()));
        assert_eq!(
            outcome_a,
            CommandOutcome::ForkSession {
                at_seq: LogSeq(1),
                directive: String::new(),
            }
        );
        assert_eq!(
            outcome_b,
            CommandOutcome::ForkSession {
                at_seq: LogSeq(2),
                directive: String::new(),
            }
        );
    }

    /// A `Plugin` implementor that DOES declare a command -- proves
    /// `commands()` round-trips a real `Arc<dyn Command>` through the trait,
    /// the shape every consumer (the TUI's registry, a future embedder)
    /// depends on.
    struct GreetPlugin;

    impl Plugin for GreetPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test.greet".into(),
                version: "0.1.0".into(),
                tools: vec![],
                required_host_caps: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![]
        }

        fn commands(&self) -> Vec<Arc<dyn Command>> {
            vec![Arc::new(GreetCommand)]
        }
    }

    #[test]
    fn plugin_commands_carries_a_real_declared_command() {
        let plugin = GreetPlugin;
        let commands = plugin.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].spec().name, "greet");
    }

    // ---- Plugin::events ----

    /// Same proof shape as `default_commands_is_empty` immediately above:
    /// a third-party `Plugin` implementor that accepts the trait's default
    /// `events` untouched must compile and return an empty list.
    #[test]
    fn default_events_is_empty() {
        let plugin = NoCommandsPlugin;
        assert!(plugin.events().is_empty());
    }

    /// A `Plugin` implementor that DOES declare an event -- proves
    /// `events()` round-trips a real `EventDecl` through the trait, the
    /// same way `plugin_commands_carries_a_real_declared_command` proves
    /// it for `commands()`.
    struct EventDeclaringPlugin;

    impl Plugin for EventDeclaringPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test.events".into(),
                version: "0.1.0".into(),
                tools: vec![],
                required_host_caps: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![]
        }

        fn events(&self) -> Vec<EventDecl> {
            vec![EventDecl {
                name: "pong_dispatched".to_string(),
                summary: "fires once per skeleton_ping call".to_string(),
                carries_tool_name: false,
            }]
        }
    }

    #[test]
    fn plugin_events_carries_a_real_declared_event() {
        let plugin = EventDeclaringPlugin;
        let events = plugin.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "pong_dispatched");
        assert!(!events[0].carries_tool_name);
    }

    // ---- PluginEventEmitter / PluginEventHandle ----

    /// Records every `(name, payload)` it was asked to dispatch.
    #[derive(Default)]
    struct RecordingEmitter {
        seen: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    }

    #[async_trait]
    impl PluginEventEmitter for RecordingEmitter {
        async fn emit(&self, name: &str, payload: serde_json::Value) {
            self.seen
                .lock()
                .expect("seen lock poisoned")
                .push((name.to_string(), payload));
        }
    }

    /// The load-bearing property: firing a bare name reaches the emitter
    /// with the FULL `plugin_id.bare_name` -- the namespaced form an
    /// operator's `[hooks].rules[].event` actually subscribes to.
    #[test]
    fn emit_assembles_the_full_namespaced_name() {
        let emitter = Arc::new(RecordingEmitter::default());
        let handle = PluginEventHandle::new(emitter.clone(), "acme_routing");

        block_on(handle.emit("candidate_chosen", serde_json::json!({"model": "x"})));

        let seen = emitter.seen.lock().expect("seen lock poisoned").clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "acme_routing.candidate_chosen");
        assert_eq!(seen[0].1["model"], "x");
    }

    /// A `plugin_id` containing the namespace separator (every real
    /// built-in plugin id in this workspace, e.g. `conway.plugin_skeleton`)
    /// emits exactly like any other -- see `validate_event_name`'s own doc
    /// ("§16.6 point 3 is reconsidered here") for why this is a deliberate
    /// reversal of an earlier draft, not an oversight.
    #[test]
    fn a_plugin_id_containing_the_separator_emits_normally() {
        let emitter = Arc::new(RecordingEmitter::default());
        let handle = PluginEventHandle::new(emitter.clone(), "acme.routing");

        block_on(handle.emit("candidate_chosen", serde_json::json!(null)));

        let seen = emitter.seen.lock().expect("seen lock poisoned").clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "acme.routing.candidate_chosen");
    }

    /// Structural namespace guarantee: `emit`'s only parameters are the
    /// BARE name and the payload -- there is no argument through which a
    /// caller could name a different plugin's namespace. This test drives
    /// that through two independently constructed handles and shows each
    /// can only ever produce names under its OWN baked-in `plugin_id`.
    #[test]
    fn a_handle_can_never_fire_under_a_different_plugins_namespace() {
        let emitter = Arc::new(RecordingEmitter::default());
        let acme = PluginEventHandle::new(emitter.clone(), "acme");
        let other = PluginEventHandle::new(emitter.clone(), "other");

        block_on(acme.emit("thing_happened", serde_json::json!(null)));
        block_on(other.emit("thing_happened", serde_json::json!(null)));

        let seen: Vec<String> = emitter
            .seen
            .lock()
            .expect("seen lock poisoned")
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        assert_eq!(seen, vec!["acme.thing_happened", "other.thing_happened"]);
    }

    /// An empty bare name cannot assemble into anything a subscriber could
    /// ever be configured against -- dropped silently, never dispatched,
    /// never a panic.
    #[test]
    fn emit_with_an_empty_bare_name_is_silently_dropped() {
        let emitter = Arc::new(RecordingEmitter::default());
        let handle = PluginEventHandle::new(emitter.clone(), "acme");

        block_on(handle.emit("", serde_json::json!(null)));

        assert!(emitter.seen.lock().expect("seen lock poisoned").is_empty());
    }

    /// [`PluginEventHandle::noop`] discards every event -- the default a
    /// tool that never calls `emit`, and every test fixture that does not
    /// care about this capability, gets.
    #[test]
    fn noop_handle_discards_every_event() {
        let handle = PluginEventHandle::noop("acme");
        // Nothing to assert against a recorder (there is none) -- the
        // property under test is that this does not panic and completes.
        block_on(handle.emit("anything", serde_json::json!({"a": 1})));
    }

    /// `PluginEventEmitter` must remain object-safe: `PluginEventHandle`
    /// holds it as `Arc<dyn PluginEventEmitter>`.
    #[test]
    fn plugin_event_emitter_is_object_safe() {
        fn assert_object_safe(_: &dyn PluginEventEmitter) {}
        let emitter = RecordingEmitter::default();
        assert_object_safe(&emitter);
    }
}
