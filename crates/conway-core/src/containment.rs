//! `CanonicalRoot`: the one correct answer to "is this candidate path inside
//! this root?" (S0 of the cwd-aware-agents charter).
//!
//! # Where this is meant to end up, and what stands in the way
//!
//! `PHILOSOPHY.md` §1 specifies confinement as a property of the plugin that
//! performs the operation: "`conway.fs` takes a root confining every path it
//! will read or write", the point being that one plugin doing both the
//! checking and the opening leaves no gap between them. Today the check runs
//! in `PermissionBroker::check_root` (`conway-runtime`) and the open runs in
//! `conway-tools`, across a task boundary -- so a symlink created inside the
//! root between the two defeats it. Closing that gap means moving enforcement
//! into `conway.fs`, and this module moving out of `conway-core` with it,
//! which is also what retires this crate's one I/O exception (architecture
//! invariant T2).
//!
//! Four things have to be answered first. They are recorded here, beside the
//! type they govern, because a plan that lives anywhere else goes stale
//! without anyone noticing:
//!
//! 1. **Where this type lives afterwards.** `conway-tools` cannot host it:
//!    `conway-runtime` still needs it for `AgentArtifactWriter`, and that
//!    dependency edge runs the wrong way. Either a small leaf crate, or
//!    `conway-core` keeps the pure half ([`resolve_candidate`],
//!    [`Containment`]) while only the `canonicalize`-calling half moves.
//!
//! 2. **Per-plugin configuration, which does not exist.** `Runtime::new`
//!    builds exactly one empty `PluginConfig` and hands the same `Arc` to
//!    every tool of every plugin, so there is no way to tell `conway.fs` what
//!    its root is. `PluginRegistry` already tracks a `plugin_id` per tool, so
//!    the keying point exists; the config path into it does not.
//!
//! 3. **Per-child narrowing.** `SubagentHost::start` enforces that a child's
//!    requested root canonicalizes inside its parent's. With confinement in
//!    the plugin there is no mechanism for a parent to narrow a child's
//!    plugin config, so this is a new core mechanism rather than a
//!    relocation -- and it is the load-bearing one, because a child that can
//!    widen its own root is not confined at all.
//!
//! 4. **The second consumer.** `AgentArtifactWriter` is confined by this same
//!    type, but `report` is a *different* plugin, and one declaring
//!    `PathArgs::Unconfinable` at that. "One plugin does both" has no story
//!    for it: either `conway.report` grows its own root (a second
//!    implementation of this logic, which is the thing worth avoiding), or
//!    artifact confinement changes shape.
//!
//! Two consequences worth knowing before starting. Retiring `PathArgs` also
//! retires `When::PathsUnder` as a permission-rule kind, which is a
//! user-visible config break. And roughly seven tests in
//! `crates/conway/tests/root_containment_seam.rs` assert broker-precedence
//! over pattern grants and `AutoAllow` -- those become meaningless once the
//! check moves past the gate, so they are deletions rather than migrations.
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
//! item ("Retire the harness-level confinement
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
        // `None` from `resolve` means "couldn't decide" (relative
        // candidate, unresolvable walk, `..` in a non-existent tail) --
        // `Undecidable`, NEVER `Outside`: those are deliberately distinct
        // variants (see `Containment`'s own doc), and collapsing "can't
        // check" into "definitively outside" would be as wrong as
        // collapsing it into "allowed" -- a caller matching on `Outside`
        // specifically (rather than treating the two as interchangeable,
        // which every caller in this tree does today, but the type exists
        // so a FUTURE one need not) would be told something this method
        // never established.
        self.resolve(candidate)
            .map_or(Containment::Undecidable, |resolved| {
                if resolved.starts_with(&self.canonical) {
                    Containment::Inside
                } else {
                    Containment::Outside
                }
            })
    }

    /// The offset-computing half of [`Self::contains`]: resolves `candidate`
    /// exactly as `contains` does (deepest-existing-ancestor walk, `..`
    /// rejected in a non-existent tail, `.` stripped) but returns the
    /// resolved absolute path on success rather than collapsing it to
    /// [`Containment`]. `None` for every case `contains` would answer
    /// `Undecidable` for (relative candidate, unresolvable walk, `..` in a
    /// non-existent tail). Shared by `contains` and
    /// [`Self::relative_if_inside`] so the two can never independently
    /// drift on what "resolved" means.
    fn resolve(&self, candidate: &Path) -> Option<PathBuf> {
        if candidate.is_relative() {
            return None;
        }

        let (existing_prefix, tail) = deepest_existing_ancestor(candidate).ok()?;

        let mut clean_tail = PathBuf::new();
        for component in tail.components() {
            match component {
                Component::ParentDir => return None,
                Component::CurDir => continue,
                other => clean_tail.push(other.as_os_str()),
            }
        }

        Some(existing_prefix.join(&clean_tail))
    }

    /// Like [`Self::contains`], but on `Inside` also returns `candidate`'s
    /// location EXPRESSED RELATIVE TO THIS ROOT, for a caller that needs to
    /// hand a relative path to an open-relative (`openat`-style) API rooted
    /// at this same canonical root -- see `conway_tools::fs::beneath`'s own
    /// doc for why that caller exists and why a relative path, not the
    /// resolved absolute one, is what it needs.
    ///
    /// This is a CONVENIENCE, not a trust boundary: the caller's own
    /// open-relative walk re-resolves `candidate` independently and is what
    /// actually enforces containment at open time (closing the TOCTOU
    /// window between this call and that one). If a race changes the
    /// filesystem between this call and the caller's open, the worst this
    /// method can do is compute a relative path that no longer denotes what
    /// it denoted here -- the caller's own walk still refuses an escape.
    pub fn relative_if_inside(&self, candidate: &Path) -> Option<PathBuf> {
        let resolved = self.resolve(candidate)?;
        if !resolved.starts_with(&self.canonical) {
            return None;
        }
        // `strip_prefix` cannot fail here: `starts_with` above already
        // confirmed `resolved` has `self.canonical` as a component-wise
        // prefix.
        Some(
            resolved
                .strip_prefix(&self.canonical)
                .expect("starts_with just confirmed this prefix")
                .to_path_buf(),
        )
    }
}

/// Why [`resolve_candidate`] could not turn `raw` into a usable `PathBuf`.
///
/// Deliberately typed rather than folded into a single `None` (as this
/// function used to return): [`Self::UnresolvableTilde`] is a case the
/// **operator-facing** message must name explicitly (INTENT.md §8.3 --
/// "when conway cannot honour a reference exactly, it refuses and names
/// what changed"), and a bare `Option` cannot distinguish it from a NUL
/// byte at the call site that builds that message. `#[non_exhaustive]`: a
/// caller that only cares "could this be resolved at all" should still
/// have to write a wildcard arm, not enumerate every reason.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// `raw` contains a NUL byte the OS path APIs cannot represent
    /// (`CString::new` fails on an interior NUL), so any resolution that
    /// returned `Ok` here would hand the caller a candidate no later
    /// filesystem call could act on either.
    #[error("path contains a NUL byte: {raw:?}")]
    NulByte { raw: String },
    /// `raw` begins with `~` -- the one prefix this function expands
    /// (exactly `~`, or a leading `~/`; see [`resolve_candidate`]'s own
    /// doc) -- but conway could not honour it: either no home directory
    /// could be determined for this process, or `raw` uses a tilde form
    /// this function does not expand (e.g. `~user/...`). `raw` is the
    /// original, unexpanded string, so the message can show the operator
    /// exactly what they wrote.
    #[error("path {raw:?} begins with `~` but could not be expanded: {reason}")]
    UnresolvableTilde { raw: String, reason: String },
}

/// Resolves a possibly-untrusted, model- or config-supplied path string
/// against `cwd`, exactly as the tool call or root check that ultimately
/// acts on it needs it resolved: `raw` beginning with exactly `~` or a
/// leading `~/` expands against the process's home directory (see below);
/// any other absolute `raw` passes through unchanged; a relative `raw`
/// joins onto `cwd`. Fails with [`ResolveError::NulByte`] for a `raw`
/// containing a NUL byte, or [`ResolveError::UnresolvableTilde`] for a
/// `raw` that begins with `~` but cannot be expanded.
///
/// # Tilde expansion is anchored, never a substring replace
///
/// Only the WHOLE of `raw` is inspected for a leading `~`: exactly `~`
/// (the bare home directory) or a leading `~/` (home-relative). A `~`
/// appearing anywhere else in `raw` -- as an ordinary filename character,
/// mid-path (`sub/~name`), or even as the very first character of a form
/// this function does not expand (`~user/docs`) -- is never rewritten by
/// substring replacement; the last of those instead fails with
/// [`ResolveError::UnresolvableTilde`] rather than being silently passed
/// through as a literal, so a caller cannot mistake "conway didn't
/// recognise this" for "conway resolved this to the literal path
/// `~user/docs`".
///
/// # The one implementation every root-enforcement site in this tree
/// shares
///
/// This exact operation (join-or-pass-through, NUL rejected, and now tilde
/// expansion) was independently restated at least three times in this tree
/// before it was collapsed here: two inlined copies in `conway-runtime`
/// (`subagent.rs`'s spawn-time confinement-root resolution and
/// `runtime.rs`'s root-agent resolution) each independently **dropped the
/// NUL guard** — the defect
/// fixed by pointing both at `conway_runtime::permission::
/// resolve_like_the_tool_will` — and a third, `conway_tools::common::
/// resolve_path`, carried the guard but as a byte-for-byte separate
/// function, kept in sync only by a doc comment demanding lockstep edits,
/// not by the compiler. Landing tilde expansion here, rather than at either
/// wrapper, is what keeps a `paths_under` permission-rule prefix and the
/// tool argument it is meant to bound expanding identically -- the two
/// `~`-prefixed strings hit the SAME code, so they cannot silently diverge
/// -- a rule and the call it bounds must never resolve a shared prefix
/// two different ways.
///
/// `conway-runtime`'s `resolve_like_the_tool_will` and `conway-tools`'
/// `resolve_path` each keep their own thin, same-signature, same-crate
/// wrapper around this function (crate layering runs `conway-runtime ->
/// conway-core` and `conway-tools -> conway-core` only, never
/// `conway-runtime -> conway-tools`, so neither crate can call the other's
/// wrapper directly, and neither may gain a new cross-crate dependency just
/// for this) — but the wrapper's BODY is now this one call, never a
/// restatement, so the two can no longer independently drop the guard.
pub fn resolve_candidate(cwd: &Path, raw: &str) -> Result<PathBuf, ResolveError> {
    if raw.contains('\0') {
        return Err(ResolveError::NulByte {
            raw: raw.to_string(),
        });
    }
    if let Some(expanded) = expand_tilde(raw)? {
        return Ok(expanded);
    }
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        Ok(candidate.to_path_buf())
    } else {
        Ok(cwd.join(candidate))
    }
}

/// The tilde half of [`resolve_candidate`], split out so the anchoring
/// rule (whole-string prefix, never a substring search) is exercised by one
/// piece of code with one job. `Ok(None)` means `raw` does not begin with
/// `~` at all -- [`resolve_candidate`] falls through to its ordinary
/// absolute/relative handling in that case, so a `~` anywhere else in
/// `raw` is untouched.
///
/// Known limit: only a literal forward-slash `~/` is recognized, never a
/// native Windows `~\`. Forward slashes work fine as path separators on
/// Windows, so `~/Documents/file.txt` still expands there; a Windows user
/// who types `~\Documents\file.txt` instead gets `UnresolvableTilde` rather
/// than expansion. Left as-is rather than special-cased.
fn expand_tilde(raw: &str) -> Result<Option<PathBuf>, ResolveError> {
    if raw == "~" {
        return home_dir()
            .map(Some)
            .ok_or_else(|| ResolveError::UnresolvableTilde {
                raw: raw.to_string(),
                reason: "no home directory could be determined for this process".to_string(),
            });
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir()
            .map(|home| Some(home.join(rest.trim_start_matches('/'))))
            .ok_or_else(|| ResolveError::UnresolvableTilde {
                raw: raw.to_string(),
                reason: "no home directory could be determined for this process".to_string(),
            });
    }
    if raw.starts_with('~') {
        return Err(ResolveError::UnresolvableTilde {
            raw: raw.to_string(),
            reason: "conway only expands a bare `~` or a leading `~/`, not `~user`-style forms"
                .to_string(),
        });
    }
    Ok(None)
}

/// The process's home directory, if one can be determined -- the same
/// lookup `conway::config::discovery::home_settings_path` already uses for
/// `~/.conway/settings.json`, via the same `directories::BaseDirs`, so a
/// test that overrides `HOME`/`USERPROFILE` to simulate a home directory
/// observes ONE home-directory answer across the whole tree, not two
/// independently-resolved ones (on Unix, at least -- see below).
///
/// The lookup is env-var-driven on Unix (`HOME`), but on Windows
/// `directories::BaseDirs::new()` does NOT read `%USERPROFILE%` -- it goes
/// through the Windows Known Folder API
/// (`known_folder(Shell::FOLDERID_Profile)`, i.e. `SHGetKnownFolderPath`).
/// A test that sets `USERPROFILE` on a spawned child process has no effect
/// on what this function returns there; see the `#[cfg(unix)]` gate on
/// `tilde_expansion.rs`'s binary-level home-directory test for the
/// consequence.
fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
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
    // root-enforcement site shares ----

    #[test]
    fn resolve_candidate_joins_relative_onto_cwd() {
        let cwd = Path::new("/tmp/x");
        assert_eq!(
            resolve_candidate(cwd, "a/b"),
            Ok(PathBuf::from("/tmp/x/a/b"))
        );
    }

    #[test]
    fn resolve_candidate_passes_absolute_through_unchanged() {
        let cwd = Path::new("/tmp/x");
        assert_eq!(
            resolve_candidate(cwd, "/etc/hosts"),
            Ok(PathBuf::from("/etc/hosts"))
        );
    }

    #[test]
    fn resolve_candidate_rejects_nul_byte_in_relative_input() {
        let cwd = Path::new("/tmp/x");
        assert_eq!(
            resolve_candidate(cwd, "a\0b"),
            Err(ResolveError::NulByte {
                raw: "a\0b".to_string()
            })
        );
    }

    #[test]
    fn resolve_candidate_rejects_nul_byte_in_absolute_input() {
        let cwd = Path::new("/tmp/x");
        assert_eq!(
            resolve_candidate(cwd, "/a\0b"),
            Err(ResolveError::NulByte {
                raw: "/a\0b".to_string()
            })
        );
    }

    // ---- tilde expansion (board item 01M10HSENWKTEE4G691XJXBH6T) ----

    /// Success cases compare against the REAL environment's home directory
    /// (via the same `directories::BaseDirs` lookup `home_dir` uses)
    /// rather than mutating `HOME`/`USERPROFILE` in-process: this test file
    /// runs alongside every other `conway-core` unit test in one binary,
    /// and mutating a process-global env var here could race a concurrently
    /// running test. Reading the CURRENT value is safe (no mutation); every
    /// dev machine and CI runner has a discoverable home directory.
    #[test]
    fn resolve_candidate_expands_a_bare_tilde_to_the_home_directory() {
        let home = directories::BaseDirs::new()
            .expect("test environment must have a discoverable home directory")
            .home_dir()
            .to_path_buf();
        let cwd = Path::new("/tmp/x");
        assert_eq!(resolve_candidate(cwd, "~"), Ok(home));
    }

    #[test]
    fn resolve_candidate_expands_a_leading_tilde_slash_onto_the_home_directory() {
        let home = directories::BaseDirs::new()
            .expect("test environment must have a discoverable home directory")
            .home_dir()
            .to_path_buf();
        let cwd = Path::new("/tmp/x");
        assert_eq!(
            resolve_candidate(cwd, "~/docs/file.txt"),
            Ok(home.join("docs/file.txt"))
        );
    }

    /// **P-15's discriminating observable for "anchored, never a substring
    /// replace."** A `~` that is not the leading character of the whole
    /// argument -- here, the middle of the SECOND component -- must be
    /// carried through byte-for-byte. Asserted by exact `PathBuf` equality,
    /// not merely "resolves to something under cwd": a naive
    /// `raw.replace('~', home_str)` implementation would ALSO resolve to
    /// something under `/tmp/x` (it would just be wrong about what), so a
    /// looser assertion would not fail against that regression.
    #[test]
    fn resolve_candidate_does_not_expand_a_tilde_that_is_not_a_leading_component() {
        let cwd = Path::new("/tmp/x");
        assert_eq!(
            resolve_candidate(cwd, "sub/~name/file.txt"),
            Ok(PathBuf::from("/tmp/x/sub/~name/file.txt"))
        );
    }

    /// `~` as an ordinary filename character, not even at a component
    /// boundary -- the plainest form of "`~` is not a substring replace".
    #[test]
    fn resolve_candidate_does_not_expand_a_tilde_embedded_in_a_filename() {
        let cwd = Path::new("/tmp/x");
        assert_eq!(
            resolve_candidate(cwd, "foo~bar.txt"),
            Ok(PathBuf::from("/tmp/x/foo~bar.txt"))
        );
    }

    /// The ruling's other named failure mode: a tilde form this function
    /// does not expand (`~user/...`) fails with a NAMED error rather than
    /// being silently passed through as a literal (which would be
    /// indistinguishable from "conway resolved `~bob` to a real path") or
    /// silently guessed at.
    #[test]
    fn resolve_candidate_rejects_a_user_relative_tilde_form_it_does_not_expand() {
        let cwd = Path::new("/tmp/x");
        let err = resolve_candidate(cwd, "~bob/docs").unwrap_err();
        match err {
            ResolveError::UnresolvableTilde { raw, reason } => {
                assert_eq!(raw, "~bob/docs");
                assert!(
                    reason.contains("~user"),
                    "reason should name the unsupported form: {reason:?}"
                );
            }
            other => panic!("expected UnresolvableTilde, got {other:?}"),
        }
    }

    /// A NUL byte anywhere in a `~`-prefixed argument is still caught by
    /// the NUL guard, which runs before tilde expansion is even attempted
    /// -- the two guards must not shadow each other in either direction.
    #[test]
    fn resolve_candidate_nul_guard_still_applies_to_a_tilde_prefixed_argument() {
        let cwd = Path::new("/tmp/x");
        assert_eq!(
            resolve_candidate(cwd, "~/a\0b"),
            Err(ResolveError::NulByte {
                raw: "~/a\0b".to_string()
            })
        );
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

    // ---- relative_if_inside ----

    #[test]
    fn relative_if_inside_returns_the_offset_when_inside() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        let candidate = repo.join("new-dir").join("file.txt");
        assert_eq!(
            root.relative_if_inside(&candidate),
            Some(PathBuf::from("new-dir/file.txt"))
        );
    }

    #[test]
    fn relative_if_inside_is_none_when_outside() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let sibling = tmp.path().join("sibling");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(&sibling).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        assert_eq!(root.relative_if_inside(&sibling.join("f.txt")), None);
    }

    #[test]
    fn relative_if_inside_is_none_when_undecidable() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        // Relative candidate: `contains` answers `Undecidable`, and this
        // must agree, never silently returning a bogus offset.
        assert_eq!(
            root.relative_if_inside(Path::new("relative/file.txt")),
            None
        );
    }

    /// Pins the `contains`/`resolve` split itself: an `Undecidable` case
    /// must stay `Undecidable`, never collapse into `Outside` just because
    /// `resolve` returns `None` for both. This is the exact regression this
    /// item's own refactor introduced and then caught via the pre-existing
    /// `dotdot_in_nonexistent_tail_is_rejected_not_normalized`/
    /// `relative_candidate_is_undecidable` tests above -- this test pins it
    /// AT the `contains`/`resolve` boundary directly, so a future
    /// refactor of either fails here first.
    #[test]
    fn contains_and_relative_if_inside_agree_on_undecidable_vs_outside() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let sibling = tmp.path().join("sibling");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(&sibling).unwrap();
        let root = CanonicalRoot::new(&repo).unwrap();

        // Genuinely outside (fully resolvable, lands elsewhere).
        let outside_candidate = sibling.join("f.txt");
        assert_eq!(root.contains(&outside_candidate), Containment::Outside);
        assert_eq!(root.relative_if_inside(&outside_candidate), None);

        // Genuinely undecidable (relative -- can't even start resolving).
        let undecidable_candidate = Path::new("relative/f.txt");
        assert_eq!(
            root.contains(undecidable_candidate),
            Containment::Undecidable
        );
        assert_eq!(root.relative_if_inside(undecidable_candidate), None);
    }
}
