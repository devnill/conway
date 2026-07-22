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
//! 4. **`--session`/`--resume`/`--fork-from` (WI-117).** All three are now
//!    wired in [`resolve_session`], and all three run straight into the
//!    *same* carried gap, from three different angles: `conway-runtime`
//!    (WI-082) exposes exactly one way to register a *live* root agent --
//!    `Runtime::start_root`, reached only through `Conway::new_session` --
//!    and that path always mints its own fresh `SessionId` and always
//!    `store.create`s a brand-new session. There is no `resume_root`/
//!    `attach` counterpart (confirmed by grep; already flagged once by
//!    WI-103's own `Conway::resume` doc, in its "`prompt()` after resume"
//!    paragraph). Concretely:
//!    - `--session <new-id>`: no facade method accepts a caller-chosen
//!      `SessionId` at all -- `SessionSpec` (WI-101) has no `id` field, and
//!      `Conway::new_session` hardcodes `SessionId::new()`. There is
//!      nothing this module can create the caller's id *as*.
//!    - `--resume <id>`: `Conway::resume` reattaches a read-only handle;
//!      `SessionHandle::prompt` looks the root agent up in `Runtime`'s
//!      in-memory `agents` map, which `resume` never populates, so it
//!      always returns `RuntimeError::AgentNotFound` for a resumed handle
//!      -- not conditionally, always.
//!    - `--fork-from <ref>`: the plan doc's own pseudocode drives this
//!      through a *live* fork (`SessionHandle::fork` on a resumed parent),
//!      which hits the identical wall one layer earlier --
//!      `SubagentHost::start` resolves the parent via
//!      `Runtime::agent_session`, the same in-memory map `resume` never
//!      populates, so a live fork off a resumed parent fails before it
//!      even reaches `SessionStore::fork`. `Conway::fork_from` (store-only,
//!      WI-103) sidesteps that -- it never needs a live parent -- and *is*
//!      what [`resolve_session`] calls, so the child session is genuinely
//!      created and independently observable via `Conway::sessions`. But
//!      `Conway::fork_from` also never registers the *child* as live
//!      either (same root cause), and it does not even persist
//!      `ForkSpec::directive` anywhere (`conway`'s own doc on that method:
//!      "only `agent_def` and `role` are consulted") -- so the `-p` prompt
//!      text has nowhere to go.
//!
//!    None of this is worked around here by reconstructing agent state in
//!    the CLI -- out of this item's file scope, and precisely what WI-103's
//!    own carried gap says not to do. [`resolve_session`]'s own doc
//!    discloses each arm's exact, real behavior; this item's Self-Check
//!    does too.

use std::io::{IsTerminal, Read};
use std::time::Duration;

use conway::gates::AllowListGate;
use conway::{
    AgentResult, Conway, ConwayError, Event, ForkSpec, LogSeq, RoleAlias, SessionHandle,
    SessionSpec,
};
use futures::StreamExt;

use crate::cli::{Cli, PermissionMode};
use crate::exit::ExitCode;
use crate::{diag, render, session_ref, signal};

/// One-shot mode's entry point (dispatched from `main.rs` when
/// `cli.print.is_some()`). `conway`'s `Runtime` already has this module's
/// [`build_gate`] wired in as its `PermissionGate` -- see reconciliation #1
/// above -- `run` itself never touches gate construction.
pub async fn run(cli: &Cli, conway: Conway) -> conway::Result<ExitCode> {
    let text = read_prompt(cli)?;

    let handle = match resolve_session(cli, &conway, &text).await? {
        SessionOutcome::Live(handle) => handle,
        SessionOutcome::Done(code) => return Ok(code),
    };

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

/// What [`resolve_session`] produced.
enum SessionOutcome {
    /// A live session whose root agent this process can drive with
    /// `.prompt(text)` -- the normal one-shot path (every plain `-p`
    /// invocation, with none of the three continuity flags set).
    Live(SessionHandle),
    /// The requested operation is already complete (or has already failed
    /// as a disclosed, non-retryable limitation) and there is no live turn
    /// to run -- `run` returns this code directly, skipping the renderer
    /// loop entirely.
    Done(ExitCode),
}

/// Resolves `cli.session`/`cli.resume`/`cli.fork_from` (WI-117) -- at most
/// one is ever `Some`, per `cli.rs`'s `conflicts_with_all` -- into either a
/// live [`SessionHandle`] or a [`SessionOutcome::Done`]. See this module's
/// doc comment, reconciliation #4, for the one carried gap that shapes
/// every non-default arm below (`conway-runtime` has no way to register an
/// already-persisted or store-only-created session's agent as live); each
/// arm's own comment repeats only what is specific to that flag.
///
/// Every error this function returns is deliberately built with
/// [`usage_error`] (`ExitCode::Usage`, 2) rather than left to propagate as
/// whatever `ConwayError` variant the facade call itself produced (which
/// would classify as `ExitCode::AgentFailed`, 1, via `exit::classify_runtime_or_routing`'s
/// default arm) -- matching this item's own binding notes: "every failure
/// mode here is a usage error, not an agent failure ... the agent never
/// starts, so no agent status exists to report." That statement is true
/// of the newly-disclosed gaps below exactly as much as it is of the
/// originally-listed ones (unknown session, malformed ref, seq beyond
/// head, duplicate id, conflicting flags): in every arm that returns
/// `Err`, no agent ever started.
async fn resolve_session(cli: &Cli, conway: &Conway, text: &str) -> conway::Result<SessionOutcome> {
    match (&cli.session, &cli.resume, &cli.fork_from) {
        (None, None, None) => {
            let role = cli
                .role_override
                .as_ref()
                .map(|r| RoleAlias::new(r.clone()));
            let spec = SessionSpec {
                role,
                cwd: cli.cwd.clone(),
                ..SessionSpec::default()
            };
            Ok(SessionOutcome::Live(conway.new_session(spec).await?))
        }

        // `--session <id>`: "use (creating if new) a specific session id."
        // The "reusing an existing id without --resume exits 2" half of
        // this item's criterion is fully implementable (a plain
        // `Conway::resume` existence probe). The "creates that id" half is
        // not: see this module's doc comment, reconciliation #4, first
        // bullet. Disclosed here rather than silently creating a
        // *different*, un-requested session id, which would be actively
        // dangerous for a script that asked for this one by name.
        (Some(id), None, None) => {
            let sid = session_ref::parse_sid(id).map_err(|e| usage_error(e.to_string()))?;
            match conway.resume(sid).await {
                Ok(_) => Err(usage_error(format!(
                    "--session {sid}: session already exists; pass --resume {sid} to continue \
                     it instead"
                ))),
                Err(_) => Err(usage_error(format!(
                    "--session {sid}: cannot create a session under a caller-chosen id -- the \
                     `conway` facade has no constructor for one (`SessionSpec` carries no `id` \
                     field, and `Conway::new_session` always mints its own `SessionId`); this is \
                     a disclosed facade gap, not a mistake in the id you passed -- see \
                     `oneshot::resolve_session`'s doc comment"
                ))),
            }
        }

        // `--resume <id>`: reattach and continue. The existence check
        // (`resume_unknown_session` exits 2, before any stdout) is fully
        // implementable and happens first. Driving a *new* turn against
        // the reattached session is not -- see reconciliation #4's second
        // bullet -- so this arm never calls `.prompt()` at all: the
        // failure is 100% deterministic once `resume` itself has
        // succeeded, so attempting it anyway would only produce a
        // misleading `RuntimeError::AgentNotFound` (-> `ExitCode::AgentFailed`,
        // masking "the agent never started" as "the agent failed").
        (None, Some(id), None) => {
            let sid = session_ref::parse_sid(id).map_err(|e| usage_error(e.to_string()))?;
            conway
                .resume(sid)
                .await
                .map_err(|e| usage_error(format!("--resume {sid}: {e}")))?;
            Err(usage_error(format!(
                "--resume {sid}: the session exists, but one-shot mode cannot drive a new turn \
                 against a resumed session yet -- `conway-runtime` exposes no way to re-register \
                 an existing session's agent as live (`Runtime::start_root` is the only \
                 session-starting entry point, and it always creates a brand-new one); this is a \
                 disclosed facade gap, not a usage mistake -- see `oneshot::resolve_session`'s \
                 doc comment"
            )))
        }

        // `--fork-from <ref>`: branch a new session from `<sid>[@seq]`.
        // Reconciliation #4's third bullet: this arm calls
        // `Conway::fork_from` (store-only), which genuinely creates the
        // child (visible via `Conway::sessions`), but cannot start a live
        // turn on it or persist the `-p` text anywhere. Exits 0 (the fork
        // itself succeeded) with a stderr diagnostic, never touching
        // stdout -- satisfying this item's own "stdout purity" criterion
        // trivially, since no assistant turn ever runs.
        (None, None, Some(r)) => {
            let (parent, seq) =
                session_ref::parse_fork_ref(r).map_err(|e| usage_error(e.to_string()))?;
            let at = match seq {
                Some(seq) => seq,
                None => {
                    // No facade method returns a session's head directly;
                    // `SessionHandle::transcript` is documented (WI-103) to
                    // read only through `SessionStore`, unaffected by the
                    // agent not being live, and -- for a session with no
                    // fork ancestry of its own, true of every parent this
                    // suite's tests construct -- its record count equals
                    // that session's own head exactly (`resolve_prefix`'s
                    // no-origin case reads precisely `[0, head)`). This
                    // undercounts for a parent that is *itself* a fork
                    // (its effective transcript also carries its own
                    // ancestor prefix); flagged as a narrower, disclosed
                    // limitation rather than reached around by adding a
                    // `SessionStore::head`-shaped facade method (out of
                    // this item's file scope).
                    let parent_handle = conway
                        .resume(parent)
                        .await
                        .map_err(|e| usage_error(format!("--fork-from {parent}: {e}")))?;
                    let root = parent_handle.root();
                    // Keep this function's invariant that every failure here
                    // is a usage error, not an agent failure (cycle-1 review
                    // M2): a transcript read failure must not leak out as
                    // ExitCode::AgentFailed.
                    let records = parent_handle
                        .transcript(root)
                        .await
                        .map_err(|e| usage_error(format!("--fork-from {parent}: {e}")))?;
                    LogSeq(records.len() as u64)
                }
            };
            let child = conway
                .fork_from(parent, at, ForkSpec::new(text.to_string()))
                .await
                .map_err(|e| usage_error(format!("--fork-from {parent}@{}: {e}", at.0)))?;
            diag::warn(format!(
                "--fork-from created session {} (forked from {parent}@{}) but could not start \
                 its agent: conway-runtime has no way to register a store-only-created session \
                 as a live agent, so the -p prompt was never delivered -- disclosed facade gap, \
                 see oneshot::resolve_session's doc comment",
                child.id(),
                at.0
            ));
            Ok(SessionOutcome::Done(ExitCode::Completed))
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
