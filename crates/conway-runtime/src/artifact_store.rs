//! `AgentArtifactWriter`: the one implementation of
//! [`conway_core::ports::ArtifactWriter`] (board item
//! 01KZ84437RMKHP5DJX7RMHH7JY -- the containment guard that makes it safe
//! for a `ContextHook` to spill content to disk).
//!
//! **Reuses, never restates, the exact machinery `PermissionBroker::
//! check_root` already confines every tool's own path arguments with
//! (P-14):** `crate::permission::resolve_like_the_tool_will` for name
//! resolution, and `AgentRoot`'s three-way match (`Unconfined` proceeds,
//! `Broken` fails closed, `Confined(root)` checks `CanonicalRoot::
//! contains`) for the containment decision itself. `AgentLoop::run_inner`
//! constructs one of these per agent, from the SAME `CwdHandle`/`AgentRoot`
//! it already builds for that agent's tool calls (see that method's own
//! doc) -- never a second, independent reconstruction.

use std::path::PathBuf;

use async_trait::async_trait;

use conway_core::containment::Containment;
use conway_core::error::ArtifactWriteError;
use conway_core::ids::AgentId;
use conway_core::ports::{ArtifactWriter, CwdHandle};

use crate::permission::{resolve_like_the_tool_will, AgentRoot};

/// See this module's own doc.
#[derive(Clone)]
pub struct AgentArtifactWriter {
    cwd: CwdHandle,
    root: AgentRoot,
}

impl AgentArtifactWriter {
    /// `cwd`/`root` are cloned in, never rebuilt here -- see this module's
    /// own doc for why this must be the SAME pair `AgentLoop::run_inner`
    /// already holds for that agent's tool calls.
    pub fn new(cwd: CwdHandle, root: AgentRoot) -> Self {
        Self { cwd, root }
    }
}

#[async_trait]
impl ArtifactWriter for AgentArtifactWriter {
    /// `agent_id` is accepted (the port's contract) but not otherwise used:
    /// this writer is already scoped to exactly one agent at construction
    /// (mirroring `chdir`/`root`'s own per-agent construction in
    /// `AgentLoop::run_inner`), so there is nothing here for it to select
    /// between.
    async fn write(
        &self,
        _agent_id: AgentId,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<PathBuf, ArtifactWriteError> {
        let cwd = self.cwd.current();
        let Some(candidate) = resolve_like_the_tool_will(&cwd, name) else {
            return Err(ArtifactWriteError::InvalidName {
                detail: format!("name contains a NUL byte: {name:?}"),
            });
        };

        // THE GUARD (board item 01KZ84437RMKHP5DJX7RMHH7JY): the identical
        // three-way match `PermissionBroker::check_root` applies to a
        // tool's own declared path arguments, reused verbatim rather than
        // restated (P-14).
        match &self.root {
            AgentRoot::Unconfined => {}
            AgentRoot::Broken => return Err(ArtifactWriteError::RootBroken),
            AgentRoot::Confined(root) => match root.contains(&candidate) {
                Containment::Inside => {}
                Containment::Outside | Containment::Undecidable => {
                    return Err(ArtifactWriteError::OutsideRoot {
                        path: candidate.display().to_string(),
                    });
                }
            },
        }

        if let Some(parent) = candidate.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| ArtifactWriteError::Io {
                    detail: format!(
                        "failed to create parent directories for {}: {err}",
                        candidate.display()
                    ),
                })?;
        }
        tokio::fs::write(&candidate, &bytes)
            .await
            .map_err(|err| ArtifactWriteError::Io {
                detail: format!("failed to write {}: {err}", candidate.display()),
            })?;

        Ok(candidate)
    }
}

#[cfg(test)]
mod tests {
    use conway_core::containment::CanonicalRoot;
    use conway_core::ports::ArtifactWriteHandle;
    use std::sync::Arc;
    use tempfile::TempDir;

    use super::*;

    fn confined(tmp: &TempDir) -> AgentArtifactWriter {
        let root = CanonicalRoot::new(tmp.path()).unwrap();
        AgentArtifactWriter::new(
            CwdHandle::new(tmp.path().to_path_buf()),
            AgentRoot::Confined(root),
        )
    }

    /// The ordinary case: a plain relative name resolves under `cwd` (which
    /// is inside the root) and the write actually lands there.
    #[tokio::test]
    async fn write_under_root_succeeds_and_returns_the_resolved_path() {
        let tmp = TempDir::new().unwrap();
        let writer = confined(&tmp);
        let handle = ArtifactWriteHandle::new(Arc::new(writer), AgentId::new());

        let path = handle.write("spill.txt", b"hello".to_vec()).await.unwrap();
        assert_eq!(path, tmp.path().join("spill.txt"));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"hello");
    }

    /// THE CONTAINMENT TEST (ACCEPTANCE): a name that walks `..` out of the
    /// root -- exactly the shape a spill hook echoing model-influenced
    /// content could receive -- is refused, and nothing is written to disk
    /// at the escaped location.
    #[tokio::test]
    async fn write_outside_root_via_dotdot_is_refused() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();

        let root = CanonicalRoot::new(&repo).unwrap();
        let writer = AgentArtifactWriter::new(CwdHandle::new(repo.clone()), AgentRoot::Confined(root));
        let handle = ArtifactWriteHandle::new(Arc::new(writer), AgentId::new());

        let err = handle
            .write("../outside/escaped.txt", b"pwned".to_vec())
            .await
            .unwrap_err();
        assert!(matches!(err, ArtifactWriteError::OutsideRoot { .. }));
        assert!(!outside.join("escaped.txt").exists());
    }

    /// Same escape shape, but via an absolute path naming a location
    /// entirely outside the root -- the other way a hook-supplied name
    /// could try to reach outside its bounds.
    #[tokio::test]
    async fn write_outside_root_via_absolute_path_is_refused() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();

        let root = CanonicalRoot::new(&repo).unwrap();
        let writer = AgentArtifactWriter::new(CwdHandle::new(repo.clone()), AgentRoot::Confined(root));
        let handle = ArtifactWriteHandle::new(Arc::new(writer), AgentId::new());

        let escaped = outside.join("escaped.txt").to_string_lossy().into_owned();
        let err = handle.write(&escaped, b"pwned".to_vec()).await.unwrap_err();
        assert!(matches!(err, ArtifactWriteError::OutsideRoot { .. }));
        assert!(!outside.join("escaped.txt").exists());
    }

    /// `AgentRoot::Broken` fails closed: every write is denied, never
    /// silently downgraded to unconfined.
    #[tokio::test]
    async fn write_with_broken_root_is_always_denied() {
        let tmp = TempDir::new().unwrap();
        let writer = AgentArtifactWriter::new(CwdHandle::new(tmp.path().to_path_buf()), AgentRoot::Broken);
        let handle = ArtifactWriteHandle::new(Arc::new(writer), AgentId::new());

        let err = handle.write("f.txt", b"hi".to_vec()).await.unwrap_err();
        assert_eq!(err, ArtifactWriteError::RootBroken);
    }

    /// `AgentRoot::Unconfined` (no root configured for this agent at all)
    /// proceeds unrestricted -- the identical no-op posture
    /// `PermissionBroker::check_root` gives an unconfined tool call, not a
    /// new, stricter default invented for this port.
    #[tokio::test]
    async fn write_with_unconfined_root_proceeds() {
        let tmp = TempDir::new().unwrap();
        let writer = AgentArtifactWriter::new(
            CwdHandle::new(tmp.path().to_path_buf()),
            AgentRoot::Unconfined,
        );
        let handle = ArtifactWriteHandle::new(Arc::new(writer), AgentId::new());

        let path = handle.write("f.txt", b"hi".to_vec()).await.unwrap();
        assert_eq!(path, tmp.path().join("f.txt"));
    }

    /// A relative name is resolved against the LIVE `cwd` -- a `chdir`
    /// between construction and this call is observed, mirroring every
    /// tool's own `ctx.cwd` resolution.
    #[tokio::test]
    async fn write_resolves_against_the_live_cwd_not_a_stale_snapshot() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let root = CanonicalRoot::new(tmp.path()).unwrap();
        let cwd = CwdHandle::new(tmp.path().to_path_buf());
        let writer = AgentArtifactWriter::new(cwd.clone(), AgentRoot::Confined(root));
        let handle = ArtifactWriteHandle::new(Arc::new(writer), AgentId::new());

        cwd.set(sub.clone()).unwrap();
        let path = handle.write("f.txt", b"hi".to_vec()).await.unwrap();
        assert_eq!(path, sub.join("f.txt"));
    }

    /// A NUL byte in the name is a distinct refusal
    /// (`InvalidName`) from an out-of-root refusal -- it is never even
    /// evaluated against the root.
    #[tokio::test]
    async fn write_with_nul_byte_is_invalid_name_not_outside_root() {
        let tmp = TempDir::new().unwrap();
        let writer = confined(&tmp);
        let handle = ArtifactWriteHandle::new(Arc::new(writer), AgentId::new());

        let err = handle.write("a\0b", b"hi".to_vec()).await.unwrap_err();
        assert!(matches!(err, ArtifactWriteError::InvalidName { .. }));
    }

    /// A name naming a nested, not-yet-existing directory under the root
    /// still succeeds (parents are created) -- the ordinary `write`/`edit`
    /// tool behavior, unchanged for this port.
    #[tokio::test]
    async fn write_creates_missing_parent_directories_under_root() {
        let tmp = TempDir::new().unwrap();
        let writer = confined(&tmp);
        let handle = ArtifactWriteHandle::new(Arc::new(writer), AgentId::new());

        let path = handle
            .write("nested/dir/f.txt", b"hi".to_vec())
            .await
            .unwrap();
        assert_eq!(path, tmp.path().join("nested/dir/f.txt"));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"hi");
    }

    /// BREAK-THE-GUARD EVIDENCE (item's own acceptance criterion): with the
    /// containment check removed -- simulated here by driving the SAME
    /// resolve step this writer uses but skipping the `root.contains` match
    /// entirely -- a `..`-escaping name resolves to (and could write) a
    /// path outside the root. This is not exercised through
    /// `AgentArtifactWriter` itself (which always applies the guard); it
    /// pins the fact that the escape is REAL and only the guard above stops
    /// it, so a future edit that deletes the `match` arm silently
    /// reopens exactly this hole.
    #[test]
    fn without_the_guard_the_same_resolution_step_would_escape_the_root() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();

        let candidate = resolve_like_the_tool_will(&repo, "../outside/escaped.txt").unwrap();
        // `resolve_like_the_tool_will` alone (the same helper `write` calls
        // first) does no containment checking -- it lexically joins `..`
        // straight onto `repo`, which a naive `fs::write(candidate, ..)`
        // would happily follow straight out of the root.
        assert_eq!(candidate, repo.join("../outside/escaped.txt"));
        // Only `CanonicalRoot::contains` (the guard `write` applies AFTER
        // resolution, and refuses to skip) recognizes this candidate is
        // actually outside `repo` -- proving the guard, not the resolution
        // step, is what stands between this candidate and a real write.
        let root = CanonicalRoot::new(&repo).unwrap();
        assert_eq!(root.contains(&candidate), Containment::Outside);
    }
}
