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
/// `sync_all`, rename over the target, `sync_all` the parent directory
/// after the rename, best-effort temp cleanup on failure) but every step --
/// `create_dir_all`, `create` the temp file, `rename`, and now the
/// post-rename parent-directory sync -- goes through the SAME
/// [`cap_std::fs::Dir`], so a symlink swapped into any intermediate
/// directory between steps is refused at the point it would be used, not
/// merely at an earlier check. Returns the number of bytes written.
///
/// Board item `01M2M5KVC9J7MWY56DQK5YPXNR` consolidated the unconfined
/// atomic-write protocol (`crate::fs::write::atomic_write`) and added that
/// parent-directory `sync_all` after rename, but deliberately did not fold
/// this confined path in (see this module's own doc for why the two stay
/// separate). That left this path with the WEAKER of the two durability
/// guarantees -- a rename whose own directory-entry update could still be
/// lost to a crash even though the renamed file's bytes were themselves
/// flushed -- for no reason connected to confinement itself. Board item
/// `01M2M8TG3YCVHJKM6PJ4S5PYKE` closed that gap: `write_file_atomic_confined`
/// (this function's own confined branch, below) now performs the identical
/// post-rename sync, expressed through `cap_std` (see
/// `sync_confined_parent_dir`, further below) rather than through
/// `write.rs`'s private `DurableSync` seam, which is not `pub` and could
/// not be reused across the crate-module boundary even if the two
/// implementations were otherwise mergeable (they are not -- see module
/// doc). The two paths' durability guarantees are now equal. The two
/// paths' mode-preservation guarantees are equal too: like
/// `write.rs::atomic_write`, an already-existing destination has its unix
/// mode copied onto the temp file before the rename (`copy_existing_mode_
/// confined`, below) so an edit keeps a script's exec bit or a keyfile's
/// 0600, with failures degrading to a tracing warning rather than failing
/// the write, and a not-yet-existing destination keeping the temp file's
/// umask-derived mode.
pub(crate) async fn write_file_atomic(
    ctx: &ToolCtx,
    candidate: &Path,
    content: &str,
) -> Result<u64, ToolError> {
    match resolve(ctx, candidate)? {
        Access::Unconfined => super::write::atomic_write_str(candidate, content).await,
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
/// nested closure duplicating the temp-name/cleanup logic. Delegates to
/// [`write_file_atomic_confined_with`], production's real
/// [`sync_confined_parent_dir`] plugged in as the post-rename sync step --
/// see that function's own doc for why the sync step is a parameter at all
/// (a test seam, not a production knob).
fn write_file_atomic_confined(
    root: &CanonicalRoot,
    relative: &Path,
    candidate: &Path,
    content: &str,
) -> Result<u64, ToolError> {
    write_file_atomic_confined_with(root, relative, candidate, content, sync_confined_parent_dir)
}

/// [`write_file_atomic_confined`]'s real body, generic over the post-rename
/// parent-directory sync step so `tests` can substitute a wrapper that
/// records when it runs (and whether the rename has already landed by
/// then) -- the confined-path mirror of `write.rs`'s own
/// `atomic_write_with`/`DurableSync` seam. It cannot literally reuse that
/// seam: `write.rs`'s `DurableSync` trait is `trait` (module-private, not
/// `pub`), and even if it were `pub`, its production impl is for
/// `std::fs::File` opened via a bare `fs::File::open(dir_path)` -- exactly
/// the ambient, non-`Dir`-relative open this module's whole reason for
/// existing (see module doc) refuses to do for anything reachable from a
/// `candidate` path. `sync_parent` here is instead threaded through
/// `cap_std::fs::Dir`, unlike `write.rs`'s directory open.
fn write_file_atomic_confined_with(
    root: &CanonicalRoot,
    relative: &Path,
    candidate: &Path,
    content: &str,
    sync_parent: impl FnOnce(&Dir, &Path) -> io::Result<()>,
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
        copy_existing_mode_confined(&dir, relative, &tmp_relative, candidate);
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

    // The durability half `write.rs::atomic_write`'s own doc names: the
    // rename alone only guarantees the directory-entry update is VISIBLE
    // to other processes on this machine, not that it has itself reached
    // the underlying device. Nothing here needs cleaning up on failure --
    // the rename already committed, so `candidate` already has the right
    // content; only the crash-survival guarantee for that fact is what
    // this step establishes. See `tests::parent_directory_synced_after_rename`
    // for mechanical proof of the ordering (call order only -- forcing an
    // actual crash from user space is not portably falsifiable at all).
    if let Err(err) = sync_parent(&dir, relative) {
        return Err(ToolError::Io {
            detail: format!(
                "wrote {} but failed to sync its parent directory: {err}",
                candidate.display()
            ),
        });
    }

    Ok(bytes)
}

/// Copies an existing destination's unix mode onto the confined branch's
/// temp file before `dir.rename` replaces the destination's inode -- the
/// confined mirror of `write.rs::copy_existing_mode`, kept inside the
/// capability: the stat and the mode set both go through the SAME
/// [`cap_std::fs::Dir`] (`Dir::metadata`, `Dir::set_permissions`), never a
/// fresh ambient open, exactly like every other filesystem step in
/// [`write_file_atomic_confined_with`].
///
/// MASKED to `write::ORDINARY_PERMISSION_BITS` first, exactly like the
/// unconfined copy -- see that constant's own doc for why (a setuid/setgid
/// destination must not have that bit reapplied on top of model-authored
/// content) and for why the mask itself lives in ONE place shared by both
/// branches rather than being re-derived here.
///
/// Same degradation posture as the unconfined copy (and the sync-parent
/// step below): a failure to `set_permissions` warns and proceeds, never
/// fails the write -- the content is already correct and still gets
/// durably flushed by the following `sync_all` (which therefore also
/// covers the mode change, as in the unconfined path). A missing
/// destination -- the ordinary new-file case -- takes the unchanged
/// umask-default path silently; any OTHER stat failure (mode
/// indeterminable) warns and proceeds, same as a failed `set_permissions`.
/// A THIRD case also warns, deliberately, even though the copy itself
/// succeeded: when the existing destination's mode carried a bit outside
/// `write::ORDINARY_PERMISSION_BITS`, that bit is silently dropped rather
/// than reapplied -- see `write::copy_existing_mode`'s own doc for the full
/// reasoning (identical here: dropping a setuid/setgid/sticky bit is a
/// real, meaningful change to a file the operator owns, worth surfacing
/// exactly like a failed copy is).
fn copy_existing_mode_confined(dir: &Dir, relative: &Path, tmp_relative: &Path, candidate: &Path) {
    match dir.metadata(relative) {
        Ok(meta) => {
            #[cfg(unix)]
            {
                use cap_std::fs::PermissionsExt;
                let mode = meta.permissions().mode();
                let masked = cap_std::fs::Permissions::from_mode(
                    mode & super::write::ORDINARY_PERMISSION_BITS,
                );
                if let Err(err) = dir.set_permissions(tmp_relative, masked) {
                    tracing::warn!(
                        "wrote {} but could not preserve its existing file mode: {err}",
                        candidate.display()
                    );
                } else if mode & !super::write::ORDINARY_PERMISSION_BITS != 0 {
                    tracing::warn!(
                        "wrote {}: its previous mode ({mode:#o}) carried a setuid, \
                         setgid, or sticky bit; dropping it rather than reapplying it \
                         to newly-written, model-authored content",
                        candidate.display()
                    );
                }
            }
            // Non-unix targets have no setuid/setgid/sticky concept in
            // `cap_std::fs::Permissions` to begin with, so the whole-object
            // copy this code always did is not the same hazard there --
            // kept unchanged.
            #[cfg(not(unix))]
            {
                if let Err(err) = dir.set_permissions(tmp_relative, meta.permissions()) {
                    tracing::warn!(
                        "wrote {} but could not preserve its existing file mode: {err}",
                        candidate.display()
                    );
                }
            }
        }
        // The ordinary new-file case: nothing to preserve, the temp file
        // keeps its umask-derived mode.
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(
                "wrote {} but could not stat the previous file to preserve its \
                 mode: {err}",
                candidate.display()
            );
        }
    }
}

/// Production `sync_parent` for [`write_file_atomic_confined_with`]:
/// durably flushes `relative`'s PARENT directory, still entirely through
/// `dir` (the already-opened, TOCTOU-safe [`cap_std::fs::Dir`] capability
/// for this agent's confinement root) -- never a fresh ambient open.
///
/// `cap_std::fs::Dir` has no `sync_all`/`sync_data` method of its own (it
/// is not `std::fs::File` and does not implement `std::io::Write`), but it
/// does not need one: `cap_std`'s own `Dir` is, at the representation
/// level, a thin wrapper around exactly one `std::fs::File` (that crate's
/// `fs::dir::Dir { std_file: fs::File }`), and `Dir::into_std_file` hands
/// that same underlying OS handle back out as a plain `std::fs::File`,
/// which has always supported `sync_all` on a directory fd on every
/// platform this workspace targets (POSIX `fsync(2)` accepts a directory
/// fd; `write.rs::atomic_write` already relies on the identical fact via a
/// bare `fs::File::open` on a directory path). So `cap_std` CAN express
/// this durability step, just not as an inherent method: the capability
/// crosses into `std::fs::File` first, then durability is
/// `std::fs::File::sync_all`, same as `write.rs`'s own
/// `DurableWrite`/`DurableSync` impls for `std::fs::File`.
///
/// `relative`'s parent is usually a subdirectory reached via
/// `dir.open_dir` (itself symlink-refusing, same as every other `Dir`
/// call in this module); when `relative` has no parent (the write target
/// sits directly under the confinement root), the "parent" IS `dir`
/// itself, so this clones the capability (`Dir::try_clone`, a cheap `dup`
/// of the underlying fd, not a fresh ambient open) rather than reopening
/// anything.
fn sync_confined_parent_dir(dir: &Dir, relative: &Path) -> io::Result<()> {
    match relative.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => dir.open_dir(parent)?.into_std_file().sync_all(),
        None => dir.try_clone()?.into_std_file().sync_all(),
    }
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

    /// Board item `01M2M8TG3YCVHJKM6PJ4S5PYKE`'s own verification anchor:
    /// the confined path's mirror of `write.rs::tests::
    /// parent_directory_synced_after_rename` and `sync_all_happens_before_
    /// rename`, sharing this name deliberately (same property, same
    /// module-doc-mandated label).
    ///
    /// `cap_std::fs::Dir` gives no injectable `sync_all` seam the way
    /// `std::fs::File` does (there is no trait to implement a recording
    /// wrapper against -- `sync_confined_parent_dir` calls the INHERENT
    /// `std::fs::File::sync_all` after converting), so this instead drives
    /// `write_file_atomic_confined_with` directly and substitutes its
    /// `sync_parent` PARAMETER with a hook that (a) asserts, via a SEPARATE
    /// `Dir` capability opened purely to observe (never to write), that the
    /// destination already exists at the moment it runs -- proving the
    /// rename has already landed -- and (b) performs the real sync itself
    /// (delegating to `sync_confined_parent_dir`), so `write_file_atomic_
    /// confined_with` cannot return `Ok` without this hook having run.
    /// Together those two facts pin "parent-directory sync happens after
    /// rename and before return" as an observed CALL, not an inference.
    ///
    /// This PINS CALL ORDER ONLY. It does not and cannot prove survival
    /// across an actual crash/power-loss -- forcing that from user space is
    /// not portably falsifiable at all, exactly the caveat `write.rs`'s own
    /// two same-named tests carry.
    #[test]
    fn parent_directory_synced_after_rename() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = CanonicalRoot::new(tmp.path()).unwrap();
        let relative = PathBuf::from("sub/f.txt");
        let candidate = tmp.path().join("sub/f.txt");

        let observer = Dir::open_ambient_dir(tmp.path(), ambient_authority()).unwrap();
        let hook_ran = std::sync::atomic::AtomicBool::new(false);

        let bytes = write_file_atomic_confined_with(
            &root,
            &relative,
            &candidate,
            "hello",
            |dir, relative| {
                assert!(
                    observer.exists(relative),
                    "parent-directory sync ran before the destination existed -- \
                     rename has not happened yet"
                );
                hook_ran.store(true, std::sync::atomic::Ordering::SeqCst);
                sync_confined_parent_dir(dir, relative)
            },
        )
        .unwrap();

        assert_eq!(bytes, 5);
        assert!(
            hook_ran.load(std::sync::atomic::Ordering::SeqCst),
            "sync_parent hook never ran -- write_file_atomic_confined_with \
             returned Ok without syncing the parent directory"
        );
        assert_eq!(std::fs::read_to_string(&candidate).unwrap(), "hello");
    }

    /// The root-level companion to the test above: when the write target
    /// sits directly under the confinement root (no intermediate parent to
    /// `open_dir`), `sync_confined_parent_dir` clones `dir` itself
    /// (`Dir::try_clone`) rather than opening a subdirectory -- exercised
    /// here so that branch is not left uncovered by the nested-path case
    /// above.
    #[test]
    fn parent_directory_synced_after_rename_at_confinement_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = CanonicalRoot::new(tmp.path()).unwrap();
        let relative = PathBuf::from("f.txt");
        let candidate = tmp.path().join("f.txt");

        let observer = Dir::open_ambient_dir(tmp.path(), ambient_authority()).unwrap();
        let hook_ran = std::sync::atomic::AtomicBool::new(false);

        write_file_atomic_confined_with(&root, &relative, &candidate, "hi", |dir, relative| {
            assert!(
                observer.exists(relative),
                "parent-directory sync ran before the destination existed -- \
                 rename has not happened yet"
            );
            hook_ran.store(true, std::sync::atomic::Ordering::SeqCst);
            sync_confined_parent_dir(dir, relative)
        })
        .unwrap();

        assert!(
            hook_ran.load(std::sync::atomic::Ordering::SeqCst),
            "sync_parent hook never ran"
        );
        assert_eq!(std::fs::read_to_string(&candidate).unwrap(), "hi");
    }

    // ---- mode preservation across the rename ----

    #[tokio::test]
    #[cfg(unix)]
    async fn write_file_atomic_confined_preserves_an_existing_files_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        let target = tmp.path().join("script.sh");
        std::fs::write(&target, b"#!/bin/sh\necho old\n").unwrap();
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&target, perms).unwrap();

        write_file_atomic(&ctx, &target, "#!/bin/sh\necho new\n")
            .await
            .unwrap();

        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755,
            "editing a file through the confined atomic write path must keep \
             its mode (the exec bit survives an edit)"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "#!/bin/sh\necho new\n"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_file_atomic_confined_preserves_a_restrictive_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        let target = tmp.path().join("secret.key");
        std::fs::write(&target, b"old secret").unwrap();
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&target, perms).unwrap();

        write_file_atomic(&ctx, &target, "new secret")
            .await
            .unwrap();

        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600,
            "a restrictive mode must survive a confined atomic overwrite"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new secret");
    }

    /// The confined-path mirror of `write::tests::
    /// atomic_write_drops_an_existing_setuid_bit` -- same defect
    /// (`copy_existing_mode_confined` used to copy the whole mode word,
    /// including setuid/setgid/sticky, onto model-authored content), same
    /// fix, and the SAME caveat that test's own doc records: an
    /// unprivileged `write(2)` to a file whose mode already carries setuid
    /// clears that bit as a kernel-level protection, independent of this
    /// crate, which launders this particular (full round-trip) assertion
    /// in a non-root test run regardless of whether the fix below is
    /// present -- confirmed by reverting `write::ORDINARY_PERMISSION_BITS`
    /// to `0o7777` and observing this test still pass.
    /// `copy_existing_mode_confined_drops_an_existing_setuid_bit`, further
    /// below, isolates the masking step from that OS behavior and is the
    /// one that actually fails under that break. This test is kept anyway
    /// as a true end-to-end statement that would matter if this process
    /// ever ran as root (where the kernel does not clear the bit).
    #[tokio::test]
    #[cfg(unix)]
    async fn write_file_atomic_confined_drops_an_existing_setuid_bit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        let target = tmp.path().join("helper");
        std::fs::write(&target, b"old").unwrap();
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_mode(0o4755);
        std::fs::set_permissions(&target, perms).unwrap();

        write_file_atomic(&ctx, &target, "new, model-authored content")
            .await
            .unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o755,
            "the ordinary permission bits must still survive"
        );
        assert_eq!(
            mode & 0o7000,
            0,
            "a setuid bit on the previous file must NOT be reapplied to \
             newly-written content through the confined write path either"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "new, model-authored content"
        );
    }

    /// The setgid mirror of the test above -- same defect, same fix, same
    /// production entry point, same OS-level-clearing caveat;
    /// `copy_existing_mode_confined_drops_an_existing_setgid_bit`, further
    /// below, is this pairing's actually-discriminating test.
    #[tokio::test]
    #[cfg(unix)]
    async fn write_file_atomic_confined_drops_an_existing_setgid_bit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        let target = tmp.path().join("helper");
        std::fs::write(&target, b"old").unwrap();
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_mode(0o2755);
        std::fs::set_permissions(&target, perms).unwrap();

        write_file_atomic(&ctx, &target, "new, model-authored content")
            .await
            .unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(
            mode & 0o7000,
            0,
            "a setgid bit on the previous file must NOT be reapplied to \
             newly-written content through the confined write path either"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "new, model-authored content"
        );
    }

    /// THE load-bearing test for the confined setuid case: calls
    /// `copy_existing_mode_confined` -- the exact function this item's spec
    /// names as the confined-path defect site -- directly, through a REAL
    /// `cap_std::fs::Dir` opened at a real temp directory, with a REAL,
    /// freshly `dir.create`d temp file (exactly as `write_file_atomic_
    /// confined_with` creates its own), so `Dir::metadata`/`Dir::
    /// set_permissions` both run for real; nothing about the mode being
    /// checked is hand-built. Checked on the temp file BEFORE anything is
    /// written to it -- see `write::tests::
    /// copy_existing_mode_drops_an_existing_setuid_bit`'s own doc for why
    /// that ordering is what actually discriminates the fix from the
    /// unrelated kernel protection that launders the full-round-trip test
    /// above: reverting `write::ORDINARY_PERMISSION_BITS` to `0o7777`
    /// fails this test but not that one.
    #[test]
    #[cfg(unix)]
    fn copy_existing_mode_confined_drops_an_existing_setuid_bit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target_relative = Path::new("helper");
        let target_path = tmp.path().join(target_relative);
        std::fs::write(&target_path, b"old").unwrap();
        let mut perms = std::fs::metadata(&target_path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o4755);
        std::fs::set_permissions(&target_path, perms).unwrap();

        let dir = Dir::open_ambient_dir(tmp.path(), ambient_authority()).unwrap();
        let tmp_relative = Path::new("helper.tmp");
        dir.create(tmp_relative).unwrap();

        copy_existing_mode_confined(&dir, target_relative, tmp_relative, &target_path);

        let mode =
            cap_std::fs::PermissionsExt::mode(&dir.metadata(tmp_relative).unwrap().permissions());
        assert_eq!(
            mode & 0o777,
            0o755,
            "the ordinary permission bits must still be copied"
        );
        assert_eq!(
            mode & 0o7000,
            0,
            "a setuid bit on the previous file must NOT be copied onto the \
             temp file that is about to receive model-authored content"
        );
    }

    /// The setgid mirror of the test above -- same reasoning, same
    /// discriminating power (also confirmed to fail under the identical
    /// `ORDINARY_PERMISSION_BITS = 0o7777` break).
    #[test]
    #[cfg(unix)]
    fn copy_existing_mode_confined_drops_an_existing_setgid_bit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target_relative = Path::new("helper");
        let target_path = tmp.path().join(target_relative);
        std::fs::write(&target_path, b"old").unwrap();
        let mut perms = std::fs::metadata(&target_path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o2755);
        std::fs::set_permissions(&target_path, perms).unwrap();

        let dir = Dir::open_ambient_dir(tmp.path(), ambient_authority()).unwrap();
        let tmp_relative = Path::new("helper.tmp");
        dir.create(tmp_relative).unwrap();

        copy_existing_mode_confined(&dir, target_relative, tmp_relative, &target_path);

        let mode =
            cap_std::fs::PermissionsExt::mode(&dir.metadata(tmp_relative).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(
            mode & 0o7000,
            0,
            "a setgid bit on the previous file must NOT be copied onto the \
             temp file that is about to receive model-authored content"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_file_atomic_confined_new_file_keeps_the_umask_default_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let (ctx, _h) = confined_ctx(tmp.path(), tmp.path());
        // A control file created the ordinary `File::create` way pins the
        // umask-derived default this process would give a fresh file --
        // whatever this process's umask happens to be -- without having to
        // read or change the (process-global, racy) umask itself.
        let control = tmp.path().join("control.bin");
        std::fs::write(&control, b"control").unwrap();
        let expected = std::fs::metadata(&control).unwrap().permissions().mode() & 0o777;

        let target = tmp.path().join("fresh.json");
        write_file_atomic(&ctx, &target, "payload").await.unwrap();

        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            expected,
            "a not-previously-existing destination keeps the temp file's \
             umask-derived mode, unchanged"
        );
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
