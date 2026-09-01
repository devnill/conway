//! The terminal `AgentResult` contract (with the MAST mitigations: a bounded
//! summary, typed facts, artifacts, a `transcript_ref`, and a
//! `Rejected{missing}` status), the two-mode `SubagentSpec` (fork vs spawn),
//! the flat agent tree snapshot, the parent<->child message enum, and the
//! permission request/decision types.
//!
//! Nothing in this module performs I/O: `SubagentSpec::validate` only checks
//! internal consistency, and the `fork`/`spawn` constructors only set field
//! defaults. The crate-wide claim is narrower than that — see the crate root
//! doc's forward-declaration label for `containment`, the one module that
//! does I/O today ( closes it).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::content::{Artifact, ToolCategory, Usage};
use crate::error::ConwayError;
use crate::ids::{AgentId, LogSeq, ModelRef, RoleAlias, SessionId, ToolName};
use crate::path::RecordRef;

/// Fork vs spawn: re-exported from `log` (the canonical definition lives
/// there because [`crate::log::ForkOrigin`] persists it). Do not redefine.
pub use crate::log::SubagentMode;

/// The maximum number of `char`s an [`AgentResult::summary`] may contain.
/// [`AgentResult::new`] truncates on a `char` boundary, never a byte offset.
pub const DEFAULT_SUMMARY_LIMIT: usize = 2000;

/// The outcome of a `SubagentHost::ask` call: the FULL concatenated
/// `TextDelta` reply text from the ephemeral child, plus the terminal
/// `AgentResult`'s `usage`/`status` and the child's `transcript_ref`.
///
/// `text` is explicitly NOT [`AgentResult::summary`]: that field is bounded
/// to [`DEFAULT_SUMMARY_LIMIT`] (2000) `char`s -- too small for a curated
/// context/prompt the orchestrator may want to feed onward -- whereas
/// `AskOutcome::text` carries the entire reply, untruncated. `transcript_ref`
/// names the ephemeral child session so the orchestrator's
/// `ToolResultRecord` can point at it, which is what keeps provenance reachable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AskOutcome {
    /// The FULL concatenated `TextDelta` reply -- NOT `AgentResult::summary`
    /// (which truncates at [`DEFAULT_SUMMARY_LIMIT`] = 2000 chars).
    pub text: String,
    pub usage: Usage,
    pub status: ResultStatus,
    /// The ephemeral child's session id -- the same noun, spelled the same
    /// way and for the same reason, as [`AgentResult::transcript_ref`],
    /// whose doc records why neither field is called `session_id`. The two
    /// move together or not at all.
    pub transcript_ref: SessionId,
}

/// The terminal outcome of one agent's run: the only thing a parent (or the
/// CLI/IDE) ever sees of a finished child, by design (MAST: bound what
/// crosses the trust boundary).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentResult {
    pub agent_id: AgentId,
    pub status: ResultStatus,
    /// Always a bounded `String` (never `Option`): a result without a
    /// summary is still required to say so explicitly (an empty string),
    /// not omit the field.
    pub summary: String,
    pub facts: Vec<Fact>,
    pub artifacts: Vec<Artifact>,
    pub structured: Option<serde_json::Value>,
    /// The finished agent's session id: an OPAQUE POINTER to the
    /// append-only log, never any of the log's content. It is also the
    /// value `--session`, `--resume`, and `--fork-from` accept, whereas
    /// `agent_id` (above) names a node in the agent tree and is rejected by
    /// all three -- see `docs/scripting.md`'s `json` section and
    /// `docs/sessions.md` on resuming, which document that trap for
    /// scripts.
    ///
    /// # Why this is not called `session_id`
    ///
    /// Board item `01M0TX4TBJTPKN4ED50EEH2SY3` weighed renaming it to
    /// exactly that, on the ground that a field whose name said what it was
    /// would need no adjacent paragraph explaining which id `--resume`
    /// takes. The decision was to KEEP `transcript_ref`, recorded here so
    /// the next reader who finds the trap finds the argument at the field
    /// rather than re-opening it. Four reasons, roughly in order of weight:
    ///
    /// 1. **The name is load-bearing about containment, not about
    ///    resuming.** `AgentResult` is the entire surface a finished child
    ///    presents across the trust boundary, and the MAST mitigation this
    ///    module's own doc lists is that what crosses it is a *reference to
    ///    a transcript*, not a transcript. `conway-runtime`'s
    ///    `result_contract` suite pins that as an executable claim
    ///    (`agent_result_serializes_only_the_bounded_field_set_no_raw_transcript`).
    ///    `session_id` names the resume handle and drops the containment
    ///    signal entirely.
    /// 2. **The same noun is used identically elsewhere, on purpose.**
    ///    [`AskOutcome::transcript_ref`] is a parent holding a reference to
    ///    an ephemeral child's transcript; so is the `AgentResult` embedded
    ///    in `crate::log::LogRecord`'s `ChildResult`. Renaming here alone
    ///    splits one noun in two at the exact seam where the reader most
    ///    needs it to be one; renaming everywhere spends the whole plugin
    ///    and subagent surface to buy a scripting affordance that a
    ///    sentence of prose already buys.
    /// 3. **This serde name is persisted, not merely printed.** The same
    ///    struct is a field of `crate::log::LogRecord`'s
    ///    `AgentResultRecord` and `ChildResult`, so the string
    ///    `transcript_ref` is in every `<session-id>.jsonl` already on
    ///    disk. A bare rename makes those unreadable -- the field is not
    ///    `Option` and has no `serde` default, so deserialization fails on
    ///    the missing field -- and `#[serde(alias)]` repairs only that
    ///    direction. It cannot help the population a rename actually
    ///    breaks: a script doing `jq -r .transcript_ref` gets `null`, not
    ///    an error, and passes it on. Board item CON-3 skipped a deprecated
    ///    alias when it renamed the facade's `ConwayError` to `FacadeError`
    ///    because `publish = false` left that type no compatibility surface
    ///    but a compile-time one; the same premise is simply false for a
    ///    wire field the operator's own log files were written with.
    /// 4. **The resume handle is no longer an id anyway.** Since session
    ///    names landed, `--session`/`--resume`/`--fork-from` take
    ///    `<session-id-or-name>[@<seq>]`, resolved by `conway-cli`'s
    ///    `session_names::resolve`. This field carries neither a name nor a
    ///    `@<seq>` suffix, so
    ///    `session_id` would be a less exact answer to "what do I pass to
    ///    `--resume`" than the rename assumed, and `session_ref` would be
    ///    wrong outright.
    ///
    /// The trap the item names is real. The instrument for it is the prose
    /// in `docs/scripting.md` and `docs/sessions.md`, which is where a
    /// reader who has the JSON in front of them and does not have this
    /// source open will actually look.
    pub transcript_ref: SessionId,
    pub usage: Usage,
    pub steps_taken: u32,
}

impl AgentResult {
    /// Builds a result, truncating `summary` to at most
    /// [`DEFAULT_SUMMARY_LIMIT`] `char`s. The truncation point is found via
    /// `char_indices` so it always lands on a UTF-8 character boundary,
    /// never a raw byte offset (which would panic on multi-byte input).
    pub fn new(
        agent_id: AgentId,
        transcript_ref: SessionId,
        status: ResultStatus,
        summary: impl Into<String>,
    ) -> Self {
        let summary = truncate_to_char_limit(summary.into(), DEFAULT_SUMMARY_LIMIT);
        Self {
            agent_id,
            status,
            summary,
            facts: Vec::new(),
            artifacts: Vec::new(),
            structured: None,
            transcript_ref,
            usage: Usage::default(),
            steps_taken: 0,
        }
    }

    /// `true` only for [`ResultStatus::Completed`].
    pub fn is_terminal_success(&self) -> bool {
        matches!(self.status, ResultStatus::Completed)
    }
}

/// Truncate `s` to at most `max_chars` `char`s, cutting only on a character
/// boundary. No-op if `s` already has `max_chars` or fewer `char`s.
fn truncate_to_char_limit(mut s: String, max_chars: usize) -> String {
    if let Some((byte_idx, _)) = s.char_indices().nth(max_chars) {
        s.truncate(byte_idx);
    }
    s
}

/// How an agent's run ended.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResultStatus {
    Completed,
    Failed {
        error: String,
    },
    Cancelled {
        reason: String,
    },
    /// The agent hit its [`Budget`] before finishing.
    BudgetExceeded {
        limit: String,
    },
    /// The agent's request was rejected outright (e.g. a fork whose inherited
    /// context plus directive already exceeds the model's window, T-1) —
    /// never truncated or escalated.
    Rejected {
        missing: Vec<String>,
    },
}

/// One typed, attributable fact extracted from an agent's run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub key: String,
    pub value: serde_json::Value,
    pub source: Option<String>,
}

/// A hard resource ceiling on a subagent's run. `max_steps` is deliberately
/// not optional: §6.4 requires every child to have a step budget so a
/// parent's pending tool call can never hang.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub max_steps: u32,
    pub deadline: Option<DateTime<Utc>>,
    pub max_tokens: Option<u32>,
    pub max_tool_calls: Option<u32>,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_steps: 40,
            deadline: None,
            max_tokens: None,
            max_tool_calls: None,
        }
    }
}

/// The complete specification for spawning or forking a subagent. `fork` and
/// `spawn` are the only two subagent modes, and they are never blurred into one
/// parameterized operation; `mode` is
/// [`SubagentMode`], re-exported above.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubagentSpec {
    pub mode: SubagentMode,
    pub prompt: String,
    pub agent_def: Option<AgentDefRef>,
    pub role: Option<RoleAlias>,
    /// Pins the child's model outright, overriding whatever it would
    /// otherwise resolve (the fork-only inheritance fill's `agent_def.model`,
    /// or -- absent that -- ordinary role-based routing). `None` (the
    /// `fork`/`spawn` constructors' default) preserves the pre-existing
    /// behavior exactly: `conway_runtime`'s `SubagentHost::start` derives the
    /// child's pin solely from its (possibly inherited) `agent_def.model`,
    /// with no way for a caller to name a specific model directly. `Some`
    /// is the mechanism `conway`'s `ForkSpec::model` (INTENT.md §5c: "changing
    /// model mid-session is ordinary") uses to switch a live conversation to
    /// a named model without touching `role` at all -- the child still
    /// inherits the forker's ENTIRE prior context (selection, per §5c,
    /// survives a model change unchanged); only this pin, and therefore the
    /// rendering the new model receives, differs. A pin the child's
    /// inherited context does not fit produces the same loud
    /// `RoutingError::ContextTooLarge` refusal an ordinary turn's admission
    /// gate already gives -- never a silent fallback to the old model, and
    /// never a silent trim.
    ///
    /// `#[serde(default)]` keeps already-persisted data readable: a
    /// `SubagentSpec` serialized before this field existed still
    /// deserializes, as `None` -- the pre-existing agent-def-only pin
    /// resolution for every such spec.
    #[serde(default)]
    pub pin: Option<ModelRef>,
    pub tools: Option<ToolSelector>,
    pub budget: Budget,
    /// A schema the child's final answer must satisfy. Evaluated on a
    /// completing turn; a violation appends a `SystemNote` with reason
    /// `result_contract_violation` and grants one corrective turn.
    ///
    /// **Cannot be combined with [`Self::keep_alive`]** -- the pair is
    /// rejected by [`Self::validate`]. A kept-alive agent never finishes, so a
    /// result it validated has nowhere to be delivered and the caller's
    /// `await_result` would hang forever. See `validate`'s own doc for the
    /// mechanism and for why this is a rejection rather than a delivery
    /// feature.
    pub result_contract: Option<schemars::schema::RootSchema>,
    /// Opt-in interactive keep-alive (mirrors `conway_runtime`'s
    /// `agent_loop::AgentSpec::keep_alive`/`runtime::RootSpec::keep_alive`):
    /// the child idles for the caller's next prompt after each turn instead
    /// of finishing on natural completion. Defaults `false` via the `fork`/
    /// `spawn` constructors below, preserving the pre-existing autonomous
    /// (one-shot, awaitable via `conway_core::ports::SubagentHost::
    /// await_result`) fork/spawn behavior unchanged. When
    /// this is `true` AND `prompt` is empty, `conway_runtime`'s
    /// `SubagentHost::start` additionally starts the child IDLE (no
    /// placeholder turn run against blank input) -- the shape a caller
    /// wanting a fresh, interactive, re-promptable session (the TUI's bare
    /// `/spawn`/`/fork`) constructs via `conway`'s `SpawnSpec::keep_alive`/
    /// `ForkSpec::keep_alive`.
    ///
    /// **Cannot be combined with [`Self::result_contract`]** -- the pair is
    /// rejected by [`Self::validate`]. Keeping the child open is precisely what
    /// stops its validated result from ever being delivered, so the two
    /// requests contradict each other; asking for both used to hang the
    /// caller silently.
    pub keep_alive: bool,
    /// `ephemeral` is a [`crate::log::SessionMeta`] listing-visibility bit,
    /// NOT a mode -- it filters the child out of default session catalog
    /// listings and the TUI `/agents` panel while keeping it attached to the
    /// live [`AgentTreeSnapshot`] for provenance. This is not a third
    /// subagent primitive: `ask` is fork+await-text, not a new mode.
    /// Defaults `false` via the `fork`/`spawn` constructors below, preserving
    /// the pre-existing non-ephemeral fork/spawn behavior unchanged.
    pub ephemeral: bool,
    /// Which `/ask`-style path is creating this child (B5), stamped VERBATIM
    /// into the child's durable `SessionMeta::ask_origin` by `conway-runtime`'s
    /// `SubagentHost::start`. Only the two ephemeral-ask builders set this:
    /// the TUI's modal `/ask` (`conway`'s `SessionHandle::ask`,
    /// [`crate::log::AskOrigin::ModalAsk`]) and the `conway_ask` tool
    /// ([`crate::log::AskOrigin::ToolAsk`]) -- see that enum's own doc for
    /// why the distinction is load-bearing (the TUI's crash-residue sweep
    /// purges modal-ask leftovers but must never touch a tool-ask child).
    /// `None` everywhere else, including via the `fork`/`spawn` constructors
    /// below.
    pub ask_origin: Option<crate::log::AskOrigin>,
    /// The child's own working directory, independent of the parent's (C1).
    /// `None` (the `fork`/`spawn` constructors' default, preserving the
    /// pre-existing "child always inherits the parent's cwd" behavior
    /// unchanged) means `conway_runtime`'s `SubagentHost::start` resolves
    /// the child's cwd from the parent's own [`crate::log::SessionMeta::cwd`]
    /// exactly as before this field existed.
    ///
    /// `Some(path)` scopes the child to `path` instead: an absolute path is
    /// used as-is; a relative path is resolved against the PARENT's cwd at
    /// spawn time (the child has no cwd of its own yet to resolve against).
    /// A nonexistent resolved path fails the spawn fast, with a clear error,
    /// rather than starting a child whose tools would silently fail on
    /// every relative path. This is defense in depth, not a sandbox: it
    /// governs relative-path resolution only (`conway_core::ports::ToolCtx::
    /// cwd`, which every filesystem tool resolves relative paths against) --
    /// an absolute path a tool is given (or a `..` that walks back out)
    /// still escapes it. The permission gate remains the actual enforcement
    /// layer.
    ///
    /// `#[serde(default)]` keeps already-persisted data readable: a
    /// `SubagentSpec` serialized before this
    /// field existed still deserializes, as `None` -- the pre-existing
    /// inherit-the-parent's-cwd behavior for every such spec.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// (S3) The child's confinement root, independent of (but validated
    /// against) `cwd` above -- placed beside it for the same reason `cwd`
    /// itself was: nothing in this module performs I/O (`SubagentSpec::
    /// validate` stays a pure consistency check; the crate-wide claim has
    /// one live exception, labeled at the crate root), so resolution,
    /// canonicalization, and
    /// containment enforcement all happen at `conway_runtime`'s
    /// `SubagentHost::start` instead. See that method's own doc for the
    /// full "inheritance algebra" this implements.
    ///
    /// `None` (the `fork`/`spawn` constructors' default, and the ONLY shape
    /// the facade's `ForkSpec` can express -- a fork inherits the forker's
    /// ENTIRE context -- a fork inherits all of it or none -- so a root override
    /// there would be incoherent
    /// with the context the child actually sees) means "inherit the
    /// parent's root, unchanged" -- including when the parent itself is
    /// unconfined (`root: None`), which stays `None` all the way down until
    /// something narrows it.
    ///
    /// `Some(requested)` scopes the child to `requested` instead, subject to
    /// the inheritance algebra `SubagentHost::start` enforces: a `requested`
    /// that resolves inside the parent's own root (or the parent is
    /// unconfined) narrows the child and is accepted; a `requested` that is
    /// wider than, or disjoint (sideways) from, the parent's root FAILS the
    /// spawn outright with a typed error naming both roots -- this is never
    /// silently clamped to the parent's root, because a silent narrowing
    /// would turn an operator's mistake into a working-but-not-what-was-
    /// asked-for configuration. A `requested` (or inherited) root that does
    /// not canonicalize, or whose child `cwd` (inherited or overridden)
    /// would fall outside it, also fails the spawn.
    ///
    /// This field is **not itself enforcement** (out of scope for this
    /// item): nothing yet checks a tool call's arguments against it. It is
    /// carried and validated end-to-end -- resolved once, persisted onto the
    /// child's own `crate::log::SessionMeta::root` -- so a later slice can
    /// wire the actual confinement check without this plumbing changing
    /// shape.
    ///
    /// `#[serde(default)]` keeps already-persisted data readable: a
    /// `SubagentSpec` serialized before this
    /// field existed still deserializes, as `None` -- the pre-existing
    /// unconfined behavior for every such spec.
    #[serde(default)]
    pub root: Option<PathBuf>,
    /// An opaque identifier an
    /// embedder attaches at creation to correlate this agent with its OWN
    /// domain object (a file, a job, a node in its own tool) -- set here,
    /// atomically with the spec that creates the agent, so there is nothing
    /// to register after `SubagentHost::start` returns and therefore no
    /// window in which the child's first turn can race a side table that
    /// does not have the association yet.
    ///
    /// **conway never reads this field.** Decision
    /// ruled out the two alternatives considered (a caller-supplied
    /// `AgentId`, and a prepare/launch split) specifically because both
    /// either hand a conway-enforced invariant to the caller or force two
    /// surfaces for one operation. The tag is the shape that survives, and
    /// the whole point of that shape is that it carries no meaning conway
    /// acts on -- unlike [`Self::role`] (a routing input,
    /// `conway_runtime::agent_loop`'s `policy.resolve(&spec.role)`) or
    /// [`Self::ask_origin`] (branched on in `conway_runtime::subagent`'s
    /// `start`, gating whether a `result_contract` may attach), which look
    /// like precedent for "another opaque consumer field" but are not --
    /// this is conway's first field of this kind, so the "never
    /// interpreted" guarantee has to be established directly (kept out of
    /// every match/branch/comparison the runtime performs) rather than by
    /// imitating either.
    ///
    /// Threaded through, unread, by `conway_runtime::subagent::SubagentHost::
    /// start` onto `AgentSpec::tag`, and from there onto every
    /// `ContextHookCtx::tag` for that agent's turns -- see those fields' own
    /// docs. `None` everywhere else, including via the `fork`/`spawn`
    /// constructors below: this item scopes the surface to `ContextHookCtx`
    /// only (`PermissionRequest` is a documented follow-on, not built here).
    ///
    /// `#[serde(default)]` keeps already-persisted data readable: a
    /// `SubagentSpec` serialized before this
    /// field existed still deserializes, as `None` -- no tag, exactly the
    /// pre-existing behavior for every such spec.
    #[serde(default)]
    pub tag: Option<String>,
    /// Per-agent plugin configuration this fork/spawn requests for the
    /// child -- the general mechanism the `[S1.5]` charter's per-agent
    /// narrowing item introduces, of which `conway.fs`'s own root is the
    /// proving consumer. `None` (the `fork`/`spawn` constructors' default)
    /// means "inherit the parent's own effective per-agent plugin config
    /// unchanged" -- the same `None`-means-inherit shape [`Self::cwd`]/
    /// [`Self::root`] already established. `Some(map)` requests an override
    /// for exactly the keys present in `map`; every other key an ancestor
    /// may already have narrowed is carried through unchanged.
    ///
    /// **Narrowing-only, and validated where the registry lives, not
    /// here.** A key in `map` must be declared narrowable by its owning
    /// plugin (`conway_core::ports::Plugin::narrowable_keys`), and its
    /// requested value must not WIDEN the value the parent's own effective
    /// config already carries for that key (a parent with no value yet for
    /// a key has nothing to narrow against, so a first-time value is always
    /// accepted). This module performs no I/O and holds no plugin registry
    /// to check against -- `conway_runtime`'s `SubagentHost::start`
    /// validates this field against the parent's own resolved config and
    /// the installed plugin set's declared rules (`conway_core::ports::
    /// PluginConfig::narrow`), the same division of labor `Self::root`'s
    /// own inheritance algebra already uses (this field carried and
    /// validated end-to-end; the runtime does the checking).
    ///
    /// Unlike `cwd`/`root`, this field is reachable from BOTH `conway`'s
    /// `ForkSpec` and `SpawnSpec` -- a fork's inherited transcript does not
    /// describe a plugin's own scoped resource the way it describes a
    /// working directory, so narrowing a plugin's per-agent config on a
    /// fork is coherent where narrowing `cwd`/`root` there was not.
    ///
    /// `#[serde(default)]` keeps already-persisted data readable: a
    /// `SubagentSpec` serialized before this field existed still
    /// deserializes, as `None` -- the pre-existing "no per-agent plugin
    /// config" behavior for every such spec.
    #[serde(default)]
    pub plugin_config: Option<crate::ports::PluginConfig>,
    /// The child's CHOSEN starting context path, as an ordered list of
    /// already-resolved `(session, seq)` references -- the eighth axis
    /// `ForkSpec` narrowed nothing on, before this field existed (`fork`
    /// inherited the WHOLE forker transcript, `spawn` inherited none, and
    /// there was no way to say "start with exactly these pieces").
    ///
    /// **What it carries, and why not a `PathSelection`/`SelectionKey`/op
    /// list instead.** A flat `Vec<RecordRef>` mirrors `conway-plugin-path`'s
    /// own `compose_context_path` tool argument shape (`include`) exactly --
    /// the caller has already turned an operator's stated intent into
    /// concrete references (its own session's records, or a completed
    /// child's `transcript_ref`) by the time either surface is reached, so
    /// neither should re-parse natural language. A raw `PathSelection` would
    /// force a caller to hand-build `PathNode`s (stamp, provenance, and all)
    /// for a value this crate can derive uniformly instead. A `SelectionKey`
    /// -- content-addressed and already storable, which would let many
    /// forks/spawns share one frozen selection ("start every reviewer from
    /// the same base") -- is a real, disclosed follow-up: nothing in this
    /// tree exposes a `SelectionKey` to a caller yet (`compose_context_path`
    /// reports the resulting head's log position, not the selection's own
    /// key), so there is nothing to hand one to today. This field is the
    /// slice that has a producer on both ends.
    ///
    /// **How it composes with [`Self::mode`].** It sits BESIDE the Fork/Spawn
    /// axis rather than replacing or refining it: `mode` still governs
    /// store-level lineage (`SessionStore::fork` vs `::create`, sibling
    /// sharing, `AgentNode.inherited_upto` tree bookkeeping) unchanged, and
    /// `directive`/`prompt` is still appended as the child's own head content
    /// record exactly as before. What changes is CONTEXT ASSEMBLY only: when
    /// `Some`, `conway_runtime`'s `SubagentHost::start` writes the given
    /// records as the child's very first `ContextPathSet` HEAD (via the same
    /// `ContextPathHost::set_head`/`ValidatedPath::derive_with` machinery
    /// `compose_context_path` calls mid-chain -- no second implementation of
    /// path derivation), so `resolve_default_path`'s "head exists" branch --
    /// not the "no head" ancestry-walk/whole-own-log default -- governs every
    /// turn from the first one on. `None` (every `fork`/`spawn` constructor's
    /// default) is a complete no-op: the mode's ordinary default (Fork's
    /// inherited prefix, Spawn's clean slate) is exactly what runs, unchanged
    /// down to the byte. `Some(vec![])` is not the same as `None` -- it is a
    /// deliberate "replace the default with nothing", e.g. a fork that keeps
    /// only its own directive and drops the forker's entire inherited
    /// transcript, which was previously inexpressible.
    ///
    /// **Narrowing: none imposed, and that is a decision, not an oversight.**
    /// `Self::resolve_records`-shaped resolution (`ContextPathHost::
    /// resolve_records`, the SAME surface `compose_context_path` already
    /// uses) can already read ANY session's records, honestly, through the
    /// masked/ancestry-aware resolver -- decision `01M0K4QT6MBXPD6PXMBBBD2P7B`
    /// states this mirrors `CurateCtx::store`'s existing "a curator may
    /// reference any record in the store" grant. This field does not add a
    /// second, narrower rule on top of an already-wide-open capability:
    /// doing so would make an identical reference resolve at fork/spawn time
    /// but not mid-chain (or vice versa), an inconsistency with no
    /// corresponding security boundary to justify it (unlike `plugin_config`,
    /// whose narrowing-only rule guards an operational/security-relevant
    /// resource -- a filesystem root -- context CONTENT is not that). The
    /// only rejection this field can produce is `resolve_records`'s existing
    /// failure mode (a masked, unresolvable, or nonexistent record), enforced
    /// at the one call site that resolves it (`SubagentHost::start`), never
    /// silently dropped.
    ///
    /// **`covers_upto`, reasoned through, not assumed.** `write_head`'s
    /// `covers_upto_for` derives the child's own-tail marker purely from the
    /// GIVEN selection's own-attributed nodes -- and this selection, being a
    /// child's very FIRST head, is written while the child's own log is still
    /// completely empty. It therefore always lands on `LogSeq::ZERO`
    /// (`covers_upto_for`'s documented no-own-records fallback), which here
    /// means exactly what it says: "read this (currently empty) own log from
    /// the beginning" -- never the silent-reversal trap finding
    /// `01M0P50E04EY3BHQJHZX74HSSC` describes, because that trap requires a
    /// PRIOR head that already excluded some own records for the reset to
    /// silently resurrect; a brand-new child has no prior head and nothing to
    /// resurrect. The directive/prompt record appended immediately after
    /// (unconditionally, by the existing head-record step) becomes this
    /// child's own tail from the very next read, exactly as intended. Pinned
    /// by `conway-runtime`'s `subagent.rs` test suite, not merely asserted
    /// here.
    ///
    /// **What this does NOT reach.** `Conway::fork_from`/`fork_child.rs` --
    /// the SEPARATE mechanism that forks a PERSISTED session at an arbitrary,
    /// possibly-earlier point with no live agent involved -- does not go
    /// through `SubagentHost::start` at all, and already drops several other
    /// `ForkSpec` fields for the same reason (`directive`'s own semantics
    /// differ there, `model`, `ephemeral`, `ask_origin` are absent from its
    /// narrower `ForkChildRequest`). Wiring `context` through that path too
    /// is a disclosed follow-up, not this field's scope.
    ///
    /// `#[serde(default)]` keeps already-persisted data readable: a
    /// `SubagentSpec` serialized before this field existed still
    /// deserializes, as `None` -- the pre-existing behavior for every such
    /// spec.
    #[serde(default)]
    pub context: Option<Vec<RecordRef>>,
}

impl SubagentSpec {
    /// **Relaxed (superseded):** §5.2's original "`agent_def` is
    /// required for `Spawn`" rule -- enforced here as an
    /// `Err(ConwayError::Config{..})` -- is relaxed by a recorded design
    /// decision: a spawn with `agent_def: None` is now valid. It means the
    /// child gets no agent-def system prompt and no model pin, and instead
    /// inherits the spawning session's role (and, transitively, its model
    /// routing) -- `conway_runtime`'s `SubagentHost::start` implements the
    /// inheriting resolution. Kept as a method (rather than removed
    /// outright) since it remains the natural place for any future
    /// spec-shape validation.
    ///
    /// **Rejects `keep_alive` combined with a `result_contract`**. The two are individually sound and
    /// individually documented; together they produce a HANG, which is the
    /// worst failure shape available because it is indistinguishable from a
    /// child that is simply still working.
    ///
    /// The mechanism, in one sentence: a contract is evaluated on a
    /// completing turn, but a kept-alive agent does not return on a
    /// completing turn -- `AgentLoop`'s `if self.spec.keep_alive` branch
    /// re-arms the resume gate and continues, so `finish` is never reached,
    /// and `finish` is the only sender of `AgentMessage::Result`. The result
    /// is validated and then has nowhere to go: `await_result` never
    /// resolves. Validation is not the casualty; delivery is.
    ///
    /// **Why rejection rather than delivery.** Making a kept-alive child's
    /// result reachable is a real feature and remains open -- it needs a
    /// mid-flight report channel, which does not exist today. A prior
    /// message-kind-plus-drain-effect-plus-event scaffold that once stood
    /// in for one was retired end to end -- 01KZQHZ18MXR7WYVPMTGM5DHT0 --
    /// after landing with every piece except a production sender in any
    /// tree that ever ran; a real channel would be unrelated new work, not
    /// a revival. Building it was declined here in favour of removing the
    /// hang now: nothing can depend on the current behaviour, because the
    /// current behaviour is a hang. Turning it into an immediate, typed
    /// error is a strict improvement and does not foreclose the feature --
    /// it forecloses only the silent version of it.
    ///
    /// Enforced HERE rather than at a tool callsite because this method is
    /// the single chokepoint every subagent path already passes through
    /// (`SubagentHost::start` calls it before anything else), matching the
    /// rule that a mode restriction belongs at the trait boundary and not at
    /// one caller, where any other caller would bypass it.
    ///
    /// Stays a pure internal-consistency check performing no I/O: this is a
    /// cross-field comparison of two values already in hand.
    pub fn validate(&self) -> Result<(), ConwayError> {
        if self.keep_alive && self.result_contract.is_some() {
            return Err(ConwayError::Config {
                detail: "`keep_alive` and `result_contract` cannot be combined on one \
                         subagent: a kept-alive agent never finishes, so its validated \
                         result is never delivered and `await_result` would hang. Set \
                         `keep_alive: false` to receive the validated result, or drop \
                         `result_contract` to keep the agent open."
                    .to_string(),
            });
        }
        Ok(())
    }

    /// Builds a `Fork` spec.
    pub fn fork(prompt: impl Into<String>, budget: Budget) -> Self {
        Self {
            mode: SubagentMode::Fork,
            prompt: prompt.into(),
            agent_def: None,
            role: None,
            pin: None,
            tools: None,
            budget,
            result_contract: None,
            keep_alive: false,
            ephemeral: false,
            ask_origin: None,
            cwd: None,
            root: None,
            tag: None,
            plugin_config: None,
            context: None,
        }
    }

    /// Builds a `Spawn` spec.
    pub fn spawn(prompt: impl Into<String>, agent_def: AgentDefRef, budget: Budget) -> Self {
        Self {
            mode: SubagentMode::Spawn,
            prompt: prompt.into(),
            agent_def: Some(agent_def),
            role: None,
            pin: None,
            tools: None,
            budget,
            result_contract: None,
            keep_alive: false,
            ephemeral: false,
            ask_origin: None,
            cwd: None,
            root: None,
            tag: None,
            plugin_config: None,
            context: None,
        }
    }
}

/// A named reference to an `AgentDef` (by name, resolved by the facade).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentDefRef(pub String);

/// Which tools a subagent may use.
///
/// Matching is case-sensitive; an entry ending in `*` is a prefix match on
/// the tool name, otherwise it is exact equality. `All` selects everything;
/// `Except` selects everything not matched by its list.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSelector {
    All,
    Only(Vec<String>),
    Except(Vec<String>),
}

impl ToolSelector {
    pub fn selects(&self, tool: &ToolName) -> bool {
        match self {
            ToolSelector::All => true,
            ToolSelector::Only(patterns) => patterns.iter().any(|p| pattern_matches(p, tool)),
            ToolSelector::Except(patterns) => !patterns.iter().any(|p| pattern_matches(p, tool)),
        }
    }
}

fn pattern_matches(pattern: &str, tool: &ToolName) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => tool.as_str().starts_with(prefix),
        None => pattern == tool.as_str(),
    }
}

/// A flat, point-in-time snapshot of the whole agent tree. A `Vec` with
/// `parent` links, not a nested tree — this matches the flat, `agent`-tagged
/// event stream and keeps the snapshot trivially serializable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentTreeSnapshot {
    pub root: AgentId,
    pub nodes: Vec<AgentNode>,
    pub at: DateTime<Utc>,
}

/// One agent's entry in an [`AgentTreeSnapshot`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentNode {
    pub agent_id: AgentId,
    pub session: SessionId,
    pub parent: Option<AgentId>,
    pub mode: Option<SubagentMode>,
    pub agent_def: Option<String>,
    pub role: Option<RoleAlias>,
    pub status: AgentStatus,
    pub steps_taken: u32,
    pub budget: Budget,
    /// Whether this agent is an ephemeral `/ask`-style aside , projected from the attached node's
    /// `ephemeral` flag at `snapshot` time (the same source
    /// `Event::AgentSpawned::ephemeral` is stamped from). The snapshot keeps
    /// ephemeral children, so their provenance survives; this flag is what lets a
    /// consumer tell them apart from persistent subagents. `#[serde(default)]`
    /// keeps old serialized snapshots readable: a missing key
    /// deserializes to `false`, matching the pre-ephemeral semantics.
    #[serde(default)]
    pub ephemeral: bool,
}

/// An agent's lifecycle state within the tree.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Starting,
    Running,
    AwaitingPermission,
    AwaitingChildren,
    Finished,
    Failed,
    Cancelled,
}

/// The two ways a caller can stop a running agent --
/// `PHILOSOPHY.md`'s `TERM`/`KILL` analogy. `Immediate` trips the target's
/// `CancellationToken` synchronously (`Runtime::cancel` -> `AgentTree::
/// cancel`) and propagates to the whole subtree structurally, since every
/// child's own token is a `child_token()` of its parent's (`tree.rs`).
/// `Graceful` instead enqueues `AgentMessage::Cancel { hard: false, .. }`,
/// landing at the target's next turn boundary and stopping ONLY the named
/// agent -- it does not itself cancel descendants (that is
/// a deliberate follow-up, not a gap in this
/// one). A graceful cancel also cannot reach an agent parked at the resume
/// gate (an idle `keep_alive` agent between turns, or a resumed root's very
/// first iteration): that wait only selects on the hard cancellation token,
/// the deadline, and the gate's own notify -- never the mailbox -- so an
/// enqueued soft cancel sits undrained until the agent's next real turn
/// boundary, which for an idle `keep_alive` agent may never come. See
/// `SessionHandle::cancel_with`'s own doc for where this is stated to a
/// caller.
///
/// **The caller-supplied reason**
/// reaches the named target's own terminal `AgentResult` on both modes --
/// `Graceful` always has, via its mailbox delivery; `Immediate` now does
/// too, via `AgentTree::cancel`'s stash, read back at whichever of
/// `AgentLoop::finish_cancelled` (the ordinary loop-boundary case) or, for
/// a cancel observed while the target's turn is mid-backend-call (board
/// item), `AgentLoop::finish_error` actually
/// unwinds the target's task. `Immediate`'s whole-subtree propagation is
/// still scoped to the reason's attribution, though: only the explicitly
/// named target carries it, since a descendant swept up by the same
/// structural token trip was never itself passed a reason to attach
/// truthfully.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelMode {
    /// Stops now, without waiting for the current turn to finish. The
    /// default -- an absent choice preserves conway's pre-existing
    /// behavior (every cancellation reachable before this type existed was
    /// this one) rather than silently downgrading it.
    #[default]
    Immediate,
    /// Lets the target finish its in-flight turn, then stops at the next
    /// turn boundary.
    Graceful,
}

impl CancelMode {
    /// `true` for `Immediate`, `false` for `Graceful` -- the `hard` flag
    /// [`AgentMessage::Cancel`] and `conway-runtime`'s `mailbox::classify`
    /// already key on.
    pub fn hard(self) -> bool {
        matches!(self, CancelMode::Immediate)
    }
}

/// A message exchanged between a parent and a child agent.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentMessage {
    Steer {
        from: AgentId,
        text: String,
        at_parent_seq: LogSeq,
    },
    Cancel {
        from: AgentId,
        reason: String,
        hard: bool,
    },
    Result {
        from: AgentId,
        result: AgentResult,
    },
}

/// The event-stream-facing projection of [`AgentMessage`] (`Event::MessageSent`).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Steer,
    Cancel,
    Result,
}

impl From<&AgentMessage> for MessageKind {
    fn from(msg: &AgentMessage) -> Self {
        match msg {
            AgentMessage::Steer { .. } => MessageKind::Steer,
            AgentMessage::Cancel { .. } => MessageKind::Cancel,
            AgentMessage::Result { .. } => MessageKind::Result,
        }
    }
}

/// A request for permission to run one tool call (architecture §4.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub agent_id: AgentId,
    pub agent_path: Vec<AgentId>,
    pub tool: ToolName,
    pub category: ToolCategory,
    pub arguments: serde_json::Value,
    pub rendered: String,
    pub call_id: String,
    /// The proposing tool's own [`crate::ports::RenderKind`] declaration,
    /// copied from the `AuthorizedCall` the broker decided on -- the SAME
    /// value `PatternRule::matches_render` evaluated, never a second lookup
    /// that could disagree with it. A gate that renders a prompt needs it:
    /// whether `rendered` is a shell command or a structured dump decides
    /// both what a pattern grant would mean (the metacharacter gate's
    /// reach) and which rule shape the prompt may honestly offer (see
    /// `permission_pattern::suggested_rule`). `#[serde(default)]` so a
    /// request serialized before this field existed still deserializes --
    /// and the default is the conservative `ShellCommand`, never the
    /// widening one.
    #[serde(default)]
    pub render_kind: crate::ports::RenderKind,
}

/// The human's (or policy's) answer to a [`PermissionRequest`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PermissionDecision {
    AllowOnce,
    AllowAlways { scope: PermissionScope },
    Deny { reason: String },
    DenyWithFeedback { message: String },
}

/// How broadly an `AllowAlways` decision applies.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    Session,
    /// The grant covers only the exact agent whose call prompted it
    /// (this module's own [`GrantScope::Agent`]). Reachable from the TUI
    /// permission prompt (`tui/input.rs`'s `s` scope key, applied by the
    /// `a`/`p` grant keys) and from the facade
    /// (`Conway::grant_permission_pattern`,
    /// `Conway::load_permission_files`, `Conway::trust_permission_file`
    /// all take a scope) -- a per-agent grant never authorizes a sibling's
    /// identical call, which the facade-level seam tests in
    /// `conway/tests/permission_scope_seam.rs` prove end to end.
    Agent,
    /// The grant covers the prompting agent's whole subtree -- any
    /// requester whose `agent_path` contains it
    /// (this module's own [`GrantScope::Subtree`]). Same reachability as
    /// `Agent` above.
    AgentSubtree,
}

/// The scope an ALLOW grant was actually resolved to, once `conway-runtime`
/// has attached the granting agent's identity (`grant_scope_for`,
/// `crates/conway-runtime/src/permission.rs`) -- the resolved counterpart of
/// [`PermissionScope`] above, which only names the *kind* of scope a caller
/// requests, not which agent it resolves to for `Agent`/`AgentSubtree`.
///
/// Lives here rather than in `conway-runtime` (Stage 2b,
/// board item `01KZVYZM7BZRQ54RRB8P814KV9`): the facade's own module doc
/// denied re-exporting any `conway-runtime` type while doing exactly that
/// for this one, and `PermissionScope` right above already named
/// `GrantScope::Agent`/`::Subtree` by hand -- a sign the two halves of one
/// concept were split across the wrong crate boundary. `conway-runtime`
/// keeps its own internal `GrantScope` for the broker's cache/store
/// machinery (`PermissionCtx`-aware `covers()` needs a runtime type this
/// crate must not depend on -- `conway-core` sits at the bottom of the
/// stack, T1), and converts to/from this type at its own boundary
/// (`From` impls in `crates/conway-runtime/src/permission.rs`); `conway`'s
/// facade (`Conway::active_structured_allow_rules`,
/// `Conway::revoke_structured_allow_rule`) carries only this type across
/// its public API, so `conway-cli`'s `/settings` structured-allow review
/// row can name a per-agent grant's `AgentId` without depending on
/// `conway-runtime` (`no_forbidden_deps`,
/// `crates/conway-cli/tests/cli_surface.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantScope {
    /// The grant applies to every requester in the session.
    Session,
    /// The grant covers only the exact agent it was granted to.
    Agent(AgentId),
    /// The grant covers the granting agent's whole subtree -- any requester
    /// whose agent path contains it (a descendant, or the granting agent
    /// itself).
    Subtree(AgentId),
}

impl GrantScope {
    /// A human-readable rendering for the review surface -- the fact a
    /// structured-allow row needs beyond the rule and its origin: who the
    /// grant actually covers. `Session` renders as `"session"`; the other
    /// two name the granting `AgentId` so an operator can tell a
    /// still-narrow per-agent grant from one that has widened.
    pub fn describe(&self) -> String {
        match self {
            GrantScope::Session => "session".to_string(),
            GrantScope::Agent(granter) => format!("agent {granter}"),
            GrantScope::Subtree(granter) => format!("agent subtree under {granter}"),
        }
    }
}

/// The event-stream-facing projection of [`PermissionDecision`]
/// (`Event::PermissionResolved`). `Cached` has no corresponding
/// `PermissionDecision` variant: it is reached when a prior `AllowAlways`
/// decision resolves a later request without prompting again.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecisionKind {
    AllowOnce,
    AllowAlways,
    Denied,
    DeniedWithFeedback,
    Cached,
}

impl From<&PermissionDecision> for PermissionDecisionKind {
    fn from(decision: &PermissionDecision) -> Self {
        match decision {
            PermissionDecision::AllowOnce => PermissionDecisionKind::AllowOnce,
            PermissionDecision::AllowAlways { .. } => PermissionDecisionKind::AllowAlways,
            PermissionDecision::Deny { .. } => PermissionDecisionKind::Denied,
            PermissionDecision::DenyWithFeedback { .. } => {
                PermissionDecisionKind::DeniedWithFeedback
            }
        }
    }
}

/// The first-party conway tool names a THIRD-PARTY agent's `tools:`
/// declaration is matched against, case-insensitively, when importing an
/// agent from another ecosystem (Claude Code's tool names are not conway's).
///
/// **One definition, deliberately.** This lived as two hand-synced copies —
/// one in `conway::agents`, one in `conway_plugin_claude::agents` — each
/// documenting that it must be kept in step with the other by hand, on the
/// stated grounds that "no shared dependency exists to enforce this at
/// compile time". Both crates depend on `conway-core`, so one did. The set
/// decides which tools an imported agent may call: drift between two copies
/// does not fail loudly, it silently changes an agent's permissions, which
/// is the class of duplication [`ToolSelector`]'s own safety story cannot
/// tolerate.
///
/// Not a live query against `conway-tools`' registry: that crate is an
/// OPTIONAL dependency behind conway's `builtin-tools` feature, so neither
/// consumer can assume it is compiled in.
///
/// A tool contributed by an MCP server or another plugin is not in this set
/// either, so an imported agent naming one is also treated as unresolved.
/// That is a fidelity gap, not a safety one — the invariant this set exists
/// to protect is **never silently widen**, and treating a real-but-unlisted
/// tool as unresolved only ever narrows further.
pub const KNOWN_BUILTIN_TOOL_NAMES: &[&str] = &[
    "read", "write", "edit", "grep", "glob", "bash", "cd", "report",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_status_five_variants_round_trip() {
        let cases = vec![
            ResultStatus::Completed,
            ResultStatus::Failed {
                error: "boom".into(),
            },
            ResultStatus::Cancelled {
                reason: "user abort".into(),
            },
            ResultStatus::BudgetExceeded {
                limit: "max_steps=40".into(),
            },
            ResultStatus::Rejected {
                missing: vec!["tool_calling".into()],
            },
        ];
        assert_eq!(cases.len(), 5, "exactly five ResultStatus variants");
        for status in cases {
            let json = serde_json::to_string(&status).unwrap();
            let back: ResultStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn agent_result_new_truncates_summary_on_char_boundary() {
        // A 5000-char summary of 3-byte-in-UTF-8 characters: byte-offset
        // truncation at 2000 would either panic or split a character.
        let summary = "あ".repeat(5000);
        let result = AgentResult::new(
            AgentId::new(),
            SessionId::new(),
            ResultStatus::Completed,
            summary,
        );
        assert_eq!(result.summary.chars().count(), DEFAULT_SUMMARY_LIMIT);
    }

    #[test]
    fn agent_result_new_leaves_short_summary_untouched() {
        let result = AgentResult::new(
            AgentId::new(),
            SessionId::new(),
            ResultStatus::Completed,
            "short summary",
        );
        assert_eq!(result.summary, "short summary");
        assert!(result.is_terminal_success());
    }

    #[test]
    fn is_terminal_success_only_for_completed() {
        let ok = AgentResult::new(
            AgentId::new(),
            SessionId::new(),
            ResultStatus::Completed,
            "",
        );
        assert!(ok.is_terminal_success());
        let failed = AgentResult::new(
            AgentId::new(),
            SessionId::new(),
            ResultStatus::Failed { error: "x".into() },
            "",
        );
        assert!(!failed.is_terminal_success());
    }

    #[test]
    fn budget_default_max_steps_is_40() {
        let b = Budget::default();
        assert_eq!(b.max_steps, 40);
        assert!(b.deadline.is_none());
        assert!(b.max_tokens.is_none());
        assert!(b.max_tool_calls.is_none());
    }

    #[test]
    fn tool_selector_all_selects_everything() {
        assert!(ToolSelector::All.selects(&ToolName::new("anything")));
    }

    #[test]
    fn tool_selector_only_matches_exact_and_prefix() {
        let sel = ToolSelector::Only(vec!["read".into(), "edit_*".into()]);
        assert!(sel.selects(&ToolName::new("read")));
        assert!(sel.selects(&ToolName::new("edit_file")));
        assert!(!sel.selects(&ToolName::new("delete")));
        assert!(!sel.selects(&ToolName::new("edit"))); // no `_` suffix content
    }

    #[test]
    fn tool_selector_except_excludes_exact_and_prefix() {
        let sel = ToolSelector::Except(vec!["delete".into(), "exec_*".into()]);
        assert!(!sel.selects(&ToolName::new("delete")));
        assert!(!sel.selects(&ToolName::new("exec_shell")));
        assert!(sel.selects(&ToolName::new("read")));
    }

    #[test]
    fn subagent_spec_validate_accepts_spawn_without_agent_def() {
        // the original "agent_def mandatory for spawn" rule is relaxed:
        // a spawn with no agent_def is valid and means "inherit the
        // spawning session's role/model" (see `validate`'s own doc).
        let spec = SubagentSpec {
            mode: SubagentMode::Spawn,
            prompt: "do it".into(),
            agent_def: None,
            role: None,
            pin: None,
            tools: None,
            budget: Budget::default(),
            result_contract: None,
            keep_alive: false,
            ephemeral: false,
            ask_origin: None,
            cwd: None,
            root: None,
            tag: None,
            plugin_config: None,
            context: None,
        };
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn subagent_spec_validate_ok_for_fork_without_agent_def() {
        let spec = SubagentSpec::fork("do it", Budget::default());
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn subagent_spec_validate_ok_for_spawn_with_agent_def() {
        let spec = SubagentSpec::spawn("do it", AgentDefRef("reviewer".into()), Budget::default());
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn fork_and_spawn_constructors_default_cwd_none() {
        // C1: `cwd` didn't exist before this item; both constructors default
        // it to `None` ("inherit the parent's cwd"), preserving the
        // pre-existing fork/spawn behavior unchanged.
        let fork = SubagentSpec::fork("x", Budget::default());
        assert_eq!(fork.cwd, None);
        let spawn = SubagentSpec::spawn("x", AgentDefRef("r".into()), Budget::default());
        assert_eq!(spawn.cwd, None);
    }

    /// (b) C1's own acceptance test: a `SubagentSpec` in the shape it had
    /// before this item -- no `cwd` key at all, exactly what pre-C1 code (or
    /// any external caller/persisted snapshot) produced -- still
    /// deserializes, with `cwd` landing on its `None` default rather than
    /// failing or requiring the key.
    #[test]
    fn legacy_subagent_spec_json_without_cwd_deserializes_to_none() {
        let spec = SubagentSpec::spawn("do it", AgentDefRef("reviewer".into()), Budget::default());
        let mut value = serde_json::to_value(&spec).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("cwd")
            .expect("cwd is a real key in the current shape, or this test proves nothing");

        let legacy: SubagentSpec = serde_json::from_value(value).unwrap();
        assert_eq!(
            legacy.cwd, None,
            "a cwd-less legacy SubagentSpec must deserialize with cwd: None"
        );
        // Every other field round-trips untouched -- this isn't a partial
        // legacy shape, just today's shape minus the one new key.
        assert_eq!(legacy.mode, spec.mode);
        assert_eq!(legacy.prompt, spec.prompt);
        assert_eq!(legacy.agent_def, spec.agent_def);
        assert_eq!(legacy.budget, spec.budget);
    }

    #[test]
    fn fork_and_spawn_constructors_default_root_none() {
        // S3: `root` didn't exist before this item; both constructors
        // default it to `None` ("inherit the parent's root, unconfined
        // stays unconfined"), preserving pre-existing fork/spawn behavior
        // unchanged -- mirrors `fork_and_spawn_constructors_default_cwd_none`.
        let fork = SubagentSpec::fork("x", Budget::default());
        assert_eq!(fork.root, None);
        let spawn = SubagentSpec::spawn("x", AgentDefRef("r".into()), Budget::default());
        assert_eq!(spawn.root, None);
    }

    /// S3's own acceptance test, mirroring
    /// `legacy_subagent_spec_json_without_cwd_deserializes_to_none`: a
    /// `SubagentSpec` in the shape it had before this item -- no `root` key
    /// at all -- still deserializes, with `root` landing on its `None`
    /// default rather than failing or requiring the key.
    #[test]
    fn legacy_subagent_spec_json_without_root_deserializes_to_none() {
        let spec = SubagentSpec::spawn("do it", AgentDefRef("reviewer".into()), Budget::default());
        let mut value = serde_json::to_value(&spec).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("root")
            .expect("root is a real key in the current shape, or this test proves nothing");

        let legacy: SubagentSpec = serde_json::from_value(value).unwrap();
        assert_eq!(
            legacy.root, None,
            "a root-less legacy SubagentSpec must deserialize with root: None"
        );
        assert_eq!(legacy.mode, spec.mode);
        assert_eq!(legacy.prompt, spec.prompt);
        assert_eq!(legacy.agent_def, spec.agent_def);
        assert_eq!(legacy.budget, spec.budget);
    }

    #[test]
    fn fork_and_spawn_constructors_default_plugin_config_none() {
        // Per-agent plugin configuration didn't exist before this item; both
        // constructors default it to `None` ("inherit the parent's own
        // effective per-agent config, unchanged") -- mirrors
        // `fork_and_spawn_constructors_default_root_none`.
        let fork = SubagentSpec::fork("x", Budget::default());
        assert_eq!(fork.plugin_config, None);
        let spawn = SubagentSpec::spawn("x", AgentDefRef("r".into()), Budget::default());
        assert_eq!(spawn.plugin_config, None);
    }

    /// Mirrors `legacy_subagent_spec_json_without_root_deserializes_to_none`:
    /// a `SubagentSpec` in the shape it had before this item -- no
    /// `plugin_config` key at all -- still deserializes, with
    /// `plugin_config` landing on its `None` default rather than failing or
    /// requiring the key.
    #[test]
    fn legacy_subagent_spec_json_without_plugin_config_deserializes_to_none() {
        let spec = SubagentSpec::spawn("do it", AgentDefRef("reviewer".into()), Budget::default());
        let mut value = serde_json::to_value(&spec).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("plugin_config")
            .expect(
                "plugin_config is a real key in the current shape, or this test proves nothing",
            );

        let legacy: SubagentSpec = serde_json::from_value(value).unwrap();
        assert_eq!(
            legacy.plugin_config, None,
            "a plugin_config-less legacy SubagentSpec must deserialize with plugin_config: None"
        );
        assert_eq!(legacy.mode, spec.mode);
        assert_eq!(legacy.prompt, spec.prompt);
        assert_eq!(legacy.agent_def, spec.agent_def);
        assert_eq!(legacy.budget, spec.budget);
    }

    #[test]
    fn subagent_spec_fork_and_spawn_default_ephemeral_false() {
        // Regression: existing fork/spawn is non-ephemeral. `ephemeral` is a
        // listing-visibility bit, NOT a mode -- provenance is unaffected -- and
        // defaults `false` via both constructors so pre-existing fork/spawn
        // behavior is unchanged.
        let fork = SubagentSpec::fork("x", Budget::default());
        assert!(!fork.ephemeral);
        let spawn = SubagentSpec::spawn("x", AgentDefRef("r".into()), Budget::default());
        assert!(!spawn.ephemeral);
    }

    #[test]
    fn message_kind_from_agent_message() {
        let msg = AgentMessage::Cancel {
            from: AgentId::new(),
            reason: "r".into(),
            hard: true,
        };
        assert_eq!(MessageKind::from(&msg), MessageKind::Cancel);
        let msg = AgentMessage::Steer {
            from: AgentId::new(),
            text: "s".into(),
            at_parent_seq: LogSeq::ZERO,
        };
        assert_eq!(MessageKind::from(&msg), MessageKind::Steer);
    }

    #[test]
    fn permission_decision_kind_from_decision() {
        assert_eq!(
            PermissionDecisionKind::from(&PermissionDecision::AllowOnce),
            PermissionDecisionKind::AllowOnce
        );
        assert_eq!(
            PermissionDecisionKind::from(&PermissionDecision::AllowAlways {
                scope: PermissionScope::Session
            }),
            PermissionDecisionKind::AllowAlways
        );
        assert_eq!(
            PermissionDecisionKind::from(&PermissionDecision::Deny {
                reason: "no".into()
            }),
            PermissionDecisionKind::Denied
        );
        assert_eq!(
            PermissionDecisionKind::from(&PermissionDecision::DenyWithFeedback {
                message: "try again".into()
            }),
            PermissionDecisionKind::DeniedWithFeedback
        );
    }

    #[test]
    fn agent_tree_snapshot_round_trips() {
        let node = AgentNode {
            agent_id: AgentId::new(),
            session: SessionId::new(),
            parent: None,
            mode: None,
            agent_def: Some("reviewer".into()),
            role: Some(RoleAlias::new("coder")),
            status: AgentStatus::Running,
            steps_taken: 3,
            budget: Budget::default(),
            ephemeral: false,
        };
        let snapshot = AgentTreeSnapshot {
            root: node.agent_id,
            nodes: vec![node],
            at: "2026-07-20T00:00:00Z".parse().unwrap(),
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: AgentTreeSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snapshot, back);
    }

    #[test]
    fn agent_node_without_ephemeral_key_deserializes_to_false() {
        // Backward compat: a snapshot serialized before the `ephemeral`
        // field existed has no key for it; `#[serde(default)]` must read it
        // back as `false` (the pre-ephemeral semantics), never fail.
        let json = r#"{
            "agent_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "session": "01ARZ3NDEKTSV4RRFFQ69G5FB0",
            "parent": null,
            "mode": null,
            "agent_def": null,
            "role": null,
            "status": "running",
            "steps_taken": 0,
            "budget": {"max_steps": 40, "deadline": null, "max_tokens": null, "max_tool_calls": null}
        }"#;
        let node: AgentNode = serde_json::from_str(json).unwrap();
        assert!(!node.ephemeral);
    }
}
