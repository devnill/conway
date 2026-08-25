//! `SessionHandle`, `TurnHandle`, `SessionSpec`: the Slice 1 consumer-facing
//! surface over one running `conway-runtime::Runtime` session.
//!
//! All `SessionHandle` methods are thin delegations to `Runtime`; no method
//! takes `&mut self` -- every state change routes through the runtime, not
//! through local mutation.
//!
//! **Relocation note (disclosed, per its own deviation #3):**
//! `SessionHandle` and `SessionSpec` were previously a minimal stub living
//! in `crate::conway` (landed only `id()`/`root()`, with an explicit
//! comment noting the move). This file is that move,
//! plus the full surface this item specifies.

use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use conway_core::agent::{
    AgentDefRef, AgentResult, AgentTreeSnapshot, Budget, CancelMode, SubagentMode, SubagentSpec,
};
use conway_core::content::{ContentBlock, ToolResult, Usage};
use conway_core::error::{RuntimeError, StoreError};
use conway_core::event::{Envelope, Event};
use conway_core::ids::{AgentId, LogSeq, ModelRef, RoleAlias, SeqRange, SessionId};
use conway_core::log::{LogRecord, SessionFilter};
use conway_core::ports::{SessionStore, SubagentHost};
use conway_core::provenance::{ContextReport, Provenance};
use conway_runtime::runtime::Runtime;
use futures_core::Stream;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::{FacadeError, Result};
use crate::event_stream::EventStream;
use crate::subagent_spec::{ForkSpec, SpawnSpec};

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
    /// Overrides the store-assigned session id. `None` mints a
    /// fresh [`SessionId`] as before; `Some` is passed straight through to
    /// `RootSpec::session`, so `Runtime::start_root`'s own internal
    /// `store.create` becomes the single, authoritative creation call under
    /// exactly that id.
    pub id: Option<SessionId>,
    pub agent_def: Option<String>,
    pub role: Option<RoleAlias>,
    /// Pins the model for this session, overriding the role's chain.
    /// Passed straight through to `RootSpec::model`, which `start_root`
    /// prefers over the `agent_def`-sourced pin (see that field's own doc).
    pub model: Option<ModelRef>,
    pub cwd: Option<PathBuf>,
    pub budget: Option<Budget>,
    /// Replaces (`Some`) the root agent's own resolved system-prompt text
    /// outright -- `agent_def` above is still resolved for `role`/`tools`/
    /// `model` as usual; only the system-prompt segment's TEXT is swapped.
    /// `None` (the default) preserves the pre-existing behavior: the
    /// resolved `agent_def`'s own `system_prompt`, or no system-prompt
    /// segment at all when `agent_def` is also `None`. `conway-cli`'s
    /// `--system-prompt`/`--append-system-prompt` are this field's
    /// motivating caller -- see `RootSpec::system_prompt_override`
    /// (`conway-runtime`), which this passes straight through via
    /// `Conway::new_session`.
    pub system_prompt_override: Option<String>,
    pub labels: Vec<String>,
    /// Opt-in multi-turn keep-alive: passed straight through to
    /// `RootSpec::keep_alive` (`conway_runtime::runtime::RootSpec` -- see
    /// that field's own doc for the confirmed bug this fixes and why it
    /// must stay opt-in, not universal). `false` (`SessionSpec::default()`)
    /// preserves this crate's pre-existing behavior exactly: the session's
    /// root agent task terminates after its first `Completed` turn, and a
    /// second `SessionHandle::prompt` on the same handle silently runs no
    /// turn. `true` is what `conway-cli`'s TUI opts its own root session
    /// into, so a second chat message in the same process actually runs.
    pub keep_alive: bool,
    /// Overrides the resolved agent def's own tool selector, passed straight
    /// through to `RootSpec::tools`. `None` preserves `start_root`'s existing
    /// fallback (`spec.tools.or(agent_def.tools)` -- see that method's own
    /// doc): the agent def's declared tools, or every builtin tool if the def
    /// declares none. `conway-cli`'s TUI root uses `Some(ToolSelector::
    /// Except(vec!["report".into()]))` -- an interactive chat root has no
    /// parent to report an `AgentResult` to, so excluding the `report` tool
    /// makes the model answer in plain text instead of hitting the
    /// permission gate for a tool call nothing downstream ever unblocks.
    pub tools: Option<conway_core::agent::ToolSelector>,
    /// The schema this session's root agent's `structured` result must
    /// satisfy, passed straight through to `RootSpec::result_contract`
    /// (`conway_runtime::runtime::RootSpec` -- see that field's own doc
    /// for the enforcement mechanism: one corrective retry, then terminal
    /// `ResultStatus::Rejected { missing }`). `None` (the default)
    /// preserves this crate's pre-existing behavior exactly: a root agent
    /// carries no contract at all.
    ///
    /// **Precedence with `agent_def`:** when both this field and the
    /// resolved `agent_def`'s own `AgentDef::result_contract` are `Some`,
    /// THIS field wins outright -- the call-site contract is never merged
    /// with, and never loses to, a def-declared one. This mirrors
    /// `subagent.rs`'s already-established rule for a forked/spawned
    /// child's own contract precedence (`SubagentSpec::result_contract`
    /// over the spawning `AgentDef`'s), applied here to the one case that
    /// rule did not yet cover: the root agent itself. `conway-cli`'s
    /// `--output-schema` is this field's motivating caller -- see that
    /// flag's own help for the exact precedence with `--agent`.
    ///
    /// Use [`crate::compile_output_schema`] to build this from an
    /// arbitrary, caller-supplied JSON Schema document (a plain
    /// `serde_json::Value`, not necessarily `schemars`-generated) --
    /// the same compile-and-validate step `crate::agents::
    /// load_agent_defs` already applies to an agent def's own
    /// frontmatter-declared `result_contract`, generalized to accept a
    /// schema from any source.
    pub result_contract: Option<schemars::schema::RootSchema>,
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
    ///
    /// **Concurrent-call footgun:** if two `prompt` calls race and both land
    /// before the agent's own idle loop wakes to consume them, both
    /// `UserTurn` records are durably appended regardless (no data lost --
    /// whichever turn runs next re-reads the full session history and sees
    /// both), but the wake signal itself is a single-permit
    /// `tokio::sync::Notify` (`conway_runtime::agent_loop::ResumeGate`'s own
    /// doc) shared by the whole agent -- a second `notify_one()` before the
    /// first is consumed is not queued, just coalesced into the same
    /// permit. The practical effect: both callers' `TurnHandle`s each hold
    /// their own event subscription, but both end up observing the SAME
    /// underlying turn's `TurnFinished`/`AgentFinished`, not one turn each.
    /// Harmless for `conway-cli`'s TUI (input is strictly sequential -- one
    /// prompt in flight at a time) but a footgun for any caller issuing
    /// concurrent `prompt`s against the same session.
    pub async fn prompt(&self, text: impl Into<String>) -> Result<TurnHandle> {
        self.prompt_agent(self.root, text).await
    }

    /// [`Self::prompt`], generalized to any agent this handle's tree can
    /// reach -- not just `self.root`. Added for the interactive keep-alive
    /// session item (bare TUI `/spawn`/`/fork`): once a caller has an
    /// `AgentId` for an interactive keep-alive child (`SpawnSpec::
    /// keep_alive`/`ForkSpec::keep_alive`), this is how it drives that
    /// child's turns directly, exactly the way `prompt` drives the root's.
    ///
    /// `Runtime::prompt(agent, text)` already accepts any agent id (the
    /// generalization, unmodified by this item) -- this method is a thin
    /// wrapper that additionally resolves `agent`'s owning `SessionId` (via
    /// `Self::resolve_agent_session`, the same resolution
    /// [`Self::agent_events`] uses) so the returned [`TurnHandle`] is scoped
    /// correctly, and subscribes to the live bus BEFORE appending the
    /// prompt -- the same subscribe-before-act ordering [`Self::prompt`]'s
    /// own doc explains, generalized to `agent` instead of hardcoding
    /// `self.root`.
    pub async fn prompt_agent(
        &self,
        agent: AgentId,
        text: impl Into<String>,
    ) -> Result<TurnHandle> {
        let session = self.resolve_agent_session(agent).await?;
        let stream = EventStream::live(session, Some(agent), self.rt.subscribe());
        self.rt.prompt(agent, text.into()).await?;
        Ok(TurnHandle::new(self.rt.clone(), session, agent, stream))
    }

    /// [`Self::prompt_agent`], stamped [`Provenance::CommandPrompt`] instead
    /// of [`Provenance::UserPrompt`] -- the facade primitive
    /// `conway_core::ports::CommandOutcome::SubmitPrompt`'s own doc names as
    /// what the host actually does with it (board item
    /// `01M0VSMF71S6VXX81YRAAF5S8Q`, "No command can submit a prompt").
    ///
    /// **Reachable here, on `SessionHandle` itself, not only from
    /// `conway-cli`'s `App`** -- this is this item's own answer to its
    /// "port variant, not a renderer `Effect`" determine-first question
    /// (GP-05/C-03: no capability may exist in only one mode). Any caller
    /// holding a `SessionHandle` and a `CommandOutcome::SubmitPrompt` a
    /// `Command::invoke` returned -- the TUI's `App`, `conway-cli`'s
    /// one-shot `<plugin-id>.<command>` dispatch, or a bare library
    /// embedder with none of those -- fulfils it identically through this
    /// one method, never a TUI-only code path.
    ///
    /// `command` is the full command name that produced `text` (`plugin_id.
    /// bare_name`) -- attributed into the stamped `Provenance::
    /// CommandPrompt { command }` so the durable log can tell a
    /// command-submitted turn apart from an operator-typed one, and name
    /// which command it was.
    pub async fn prompt_command(
        &self,
        agent: AgentId,
        text: impl Into<String>,
        command: impl Into<String>,
    ) -> Result<TurnHandle> {
        let session = self.resolve_agent_session(agent).await?;
        let stream = EventStream::live(session, Some(agent), self.rt.subscribe());
        self.rt
            .prompt_with_provenance(
                agent,
                text.into(),
                Provenance::CommandPrompt {
                    command: command.into(),
                },
            )
            .await?;
        Ok(TurnHandle::new(self.rt.clone(), session, agent, stream))
    }

    /// The `/ask` fork-ask primitive: forks this session's root agent at its
    /// CURRENT head into a fresh, ephemeral child -- inheriting this
    /// session's entire context, agent-def system prompt, and tool set,
    /// since a fork always does -- and drives the child's first (and only)
    /// turn with `text`.
    ///
    /// **Attach semantics (B2):** the child goes through the
    /// runtime's own subagent machinery (`SubagentHost::start`, the same
    /// path `SessionHandle::fork` and the `conway_ask` tool already use),
    /// NOT the `fork_child` -> `resume_root` sequence this method used
    /// before B2. That means the child attaches as a proper fork child of
    /// the asker -- `kind: Some(SubagentMode::Fork)`, `parent: Some(
    /// self.root)`, `inherited_upto: Some(<fork-point seq>)` -- and
    /// `AgentTree::attach` emits `Event::AgentSpawned { kind: Fork,
    /// parent: Some(asker), ephemeral: true, .. }` on the live bus, so the
    /// TUI's tree view shows the node (ephemeral children stay
    /// attached and visible to provenance; never-attach was rejected). The
    /// old path attached the child as a `kind: None` root with no
    /// `AgentSpawned` at all.
    ///
    /// Returns a [`TurnHandle`] over the CHILD, in the same shape `prompt`
    /// returns one over `self` -- so a caller (e.g. the `/ask` TUI command)
    /// subscribes and renders it exactly like a normal prompt turn, with no
    /// special-casing. The raw bus subscription is taken out BEFORE `start`
    /// launches the child (the same subscribe-before-launch ordering
    /// `prompt_agent` and `SubagentHost::ask`'s own doc explain), so the
    /// child's first `TextDelta` cannot be missed. `self`'s own transcript
    /// is untouched: no record is appended to `self.session` -- the
    /// question lands in the CHILD's log as its `ForkDirective` head
    /// record, and fork semantics bound the child at its fork seq (the
    /// parent only ever reads its own ancestry, never a child's turns).
    ///
    /// The child is born with `SessionMeta::ephemeral: true` (via
    /// `SubagentSpec::ephemeral`, which `SubagentHost::start` threads into
    /// the forked header and the attached `AgentNode` verbatim) -- set
    /// once, at fork time, in the child's own header; the only way to flip
    /// it later is the one-way promote (`Conway::promote`, B3 -- the `/ask`
    /// modal's `[f]` "keep" fate), the store's single sanctioned header
    /// mutation. This is what keeps it out of `Conway::sessions`'s
    /// default listing and `SessionStore::children` -- see those methods'
    /// own docs for the default-exclude filtering this depends on. The
    /// child remains reachable through this call's own returned
    /// `TurnHandle`, through [`Self::tree`]'s snapshot (it stays attached
    /// under the asker), or via `SessionFilter{include_ephemeral: true, ..}`.
    ///
    /// **Agent-def/role/tool inheritance:** `agent_def` is carried over
    /// from this session's own `SessionMeta` (what the pre-B2
    /// `fork_child` fallback did), so the child resolves the same system
    /// prompt and tool selector this session's own agent runs with;
    /// `role: None` lets `SubagentHost::start` inherit the parent's
    /// effective role itself (its inheritance fallback). Tool calls the child
    /// makes during the ask are real and permanent -- only the transcript
    /// is ephemeral; that is intended, not a gap (a throwaway *question*
    /// does not imply throwaway tool side effects).
    ///
    /// **Not keep-alive:** `keep_alive: false` matches the pre-B2 behavior
    /// exactly (a resumed root always terminated on its first `Completed`
    /// turn) -- the child finishes after answering, which is what makes the
    /// returned `TurnHandle`'s `result()` resolve.
    ///
    /// **Disclosed simplification:** the child's `budget` is always
    /// `Budget::default()` (`conway-core`'s baseline), not this session's own
    /// configured budget -- `SessionHandle` has no reference to that
    /// configuration to read it from, the same disclosed deviation
    /// `ForkSpec::new`'s own doc already makes for `SessionHandle::fork`.
    ///
    /// **Cancellation:** the child's cancel token is a structural child of
    /// the asker's (`tree.child_cancel_token`), so cancelling the asker (or
    /// its subtree) cancels an in-flight ask too — post-B2 behavior,
    /// matching `conway_ask` children (pre-B2 the child survived parent
    /// cancellation).
    pub async fn ask(&self, text: impl Into<String>) -> Result<TurnHandle> {
        let parent_meta = self.store.meta(&self.session).await?;
        let spec = SubagentSpec {
            mode: SubagentMode::Fork,
            prompt: text.into(),
            // Inherit this session's own agent def (system prompt + tool
            // selector); `SubagentHost::start` resolves it through the same
            // registry the parent's own start did.
            agent_def: parent_meta.agent_def.map(AgentDefRef),
            // `None` -> the runtime inherits the parent's effective role
            // (`subagent.rs`'s inheritance fallback), same routing as the asker.
            role: None,
            // `None` -> the runtime inherits the parent's (possibly
            // inherited-def) model pin, unchanged -- an ask is not a model
            // switch.
            pin: None,
            tools: None,
            budget: Budget::default(),
            result_contract: None,
            keep_alive: false,
            ephemeral: true,
            // B5: tag this child as MODAL-ask residue (the TUI's `/ask
            // <prompt>` modal drives this method) -- DISTINCT from a
            // `conway_ask` tool child (`AskOrigin::ToolAsk`, set in
            // `conway-tools`' `AskTool`). The TUI's startup crash-residue
            // sweep (`Conway::sweep_stale_modal_asks`) purges only
            // `ModalAsk`-tagged leftovers; a tool-ask child's
            // `EphemeralSessionRef` artifact would dangle if it were ever
            // swept (see `conway_core::log::AskOrigin`'s own doc).
            ask_origin: Some(conway_core::log::AskOrigin::ModalAsk),
            // A fork inherits the asker's entire context (C1's rationale for
            // never exposing cwd on `ForkSpec`) -- inherit its cwd too.
            cwd: None,
            // (S3) Same rationale -- inherit its root too.
            root: None,
            // A modal `/ask` has no
            // embedder-supplied correlation identifier of its own -- no tag.
            tag: None,
            // `[S1.5]`: same rationale as `cwd`/`root` above -- inherit the
            // asker's per-agent plugin config too.
            plugin_config: None,
            // An ask is fork+await-text, never a chosen-context fork: `None`
            // preserves the pre-existing "inherit the asker's entire
            // context" behavior unchanged -- see `SubagentSpec::context`'s
            // own doc.
            context: None,
        };
        // Subscribe BEFORE `start` so the child's first events cannot race
        // past this handle's stream (see the doc above).
        let live = self.rt.subscribe();
        // `caller` and `parent`
        // are both `self.root` -- a modal `/ask` always forks the SESSION's
        // own root, mirroring `steer`/`await_agent`/`cancel`'s own
        // root/operator-exemption doc below.
        let child = SubagentHost::start(self.rt.as_ref(), self.root, self.root, spec)
            .await
            .map_err(FacadeError::Runtime)?;
        // The child is already attached (start -> launch_agent -> attach),
        // so its session is listable here; `resolve_agent_session`'s
        // ephemeral-inclusive lookup is exactly the "resolve an agent id the
        // caller legitimately holds" case its own doc describes.
        let child_session = self.resolve_agent_session(child).await?;
        let stream = EventStream::live(child_session, Some(child), live);
        Ok(TurnHandle::new(
            self.rt.clone(),
            child_session,
            child,
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
    /// `EventStream::replay_then_live` and `record_to_event` for the
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

    /// Observes ONE specific agent's own conversation: that agent's OWN
    /// records (`[0, head)` of its own session, NOT the ancestry-prefixed
    /// effective view -- see the "Own records only" note below), replayed
    /// as synthesized envelopes, followed by that agent's live event stream
    /// ("switching the focused agent switches the transcript to
    /// that agent's conversation").
    ///
    /// This is [`SessionHandle::events_from`]'s own no-gap-first ordering,
    /// generalized from "this handle's own root" to an arbitrary `agent`
    /// this handle's tree can reach: subscribe to the live bus BEFORE
    /// reading the persisted store, so nothing broadcast in between is
    /// missed, then let `EventStream::replay_then_live`'s junction-dedup
    /// drop the resulting duplicate. See that method's doc for the
    /// mechanism and its disclosed residual gap; see `record_to_event`'s
    /// doc for the (also disclosed) `LogRecord` -> `Event` mapping the
    /// replay batch is built from -- the SAME mapping `events_from` uses,
    /// not a parallel one.
    ///
    /// **Deviation from the suggested `fn agent_events(&self, agent:
    /// AgentId) -> EventStream` shape (disclosed):** that signature cannot
    /// be implemented as written. Resolving `agent` to its owning
    /// `SessionId` (an as-yet-not-necessarily-live agent may belong to a
    /// DIFFERENT session than `self.session` -- see
    /// `SessionHandle::resolve_agent_session`) and reading its own
    /// records both require `SessionStore` I/O, which is `async` and
    /// fallible (`Err` on an unknown/foreign `agent`, exactly like
    /// [`SessionHandle::transcript`]). This method is `pub async fn ... ->
    /// Result<EventStream>` instead -- the same shape `events_from` already
    /// has, for the same reason.
    ///
    /// **Finished-agent edge case:** if `agent` has already finished, this
    /// still returns `Ok` -- the replay batch is that agent's complete,
    /// final transcript, and the live half of the returned stream simply
    /// never yields another envelope for it (a finished agent's task has
    /// already stopped emitting). A caller polling this stream does not
    /// hang; it just sees no further growth, which is the correct
    /// behavior for a conversation that is genuinely over.
    ///
    /// **Ephemeral agents:** `agent` is resolved the same way
    /// [`SessionHandle::transcript`] resolves one -- via
    /// `resolve_agent_session`, which searches with
    /// `SessionFilter{include_ephemeral: true, ..}` -- so an `/ask`
    /// scratchpad child (hidden from listings, but a caller who already
    /// holds its `AgentId` may still resolve it) is observable here too.
    ///
    /// **Own records only, NOT the ancestry-prefixed effective transcript
    /// (bug 4 fix):** the replay batch
    /// below reads `agent`'s own session, `[0, head)` -- exactly the same
    /// read [`Self::session_usage`] already performs, and for the same
    /// reason that method's doc explains: an inherited fork/spawn prefix is
    /// the PARENT's own prior conversation, not this agent's. Building the
    /// replay from `Self::effective_transcript` (as this method used to)
    /// prepended that parent conversation ahead of the focused agent's own
    /// records, so switching focus to a spawned or forked child appeared to
    /// show "the previous chat log" -- the parent's, not the child's. This
    /// method now shows the SAME view uniformly for spawn and fork alike;
    /// per the governing decision, a user who wants the inherited/shared
    /// history focuses the parent instead. [`Self::transcript`] is
    /// unaffected -- it still returns the ancestry-prefixed effective view,
    /// which remains the correct answer for callers that explicitly want
    /// the full lineage.
    pub async fn agent_events(&self, agent: AgentId) -> Result<EventStream> {
        let session = self.resolve_agent_session(agent).await?;
        // Subscribe first, exactly as `events_from` does -- see that
        // method's own doc for why this ordering (not read-then-subscribe)
        // is what prevents a silent gap at the cost of a detectable,
        // dedupable duplicate.
        let live = self.rt.subscribe();
        let subscribed_at = Utc::now();
        let head = self.store.head(&session).await?;
        let records = self
            .store
            .read(&session, SeqRange::new(LogSeq::ZERO, Some(head)))
            .await?;
        let replay = records
            .iter()
            .filter_map(record_to_event)
            .map(|(_, ts, event)| Envelope {
                seq: 0, // renumbered by `EventStream::replay_then_live`
                ts,
                session,
                agent,
                event,
            })
            .collect();
        Ok(EventStream::replay_then_live(
            session,
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

    /// Board `01M0VWMMEG4CER8Y8VH77KZ0CV` ("focusing back onto a streaming
    /// agent mid-turn loses the working indicator"): whether `agent` has a
    /// model round-trip in flight RIGHT NOW, straight from `Runtime::
    /// turn_in_flight` (sync, no I/O, like `Self::tree` immediately above).
    ///
    /// **This is the chosen fix's whole surface (P-8/GP-05: a behavioural
    /// difference between modes is a renderer bug, so this belongs on the
    /// facade, not bolted onto the TUI's own `AppState` alone).** The item's
    /// three candidates were: (1) a runtime query -- this method, (2) make
    /// `Event::TurnStarted` survive resubscription (persisted or
    /// synthesized), (3) track it in the TUI. Option 3 was checked and
    /// falsified first: the TUI's own live subscription is *always* scoped
    /// to at most one agent at a time (`Self::events`/`Self::agent_events`
    /// both filter on `self.agent`/`agent`, and the interactive app loop
    /// only ever holds one such stream, swapped wholesale on every focus
    /// switch) -- it structurally cannot observe `TurnStarted`/
    /// `TurnFinished` for an agent it is not currently subscribed to, so
    /// there is nothing for a per-agent "seen it" map to accumulate for an
    /// agent the operator is not looking at. That collapses the choice to
    /// (1) or (2).
    ///
    /// **Deliberately NOT `NodeStatus::Running`/`AgentStatus::Running`,
    /// which the item's own text names as a trap.** That status answers
    /// "has a terminal result been published" -- `true` for a keep-alive
    /// agent's entire idle-between-prompts lifetime, exactly the case a
    /// pull-in merges into, so seeding from it would reinstate the
    /// permanent wedge `01M0VQ650R31MGTXD8E225RRFH` fixed.
    /// `AgentTree::turn_in_flight` (this method's source) answers a
    /// strictly narrower question instead -- see its own doc for the
    /// `mark_turn_started`/`mark_turn_finished` bracket and why an
    /// error/cancelled/budget-exceeded exit cannot leave it stuck `true`.
    ///
    /// **Not a wire-format change.** No new `LogRecord`/`Event` variant,
    /// nothing persisted, nothing added to the replay path -- purely
    /// in-process bookkeeping inside `conway-runtime`'s `AgentTree`,
    /// queried fresh at focus time. Chosen over option 2 (persisting a
    /// turn boundary) precisely to avoid that wire-format commitment for
    /// what is, today, a UI-indicator-only need.
    pub fn turn_in_progress(&self, agent: AgentId) -> bool {
        self.rt.turn_in_flight(agent)
    }

    /// Delegates to `Runtime::context_report`, which is itself synchronous
    /// (an in-memory read, no I/O) -- this method is `async` only to match
    /// the binding criterion's signature; there is nothing to await.
    pub async fn context_report(&self, agent: AgentId) -> Result<ContextReport> {
        Ok(self.rt.context_report(agent)?)
    }

    /// The persisted `ContextReport` for `agent`'s historical `turn`
    /// (carried from the capstone review-adjacent): a thin
    /// delegation to `Runtime::context_report_at`, which -- unlike
    /// `context_report` above -- reads durably from the store rather than
    /// the live `last_report` slot, so it works even across a process
    /// restart (see that method's own doc for `resolve_session`'s
    /// live-then-store-scan fallback).
    pub async fn context_report_at(&self, agent: AgentId, turn: u32) -> Result<ContextReport> {
        Ok(self.rt.context_report_at(agent, turn).await?)
    }

    /// T3 follow-up: [`Self::context_report`], with the resumed-session gap
    /// closed -- a thin delegation to `Runtime::context_report_current`,
    /// which falls back to the most recently PERSISTED report when this
    /// process has no live one for `agent` yet (most commonly: a session
    /// resumed from a prior process, focused before it has run any turn in
    /// THIS one). Prefer this over `context_report` for any caller that
    /// wants "the true current total, freshest available" rather than
    /// specifically "only what this process has itself observed live" --
    /// see `Runtime::context_report_current`'s own doc for the exact
    /// fallback rule and why it is additive rather than a change to
    /// `context_report` itself.
    pub async fn context_report_current(&self, agent: AgentId) -> Result<ContextReport> {
        Ok(self.rt.context_report_current(agent).await?)
    }

    /// The `ModelRef` that served `agent`'s most recently completed turn
    /// (T3 follow-up: the TUI status line's `focused_model`, re-fetchable
    /// on a focus switch instead of staying blank until the newly focused
    /// agent's own next LIVE turn). `None` if `agent` has not yet completed
    /// any turn.
    ///
    /// Reads `agent`'s own session log directly and returns the `model` of
    /// the LAST `LogRecord::Assistant` record found -- the same
    /// resolve-session-then-scan-the-log shape [`Self::session_usage`]
    /// already uses, rather than any live in-memory routing state. That
    /// makes it durable across a process restart the same way
    /// `session_usage` already is: a resumed session's last assistant
    /// record is exactly as available as a live one's.
    ///
    /// **Why an explicit accessor, not a synthesized `Event::ModelDecision`
    /// out of `record_to_event` below:** `LogRecord::Assistant::route_reason`
    /// is deliberately loosely-typed (`serde_json::Value` -- see
    /// `conway_core::log`'s own module doc: "the reason as data; typed
    /// access is via `Event::ModelDecision`"). Synthesizing a replay-time
    /// `ModelDecision` event would need to reconstruct a `RoleAlias` and a
    /// `RoutingReason` from that untyped value -- a decoder for a shape the
    /// writer never committed to keeping stable, maintained only for this
    /// one call site. This accessor needs none of that: it reads the one
    /// field (`model`) the caller actually wants, directly off the same
    /// record.
    pub async fn last_model(&self, agent: AgentId) -> Result<Option<ModelRef>> {
        let session = self.resolve_agent_session(agent).await?;
        let head = self.store.head(&session).await?;
        let records = self
            .store
            .read(&session, SeqRange::new(LogSeq::ZERO, Some(head)))
            .await?;
        Ok(records.iter().rev().find_map(|record| match record {
            LogRecord::Assistant { model, .. } => Some(model.clone()),
            _ => None,
        }))
    }

    /// Cumulative token spend for `agent`'s OWN turns (/// -- the TUI status line's per-agent
    /// counter): sums the `usage` of every `LogRecord::Assistant` record in
    /// `agent`'s own session log.
    ///
    /// **Deliberately NOT [`Self::transcript`]'s effective (ancestry-
    /// prefixed) view:** a forked/spawned child's inherited prefix is the
    /// FORKER's own prior turns -- those tokens were already spent (and
    /// already counted) under the forker's own `session_usage`, not this
    /// agent's. Summing the effective transcript here would double-count
    /// the same tokens under two different agents' cumulative totals, so
    /// this reads only `agent`'s own session records, `[0, head)`, the same
    /// way `Self::effective_transcript`'s inner read does for one link of
    /// the chain -- just without walking `SessionMeta.origin` at all.
    ///
    /// **Not [`Self::context_report`]'s `total_tokens_est`:** that number is
    /// context-WINDOW occupancy (what the NEXT turn's prompt would cost),
    /// not cumulative spend across turns already run -- a different
    /// question with a different answer, disclosed here so a future reader
    /// does not "simplify" this into a call to that method.
    pub async fn session_usage(&self, agent: AgentId) -> Result<Usage> {
        let session = self.resolve_agent_session(agent).await?;
        let head = self.store.head(&session).await?;
        let records = self
            .store
            .read(&session, SeqRange::new(LogSeq::ZERO, Some(head)))
            .await?;
        Ok(records
            .iter()
            .filter_map(|record| match record {
                LogRecord::Assistant { usage, .. } => Some(*usage),
                _ => None,
            })
            .fold(Usage::default(), |acc, usage| acc + usage))
    }

    /// The *effective* transcript for `agent`: its own records, prefixed by
    /// its full fork ancestry (recursively resolved), matching
    /// `conway_core::transcript::TranscriptResolver::resolve`'s semantics.
    ///
    /// **Reconciliation (disclosed):** this item's binding notes name
    /// `TranscriptResolver` as the mechanism (at the time, `conway_session::
    /// TranscriptResolver`; since board item `01KZVYVTVWRH20R6VJ6G3SWTJ6`,
    /// `conway_core::transcript::TranscriptResolver`, always available since
    /// `conway-core` has no features). It is still not used directly here:
    /// `SessionHandle` is core surface and must stay allocation-cheap per
    /// call rather than own a persistent, per-instance cache. Instead, this
    /// method (and its private helper, `resolve_prefix`) reimplements
    /// `TranscriptResolver`'s ancestry walk directly against
    /// `conway_core::ports::SessionStore` -- the same algorithm, without
    /// that type's LRU memoization (sibling forks each re-walk their shared
    /// prefix; a correctness/performance tradeoff, not a correctness gap).
    ///
    /// Also resolves `agent` to its owning `SessionId` first
    /// (`Runtime::agent_session`/`resolve_session`, the only existing
    /// AgentId -> SessionId lookups in this workspace, are both
    /// `pub(crate)` to `conway-runtime` and unreachable from here), by the
    /// same list-and-match fallback `Runtime::resolve_session`'s own doc
    /// describes as an accepted O(session count) MVP cost.
    ///
    /// **Spawned children show a parent prefix here** even though their
    /// *context* is clean-slate -- see [`SessionHandle::spawn`]'s doc for
    /// why (`SessionMeta.origin` is recorded on spawn too, and this
    /// method's ancestry walk does not distinguish fork from spawn).
    pub async fn transcript(&self, agent: AgentId) -> Result<Vec<LogRecord>> {
        let session = self.resolve_agent_session(agent).await?;
        self.effective_transcript(session).await
    }

    async fn resolve_agent_session(&self, agent: AgentId) -> Result<SessionId> {
        if agent == self.root {
            return Ok(self.session);
        }
        // `include_ephemeral: true` -- this is an identity lookup (does
        // `agent` belong to some session this store knows about?), not a
        // catalog browse; the default exclude-ephemeral filter exists to
        // hide `/ask` scratchpads from *listings* (`Conway::sessions`,
        // `sessions tree`), not to make them unresolvable by an agent id a
        // caller already legitimately holds (e.g. from `SessionHandle::ask`'s
        // own returned `TurnHandle`).
        let sessions = self
            .store
            .list(SessionFilter {
                include_ephemeral: true,
                ..SessionFilter::default()
            })
            .await?;
        sessions
            .into_iter()
            .find(|meta| meta.agent_id == agent)
            .map(|meta| meta.id)
            .ok_or(FacadeError::Runtime(RuntimeError::AgentNotFound { agent }))
    }

    async fn effective_transcript(&self, session: SessionId) -> Result<Vec<LogRecord>> {
        let head = self.store.head(&session).await?;
        self.resolve_prefix(session, head).await
    }

    /// Mirrors `conway_core::transcript::TranscriptResolver::resolve_prefix`: walks
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
                return Err(FacadeError::Runtime(RuntimeError::Store(
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

// ---------------------------------------------------------------------
// Subagent surface (fork/spawn/steer/await_agent/cancel).
//
// Pure delegation to `Runtime`'s `impl SubagentHost` (conway-runtime)
// -- see `crate::subagent_spec` for `ForkSpec`/`SpawnSpec` and their `From`
// conversions into `conway_core::agent::SubagentSpec`. Fork and spawn stay
// visibly distinct types: `fork` inherits the forker's entire context plus a
// directive; `spawn` is clean-slate -- kept as distinct methods/types rather
// than one mode-flagged call, matching `ForkSpec`/`SpawnSpec`'s own split.
//
// This block intentionally names none of the four storage/tree-internal
// port and helper types fork/spawn *logic* would touch (the session-log
// port, the fork-ancestry resolver, the context assembler, or the
// in-memory multi-agent tree type) -- that logic lives in conway-runtime,
// not here; this block only calls through `SubagentHost` and reads
// `Runtime::tree()`'s already-public snapshot. `tests/
// session_handle_subagent.rs` greps everything from this marker onward to
// check that (see that test for the exact identifiers, deliberately not
// spelled out here so the rule this comment describes doesn't itself trip
// the check it describes). It is scoped to this block rather than the
// whole file because the methods above this marker (unmodified by
// this item) already reference one of those types legitimately, for a
// concern this item has nothing to do with.
// ---------------------------------------------------------------------
impl SessionHandle {
    /// Forks a live agent: the "inherit everything, plus a directive"
    /// mode. Delegates to `Runtime::start` (`impl SubagentHost`) with
    /// `spec.into()` unmodified beyond the `ForkSpec` -> `SubagentSpec`
    /// conversion itself.
    ///
    /// **T-1 (disclosed, unresolved in the architecture):** the fork
    /// overflow policy -- what happens when the inherited context plus
    /// `spec.directive` already exceeds the target model's window -- has no
    /// settled design. This method does not add an `on_overflow` field or
    /// otherwise paper over it, but the rejection does NOT surface here:
    /// `start` (`impl SubagentHost`) launches the child's `AgentLoop` as a
    /// background task and returns `Ok(AgentId)` as soon as it is attached,
    /// before the loop's first turn ever runs -- a T-1 overflow is only
    /// detected once that turn actually attempts routing. So this method
    /// returns `Ok` even for a fork doomed to overflow; the rejection
    /// arrives later, as `ResultStatus::Failed { error }` on the child's own
    /// terminal `AgentResult` (`agent_loop.rs`'s `route_and_attempt`,
    /// `finish_error`), naming the shortfall in `error`'s text. `RuntimeError::
    /// ForkContextOverflow` is NOT the live path for this: it is never
    /// constructed anywhere outside `conway-core`'s own error-taxonomy
    /// tests -- the real T-1 rejection is always a `RoutingError::
    /// ContextTooLarge` surfacing through the child's `AgentResult`, not an
    /// `Err` this method itself returns.
    ///
    /// Rejects `from` with `Err(FacadeError::Runtime)` when it does not
    /// belong to this session's agent tree -- see
    /// `SessionHandle::ensure_agent_in_session`'s doc for exactly what error
    /// that produces (`RuntimeError::AgentNotFound` vs. `AgentNotInSession`).
    ///
    /// **`caller`:** `self.root` is
    /// passed as `caller` to the trait's own `caller`-owns-`parent` check --
    /// see `steer`'s own doc for the root/operator-exemption mechanism this
    /// mirrors exactly. Not a bypass: `from` was already proven to be in
    /// `self.root`'s subtree by `ensure_agent_in_session` above, so the
    /// trait's check always succeeds for a call that reaches this point.
    pub async fn fork(&self, from: AgentId, spec: ForkSpec) -> Result<AgentId> {
        self.ensure_agent_in_session(from)?;
        self.rt
            .start(self.root, from, spec.into())
            .await
            .map_err(FacadeError::Runtime)
    }

    /// Spawns a fresh agent: the clean-slate mode. Delegates to
    /// `Runtime::start` (`impl SubagentHost`) with `spec.into()` unmodified
    /// beyond the `SpawnSpec` -> `SubagentSpec` conversion itself.
    ///
    /// Rejects `from` with `Err(FacadeError::Runtime)` when it does not
    /// belong to this session's agent tree -- see
    /// `SessionHandle::ensure_agent_in_session`'s doc.
    ///
    /// **`caller`:** `self.root` is
    /// passed as `caller`, exactly like [`Self::fork`] above -- see that
    /// method's own doc for why this is not a bypass.
    ///
    /// **Transcript quirk (disclosed):** "clean-slate" describes the
    /// spawned child's *context* only -- `inherited` stays `None`, so
    /// nothing from `from`'s history is fed to the model. It does not
    /// describe [`SessionHandle::transcript`]'s output for that child:
    /// `SessionMeta.origin` is still recorded on spawn (for tree
    /// reconstructability), and `transcript`'s ancestry walk follows any
    /// `Some(origin)` unconditionally, without distinguishing fork from
    /// spawn. So a spawned child's *transcript* still shows the parent's
    /// prefix, even though its *context* never did. An embedder building a
    /// history UI from `transcript()` should account for this.
    pub async fn spawn(&self, from: AgentId, spec: SpawnSpec) -> Result<AgentId> {
        self.ensure_agent_in_session(from)?;
        self.rt
            .start(self.root, from, spec.into())
            .await
            .map_err(FacadeError::Runtime)
    }

    /// Delivers `text` to `target` as a steer message, landing at `target`'s
    /// next turn boundary. Delegates to `Runtime::steer` (`impl
    /// SubagentHost`) with `text` converted and otherwise unmodified.
    ///
    /// Rejects `target` with `Err(FacadeError::Runtime)` when it does not
    /// belong to this session's agent tree -- `Arc<Runtime>` (and its
    /// runtime-wide, unscoped `tree()`) is shared across every
    /// `SessionHandle` a `Conway` produces, so without this check any handle
    /// could steer another session's agent. See
    /// `SessionHandle::ensure_agent_in_session`'s doc for exactly what error
    /// that produces (`RuntimeError::AgentNotFound` vs. `AgentNotInSession`).
    ///
    /// **Root/operator exemption:**
    /// `self.root` is passed as `caller` to the trait's own descendancy
    /// check (`SubagentHost`'s own doc). This is not a bypass -- `target`
    /// was already proven to be in `self.root`'s subtree by
    /// `ensure_agent_in_session` above, so the trait's check always
    /// succeeds for a call that reaches this point; it exists so this
    /// operator/embedder path (never reachable from model-supplied tool
    /// arguments) and the model-invoked `conway_steer` tool are enforced by
    /// the exact same mechanism, not two different ones.
    pub async fn steer(&self, target: AgentId, text: impl Into<String>) -> Result<()> {
        self.ensure_agent_in_session(target)?;
        self.rt
            .steer(self.root, target, text.into())
            .await
            .map_err(FacadeError::Runtime)
    }

    /// Awaits `target`'s terminal result. Always resolves `Ok` -- including
    /// when `target` finished `BudgetExceeded` or `Cancelled` -- since the
    /// runtime's supervisor guarantees a result is published no matter how
    /// the agent ends; only an unknown `target` produces `Err`. Delegates to
    /// `Runtime::await_result` (`impl SubagentHost`) unmodified.
    ///
    /// Rejects `target` with `Err(FacadeError::Runtime)` when it does not
    /// belong to this session's agent tree -- `AgentResult` is another
    /// session's data, and reading it across the session boundary is an
    /// isolation violation just as steering/cancelling it would be. See
    /// `SessionHandle::ensure_agent_in_session`'s doc.
    ///
    /// Passes `self.root` as `caller` -- see `steer`'s own doc for the
    /// root/operator-exemption mechanism this mirrors exactly.
    pub async fn await_agent(&self, target: AgentId) -> Result<AgentResult> {
        self.ensure_agent_in_session(target)?;
        self.rt
            .await_result(self.root, target)
            .await
            .map_err(FacadeError::Runtime)
    }

    /// Cancels `target` with `reason`, immediately -- delegates to [`Self::cancel_with`]
    /// with [`CancelMode::Immediate`], the pre-existing behavior, so every
    /// caller of this method keeps its exact prior semantics without
    /// needing to name a mode.
    pub async fn cancel(&self, target: AgentId, reason: &str) -> Result<()> {
        self.cancel_with(target, reason, CancelMode::Immediate)
            .await
    }

    /// Cancels `target` with `reason`, in `mode` -- the primitive [`Self::cancel`]
    /// delegates to. `CancelMode::Immediate` trips `target`'s
    /// `CancellationToken` now and propagates to `target`'s whole subtree,
    /// structurally (`tree.rs`: every child's own token is a
    /// `child_token()` of its parent's). `CancelMode::Graceful` instead lets
    /// `target` finish its in-flight turn, landing at its next turn
    /// boundary, and stops ONLY `target` itself -- it does not cancel
    /// descendants (a deliberate, narrow scope: see [`CancelMode`]'s own
    /// doc). **A graceful cancel cannot reach `target` while it is parked at
    /// the resume gate** -- an idle `keep_alive` agent between turns, or a
    /// resumed root's very first iteration -- since that wait selects only
    /// on the hard cancellation token, the deadline, and the gate's own
    /// notify, never the mailbox a graceful cancel is delivered through; use
    /// `CancelMode::Immediate` for that case.
    ///
    /// Rejects `target` with `Err(FacadeError::Runtime)` when it does not
    /// belong to this session's agent tree -- without this check any handle
    /// could cancel another session's agent, since `cancel`/`cancel_with`
    /// is a mutating control-plane op reached through the same runtime-wide
    /// `Arc<Runtime>` every `SessionHandle` shares. See
    /// `SessionHandle::ensure_agent_in_session`'s doc.
    ///
    /// Passes `self.root` as `caller` -- see `steer`'s own doc for the
    /// root/operator-exemption mechanism this mirrors exactly.
    ///
    /// Called through the `SubagentHost` trait explicitly (`SubagentHost::
    /// cancel(...)`, not `self.rt.cancel(...)`): `Runtime` also has its own
    /// inherent, synchronous `cancel` method (pre-existing, used elsewhere
    /// in this crate's own dependency graph) with the same name and a
    /// compatible-looking signature; Rust's method resolution prefers an
    /// inherent method over a trait method with the same receiver type, so
    /// a plain `self.rt.cancel(...)` call would silently bind to that
    /// inherent method instead of the trait method this criterion is about
    /// -- harmless for the immediate path (the trait impl's immediate arm is
    /// a pure pass-through to that same inherent method, confirmed in
    /// `conway-runtime`'s `subagent.rs`), but wrong for the graceful path
    /// (the inherent method has no such mode at all), so this is named
    /// explicitly rather than left to an incidental method-resolution
    /// tie-break either way.
    pub async fn cancel_with(&self, target: AgentId, reason: &str, mode: CancelMode) -> Result<()> {
        self.ensure_agent_in_session(target)?;
        SubagentHost::cancel(
            self.rt.as_ref(),
            self.root,
            target,
            reason.to_string(),
            mode,
        )
        .await
        .map_err(FacadeError::Runtime)
    }

    /// Verifies `agent` is reachable from `self.root` by walking
    /// `AgentNode.parent` links in `Runtime::tree()`'s snapshot -- the
    /// "session-ownership check" the binding notes describe. Called
    /// as the first step of every method above that takes an `AgentId` not
    /// already known to be `self.root` (`fork`, `spawn`, `steer`,
    /// `await_agent`, `cancel`): `Arc<Runtime>` is shared across every
    /// `SessionHandle` a `Conway` produces and `tree()` is runtime-wide, so
    /// without this check any handle could act on -- or, for `await_agent`,
    /// read the result of -- another session's agent.
    ///
    /// This check is deliberately structural (not a `session` field comparison): every
    /// agent in this workspace gets its own `SessionId` (one agent's
    /// append-only log per `SessionId`'s own doc), so a forked or spawned
    /// child's `AgentNode.session` is never equal to `self.session` -- only
    /// reachability via `parent` proves membership in this handle's tree.
    ///
    /// **, resolved:** `conway_core::error::RuntimeError`
    /// now has a dedicated `AgentNotInSession { agent, session }` variant
    /// (rendering `"agent {agent} does not belong to session {session}"`),
    /// added specifically to close the gap this method's previous doc
    /// disclosed. This method now distinguishes the two failure shapes:
    /// `agent` absent from `Runtime::tree()` entirely -> `AgentNotFound`
    /// (unknown to this runtime, full stop); `agent` present in the tree but
    /// not a descendant of `self.root` -> `AgentNotInSession` (a real agent,
    /// just not one this handle may act on).
    fn ensure_agent_in_session(&self, agent: AgentId) -> Result<()> {
        if agent == self.root {
            return Ok(());
        }
        let snapshot = self.rt.tree();
        let mut parent_of = std::collections::HashMap::new();
        let mut present = false;
        for node in &snapshot.nodes {
            if node.agent_id == agent {
                present = true;
            }
            parent_of.insert(node.agent_id, node.parent);
        }
        if !present {
            return Err(FacadeError::Runtime(RuntimeError::AgentNotFound { agent }));
        }
        let mut cursor = agent;
        loop {
            match parent_of.get(&cursor) {
                Some(Some(parent)) if *parent == self.root => return Ok(()),
                Some(Some(parent)) => cursor = *parent,
                _ => break,
            }
        }
        Err(FacadeError::Runtime(RuntimeError::AgentNotInSession {
            agent,
            session: self.session,
        }))
    }
}

/// Synthesizes an `(seq, ts, Event)` from one persisted `LogRecord`, for
/// `SessionHandle::events_from`'s replay batch. Returns `None` for
/// `LogRecord::Header` (no `seq`, not a replayable occurrence) and for any
/// future variant this `#[non_exhaustive]` enum grows that this function
/// does not yet know about.
///
/// **Disclosed gap (partially closed by this item -- see below):** no
/// committed mapping between `LogRecord` (persisted, one entry per
/// session-log line) and `Event` (the live, ephemeral broadcast wire format)
/// exists for most record kinds. They are independent representations of
/// different cardinality: e.g. live, one `Assistant` record's worth of a
/// turn corresponds to a run of `TextDelta`s plus one `TurnFinished`. This
/// function uses the faithful mappings that exist -- `AgentResultRecord` ->
/// `Event::AgentFinished` (matching exactly what `conway-runtime`'s agent
/// loop emits live for that occurrence) and, as of this item,
/// `LogRecord::UserTurn` -> `Event::UserTurn` (see that variant's own doc)
/// -- and falls back to `Event::AgentProgress{note}` (the one variant that
/// exists precisely for free-text informational replay) for every other
/// record kind with no faithful equivalent, rather than inventing a new
/// `Event` variant outside this item's file scope (`conway-core` owns that
/// enum).
///
/// **`ForkDirective`/`ParentSteer` still fall back to `AgentProgress`,
/// deliberately (this item's own decision, not an oversight):** both share
/// `UserTurn`'s root cause, but closing them safely requires the SAME
/// attach-ordering care `UserTurn`'s live emission needed (see
/// `conway-runtime::subagent::start`'s own note on why a `Spawn`'s initial
/// `UserTurn` had to be emitted AFTER `launch_agent`, not inline with its
/// append) PLUS auditing an entirely different call site
/// (`conway-runtime::mailbox`'s drain path) for `ParentSteer` -- a
/// materially larger, differently-shaped change this item's acceptance
/// criteria do not exercise. Left as explicit follow-up rather than folded
/// in silently.
///
/// **`Assistant` -> `Event::TextDelta`, not `Event::TurnFinished` (an earlier
/// review fix, was the opposite -- see this arm's own inline doc):** a bare
/// `TurnFinished{usage, stop}` carries no reply text, and nothing downstream
/// (`conway-cli`'s `AppState::apply`) turns one into visible transcript
/// content -- by design, since live, `TurnFinished` only ever marks the END
/// of a run of real `TextDelta`s that already rendered the reply. Replaying
/// `Assistant` as `TurnFinished` therefore made a focus-switched transcript
/// silently omit every assistant reply. Mapping to one `TextDelta` carrying
/// the record's full, concatenated text instead lets the SAME live
/// `TextDelta -> append_assistant_text` path (`AppState::apply`) render it,
/// with no second, parallel replay-only rendering path introduced.
fn record_to_event(record: &LogRecord) -> Option<(LogSeq, DateTime<Utc>, Event)> {
    match record {
        LogRecord::Header(_) => None,
        // Faithful, not a fallback (this item): mirrors exactly what
        // `conway-runtime` emits live for the same occurrence (`Runtime::
        // prompt`/`start_root`, `subagent.rs::start` for a non-empty-prompt
        // `Spawn`) -- see `Event::UserTurn`'s own doc.
        LogRecord::UserTurn {
            seq,
            ts,
            text,
            prov,
        } => Some((
            *seq,
            *ts,
            Event::UserTurn {
                text: text.clone(),
                prov: prov.clone(),
            },
        )),
        LogRecord::Assistant {
            seq, ts, content, ..
        } => Some((
            *seq,
            *ts,
            // **Fixed (was `Event::TurnFinished{usage, stop}`
            // review finding 1, CRITICAL):** that mapping discarded the
            // reply text entirely, so a focus-switch replay showed no
            // assistant dialogue at all -- `AppState::apply` has no arm
            // that turns a bare `TurnFinished` into transcript content (by
            // design; live `TurnFinished` only marks the end of a run of
            // real `TextDelta`s, which is exactly what replay was missing).
            // Mapping to `TextDelta` with the record's full text instead
            // makes `apply`'s existing `TextDelta -> append_assistant_text`
            // path build a real `Entry::Assistant` bubble on replay, with
            // no second, parallel `LogRecord -> Entry` mapper needed.
            // `usage`/`stop` are dropped -- irrelevant to what the
            // transcript pane renders, and (per this function's own
            // one-event-per-record shape) there is nowhere left to carry
            // them once this record maps to `TextDelta` instead of
            // `TurnFinished`.
            Event::TextDelta {
                text: assistant_text(content),
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
                // Replay synthesis: a persisted `AgentResultRecord` does not
                // carry the `ephemeral` flag (provenance is preserved via
                // the session store, not via this replayed event). Default
                // `false` to match the pre-ephemeral replay semantics -- the
                // live `Event::AgentFinished` at finish time carries the true
                // value; this replay path is only for reconstructing the event
                // stream from a cold log.
                ephemeral: false,
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
        //: a child's result recorded into this
        // (the parent's) log -- same `AgentProgress` fallback shape as
        // `ForkDirective`/`ParentSteer`/`ContextReportRecord` above, not a
        // faithful `AgentFinished` (that event describes the RECORD-OWNING
        // agent's own finish, and this record's owning agent is the
        // parent, still running).
        LogRecord::ChildResultRecord {
            seq, ts, result, ..
        } => Some((
            *seq,
            *ts,
            Event::AgentProgress {
                note: format!("child {} finished: {}", result.agent_id, result.summary),
            },
        )),
        _ => None,
    }
}

/// Concatenates every `ContentBlock::Text` block's text, in order, with no
/// separator -- exactly how live `TextDelta`s already accumulate into one
/// `Entry::Assistant` bubble (`conway-cli`'s `AppState::append_assistant_text`
/// just `push_str`s each delta onto the last one with nothing in between),
/// so replaying an `Assistant` record's full text as a single `TextDelta`
/// (`record_to_event`'s `LogRecord::Assistant` arm) renders identically to
/// however that same reply looked when it streamed in live. Non-`Text`
/// blocks (`Thinking`/`ToolUse`/etc.) are skipped -- this is the reply TEXT
/// specifically, the same narrowing `tool_result_preview` below already
/// applies for the analogous tool-result case.
fn assistant_text(content: &[ContentBlock]) -> String {
    // One implementation, shared with `conway-runtime`'s live pull-in path
    // so a replayed transcript and a live one cannot diverge.
    conway_core::content::assistant_text(content)
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

    /// The agent this turn belongs to (B5): for a `SessionHandle::ask` turn
    /// this is the ephemeral CHILD's id -- the value the `/ask` modal's
    /// forced fates need (`Conway::promote`/`pull_in`/`purge` all take the
    /// child `AgentId`, and nothing else on this handle exposes it).
    pub fn agent(&self) -> AgentId {
        self.agent
    }

    /// Concatenates every `Event::TextDelta` observed for this turn, up to
    /// (not including) the first `Event::TurnFinished` -- or, if the agent
    /// finishes within the same generation without a distinct
    /// `TurnFinished`, up to `Event::AgentFinished` (whose `AgentResult` is
    /// buffered for a subsequent `result()` call).
    ///
    /// **`AgentFinished` is agent-id-checked here, not just filtered
    /// upstream:** `EventStream::accept` passes every `AgentFinished`
    /// through regardless of session/agent (tree lifecycle is global -- see
    /// its doc), so this turn's own internal stream can now observe a
    /// DIFFERENT agent's (e.g. a subagent spawned mid-turn) completion.
    /// Only an `AgentFinished` whose `result.agent_id` matches this turn's
    /// own `self.agent` is treated as terminal; any other is silently
    /// ignored here, same as an unrelated lifecycle note would be.
    pub async fn text(&self) -> Result<String> {
        let mut inner = self.inner.lock().await;
        let mut text = String::new();
        while let Some(envelope) = next_envelope(&mut inner.stream).await {
            match envelope.event {
                Event::TextDelta { text: delta } => text.push_str(&delta),
                Event::TurnFinished { .. } => break,
                Event::AgentFinished { result, .. } if result.agent_id == self.agent => {
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
    /// **`SessionSpec::keep_alive` sessions:** the "exactly one
    /// `AgentFinished`" pairing above holds for the session's root agent as
    /// a WHOLE, not per turn. A keep-alive turn that completes with no
    /// pending work does not emit `AgentFinished` at all -- it idle-awaits
    /// the next prompt instead (`conway_runtime::agent_loop::AgentLoop`'s
    /// own doc on its natural-completion branch); the ONE `AgentFinished`
    /// a keep-alive session ever produces arrives only when the session
    /// itself really ends (cancel/deadline/budget), for whichever turn is
    /// in flight at that moment. Concretely: `let turn =
    /// handle.prompt(x).await?; turn.result().await` will hang for the
    /// lifetime of the session if `x`'s turn completes normally -- consume
    /// a keep-alive session's individual turns via [`Self::text`] or
    /// [`Self::events`] instead, and reserve `result()` for the case where
    /// the caller genuinely wants to block until the whole session
    /// terminates.
    ///
    /// The `AgentNotFound` error below is not expected to occur in
    /// practice (it only fires if the runtime's broadcast bus itself ends,
    /// which happens only when every `Arc<Runtime>` -- including the one
    /// this handle and its owning `SessionHandle` hold -- has already been
    /// dropped); it exists so this method has a total, typed return rather
    /// than panicking on an unreachable-but-not-provably-impossible stream
    /// end.
    ///
    /// **Agent-id-checked, same reason as [`Self::text`]:** `EventStream`'s
    /// tree-lifecycle passthrough means this turn's stream can observe an
    /// `AgentFinished` for an agent other than `self.agent` (e.g. a
    /// subagent this turn spawned finishing first); only a matching
    /// `result.agent_id` resolves this call.
    pub async fn result(&self) -> Result<AgentResult> {
        let mut inner = self.inner.lock().await;
        if let Some(result) = inner.buffered_result.take() {
            return Ok(result);
        }
        loop {
            match next_envelope(&mut inner.stream).await {
                Some(envelope) => {
                    if let Event::AgentFinished { result, .. } = envelope.event {
                        if result.agent_id == self.agent {
                            return Ok(result);
                        }
                    }
                }
                None => {
                    return Err(FacadeError::Runtime(RuntimeError::AgentNotFound {
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
    ///
    /// One exception to the "scoped to this turn's agent" filter: as with
    /// every [`EventStream`], `AgentSpawned`/`AgentFinished` bypass the
    /// filter and are delivered regardless of which agent they name (tree
    /// lifecycle is a global concern -- see [`EventStream`]). A consumer
    /// building a "this agent only" view from lifecycle events must check
    /// `envelope.agent` itself; `text()`/`result()` do exactly that
    /// internally to avoid resolving on a subagent's finish.
    pub fn events(&self) -> EventStream {
        EventStream::live(self.session, Some(self.agent), self.rt.subscribe())
    }
}

async fn next_envelope(stream: &mut EventStream) -> Option<Envelope> {
    poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await
}
