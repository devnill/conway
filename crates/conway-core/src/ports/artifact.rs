//! The `ArtifactWriter` port: the one safe way an in-process
//! [`crate::ports::ContextHook`] can spill content to disk (board item
//! 01KZ84437RMKHP5DJX7RMHH7JY).
//!
//! # The problem this closes
//!
//! A hook that wants to write oversized tool output to a file (leaving a
//! short preview plus a pointer in context -- the `TruncationPolicy::
//! Artifact` shape, board item 01KYTN3A9SPDMRG610YSB5QQXX) has to put that
//! file somewhere the agent can later `read` back -- which means somewhere
//! under the agent's own confinement root, or the subsequent read is denied
//! by the exact same root-containment check every tool call already goes
//! through (`conway_runtime::permission::PermissionBroker::check_root`).
//!
//! Before this port existed, [`crate::ports::ContextHookCtx`] handed a hook
//! no root and no cwd at all, which left a hook author with only bad
//! options: reach for ambient filesystem access and guess a path (bypassing
//! the per-agent cwd tracking every tool goes through); receive a path
//! out-of-band at plugin construction, fixed at install (which cannot
//! follow an agent that `chdir`s, or a subagent with a narrower root); or
//! write outside the root and produce an artifact the agent provably cannot
//! read back.
//!
//! # Why this is a trait (port) rather than a plain value
//!
//! Actually writing bytes to disk is I/O, and `conway-core` performs none
//! (see this crate's own root doc) -- `conway_core::containment::
//! CanonicalRoot` is this crate's one, deliberate exception, and it only
//! canonicalizes, it never creates or writes a file. So, exactly like
//! [`crate::ports::SubagentHost`]/[`crate::ports::SubagentHandle`], the
//! CONTRACT lives here and the CONCRETE resolution+write logic lives in
//! `conway-runtime` (`conway_runtime::permission`), reusing -- never
//! restating (P-14) -- the identical `AgentRoot`/`CanonicalRoot`/
//! `resolve_like_the_tool_will` machinery `PermissionBroker::check_root`
//! already uses to confine a tool's own path arguments. A hook's artifact
//! lands under the SAME root a tool's `write` call would be confined to,
//! by construction, not by a second, hand-rolled copy of that rule.
//!
//! # Why a "ready write" method, not a raw root or cwd handle
//!
//! Three shapes were weighed for what [`crate::ports::ContextHookCtx`]
//! should carry (see this item's own board record for the full
//! deliberation):
//!
//! 1. **The resolved `AgentRoot` itself.** Wrong on two counts. First,
//!    layering: `AgentRoot` lives in `conway-runtime`, which depends on
//!    `conway-core` -- not the other way around -- so `conway-core`'s
//!    `ContextHookCtx` cannot even name that type. Second, even ignoring
//!    layering, handing over the raw root would force every hook author to
//!    re-implement "resolve the candidate against cwd, then
//!    `CanonicalRoot::contains`, then handle `Unconfined`/`Broken`"
//!    themselves -- restating exactly the kind of safety-critical
//!    resolution logic P-14 exists to keep singular. This is not
//!    hypothetical: two INLINED copies of this crate's own path-resolution
//!    rule have already independently dropped the NUL-byte guard (see
//!    `conway_runtime::permission::resolve_like_the_tool_will`'s own doc).
//! 2. **A [`crate::ports::CwdHandle`]-style capability.** Wrong in kind, not
//!    degree: `CwdHandle` tracks a freely mutable "where am I" location with
//!    NO containment semantics of its own -- its own doc is explicit that
//!    `set` performs no root/containment check, because "cwd was never the
//!    boundary" (GP-13's cwd-vs-root split: cwd is where I am, freely
//!    mutable, never a security boundary; root is what I can reach,
//!    parent-set, narrow-only). Exposing a `CwdHandle` to a hook wanting to
//!    WRITE safely would just relocate the exact guess-and-hope problem
//!    this port exists to close.
//! 3. **A purpose-built accessor that performs the write itself, refusing
//!    anything that resolves outside the root** -- what this module is.
//!    [`ArtifactWriteHandle::write`] is the ONLY place a hook-written
//!    artifact's path is resolved and checked; there is no second call a
//!    hook author could get subtly wrong, because there is no path
//!    resolution surface exposed to get wrong in the first place -- the
//!    hook supplies a name, the handle returns where it actually landed (or
//!    refuses).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::ArtifactWriteError;
use crate::ids::AgentId;

/// Resolves `name` against this agent's own cwd/confinement root and writes
/// `bytes` to it, exactly as a tool's own path-confined write would be
/// resolved and checked -- or refuses, if the resolved candidate does not
/// land inside the root.
///
/// Implemented once, in `conway-runtime`, atop the same `AgentRoot`/
/// `CanonicalRoot` machinery `PermissionBroker::check_root` uses (P-14).
/// `conway-core` ships no implementation (nothing in this crate writes
/// files; the crate's one I/O exception, `containment`, only reads path
/// metadata and is labeled at the crate root); see this module's own doc for
/// why the contract lives here regardless.
#[async_trait]
pub trait ArtifactWriter: Send + Sync + 'static {
    /// `name` is resolved exactly as a tool's own path argument would be:
    /// relative joins onto this agent's current cwd, absolute passes
    /// through unchanged -- see `conway_runtime::permission::
    /// resolve_like_the_tool_will`, the one implementation of that rule
    /// this reuses. Returns the resolved, written-to path on success.
    ///
    /// P-10: `name` may be model-influenced (e.g. echoed from a tool's own
    /// output). This must never panic on any input, including a `name`
    /// containing `..`, a NUL byte, or an absolute path naming a location
    /// entirely outside the root -- every one of those is a refusal
    /// ([`ArtifactWriteError::OutsideRoot`]), never a crash.
    async fn write(
        &self,
        agent_id: AgentId,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<PathBuf, ArtifactWriteError>;
}

/// The [`ContextHookCtx`](crate::ports::ContextHookCtx)-facing capability: a
/// cheaply-`Clone`d handle wrapping an [`ArtifactWriter`], with this
/// invocation's own [`AgentId`] baked in -- the same narrowing
/// [`crate::ports::SubagentHandle`] applies to `Arc<dyn SubagentHost>` (a
/// concrete handle on the context type, not a raw trait object a hook could
/// pass a foreign `agent_id` through).
#[derive(Clone)]
pub struct ArtifactWriteHandle {
    writer: Arc<dyn ArtifactWriter>,
    agent_id: AgentId,
}

impl std::fmt::Debug for ArtifactWriteHandle {
    // Manual impl: `Arc<dyn ArtifactWriter>` carries no `Debug` bound
    // (mirrors `SubagentHandle`'s own manual `Debug`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactWriteHandle")
            .field("agent_id", &self.agent_id)
            .field("writer", &"<dyn ArtifactWriter>")
            .finish()
    }
}

impl ArtifactWriteHandle {
    /// Wraps `writer`, baking `agent_id` in as the identity every
    /// [`Self::write`] call uses -- there is no parameter through which a
    /// caller could supply a different one.
    pub fn new(writer: Arc<dyn ArtifactWriter>, agent_id: AgentId) -> Self {
        Self { writer, agent_id }
    }

    /// A handle backed by a private [`ArtifactWriter`] that performs no I/O
    /// and always succeeds, returning `name` unchanged (as a [`PathBuf`]) --
    /// for a [`crate::ports::ContextHookCtx`] fixture in a test for a hook
    /// that never calls [`Self::write`] (board item 01KZJ5S3ZC8SPWTX94C4HTEC2R).
    ///
    /// **Why this exists, given `ArtifactWriteHandle::new` already takes any
    /// `Arc<dyn ArtifactWriter>`.** `ContextHookCtx::artifacts` became a
    /// required field when the real containment guarantee landed
    /// (`ArtifactWriter`'s own module doc): correct, but it means EVERY
    /// construction site -- including a test fixture for a hook that writes
    /// nothing -- must supply *some* implementation. Before this existed, a
    /// hook author's only path was to hand-roll one: an `#[async_trait] impl
    /// ArtifactWriter` returning `Ok(PathBuf::from(name))`, plus the `Arc`/
    /// `PathBuf` imports that come with it -- exactly what `conway-core`'s
    /// own `ports::plugin` tests did privately (GP-03/P-6: a capability a
    /// built-in needs belongs on the shared surface, not kept private).
    ///
    /// **Not gated behind `feature = "fakes"`, unlike this crate's other test
    /// doubles (`crate::fakes`).** Those exist to stand in for a PRODUCTION
    /// capability a test wants to script (a backend that returns scripted
    /// responses, a store that records what was appended) -- gating them
    /// keeps this crate's "no I/O, except behind an explicit test feature"
    /// promise legible. (That promise is itself a forward declaration today
    /// -- `containment` does unfeatured `std::fs` I/O; see the crate root's
    /// label and board item 01KZDC30CBY9CPJ8YEM7HSRV0Y. The gating rationale
    /// below is unaffected: it is about not adding a SECOND exception.)
    /// A no-op artifact writer scripts nothing and performs
    /// no I/O either way, so it carries none of that risk; gating it would
    /// only have reproduced the exact reachability gap this constructor
    /// exists to close, since `conway`'s facade does not forward `fakes` to
    /// its own dependents (only its `[dev-dependencies]` enable it, for this
    /// workspace's own test suites) -- see `crate::ports`' own module doc
    /// for the two prior production-fallback exceptions
    /// (`MinimalRouter`/`AlwaysClosedHealthRegistry`) this is a third,
    /// identically narrow instance of.
    pub fn noop(agent_id: AgentId) -> Self {
        Self::new(Arc::new(NoopArtifactWriter), agent_id)
    }

    /// Writes `bytes` to `name`, resolved and confined exactly as
    /// [`ArtifactWriter::write`] documents. See this type's own doc, and
    /// this module's own doc, for the full containment guarantee.
    pub async fn write(&self, name: &str, bytes: Vec<u8>) -> Result<PathBuf, ArtifactWriteError> {
        self.writer.write(self.agent_id, name, bytes).await
    }
}

/// The private implementation behind [`ArtifactWriteHandle::noop`]. Not
/// itself exported -- see that constructor's own doc for why a name that
/// exists purely for tests belongs behind a constructor on an already-
/// justified type, rather than as a second top-level name in the facade's
/// curated `conway::plugin` module (whose own doc requires every name in it
/// to be justified by an authoring need, not a testing convenience).
struct NoopArtifactWriter;

#[async_trait]
impl ArtifactWriter for NoopArtifactWriter {
    async fn write(
        &self,
        _agent_id: AgentId,
        name: &str,
        _bytes: Vec<u8>,
    ) -> Result<PathBuf, ArtifactWriteError> {
        Ok(PathBuf::from(name))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A minimal in-memory `ArtifactWriter` double: records every
    /// `(agent_id, name)` it was called with and returns a scripted result,
    /// without touching the real filesystem. Enough to prove
    /// `ArtifactWriteHandle` bakes in `agent_id` and delegates faithfully --
    /// `conway-runtime`'s own tests cover the REAL containment guard (this
    /// crate ships no filesystem-writing implementation, so it cannot
    /// construct a real one -- its one I/O exception, `containment`, only
    /// resolves paths; see the crate root's label).
    #[derive(Default)]
    struct RecordingWriter {
        result: Mutex<Option<Result<PathBuf, ArtifactWriteError>>>,
        last_call: Mutex<Option<(AgentId, String)>>,
    }

    #[async_trait]
    impl ArtifactWriter for RecordingWriter {
        async fn write(
            &self,
            agent_id: AgentId,
            name: &str,
            _bytes: Vec<u8>,
        ) -> Result<PathBuf, ArtifactWriteError> {
            *self.last_call.lock().unwrap() = Some((agent_id, name.to_string()));
            self.result
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Ok(PathBuf::from(name)))
        }
    }

    /// Dependency-free async-test helper (`conway-core` has no `tokio`/
    /// `futures-executor` dev-dependency) -- mirrors `ports::plugin`'s and
    /// `ports::subagent`'s own `block_on` exactly.
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
    fn write_always_passes_the_handles_own_agent_id() {
        let agent_id = AgentId::new();
        let writer = Arc::new(RecordingWriter::default());
        let handle = ArtifactWriteHandle::new(writer.clone(), agent_id);

        block_on(handle.write("notes.txt", b"hi".to_vec())).unwrap();
        assert_eq!(
            *writer.last_call.lock().unwrap(),
            Some((agent_id, "notes.txt".to_string()))
        );
    }

    #[test]
    fn write_surfaces_the_writers_error_unchanged() {
        let agent_id = AgentId::new();
        let writer = Arc::new(RecordingWriter::default());
        *writer.result.lock().unwrap() = Some(Err(ArtifactWriteError::OutsideRoot {
            path: "/etc/passwd".into(),
        }));
        let handle = ArtifactWriteHandle::new(writer, agent_id);

        let err = block_on(handle.write("../../etc/passwd", b"hi".to_vec())).unwrap_err();
        assert_eq!(
            err,
            ArtifactWriteError::OutsideRoot {
                path: "/etc/passwd".into()
            }
        );
    }

    /// A clone shares the same underlying writer `Arc` and the same
    /// baked-in `agent_id` -- the cheap-`Clone` contract
    /// [`crate::ports::ContextHookCtx`] relies on (it derives `Clone`).
    #[test]
    fn clones_share_the_writer_and_carry_the_same_agent_id() {
        let agent_id = AgentId::new();
        let writer = Arc::new(RecordingWriter::default());
        let handle = ArtifactWriteHandle::new(writer.clone(), agent_id);
        let clone = handle.clone();

        block_on(clone.write("a.txt", b"hi".to_vec())).unwrap();
        assert_eq!(
            *writer.last_call.lock().unwrap(),
            Some((agent_id, "a.txt".to_string()))
        );
    }

    #[test]
    fn artifact_write_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ArtifactWriteHandle>();
    }
}
