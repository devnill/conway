//! In-crate test doubles: everything downstream `conway-tools` work items
//! need to unit-test a tool with zero runtime.
//!
//! `conway-core` ships its own `SubagentHost`/`EventSink` fakes behind its
//! `fakes` feature (`conway_core::fakes::{FakeSubagentHost,
//! CollectingEventSink}`), but this crate defines its own lightweight
//! doubles instead of depending on that feature, for two reasons binding on
//! this work item's criteria:
//! 1. The `FakeSubagentHost` this crate's downstream tests need must also
//!    record `steer`/`cancel` calls (conway-core's fake does not — it is a
//!    no-op on both) and must fail `await_result` for an unknown agent id
//!    (conway-core's fake always synthesizes a fallback result instead).
//! 2. Keeping this crate's dependency on `conway-core` at its default
//!    feature set (no `fakes`) keeps the boundary between "port consumer"
//!    and "port test double provider" clean; this crate's fakes have zero
//!    extra dependencies beyond what's already required to implement the
//!    ports themselves.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;

use conway_core::agent::{AgentResult, AgentTreeSnapshot, SubagentSpec};
use conway_core::error::RuntimeError;
use conway_core::event::Event;
use conway_core::ids::{AgentId, SessionId};
use conway_core::ports::{
    CancellationToken, EventSink, EventSinkHandle, PluginConfig, SubagentHost, ToolCtx,
};

/// An in-memory [`SubagentHost`] that only records calls and plays back
/// scripted results. Contains no fork/spawn logic — it is a recorder.
#[derive(Debug)]
pub struct FakeSubagentHost {
    /// The `AgentId` every `start` call returns, until overridden via
    /// [`Self::with_next_agent_id`].
    next_agent_id: AgentId,
    started: Mutex<Vec<(AgentId, SubagentSpec)>>,
    steers: Mutex<Vec<(AgentId, String)>>,
    cancels: Mutex<Vec<(AgentId, String)>>,
    results: Mutex<HashMap<AgentId, AgentResult>>,
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

    /// The `AgentId` the next `start` call will return.
    pub fn next_agent_id(&self) -> AgentId {
        self.next_agent_id
    }

    /// Every `(child_id, spec)` pair recorded by `start`, in call order.
    pub fn started(&self) -> Vec<(AgentId, SubagentSpec)> {
        self.started.lock().unwrap().clone()
    }

    /// Every `(target, text)` pair recorded by `steer`, in call order.
    pub fn steers(&self) -> Vec<(AgentId, String)> {
        self.steers.lock().unwrap().clone()
    }

    /// Every `(target, reason)` pair recorded by `cancel`, in call order.
    pub fn cancels(&self) -> Vec<(AgentId, String)> {
        self.cancels.lock().unwrap().clone()
    }
}

impl Default for FakeSubagentHost {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SubagentHost for FakeSubagentHost {
    async fn start(&self, _parent: AgentId, spec: SubagentSpec) -> Result<AgentId, RuntimeError> {
        let child = self.next_agent_id;
        self.started.lock().unwrap().push((child, spec));
        Ok(child)
    }

    async fn steer(&self, target: AgentId, text: String) -> Result<(), RuntimeError> {
        self.steers.lock().unwrap().push((target, text));
        Ok(())
    }

    /// Terminates immediately (never blocks): returns the scripted result
    /// for `target`, or `Err(RuntimeError::AgentNotFound)` if none was
    /// configured via [`Self::with_result`].
    async fn await_result(&self, target: AgentId) -> Result<AgentResult, RuntimeError> {
        self.results
            .lock()
            .unwrap()
            .get(&target)
            .cloned()
            .ok_or(RuntimeError::AgentNotFound { agent: target })
    }

    async fn cancel(&self, target: AgentId, reason: String) -> Result<(), RuntimeError> {
        self.cancels.lock().unwrap().push((target, reason));
        Ok(())
    }

    fn tree(&self) -> AgentTreeSnapshot {
        AgentTreeSnapshot {
            root: self.next_agent_id,
            nodes: Vec::new(),
            at: Utc::now(),
        }
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
pub fn test_ctx(cwd: PathBuf) -> (ToolCtx, TestHandles) {
    let subagents = Arc::new(FakeSubagentHost::new());
    let events = Arc::new(RecordingEventSink::new());
    let cancel = CancellationToken::new();

    let ctx = ToolCtx {
        agent_id: AgentId::new(),
        session_id: SessionId::new(),
        cwd,
        cancel: cancel.clone(),
        events: events.clone() as EventSinkHandle,
        subagents: subagents.clone() as Arc<dyn SubagentHost>,
        config: Arc::new(PluginConfig::default()),
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

        let returned = host.start(parent, fork_spec("do it")).await.unwrap();
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

        let result = host.await_result(agent_id).await.unwrap();
        assert_eq!(result, scripted);
    }

    #[tokio::test]
    async fn fake_subagent_host_await_result_unknown_id_errs() {
        let host = FakeSubagentHost::new();
        let err = host.await_result(AgentId::new()).await.unwrap_err();
        assert!(matches!(err, RuntimeError::AgentNotFound { .. }));
    }

    #[tokio::test]
    async fn fake_subagent_host_records_steer_and_cancel() {
        let host = FakeSubagentHost::new();
        let target = AgentId::new();

        host.steer(target, "keep going".into()).await.unwrap();
        host.cancel(target, "stop".into()).await.unwrap();

        assert_eq!(host.steers(), vec![(target, "keep going".to_string())]);
        assert_eq!(host.cancels(), vec![(target, "stop".to_string())]);
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
