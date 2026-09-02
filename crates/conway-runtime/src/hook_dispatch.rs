//! Hook dispatch for every event OUTSIDE the permission broker, in THREE
//! tiers with deliberately different failure/edit postures.
//!
//! # The three tiers
//!
//! **Observation-only**: `post_tool_use`, `session_starting`,
//! `child_spawned`, `child_reported`. These cannot say no and cannot edit
//! anything, and that is the whole point of them — an operator wanting a log
//! line per command, or a notification when a child starts, needs to be
//! told a thing happened and nothing more. Each fires at a place that
//! already knows the moment has passed. [`HookDispatcher::dispatch`] serves
//! them and returns `()`, discarding whatever the hook answered.
//! `child_reported` fires for BOTH a normal completion (`AgentLoop::
//! finish`) and a supervisor-synthesized terminal result (a panic, or a
//! task unresponsive past `supervisor::DEFAULT_GRACE` -- `supervisor.rs`),
//! because a hook that only sees the happy path is misleading about what "a
//! child reporting" means.
//!
//! `child_spawned` itself is dispatched (`crate::subagent::SubagentHost::
//! start`) for BOTH `SubagentMode::Fork` and `SubagentMode::Spawn`,
//! unconditionally and unchanged -- that firing site does not discriminate.
//! [`HookSpec::spawn_only`] is a PER-HOOK, subscriber-side narrowing on top
//! of that (board item `01M129Y98V4C1050QBPPMY37X0`): a hook that sets it
//! only ever sees the `Spawn` occurrences. The default (`false`, every
//! hook before this field existed) is unaffected -- an operator-authored
//! `[hooks].rules[]` entry, or any OTHER plugin's own `child_spawned`
//! subscription, still sees every child, fork included, exactly as before.
//!
//! **Context-editing**: `request_assembled`/`context_overflow` (board item
//! `01KZRZZP6A4A27R3EN0HQAENBS`). [`HookDispatcher::dispatch_context`]
//! serves them: like `dispatch`, a broken hook cannot fail the turn (fails
//! OPEN, per hook, with a `tracing::warn!` -- see that method's own doc),
//! but UNLIKE `dispatch`, a successful answer's `HookAnswer.context`
//! (`conway_core::hook::ContextDelta`) is read and returned for the caller
//! (`crate::agent_loop`) to apply append-only
//! (`crate::context::script_hook::apply_script_deltas`) through the SAME
//! tool-call/result coherence guard the Rust `ContextHook` path already
//! goes through. `request_assembled` sits at the seam `ContextHook::
//! before_request` (`agent_loop.rs`) already edits the assembled payload
//! at, and now composes with it rather than merely observing it (§16.3's
//! union rule, no chaining) -- widening `HookAnswer`'s vocabulary being
//! read here is additive relative to every existing rule that never set
//! `context` (an empty `ContextDelta` applies as a no-op), never a breaking
//! change to what shipped before this item.
//!
//! **Deny-capable but never modifying**:
//! `prompt_submitted`. It fires BEFORE the prompt is processed, so a hook
//! here CAN refuse — but it may never rewrite a word of what the user typed.
//! [`HookDispatcher::dispatch_deny_only`] serves it and returns
//! `Option<String>`: the denial reason, or `None` to proceed.
//!
//! **The module is named for the dispatch, not for one tier**, because a
//! module called `observation` hosting a deny-capable AND a context-editing
//! tier would be a declaration that does not match what it contains.
//!
//! # Why this is not on `PermissionBroker`
//!
//! `pre_tool_use` dispatch lives on the broker
//! (`PermissionBroker::pre_tool_use_hook_denial`, private to that module) because
//! its answer changes a permission decision. Nothing here can change any
//! decision, so hosting it there would put an observation path inside the
//! type whose job is authorization — and the whole hazard this module guards
//! against is an observation event acquiring denial-shaped side effects by
//! proximity.
//!
//! # The two tiers fail in OPPOSITE directions, on purpose
//!
//! Observation-only fails OPEN. A hook that errors, times out, or returns
//! garbage produces a `tracing::warn` and nothing else. A failing `post_tool_use` hook must not
//! fail the tool call it observed; a failing `child_spawned` hook must not
//! fail the spawn. [`HookDispatcher::dispatch`] therefore returns `()`
//! rather than a `Result` — the failure-does-not-propagate property is
//! structural, not a discipline a future caller has to remember, and there is
//! no value a caller could accidentally `?` on.
//!
//! Contrast `pre_tool_use`, which fails CLOSED: there, a broken hook denying
//! the call is the safe direction. Here the observed thing has already
//! happened (or, for `child_spawned`, has already been decided), so failing
//! closed would mean breaking a working operation because a log script was
//! misconfigured. The two events differ in kind, and the difference is
//! deliberate rather than an inconsistency.
//!
//! `prompt_submitted` fails CLOSED, like `pre_tool_use` and for the same
//! reason: it fires before anything has happened, so refusing on a broken
//! hook denies an action rather than breaking a completed one. A missing,
//! timing-out or unparseable script denies the prompt.
//!
//! # `prompt_submitted` may never touch the text
//!
//! the extension design forbids any participant, hook
//! included, from editing a user's submitted prompt. That is stricter than
//! the tool-call-arguments rule elsewhere: there, the argument against
//! rewriting rests partly on a human having approved a specific rendered
//! string, and here there is no equivalent approval step to fall back on.
//! The user's own words are the one thing in the pipeline nothing gets to
//! launder.
//!
//! **That is enforced by the TYPE, not by not wiring a path.**
//! [`dispatch_deny_only`](HookDispatcher::dispatch_deny_only) reads only
//! [`HookPermissionVerdict`], whose entire vocabulary is `NoOpinion` and
//! `Deny { reason }` — there is no variant and no field capable of carrying
//! replacement text back. `HookAnswer::context` is ignored here and
//! documented as ignored: a `ContextDelta` is about ASSEMBLED CONTEXT
//! (`request_assembled`/`context_overflow`'s own tier above), never about
//! the raw submitted prompt this method dispatches for -- `dispatch_deny_only`
//! reads `HookPermissionVerdict` only, structurally incapable of reaching
//! `context` at all. The `reason` on a denial is surfaced to the CALLER as
//! an error; it is never substituted for the prompt.
//!
//! # `child_spawned` and denial — an open question, deliberately deferred
//!
//! Nothing structurally forces `child_spawned` to be observe-only the way
//! `post_tool_use` is (there, the call has already run). A spawn COULD in
//! principle be refused. It is shipped observe-only anyway, because refusing
//! raises questions did not scope: what
//! does the parent agent see when its own spawn is denied — a tool error, a
//! silent no-op? Does the caller need new error handling? Answering those by
//! accident, in the shape of a return type, is exactly the trap
//! `PluginManifest` avoided by refusing to carry an `on_init` nobody had
//! wired: an unwired lifecycle hook costs an implementer a debugging session.
//! **If that question is settled later, the change is deliberate and visible;
//! it is not settled here by omission.**
//!
//! # Plugin-declared events
//!
//! `PHILOSOPHY.md` §5 is explicit that the event vocabulary above is open,
//! not closed: "A plugin declares the events it emits... Those events sit
//! at the same level as the ones conway emits." A plugin's own
//! `Plugin::events` declares zero or more [`EventDecl`]s;
//! [`declared_plugin_events`] namespaces and validates them
//! (`plugin_id.bare_name`, via [`validate_event_name`] -- the SAME shared
//! validator `conway_cli::tui::commands::CommandRegistry::build` already
//! uses for plugin-declared TUI commands); `ConwayBuilder::build` unions
//! the result into what [`HookDispatcher::dispatch`] will actually fire
//! for.
//!
//! **One dispatch path, deliberately, not a second tier.** A plugin-declared
//! event is dispatched through the IDENTICAL observation-only
//! [`HookDispatcher::dispatch`] every core observation event above already
//! uses -- fails open, cannot deny. This module's own `impl
//! PluginEventEmitter for HookDispatcher` (below) IS the fan-out layer a
//! plugin's own `PluginEventHandle::emit` call reaches. Building a second,
//! deny-capable tier for plugin events -- something PHILOSOPHY's
//! routing/compaction examples could plausibly want ("a routing plugin can
//! offer a point before it commits to a candidate") -- is explicitly left
//! for a later item to justify with a real consumer, not built ahead of one
//! (this item's own YAGNI).
//!
//! **The one structural difference from a core event: WHO fires it.** Every
//! core event above dispatches from a fixed seam in this workspace's own
//! code (`ToolRunner`, `Runtime::start_root`, `SubagentHost::start`, ...).
//! A plugin-declared event has no such seam -- only the plugin's own code
//! knows when "before committing to a candidate" happens -- so it fires
//! from inside that plugin's own [`conway_core::ports::Tool::invoke`],
//! through the [`conway_core::ports::PluginEventHandle`] threaded onto its
//! [`conway_core::ports::ToolCtx`], bound to that tool's own declaring
//! plugin id so it can never fire under a different plugin's namespace
//! (see that type's own doc).

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use conway_core::event_name::{validate_event_name, EVENT_NAMESPACE_SEPARATOR};
use conway_core::hook::{
    tool_matcher_matches, HookEvent, HookInvocation, HookOrigin, HookPermissionVerdict,
};
use conway_core::ports::{EventDecl, HookRunner, Plugin, PluginEventEmitter};

/// One configured observation hook: the operator's own id for it, plus what
/// to spawn and how long it may take.
///
/// Deliberately the same shape as
/// [`crate::permission::PreToolUseHookSpec`] rather than a reuse of it: the
/// two are translated from the same `[hooks].rules[]` config by
/// `ConwayBuilder::build`, but they are consumed by different tiers for
/// different purposes, and collapsing them would invite a future field that
/// only makes sense for one to appear on both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookSpec {
    /// The rule's `HookEntry::id`, used to name the hook in a warning so an
    /// operator can tell WHICH script failed rather than only that one did.
    pub id: String,
    pub command: Vec<String>,
    pub timeout_ms: u64,
    /// The rule's `HookEntry::match_tool` , carried through untouched. `None`
    /// (the config-default) fires this hook for every event it is
    /// subscribed to, unchanged from before this field existed. `Some`
    /// only ever NARROWS which of `event`'s occurrences invoke this hook --
    /// see `Self::applies_to`.
    ///
    /// Only meaningful for [`POST_TOOL_USE`]: it is the only event this
    /// tier dispatches whose payload names a tool
    /// (`crates/conway-runtime/src/tools/runner.rs`'s `"tool"` key). For
    /// every OTHER event this tier dispatches, `merge::validate` refuses to
    /// load a config that set `match` on that rule in the first place, so a
    /// `Some` here for a non-`post_tool_use` event is a state the loader
    /// already rejected -- `Self::applies_to` handles it defensively
    /// anyway (never matches, rather than panicking or matching everything)
    /// for any caller that constructs a `HookSpec` directly rather than
    /// through the loader (e.g. this module's own tests).
    pub matcher: Option<String>,
    /// Where this rule came from -- see `conway_core::hook::HookOrigin`'s
    /// own doc (board item `01M129QW0GV90QTQS6B3BY3DAR`), and
    /// `crate::permission::PreToolUseHookSpec::origin`'s identical field
    /// for the sibling tier. Defaults to [`HookOrigin::Operator`], so every
    /// construction site that predates this field keeps reporting exactly
    /// what it always implicitly was.
    pub origin: HookOrigin,
    /// Narrows this hook to fire only when [`CHILD_SPAWNED`]'s own payload
    /// names a [`conway_core::agent::SubagentMode::Spawn`] child -- the
    /// SAME field, carried straight through, as
    /// `conway_core::ports::PluginHookRule::spawn_only`; see that field's
    /// own doc for the full rationale (board item
    /// `01M129Y98V4C1050QBPPMY37X0`). `false` (the default every
    /// construction site that predates this field keeps) fires for every
    /// mode, unchanged. Meaningless for every event OTHER than
    /// [`CHILD_SPAWNED`] (no other dispatched event's payload carries a
    /// `"mode"` field): `HookSpec::applies_to` (private) treats a payload
    /// with no parseable `"mode"` as never matching, the identical
    /// defensive fallback [`HookSpec::matcher`] already has for a toolless
    /// payload.
    pub spawn_only: bool,
}

impl HookSpec {
    /// Whether this hook should run for `payload`: `true` when
    /// [`Self::spawn_only`] is unset or satisfied AND [`Self::matcher`] is
    /// unset or satisfied -- both narrowing dimensions must agree; either
    /// one can veto.
    ///
    /// **`spawn_only`**: `false` always passes (the default, unconditional
    /// fire). `true` requires `payload`'s own `"mode"` field to parse as
    /// [`conway_core::agent::SubagentMode`] and equal `Spawn` exactly --
    /// sourced from the real enum, never a re-typed `"spawn"` string
    /// literal, so a future third `SubagentMode` variant cannot silently
    /// mismatch here (P-14). A payload with no `"mode"` field, or one that
    /// fails to parse, never matches -- fails closed on the FILTER (this
    /// hook simply does not run), not on the spawn itself: `dispatch`'s own
    /// observation-only posture is untouched.
    ///
    /// **`matcher`**: `None` always passes. `Some(pattern)` requires
    /// `payload`'s `"tool"` string field to satisfy it
    /// (`conway_core::hook::tool_matcher_matches`) -- a matcher set on a
    /// payload with no `"tool"` field at all (every dispatched event except
    /// [`POST_TOOL_USE`]) never matches -- see this field's own doc for why
    /// that state should not occur past config load.
    fn applies_to(&self, payload: &serde_json::Value) -> bool {
        if self.spawn_only {
            let mode = payload.get("mode").and_then(|v| {
                serde_json::from_value::<conway_core::agent::SubagentMode>(v.clone()).ok()
            });
            if mode != Some(conway_core::agent::SubagentMode::Spawn) {
                return false;
            }
        }
        match &self.matcher {
            None => true,
            Some(pattern) => payload
                .get("tool")
                .and_then(|v| v.as_str())
                .is_some_and(|tool| tool_matcher_matches(pattern, tool)),
        }
    }
}

/// The event name every `post_tool_use` subscription is keyed on.
pub const POST_TOOL_USE: &str = "post_tool_use";
/// The event name every `session_starting` subscription is keyed on.
pub const SESSION_STARTING: &str = "session_starting";
/// The event name every `child_spawned` subscription is keyed on.
pub const CHILD_SPAWNED: &str = "child_spawned";
/// The event name every `request_assembled` subscription is keyed on
///. Fired once per turn by
/// `AgentLoop::run_inner`, after `ContextBuilder::build` (and, if
/// registered, `ContextHook::before_request`'s own edit) and before that
/// turn's route/attempt call.
///
/// **CONTEXT-EDITING, not observation-only, as of board item
/// `01KZRZZP6A4A27R3EN0HQAENBS`.** A subscriber's `HookAnswer.context`
/// (`conway_core::hook::ContextDelta`) is now read, via
/// [`HookDispatcher::dispatch_context`] rather than [`HookDispatcher::
/// dispatch`] -- the discard-on-purpose method every OTHER event in
/// [`OBSERVATION_EVENTS`] still uses. A subscriber that never sets
/// `context` (the default: absent stdout, or an answer that only sets
/// `permission`, which this event still ignores) observes byte-identical
/// behavior to before this item, so existing logging-only `request_assembled`
/// rules are unaffected. See `crate::context::script_hook`'s module doc for
/// the append-only application logic, and `docs/plugins/hooks.md` point 3/13
/// for the normative contract.
pub const REQUEST_ASSEMBLED: &str = "request_assembled";
/// The event name every `context_overflow` subscription is keyed on (board
/// item `01KZRZZP6A4A27R3EN0HQAENBS`) -- the script-hook counterpart of
/// `ContextHook::on_overflow`. Fired from `AgentLoop::route_and_attempt`,
/// at the SAME call site and under the SAME trigger boundary the Rust
/// `on_overflow` seam already enforces: only when routing/the attempt
/// engine rejects with `RoutingError::ContextTooLarge` (every candidate
/// failed SOLELY on headroom), never for a mixed `RoutingError::
/// NoCandidate` rejection -- `docs/plugins/hooks.md` point 4's own
/// disclosed boundary, unchanged and unwidened by this event's addition.
/// CONTEXT-EDITING, like [`REQUEST_ASSEMBLED`] above -- see that constant's
/// own doc.
pub const CONTEXT_OVERFLOW: &str = "context_overflow";
/// The event name every `child_reported` subscription is keyed on (board
/// item). Fired for every terminal `AgentResult`
/// that crosses back to a parent -- both a normal completion
/// (`AgentLoop::finish`) and a supervisor-synthesized one (`supervisor.rs`,
/// a panic or a task unresponsive past `supervisor::DEFAULT_GRACE`) --
/// gated on the SAME publish-race winner as `Event::AgentFinished` at each
/// site, so this fires exactly once per agent, from whichever side won.
/// Never fires for a ROOT agent's own finish: a root has no parent for a
/// result to cross back to.
pub const CHILD_REPORTED: &str = "child_reported";

/// The deny-capable-but-never-modifying event. Fires at both prompt-submission call sites.
pub const PROMPT_SUBMITTED: &str = "prompt_submitted";

/// Every event this tier dispatches through the discard-the-answer
/// [`HookDispatcher::dispatch`] -- fire-and-forget, cannot deny, cannot
/// edit. **`REQUEST_ASSEMBLED` is deliberately NOT a member as of board item
/// `01KZRZZP6A4A27R3EN0HQAENBS`** -- it moved to the context-editing tier
/// ([`HookDispatcher::dispatch_context`]; see that constant's own doc). It
/// stays fail-open (a broken hook cannot fail the turn it would have
/// observed) but is no longer discard-only.
pub const OBSERVATION_EVENTS: &[&str] = &[
    POST_TOOL_USE,
    SESSION_STARTING,
    CHILD_SPAWNED,
    CHILD_REPORTED,
];

/// Every event whose `HookAnswer.context` is read and applied append-only
/// (decision `01KZRZAFD8T3GX407MZC8P1W1E`), via [`HookDispatcher::
/// dispatch_context`] rather than [`HookDispatcher::dispatch`]. Still fails
/// OPEN like [`OBSERVATION_EVENTS`] (a broken hook leaves the payload
/// unchanged rather than failing the turn) -- editing capability and
/// deny capability are independent axes; this tier has the former, not the
/// latter.
pub const CONTEXT_EDITING_EVENTS: &[&str] = &[REQUEST_ASSEMBLED, CONTEXT_OVERFLOW];

/// Every event this module dispatches at all, observation, context-editing,
/// and deny-capable alike -- what `ConwayBuilder::build` filters
/// `[hooks].rules[]` against. Both dispatch tiers above share ONE
/// subscription map (`HookDispatcher::hooks`) and one config surface
/// (`[hooks].rules[]`) -- which method reads a given event's answer is
/// decided by the DISPATCH CALL SITE (`crate::agent_loop`), never by a
/// per-event flag on the stored `HookSpec` -- so this list is a plain union,
/// not something a caller needs to split by tier itself.
pub const DISPATCHED_EVENTS: &[&str] = &[
    POST_TOOL_USE,
    SESSION_STARTING,
    CHILD_SPAWNED,
    REQUEST_ASSEMBLED,
    CHILD_REPORTED,
    CONTEXT_OVERFLOW,
    PROMPT_SUBMITTED,
];

/// Every event whose payload never carries a `"tool"` name -- what
/// `crates/conway/src/config/merge.rs`'s hooks check refuses to let a rule
/// pair `match` with. `pre_tool_use` (this module does not dispatch it --
/// see `crate::permission::PermissionBroker`) belongs on this same
/// semantic list; it is enumerated separately by that check because it is
/// not one of THIS module's own dispatched events.
pub const EVENTS_WITHOUT_TOOL_NAME: &[&str] = &[
    SESSION_STARTING,
    CHILD_SPAWNED,
    REQUEST_ASSEMBLED,
    CHILD_REPORTED,
    CONTEXT_OVERFLOW,
    PROMPT_SUBMITTED,
];

/// Holds the injected [`HookRunner`] and the per-event subscription lists,
/// and invokes them.
///
/// Shared by `Arc` between [`crate::runtime::Runtime`] and
/// [`crate::tools::ToolRunner`] — `ToolRunner::new` constructs one and the
/// runtime reads it back via [`crate::tools::ToolRunner::hooks`], so
/// both see the same interior-mutable state and `ToolRunner::new` keeps its
/// existing arity (five test call sites construct it directly).
///
/// **With no runner injected and no hooks configured, every `dispatch` is a
/// byte-for-byte no-op** — it returns before building a payload. That is the
/// default for every consumer that never calls the setters, matching
/// `PermissionBroker::set_hook_runner`'s own contract.
///
/// `Debug` is hand-written: `dyn HookRunner` is not `Debug` (it is a port
/// implemented outside this crate), and deriving would leak that requirement
/// onto every implementor. Reports whether a runner is installed and the
/// subscribed event names, which is what a debug print is wanted for.
#[derive(Default)]
pub struct HookDispatcher {
    runner: RwLock<Option<Arc<dyn HookRunner>>>,
    hooks: RwLock<BTreeMap<String, Vec<HookSpec>>>,
}

impl std::fmt::Debug for HookDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookDispatcher")
            .field(
                "runner_installed",
                &self
                    .runner
                    .read()
                    .expect("observation runner lock poisoned")
                    .is_some(),
            )
            .field(
                "events",
                &self
                    .hooks
                    .read()
                    .expect("observation hooks lock poisoned")
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl HookDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Injects (or clears, via `None`) the runner every observation event is
    /// invoked through. Mirrors
    /// [`crate::permission::PermissionBroker::set_hook_runner`]: not called
    /// at all is the default, and leaves every dispatch a no-op.
    pub fn set_runner(&self, runner: Option<Arc<dyn HookRunner>>) {
        *self
            .runner
            .write()
            .expect("observation runner lock poisoned") = runner;
    }

    /// Replaces the subscription lists wholesale, keyed by event name.
    ///
    /// Called once by `ConwayBuilder::build` from `[hooks].rules[]` filtered
    /// to the enabled rules whose `event` is one of [`OBSERVATION_EVENTS`].
    /// An event with no subscribers, or an absent key, is the same no-op as
    /// no runner at all.
    pub fn set_hooks(&self, hooks: BTreeMap<String, Vec<HookSpec>>) {
        *self.hooks.write().expect("observation hooks lock poisoned") = hooks;
    }

    /// The whole subscription map, cloned -- the review-list counterpart of
    /// [`Self::set_hooks`]. Cloning
    /// the WHOLE map, not one event's list, is deliberate: `Self::set_hooks`
    /// replaces every event's subscriptions wholesale, so a caller that
    /// wants to remove one hook from one event (e.g. `prompt_submitted`,
    /// the deny-capable event this item's own review list revokes) must
    /// read back every OTHER event's subscriptions too, mutate only the one
    /// list it means to change, and write the whole map back -- otherwise
    /// every sibling event's hooks would be silently dropped by the
    /// wholesale replace.
    pub fn hooks_snapshot(&self) -> BTreeMap<String, Vec<HookSpec>> {
        self.hooks
            .read()
            .expect("observation hooks lock poisoned")
            .clone()
    }

    /// True when `event` has at least one subscriber AND a runner exists —
    /// i.e. when [`Self::dispatch`] would actually spawn something. Lets a
    /// caller skip assembling a payload it would only throw away.
    ///
    /// **Per-EVENT, not per-matcher.** A subscriber whose
    /// [`HookSpec::matcher`] would reject the specific call this event fires
    /// for still counts as "at least one subscriber" here -- this method has
    /// no `tool` argument to check a matcher against, only `event`'s name.
    /// [`Self::dispatch`] itself is where a matcher can still decide, per
    /// hook, to run nothing; this remains a coarse, cheap precheck, not a
    /// guarantee that a spawn follows.
    pub fn will_dispatch(&self, event: &str) -> bool {
        self.runner
            .read()
            .expect("observation runner lock poisoned")
            .is_some()
            && self
                .hooks
                .read()
                .expect("observation hooks lock poisoned")
                .get(event)
                .is_some_and(|h| !h.is_empty())
    }

    /// Invokes every hook subscribed to `event` whose [`HookSpec::matcher`]
    /// is unset or satisfied by
    /// `payload`'s `"tool"` field, in configured order.
    ///
    /// **Returns `()` on purpose** — see the module doc. A hook that fails,
    /// times out, or returns an unparseable answer is logged and skipped; the
    /// next hook still runs, and the caller is never told. Any `permission`
    /// or `context` field in an answer is ignored here: only `pre_tool_use`
    /// reads the former, and no observation event may edit context.
    pub async fn dispatch(&self, event: &str, payload: serde_json::Value) {
        let Some(runner) = self
            .runner
            .read()
            .expect("observation runner lock poisoned")
            .clone()
        else {
            return;
        };
        let hooks = self
            .hooks
            .read()
            .expect("observation hooks lock poisoned")
            .get(event)
            .cloned()
            .unwrap_or_default();

        for hook in hooks.iter().filter(|hook| hook.applies_to(&payload)) {
            let invocation = HookInvocation::new(
                hook.command.clone(),
                hook.timeout_ms,
                HookEvent::new(event, payload.clone()),
            );
            if let Err(failure) = runner.run(&invocation).await {
                // The whole failure posture of this tier, in one place: warn
                // and carry on. Never `?`, never a return value the caller
                // could mistake for a verdict.
                tracing::warn!(
                    event = event,
                    hook = hook.id.as_str(),
                    "observation hook failed; the observed operation is unaffected: {failure}"
                );
            }
        }
    }

    /// Invokes every hook subscribed to `event` and returns the FIRST denial,
    /// or `None` to proceed. Serves `prompt_submitted`.
    ///
    /// **Fails CLOSED**, unlike [`Self::dispatch`]: a hook that errors, times
    /// out, or returns an unparseable answer denies, because this fires before
    /// anything has happened and refusing is the safe direction there. A
    /// verdict this build does not recognize denies too, via
    /// [`conway_core::hook::HookPermissionVerdict::denies`] -- the same
    /// judgment `PermissionBroker::pre_tool_use_hook_denial` applies,
    /// shared rather than re-derived.
    ///
    /// **Reads only [`HookPermissionVerdict`], which structurally cannot carry
    /// replacement text** -- see the module doc. `HookAnswer::context` is
    /// deliberately IGNORED: a `ContextDelta` describes assembled context, not
    /// the submitted prompt, and honouring one here would be a text-editing
    /// path arriving through a side door.
    ///
    /// Order-independent for the boolean outcome: a deny beats a no-opinion
    /// however many hooks run, so which hook is consulted first only changes
    /// whose `reason` is reported.
    pub async fn dispatch_deny_only(
        &self,
        event: &str,
        payload: serde_json::Value,
    ) -> Option<String> {
        let runner = self
            .runner
            .read()
            .expect("observation runner lock poisoned")
            .clone()?;
        let hooks = self
            .hooks
            .read()
            .expect("observation hooks lock poisoned")
            .get(event)
            .cloned()
            .unwrap_or_default();

        for hook in &hooks {
            let invocation = HookInvocation::new(
                hook.command.clone(),
                hook.timeout_ms,
                HookEvent::new(event, payload.clone()),
            );
            match runner.run(&invocation).await {
                // NOTE the binding: only `answer.permission` is read. There is
                // deliberately no `answer.context` arm -- see the module doc.
                // `HookPermissionVerdict::denies` is the single
                // implementation of the fail-closed-on-unrecognized-variant
                // judgment (see that method's own doc); this is one of its
                // two callers, alongside
                // `permission::PermissionBroker::pre_tool_use_hook_denial`.
                Ok(answer) => {
                    if answer.permission.denies() {
                        let reason = match &answer.permission {
                            HookPermissionVerdict::Deny { reason } => reason.clone(),
                            _ => "unrecognized permission verdict -- fail-closed".to_string(),
                        };
                        return Some(format!("`{event}` hook `{}`: {reason}", hook.id));
                    }
                }
                Err(failure) => {
                    return Some(format!(
                        "`{event}` hook `{}` failed ({failure}) -- fail-closed",
                        hook.id
                    ));
                }
            }
        }
        None
    }

    /// Invokes every hook subscribed to `event` (one of
    /// [`CONTEXT_EDITING_EVENTS`]) and collects each one's
    /// `HookAnswer.context` (`conway_core::hook::ContextDelta`) rather than
    /// discarding it -- the ONE place in this module that reads that field.
    ///
    /// **Fails OPEN per hook, visibly, matching [`Self::dispatch`]'s own
    /// posture, never [`Self::dispatch_deny_only`]'s.** A hook that errors,
    /// times out, or returns unparseable stdout contributes nothing --
    /// there is no `Err`/denial for the whole call, only a per-hook
    /// [`ContextHookFailure`] alongside a `tracing::warn!` (the SAME
    /// channel [`Self::dispatch`]'s own failing-hook branch already uses,
    /// so an operator watching for one already watches for both). The
    /// caller (`crate::agent_loop`) applies whatever DID answer through
    /// `crate::context::script_hook::apply_script_deltas` and runs the SAME
    /// tool-call/result coherence guard `crate::context::hook_guard`
    /// already enforces for the Rust `ContextHook` path -- there is no
    /// second, unguarded way for an edited payload to reach a request.
    ///
    /// **A hook with nothing to say (empty stdout, or an answer that never
    /// set `context`) still produces an [`ContextHookAnswer`] carrying the
    /// default, empty `ContextDelta`** -- applying it is a no-op
    /// (`apply_script_deltas` appends/excludes nothing), so an existing
    /// `request_assembled` rule written purely for observation (its answer
    /// never touches `context`) is unaffected by this method reading a
    /// field [`Self::dispatch`] used to discard.
    pub async fn dispatch_context(
        &self,
        event: &str,
        payload: serde_json::Value,
    ) -> ContextDispatchOutcome {
        let mut outcome = ContextDispatchOutcome::default();
        let Some(runner) = self
            .runner
            .read()
            .expect("observation runner lock poisoned")
            .clone()
        else {
            return outcome;
        };
        let hooks = self
            .hooks
            .read()
            .expect("observation hooks lock poisoned")
            .get(event)
            .cloned()
            .unwrap_or_default();

        for hook in hooks.iter().filter(|hook| hook.applies_to(&payload)) {
            let invocation = HookInvocation::new(
                hook.command.clone(),
                hook.timeout_ms,
                HookEvent::new(event, payload.clone()),
            );
            match runner.run(&invocation).await {
                Ok(answer) => outcome.answers.push(ContextHookAnswer {
                    hook_id: hook.id.clone(),
                    delta: answer.context,
                }),
                Err(failure) => {
                    // The VISIBLE failure signal the acceptance asks for --
                    // the identical channel `Self::dispatch`'s own failing
                    // branch already uses, plus the structured record below
                    // a caller can act on further (e.g. surface as an
                    // `Event`).
                    tracing::warn!(
                        event = event,
                        hook = hook.id.as_str(),
                        "context hook failed; the pre-hook payload is left unchanged for this \
                         hook: {failure}"
                    );
                    outcome.failures.push(ContextHookFailure {
                        hook_id: hook.id.clone(),
                        reason: failure.to_string(),
                    });
                }
            }
        }
        outcome
    }
}

/// One [`CONTEXT_EDITING_EVENTS`] hook's successful answer, tagged with the
/// [`HookSpec::id`] that produced it. The tag is what makes
/// `crate::context::script_hook::apply_script_deltas`'s appended segments
/// attributable -- every context segment a script hook appends carries this
/// id in its own provenance, per the ACCEPTANCE "every segment a script
/// appends carries provenance naming the hook's configured id."
#[derive(Clone, Debug, PartialEq)]
pub struct ContextHookAnswer {
    pub hook_id: String,
    pub delta: conway_core::hook::ContextDelta,
}

/// One [`CONTEXT_EDITING_EVENTS`] hook that did NOT answer -- errored, timed
/// out, or returned output `HookAnswer` cannot parse. `reason` is the
/// `Display` of the `conway_core::error::HookFailure` that caused it, so a
/// caller surfacing this further names the same failure `dispatch_context`'s
/// own `tracing::warn!` already logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextHookFailure {
    pub hook_id: String,
    pub reason: String,
}

/// What [`HookDispatcher::dispatch_context`] learned: every hook that
/// answered, and every hook that failed to. Both lists are in the SAME
/// configured-subscription order `Self::dispatch`/`Self::dispatch_deny_only`
/// already iterate in; composing them (union excludes, concatenate appends)
/// is `crate::context::script_hook::apply_script_deltas`'s job, not this
/// type's -- this struct is a transport-level record, not a merge.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextDispatchOutcome {
    pub answers: Vec<ContextHookAnswer>,
    pub failures: Vec<ContextHookFailure>,
}

/// The fan-out layer a plugin's own `PluginEventHandle::emit` call reaches
/// (module doc, "Plugin-declared events"): dispatching a plugin-declared
/// event is [`Self::dispatch`], unconditionally -- the identical
/// observation-only path, same failure posture, same subscriber map, no
/// separate bookkeeping for "core" vs. "plugin" once an event has reached
/// this method. `name` here is already the FULL `plugin_id.bare_name` --
/// `PluginEventHandle::emit` performed the namespacing and validation
/// before calling this.
#[async_trait::async_trait]
impl PluginEventEmitter for HookDispatcher {
    async fn emit(&self, name: &str, payload: serde_json::Value) {
        self.dispatch(name, payload).await;
    }
}

/// Collects every installed plugin's own declared events (`Plugin::
/// events`), namespaced with its declaring plugin's manifest id and
/// validated with [`validate_event_name`] -- the SAME bare-name-plus-
/// host-prefixing pattern `conway_cli::tui::commands::CommandRegistry::
/// build` already established for [`conway_core::ports::Command`] (see
/// that function's own doc, and [`EventDecl`]'s own doc, "A third
/// consumer, same rule, different vocabulary").
///
/// **This is also the answer to "how does an operator discover what is
/// hookable given what they have installed"** (module doc): no separate
/// registry, no new port -- an embedder holding the exact
/// `&[Arc<dyn Plugin>]` it is about to hand `ConwayBuilder` can call this
/// function itself, before `build()`, and read back every plugin event's
/// full name, one-line summary, and whether it carries a tool name.
/// `ConwayBuilder::build` calls this SAME function to decide what
/// `[hooks].rules[]` may subscribe to -- one implementation, not a
/// parallel one for "validate" vs. "enumerate".
///
/// Returns a map keyed by full name (`plugin_id.bare_name`); [`BTreeMap`]'s
/// own sorted-key iteration order is deterministic regardless of the input
/// `Vec`'s order, mirroring `conway_runtime::tools::PluginRegistry::specs`'s
/// own "stable across runs" rationale.
///
/// Errors on: a bare name that fails [`validate_event_name`] once
/// prefixed (empty, or -- structurally unreachable in practice, since a
/// plugin's own manifest id is Rust-code-supplied -- a plugin id
/// containing [`EVENT_NAMESPACE_SEPARATOR`]), or two events (from the same
/// plugin or different ones) landing on the identical full name.
pub fn declared_plugin_events(
    plugins: &[Arc<dyn Plugin>],
) -> Result<BTreeMap<String, EventDecl>, String> {
    let mut events: BTreeMap<String, EventDecl> = BTreeMap::new();
    for plugin in plugins {
        let manifest = plugin.manifest();
        for decl in plugin.events() {
            let full_name = format!("{}{EVENT_NAMESPACE_SEPARATOR}{}", manifest.id, decl.name);
            validate_event_name(&full_name, Some(&manifest.id))?;
            if events.contains_key(&full_name) {
                return Err(format!(
                    "duplicate plugin event '{full_name}' -- declared more than once"
                ));
            }
            events.insert(full_name, decl);
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::error::HookFailure;
    use conway_core::hook::HookAnswer;
    use std::sync::Mutex;

    /// Records what it was invoked with, and can be scripted to fail.
    #[derive(Debug, Default)]
    struct RecordingRunner {
        seen: Mutex<Vec<(String, serde_json::Value)>>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl HookRunner for RecordingRunner {
        async fn run(&self, invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
            self.seen.lock().expect("seen lock poisoned").push((
                invocation.event.name.clone(),
                invocation.event.payload.clone(),
            ));
            if self.fail {
                return Err(HookFailure::Spawn {
                    detail: "scripted failure".to_string(),
                });
            }
            Ok(HookAnswer::default())
        }
    }

    fn spec(id: &str) -> HookSpec {
        HookSpec {
            id: id.to_string(),
            command: vec!["/bin/true".to_string()],
            timeout_ms: 1_000,
            matcher: None,
            origin: HookOrigin::Operator,
            spawn_only: false,
        }
    }

    fn spec_matching(id: &str, matcher: &str) -> HookSpec {
        HookSpec {
            matcher: Some(matcher.to_string()),
            ..spec(id)
        }
    }

    fn wired(fail: bool) -> (HookDispatcher, Arc<RecordingRunner>) {
        let runner = Arc::new(RecordingRunner {
            fail,
            ..Default::default()
        });
        let d = HookDispatcher::new();
        d.set_runner(Some(runner.clone()));
        d.set_hooks(BTreeMap::from([(
            POST_TOOL_USE.to_string(),
            vec![spec("watcher")],
        )]));
        (d, runner)
    }

    /// The default: no runner, no hooks, nothing spawned.
    #[tokio::test]
    async fn dispatch_is_a_no_op_with_no_runner_installed() {
        let d = HookDispatcher::new();
        assert!(!d.will_dispatch(POST_TOOL_USE));
        d.dispatch(POST_TOOL_USE, serde_json::json!({})).await;
    }

    /// A runner with no subscription for the event is still a no-op.
    #[tokio::test]
    async fn dispatch_is_a_no_op_for_an_event_with_no_subscribers() {
        let (d, runner) = wired(false);
        assert!(!d.will_dispatch(SESSION_STARTING));
        d.dispatch(SESSION_STARTING, serde_json::json!({})).await;
        assert!(runner.seen.lock().expect("seen lock poisoned").is_empty());
    }

    #[tokio::test]
    async fn dispatch_invokes_a_subscribed_hook_with_the_event_name_and_payload() {
        let (d, runner) = wired(false);
        assert!(d.will_dispatch(POST_TOOL_USE));
        d.dispatch(POST_TOOL_USE, serde_json::json!({"tool": "bash"}))
            .await;
        let seen = runner.seen.lock().expect("seen lock poisoned");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, POST_TOOL_USE);
        assert_eq!(seen[0].1["tool"], "bash");
    }

    /// The load-bearing property of this whole module: a failing hook is
    /// swallowed. `dispatch` returns `()`, so there is nothing a caller could
    /// propagate even by accident -- this test pins that it does not panic
    /// or hang either.
    #[tokio::test]
    async fn a_failing_hook_does_not_propagate_and_does_not_stop_the_next_one() {
        let runner = Arc::new(RecordingRunner {
            fail: true,
            ..Default::default()
        });
        let d = HookDispatcher::new();
        d.set_runner(Some(runner.clone()));
        d.set_hooks(BTreeMap::from([(
            POST_TOOL_USE.to_string(),
            vec![spec("first"), spec("second")],
        )]));

        d.dispatch(POST_TOOL_USE, serde_json::json!({})).await;

        // BOTH ran even though the first failed: a broken hook must not
        // silently disable the ones configured after it.
        assert_eq!(runner.seen.lock().expect("seen lock poisoned").len(), 2);
    }

    /// Clearing the runner returns the dispatcher to its no-op default.
    #[tokio::test]
    async fn clearing_the_runner_restores_the_no_op_default() {
        let (d, runner) = wired(false);
        d.set_runner(None);
        assert!(!d.will_dispatch(POST_TOOL_USE));
        d.dispatch(POST_TOOL_USE, serde_json::json!({})).await;
        assert!(runner.seen.lock().expect("seen lock poisoned").is_empty());
    }

    // -------------------------------------------------------- dispatch_context --

    use conway_core::hook::ContextDelta;

    /// Answers every invocation with a scripted `ContextDelta`, recording
    /// what it was invoked with -- the `dispatch_context` counterpart of
    /// `RecordingRunner` above, which only ever answers the trait's
    /// `Default`.
    #[derive(Debug, Default)]
    struct ScriptedContextRunner {
        delta: ContextDelta,
    }

    #[async_trait::async_trait]
    impl HookRunner for ScriptedContextRunner {
        async fn run(&self, _invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
            Ok(HookAnswer::new(
                self.delta.clone(),
                HookPermissionVerdict::default(),
            ))
        }
    }

    /// The transport-level property: a subscribed hook's `context` field
    /// (previously read by nothing at all -- `Self::dispatch` discards it)
    /// comes back tagged with its own configured id, unchanged.
    #[tokio::test]
    async fn dispatch_context_returns_the_answering_hooks_delta_tagged_with_its_id() {
        let runner = Arc::new(ScriptedContextRunner {
            delta: ContextDelta::new(
                vec![serde_json::json!({"role": "system", "text": "note"})],
                vec!["seg-1".to_string()],
            ),
        });
        let d = HookDispatcher::new();
        d.set_runner(Some(runner));
        d.set_hooks(BTreeMap::from([(
            REQUEST_ASSEMBLED.to_string(),
            vec![spec("annotator")],
        )]));

        let outcome = d
            .dispatch_context(REQUEST_ASSEMBLED, serde_json::json!({}))
            .await;

        assert!(outcome.failures.is_empty());
        assert_eq!(outcome.answers.len(), 1);
        assert_eq!(outcome.answers[0].hook_id, "annotator");
        assert_eq!(outcome.answers[0].delta.excludes, vec!["seg-1".to_string()]);
    }

    /// Two hooks subscribed to the same event both answer, in configured
    /// order -- the raw material `apply_script_deltas` composes under the
    /// no-chaining union rule; this level only proves both are COLLECTED,
    /// not merged (merging is that function's own test's job).
    #[tokio::test]
    async fn dispatch_context_collects_every_subscribed_hooks_answer() {
        let runner = Arc::new(ScriptedContextRunner {
            delta: ContextDelta::new(vec![], vec!["shared".to_string()]),
        });
        let d = HookDispatcher::new();
        d.set_runner(Some(runner));
        d.set_hooks(BTreeMap::from([(
            REQUEST_ASSEMBLED.to_string(),
            vec![spec("first"), spec("second")],
        )]));

        let outcome = d
            .dispatch_context(REQUEST_ASSEMBLED, serde_json::json!({}))
            .await;

        assert_eq!(outcome.answers.len(), 2);
        assert_eq!(outcome.answers[0].hook_id, "first");
        assert_eq!(outcome.answers[1].hook_id, "second");
    }

    /// ACCEPTANCE, the visible-failure half: a hook that errors contributes
    /// NO answer (so applying `outcome.answers` alone leaves the payload
    /// unchanged for this hook) AND a structured [`ContextHookFailure`]
    /// naming it -- both assertable, not just the absence of a panic.
    #[tokio::test]
    async fn dispatch_context_reports_a_failing_hook_visibly_and_contributes_no_answer() {
        let (d, _runner) = {
            let runner = Arc::new(RecordingRunner {
                fail: true,
                ..Default::default()
            });
            let d = HookDispatcher::new();
            d.set_runner(Some(runner.clone()));
            d.set_hooks(BTreeMap::from([(
                REQUEST_ASSEMBLED.to_string(),
                vec![spec("broken")],
            )]));
            (d, runner)
        };

        let outcome = d
            .dispatch_context(REQUEST_ASSEMBLED, serde_json::json!({}))
            .await;

        assert!(
            outcome.answers.is_empty(),
            "a failing hook must contribute no answer to apply"
        );
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].hook_id, "broken");
        assert!(
            outcome.failures[0].reason.contains("scripted failure"),
            "{:?}",
            outcome.failures[0]
        );
    }

    /// A hook whose answer never sets `context` (the wire default) still
    /// produces an [`ContextHookAnswer`] carrying the empty `ContextDelta`
    /// -- an existing observation-only rule's non-answer is a documented
    /// no-op when applied, not a failure.
    #[tokio::test]
    async fn dispatch_context_treats_an_empty_answer_as_a_no_op_delta() {
        let (d, _runner) = wired(false);
        d.set_hooks(BTreeMap::from([(
            REQUEST_ASSEMBLED.to_string(),
            vec![spec("silent")],
        )]));

        let outcome = d
            .dispatch_context(REQUEST_ASSEMBLED, serde_json::json!({}))
            .await;

        assert_eq!(outcome.answers.len(), 1);
        assert!(outcome.answers[0].delta.appends.is_empty());
        assert!(outcome.answers[0].delta.excludes.is_empty());
    }

    /// The default posture -- no runner installed -- is still a byte-for-byte
    /// no-op for the context-editing dispatch, mirroring `Self::dispatch`'s
    /// own identical guarantee.
    #[tokio::test]
    async fn dispatch_context_is_a_no_op_with_no_runner_installed() {
        let d = HookDispatcher::new();
        let outcome = d
            .dispatch_context(REQUEST_ASSEMBLED, serde_json::json!({}))
            .await;
        assert!(outcome.answers.is_empty());
        assert!(outcome.failures.is_empty());
    }

    // ---------------------------------------------------------------- matcher --

    /// ACCEPTANCE: a `post_tool_use`
    /// rule matching one tool fires for that tool and does NOT fire for
    /// another -- the VERIFICATION ANCHOR, at the unit level (the
    /// integration-level version drives this through a real `ToolRunner`;
    /// see `crates/conway-runtime/tests/hook_dispatch.rs`).
    #[tokio::test]
    async fn a_matcher_fires_only_for_its_own_tool() {
        let runner = Arc::new(RecordingRunner::default());
        let d = HookDispatcher::new();
        d.set_runner(Some(runner.clone()));
        d.set_hooks(BTreeMap::from([(
            POST_TOOL_USE.to_string(),
            vec![
                spec_matching("read-watcher", "read"),
                spec_matching("edit-watcher", "edit"),
            ],
        )]));

        d.dispatch(POST_TOOL_USE, serde_json::json!({"tool": "read"}))
            .await;
        d.dispatch(POST_TOOL_USE, serde_json::json!({"tool": "edit"}))
            .await;

        let seen = runner.seen.lock().expect("seen lock poisoned").clone();
        let ids_for = |tool: &str| -> usize {
            seen.iter()
                .filter(|(_, payload)| payload["tool"] == tool)
                .count()
        };
        assert_eq!(
            seen.len(),
            2,
            "each matcher must fire exactly once: {seen:?}"
        );
        assert_eq!(ids_for("read"), 1);
        assert_eq!(ids_for("edit"), 1);
    }

    /// Shown to fail by removing the matcher check (VERIFICATION ANCHOR):
    /// with no `matcher` set, a rule fires for every tool, unchanged from
    /// before this field existed -- an existing config with no `match` key
    /// behaves identically.
    #[tokio::test]
    async fn an_absent_matcher_fires_for_every_tool() {
        let (d, runner) = wired(false);
        d.dispatch(POST_TOOL_USE, serde_json::json!({"tool": "read"}))
            .await;
        d.dispatch(POST_TOOL_USE, serde_json::json!({"tool": "edit"}))
            .await;
        assert_eq!(runner.seen.lock().expect("seen lock poisoned").len(), 2);
    }

    /// A matcher set on an event whose payload carries no `"tool"` key never
    /// matches -- the defensive fallback `HookSpec::applies_to`'s own doc
    /// describes for a state `merge::validate` already refuses to load.
    #[tokio::test]
    async fn a_matcher_on_a_toolless_payload_never_fires() {
        let runner = Arc::new(RecordingRunner::default());
        let d = HookDispatcher::new();
        d.set_runner(Some(runner.clone()));
        d.set_hooks(BTreeMap::from([(
            SESSION_STARTING.to_string(),
            vec![spec_matching("stray", "bash")],
        )]));

        d.dispatch(SESSION_STARTING, serde_json::json!({})).await;
        assert!(runner.seen.lock().expect("seen lock poisoned").is_empty());
    }

    // -------------------------------------------------------- spawn_only --
    // Board item `01M129Y98V4C1050QBPPMY37X0`: `HookSpec::spawn_only`'s own
    // dispatcher-level unit coverage, the sibling of the matcher section
    // immediately above. `crates/conway-plugin-claude/tests/hooks_dispatch.rs`
    // is the end-to-end proof that a REAL translated `SubagentStart` hook,
    // driven through a REAL `SubagentHost::start` fork/spawn, obeys this --
    // this section only pins the dispatcher's own filtering logic in
    // isolation, exactly like `a_matcher_fires_only_for_its_own_tool` does
    // for `matcher`.

    fn spec_spawn_only(id: &str) -> HookSpec {
        HookSpec {
            spawn_only: true,
            ..spec(id)
        }
    }

    /// **The discriminating pair, in one fixture** (P-15: a test asserting
    /// only the negative -- a fork never fires -- would pass just as
    /// happily against a hook that never registered, a runner never
    /// injected, or a typo'd event name; the positive case in the SAME
    /// fixture rules all three out). One `spawn_only` hook, one dispatcher:
    /// a `mode: "fork"` payload never reaches it, a `mode: "spawn"` payload
    /// does.
    #[tokio::test]
    async fn spawn_only_fires_for_a_spawn_mode_payload_and_not_for_a_fork() {
        let runner = Arc::new(RecordingRunner::default());
        let d = HookDispatcher::new();
        d.set_runner(Some(runner.clone()));
        d.set_hooks(BTreeMap::from([(
            CHILD_SPAWNED.to_string(),
            vec![spec_spawn_only("subagent-start")],
        )]));

        d.dispatch(CHILD_SPAWNED, serde_json::json!({"mode": "fork"}))
            .await;
        assert!(
            runner.seen.lock().expect("seen lock poisoned").is_empty(),
            "a spawn_only hook must not fire for a fork"
        );

        d.dispatch(CHILD_SPAWNED, serde_json::json!({"mode": "spawn"}))
            .await;
        let seen = runner.seen.lock().expect("seen lock poisoned").clone();
        assert_eq!(
            seen.len(),
            1,
            "a spawn_only hook must fire for a spawn: {seen:?}"
        );
        assert_eq!(seen[0].1["mode"], "spawn");
    }

    /// The default, pre-existing behavior is unchanged: `spawn_only: false`
    /// (every `HookSpec` before this field existed) fires for both modes --
    /// regression coverage for the "an operator-authored `child_spawned`
    /// rule still sees every child" guarantee this field's own doc makes.
    #[tokio::test]
    async fn an_unset_spawn_only_still_fires_for_both_modes() {
        let (d, runner) = wired(false);
        d.set_hooks(BTreeMap::from([(
            CHILD_SPAWNED.to_string(),
            vec![spec("native-subscriber")],
        )]));

        d.dispatch(CHILD_SPAWNED, serde_json::json!({"mode": "fork"}))
            .await;
        d.dispatch(CHILD_SPAWNED, serde_json::json!({"mode": "spawn"}))
            .await;

        assert_eq!(runner.seen.lock().expect("seen lock poisoned").len(), 2);
    }

    /// A `spawn_only` hook set on a payload with no parseable `"mode"` field
    /// never fires -- the same defensive-fallback posture
    /// `a_matcher_on_a_toolless_payload_never_fires` pins for `matcher`.
    #[tokio::test]
    async fn spawn_only_never_fires_for_a_payload_with_no_mode_field() {
        let runner = Arc::new(RecordingRunner::default());
        let d = HookDispatcher::new();
        d.set_runner(Some(runner.clone()));
        d.set_hooks(BTreeMap::from([(
            CHILD_SPAWNED.to_string(),
            vec![spec_spawn_only("stray")],
        )]));

        d.dispatch(CHILD_SPAWNED, serde_json::json!({})).await;
        assert!(runner.seen.lock().expect("seen lock poisoned").is_empty());
    }

    // -------------------------------------- declared_plugin_events --

    use conway_core::ports::{PluginManifest, Tool};

    /// A minimal `Plugin` double declaring whatever `EventDecl`s it is
    /// constructed with -- this module's own tests only need `manifest`
    /// and `events`, never `tools`.
    struct EventOnlyPlugin {
        id: &'static str,
        decls: Vec<EventDecl>,
    }

    impl Plugin for EventOnlyPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: self.id.to_string(),
                version: "0.1.0".to_string(),
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
            self.decls.clone()
        }
    }

    fn decl(name: &str, carries_tool_name: bool) -> EventDecl {
        EventDecl {
            name: name.to_string(),
            summary: "test event".to_string(),
            carries_tool_name,
        }
    }

    #[test]
    fn declared_plugin_events_is_empty_for_a_plugin_with_no_events() {
        let plugins: Vec<Arc<dyn Plugin>> = vec![Arc::new(EventOnlyPlugin {
            id: "acme",
            decls: vec![],
        })];
        assert!(declared_plugin_events(&plugins).unwrap().is_empty());
    }

    /// The load-bearing shape check: a declared bare name comes back
    /// namespaced under its OWN declaring plugin's id, with the declared
    /// metadata preserved.
    #[test]
    fn declared_plugin_events_namespaces_and_preserves_the_declaration() {
        let plugins: Vec<Arc<dyn Plugin>> = vec![Arc::new(EventOnlyPlugin {
            id: "acme_routing",
            decls: vec![decl("candidate_chosen", true)],
        })];
        let events = declared_plugin_events(&plugins).unwrap();
        assert_eq!(events.len(), 1);
        let found = events
            .get("acme_routing.candidate_chosen")
            .expect("full name must be plugin_id + separator + bare name");
        assert_eq!(found.name, "candidate_chosen");
        assert!(found.carries_tool_name);
    }

    /// ACCEPTANCE: two plugins, each declaring events, both come back
    /// correctly namespaced -- proves this is a per-plugin fold, not a
    /// single-plugin-only path.
    #[test]
    fn declared_plugin_events_collects_across_multiple_plugins() {
        let plugins: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(EventOnlyPlugin {
                id: "acme",
                decls: vec![decl("thing_a", false)],
            }),
            Arc::new(EventOnlyPlugin {
                id: "other",
                decls: vec![decl("thing_b", false)],
            }),
        ];
        let events = declared_plugin_events(&plugins).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.contains_key("acme.thing_a"));
        assert!(events.contains_key("other.thing_b"));
    }

    /// An empty bare name fails `validate_event_name` once prefixed --
    /// surfaced as a named error, not silently dropped or panicking.
    #[test]
    fn declared_plugin_events_rejects_an_empty_bare_name() {
        let plugins: Vec<Arc<dyn Plugin>> = vec![Arc::new(EventOnlyPlugin {
            id: "acme",
            decls: vec![decl("", false)],
        })];
        let err = declared_plugin_events(&plugins).expect_err("empty bare name must be rejected");
        assert!(err.contains("acme"), "{err}");
    }

    /// Two events landing on the identical full name -- here, the SAME
    /// plugin declaring the same bare name twice -- is a named collision
    /// error, mirroring `CommandRegistry::build`'s own duplicate-command
    /// check.
    #[test]
    fn declared_plugin_events_rejects_a_duplicate_full_name() {
        let plugins: Vec<Arc<dyn Plugin>> = vec![Arc::new(EventOnlyPlugin {
            id: "acme",
            decls: vec![decl("thing", false), decl("thing", true)],
        })];
        let err = declared_plugin_events(&plugins).expect_err("duplicate full name must error");
        assert!(err.contains("acme.thing"), "{err}");
    }

    // -------------------------------------- PluginEventEmitter for HookDispatcher --

    /// The load-bearing wiring proof: `HookDispatcher` implementing
    /// `PluginEventEmitter` reaches the SAME subscriber map `dispatch`
    /// itself reads -- a hook subscribed to a plugin-namespaced event name
    /// fires when that name is `emit`ted through the trait, exactly as it
    /// would through a direct `dispatch` call.
    #[tokio::test]
    async fn hook_dispatcher_as_plugin_event_emitter_reaches_a_subscribed_hook() {
        let (d, runner) = wired(false);
        d.set_hooks(BTreeMap::from([(
            "acme.candidate_chosen".to_string(),
            vec![spec("routing-watcher")],
        )]));

        let emitter: &dyn PluginEventEmitter = &d;
        emitter
            .emit("acme.candidate_chosen", serde_json::json!({"model": "x"}))
            .await;

        let seen = runner.seen.lock().expect("seen lock poisoned").clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "acme.candidate_chosen");
        assert_eq!(seen[0].1["model"], "x");
    }

    /// The SAME fail-open posture as every other observation dispatch: an
    /// unsubscribed plugin event name is a no-op, not an error.
    #[tokio::test]
    async fn hook_dispatcher_as_plugin_event_emitter_is_a_no_op_with_no_subscriber() {
        let (d, runner) = wired(false);
        let emitter: &dyn PluginEventEmitter = &d;
        emitter
            .emit("nobody.subscribed", serde_json::json!({}))
            .await;
        assert!(runner.seen.lock().expect("seen lock poisoned").is_empty());
    }
}
