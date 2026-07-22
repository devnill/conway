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
//! 4. **`--session`/`--resume`/`--fork-from` are not handled here.** WI-117
//!    (`oneshot.rs (modify)`, depends on this item) owns wiring those three
//!    flags into session construction; this item unconditionally calls
//!    `conway.new_session`, matching its own scope.

use std::io::{IsTerminal, Read};
use std::time::Duration;

use conway::gates::AllowListGate;
use conway::{AgentResult, Conway, ConwayError, Event, RoleAlias, SessionSpec};
use futures::StreamExt;

use crate::cli::{Cli, PermissionMode};
use crate::exit::ExitCode;
use crate::{diag, render, signal};

/// One-shot mode's entry point (dispatched from `main.rs` when
/// `cli.print.is_some()`). `conway`'s `Runtime` already has this module's
/// [`build_gate`] wired in as its `PermissionGate` -- see reconciliation #1
/// above -- `run` itself never touches gate construction.
pub async fn run(cli: &Cli, conway: Conway) -> conway::Result<ExitCode> {
    let text = read_prompt(cli)?;

    let role = cli
        .role_override
        .as_ref()
        .map(|r| RoleAlias::new(r.clone()));
    let spec = SessionSpec {
        role,
        cwd: cli.cwd.clone(),
        ..SessionSpec::default()
    };
    let handle = conway.new_session(spec).await?;

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
