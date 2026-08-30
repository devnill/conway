//! In-crate test doubles: everything downstream `conway-tools` work items
//! need to unit-test a tool with zero runtime.
//!
//! `conway-testkit` ships its own `SubagentHost`/`EventSink` fakes
//! (`conway_testkit::{FakeSubagentHost, CollectingEventSink}`), but this
//! crate defines its own lightweight doubles instead of depending on that
//! crate, for real behavioural reasons -- a shared implementation is
//! restated only when there is one -- checked against `conway-testkit`'s
//! doubles directly, not assumed:
//! 1. The `FakeSubagentHost` this crate's downstream tests need must also
//!    record `steer`/`cancel` calls (`conway_testkit`'s does not — it is a
//!    no-op on both) and must fail `await_result` for an unknown agent id
//!    (`conway_testkit`'s always synthesizes a fallback result instead).
//! 2. `test_ctx` below still needs its OWN entry point (a `(ToolCtx,
//!    TestHandles)` pair keyed on `cwd` alone, returning this crate's own
//!    doubles pre-wrapped for inspection) -- `conway-testkit` stops at the
//!    individual port doubles and does not attempt that, since a
//!    downstream-crate-specific assembly convenience is this crate's own
//!    concern, not a port-fake concern `conway-core`/`conway-testkit`
//!    share. As of board item 01KZQ3AZWG3NNJNZEJFX21MDJT, THIS reason
//!    narrowed: `test_ctx` no longer hand-assembles every `ToolCtx` field
//!    itself -- it calls [`conway_core::ports::ToolCtx::for_test`] (the
//!    same constructor a third party depending only on `conway` now has)
//!    and overrides only `cancel`, so the *fields this crate's doubles
//!    don't need to differ on* are one implementation, not two.
//!
//! (`conway-testkit` used to be `conway-core`'s own `fakes` feature,
//! unreachable outside this workspace; board item 01KZVYWNA24EYMPVW3NPGBW51M
//! extracted it into a crate of its own and made it reachable by a third
//! party through `conway`'s `testkit` feature. That move does not change
//! the calculus above -- reason 1 is a behavioural difference, not a
//! reachability one, and would apply just as much to a third party building
//! on `conway-testkit` today.)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;

use conway_core::agent::{AgentResult, AgentTreeSnapshot, AskOutcome, CancelMode, SubagentSpec};
use conway_core::content::Usage;
use conway_core::error::RuntimeError;
use conway_core::event::Event;
use conway_core::ids::{AgentId, SessionId};
use conway_core::ports::{CancellationToken, EventSink, SubagentHost, ToolCtx};

/// An in-memory [`SubagentHost`] that only records calls and plays back
/// scripted results. Contains no fork/spawn logic — it is a recorder.
#[derive(Debug)]
pub struct FakeSubagentHost {
    /// The `AgentId` every `start` call returns, until overridden via
    /// [`Self::with_next_agent_id`].
    next_agent_id: AgentId,
    started: Mutex<Vec<(AgentId, SubagentSpec)>>,
    /// `(caller, target, text)`, in call order.
    /// added `caller` to `SubagentHost::steer`;
    /// recording it here lets a test confirm the tool layer (`conway_steer`)
    /// actually threads `ToolCtx::agent_id` through as `caller`, not just
    /// that `target`/`text` arrived intact.
    steers: Mutex<Vec<(AgentId, AgentId, String)>>,
    /// `(caller, target, reason, mode)`, in call order -- see `steers`' own
    /// doc. `mode` added alongside
    /// the trait's new parameter.
    cancels: Mutex<Vec<(AgentId, AgentId, String, CancelMode)>>,
    results: Mutex<HashMap<AgentId, AgentResult>>,
    asks: Mutex<Vec<(AgentId, SubagentSpec)>>,
    ask_outcomes: Mutex<HashMap<AgentId, AskOutcome>>,
    /// When set (via [`Self::with_ask_error`]), every `ask` call fails with
    /// this error instead of returning an outcome — drives the
    /// `SubagentError`/`ToolError::from` translation path, which a scripted
    /// [`AskOutcome`] cannot reach. Read-only after construction, so no
    /// `Mutex`.
    ask_error: Option<RuntimeError>,
    /// When set (via [`Self::with_steer_error`]), every `steer` call fails
    /// with this error instead of recording the message — the `steer`
    /// counterpart to `ask_error`, letting a test drive the same
    /// translation path against a `steer` call site (e.g. a foreign-id steer
    /// scripted as `RuntimeError::AgentNotInSubtree`). Read-only after
    /// construction, so no `Mutex`.
    steer_error: Option<RuntimeError>,
}

impl FakeSubagentHost {
    /// A fresh host with a randomly generated scripted `next_agent_id` and
    /// no scripted results.
    pub fn new() -> Self {
        Self {
            next_agent_id: AgentId::new(),
            started: Mutex::new(Vec::new()),
            steers: Mutex::new(Vec::new()),
            cancels: Mutex::new(Vec::new()),
            results: Mutex::new(HashMap::new()),
            asks: Mutex::new(Vec::new()),
            ask_outcomes: Mutex::new(HashMap::new()),
            ask_error: None,
            steer_error: None,
        }
    }

    /// Overrides the `AgentId` future `start` calls return.
    pub fn with_next_agent_id(mut self, agent_id: AgentId) -> Self {
        self.next_agent_id = agent_id;
        self
    }

    /// Preconfigures the result `await_result(agent_id)` returns.
    pub fn with_result(self, agent_id: AgentId, result: AgentResult) -> Self {
        self.results.lock().unwrap().insert(agent_id, result);
        self
    }

    /// Preconfigures the [`AskOutcome`] `ask(parent, spec)` returns when
    /// called with `parent`. Unconfigured parents get the default
    /// `AskOutcome { text: "fake", usage: Usage::default(), status:
    /// ResultStatus::Completed, transcript_ref: SessionId::new() }`.
    pub fn with_ask_outcome(self, parent: AgentId, outcome: AskOutcome) -> Self {
        self.ask_outcomes.lock().unwrap().insert(parent, outcome);
        self
    }

    /// Makes every `ask(parent, spec)` call fail with `error` (cloned), the
    /// call still being recorded. Takes precedence over any scripted
    /// [`Self::with_ask_outcome`] — the host-error path is infrastructure
    /// failure, not a per-parent outcome.
    pub fn with_ask_error(mut self, error: RuntimeError) -> Self {
        self.ask_error = Some(error);
        self
    }

    /// Makes every `steer(caller, target, text)` call fail with `error`
    /// (cloned), the call still being recorded — the `steer` counterpart to
    /// [`Self::with_ask_error`].
    pub fn with_steer_error(mut self, error: RuntimeError) -> Self {
        self.steer_error = Some(error);
        self
    }

    /// The `AgentId` the next `start` call will return.
    pub fn next_agent_id(&self) -> AgentId {
        self.next_agent_id
    }

    /// Every `(child_id, spec)` pair recorded by `start`, in call order.
    pub fn started(&self) -> Vec<(AgentId, SubagentSpec)> {
        self.started.lock().unwrap().clone()
    }

    /// Every `(caller, target, text)` triple recorded by `steer`, in call
    /// order.
    pub fn steers(&self) -> Vec<(AgentId, AgentId, String)> {
        self.steers.lock().unwrap().clone()
    }

    /// Every `(caller, target, reason, mode)` quadruple recorded by
    /// `cancel`, in call order.
    pub fn cancels(&self) -> Vec<(AgentId, AgentId, String, CancelMode)> {
        self.cancels.lock().unwrap().clone()
    }

    /// Every `(parent, spec)` pair recorded by `ask`, in call order.
    pub fn asks(&self) -> Vec<(AgentId, SubagentSpec)> {
        self.asks.lock().unwrap().clone()
    }
}

impl Default for FakeSubagentHost {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SubagentHost for FakeSubagentHost {
    // added `caller` to `start`/`ask`
    // (mirroring the `steer`/`await_result`/`cancel` trio below
    //); this fake stays a pure recorder/no-op
    // (module doc) and does not itself enforce the `caller`-owns-`parent`
    // invariant -- see the real `Runtime` impl's own tests
    // (`crates/conway-runtime/tests/subagent_fork_spawn.rs`) and
    // `crates/conway/tests/subagent_control_seam.rs` for coverage driven
    // against the real trait boundary instead of this fixture.
    async fn start(
        &self,
        _caller: AgentId,
        _parent: AgentId,
        spec: SubagentSpec,
    ) -> Result<AgentId, RuntimeError> {
        let child = self.next_agent_id;
        self.started.lock().unwrap().push((child, spec));
        Ok(child)
    }

    async fn steer(
        &self,
        caller: AgentId,
        target: AgentId,
        text: String,
    ) -> Result<(), RuntimeError> {
        self.steers.lock().unwrap().push((caller, target, text));
        match &self.steer_error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    /// Terminates immediately (never blocks): returns the scripted result
    /// for `target`, or `Err(RuntimeError::AgentNotFound)` if none was
    /// configured via [`Self::with_result`]. This fake is a pure
    /// recorder/no-op (module doc, item 1) and does not itself enforce the
    /// `caller`-owns-`target` invariant
    /// added to the real `Runtime` impl -- see that item's own tests for
    /// coverage driven against the real trait boundary instead of this
    /// fixture.
    async fn await_result(
        &self,
        _caller: AgentId,
        target: AgentId,
    ) -> Result<AgentResult, RuntimeError> {
        self.results
            .lock()
            .unwrap()
            .get(&target)
            .cloned()
            .ok_or(RuntimeError::AgentNotFound { agent: target })
    }

    async fn cancel(
        &self,
        caller: AgentId,
        target: AgentId,
        reason: String,
        mode: CancelMode,
    ) -> Result<(), RuntimeError> {
        self.cancels
            .lock()
            .unwrap()
            .push((caller, target, reason, mode));
        Ok(())
    }

    // `caller` accepted and ignored -- this fake's `tree` is a fixed stub
    // (`root: self.next_agent_id`, no nodes), not a live scoped snapshot.
    fn tree(&self, caller: AgentId) -> AgentTreeSnapshot {
        AgentTreeSnapshot {
            root: caller,
            nodes: Vec::new(),
            at: Utc::now(),
        }
    }

    async fn ask(
        &self,
        _caller: AgentId,
        parent: AgentId,
        spec: SubagentSpec,
    ) -> Result<AskOutcome, RuntimeError> {
        self.asks.lock().unwrap().push((parent, spec));
        if let Some(error) = &self.ask_error {
            return Err(error.clone());
        }
        let outcome = self.ask_outcomes.lock().unwrap().get(&parent).cloned();
        Ok(outcome.unwrap_or_else(|| AskOutcome {
            text: "fake".into(),
            usage: Usage::default(),
            status: conway_core::agent::ResultStatus::Completed,
            transcript_ref: SessionId::new(),
        }))
    }
}

/// Collects every emitted [`Event`] in order, for assertion.
#[derive(Debug, Default)]
pub struct RecordingEventSink {
    events: Mutex<Vec<Event>>,
}

impl RecordingEventSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every event emitted so far, in emission order.
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }
}

impl EventSink for RecordingEventSink {
    fn emit(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }
}

/// Handles to the test doubles wired into a [`test_ctx`]-built `ToolCtx`, so
/// a test can inspect recorded calls/events and cancel mid-invoke.
pub struct TestHandles {
    pub subagents: Arc<FakeSubagentHost>,
    pub events: Arc<RecordingEventSink>,
    pub cancel: CancellationToken,
}

/// Builds a fully-wired `ToolCtx` — a fresh `agent_id`/`session_id`, `cwd`,
/// an uncancelled `CancellationToken`, a [`RecordingEventSink`], a
/// [`FakeSubagentHost`], and a default (empty) `PluginConfig` — plus the
/// [`TestHandles`] to inspect/drive those doubles.
///
/// Delegates the field-by-field assembly to
/// [`conway_core::ports::ToolCtx::for_test`] -- one implementation of
/// "build a `ToolCtx`", not two -- and overrides only `cancel` — every other
/// caller of `for_test` gets a fresh, unobservable token of its own, but
/// this crate's own tests need the SAME token back out through
/// [`TestHandles::cancel`] so a test can cancel it mid-invoke and observe
/// `ctx.cancel.is_cancelled()` flip, which requires constructing it here
/// first rather than letting `for_test` mint one no caller can reach.
pub fn test_ctx(cwd: PathBuf) -> (ToolCtx, TestHandles) {
    let subagents = Arc::new(FakeSubagentHost::new());
    let events = Arc::new(RecordingEventSink::new());
    let cancel = CancellationToken::new();
    let agent_id = AgentId::new();

    let ctx = ToolCtx {
        cancel: cancel.clone(),
        ..ToolCtx::for_test(
            agent_id,
            cwd,
            subagents.clone() as Arc<dyn SubagentHost>,
            events.clone() as Arc<dyn EventSink>,
        )
    };
    let handles = TestHandles {
        subagents,
        events,
        cancel,
    };
    (ctx, handles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::agent::{Budget, ResultStatus, SubagentSpec};
    use conway_core::log::SubagentMode;

    fn fork_spec(prompt: &str) -> SubagentSpec {
        SubagentSpec::fork(prompt, Budget::default())
    }

    #[tokio::test]
    async fn fake_subagent_host_records_start_and_returns_scripted_id() {
        let scripted_id = AgentId::new();
        let host = FakeSubagentHost::new().with_next_agent_id(scripted_id);
        let parent = AgentId::new();

        let returned = host
            .start(parent, parent, fork_spec("do it"))
            .await
            .unwrap();
        assert_eq!(returned, scripted_id);

        let started = host.started();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].0, scripted_id);
        assert_eq!(started[0].1.prompt, "do it");
        assert!(matches!(started[0].1.mode, SubagentMode::Fork));
    }

    #[tokio::test]
    async fn fake_subagent_host_await_result_returns_scripted_result() {
        let agent_id = AgentId::new();
        let scripted =
            AgentResult::new(agent_id, SessionId::new(), ResultStatus::Completed, "done");
        let host = FakeSubagentHost::new().with_result(agent_id, scripted.clone());

        let result = host.await_result(AgentId::new(), agent_id).await.unwrap();
        assert_eq!(result, scripted);
    }

    #[tokio::test]
    async fn fake_subagent_host_await_result_unknown_id_errs() {
        let host = FakeSubagentHost::new();
        let err = host
            .await_result(AgentId::new(), AgentId::new())
            .await
            .unwrap_err();
        assert!(matches!(err, RuntimeError::AgentNotFound { .. }));
    }

    #[tokio::test]
    async fn fake_subagent_host_records_steer_and_cancel() {
        let host = FakeSubagentHost::new();
        let caller = AgentId::new();
        let target = AgentId::new();

        host.steer(caller, target, "keep going".into())
            .await
            .unwrap();
        host.cancel(caller, target, "stop".into(), CancelMode::Immediate)
            .await
            .unwrap();

        assert_eq!(
            host.steers(),
            vec![(caller, target, "keep going".to_string())]
        );
        assert_eq!(
            host.cancels(),
            vec![(caller, target, "stop".to_string(), CancelMode::Immediate)]
        );
    }

    #[tokio::test]
    async fn fake_subagent_host_ask_errors_when_scripted_and_still_records() {
        let parent = AgentId::new();
        let host =
            FakeSubagentHost::new().with_ask_error(RuntimeError::AgentNotFound { agent: parent });

        let err = host
            .ask(parent, parent, fork_spec("do it"))
            .await
            .unwrap_err();
        assert!(matches!(err, RuntimeError::AgentNotFound { agent } if agent == parent));
        assert_eq!(host.asks().len(), 1, "the failed call is still recorded");
    }

    #[test]
    fn recording_event_sink_records_in_order() {
        let sink = RecordingEventSink::new();
        sink.emit(Event::ToolProgress {
            call_id: "tc_1".into(),
            note: "a".into(),
        });
        sink.emit(Event::ToolProgress {
            call_id: "tc_1".into(),
            note: "b".into(),
        });
        let events = sink.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Event::ToolProgress { note, .. } if note == "a"));
        assert!(matches!(&events[1], Event::ToolProgress { note, .. } if note == "b"));
    }

    #[test]
    fn test_ctx_builds_fully_wired_tool_ctx() {
        let (ctx, handles) = test_ctx(PathBuf::from("/tmp/x"));
        assert_eq!(ctx.cwd, PathBuf::from("/tmp/x"));
        assert!(!ctx.cancel.is_cancelled());

        // The ToolCtx's cancel token is derived from the same handle: it
        // observes handles.cancel being cancelled.
        handles.cancel.cancel();
        assert!(ctx.cancel.is_cancelled());

        ctx.events.emit(Event::ToolProgress {
            call_id: "tc_1".into(),
            note: "hi".into(),
        });
        assert_eq!(handles.events.events().len(), 1);
    }
}
