//! `@`-mention completion and plain-`Tab` path completion's `AppState`-side
//! glue (board item `01M1YVF4X864GKSGZM4PSCTMEH`): the ONE place that walks
//! a filesystem or reads the live agent tree for this feature, cached on
//! [`AppState`] rather than redone on every keystroke.
//! `crate::tui::mentions` itself stays a pure, `AppState`-free module of
//! plain functions precisely so IT can be driven directly by a fixture tree
//! in tests with no `AppState`/terminal at all -- mirroring the exact split
//! `view/palette.rs::matches` (pure) and `input.rs::palette_navigate`/
//! `AppState::palette_stem` (glue) already use for the slash palette.
//!
//! **The walk never runs on this struct's own call stack (review finding
//! CRITICAL, round 1).** [`AppState::sync_mention`] is reached from
//! `input::handle_key`, which runs synchronously inside `App::run`'s own
//! `tokio::select!` loop -- calling [`mentions::scan_paths`] directly from
//! there would block the WHOLE TUI (no redraw, no keys) for as long as the
//! walk takes, exactly the hazard `app/startup.rs::read_git_branch` already
//! avoids for its own `git` call via `tokio::task::spawn_blocking`. Instead,
//! a Path-mode mention with no fresh cache entry sets
//! [`AppState::take_pending_mention_scan_request`]'s own field and leaves
//! `mention_candidates` EMPTY; `App::run`'s loop (not this module -- it has
//! no access to a `tokio` runtime handle or a channel) drains that request
//! after every key and does the actual walk off-loop, in
//! `app/mention_scan.rs`, delivering the result back over its own
//! `mpsc` channel the same way `provider_status.rs`'s background probe
//! already does. [`AppState::apply_mention_scan_result`] is the landing
//! side of that reply.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::tui::mentions::{self, MentionMode, MentionQuery, PathScan};

use super::*;

/// How long a completed walk stays valid before a NEW `@`-token re-walks
/// instead of reusing it -- review finding (CRITICAL, round 1): re-spawning
/// `git ls-files` for every single `@` the operator types (common across one
/// message referencing several files) would mean every one of them pays the
/// walk's latency, even though the tree essentially never changes shape
/// between two mentions typed seconds apart. 15 seconds is chosen as
/// "comfortably longer than one burst of `@`-typing within a single
/// message, short enough that a file created/deleted mid-session becomes
/// visible again without requiring a restart" -- there is no finer signal
/// available (no filesystem-watch plumbing in this crate), so a flat TTL is
/// the simplest honest choice.
pub const MENTION_SCAN_CACHE_TTL: Duration = Duration::from_secs(15);

/// One already-completed walk, cached so a later `@`-token opened against
/// the SAME [`AppState::mention_scan_root`] within `MENTION_SCAN_CACHE_TTL`
/// can reuse it instead of re-walking -- see this module's own doc.
#[derive(Debug, Clone)]
pub(super) struct MentionScanCacheEntry {
    root: PathBuf,
    scan: PathScan,
    computed_at: Instant,
}

/// A path walk [`AppState::sync_mention`] determined is needed (a Path-mode
/// mention opened with no fresh `MentionScanCacheEntry`) but cannot run
/// itself -- see this module's own doc. `App::run`'s loop drains this via
/// [`AppState::take_pending_mention_scan_request`] after every key and
/// spawns the actual walk off-loop (`app/mention_scan.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionScanRequest {
    /// The char index of the `@` this request was opened for -- threaded
    /// back through [`AppState::apply_mention_scan_result`] so a reply that
    /// arrives after the operator has since moved to a DIFFERENT `@`-token
    /// updates the cache but does not overwrite whatever that newer token's
    /// own (possibly already-cached) candidates are.
    pub anchor: usize,
    /// The directory [`mentions::scan_paths`] should walk -- a snapshot of
    /// [`AppState::mention_scan_root`] at request time, so a later change to
    /// that field (there is none in this crate today, but nothing enforces
    /// it staying fixed for a session's whole life) cannot retroactively
    /// change what an already-in-flight request walks.
    pub root: PathBuf,
}

impl AppState {
    /// The `@`-mention the cursor currently sits inside, if any -- a pure,
    /// disk-free read of `input`/`cursor`
    /// ([`mentions::active_mention`]). Safe to call on every
    /// render/keystroke; the expensive part ([`Self::sync_mention`]'s own
    /// walk) only runs when this transitions to a NEW `@`-token.
    pub fn mention_query(&self) -> Option<MentionQuery> {
        mentions::active_mention(&self.input, self.cursor)
    }

    /// Re-anchors the mention-completion state to whatever `@`-token (if
    /// any) the cursor now sits inside. Called from [`Self::
    /// sync_palette_stem`]/[`Self::sync_palette_stem_after_paste`] after
    /// every edit to `input` -- the SAME funnel that already keeps the
    /// slash palette's own stem honest, so there is exactly one place a
    /// future input-mutating call site could forget to wire this up, not
    /// several. `via_paste` is `true` only when the edit that triggered this
    /// call was a bracketed paste -- see [`Self::mention_blocks_enter_accept`]
    /// for why that distinction matters.
    ///
    /// Re-anchors (clearing `mention_candidates` and deciding whether a walk
    /// is needed) ONLY when the mention just opened or the cursor moved into
    /// a DIFFERENT `@`-token (`mention_anchor` changes) -- never on every
    /// keystroke inside the SAME token, which is what keeps live filtering
    /// cheap ([`Self::mention_matches`] only ever filters the already-walked
    /// `mention_candidates` field). **Never walks the filesystem itself** --
    /// see this module's own doc for why, and [`Self::
    /// take_pending_mention_scan_request`] for the actual walk's home.
    pub fn sync_mention(&mut self, via_paste: bool) {
        match self.mention_query() {
            Some(q) if self.mention_dismissed_for == Some(q.start) => {
                // `Esc` dismissed THIS exact token and the cursor has not
                // left it since -- stay closed, no re-walk, until the
                // cursor moves to a different token (or out of one
                // entirely), handled by the two arms below.
            }
            Some(q) if self.mention_anchor != Some(q.start) => {
                self.mention_dismissed_for = None;
                self.mention_anchor = Some(q.start);
                self.mention_via_paste = via_paste;
                let mode = mentions::mention_mode(&self.input);
                self.mention_mode = Some(mode);
                self.mention_selected = None;
                self.mention_scan_in_flight = None;
                self.pending_mention_scan_request = None;
                match mode {
                    MentionMode::Agent => {
                        self.mention_candidates = self.agent_mention_candidates();
                        self.mention_capped = false;
                    }
                    MentionMode::Path => {
                        // An owned snapshot, not a borrow: this ends the
                        // read of `self.mention_scan_cache` right here, so
                        // the writes to `self.mention_candidates`/
                        // `self.pending_mention_scan_request` below are
                        // never fighting a live borrow of a different field.
                        let fresh: Option<PathScan> = match &self.mention_scan_cache {
                            Some(entry)
                                if entry.root == self.mention_scan_root
                                    && entry.computed_at.elapsed() < MENTION_SCAN_CACHE_TTL =>
                            {
                                Some(entry.scan.clone())
                            }
                            _ => None,
                        };
                        match fresh {
                            Some(scan) => {
                                self.mention_candidates = scan.paths;
                                self.mention_capped = scan.capped;
                            }
                            None => {
                                self.mention_candidates = Vec::new();
                                self.mention_capped = false;
                                self.mention_scan_in_flight = Some(q.start);
                                self.pending_mention_scan_request = Some(MentionScanRequest {
                                    anchor: q.start,
                                    root: self.mention_scan_root.clone(),
                                });
                            }
                        }
                    }
                }
            }
            Some(_) => {
                // Further ordinary typing into a paste-opened mention counts
                // as the operator having interacted with it too -- see
                // `mention_blocks_enter_accept`'s own doc.
                if !via_paste {
                    self.mention_via_paste = false;
                }
            }
            None => {
                self.mention_dismissed_for = None;
                self.mention_via_paste = false;
                self.close_mention();
            }
        }
    }

    /// Drains the walk [`Self::sync_mention`] determined is needed, if any
    /// -- `App::run`'s loop calls this after EVERY key/paste it dispatches
    /// (unconditionally; cheap when `None`) and, when it returns `Some`,
    /// spawns the actual walk off-loop (`app/mention_scan.rs::
    /// App::spawn_mention_scan`). Takes rather than peeks: the request is
    /// consumed the instant the spawn is kicked off, so a loop iteration
    /// that is slow to get around to spawning it can never double-spawn the
    /// same walk on its next poll.
    pub fn take_pending_mention_scan_request(&mut self) -> Option<MentionScanRequest> {
        self.pending_mention_scan_request.take()
    }

    /// Whether the CURRENTLY open mention is the one a background walk is
    /// still running for -- distinct from merely "a request exists" (that
    /// window is closed the instant `App::run` takes it); this stays `true`
    /// for the whole time the walk is actually in flight, which is what
    /// `view/mentions.rs` reads to render "scanning..." instead of either a
    /// (possibly stale) empty list or nothing at all.
    pub fn mention_scan_pending(&self) -> bool {
        self.mention_anchor.is_some() && self.mention_scan_in_flight == self.mention_anchor
    }

    /// Applies one completed background walk (`app/mention_scan.rs`'s own
    /// channel reply). Updates the `MentionScanCacheEntry` for `root`
    /// UNCONDITIONALLY (a later mention against the same root should benefit
    /// from this result regardless of whether the mention that originally
    /// asked for it is still open) -- but only overwrites the LIVE
    /// `mention_candidates`/`mention_capped` when `anchor` is still the
    /// currently-open mention's own anchor, so a reply that arrives after
    /// the operator closed that token (or opened a different one) cannot
    /// clobber whatever is on screen now.
    pub fn apply_mention_scan_result(&mut self, anchor: usize, root: PathBuf, scan: PathScan) {
        self.mention_scan_cache = Some(MentionScanCacheEntry {
            root,
            scan: scan.clone(),
            computed_at: Instant::now(),
        });
        if self.mention_scan_in_flight == Some(anchor) {
            self.mention_scan_in_flight = None;
        }
        if self.mention_anchor == Some(anchor) {
            self.mention_candidates = scan.paths;
            self.mention_capped = scan.capped;
        }
    }

    /// Review finding (minor, round 1): a bracketed paste that happens to
    /// start with `@` at a word boundary (e.g. pasting `@property` from a
    /// stylesheet) opens the SAME mention list ordinary typing would, with
    /// no deliberate intent to reference a file behind it -- fuzzy
    /// subsequence matching can then make a bare `Enter` (meant to submit)
    /// silently accept whatever candidate happens to match, instead.
    ///
    /// This is `true` -- `Enter` must NOT accept, and falls through to its
    /// ordinary submit meaning instead -- exactly when the currently-open
    /// mention was opened by a paste AND the operator has not since
    /// interacted with it (arrow-navigated, setting `mention_selected`, or
    /// typed a further ordinary character into it, clearing
    /// `mention_via_paste` back to `false` in [`Self::sync_mention`]).
    /// `Tab` is UNAFFECTED by this (`input.rs::accept_mention` only checks
    /// this for `KeyCode::Enter`): a bracketed paste delivers its whole
    /// payload as one `Event::Paste`, never as a separate literal `Tab`
    /// keypress, so `Tab` reaching the mention list always means a real,
    /// deliberate keystroke.
    pub fn mention_blocks_enter_accept(&self) -> bool {
        self.mention_via_paste && self.mention_selected.is_none()
    }

    /// Whether the mention-completion OVERLAY is currently showing --
    /// distinct from [`Self::mention_query`] (which only asks "does the
    /// cursor sit inside an `@`-token", true even right after `Esc`
    /// dismissed that exact token's list). `input.rs`'s own key dispatch
    /// and `view/mentions.rs`'s draw gate both check THIS, not
    /// `mention_query`, so a dismissed list stays dismissed and a second
    /// `Esc` falls through to its ordinary meaning instead of being eaten
    /// by an overlay that is not actually on screen any more.
    pub fn mention_open(&self) -> bool {
        self.mention_anchor.is_some()
    }

    /// `Esc`: closes the mention overlay AND remembers its token so typing
    /// further inside the SAME token does not immediately re-open it (see
    /// `mention_dismissed_for`'s own doc). Distinct from [`Self::
    /// close_mention`] (used when there is no "this token" left to
    /// remember -- the cursor left it, the line was submitted, or a
    /// candidate was just accepted), which must NOT set the dismissal
    /// marker: a fresh `@` always deserves a fresh chance.
    pub fn dismiss_mention(&mut self) {
        self.mention_dismissed_for = self.mention_anchor;
        self.close_mention();
    }

    /// Closes the mention overlay without marking its token as dismissed
    /// -- see [`Self::dismiss_mention`]'s own doc for the distinction.
    /// Clears every cached field so the next `@` starts fresh.
    pub fn close_mention(&mut self) {
        self.mention_anchor = None;
        self.mention_mode = None;
        self.mention_candidates.clear();
        self.mention_capped = false;
        self.mention_selected = None;
        self.mention_via_paste = false;
        // Deliberately NOT cleared: `mention_scan_in_flight`/
        // `pending_mention_scan_request`. A background walk already kicked
        // off keeps running either way (nothing to cancel), and
        // `apply_mention_scan_result`'s own anchor check already makes its
        // eventual reply a no-op for the live candidate list once the
        // mention it was for has closed -- clearing these here would only
        // let a SECOND, redundant request be queued if the operator reopens
        // the identical token before that reply lands.
    }

    /// Which candidate kind the anchored mention addresses, for the
    /// overlay's own title (`view/mentions.rs`) -- falls back to
    /// [`MentionMode::Path`] when nothing is open (never reached by a real
    /// render, since `view/mentions.rs::draw_overlay` checks
    /// [`Self::mention_open`] first, but total over any `&AppState` rather
    /// than panicking).
    pub fn mention_mode_for_display(&self) -> MentionMode {
        self.mention_mode.unwrap_or(MentionMode::Path)
    }

    /// Test-only seam: sets the cached mention state directly, for a
    /// `view/mentions.rs` test that needs to render a CAPPED listing
    /// without depending on `crate::tui::mentions::DEFAULT_SCAN_CAP`'s own
    /// real value (producing 2000+ real files in a fixture tree would be
    /// both slow and a poor proxy for "the overlay surfaces whatever
    /// `mention_capped` says").
    #[cfg(test)]
    pub(crate) fn set_mention_state_for_test(
        &mut self,
        mode: MentionMode,
        candidates: Vec<String>,
        capped: bool,
    ) {
        self.mention_anchor = Some(0);
        self.mention_mode = Some(mode);
        self.mention_candidates = candidates;
        self.mention_capped = capped;
        self.mention_selected = None;
    }

    /// The live-filtered candidate list the overlay draws, best match
    /// first -- cheap even on every keystroke, since it only filters the
    /// already-walked `mention_candidates` field
    /// ([`mentions::fuzzy_filter`]), never re-walks it.
    pub fn mention_matches(&self) -> Vec<&str> {
        let fragment = self.mention_query().map(|q| q.fragment).unwrap_or_default();
        mentions::fuzzy_filter(&self.mention_candidates, &fragment)
    }

    /// Whether the cached universe behind [`Self::mention_matches`] was
    /// capped by the bounded walk -- surfaced in the overlay's own title so
    /// a capped listing never silently looks complete.
    pub fn mention_capped(&self) -> bool {
        self.mention_capped
    }

    /// Agent-mention candidates (`/fork`/`/spawn @<agent>`): the
    /// operator-chosen name when one is set (`AppState::agent_names`), else
    /// the bare id -- the SAME two-tier identity `commands::resolve_agent`
    /// itself accepts, so every candidate this list offers actually
    /// resolves.
    fn agent_mention_candidates(&self) -> Vec<String> {
        self.tree
            .nodes
            .iter()
            .map(|n| {
                self.agent_names
                    .as_ref()
                    .and_then(|names| names.get(&n.agent_id))
                    .unwrap_or_else(|| n.agent_id.to_string())
            })
            .collect()
    }

    /// Plain-`Tab` path completion (point 3) reads the SAME
    /// `MentionScanCacheEntry`/`MENTION_SCAN_CACHE_TTL` the `@`-mention
    /// overlay does -- one shared cache of "what does this root currently
    /// look like," not two independently-walked copies of it. Review
    /// finding (CRITICAL, round 1): the OLD version of this method called
    /// [`mentions::scan_paths`] directly, which is exactly the same
    /// "blocks `handle_key`, freezes the TUI" hazard `sync_mention` had --
    /// `Tab` is reached from `input::handle_key` too. `None` on a cache
    /// miss/stale entry means "nothing to complete YET" (`input.rs::
    /// complete_path_at_cursor` treats that identically to "no match" --
    /// `Tab` stays its pre-item no-op for this one press) rather than ever
    /// blocking to produce an answer; [`Self::request_path_scan_warm_up`]
    /// is the caller's seam for kicking off the walk that will make the
    /// NEXT press able to complete.
    pub fn path_scan_cached_if_fresh(&self) -> Option<PathScan> {
        match &self.mention_scan_cache {
            Some(entry)
                if entry.root == self.mention_scan_root
                    && entry.computed_at.elapsed() < MENTION_SCAN_CACHE_TTL =>
            {
                Some(entry.scan.clone())
            }
            _ => None,
        }
    }

    /// Queues a background walk to warm the shared cache for plain-`Tab`
    /// completion, the same way a Path-mode `@`-mention's own cache miss
    /// does -- `anchor` is `Self::TAB_WARM_UP_ANCHOR`, a sentinel no real
    /// `@`-mention can ever produce (a mention's anchor is always a valid
    /// char index into `input`, and `usize::MAX` never is), so
    /// [`Self::apply_mention_scan_result`]'s own anchor check naturally
    /// treats this reply as "update the cache, nothing live to overwrite"
    /// -- exactly right, since no overlay is waiting on it. A no-op if a
    /// request (either kind) is already pending, so a flurry of `Tab`
    /// presses before the first warm-up lands cannot queue more than one.
    pub fn request_path_scan_warm_up(&mut self) {
        if self.pending_mention_scan_request.is_some() {
            return;
        }
        self.pending_mention_scan_request = Some(MentionScanRequest {
            anchor: Self::TAB_WARM_UP_ANCHOR,
            root: self.mention_scan_root.clone(),
        });
    }

    /// See [`Self::request_path_scan_warm_up`]'s own doc.
    const TAB_WARM_UP_ANCHOR: usize = usize::MAX;
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::AgentId;

    fn tempdir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "conway-state-mentions-test-{}-{}-{name}",
            std::process::id(),
            {
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                COUNTER.fetch_add(1, Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("tempdir must be creatable");
        path
    }

    /// Review finding (CRITICAL, round 1), the core architectural proof:
    /// opening a Path-mode mention with no fresh cache must NOT walk the
    /// filesystem on this call -- it must leave `mention_candidates` empty
    /// and queue a request for `App::run`'s loop to run off-loop instead.
    /// This is what makes `sync_mention` (reached from `input::handle_key`,
    /// synchronous inside `App::run`'s own `select!`) safe to call from
    /// there regardless of how slow a real walk might be: the walk simply
    /// never happens on this stack at all.
    #[test]
    fn a_fresh_path_mention_does_not_scan_synchronously() {
        let root = tempdir("no-sync-scan");
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        let mut state = AppState::new(AgentId::new());
        state.mention_scan_root = root.clone();

        state.input = "@mai".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);

        assert_eq!(state.mention_mode, Some(MentionMode::Path));
        assert!(
            state.mention_candidates.is_empty(),
            "candidates must stay empty until a background scan reports back"
        );
        assert!(state.mention_scan_pending());
        let request = state
            .take_pending_mention_scan_request()
            .expect("a cache-miss mention must queue a scan request");
        assert_eq!(request.root, root);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The full round trip, simulating what `App::run`/`app/mention_scan.rs`
    /// do with the request the test above proved gets queued: run the walk
    /// (here, inline, since this is a pure-state test with no `tokio`
    /// runtime) and hand the result to [`AppState::apply_mention_scan_result`]
    /// -- the SAME landing method the real background task's channel reply
    /// calls.
    #[test]
    fn applying_a_completed_scan_populates_the_still_open_mention() {
        let root = tempdir("apply-scan");
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        let mut state = AppState::new(AgentId::new());
        state.mention_scan_root = root.clone();

        state.input = "@mai".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);
        let request = state.take_pending_mention_scan_request().unwrap();
        let scan = mentions::scan_paths(&request.root, mentions::DEFAULT_SCAN_CAP);

        state.apply_mention_scan_result(request.anchor, request.root, scan);

        assert!(!state.mention_scan_pending());
        assert_eq!(state.mention_matches(), vec!["main.rs"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A reply for a mention the operator has since CLOSED (moved the
    /// cursor elsewhere) must not resurrect it -- only the cache benefits.
    #[test]
    fn a_stale_scan_reply_updates_the_cache_but_not_a_closed_mentions_candidates() {
        let root = tempdir("stale-reply");
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        let mut state = AppState::new(AgentId::new());
        state.mention_scan_root = root.clone();

        state.input = "@mai".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);
        let request = state.take_pending_mention_scan_request().unwrap();

        // The operator finishes the thought and moves on before the reply
        // arrives.
        state.input = "@mai looks good".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);
        assert!(!state.mention_open());

        let scan = mentions::scan_paths(&request.root, mentions::DEFAULT_SCAN_CAP);
        state.apply_mention_scan_result(request.anchor, request.root, scan);

        assert!(
            !state.mention_open(),
            "a stale reply must not reopen a mention the operator already left"
        );

        // But a FRESH mention right after reuses the cache the stale reply
        // just populated, rather than queuing a second walk.
        state.input = "@mai".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);
        assert_eq!(state.mention_matches(), vec!["main.rs"]);
        assert!(state.take_pending_mention_scan_request().is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Review finding (CRITICAL, round 1): re-spawning a walk for every
    /// `@`-token is wasteful when the tree has not had time to change --
    /// a second mention against the SAME root, opened right after the
    /// first one's scan landed, must reuse the cached result instead of
    /// queuing a new request.
    #[test]
    fn a_second_mention_within_the_ttl_reuses_the_cache_not_a_new_request() {
        let root = tempdir("reuse-cache");
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("markdown.rs"), "// md").unwrap();
        let mut state = AppState::new(AgentId::new());
        state.mention_scan_root = root.clone();

        state.input = "@m".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);
        let request = state.take_pending_mention_scan_request().unwrap();
        let scan = mentions::scan_paths(&request.root, mentions::DEFAULT_SCAN_CAP);
        state.apply_mention_scan_result(request.anchor, request.root, scan);

        // Close this mention, then open a DIFFERENT one against the same
        // root -- a file created in between must NOT appear, proving the
        // cache (not a fresh walk) answered it.
        state.input = "@m ".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);
        std::fs::write(root.join("mint.rs"), "// arrives after the cache was built").unwrap();

        state.input = "@m @ma".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);

        assert!(
            state.take_pending_mention_scan_request().is_none(),
            "a fresh cache entry must be reused, not re-walked"
        );
        assert!(
            !state.mention_candidates.contains(&"mint.rs".to_string()),
            "a file created after the cache was built must not retroactively appear"
        );
        assert!(state.mention_candidates.contains(&"main.rs".to_string()));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn leaving_the_token_closes_the_mention() {
        let mut state = AppState::new(AgentId::new());
        state.input = "@foo".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);
        assert!(state.mention_anchor.is_some());

        state.input = "@foo ".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);
        assert_eq!(state.mention_anchor, None);
        assert!(state.mention_candidates.is_empty());
    }

    /// Agent-mode mentions read the live tree in memory -- no filesystem
    /// walk, no background request, populated synchronously exactly as
    /// before the round-1 review fix (only Path mode changed).
    #[test]
    fn a_fork_mention_lists_agent_names_not_paths_and_never_scans() {
        let root = tempdir("fork-agents");
        std::fs::write(root.join("agent-named-file.rs"), "// decoy").unwrap();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.mention_scan_root = root.clone();

        state.input = "/fork @".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);

        assert_eq!(state.mention_mode, Some(MentionMode::Agent));
        assert_eq!(state.mention_candidates, vec![agent.to_string()]);
        assert!(state.take_pending_mention_scan_request().is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Plain-`Tab` completion (point 3) shares the SAME cache: a cache miss
    /// queues a warm-up request rather than scanning synchronously, and a
    /// fresh cache entry is read without re-walking.
    #[test]
    fn path_scan_cached_if_fresh_is_none_on_a_miss_and_queues_a_warm_up() {
        let root = tempdir("tab-cache-miss");
        std::fs::write(root.join("a.rs"), "// a").unwrap();
        let mut state = AppState::new(AgentId::new());
        state.mention_scan_root = root.clone();

        assert!(state.path_scan_cached_if_fresh().is_none());
        state.request_path_scan_warm_up();
        let request = state
            .take_pending_mention_scan_request()
            .expect("a cache miss must queue a warm-up request");

        let scan = mentions::scan_paths(&request.root, mentions::DEFAULT_SCAN_CAP);
        state.apply_mention_scan_result(request.anchor, request.root, scan);

        let cached = state
            .path_scan_cached_if_fresh()
            .expect("the warm-up's own reply must have populated the cache");
        assert_eq!(cached.paths, vec!["a.rs".to_string()]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_warm_up_request_does_not_replace_a_pending_one() {
        let mut state = AppState::new(AgentId::new());
        let first_root = state.mention_scan_root.clone();
        state.request_path_scan_warm_up();

        // Changing the root and asking again must NOT overwrite the
        // already-queued request -- only one request slot exists, and the
        // first one queued still wins until it is drained.
        state.mention_scan_root = first_root.join("somewhere-else");
        state.request_path_scan_warm_up();

        let request = state.take_pending_mention_scan_request().unwrap();
        assert_eq!(request.root, first_root);
    }
}
