//! The `ToolObserver` port: in-process observation of a tool call that has
//! already run, with a return channel.
//!
//! ## Why this exists
//!
//! `PHILOSOPHY.md` §6 leaves loop intervention to the operator — "repeated-step
//! detection, retry ceilings, and circling-agent heuristics are not in the
//! core. The events exist, so the policy is yours to write, including writing
//! none." Honouring that requires a seam a policy can actually attach to, and
//! before this port there was none: `post_tool_use` reaches a subprocess and
//! returns `()`, `ContextHook` sees assembled segments rather than individual
//! results, and nothing at all could add to the durable record.
//!
//! So the harness kept its own repeated-call detector compiled in, which is
//! precisely the arrangement §6 rules out. This port is what let that move to
//! `conway-plugin-stepguard`, where an operator can decline it, replace it, or
//! fork it.
//!
//! ## Shape: declare an effect, do not perform one
//!
//! An observer returns [`ObserverAnswer`] and the runtime performs whatever it
//! describes. It is handed no `SessionStore`, no event bus, and no agent
//! handle. That is deliberate and follows the shape already established by
//! `ContextHook` (returns an edited payload) and `CommandOutcome::ForkSession`
//! (returns a request to fork, rather than receiving a fork-capable handle):
//! the smallest capability that does the job, so a misbehaving plugin's blast
//! radius is bounded by the return type rather than by its own restraint.
//!
//! Concretely, an observer cannot delete a record, rewrite one, forge a
//! terminal result, or touch a session it was not called about.
//!
//! ## Observation only, and fail-open
//!
//! The call has already run; its side effects have already happened. An
//! observer therefore cannot deny, cancel, or alter it, and a panicking or
//! slow observer must not fail the call it watched — the same posture
//! `post_tool_use` already takes, for the same reason. An observer that wants
//! to *stop* something wants a different seam: `PermissionGate` or a
//! `pre_tool_use` hook, both of which run before anything happens.
//!
//! ## A pre-call seam, added later, on the same terms
//!
//! Board item `01M20RYAK1T1DK7XWX431FFCYQ`: [`ToolObserver::before_tool_call`]
//! closes a real gap in that original design -- a plugin that wants to
//! observe a file's bytes BEFORE a `write`/`edit` overwrites them had no
//! in-process seam at all (`conway-plugin-checkpoint`'s own module doc,
//! "the seam gap that shapes it", written when this port had exactly the
//! one method above). It is deliberately NOT a second permission gate: it
//! returns nothing, so it cannot deny, cancel, or alter the call either --
//! the same observation-only posture as [`ToolObserver::after_tool_call`], just
//! timed differently. `PermissionGate` remains the one place a call is
//! allowed or denied; this method runs strictly AFTER that decision has
//! already resolved to allow, so a denied call never reaches it and never
//! trips an observer's side effects (e.g. a snapshot) for a change that was
//! never going to happen. Defaulted to a no-op so every observer written
//! against the original one-method trait keeps compiling and behaving
//! identically -- see this method's own doc for the containment/latency
//! contract, identical to `after_tool_call`'s.

use std::sync::Arc;

use async_trait::async_trait;

use crate::ids::{AgentId, LogSeq, SessionId, ToolName};
use crate::ports::plugin::PluginEventHandle;

/// One finished tool call, as an observer sees it.
///
/// Carries `arguments` as well as `tool`, which the `post_tool_use` payload
/// does not: any policy that asks "has this exact call happened before" needs
/// the arguments, and a tool name alone cannot answer it.
#[derive(Clone, Debug)]
pub struct ObservedCall {
    pub agent_id: AgentId,
    pub session: SessionId,
    /// The provider-assigned id tying this call to its result.
    pub call_id: String,
    pub tool: ToolName,
    /// The arguments the model supplied. UNTRUSTED, like every other
    /// model-supplied value.
    pub arguments: serde_json::Value,
    pub is_error: bool,
    /// Where this call's result landed in the session log, so a note an
    /// observer returns can point a reader (or the model) at it.
    pub result_seq: LogSeq,
}

/// One tool call about to execute, as [`ToolObserver::before_tool_call`]
/// sees it: authorized (the permission decision already resolved to allow)
/// but not yet run, so `arguments` are the model's own proposed arguments
/// and there is no result yet to carry -- unlike [`ObservedCall`], there is
/// no `is_error` (nothing has happened yet) and no `result_seq` (nothing has
/// been persisted yet).
#[derive(Clone, Debug)]
pub struct PendingCall {
    pub agent_id: AgentId,
    pub session: SessionId,
    /// The provider-assigned id tying this call to its eventual result --
    /// the SAME id `ObservedCall::call_id` carries, so an observer that
    /// wants to correlate its own pre-call and post-call sightings of one
    /// call can key on it.
    pub call_id: String,
    pub tool: ToolName,
    /// The arguments the model supplied. UNTRUSTED, like every other
    /// model-supplied value -- see `ObservedCall::arguments`'s own doc.
    pub arguments: serde_json::Value,
}

/// A note an observer asks the runtime to append to the session log.
///
/// It becomes a `LogRecord::SystemNote`, which the model reads on its next
/// turn — so this is how an observer says something the agent will actually
/// see, as opposed to something only an operator reading the log will.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObserverNote {
    /// The text the model reads.
    pub text: String,
    /// A short stable tag naming what kind of note this is, recorded on the
    /// record as `SystemNote::reason`. Use one value per kind of note so a
    /// reader filtering the log can select them.
    pub reason: String,
}

/// What an observer asks the runtime to do about the call it just saw.
///
/// [`Default`] is "nothing", which is the answer on the overwhelming majority
/// of calls — an observer that only acts occasionally should return
/// `ObserverAnswer::default()` the rest of the time rather than allocating.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObserverAnswer {
    /// Appended to the session log in order, before the next turn's context
    /// is assembled, so the model sees them on its very next turn.
    pub notes: Vec<ObserverNote>,
}

/// Everything an observer is handed besides the call itself.
///
/// `events` is the SAME [`PluginEventHandle`] a plugin's own tools receive
/// through `ToolCtx`, bound to the observing plugin's manifest id — so an
/// observer fires its own declared events under its own namespace
/// (`plugin_id.bare_name`) and cannot emit a core event or impersonate
/// another plugin. Declaring those events in `Plugin::events` remains the
/// author's job; an event fired but never declared is as much a defect as
/// one declared and never fired.
#[derive(Clone, Debug)]
pub struct ObserverCtx {
    pub events: PluginEventHandle,
}

/// Observes tool calls after they run, and may ask the runtime to record
/// something about them. See the module doc for the shape and its limits.
#[async_trait]
pub trait ToolObserver: Send + Sync + 'static {
    /// Called once per finished tool call, after its result is durable and
    /// before the next turn's context is assembled.
    ///
    /// MUST NOT block for long: this sits between a tool batch completing and
    /// the next turn starting, so latency here is latency the agent pays every
    /// step. An observer with real work to do should return quickly and do it
    /// elsewhere.
    ///
    /// A panic is contained by the runtime and the call proceeds unaffected —
    /// observation never fails the thing it observed.
    async fn after_tool_call(&self, ctx: &ObserverCtx, call: &ObservedCall) -> ObserverAnswer;

    /// Called once per call, after the permission decision has already
    /// resolved to allow it and before the tool actually runs -- see this
    /// module's own doc, "A pre-call seam, added later, on the same terms",
    /// for why this exists and why it is timed exactly there. A denied call
    /// never reaches this method.
    ///
    /// Returns nothing: this is NOT a second permission gate and cannot
    /// refuse, alter, or delay the call -- `PermissionGate` is the one place
    /// that decision is made. An observer that wants to stop something
    /// wants that seam, not this one.
    ///
    /// MUST NOT block for long, for the same reason as [`Self::
    /// after_tool_call`]: a slow `before_tool_call` is latency the agent
    /// pays on every tool call.
    ///
    /// A panic is contained by the runtime and the call proceeds unaffected
    /// -- observation never fails the thing it observed, whether the
    /// observation happens before or after.
    ///
    /// Defaults to doing nothing, so every observer written before this
    /// method existed keeps compiling and behaving identically without any
    /// change of its own.
    async fn before_tool_call(&self, _ctx: &ObserverCtx, _call: &PendingCall) {}
}

/// A [`ToolObserver`] together with the plugin that supplied it, so the
/// runtime can bind an [`ObserverCtx`] to the right namespace without asking
/// the observer to carry (and be trusted with) its own id.
#[derive(Clone)]
pub struct RegisteredObserver {
    pub plugin_id: String,
    pub observer: Arc<dyn ToolObserver>,
}

impl std::fmt::Debug for RegisteredObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredObserver")
            .field("plugin_id", &self.plugin_id)
            .field("observer", &"<dyn ToolObserver>")
            .finish()
    }
}
