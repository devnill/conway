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
