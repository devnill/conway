//! Compiled-binary, real-pty acceptance test for the "permission-mode
//! cycling changes what a flagged call does" row of the TUI harness table
//! (board item `01M2M6NR49FYQSRS00B95PTAZT`), tracing to
//! `docs/vision/review/lens-operator.md` (its §3 names "permission-mode
//! cycling" among the paths this project's actual defects live in and
//! expects driven, not merely read from source).
//!
//! Drives `Shift-Tab` (`tui/keybindings.rs`'s own default binding for
//! `cycle_permission_mode`) through Prompt -> Plan -> AutoAllow
//! (`tui/app/run.rs`'s own cycle order) and proves each mode change is
//! BEHAVIORAL, not cosmetic, by pointing the same `bash` tool call at it
//! twice: denied outright in Plan mode (`conway-runtime`'s
//! `PermissionBroker::decide` denies a non-`Read`/`Search`/`Think`
//! category before ever reaching the gate -- no prompt overlay, an
//! immediate, named refusal), then actually executed in AutoAllow mode (a
//! real `bash echo`, matching this suite's own precedent for running one
//! for real -- `tests/oneshot_persona_and_budget.rs`'s
//! `max_turns_flag_overrides_the_configured_default_and_stops_the_run`).
//!
//! **`bash` is opted in for THIS fixture only, via [`write_fixture_with_bash`]
//! below -- not via `fixtures/conway.json.tmpl`.** The TUI's real,
//! shipped default (`main.rs::build_conway`'s `is_tui` branch, which
//! never widens to `PluginSelection::All` the way every non-interactive
//! target does) never registers `bash` at all; every other suite sharing
//! the template fixture relies on that being true. A prior version of
//! this test never opted in, so its own scripted `bash` calls silently
//! never reached the runtime -- `AuthorizedCall`/`PermissionBroker::decide`
//! were never invoked, and BOTH this test's assertions (a negative on the
//! plan-mode output, and a wait on the model's own unconditionally
//! scripted acknowledgement text) held regardless. It was green because
//! the call never happened, not because permission-mode cycling changed
//! what it did. See board item `01M2NSJ0ADSTADK536GHQ7BTB6`, filed on the
//! false premise this test's old green run seemed to prove.

#[allow(dead_code)]
mod common;

use std::time::Duration;

use common::mock_backend::{Chunk, MockBackend, MockHandle, Script};
use common::pty::PtySession;
use common::Fixture;

const LANDED: &str = "Type a message, or / for commands";

/// As [`common::write_fixture`], except `tools.builtin_plugins` also names
/// `"conway.shell"` -- the one opt-in this test needs for its own scripted
/// `bash` calls to reach the runtime at all.
///
/// **Deliberately NOT a change to `fixtures/conway.json.tmpl`.** That
/// template's no-bash default is *correct*: it mirrors a real TUI
/// install's own shipped default (`ToolsConfig::default()`, every builtin
/// EXCEPT `conway.shell`), and many other suites in this directory share
/// it precisely because it is. Widening the shared template would widen
/// every one of those suites' own tool surface along with this one --
/// exactly the kind of fixture-only, test-scoped need
/// `common::write_fixture_with_deadline` already sets a precedent for
/// solving by building its own JSON directly rather than templating (see
/// that function's own doc). This helper follows the identical shape:
/// same `default_role`/`limits`/`backends`/`roles` skeleton
/// [`common::write_fixture`] renders from the template, plus one more
/// top-level `tools.builtin_plugins` key naming all four built-ins
/// (`conway-cli/src/main.rs`'s own `is_tui` branch never widens this, so
/// listing only `conway.shell` on top of the template's implicit three
/// would silently narrow `fs`/`subagent`/`report` back OUT for this one
/// fixture too -- naming all four keeps every OTHER default-on tool this
/// test doesn't otherwise touch unchanged).
fn write_fixture_with_bash(mock: &MockHandle, max_steps: u32) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = serde_json::json!({
        "default_role": "default",
        "limits": { "max_steps": max_steps },
        "backends": {
            "mock": { "kind": "openai-compat", "base_url": mock.base_url, "dialect": "openai" }
        },
        "roles": {
            "default": { "chain": [format!("mock/{}", mock.model)] },
            "coder": { "chain": [format!("mock/{}", mock.model)] }
        },
        "tools": {
            "builtin_plugins": [
                "conway.fs",
                "conway.subagent",
                "conway.report",
                "conway.shell"
            ]
        }
    });
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config).expect("serialize conway.json"),
    )
    .expect("write conway.json");

    // Same `.conway/models.json` requirement as
    // `common::write_fixture_with`/`write_fixture_with_deadline` -- see
    // either's own comment for why the router needs it.
    let models_dir = dir.path().join(".conway");
    std::fs::create_dir_all(&models_dir).expect("create .conway dir");
    let models_json = serde_json::json!({
        "models": {
            format!("mock/{}", mock.model): {
                "max_context_tokens": 128_000,
                "tool_calling": "streaming_validated",
                "reasoning": false,
                "reliability_tier": "verified",
            }
        }
    });
    std::fs::write(
        models_dir.join("models.json"),
        serde_json::to_vec(&models_json).expect("serialize models.json"),
    )
    .expect("write models.json");

    Fixture { dir, config_path }
}

/// Two full tool round-trips: a `bash` call, denied in plan mode, followed
/// by the model's own acknowledgement once it sees the denial; then the
/// SAME shape again once permission mode has moved on to auto-allow, where
/// the call actually runs.
fn two_bash_round_trips() -> Script {
    Script(vec![
        vec![
            Chunk::ToolCall {
                name: "bash",
                args: serde_json::json!({ "command": "echo plan-mode-should-never-run-this" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![
            Chunk::Text("plan-mode-denial-acknowledged"),
            Chunk::Finish("stop"),
        ],
        vec![
            Chunk::ToolCall {
                name: "bash",
                args: serde_json::json!({ "command": "echo auto-allow-ran-it" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![
            Chunk::Text("auto-allow-turn-complete"),
            Chunk::Finish("stop"),
        ],
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permission_mode_cycling_changes_what_a_flagged_call_does() {
    let mock = MockBackend::start(two_bash_round_trips()).await;
    // `write_fixture_with_bash`, NOT `common::write_fixture`: this test's
    // whole point requires `bash` to actually be registered -- see that
    // helper's own doc, and this file's top doc, for why the shared
    // template's no-bash default cannot be used here.
    let fixture = write_fixture_with_bash(&mock, 10);

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    let landed = session.wait_for(LANDED, Duration::from_secs(15));

    // Prompt (the default) -> Plan: one Shift-Tab.
    session.send_shift_tab();
    let in_plan = session.wait_for_since("plan", landed, Duration::from_secs(10));

    // A `bash` call in plan mode is denied outright -- `Execute` is not in
    // plan mode's read-only allow-list, and the broker checks that BEFORE
    // the gate is ever consulted (`conway-runtime/src/permission.rs`'s own
    // "PLAN's denial is checked before EVERY allow path" ordering note),
    // so this proves a real behavioral difference, not merely a status
    // line change.
    session.send("ask for a plan-mode tool call\r");
    // THE STRUCTURAL PROOF that this was specifically a PLAN-MODE broker
    // refusal, not e.g. "tool not registered" (a client-side parse
    // rejection this test would have gotten for free before its own
    // `write_fixture_with_bash` opt-in existed, and a DIFFERENT rejection
    // from the one this test claims to exercise -- see this file's top
    // doc). "tool not registered" never reaches
    // `conway_runtime::PermissionBroker::decide` at all (it is rejected in
    // `conway-plugin-backends::tool_calls::validate` before an
    // `AuthorizedCall` is ever built), so it never emits
    // `Event::PermissionResolved` and never produces this line. A PLAN
    // refusal does: `PermissionBroker::decide`'s own V2 mode gate denies a
    // non-`Read`/`Search`/`Think` category BEFORE the gate is ever
    // consulted, emitting `Event::PermissionResolved { decision: Denied }`
    // -- and `conway-cli/src/tui/state.rs`'s own handler for that event
    // pushes exactly one dim transcript line for `Denied`/
    // `DeniedWithFeedback`: `format!("tool call {call_id} denied")`. No
    // other rejection shape in this codebase produces that exact line.
    // `call_1` is deterministic, not guessed: `MockBackend`'s own
    // `call_id_counter` starts at 1 and this plan-mode `bash` call is the
    // very first `Chunk::ToolCall` `two_bash_round_trips` ever scripts.
    let denial_note = session.wait_for_since(
        "tool call call_1 denied",
        in_plan,
        Duration::from_secs(15),
    );
    // The model's own acknowledgement, chained AFTER the structural denial
    // note above -- proving the ORDER too: the broker resolves and records
    // the refusal before the tool result ever reaches the model for its
    // follow-up reply. Also still useful on its own terms: it is what
    // lets this test move on to the next turn at all, rather than
    // `permission.rs`'s "plan mode does not permit" wording, which is
    // composed into the tool RESULT the model receives and is not
    // guaranteed to surface as its own separate rendered transcript line
    // (an earlier version of this test waited 15s for that wording
    // directly and timed out while the denial had in fact worked
    // perfectly).
    let denied = session.wait_for_since(
        "plan-mode-denial-acknowledged",
        denial_note,
        Duration::from_secs(15),
    );
    // The security property, asserted directly: the command plan mode was
    // supposed to refuse must never have run. This is the half a status-line
    // check cannot give you, and it is stronger than matching the denial's
    // wording -- that text could change without the guarantee changing.
    let screen = session.screen();
    assert!(
        !screen.contains("plan-mode-should-never-run-this"),
        "plan mode must refuse the bash call outright -- its output must never appear. \
         Screen:\n{screen}"
    );

    // Plan -> AutoAllow: a second Shift-Tab.
    session.send_shift_tab();
    let in_auto_allow = session.wait_for_since("AUTO-ALLOW", denied, Duration::from_secs(10));

    // The IDENTICAL kind of call (`bash`) now runs for real, with no
    // permission prompt at all -- the same flagged call, a different
    // outcome, purely from the mode cycle above.
    session.send("ask for an auto-allow tool call\r");
    // THE POSITIVE this whole test exists to prove -- the OTHER half of
    // the defect this file's top doc describes. The mock's model text is
    // unconditionally scripted regardless of what the tool actually did
    // (`two_bash_round_trips` emits `Chunk::Text("auto-allow-turn-
    // complete")` at a fixed script position, not in response to any real
    // tool result), so that text alone cannot distinguish "the call ran"
    // from "the call never happened" -- exactly the gap that let the
    // original version of this test stay green while `bash` was never
    // even registered. `bash`'s own real stdout, "auto-allow-ran-it",
    // only ever reaches the transcript if the call was actually
    // dispatched: registered (this fixture's `conway.shell` opt-in) AND
    // authorized (AutoAllow's own `PermissionBroker::decide`
    // short-circuit, no prompt overlay to block on). Matches this suite's
    // own precedent for asserting directly on real shell stdout
    // (`dogfood_routes_and_status.rs`'s `wait_for("AAAAA", ..)`/
    // `wait_for("BBBBB", ..)`, off a real `bash echo`).
    let ran = session.wait_for_since("auto-allow-ran-it", in_auto_allow, Duration::from_secs(15));

    // Reaching this point at all is FURTHER evidence, chained after the
    // real output above: a permission-prompt overlay (`prompt` mode's own
    // behavior) would have blocked here forever waiting for a keypress
    // this test never sends, and `wait_for_since` would have panicked out
    // with a timeout instead of finding this text -- auto-allow's whole
    // claim is that no such block happens.
    session.wait_for_since("auto-allow-turn-complete", ran, Duration::from_secs(15));
}
