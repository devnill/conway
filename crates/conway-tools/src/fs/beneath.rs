//! Open-relative enforcement of `conway.fs`'s own per-agent root
//! ([S1.5]/[the retirement item]): the check and the use become ONE
//! `openat`-family syscall sequence, so a symlink swapped in between
//! "resolve" and "open" can no longer defeat confinement the way a
//! canonicalize-then-open two-step could.
//!
//! # Why this is not just `crate::fs::check_root` called earlier
//!
//! The retired predecessor (`check_root`, a plain
//! `conway_core::containment::CanonicalRoot::contains` call) answered "is
//! `candidate` inside the root, AS OF RIGHT NOW" and then handed the caller
//! back a plain `PathBuf` to open SEPARATELY, via ordinary `tokio::fs`. Any
//! filesystem change between those two steps -- most concretely, an
//! attacker (or a racing sibling agent, or the model's own next tool call)
//! replacing an existing path component with a symlink after the check
//! passed -- defeats it: the check saw a real directory, the open follows a
//! symlink the check never saw.
//!
//! This module closes that gap by never producing a `PathBuf` a caller opens
//! independently. `resolve` answers containment (using
//! [`CanonicalRoot::relative_if_inside`], the SAME symlink-aware algorithm
//! `check_root` used) but the value it returns on `Confined` is a path
//! RELATIVE to the root, meaningless on its own -- it can only be used
//! through [`cap_std::fs::Dir`], opened at the SAME canonical root, whose
//! `open`/`create`/`rename`/`metadata` methods resolve every component
//! themselves, at call time, refusing (not merely failing to notice) any
//! component -- intermediate or leaf, pre-existing or raced in after
//! `resolve` returned -- that would step outside the `Dir`'s own root. Even
//! if `resolve`'s own answer were somehow stale, the `Dir` call that
//! actually touches the filesystem re-verifies independently; `resolve` is a
//! convenience that computes an offset, never the trust boundary.
//!
//! A hand-rolled `openat` loop (this crate already depends on `nix`, which
//! exposes the raw syscall) was considered and rejected: correctly handling
//! every component -- intermediate vs. leaf, existing vs. not-yet-existing
//! (a `write` target legitimately doesn't exist), an intra-root symlink
//! (legitimate, must still be followed) vs. an escaping one (must not), a
//! non-existent parent chain a `write` must create without ever crossing a
//! symlink to do it -- is exactly the amount of subtlety `cap-std`
//! (`cap_std`/`cap_primitives`, used by `wasmtime`/`wasi-common` for the
//! identical guarantee) already gets right and keeps getting right; a
//! from-scratch reimplementation here would be a second, less-audited copy
//! of the same logic, the thing this whole item's retirement exists to stop
//! doing.
//!
//! # What this does NOT close
//!
//! `glob`/`grep` walk a whole tree (`crate::fs::walk_files`, `ignore`-crate
//! based, which does not integrate with `cap_std::fs::Dir`); reimplementing
//! that walk atop `Dir` is out of this item's scope. Those two tools instead
//! validate their search ROOT through `resolve` plus a probe `Dir::open_dir`
//! (closing the TOCTOU window for the root argument itself, at the moment
//! the walk begins) and rely on `ignore::WalkBuilder`'s own
//! `follow_links(false)` default (unchanged, not part of this item) to keep
//! the walk itself from crossing a symlink out of the validated root.

use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use cap_std::ambient_authority;
use cap_std::fs::Dir;

use conway_core::containment::CanonicalRoot;
use conway_core::error::ToolError;
use conway_core::ports::ToolCtx;

use super::FULL_ROOT_CONFIG_KEY;

/// Where a resolved candidate stands relative to this agent's `conway.fs`
/// root, right now -- see the module doc for why `Confined`'s `relative`
/// path is a convenience offset, not itself the enforcement.
#[derive(Debug)]
pub(crate) enum Access {
    /// No root configured for this agent (today's pre-existing, opt-in-only
    /// behavior, Decision 01KZ7PMYR72T329G3RKWMW2SX8, unchanged): every
    /// operation proceeds exactly as it did before `[S1.5]` existed.
    Unconfined,
    /// A root is configured, and the candidate resolved inside it as of
    /// this call. `relative` is the candidate expressed relative to `root`
    /// -- the shape every [`cap_std::fs::Dir`] method below needs.
    Confined {
        root: CanonicalRoot,
        relative: PathBuf,
    },
}

/// Reads this agent's `conway.fs` root from `ctx.config` (if any) and
/// answers containment for `candidate` -- the resolution half of every
/// function below, factored out because `edit`'s read and write share one
/// call, and `cd`'s metadata check needs the identical answer.
///
/// `Err(ToolError::Denied)` for a misconfigured root (not a string, or does
/// not canonicalize) or a `candidate` that does not resolve inside it right
/// now -- fail closed, matching `Containment`'s own "can't check is never
/// allow" discipline. This early answer is advisory for the `Confined` case
/// (see module doc); the `Denied` case for an OUTSIDE candidate is still a
/// real, useful fast-path refusal that avoids opening anything at all for
/// the common case (an obviously wrong path), even though the LATER `Dir`
/// call is what actually enforces it against a race.
pub(crate) fn resolve(ctx: &ToolCtx, candidate: &Path) -> Result<Access, ToolError> {
    let Some(configured) = ctx.config.values.get(FULL_ROOT_CONFIG_KEY) else {
        return Ok(Access::Unconfined);
    };
    let Some(root_str) = configured.as_str() else {
        return Err(ToolError::Denied {
            reason: format!(
                "{FULL_ROOT_CONFIG_KEY} is configured but is not a string ({configured}); \
                 refusing to resolve any path against it"
            ),
        });
    };
    let root = CanonicalRoot::new(Path::new(root_str)).map_err(|err| ToolError::Denied {
        reason: format!(
            "{FULL_ROOT_CONFIG_KEY} ({root_str}) does not canonicalize: {err}; refusing every \
             path under an unresolvable root"
        ),
    })?;
    match root.relative_if_inside(candidate) {
        Some(relative) => Ok(Access::Confined { root, relative }),
        None => Err(ToolError::Denied {
            reason: format!(
                "{} is outside this agent's {FULL_ROOT_CONFIG_KEY} ({})",
                candidate.display(),
                root.as_path().display()
            ),
        }),
    }
}

/// Opens `root` as an ambient capability -- the ONE place this module calls
/// `Dir::open_ambient_dir`, i.e. the one place it trusts a plain OS path
/// rather than an existing capability, because there is no pre-existing
/// capability to derive this root FROM (it comes from per-agent config, a
/// plain string). Every subsequent operation goes through the returned
/// `Dir`, never back through a bare path.
fn open_root(root: &CanonicalRoot) -> Result<Dir, ToolError> {
    Dir::open_ambient_dir(root.as_path(), ambient_authority()).map_err(|err| ToolError::Io {
        detail: format!(
            "failed to open this agent's confinement root {}: {err}",
            root.as_path().display()
        ),
    })
}

/// Classifies an `io::Error` from a `Dir` method call against `candidate`.
/// `cap_std`'s own escape refusal is synthetic (no underlying OS errno --
/// `raw_os_error() == None`) and reports `ErrorKind::PermissionDenied`;
/// distinguishing it from a REAL OS permission-denied (which always carries
/// an errno) is what lets this module surface an escape as a `Denied`
/// (model-recoverable-shaped, matching `check_root`'s predecessor contract)
/// rather than folding it into the generic `Io` bucket every other host
/// failure uses.
fn classify(err: io::Error, candidate: &Path, root: &CanonicalRoot) -> ClassifiedErr {
    if err.kind() == ErrorKind::NotFound {
        return ClassifiedErr::NotFound;
    }
    if err.kind() == ErrorKind::PermissionDenied && err.raw_os_error().is_none() {
        return ClassifiedErr::Denied(ToolError::Denied {
            reason: format!(
                "{} escapes this agent's {FULL_ROOT_CONFIG_KEY} ({}); refused at open time",
                candidate.display(),
                root.as_path().display()
            ),
        });
    }
    ClassifiedErr::Io(ToolError::Io {
        detail: format!("failed to access {}: {err}", candidate.display()),
    })
}

#[derive(Debug)]
enum ClassifiedErr {
    NotFound,
    Denied(ToolError),
    Io(ToolError),
}

/// The outcome of [`read_file`]: model-recoverable "not found" is kept
/// distinct from bytes, mirroring every caller's pre-existing
/// `tokio::fs::read` match (a missing file is the model's mistake, not a
/// host failure).
#[derive(Debug)]
pub(crate) enum ReadOutcome {
    Bytes(Vec<u8>),
    NotFound,
}

/// Reads `candidate`'s full contents, enforcing this agent's `conway.fs`
/// root as ONE step with the open when a root is configured (see module
/// doc). Unconfined is byte-for-byte the pre-existing `tokio::fs::read`.
pub(crate) async fn read_file(ctx: &ToolCtx, candidate: &Path) -> Result<ReadOutcome, ToolError> {
    match resolve(ctx, candidate)? {
        Access::Unconfined => match tokio::fs::read(candidate).await {
            Ok(bytes) => Ok(ReadOutcome::Bytes(bytes)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(ReadOutcome::NotFound),
            Err(err) => Err(ToolError::Io {
                detail: format!("failed to read {}: {err}", candidate.display()),
            }),
        },
        Access::Confined { root, relative } => {
            let candidate = candidate.to_path_buf();
            tokio::task::spawn_blocking(move || {
                use std::io::Read;
                let dir = open_root(&root)?;
                match dir.open(&relative) {
                    Ok(mut file) => {
                        let mut bytes = Vec::new();
                        file.read_to_end(&mut bytes).map_err(|err| ToolError::Io {
                            detail: format!("failed to read {}: {err}", candidate.display()),
                        })?;
                        Ok(ReadOutcome::Bytes(bytes))
                    }
                    Err(err) => match classify(err, &candidate, &root) {
                        ClassifiedErr::NotFound => Ok(ReadOutcome::NotFound),
                        ClassifiedErr::Denied(err) | ClassifiedErr::Io(err) => Err(err),
                    },
                }
            })
            .await
            .map_err(|err| ToolError::Io {
                detail: format!("read task panicked: {err}"),
            })?
        }
    }
}

/// Atomically replaces `candidate`'s contents with `content` (parent
/// directories created as needed), enforcing this agent's `conway.fs` root
/// as ONE step with the write when a root is configured. Mirrors
/// `crate::fs::write::atomic_write`'s shape (sibling temp file, `flush` +
/// `sync_all`, rename over the target, best-effort temp cleanup on failure)
/// but every step -- `create_dir_all`, `create` the temp file, `rename` --
/// goes through the SAME [`cap_std::fs::Dir`], so a symlink swapped into any
/// intermediate directory between steps is refused at the point it would be
/// used, not merely at an earlier check. Returns the number of bytes
/// written.
pub(crate) async fn write_file_atomic(
    ctx: &ToolCtx,
    candidate: &Path,
    content: &str,
) -> Result<u64, ToolError> {
    match resolve(ctx, candidate)? {
        Access::Unconfined => super::write::atomic_write(candidate, content).await,
        Access::Confined { root, relative } => {
            let candidate = candidate.to_path_buf();
            let content = content.to_string();
            tokio::task::spawn_blocking(move || {
                write_file_atomic_confined(&root, &relative, &candidate, &content)
            })
            .await
            .map_err(|err| ToolError::Io {
                detail: format!("write task panicked: {err}"),
            })?
        }
    }
}

/// The synchronous, `Dir`-relative body of [`write_file_atomic`]'s confined
/// branch -- split out so it can run inside `spawn_blocking` without a
/// nested closure duplicating the temp-name/cleanup logic.
fn write_file_atomic_confined(
    root: &CanonicalRoot,
    relative: &Path,
    candidate: &Path,
    content: &str,
) -> Result<u64, ToolError> {
    use std::io::Write;

    let dir = open_root(root)?;

    if let Some(parent) = relative.parent().filter(|p| !p.as_os_str().is_empty()) {
        dir.create_dir_all(parent).map_err(|err| ToolError::Io {
            detail: format!(
                "failed to create parent directories for {}: {err}",
                candidate.display()
            ),
        })?;
    }

    let tmp_relative = super::write::tmp_sibling(relative);

    let write_result: io::Result<u64> = (|| {
        let mut file = dir.create(&tmp_relative)?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        Ok(content.len() as u64)
    })();

    let bytes = match write_result {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = dir.remove_file(&tmp_relative);
            return Err(ToolError::Io {
                detail: format!("failed to write {}: {err}", candidate.display()),
            });
        }
    };

    if let Err(err) = dir.rename(&tmp_relative, &dir, relative) {
        let _ = dir.remove_file(&tmp_relative);
        return match classify(err, candidate, root) {
            ClassifiedErr::NotFound | ClassifiedErr::Denied(_) => Err(ToolError::Denied {
                reason: format!(
                    "{} escapes this agent's {FULL_ROOT_CONFIG_KEY} ({}); refused at open time",
                    candidate.display(),
                    root.as_path().display()
                ),
            }),
            ClassifiedErr::Io(err) => Err(err),
        };
    }

    Ok(bytes)
}

/// The outcome of [`confined_metadata`]: `cd` needs to distinguish all
/// three (model-recoverable "not found", model-recoverable "not a
/// directory", and success) exactly as it did against `tokio::fs::metadata`
/// before this item.
#[derive(Debug)]
pub(crate) enum StatOutcome {
    Dir,
    NotADir,
    NotFound,
}

/// Stats `candidate`, enforcing this agent's `conway.fs` root as ONE step
/// with the stat when a root is configured. `cd` is the one caller: it
/// never opens `candidate` for I/O (there is nothing to read or write), so
/// this checks-and-uses a `Dir::metadata` call rather than an `open` --
/// still symlink-aware and still refusing an escape at call time, the same
/// property every other function in this module has, even though "use" here
/// means "stat", not "read bytes".
pub(crate) async fn confined_metadata(
    ctx: &ToolCtx,
    candidate: &Path,
) -> Result<StatOutcome, ToolError> {
    match resolve(ctx, candidate)? {
        Access::Unconfined => match tokio::fs::metadata(candidate).await {
            Ok(meta) if meta.is_dir() => Ok(StatOutcome::Dir),
            Ok(_) => Ok(StatOutcome::NotADir),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(StatOutcome::NotFound),
            Err(err) => Err(ToolError::Io {
                detail: format!("failed to stat {}: {err}", candidate.display()),
            }),
        },
        Access::Confined { root, relative } => {
            let candidate = candidate.to_path_buf();
            tokio::task::spawn_blocking(move || {
                let dir = open_root(&root)?;
                match dir.metadata(&relative) {
                    Ok(meta) if meta.is_dir() => Ok(StatOutcome::Dir),
                    Ok(_) => Ok(StatOutcome::NotADir),
                    Err(err) => match classify(err, &candidate, &root) {
                        ClassifiedErr::NotFound => Ok(StatOutcome::NotFound),
                        ClassifiedErr::Denied(err) | ClassifiedErr::Io(err) => Err(err),
                    },
                }
            })
            .await
            .map_err(|err| ToolError::Io {
                detail: format!("stat task panicked: {err}"),
            })?
        }
    }
}

/// Validates a `glob`/`grep` search root against this agent's `conway.fs`
/// root, closing the TOCTOU window for the root argument ITSELF (not the
/// files under it -- see module doc for why the walk itself is out of
/// scope): when confined, this probes `Dir::open_dir` on the resolved
/// offset -- an ACTUAL open-relative syscall, refusing an escape exactly
/// like every other function here -- before returning `candidate` unchanged
/// for `crate::fs::walk_files` to walk with its own (unrelated,
/// pre-existing) `ignore::WalkBuilder`. Unconfined returns `candidate`
/// immediately, performing no extra I/O -- byte-for-byte the pre-existing
/// behavior.
pub(crate) async fn confine_search_root(
    ctx: &ToolCtx,
    candidate: &Path,
) -> Result<PathBuf, ToolError> {
    match resolve(ctx, candidate)? {
        Access::Unconfined => Ok(candidate.to_path_buf()),
        Access::Confined { root, relative } => {
            let candidate = candidate.to_path_buf();
            let candidate_for_probe = candidate.clone();
            tokio::task::spawn_blocking(move || {
                let dir = open_root(&root)?;
                match dir.open_dir(&relative) {
                    Ok(_opened) => Ok(candidate_for_probe),
                    Err(err) => Err(match classify(err, &candidate, &root) {
                        ClassifiedErr::NotFound => ToolError::Denied {
                            reason: format!(
                                "{} does not exist under this agent's {FULL_ROOT_CONFIG_KEY}",
                                candidate.display()
                            ),
                        },
                        ClassifiedErr::Denied(err) | ClassifiedErr::Io(err) => err,
                    }),
                }
            })
            .await
            .map_err(|err| ToolError::Io {
                detail: format!("search-root probe task panicked: {err}"),
            })?
        }
    }
}

/// Test-only decomposition of this module's check-then-use shape into its
/// two halves, gated behind `test-fakes` exactly like [`crate::testing`] --
/// exists ONLY so an external integration test
/// (`conway-tools/tests/fs_confinement.rs`, this item's own verification
/// anchor) can construct a genuinely deterministic TOCTOU proof: resolve
/// containment once, mutate the filesystem, then perform ONLY the open
/// step using nothing recomputed from the mutated state -- see this
/// module's own `open_confined_denies_a_symlink_swapped_in_after_resolve_
/// but_before_open` unit test for why calling `read_file` twice (or once,
/// against an already-mutated filesystem) does NOT discriminate a
/// TOCTOU-closed implementation from a pre-check-then-open one, and
/// therefore does not prove what this item claims. Not part of this
/// crate's real dispatch path: `read_file`/`write_file_atomic`/
/// `confined_metadata` never call this; they inline the equivalent logic
/// directly.
#[cfg(feature = "test-fakes")]
pub mod toctou_probe {
    use std::path::{Path, PathBuf};

    /// The "check" half: resolves `ctx`+`candidate` exactly as
    /// `super::read_file` does internally, returning this agent's
    /// confinement root and `candidate` expressed relative to it -- `None`
    /// for an unconfined agent or a candidate that is not (right now)
    /// inside the root (neither is the interesting case for this probe).
    pub fn resolve_confined(
        ctx: &conway_core::ports::ToolCtx,
        candidate: &Path,
    ) -> Option<(PathBuf, PathBuf)> {
        match super::resolve(ctx, candidate).ok()? {
            super::Access::Unconfined => None,
            super::Access::Confined { root, relative } => {
                Some((root.as_path().to_path_buf(), relative))
            }
        }
    }

    /// The "use" half: opens `relative` through a FRESH [`cap_std::fs::
    /// Dir`] capability opened at `root_path` and reads it to the end --
    /// exactly what `super::read_file`'s confined branch does, given
    /// nothing but the pair [`resolve_confined`] already returned. A
    /// filesystem mutation performed between the two calls is precisely
    /// what this whole probe exists to let a caller inject. Returns a
    /// plain `String` error (this is a test probe, not production error
    /// plumbing) so a caller can assert on it without importing this
    /// crate's internal `ToolError`-classification machinery.
    pub fn open_confined(root_path: &Path, relative: &Path) -> Result<Vec<u8>, String> {
        use std::io::Read;
        let root = conway_core::containment::CanonicalRoot::new(root_path)
            .map_err(|err| format!("root no longer canonicalizes: {err}"))?;
        let dir = cap_std::fs::Dir::open_ambient_dir(root.as_path(), cap_std::ambient_authority())
            .map_err(|err| format!("failed to open root: {err}"))?;
        let mut file = dir
            .open(relative)
            .map_err(|err| format!("open refused (this is the property under test): {err}"))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|err| format!("read failed: {err}"))?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_ctx;
    use std::sync::Arc;

    fn confined_ctx(root: &Path, cwd: &Path) -> (ToolCtx, crate::testing::TestHandles) {
        let (mut ctx, handles) = test_ctx(cwd.to_path_buf());
        let mut values = serde_json::Map::new();
        values.insert(
            FULL_ROOT_CONFIG_KEY.to_string(),
            serde_json::json!(root.display().to_string()),
        );
        ctx.config = Arc::new(conway_core::ports::PluginConfig { values });
        (ctx, handles)
    }

    // ---- resolve ----

    #[test]
    fn resolve_is_unconfined_when_no_root_configured() {
        let (ctx, _h) = test_ctx(PathBuf::from("/tmp"));
        assert!(matches!(
            resolve(&ctx, Path::new("/anything")).unwrap(),
            Access::Unconfined
        ));
    }

    #[test]
    fn resolve_is_confined_with_relative_offset_when_inside() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        match resolve(&ctx, &sub.join("f.txt")).unwrap() {
            Access::Confined { relative, .. } => assert_eq!(relative, PathBuf::from("sub/f.txt")),
            Access::Unconfined => panic!("expected Confined"),
        }
    }

    #[test]
    fn resolve_denies_a_candidate_outside_the_configured_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root_dir = tmp.path().join("root");
        let outside_dir = tmp.path().join("outside");
        std::fs::create_dir(&root_dir).unwrap();
        std::fs::create_dir(&outside_dir).unwrap();
        let (ctx, _h) = confined_ctx(&root_dir, &root_dir);
        let err = resolve(&ctx, &outside_dir.join("secret.txt")).unwrap_err();
        assert!(matches!(err, ToolError::Denied { .. }));
    }

    // ---- read_file ----

    #[tokio::test]
    async fn read_file_confined_reads_inside_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), b"hello").unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        match read_file(&ctx, &tmp.path().join("f.txt")).await.unwrap() {
            ReadOutcome::Bytes(bytes) => assert_eq!(bytes, b"hello"),
            ReadOutcome::NotFound => panic!("expected Bytes"),
        }
    }

    #[tokio::test]
    async fn read_file_confined_not_found_is_recoverable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        assert!(matches!(
            read_file(&ctx, &tmp.path().join("missing.txt"))
                .await
                .unwrap(),
            ReadOutcome::NotFound
        ));
    }

    #[tokio::test]
    async fn read_file_confined_denies_a_preexisting_escaping_symlink() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let tmp = tempfile::TempDir::new().unwrap();
            let root_dir = tmp.path().join("root");
            let outside_dir = tmp.path().join("outside");
            std::fs::create_dir(&root_dir).unwrap();
            std::fs::create_dir(&outside_dir).unwrap();
            std::fs::write(outside_dir.join("secret.txt"), b"TOP SECRET").unwrap();
            symlink(Path::new("../outside"), root_dir.join("link")).unwrap();

            let (ctx, _h) = confined_ctx(&root_dir, &root_dir);
            let err = read_file(&ctx, &root_dir.join("link").join("secret.txt"))
                .await
                .unwrap_err();
            assert!(matches!(err, ToolError::Denied { .. }));
        }
    }

    /// THE load-bearing test, and this item's own verification anchor
    /// (mirrored end to end in `tests/fs_confinement.rs`).
    ///
    /// This does NOT merely call `read_file` once against an
    /// already-swapped filesystem -- that would prove nothing about
    /// TOCTOU-closure, since `resolve`'s own containment walk (which
    /// follows symlinks) would ALSO deny an already-swapped escape on a
    /// fresh call, whether or not the eventual open re-verifies anything.
    /// Instead this test reproduces the actual shape of the race
    /// deterministically:
    ///
    /// 1. Call `resolve` while `staging` is still a REAL directory that
    ///    does not yet contain `secret.txt` -- `Access::Confined` (the
    ///    retired `check_root`'s "OK, proceed" -- proven by asserting the
    ///    variant, not by inference).
    /// 2. Mutate the filesystem: swap `staging` for a symlink to
    ///    `outside_dir` -- simulating whatever real-world gap a
    ///    pre-check-then-open shape leaves open (an `await` on the
    ///    operator's gate, a cooperative scheduling point, a genuinely
    ///    concurrent sibling operation -- the exact mechanism does not
    ///    matter; what matters is that SOMETHING can run between step 1 and
    ///    step 3).
    /// 3. Perform ONLY the "use" half -- opening `relative` through a
    ///    `Dir` at `root`, exactly what `read_file`'s confined branch does
    ///    -- using NOTHING but the `(root, relative)` step 1 already
    ///    computed, never re-deriving anything from the now-mutated
    ///    filesystem. A pre-check-then-open implementation's "use" half
    ///    would be `tokio::fs::read` on the ORIGINAL absolute candidate
    ///    path (also fixed at step 1) and WOULD follow the swapped symlink.
    ///    `Dir::open` must not.
    #[tokio::test]
    async fn open_confined_denies_a_symlink_swapped_in_after_resolve_but_before_open() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let tmp = tempfile::TempDir::new().unwrap();
            let root_dir = tmp.path().join("root");
            let outside_dir = tmp.path().join("outside");
            std::fs::create_dir(&root_dir).unwrap();
            std::fs::create_dir(&outside_dir).unwrap();
            std::fs::write(outside_dir.join("secret.txt"), b"TOP SECRET").unwrap();
            std::fs::create_dir(root_dir.join("staging")).unwrap();

            let (ctx, _h) = confined_ctx(&root_dir, &root_dir);
            let candidate = root_dir.join("staging").join("secret.txt");

            // Step 1: the check, while the filesystem is still exactly what
            // it appears to be.
            let (root, relative) = match resolve(&ctx, &candidate).unwrap() {
                Access::Confined { root, relative } => (root, relative),
                Access::Unconfined => panic!("expected Confined"),
            };
            assert_eq!(relative, PathBuf::from("staging/secret.txt"));

            // Step 2: the race. `resolve` already returned; nothing above
            // this line runs again.
            std::fs::remove_dir(root_dir.join("staging")).unwrap();
            symlink(&outside_dir, root_dir.join("staging")).unwrap();

            // Step 3: the use, given only step 1's `(root, relative)` --
            // the same information (and nothing more) `read_file`'s
            // confined branch would have to work with at this point.
            let dir = open_root(&root).unwrap();
            let err = dir.open(&relative).unwrap_err();
            match classify(err, &candidate, &root) {
                ClassifiedErr::Denied(_) => {}
                other => panic!(
                    "expected the swapped-in symlink to be classified as an escape denial, \
                     got a different classification instead (err={other:?})"
                ),
            }
        }
    }

    /// The end-to-end companion: `read_file`, called once, against an
    /// escape that is already present (not raced in) -- the ordinary case
    /// every caller actually hits, kept alongside the load-bearing test
    /// above for contrast (a pre-check-then-open implementation would ALSO
    /// deny this one; it is not what discriminates the two shapes).
    #[tokio::test]
    async fn read_file_confined_denies_a_preexisting_symlink_escape_end_to_end() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let tmp = tempfile::TempDir::new().unwrap();
            let root_dir = tmp.path().join("root");
            let outside_dir = tmp.path().join("outside");
            std::fs::create_dir(&root_dir).unwrap();
            std::fs::create_dir(&outside_dir).unwrap();
            std::fs::write(outside_dir.join("secret.txt"), b"TOP SECRET").unwrap();
            symlink(&outside_dir, root_dir.join("staging")).unwrap();

            let (ctx, _h) = confined_ctx(&root_dir, &root_dir);
            let candidate = root_dir.join("staging").join("secret.txt");
            let err = read_file(&ctx, &candidate).await.unwrap_err();
            assert!(matches!(err, ToolError::Denied { .. }));
        }
    }

    #[tokio::test]
    async fn read_file_unconfined_behaves_as_before() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), b"hi").unwrap();
        let (ctx, _h) = test_ctx(tmp.path().to_path_buf());
        match read_file(&ctx, &tmp.path().join("f.txt")).await.unwrap() {
            ReadOutcome::Bytes(bytes) => assert_eq!(bytes, b"hi"),
            ReadOutcome::NotFound => panic!("expected Bytes"),
        }
    }

    // ---- write_file_atomic ----

    #[tokio::test]
    async fn write_file_atomic_confined_creates_parents_and_writes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        let target = tmp.path().join("a/b/c.txt");
        write_file_atomic(&ctx, &target, "hello").await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
    }

    #[tokio::test]
    async fn write_file_atomic_confined_denies_outside_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root_dir = tmp.path().join("root");
        let outside_dir = tmp.path().join("outside");
        std::fs::create_dir(&root_dir).unwrap();
        std::fs::create_dir(&outside_dir).unwrap();
        let (ctx, _h) = confined_ctx(&root_dir, &root_dir);
        let err = write_file_atomic(&ctx, &outside_dir.join("f.txt"), "hi")
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied { .. }));
        assert!(!outside_dir.join("f.txt").exists());
    }

    #[tokio::test]
    async fn write_file_atomic_confined_leaves_no_tmp_sibling_after_success() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        write_file_atomic(&ctx, &tmp.path().join("f.txt"), "hi")
            .await
            .unwrap();
        let leftover: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".conway.tmp"))
            .collect();
        assert!(leftover.is_empty(), "leftover tmp files: {leftover:?}");
    }

    // ---- confined_metadata ----

    #[tokio::test]
    async fn confined_metadata_dir_inside_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        assert!(matches!(
            confined_metadata(&ctx, &sub).await.unwrap(),
            StatOutcome::Dir
        ));
    }

    #[tokio::test]
    async fn confined_metadata_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        assert!(matches!(
            confined_metadata(&ctx, &tmp.path().join("missing"))
                .await
                .unwrap(),
            StatOutcome::NotFound
        ));
    }

    #[tokio::test]
    async fn confined_metadata_not_a_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), b"hi").unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        assert!(matches!(
            confined_metadata(&ctx, &tmp.path().join("f.txt"))
                .await
                .unwrap(),
            StatOutcome::NotADir
        ));
    }

    #[tokio::test]
    async fn confined_metadata_denies_outside_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root_dir = tmp.path().join("root");
        let outside_dir = tmp.path().join("outside");
        std::fs::create_dir(&root_dir).unwrap();
        std::fs::create_dir(&outside_dir).unwrap();
        let (ctx, _h) = confined_ctx(&root_dir, &root_dir);
        let err = confined_metadata(&ctx, &outside_dir).await.unwrap_err();
        assert!(matches!(err, ToolError::Denied { .. }));
    }

    // ---- confine_search_root ----

    #[tokio::test]
    async fn confine_search_root_unconfined_passes_through() {
        let (ctx, _h) = test_ctx(PathBuf::from("/tmp"));
        let out = confine_search_root(&ctx, Path::new("/tmp")).await.unwrap();
        assert_eq!(out, PathBuf::from("/tmp"));
    }

    #[tokio::test]
    async fn confine_search_root_confined_allows_inside_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        let out = confine_search_root(&ctx, &sub).await.unwrap();
        assert_eq!(out, sub);
    }

    #[tokio::test]
    async fn confine_search_root_confined_denies_outside_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root_dir = tmp.path().join("root");
        let outside_dir = tmp.path().join("outside");
        std::fs::create_dir(&root_dir).unwrap();
        std::fs::create_dir(&outside_dir).unwrap();
        let (ctx, _h) = confined_ctx(&root_dir, &root_dir);
        let err = confine_search_root(&ctx, &outside_dir).await.unwrap_err();
        assert!(matches!(err, ToolError::Denied { .. }));
    }

    #[tokio::test]
    async fn confine_search_root_confined_denies_a_symlink_escape() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let tmp = tempfile::TempDir::new().unwrap();
            let root_dir = tmp.path().join("root");
            let outside_dir = tmp.path().join("outside");
            std::fs::create_dir(&root_dir).unwrap();
            std::fs::create_dir(&outside_dir).unwrap();
            symlink(Path::new("../outside"), root_dir.join("link")).unwrap();

            let (ctx, _h) = confined_ctx(&root_dir, &root_dir);
            let err = confine_search_root(&ctx, &root_dir.join("link"))
                .await
                .unwrap_err();
            assert!(matches!(err, ToolError::Denied { .. }));
        }
    }
}
