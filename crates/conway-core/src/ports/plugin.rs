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
use crate::ports::{
    ArtifactWriteHandle, ContextPathHandle, EventSink, EventSinkHandle, SessionDiscoveryHandle,
    SubagentHandle, SubagentHost,
};
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

    /// Zero or more instruction fragments this plugin declares -- the
    /// mechanism board item `01M0K5MD59YZRSHE31JKZKFRMY` gives decision
    /// `01M0K4S2S1NBW63KNF1NEY5XT3`'s "conway's idiom ships as text a model
    /// reads" claim: BEFORE this method, a plugin could only put a
    /// paragraph into a context by mutating the assembled request from
    /// inside [`ContextHook::before_request`] (`conway.skills`'s own
    /// `SkillIndexHook` does exactly this, to narrow -- not author -- a
    /// segment). That is *expressible* but not *legible*: the text lives
    /// in Rust, not a file; there is no way to ask what instruction conway
    /// is running with short of reading every hook; nothing states which
    /// paragraph outranks which when several disagree; and nothing
    /// connects a paragraph to the tool calls it assumes the model can
    /// make. This method exists so the declaration is DATA a host can
    /// inspect, order, and check -- not a second, hook-shaped injection
    /// path alongside the one that already exists.
    ///
    /// **What "alongside `tools`" buys structurally, not by convention.**
    /// A fragment naming a `tool_id` this SAME plugin also returns from
    /// [`Self::tools`] can never fail the reachability check
    /// (`conway_runtime::context::builder::ContextBuilder::build`'s own
    /// "Plugin instruction fragments" section performs it): both are
    /// contributed by the same `Arc<dyn Plugin>`, installed through the
    /// same `with_plugin`/`install_selected` call, so they ship and leave
    /// together by construction -- reachability for THAT case needs no
    /// runtime check at all, only the fact that this method sits on the
    /// same trait as `tools`. A fragment naming a tool_id belonging to a
    /// DIFFERENT plugin, or to no installed plugin, is the genuinely
    /// checkable case -- see below.
    ///
    /// **The default returns none**, the SAME zero-cost-default precedent
    /// [`Self::commands`]/[`Self::events`]/[`Self::observers`] establish
    /// above: every existing `Plugin` implementor keeps compiling
    /// unmodified, and a build with no instruction-declaring plugin
    /// injects no new segment and excludes nothing.
    ///
    /// **The reachability check runs at context-assembly time, not at
    /// `ConwayBuilder::build` and not in CI** -- deliberately, per the
    /// operator's own CLI ruling (`01M0K5K8DCRVR523P54DZF4BY3`). A
    /// fragment can name a tool that exists somewhere in this repository
    /// but is not among `ContextInput.tools` for THIS session's THIS
    /// turn (e.g. an operator installed the fragment's plugin but not the
    /// plugin providing the tool it assumes) -- a fact no static grep over
    /// source can see, because it depends on what `plugins.install`
    /// resolved to for this one operator's config. An unreachable
    /// fragment's text is WITHHELD from every agent's assembled context
    /// (never sent, so the model can never try a tool that is not there
    /// and fail silently, forever) and recorded in
    /// [`crate::provenance::ContextReport::instruction_fragments`]
    /// with the missing tool ids named, so `/context`'s preamble section
    /// renders the omission inline rather than only warning once in a log
    /// line that scrolls away.
    ///
    /// **Precedence.** `ContextBuilder::build` injects a plugin's
    /// instruction fragments as their own `[1] PluginInstructions*` step,
    /// positioned AFTER `[0] SystemPrompt` (the agent definition's own
    /// base idiom) and BEFORE `[1b] SkillFragments*` (the operator's own,
    /// directory-authored skills, `AgentDef.skills`) -- base, then
    /// capability-declared, then operator-authored-last, matching this
    /// item's own illustrative `/context` rendering
    /// (`conway.idiom` "base" -> `conway.trim`/`conway.memory`
    /// plugin-sourced -> `house-style` "(yours)"). Multiple plugins'
    /// fragments are injected in `with_plugin`/`install_selected` install
    /// order -- the SAME "the seam owns precedence, not its call sites"
    /// composition shape [`Self::context_hooks`]/[`Self::curators`]
    /// already establish above, applied here to context CONTENT rather
    /// than to a hook or curator.
    ///
    /// **Convention, not enforcement: text lives in a markdown file.**
    /// Nothing in this trait forces a plugin to source `text` from a file
    /// rather than a Rust string literal -- `String` cannot tell the
    /// difference. The convention is `include_str!("../fragments/foo.md")`
    /// (a file in the plugin's own crate, read at compile time) or, for a
    /// plugin distributed as data alongside a compiled binary, a genuine
    /// file read at construction time. See this crate's own doc for the
    /// argued tradeoff between the two: `include_str!` has no file an
    /// operator can delete to disable ONE fragment without uninstalling
    /// the whole plugin (`01M0K5K8DCRVR523P54DZF4BY3`'s own open
    /// question); a files-beside-the-plugin convention keeps every
    /// fragment removable with no settings UI at all, which is why it is
    /// the recommended shape even though `include_str!` remains legal for
    /// a plugin that ships as a single compiled artifact with nothing else
    /// to distribute.
    ///
    /// **Not `conway.skills`, and not folded into it without arguing so.**
    /// A skill (`crate::config::SkillDef`, loaded by
    /// `conway::skills::load_skill_defs` from an operator-maintained
    /// directory) OUTLIVES any plugin -- it is the operator's own file,
    /// selected by name in `AgentDef.skills`, with no `Plugin` in its
    /// authorship chain at all. An instruction fragment does not outlive
    /// its plugin: it ships and leaves with `with_plugin`, by
    /// construction, which is the property this whole method exists to
    /// make structural. They render through the SAME machinery
    /// (`conway_runtime::context::builder::SkillFragment`,
    /// `Provenance::Skill`) once resolved, because both are, at that
    /// point, "a named text fragment injected into context" -- but the
    /// SOURCING differs (capability-authored vs. operator-authored) and so
    /// does the LIFETIME (bound to a plugin vs. bound to a file an
    /// operator manages directly), which is why this is a distinct
    /// contribution method rather than a widened `Self::commands`-shaped
    /// reuse of skills' own directory-loading path.
    fn instructions(&self) -> Vec<InstructionFragment> {
        Vec::new()
    }

    /// An operator-facing description of this plugin -- what a plugin
    /// browser (board item `01M0KARX71A64NTSYTDBVANVPF`) shows next to a
    /// toggle, so someone deciding whether to turn a plugin on or off can
    /// see what changes without reading source. **A different audience
    /// from [`Self::instructions`]:** that method ships text for the
    /// MODEL (injected into context, read by the agent); this one ships
    /// text for the PERSON running conway, read at `ConwayBuilder::build`-adjacent time by a
    /// TUI/CLI surface, never assembled into a prompt.
    ///
    /// **Why a trait method with a zero-cost default, not a field on
    /// [`PluginManifest`] or an addition to [`InstructionFragment`] --
    /// argued, not assumed, since the item that added `instructions()`
    /// deliberately left this choice open:**
    ///
    /// - **Not a new `PluginManifest` field.** `PluginManifest` is
    ///   constructed as a plain struct literal (no `Default` impl) at
    ///   three dozen call sites across this workspace -- every first-party
    ///   plugin crate, every fixture `Plugin` a test defines, every fake in
    ///   `conway-runtime`/`conway-tools`'s own test suites. A required
    ///   field there would force every one of those (most of which have no
    ///   operator-facing browser to describe themselves for at all -- a
    ///   skeleton, a hang-detector fixture, a panic-isolation probe) to
    ///   invent placeholder description text just to keep compiling. A
    ///   trait method with a default -- the SAME zero-cost-default
    ///   precedent [`Self::commands`]/[`Self::events`]/
    ///   [`Self::observers`]/[`Self::instructions`] itself all establish
    ///   above -- costs those call sites nothing: every existing `Plugin`
    ///   implementor keeps compiling unmodified, and only the six
    ///   plugins a real browser actually lists override it.
    /// - **Not an addition to [`InstructionFragment`].** Cardinality
    ///   differs: a plugin has exactly ONE description (matching
    ///   [`PluginManifest`]'s own one-per-plugin identity), but declares
    ///   ZERO OR MANY instruction fragments -- `conway.skeleton`,
    ///   `conway.stepguard`, and `conway.trim` all ship zero fragments
    ///   today, yet every one of them still has something to say to an
    ///   operator deciding whether to turn it on. Bolting a description
    ///   onto a per-fragment type would leave a fragment-less plugin with
    ///   no operator-facing text at all, or force it to declare a fragment
    ///   solely to carry a description the model was never meant to read
    ///   -- the wrong mechanism wearing the right method's clothes.
    ///
    /// **Where the text lives: a Rust literal, not a markdown file --
    /// deliberately the opposite of [`Self::instructions`]'s own
    /// convention.** That convention exists so an operator can delete ONE
    /// fragment's file to disable a MODEL-facing behavior with no
    /// recompile (see that method's own doc, "Convention, not
    /// enforcement"). A description has no equivalent removability need:
    /// it does not change what the model does, and there is no
    /// "keep the plugin, lose only its description" state anyone would
    /// want -- the description IS the plugin's own identity blurb,
    /// exactly as fixed-at-compile-time as [`PluginManifest::id`]/
    /// [`PluginManifest::version`] already are. Applying the file
    /// convention here would buy operator control over nothing.
    ///
    /// The default returns [`PluginDescription::default`] (every field
    /// empty) -- a browser renders an empty summary/you-get/you-lose/costs
    /// honestly (e.g. "(no description)"), never a placeholder that
    /// invents a claim this plugin never made.
    fn description(&self) -> PluginDescription {
        PluginDescription::default()
    }

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

    /// Zero or more per-agent [`PluginConfig`] keys this plugin allows to
    /// vary per agent, narrowing-only down the fork/spawn tree -- the
    /// general mechanism `[S1.5]`'s per-agent-plugin-configuration item
    /// introduces (`conway.fs`'s own root, via `conway-tools`' `FsPlugin`
    /// implementation, is its proving consumer).
    ///
    /// **"Narrowing" is undefined for an arbitrary JSON value, so a plugin
    /// declares which of its keys are narrowable and supplies the
    /// comparison itself** ([`NarrowingRule::narrows`]) -- this trait has no
    /// way to know, in general, whether one plugin-specific value is
    /// "inside" another; only the plugin that gives the value meaning can
    /// say. The host prefixes each [`NarrowingRule::key`] with THIS
    /// plugin's own [`PluginManifest::id`] before it is ever reachable in a
    /// caller's [`crate::agent::SubagentSpec::plugin_config`] map, the SAME
    /// "an author never picks their own namespace" rule [`Self::events`]/
    /// [`Self::commands`] already establish for event/command names.
    ///
    /// **The default returns none, the same zero-cost-default precedent
    /// [`Self::commands`]/[`Self::events`]/[`Self::observers`] established
    /// above: every existing `Plugin` implementor keeps compiling
    /// unmodified, and a plugin that declares nothing narrowable keeps
    /// today's global-only configuration semantics exactly** -- an attempt
    /// to set any of its keys per-agent is rejected
    /// ([`PluginConfigError::NotNarrowable`]) rather than silently
    /// accepted, so "declare nothing" is the correct, zero-tax default
    /// rather than an opt-out a plugin author has to remember to write.
    fn narrowable_keys(&self) -> Vec<NarrowingRule> {
        Vec::new()
    }

    /// Zero or more [`ContextHook`]s this plugin installs -- per-request
    /// context curation (segment edit/drop/replace, tool-announcement
    /// narrowing) that runs before every LLM request. The default returns
    /// none, the SAME zero-cost-default precedent [`Self::commands`]/
    /// [`Self::events`]/[`Self::observers`]/[`Self::narrowable_keys`]
    /// established above: every existing `Plugin` implementor keeps
    /// compiling unmodified, and a build with no curating plugin installed
    /// leaves the runtime's context hook unset -- byte-identical to never
    /// calling `ConwayBuilder::with_context_hook` at all.
    ///
    /// **Why this is a `Plugin` method rather than only a `ConwayBuilder`
    /// setter.** `ConwayBuilder::with_context_hook` remains the lower-level
    /// surface -- an embedder with a standalone hook and no plugin still
    /// uses it directly. But before this method, a plugin-contributed tool
    /// ([`Self::tools`]) had no way to ALSO contribute the context curation
    /// that tool's value proposition often depends on: progressive skill
    /// disclosure, for instance, is a `ContextHook` that narrows a
    /// `Provenance::Skill` segment to a one-line index PLUS a `read_skill`
    /// tool that fetches the full body on demand, and the two only make
    /// sense installed together. Requiring a plugin author to also reach
    /// for a separate builder setter their consumer might forget to call
    /// would have been exactly the kind of privileged channel GP-03 rules
    /// out -- a first-party plugin needing a hook a third party cannot
    /// reach through the same `with_plugin` surface. This method closes
    /// that gap: a plugin's hooks install through the SAME `with_plugin`/
    /// `install_selected` surface its tools do, and `ConwayBuilder::build`
    /// composes every installed plugin's hooks (plus any
    /// `with_context_hook`-injected one) into the single
    /// `Runtime::set_context_hook` call the runtime actually reads.
    ///
    /// **Composition, stated for the multi-plugin case.** The runtime holds
    /// one context hook; the builder therefore chains multiple contributed
    /// hooks in installation order (an injected `with_context_hook` hook
    /// first, then each plugin's hooks in `with_plugin`/`install_selected`
    /// order), feeding each hook's returned payload to the next. This is
    /// the same "every downstream consumer sees the post-hook payload"
    /// property `agent_loop`'s own hook call site already guarantees for a
    /// single hook, extended to a chain by construction. A hook that wants
    /// to opt out of chaining narrows only its own segments and leaves the
    /// rest untouched, which composes cleanly with a sibling that narrows
    /// different segments.
    fn context_hooks(&self) -> Vec<Arc<dyn ContextHook>> {
        Vec::new()
    }

    /// Zero or more selection-layer [`Curator`](crate::ports::Curator)s this
    /// plugin installs -- the curation capability (DESIGN-context-path
    /// §11.3, §11.4). A curator runs BEFORE assembly, operating on the
    /// resolved [`ValidatedPath`](crate::path::ValidatedPath), and returns a
    /// validated [`Derivation`](crate::path::Derivation) (or
    /// `Unchanged`/`Failed`); see [`CurateOutcome`](crate::ports::CurateOutcome).
    /// This is the SEAM a cross-tree memory curator (Unit 3) plugs into, and
    /// the capability that turns the path mechanism from a data model into
    /// something any plugin can use.
    ///
    /// **Why a separate port, not another `ContextHook`.** `ContextHook`
    /// runs AFTER assembly and sees rendered `Vec<PromptSegment>`; a curator
    /// runs BEFORE assembly and sees records. Every advantage the path model
    /// claims (byte-identical records, knowable cache cost, refusal instead
    /// of silent repair, structural predicates) lives at the selection
    /// layer, not the segment layer -- see `crate::ports::curator`'s module
    /// doc for the full §11.3 comparison table.
    ///
    /// **GP-03 -- same surface as tools/hooks.** Same argument as
    /// [`Self::context_hooks`] in spirit: a plugin-contributed tool whose
    /// value depends on curation installs the curator through the SAME
    /// `with_plugin`/`install_selected` surface its tools use, and
    /// `ConwayBuilder::build` composes every installed plugin's curators
    /// (plus any `with_curator`-injected one) into the single
    /// `Runtime::set_context_curator` call the runtime reads. No privileged
    /// first-party channel -- a third-party plugin reaches this exact same
    /// surface. The default returns none, so every existing `Plugin`
    /// implementor -- built-in, first-party, third party -- keeps compiling
    /// unmodified, and a build with no curating plugin leaves the runtime's
    /// curator unset, byte-identical to never installing one at all (the
    /// zero-cost pass-through the stage guarantees when
    /// `context_curator` is `None`).
    fn curators(&self) -> Vec<Arc<dyn crate::ports::Curator>> {
        Vec::new()
    }

    /// Zero or more NARROWING permission rules this plugin contributes --
    /// the in-process / host-side projection of a plugin's declared
    /// permission policy (the wire form is `permission.policy/1`, board item
    /// `01M03VKJG7JJ0JEKY265WA7MJ7`; see `docs/plugins/hooks.md` point 8).
    /// The default returns none, the SAME zero-cost-default precedent
    /// [`Self::commands`]/[`Self::events`]/[`Self::observers`]/
    /// [`Self::narrowable_keys`]/[`Self::context_hooks`] established above:
    /// every existing `Plugin` implementor keeps compiling unmodified, and a
    /// build with no policy-contributing plugin installs nothing.
    ///
    /// **Narrowing-only, by type construction.** [`PluginPermissionVerdict`]
    /// has no `Allow` variant -- a plugin may `Deny` a call outright, force
    /// it to the operator's gate with `Prompt`, or `Abstain` (no opinion).
    /// It can never WIDEN what the operator authorized. This is the
    /// narrowing-only (no `Allow`) shape `docs/plugins/hooks.md` point 8's own
    /// `NarrowingPolicy` prescribes, extended with `Prompt` for this spec's
    /// `Dangerous`->prompt mapping -- `NarrowingPolicy` itself is `Deny`|`Abstain`
    /// (no `Prompt`); the full `NarrowingPolicy`/`DecidingPolicy` per-call
    /// inference chain remains design-only (see hooks.md point 8). "May only
    /// narrow" is a property of the return type a plugin cannot talk its way
    /// around, not a runtime flag the broker has to remember to check. The
    /// broker installs `Deny`/`Prompt`
    /// rules as `PatternOrigin::Plugin` deny/prompt rules (the SAME
    /// admission `remember_deny_rule`/`remember_prompt_rule` already
    /// provide); `Abstain` installs nothing.
    ///
    /// **Subordination to the operator -- the load-bearing boundary.** The
    /// operator's own `permissions.json`/`PermissionMode` STILL wins over a
    /// plugin-contributed rule: a plugin `Deny` is checked at the SAME tier
    /// as an operator `Deny` (most-restrictive-wins, before every allow
    /// path); a plugin `Prompt` forces the gate, but the operator's `Deny`
    /// and plan-mode denial fire FIRST and outrank it; and there is no
    /// plugin `Allow` to widen anything the operator denied. A plugin
    /// declaring `Abstain` for a tool the operator independently marked
    /// dangerous (an operator `Deny` rule, or a `Plan`-mode category
    /// refusal) leaves the operator's decision standing -- the wire policy
    /// cannot widen. `crates/conway-runtime/src/permission.rs`'s
    /// `plugin_permission_subordination_*` tests pin this.
    ///
    /// `tool` is matched as an exact tool name (the SAME
    /// [`crate::permission_pattern::Select::Tools`] exact-match a flat
    /// `tool:*` rule uses), scoped to THIS plugin's own declared tools in
    /// practice -- the host does not enforce that scoping here (a rule
    /// naming a tool no installed plugin declares simply never matches),
    /// but a plugin authoring a rule for a tool it does not own is a
    /// authoring bug, not a security boundary.
    fn permission_rules(&self) -> Vec<PluginPermissionRule> {
        Vec::new()
    }

    /// An [`EventSinkHandle`] the host fans the runtime's live `Event`
    /// stream onto so this plugin can OBSERVE host events over its session --
    /// the host-side half of the `observe/1` wire point (board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`, see `docs/plugins/hooks.md` point 11).
    /// `None` (the default) for every `Plugin` implementor that does not
    /// speak `observe/1`: an in-process plugin, a one-shot subprocess plugin,
    /// or a persistent subprocess plugin that did not declare the point at a
    /// supported version. The SAME zero-cost-default precedent
    /// [`Self::permission_rules`] establishes above -- every existing
    /// implementor keeps compiling unmodified, and a build with no observing
    /// plugin spawns no forwarding task.
    ///
    /// **One-way, lossy-with-notice, observer-class.** The sink the plugin
    /// returns receives `Event`s the host fan-outs from the runtime's
    /// `EventBus`; the host's forwarding task is a SUBSCRIBER of that bus, so
    /// a slow plugin falls behind the bus's broadcast buffer and sees
    /// `Event::Lagged` rather than stalling any producer -- the identical
    /// lossy-with-notice discipline `conway::EventStream`
    /// (`crates/conway/src/event_stream.rs`) already guarantees an embedder's
    /// own stream, mirrored here for a plugin. The sink itself pushes to a
    /// bounded queue and drops+warns on overflow, so the host turn NEVER
    /// blocks on a slow plugin read loop. There is no reply channel: an
    /// observer changes nothing by construction
    /// (`docs/plugins/compatibility.md`'s "Observers vs participants"), so an
    /// unknown `Event` tag the plugin receives is IGNORED (the one
    /// enum-versioning case where "ignore" is the right answer), and a plugin
    /// that never reads its stdin notifications cannot fail the run -- the
    /// worst it can do is fall behind and see `Event::Lagged`.
    ///
    /// **Advertising a point means the host speaks it, not that the host
    /// requires it.** A plugin that does not declare `observe/1` in its
    /// `initialize/1` answer loads normally and contributes no observe sink;
    /// a plugin that declares it at an UNSUPPORTED version DEGRADES (the host
    /// surfaces a `tracing::warn!` naming both versions and loads the plugin
    /// WITHOUT that point) -- the observer rule, the OPPOSITE of the
    /// participant refusal `permission.policy/1` uses. See
    /// `crates/conway-plugin-subprocess`'s `session` module for the
    /// version-negotiation behavior.
    fn observe_sink(&self) -> Option<EventSinkHandle> {
        None
    }

    /// Zero or more status contributions this plugin is CURRENTLY pushing --
    /// the host-side half of the `status.declare/1` / `status/1` wire point
    /// (board item `01M03VKQ738DTGHHK2C4RWXC0E`, see `docs/plugins/hooks.md`
    /// point 12). The default returns none, the SAME zero-cost-default
    /// precedent [`Self::permission_rules`]/[`Self::observe_sink`] establish
    /// above: every existing `Plugin` implementor keeps compiling unmodified,
    /// and a build with no status-declaring plugin surfaces nothing.
    ///
    /// **A polled snapshot of an asynchronous push.** A persistent subprocess
    /// plugin pushes `status/1` notifications as inbound no-`id` NDJSON lines
    /// on its stdout; the host's reader routes those to a bounded notification
    /// channel (drop+warn on overflow, never blocks the host turn) and stores
    /// the latest contribution per `key` on the session. This method returns a
    /// POINT-IN-TIME snapshot of that store -- it is NOT a build-time
    /// declaration. An unknown `crate::agent::ResultStatus` tag the plugin
    /// pushes degrades to `crate::agent::ResultStatus::Failed` (the
    /// compatibility table's `ResultStatus` row), never `Completed`; a missing
    /// or structurally-invalid notification is dropped with a `tracing::warn!`
    /// (observer-class, degrade -- never fails the session).
    ///
    /// **What is built vs design-only.** The WIRE half is built: the
    /// notification channel, the parser, the degrade-on-unknown-tag rule, the
    /// per-key store, and this trait surface. The TUI status-line RENDER path
    /// that would display a plugin's contributed status alongside conway's own
    /// computed state remains DESIGN-ONLY (see `docs/plugins/hooks.md` point
    /// 12's own "Status" row); this method is the surface a future render path
    /// will read, exposed now so the wire half has a reachable consumer.
    fn status_contributions(&self) -> Vec<PluginStatusContribution> {
        Vec::new()
    }
}

/// One status contribution a plugin pushes via `status/1` notifications --
/// the host-side projection of a `{ key, status, value }` wire line (board
/// item `01M03VKQ738DTGHHK2C4RWXC0E`). See `Plugin::status_contributions`'s
/// own doc for the polled-snapshot / lossy-with-notice discipline and the
/// degrade-on-unknown-tag rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStatusContribution {
    /// The plugin-authored key this contribution is filed under (e.g.
    /// `"build"`). The host stores the LATEST contribution per key -- a later
    /// `status/1` line for the same `key` overwrites an earlier one, matching
    /// `docs/plugins/hooks.md` point 12's "a stale value expires at snapshot
    /// time" shape (the ttl/expiry render path itself stays design-only).
    pub key: String,
    /// The status the plugin pushed, with an unknown wire tag already degraded
    /// to `crate::agent::ResultStatus::Failed` at parse time (the
    /// compatibility table's `ResultStatus` row, never `Completed`). A plugin
    /// pushing `"completed"` surfaces `crate::agent::ResultStatus::Completed`;
    /// a plugin pushing an unknown tag surfaces
    /// `crate::agent::ResultStatus::Failed` carrying the unknown tag in its
    /// `error` string, so the degradation is auditable.
    pub status: crate::agent::ResultStatus,
    /// The plugin-authored free-text value carried alongside `status`. Reused
    /// as the `error`/`reason`/`limit` string for the `ResultStatus` variants
    /// that carry one (`Failed`/`Cancelled`/`BudgetExceeded`); ignored for
    /// variants that carry none (`Completed`/`Rejected`).
    pub value: String,
}

/// One NARROWING permission rule a plugin contributes via
/// [`Plugin::permission_rules`] -- a per-tool verdict the host's
/// `PermissionBroker` consults for that tool's calls. See that method's own
/// doc for the subordination boundary (operator wins; wire policy is
/// advisory-under-enforcement, narrowing only) and the type-level
/// "no `Allow` variant" proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginPermissionRule {
    /// The tool name this rule scopes to, matched exactly (the identical
    /// match `Select::Tools([tool])` uses). A rule naming a tool this plugin
    /// does not own is an authoring bug, not a security boundary -- the host
    /// does not enforce ownership here.
    pub tool: String,
    /// The narrowing verdict. `Deny` -> the broker installs a
    /// `PatternOrigin::Plugin` deny rule; `Prompt` -> a prompt rule (forces
    /// the gate); `Abstain` -> installs nothing (no opinion).
    pub verdict: PluginPermissionVerdict,
    /// A free-text reason the plugin authors the rule with. Parsed from the
    /// wire and surfaced at the plugin layer -- `SubprocessPlugin::permission_rules`
    /// exposes it and the integration tests assert it round-trips -- but NOT yet
    /// carried into the broker's rendered denial: the broker renders a `Deny` via
    /// `Rule::describe` (the select/when label) with no `PatternOrigin::Plugin`
    /// attribution and no per-rule reason text. Threading the reason (and the
    /// plugin attribution) into the denial/prompt surface is an unbundled
    /// follow-up, the same class of gap as `PermissionBroker::decide`'s own "why
    /// the operator is being asked" rendering. `Abstain` installs nothing, so the
    /// reason is unused there regardless.
    pub reason: String,
}

/// The verdict a [`PluginPermissionRule`] carries -- NARROWING-only, by type
/// construction: there is no `Allow` variant, so a plugin can never widen
/// what the operator authorized. This is the narrowing-only (no `Allow`)
/// shape `docs/plugins/hooks.md` point 8's `NarrowingPolicy` prescribes
/// ("may only narrow" is a property of the return type, not a runtime flag),
/// extended with `Prompt` for this spec's `Dangerous`->prompt mapping --
/// `NarrowingPolicy` itself is `Deny`|`Abstain` (no `Prompt`), and the full
/// `NarrowingPolicy`/`DecidingPolicy` per-call inference chain remains
/// design-only (see hooks.md point 8). See [`Plugin::permission_rules`]'s own
/// doc for the subordination boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginPermissionVerdict {
    /// Refuse the call outright, before any allow path is consulted. Maps
    /// to a `PatternOrigin::Plugin` deny rule at the broker's deny tier
    /// (step 2 of `PermissionBroker::decide`'s ordering -- before plan-mode,
    /// the cache, pattern-allow, and `AutoAllow`).
    Deny,
    /// Force the call to the operator's gate even in `AutoAllow` mode and
    /// even over a matching `allow` grant. Maps to a `PatternOrigin::Plugin`
    /// prompt rule (step 4 -- sets `must_reach_gate`, skipping the
    /// cache/pattern-allow/`AutoAllow` shortcuts). The operator's own
    /// denial/plan-mode refusal still fires FIRST and outranks this.
    Prompt,
    /// No opinion -- the plugin contributes no rule for this tool, and the
    /// operator's own `permissions.json`/`PermissionMode` decides alone.
    /// Installs nothing.
    Abstain,
}

/// One [`PluginConfig`] key a plugin declares narrowable in per-agent
/// state, plus the pure comparison [`PluginConfig::narrow`] uses to decide
/// whether a requested child value narrows (or merely equals) the parent's
/// own value for that key. See [`Plugin::narrowable_keys`]'s own doc for
/// why this declare-your-own-comparison shape exists at all.
///
/// `narrows` is a plain `fn` pointer, not a `Box<dyn Fn>`: it must be a
/// pure, call-independent property of the key (like [`PathArgs`]/
/// [`RenderKind`] are pure properties of a tool), never a closure capturing
/// per-call state, so [`NarrowingRule`] stays cheaply `Copy` and a
/// registry built from it needs no lifetime management beyond the
/// declaring plugin's own `'static` lifetime. It MAY perform I/O internally
/// (`conway-core` itself never calls it except through [`PluginConfig::
/// narrow`], which performs none) -- `conway.fs`'s own root key, for
/// instance, canonicalizes both paths to answer honestly under symlinks,
/// exactly as `conway_core::containment::CanonicalRoot` already does for
/// the harness-level root this mechanism supersedes.
#[derive(Clone, Copy)]
pub struct NarrowingRule {
    /// Bare key name (e.g. `"root"` for a plugin whose manifest id is
    /// `"conway.fs"`), reachable in a caller's `SubagentSpec::plugin_config`
    /// map as `"conway.fs.root"` once the host prefixes it -- see
    /// [`Plugin::narrowable_keys`]'s own doc.
    pub key: &'static str,
    /// `true` iff `child` is narrower than, or equal to, `parent`. MUST be
    /// a total function over any two [`serde_json::Value`]s a caller might
    /// supply (untrusted, JSON-typed input, both model- and
    /// embedder-reachable) -- never panics; a value of the wrong shape
    /// (e.g. a number where a path string is expected) is simply not a
    /// narrowing, so the correct answer is `false`, never a panic.
    pub narrows: fn(parent: &serde_json::Value, child: &serde_json::Value) -> bool,
}

impl std::fmt::Debug for NarrowingRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NarrowingRule")
            .field("key", &self.key)
            .field("narrows", &"<fn>")
            .finish()
    }
}

/// [`PluginConfig::narrow`]'s typed rejection -- a child's requested
/// per-agent override is refused outright, never silently clamped to the
/// parent's value and never silently honored. Both variants name the
/// offending `key` so a caller (`conway_runtime`'s `SubagentHost::start`,
/// which surfaces this as a spec-rejection error) can report exactly what
/// was wrong.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PluginConfigError {
    /// `key` is not declared narrowable by any installed plugin
    /// ([`Plugin::narrowable_keys`]) -- it can never vary per-agent, at any
    /// level, so an ancestor cannot have "permitted" it either. This is
    /// what makes "a plugin that declares nothing narrowable keeps
    /// global-only configuration" true: without a declaration, nothing --
    /// not even a root session's own initial config -- can set the key
    /// per-agent.
    #[error("plugin config key '{key}' is not declared narrowable by any installed plugin")]
    NotNarrowable { key: String },
    /// `key` IS declared narrowable, but the parent already has an
    /// effective value for it and the plugin's own [`NarrowingRule::narrows`]
    /// returned `false` for `(parent_value, requested_value)` -- the
    /// requested value would widen (or move sideways from) what the parent
    /// already carries.
    #[error(
        "plugin config key '{key}' may only narrow the value inherited from its parent, and \
         the requested value does not"
    )]
    WouldWiden { key: String },
}

/// One instruction fragment a plugin declares via [`Plugin::instructions`]
/// -- see that method's own doc for the full argument (legibility over
/// expressibility, the structural-vs-checkable reachability split, the
/// precedence this composes under, and the relationship to
/// `conway.skills`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstructionFragment {
    /// A bare name, unique across every fragment every installed plugin
    /// declares (checked at `ConwayBuilder::build` -- a build-time,
    /// configuration-INDEPENDENT fact, unlike the reachability check
    /// below, so it is caught there rather than deferred to context
    /// assembly). Not prefixed with this plugin's own
    /// [`PluginManifest::id`] on the wire -- unlike [`EventDecl::name`]/
    /// [`CommandSpec::name`], a duplicate fragment name is not a
    /// namespace COLLISION to avoid (nothing routes on it), so the
    /// assembling host attributes it by pairing `(plugin_id, name)`
    /// wherever it renders one, rather than mangling the two into a
    /// single string.
    pub name: String,
    /// The fragment's instruction text -- injected as its own
    /// `Role::System` segment when every id in [`Self::tool_ids`] is
    /// reachable, withheld entirely otherwise. See [`Plugin::instructions`]'s
    /// own doc for the markdown-file convention this field is meant to be
    /// sourced from.
    pub text: String,
    /// Every tool id [`Self::text`] assumes the model can call. May be
    /// empty for a fragment that names no specific tool (e.g. a general
    /// style note) -- an empty list is trivially always reachable.
    pub tool_ids: Vec<ToolName>,
}

/// [`Plugin::description`]'s own return type -- an operator-facing
/// description of what turning this plugin on or off actually changes.
/// See that method's own doc for why this is a separate type from
/// [`InstructionFragment`] (different audience, different cardinality) and
/// why its text is a Rust literal rather than a loaded file.
///
/// **"You get / you lose / costs" is deliberate, load-bearing phrasing --
/// kept literally, not paraphrased into a generic "description" field.**
/// It names what CHANGES, which is the actual question an operator is
/// asking when deciding whether to flip a toggle; a prose description
/// answers a different, less actionable question ("what is this").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginDescription {
    /// One line, fit for a compact list row alongside this plugin's id and
    /// on/off state (e.g. "notes that survive a restart"). Empty means no
    /// description was supplied -- a browser renders that honestly (e.g.
    /// "(no description)"), never a placeholder claim.
    pub summary: String,
    /// What turning this plugin ON adds -- tools, commands, an
    /// instruction, ... (e.g. "3 tools · /memory · an instruction telling
    /// the model when to write things down"). Empty means nothing to
    /// report beyond the summary.
    pub you_get: String,
    /// What is different with this plugin OFF, phrased for someone
    /// deciding whether to flip it (e.g. "nothing else -- recall falls
    /// back to context"). Empty means no notable loss beyond what
    /// [`Self::you_get`] already names.
    pub you_lose: String,
    /// The ongoing cost of running this plugin, if any (e.g. "a small
    /// read at the start of every turn"). Empty means no notable
    /// standing cost.
    pub costs: String,
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
/// Most of the narrowing still stands: a command still cannot steer any
/// agent, read/write a file through conway's own mediation, or reach the
/// permission broker. An extension point earns a wider grant only once a
/// real consumer needs it, not ahead of one (YAGNI) -- `/rewind`, the item
/// that asked this question first, was that consumer for forking one's own
/// session.
///
/// **One further, deliberate widening: [`CommandOutcome::Checkout`].** A
/// command CAN now name a DIFFERENT, already-existing session -- the
/// capability `/checkout <session>` needs (board item
/// 01KZY8QRAVVVKCRBZ6HAEGW3GG) and the one case `ForkSession`'s own doc
/// above says is deliberately impossible for IT. This does not reopen that
/// argument: `Checkout` still cannot act ON the named session beyond
/// asking the host to fork it (see that variant's own doc) -- the widening
/// is "which session can be named," not "what can be done to it once
/// named." A second addition, [`CommandOutcome::MaskRecord`], stays bound
/// to the invoking session exactly like `ForkSession` -- see its own doc.
///
/// **A third widening in KIND, not in reach: [`CommandOutcome::
/// SubmitPrompt`]** (board item `01M0VSMF71S6VXX81YRAAF5S8Q`). Every
/// variant above lets a command act on session HISTORY (fork it, mask a
/// record in it, check another one out); this one lets a command START A
/// NEW TURN -- text a command supplies flows into the conversation exactly
/// as if the operator had typed it. Bound to the invoking agent/session
/// exactly like `ForkSession`/`MaskRecord` (see that variant's own doc for
/// the full binding argument, which applies unchanged), and, like every
/// widening above it, earned by a real consumer: this item's own
/// file-backed command (see `conway_plugin_skeleton`'s `FilePromptCommand`)
/// is that consumer, not a speculative grant ahead of one.
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
    /// Asks the host to append a `LogRecord::ContextMask` to the CALLING
    /// session's own log -- the producer `/rewind`'s own item deferred
    /// (board item 01KZY8QRAVVVKCRBZ6HAEGW3GG, "`/checkout` and a reachable
    /// `ContextMask`"). See that record's own doc
    /// (`conway_core::log::LogRecord::ContextMask`) for the full overlay
    /// contract; the short version is that this masks (or, with `excluded:
    /// false`, un-masks) `target_seq` out of what a FUTURE fork of THIS
    /// session inherits -- never the owning session's own later turns, and
    /// never a mutation of the targeted record itself.
    ///
    /// **Bound to the invoking session, structurally, exactly like
    /// [`Self::ForkSession`].** No field here names a session other than
    /// the one this command was invoked from -- see that variant's own doc
    /// for the full "acts on its own session, never one it names" argument,
    /// which applies here unchanged. `target_seq` is a LOCAL seq within
    /// that same session (the same units `Self::ForkSession::at_seq`
    /// uses), naming another record already in its log.
    ///
    /// **What the host actually does with this, disclosed here since this
    /// crate performs none of it:** `Conway::mask_record(session_id,
    /// target_seq, excluded)`, which appends the record via
    /// `SessionStore::append` -- an ordinary, reversible, append-only write
    /// (masking and un-masking are both just another appended record), not
    /// a mutation of `target_seq`'s own stored bytes. Unlike
    /// `ForkSession`, a successful `MaskRecord` never swaps the host's
    /// driven session -- there is no new session to drive.
    MaskRecord {
        /// The LOCAL seq, within the CALLING session's own log, to mask (or
        /// un-mask). Out-of-range is a host-side error (`SessionStore::
        /// append` performs no bounds check of its own against `head`, so
        /// today this can name a seq that does not exist yet; a future
        /// item may tighten that, but nothing about this crate can check it
        /// -- same disclosed limit `ForkSession::at_seq` names for its own
        /// bounds check).
        target_seq: LogSeq,
        /// `true` excludes `target_seq` from what a future fork inherits;
        /// `false` reverses a previous exclusion (`LogRecord::ContextMask`'s
        /// own doc: "the latest `ContextMask` for a given `target_seq` --
        /// by append order -- decides").
        excluded: bool,
    },
    /// Asks the host to CHECK OUT another already-existing session:
    /// fork `target` at ITS OWN current head and drive the resulting child
    /// in place of whatever session the host is currently driving -- the
    /// answer to "`/checkout <session>` does not exist" (board item
    /// 01KZY8QRAVVVKCRBZ6HAEGW3GG).
    ///
    /// **Always forks, never attaches to the live session directly.**
    /// `PHILOSOPHY.md` §1: a finished session is forkable at any point, and
    /// forking is the safer default -- it preserves append-only (the
    /// checked-out session's own log is never written to just because an
    /// operator looked at it) and needs no new "two live agents driving one
    /// session" concurrency story. A no-op checkout onto the session
    /// already being driven still forks (a new child at the current head),
    /// by design -- this variant does not special-case that as an error or
    /// a silent identity return, since detecting "already there" would
    /// require the host to compare `target` against whatever it currently
    /// drives, which is exactly the kind of implicit self-reference
    /// [`Self::ForkSession`]'s own doc argues against baking into a
    /// command's request.
    ///
    /// **Deliberately widens what a command can name, unlike
    /// `ForkSession`.** `ForkSession` structurally cannot name a session
    /// other than the invoking one; `Checkout` structurally MUST be able
    /// to, since checking out is the entire point -- there is no narrower
    /// shape that still does what `/checkout <session>` asks for. This is
    /// the one new capability this crate grants for this item, and grants
    /// nothing else: a command still cannot read another session's
    /// content, steer it, or act on it in any way other than "hand me a
    /// fresh fork of it to drive."
    ///
    /// **What the host actually does with this, disclosed here since this
    /// crate performs none of it:** resolves `target`'s current head via
    /// `Conway::session_head(target)`, then `Conway::fork_from(target,
    /// head, ForkSpec::new(""))` -- the same zero-copy-by-reference
    /// `SessionStore::fork` contract `ForkSession` relies on, so `target`'s
    /// own log is untouched and it stays listed exactly as before
    /// (`Conway::sessions` enumerates unchanged). The host then swaps its
    /// driven `SessionHandle` for the returned child, the same swap
    /// `ForkSession`'s own host-side doc describes.
    Checkout {
        /// The session to check out. Not validated by this crate (no I/O
        /// here) -- an unknown or malformed id is a host-side error,
        /// surfaced the same way an out-of-range `ForkSession::at_seq` is.
        target: SessionId,
    },
    /// Asks the host to submit `text` as a new turn on the CALLING agent --
    /// as if the operator had typed it -- closing the one gap board item
    /// `01M0VSMF71S6VXX81YRAAF5S8Q` found: no existing variant could put
    /// text into the conversation as a turn, which is what a
    /// prompt-template command's entire job is (`/review-this`, `/explain`,
    /// the shape Claude Code's own `commands/*.md` plugins are built almost
    /// entirely on -- see that item's own spec for why this was filed
    /// separately from the compatibility layer that first needed it).
    ///
    /// **Determine-first question 1 -- provenance, answered, not defaulted.**
    /// This text was authored by conway (a plugin's own template or logic),
    /// never typed by the operator, even though the model reads it in the
    /// identical `Role::User` position an operator's own turn would occupy.
    /// The host does NOT stamp the resulting `LogRecord::UserTurn` with
    /// `Provenance::UserPrompt` -- doing so would be exactly the
    /// misattribution this crate's provenance discipline exists to catch.
    /// It stamps [`crate::provenance::Provenance::CommandPrompt`] instead,
    /// naming the full command (`plugin_id.bare_name`) that produced it --
    /// see that variant's own doc. This is a new, dedicated `Provenance`
    /// variant (a persisted wire-format addition), not a repurposing of
    /// `MergedAsk`/`ChildResult`/any existing one: none of those describe
    /// "conway generated this turn's text from a plugin's own logic",
    /// which is a genuinely distinct origin from a merged `/ask` question
    /// or a child agent's terminal result.
    ///
    /// **Determine-first question 2 -- port variant, not a renderer
    /// `Effect`, answered, not assumed.** `crate` (`conway-core`) cannot
    /// depend on `conway-cli`, so a TUI-only `Effect` could never live
    /// here regardless; the real question this item's spec raises is
    /// whether the CAPABILITY should be TUI-only at all. This project's
    /// own rule (GP-05/C-03: "no capability may exist in only one mode")
    /// decides it: a library embedder holding a live `Conway`/
    /// `SessionHandle` and a `Command` it invoked directly must be able to
    /// fulfil this exactly like `conway-cli`'s `App` does, with no TUI in
    /// the loop at all. `conway::SessionHandle::prompt_command` (disclosed
    /// below) is that facade primitive, reachable by any consumer holding
    /// a `SessionHandle` -- TUI, one-shot, or a bare library caller alike
    /// -- not a method `conway-cli` alone can reach.
    ///
    /// **Determine-first question 3 -- v1 does NO interpolation, stated
    /// rather than built.** `text` is a literal string this crate never
    /// parses, templates, or substitutes into -- no `{{args}}`/
    /// `$ARGUMENTS`/positional-placeholder syntax exists anywhere in this
    /// port. A `Command::invoke` implementation that wants to fold
    /// [`CommandCtx::args`] into the submitted text does so itself, with
    /// ordinary Rust string building (exactly like [`Self::Output`]'s own
    /// `GreetCommandFixture`-shaped examples already echo `ctx.args` back
    /// today) -- there is no template language for this crate to parse
    /// untrusted argument text through, which is the smaller, safer slice
    /// P-10 (range-check untrusted input at the boundary) prefers over
    /// building one ahead of a real consumer that needs it.
    ///
    /// **Bound to the invoking session AND the invoking agent,
    /// structurally, the same shape [`Self::ForkSession`]/
    /// [`Self::MaskRecord`] already establish.** No field here names a
    /// session or agent other than the ones this command was invoked
    /// from -- `conway_cli::tui::app::App` (the one production host)
    /// resolves the submission against the SAME `CommandCtx::
    /// focused_agent`/`CommandCtx::session_id` it captured when it spawned
    /// this invocation's `Command::invoke` call, never against whatever
    /// agent/session it happens to be driving by the time the reply
    /// arrives. Targeting `focused_agent` (not `root_agent`) matches "as if
    /// the operator had typed it" literally: an ordinary typed message
    /// targets whichever agent the operator is currently looking at, and
    /// this variant does too.
    ///
    /// **Determine-first question 4 -- the in-flight guard, disclosed
    /// here since this crate performs none of it.** `App::
    /// apply_plugin_command_done`'s own `SubmitPrompt` arm refuses (a
    /// `Notice`, nothing appended) rather than silently racing a second
    /// turn onto the SAME agent the TUI is currently watching mid-turn --
    /// see that arm's own doc for the exact predicate and its disclosed
    /// limit. Never swaps the driven session -- submitting a prompt never
    /// changes which session is driven, exactly like `MaskRecord`.
    ///
    /// **What the host actually does with this, disclosed here since this
    /// crate performs none of it:** `SessionHandle::prompt_command(ctx.
    /// focused_agent, text, full_name)` -- the SAME `Runtime::prompt`
    /// machinery an ordinary operator turn uses (persist-before-act, the
    /// live `Event::UserTurn` twin, the `prompt_notify` wake), except
    /// stamped with `Provenance::CommandPrompt` instead of `Provenance::
    /// UserPrompt`.
    SubmitPrompt {
        /// The literal text to submit, verbatim -- see this variant's own
        /// doc for why v1 performs no interpolation of any kind.
        text: String,
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

/// A host capability a plugin may declare it requires (via
/// [`PluginManifest::required_host_caps`]) or may only optionally use (via
/// [`PluginManifest::optional_host_caps`]), and the host separately grants
/// at build time (via `conway::HostCaps`), never implied by trust alone.
///
/// **An OPEN, namespaced vocabulary -- not the closed two-variant enum this
/// type used to be.** Until `docs/vision/DESIGN-plugin-dependencies.md` §2
/// (Edge A) named the defect, a plugin could name a cap only from a fixed,
/// `#[non_exhaustive]`-but-still-closed membership list (`Subagent`,
/// `PersistentTransport`): a third party could never declare a capability
/// core had not already blessed, and every new host surface was a breaking
/// enum edit. This reuses the naming discipline that already solved the
/// identical problem for a plugin's own event names
/// (`crate::event_name`'s own module doc; design §2: *"That is the right
/// model for a capability vocabulary"*): [`crate::event_name::
/// validate_event_name`] (the `None`-declaring-plugin, subscriber-side
/// branch -- the SAME shape check `[hooks].rules[].event` already applies)
/// is reused rather than reimplemented, so a name is legal here iff it is
/// either bare (no [`crate::event_name::EVENT_NAMESPACE_SEPARATOR`]) or a
/// well-formed `namespace.name`. Two bare names are reserved for what the
/// CORE host itself blesses and stay unit variants for that reason --
/// [`Self::Subagent`] and [`Self::PersistentTransport`] -- so both keep
/// resolving with **no `settings.json` change** and no existing call site
/// naming them needs to change. Any other well-formed name -- conventionally
/// `plugin_id.cap_name`, the offering plugin's own namespace, though this
/// type does not enforce that convention structurally the same way
/// [`crate::event_name::validate_event_name`]'s `Some(id)` branch enforces
/// self-namespacing for a plugin's OWN declared events -- constructs
/// [`Self::Named`] via [`Self::named`].
///
/// **Shape only, not a closed-vocabulary check.** Exactly as
/// `crate::event_name`'s own module doc records for events (§16.6 point 2):
/// this type answers "is `name` well-formed", never "does anything actually
/// offer `name`". The latter is `conway::HostCaps::check_manifest`/
/// `missing_optional`'s job, comparing a manifest's declared caps against
/// what the host built at `ConwayBuilder::build` -- unchanged by this item,
/// still a hard `PluginError::MissingHostCapability` for a missing
/// *required* cap, still narrowing/safe.
///
/// **Still wire-compatible with the old closed form.** Serialization is a
/// bare string (`"subagent"`, `"persistent_transport"`, or an arbitrary
/// well-formed name for [`Self::Named`]) -- the identical shape the old
/// `#[serde(rename_all = "snake_case")]` derive produced for the two
/// original variants, so a manifest already on disk or over the wire parses
/// unchanged. A malformed tag (empty, or containing the separator with an
/// empty prefix or suffix) still fails closed at deserialization -- the
/// NARROWING/safe direction, consistent with the unknown-tag item
/// `01M03VJPRT8629CYR8JK4A8JPF`'s "structural malformation fails closed"
/// line; only a well-formed but previously-unknown tag now succeeds, which
/// is the entire point of opening the vocabulary.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HostCapability {
    /// Fork/spawn a child session through a `SubagentHost`. Required by the
    /// `conway.subagent` built-in; offered by the `conway` runtime (which
    /// always provides a `SubagentHost`).
    Subagent,
    /// A persistent NDJSON `tool/1` channel to a subprocess plugin. Offered
    /// iff at least one `[plugins].subprocess[]` entry is configured with
    /// `SubprocessTransport::Persistent`; a plugin requiring it against a
    /// one-shot-only host is refused at registration.
    PersistentTransport,
    /// An open-vocabulary capability name -- anything other than the two
    /// core-blessed bare caps above. `#[non_exhaustive]` on the enum already
    /// keeps a downstream crate from constructing this variant directly;
    /// within this crate, prefer [`Self::named`] so the shape check and the
    /// "known bare name collapses to the matching unit variant"
    /// normalization (see that method's own doc) run rather than being
    /// bypassed.
    Named(String),
}

impl HostCapability {
    /// The wire string for [`Self::Subagent`] -- kept as a named constant
    /// so [`Self::named`]/[`Self::as_wire_str`] and `Deserialize` all read
    /// from one place rather than restating the literal.
    const SUBAGENT_WIRE: &'static str = "subagent";
    /// The wire string for [`Self::PersistentTransport`]; see
    /// [`Self::SUBAGENT_WIRE`]'s own doc.
    const PERSISTENT_TRANSPORT_WIRE: &'static str = "persistent_transport";

    /// Constructs an open-vocabulary [`HostCapability`] from `name`,
    /// validating its shape with [`crate::event_name::validate_event_name`]
    /// (`None` declaring-plugin branch) -- the SAME subscriber-side shape
    /// rule `[hooks].rules[].event` already applies: bare (no
    /// [`crate::event_name::EVENT_NAMESPACE_SEPARATOR`]) or a well-formed
    /// `namespace.name`, reused rather than reimplemented (see this enum's
    /// own doc). `name` equal to one of the two core wire strings
    /// (`"subagent"`, `"persistent_transport"`) normalizes to
    /// [`Self::Subagent`]/[`Self::PersistentTransport`] rather than a
    /// [`Self::Named`] wrapping the identical string, so equality and
    /// matching against the core variants keep working regardless of which
    /// construction path produced a given value (the [`Deserialize`] impl
    /// performs the same normalization for a value that arrives over the
    /// wire).
    pub fn named(name: impl Into<String>) -> Result<Self, String> {
        let name = name.into();
        validate_event_name(&name, None).map_err(|err| {
            format!(
                "host capability name is malformed (reusing the plugin-event name shape rule): \
                 {err}"
            )
        })?;
        Ok(Self::normalize(name))
    }

    /// Collapses a validated, well-formed `name` to the matching core unit
    /// variant if it equals one of the two reserved bare wire strings,
    /// otherwise wraps it as [`Self::Named`]. Shared by [`Self::named`] and
    /// `Deserialize` so both construction paths normalize identically.
    fn normalize(name: String) -> Self {
        match name.as_str() {
            Self::SUBAGENT_WIRE => HostCapability::Subagent,
            Self::PERSISTENT_TRANSPORT_WIRE => HostCapability::PersistentTransport,
            _ => HostCapability::Named(name),
        }
    }

    /// The wire string for this cap -- the same value `to_string()` returns
    /// and the only form a plugin author puts on the wire. Centralized so
    /// the two core names live in exactly one place, not restated per use.
    pub fn as_wire_str(&self) -> &str {
        match self {
            HostCapability::Subagent => Self::SUBAGENT_WIRE,
            HostCapability::PersistentTransport => Self::PERSISTENT_TRANSPORT_WIRE,
            HostCapability::Named(name) => name.as_str(),
        }
    }
}

impl std::fmt::Display for HostCapability {
    /// The wire string (see [`Self::as_wire_str`]) -- slots into
    /// `PluginError::MissingHostCapability { capability: String }`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

impl Serialize for HostCapability {
    /// A bare string -- the wire form every consumer (this crate's own
    /// round-trip test, `conway-plugin-subprocess`'s `WireManifest`) already
    /// expects, unchanged by opening the vocabulary. See this enum's own doc,
    /// "Still wire-compatible with the old closed form".
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_wire_str())
    }
}

impl<'de> Deserialize<'de> for HostCapability {
    /// Parses the bare wire string and re-validates its shape with
    /// [`crate::event_name::validate_event_name`] (via `Self::normalize`,
    /// which is private -- a plain code span, not an intra-doc link, since
    /// the `-D warnings` doc gate rejects a public doc linking to a private
    /// item; the same normalization [`Self::named`] performs) -- a malformed tag
    /// FAILS CLOSED here exactly as the old closed-enum derive did for an
    /// unrecognized tag; a well-formed but previously-unknown tag now
    /// succeeds as [`Self::Named`], which is the point of opening the
    /// vocabulary. See this enum's own doc.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        validate_event_name(&raw, None).map_err(serde::de::Error::custom)?;
        Ok(Self::normalize(raw))
    }
}

/// A plugin's static identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub tools: Vec<ToolName>,
    /// Host capabilities this plugin requires the host to offer. An empty vec
    /// means "needs nothing the host might lack" (the common case). Any cap
    /// the host does NOT offer -> `PluginError::MissingHostCapability` at
    /// registration (`ConwayBuilder::build`), naming both the plugin and the
    /// cap. See [`HostCapability`] for the open, namespaced vocabulary.
    pub required_host_caps: Vec<HostCapability>,
    /// Host capabilities whose absence degrades only a presentation or
    /// convenience of this plugin -- the host-capability analogue of
    /// [`Self::optional`] (plugin -> plugin), applying design §4a's
    /// identical criterion one edge up: Edge A (plugin -> host) rather than
    /// Edge B (plugin -> plugin). An empty vec (the common case) means
    /// "nothing about this plugin degrades based on a host capability's
    /// absence".
    ///
    /// **Absence never fails `build()`.** Same posture as [`Self::optional`]:
    /// a missing optional cap loads this plugin anyway, degraded -- and the
    /// degradation is always announced, never silent: `ConwayBuilder::build`
    /// records a `ConfigWarning` (`WarningCode::OptionalHostCapabilityMissing`,
    /// in the `conway` facade's `config` module) naming both this plugin and
    /// the missing cap, and emits a `tracing::warn!` with the same two
    /// names, so a headless run with no plugin browser to render a notice
    /// into still has SOMEWHERE the omission is written down
    /// (`docs/vision/DESIGN-plugin-dependencies.md` §4b: "no surface may
    /// degrade silently") -- the SAME two-channel announcement
    /// [`Self::optional`]'s own doc already describes for a missing
    /// optional plugin dependency, reused rather than a second mechanism
    /// invented for the identical idea one edge over.
    ///
    /// `#[serde(default)]`, the same reason [`Self::required_host_caps`]
    /// predates it needing none and [`Self::requires`]/[`Self::optional`]
    /// have it: a manifest predating this field parses as empty, never an
    /// error.
    #[serde(default)]
    pub optional_host_caps: Vec<HostCapability>,
    /// Plugin ids this plugin's stated function cannot perform at all
    /// without (`docs/vision/DESIGN-plugin-dependencies.md` §4/§4a's
    /// participant/observer-derived criterion: "the dependent cannot
    /// perform its stated function at all without the dependency"). An
    /// empty vec (the common case) means "depends on no other plugin".
    ///
    /// **Enforced at `ConwayBuilder::build`, not at registration order.**
    /// A dependency id absent from the FINAL installed plugin set (built-in
    /// ++ every `with_plugin`/`install_selected`-installed plugin, in
    /// whatever order they were added) is a hard `FacadeError::Build`
    /// naming both the dependent and the missing dependency -- the same
    /// "a plugin cannot be enabled without its dependencies enabled; not
    /// degraded, not silently auto-installed -- refused" posture
    /// [`required_host_caps`](Self::required_host_caps) already has for
    /// host capabilities, applied here to plugin-to-plugin edges. A cycle
    /// among `requires` edges (`a` requires `b` requires `a`) is its own
    /// distinct, named build error -- neither side of a cycle can ever be
    /// "satisfied first", so it is refused rather than resolved by
    /// accident of iteration order.
    ///
    /// **Name-only: NO version constraint is expressed or checked.** There
    /// is no `semver` crate anywhere in this workspace, and this field is a
    /// bare plugin id string -- `requires: vec!["conway.ui".into()]`
    /// verifies only that SOME plugin with that id is installed, never
    /// which [`PluginManifest::version`] it reports. A dependent that needs
    /// a version floor (e.g. "a widget `ui.form/1` only gained in
    /// `conway.ui` 0.4") has no way to express that yet; see
    /// `docs/vision/DESIGN-plugin-dependencies.md` §7b for the tradeoff
    /// argued and the condition under which this should change.
    ///
    /// **Resolution order is topological; installation/injection order is
    /// not.** `ConwayBuilder::install_selected` uses a dependency-ordered
    /// (topological) walk internally to validate this graph -- detecting a
    /// missing-required id early where it can, and any cycle authoritatively
    /// -- but never reorders the actual `with_plugin` calls it makes: those
    /// stay in `[plugins].install`'s own order, because [`Plugin::instructions`]'s
    /// own precedence rule ("Multiple plugins' fragments
    /// are injected in `with_plugin`/`install_selected` install order")
    /// depends on it. Resolving a dependency graph and deciding what
    /// PRECEDES what in an assembled prompt are two different questions;
    /// this field answers only the first.
    ///
    /// `#[serde(default)]`: a manifest deserialized from a source that
    /// predates this field (or simply omits it, the common case) parses as
    /// empty -- "depends on nothing" -- never a deserialization error.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Plugin ids whose absence degrades only a presentation or convenience
    /// of this plugin -- its stated function otherwise survives intact
    /// (`docs/vision/DESIGN-plugin-dependencies.md` §4a's observer half of
    /// the same criterion: "the dependent's function survives; only a
    /// presentation or convenience of it is lost"). An empty vec (the
    /// common case) means "nothing about this plugin degrades based on
    /// another plugin's presence".
    ///
    /// **Absence never fails `build()`.** Unlike [`Self::requires`], a
    /// missing optional dependency loads this plugin anyway, degraded --
    /// and the degradation is always announced, never silent: `build()`
    /// records a `ConfigWarning` (`WarningCode::OptionalPluginDependencyMissing`,
    /// in the `conway` facade's `config` module) naming both this plugin and
    /// the missing dependency, and emits a `tracing::warn!` with the same
    /// two ids, so a headless run with no plugin browser to render a notice
    /// into still has SOMEWHERE the omission is written down (see that
    /// design page's §4b: "no surface may degrade silently").
    ///
    /// **Same name-only limitation as [`Self::requires`]**: an id here
    /// names a plugin, never a version of one.
    ///
    /// `#[serde(default)]`, the same reason [`Self::requires`] has it: a
    /// manifest predating this field parses as empty, never an error.
    #[serde(default)]
    pub optional: Vec<String>,
}

/// A plugin's untyped configuration values, as loaded and handed down by the
/// facade. This crate does no config loading itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginConfig {
    pub values: serde_json::Map<String, serde_json::Value>,
}

impl PluginConfig {
    /// Computes a CHILD's own effective per-agent overrides, given
    /// `requested` (the child's own `SubagentSpec::plugin_config`, if any)
    /// and `rules` (every installed plugin's declared narrowing rules,
    /// keyed by the ALREADY-prefixed `"{plugin_id}.{bare_key}"` string --
    /// see [`NarrowingRule`]/[`Plugin::narrowable_keys`]'s own docs).
    /// `self` is the PARENT's own effective per-agent overrides.
    ///
    /// `requested: None` means "inherit unchanged" -- returns `self.clone()`
    /// verbatim, the same "no override" shape `SubagentSpec::root: None`
    /// already established.
    ///
    /// For each key in `requested.values`:
    /// - absent from `rules` -> [`PluginConfigError::NotNarrowable`]: no
    ///   installed plugin declared this key narrowable, so it can never
    ///   vary per-agent at all.
    /// - present in `self` (the parent already has an effective value) ->
    ///   the rule's [`NarrowingRule::narrows`]`(parent_value, child_value)`
    ///   must return `true`, or [`PluginConfigError::WouldWiden`].
    /// - absent from `self` (nothing has narrowed this key anywhere in the
    ///   ancestor chain yet) -> accepted unconditionally: going from
    ///   unbounded to *some* bound is always a narrowing, never a widening
    ///   (mirrors `SubagentSpec::root`'s own "parent with no root at all,
    ///   i.e. nothing to narrow against yet" rule).
    ///
    /// On success, returns the merged map: `self`'s values, with every key
    /// in `requested` overwritten by the now-validated child value. Pure --
    /// no I/O performed by this method itself (a plugin's own `narrows` fn
    /// may perform I/O; see [`NarrowingRule`]'s own doc).
    pub fn narrow(
        &self,
        requested: Option<&PluginConfig>,
        rules: &std::collections::HashMap<String, NarrowingRule>,
    ) -> Result<PluginConfig, PluginConfigError> {
        let Some(requested) = requested else {
            return Ok(self.clone());
        };
        let mut merged = self.values.clone();
        for (key, child_value) in requested.values.iter() {
            let rule = rules
                .get(key)
                .ok_or_else(|| PluginConfigError::NotNarrowable { key: key.clone() })?;
            if let Some(parent_value) = self.values.get(key) {
                if !(rule.narrows)(parent_value, child_value) {
                    return Err(PluginConfigError::WouldWiden { key: key.clone() });
                }
            }
            merged.insert(key.clone(), child_value.clone());
        }
        Ok(PluginConfig { values: merged })
    }
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
    /// way, so it carries none of the risk that gates `conway-testkit`'s
    /// doubles behind that separate crate's own opt-in, and needs no
    /// opt-in at all -- reachable from every crate in the workspace, and
    /// from a third party depending only on `conway` too.
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
    /// The context-path composition capability (decision
    /// `01M0K4QT6MBXPD6PXMBBBD2P7B`; [`crate::ports::ContextPathHost`]'s own
    /// module doc): bound to [`Self::session_id`] for
    /// `default_path`/`set_head`, and reachable for any session's records
    /// via `resolve_records`. Mirrors [`Self::subagents`] exactly -- a
    /// caller-bound handle, never a raw store.
    pub context_path: ContextPathHandle,
    /// The cross-session discovery capability (board item
    /// `01M0PS8J3AK7Z7253Z3E3RD3GY`; `SessionDiscoveryHost`'s own module
    /// doc): finds a session a caller neither owns nor holds a
    /// `transcript_ref` for, so its `(session, seq)` refs can be handed to
    /// [`Self::context_path`]'s `resolve_records`/`compose_context_path`.
    /// Cross-session by construction -- unlike `context_path`, there is no
    /// single session to bind this to.
    pub session_discovery: SessionDiscoveryHandle,
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
            .field("context_path", &self.context_path)
            .field("session_discovery", &self.session_discovery)
            .finish()
    }
}

impl ToolCtx {
    /// Builds a `ToolCtx` for a [`Tool::invoke`] unit test, wiring
    /// caller-supplied `subagents`/`events` in place of the two fields that
    /// otherwise force a hand-rolled `SubagentHost`/`EventSink` impl (board
    /// item 01KZQ3AZWG3NNJNZEJFX21MDJT, "ToolCtx carries the same
    /// construction tax `ContextHookCtx` just shed"; that item's own
    /// precedent, [`ArtifactWriteHandle::noop`], is [`Self::plugin_events`]'s
    /// analog here -- `plugin_events` already had one via
    /// [`PluginEventHandle::noop`] before this constructor existed).
    ///
    /// **Deliberately NOT a silent no-op default for `subagents`/`events`,
    /// unlike `ArtifactWriteHandle::noop`.** A `ContextHookCtx` fixture for a
    /// hook that never writes an artifact has nothing to observe on
    /// `artifacts`, so a no-op that discards every write is the right
    /// default there. A `Tool::invoke` unit test is the opposite case far
    /// more often than not -- asserting that a subagent was started
    /// (`conway_fork`/`conway_spawn`) or that a progress event was emitted
    /// is usually the *point* of the test, so silently swallowing both would
    /// make the common case unwritable. This constructor therefore takes
    /// concrete doubles as required parameters instead of defaulting them:
    /// pass `conway_testkit::{FakeSubagentHost, CollectingEventSink}` (or
    /// any other `SubagentHost`/`EventSink` impl) already wrapped in their
    /// own `Arc` -- clone it first if the test wants to inspect it after
    /// `invoke` returns, the same pattern `conway-tools`' own `test_ctx`
    /// helper uses for its `TestHandles`.
    ///
    /// Every OTHER field is defaulted the way a test that doesn't care about
    /// it wants: a fresh `session_id`, an uncancelled `cancel`, `chdir`
    /// seeded from `cwd`, and `plugin_events` a [`PluginEventHandle::noop`]
    /// (a plugin's own custom-event firing is exercised end to end by
    /// `conway-plugin-skeleton`, not by a generic `Tool` fixture) and an
    /// empty `config`. A test that needs a non-default value for any of
    /// those still uses ordinary struct-update syntax: `ToolCtx { cancel:
    /// my_token, ..ToolCtx::for_test(..) }` -- this constructor does not
    /// replace literal construction (see this struct's own doc for why it
    /// stays a plain, non-`#[non_exhaustive]` public-field struct), it only
    /// removes the two fields a third party could not otherwise name a type
    /// for.
    ///
    /// Adds no new name to `conway::plugin`'s curated facade surface --
    /// exactly like `ArtifactWriteHandle::noop`, this is a constructor on a
    /// type the facade already re-exports (`ToolCtx` itself), not a second
    /// top-level export. Unconditional (not gated behind any feature), for
    /// the same reachability reason `ArtifactWriteHandle::noop` is: a
    /// feature gate on this crate is invisible to `conway`'s own dependents
    /// unless the facade forwards it, and gating a constructor that performs
    /// no I/O either way would only reproduce the gap it closes. See
    /// `crate::ports`'s own module doc for why this is a second, no-longer-
    /// unprecedented instance of "kind 2" (a test-fixture constructor,
    /// backing no production call path -- `conway_runtime::tools::runner` is
    /// still the one production construction site, and still builds every
    /// field itself).
    pub fn for_test(
        agent_id: AgentId,
        cwd: PathBuf,
        subagents: Arc<dyn SubagentHost>,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            agent_id,
            session_id: SessionId::new(),
            chdir: CwdHandle::new(cwd.clone()),
            cwd,
            cancel: CancellationToken::new(),
            events,
            subagents: SubagentHandle::new(subagents, agent_id),
            plugin_events: PluginEventHandle::noop("test"),
            config: Arc::new(PluginConfig::default()),
            // A `Tool::invoke` fixture that DOES exercise context-path
            // composition builds its own `ContextPathHandle` (a real
            // `ContextPathHost`, or `conway_testkit`'s fake) and overrides
            // this field via struct-update syntax, the SAME escape hatch
            // `cancel`/`subagents` overrides already use -- see
            // `ContextPathHandle::noop`'s own doc for why the default here
            // is a refusal, not a silent no-op.
            context_path: ContextPathHandle::noop(),
            // Same reasoning, same escape hatch -- a fixture that DOES
            // exercise session discovery builds its own
            // `SessionDiscoveryHandle` (a real host, or a
            // `conway_testkit` fake) and overrides this field via
            // struct-update syntax, mirroring `context_path` immediately
            // above.
            session_discovery: SessionDiscoveryHandle::noop(),
        }
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
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, back);
    }

    /// A manifest predating `optional_host_caps` -- no such key in the JSON
    /// at all -- still parses, defaulting to empty (`#[serde(default)]`,
    /// the same reason `requires`/`optional` predating manifests parse
    /// unmodified).
    #[test]
    fn plugin_manifest_without_optional_host_caps_key_defaults_to_empty() {
        let json = serde_json::json!({
            "id": "builtin.fs",
            "version": "0.1.0",
            "tools": [],
            "required_host_caps": [],
            "requires": [],
            "optional": []
        });
        let manifest: PluginManifest = serde_json::from_value(json).unwrap();
        assert_eq!(manifest.optional_host_caps, Vec::<HostCapability>::new());
    }

    // -----------------------------------------------------------------
    // HostCapability -- open, namespaced vocabulary (Acceptance 1, 5)
    // -----------------------------------------------------------------

    /// `subagent`/`persistent_transport` -- the two core-blessed bare
    /// caps -- still resolve to the SAME unit variants with no
    /// `settings.json` change: constructing via `named` normalizes to
    /// them rather than wrapping them as `Named`.
    #[test]
    fn named_normalizes_known_core_wire_strings_to_the_unit_variants() {
        assert_eq!(
            HostCapability::named("subagent").unwrap(),
            HostCapability::Subagent
        );
        assert_eq!(
            HostCapability::named("persistent_transport").unwrap(),
            HostCapability::PersistentTransport
        );
    }

    /// A bare, previously-unknown name is shape-legal (open vocabulary --
    /// "does anything offer it" is a separate, host-side check this type
    /// does not perform; see `HostCapability`'s own doc).
    #[test]
    fn named_accepts_a_bare_previously_unknown_name() {
        let cap = HostCapability::named("widgets").unwrap();
        assert_eq!(cap, HostCapability::Named("widgets".to_string()));
        assert_eq!(cap.as_wire_str(), "widgets");
    }

    /// A well-formed `plugin_id.cap_name` -- the conventional shape for a
    /// plugin-declared capability -- is accepted, matching the identical
    /// shape `validate_event_name` already accepts for a subscriber-side
    /// event name.
    #[test]
    fn named_accepts_a_well_formed_namespaced_name() {
        let cap = HostCapability::named("acme.ui.ask").unwrap();
        assert_eq!(cap, HostCapability::Named("acme.ui.ask".to_string()));
    }

    /// A malformed tag (empty, or a dot with an empty side) still fails
    /// closed -- opening the vocabulary does not relax shape validity.
    #[test]
    fn named_rejects_malformed_names() {
        assert!(HostCapability::named("").is_err());
        assert!(HostCapability::named(".ask").is_err());
        assert!(HostCapability::named("acme.").is_err());
    }

    /// `HostCapability` serializes/deserializes as a bare string for every
    /// variant, including `Named` -- wire-compatible with the old closed
    /// two-variant form (see the enum's own doc, "Still wire-compatible").
    #[test]
    fn host_capability_named_round_trips_as_a_bare_wire_string() {
        let cap = HostCapability::named("acme.ui.ask").unwrap();
        let json = serde_json::to_value(&cap).unwrap();
        assert_eq!(json, serde_json::json!("acme.ui.ask"));
        let back: HostCapability = serde_json::from_value(json).unwrap();
        assert_eq!(cap, back);
    }

    /// A well-formed but previously-unknown tag on the wire now succeeds
    /// (`Named`) rather than failing closed the way the old closed enum
    /// derive did -- the entire point of opening the vocabulary. A
    /// malformed tag still fails closed at deserialization.
    #[test]
    fn host_capability_deserialize_accepts_unknown_well_formed_tag_rejects_malformed() {
        let ok: Result<HostCapability, _> = serde_json::from_value(serde_json::json!("acme.new"));
        assert_eq!(ok.unwrap(), HostCapability::Named("acme.new".to_string()));

        let err: Result<HostCapability, _> = serde_json::from_value(serde_json::json!(".bad"));
        assert!(err.is_err());
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

    // ---- PluginConfig::narrow ----

    /// A numeric "smaller-or-equal is narrower" rule -- deliberately not
    /// path-shaped, so these tests stay abstract over what "narrower" means
    /// for a key and exercise `narrow`'s own algebra, not a concrete
    /// plugin's `narrows` implementation (that's `conway-tools`'
    /// `FsPlugin` root key's own end-to-end job).
    fn ceiling_narrows(parent: &serde_json::Value, child: &serde_json::Value) -> bool {
        match (parent.as_u64(), child.as_u64()) {
            (Some(p), Some(c)) => c <= p,
            _ => false,
        }
    }

    fn ceiling_rules() -> std::collections::HashMap<String, NarrowingRule> {
        let mut rules = std::collections::HashMap::new();
        rules.insert(
            "acme.limit".to_string(),
            NarrowingRule {
                key: "limit",
                narrows: ceiling_narrows,
            },
        );
        rules
    }

    fn config_with(key: &str, value: u64) -> PluginConfig {
        let mut values = serde_json::Map::new();
        values.insert(key.to_string(), serde_json::json!(value));
        PluginConfig { values }
    }

    #[test]
    fn narrow_with_no_requested_inherits_parent_unchanged() {
        let parent = config_with("acme.limit", 10);
        let child = parent.narrow(None, &ceiling_rules()).unwrap();
        assert_eq!(child, parent);
    }

    #[test]
    fn narrow_accepts_first_time_set_of_a_declared_key_with_no_parent_value() {
        let parent = PluginConfig::default();
        let requested = config_with("acme.limit", 5);
        let child = parent.narrow(Some(&requested), &ceiling_rules()).unwrap();
        assert_eq!(child.values.get("acme.limit").unwrap().as_u64(), Some(5));
    }

    #[test]
    fn narrow_accepts_a_genuine_narrowing() {
        let parent = config_with("acme.limit", 10);
        let requested = config_with("acme.limit", 3);
        let child = parent.narrow(Some(&requested), &ceiling_rules()).unwrap();
        assert_eq!(child.values.get("acme.limit").unwrap().as_u64(), Some(3));
    }

    #[test]
    fn narrow_rejects_widening_with_a_typed_error_not_silently_clamped_or_honored() {
        let parent = config_with("acme.limit", 3);
        let requested = config_with("acme.limit", 10);
        let err = parent
            .narrow(Some(&requested), &ceiling_rules())
            .unwrap_err();
        assert_eq!(
            err,
            PluginConfigError::WouldWiden {
                key: "acme.limit".to_string()
            }
        );
    }

    /// (S1.5 resume gap) `conway_runtime::runtime::root::Runtime::
    /// resume_root` re-validates a resumed session's persisted
    /// `SessionMeta::plugin_config` by calling exactly this method --
    /// `self.loop_deps.plugin_config.narrow(Some(&meta.plugin_config), ..)`
    /// -- with the CURRENT process-wide global config as `self` (the
    /// ceiling) and the PERSISTED value as `requested`. This test proves
    /// that call refuses a genuine widen attempt, using a synthetic
    /// non-empty ceiling: today's `RuntimeDeps` carries no
    /// operator-configurable global `plugin_config` (`Runtime::new` always
    /// builds it as `PluginConfig::default()`), so a `Runtime`-level
    /// integration test cannot yet manufacture a non-empty ceiling to
    /// exercise this exact branch through `resume_root` itself -- see
    /// `crates/conway-runtime/src/runtime/root.rs`'s own doc comment on that
    /// call site, which names this test directly. This unit test closes that
    /// gap by exercising the identical function `resume_root` calls, proving
    /// the branch is real and reachable, not dead code: a future non-empty
    /// global default needs no further change at that call site to be
    /// protected by it.
    #[test]
    fn resuming_a_persisted_plugin_config_wider_than_the_current_global_default_is_refused() {
        // The CURRENT global default (what a brand-new root's own effective
        // config would be) already narrows `acme.limit` to 3 -- e.g. an
        // operator tightened the operator-level config between the process
        // that spawned this session and the process resuming it.
        let current_global_default = config_with("acme.limit", 3);
        // The session's OWN persisted value, written back when a WIDER
        // global default (or no ceiling at all) was in effect -- exactly
        // what a resume path that "reconstructs a root by any route other
        // than the one that validated it" (this item's own hazard
        // language) could otherwise let through unchecked.
        let persisted = config_with("acme.limit", 10);

        let err = current_global_default
            .narrow(Some(&persisted), &ceiling_rules())
            .unwrap_err();
        assert_eq!(
            err,
            PluginConfigError::WouldWiden {
                key: "acme.limit".to_string()
            },
            "a resumed value wider than the current global default must be refused, \
             never silently clamped and never silently honored"
        );
    }

    #[test]
    fn narrow_rejects_a_key_no_plugin_declared_narrowable() {
        let parent = PluginConfig::default();
        let requested = config_with("acme.undeclared", 1);
        let err = parent
            .narrow(Some(&requested), &ceiling_rules())
            .unwrap_err();
        assert_eq!(
            err,
            PluginConfigError::NotNarrowable {
                key: "acme.undeclared".to_string()
            }
        );
    }

    #[test]
    fn narrow_preserves_untouched_keys_from_the_parent() {
        let mut values = serde_json::Map::new();
        values.insert("acme.limit".to_string(), serde_json::json!(10));
        values.insert("acme.other".to_string(), serde_json::json!("kept"));
        let parent = PluginConfig { values };
        let requested = config_with("acme.limit", 2);
        let child = parent.narrow(Some(&requested), &ceiling_rules()).unwrap();
        assert_eq!(child.values.get("acme.limit").unwrap().as_u64(), Some(2));
        assert_eq!(
            child.values.get("acme.other").unwrap().as_str(),
            Some("kept")
        );
    }

    #[test]
    fn default_plugin_declares_no_narrowable_keys() {
        struct NoopPlugin;
        impl Plugin for NoopPlugin {
            fn manifest(&self) -> PluginManifest {
                PluginManifest {
                    id: "acme.noop".into(),
                    version: "0.1.0".into(),
                    tools: vec![],
                    required_host_caps: vec![],
                    optional_host_caps: vec![],
                    requires: vec![],
                    optional: vec![],
                }
            }
            fn tools(&self) -> Vec<Arc<dyn Tool>> {
                Vec::new()
            }
        }
        assert!(NoopPlugin.narrowable_keys().is_empty());
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
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
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
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
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
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
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

    // ---- ToolCtx::for_test ----

    /// Records every `start` call; every other method is a fixed no-op or
    /// terminates immediately, per the trait's own always-terminates
    /// contract. Just enough to prove `ToolCtx::for_test` wires the
    /// caller-supplied `Arc<dyn SubagentHost>` through untouched -- a
    /// general-purpose scripted fixture is `conway-testkit`'s job
    /// (`FakeSubagentHost`), not this crate's own (T1: this crate depends
    /// on no workspace crate, `conway-testkit` included, so it cannot reuse
    /// that one instead of a second, narrower double).
    #[derive(Default)]
    struct RecordingSubagentHost {
        started: std::sync::Mutex<Vec<(AgentId, AgentId)>>,
    }

    #[async_trait]
    impl SubagentHost for RecordingSubagentHost {
        async fn start(
            &self,
            caller: AgentId,
            parent: AgentId,
            _spec: crate::agent::SubagentSpec,
        ) -> Result<AgentId, crate::error::RuntimeError> {
            self.started
                .lock()
                .expect("started lock poisoned")
                .push((caller, parent));
            Ok(AgentId::new())
        }

        async fn steer(
            &self,
            _caller: AgentId,
            _target: AgentId,
            _text: String,
        ) -> Result<(), crate::error::RuntimeError> {
            Ok(())
        }

        async fn await_result(
            &self,
            _caller: AgentId,
            target: AgentId,
        ) -> Result<crate::agent::AgentResult, crate::error::RuntimeError> {
            Err(crate::error::RuntimeError::AgentNotFound { agent: target })
        }

        async fn cancel(
            &self,
            _caller: AgentId,
            _target: AgentId,
            _reason: String,
            _mode: crate::agent::CancelMode,
        ) -> Result<(), crate::error::RuntimeError> {
            Ok(())
        }

        fn tree(&self, caller: AgentId) -> crate::agent::AgentTreeSnapshot {
            crate::agent::AgentTreeSnapshot {
                root: caller,
                nodes: Vec::new(),
                at: chrono::Utc::now(),
            }
        }

        async fn ask(
            &self,
            _caller: AgentId,
            _parent: AgentId,
            _spec: crate::agent::SubagentSpec,
        ) -> Result<crate::agent::AskOutcome, crate::error::RuntimeError> {
            Ok(crate::agent::AskOutcome {
                text: "recorded".into(),
                usage: crate::content::Usage::default(),
                status: crate::agent::ResultStatus::Completed,
                transcript_ref: SessionId::new(),
            })
        }
    }

    /// Collects every emitted [`crate::event::Event`] -- the `ToolCtx.events`
    /// counterpart to [`RecordingSubagentHost`] above, same reasoning (T1)
    /// for why this crate defines its own narrow double rather than reusing
    /// `conway-testkit::CollectingEventSink`.
    #[derive(Default)]
    struct RecordingEventSink {
        events: std::sync::Mutex<Vec<crate::event::Event>>,
    }

    impl EventSink for RecordingEventSink {
        fn emit(&self, event: crate::event::Event) {
            self.events
                .lock()
                .expect("events lock poisoned")
                .push(event);
        }
    }

    /// The load-bearing property this constructor exists for: a third party
    /// can build a fully-wired `ToolCtx` from nothing but an `AgentId`, a
    /// `cwd`, and two `Arc`s it already had to construct anyway (its own
    /// `SubagentHost`/`EventSink` doubles) -- no hand-rolled `CwdHandle`/
    /// `SubagentHandle` assembly, and the recording doubles it passed in are
    /// observable afterward through the clones it kept.
    #[test]
    fn for_test_wires_the_supplied_doubles_through_untouched() {
        let agent_id = AgentId::new();
        let subagents = Arc::new(RecordingSubagentHost::default());
        let events = Arc::new(RecordingEventSink::default());

        let ctx = ToolCtx::for_test(
            agent_id,
            PathBuf::from("/tmp/x"),
            subagents.clone(),
            events.clone(),
        );

        assert_eq!(ctx.agent_id, agent_id);
        assert_eq!(ctx.cwd, PathBuf::from("/tmp/x"));
        assert_eq!(ctx.chdir.current(), PathBuf::from("/tmp/x"));
        assert!(!ctx.cancel.is_cancelled());

        block_on(ctx.subagents.start(crate::agent::SubagentSpec::fork(
            "do it",
            crate::agent::Budget::default(),
        )))
        .unwrap();
        assert_eq!(
            subagents
                .started
                .lock()
                .expect("started lock poisoned")
                .len(),
            1
        );

        ctx.events.emit(crate::event::Event::ToolProgress {
            call_id: "tc_1".into(),
            note: "hi".into(),
        });
        assert_eq!(events.events.lock().expect("events lock poisoned").len(), 1);
    }

    /// `for_test` bakes `agent_id` into the returned `SubagentHandle`
    /// exactly like a real construction site would -- `ctx.subagents.start`
    /// has no caller-supplied `parent`/`caller` to override it (see
    /// `SubagentHandle`'s own doc).
    #[test]
    fn for_test_bakes_agent_id_into_the_subagent_handle() {
        let agent_id = AgentId::new();
        let subagents = Arc::new(RecordingSubagentHost::default());
        let ctx = ToolCtx::for_test(
            agent_id,
            PathBuf::from("/tmp/x"),
            subagents.clone(),
            Arc::new(RecordingEventSink::default()),
        );

        block_on(ctx.subagents.start(crate::agent::SubagentSpec::fork(
            "do it",
            crate::agent::Budget::default(),
        )))
        .unwrap();

        let started = subagents
            .started
            .lock()
            .expect("started lock poisoned")
            .clone();
        assert_eq!(started, vec![(agent_id, agent_id)]);
    }
}
