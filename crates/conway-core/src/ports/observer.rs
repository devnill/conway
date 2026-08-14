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
