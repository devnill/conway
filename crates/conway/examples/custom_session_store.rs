//! A third-party `SessionStore` decorator: an audit trail over every
//! header-mutating call, wrapping whatever store a host already runs in
//! production -- the shape a compliance-minded embedder reaches for when it
//! wants to know "who/what wrote this session's header, and when" without
//! forking `conway-session` or reimplementing storage from scratch.
//!
//! ```console
//! cargo run -p conway --example custom_session_store
//! ```
//!
//! There is exactly one extension mechanism for this
//! (`conway::ConwayBuilder::with_session_store`) -- a third-party store (or,
//! as here, a DECORATOR wrapping one) is installed on the identical surface
//! the built-in `JsonlSessionStore` would be (`crates/conway/src/
//! builder.rs`'s own `jsonl-store` default), no privileged path either way.
//! `SessionStore` is one of the two ports board item
//! `01M2M5HVF43FERZA5DFJ88CWDX` ruled "proof-required" rather than
//! "harness-fixed" (`conway_core::ports::SessionStore`'s own module doc
//! comment references the same ruling): a host with its own database, or --
//! as demonstrated here -- its own audit requirements layered over an
//! existing store, is exactly the genuine extension point that ruling kept
//! open.
//!
//! [`AuditingSessionStore`] below delegates every call to an inner
//! `Arc<dyn SessionStore>` unchanged, and additionally appends a one-line
//! entry to its own in-memory log for every call that mutates a session's
//! header or its log (`create`, `append`, `fork`, `remove`, `set_ephemeral`,
//! `add_label`, `remove_label`) -- the same shape a real embedder would use
//! to feed a compliance/SIEM sink, deliberately kept in-memory here so this
//! example stays offline and dependency-free.
//!
//! Runs fully offline against `conway_testkit`'s fakes, mirroring
//! `minimal_session.rs`'s own wiring -- this example's point is the STORE,
//! not the backend or router, so both of those stay the simplest fake that
//! exists.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use conway::backend::{BackendId, ModelId};
use conway::plugin::{LiveOwner, SeqRange, StoreError};
use conway::{
    ConwayBuilder, LogRecord, LogSeq, ModelRef, PluginSelection, SessionFilter, SessionId,
    SessionMeta, SessionSpec, SessionStore,
};
use conway_testkit::{FakeBackend, FakeRouter, FakeStore};

/// A [`SessionStore`] decorator that appends one line to its own in-memory
/// audit log for every mutating call, then delegates to `inner` unchanged.
/// See the module doc for why this, rather than a from-scratch store, is
/// the realistic third-party shape for this port.
struct AuditingSessionStore {
    inner: Arc<dyn SessionStore>,
    log: Mutex<Vec<String>>,
}

impl AuditingSessionStore {
    fn new(inner: Arc<dyn SessionStore>) -> Self {
        Self {
            inner,
            log: Mutex::new(Vec::new()),
        }
    }

    /// Appends `entry` to the audit log. A real embedder would forward this
    /// to its own sink (a log line, a SIEM event, a database row) instead of
    /// holding it in memory.
    fn audit(&self, entry: String) {
        self.log
            .lock()
            .expect("audit log mutex poisoned")
            .push(entry);
    }

    /// The audit trail collected so far, oldest first.
    fn entries(&self) -> Vec<String> {
        self.log.lock().expect("audit log mutex poisoned").clone()
    }
}

#[async_trait]
impl SessionStore for AuditingSessionStore {
    async fn create(&self, meta: SessionMeta) -> Result<SessionId, StoreError> {
        let agent_def = meta.agent_def.clone();
        let sid = self.inner.create(meta).await?;
        self.audit(format!("create: session={sid} agent_def={agent_def:?}"));
        Ok(sid)
    }

    async fn append(&self, sid: &SessionId, rec: LogRecord) -> Result<LogSeq, StoreError> {
        let seq = self.inner.append(sid, rec).await?;
        self.audit(format!("append: session={sid} seq={seq}"));
        Ok(seq)
    }

    async fn read(&self, sid: &SessionId, range: SeqRange) -> Result<Vec<LogRecord>, StoreError> {
        self.inner.read(sid, range).await
    }

    async fn head(&self, sid: &SessionId) -> Result<LogSeq, StoreError> {
        self.inner.head(sid).await
    }

    async fn fork(
        &self,
        parent: &SessionId,
        at: LogSeq,
        meta: SessionMeta,
    ) -> Result<SessionId, StoreError> {
        let child = self.inner.fork(parent, at, meta).await?;
        self.audit(format!("fork: parent={parent} at={at} child={child}"));
        Ok(child)
    }

    async fn meta(&self, sid: &SessionId) -> Result<SessionMeta, StoreError> {
        self.inner.meta(sid).await
    }

    async fn children(&self, sid: &SessionId) -> Result<Vec<SessionId>, StoreError> {
        self.inner.children(sid).await
    }

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>, StoreError> {
        self.inner.list(filter).await
    }

    async fn remove(&self, sid: &SessionId) -> Result<(), StoreError> {
        self.inner.remove(sid).await?;
        self.audit(format!("remove: session={sid}"));
        Ok(())
    }

    async fn set_ephemeral(&self, sid: &SessionId, ephemeral: bool) -> Result<(), StoreError> {
        self.inner.set_ephemeral(sid, ephemeral).await?;
        self.audit(format!(
            "set_ephemeral: session={sid} ephemeral={ephemeral}"
        ));
        Ok(())
    }

    async fn add_label(&self, sid: &SessionId, label: &str) -> Result<(), StoreError> {
        self.inner.add_label(sid, label).await?;
        self.audit(format!("add_label: session={sid} label={label:?}"));
        Ok(())
    }

    async fn remove_label(&self, sid: &SessionId, label: &str) -> Result<(), StoreError> {
        self.inner.remove_label(sid, label).await?;
        self.audit(format!("remove_label: session={sid} label={label:?}"));
        Ok(())
    }

    async fn live_owner(&self) -> Result<Option<LiveOwner>, StoreError> {
        self.inner.live_owner().await
    }

    async fn touch_live_owner(&self, pid: u32) -> Result<(), StoreError> {
        self.inner.touch_live_owner(pid).await
    }

    async fn clear_live_owner(&self) -> Result<(), StoreError> {
        self.inner.clear_live_owner().await
    }
}

/// See `discover_getting_started.rs`'s own copy of this same helper:
/// isolates `ConwayBuilder::discover()` from whatever happens to be
/// configured on the machine running this example, purely for
/// reproducibility. A real host application does not do this.
fn isolate_ambient_config_for_this_example() {
    let scratch = std::env::temp_dir().join(format!(
        "conway-custom-session-store-example-{}",
        std::process::id()
    ));
    let config_dir = scratch.join("config_dir-config-home");
    let cwd = scratch.join("cwd");
    std::fs::create_dir_all(&config_dir).expect("create scratch CONWAY_CONFIG_DIR");
    std::fs::create_dir_all(&cwd).expect("create scratch cwd");
    std::env::set_var("CONWAY_CONFIG_DIR", &config_dir);
    std::env::set_current_dir(&cwd).expect("set scratch cwd");
}

#[tokio::main]
async fn main() -> conway::Result<()> {
    isolate_ambient_config_for_this_example();

    let store = Arc::new(AuditingSessionStore::new(Arc::new(FakeStore::new())));
    let backend = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let route = ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    };

    let conway = ConwayBuilder::discover()?
        // No tool will ever be called by this offline echo turn, so `Deny`
        // (no permission gate needed) plus no built-in plugins keeps this
        // example self-contained, mirroring `discover_getting_started.rs`.
        .with_gate_config(conway::gates::GateConfig {
            mode: conway::gates::GateMode::Deny,
            ..conway::gates::GateConfig::default()
        })
        .with_builtin_plugins(PluginSelection::None)
        .with_backend(backend)
        .with_router(Arc::new(FakeRouter::single(route)))
        .with_session_store(store.clone())
        .build()?;

    let session = conway.new_session(SessionSpec::default()).await?;
    let turn = session.prompt("Hello, conway!").await?;
    println!("prompt -> {}", turn.text().await?);
    let _ = turn.result().await?;

    // A host's own bookkeeping, layered on top of the store it already
    // uses -- exercised directly against the injected store, not only
    // through the turn above.
    store.add_label(&session.id(), "reviewed").await?;
    let meta = store.meta(&session.id()).await?;
    println!("session meta after labeling: labels={:?}", meta.labels);

    println!("audit trail:");
    for entry in store.entries() {
        println!("  {entry}");
    }

    Ok(())
}
