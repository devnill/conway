//! Session names: an operator-chosen label attached to a session id, via
//! `conway sessions name <id-or-name> <name>`/`conway sessions unname
//! <id-or-name>` -- INTENT.md §7b: "a session an operator will return to
//! can carry a name the operator chose. The id stays the machine's; the
//! name is furniture, and furniture follows convention." See this crate's
//! `commands/sessions.rs` for why a `sessions` subcommand pair, not a root
//! `--name` flag, is the surface this item chose.
//!
//! # Where a name lives, and why renaming it cannot rewrite a record
//!
//! A session's identity is its append-only log
//! (`conway-session::JsonlSessionStore`, one `root/<session-id>.jsonl` file
//! per session, plus `root/index.jsonl` -- see `docs/sessions.md`). Nothing
//! in this module ever opens either of those files. A name lives in a
//! wholly separate sidecar, [`NAMES_FILE`] (`root/session-names.json`, the
//! SAME `root` `Conway::config().session.root` already resolves to, i.e.
//! the central, project-keyed sessions directory -- see
//! `conway::config::schema::SessionConfig`'s own doc for exactly how that
//! path is derived), holding nothing but a flat `{name: session-id}` JSON
//! object.
//!
//! Naming, renaming, or removing a name is therefore entirely a read
//! /mutate/write cycle over this one small file: [`NamesStore::set`]/
//! [`NamesStore::unset`] never open, read, or write ANY `<session-id>
//! .jsonl`, so a session's own persisted log is byte-for-byte unaffected by
//! any of the operations this module performs -- there is no code path
//! from here into `conway_core::ports::SessionStore`'s `append`/`create` at
//! all. `NamesStore::load`/`save` round-trip the sidecar atomically (a
//! temp-file write plus a rename), matching every other durability
//! expectation this crate holds for files under the config root, but this
//! is furniture, not the log: a lost or corrupted sidecar loses labels, not
//! sessions, and every session is still fully addressable by its own id
//! exactly as if this module did not exist.
//!
//! Because `root` is the same project-keyed directory
//! `01M0QK8J757ZH6R06WYJ0PQGEM` moved every session under, the sidecar
//! moves with it automatically -- there is no separate migration for this
//! module to own.
//!
//! # The reference grammar this module adds
//!
//! `session_ref.rs` owns the bare `<session-id>[@<seq>]` grammar (ULIDs
//! only, no I/O). This module sits one layer up: [`resolve`]/
//! [`resolve_fork_ref`] try that grammar FIRST, and only consult the name
//! table when the input is not itself a syntactically valid `SessionId` --
//! so a bare ULID never pays for a names-file read, and (per
//! [`NamesStore::set`]) a name can never itself be ULID-shaped, which is
//! what keeps "is this token an id or a name" from ever being ambiguous.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use conway::{Conway, LogSeq, SessionId};

use crate::session_ref;

/// The sidecar file name, relative to the resolved session root.
pub const NAMES_FILE: &str = "session-names.json";

/// The effective session root this `Conway` is using -- the SAME value
/// `ConwayBuilder::build` resolved `JsonlSessionStore::open` against.
/// `conway.config().session.root` is `Some` for every config reached
/// through `config::load`/`load_ignoring_user_config` (which is every path
/// that builds a real, running `conway` binary) -- the `None` fallback
/// below only matters for a `Conway` assembled directly via
/// `ConwayBuilder::from_parts`, bypassing `load` entirely, and mirrors
/// `builder.rs`'s OWN identical fallback (`.conway/sessions`, resolved
/// against `cwd`) so this always names the exact directory the live store
/// actually opened -- the same "read the same effective value the builder
/// already computed" duplication `oneshot::resolve_agents_dir` already
/// documents for `--agent`'s own directory.
pub fn session_root(conway: &Conway) -> PathBuf {
    let config = conway.config();
    let root = config
        .session
        .root
        .clone()
        .unwrap_or_else(|| PathBuf::from(".conway/sessions"));
    if root.is_absolute() {
        root
    } else {
        config.cwd.join(root)
    }
}

/// A naming/renaming/resolution failure. Every variant is a **loud, typed
/// refusal naming what collided or what was missing** (INTENT §8.3) --
/// never a guess.
#[derive(Debug)]
pub enum NameError {
    /// The candidate name parses as a valid `SessionId` (ULID) -- refused
    /// at naming time, per this item's acceptance criteria, so the
    /// `--session`/`--resume`/`sessions show` reference grammar never has
    /// to guess whether a bare token is an id or a name.
    LooksLikeUlid(String),
    /// `name` is already bound to a DIFFERENT session than the one this
    /// call is trying to bind it to.
    AlreadyBound { name: String, existing: SessionId },
    /// The name (or the name bound to the given id) has no entry to
    /// remove/rename.
    Unknown(String),
    /// The sidecar file exists but could not be read, or could not be
    /// written back.
    Io(std::io::Error),
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameError::LooksLikeUlid(name) => write!(
                f,
                "name {name:?} looks like a session id (a valid ULID); names must not parse as \
                 ULIDs, so the reference grammar never has to guess which one a bare token means"
            ),
            NameError::AlreadyBound { name, existing } => write!(
                f,
                "name {name:?} is already bound to session {existing}; unname it first (`conway \
                 sessions unname {name}`) or choose a different name"
            ),
            NameError::Unknown(target) => {
                write!(f, "no name bound to {target:?}")
            }
            NameError::Io(e) => write!(f, "session names file: {e}"),
        }
    }
}

impl std::error::Error for NameError {}

impl From<std::io::Error> for NameError {
    fn from(e: std::io::Error) -> Self {
        NameError::Io(e)
    }
}

/// A session-reference token that resolved to neither a valid ULID nor a
/// known name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError(String);

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid session reference {:?}: not a valid session id (ULID) and no session is \
             named that",
            self.0
        )
    }
}

impl std::error::Error for ResolveError {}

/// The name <-> id side table for one session root. A flat bijection --
/// [`NamesStore::set`] refuses a name already bound elsewhere, so a name
/// always resolves to at most one session and a session always carries at
/// most one name.
#[derive(Debug, Default, Clone)]
pub struct NamesStore {
    path: PathBuf,
    by_name: BTreeMap<String, SessionId>,
}

impl NamesStore {
    /// Loads the sidecar at `session_root/session-names.json`, or starts
    /// empty if it does not exist yet (a fresh store, or one where no
    /// session has ever been named, is not an error).
    pub fn load(session_root: &Path) -> Result<Self, NameError> {
        let path = session_root.join(NAMES_FILE);
        let by_name = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                NameError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                ))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(NameError::Io(e)),
        };
        Ok(Self { path, by_name })
    }

    /// The session bound to `name`, if any.
    pub fn resolve(&self, name: &str) -> Option<SessionId> {
        self.by_name.get(name).copied()
    }

    /// The name bound to `id`, if any (at most one, per this type's own
    /// bijection invariant).
    pub fn name_of(&self, id: SessionId) -> Option<&str> {
        self.by_name
            .iter()
            .find(|(_, v)| **v == id)
            .map(|(k, _)| k.as_str())
    }

    /// Binds `name` to `id`, persisting immediately. Idempotent when `name`
    /// is already bound to `id`. If `id` already carries a DIFFERENT name,
    /// that old entry is removed first -- a session carries at most one
    /// name, so re-naming an already-named session is exactly this call,
    /// not a separate operation.
    pub fn set(&mut self, name: &str, id: SessionId) -> Result<(), NameError> {
        if name.parse::<SessionId>().is_ok() {
            return Err(NameError::LooksLikeUlid(name.to_string()));
        }
        if let Some(existing) = self.by_name.get(name) {
            if *existing == id {
                return Ok(());
            }
            return Err(NameError::AlreadyBound {
                name: name.to_string(),
                existing: *existing,
            });
        }
        if let Some(old_name) = self.name_of(id).map(|s| s.to_string()) {
            self.by_name.remove(&old_name);
        }
        self.by_name.insert(name.to_string(), id);
        self.save()
    }

    /// Removes whichever name is bound to `target` -- `target` may be
    /// itself a bound name, or the session id that name is bound to.
    pub fn unset(&mut self, target: &str) -> Result<(), NameError> {
        let key = if let Ok(sid) = target.parse::<SessionId>() {
            self.name_of(sid).map(|s| s.to_string())
        } else if self.by_name.contains_key(target) {
            Some(target.to_string())
        } else {
            None
        };
        match key {
            Some(k) => {
                self.by_name.remove(&k);
                self.save()
            }
            None => Err(NameError::Unknown(target.to_string())),
        }
    }

    /// Atomic write: serialize to a temp file beside the target, then
    /// rename over it -- a reader never observes a half-written sidecar.
    fn save(&self) -> Result<(), NameError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.by_name).map_err(|e| {
            NameError::Io(std::io::Error::other(format!(
                "serializing session names: {e}"
            )))
        })?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Resolves a bare `--session`/`--resume`/`sessions show|tree|export`
/// value: a full ULID is used directly (never consults the names file); any
/// other string is looked up in `names`.
pub fn resolve(raw: &str, names: &NamesStore) -> Result<SessionId, ResolveError> {
    if let Ok(sid) = raw.parse::<SessionId>() {
        return Ok(sid);
    }
    names
        .resolve(raw)
        .ok_or_else(|| ResolveError(raw.to_string()))
}

/// Resolves a `--fork-from`-shaped `<session-id-or-name>[@<seq>]` value:
/// splits off the optional `@<seq>` suffix via `session_ref::
/// split_fork_ref` (the same syntactic step `session_ref::parse_fork_ref`
/// itself uses), then resolves the sid half exactly as [`resolve`] does.
pub fn resolve_fork_ref(
    raw: &str,
    names: &NamesStore,
) -> Result<(SessionId, Option<LogSeq>), ResolveError> {
    let (sid_part, seq) =
        session_ref::split_fork_ref(raw).map_err(|_| ResolveError(raw.to_string()))?;
    let sid = resolve(sid_part, names).map_err(|_| ResolveError(raw.to_string()))?;
    Ok((sid, seq))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const SID_B: &str = "01BX5ZZKBKACTAV9WEVGEMMVRZ";

    fn store(dir: &Path) -> NamesStore {
        NamesStore::load(dir).expect("load fresh store")
    }

    #[test]
    fn fresh_store_has_no_names() {
        let dir = tempfile::tempdir().unwrap();
        let names = store(dir.path());
        assert_eq!(names.resolve("daily"), None);
    }

    #[test]
    fn set_then_resolve_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut names = store(dir.path());
        let sid: SessionId = SID_A.parse().unwrap();
        names.set("daily", sid).expect("set");
        assert_eq!(names.resolve("daily"), Some(sid));
        assert_eq!(names.name_of(sid), Some("daily"));
    }

    #[test]
    fn set_persists_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let sid: SessionId = SID_A.parse().unwrap();
        store(dir.path()).set("daily", sid).expect("set");
        let reloaded = store(dir.path());
        assert_eq!(reloaded.resolve("daily"), Some(sid));
    }

    #[test]
    fn ulid_shaped_name_is_refused_at_naming_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut names = store(dir.path());
        let sid: SessionId = SID_A.parse().unwrap();
        let err = names.set(SID_B, sid).expect_err("ULID-shaped name refused");
        assert!(matches!(err, NameError::LooksLikeUlid(n) if n == SID_B));
    }

    #[test]
    fn name_already_bound_to_another_session_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut names = store(dir.path());
        let a: SessionId = SID_A.parse().unwrap();
        let b: SessionId = SID_B.parse().unwrap();
        names.set("daily", a).expect("first bind");
        let err = names.set("daily", b).expect_err("collision refused");
        match err {
            NameError::AlreadyBound { name, existing } => {
                assert_eq!(name, "daily");
                assert_eq!(existing, a);
            }
            other => panic!("expected AlreadyBound, got {other:?}"),
        }
        // The refusal must not have mutated the table.
        assert_eq!(names.resolve("daily"), Some(a));
    }

    #[test]
    fn re_setting_the_same_name_to_the_same_session_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut names = store(dir.path());
        let a: SessionId = SID_A.parse().unwrap();
        names.set("daily", a).expect("first bind");
        names.set("daily", a).expect("idempotent re-bind");
        assert_eq!(names.resolve("daily"), Some(a));
    }

    #[test]
    fn renaming_moves_the_one_name_a_session_carries() {
        let dir = tempfile::tempdir().unwrap();
        let mut names = store(dir.path());
        let a: SessionId = SID_A.parse().unwrap();
        names.set("daily", a).expect("first bind");
        names.set("standup", a).expect("rename");
        assert_eq!(names.resolve("daily"), None);
        assert_eq!(names.resolve("standup"), Some(a));
        assert_eq!(names.name_of(a), Some("standup"));
    }

    #[test]
    fn unset_by_name_removes_the_binding() {
        let dir = tempfile::tempdir().unwrap();
        let mut names = store(dir.path());
        let a: SessionId = SID_A.parse().unwrap();
        names.set("daily", a).expect("bind");
        names.unset("daily").expect("unset");
        assert_eq!(names.resolve("daily"), None);
    }

    #[test]
    fn unset_by_id_removes_the_binding() {
        let dir = tempfile::tempdir().unwrap();
        let mut names = store(dir.path());
        let a: SessionId = SID_A.parse().unwrap();
        names.set("daily", a).expect("bind");
        names.unset(SID_A).expect("unset by id");
        assert_eq!(names.resolve("daily"), None);
    }

    #[test]
    fn unset_unknown_name_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut names = store(dir.path());
        let err = names.unset("nope").expect_err("unknown name refused");
        assert!(matches!(err, NameError::Unknown(n) if n == "nope"));
    }

    #[test]
    fn resolve_prefers_ulid_over_a_same_named_entry() {
        // Can't happen in practice (`set` refuses ULID-shaped names), but
        // `resolve` itself must still try the ULID grammar first -- this
        // pins that ordering directly rather than relying on `set`'s guard
        // alone to keep it true.
        let dir = tempfile::tempdir().unwrap();
        let names = store(dir.path());
        let sid = resolve(SID_A, &names).expect("bare ULID resolves without any names lookup");
        assert_eq!(sid, SID_A.parse::<SessionId>().unwrap());
    }

    #[test]
    fn resolve_unknown_name_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let names = store(dir.path());
        let err = resolve("nope", &names).expect_err("unknown name refused");
        assert!(err.to_string().contains("not a valid session id"));
    }

    #[test]
    fn resolve_fork_ref_resolves_a_name_with_seq() {
        let dir = tempfile::tempdir().unwrap();
        let mut names = store(dir.path());
        let a: SessionId = SID_A.parse().unwrap();
        names.set("daily", a).expect("bind");
        let (sid, seq) = resolve_fork_ref("daily@3", &names).expect("resolves");
        assert_eq!(sid, a);
        assert_eq!(seq, Some(LogSeq(3)));
    }

    #[test]
    fn resolve_fork_ref_resolves_a_bare_ulid_with_no_seq() {
        let dir = tempfile::tempdir().unwrap();
        let names = store(dir.path());
        let (sid, seq) = resolve_fork_ref(SID_A, &names).expect("resolves");
        assert_eq!(sid, SID_A.parse::<SessionId>().unwrap());
        assert_eq!(seq, None);
    }
}
