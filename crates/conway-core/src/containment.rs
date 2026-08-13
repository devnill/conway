//! `CanonicalRoot`: the one correct answer to "is this candidate path inside
//! this root?" (S0 of the cwd-aware-agents charter).
//!
//! Nothing else in the tree answers this question safely.
//! `conway-tools::common::resolve_path` explicitly does not (isolation belongs
//! to tools, not the harness: no sandboxing in that layer). A later slice wires
//! this primitive into `PermissionBroker::decide`; this module is deliberately
//! pure and unwired.
//!
//! # Why a naive check is wrong
//!
//! `candidate.starts_with(root)` is broken two independent ways, both
//! empirically confirmed against a real tree containing
//! `repo/frontend/link -> ../backend`:
//!
//! - **Symlink escape**: `root/frontend/link/secret.txt` lexically starts
//!   with `root/frontend`, but `link` really points outside it.
//! - **`..` traversal**: `Path::components()` does not strip `..`, so a
//!   lexically-rebuilt path can contain an unresolved `../../../etc/passwd`
//!   that still lexically "starts with" the base.
//!
//! `fs::canonicalize` alone is not a fix either: it requires every
//! component of the path to exist, so it cannot be used on a `write`
//! target that does not exist yet. And naive lexical normalization
//! (`Path::join` + manually popping `..` components) is wrong under
//! symlinks: `/repo/link/..` is not the same as `/repo` unless `link`
//! happens to be a plain subdirectory.
//!
//! # Algorithm
//!
//! 1. The root is canonicalized once, at [`CanonicalRoot::new`] — never
//!    per-check, and never compared against a non-canonical candidate
//!    prefix (a symlinked root would otherwise give inconsistent answers).
//! 2. Per candidate, find the deepest existing ancestor by looping
//!    `fs::canonicalize`, popping one path component at a time on
//!    `NotFound`. This splits the candidate into a
//!    `canonical_existing_prefix` (real, symlink-resolved) and a `tail`
//!    (components that don't exist yet).
//! 3. Any [`Component::ParentDir`] in the `tail` is rejected
//!    (`Containment::Undecidable`): a `..` inside the *existing* portion
//!    was already resolved correctly by `fs::canonicalize` against the
//!    real filesystem in step 2, but a `..` in a *non-existent* tail has
//!    no correct lexical answer — its meaning depends on what gets
//!    created later. Fail closed rather than guess.
//!    [`Component::CurDir`] is stripped; it is genuinely inert.
//! 4. `canonical_existing_prefix.join(tail)` is compared against the
//!    canonical root with [`Path::starts_with`] (component-wise) — never
//!    `str`/`OsStr` prefix comparison, which would let `/repo/frontend-evil`
//!    match `/repo/frontend`.
//! 5. Any `io::Error` other than the `NotFound` walk-up denies: this
//!    module never treats "can't check" as "allow".
//!
//! # Relative candidates
//!
//! [`CanonicalRoot::contains`] returns [`Containment::Undecidable`] for a
//! relative candidate; it does not resolve it against the root or against
//! the process's current directory. Resolving a relative path implicitly
//! requires picking *one* of at least two plausible bases (the root? the
//! process cwd? — which is not even the agent's cwd), and a silent choice
//! here is exactly the kind of guess this primitive exists to avoid. Later
//! slices resolve relative tool arguments against the agent's cwd (see
//! `conway-tools::common::resolve_path`) *before* a candidate ever reaches
//! this check, so by the time `contains` is called the candidate should
//! already be absolute; a relative candidate arriving here is treated as a
//! caller bug, not silently guessed at.
//!
//! # No I/O elsewhere in `conway-core`
//!
//! This module is the one exception to "conway-core performs no I/O"
//! (see the crate root doc comment, which carries the forward-declaration
//! label this module is the subject of): a filesystem-symlink-aware
//! containment check fundamentally cannot be pure-computation-only. It
//! remains dependency-free (`std` only, no `ToolCtx`/broker/runtime) and
//! is exhaustively tested against real tempdirs and real symlinks.
//!
//! **The exception is temporary, and it is this module that goes.** Board
//! item 01KZDC30CBY9CPJ8YEM7HSRV0Y ("Retire the harness-level confinement
//! root once conway.fs enforces its own", under Stage 1.5) moves confinement
//! into `conway.fs`, where the check and the open become one step. That item
//! must delete the crate root's label along with this module.

use std::io;
use std::path::{Component, Path, PathBuf};

/// A filesystem root, canonicalized once at construction.
///
/// Construction resolves symlinks in the root itself so every subsequent
/// [`contains`](CanonicalRoot::contains) check compares canonical form
/// against canonical form — comparing a canonical candidate against a
/// non-canonical root would give inconsistent answers whenever the root
/// itself is reached through a symlink.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CanonicalRoot {
    canonical: PathBuf,
}

/// The result of a containment check.
///
/// Deliberately not `bool`-shaped (no `From<bool>`, no `Into<bool>`, no
/// `is_inside`-style shortcut) so a caller cannot accidentally coalesce
/// "couldn't check" into "allowed". Callers must match exhaustively; this
/// type is intentionally not `#[non_exhaustive]` so that exhaustive match
/// is actually enforced at compile time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Containment {
    /// The candidate resolves to a real filesystem location underneath
    /// the root (or equal to it).
    Inside,
    /// The candidate resolves to a real filesystem location that is
    /// definitively not underneath the root (e.g. a symlink escape, a
    /// `..` walk-up that lands outside, or a sibling whose name merely
    /// shares the root's prefix as text).
    Outside,
    /// No safe answer could be computed: a relative candidate, a `..`
    /// inside a non-existent tail (unresolvable — its target depends on
    /// what gets created later), or an `io::Error` other than the
    /// `NotFound` walk-up (e.g. permission denied). Callers must treat
    /// this the same as `Outside` for any allow decision — "can't check"
    /// is never "allow".
    Undecidable,
}

impl CanonicalRoot {
    /// Canonicalizes `root` once and stores the canonical form.
    ///
    /// Errors if `root` does not canonicalize (does not exist, a
    /// component is not traversable, etc). The caller decides what that
    /// means; a later slice fails agent spawn on this error.
    pub fn new(root: &Path) -> io::Result<Self> {
        let canonical = root.canonicalize()?;
        Ok(Self { canonical })
    }

    /// The canonical form of the root, as stored at construction.
    pub fn as_path(&self) -> &Path {
        &self.canonical
    }

    /// Whether `candidate` resolves to a real filesystem location inside
    /// this root. See the module doc comment for the full algorithm and
    /// the relative-candidate decision.
    pub fn contains(&self, candidate: &Path) -> Containment {
        if candidate.is_relative() {
            return Containment::Undecidable;
        }

        let (existing_prefix, tail) = match deepest_existing_ancestor(candidate) {
            Ok(split) => split,
            Err(_) => return Containment::Undecidable,
        };

        let mut clean_tail = PathBuf::new();
        for component in tail.components() {
            match component {
                Component::ParentDir => return Containment::Undecidable,
                Component::CurDir => continue,
                other => clean_tail.push(other.as_os_str()),
            }
        }

        let resolved = existing_prefix.join(&clean_tail);
        if resolved.starts_with(&self.canonical) {
            Containment::Inside
        } else {
            Containment::Outside
        }
    }
}

/// Resolves a possibly-untrusted, model- or config-supplied path string
/// against `cwd`, exactly as the tool call or root check that ultimately
/// acts on it needs it resolved: an absolute `raw` passes through
/// unchanged; a relative `raw` joins onto `cwd`. Returns `None` for a `raw`
/// containing a NUL byte — the OS path APIs cannot represent one
/// (`CString::new` fails on an interior NUL), so any resolution that
/// returned `Some` here would hand the caller a candidate no later
/// filesystem call could act on either.
///
/// **The one implementation every root-enforcement site in this tree
/// shares — board item 01KZVZ56SBPSTZHAXXGYCNETNX.** This exact operation
/// (join-or-pass-through, NUL rejected) was independently restated at least
/// three times in this tree before it was collapsed here: two inlined
/// copies in `conway-runtime` (`subagent.rs`'s spawn-time confinement-root
/// resolution and `runtime.rs`'s root-agent resolution) each independently
/// **dropped the NUL guard** — the defect board item 01KZ00VV3F3EBZ9WQSB292TBJZ
/// fixed by pointing both at `conway_runtime::permission::
/// resolve_like_the_tool_will` — and a third, `conway_tools::common::
/// resolve_path`, carried the guard but as a byte-for-byte separate
/// function, kept in sync only by a doc comment demanding lockstep edits,
/// not by the compiler.
///
/// `conway-runtime`'s `resolve_like_the_tool_will` and `conway-tools`'
/// `resolve_path` each keep their own thin, same-signature, same-crate
/// wrapper around this function (crate layering runs `conway-runtime ->
/// conway-core` and `conway-tools -> conway-core` only, never
/// `conway-runtime -> conway-tools`, so neither crate can call the other's
/// wrapper directly, and neither may gain a new cross-crate dependency just
/// for this) — but the wrapper's BODY is now this one call, never a
/// restatement, so the two can no longer independently drop the guard.
pub fn resolve_candidate(cwd: &Path, raw: &str) -> Option<PathBuf> {
    if raw.contains('\0') {
        return None;
    }
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        Some(candidate.to_path_buf())
    } else {
        Some(cwd.join(candidate))
    }
}

/// Finds the deepest ancestor of `candidate` that exists on the real
/// filesystem, canonicalizing it (resolving any real symlinks and any
/// `..`/`.` along the way), and returns `(canonical_existing_prefix,
/// tail)` where `tail` is whatever components of `candidate` come after
/// that prefix, verbatim (not yet cleaned or checked).
///
/// Loops `fs::canonicalize`, popping one path component off the end on
/// `NotFound` and retrying. Any other `io::Error` (permission denied, a
/// non-directory component, ...) is propagated immediately — the caller
/// maps that to [`Containment::Undecidable`], never to `Inside`.
fn deepest_existing_ancestor(candidate: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let components: Vec<Component> = candidate.components().collect();
    let mut split = components.len();
    loop {
        let prefix: PathBuf = components[..split].iter().collect();
        match prefix.canonicalize() {
            Ok(canonical_prefix) => {
                let tail: PathBuf = components[split..].iter().collect();
                return Ok((canonical_prefix, tail));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound && split > 0 => {
                split -= 1;
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    /// `assert_not_impl_any!(Containment: From<bool>)` would require the
    /// trait to be nameable at this path; instead we assert the property
    /// this type is designed to guarantee: `Inside` and `Undecidable` (and
    /// `Outside`) are three distinct, non-overlapping variants, so no
    /// caller `match` can conflate "couldn't decide" with "allowed"
    /// without the compiler flagging an unreachable/non-exhaustive arm.
    #[test]
    fn containment_variants_are_pairwise_distinct() {
        assert_ne!(Containment::Inside, Containment::Outside);
        assert_ne!(Containment::Inside, Containment::Undecidable);
        assert_ne!(Containment::Outside, Containment::Undecidable);
    }

    #[test]
    fn symlink_escape_via_nonexistent_tail_is_outside() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let outside = tmp.path().join("outside");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(&outside).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        // repo/link -> ../outside
        let link = repo.join("link");
        symlink(Path::new("../outside"), &link).unwrap();

        // secret.txt does not exist: this is the write-target bypass case.
        let candidate = link.join("secret.txt");
        assert_eq!(root.contains(&candidate), Containment::Outside);
    }

    #[test]
    fn symlink_escape_to_existing_file_is_outside() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let outside = tmp.path().join("outside");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"hi").unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        let link = repo.join("link");
        symlink(Path::new("../outside"), &link).unwrap();

        let candidate = link.join("secret.txt");
        assert_eq!(root.contains(&candidate), Containment::Outside);
    }

    #[test]
    fn dotdot_in_existing_portion_resolving_back_inside_is_inside() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let sub = repo.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(repo.join("file.txt"), b"hi").unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        // repo/sub/../file.txt: `sub` and `file.txt` both exist, so the
        // whole candidate canonicalizes directly (no walk-up needed) and
        // the `..` is resolved by the real filesystem, landing back
        // inside the root.
        let candidate = sub.join("..").join("file.txt");
        assert_eq!(root.contains(&candidate), Containment::Inside);
    }

    #[test]
    fn dotdot_in_existing_portion_escaping_is_outside() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let sibling = tmp.path().join("sibling");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(&sibling).unwrap();
        fs::write(sibling.join("file.txt"), b"hi").unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        // repo/../sibling/file.txt: every component exists, so this
        // canonicalizes directly and lands outside the root.
        let candidate = repo.join("..").join("sibling").join("file.txt");
        assert_eq!(root.contains(&candidate), Containment::Outside);
    }

    #[test]
    fn dotdot_in_nonexistent_tail_is_rejected_not_normalized() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        // repo/does-not-exist/../file.txt: `does-not-exist` never exists,
        // so the `..` is inside the non-existent tail and cannot be
        // lexically resolved (a naive normalizer would silently collapse
        // this back to repo/file.txt, which is exactly the bug this
        // primitive exists to avoid).
        let candidate = repo.join("does-not-exist").join("..").join("file.txt");
        assert_eq!(root.contains(&candidate), Containment::Undecidable);
    }

    #[test]
    fn nonexistent_write_target_under_root_is_inside() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        // A plain, clean, non-existent path under the root: this is the
        // ordinary `write` case that plain `fs::canonicalize` cannot
        // handle on its own.
        let candidate = repo.join("new-dir").join("new-file.txt");
        assert_eq!(root.contains(&candidate), Containment::Inside);
    }

    #[test]
    fn sibling_prefix_is_outside() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let frontend = repo.join("frontend");
        let frontend_evil = repo.join("frontend-evil");
        fs::create_dir(&frontend).unwrap();
        fs::create_dir(&frontend_evil).unwrap();
        let root = CanonicalRoot::new(&frontend).unwrap();

        let candidate = frontend_evil.join("secret.txt");
        assert_eq!(root.contains(&candidate), Containment::Outside);
    }

    #[test]
    fn non_canonical_root_gives_consistent_answers() {
        let tmp = TempDir::new().unwrap();
        let real_repo = tmp.path().join("real-repo");
        let outside = tmp.path().join("outside");
        fs::create_dir(&real_repo).unwrap();
        fs::create_dir(&outside).unwrap();
        let repo_link = tmp.path().join("repo-link");
        symlink(&real_repo, &repo_link).unwrap();

        // Root is reached via a symlink; construction must canonicalize
        // it so checks are consistent regardless of which spelling was
        // used to construct the root.
        let root = CanonicalRoot::new(&repo_link).unwrap();
        assert_eq!(root.as_path(), real_repo.canonicalize().unwrap());

        let inside_via_link = repo_link.join("file.txt");
        let inside_via_real = real_repo.join("file.txt");
        assert_eq!(root.contains(&inside_via_link), Containment::Inside);
        assert_eq!(root.contains(&inside_via_real), Containment::Inside);

        let outside_candidate = outside.join("secret.txt");
        assert_eq!(root.contains(&outside_candidate), Containment::Outside);
    }

    #[test]
    fn canonicalize_error_other_than_not_found_is_not_inside() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        // A regular file used as a path prefix: canonicalizing
        // `repo/not-a-dir/child` fails with `NotADirectory`, not
        // `NotFound` — the walk-up must not treat this as "keep popping",
        // and the result must never be `Inside`.
        let not_a_dir = repo.join("not-a-dir");
        fs::write(&not_a_dir, b"hi").unwrap();
        let candidate = not_a_dir.join("child");

        assert_ne!(root.contains(&candidate), Containment::Inside);
    }

    #[test]
    fn candidate_equal_to_root_is_inside() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        assert_eq!(root.contains(&repo), Containment::Inside);
    }

    #[test]
    fn relative_candidate_is_undecidable() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        assert_eq!(
            root.contains(Path::new("relative/file.txt")),
            Containment::Undecidable
        );
        assert_eq!(root.contains(Path::new(".")), Containment::Undecidable);
    }

    #[test]
    fn root_construction_fails_on_nonexistent_root() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let err = CanonicalRoot::new(&missing).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn deep_nonexistent_tail_stays_inside_when_clean() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        let candidate = repo.join("a").join("b").join("c").join("d.txt");
        assert_eq!(root.contains(&candidate), Containment::Inside);
    }

    // ---- resolve_candidate: the single resolution rule every
    // root-enforcement site shares (board item 01KZVZ56SBPSTZHAXXGYCNETNX) ----

    #[test]
    fn resolve_candidate_joins_relative_onto_cwd() {
        let cwd = Path::new("/tmp/x");
        assert_eq!(
            resolve_candidate(cwd, "a/b"),
            Some(PathBuf::from("/tmp/x/a/b"))
        );
    }

    #[test]
    fn resolve_candidate_passes_absolute_through_unchanged() {
        let cwd = Path::new("/tmp/x");
        assert_eq!(
            resolve_candidate(cwd, "/etc/hosts"),
            Some(PathBuf::from("/etc/hosts"))
        );
    }

    #[test]
    fn resolve_candidate_rejects_nul_byte_in_relative_input() {
        let cwd = Path::new("/tmp/x");
        assert_eq!(resolve_candidate(cwd, "a\0b"), None);
    }

    #[test]
    fn resolve_candidate_rejects_nul_byte_in_absolute_input() {
        let cwd = Path::new("/tmp/x");
        assert_eq!(resolve_candidate(cwd, "/a\0b"), None);
    }

    #[test]
    fn curdir_in_tail_is_stripped_not_rejected() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        let candidate = repo.join("new-dir").join(".").join("file.txt");
        assert_eq!(root.contains(&candidate), Containment::Inside);
    }
}
