//! Board item `01M1WVQM440XZDSP0KC664HJ7S` (architecture review finding
//! F11): `ConwayBuilder::with_artifact_writer` is new -- before this item
//! there was no injection point at all for `ArtifactWriter` (the trait
//! itself and its handle/error types were already nameable from a
//! facade-only crate, per `crates/conway/tests/plugin_surface.rs`'s own
//! `RecordingArtifactWriter`, but nothing let an embedder make conway
//! actually USE one). This test proves the new method's override is
//! genuinely consulted by a real turn -- not merely accepted by the
//! builder, and not merely constructible in isolation -- by installing a
//! `ContextHook` that writes through `ContextHookCtx::artifacts`, the one
//! production call site `ArtifactWriter::write` has.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use conway::plugin::{
    async_trait, ArtifactWriteError, ContextHook, ContextHookCtx, ContextPayload,
};
use conway::test_support::base_config;
use conway::{AgentId, ArtifactWriter, ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::ids::{BackendId, ModelId, ModelRef};
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};

const T: Duration = Duration::from_secs(5);

/// A facade-external, non-default `ArtifactWriter`: records every call
/// rather than resolving/confining a path on disk the way the runtime's own
/// `AgentArtifactWriter` does.
#[derive(Default)]
struct RecordingArtifactWriter {
    calls: Mutex<Vec<(AgentId, String, Vec<u8>)>>,
}

#[async_trait]
impl ArtifactWriter for RecordingArtifactWriter {
    async fn write(
        &self,
        agent_id: AgentId,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<PathBuf, ArtifactWriteError> {
        self.calls
            .lock()
            .unwrap()
            .push((agent_id, name.to_string(), bytes));
        Ok(PathBuf::from(name))
    }
}

/// Writes exactly one artifact through `ctx.artifacts` on every
/// `before_request` -- the only production caller of `ArtifactWriter::write`
/// (a hook holds `ContextHookCtx::artifacts`; nothing else in the runtime
/// does).
struct WritingHook;

#[async_trait]
impl ContextHook for WritingHook {
    async fn before_request(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
    ) -> ContextPayload {
        let _ = ctx
            .artifacts
            .write(
                "greeting.txt",
                b"hello from a custom ArtifactWriter".to_vec(),
            )
            .await;
        payload
    }
}

fn route() -> ModelRef {
    ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }
}

#[tokio::test]
async fn with_artifact_writer_is_genuinely_consulted_by_a_context_hook() {
    let writer = Arc::new(RecordingArtifactWriter::default());

    let conway = ConwayBuilder::from_parts(base_config())
        .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_router(Arc::new(FakeRouter::single(route())))
        .with_context_hook(Arc::new(WritingHook))
        .with_artifact_writer(writer.clone())
        .build()
        .expect("build with a custom, non-default ArtifactWriter should succeed");

    let session = tokio::time::timeout(T, conway.new_session(SessionSpec::default()))
        .await
        .expect("new_session must not hang")
        .expect("new_session should succeed");
    let turn = tokio::time::timeout(T, session.prompt("Hello, custom artifact writer!"))
        .await
        .expect("prompt must not hang")
        .expect("prompt should succeed");
    let _ = tokio::time::timeout(T, turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    // The proof: the installed `ContextHook` must have actually called
    // `write` on the INJECTED writer, not the runtime's own built-in
    // `AgentArtifactWriter` -- not merely accepted at build time.
    let calls = writer.calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "the hook's before_request must call into the injected ArtifactWriter exactly \
         once, for the one turn this test ran"
    );
    assert_eq!(calls[0].1, "greeting.txt");
    assert_eq!(calls[0].2, b"hello from a custom ArtifactWriter".to_vec());
}
