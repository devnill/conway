//! `-p`/`--print` one-shot mode: reads the prompt, builds the
//! session, streams the event stream through a `render::Renderer`, and maps
//! termination to an `ExitCode`.
//!
//! ## Reconciliations (disclosed)
//!
//! 1. **Gate wiring.** `Conway`/`SessionHandle`/`SessionSpec` expose no way
//!    to attach a `PermissionGate` to an already-built `Conway`, or to one
//!    session within it -- `conway_runtime::runtime::RuntimeDeps.gate` is
//!    set exactly once, at `Runtime::new`, inside `ConwayBuilder::build()`.
//!    So the gate this module builds ([`build_gate`]) cannot be attached
//!    from inside `run` itself; `main.rs` (coordinated with the TUI work, which
//!    has the identical need for its own TUI gate) calls [`build_gate`]
//!    *before* `Conway` is constructed and passes it through
//!    `ConwayBuilder::with_permission_gate` — see `main.rs`'s own doc
//!    comment on that reconciliation. By the time [`run`] receives its
//!    `conway: Conway` parameter, the gate this module builds is already
//!    live in that `Conway`'s `Runtime`; `run` itself does not construct or
//!    attach anything.
//! 2. **`build_gate`'s "no `--allowed-tools`" default.** The plan doc's
//!    prose reads "`--allowed-tools` if non-empty, else 'all tools'; then
//!    subtract `--deny-tools`." Two already-committed facts make that
//!    literal reading both unbuildable and, on reflection, the wrong
//!    default: (a) `conway::gates::AllowListGate` has no tool-name
//!    wildcard -- its `allowed` list is matched by exact tool-name equality
//!    -- so "all tools" cannot be expressed without either enumerating
//!    every registered tool name (not available here: tool registration
//!    lives in `conway-tools`, reached only through a live
//!    `Conway`/`Runtime`, not this pure function) or hand-writing a new
//!    `PermissionGate` impl, which is impossible without naming
//!    `conway_core::agent::{PermissionRequest, PermissionDecision}` --
//!    neither is re-exported by the facade, and depending on `conway-core`
//!    directly is exactly what the `no_forbidden_deps` test forbids.
//!    (b) Even setting the dependency problem aside,
//!    `crates/conway/src/presets.rs::default_permissions_for_one_shot`
//!    (already committed) documents the *opposite* default as the
//!    deliberately safe one: "allow-list mode with an empty allow list,
//!    i.e. every tool call is denied with feedback unless the embedder
//!    populates `allowed_tools` itself" -- specifically because one-shot
//!    mode cannot prompt, so a fail-open default would let an unattended
//!    script's tool calls through with nobody watching. [`build_gate`]
//!    follows that committed precedent: an empty `--allowed-tools` denies
//!    every tool (fail-closed), which is exactly what a bare
//!    `AllowListGate::new(vec![], deny_tools)` already does with zero
//!    custom code. Direct, disclosed consequence: the plan doc's own
//!    listed unit-test scenario ("with `--deny-tools bash` and no
//!    allow-list, ... `read` yields `AllowOnce`") is deliberately **not**
//!    implemented -- this module's own test for that scenario asserts
//!    `read` is denied instead, and says why.
//! 3. **`--session`/`--resume`/`--fork-from`.** An earlier item wired these
//!    three flags but could only validate
//!    them: `conway-runtime` had exactly one way to register a *live* root
//!    agent (`Runtime::start_root`, reached only through
//!    `Conway::new_session`), which always minted its own fresh `SessionId`
//!    and always `store.create`d a brand-new session -- there was no
//!    `resume_root`/`attach` counterpart. A later change added
//!    `Runtime::resume_root` (re-registers a persisted agent as live,
//!    gated behind a `ResumeGate` so it idles until the first `prompt`
//!    rather than racing a spurious turn) and wired it through the
//!    facade: `SessionSpec` gained a caller-chosen `id` field,
//!    `Conway::resume` now returns a drivable handle, and
//!    `Conway::fork_from`'s child is registered live too (genuinely
//!    inheriting the parent's context via the shared `TranscriptResolver`
//!    walk). `resolve_session` now drives all three flags for real:
//!    - `--session <new-id>`: `conway.new_session(SessionSpec { id: Some(sid), .. })`
//!      creates exactly that id (an existence probe via `conway.resume`
//!      first turns a collision into a usage error naming `--resume`
//!      instead, rather than surfacing `Conway::new_session`'s own
//!      `StoreError::AlreadyExists` as a less actionable message).
//!    - `--resume <id>`: `conway.resume(sid)` now returns a handle whose
//!      `prompt` genuinely continues the persisted transcript -- this arm
//!      returns it straight to [`run`], which subscribes and prompts
//!      exactly as the flag-free path does.
//!    - `--fork-from <ref>`: `conway.fork_from(parent, at, spec)` returns a
//!      drivable child handle; [`run`]'s own subsequent `handle.prompt(text)`
//!      is the child's first (and, per `ResumeGate`, only-until-prompted)
//!      turn -- `spec.directive` is left empty (`ForkSpec::new(String::new())`)
//!      since `fork_from`'s offline path never writes a `LogRecord::
//!      ForkDirective`; the real turn is `run`'s own `handle.prompt`. When
//!      `<ref>` names no `@seq`, the fork point is `conway.session_head
//!      (parent)` (`Conway::session_head`, added for this fix) -- the
//!      parent's own *local* record count, matching exactly what `Conway::
//!      fork_from`'s bounds check compares `at` against, so "fork this
//!      branch at its current head" is correct even when `parent` is itself
//!      a fork child (a naive `SessionHandle::transcript().len()` would
//!      overcount by the inherited-prefix size and fail with a confusing
//!      `SeqOutOfRange`). `--role-override` is wired through `spec.role`
//!      (honored by `fork_from`); `--cwd` has no `ForkSpec` field to carry
//!      it, so combining it with `--fork-from` is a usage error rather than
//!      a silent drop.
//!
//!    Every arm now returns a live [`SessionHandle`] straight from
//!    `resolve_session`, which is why the flag-free default arm and
//!    these three no longer need to diverge in [`run`]'s own driving loop
//!    -- the now-single-outcome-shape `resolve_session` dropped its
//!    former `SessionOutcome::Done` branch entirely (nothing produces it
//!    any more).
//! 4. **`--agent`/`--system-prompt`/`--append-system-prompt`/budget
//!    flags.** `SessionSpec` (`conway`) had `agent_def`/`budget` fields
//!    already but no facade-reachable way to inject a raw, literal system
//!    prompt independent of any named agent definition -- the mechanism
//!    the D4 finding's "stop every one-shot run from being the built-in
//!    coding agent" line names directly. `SessionSpec` gained a new
//!    `system_prompt_override: Option<String>` field, threaded through
//!    `Conway::new_session` into a new `RootSpec::system_prompt_override`
//!    (`conway-runtime`), which wins over any `agent_def`-derived text when
//!    `Some` -- see that field's own doc for the exact precedence. Wired
//!    into two of `resolve_session`'s three arms only (flag-free and
//!    `--session`): `--resume` has no facade parameter to carry an
//!    override through at all (`Conway::resume` takes only a `SessionId`),
//!    and `--fork-from`'s `ForkSpec` has no literal-text field (only
//!    `agent_def: Option<String>`, a NAMED def) -- both combinations are
//!    therefore usage errors rather than a silent drop, checked up front in
//!    `resolve_session` before either flag's value is ever read. `--agent`
//!    itself is more permissive: `ForkSpec::agent_def` already exists and
//!    is wired for real (a fork can select a different named persona,
//!    exactly as fresh sessions can); only `--resume` refuses `--agent`,
//!    for the same "no facade parameter" reason. Budget flags
//!    (`--max-turns`/`--max-tokens`/`--max-seconds`) are scoped
//!    identically to the system-prompt flags (flag-free and `--session`
//!    only, usage error with `--resume`/`--fork-from`) for the same
//!    reason: neither `Conway::resume` nor this module's `ForkSpec`
//!    construction accepts a caller override today. `--agent`'s own name is
//!    validated eagerly, via `conway::agents::load_agent_defs` (already
//!    `pub` on the facade) against the same directory
//!    `ConwayBuilder::build` itself resolved (`conway.config().agents.dir`,
//!    joined against `conway.config().cwd` the same way `conway::builder`'s
//!    private `resolve_path` does -- duplicated here in miniature since
//!    that helper is not exported) -- an unknown name is a usage error
//!    naming the directory searched, never a silent no-op.
//! 5. **Piped stdin composed with an explicit `--print <text>`.** Before this
//!    item, this module's private `read_prompt` helper read stdin only when
//!    `--print` carried no text (bare `-p`, or `-p` omitted its value) --
//!    with `--print "<text>"` given, it returned that text immediately and
//!    never touched stdin at all. That made `cat error.log | conway -p
//!    "what broke?"` -- this item's own name, and the D4 finding's own
//!    motivating example -- silently drop the piped log with no error and
//!    nothing in the response hinting it was ignored: the *combination* of
//!    an argv prompt and piped data was never a documented, tested
//!    precedence, just an accident of the early `return` above it.
//!    `read_prompt` now treats `--print`'s text as the DIRECTIVE and piped
//!    stdin as the DATA it operates on -- the same split Unix `grep
//!    PATTERN` already makes between its own argv pattern and the corpus it
//!    reads from stdin -- and joins them, directive first, when both are
//!    present (see that private function's own doc, in this same file, for
//!    the exact join and every other arm).
//!    **Disclosed consequence:** stdin is now read to EOF whenever it is
//!    not a terminal, *even when `--print` already has text* -- a caller
//!    that runs `conway -p "<text>"` with stdin inherited from a pipe that
//!    never closes and never terminates will now block on that read where
//!    it previously would not have. This is the same trade-off this module
//!    already made, unconditionally, for the text-absent case since WI-112
//!    (139fe4a) -- standard Unix filter behavior (`grep PATTERN` blocks the
//!    same way against a non-terminating stdin), not a new hazard category
//!    -- and there is no way to distinguish "an operator forgot to close an
//!    inherited pipe" from "an operator is deliberately piping a second
//!    input" other than reading it. A caller that wants `--print`'s text
//!    alone, with whatever stdin it inherited left untouched, should
//!    redirect stdin from `/dev/null`.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::time::Duration;

use conway::gates::AllowListGate;
use conway::{
    AgentDef, AgentResult, Budget, Conway, Event, ForkSpec, RoleAlias, SessionHandle, SessionSpec,
};
use futures::StreamExt;
use schemars::schema::RootSchema;

use crate::cli::{Cli, PermissionMode};
use crate::exit::ExitCode;
use crate::model_pin::{parse_model_pin, usage_error};
use crate::{diag, render, session_ref, signal};

/// One-shot mode's entry point (dispatched from `main.rs` when
/// `cli.print.is_some()`). `conway`'s `Runtime` already has this module's
/// [`build_gate`] wired in as its `PermissionGate` -- see reconciliation #1
/// above -- `run` itself never touches gate construction.
pub async fn run(cli: &Cli, conway: Conway) -> conway::Result<ExitCode> {
    let text = read_prompt(cli)?;

    let handle = resolve_session(cli, &conway).await?;

    // Subscribe before prompting: an `events()` subscription taken out
    // after `prompt()` has already appended could miss the turn's own
    // first envelopes.
    let mut events = handle.events();
    let _turn = handle.prompt(text).await?;

    let sigint = signal::install();
    let mut renderer = render::make(
        cli.output_format,
        Box::new(std::io::BufWriter::new(std::io::stdout())),
    );

    let root = handle.root();
    // The renderer needs the root's id so it treats only the root's own
    // `AgentFinished` as terminal: a subagent's lifecycle events now reach
    // this session-scoped stream too (they bypass the stream filter).
    renderer.set_root(root);
    let mut final_result: Option<AgentResult> = None;
    let mut grace_deadline: Option<tokio::time::Instant> = None;

    loop {
        let grace = async {
            match grace_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            maybe_env = events.next() => {
                let Some(env) = maybe_env else { break };
                if let Event::Lagged { skipped } = &env.event {
                    // "never stdout": intercepted here, before any
                    // renderer (of any --output-format) ever sees it.
                    diag::warn(format!("event stream lagged: {skipped} event(s) dropped"));
                    continue;
                }
                renderer.on_event(&env)?;
                if let Event::AgentFinished { result, .. } = &env.event {
                    if env.agent == root {
                        final_result = Some(result.clone());
                        break;
                    }
                }
            }
            _ = sigint.notified(), if grace_deadline.is_none() => {
                let _ = handle.cancel(root, "sigint").await;
                grace_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
            }
            _ = grace, if grace_deadline.is_some() => {
                diag::warn("no terminal result within the SIGINT grace window; exiting");
                break;
            }
        }
    }

    renderer.finish(final_result.as_ref())?;

    let sigint_seen = sigint.hits() > 0;
    let code = match &final_result {
        Some(result) => ExitCode::from_result_with_sigint(result, sigint_seen),
        None if sigint_seen => ExitCode::Interrupted,
        // The event stream ended without ever producing a terminal result
        // and without a SIGINT -- not expected in practice (the broadcast
        // bus only closes once every `Arc<Runtime>` is dropped), but this
        // fn must still return *something* rather than hang or panic.
        None => ExitCode::AgentFailed,
    };
    Ok(code)
}

/// Resolves `cli.session`/`cli.resume`/`cli.fork_from` (driven live
/// here) -- at most one is ever `Some`, per `cli.rs`'s
/// `conflicts_with_all` -- into a live [`SessionHandle`] that [`run`] then
/// subscribes to and prompts uniformly, regardless of which arm produced
/// it. See this module's doc comment, reconciliation #3, for how each flag
/// reaches a drivable handle; each arm's own comment below covers only what
/// is specific to that flag.
///
/// Every error this function returns is deliberately built with
/// [`usage_error`] (`ExitCode::Usage`, 2) rather than left to propagate as
/// whatever `ConwayError` variant the facade call itself produced (which
/// would classify as `ExitCode::AgentFailed`, 1, via `exit::classify_runtime_or_routing`'s
/// default arm) -- matching this item's own binding notes: "every failure
/// mode here is a usage error, not an agent failure ... the agent never
/// starts, so no agent status exists to report." That holds for every arm
/// below (unknown session, malformed ref, seq beyond head, duplicate id,
/// conflicting flags): in every arm that returns `Err`, no agent ever
/// started.
async fn resolve_session(cli: &Cli, conway: &Conway) -> conway::Result<SessionHandle> {
    // Up-front usage-error guards for the two flag groups this module's
    // doc comment, reconciliation #4, says are not wired for `--resume`/
    // `--fork-from` -- checked before either flag's value (or `--agent`'s
    // own validity) is ever read, so an operator combining them gets a
    // named reason, never a silent drop.
    let system_prompt_requested = cli.system_prompt.is_some() || cli.append_system_prompt.is_some();
    let budget_requested =
        cli.max_turns.is_some() || cli.max_tokens.is_some() || cli.max_seconds.is_some();
    let output_schema_requested = cli.output_schema.is_some();
    let continuing = cli.resume.is_some() || cli.fork_from.is_some();
    if system_prompt_requested && continuing {
        return Err(usage_error(
            "--system-prompt/--append-system-prompt are not supported with --resume or \
             --fork-from: a continued session's system prompt is fixed by the session it \
             continues, not by this invocation",
        ));
    }
    if budget_requested && continuing {
        return Err(usage_error(
            "--max-turns/--max-tokens/--max-seconds are not supported with --resume or \
             --fork-from in this release: neither facade path accepts a caller-supplied \
             budget override yet",
        ));
    }
    // `ForkSpec::result_contract` exists on the facade type (mirroring
    // `SubagentSpec::result_contract`, for a model-triggered `conway_fork`)
    // but `Conway::fork_from` -- what `--fork-from` drives -- never reads it
    // (its `fork_child` helper goes through `conway_runtime::runtime::
    // ResumeSpec`, which has no `result_contract` field at all); pre-existing,
    // not introduced by this item, and flagged rather than silently worked
    // around. `--resume` has no facade parameter to carry a contract through
    // either (same shape as `--system-prompt`/the budget flags above). Both
    // arms are therefore usage errors, not a silent drop.
    if output_schema_requested && continuing {
        return Err(usage_error(
            "--output-schema is not supported with --resume or --fork-from in this release: \
             neither facade path accepts a caller-supplied result-contract override yet",
        ));
    }
    if cli.agent.is_some() && cli.resume.is_some() {
        return Err(usage_error(
            "--agent is not supported with --resume: a resumed session's agent definition is \
             fixed by the session it continues",
        ));
    }

    // Loaded (and, for `--agent`, validated) once, up front, so every arm
    // below sees the identical, already-checked value -- an unknown
    // `--agent` name is caught here regardless of which arm would
    // otherwise have run.
    let agent_def = load_agent_def(cli, conway)?;
    let output_schema = load_output_schema(cli)?;
    let system_prompt_override =
        resolve_system_prompt_override(cli, agent_def.as_ref(), output_schema.as_ref());
    let result_contract = resolve_result_contract(output_schema, agent_def.as_ref());
    let budget = resolve_budget(cli, conway);

    match (&cli.session, &cli.resume, &cli.fork_from) {
        (None, None, None) => {
            let role = cli
                .role_override
                .as_ref()
                .map(|r| RoleAlias::new(r.clone()));
            let model = parse_model_pin(cli)?;
            let spec = SessionSpec {
                role,
                cwd: cli.cwd.clone(),
                model,
                agent_def: cli.agent.clone(),
                system_prompt_override,
                budget,
                result_contract,
                ..SessionSpec::default()
            };
            conway.new_session(spec).await
        }

        // `--session <id>`: "use (creating if new) a specific session id."
        // A `Conway::resume` existence probe runs first so a collision
        // surfaces as an actionable "use --resume instead" usage error
        // rather than `Conway::new_session`'s own generic
        // `StoreError::AlreadyExists` -- a real race between this probe
        // and the `new_session` call below (another process creating the
        // same id in between) is not a concern this single-shot CLI
        // invocation needs to close: `new_session` would then fail with
        // that same typed store error, still surfaced as a usage error via
        // the `map_err` below, just with a less specific message.
        (Some(id), None, None) => {
            let sid = session_ref::parse_sid(id).map_err(|e| usage_error(e.to_string()))?;
            if conway.resume(sid).await.is_ok() {
                return Err(usage_error(format!(
                    "--session {sid}: session already exists; pass --resume {sid} to continue \
                     it instead"
                )));
            }
            let role = cli
                .role_override
                .as_ref()
                .map(|r| RoleAlias::new(r.clone()));
            let model = parse_model_pin(cli)?;
            let spec = SessionSpec {
                id: Some(sid),
                role,
                cwd: cli.cwd.clone(),
                model,
                agent_def: cli.agent.clone(),
                system_prompt_override,
                budget,
                result_contract,
                ..SessionSpec::default()
            };
            conway
                .new_session(spec)
                .await
                .map_err(|e| usage_error(format!("--session {sid}: {e}")))
        }

        // `--resume <id>`: reattach and hand the drivable handle straight
        // back to `run`, which subscribes and prompts it exactly like the
        // flag-free path above -- `Conway::resume` now returns a
        // handle whose `prompt` genuinely continues the persisted
        // transcript (`Runtime::resume_root`), so there is nothing
        // arm-specific left to do here beyond the existence check.
        (None, Some(id), None) => {
            let sid = session_ref::parse_sid(id).map_err(|e| usage_error(e.to_string()))?;
            conway
                .resume(sid)
                .await
                .map_err(|e| usage_error(format!("--resume {sid}: {e}")))
        }

        // `--fork-from <ref>`: branch a new session from `<sid>[@seq]` and
        // hand the drivable child straight back to `run` -- `Conway::
        // fork_from` now registers the child live too (genuinely
        // inheriting the parent's context up to `at`), so `run`'s own
        // subsequent `handle.prompt(text)` is the child's first turn.
        //
        // `--cwd` has no `ForkSpec` field to carry it through, and
        // `Conway::fork_from` hardcodes the child's `ResumeSpec.cwd` to
        // `None` (falling back to the parent's own `cwd`) -- plumbing it
        // through would mean growing `ForkSpec`/`fork_from`'s signature,
        // out of this item's minimal-blast-radius scope. Rather than
        // silently drop a `--cwd` the user actually typed, combining it
        // with `--fork-from` is a usage error naming both flags.
        (None, None, Some(r)) => {
            if cli.cwd.is_some() {
                return Err(usage_error(
                    "--cwd is not supported with --fork-from: the forked child always inherits \
                     the parent session's cwd",
                ));
            }
            let (parent, seq) =
                session_ref::parse_fork_ref(r).map_err(|e| usage_error(e.to_string()))?;
            let at = match seq {
                Some(seq) => seq,
                None => {
                    // "Fork this branch at its current head": the head must
                    // be the parent's own LOCAL record count
                    // (`SessionStore::head`, what `Conway::fork_from`'s own
                    // bounds check compares `at` against), not
                    // `SessionHandle::transcript().len()` -- that reads the
                    // effective, ancestry-resolved transcript (inherited
                    // prefix + the session's own records), which overcounts
                    // the local head whenever `parent` is itself a fork
                    // child, sending `at > head` into `fork_from` and
                    // failing with a confusing `SeqOutOfRange` naming a seq
                    // the user never typed. `Conway::session_head` (added
                    // for this fix) reads `SessionStore::head` directly, so
                    // this is correct for a fork-of-a-fork too.
                    conway
                        .session_head(parent)
                        .await
                        .map_err(|e| usage_error(format!("--fork-from {parent}: {e}")))?
                }
            };
            let role = cli
                .role_override
                .as_ref()
                .map(|r| RoleAlias::new(r.clone()));
            let mut spec = ForkSpec::new(String::new());
            spec.role = role;
            // `--agent` genuinely wires here (unlike `--system-prompt`/
            // budget -- see this module's doc comment, reconciliation #4):
            // `ForkSpec::agent_def` already exists and, per its own doc,
            // overrides the forked child's system prompt/tools/model pin
            // with the named def's, exactly the same "select a persona"
            // capability `--agent` gives a fresh session.
            spec.agent_def = cli.agent.clone();
            conway
                .fork_from(parent, at, spec)
                .await
                .map_err(|e| usage_error(format!("--fork-from {parent}@{}: {e}", at.0)))
        }

        _ => Err(usage_error(
            "--session, --resume, and --fork-from are mutually exclusive",
        )),
    }
}

/// The directory `--agent` searches, mirroring `conway::builder`'s own
/// private `resolve_path(&cwd, &config.agents.dir)` -- not itself exported
/// (`ConwayBuilder::build` resolves it internally, once, at build time),
/// so this is a small, deliberate duplicate rather than a new dependency on
/// a private helper. `conway.config()` is the already-loaded, already-
/// merged config this `Conway` was built from (defaults < XDG < project <
/// env < `--config`/CLI), so this reads the SAME effective `[agents].dir`
/// `ConwayBuilder::build` itself used to populate the `agent_defs` registry
/// `--agent` ultimately resolves against.
fn resolve_agents_dir(conway: &Conway) -> PathBuf {
    let config = conway.config();
    if config.agents.dir.is_absolute() {
        config.agents.dir.clone()
    } else {
        config.cwd.join(&config.agents.dir)
    }
}

/// Loads and validates `cli.agent` (`None` when the flag was not given, in
/// which case this is a no-op returning `Ok(None)`): an unknown name is a
/// usage error naming the directory searched, not a silent no-op. Returns
/// the loaded [`AgentDef`] itself (not just a validity bool) so callers
/// (namely [`resolve_system_prompt_override`]) can read its own
/// `system_prompt` as the base text for `--append-system-prompt`, without a
/// second directory scan.
fn load_agent_def(cli: &Cli, conway: &Conway) -> conway::Result<Option<AgentDef>> {
    let Some(name) = &cli.agent else {
        return Ok(None);
    };
    let dir = resolve_agents_dir(conway);
    let mut defs = conway::agents::load_agent_defs(&dir)?;
    match defs.remove(name.as_str()) {
        Some(def) => Ok(Some(def)),
        None => Err(usage_error(format!(
            "--agent {name}: no agent definition named `{name}` found in {} (looked for \
             `{name}.md`)",
            dir.display()
        ))),
    }
}

/// Combines `--system-prompt`/`--append-system-prompt`/`--output-schema`
/// into the single string [`RootSpec::system_prompt_override`]
/// (`conway-runtime`) takes, or `None` when none of the three was given
/// (preserving the pre-existing, `agent_def`-alone behavior exactly).
/// `agent_def` is `--agent`'s own already-loaded, already-validated def
/// (from [`load_agent_def`]) -- its `system_prompt` is the base
/// `--append-system-prompt` appends to when `--system-prompt` itself is
/// absent. `output_schema` is `--output-schema`'s own already-compiled
/// schema (from [`load_output_schema`]).
///
/// **Precedence:** `--output-schema`'s own instruction text (naming the
/// schema and directing the model to the `report` tool -- see
/// [`schema_instruction`]) is always appended LAST, after whatever
/// `--system-prompt`/`--append-system-prompt`/`--agent` already produced --
/// it is the outermost, always-final constraint, mirroring how
/// [`resolve_result_contract`]'s validation always wins over an agent
/// def's own declared contract regardless of which text is in effect. This
/// is deliberately NOT the enforcement mechanism itself (that is
/// `RootSpec::result_contract`, wired separately in `resolve_session`) --
/// it exists only because, unlike a `conway_fork`/`conway_spawn` result
/// contract (whose spawning AGENT is expected to write a prompt telling the
/// child what shape to produce), a one-shot root's `--print` text is the
/// operator's own task description, not persona-authoring text -- without
/// this, an operator would have to know to describe the schema themselves
/// or the run would spend its one corrective retry doing nothing but
/// re-stating what already failed.
fn resolve_system_prompt_override(
    cli: &Cli,
    agent_def: Option<&AgentDef>,
    output_schema: Option<&RootSchema>,
) -> Option<String> {
    if cli.system_prompt.is_none() && cli.append_system_prompt.is_none() && output_schema.is_none()
    {
        return None;
    }
    let base = cli
        .system_prompt
        .clone()
        .or_else(|| agent_def.map(|d| d.system_prompt.clone()));
    let mut text = match (base, &cli.append_system_prompt) {
        (Some(b), Some(a)) => format!("{b}\n\n{a}"),
        (Some(b), None) => b,
        (None, Some(a)) => a.clone(),
        // Reachable now that `output_schema` alone can trigger this
        // function (unlike the pre-`--output-schema` version, where the
        // guard above guaranteed at least one of the two text flags was
        // `Some`): no base text and no `--append-system-prompt`, but
        // `--output-schema` is `Some` -- `schema_instruction` below becomes
        // the entire system prompt by itself.
        (None, None) => String::new(),
    };
    if let Some(schema) = output_schema {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&schema_instruction(schema));
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// The instruction text [`resolve_system_prompt_override`] appends when
/// `--output-schema` is given: tells the model to conclude via the `report`
/// tool's `structured` argument, matching `schema` exactly, and states the
/// retry-then-terminal consequence of not doing so -- so a model that reads
/// its own system prompt has a real chance to comply on the first attempt,
/// rather than relying solely on the one corrective `SystemNote` the
/// `result_contract` enforcement mechanism sends after a first failure (see
/// `RootSpec::result_contract`'s own doc, `conway-runtime`, for that
/// mechanism). `schema` is rendered as pretty-printed JSON via `serde_json`
/// (already a dependency; no new templating dependency reached for).
fn schema_instruction(schema: &RootSchema) -> String {
    let pretty = serde_json::to_string_pretty(schema)
        .unwrap_or_else(|_| "<schema serialization failed>".to_string());
    format!(
        "Before finishing, call the `report` tool with its `structured` argument set to a JSON \
         value that satisfies the following JSON Schema exactly -- no prose, no code fence, no \
         other tool call may substitute for it. If your first attempt does not satisfy the \
         schema, you will be told exactly what failed and get exactly one more turn to correct \
         it; a second failure ends the run without a usable result.\n\n{pretty}"
    )
}

/// Resolves `--output-schema`'s eventual [`RootSpec::result_contract`]
/// value: `output_schema` (already loaded/compiled by
/// [`load_output_schema`]) when `Some`, else the resolved `--agent` def's
/// own `AgentDef::result_contract` when it declares one, else `None`.
///
/// **Precedence: the call site always wins.** `--output-schema`'s schema
/// is never merged with, and never loses to, an agent def's own declared
/// contract -- mirroring `conway-runtime`'s `subagent.rs`, which documents
/// the identical rule for a forked/spawned child's contract ("the explicit
/// call-site contract ... wins ... over ... the AgentDef's own contract").
/// This is the same precedence [`resolve_system_prompt_override`]'s own doc
/// describes for the INSTRUCTION text; this function is the ENFORCEMENT
/// half of that same combination.
fn resolve_result_contract(
    output_schema: Option<RootSchema>,
    agent_def: Option<&AgentDef>,
) -> Option<RootSchema> {
    output_schema.or_else(|| agent_def.and_then(|d| d.result_contract.clone()))
}

/// Loads and compiles `cli.output_schema` (`None` when the flag was not
/// given, in which case this is a no-op returning `Ok(None)`): the named
/// file must exist, parse as JSON, and compile as a JSON Schema document
/// (via [`conway::compile_output_schema`]) -- any failure is a usage error
/// naming the path and the underlying cause, never a silent no-op and never
/// a run that starts with an unenforceable "schema".
fn load_output_schema(cli: &Cli) -> conway::Result<Option<RootSchema>> {
    let Some(path) = &cli.output_schema else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(path).map_err(|e| {
        usage_error(format!(
            "--output-schema {}: could not read file: {e}",
            path.display()
        ))
    })?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        usage_error(format!(
            "--output-schema {}: not valid JSON: {e}",
            path.display()
        ))
    })?;
    let schema = conway::compile_output_schema(value)
        .map_err(|e| usage_error(format!("--output-schema {}: {e}", path.display())))?;
    Ok(Some(schema))
}

/// Builds a [`Budget`] from `--max-turns`/`--max-tokens`/`--max-seconds`,
/// or `None` when none of the three was given -- preserving the
/// pre-existing behavior exactly (`SessionSpec::budget: None` falls back to
/// `Conway::new_session`'s own `self.default_budget()`, config-sourced).
/// When at least one flag IS given, this starts from that SAME config
/// baseline (`conway.config().limits`, replicating `Conway::
/// default_budget`'s own `0`-means-unset mapping, not exported either) and
/// overrides only the specific dimension(s) named -- so `--max-turns 5`
/// alone still respects a configured `[limits].max_tokens`/
/// `deadline_secs`, rather than silently clearing them.
fn resolve_budget(cli: &Cli, conway: &Conway) -> Option<Budget> {
    if cli.max_turns.is_none() && cli.max_tokens.is_none() && cli.max_seconds.is_none() {
        return None;
    }
    let limits = &conway.config().limits;
    let mut budget = Budget {
        max_steps: limits.max_steps,
        deadline: if limits.deadline_secs == 0 {
            None
        } else {
            Some(chrono::Utc::now() + chrono::Duration::seconds(limits.deadline_secs as i64))
        },
        max_tokens: if limits.max_tokens == 0 {
            None
        } else {
            Some(limits.max_tokens)
        },
        max_tool_calls: if limits.max_tool_calls == 0 {
            None
        } else {
            Some(limits.max_tool_calls)
        },
    };
    if let Some(turns) = cli.max_turns {
        budget.max_steps = turns;
    }
    if let Some(tokens) = cli.max_tokens {
        budget.max_tokens = Some(tokens);
    }
    if let Some(secs) = cli.max_seconds {
        budget.deadline = Some(chrono::Utc::now() + chrono::Duration::seconds(secs as i64));
    }
    Some(budget)
}

/// Resolves the prompt text from `--print <text>` (the DIRECTIVE) and piped
/// stdin (the DATA it operates on) -- see this module's doc comment,
/// reconciliation #5, for the precedence this implements and why. In short:
///
/// - **Both present** (`-p "<text>"` with stdin piped and non-empty): joined
///   directive-first, `"{text}\n\n{piped}"` -- the same `"\n\n"` join
///   [`resolve_system_prompt_override`] already uses for `--system-prompt`/
///   `--append-system-prompt`. This is what makes `cat error.log | conway -p
///   "what broke?"` (this item's own motivating example) actually work: the
///   model sees both the question and the log, not one silently dropping
///   the other.
/// - **Only `--print` has text**: that text alone, byte-for-byte -- this
///   module's pre-item behavior, unchanged. Stdin is still probed (see
///   reconciliation #5 for why that alone is a behavior change worth
///   flagging), but an empty/absent piped stdin leaves this arm identical
///   to before.
/// - **Only stdin is piped** (bare `-p`, or `-p` given no value): the piped
///   text alone -- this module's pre-item behavior for that case, unchanged.
/// - **Neither**: a usage error. `--print` present but empty (the flag with
///   no value) on a TTY stdin is "pass -p or pipe text on stdin" (nothing
///   was ever going to arrive, so this must not block on interactive input a
///   one-shot script never intends to provide); with a non-TTY stdin that
///   simply produced no non-whitespace bytes, it is "stdin was empty"
///   instead -- distinct messages, both usage errors either way.
///
/// Piped stdin is read to EOF whenever stdin is not a terminal
/// ([`IsTerminal::is_terminal`]), regardless of whether `--print` already
/// carries text -- see reconciliation #5 for this file's own doc comment
/// for the full disclosure of what changes as a result.
fn read_prompt(cli: &Cli) -> conway::Result<String> {
    let directive = cli.print.as_ref().filter(|t| !t.is_empty()).cloned();

    let is_tty = std::io::stdin().is_terminal();
    let piped = if is_tty {
        None
    } else {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        if buf.trim().is_empty() {
            None
        } else {
            Some(buf)
        }
    };

    match (directive, piped) {
        (Some(directive), Some(piped)) => Ok(format!("{directive}\n\n{piped}")),
        (Some(directive), None) => Ok(directive),
        (None, Some(piped)) => Ok(piped),
        (None, None) if is_tty => Err(usage_error(
            "no prompt provided: pass -p \"<prompt>\" or pipe text on stdin",
        )),
        (None, None) => Err(usage_error("no prompt provided: stdin was empty")),
    }
}

/// Builds the one-shot gate from `--permission-mode`/`--allowed-tools`/
/// `--deny-tools`. See this module's doc comment, reconciliation #2, for
/// why an empty `--allowed-tools` denies every tool rather than allowing
/// everything.
pub fn build_gate(cli: &Cli) -> AllowListGate {
    match cli.permission_mode {
        PermissionMode::Deny => AllowListGate::new(Vec::new(), Vec::new()),
        PermissionMode::Allowlist => {
            AllowListGate::new(cli.allowed_tools.clone(), cli.deny_tools.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use conway::PermissionGate;
    use conway_core::agent::{PermissionDecision, PermissionRequest};
    use conway_core::content::ToolCategory;
    use conway_core::ids::{AgentId, ToolName};

    use super::*;
    use crate::cli::OutputFormat;

    fn request(tool: &str) -> PermissionRequest {
        PermissionRequest {
            agent_id: AgentId::new(),
            agent_path: Vec::new(),
            tool: ToolName::new(tool),
            category: ToolCategory::Execute,
            arguments: serde_json::json!({}),
            rendered: format!("{tool}()"),
            call_id: "tc_1".into(),
            render_kind: conway::RenderKind::ShellCommand,
        }
    }

    fn cli_with(mode: PermissionMode, allowed: Vec<String>, denied: Vec<String>) -> Cli {
        Cli {
            print: Some("hi".into()),
            output_format: OutputFormat::Text,
            allowed_tools: allowed,
            deny_tools: denied,
            permission_mode: mode,
            role_override: None,
            model: None,
            agent: None,
            system_prompt: None,
            append_system_prompt: None,
            max_turns: None,
            max_tokens: None,
            max_seconds: None,
            output_schema: None,
            session: None,
            resume: None,
            fork_from: None,
            config: None,
            cwd: None,
            root: None,
            verbose: 0,
            command: None,
        }
    }

    #[tokio::test]
    async fn allowlist_explicit_denies_unlisted_allows_listed() {
        let cli = cli_with(
            PermissionMode::Allowlist,
            vec!["read".into(), "glob".into()],
            Vec::new(),
        );
        let gate = build_gate(&cli);
        assert!(matches!(
            gate.check(request("bash")).await,
            PermissionDecision::DenyWithFeedback { .. }
        ));
        assert!(matches!(
            gate.check(request("read")).await,
            PermissionDecision::AllowOnce
        ));
    }

    #[tokio::test]
    async fn no_allow_list_denies_everything_fail_closed() {
        // Deliberate deviation from the plan doc's "no allow-list => allow
        // all" reading -- see this module's doc comment, reconciliation #2.
        let cli = cli_with(PermissionMode::Allowlist, Vec::new(), vec!["bash".into()]);
        let gate = build_gate(&cli);
        assert!(matches!(
            gate.check(request("bash")).await,
            PermissionDecision::DenyWithFeedback { .. }
        ));
        assert!(
            matches!(
                gate.check(request("read")).await,
                PermissionDecision::DenyWithFeedback { .. }
            ),
            "fail-closed: with no explicit --allowed-tools, every tool -- including one not \
             named by --deny-tools -- is denied, matching presets::default_permissions_for_one_shot's \
             committed safe-default rationale"
        );
    }

    #[tokio::test]
    async fn deny_mode_denies_every_tool_with_feedback_never_hard_deny() {
        let cli = cli_with(PermissionMode::Deny, Vec::new(), Vec::new());
        let gate = build_gate(&cli);
        for tool in ["bash", "read", "anything"] {
            assert!(matches!(
                gate.check(request(tool)).await,
                PermissionDecision::DenyWithFeedback { .. }
            ));
        }
    }

    #[test]
    fn conflicting_continuity_flags_are_a_usage_error() {
        // `Cli::parse()` (`main.rs`'s only real caller) already implements
        // "every parse error prints to stderr and exits 2" via clap's own
        // `Error::exit()` -- this test only needs to confirm clap's
        // `conflicts_with_all` (`cli.rs`, frozen) is actually wired for
        // every pair, which is this item's own criterion ("clap
        // `conflicts_with_all` is acceptable as the mechanism").
        use clap::Parser;

        let pairs: &[[&str; 2]] = &[
            ["--session", "--resume"],
            ["--session", "--fork-from"],
            ["--resume", "--fork-from"],
        ];
        for [a, b] in pairs {
            let err = Cli::try_parse_from(["conway", "-p", "hi", a, "x", b, "y"])
                .expect_err(&format!("{a} and {b} must conflict"));
            let rendered = err.to_string();
            assert!(
                rendered.contains(&a.trim_start_matches('-').replace('-', "_"))
                    || rendered.contains(a),
                "error should name the conflicting flag {a}: {rendered}"
            );
            assert!(
                rendered.contains(&b.trim_start_matches('-').replace('-', "_"))
                    || rendered.contains(b),
                "error should name the conflicting flag {b}: {rendered}"
            );
        }
    }

    #[test]
    fn source_never_references_prompting_gate() {
        // Only the production code above `#[cfg(test)]` is in scope for
        // this check -- this test module's own name/assertion text below
        // necessarily mentions the forbidden identifier to describe what
        // it's checking for, which would otherwise make this assert
        // trivially fail against its own source.
        let source = include_str!("oneshot.rs");
        let production_code = source
            .split_once("#[cfg(test)]")
            .expect("oneshot.rs has a #[cfg(test)] module")
            .0;
        let needle = concat!("Prompting", "Gate");
        assert!(
            !production_code.contains(needle),
            "one-shot mode must never construct a {needle} -- it has no interactive channel to \
             prompt through"
        );
    }
}
