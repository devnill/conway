//! `-p`/`--print` one-shot mode (WI-112): reads the prompt, builds the
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
//!    from inside `run` itself; `main.rs` (coordinated with WI-114, which
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
//!    directly is exactly what WI-111's `no_forbidden_deps` test forbids.
//!    (b) Even setting the dependency problem aside,
//!    `crates/conway/src/presets.rs::default_permissions_for_one_shot`
//!    (already committed, WI-098) documents the *opposite* default as the
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
//! 3. **`--model` has no wiring point.** Neither `SessionSpec` nor
//!    `conway_runtime::runtime::RootSpec` (the two structs `new_session`
//!    passes through) has a model-pin field of any kind -- confirmed by
//!    reading both. `cli.model` is accepted by the parser (WI-111) but is
//!    inert here; flagged for the facade to grow a pin field, not papered
//!    over with an invented mechanism.
//! 4. **`--session`/`--resume`/`--fork-from` (WI-117, driven live by
//!    WI-120).** WI-117 wired these three flags but could only validate
//!    them: `conway-runtime` had exactly one way to register a *live* root
//!    agent (`Runtime::start_root`, reached only through
//!    `Conway::new_session`), which always minted its own fresh `SessionId`
//!    and always `store.create`d a brand-new session -- there was no
//!    `resume_root`/`attach` counterpart. WI-118 added
//!    `Runtime::resume_root` (re-registers a persisted agent as live,
//!    gated behind a `ResumeGate` so it idles until the first `prompt`
//!    rather than racing a spurious turn) and WI-119 wired it through the
//!    facade: `SessionSpec` gained a caller-chosen `id` field,
//!    `Conway::resume` now returns a drivable handle, and
//!    `Conway::fork_from`'s child is registered live too (genuinely
//!    inheriting the parent's context via the shared `TranscriptResolver`
//!    walk). [`resolve_session`] now drives all three flags for real:
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
//!    [`resolve_session`], which is why the flag-free default arm and
//!    these three no longer need to diverge in [`run`]'s own driving loop
//!    -- the now-single-outcome-shape [`resolve_session`] dropped its
//!    former `SessionOutcome::Done` branch entirely (nothing produces it
//!    any more).

use std::io::{IsTerminal, Read};
use std::time::Duration;

use conway::gates::AllowListGate;
use conway::{
    AgentResult, Conway, ConwayError, Event, ForkSpec, ModelRef, RoleAlias, SessionHandle,
    SessionSpec,
};
use futures::StreamExt;
use std::str::FromStr;

use crate::cli::{Cli, PermissionMode};
use crate::exit::ExitCode;
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
                if let Event::AgentFinished { result } = &env.event {
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

/// Resolves `cli.session`/`cli.resume`/`cli.fork_from` (WI-117, driven live
/// by WI-120) -- at most one is ever `Some`, per `cli.rs`'s
/// `conflicts_with_all` -- into a live [`SessionHandle`] that [`run`] then
/// subscribes to and prompts uniformly, regardless of which arm produced
/// it. See this module's doc comment, reconciliation #4, for how each flag
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
                ..SessionSpec::default()
            };
            conway
                .new_session(spec)
                .await
                .map_err(|e| usage_error(format!("--session {sid}: {e}")))
        }

        // `--resume <id>`: reattach and hand the drivable handle straight
        // back to `run`, which subscribes and prompts it exactly like the
        // flag-free path above -- `Conway::resume` (WI-119) now returns a
        // handle whose `prompt` genuinely continues the persisted
        // transcript (`Runtime::resume_root`, WI-118), so there is nothing
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
        // fork_from` (WI-119) now registers the child live too (genuinely
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

/// Resolves the prompt text: `--print <text>` if non-empty, else stdin read
/// to EOF. A present-but-empty `--print` (the flag with no value) on a TTY
/// stdin is a usage error rather than blocking on interactive input a
/// one-shot script never intends to provide.
fn read_prompt(cli: &Cli) -> conway::Result<String> {
    if let Some(text) = &cli.print {
        if !text.is_empty() {
            return Ok(text.clone());
        }
    }
    if std::io::stdin().is_terminal() {
        return Err(usage_error(
            "no prompt provided: pass -p \"<prompt>\" or pipe text on stdin",
        ));
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Err(usage_error("no prompt provided: stdin was empty"));
    }
    Ok(buf)
}

fn usage_error(message: impl Into<String>) -> ConwayError {
    ConwayError::Config {
        path: None,
        message: message.into(),
    }
}

/// Parses `--model <ref>` (WI-128) into a [`ModelRef`] pin, or `None` when
/// the flag was not passed. A malformed ref is a usage error (`ExitCode::
/// Usage`, 2), consistent with every other flag this module parses in
/// [`resolve_session`].
fn parse_model_pin(cli: &Cli) -> conway::Result<Option<ModelRef>> {
    cli.model
        .as_deref()
        .map(|r| ModelRef::from_str(r).map_err(|e| usage_error(format!("--model {r}: {e}"))))
        .transpose()
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
            session: None,
            resume: None,
            fork_from: None,
            config: None,
            cwd: None,
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
