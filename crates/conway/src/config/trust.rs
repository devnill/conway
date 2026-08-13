//! Recording an explicit, content-scoped trust decision for a
//! project-scoped `.conway/permissions.json`, per D4
//! (`.design/d4-trust-model.md` §4-5, §11) and board item
//! 01KYT8SGX32CP56PRJNG72V2W5.
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
//! own global file (`<xdg>/permissions.json`) is trusted by authorship --
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
//! ## Deliberately NOT `#[serde(deny_unknown_fields)]` (board item
//! 01KZHVDDQQ7XT0RK3JVNM2YV83)
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The on-disk shape of `<xdg>/trust.json`. `permission_files` maps a
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
    /// file this crate resolves (`xdg_config_path`, `history_file_path`) --
    /// a third consumer of machinery that already exists, not a new
    /// discovery paradigm. Deliberately global-only: a project-scoped
    /// trust file would let untrusted content trust itself (D4 §4).
    pub fn path(env: &HashMap<String, String>) -> Option<PathBuf> {
        super::discovery::xdg_config_path(env)
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
        match self.file.permission_files.get(&path_key(abs_path)) {
            Some(record) => record.content_digest == content_digest(contents),
            None => false,
        }
    }

    /// Records an explicit trust decision for `abs_path`'s CURRENT bytes
    /// on disk, then writes the store back out (tmp-then-rename,
    /// mirroring `tui/history.rs`'s and `tui/app.rs::persist_permission_rule`'s
    /// own precedent so a crash mid-write cannot corrupt the file).
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
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serialized)?;
        std::fs::rename(&tmp, &path)?;
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

    fn env_for(xdg: &Path) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert("XDG_CONFIG_HOME".to_string(), xdg.display().to_string());
        env
    }

    #[test]
    fn an_untrusted_file_is_untrusted_by_default() {
        let xdg = tempfile_dir();
        let env = env_for(&xdg);
        let store = TrustStore::load(&env);
        let project = tempfile_dir().join("permissions.json");
        fs::write(&project, r#"{"allow":["bash:*"]}"#).unwrap();
        let contents = fs::read_to_string(&project).unwrap();
        assert!(!store.is_trusted(&project, &contents));
    }

    #[test]
    fn trusting_a_file_makes_is_trusted_true_for_its_current_bytes() {
        let xdg = tempfile_dir();
        let env = env_for(&xdg);
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
        let xdg = tempfile_dir();
        let env = env_for(&xdg);
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
        let xdg = tempfile_dir();
        let env = env_for(&xdg);
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
        let xdg = tempfile_dir();
        let env = env_for(&xdg);
        let store = TrustStore::load(&env);
        assert!(!store.is_trusted(Path::new("/does/not/matter"), "anything"));
    }

    /// A corrupt `trust.json` is treated as empty, never partially
    /// applied and never a panic.
    #[test]
    fn a_corrupt_trust_file_is_treated_as_empty() {
        let xdg = tempfile_dir();
        fs::create_dir_all(xdg.join("conway")).unwrap();
        fs::write(xdg.join("conway").join("trust.json"), "not json at all").unwrap();
        let env = env_for(&xdg);

        let store = TrustStore::load(&env);
        assert!(!store.is_trusted(Path::new("/anything"), "anything"));
    }

    /// Board item 01KZHVDDQQ7XT0RK3JVNM2YV83, question 3: pins the
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
        let xdg = tempfile_dir();
        fs::create_dir_all(xdg.join("conway")).unwrap();
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
        fs::write(xdg.join("conway").join("trust.json"), trust_json).unwrap();
        let env = env_for(&xdg);

        let store = TrustStore::load(&env);
        assert!(
            store.is_trusted(&project, contents),
            "a field this build does not recognize -- at either nesting \
             level -- must not prevent an otherwise-valid recorded trust \
             decision from matching"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_world_writable_trust_file_is_refused_like_a_loose_ssh_key() {
        use std::os::unix::fs::PermissionsExt;
        let xdg = tempfile_dir();
        let env = env_for(&xdg);
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
}
