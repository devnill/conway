//! Fetches a `git-subdir`/`github` [`crate::manifest::PluginSource`] by
//! invoking the SYSTEM `git` binary -- board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`,
//! layer 4, bounded exactly the way the ruling bounds it: **no git library
//! enters this workspace's lock.** `git2` is still not a dependency
//! (`Cargo.toml`'s own doc, amended for this ruling); this module shells out
//! to whatever `git` the operator already has installed, the identical
//! resolution Claude Code itself performs for the same two source kinds.
//!
//! # Why shelling out, and not a crate
//!
//! C-04 (dependency minimalism) plus the ruling's own words: "invoke the
//! system `git` (no crate enters the lock; refuse legibly if `git` is
//! absent)". An operator installing conway already needs `git` for
//! everything else this project assumes (`docs/plugins/marketplace.md`
//! itself is read from a git checkout); a plugin marketplace that needs the
//! SAME binary asks nothing new of that operator's machine, unlike `git2`
//! (a whole libgit2 build) or `tar`/`zip` (a new archive-extraction attack
//! surface -- P-10 -- that this item's ruling explicitly declines to open).
//!
//! # No archive support -- still, deliberately
//!
//! [`fetch_git_source`] fetches exactly the two kinds
//! [`crate::manifest::PluginSource`] can represent as fetchable
//! (`GitSubdir`/`Github`); [`crate::manifest::PluginSource::Unsupported`]
//! refuses by name via [`clone_url`] before invoking `git` at all -- an
//! archive-requiring source kind never reaches a subprocess.
//!
//! # Bounded, never a hang (P-10)
//!
//! [`GIT_TIMEOUT`] bounds the whole clone -- generous for a real
//! repository over a slow connection, but an unreachable git remote must be
//! an ordinary reported failure, never a hang, exactly like this crate's
//! own HTTP client's 20-second bound (`manifest::client`'s own doc) applied
//! to a transport that can legitimately take longer. "Bounded" means the
//! CHILD PROCESS itself, not merely this module's own `.await`:
//! [`run_git_clone`] holds the spawned `git`'s [`tokio::process::Child`]
//! for its whole run (never `Child::wait_with_output`, which consumes it)
//! specifically so that on timeout it can `child.kill().await` BEFORE
//! returning the timeout error -- `.kill_on_drop(true)` is set too, as a
//! floor, but is not relied on alone: a merely-dropped, not explicitly
//! killed, child can keep running and keep writing into `checkout_dir`
//! while [`fail_cleaning_up`]'s `remove_dir_all` is concurrently deleting
//! it, a live race an orphaned `git clone` would otherwise create.
//!
//! # A git checkout can contain a symlink too (P-10)
//!
//! `install.rs`'s own "no archive, so no archive-traversal class of bug"
//! argument bounds a DIFFERENT surface than this one. A git checkout is not
//! an archive, but it is still untrusted, network-supplied content, and
//! nothing stops a malicious repository from committing a symlink that
//! resolves outside its own tree. [`validate_checkout_tree`] refuses the
//! WHOLE install if the checked-out plugin root contains any symlink
//! anywhere -- never followed, never partially accepted -- before a single
//! byte is copied into conway's own plugin store. This is "a narrower
//! surface, not an absent one" (the board item's ruling, verbatim): the
//! hazard CLASS P-10 names is real here, just smaller than an arbitrary
//! archive format's.
//!
//! **The plugin root itself is not a "descendant" [`validate_checkout_tree`]
//! ever visits -- it is where the walk STARTS.** `Path::is_dir` and
//! `std::fs::read_dir` both FOLLOW a symlink they are given directly, so a
//! `git-subdir` entry naming `path: "plugin"` where the repository commits
//! `plugin` itself as a symlink (git's own mode `120000` blob) to an
//! arbitrary absolute path would previously have its TARGET walked and
//! copied, unnoticed, by code whose own doc claimed every symlink
//! "anywhere" was refused. [`validate_plugin_root`] closes this before
//! [`validate_checkout_tree`] ever runs: `std::fs::symlink_metadata` (never
//! followed) on the resolved root itself, PLUS canonicalizing both
//! `checkout_dir` and the resolved root and requiring the second to start
//! with the first -- the second check is what catches a symlink in an
//! INTERMEDIATE path component (`path: "a/b/plugin"` where `a` escapes),
//! which the first alone would miss whenever the final component happens
//! to be an ordinary directory once the earlier symlink is resolved.
//!
//! # `git-subdir`'s own URL is untrusted input too (P-10)
//!
//! `git-subdir`'s `url` comes directly from the marketplace manifest --
//! network-supplied, untrusted. Passed to `git clone` unchecked, it could
//! name one of git's OTHER transports (`ext::<command>`, `fd::<n>`, a bare
//! local path) rather than a network remote at all; `ext::` in particular
//! runs an arbitrary shell command as this crate's own operator. [`clone_url`]
//! refuses any `git-subdir` URL that is not `http://`/`https://` before
//! invoking `git` at all (`MarketplaceError::UnsafeGitUrl`) -- an ALLOW-by-
//! prefix, not a deny-by-prefix: everything not on the allow-list is
//! refused outright, matching `docs/plugins/trust-and-security.md`'s own
//! "deny-by-prefix is a seatbelt, not a boundary" lesson applied in the
//! other direction. `github`'s own clone URL is never operator- or
//! network-supplied text passed through verbatim: this module BUILDS it
//! from `owner/repo`, always `https://github.com/<repo>.git`.
//!
//! [`clone_url`] also refuses a `git-subdir` URL whose authority embeds
//! userinfo (`https://user:pass@host/...`) outright, rather than stripping
//! it and proceeding: a legitimate public marketplace has no reason to ask
//! this crate to carry a credential through a clone, and the credential
//! would otherwise survive into [`MarketplaceError::GitFailed`]'s own
//! `detail` and into `conway-cli`'s operator-facing "fetched via git from
//! {url}" disclosure -- a TUI transcript entry that can be copied,
//! screen-shared, or logged. Because this refusal happens before `git` is
//! ever invoked, no downstream error variant can carry a credentialed URL
//! either; nothing downstream needs its own redaction.

use std::path::Path;
use std::time::Duration;

use crate::error::MarketplaceError;
use crate::manifest::PluginSource;

/// Overrides which program this module invokes as `git`, so a test can
/// simulate "git is not installed" (or substitute a stub script)
/// deterministically, without depending on whether the machine actually
/// running the test suite happens to have `git` on its `PATH` (steering
/// policy P-15: acceptance must not depend on one machine's local
/// configuration). Not part of this crate's public API.
///
/// **`#[cfg(test)]`, not merely documented-as-test-only.** An env var this
/// module's own `git_program()` reads unconditionally would let ANYTHING
/// that can influence this process's environment -- a shell profile, a
/// `.env` loader, a container/CI definition, a wrapper script --
/// substitute an arbitrary binary for every git install this crate ever
/// performs, invisibly, in a release build. Gating the constant (and
/// [`git_program`]'s reading half) behind `#[cfg(test)]` means the seam
/// does not exist in a compiled release binary AT ALL: there is no
/// mechanism left to accidentally rely on, only a `cfg(not(test))` fallback
/// that always returns `"git"`. This crate's own tests (here and in
/// `install.rs`, both integration-free `#[cfg(test)] mod tests` inside this
/// crate's own `src/`, never a `tests/` integration binary) compile with
/// `cfg(test)` set, so no feature flag is needed to reach this from either.
#[cfg(test)]
pub(crate) const GIT_PROGRAM_ENV: &str = "CONWAY_MARKETPLACE_GIT_PROGRAM";

/// How long a single `git` invocation may run before this crate gives up
/// and reports [`MarketplaceError::GitFailed`] rather than hang -- generous
/// for a real clone over a slow connection, bounded so an unreachable
/// remote cannot hang `/plugin install` forever (P-10, extended from the
/// HTTP client's own 20s to a transport that may legitimately need
/// longer).
pub(crate) const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// The `git` binary to invoke -- always `"git"` (resolved via `PATH`, like
/// every other subprocess this crate spawns) in a release build. Only a
/// `#[cfg(test)]` build can override it, via `GIT_PROGRAM_ENV` (a plain
/// code span here, deliberately not an intra-doc link: that constant is
/// itself `#[cfg(test)]`-gated, so it does not exist at all in the default
/// build this doc comment is compiled under, and an intra-doc link to a
/// cfg'd-out item is exactly the broken-link shape this workspace's own
/// `cargo doc` gate refuses) -- see that constant's own doc for why the
/// override does not exist as a mechanism outside a test binary at all.
#[cfg(not(test))]
fn git_program() -> String {
    "git".to_string()
}

#[cfg(test)]
fn git_program() -> String {
    std::env::var(GIT_PROGRAM_ENV).unwrap_or_else(|_| "git".to_string())
}

/// Refuses, by name, if the system `git` binary cannot be run at all --
/// board item's acceptance 5. Checked up front, before any attempt to
/// clone, so a missing `git` is reported as exactly that rather than a
/// confusing failure partway through.
async fn require_git(program: &str) -> Result<(), MarketplaceError> {
    let result = tokio::process::Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
    match result {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(MarketplaceError::GitUnavailable {
            program: program.to_string(),
            detail: format!("`{program} --version` exited with {status}"),
        }),
        Err(source) => Err(MarketplaceError::GitUnavailable {
            program: program.to_string(),
            detail: source.to_string(),
        }),
    }
}

/// The whole-repository clone URL for `source`, and a refusal (never a
/// subprocess) for anything this module cannot fetch at all -- see this
/// module's own doc for both hazards this checks: an archive-requiring
/// source kind, and a `git-subdir` URL naming a git transport other than
/// `http(s)://`.
fn clone_url(plugin_id: &str, source: &PluginSource) -> Result<String, MarketplaceError> {
    match source {
        PluginSource::GitSubdir { url, .. } => {
            let allowed_scheme = url.starts_with("https://") || url.starts_with("http://");
            if !allowed_scheme {
                return Err(MarketplaceError::UnsafeGitUrl {
                    id: plugin_id.to_string(),
                    url: url.clone(),
                });
            }
            // Refused outright, not stripped-and-proceeded -- this
            // module's own doc, "git-subdir's own URL is untrusted input
            // too". `redacted` never carries the credential itself, even
            // in the error this returns.
            if let Some(redacted) = credentialed_url_redacted(url) {
                return Err(MarketplaceError::CredentialedGitUrl {
                    id: plugin_id.to_string(),
                    url: redacted,
                });
            }
            Ok(url.clone())
        }
        // Built by this crate from `owner/repo`, never operator- or
        // network-supplied text passed through verbatim -- always
        // `https://`, so no scheme/credential check applies here.
        PluginSource::Github { repo } => Ok(format!("https://github.com/{repo}.git")),
        PluginSource::Unsupported { kind } => Err(MarketplaceError::UnsupportedSourceKind {
            id: plugin_id.to_string(),
            kind: kind.clone(),
        }),
    }
}

/// If `url`'s authority section embeds userinfo (`user[:pass]@host`),
/// returns a redacted copy with everything before the final `@` in that
/// section replaced by `***` -- otherwise `None`. Never returns (or even
/// separately extracts) the credential itself; the caller uses the `Some`
/// case only to build an error, never to proceed with the original `url`.
fn credentialed_url_redacted(url: &str) -> Option<String> {
    let scheme_end = url.find("://")?;
    let (scheme, rest) = url.split_at(scheme_end + 3);
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let at = authority.rfind('@')?;
    let host = &authority[at + 1..];
    Some(format!("{scheme}***@{host}{tail}"))
}

/// The subdirectory inside the clone that is the plugin's own root --
/// `git-subdir`'s own `path`; `github` names none, so the plugin root is
/// the repository root itself.
fn subdir(source: &PluginSource) -> Option<&str> {
    match source {
        PluginSource::GitSubdir { path, .. } => Some(path.as_str()),
        PluginSource::Github { .. } | PluginSource::Unsupported { .. } => None,
    }
}

/// Clones `source`'s repository into a fresh, throwaway directory beside
/// `staging_dir` (under `store_root`, so the later copy stays on one
/// filesystem) and copies its plugin root (`subdir`, or the repository
/// root itself for `github`) into `staging_dir`, which the caller
/// ([`crate::install::install_entry`]) has already ensured is empty or
/// nonexistent -- mirrors [`crate::install::stage_files`]'s own contract
/// exactly, so `install_entry` treats a git-sourced entry and a files-map
/// entry identically once this returns `Ok`.
///
/// Every entry in the copied tree is validated BEFORE anything is written
/// into `staging_dir` (this module's own doc, "A git checkout can contain a
/// symlink too"): a symlink anywhere in the plugin root refuses the whole
/// install, never a partial one.
pub(crate) async fn fetch_git_source(
    plugin_id: &str,
    source: &PluginSource,
    store_root: &Path,
    staging_dir: &Path,
) -> Result<(), MarketplaceError> {
    // Checked BEFORE `git` is required to be present at all: an
    // archive-requiring source kind, or a `git-subdir` URL naming an unsafe
    // transport, is refused on the manifest's own say-so, regardless of
    // whether this machine happens to have a working `git` -- neither
    // refusal should depend on the other.
    let url = clone_url(plugin_id, source)?;
    let program = git_program();
    require_git(&program).await?;

    let checkout_dir = store_root.join(format!(".{plugin_id}.git-checkout-tmp"));
    let _ = tokio::fs::remove_dir_all(&checkout_dir).await;

    if let Err(err) = run_git_clone(&program, &url, &checkout_dir, plugin_id).await {
        return fail_cleaning_up(&checkout_dir, err).await;
    }

    let plugin_root = match subdir(source) {
        Some(rel) => match crate::install::validate_relative_path(plugin_id, rel) {
            Ok(safe_rel) => checkout_dir.join(safe_rel),
            Err(err) => return fail_cleaning_up(&checkout_dir, err).await,
        },
        None => checkout_dir.clone(),
    };
    if let Err(err) =
        validate_plugin_root(plugin_id, &url, subdir(source), &checkout_dir, &plugin_root)
    {
        return fail_cleaning_up(&checkout_dir, err).await;
    }

    if let Err(err) = validate_checkout_tree(plugin_id, &plugin_root) {
        return fail_cleaning_up(&checkout_dir, err).await;
    }
    if let Err(err) = copy_tree(&plugin_root, staging_dir, plugin_id) {
        return fail_cleaning_up(&checkout_dir, err).await;
    }

    let _ = tokio::fs::remove_dir_all(&checkout_dir).await;
    Ok(())
}

/// Removes `checkout_dir` (best-effort, never a second error on top of the
/// one already being reported) and returns `err` -- every early-return
/// branch in [`fetch_git_source`] goes through this so the throwaway
/// checkout directory never survives a failed install.
async fn fail_cleaning_up(
    checkout_dir: &Path,
    err: MarketplaceError,
) -> Result<(), MarketplaceError> {
    let _ = tokio::fs::remove_dir_all(checkout_dir).await;
    Err(err)
}

/// Runs `git clone --depth 1 --single-branch -- <url> <into>`, bounded by
/// [`GIT_TIMEOUT`]. `--` separates the URL from any flag `git` might
/// otherwise mistake it for (defense in depth alongside [`clone_url`]'s own
/// scheme check).
///
/// **Timing out KILLS the child, it does not merely stop awaiting it** --
/// this module's own doc, "Bounded, never a hang". Deliberately never
/// `Child::wait_with_output`, which CONSUMES the `Child`: doing so would
/// leave no handle to kill if the wait times out, and `.kill_on_drop(true)`
/// alone (set below as a floor, not relied on exclusively) only reaps the
/// child on a best-effort background task, racing
/// [`fail_cleaning_up`]'s own `remove_dir_all` of the same directory the
/// orphaned `git` may still be writing into. `child.wait()` (borrows, never
/// consumes) is used instead specifically so `child` is still ours to
/// `.kill().await` on the timeout branch, before this function returns.
async fn run_git_clone(
    program: &str,
    url: &str,
    into: &Path,
    plugin_id: &str,
) -> Result<(), MarketplaceError> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(["clone", "--depth", "1", "--single-branch", "--quiet", "--"])
        .arg(url)
        .arg(into)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|source| MarketplaceError::GitUnavailable {
            program: program.to_string(),
            detail: source.to_string(),
        })?;
    let mut stderr_pipe = child.stderr.take();

    // Drains stderr concurrently with waiting for exit (the same shape
    // `Child::wait_with_output` uses internally, reimplemented by hand
    // here because that method consumes `child` -- see this function's own
    // doc). This whole future only ever BORROWS `child`/`stderr_pipe`; on
    // timeout it is dropped, handing both back for the explicit kill
    // below.
    let run = async {
        let mut stderr_buf = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = tokio::io::AsyncReadExt::read_to_end(pipe, &mut stderr_buf).await;
        }
        let status = child.wait().await;
        (status, stderr_buf)
    };

    let (status, stderr_buf) = match tokio::time::timeout(GIT_TIMEOUT, run).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(MarketplaceError::GitFailed {
                id: plugin_id.to_string(),
                url: url.to_string(),
                detail: format!("timed out after {}s", GIT_TIMEOUT.as_secs()),
            });
        }
    };
    let status = status.map_err(|source| MarketplaceError::GitUnavailable {
        program: program.to_string(),
        detail: source.to_string(),
    })?;

    if !status.success() {
        return Err(MarketplaceError::GitFailed {
            id: plugin_id.to_string(),
            url: url.to_string(),
            detail: String::from_utf8_lossy(&stderr_buf).trim().to_string(),
        });
    }
    Ok(())
}

/// Refuses `plugin_root` outright unless it is an ordinary directory
/// strictly inside `checkout_dir` -- called BEFORE [`validate_checkout_tree`]
/// ever runs, closing the gap that walk cannot: `Path::is_dir` and
/// `std::fs::read_dir` both FOLLOW a symlink they are given directly, so a
/// walk that starts AT `plugin_root` never gets a chance to flag `plugin_root`
/// itself being one (see this module's own doc, "The plugin root itself is
/// not a 'descendant' `validate_checkout_tree` ever visits").
///
/// Two independent checks, not one:
///
/// 1. [`std::fs::symlink_metadata`] on `plugin_root` itself (never
///    followed) -- catches a symlink that IS the resolved plugin root: a
///    `git-subdir` entry naming `path: "plugin"` where the repository
///    commits `plugin` as a symlink (git's own mode `120000` blob) to an
///    arbitrary absolute path.
/// 2. Canonicalizing both `checkout_dir` and `plugin_root` and requiring
///    the second to start with the first -- catches a symlink in an
///    INTERMEDIATE path component (`path: "a/b/plugin"` where `a` is the
///    symlink), which check 1 alone would miss whenever the final
///    component happens to be an ordinary directory once the earlier
///    symlink is resolved.
fn validate_plugin_root(
    plugin_id: &str,
    url: &str,
    subdir_label: Option<&str>,
    checkout_dir: &Path,
    plugin_root: &Path,
) -> Result<(), MarketplaceError> {
    let no_directory = || MarketplaceError::GitFailed {
        id: plugin_id.to_string(),
        url: url.to_string(),
        detail: format!(
            "the checkout has no directory at '{}'",
            subdir_label.unwrap_or(".")
        ),
    };
    let unsafe_root = || MarketplaceError::UnsafeFilePath {
        id: plugin_id.to_string(),
        path: plugin_root.display().to_string(),
    };

    let meta = match std::fs::symlink_metadata(plugin_root) {
        Ok(meta) => meta,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Err(no_directory()),
        Err(source) => {
            return Err(MarketplaceError::Io {
                id: plugin_id.to_string(),
                source,
            })
        }
    };
    if meta.file_type().is_symlink() {
        return Err(unsafe_root());
    }
    if !meta.is_dir() {
        return Err(no_directory());
    }

    let checkout_canonical =
        std::fs::canonicalize(checkout_dir).map_err(|source| MarketplaceError::Io {
            id: plugin_id.to_string(),
            source,
        })?;
    let root_canonical =
        std::fs::canonicalize(plugin_root).map_err(|source| MarketplaceError::Io {
            id: plugin_id.to_string(),
            source,
        })?;
    if !root_canonical.starts_with(&checkout_canonical) {
        return Err(unsafe_root());
    }
    Ok(())
}

/// Walks every entry under `root` and refuses the whole install if any one
/// of them is a symlink -- see this module's own doc, "A git checkout can
/// contain a symlink too". `.git` itself is skipped, never descended into:
/// it is not part of the plugin, and refusing on whatever it happens to
/// contain would refuse installs that have nothing wrong with them.
fn validate_checkout_tree(plugin_id: &str, root: &Path) -> Result<(), MarketplaceError> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|source| MarketplaceError::Io {
            id: plugin_id.to_string(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| MarketplaceError::Io {
                id: plugin_id.to_string(),
                source,
            })?;
            // `DirEntry::metadata` does not traverse a symlink -- exactly
            // the check this needs, so a symlinked directory is caught
            // here rather than followed into.
            let meta = entry.metadata().map_err(|source| MarketplaceError::Io {
                id: plugin_id.to_string(),
                source,
            })?;
            let path = entry.path();
            if meta.file_type().is_symlink() {
                return Err(MarketplaceError::UnsafeFilePath {
                    id: plugin_id.to_string(),
                    path: path.display().to_string(),
                });
            }
            if meta.is_dir() {
                if path.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                stack.push(path);
            }
        }
    }
    Ok(())
}

/// Copies every entry under `src` into `dest` (which the caller has already
/// ensured is empty or nonexistent), skipping `.git`. Called only after
/// [`validate_checkout_tree`] has already refused any symlink in the same
/// tree, so this never has to re-check for one.
fn copy_tree(src: &Path, dest: &Path, plugin_id: &str) -> Result<(), MarketplaceError> {
    std::fs::create_dir_all(dest).map_err(|source| MarketplaceError::Io {
        id: plugin_id.to_string(),
        source,
    })?;
    for entry in std::fs::read_dir(src).map_err(|source| MarketplaceError::Io {
        id: plugin_id.to_string(),
        source,
    })? {
        let entry = entry.map_err(|source| MarketplaceError::Io {
            id: plugin_id.to_string(),
            source,
        })?;
        let file_name = entry.file_name();
        if file_name == ".git" {
            continue;
        }
        let from = entry.path();
        let to = dest.join(&file_name);
        let meta = entry.metadata().map_err(|source| MarketplaceError::Io {
            id: plugin_id.to_string(),
            source,
        })?;
        if meta.is_dir() {
            copy_tree(&from, &to, plugin_id)?;
        } else {
            std::fs::copy(&from, &to).map_err(|source| MarketplaceError::Io {
                id: plugin_id.to_string(),
                source,
            })?;
        }
    }
    Ok(())
}

/// Test-only support for [`GIT_PROGRAM_ENV`] -- shared by this module's own
/// tests AND `install.rs`'s (both drive a stub `git` through the same
/// process-global env var). [`with_program`] serializes every test that
/// touches it against every other one, via a `tokio::sync::Mutex` held
/// across the `.await` its own body performs -- a plain `std::sync::Mutex`
/// would need dropping before the await to avoid a lint against holding a
/// sync lock there, which would reopen exactly the race window this exists
/// to close (another test setting the SAME env var between this test's own
/// set and its restore). Each of this crate's `#[tokio::test]`s gets its
/// own single-task `current_thread` runtime, so holding an async mutex
/// across an await here serializes ACROSS test THREADS without blocking
/// anything else within any one test's own runtime.
#[cfg(test)]
pub(crate) mod test_support {
    use std::future::Future;

    use super::GIT_PROGRAM_ENV;

    fn lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// Runs `body` with [`GIT_PROGRAM_ENV`] set to `program` for its whole
    /// duration, restoring whatever value (if any) preceded it -- see this
    /// module's own doc for why this serializes against every OTHER test
    /// touching the same env var rather than merely saving/restoring it.
    pub(crate) async fn with_program<F, Fut, T>(program: &std::ffi::OsStr, body: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let _guard = lock().lock().await;
        let previous = std::env::var(GIT_PROGRAM_ENV).ok();
        std::env::set_var(GIT_PROGRAM_ENV, program);
        let result = body().await;
        match previous {
            Some(value) => std::env::set_var(GIT_PROGRAM_ENV, value),
            None => std::env::remove_var(GIT_PROGRAM_ENV),
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Board item's acceptance 4: an archive-requiring (or any otherwise
    /// unrecognized) source kind refuses BY NAME, before `git` is ever
    /// invoked -- proven at the `clone_url` level, no subprocess needed.
    #[test]
    fn an_unsupported_source_kind_is_refused_by_name_before_any_git_invocation() {
        let err = clone_url(
            "archived-thing",
            &PluginSource::Unsupported {
                kind: "url".to_string(),
            },
        )
        .expect_err("archive-requiring kinds are refused");
        assert_eq!(err.kind(), "unsupported_source_kind");
        assert!(err.to_string().contains("'url'"), "{err}");
    }

    /// A `git-subdir` URL naming a non-http(s) git transport is refused
    /// before `git` is ever invoked -- this module's own doc, "git-subdir's
    /// own URL is untrusted input too".
    #[test]
    fn a_non_http_git_subdir_url_is_refused() {
        for dangerous in [
            "ext::sh -c id",
            "fd::0",
            "/etc/passwd",
            "file:///etc",
            "ssh://git@example.com/repo.git",
        ] {
            let err = clone_url(
                "acme-tools",
                &PluginSource::GitSubdir {
                    url: dangerous.to_string(),
                    path: "plugin".to_string(),
                },
            )
            .expect_err(dangerous);
            assert_eq!(err.kind(), "unsafe_git_url", "{dangerous}");
        }
    }

    /// An ordinary `https://` `git-subdir` URL is accepted unchanged.
    #[test]
    fn an_https_git_subdir_url_is_accepted() {
        let url = clone_url(
            "beepboop",
            &PluginSource::GitSubdir {
                url: "https://github.com/devnill/beepboop".to_string(),
                path: "plugin".to_string(),
            },
        )
        .expect("https is allowed");
        assert_eq!(url, "https://github.com/devnill/beepboop");
    }

    /// An `https://` `git-subdir` URL with embedded userinfo is refused
    /// outright -- BEFORE `git` is ever invoked -- and the credential never
    /// appears anywhere in the error this produces. The discriminating
    /// observable: if `clone_url`'s credential check were deleted, this
    /// URL would be ACCEPTED (it already passes the scheme allow-list), so
    /// asserting the specific `credentialed_git_url` kind -- not merely
    /// "an error occurred" -- is what actually exercises the fix.
    #[test]
    fn a_git_subdir_url_with_embedded_credentials_is_refused_and_never_echoed() {
        let err = clone_url(
            "acme-tools",
            &PluginSource::GitSubdir {
                url: "https://attacker:s3cr3t@example.com/repo.git".to_string(),
                path: "plugin".to_string(),
            },
        )
        .expect_err("a credentialed url must be refused");
        assert_eq!(err.kind(), "credentialed_git_url");
        let message = err.to_string();
        assert!(!message.contains("s3cr3t"), "{message}");
        assert!(!message.contains("attacker"), "{message}");
        assert!(message.contains("example.com"), "{message}");
    }

    /// `github` sources build their own clone URL from `owner/repo` --
    /// always `https://`, regardless of what `repo` itself contains.
    #[test]
    fn a_github_source_builds_its_own_https_clone_url() {
        let url = clone_url(
            "ideate",
            &PluginSource::Github {
                repo: "ideate-ai/ideate".to_string(),
            },
        )
        .expect("github sources always build an https url");
        assert_eq!(url, "https://github.com/ideate-ai/ideate.git");
    }

    /// Board item's acceptance 5: `git` being unusable at all is refused by
    /// name, deterministically -- via [`GIT_PROGRAM_ENV`], not by depending
    /// on whether the machine running this test happens to have a real
    /// `git` (P-15).
    #[tokio::test]
    async fn git_being_unusable_is_refused_by_name() {
        let program = "conway-test-nonexistent-git-binary-2f9a7c";
        let err = require_git(program)
            .await
            .expect_err("this program name must not exist on any machine");
        assert_eq!(err.kind(), "git_unavailable");
        assert!(err.to_string().contains(program), "{err}");
    }

    /// The same refusal, reached through the real public entry point this
    /// crate's callers use -- proves the env-var seam actually wires into
    /// [`fetch_git_source`], not only into the private [`require_git`]
    /// helper this test exercises directly above.
    #[tokio::test]
    async fn fetch_git_source_refuses_when_git_is_unusable() {
        let store = tempfile::tempdir().expect("store tempdir");
        let source = PluginSource::Github {
            repo: "devnill/outpost".to_string(),
        };
        // Bound to a local: `&store.path().join(..)` would be a temporary the
        // returned future outlives (E0515).
        let plugin_root = store.path().join("outpost");
        let result = test_support::with_program(
            std::ffi::OsStr::new("conway-test-nonexistent-git-binary-2f9a7c"),
            || fetch_git_source("outpost", &source, store.path(), &plugin_root),
        )
        .await;

        let err = result.expect_err("git is unusable under the overridden program name");
        assert_eq!(err.kind(), "git_unavailable");
        assert!(
            !store.path().join("outpost").exists(),
            "nothing may be written when git itself cannot be run"
        );
    }

    /// The CRITICAL fix this module was reviewed for: a `git-subdir` entry
    /// whose repository commits its own `path` AS A SYMLINK (git's mode
    /// `120000` blob) to an arbitrary filesystem location must be refused,
    /// never silently followed. A REAL symlink
    /// (`std::os::unix::fs::symlink`, via `ln -s` in a stub `git` script
    /// standing in for an ordinary `mkdir -p`) proves the fix against the
    /// actual hostile shape, not a mocked-out check.
    ///
    /// **The discriminating observable.** Delete [`validate_plugin_root`]'s
    /// new check and this test does not merely stop erroring -- the install
    /// SUCCEEDS, having copied `secret_dir`'s own file into the store:
    /// `Path::is_dir`/`std::fs::read_dir` both follow the symlink, and
    /// [`validate_checkout_tree`]'s descendant walk finds no symlink of
    /// `secret_dir`'s OWN inside it to flag. Asserting the specific
    /// `unsafe_file_path` kind (not merely "an error occurred") and the
    /// absence of any store entry are both required to catch that.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_plugin_root_is_refused_and_writes_nothing() {
        use std::os::unix::fs::PermissionsExt;

        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let secret_dir = tempfile::tempdir().expect("secret tempdir");
        std::fs::write(secret_dir.path().join("id_rsa"), "not a real key").expect("write secret");

        let git_path = bin_dir.path().join("git");
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "git version 2.99.0 (stub)"
  exit 0
fi
if [ "$1" = "clone" ]; then
  last=""
  for a in "$@"; do
    last="$a"
  done
  dest="$last"
  mkdir -p "$dest"
  ln -s "{target}" "$dest/plugin"
  exit 0
fi
exit 1
"#,
            target = secret_dir.path().display()
        );
        std::fs::write(&git_path, script).expect("write stub git");
        std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x stub git");

        let store = tempfile::tempdir().expect("store tempdir");
        let source = PluginSource::GitSubdir {
            url: "https://github.com/devnill/beepboop".to_string(),
            path: "plugin".to_string(),
        };

        // Bound to a local: `&store.path().join(..)` would be a temporary the
        // returned future outlives (E0515).
        let plugin_root = store.path().join("beepboop");
        let result = test_support::with_program(git_path.as_os_str(), || {
            fetch_git_source("beepboop", &source, store.path(), &plugin_root)
        })
        .await;

        let err = result.expect_err("a symlinked plugin root must be refused");
        assert_eq!(err.kind(), "unsafe_file_path");
        assert!(
            std::fs::read_dir(store.path()).unwrap().next().is_none(),
            "nothing -- not even a partial store entry -- may be written when the plugin root \
             itself is a symlink"
        );
    }

    /// The SECOND half of the fix: a symlink in an INTERMEDIATE path
    /// component (`path: "a/b/plugin"` where `a` itself escapes
    /// `checkout_dir`) is caught by the canonicalize-and-`starts_with`
    /// check even though the FINAL component (`plugin`) is an ordinary
    /// directory once `a` resolves -- exercised directly against
    /// [`validate_plugin_root`], since a stub-`git`-driven end-to-end test
    /// would only re-prove the stub mechanism the symlink-at-the-root test
    /// above already covers, not this specific check.
    ///
    /// Discriminating observable: delete the canonicalize check (keeping
    /// only `symlink_metadata` on `plugin_root` itself) and this test
    /// starts passing incorrectly -- `plugin_root`'s OWN
    /// `symlink_metadata` reports an ordinary directory, because the
    /// symlink is in `a`, not in the final component.
    #[cfg(unix)]
    #[test]
    fn an_intermediate_symlink_component_is_refused_even_when_the_final_component_is_ordinary() {
        let checkout = tempfile::tempdir().expect("checkout tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::fs::create_dir_all(outside.path().join("b/plugin")).expect("create outside dir");
        std::os::unix::fs::symlink(outside.path(), checkout.path().join("a"))
            .expect("create intermediate symlink");

        let plugin_root = checkout.path().join("a").join("b").join("plugin");
        assert!(
            plugin_root.is_dir(),
            "the final component must be an ordinary directory once `a` resolves, for this to \
             actually exercise the canonicalize check rather than the symlink_metadata one"
        );

        let err = validate_plugin_root(
            "acme-tools",
            "https://example.com/repo.git",
            Some("a/b/plugin"),
            checkout.path(),
            &plugin_root,
        )
        .expect_err("a symlinked intermediate component must still be refused");
        assert_eq!(err.kind(), "unsafe_file_path");
    }
}
