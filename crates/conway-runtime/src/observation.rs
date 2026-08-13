//! Dispatch for the OBSERVATION-ONLY hook events (board item
//! 01KZS019NHG11RVQYSVT7RG0P5): `post_tool_use`, `session_starting`, and
//! `child_spawned`.
//!
//! **These events cannot say no, and that is the whole point of them.** Not
//! every useful hook needs to block something — an operator wanting a log
//! line per command, or a notification when a child starts, needs to be told
//! a thing happened and nothing more. Each of the three fires at a place that
//! already knows the moment has passed.
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
//! # Failure never propagates, and this is the invariant to preserve
//!
//! A hook that errors, times out, or returns garbage produces a
//! `tracing::warn` and nothing else. A failing `post_tool_use` hook must not
//! fail the tool call it observed; a failing `child_spawned` hook must not
//! fail the spawn. [`ObservationDispatcher::dispatch`] therefore returns `()`
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
//! # `child_spawned` and denial — an open question, deliberately deferred
//!
//! Nothing structurally forces `child_spawned` to be observe-only the way
//! `post_tool_use` is (there, the call has already run). A spawn COULD in
//! principle be refused. It is shipped observe-only anyway, because refusing
//! raises questions board item 01KZS019NHG11RVQYSVT7RG0P5 did not scope: what
//! does the parent agent see when its own spawn is denied — a tool error, a
//! silent no-op? Does the caller need new error handling? Answering those by
//! accident, in the shape of a return type, is exactly the trap
//! `PluginManifest` avoided by refusing to carry an `on_init` nobody had
//! wired: an unwired lifecycle hook costs an implementer a debugging session.
//! **If that question is settled later, the change is deliberate and visible;
//! it is not settled here by omission.**

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use conway_core::hook::{HookEvent, HookInvocation};
use conway_core::ports::HookRunner;

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
pub struct ObservationHookSpec {
    /// The rule's `HookEntry::id`, used to name the hook in a warning so an
    /// operator can tell WHICH script failed rather than only that one did.
    pub id: String,
    pub command: Vec<String>,
    pub timeout_ms: u64,
}

/// The event name every `post_tool_use` subscription is keyed on.
pub const POST_TOOL_USE: &str = "post_tool_use";
/// The event name every `session_starting` subscription is keyed on.
pub const SESSION_STARTING: &str = "session_starting";
/// The event name every `child_spawned` subscription is keyed on.
pub const CHILD_SPAWNED: &str = "child_spawned";

/// Every observation event this tier dispatches, in one place so a caller
/// wiring config can iterate the supported set rather than restating it.
pub const OBSERVATION_EVENTS: &[&str] = &[POST_TOOL_USE, SESSION_STARTING, CHILD_SPAWNED];

/// Holds the injected [`HookRunner`] and the per-event subscription lists,
/// and invokes them.
///
/// Shared by `Arc` between [`crate::runtime::Runtime`] and
/// [`crate::tools::ToolRunner`] — `ToolRunner::new` constructs one and the
/// runtime reads it back via [`crate::tools::ToolRunner::observation`], so
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
pub struct ObservationDispatcher {
    runner: RwLock<Option<Arc<dyn HookRunner>>>,
    hooks: RwLock<BTreeMap<String, Vec<ObservationHookSpec>>>,
}

impl std::fmt::Debug for ObservationDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObservationDispatcher")
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

impl ObservationDispatcher {
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
    pub fn set_hooks(&self, hooks: BTreeMap<String, Vec<ObservationHookSpec>>) {
        *self.hooks.write().expect("observation hooks lock poisoned") = hooks;
    }

    /// True when `event` has at least one subscriber AND a runner exists —
    /// i.e. when [`Self::dispatch`] would actually spawn something. Lets a
    /// caller skip assembling a payload it would only throw away.
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

    /// Invokes every hook subscribed to `event`, in configured order.
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

        for hook in &hooks {
            let invocation = HookInvocation {
                command: hook.command.clone(),
                timeout_ms: hook.timeout_ms,
                event: HookEvent {
                    name: event.to_string(),
                    payload: payload.clone(),
                },
            };
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

    fn spec(id: &str) -> ObservationHookSpec {
        ObservationHookSpec {
            id: id.to_string(),
            command: vec!["/bin/true".to_string()],
            timeout_ms: 1_000,
        }
    }

    fn wired(fail: bool) -> (ObservationDispatcher, Arc<RecordingRunner>) {
        let runner = Arc::new(RecordingRunner {
            fail,
            ..Default::default()
        });
        let d = ObservationDispatcher::new();
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
        let d = ObservationDispatcher::new();
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
        let d = ObservationDispatcher::new();
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
}
