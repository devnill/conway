//! A third-party `PathStore` behind a host's own storage substrate.
//!
//! ```console
//! cargo run -p conway --example custom_path_store
//! ```
//!
//! # `PathStore` is engine-internal, not facade-only -- this example is the
//! documented exception, not a workaround
//!
//! Unlike every other port this crate curates for a third party, `PathStore`
//! is deliberately NOT re-exported through `conway::plugin`
//! (`conway_core::ports::PathStore`'s own module doc, board item
//! `01M0EMCK55628YJXGBQY8YGXHE`) -- and `ConwayBuilder::with_path_store`'s
//! own doc names exactly who this example is written for: "a facade-only
//! caller ... cannot name this method's parameter type and is not expected
//! to need to. This method exists for parity with `with_session_store` and
//! for callers already depending on `conway-core` directly." That is this
//! file: it imports `conway_core::ports::PathStore` and
//! `conway_core::error::PathStoreError` directly (both are already ordinary,
//! non-dev dependencies of this crate -- `crates/conway/Cargo.toml`), rather
//! than through `conway::plugin` -- the same escape hatch that doc
//! describes, exercised rather than merely asserted. Every other type this
//! example needs (`PathSelection`, `PathNode`, `SelectionKey`, `RecordRef`,
//! ...) IS re-exported at the `conway` crate root, so only these two names
//! come from `conway-core` directly.
//!
//! Board item `01M2M5HVF43FERZA5DFJ88CWDX` ruled `PathStore`
//! "proof-required" rather than "harness-fixed": it stays a genuine
//! extension point because a host with its own database is a real,
//! anticipated caller of `with_path_store` (see that method's own doc) --
//! even though no production consumer has needed one yet
//! (`conway_core::ports::PathStore`'s own doc: "the one production consumer
//! that could have needed this port didn't"). This example is that
//! anticipated caller, made concrete: [`HostDatabasePathStore`] stands in
//! for a host's own already-existing storage (a real embedder would swap
//! the `Mutex<HashMap<..>>` fields below for actual database queries; the
//! PORT usage -- write-once content addressing, prefix expansion, the
//! session-keyed reverse index -- stays identical either way).
//!
//! Because nothing in this crate's own dependency graph composes a context
//! path at runtime (that tool lives in `conway-plugin-path`, not a
//! dependency of this crate), this example exercises the port directly --
//! `put`/`get`/`selections_referencing` -- the same three calls
//! `compose_context_path`'s production implementation
//! (`conway_runtime::context::RuntimeContextPathHost`) makes, and separately
//! proves the store type-checks as `ConwayBuilder::with_path_store`'s real
//! injection point by building a full, running `Conway` with it installed.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use conway::backend::{BackendId, ModelId};
use conway::{
    ConwayBuilder, LogSeq, ModelRef, NodeProvenance, NodeStamp, PathNode, PathSelection,
    PluginSelection, RecordRef, SelectionKey, Selector, SessionId, SessionSpec,
};
use conway_core::error::PathStoreError;
use conway_core::ports::PathStore;
use conway_testkit::{FakeBackend, FakeRouter, FakeStore};

/// Stands in for a host's own already-existing storage substrate -- a real
/// embedder replaces the two `Mutex<HashMap<..>>` fields with real queries
/// against whatever database it already runs. See the module doc for why a
/// third party writing exactly this impl is the ruling this example proves
/// out.
#[derive(Default)]
struct HostDatabasePathStore {
    selections: Mutex<HashMap<SelectionKey, PathSelection>>,
    /// The derived, rebuildable reverse index (DESIGN §4.4): which stored
    /// selections reference a given session's own records, keyed by that
    /// session's OWN nodes only -- never transitively through a prefix (see
    /// `conway_core::ports::PathStore`'s own module doc for why).
    by_session: Mutex<HashMap<SessionId, BTreeSet<SelectionKey>>>,
}

impl HostDatabasePathStore {
    fn new() -> Self {
        Self::default()
    }

    /// Follows `selection`'s own prefix chain (each link fetched from THIS
    /// store), flattening every ancestor's nodes ahead of `selection`'s own,
    /// oldest first -- the expansion `SelectionKey::from_nodes` (DESIGN
    /// §2.3) is computed over. A toy depth bound stands in for the real
    /// `MAX_ANCESTRY_DEPTH` `FsPathStore` reuses (`conway-session`).
    fn expand_nodes(&self, selection: &PathSelection) -> Result<Vec<PathNode>, PathStoreError> {
        const MAX_DEPTH: usize = 64;

        let mut chain = vec![selection.nodes.clone()];
        let mut next_prefix = selection.prefix.clone();
        let mut depth = 0usize;
        while let Some(key) = next_prefix {
            depth += 1;
            if depth > MAX_DEPTH {
                return Err(PathStoreError::PrefixChainTooDeep { depth });
            }
            let stored = self
                .selections
                .lock()
                .expect("selections mutex poisoned")
                .get(&key)
                .cloned();
            let parent = stored.ok_or_else(|| PathStoreError::NotFound { key: key.clone() })?;
            next_prefix = parent.prefix.clone();
            chain.push(parent.nodes);
        }

        chain.reverse();
        Ok(chain.into_iter().flatten().collect())
    }
}

#[async_trait]
impl PathStore for HostDatabasePathStore {
    async fn put(&self, selection: PathSelection) -> Result<SelectionKey, PathStoreError> {
        let expanded = self.expand_nodes(&selection)?;
        let key = SelectionKey::from_nodes(&expanded);

        let mut selections = self.selections.lock().expect("selections mutex poisoned");
        if selections.contains_key(&key) {
            // Write-once: a second `put` of the same content is a no-op
            // (DESIGN §2.6/§2.9).
            return Ok(key);
        }

        let mut by_session = self.by_session.lock().expect("by_session mutex poisoned");
        for node in &selection.nodes {
            by_session
                .entry(node.record.session)
                .or_default()
                .insert(key.clone());
        }

        selections.insert(key.clone(), selection);
        Ok(key)
    }

    async fn get(&self, key: &SelectionKey) -> Result<PathSelection, PathStoreError> {
        self.selections
            .lock()
            .expect("selections mutex poisoned")
            .get(key)
            .cloned()
            .ok_or_else(|| PathStoreError::NotFound { key: key.clone() })
    }

    async fn selections_referencing(
        &self,
        sid: &SessionId,
    ) -> Result<Vec<SelectionKey>, PathStoreError> {
        Ok(self
            .by_session
            .lock()
            .expect("by_session mutex poisoned")
            .get(sid)
            .map(|keys| keys.iter().cloned().collect())
            .unwrap_or_default())
    }
}

fn own_node(session: SessionId, seq: u64) -> PathNode {
    PathNode {
        record: RecordRef {
            session,
            seq: LogSeq(seq),
        },
        stamp: NodeStamp::Own,
        prov: NodeProvenance {
            selected_by: Selector::DefaultRule,
            at: Utc::now(),
        },
    }
}

/// See `discover_getting_started.rs`'s own copy of this same helper.
fn isolate_ambient_config_for_this_example() {
    let scratch = std::env::temp_dir().join(format!(
        "conway-custom-path-store-example-{}",
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
    let store = Arc::new(HostDatabasePathStore::new());

    // A root session's own selection: no prefix, two of its own records.
    let session_a = SessionId::new();
    let selection_a = PathSelection {
        prefix: None,
        nodes: vec![own_node(session_a, 1), own_node(session_a, 2)],
        incoherence: vec![],
    };
    let key_a = store.put(selection_a.clone()).await?;
    println!("put session A's own selection -> {key_a}");

    // A second put of the IDENTICAL content is a no-op: write-once,
    // content-addressed storage returns the same key rather than a new one.
    let key_a_again = store.put(selection_a).await?;
    assert_eq!(key_a, key_a_again, "same content must hash to the same key");
    println!("re-put of the same content -> the identical key (write-once)");

    // A forked child's selection prefixes A's -- ten forks of the same
    // parent share A's stored object rather than each copying it (DESIGN
    // §2.6's "ten heads, one selection" story).
    let session_b = SessionId::new();
    let selection_b = PathSelection {
        prefix: Some(key_a.clone()),
        nodes: vec![own_node(session_b, 1)],
        incoherence: vec![],
    };
    let key_b = store.put(selection_b).await?;
    println!("put session B's selection (prefixed by A) -> {key_b}");

    let round_tripped = store.get(&key_b).await?;
    println!(
        "get({key_b}) round-trips: prefix={:?}, {} of B's own nodes",
        round_tripped.prefix,
        round_tripped.nodes.len()
    );

    // The reverse index is keyed by a selection's OWN nodes only -- B's
    // selection references session B, not (transitively) session A, even
    // though B's full rendered path includes A's records via the prefix.
    let referencing_a = store.selections_referencing(&session_a).await?;
    let referencing_b = store.selections_referencing(&session_b).await?;
    println!("selections referencing A directly: {referencing_a:?}");
    println!("selections referencing B directly: {referencing_b:?}");
    assert!(referencing_a.contains(&key_a));
    assert!(!referencing_a.contains(&key_b));
    assert!(referencing_b.contains(&key_b));

    // Finally, prove this is genuinely `ConwayBuilder::with_path_store`'s
    // real injection point, not merely a standalone `PathStore` impl: build
    // a full `Conway` with it installed alongside the simplest fake backend
    // and router, exactly the shape a host embeds today.
    isolate_ambient_config_for_this_example();
    let backend = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let route = ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    };
    let conway = ConwayBuilder::discover()?
        .with_gate_config(conway::gates::GateConfig {
            mode: conway::gates::GateMode::Deny,
            ..conway::gates::GateConfig::default()
        })
        .with_builtin_plugins(PluginSelection::None)
        .with_backend(backend)
        .with_router(Arc::new(FakeRouter::single(route)))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_path_store(store)
        .build()?;

    let session = conway.new_session(SessionSpec::default()).await?;
    let turn = session.prompt("Hello, conway!").await?;
    println!("prompt -> {}", turn.text().await?);
    let _ = turn.result().await?;

    Ok(())
}
