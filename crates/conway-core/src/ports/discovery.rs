//! `SessionDiscoveryHost`: the capability a `Tool` needs to find a session
//! it did not name and did not spawn -- the read half board item
//! `01M0PS8J3AK7Z7253Z3E3RD3GY` closes. [`crate::ports::ContextPathHost::
//! resolve_records`] can already read any `(session, seq)` pair a caller
//! already knows; this port answers the question that comes BEFORE that
//! one -- which session, which seq -- for a session the caller neither owns
//! nor holds a `transcript_ref` for.
//!
//! # Why a new port, not a widened `ContextPathHost` (board item's own
//! determine-before-building #2, "tool, or host?", argued here at the port
//! boundary rather than assumed)
//!
//! `ContextPathHost`'s own module doc draws a narrow, purpose-built line:
//! [`crate::ports::ContextPathHost::resolve_records`] resolves KNOWN refs,
//! it does not list or search. Bolting a `search` method onto that trait
//! would fuse two genuinely separate capabilities -- composition and
//! discovery -- into one port, exactly the scope-doubling the board item's
//! own history already names as a mistake avoided once (cherry-pick,
//! `01M0KZ6J0DF6XR1TVSDH2KDPRX`, correctly left discovery for a later item
//! rather than cramming it into the composing tool). Repeating that fusion
//! one layer down, inside the port instead of the tool, would be the same
//! trap wearing a different hat. A new, narrow port keeps each seam
//! answering exactly one question.
//!
//! # Reach (determine-before-building #4): a directory listing over one
//! root, never a crawler or a registry
//!
//! Decision `01M0QK8J757ZH6R06WYJ0PQGEM` moved sessions to a central,
//! project-keyed root (`config::discovery::session_root`'s central-default
//! branch) specifically so machine-wide discovery would not need either. An
//! implementation of this port therefore does exactly one `read_dir` over
//! that root, never a filesystem crawl for `.conway/sessions` directories
//! and never a side table anything must keep in sync. See
//! `conway_session::discovery`'s own module doc for exactly how.
//!
//! # Search surface (determine-before-building #1): session metadata always,
//! record text only when asked and only up to a caller-visible bound
//!
//! [`SessionSearchQuery::text`] is the one field that turns this into
//! content search, and content search is real I/O -- reading a session's
//! records to grep them costs exactly what reading them for composition
//! would. `SessionSearchQuery::max_sessions` is the bound a caller sets
//! BEFORE paying anything (determine-before-building #3: "the price of a
//! curation decision is knowable in advance," DESIGN §5b): it caps how many
//! sessions this call will ever open and read, in EITHER mode, and
//! [`SessionSearchResult`] reports what was actually scanned and whether
//! more existed beyond the cap (`truncated`). No implementation of this
//! port may read more sessions than `max_sessions` names, in any code path.
//!
//! # Does an index already exist (determine-before-building #5)?
//!
//! `conway_session::SessionIndex` already exists and already holds exactly
//! the HEADER metadata `SessionSearchQuery`'s label/agent_def filters need
//! -- one per project, rebuildable, never a source of truth. Reusing it (via
//! the ordinary `SessionStore::list` a caller of THIS project's own store
//! already has) is what keeps a metadata-only query free of new storage
//! entirely. Nothing in this port or its implementation adds a second index
//! of record CONTENT -- that would be the "much larger commitment" the
//! board item's own text warns against, and a bounded scan over
//! `max_sessions` sessions is what this item chose instead. See
//! `conway_session::discovery`'s own module doc for the concrete reuse.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::StoreError;
use crate::ids::{LogSeq, SessionId};

/// Which sessions a [`SessionSearchQuery`] is willing to look at.
///
/// **Defaults to [`Self::CurrentProject`]** (see [`SessionSearchQuery`]'s
/// own `Default` impl) -- the board item's own motivating example ("what we
/// worked out about the retry logic yesterday") is almost always a sibling
/// session in the SAME project, and a machine may host many projects under
/// the central root: defaulting wide would make an ordinary query's cost
/// depend on how many OTHER projects happen to exist on this machine, not
/// on the question actually asked. [`Self::AllProjects`] is a caller's
/// explicit widening, not conway's default guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionSearchScope {
    /// Only the calling session's own project -- the sessions that would
    /// have lived at the OLD, pre-central-root default (`.conway/sessions`
    /// relative to this project's checkout).
    #[default]
    CurrentProject,
    /// Every project directory found under the central sessions root
    /// (`config::discovery::session_root`'s central-default branch's own
    /// parent). A project whose `[session].root` was explicitly configured
    /// to somewhere OTHER than the central default is invisible to this
    /// scope for its OWN sessions -- it never wrote there, so there is
    /// nothing to list. This is the disclosed edge the central-root
    /// decision's own doc already names, not a new gap this port opens.
    AllProjects,
}

/// One discovery request. See the module doc for the cost/scope reasoning
/// behind each field.
#[derive(Debug, Clone)]
pub struct SessionSearchQuery {
    pub scope: SessionSearchScope,
    /// Exact match against a session's own `SessionMeta::labels` -- the
    /// SAME exact-match contract `SessionFilter::label`/`SessionIndex::list`
    /// already establish, reused rather than reinvented.
    pub label: Option<String>,
    /// Exact match against a session's own `SessionMeta::agent_def`.
    pub agent_def: Option<String>,
    /// A case-insensitive substring to match against each candidate
    /// session's own logged record text. `None` (the default) means
    /// METADATA ONLY -- zero records are ever read, and
    /// [`SessionSearchResult::records_scanned`] is always `0`. `Some` turns
    /// this into content search, bounded by [`Self::max_sessions`] exactly
    /// as metadata search is (see the module doc's cost section).
    pub text: Option<String>,
    /// The hard cap on how many sessions this call will ever open and read
    /// (metadata or content, whichever [`Self::text`] selects) -- the price
    /// named BEFORE the call runs. Clamped into `1..=100` by every
    /// implementation of this port (never trusted verbatim from a caller,
    /// the same discipline `ideate-work list --limit`'s own clamp
    /// establishes for an unrelated but structurally identical "a caller's
    /// number becomes real I/O" surface).
    pub max_sessions: usize,
}

impl Default for SessionSearchQuery {
    fn default() -> Self {
        Self {
            scope: SessionSearchScope::default(),
            label: None,
            agent_def: None,
            text: None,
            max_sessions: 20,
        }
    }
}

/// One record, within a matched session, whose text contained
/// [`SessionSearchQuery::text`]. Only ever populated when `text` was
/// `Some` -- a metadata-only match ([`SessionSearchQuery::text`] `None`)
/// always has an empty `matched_records` on its [`SessionMatch`].
#[derive(Debug, Clone)]
pub struct MatchedRecord {
    pub seq: LogSeq,
    /// A short, human-readable excerpt of the record's own text, centered
    /// on the match where practical. Never the whole record -- this is a
    /// PREVIEW for a model deciding whether to compose the record in via
    /// `compose_context_path`, not a substitute for reading it.
    pub snippet: String,
}

/// One session this query considered a match -- either a metadata match
/// (`matched_records` empty, `SessionSearchQuery::text` was `None`) or a
/// content match (`matched_records` non-empty).
#[derive(Debug, Clone)]
pub struct SessionMatch {
    pub session: SessionId,
    /// The project-key directory name this session was found under
    /// (`config::discovery::encode_project_key`'s own output) -- how a
    /// person or a model tells two same-named projects apart, and what a
    /// human-readable audit of "what was searched" actually names.
    pub project_key: String,
    pub cwd: PathBuf,
    pub created: DateTime<Utc>,
    pub agent_def: Option<String>,
    pub labels: Vec<String>,
    pub matched_records: Vec<MatchedRecord>,
}

/// What one [`SessionDiscoveryHost::search`] call found AND what it cost --
/// the module doc's "knowable in advance, visible afterward" pair. Every
/// field here answers "what was searched, and what did it cost" verbatim
/// (board item acceptance criterion 2), never just "here are your matches."
#[derive(Debug, Clone, Default)]
pub struct SessionSearchResult {
    pub matches: Vec<SessionMatch>,
    /// How many project directories were considered (1 for
    /// [`SessionSearchScope::CurrentProject`]; the number of entries under
    /// the central root, filtered by nothing else, for
    /// [`SessionSearchScope::AllProjects`]).
    pub projects_scanned: usize,
    /// How many sessions' METADATA was evaluated (index/header reads only
    /// -- zero record bodies).
    pub sessions_considered: usize,
    /// How many sessions had their own record bodies actually read (always
    /// `0` when `SessionSearchQuery::text` was `None`).
    pub sessions_content_scanned: usize,
    /// The total record count read across every session in
    /// `sessions_content_scanned` -- the literal I/O cost this call paid,
    /// always `0` when `SessionSearchQuery::text` was `None`.
    pub records_scanned: usize,
    /// `true` when `SessionSearchQuery::max_sessions` cut this search off
    /// before every eligible candidate was considered -- more matches may
    /// exist. A caller that needs them re-issues the query with a larger
    /// `max_sessions`, paying the larger, now-explicit cost.
    pub truncated: bool,
}

/// The discovery capability (see module doc). Object-safe (no generic
/// params, `#[async_trait]`) so the runtime can hold `Arc<dyn
/// SessionDiscoveryHost>`, mirroring [`crate::ports::ContextPathHost`] and
/// [`crate::ports::SubagentHost`] exactly.
#[async_trait]
pub trait SessionDiscoveryHost: Send + Sync + 'static {
    async fn search(&self, query: SessionSearchQuery) -> Result<SessionSearchResult, StoreError>;
}

/// The `ToolCtx`-facing wrapper around `Arc<dyn SessionDiscoveryHost>` --
/// mirrors [`crate::ports::ContextPathHandle`]'s own shape, minus session
/// binding: discovery is cross-session by construction (there is no single
/// session a search is "about"), so unlike `ContextPathHandle` there is no
/// caller session to bake in structurally.
#[derive(Clone)]
pub struct SessionDiscoveryHandle {
    host: Arc<dyn SessionDiscoveryHost>,
}

impl std::fmt::Debug for SessionDiscoveryHandle {
    // Manual impl: `Arc<dyn SessionDiscoveryHost>` carries no `Debug` bound,
    // mirroring `ContextPathHandle`'s own manual `Debug` exactly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionDiscoveryHandle")
            .field("host", &"<dyn SessionDiscoveryHost>")
            .finish()
    }
}

impl SessionDiscoveryHandle {
    pub fn new(host: Arc<dyn SessionDiscoveryHost>) -> Self {
        Self { host }
    }

    /// A double that refuses every call with `StoreError::Io` -- the
    /// `ToolCtx::for_test` default for tools that never exercise session
    /// discovery, mirroring [`crate::ports::ContextPathHandle::noop`]'s own
    /// "unconditional, no I/O, REFUSES rather than silently succeeding"
    /// shape (that type's own doc: a silent empty-result default would
    /// mask a fixture that forgot to wire a real host, exactly the "claims
    /// to be reached that isn't" failure this project's own rule forbids).
    pub fn noop() -> Self {
        Self {
            host: Arc::new(NoopSessionDiscoveryHost),
        }
    }

    pub async fn search(
        &self,
        query: SessionSearchQuery,
    ) -> Result<SessionSearchResult, StoreError> {
        self.host.search(query).await
    }
}

/// The private implementation behind [`SessionDiscoveryHandle::noop`]. Not
/// itself exported -- mirrors `context_path`'s own private
/// `NoopContextPathHost` exactly.
struct NoopSessionDiscoveryHost;

#[async_trait]
impl SessionDiscoveryHost for NoopSessionDiscoveryHost {
    async fn search(&self, _query: SessionSearchQuery) -> Result<SessionSearchResult, StoreError> {
        Err(StoreError::Io {
            detail: "no SessionDiscoveryHost configured for this ToolCtx fixture \
                     (SessionDiscoveryHandle::noop)"
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn noop(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);

        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        loop {
            if let Poll::Ready(val) = fut.as_mut().poll(&mut cx) {
                return val;
            }
        }
    }

    #[test]
    fn default_query_scopes_to_current_project() {
        assert_eq!(
            SessionSearchQuery::default().scope,
            SessionSearchScope::CurrentProject
        );
    }

    #[test]
    fn noop_refuses_rather_than_returning_a_silently_empty_result() {
        let handle = SessionDiscoveryHandle::noop();
        let err = block_on(handle.search(SessionSearchQuery::default())).unwrap_err();
        assert!(matches!(err, StoreError::Io { .. }));
    }

    #[test]
    fn session_discovery_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SessionDiscoveryHandle>();
    }
}
