//! Root-agent lifecycle: [`RootSpec`], [`ResumeSpec`], and
//! [`Runtime::start_root`]/[`Runtime::resume_root`] -- the two entry points
//! that create a live root [`AgentLoop`] task and attach it to
//! `super::Runtime`'s bookkeeping (`agents`, `AgentTree`). Split out of
//! `runtime.rs` itself (board item `01KZY8SB15M9KWZGGGMZAEM1E0`) because
//! together they are its largest and highest-churn seam; nothing about
//! `Runtime`'s own fields, construction, or the shared `launch_agent`/
//! `agent_session`/`agent_mailbox` tail moved here -- see `super`'s module
//! doc for those.
//!
//! `start_root` is a hook seam in its own right: it fires `prompt_submitted`
//! (deny-capable, at the very top, before anything is created) and
//! `session_starting` (observation-only, as the very last statement, once
//! every id it names already exists and the agent is fully attached) --
//! both stay exactly where they were, at the same two boundaries, since
//! moving either would change what is and is not yet visible when a hook
//! observes it. `resume_root` deliberately does not fire `session_starting`
//! -- see its own call site below for why.
//!
//! **Skill resolution (board item `01M03GKZ3MGZK3ETP6R27E2M9Y`):** both
//! `start_root` and `resume_root` resolve the resolved `AgentDef.skills`
//! name list against `Runtime.skills` (`RuntimeDeps.skills`, sourced from
//! `crate::skills::load_skill_defs` on the facade side) via
//! [`resolve_skills`] below, replacing what used to be an unconditional
//! `skills: Vec::new()` at both call sites. A name the registry does not
//! contain is `RuntimeError::InvalidSpec`, via the same `subagent::
//! invalid_spec` helper this file's own root/cwd containment checks already
//! use -- never a silent drop.

use super::*;

/// Resolves `agent_def.skills` (a list of skill *names*) against `skills`
/// (the runtime's discovered `SkillDef` registry) into the ordered
/// `SkillFragment` list `AgentSpec.skills` carries into context assembly
/// (`ContextBuilder::build`'s `[1] SkillFragments*` step, which turns each
/// into a `Provenance::Skill { name }` segment). `agent_def: None` (no
/// resolved `AgentDef`, e.g. a root started without one) yields no skills,
/// exactly as before this item -- there is no other selection mechanism.
///
/// An unknown name is a spawn-time error, not a silent drop: the operator
/// wrote `skills: [name]` in that def's frontmatter expecting it to resolve
/// to real body text, mirroring `crate::skills`' (`crates/conway/src/
/// skills.rs`, facade-side) own loud-failure discipline for a malformed
/// `SKILL.md`.
fn resolve_skills(
    agent_def: Option<&AgentDef>,
    skills: &HashMap<String, conway_core::config::SkillDef>,
) -> Result<Vec<crate::context::SkillFragment>, RuntimeError> {
    let Some(def) = agent_def else {
        return Ok(Vec::new());
    };
    def.skills
        .iter()
        .map(|name| {
            skills
                .get(name)
                .map(|skill| crate::context::SkillFragment {
                    name: skill.name.clone(),
                    text: skill.body.clone(),
                })
                .ok_or_else(|| {
                    crate::subagent::invalid_spec(ConwayError::Config {
                        detail: format!(
                            "agent def `{}` names unknown skill `{name}` \
                             (no `.conway/skills/{name}/SKILL.md` was discovered)",
                            def.name
                        ),
                    })
                })
        })
        .collect()
}

/// The complete specification for starting a new root agent (i.e. one with
/// no parent — the entry point of a fresh agent tree).
pub struct RootSpec {
    /// Overrides the store-assigned session id (useful for reproducible
    /// tests); `None` generates a fresh one.
    pub session: Option<SessionId>,
    pub agent_def: Option<AgentDefRef>,
    pub role: Option<RoleAlias>,
    pub tools: Option<ToolSelector>,
    pub budget: Budget,
    pub cwd: PathBuf,
    /// This root agent's own
    /// confinement root -- the S3/S5 primitive (`SubagentSpec::root`,
    /// `AgentRoot`, `PermissionBroker::check_root`), finally reachable for
    /// the agent an operator actually talks to. Before this field existed,
    /// `start_root` always passed `SessionMeta.root: None` and
    /// `AgentLoop.root: None`, so `AgentRoot::reconstruct` always produced
    /// `Unconfined` for a root agent and `check_root` returned `Proceed`
    /// without ever inspecting `path_args` -- the root check was real, but
    /// entirely unreachable from the top of the tree.
    ///
    /// `None` (every caller before this field existed, and still the
    /// default for every caller that does not set it) preserves that exact
    /// behavior: unconfined, byte-for-byte. `Some(path)` is resolved exactly
    /// like a spawned child's `SubagentSpec::root` (`subagent.rs`'s
    /// `SubagentHost::start`): relative paths resolve against `cwd` above
    /// (a root agent has no parent cwd to resolve against), the result must
    /// canonicalize, and `cwd` itself must already fall inside it --
    /// `start_root` returns `RuntimeError::InvalidSpec { .. }` (via the same
    /// `subagent::invalid_spec` helper `resume_root` already uses) rather
    /// than starting an agent whose own working directory sits outside its
    /// own confinement before a single tool call ever runs.
    /// Cwd is never itself a security boundary (S0's own charter) -- root is
    /// -- so this is a distinct field, not an inference from `cwd`; the two
    /// are configured as a pair (see `conway-cli`'s `--root`) precisely so
    /// an operator cannot confuse them.
    pub root: Option<PathBuf>,
    pub prompt: Option<String>,
    /// Opt-in multi-turn keep-alive (see `agent_loop::AgentSpec::keep_alive`'s
    /// own doc for the bug this fixes and why it must stay opt-in). `false`
    /// preserves this crate's pre-existing behavior exactly: the started
    /// agent's task terminates after its first `Completed` turn, same as
    /// every `RootSpec` caller before this field existed.
    pub keep_alive: bool,
    /// Pins the model for this session, overriding the role's chain.
    /// `start_root` prefers this over the `agent_def`-sourced pin when
    /// present -- see that method's own doc for the precedence.
    pub model: Option<ModelRef>,
    /// Replaces the `[0] SystemPrompt` segment's text outright when `Some`,
    /// regardless of whether `agent_def` also resolves to a known def --
    /// `start_root` still resolves `agent_def` for `role`/`tools`/`model`
    /// exactly as before, only the system-prompt TEXT is swapped. `None`
    /// (every caller before this field existed) preserves the prior
    /// behavior exactly: the resolved `agent_def`'s own `system_prompt`, or
    /// no `SystemPrompt` segment at all when there is no `agent_def`.
    /// `conway-cli`'s `--system-prompt`/`--append-system-prompt` are this
    /// field's one caller today (`conway::Conway::new_session`, from
    /// `SessionSpec::system_prompt_override`) -- see that field's own doc
    /// for how the two flags combine into the single string landing here.
    pub system_prompt_override: Option<String>,
    /// The schema this root agent's `structured` result must satisfy,
    /// threaded straight into `AgentSpec::result_contract` -- see that
    /// field's own doc for the enforcement mechanism (`AgentLoop::
    /// run_inner`'s natural-completion branch: `Ok` proceeds, a first
    /// failure gets one corrective retry via a `SystemNote`, a second is
    /// terminal `ResultStatus::Rejected { missing }`). Before this field
    /// existed, a root agent had no way to carry a contract at all --
    /// `start_root` always passed `AgentSpec::result_contract: None`
    /// (only a fork/spawn child, via `SubagentSpec::result_contract`,
    /// could declare one) -- so the identical, already-tested enforcement
    /// mechanism simply never reached the one agent an operator's own
    /// prompt talks to. `None` (every caller before this field existed)
    /// preserves that exact behavior. `conway-cli`'s `--output-schema` is
    /// this field's motivating caller, via `conway::SessionSpec::
    /// result_contract` (`conway::Conway::new_session`) -- see that
    /// field's own doc for the call-site-wins-over-agent-def precedence,
    /// mirrored from `subagent.rs`'s identical rule for a forked/spawned
    /// child's own contract.
    pub result_contract: Option<schemars::schema::RootSchema>,
}

/// The specification for re-registering a persisted session's agent as a
/// live root agent (— closes the//Q-1 session-
/// continuity gap). Mirrors [`RootSpec`] minus `prompt` (resuming never
/// appends an initial `UserTurn` — the caller's continuation arrives via a
/// later [`Runtime::prompt`] call) and minus the fields recoverable from the
/// persisted [`SessionMeta`] once it is loaded (`agent_def`, `role`, `cwd`
/// all fall back to the header's own values when left `None` here, exactly
/// as `start_root` falls back to an `AgentDef`'s values). `tools` and
/// `budget` are never persisted in `SessionMeta` — like `RootSpec`, both
/// must be supplied fresh on every resume.
pub struct ResumeSpec {
    /// The session to resume. Must already exist in the store — `resume_root`
    /// reads its `SessionMeta` via `store.meta` and does NOT `store.create`.
    pub session: SessionId,
    pub agent_def: Option<AgentDefRef>,
    pub role: Option<RoleAlias>,
    pub tools: Option<ToolSelector>,
    pub budget: Budget,
    /// Overrides the persisted `SessionMeta::cwd`; `None` reuses it.
    pub cwd: Option<PathBuf>,
    /// The schema this resumed agent's `structured` result must satisfy,
    /// threaded straight into `AgentSpec::result_contract` -- see
    /// [`RootSpec::result_contract`]'s own doc for the enforcement mechanism,
    /// which is identical here (the field feeds the same `AgentLoop::
    /// run_inner` natural-completion check regardless of which spec started
    /// the agent).
    ///
    /// **Closes a real gap (board item `01M03FQDF33AZ8G258516EDWQD`):**
    /// before this field existed, `resume_root` always passed `AgentSpec::
    /// result_contract: None`, unconditionally -- not because resuming a
    /// session is incoherent with a contract (it is not: the SAME `AgentLoop`
    /// enforcement this field now reaches for a resumed agent is already
    /// exercised for a freshly `start_root`ed one via `RootSpec::
    /// result_contract`, and for a live fork/spawn child via `SubagentSpec::
    /// result_contract`), but because `ResumeSpec` itself had no field to
    /// carry one through. That made `conway::Conway::fork_from` -- the ONE
    /// caller of `resume_root` that receives a fresh, per-call spec
    /// (`ForkSpec`) with its own `result_contract` field already on it --
    /// silently drop a contract an embedder set: `ForkSpec::result_contract`
    /// round-tripped through `From<ForkSpec> for SubagentSpec` (honored on
    /// the live `SessionHandle::fork` path) but was never even read by
    /// `crate::fork_child::fork_child` (`conway`'s own module), which built a
    /// `ResumeSpec` with no way to carry it. `None` (`conway::Conway::
    /// resume`'s own caller, which has no per-call spec at all -- `resume`
    /// takes only a `SessionId` -- and so always passes `None` here,
    /// preserving that binding's existing behavior exactly) is the only
    /// value that can reach this field for a genuine resume; `fork_from`'s
    /// `ForkChildRequest::result_contract` is the one caller that can supply
    /// `Some`.
    pub result_contract: Option<schemars::schema::RootSchema>,
    /// Opt-in multi-turn keep-alive for a resumed/forked agent -- the
    /// `ResumeSpec` counterpart of [`RootSpec::keep_alive`], threaded
    /// straight into `AgentSpec::keep_alive` (see that field's own doc for
    /// the `ResumeGate` re-arming mechanism that makes a `keep_alive` agent
    /// idle for its next prompt instead of terminating on natural
    /// completion).
    ///
    /// **Closes a real gap (board item `01M03KZXR1KF77YRAW4W4GE6KK`):**
    /// before this field existed, `resume_root` hardcoded
    /// `AgentSpec::keep_alive: false`, so `conway::Conway::fork_from` -- the
    /// ONE caller of `resume_root` that receives a fresh, per-call spec
    /// (`ForkSpec`) with its own `keep_alive` field already on it --
    /// silently dropped the flag: `ForkSpec::keep_alive(true)` round-tripped
    /// through `From<ForkSpec> for SubagentSpec` (honored on the live
    /// `SessionHandle::fork` path) but was never even read by
    /// `crate::fork_child::fork_child` (`conway`'s own module), which built a
    /// `ResumeSpec` with no way to carry it through. A caller that set
    /// `keep_alive(true)` on a `fork_from` got a one-shot child that
    /// terminated on its first completed turn, with no error -- a silent
    /// behavioural difference between two ways of doing the same thing.
    ///
    /// **Re-arming semantics (the design decision the spec demanded before
    /// wiring):** `resume_root` always sets `ResumeGate::awaiting_prompt:
    /// true` up front (see the `resume_gate` field literal below), so a
    /// resumed/forked agent idles until its caller's FIRST `prompt` rather
    /// than racing the persisted transcript. `keep_alive`'s end-of-turn
    /// re-arming (`AgentLoop::run_inner`'s natural-completion branch sets
    /// `awaiting_prompt: true` and `continue`s when `AgentSpec::keep_alive`
    /// is `true`) composes with that initial gate cleanly -- BOTH set
    /// `awaiting_prompt: true` and wait on the SAME `notify`, so there is no
    /// second mechanism and no conflict: the first-turn gate ensures the
    /// child never runs before the caller's first prompt, and `keep_alive`
    /// ensures the child never finishes after a completed turn. The previous
    /// item left this field out not because of a real interaction problem
    /// but to reason about it separately; that reasoning confirms the
    /// composition is safe. `false` (the `Conway::resume` caller, which has
    /// no per-call spec and so always passes `false` here, preserving that
    /// binding's existing one-shot behaviour exactly) is the only value that
    /// can reach this field for a genuine resume; `fork_from`'s
    /// `ForkChildRequest::keep_alive` is the one caller that can supply
    /// `true`.
    pub keep_alive: bool,
}

impl Runtime {
    pub async fn start_root(&self, spec: RootSpec) -> Result<AgentId, RuntimeError> {
        let agent_id = AgentId::new();
        let session_id = spec.session.unwrap_or_default();

        // `prompt_submitted` for a session's FIRST prompt (//). Dispatched here, at the very top,
        // rather than beside `session_starting` at the bottom: a denial must
        // prevent the prompt from ever reaching the agent loop, and doing it
        // before any store append or tree attach means a refused prompt leaves
        // NOTHING half-created. The ids above are minted first only so the
        // payload can name them.
        //
        // A prompt-less root (the interactive TUI, which idles until the user
        // types) submits no text, so there is nothing to submit and nothing
        // fires -- the event is about a prompt, not about a session, and
        // `session_starting` is the one that says a session began.
        if let Some(text) = spec.prompt.as_deref() {
            if let Some(reason) = self
                .hooks
                .dispatch_deny_only(
                    crate::hook_dispatch::PROMPT_SUBMITTED,
                    serde_json::json!({
                        "text": text,
                        "agent_id": agent_id,
                        "session": session_id,
                        "first_prompt": true,
                    }),
                )
                .await
            {
                return Err(RuntimeError::PromptDenied { reason });
            }
        }

        let agent_def = spec
            .agent_def
            .as_ref()
            .and_then(|r| self.agent_defs.get(r.0.as_str()));

        let role = spec
            .role
            .clone()
            .or_else(|| agent_def.and_then(|d| d.role.clone()))
            .unwrap_or_else(|| RoleAlias::new("default"));

        // `spec.system_prompt_override` (`--system-prompt`/
        // `--append-system-prompt`) wins outright when present -- see
        // `RootSpec::system_prompt_override`'s own doc. `agent_def` is
        // still resolved above regardless, for `role`/`tools`/`model`
        // below; only the system-prompt TEXT is swapped here.
        let system_prompt = match &spec.system_prompt_override {
            Some(text) => Some(crate::context::SystemPromptSpec {
                agent_def: agent_def
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| "cli-override".to_string()),
                text: text.clone(),
            }),
            None => agent_def.map(|d| crate::context::SystemPromptSpec {
                agent_def: d.name.clone(),
                text: d.system_prompt.clone(),
            }),
        };
        // Resolves `agent_def.skills` (names) against the discovered
        // registry into `SkillFragment`s -- see the module doc's "Skill
        // resolution" note and `resolve_skills`'s own doc.
        let skills = resolve_skills(agent_def, &self.skills)?;
        let tools = spec
            .tools
            .clone()
            .or_else(|| agent_def.map(|d| d.tools.clone()));
        // `spec.model` (a caller-supplied pin, e.g. `--model`) takes
        // precedence over the `agent_def`'s own configured model.
        let pin = spec
            .model
            .clone()
            .or_else(|| agent_def.and_then(|d| d.model.clone()));

        // resolve and validate this
        // root agent's own confinement root ONCE, mirroring `subagent.rs`'s
        // `SubagentHost::start` spawn-time validation for a spawned child's
        // `Some(requested)` root (see that method's own doc for the full
        // shape this repeats). A root agent has no parent root to narrow
        // against -- it IS the top of the tree -- so only two checks apply
        // here: the requested root itself must canonicalize, and `spec.cwd`
        // must already fall inside it. A relative `spec.root` resolves
        // against `spec.cwd` (there is no parent cwd to resolve against, and
        // `cwd`/`root` are configured as a pair -- see `RootSpec::root`'s
        // own doc).
        let root: Option<PathBuf> = match &spec.root {
            None => None,
            Some(requested) => {
                // Min-1: resolve via the SHARED rule -- one implementation,
                // never restated (absolute -> as-is, relative -> join base, NUL
                // -> None) instead of inlining two-thirds of it and silently
                // dropping the NUL guard -- the same call `subagent.rs`'s
                // spawn-time root resolution now makes. A relative root
                // resolves against `spec.cwd`, exactly as before. A non-UTF-8
                // or NUL-carrying root is a typed config rejection -- untrusted
                // input -- never a panic.
                let requested_str = requested.to_str().ok_or_else(|| {
                    crate::subagent::invalid_spec(ConwayError::Config {
                        detail: format!(
                            "root agent's root {} is not valid UTF-8",
                            requested.display()
                        ),
                    })
                })?;
                let resolved =
                    crate::permission::resolve_like_the_tool_will(&spec.cwd, requested_str)
                        .ok_or_else(|| {
                            crate::subagent::invalid_spec(ConwayError::Config {
                                detail:
                                    "root agent's root contains a NUL byte the OS cannot resolve"
                                        .to_string(),
                            })
                        })?;
                let canonical_root = CanonicalRoot::new(&resolved).map_err(|err| {
                    crate::subagent::invalid_spec(ConwayError::Config {
                        detail: format!(
                            "root agent's root {} does not canonicalize: {err}",
                            resolved.display()
                        ),
                    })
                })?;
                match canonical_root.contains(&spec.cwd) {
                    Containment::Inside => {}
                    Containment::Outside | Containment::Undecidable => {
                        // Same "show both operands on the same footing"
                        // treatment as `resume_root`'s identical check
                        // below, and `subagent.rs`'s own cwd-outside-root
                        // error -- see either's comment for why.
                        let canonical_cwd = spec.cwd.canonicalize().ok();
                        let shown = match &canonical_cwd {
                            Some(c) if c != &spec.cwd => {
                                format!("{} (resolved: {})", spec.cwd.display(), c.display())
                            }
                            _ => spec.cwd.display().to_string(),
                        };
                        return Err(crate::subagent::invalid_spec(ConwayError::Config {
                            detail: format!(
                                "root agent's cwd {} is outside its own root {}",
                                shown,
                                canonical_root.as_path().display(),
                            ),
                        }));
                    }
                }
                Some(canonical_root.as_path().to_path_buf())
            }
        };

        // (retirement) `conway.fs`'s
        // OWN root, derived from the SAME `root` just resolved above --
        // never a second `spec`-level field, never a second validation. See
        // `crate::permission::derive_fs_root_config`'s own doc for why this
        // derivation exists at all: `root` alone, since the harness-level
        // per-tool `PathArgs::Named` check retired, no longer confines a
        // single ordinary tool call by itself -- and why it is gated on
        // `fs_root_is_narrowable`: a `Runtime` with no `conway.fs`-shaped
        // plugin installed must not turn an unrelated `--root` into a hard
        // startup failure.
        let plugin_config = crate::permission::derive_fs_root_config(
            root.as_deref(),
            None,
            self.loop_deps
                .registry
                .narrowing_rules()
                .contains_key(crate::permission::CONWAY_FS_ROOT_CONFIG_KEY),
        )
        .unwrap_or_default();

        let meta = SessionMeta {
            id: session_id,
            agent_id,
            origin: None,
            agent_def: agent_def.map(|d| d.name.clone()),
            role: Some(role.clone()),
            created: Utc::now(),
            cwd: spec.cwd.clone(),
            labels: Vec::new(),
            // A root is never ephemeral -- only a facade-level fork-ask
            // child is (`conway`'s `SessionHandle::ask`, which sets this
            // itself before calling `store.fork`, never `start_root`).
            ephemeral: false,
            // B5: a root is never an `/ask` child either -- the tag only
            // exists on ephemeral ask children (stamped from the spec in
            // `subagent.rs`'s `SubagentHost::start`).
            ask_origin: None,
            // the resolved, canonical
            // root computed above -- `None` (unconfined) exactly as before
            // this field existed unless `spec.root` was set.
            root: root.clone(),
            // (retirement) A root agent's own
            // per-agent plugin config is the process-wide global config
            // (mirrors `AgentLoop::plugin_config`'s own doc below) PLUS the
            // derived `conway.fs.root` entry computed just above -- `Runtime::
            // resume_root`'s existing (unchanged) generic re-derivation
            // logic re-validates this exactly as it would any other
            // per-agent config on resume, so persisting it here is enough
            // for it to survive a resume correctly with no further wiring.
            plugin_config: plugin_config.clone(),
        };
        self.store.create(meta).await?;

        // Seed an initial user turn ONLY when the caller supplied a prompt.
        // A prompt-less root (the interactive TUI, and any `new_session`
        // whose first prompt arrives later via `Runtime::prompt`) starts
        // IDLE: no empty placeholder turn is written and the loop gates its
        // first iteration (see `resume_gate` below), so the agent never runs
        // a turn against an empty prompt and "explores" before the user has
        // said anything. `append`'s `assign_seq` overwrites `seq` with the
        // store's own next value regardless.
        //
        // The matching live `Event::UserTurn` (this item) is emitted right
        // after the append succeeds -- ordering-safe unconditionally: a root
        // is attached below with `kind: None` (see that call's own comment),
        // so `AgentTree::attach` never emits `Event::AgentSpawned` for it at
        // all, and the "AgentSpawned precedes every event for its agent"
        // guarantee is vacuous for a root either way.
        if let Some(text) = spec.prompt.clone() {
            self.store
                .append(
                    &session_id,
                    LogRecord::UserTurn {
                        seq: LogSeq::ZERO,
                        ts: Utc::now(),
                        text: text.clone(),
                        prov: Provenance::UserPrompt,
                    },
                )
                .await?;
            self.bus.emit(
                session_id,
                agent_id,
                Event::UserTurn {
                    text,
                    prov: Provenance::UserPrompt,
                },
            );
        }

        let last_report = Arc::new(Mutex::new(None));
        let agent_spec = AgentSpec {
            system_prompt,
            skills,
            tools,
            role: role.clone(),
            pin,
            budget: spec.budget.clone(),
            // Deliberately `None`, not a gap: `ContextBuilder::build` runs
            // before routing resolves a concrete model, so this can only
            // ever be a pre-routing placeholder. The prompt-caching item's
            // real, capability-keyed cache-hint attachment happens as a
            // POST-routing pass in `attempt.rs`'s `attach_route_cache_hints`
            // -- see `subagent.rs`'s module doc ("`CacheMode` is hardcoded,
            // not caller-supplied") for the full rationale, which applies
            // identically to a root.
            cache_mode: CacheMode::None,
            cache_ttl: CacheTtl::FiveMinutes,
            headroom_override: None,
            max_parallel_tools: DEFAULT_MAX_PARALLEL_TOOLS,
            report_slot: Some(last_report.clone()),
            // `RootSpec::result_contract` -- see that field's own doc for
            // the mechanism this finally makes reachable for a root agent
            // (previously always `None` here, unconditionally).
            result_contract: spec.result_contract,
            keep_alive: spec.keep_alive,
            // A root agent has no `SubagentSpec` to source a consumer tag
            // from either -- `RootSpec` gains
            // no counterpart field; out of this item's scope.
            tag: None,
        };

        let cancel = CancellationToken::new();
        let (mailbox_tx, mailbox_rx) = Mailbox::new(mailbox::RUNTIME_CAPACITY);
        let mailbox_tx =
            mailbox_tx.with_events(self.bus.clone(), session_id, agent_id, cancel.clone());
        let agent_loop = AgentLoop {
            agent_id,
            session: session_id,
            parent: None,
            agent_path: vec![agent_id],
            cwd: spec.cwd.clone(),
            // matches `meta.root`
            // above -- the same resolved, canonical root (or `None`,
            // unconfined, unchanged from before this field existed).
            root: root.clone(),
            // (retirement) the SAME
            // effective config just computed for `meta.plugin_config` above
            // (process-wide global config plus the derived `conway.fs.root`
            // entry, if any) -- never re-derived, never
            // `self.loop_deps.plugin_config.clone()` alone (that would drop
            // the derived entry and silently unconfine every ordinary tool
            // call again).
            plugin_config: Arc::new(plugin_config.clone()),
            deps: self.loop_deps.clone(),
            spec: agent_spec,
            cancel: cancel.clone(),
            // A root agent's context never inherits anything (only
            // a fork child gets `Some`).
            inherited: None,
            inbox: mailbox_rx,
            // A root has no parent to deliver a terminal `Result` to
            //.
            parent_mailbox: None,
            pending_cancel: None,
            // A root started WITHOUT an initial prompt (the interactive TUI;
            // any `new_session` whose first prompt arrives later via
            // `Runtime::prompt`) gates its first iteration and idles until
            // that prompt arrives -- otherwise it would immediately run a
            // turn against the empty placeholder and "explore" before the
            // user has typed anything. A root started WITH a prompt runs its
            // first turn immediately, as before. For a `keep_alive` root,
            // `run_inner` re-arms this same gate at each turn boundary.
            resume_gate: crate::agent_loop::ResumeGate {
                awaiting_prompt: spec.prompt.is_none(),
                notify: Arc::new(tokio::sync::Notify::new()),
            },
        };
        let prompt_notify = agent_loop.resume_gate.notify.clone();
        // [S1.5]: cloned out for the SAME reason `prompt_notify` is,
        // immediately above -- `agent_loop` moves into the spawned task
        // below.
        let plugin_config = agent_loop.plugin_config.clone();

        // A root is started, not spawned (`kind: None`) — see `tree.rs`'s
        // module doc on why that means `attach` will not emit
        // `Event::AgentSpawned` for it.
        self.tree.attach(AgentNode {
            id: agent_id,
            parent: None,
            session: session_id,
            kind: None,
            agent_def: agent_def.map(|d| d.name.clone()),
            role: Some(role),
            budget: spec.budget.clone(),
            cancel: cancel.clone(),
            inherited_upto: None,
            // A root is never ephemeral (:
            // only `conway`'s facade `SessionHandle::ask` builds an ephemeral
            // child, and that goes through `resume_root`, not `start_root`).
            ephemeral: false,
        })?;

        let task: JoinHandle<AgentResult> = tokio::spawn(async move { agent_loop.run().await });
        let join = supervisor::supervise(SuperviseArgs {
            tree: self.tree.clone(),
            bus: self.bus.clone(),
            agent: agent_id,
            session: session_id,
            cancel,
            deadline: spec.budget.deadline,
            grace: supervisor::DEFAULT_GRACE,
            task,
            hooks: self.hooks.clone(),
            // A root has no parent for a `child_reported` result to cross
            // back to (`AgentLoop::finish`'s identical check, and this
            // agent's own `parent: None` a few lines above).
            parent: None,
        });

        let handle = AgentHandle {
            session: session_id,
            mailbox: mailbox_tx,
            last_report,
            prompt_notify,
            join: Arc::new(Mutex::new(Some(join))),
            plugin_config,
        };

        self.agents
            .write()
            .expect("agents lock poisoned")
            .insert(agent_id, handle);

        // `session_starting`, fired
        // ONCE per `start_root` -- not per turn and not per tool call. This is
        // the last statement before the id is returned, so every id the
        // payload names already exists and the agent is fully attached.
        //
        // OBSERVATION ONLY: `dispatch` returns `()`, so a failing hook cannot
        // fail the session start. `resume_root` deliberately does NOT fire it
        // -- resuming an existing session is not starting one, and conflating
        // them would make the event fire twice for one session's lifetime.
        if self
            .hooks
            .will_dispatch(crate::hook_dispatch::SESSION_STARTING)
        {
            self.hooks
                .dispatch(
                    crate::hook_dispatch::SESSION_STARTING,
                    serde_json::json!({
                        "agent_id": agent_id,
                        "session": session_id,
                        "cwd": spec.cwd,
                    }),
                )
                .await;
        }

        Ok(agent_id)
    }

    /// Re-registers an already-persisted session's agent as live:
    /// reads its existing `SessionMeta` via `store.meta` (erroring
    /// `RuntimeError::Store(StoreError::NotFound { .. })` — already a typed
    /// `RuntimeError` via `#[from]`, not a panic and not a `create` — for an
    /// unknown or record-less session id) and registers it into
    /// `Runtime.agents`/`AgentTree` through the same `Runtime::launch_agent`
    /// path `start_root` uses, so `prompt`/`cancel`/`tree`/`context_report`
    /// all work on the returned `AgentId` exactly as they do for a
    /// `start_root` agent.
    ///
    /// Unlike `start_root`, this method does NOT `store.create` (the session
    /// already exists) and does NOT append an initial `UserTurn` — the
    /// caller's continuation prompt arrives via a subsequent
    /// [`Runtime::prompt`] call. `AgentLoop` re-resolves the full effective
    /// transcript from the store on every turn (`conway_core::transcript`'s
    /// `TranscriptResolver`), so "continue from where it left off" falls out
    /// of that existing mechanism once this agent is registered — this
    /// method's job is registration, not transcript replay.
    ///
    /// The returned `AgentId` is `SessionMeta::agent_id` (the id the session
    /// was originally created under), never a freshly minted one: a caller
    /// that persisted the original agent id (e.g. `conway::conway::resume`)
    /// can address this resumed agent with the same id it already has, and
    /// `Runtime::agent_session`/`prompt`/`tree` all resolve against this same
    /// id since it is exactly what gets inserted into `self.agents` and
    /// attached to the tree below.
    ///
    /// ## Child re-registration (criterion 4 disclosure)
    ///
    /// This method attaches only the resumed root to `AgentTree` — it does
    /// NOT walk `store.children` to re-attach past fork/spawn children as
    /// live tree nodes. Those children's own agent tasks are long gone (this
    /// is a process restart, not a live process with tasks to reconnect to);
    /// attaching them as `AgentTree` nodes with no backing task would leave
    /// them permanently `AgentStatus::Running` in `Runtime::tree()` (nothing
    /// would ever call `AgentTree::publish_result` for them), which is a
    /// worse misrepresentation than omitting them. Their history remains
    /// fully readable via `store.children`/`store.read` directly and via
    /// [`Runtime::context_report_at`] (which already resolves an agent id
    /// via a store scan, not the live tree); only *live* re-registration —
    /// i.e., resuming a child as a promptable agent in its own right — is
    /// out of scope for this item. A caller that needs that can call
    /// `resume_root` again with that child's own `SessionId`.
    pub async fn resume_root(&self, spec: ResumeSpec) -> Result<AgentId, RuntimeError> {
        let meta = self.store.meta(&spec.session).await?;
        let agent_id = meta.agent_id;

        // a genuine root's own session records ARE its complete
        // history (`inherited` stays `None`, matching the original,
        // unaffected behavior below) -- but a fork child's own records are,
        // by the zero-copy fork contract (`SessionStore::fork`, D-11), only
        // its OWN post-fork turns; the inherited portion lives in the
        // parent's session by reference and must be resolved here, exactly
        // as `subagent.rs`'s live fork path resolves it for a
        // freshly-forked child (see that module's doc). Detected via
        // `SessionMeta::origin`, the same signal `subagent.rs` itself
        // produces on fork (`mode: SubagentMode::Fork`) -- a spawned
        // child's `origin` is `Some` too, but with `mode: SubagentMode::
        // Spawn`, for which context assembly has never inherited anything
        // (`subagent.rs`'s own spawn branch always builds `inherited:
        // None`), so only the `Fork` arm resolves a prefix here.
        //
        // `resolver().resolve(store, &child)` -- what `subagent.rs` uses --
        // is NOT reusable as-is: it returns the child's FULL effective
        // transcript at its current head, which is only exactly "the
        // parent's prefix" at the one moment `subagent.rs` calls it
        // (immediately after `store.fork`, before the child owns any
        // records of its own). A resumed fork child may already have run
        // turns of its own (non-empty own records), and `AgentLoop` reads
        // those own records separately every turn -- folding them into
        // `inherited` too would double-count them. `TranscriptResolver::
        // resolve_prefix` (made `pub` for this, `conway-session`)
        // is the shared primitive both paths already bottom out on; calling
        // it directly against `(origin.parent, origin.at_seq)` resolves
        // exactly the parent-only portion, at any depth, without a second,
        // divergent copy of the D-11 ancestry walk in this crate.
        let inherited = match meta.origin {
            Some(ForkOrigin {
                parent,
                at_seq,
                mode: SubagentMode::Fork,
            }) => {
                let records = self
                    .resolver
                    .resolve_prefix(self.store.as_ref(), &parent, at_seq)
                    .await?;
                Some(InheritedPrefix {
                    from: parent,
                    seq_range: SeqRange::new(LogSeq::ZERO, Some(at_seq)),
                    records,
                })
            }
            _ => None,
        };

        let agent_def_ref = spec
            .agent_def
            .clone()
            .or_else(|| meta.agent_def.clone().map(AgentDefRef));
        let agent_def = agent_def_ref
            .as_ref()
            .and_then(|r| self.agent_defs.get(r.0.as_str()));

        let role = spec
            .role
            .clone()
            .or_else(|| meta.role.clone())
            .or_else(|| agent_def.and_then(|d| d.role.clone()))
            .unwrap_or_else(|| RoleAlias::new("default"));

        let system_prompt = agent_def.map(|d| crate::context::SystemPromptSpec {
            agent_def: d.name.clone(),
            text: d.system_prompt.clone(),
        });
        // Resolves `agent_def.skills` (names) against the discovered
        // registry into `SkillFragment`s -- see the module doc's "Skill
        // resolution" note and `resolve_skills`'s own doc.
        let skills = resolve_skills(agent_def, &self.skills)?;
        let tools = spec
            .tools
            .clone()
            .or_else(|| agent_def.map(|d| d.tools.clone()));
        let pin = agent_def.and_then(|d| d.model.clone());
        let cwd = spec.cwd.clone().unwrap_or_else(|| meta.cwd.clone());

        // (S3) `ResumeSpec` carries no `root` override field at all -- this
        // session's `root` is therefore always whatever `meta.root` already
        // says (this method never rewrites the header), which by
        // construction can neither widen nor null it: there is no code path
        // here that could turn a persisted `Some(root)` into `None`, or
        // replace it with something wider. What CAN change on resume is
        // `cwd` (`spec.cwd` may override the persisted `meta.cwd` above),
        // and an overridden `cwd` must still satisfy "cwd subset of root,
        // always" (the same invariant `subagent.rs`'s `SubagentHost::start`
        // enforces at spawn time) -- a resumed root confined to `meta.root`
        // whose caller-supplied `cwd` override has drifted outside it (e.g.
        // the root directory itself moved, or the caller passed the wrong
        // path) must fail loudly here rather than resume into an incoherent
        // state. `Containment::Undecidable` is treated identically to
        // `Outside` -- "can't check" is never "allow" (see
        // `conway_core::containment::Containment`'s own doc).
        if let Some(root_path) = meta.root.as_deref() {
            let canonical_root = CanonicalRoot::new(root_path).map_err(|err| {
                crate::subagent::invalid_spec(ConwayError::Config {
                    detail: format!(
                        "resumed session's root {} does not canonicalize: {err}",
                        root_path.display()
                    ),
                })
            })?;
            match canonical_root.contains(&cwd) {
                Containment::Inside => {}
                Containment::Outside | Containment::Undecidable => {
                    // Both operands on the same footing -- see the identical
                    // treatment in `subagent.rs`'s cwd-outside-root error for
                    // why a raw cwd beside a canonical root misleads.
                    let canonical_cwd = cwd.canonicalize().ok();
                    let shown = match &canonical_cwd {
                        Some(c) if c != &cwd => {
                            format!("{} (resolved: {})", cwd.display(), c.display())
                        }
                        _ => cwd.display().to_string(),
                    };
                    return Err(crate::subagent::invalid_spec(ConwayError::Config {
                        detail: format!(
                            "resumed cwd {} is outside the session's own root {}",
                            shown,
                            canonical_root.as_path().display()
                        ),
                    }));
                }
            }
        }

        // (S1.5 resume gap) Re-derive this agent's own EFFECTIVE per-agent
        // plugin config from its PERSISTED `meta.plugin_config` -- never
        // simply `self.loop_deps.plugin_config.clone()` (the global
        // default), which is exactly the fail-open `01KZDC0269171BZDB3HH00179B`
        // disclosed: every resumed agent silently reverting to the
        // unconfined global default, no error, no warning. See
        // `SessionMeta::plugin_config`'s own doc for the full contract this
        // implements.
        //
        // **Re-applies the SAME check `subagent.rs`'s `SubagentHost::start`
        // validated this value with originally** (`PluginConfig::narrow`),
        // never merely trusting the persisted record -- the widening hazard
        // this method's own item exists to close is a resume path that
        // reconstructs a confinement value by any route OTHER than the one
        // that validated it; calling the identical validating function again
        // here, rather than a bespoke or looser check, is what keeps this
        // resume path the SAME route, not a second one. The CURRENT
        // process-wide global config (`self.loop_deps.plugin_config`) is the
        // ceiling `narrow` validates the persisted value against, exactly as
        // it would be for a brand-new root -- a root's own effective config
        // IS the global default (`start_root`, immediately above this
        // method), so every fork/spawn descendant's effective value is, by
        // construction, always some narrowing of it; re-validating a
        // resumed value against that SAME ceiling (rather than trusting it
        // verbatim) is what catches the two cases `narrow` can still refuse:
        //
        // - a key the persisted record still carries a value for, but that
        //   no CURRENTLY installed plugin declares narrowable any more
        //   (`PluginConfigError::NotNarrowable` -- e.g. the plugin that
        //   declared it was uninstalled since this session was created) --
        //   refuses to resume outright, a typed `RuntimeError::InvalidSpec`.
        //   The disclosed, deliberate choice among three candidate outcomes
        //   (module doc references the item's own reasoning): silently
        //   dropping the narrowing is not defensible (a resumed agent would
        //   come back WIDER, unconfined for that key, with no signal at
        //   all -- the exact fail-open this whole item exists to close);
        //   silently keeping a value nothing enforces is its own trap (an
        //   operator reading the persisted header would see a root that
        //   looks confined while nothing checks it, since the plugin that
        //   would have enforced it -- `conway_tools::fs::beneath` -- is no
        //   longer even registered); refusing to resume is the only outcome
        //   that is never silently wrong in either direction.
        // - a key whose value would WIDEN the current global default's own
        //   value for that same key (`PluginConfigError::WouldWiden`) --
        //   refused the same way. Unreachable with today's `RuntimeDeps`
        //   (which carries no operator-configurable global `plugin_config`
        //   at all, so `self.loop_deps.plugin_config` is always the empty
        //   map in every build this crate ships, making every key here
        //   "absent from the ceiling" and thus unconditionally accepted --
        //   `PluginConfig::narrow`'s own "unbounded to bounded is always a
        //   narrowing" rule) but not dead code: this call site re-applies
        //   the identical validating function a future non-empty global
        //   default would need no further change here to be protected by.
        //   `conway_core::ports::plugin`'s own unit tests
        //   (`resuming_a_persisted_plugin_config_wider_than_the_current_
        //   global_default_is_refused` and neighbors) exercise this branch
        //   directly, with a synthetic non-empty ceiling, since this
        //   `Runtime`-level call site cannot manufacture one today.
        //
        // Never rewrites `meta.plugin_config` itself (this method never
        // rewrites the header, matching `root`'s own resume-time treatment
        // above) -- only the freshly computed, re-validated EFFECTIVE value
        // handed to the new `AgentLoop`/`AgentHandle` can differ from what
        // was persisted, and only by refusing the whole resume, never by
        // silently substituting a different value.
        let plugin_config = Arc::new(
            self.loop_deps
                .plugin_config
                .narrow(
                    Some(&meta.plugin_config),
                    self.loop_deps.registry.narrowing_rules(),
                )
                .map_err(|err| {
                    crate::subagent::invalid_spec(ConwayError::Config {
                        detail: format!(
                            "resumed session's persisted plugin_config could not be \
                             re-validated against the current plugin set: {err}"
                        ),
                    })
                })?,
        );

        let last_report = Arc::new(Mutex::new(None));
        let agent_spec = AgentSpec {
            system_prompt,
            skills,
            tools,
            role: role.clone(),
            pin,
            budget: spec.budget.clone(),
            // Pre-routing placeholder, same as `start_root` above -- see
            // that field's comment there for the full rationale.
            cache_mode: CacheMode::None,
            cache_ttl: CacheTtl::FiveMinutes,
            headroom_override: None,
            max_parallel_tools: DEFAULT_MAX_PARALLEL_TOOLS,
            report_slot: Some(last_report.clone()),
            // `ResumeSpec::result_contract` -- see that field's own doc
            // (board item `01M03FQDF33AZ8G258516EDWQD`) for the gap this
            // closes: `conway::Conway::resume` always passes `None` here
            // (it has no per-call spec to source one from), but
            // `conway::Conway::fork_from` -- the other `resume_root` caller
            // -- now threads its own `ForkSpec::result_contract` through
            // `crate::fork_child::fork_child`'s `ForkChildRequest`, so it no
            // longer silently drops a contract set on the facade fork path.
            result_contract: spec.result_contract,
            // `ResumeSpec::keep_alive` -- see that field's own doc (board
            // item `01M03KZXR1KF77YRAW4W4GE6KK`) for the gap this closes:
            // `conway::Conway::resume` always passes `false` here (it has no
            // per-call spec to source the flag from, preserving the one-shot
            // resume behaviour exactly), but `conway::Conway::fork_from` --
            // the other `resume_root` caller -- now threads its own
            // `ForkSpec::keep_alive` through `crate::fork_child::fork_child`'s
            // `ForkChildRequest`, so a `keep_alive(true)` fork_from child no
            // longer silently terminates on its first completed turn.
            keep_alive: spec.keep_alive,
            // A resumed root has no `SubagentSpec` to source a consumer tag
            // from either -- same as
            // `start_root`.
            tag: None,
        };

        let cancel = CancellationToken::new();
        let (mailbox_tx, mailbox_rx) = Mailbox::new(mailbox::RUNTIME_CAPACITY);
        let mailbox_tx =
            mailbox_tx.with_events(self.bus.clone(), spec.session, agent_id, cancel.clone());
        let agent_loop = AgentLoop {
            agent_id,
            session: spec.session,
            parent: None,
            agent_path: vec![agent_id],
            cwd: cwd.clone(),
            // (S3/S5) `meta.root` was already validated against `cwd` just
            // above (the `Containment` check this method's own doc
            // describes) -- passed straight through unchanged, exactly as
            // the module doc for `ResumeSpec` promises ("no code path here
            // could turn a persisted `Some(root)` into `None`, or replace
            // it with something wider").
            root: meta.root.clone(),
            // (S1.5 resume gap) The re-validated effective value computed
            // just above -- never `self.loop_deps.plugin_config.clone()`
            // (the global default) unconditionally, which is exactly the
            // silent revert this item closes. See that computation's own
            // comment for the full contract.
            plugin_config: plugin_config.clone(),
            deps: self.loop_deps.clone(),
            spec: agent_spec,
            cancel: cancel.clone(),
            // A genuine resumed root still inherits nothing (`None`, same
            // as `start_root`; the resolver rebuilds its full effective
            // transcript from its own records instead) -- a resumed fork
            // child gets its resolved parent prefix instead, computed just
            // above.
            inherited,
            inbox: mailbox_rx,
            // A root has no parent to deliver a terminal `Result` to.
            parent_mailbox: None,
            pending_cancel: None,
            // This loop's first iteration must
            // wait for the caller's next `Runtime::prompt` rather than
            // racing it against the persisted (already-completed)
            // transcript -- see `ResumeGate`'s and `run_inner`'s own docs.
            // `launch_agent` clones this same `notify` `Arc` into this
            // agent's `AgentHandle` before `agent_loop` moves into its
            // spawned task, which is what `Runtime::prompt` signals below.
            resume_gate: crate::agent_loop::ResumeGate {
                awaiting_prompt: true,
                notify: Arc::new(tokio::sync::Notify::new()),
            },
        };

        // A resumed root is re-started, not spawned (`kind: None`) -- see
        // `tree.rs`'s module doc on why that means `attach` will not emit
        // `Event::AgentSpawned` for it, matching `start_root`'s own root
        // node.
        let node = AgentNode {
            id: agent_id,
            parent: None,
            session: spec.session,
            kind: None,
            agent_def: agent_def
                .map(|d| d.name.clone())
                .or_else(|| meta.agent_def.clone()),
            role: Some(role),
            budget: spec.budget.clone(),
            cancel: cancel.clone(),
            inherited_upto: None,
            // Stamped from the persisted `SessionMeta::ephemeral`: a session
            // forked off ephemeral (e.g. a `/ask` child, born via
            // `SubagentHost::start` with `spec.ephemeral = true` -- board
            // item B2 moved the facade `/ask` onto that path) keeps the bit
            // in its header, so resuming it later re-attaches an ephemeral
            // node; a normal `conway::resume`/`fork_from` header has it
            // `false`. `attach` does not emit `Event::AgentSpawned` for this
            // node (a resumed root has `kind: None`), but `ephemeral_of`
            // reads this field for the `Event::AgentFinished` stamp at
            // finish time.
            ephemeral: meta.ephemeral,
        };

        self.launch_agent(node, agent_loop, last_report, mailbox_tx)?;

        Ok(agent_id)
    }
}
