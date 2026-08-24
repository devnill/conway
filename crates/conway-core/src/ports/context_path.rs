//! The `ContextPathHost` port: the capability a `Tool` needs to compose and
//! freeze a context-path head (decision `01M0K4QT6MBXPD6PXMBBBD2P7B`; the
//! writer half, `write_head`/`resolve_default_path`, lives in
//! `conway-runtime`, not here -- this port is what makes it reachable from a
//! `Tool::invoke`, exactly as [`crate::ports::SubagentHost`] is what makes
//! fork/spawn reachable from one).
//!
//! # Why a port, not a widened `CurateCtx`
//!
//! `CurateCtx` (`crate::ports::curator`) carries `model: Option<ModelId>` as
//! an IDENTIFIER for sizing, never a callable backend, and a `Curator` runs
//! per-turn before routing -- inference there would be re-entrant. Composing
//! a path from an operator's stated intent needs a MODEL to have already
//! done the interpreting (decision `01M0K4QT6MBXPD6PXMBBBD2P7B`), so it is a
//! tool, called where inference is already in flight, not a curator. This
//! port backs that tool's `ToolCtx` field, never `CurateCtx`.
//!
//! # Why a port, not a raw `Arc<dyn PathStore>` on `ToolCtx`
//!
//! `PathStore` (`crate::ports::path_store`) is a deliberate, stated
//! exception to this crate's "every port is part of the extension surface"
//! rule -- engine-internal, not re-exported through `conway::plugin`, board
//! item `01M0EMCK55628YJXGBQY8YGXHE`. That decision's own doc names the
//! honest widening path IF a genuine consumer appeared: re-export the trait.
//! This port takes the OTHER path, the one `SubagentHost`/`SubagentHandle`
//! already established for exactly this shape of problem: a narrow,
//! purpose-built capability (`default_path`/`resolve_records`/`set_head` --
//! not `put`/`get`/`selections_referencing`) that an implementation backs
//! with `PathStore`/`SessionStore`/`TranscriptResolver` internally, never
//! handing the raw store out. `PathStore` itself stays exactly as
//! unreachable-by-name from a facade-only crate as it was before this port
//! existed -- nothing here re-opens `01M0EMCK55628YJXGBQY8YGXHE`.
//!
//! # Read surface, deliberately wider than `set_head`'s write surface
//!
//! [`Self::resolve_records`] takes an arbitrary slice of [`RecordRef`]s,
//! each naming ANY session, not just the caller's own -- mirroring
//! `CurateCtx::store`'s own "a curator may reference any record in the
//! store" grant (§11.5) and `CommandOutcome::Checkout`'s own "deliberately
//! widens what a command can name" precedent. [`Self::default_path`] and
//! [`Self::set_head`], by contrast, are narrowed to ONE session by
//! [`ContextPathHandle`] below -- a tool composes and freezes only the
//! calling session's own head, never another session's.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::ids::{LogSeq, SessionId};
use crate::log::LogRecord;
use crate::path::{PathError, RecordRef, ValidatedPath};

/// The context-path composition capability (see module doc). Object-safe by
/// construction (no generic params, `#[async_trait]`), so the runtime can
/// hold `Arc<dyn ContextPathHost>` -- the same shape [`crate::ports::
/// SubagentHost`] already establishes for the identical problem.
#[async_trait]
pub trait ContextPathHost: Send + Sync + 'static {
    /// `session`'s CURRENT default path (DESIGN §2.5, §6) -- the head it
    /// reads for today, expanded and record-resolved. This is the base a
    /// tool composes FROM: it already carries `session`'s own tail (every
    /// record from the current head's `covers_upto` onward, or the whole
    /// log if there is no head yet), so a caller that only ADDS foreign
    /// records and never explicitly asks to drop that tail can never
    /// silently lose it (see `conway_runtime::context::path`'s own
    /// `covers_upto_for` doc for the trap this is what avoids).
    async fn default_path(&self, session: SessionId) -> Result<ValidatedPath, PathError>;

    /// Resolve each of `refs` to its logged record, honestly -- through the
    /// SAME masked, ancestry-aware resolution [`Self::default_path`] itself
    /// reads through (`TranscriptResolver::resolve_prefix`), so a record an
    /// operator excluded via `ContextMask` stays excluded here too: this is
    /// a new COMPOSITION surface, not a new way to bypass an existing
    /// exclusion. A `RecordRef` naming a masked or unresolvable record is
    /// simply absent from the returned map, never a partial/corrupt entry --
    /// the caller (`ContextPathHandle::resolve_records`) reports it, per
    /// `PathError::UnresolvableNode`'s own contract, as `derive_with`'s
    /// `Include` refusal does for a ref in neither the base nor this map.
    async fn resolve_records(
        &self,
        refs: &[RecordRef],
    ) -> Result<BTreeMap<RecordRef, Arc<LogRecord>>, PathError>;

    /// Freeze `path` as `session`'s new context-path HEAD -- the ONE call
    /// that reaches `conway_runtime::context::path::write_head`. `path` is
    /// flattened into a fresh, prefix-less [`crate::path::PathSelection`]
    /// before it is stored: a tool-composed selection has no natural prefix
    /// chain of its own to share (unlike a curator's `derive`, which
    /// re-uses `self`'s existing chain), so there is nothing to lose by
    /// storing the expanded list directly.
    async fn set_head(&self, session: SessionId, path: ValidatedPath) -> Result<LogSeq, PathError>;
}

/// A [`ContextPathHost`] bound to ONE session -- the `ToolCtx`-facing
/// capability a context-composing tool actually gets, mirroring
/// [`crate::ports::SubagentHandle`] exactly: [`Self::default_path`] and
/// [`Self::set_head`] bake the caller's own `session` in structurally (no
/// parameter through which a call could name a different one), while
/// [`Self::resolve_records`] stays deliberately wide (see the module doc).
#[derive(Clone)]
pub struct ContextPathHandle {
    host: Arc<dyn ContextPathHost>,
    session: SessionId,
}

impl std::fmt::Debug for ContextPathHandle {
    // Manual impl: `Arc<dyn ContextPathHost>` carries no `Debug` bound,
    // mirroring `SubagentHandle`'s own manual `Debug` exactly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextPathHandle")
            .field("session", &self.session)
            .field("host", &"<dyn ContextPathHost>")
            .finish()
    }
}

impl ContextPathHandle {
    /// Wraps `host`, baking `session` in as the one session
    /// [`Self::default_path`]/[`Self::set_head`] ever act on.
    pub fn new(host: Arc<dyn ContextPathHost>, session: SessionId) -> Self {
        Self { host, session }
    }

    /// A double that refuses every call with [`PathError::UnresolvableNode`]
    /// -- the `ToolCtx::for_test` default for tools that never exercise
    /// context-path composition, mirroring [`crate::ports::PluginEventHandle
    /// ::noop`]'s "unconditional, no I/O" shape (`crate::ports`'s own module
    /// doc: no default here may perform I/O, and a refusal performs none).
    /// A test that DOES exercise this capability supplies a real
    /// [`ContextPathHost`] instead (`conway_testkit`'s fakes, or a real
    /// runtime), the same escape hatch `ToolCtx::for_test`'s own doc
    /// describes for `subagents`/`events`.
    pub fn noop() -> Self {
        Self {
            host: Arc::new(NoopContextPathHost),
            session: SessionId::new(),
        }
    }

    /// This handle's own session's current default path.
    pub async fn default_path(&self) -> Result<ValidatedPath, PathError> {
        self.host.default_path(self.session).await
    }

    /// Resolves `refs` -- any session, per [`ContextPathHost::
    /// resolve_records`]'s own doc.
    pub async fn resolve_records(
        &self,
        refs: &[RecordRef],
    ) -> Result<BTreeMap<RecordRef, Arc<LogRecord>>, PathError> {
        self.host.resolve_records(refs).await
    }

    /// Freezes `path` as this handle's own session's new head. There is no
    /// parameter to name a different session -- see this type's own doc.
    pub async fn set_head(&self, path: ValidatedPath) -> Result<LogSeq, PathError> {
        self.host.set_head(self.session, path).await
    }
}

/// The private implementation behind [`ContextPathHandle::noop`]. Not
/// itself exported -- mirrors `ports::artifact`'s own private
/// `NoopArtifactWriter` exactly.
struct NoopContextPathHost;

fn refused(record: RecordRef) -> PathError {
    PathError::UnresolvableNode {
        record,
        detail: "no ContextPathHost configured for this ToolCtx fixture (ContextPathHandle::noop)"
            .to_string(),
    }
}

#[async_trait]
impl ContextPathHost for NoopContextPathHost {
    async fn default_path(&self, session: SessionId) -> Result<ValidatedPath, PathError> {
        Err(refused(RecordRef {
            session,
            seq: LogSeq::ZERO,
        }))
    }

    async fn resolve_records(
        &self,
        refs: &[RecordRef],
    ) -> Result<BTreeMap<RecordRef, Arc<LogRecord>>, PathError> {
        match refs.first() {
            Some(r) => Err(refused(*r)),
            None => Ok(BTreeMap::new()),
        }
    }

    async fn set_head(
        &self,
        session: SessionId,
        _path: ValidatedPath,
    ) -> Result<LogSeq, PathError> {
        Err(refused(RecordRef {
            session,
            seq: LogSeq::ZERO,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn noop(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);

        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        loop {
            if let Poll::Ready(val) = fut.as_mut().poll(&mut cx) {
                return val;
            }
        }
    }

    #[test]
    fn noop_default_path_refuses_rather_than_panicking() {
        let handle = ContextPathHandle::noop();
        let err = block_on(handle.default_path()).unwrap_err();
        assert!(matches!(err, PathError::UnresolvableNode { .. }));
    }

    #[test]
    fn noop_resolve_records_of_empty_slice_succeeds_with_empty_map() {
        let handle = ContextPathHandle::noop();
        let map = block_on(handle.resolve_records(&[])).unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn noop_set_head_refuses_rather_than_panicking() {
        let handle = ContextPathHandle::noop();
        let path = ValidatedPath::default_path(Vec::new());
        let err = block_on(handle.set_head(path)).unwrap_err();
        assert!(matches!(err, PathError::UnresolvableNode { .. }));
    }

    #[test]
    fn context_path_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ContextPathHandle>();
    }
}
