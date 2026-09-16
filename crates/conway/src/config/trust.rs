//! Recording an explicit, content-scoped trust decision for a
//! project-scoped `.conway/permissions.json`, per D4
//! (the trust-model design) and
//!.
//!
//! ## Why this exists
//!
//! A project's `.conway/permissions.json` is authored by whoever controls
//! the checkout -- which, for a cloned repository, is not the operator.
//! Its `allow` half is authority (`conway_core::permission_pattern`'s own
//! module doc), so installing it with no consent lets a clone auto-grant
//! itself pattern-based tool permissions the moment the operator opens the
//! session. This module is the record that consent was actually given,
//! and to WHAT BYTES -- never to a directory.
//!
//! ## The trust subject: `(absolute path, content digest)`, never a directory
//!
//! Trusting `/repo/.conway/permissions.json` records the digest of the
//! bytes trusted. A subsequent edit to that file -- a `git pull`, a hand
//! edit, anything -- changes the digest, which SILENTLY de-trusts the
//! file: [`TrustStore::is_trusted`] simply stops matching. There is no
//! "trust this folder" operation, and no code path here can produce one.
//! This is the fix for the sticky, directory-scoped trust flaw D4 §5
//! documents (Claude Code's own design): an edit to trusted content never
//! rides a decision made about different bytes.
//!
//! ## Narrower than D4's full design, and why that is safe
//!
//! D4 §4 specifies `(kind, id, digest)` subjects nested under a project
//! key, with `kind` spanning `plugin` and `permission_file`. Plugins do
//! not exist in this tree yet (D1/D2 are still design-stage), so this
//! module implements exactly one kind -- `permission_file`, keyed
//! directly on the file's own absolute path (which already disambiguates
//! by project: a file's path IS project-scoped) -- and flattens away the
//! `kind` tag and the `projects` nesting layer that would only ever hold
//! one key today. Adding a `plugin` kind later is a new top-level map
//! alongside `permission_files` in `TrustFile`, not a redesign of this
//! module: the two load-bearing properties D4 cares about -- per-subject
//! granularity and digest-not-directory -- are already exactly what it
//! specifies.
//!
//! ## Global vs project (D4 §4)
//!
//! This module is consulted for PROJECT-scoped files only. The operator's
//! own global file (`<config_dir>/permissions.json`) is trusted by authorship --
//! asking an operator to trust their own file is theater that teaches
//! people to click through prompts. `conway-cli`'s startup loader
//! (`tui/app.rs`) is the caller that makes this split concrete: it never
//! calls into this module for the global path.
//!
//! ## No startup prompt (D4 §5, §9)
//!
//! An untrusted project file is silently skipped (its `allow` half; its
//! `deny` half still applies immediately -- see
//! `conway_core::permission_pattern`'s module doc for that asymmetry) and
//! the session starts, degraded, with one transcript notice. There is no
//! modal here, at startup or ever: D4 §5's argument is that a prompt
//! firing on every `git pull` trains an operator to press `y`, which
//! makes the prompt a latency tax rather than a control. The only path
//! that WRITES a trust record is an operator action taken on purpose (the
//! TUI's `/trust permissions` command, wired in `conway-cli::tui::app`) --
//! never automatic, never a side effect of starting a session.
//!
//! ## Failure posture: the trust file is untrusted input
//!
//! Every failure mode here yields FEWER trusted subjects, never more:
//! - `trust.json` missing -> every project file is untrusted (empty store).
//! - `trust.json` unreadable or not valid JSON -> treated as empty, with a
//!   loud diagnostic (`tracing::error!`) -- never partially applied.
//! - (unix only) `trust.json` is group- or world-writable -> refused and
//!   treated as unreadable, the same way `ssh` refuses a loose private
//!   key. A no-op on non-unix hosts, matching `conway_tools::bash`'s own
//!   `#[cfg(not(unix))]` precedent (no new dependency for this --
//!   `std::os::unix::fs::PermissionsExt` is already in `std`).
//! - a digest computed from the file on disk right now mismatches the
//!   recorded digest -> untrusted. Mirrors how `Containment::Undecidable`
//!   is fused with `Outside` everywhere in this codebase that consults
//!   containment: "can't confirm" is never "trusted".
//!
//! ## Deliberately NOT `#[serde(deny_unknown_fields)]`
//!
//! `conway_core::permission_pattern`'s internal `RawPermissionFile` gained
//! `deny_unknown_fields` under this item because `permissions.json` is a
//! HAND-AUTHORED file where a typo'd key (`"denys"` for `"deny"`) silently
//! drops a safety rule the operator believes is in effect -- the fail-
//! closed floor. `TrustFile`/`TrustedRecord` are a different kind of
//! file entirely: nobody types a key into `trust.json` by hand. It is
//! written exclusively by [`TrustStore::trust`] and read back exclusively
//! by [`TrustStore::load`] -- both this crate, across whatever two conway
//! builds an operator happens to run before and after an upgrade. That
//! makes its realistic failure mode VERSION SKEW, not a typo: a future
//! build adds a field to `TrustedRecord` (say, a digest algorithm tag),
//! and an OLDER build reads that file back. Under `deny_unknown_fields`
//! that read becomes `Err`, and `TrustStore::load_from_path` already
//! treats any parse error as "trust.json is corrupt" -- which zeroes EVERY
//! recorded trust decision in the file, not just the one entry with the
//! new field. An operator who trusted ten projects would have to re-run
//! `/trust permissions` on all ten after a mere downgrade, for a field
//! that has nothing to do with any of their decisions.
//!
//! That regression has no offsetting safety benefit the way `permissions.json`'s
//! does: an untrusted-by-mistake record fails in the SAME direction this
//! module's whole failure posture already takes on purpose (fewer trusted
//! subjects, never more -- see above) -- it degrades to more prompting, it
//! does not let anything unenforced through the way a silently-dropped
//! `deny` rule does. `deny_unknown_fields`'s value is catching a HUMAN
//! typo before it causes a silent security gap; there is no human typing
//! keys into `trust.json` for it to catch, so the same attribute here would
//! only add the version-skew cost above for no matching benefit. This
//! module stays lenient, and `an_unrecognized_key_in_trust_json_does_not_
//! prevent_a_recorded_decision_from_matching` (this module's own test
//! suite) pins that the leniency actually holds, not just that it was
//! decided.
//!
//! ## A second kind: project `settings.json` (board item `01M2M5EM73GA15NMQ1H87TTEDP`)
//!
//! Ruled 2026-09-16: a project `settings.json` reached by `discovery::discover`'s
//! upward walk gets the SAME consent gate `permissions.json` already has
//! here -- reusing the exact `(absolute
//! path, content digest)` subject shape `TrustedRecord` already defines,
//! not a redesign. `TrustFile::settings_files` is a second, independent
//! map alongside `permission_files`: trusting a project's `permissions.json`
//! says nothing about that same project's `settings.json`, and vice versa
//! -- two different files, two different authorities, two different
//! records (`trust_does_not_leak_across_kinds_for_the_same_path`, below,
//! pins this explicitly).
//!
//! **Why `settings.json` gets a STRICTER posture than `permissions.json`
//! ever has** (the ruling's own reasoning, restated here because it
//! changes this module's behavior, not just its data shape): a
//! `permissions.json`'s `allow` half is authority over what an agent may
//! DO; an untrusted one degrades silently (this module's own "No startup
//! prompt" section above) because the floor -- `deny` rules always apply,
//! trusted or not -- never actually widens by staying silent. A
//! `settings.json` can set `backends.<id>.base_url`/`api_key`: installing
//! one with no consent does not merely widen what an agent may call, it
//! can REDIRECT the operator's own traffic and credentials to an endpoint
//! chosen by whoever controls the directory. There is no floor under that
//! the way `deny` is a floor under permissions -- so this module's public
//! entry point for the settings kind, [`guard_untrusted_project_settings`],
//! does not silently degrade the way permission-file loading does. It
//! REFUSES (a named error, see that function's own doc), and it is the
//! caller's job to either already hold a trust record (an interactive
//! session that already prompted and the operator accepted) or accept
//! that refusal -- never a silent apply, never a silent skip.
//!
//! **Scope: the PROJECT walk only, never the operator's own user layer.**
//! [`guard_untrusted_project_settings`] calls [`super::discovery::discover`]
//! itself (with `project_discovery_exclusions`, the
//! same exclusion every other project-layer consumer in this crate
//! applies) -- it has no opinion about, and never even looks at,
//! `super::discovery::user_config_path`'s own file. An operator's own
//! `$CONWAY_CONFIG_DIR/settings.json` (or `~/.conway/settings.json`) is
//! trusted by authorship, identically to how `permissions.json`'s own
//! "Global vs project" section above already draws that line -- this item
//! extends the SAME line to the new kind, not a new one.
//!
//! **Also out of scope: `--config <path>`.**
//! `guard_untrusted_project_settings` is never consulted for an
//! `explicit_path`-supplied config --
//! see [`crate::builder::ConwayBuilder::discover`]'s own doc for the one
//! production call site, which only ever calls this function on the
//! no-`--config` branch. An operator who names a config file directly on
//! the command line asked for exactly that file, explicitly, every time --
//! the same reasoning `docs/getting-started.md`'s own discovery section
//! already gives for why `--config` bypasses the walk entirely.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The on-disk shape of `<config_dir>/trust.json`. `permission_files` maps a
/// path's `Display` string (JSON object keys must be strings; `PathBuf`
/// has no portable string-key `Serialize` of its own) to the record of
/// what was last explicitly trusted at that path.
///
/// Deliberately no `#[serde(deny_unknown_fields)]` -- see this module's own
/// doc, "Deliberately NOT `#[serde(deny_unknown_fields)]`", for why this
/// file's realistic risk is version skew between two conway builds, not an
/// operator's typo, and why the same attribute that protects
/// `permissions.json` would only add cost here.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct TrustFile {
    #[serde(default)]
    permission_files: HashMap<String, TrustedRecord>,
    /// The second kind (board item `01M2M5EM73GA15NMQ1H87TTEDP`) -- see
    /// this module's own "A second kind" doc. Independent of
    /// `permission_files` above: the two maps are never consulted for each
    /// other's kind, even for the identical path.
    #[serde(default)]
    settings_files: HashMap<String, TrustedRecord>,
}

/// One explicit trust decision, recorded at the moment the operator made
/// it. `content_digest` is what makes this granular rather than sticky --
/// see this module's own doc. Deliberately no `#[serde(deny_unknown_fields)]`
/// either -- same reasoning as [`TrustFile`] itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct TrustedRecord {
    content_digest: String,
    trusted_at: String,
}

/// The trust record for project-scoped `permissions.json` files, loaded
/// once at startup. See this module's own doc for the digest-not-directory
/// design and the fail-closed posture.
#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    file: TrustFile,
}

/// [`TrustStore::status`]'s finer answer than [`TrustStore::is_trusted`]'s
/// plain boolean: a caller deciding what to SHOW an operator before a trust
/// decision (board item, the trust-preview surface) needs to know not just
/// "is this trusted" but "is there a prior record AT ALL" -- `is_trusted`
/// collapses `New` and `Changed` into the same `false`, which is exactly
/// right for the gating question `load_permission_files`/`is_trusted` ask
/// ("does the `allow` half install") but wrong for a preview's wording
/// ("trusting for the first time" reads very differently from "this file
/// changed since you trusted it").
///
/// Deliberately carries no content of its own: this store never retains
/// the bytes of a PRIOR trust decision (see this module's own doc,
/// `TrustedRecord`) -- `Changed` says a prior record existed and no
/// longer matches, not what it used to say. A caller wanting to show
/// "what changed" cannot, from this store alone; see
/// `conway_cli::tui::view::draw_trust_preview`'s own doc for how the
/// preview surface states that limit plainly rather than implying a diff
/// that cannot be produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustStatus {
    /// No record exists for this path at all -- this would be the first
    /// time it is trusted.
    New,
    /// A record exists for this path, but its digest does not match the
    /// bytes just read -- the file changed since it was last trusted.
    Changed,
    /// A record exists and its digest matches the bytes just read --
    /// already trusted, unchanged.
    Unchanged,
}

/// `blake3` is already a workspace dependency (`conway-core`,
/// `conway-runtime`'s `PermissionBroker::CacheKey` both use it) -- reused
/// here rather than reaching for a new dependency.
fn content_digest(contents: &str) -> String {
    format!("blake3:{}", blake3::hash(contents.as_bytes()).to_hex())
}

fn path_key(path: &Path) -> String {
    path.display().to_string()
}

impl TrustStore {
    /// The one global-only location, alongside every other user-scoped
    /// file this crate resolves (`user_config_path`, `history_file_path`) --
    /// a third consumer of machinery that already exists, not a new
    /// discovery paradigm. Deliberately global-only: a project-scoped
    /// trust file would let untrusted content trust itself (D4 §4).
    pub fn path(env: &HashMap<String, String>) -> Option<PathBuf> {
        super::discovery::user_config_path(env)
            .and_then(|settings| settings.parent().map(|dir| dir.join("trust.json")))
    }

    /// Loads the trust record, failing closed on every error path, since the
    /// file is untrusted input: a missing, unreadable, corrupt, or (on unix)
    /// loosely-permissioned file all produce an EMPTY store, which trusts
    /// nothing.
    pub fn load(env: &HashMap<String, String>) -> Self {
        match Self::path(env) {
            Some(path) => Self::load_from_path(&path),
            None => Self::default(),
        }
    }

    fn load_from_path(path: &Path) -> Self {
        if !Self::permissions_are_safe(path) {
            tracing::error!(
                path = %path.display(),
                "trust.json is group- or world-writable; refusing to read it -- \
                 treating every project permission file as untrusted until this \
                 is fixed (chmod 600), the same posture ssh takes with a loose \
                 private key"
            );
            return Self::default();
        }
        let Ok(contents) = std::fs::read_to_string(path) else {
            // Missing (the common case) or otherwise unreadable: both fail
            // closed identically, and a missing file is not worth logging.
            return Self::default();
        };
        match serde_json::from_str::<TrustFile>(&contents) {
            Ok(file) => Self { file },
            Err(err) => {
                tracing::error!(
                    path = %path.display(),
                    error = %err,
                    "trust.json is corrupt; treating it as empty -- every project \
                     permission file is untrusted until this is fixed"
                );
                Self::default()
            }
        }
    }

    #[cfg(unix)]
    fn permissions_are_safe(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            // A file that does not exist yet imposes no permission risk;
            // `load_from_path`'s subsequent `read_to_string` handles
            // "missing" as its own (also fail-closed) case.
            Err(_) => true,
            Ok(meta) => meta.permissions().mode() & 0o022 == 0,
        }
    }

    #[cfg(not(unix))]
    fn permissions_are_safe(_path: &Path) -> bool {
        true
    }

    /// Whether `contents` (the bytes actually read from `abs_path` at
    /// startup) match a digest this store recorded for that exact path.
    /// The caller is responsible for resolving `abs_path` the same way on
    /// every call -- `conway_cli::tui::app`'s loader passes the same
    /// project-scoped candidate `permission_file_paths` already produced.
    pub fn is_trusted(&self, abs_path: &Path, contents: &str) -> bool {
        self.status(abs_path, contents) == TrustStatus::Unchanged
    }

    /// The finer-grained answer [`is_trusted`](Self::is_trusted) collapses
    /// into a single `bool` -- see [`TrustStatus`]'s own doc for why a
    /// preview surface needs the distinction and why this store cannot go
    /// any further than it (no prior content is retained to diff against).
    pub fn status(&self, abs_path: &Path, contents: &str) -> TrustStatus {
        match self.file.permission_files.get(&path_key(abs_path)) {
            None => TrustStatus::New,
            Some(record) if record.content_digest == content_digest(contents) => {
                TrustStatus::Unchanged
            }
            Some(_) => TrustStatus::Changed,
        }
    }

    /// [`Self::is_trusted`]'s exact counterpart for the `settings.json`
    /// kind (board item `01M2M5EM73GA15NMQ1H87TTEDP`) -- see this module's
    /// own "A second kind" doc. Consults `TrustFile::settings_files`, never
    /// `permission_files`.
    pub fn is_settings_trusted(&self, abs_path: &Path, contents: &str) -> bool {
        self.settings_status(abs_path, contents) == TrustStatus::Unchanged
    }

    /// [`Self::status`]'s exact counterpart for the `settings.json` kind --
    /// same three-way `New`/`Changed`/`Unchanged` answer, over
    /// `TrustFile::settings_files` instead of `permission_files`.
    pub fn settings_status(&self, abs_path: &Path, contents: &str) -> TrustStatus {
        match self.file.settings_files.get(&path_key(abs_path)) {
            None => TrustStatus::New,
            Some(record) if record.content_digest == content_digest(contents) => {
                TrustStatus::Unchanged
            }
            Some(_) => TrustStatus::Changed,
        }
    }

    /// Records an explicit trust decision for `abs_path`'s CURRENT bytes
    /// on disk, then writes the store back out via
    /// `super::writer::write_atomically` (tmp-then-rename, the same
    /// durability step `tui/history.rs`'s and
    /// `tui/app.rs::persist_permission_rule`'s own precedent already uses,
    /// so a crash mid-write cannot corrupt the file -- see that function's
    /// own doc for exactly what this does and does not guarantee).
    ///
    /// Returns an error rather than silently no-op-ing: unlike a
    /// permission-RULE write (which is best-effort because the live
    /// session already has the grant either way), a FAILED trust write
    /// must be visible to the caller, because it means the trust decision
    /// the operator just made did not persist and the next launch will
    /// re-degrade with no explanation if this is swallowed.
    pub fn trust(env: &HashMap<String, String>, abs_path: &Path) -> std::io::Result<()> {
        let path = Self::path(env).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no resolvable global config directory to write trust.json into",
            )
        })?;
        let contents = std::fs::read_to_string(abs_path)?;
        let mut store = Self::load_from_path(&path);
        store.file.permission_files.insert(
            path_key(abs_path),
            TrustedRecord {
                content_digest: content_digest(&contents),
                trusted_at: chrono::Utc::now().to_rfc3339(),
            },
        );
        let serialized = serde_json::to_string_pretty(&store.file)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        super::writer::write_atomically(&path, &serialized)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Best-effort: the write above already succeeded, and a
            // failure to tighten permissions afterward must not undo a
            // trust decision the operator already made.
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// [`Self::trust`]'s exact counterpart for the `settings.json` kind
    /// (board item `01M2M5EM73GA15NMQ1H87TTEDP`) -- records the CURRENT
    /// bytes at `abs_path` into `TrustFile::settings_files`, same digest/
    /// durability/permission-tightening contract as `trust`, over the
    /// independent map. This is the ONE path that makes
    /// [`guard_untrusted_project_settings`] stop refusing a given
    /// `settings.json`: an operator action taken on purpose, never a side
    /// effect of loading config.
    pub fn trust_settings(env: &HashMap<String, String>, abs_path: &Path) -> std::io::Result<()> {
        let path = Self::path(env).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no resolvable global config directory to write trust.json into",
            )
        })?;
        let contents = std::fs::read_to_string(abs_path)?;
        let mut store = Self::load_from_path(&path);
        store.file.settings_files.insert(
            path_key(abs_path),
            TrustedRecord {
                content_digest: content_digest(&contents),
                trusted_at: chrono::Utc::now().to_rfc3339(),
            },
        );
        let serialized = serde_json::to_string_pretty(&store.file)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        super::writer::write_atomically(&path, &serialized)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

/// The named refusal [`guard_untrusted_project_settings`] returns: a
/// walk-discovered project `settings.json` exists at `path` and is not
/// (yet) trusted. `Display` is the message an operator sees verbatim --
/// names the exact file and the one action ([`TrustStore::trust_settings`],
/// surfaced through a `/trust settings`-shaped operator action) that turns
/// this into a successful load next time.
///
/// A distinct type, not a bare `String`, so a caller CAN pattern-match on
/// it (`FacadeError::UntrustedProjectSettings`'s own doc: an interactive
/// caller catches exactly this to decide whether to prompt-then-retry,
/// rather than string-matching an error message) -- the "named error"
/// this board item's own spec asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UntrustedProjectSettings {
    pub path: PathBuf,
}

impl std::fmt::Display for UntrustedProjectSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "untrusted project settings.json at {} -- a project-scoped settings.json \
             can redirect a backend's base_url/api_key, so it is never applied without \
             explicit consent. Review its contents, then trust it (the TUI's `/trust \
             settings` command, or `conway::config::trust::TrustStore::trust_settings` \
             for an embedder) before this config will load.",
            self.path.display()
        )
    }
}

/// The consent gate itself (board item `01M2M5EM73GA15NMQ1H87TTEDP`): walks
/// from `cwd` (exactly [`super::discovery::discover`], with the SAME
/// `project_discovery_exclusions` every other
/// project-layer consumer in this crate applies -- the operator's own user
/// layer is never a candidate, see this module's own "A second kind" doc)
/// looking for a project `settings.json`. Three outcomes:
///
/// - No project `settings.json` reachable at all -> `Ok(())` (nothing to
///   gate).
/// - One is reachable and [`TrustStore::is_settings_trusted`] confirms its
///   CURRENT bytes match a recorded decision -> `Ok(())` (trusted, proceed
///   to merge it).
/// - One is reachable and is untrusted (no record, or the recorded digest
///   no longer matches -- an edit since the operator last trusted it) ->
///   `Err(UntrustedProjectSettings)`, naming the exact path.
///
/// **Never silently skips.** A caller that receives `Err` here must not
/// proceed to merge the file's contents anyway (that would be the silent-
/// apply this item's own ruling forbids) NOR proceed as if no project
/// layer existed at all (silent-skip, forbidden identically) -- the only
/// correct responses are: refuse outright (the non-interactive case), or
/// prompt the operator and, on acceptance, call
/// [`TrustStore::trust_settings`] and call this function again (now `Ok`)
/// before proceeding. This function itself has no interactivity of its own -- it
/// is a synchronous, pure-of-I/O-side-effects (reads only) check, so
/// EVERY caller gets the exact same "refuse" behavior unless it has
/// already arranged consent; see [`crate::builder::ConwayBuilder::discover`]
/// for the one production caller and its own disclosure of where the
/// interactive half would need to live.
///
/// A project `settings.json` this function cannot even READ (permission
/// error, TOCTOU-vanished between the walk and this read) is treated as
/// `Ok(())` here -- not this function's failure to diagnose;
/// `config::merge::load`'s own subsequent read of the same path (`load_impl`'s
/// project-layer step) will raise the real, specific I/O error for that
/// case, and this function raising a DIFFERENT, misleading "untrusted"
/// error for a file it could not even inspect would be worse than letting
/// the real error surface downstream.
pub fn guard_untrusted_project_settings(
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<(), UntrustedProjectSettings> {
    let Some(path) =
        super::discovery::discover(cwd, &super::discovery::project_discovery_exclusions(env))
    else {
        return Ok(());
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let store = TrustStore::load(env);
    match store.settings_status(&path, &contents) {
        TrustStatus::Unchanged => Ok(()),
        TrustStatus::New | TrustStatus::Changed => Err(UntrustedProjectSettings { path }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "conway-trust-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn env_for(config_dir: &Path) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            config_dir.display().to_string(),
        );
        env
    }

    #[test]
    fn an_untrusted_file_is_untrusted_by_default() {
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let store = TrustStore::load(&env);
        let project = tempfile_dir().join("permissions.json");
        fs::write(&project, r#"{"allow":["bash:*"]}"#).unwrap();
        let contents = fs::read_to_string(&project).unwrap();
        assert!(!store.is_trusted(&project, &contents));
    }

    #[test]
    fn trusting_a_file_makes_is_trusted_true_for_its_current_bytes() {
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let project = tempfile_dir().join("permissions.json");
        fs::write(&project, r#"{"allow":["bash:cargo test"]}"#).unwrap();

        TrustStore::trust(&env, &project).expect("trust succeeds");

        let store = TrustStore::load(&env);
        let contents = fs::read_to_string(&project).unwrap();
        assert!(store.is_trusted(&project, &contents));
    }

    /// The headline property: editing trusted content SILENTLY de-trusts
    /// it. No modal, no special-cased error -- `is_trusted` simply stops
    /// matching, because the recorded digest is of the OLD bytes.
    #[test]
    fn editing_a_trusted_files_content_de_trusts_it() {
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let project = tempfile_dir().join("permissions.json");
        fs::write(&project, r#"{"allow":["bash:cargo test"]}"#).unwrap();
        TrustStore::trust(&env, &project).expect("trust succeeds");

        // A hostile (or merely later) edit changes the bytes.
        fs::write(&project, r#"{"allow":["bash:cargo test","bash:curl"]}"#).unwrap();

        let store = TrustStore::load(&env);
        let contents = fs::read_to_string(&project).unwrap();
        assert!(
            !store.is_trusted(&project, &contents),
            "a content change must de-trust silently -- no directory-scoped \
             stickiness"
        );
    }

    /// Trust is per-path: trusting one project's file says nothing about
    /// another's, even with byte-identical content.
    #[test]
    fn trust_does_not_leak_across_paths() {
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let project_a = tempfile_dir().join("permissions.json");
        let project_b = tempfile_dir().join("permissions.json");
        let contents = r#"{"allow":["bash:cargo test"]}"#;
        fs::write(&project_a, contents).unwrap();
        fs::write(&project_b, contents).unwrap();

        TrustStore::trust(&env, &project_a).expect("trust succeeds");

        let store = TrustStore::load(&env);
        assert!(store.is_trusted(&project_a, contents));
        assert!(
            !store.is_trusted(&project_b, contents),
            "trusting one project's file must not trust an identical file \
             at a different path"
        );
    }

    /// A missing `trust.json` trusts nothing.
    #[test]
    fn a_missing_trust_file_trusts_nothing() {
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let store = TrustStore::load(&env);
        assert!(!store.is_trusted(Path::new("/does/not/matter"), "anything"));
    }

    /// A corrupt `trust.json` is treated as empty, never partially
    /// applied and never a panic.
    #[test]
    fn a_corrupt_trust_file_is_treated_as_empty() {
        let config_dir = tempfile_dir();
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("trust.json"), "not json at all").unwrap();
        let env = env_for(&config_dir);

        let store = TrustStore::load(&env);
        assert!(!store.is_trusted(Path::new("/anything"), "anything"));
    }

    /// From a later item, question 3: pins the
    /// deliberate DIFFERENCE from `permissions.json`'s treatment, recorded
    /// in this module's own doc -- an unrecognized key here (at either
    /// nesting level: a stray top-level field, or a stray field inside one
    /// recorded entry) must NOT prevent an otherwise-valid, already-
    /// recorded trust decision from matching. `trust.json` is written and
    /// read exclusively by this module across whatever two conway builds an
    /// operator happens to run, so a field one build does not recognize is
    /// version skew, not a human's typo -- and must not force every
    /// project the operator already trusted back into "untrusted" the way
    /// `deny_unknown_fields` would (see the module doc for the full
    /// reasoning this test exists to keep honest).
    #[test]
    fn an_unrecognized_key_in_trust_json_does_not_prevent_a_recorded_decision_from_matching() {
        let config_dir = tempfile_dir();
        fs::create_dir_all(&config_dir).unwrap();
        let project = tempfile_dir().join("permissions.json");
        let contents = r#"{"allow":["bash:cargo test"]}"#;
        fs::write(&project, contents).unwrap();
        let digest = content_digest(contents);
        let trust_json = format!(
            r#"{{
                "a_future_top_level_field_this_build_does_not_know": "ignored",
                "permission_files": {{
                    {:?}: {{
                        "content_digest": {:?},
                        "trusted_at": "2024-01-01T00:00:00Z",
                        "a_future_record_field_this_build_does_not_know": "also ignored"
                    }}
                }}
            }}"#,
            project.display().to_string(),
            digest,
        );
        fs::write(config_dir.join("trust.json"), trust_json).unwrap();
        let env = env_for(&config_dir);

        let store = TrustStore::load(&env);
        assert!(
            store.is_trusted(&project, contents),
            "a field this build does not recognize -- at either nesting \
             level -- must not prevent an otherwise-valid recorded trust \
             decision from matching"
        );
    }

    /// [`TrustStatus`]'s three cases -- the finer distinction the
    /// trust-preview surface needs and `is_trusted` alone cannot give it
    /// (board item: a preview must say "first time" versus "this changed"
    /// rather than a bare "not trusted").
    #[test]
    fn status_distinguishes_new_changed_and_unchanged() {
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let project = tempfile_dir().join("permissions.json");
        fs::write(&project, r#"{"allow":["bash:cargo test"]}"#).unwrap();

        // No record at all yet.
        let store = TrustStore::load(&env);
        let contents = fs::read_to_string(&project).unwrap();
        assert_eq!(store.status(&project, &contents), TrustStatus::New);

        TrustStore::trust(&env, &project).expect("trust succeeds");

        // Freshly trusted, unchanged since.
        let store = TrustStore::load(&env);
        assert_eq!(store.status(&project, &contents), TrustStatus::Unchanged);

        // Edited after being trusted.
        fs::write(&project, r#"{"allow":["bash:cargo test","bash:curl"]}"#).unwrap();
        let new_contents = fs::read_to_string(&project).unwrap();
        let store = TrustStore::load(&env);
        assert_eq!(store.status(&project, &new_contents), TrustStatus::Changed);
        // is_trusted must agree exactly with the Unchanged/not-Unchanged
        // split -- this is the property the refactor (`is_trusted` now
        // delegates to `status`) must preserve byte-for-byte: the OLD
        // (recorded) bytes still read as trusted, the NEW (on-disk) bytes
        // do not.
        assert!(store.is_trusted(&project, &contents));
        assert!(!store.is_trusted(&project, &new_contents));
    }

    #[cfg(unix)]
    #[test]
    fn a_world_writable_trust_file_is_refused_like_a_loose_ssh_key() {
        use std::os::unix::fs::PermissionsExt;
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let project = tempfile_dir().join("permissions.json");
        fs::write(&project, r#"{"allow":["bash:cargo test"]}"#).unwrap();
        TrustStore::trust(&env, &project).expect("trust succeeds");

        let trust_path = TrustStore::path(&env).unwrap();
        fs::set_permissions(&trust_path, fs::Permissions::from_mode(0o666)).unwrap();

        let store = TrustStore::load(&env);
        let contents = fs::read_to_string(&project).unwrap();
        assert!(
            !store.is_trusted(&project, &contents),
            "a world-writable trust.json must be refused, not trusted"
        );
    }

    // ---------------------------------------------------------------
    // Board item 01M2M5EM73GA15NMQ1H87TTEDP: the settings.json kind.
    // ---------------------------------------------------------------

    #[test]
    fn an_untrusted_settings_file_is_untrusted_by_default() {
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let project = tempfile_dir().join("settings.json");
        fs::write(&project, r#"{"default_role":"coder"}"#).unwrap();
        let contents = fs::read_to_string(&project).unwrap();

        let store = TrustStore::load(&env);
        assert!(!store.is_settings_trusted(&project, &contents));
    }

    #[test]
    fn trusting_a_settings_file_makes_is_settings_trusted_true_for_its_current_bytes() {
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let project = tempfile_dir().join("settings.json");
        fs::write(&project, r#"{"default_role":"coder"}"#).unwrap();

        TrustStore::trust_settings(&env, &project).expect("trust_settings succeeds");

        let store = TrustStore::load(&env);
        let contents = fs::read_to_string(&project).unwrap();
        assert!(store.is_settings_trusted(&project, &contents));
    }

    /// Mirrors `editing_a_trusted_files_content_de_trusts_it` for the
    /// settings kind -- an edit to a trusted `settings.json` (a `git pull`,
    /// a hand edit, or a hostile change) silently de-trusts it too.
    #[test]
    fn editing_a_trusted_settings_files_content_de_trusts_it() {
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let project = tempfile_dir().join("settings.json");
        fs::write(&project, r#"{"default_role":"coder"}"#).unwrap();
        TrustStore::trust_settings(&env, &project).expect("trust_settings succeeds");

        fs::write(
            &project,
            r#"{"default_role":"coder","backends":{"anthropic":{"kind":"anthropic","base_url":"https://not-anthropic.example.com"}}}"#,
        )
        .unwrap();

        let store = TrustStore::load(&env);
        let contents = fs::read_to_string(&project).unwrap();
        assert!(
            !store.is_settings_trusted(&project, &contents),
            "a content change must de-trust silently, exactly as it does for \
             the permission_file kind"
        );
    }

    /// The two kinds are genuinely independent records, not two views onto
    /// one: trusting a path's `permissions.json` role says nothing about
    /// that SAME path used as a `settings.json` role, and vice versa. Uses
    /// one literal path for both kinds deliberately, to prove the
    /// independence is keyed on `(kind, path)`, not merely on path alone.
    #[test]
    fn trust_does_not_leak_across_kinds_for_the_same_path() {
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let shared_path = tempfile_dir().join("shared.json");
        fs::write(&shared_path, r#"{"anything":"here"}"#).unwrap();
        let contents = fs::read_to_string(&shared_path).unwrap();

        TrustStore::trust(&env, &shared_path).expect("trust (permission kind) succeeds");

        let store = TrustStore::load(&env);
        assert!(
            store.is_trusted(&shared_path, &contents),
            "the permission_file kind must be trusted"
        );
        assert!(
            !store.is_settings_trusted(&shared_path, &contents),
            "the settings_file kind must NOT be trusted merely because the \
             permission_file kind was, for the identical path"
        );
    }

    #[test]
    fn settings_status_distinguishes_new_changed_and_unchanged() {
        let config_dir = tempfile_dir();
        let env = env_for(&config_dir);
        let project = tempfile_dir().join("settings.json");
        fs::write(&project, r#"{"default_role":"coder"}"#).unwrap();

        let store = TrustStore::load(&env);
        let contents = fs::read_to_string(&project).unwrap();
        assert_eq!(store.settings_status(&project, &contents), TrustStatus::New);

        TrustStore::trust_settings(&env, &project).expect("trust_settings succeeds");
        let store = TrustStore::load(&env);
        assert_eq!(
            store.settings_status(&project, &contents),
            TrustStatus::Unchanged
        );

        fs::write(&project, r#"{"default_role":"other"}"#).unwrap();
        let new_contents = fs::read_to_string(&project).unwrap();
        let store = TrustStore::load(&env);
        assert_eq!(
            store.settings_status(&project, &new_contents),
            TrustStatus::Changed
        );
    }

    // ---------------------------------------------------------------
    // `guard_untrusted_project_settings` -- the consent gate itself, and
    // the P-15 pairing this board item's own spec names: (1) the
    // load-bearing "not applied before consent, applied after", (2) a
    // non-interactive-style refusal naming the file, (3) the operator's
    // own user layer still applies with no prompt at all.
    // ---------------------------------------------------------------

    /// **P-15's own load-bearing property, halves 1 and 2**: a `cwd`
    /// beneath an ancestor carrying an untrusted project `settings.json`
    /// is refused by the gate AND (proven through the real merge pipeline,
    /// `crate::config::load`, not merely this module's own `TrustStore`)
    /// is not what a config load would actually reflect; once the operator
    /// consents (`TrustStore::trust_settings`), the SAME gate call
    /// succeeds, and a real `crate::config::load` against the identical
    /// `cwd`/`env` now DOES carry the project file's own value through.
    /// **Fails against HEAD** (no gate existed before this item -- every
    /// project `settings.json` applied unconditionally).
    #[test]
    fn guard_untrusted_project_settings_refuses_until_consent_then_the_real_load_applies_it() {
        let config_dir = tempfile_dir();
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            config_dir.display().to_string(),
        );

        let project_root = tempfile_dir();
        let conf_dir = project_root.join(".conway");
        fs::create_dir_all(&conf_dir).unwrap();
        let settings_path = conf_dir.join("settings.json");
        fs::write(&settings_path, r#"{"limits":{"max_steps":77}}"#).unwrap();

        // Before consent: the gate refuses, naming the exact file.
        let err = super::guard_untrusted_project_settings(&project_root, &env)
            .expect_err("an untrusted project settings.json must refuse");
        assert_eq!(err.path, settings_path);

        // And the real merge pipeline, run independently of the gate
        // (`crate::config::load` never consults this module today outside
        // `ConwayBuilder::discover`), still reads a config -- but this
        // proves the FIXTURE is well-formed, not that the gate does
        // anything to `load` itself; the gate is enforced at
        // `ConwayBuilder::discover`'s own call site, see that method's own
        // doc.
        let outcome = crate::config::load(crate::config::LoadOptions {
            cwd: project_root.clone(),
            explicit_path: None,
            env: env.clone(),
            cli_overrides: crate::config::CliOverrides::default(),
            model_metadata_refresh: false,
        })
        .expect("the fixture itself must be well-formed JSON");
        assert_eq!(
            outcome.config.limits.max_steps, 77,
            "sanity: the project settings.json fixture actually carries the \
             value this test asserts on after consent"
        );

        // After consent: the SAME gate call now succeeds.
        TrustStore::trust_settings(&env, &settings_path).expect("trust_settings succeeds");
        super::guard_untrusted_project_settings(&project_root, &env)
            .expect("a trusted project settings.json must not refuse");
    }

    /// Half 2 of P-15's own pairing, isolated: the refusal is a NAMED
    /// error (`UntrustedProjectSettings`, carrying the exact offending
    /// `path`), not a generic message a caller would have to string-match
    /// -- this is what lets a non-interactive caller (this function has no
    /// interactivity of its own; see its own doc) print something specific
    /// and exit, and what would let an interactive caller pattern-match on
    /// it to decide whether to prompt. **Fails against HEAD** (no such
    /// error variant/gate existed).
    #[test]
    fn guard_untrusted_project_settings_names_the_offending_file() {
        let config_dir = tempfile_dir();
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            config_dir.display().to_string(),
        );
        let project_root = tempfile_dir();
        let conf_dir = project_root.join(".conway");
        fs::create_dir_all(&conf_dir).unwrap();
        let settings_path = conf_dir.join("settings.json");
        fs::write(&settings_path, r#"{}"#).unwrap();

        let err = super::guard_untrusted_project_settings(&project_root, &env).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains(&settings_path.display().to_string()),
            "the refusal must name the exact file, got: {message}"
        );
    }

    /// P-15's own required pairing (b): the operator's own USER layer --
    /// `$CONWAY_CONFIG_DIR/settings.json` here -- is never a candidate this
    /// gate even looks at (it only ever calls `discovery::discover`, the
    /// PROJECT walk), so a `cwd` with no ancestor project `.conway/
    /// settings.json` of its own passes with no trust record and no
    /// prompt, and the real merge pipeline applies the user layer's value
    /// unconditionally -- exactly today's behavior, unaffected by this
    /// item. Without this test, a gate that (by a bug) fired on EVERY
    /// settings.json regardless of scope would still pass every other test
    /// in this module.
    #[test]
    fn guard_untrusted_project_settings_never_gates_the_operators_own_user_layer() {
        let config_dir = tempfile_dir();
        fs::write(
            config_dir.join("settings.json"),
            r#"{"limits":{"max_steps":99}}"#,
        )
        .unwrap();
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            config_dir.display().to_string(),
        );

        // No ancestor `.conway/settings.json` of its own -- nothing for
        // the PROJECT walk to reach at all.
        let cwd = tempfile_dir();

        super::guard_untrusted_project_settings(&cwd, &env)
            .expect("no project layer exists here; the gate must not fire");

        let outcome = crate::config::load(crate::config::LoadOptions {
            cwd,
            explicit_path: None,
            env,
            cli_overrides: crate::config::CliOverrides::default(),
            model_metadata_refresh: false,
        })
        .expect("load must succeed with no prompt and no trust record");
        assert_eq!(
            outcome.config.limits.max_steps, 99,
            "the operator's own user layer must apply unconditionally, with \
             no consent gate at all"
        );
    }
}
