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
//! to a transport that can legitimately take longer.
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

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::MarketplaceError;
use crate::manifest::PluginSource;

/// Overrides which program this module invokes as `git`. Production code
/// never sets this; it exists purely so a test can simulate "git is not
/// installed" deterministically, without depending on whether the machine
/// actually running the test suite happens to have `git` on its `PATH`
/// (steering policy P-15: acceptance must not depend on one machine's local
/// configuration). Not part of this crate's public API.
pub(crate) const GIT_PROGRAM_ENV: &str = "CONWAY_MARKETPLACE_GIT_PROGRAM";

/// How long a single `git` invocation may run before this crate gives up
/// and reports [`MarketplaceError::GitFailed`] rather than hang -- generous
/// for a real clone over a slow connection, bounded so an unreachable
/// remote cannot hang `/plugin install` forever (P-10, extended from the
/// HTTP client's own 20s to a transport that may legitimately need
/// longer).
pub(crate) const GIT_TIMEOUT: Duration = Duration::from_secs(120);

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
            if url.starts_with("https://") || url.starts_with("http://") {
                Ok(url.clone())
            } else {
                Err(MarketplaceError::UnsafeGitUrl {
                    id: plugin_id.to_string(),
                    url: url.clone(),
                })
            }
        }
        // Built by this crate from `owner/repo`, never operator- or
        // network-supplied text passed through verbatim -- always
        // `https://`, so no scheme check applies here.
        PluginSource::Github { repo } => Ok(format!("https://github.com/{repo}.git")),
        PluginSource::Unsupported { kind } => Err(MarketplaceError::UnsupportedSourceKind {
            id: plugin_id.to_string(),
            kind: kind.clone(),
        }),
    }
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
    if !plugin_root.is_dir() {
        let err = MarketplaceError::GitFailed {
            id: plugin_id.to_string(),
            url,
            detail: format!(
                "the checkout has no directory at '{}'",
                subdir(source).unwrap_or(".")
            ),
        };
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
        .arg(into);

    let output = tokio::time::timeout(GIT_TIMEOUT, command.output()).await;
    let output = match output {
        Ok(result) => result,
        Err(_) => {
            return Err(MarketplaceError::GitFailed {
                id: plugin_id.to_string(),
                url: url.to_string(),
                detail: format!("timed out after {}s", GIT_TIMEOUT.as_secs()),
            })
        }
    };
    let output = output.map_err(|source| MarketplaceError::GitUnavailable {
        program: program.to_string(),
        detail: source.to_string(),
    })?;

    if !output.status.success() {
        return Err(MarketplaceError::GitFailed {
            id: plugin_id.to_string(),
            url: url.to_string(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
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
        let result = test_support::with_program(
            std::ffi::OsStr::new("conway-test-nonexistent-git-binary-2f9a7c"),
            || {
                fetch_git_source(
                    "outpost",
                    &source,
                    store.path(),
                    &store.path().join("outpost"),
                )
            },
        )
        .await;

        let err = result.expect_err("git is unusable under the overridden program name");
        assert_eq!(err.kind(), "git_unavailable");
        assert!(
            !store.path().join("outpost").exists(),
            "nothing may be written when git itself cannot be run"
        );
    }
}
