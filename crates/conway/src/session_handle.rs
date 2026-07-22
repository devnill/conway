//! `SessionHandle`, `TurnHandle`, `SessionSpec`: the Slice 1 consumer-facing
//! surface over one running `conway-runtime::Runtime` session (WI-101).
//!
//! All `SessionHandle` methods are thin delegations to `Runtime`; no method
//! takes `&mut self` -- every state change routes through the runtime, not
//! through local mutation.
//!
//! **Relocation note (disclosed, per WI-100's own F-100-1 deviation #3):**
//! `SessionHandle` and `SessionSpec` were previously a minimal stub living
//! in `crate::conway` (WI-100 landed only `id()`/`root()`, with an explicit
//! comment that WI-101 owns moving them here). This file is that move,
//! plus the full surface this item specifies.

use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use conway_core::agent::{AgentResult, AgentTreeSnapshot, Budget};
use conway_core::content::{ContentBlock, ToolResult};
use conway_core::error::{RuntimeError, StoreError};
use conway_core::event::{Envelope, Event};
use conway_core::ids::{AgentId, LogSeq, RoleAlias, SeqRange, SessionId};
use conway_core::log::{LogRecord, SessionFilter};
use conway_core::ports::SessionStore;
use conway_core::provenance::ContextReport;
use conway_runtime::runtime::Runtime;
use futures_core::Stream;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::{ConwayError, Result};
use crate::event_stream::EventStream;

/// The parameters for `Conway::new_session`.
///
/// Every field defaults to `None`/`vec![]` via `#[derive(Default)]`;
/// `Conway::new_session` resolves each absent field from its `ConwayConfig`
/// at call time. `SessionSpec::default()` itself is necessarily
/// config-agnostic (`Default::default()` takes no arguments) -- the
/// "defaulted" shape described in terms of `config.default_role`/
/// `config.cwd`/`config.limits` describes the *effective*, post-resolution
/// session `new_session` produces, not the literal struct this type's
/// `Default` impl returns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionSpec {
    pub agent_def: Option<String>,
    pub role: Option<RoleAlias>,
    pub cwd: Option<PathBuf>,
    pub budget: Option<Budget>,
    pub labels: Vec<String>,
}

/// A live handle onto one running session: `id()`/`root()` are static, and
/// every other method is a thin delegation to the `Arc<Runtime>` this
/// `Conway` assembled. Cheap to `Clone` -- every field is `Arc`/`Copy`.
#[derive(Clone)]
pub struct SessionHandle {
    rt: Arc<Runtime>,
    session: SessionId,
    root: AgentId,
    store: Arc<dyn SessionStore>,
}

impl SessionHandle {
    pub(crate) fn new(
        rt: Arc<Runtime>,
        session: SessionId,
        root: AgentId,
        store: Arc<dyn SessionStore>,
    ) -> Self {
        Self {
            rt,
            session,
            root,
            store,
        }
    }

    pub fn id(&self) -> SessionId {
        self.session
    }

    pub fn root(&self) -> AgentId {
        self.root
    }

    /// Delegates to `Runtime::prompt(self.root, text)` with no
    /// transformation of `text`, then returns a [`TurnHandle`] over a
    /// broadcast subscription taken out *before* the prompt is appended --
    /// so the turn's own first events can never be missed by a
    /// subscribe-after-append race.
    pub async fn prompt(&self, text: impl Into<String>) -> Result<TurnHandle> {
        let stream = EventStream::live(self.session, Some(self.root), self.rt.subscribe());
        self.rt.prompt(self.root, text.into()).await?;
        Ok(TurnHandle::new(
            self.rt.clone(),
            self.session,
            self.root,
            stream,
        ))
    }

    /// Every envelope emitted for this session (no agent filter beyond
    /// that -- see `events_from`'s doc on why session alone is already
    /// agent-scoped in this architecture).
    pub fn events(&self) -> EventStream {
        EventStream::live(self.session, None, self.rt.subscribe())
    }

    /// Replays persisted envelopes for this session from `seq` onward, then
    /// switches to the live broadcast. See
    /// [`EventStream::replay_then_live`] and [`record_to_event`] for the
    /// disclosed reconciliations this method's replay batch depends on --
    /// in particular, what the replay/live junction does and does not
    /// guarantee about duplicates.
    ///
    /// Session-scoping note: `SessionStore` keys one session per agent
    /// (`SessionId` docs: "one agent's append-only log"), so a session's
    /// own live envelopes are already exactly that agent's envelopes --
    /// filtering by `session` alone (as `EventStream::live`/
    /// `replay_then_live` do) cannot admit another agent's events, since
    /// those are published under a different `SessionId`.
    pub async fn events_from(&self, seq: LogSeq) -> Result<EventStream> {
        // Subscribing before reading is what guarantees NO GAP: everything
        // broadcast from this instant onward is captured live, so nothing
        // can fall through the seam uncaptured. The cost is the mirror-image
        // risk -- a record persisted (and broadcast live) in the gap
        // between this subscribe and the store read below lands in *both*
        // the replay batch and on `live`. `EventStream::replay_then_live`
        // is handed `subscribed_at` precisely so it can detect and drop
        // that live-side duplicate at the junction; see its doc for the
        // mechanism and its disclosed residual gap.
        let live = self.rt.subscribe();
        let subscribed_at = Utc::now();
        let records = self
            .store
            .read(&self.session, SeqRange::new(seq, None))
            .await?;
        let replay = records
            .iter()
            .filter_map(record_to_event)
            .map(|(_, ts, event)| Envelope {
                seq: 0, // renumbered by `EventStream::replay_then_live`
                ts,
                session: self.session,
                agent: self.root,
                event,
            })
            .collect();
        Ok(EventStream::replay_then_live(
            self.session,
            None,
            replay,
            subscribed_at,
            live,
        ))
    }

    /// A snapshot of the whole agent tree this `Conway`'s `Runtime` knows
    /// about (`Runtime::tree`, sync, no I/O). Delegated unchanged: neither
    /// `Runtime::tree` nor `AgentTreeSnapshot` offers a way to scope the
    /// snapshot to one session's own subtree, so (matching this item's
    /// "thin delegation" objective) this method does not attempt to filter
    /// it -- disclosed rather than silently narrowed or widened.
    pub fn tree(&self) -> AgentTreeSnapshot {
        self.rt.tree()
    }

    /// Delegates to `Runtime::context_report`, which is itself synchronous
    /// (an in-memory read, no I/O) -- this method is `async` only to match
    /// the binding criterion's signature; there is nothing to await.
    pub async fn context_report(&self, agent: AgentId) -> Result<ContextReport> {
        Ok(self.rt.context_report(agent)?)
    }

    /// The *effective* transcript for `agent`: its own records, prefixed by
    /// its full fork ancestry (recursively resolved), matching
    /// `conway_session::TranscriptResolver::resolve`'s semantics.
    ///
    /// **Reconciliation (disclosed):** this item's binding notes name
    /// `conway_session::TranscriptResolver` as the mechanism. It cannot be
    /// used directly here: `crates/conway/Cargo.toml` (WI-096, out of this
    /// item's file scope) gates the `conway-session` dependency behind the
    /// optional `jsonl-store` feature, but `SessionHandle` is core surface
    /// and must stay feature-independent (this item's own test matrix runs
    /// `--no-default-features`). Depending on `conway_session::` here
    /// unconditionally would not compile under that configuration; gating
    /// just this method behind `#[cfg(feature = "jsonl-store")]` would
    /// silently remove a criterion-mandated method under a feature
    /// combination nothing else requires it to disappear under. Instead,
    /// this method (and its private helper, `resolve_prefix`) reimplements
    /// `TranscriptResolver`'s ancestry walk directly against
    /// `conway_core::ports::SessionStore` (always available, unconditional
    /// port trait) -- the same algorithm, without that type's LRU
    /// memoization (sibling forks each re-walk their shared prefix; a
    /// correctness/performance tradeoff, not a correctness gap).
    ///
    /// Also resolves `agent` to its owning `SessionId` first
    /// (`Runtime::agent_session`/`resolve_session`, the only existing
    /// AgentId -> SessionId lookups in this workspace, are both
    /// `pub(crate)` to `conway-runtime` and unreachable from here), by the
    /// same list-and-match fallback `Runtime::resolve_session`'s own doc
    /// describes as an accepted O(session count) MVP cost.
    pub async fn transcript(&self, agent: AgentId) -> Result<Vec<LogRecord>> {
        let session = self.resolve_agent_session(agent).await?;
        self.effective_transcript(session).await
    }

    async fn resolve_agent_session(&self, agent: AgentId) -> Result<SessionId> {
        if agent == self.root {
            return Ok(self.session);
        }
        let sessions = self.store.list(SessionFilter::default()).await?;
        sessions
            .into_iter()
            .find(|meta| meta.agent_id == agent)
            .map(|meta| meta.id)
            .ok_or(ConwayError::Runtime(RuntimeError::AgentNotFound { agent }))
    }

    async fn effective_transcript(&self, session: SessionId) -> Result<Vec<LogRecord>> {
        let head = self.store.head(&session).await?;
        self.resolve_prefix(session, head).await
    }

    /// Mirrors `conway_session::TranscriptResolver::resolve_prefix`: walks
    /// `session`'s fork ancestry (via `SessionMeta.origin`) up to a root,
    /// bounding each ancestor at its own fork point, then concatenates from
    /// the root down, ending with `session`'s own records up to `upto`.
    async fn resolve_prefix(&self, session: SessionId, upto: LogSeq) -> Result<Vec<LogRecord>> {
        const MAX_ANCESTRY_DEPTH: usize = 256;

        // Walk upward, collecting each level's (session, own-record-bound)
        // pair, then read root-to-leaf.
        let mut chain = vec![(session, upto)];
        loop {
            let (cur, _) = *chain
                .last()
                .expect("chain always has at least the starting session");
            let meta = self.store.meta(&cur).await?;
            match meta.origin {
                Some(origin) => chain.push((origin.parent, origin.at_seq)),
                None => break,
            }
            if chain.len() > MAX_ANCESTRY_DEPTH {
                return Err(ConwayError::Runtime(RuntimeError::Store(
                    StoreError::Corrupt {
                        session,
                        line: 0,
                        detail: format!("fork ancestry exceeds max depth ({MAX_ANCESTRY_DEPTH})"),
                    },
                )));
            }
        }

        let mut records = Vec::new();
        for (sid, bound) in chain.into_iter().rev() {
            let batch = self
                .store
                .read(&sid, SeqRange::new(LogSeq::ZERO, Some(bound)))
                .await?;
            records.extend(batch);
        }
        Ok(records)
    }
}

/// Synthesizes an `(seq, ts, Event)` from one persisted `LogRecord`, for
/// `SessionHandle::events_from`'s replay batch. Returns `None` for
/// `LogRecord::Header` (no `seq`, not a replayable occurrence) and for any
/// future variant this `#[non_exhaustive]` enum grows that this function
/// does not yet know about.
///
/// **Disclosed gap:** no committed mapping between `LogRecord` (persisted,
/// one entry per session-log line) and `Event` (the live, ephemeral
/// broadcast wire format) exists anywhere in this workspace -- confirmed by
/// grep, not merely unwritten. They are independent representations of
/// different cardinality: e.g. live, one `Assistant` record's worth of a
/// turn corresponds to a run of `TextDelta`s plus one `TurnFinished`, and
/// `UserTurn`/`ForkDirective`/`ParentSteer` have no `Event` counterpart at
/// all today. This function uses the one faithful mapping that does exist
/// where it exists -- `AgentResultRecord` -> `Event::AgentFinished`,
/// matching exactly what `conway-runtime`'s agent loop emits live for that
/// occurrence, and `Assistant` -> `Event::TurnFinished{usage, stop}`, same
/// rationale -- and falls back to `Event::AgentProgress{note}` (the one
/// variant that exists precisely for free-text informational replay) for
/// every record kind with no faithful equivalent, rather than inventing a
/// new `Event` variant outside this item's file scope (`conway-core` owns
/// that enum).
fn record_to_event(record: &LogRecord) -> Option<(LogSeq, DateTime<Utc>, Event)> {
    match record {
        LogRecord::Header(_) => None,
        LogRecord::UserTurn { seq, ts, text, .. } => Some((
            *seq,
            *ts,
            Event::AgentProgress {
                note: format!("user turn: {text}"),
            },
        )),
        LogRecord::Assistant {
            seq,
            ts,
            usage,
            stop,
            ..
        } => Some((
            *seq,
            *ts,
            Event::TurnFinished {
                usage: *usage,
                stop: *stop,
            },
        )),
        LogRecord::ToolCallRecord { seq, ts, call } => Some((
            *seq,
            *ts,
            Event::ToolCallProposed {
                call_id: call.call_id.clone(),
                tool: call.name.clone(),
                args: call.arguments.clone(),
            },
        )),
        LogRecord::ToolResultRecord { seq, ts, result } => Some((
            *seq,
            *ts,
            Event::ToolCallFinished {
                call_id: result.call_id.clone(),
                is_error: result.is_error,
                preview: tool_result_preview(result),
            },
        )),
        LogRecord::ForkDirective {
            seq, ts, text, by, ..
        } => Some((
            *seq,
            *ts,
            Event::AgentProgress {
                note: format!("fork directive from {by}: {text}"),
            },
        )),
        LogRecord::ParentSteer {
            seq,
            ts,
            text,
            from,
            ..
        } => Some((
            *seq,
            *ts,
            Event::AgentProgress {
                note: format!("parent steer from {from}: {text}"),
            },
        )),
        LogRecord::SystemNote {
            seq,
            ts,
            text,
            reason,
            ..
        } => Some((
            *seq,
            *ts,
            Event::AgentProgress {
                note: format!("{reason}: {text}"),
            },
        )),
        LogRecord::AgentResultRecord { seq, ts, result } => Some((
            *seq,
            *ts,
            Event::AgentFinished {
                result: result.clone(),
            },
        )),
        LogRecord::ContextReportRecord { seq, ts, report } => Some((
            *seq,
            *ts,
            Event::AgentProgress {
                note: format!(
                    "context report: {} segments, {} tokens",
                    report.segments.len(),
                    report.total_tokens_est
                ),
            },
        )),
        _ => None,
    }
}

/// The first text block's text, truncated to 200 chars -- mirrors
/// `conway-runtime`'s own live `ToolCallFinished.preview` derivation
/// (`crates/conway-runtime/src/tools/runner.rs`'s `preview_text`, private
/// to that crate and thus not reusable here).
fn tool_result_preview(result: &ToolResult) -> String {
    const PREVIEW_LIMIT: usize = 200;
    let text = result
        .blocks
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or_default();
    text.chars().take(PREVIEW_LIMIT).collect()
}

/// A prompt in flight: wraps one internal, agent-scoped [`EventStream`]
/// subscription taken out before the prompt was appended (see
/// `SessionHandle::prompt`).
pub struct TurnHandle {
    rt: Arc<Runtime>,
    session: SessionId,
    agent: AgentId,
    inner: AsyncMutex<TurnHandleInner>,
}

struct TurnHandleInner {
    stream: EventStream,
    /// `text()` stops draining as soon as it sees `Event::AgentFinished`
    /// (a turn that both streams and terminates the agent within the same
    /// generation), buffering the result here so a later `result()` call
    /// on the same handle resolves it instead of re-draining a stream that
    /// has nothing left to yield -- the mechanism the binding criterion
    /// asks for ("`text()` then `result()` on the same handle must not
    /// deadlock").
    buffered_result: Option<AgentResult>,
}

impl TurnHandle {
    fn new(rt: Arc<Runtime>, session: SessionId, agent: AgentId, stream: EventStream) -> Self {
        Self {
            rt,
            session,
            agent,
            inner: AsyncMutex::new(TurnHandleInner {
                stream,
                buffered_result: None,
            }),
        }
    }

    /// Concatenates every `Event::TextDelta` observed for this turn, up to
    /// (not including) the first `Event::TurnFinished` -- or, if the agent
    /// finishes within the same generation without a distinct
    /// `TurnFinished`, up to `Event::AgentFinished` (whose `AgentResult` is
    /// buffered for a subsequent `result()` call).
    pub async fn text(&self) -> Result<String> {
        let mut inner = self.inner.lock().await;
        let mut text = String::new();
        while let Some(envelope) = next_envelope(&mut inner.stream).await {
            match envelope.event {
                Event::TextDelta { text: delta } => text.push_str(&delta),
                Event::TurnFinished { .. } => break,
                Event::AgentFinished { result } => {
                    inner.buffered_result = Some(result);
                    break;
                }
                _ => {}
            }
        }
        Ok(text)
    }

    /// Resolves on `Event::AgentFinished` -- including when the terminal
    /// `AgentResult.status` is `BudgetExceeded` or `Cancelled`: both are
    /// still delivered as one `AgentFinished` event (architecture §8: every
    /// `AgentSpawned` is eventually followed by exactly one
    /// `AgentFinished`), never as a stream error.
    ///
    /// The `AgentNotFound` error below is not expected to occur in
    /// practice (it only fires if the runtime's broadcast bus itself ends,
    /// which happens only when every `Arc<Runtime>` -- including the one
    /// this handle and its owning `SessionHandle` hold -- has already been
    /// dropped); it exists so this method has a total, typed return rather
    /// than panicking on an unreachable-but-not-provably-impossible stream
    /// end.
    pub async fn result(&self) -> Result<AgentResult> {
        let mut inner = self.inner.lock().await;
        if let Some(result) = inner.buffered_result.take() {
            return Ok(result);
        }
        loop {
            match next_envelope(&mut inner.stream).await {
                Some(envelope) => {
                    if let Event::AgentFinished { result } = envelope.event {
                        return Ok(result);
                    }
                }
                None => {
                    return Err(ConwayError::Runtime(RuntimeError::AgentNotFound {
                        agent: self.agent,
                    }));
                }
            }
        }
    }

    /// A fresh, independent, live event subscription scoped to this turn's
    /// agent -- distinct from the internal stream `text()`/`result()`
    /// drain, so calling this does not consume events those methods still
    /// need (and vice versa).
    pub fn events(&self) -> EventStream {
        EventStream::live(self.session, Some(self.agent), self.rt.subscribe())
    }
}

async fn next_envelope(stream: &mut EventStream) -> Option<Envelope> {
    poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await
}
