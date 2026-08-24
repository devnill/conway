//! The `PathStore` port (DESIGN-context-path §2.6, §2.9, §4.4).
//!
//! Stores immutable, content-addressed `PathSelection` objects under their
//! EXPANDED [`SelectionKey`] and serves the derived, rebuildable
//! session→selection reverse index that retention (§4.4) accelerates with.
//! The port is read-only from the caller's perspective: the implementation
//! holds the root path and performs all I/O; the port never hands out file
//! handles or the root path (GP-03).
//!
//! MVP impl: `FsPathStore` in `conway-session` — one JSON file per selection
//! under `<root>/paths/<key-hex>`, plus a rebuildable `paths-index.jsonl`
//! reverse index, mirroring `JsonlSessionStore` + `SessionIndex`.

use async_trait::async_trait;

use crate::error::PathStoreError;
use crate::ids::SessionId;
use crate::path::{PathSelection, SelectionKey};

/// Content-addressed, write-once storage for path selections, plus the
/// derived retention reverse index (DESIGN §2.6, §2.9, §4.4).
///
/// A selection is stored under the [`SelectionKey`] computed over its
/// **expanded** node list (prefix chains flattened first, §2.3). Because the
/// key is model-free, ten siblings routing to four models share one stored
/// object (§2.6). Write-once: a second `put` of the SAME key is a no-op — the
/// selection is immutable once stored, and same key ⇒ same expanded
/// selection ⇒ same content.
///
/// The reverse index ([`PathStore::selections_referencing`]) is a derived,
/// rebuildable accelerator (§4.4) — NEVER the source of truth; rebuildable by
/// scanning stored bodies' nodes' `node.record.session`. It keys a selection
/// by the sessions in its OWN nodes only, NOT transitively the prefix's
/// sessions: the prefix is itself a stored selection with its own index lines,
/// so transitive coverage would double-count (see `FsPathStore`'s coverage
/// doc for the §4.4 reasoning).
///
/// # A labeled exception: this port is engine-internal, not part of the
/// extension surface (board item `01M0EMCK55628YJXGBQY8YGXHE`)
///
/// Twenty other `conway-core` port traits are re-exported through
/// `conway::plugin` (or the facade root) precisely so a third party can name
/// and implement them — `SessionStore`, `MemoryStore`, `HookRunner`,
/// `EventSink`, and the rest. `PathStore` is a deliberate, stated exception,
/// not an oversight: it is defined here, `conway::ConwayBuilder` constructs
/// and injects a default `FsPathStore` (`crates/conway-session`), but neither
/// this trait nor `FsPathStore` is reachable from a crate that depends only
/// on `conway`.
///
/// **Why, weighed rather than assumed.** The candidate consumer for a
/// third-party `PathStore` would be a `Curator` — the one seam that works in
/// terms of `PathSelection`/`PathOp` at all. `Curator::curate` receives a
/// [`crate::ports::CurateCtx`] carrying `store: Arc<dyn SessionStore>` and
/// `resolver: Arc<TranscriptResolver>` (the §11.5 read surface) — no path
/// store. Board item `01M0EMAC4CCDQ8QJYM21RXPKRY` put a real curator
/// (`conway-plugin-trim`) through that seam: it reads records, names
/// `PathOp`s, and lets `ValidatedPath::derive` do the deriving. It never
/// needed to `put` or `get` a `PathSelection` directly, because a curator's
/// job ends at proposing ops — persisting the resulting content-addressed
/// selection, and resolving an existing one's prefix chain, is the engine's
/// own bookkeeping, done once per turn regardless of which curators ran.
/// That is real evidence, not speculation: the one production consumer that
/// could have needed this port didn't.
///
/// The invariants above compound the case for keeping it internal even if a
/// consumer later appears: write-once content addressing and a reverse
/// index that is documented as "NEVER the source of truth, rebuildable" are
/// correctness properties the engine's retention machinery (§4.4) depends on
/// holding *exactly*, across every implementation in the process — the kind
/// of invariant a second, independently-written implementation is most
/// likely to get subtly wrong (e.g. hashing the unexpanded selection, or
/// treating the reverse index as authoritative). That is a materially
/// different risk than swapping `SessionStore`'s storage backend, which
/// carries no derived/rebuildable index of its own.
///
/// If a genuine third-party use case for `PathStore` shows up, the honest
/// path is to re-export this trait through `conway::plugin`, matching every
/// other port — `conway::ConwayBuilder::with_path_store` already exists
/// (mirroring `with_session_store`'s override-the-default shape) but today
/// only an embedder willing to depend on `conway-core` directly can name its
/// `Arc<dyn PathStore>` parameter, which is this same exception surfacing at
/// a second declaration site (see that method's own doc). Widening is
/// exactly this item's option 1 — not to route around this doc by depending
/// on `conway-core` directly.
///
/// **Resolved consequence (board item `01M0J7KWQDM4PMPD0TFFKSFTES`):** this
/// exception, combined with the `jsonl-store` feature being the only source
/// of a default `PathStore`, means a facade-only caller building with
/// `jsonl-store` OFF cannot get a `PathStore` from ANY source — not the
/// default (feature-gated), not `with_path_store` (parameter unnameable) —
/// even when they supply their own `SessionStore` via
/// `with_session_store` (which needs no feature). `ConwayBuilder::build()`
/// fails in that configuration; that item confirmed the combination was
/// untested in either direction, decided this was the correct, INTENTIONAL
/// consequence of this decision rather than a bug to route around, and
/// fixed `build()`'s error message (previously suggesting `with_path_store`
/// as if it were reachable, which it is not for this exact caller) to name
/// the real constraint instead. Recorded here, not left
/// discovered-and-forgotten, per that item's own acceptance criterion 4.
#[async_trait]
pub trait PathStore: Send + Sync + 'static {
    /// Store `selection` content-addressed under its EXPANDED `SelectionKey`.
    /// The key is computed by expanding `selection.prefix` (fetching prefix
    /// selections from THIS store, bounded by the prefix-depth limit →
    /// [`PathStoreError::PrefixChainTooDeep`]) then [`SelectionKey::from_nodes`]
    /// over the flattened node list (DESIGN §2.3/§2.6).
    ///
    /// Write-once: a second `put` of the SAME key is a no-op (the selection is
    /// immutable once stored; same key ⇒ same expanded selection). Returns the
    /// computed [`SelectionKey`].
    async fn put(&self, selection: PathSelection) -> Result<SelectionKey, PathStoreError>;

    /// Fetch the selection stored under `key`. Absent →
    /// [`PathStoreError::NotFound`].
    async fn get(&self, key: &SelectionKey) -> Result<PathSelection, PathStoreError>;

    /// Selection keys whose stored body references any record in `sid`. A
    /// derived, rebuildable accelerator (DESIGN §4.4) — NEVER the source of
    /// truth; rebuildable by scanning stored bodies' nodes'
    /// `node.record.session`.
    async fn selections_referencing(
        &self,
        sid: &SessionId,
    ) -> Result<Vec<SelectionKey>, PathStoreError>;
}
