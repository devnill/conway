//! [`install_plugin`]/[`install_entry`] (the "install" half of acceptance
//! 1) and [`uninstall_plugin`] (acceptance 3) -- conway's plugin store.
//!
//! # Where a fetched artifact lives, and who owns it (determine-first Q1)
//!
//! `store_root` is a directory this crate treats as fully its own: every
//! plugin it installs lives at `store_root/<plugin id>`, and nothing else
//! is ever written there. The CALLER decides what `store_root` actually
//! is (`crates/conway-cli/src/tui/app/marketplace.rs` resolves it to
//! `<config dir>/plugins/marketplace`, alongside the `settings.json` the
//! matching `[plugins].claude_compat[]` entry is written into -- see that
//! module's own doc) -- this crate has no opinion about project- vs.
//! user-scoping and reads no environment variable of its own. Because
//! nothing checks a fetched artifact against a digest or an allow-list
//! (the trust ruling, `docs/plugins/trust-and-security.md`), knowing
//! EXACTLY where it landed and being able to remove it completely is the
//! whole of the operator's own control over it -- which is why
//! [`uninstall_plugin`] removes the directory outright rather than leaving
//! it and merely forgetting about it.
//!
//! # Path safety (P-10): no archive, so no archive-traversal class of bug
//!
//! `validate_relative_path` (private to this module, also used by
//! `crate::git_source` to validate a `git-subdir` entry's own `path` field)
//! is the ONLY thing standing between a marketplace-controlled `files` map
//! (or `git-subdir` subdirectory) and an arbitrary filesystem write. It
//! accepts a relative path only when EVERY component is
//! [`std::path::Component::Normal`] -- an absolute path, a `..`, a bare
//! `.`, or (on Windows) a drive/prefix component is refused outright, never
//! "sanitized" by stripping the dangerous part and proceeding (this
//! project's own `docs/plugins/trust-and-security.md` "Deny-by-prefix is a
//! seatbelt, not a boundary" lesson: refuse the whole input rather than
//! infer a safe subset of it). Because a plugin's declared file is written
//! by THIS crate, one file at a time, into a staging directory it just
//! created (never extracted from an archive whose own entries might
//! contain symlinks pointing outside the extraction root), the
//! symlink-in-an-extracted-archive hazard P-10 names does not apply here at
//! all -- there is no archive-extraction step for it to attach to, and this
//! remains true even now that a git-sourced entry is also supported: this
//! crate still never extracts an archive of any kind (`Cargo.toml`'s own
//! doc, and board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`'s ruling, both state
//! this deliberately rather than incidentally). A GIT CHECKOUT is a
//! narrower version of the same hazard class, not an absent one -- it
//! cannot be extracted-archive-traversed, but it can still contain a
//! symlink, which `crate::git_source::validate_checkout_tree` refuses
//! outright before this module ever copies a byte of it into a staging
//! directory. See `crate::git_source`'s own doc for that half.
//!
//! # Never a partial install (P-13)
//!
//! Every file `stage_files` (private to this module) fetches is written into a STAGING directory
//! (`store_root/.<plugin id>.install-tmp`), never directly into
//! `store_root/<plugin id>`. Only once every file has been fetched and
//! written successfully does [`install_entry`] remove any prior install at
//! that path and `rename` the staging directory into place -- a single
//! atomic filesystem operation on the same volume (mirroring
//! `conway::config::writer::set_plugin_installed`'s own tmp-then-rename
//! durability shape for `settings.json`). Any failure partway through
//! staging removes the staging directory and returns the error; the
//! caller's `[plugins].claude_compat[]` config write (the CALLER's
//! responsibility, not this crate's -- see `marketplace.rs`) only happens
//! AFTER this function returns `Ok`, so a failed install can never leave a
//! config entry pointing at a directory that does not exist, and a failed
//! config write (a separate step) never leaves a half-fetched directory
//! behind uncleaned either, because the directory this function commits is
//! already complete before that later step runs.

use std::path::{Path, PathBuf};

use crate::error::MarketplaceError;
use crate::manifest::{client, fetch_bytes, MarketplacePluginEntry};

/// Safety cap on any single fetched file's size -- generous for a
/// `.claude-plugin/plugin.json`, a `.mcp.json`, a `commands/*.md`, or a
/// small server script, small enough that a malicious or broken
/// marketplace entry cannot make one file fetch exhaust memory or disk
/// (P-10).
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Safety cap on how many files a single plugin entry may declare -- bounds
/// the number of HTTP requests one `install_entry` call can make.
pub const MAX_FILES_PER_PLUGIN: usize = 200;

/// A successfully installed plugin: its id and the directory it now lives
/// in under a `store_root` -- exactly the two fields
/// `conway::config::schema::ClaudeCompatPluginEntry` needs (`id`, `dir`;
/// see spec update 1: a fetched artifact is declared to conway as a
/// `[plugins].claude_compat[]` entry, nothing more).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub id: String,
    pub dir: PathBuf,
}

/// The directory a plugin with `plugin_id` would be installed into under
/// `store_root` -- exposed so a caller (the config writer's own caller,
/// which needs this exact value for a `[plugins].claude_compat[].dir`
/// entry) never duplicates this crate's own naming rule by hand.
pub fn plugin_dir(plugin_id: &str, store_root: &Path) -> PathBuf {
    store_root.join(plugin_id)
}

/// `plugin_id` must be a single, ordinary path component: non-empty, no
/// `/`/`\`, no `..`, and not itself `.` -- the plugin-id half of P-10's
/// named "path traversal in a plugin name (`../../etc`)" hazard. Checked
/// BEFORE this crate ever joins `plugin_id` onto `store_root`, in both
/// [`install_entry`] and [`uninstall_plugin`].
pub fn validate_plugin_id(id: &str) -> Result<(), MarketplaceError> {
    let unsafe_id = id.is_empty()
        || id == "."
        || id == ".."
        || id.contains('/')
        || id.contains('\\')
        || id.starts_with('.')
        || id.chars().any(|c| c.is_control());
    if unsafe_id {
        return Err(MarketplaceError::UnsafePluginId { id: id.to_string() });
    }
    Ok(())
}

/// A single declared file's relative path must resolve to somewhere
/// strictly inside its own plugin directory: every path component must be
/// [`std::path::Component::Normal`], so an absolute path, a `..`, a bare
/// `.`, or a Windows drive/prefix component is refused outright rather than
/// partially accepted. See this module's own doc for the full argument.
pub(crate) fn validate_relative_path(id: &str, rel: &str) -> Result<PathBuf, MarketplaceError> {
    if rel.is_empty() {
        return Err(MarketplaceError::UnsafeFilePath {
            id: id.to_string(),
            path: rel.to_string(),
        });
    }
    let mut out = PathBuf::new();
    for component in Path::new(rel).components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            _ => {
                return Err(MarketplaceError::UnsafeFilePath {
                    id: id.to_string(),
                    path: rel.to_string(),
                })
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(MarketplaceError::UnsafeFilePath {
            id: id.to_string(),
            path: rel.to_string(),
        });
    }
    Ok(out)
}

/// Fetches every file `entry.files` declares into `staging_dir`, which the
/// caller has already ensured is empty/nonexistent. Stops at the first
/// failure -- the caller is responsible for removing `staging_dir` on
/// error (kept out of this function so a caller that wants to retry into
/// the SAME staging directory could, though nothing in this crate does
/// that today).
async fn stage_files(
    client: &reqwest::Client,
    entry: &MarketplacePluginEntry,
    staging_dir: &Path,
) -> Result<(), MarketplaceError> {
    for (rel, url) in &entry.files {
        let safe_rel = validate_relative_path(&entry.id, rel)?;
        let bytes = fetch_bytes(client, url, MAX_FILE_BYTES).await?;
        let full = staging_dir.join(&safe_rel);
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| MarketplaceError::Io {
                    id: entry.id.clone(),
                    source,
                })?;
        }
        tokio::fs::write(&full, &bytes)
            .await
            .map_err(|source| MarketplaceError::Io {
                id: entry.id.clone(),
                source,
            })?;
    }
    Ok(())
}

/// Installs `entry` (already fetched -- see [`install_plugin`] for the
/// "fetch the manifest, then install by id" convenience wrapper) into
/// `store_root`, returning the [`InstalledPlugin`] once every one of its
/// declared files has landed. `marketplace_url` is used only to attribute a
/// client-construction failure to a URL in the returned error; no further
/// network call to it happens here (the manifest was already fetched by
/// the caller).
///
/// Re-installing the SAME id replaces whatever was there before -- the
/// staging-then-rename mechanics (this module's own doc) mean the OLD
/// install is only removed once the NEW one has fully, successfully
/// staged, so a failed re-install leaves the previous, working install
/// untouched rather than half-overwritten.
///
/// **Two fetch paths, chosen by which `entry` actually declared** (board
/// item `01M0Y6RYZA94BK6YXJ7X8TNEGR`): `entry.source` present routes to
/// `crate::git_source::fetch_git_source` (a real Claude Code entry);
/// `entry.files` non-empty routes to this module's own `stage_files` (a
/// conway-native entry). Both land in the identical `staging_dir`, so
/// everything below this branch -- the atomic rename, the `InstalledPlugin`
/// this returns -- is unchanged by which path ran. An entry declaring
/// neither is [`MarketplaceError::NoFiles`], exactly as before.
pub async fn install_entry(
    marketplace_url: &str,
    entry: &MarketplacePluginEntry,
    store_root: &Path,
) -> Result<InstalledPlugin, MarketplaceError> {
    let id = entry
        .identity()
        .ok_or(MarketplaceError::MissingIdentity)?
        .to_string();
    validate_plugin_id(&id)?;
    if entry.source.is_none() {
        if entry.files.is_empty() {
            return Err(MarketplaceError::NoFiles { id });
        }
        if entry.files.len() > MAX_FILES_PER_PLUGIN {
            return Err(MarketplaceError::TooManyFiles {
                id,
                count: entry.files.len(),
                limit: MAX_FILES_PER_PLUGIN,
            });
        }
    }

    let dest_dir = plugin_dir(&id, store_root);
    let staging_dir = store_root.join(format!(".{id}.install-tmp"));
    // Best-effort cleanup of a staging directory left behind by a prior
    // crashed/killed run -- never a hard error if it is not there, or
    // cannot be removed for some other reason (the fresh staging attempt
    // below will surface any real problem writing into it).
    let _ = std::fs::remove_dir_all(&staging_dir);

    let stage_result = if let Some(source) = &entry.source {
        crate::git_source::fetch_git_source(&id, source, store_root, &staging_dir).await
    } else {
        match client().map_err(|source| MarketplaceError::Network {
            url: marketplace_url.to_string(),
            source,
        }) {
            Ok(http) => stage_files(&http, entry, &staging_dir).await,
            Err(err) => Err(err),
        }
    };

    if let Err(err) = stage_result {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(err);
    }

    if dest_dir.exists() {
        tokio::fs::remove_dir_all(&dest_dir)
            .await
            .map_err(|source| MarketplaceError::Io {
                id: id.clone(),
                source,
            })?;
    }
    tokio::fs::rename(&staging_dir, &dest_dir)
        .await
        .map_err(|source| MarketplaceError::Io {
            id: id.clone(),
            source,
        })?;

    Ok(InstalledPlugin { id, dir: dest_dir })
}

/// Fetches `marketplace_url`'s manifest, finds `plugin_id` in it, and
/// installs it into `store_root` -- the whole "browse, then install"
/// path in one call, for a caller that already knows which id it wants
/// (`marketplace.rs`'s own `App::apply_marketplace_install`). A caller
/// that wants to show the operator the full listing FIRST calls
/// [`crate::fetch_marketplace`] directly and then [`install_entry`] on
/// whichever [`MarketplacePluginEntry`] the operator picked, without a
/// second network round trip to re-fetch the manifest.
pub async fn install_plugin(
    marketplace_url: &str,
    plugin_id: &str,
    store_root: &Path,
) -> Result<InstalledPlugin, MarketplaceError> {
    let manifest = crate::manifest::fetch_marketplace(marketplace_url).await?;
    let entry = manifest.find(marketplace_url, plugin_id)?.clone();
    install_entry(marketplace_url, &entry, store_root).await
}

/// Removes `plugin_id`'s own directory under `store_root` -- acceptance 3.
/// Returns `Ok(true)` if a directory was actually removed, `Ok(false)` if
/// `plugin_id` was never installed there (a true no-op, mirroring
/// `conway::config::writer::set_plugin_installed`'s own "removing an
/// already-absent id is a no-op, never an error" contract) -- never an
/// error for the "nothing to remove" case.
pub fn uninstall_plugin(plugin_id: &str, store_root: &Path) -> Result<bool, MarketplaceError> {
    validate_plugin_id(plugin_id)?;
    let dir = plugin_dir(plugin_id, store_root);
    if !dir.exists() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&dir).map_err(|source| MarketplaceError::Io {
        id: plugin_id.to_string(),
        source,
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn entry(id: &str, files: &[(&str, &str)]) -> MarketplacePluginEntry {
        MarketplacePluginEntry {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            version: "1.0.0".to_string(),
            source: None,
            files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn rejects_every_shape_of_unsafe_plugin_id() {
        for bad in ["", ".", "..", "a/b", "a\\b", "../etc", ".hidden"] {
            let err = validate_plugin_id(bad).expect_err(bad);
            assert_eq!(err.kind(), "unsafe_plugin_id", "{bad}");
        }
        assert!(validate_plugin_id("acme-tools").is_ok());
        assert!(validate_plugin_id("acme.tools").is_ok());
    }

    #[test]
    fn rejects_path_traversal_in_a_declared_file_path() {
        for bad in ["../../etc/passwd", "/etc/passwd", "a/../../b", "."] {
            let err = validate_relative_path("acme-tools", bad).expect_err(bad);
            assert_eq!(err.kind(), "unsafe_file_path", "{bad}");
        }
        assert_eq!(
            validate_relative_path("acme-tools", ".claude-plugin/plugin.json").unwrap(),
            PathBuf::from(".claude-plugin").join("plugin.json")
        );
    }

    #[tokio::test]
    async fn installing_writes_every_declared_file_under_the_plugin_id_directory() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/plugin.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"name":"acme-tools"}"#))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/mcp.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let store = tempfile::tempdir().expect("store tempdir");
        let e = entry(
            "acme-tools",
            &[
                (
                    ".claude-plugin/plugin.json",
                    &format!("{}/plugin.json", server.uri()),
                ),
                (".mcp.json", &format!("{}/mcp.json", server.uri())),
            ],
        );

        let installed = install_entry("http://marketplace.example/mp.json", &e, store.path())
            .await
            .expect("install");
        assert_eq!(installed.id, "acme-tools");
        assert_eq!(installed.dir, store.path().join("acme-tools"));
        assert_eq!(
            std::fs::read_to_string(installed.dir.join(".claude-plugin/plugin.json")).unwrap(),
            r#"{"name":"acme-tools"}"#
        );
        assert_eq!(
            std::fs::read_to_string(installed.dir.join(".mcp.json")).unwrap(),
            "{}"
        );
        // No staging directory left behind on success.
        assert!(!store.path().join(".acme-tools.install-tmp").exists());
    }

    #[tokio::test]
    async fn a_plugin_declaring_a_path_traversal_file_is_refused_and_writes_nothing() {
        let store = tempfile::tempdir().expect("store tempdir");
        let e = entry(
            "acme-tools",
            &[("../../etc/passwd", "https://example.com/x")],
        );

        let err = install_entry("http://marketplace.example/mp.json", &e, store.path())
            .await
            .expect_err("must refuse a traversal path");
        assert_eq!(err.kind(), "unsafe_file_path");
        assert!(
            std::fs::read_dir(store.path()).unwrap().next().is_none(),
            "nothing may be written when any declared file is unsafe"
        );
    }

    #[tokio::test]
    async fn a_failure_partway_through_leaves_no_partial_install_and_no_staging_directory() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/missing.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let store = tempfile::tempdir().expect("store tempdir");
        let e = entry(
            "acme-tools",
            &[
                ("a.json", &format!("{}/ok.json", server.uri())),
                ("b.json", &format!("{}/missing.json", server.uri())),
            ],
        );

        let err = install_entry("http://marketplace.example/mp.json", &e, store.path())
            .await
            .expect_err("one file 404s");
        assert_eq!(err.kind(), "http");
        assert!(
            !store.path().join("acme-tools").exists(),
            "no partial install directory may be visible"
        );
        assert!(
            !store.path().join(".acme-tools.install-tmp").exists(),
            "the staging directory must be cleaned up on failure"
        );
    }

    #[tokio::test]
    async fn too_many_files_is_refused_before_any_network_call() {
        let store = tempfile::tempdir().expect("store tempdir");
        let files: Vec<(String, String)> = (0..MAX_FILES_PER_PLUGIN + 1)
            .map(|i| {
                (
                    format!("f{i}.txt"),
                    "http://127.0.0.1:1/unreachable".to_string(),
                )
            })
            .collect();
        let e = MarketplacePluginEntry {
            id: "acme-tools".to_string(),
            name: String::new(),
            description: String::new(),
            version: String::new(),
            source: None,
            files: files.into_iter().collect(),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            install_entry("http://marketplace.example/mp.json", &e, store.path()),
        )
        .await
        .expect("must be refused immediately, never attempt the network");
        let err = result.expect_err("too many files");
        assert_eq!(err.kind(), "too_many_files");
    }

    #[tokio::test]
    async fn no_files_is_refused() {
        let store = tempfile::tempdir().expect("store tempdir");
        let e = entry("acme-tools", &[]);
        let err = install_entry("http://marketplace.example/mp.json", &e, store.path())
            .await
            .expect_err("no files");
        assert_eq!(err.kind(), "no_files");
    }

    /// Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`, acceptance 4: an archive-
    /// requiring source kind is refused BY NAME before anything is written
    /// -- no stub `git` needed, since [`crate::git_source::fetch_git_source`]
    /// refuses before ever invoking a subprocess (this module's own updated
    /// doc: two fetch paths, chosen by what `entry` declares).
    #[tokio::test]
    async fn a_source_entry_naming_an_unsupported_kind_is_refused_and_writes_nothing() {
        let store = tempfile::tempdir().expect("store tempdir");
        let e = MarketplacePluginEntry {
            id: String::new(),
            name: "archived-thing".to_string(),
            description: String::new(),
            version: String::new(),
            source: Some(crate::manifest::PluginSource::Unsupported {
                kind: "url".to_string(),
            }),
            files: BTreeMap::new(),
        };
        let err = install_entry("http://marketplace.example/mp.json", &e, store.path())
            .await
            .expect_err("archive-requiring source kinds are refused");
        assert_eq!(err.kind(), "unsupported_source_kind");
        assert!(
            std::fs::read_dir(store.path()).unwrap().next().is_none(),
            "nothing may be written for a source kind conway cannot fetch"
        );
    }

    /// An entry with neither `id` nor `name` is refused before anything is
    /// written -- [`MarketplacePluginEntry::identity`]'s own `None` case.
    #[tokio::test]
    async fn an_entry_with_no_identity_is_refused() {
        let store = tempfile::tempdir().expect("store tempdir");
        let e = MarketplacePluginEntry {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            version: String::new(),
            source: None,
            files: BTreeMap::new(),
        };
        let err = install_entry("http://marketplace.example/mp.json", &e, store.path())
            .await
            .expect_err("no identity");
        assert_eq!(err.kind(), "missing_identity");
    }

    // -----------------------------------------------------------------
    // Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`, layer 4: a `source` entry
    // actually fetched -- via a STUB `git` (a plain shell script, written
    // fresh into a temp dir, the same fixture-script technique
    // `conway-plugin-mcp`/`conway-plugin-subprocess`'s own `tests/common/
    // mod.rs::write_script` already use), never a real network host or a
    // dependency on whether THIS machine has real `git` at all (P-15). The
    // stub ignores the URL it is given entirely -- it exists to prove this
    // module's own orchestration (subdir resolution, the checkout-to-
    // staging copy, cleanup), not `git` itself, which this crate does not
    // own and will never re-test.
    // -----------------------------------------------------------------

    /// Writes a stub `git` that answers `--version` and `clone ... <url>
    /// <dest>` by ignoring every argument except the LAST one (`<dest>`,
    /// always the final positional argument in `crate::git_source::
    /// run_git_clone`'s own invocation) -- populating a fixed tree rather
    /// than actually cloning anything.
    #[cfg(unix)]
    fn write_stub_git(dir: &std::path::Path) -> std::path::PathBuf {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join("git");
        let mut f = std::fs::File::create(&path).expect("create stub git");
        f.write_all(
            br#"#!/bin/sh
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
  mkdir -p "$dest/plugin/.claude-plugin"
  echo '{"name":"stub"}' > "$dest/plugin/.claude-plugin/plugin.json"
  echo 'root' > "$dest/root-file.txt"
  exit 0
fi
exit 1
"#,
        )
        .expect("write stub git");
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x stub git");
        path
    }

    /// Points [`crate::git_source::GIT_PROGRAM_ENV`] at `program` for the
    /// duration of `body` -- a thin, path-typed wrapper over
    /// [`crate::git_source::test_support::with_program`], which is what
    /// actually serializes this against `git_source.rs`'s OWN test doing
    /// the identical thing to the identical process-global env var (that
    /// module's own doc has the full argument for why this cannot be a
    /// bare save/restore).
    #[cfg(unix)]
    async fn with_stub_git<F, Fut, T>(program: &std::path::Path, body: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        crate::git_source::test_support::with_program(program.as_os_str(), body).await
    }

    /// A `git-subdir` entry installs ONLY its own declared subdirectory --
    /// `root-file.txt`, which the stub also writes at the checkout root but
    /// outside `plugin/`, must never appear in the installed directory.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_git_subdir_entry_installs_only_its_own_subdirectory() {
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let git = write_stub_git(bin_dir.path());
        let store = tempfile::tempdir().expect("store tempdir");

        let e = MarketplacePluginEntry {
            id: String::new(),
            name: "beepboop".to_string(),
            description: "plays sounds".to_string(),
            version: "1.4.0".to_string(),
            source: Some(crate::manifest::PluginSource::GitSubdir {
                url: "https://github.com/devnill/beepboop".to_string(),
                path: "plugin".to_string(),
            }),
            files: BTreeMap::new(),
        };

        let installed = with_stub_git(&git, || {
            install_entry("https://example.com/marketplace.json", &e, store.path())
        })
        .await
        .expect("install via stub git");

        assert_eq!(installed.id, "beepboop");
        assert!(installed.dir.join(".claude-plugin/plugin.json").is_file());
        assert!(
            !installed.dir.join("root-file.txt").exists(),
            "content outside the declared subdirectory must never be installed"
        );
        // No leftover checkout or staging directory.
        assert!(!store.path().join(".beepboop.git-checkout-tmp").exists());
        assert!(!store.path().join(".beepboop.install-tmp").exists());
    }

    /// A `github` entry (no subdirectory) installs the WHOLE checkout root.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_github_entry_with_no_subdirectory_installs_the_whole_checkout() {
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let git = write_stub_git(bin_dir.path());
        let store = tempfile::tempdir().expect("store tempdir");

        let e = MarketplacePluginEntry {
            id: String::new(),
            name: "outpost".to_string(),
            description: String::new(),
            version: "0.2.0".to_string(),
            source: Some(crate::manifest::PluginSource::Github {
                repo: "devnill/outpost".to_string(),
            }),
            files: BTreeMap::new(),
        };

        let installed = with_stub_git(&git, || {
            install_entry("https://example.com/marketplace.json", &e, store.path())
        })
        .await
        .expect("install via stub git");

        assert_eq!(installed.id, "outpost");
        assert!(installed.dir.join("plugin/.claude-plugin/plugin.json").is_file());
        assert!(
            installed.dir.join("root-file.txt").is_file(),
            "a `github` source has no subdirectory -- the whole checkout root installs"
        );
    }

    #[tokio::test]
    async fn uninstalling_removes_the_plugin_directory() {
        let store = tempfile::tempdir().expect("store tempdir");
        std::fs::create_dir_all(store.path().join("acme-tools")).unwrap();
        std::fs::write(store.path().join("acme-tools").join("f.txt"), "x").unwrap();

        let removed = uninstall_plugin("acme-tools", store.path()).expect("uninstall");
        assert!(removed);
        assert!(!store.path().join("acme-tools").exists());
    }

    #[tokio::test]
    async fn uninstalling_an_absent_plugin_is_a_no_op_not_an_error() {
        let store = tempfile::tempdir().expect("store tempdir");
        let removed = uninstall_plugin("never-installed", store.path()).expect("no-op");
        assert!(!removed);
    }

    #[test]
    fn uninstalling_an_unsafe_id_is_refused() {
        let store = tempfile::tempdir().expect("store tempdir");
        let err = uninstall_plugin("../../etc", store.path()).expect_err("unsafe id");
        assert_eq!(err.kind(), "unsafe_plugin_id");
    }
}
