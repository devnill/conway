//! Dogfood gates 7-8 (board item `01M2MEZMH2KJ79XSADQWCBFVFH`), driven
//! against the real, compiled `conway` binary. No feature under test is
//! touched by this file; a failing assertion here is a regression to
//! report, not to fix.
//!
//! # Gate 7 -- children get their budget warning (`01M1ZJSV0CJKQR68FRGYDSY4EJ`)
//!
//! The underlying mechanism (the 80% wrap-up notice reaching a CHILD, the
//! parent's own log recording both the crossing and the termination, and a
//! deadline-killed child naming its interrupted tool call) already has a
//! precise, dedicated in-process test:
//! `crates/conway-runtime/tests/child_budget_notices.rs`'s own single
//! test proves all three together against a real `Runtime` with a
//! `ScriptedBackend`. This file's job is different and narrower: prove the
//! SAME facts are actually OBSERVABLE from the compiled binary an operator
//! runs -- the live transcript notice, the `/agents` panel tag, and the
//! durable session log `conway sessions show` prints -- which that
//! in-process test cannot see (it never runs the CLI binary, the TUI, or
//! the `conway sessions show` formatter at all).
//!
//! # Gate 8 -- a slow plugin does not end the session (`01M1ZJRTKB2JRTYC03SFY8G2BH`)
//!
//! **The no-duplicate-call assertion is the highest-value thing in this
//! whole item** (the board item's own words). `no_call_is_ever_executed_
//! twice_across_a_respawn`, below, is that test: a real MCP server that
//! dies mid-call on generation 1, logging every `tools/call` it ever
//! receives to a file BEFORE deciding whether to answer or die. Two
//! distinct user-level calls, from the model's own script, produce exactly
//! two lines in that file -- never three -- which is the only way this
//! writer found to prove a resend never happened: the file is written
//! SERVER-SIDE, so a client-side resend the client itself considered
//! successful would still show up here as an extra line the client never
//! knew to look for.
//!
//! **CPU-load reproduction, explicitly not attempted.** The board item's
//! own text: "reproducing this under real CPU load (a concurrent `cargo
//! build`) is the shape that originally broke... assert the budgets
//! directly and say what was not covered" if the harness cannot express
//! it. This writer's own hard constraint (rule 2 in this item's brief:
//! "DO NOT RUN ANY CARGO COMMAND") makes spawning a concurrent `cargo
//! build` from inside a test impossible regardless of harness capability --
//! disclosed here, not silently dropped. Every gate 8 test below instead
//! asserts the budgets/grace/respawn/no-duplicate mechanics directly and
//! deterministically, via scripted delays on a real subprocess, never a
//! fixed `sleep` used as this TEST's own synchronization primitive (see
//! each test's own comment on where the delay is the thing under test
//! versus where a `wait_for`/`.output()` block is the thing doing the
//! waiting).
//!
//! **That gap was later carried as its own board item and closed here.**
//! The gate's no-duplicate claim was DEFERRED, not passed, when its item
//! closed ("I think we can skip 7 for now"), and the unrun work became
//! `01M2XN175H315FVAYRSJYE5RA5`: "no call ever executed twice" under real
//! CPU contention. Its tests are in the "exact-once under real CPU
//! contention" section at the bottom of this file. They burn one `sh` loop
//! per core rather than spawning a nested `cargo build` -- a test running
//! under `cargo test` cannot touch a source file to force recompilation,
//! and the relink-under-test hazard (a pty test spawning a binary that a
//! parallel build is mid-relinking) is exactly what this file's fixtures
//! must never race -- but one burner per core saturates the machine the
//! same way the incident's build did (load average 6/11/12, per
//! `GRACE_CEILING_FACTOR`'s own doc), and the one-off evidence run behind
//! the committed tests DID use a real, workspace-wide `cargo build` as the
//! contention source against the REAL ideate plugin (that run is reported
//! on the board item, not asserted here, because it depends on the
//! operator's installed plugin and cannot be hermetic).

mod common;

// A sibling top-level module, not nested inside `common` (`common/mod.rs`
// is out of this writer's fence) -- see its own top doc.
#[path = "common/mcp_fixtures.rs"]
mod mcp_fixtures;

use std::time::Duration;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::pty::PtySession;
use common::{open_conway, run_conway};
use conway::{SessionFilter, SessionId};

const LANDED: &str = "Type a message, or / for commands";

/// As [`common::write_fixture`], except `tools.builtin_plugins` also names
/// `"conway.shell"` -- the one opt-in the TUI test below needs for its own
/// scripted `bash` calls to reach the runtime at all.
///
/// **Why only the TUI test needs this, when its siblings call `bash`
/// freely.** Board item `01M2NSJ0ADSTADK536GHQ7BTB6` was filed believing a
/// SPAWNED CHILD could not call `bash` where the root could. It can; there
/// is no spawn asymmetry. The real split is the ENTRY POINT:
/// `main.rs::build_conway`'s `is_tui` branch never widens to
/// `PluginSelection::All`, deliberately mirroring a real TUI install's
/// shipped default (`ToolsConfig::default()` -- every builtin EXCEPT
/// `conway.shell`), while every non-interactive target does widen. Every
/// other `bash` call in this file runs through `run_conway` (one-shot, so
/// widened); the one below runs through `PtySession` (the TUI path, so
/// not). Root and child alike are refused there, and the diagnostic now
/// says so plainly -- `tool `bash` is not among the 12 tool(s) available
/// to this turn`, that item's own fix.
///
/// **Deliberately NOT a change to `fixtures/conway.json.tmpl`**, for the
/// reason `tui_permission_mode.rs::write_fixture_with_bash` -- this
/// helper's direct model, down to naming all four built-ins rather than
/// only `conway.shell` -- gives at length: the template's no-bash default
/// is CORRECT, many suites share it precisely because it is, and widening
/// it would widen every one of their tool surfaces too.
fn write_fixture_with_bash(
    mock: &common::mock_backend::MockHandle,
    max_steps: u32,
) -> common::Fixture {
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

    // Same `.conway/models.json` requirement as `common::write_fixture` --
    // see its own comment for why the router needs it.
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

    common::Fixture { dir, config_path }
}

// ---------------------------------------------------------------------
// Gate 7 -- children get their budget warning
// ---------------------------------------------------------------------

/// The partial work the budget-killed child says out loud on every turn.
/// Deliberately a phrase that appears nowhere else in this fixture -- not
/// in the root's prompt, not in any scripted root turn, not in any status
/// line -- so finding it in the PARENT's durable log can only mean the
/// child's handback carried it there.
const CHILD_PARTIAL: &str = "so far I covered crates/conway-cli and crates/conway-core";

/// A child spawned with `max_steps=5` on a job that cannot finish (it keeps
/// calling `bash` every turn, never producing a bare text reply) receives
/// the 80% wrap-up notice BEFORE it dies, and that notice reaches BOTH the
/// live transcript (`tui/state.rs`'s `Event::BudgetWarning` arm) and the
/// `/agents` panel's `!budget` tag (`tui/view/agents.rs`) -- proven for a
/// CHILD specifically, never the root, since the board item's own text
/// names the original defect as "the notice fired for the root but not for
/// children" and a root-only test would have passed against that bug.
///
/// `conway_spawn`'s `budget` argument is the child's OWN ceiling
/// (`crates/conway-tools/src/subagent/tools.rs::BudgetArg`), independent
/// of the root's `[limits].max_steps` (set generously large here so only
/// the CHILD's budget is ever in play).
///
/// ## Board item `01M2V047Q99N4HGXF0Y42JVBB9`: the notice must BUY something
///
/// As first written this test proved only that the notice ARRIVED. Its mock
/// child scripted `Chunk::ToolCall` + `Chunk::Finish("tool_calls")` with no
/// `Chunk::Text` anywhere, so the child's `last_assistant_text` was
/// structurally always empty and its handback was always the
/// `terminal_account` placeholder -- meaning the test would have passed
/// unchanged if a budget-killed child could never hand back anything at
/// all, which (per that item, reproduced against a real backend) is exactly
/// what was happening.
///
/// The child now speaks on every turn, so it is a child that CAN report
/// partial work, and the assertions below check that what reaches the
/// parent's durable log is (a) the child's own partial prose and (b) the
/// derived stop note (`conway-runtime`'s `result::budget_stop_note`)
/// naming the ceiling, the progress, the last call, and the transcript to
/// resume from. The original notice-arrival and `!budget` assertions are
/// unchanged and still run first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_with_max_steps_five_gets_the_wrap_up_notice_before_it_dies() {
    // No `tools` selector on the spawn args. `SpawnArgs.tools` maps onto
    // `ToolSelector::Only([...])` (`crates/conway-tools/src/subagent/
    // tools.rs`'s `start_and_maybe_await`), restricting the child's own
    // announced set to exactly that allow-list; omitting it is the
    // documented "inherits this agent's own role/model" default, which is
    // what this test wants -- the child should get the same toolset the
    // root has.
    //
    // The fixture is `write_fixture_with_bash` (above), not
    // `common::write_fixture`. Board item `01M2NSJ0ADSTADK536GHQ7BTB6`
    // was filed against this test believing a spawned CHILD was refused
    // `bash` where the root was allowed it. That was wrong: the TUI never
    // registers `conway.shell` for anyone, root or child, and this is the
    // only test in this file that reaches the runtime through the TUI
    // rather than through one-shot. See that helper's own doc.
    let mut turns = vec![vec![
        Chunk::ToolCall {
            name: "conway_spawn",
            args: serde_json::json!({
                "prompt": "keep running `echo still going` in a loop and report back",
                "budget": {"max_steps": 5},
            }),
        },
        Chunk::Finish("tool_calls"),
    ]];
    // The child's own turns: an unfinishable job -- every turn calls
    // `bash`, never a bare text reply, so ONLY the budget can end it.
    // Scripted generously (6, one more than the 5-step ceiling): if the
    // ceiling trips after exactly 5, the 6th entry is simply never
    // consumed (the shared mock's own documented graceful-unscripted-
    // request fallback covers any request beyond what was scripted
    // anyway, so this is not load-bearing, only a safety margin).
    //
    // Each turn ALSO emits prose alongside its tool call -- the fix for
    // this test's own blind spot (see this fn's doc). `Chunk::Text` here is
    // what `agent_loop`'s `full_text(&outcome.response.content)` captures
    // into `LoopState::last_assistant_text` on every turn, which is in turn
    // what `terminal_account` reports when the ceiling ends the run. The
    // same sentence every turn (rather than a per-turn counter) so the
    // assertion below does not depend on WHICH turn happened to be last;
    // text BEFORE the tool call, mirroring how a real model narrates then
    // acts.
    //
    // Exactly FIVE such entries, matching the ceiling -- the 6th safety-
    // margin entry below deliberately carries NO `CHILD_PARTIAL`. `Script`
    // is one flat, globally-sequential list (`common/mock_backend.rs`: "one
    // entry per successive request the CLI makes"), so the margin entry is
    // consumed by whichever agent makes the 6th request -- in practice the
    // ROOT's own follow-up turn, since the ceiling trips the child after
    // exactly 5. Putting `CHILD_PARTIAL` in it would have the ROOT say the
    // sentence into its own session log, and the assertion below would then
    // pass against a completely empty handback -- the very false pass this
    // item is about. Keeping it out means `CHILD_PARTIAL` can only ever
    // reach the parent's log by travelling child -> handback -> parent.
    for _ in 0..5 {
        turns.push(vec![
            Chunk::Text(CHILD_PARTIAL),
            Chunk::ToolCall {
                name: "bash",
                args: serde_json::json!({"command": "echo still going"}),
            },
            Chunk::Finish("tool_calls"),
        ]);
    }
    turns.push(vec![
        Chunk::ToolCall {
            name: "bash",
            args: serde_json::json!({"command": "echo still going"}),
        },
        Chunk::Finish("tool_calls"),
    ]);
    // The root's own follow-up turn once the child's terminal result comes
    // back as `conway_spawn`'s own tool output.
    turns.push(vec![Chunk::Text("done"), Chunk::Finish("stop")]);

    let mock = MockBackend::start(Script(turns)).await;
    let fixture = write_fixture_with_bash(&mock, 40);

    // `--allowed-tools` is a ONE-SHOT-only gate (`conway-cli/src/oneshot.rs`
    // reads `cli.allowed_tools`; the interactive TUI path never does) --
    // the interactive equivalent, matching `tui_permission_mode.rs`'s own
    // precedent, is cycling permission mode to AutoAllow (Prompt -> Plan ->
    // AutoAllow, two `Shift-Tab`s) so neither the root's `conway_spawn` nor
    // the child's own `bash` calls ever block on an unanswered permission
    // prompt this test would otherwise hang on.
    //
    // NOT `--session <id>` here to pin the root up front (this test's
    // first fix attempt): `cli.rs`'s own doc on `Cli::session` states it is
    // "still one-shot-only" -- the TUI refuses it outright at startup
    // (`conflicts_with_all`-adjacent refusal). The root is identified
    // AFTER the run instead, by filtering `conway.sessions(..)` for the
    // one entry with `origin: None` -- see the comment at that call site,
    // below, for why that is reliable even though `conway_spawn` makes
    // this fixture end up with TWO sessions on disk, not one.
    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    let landed = session.wait_for(LANDED, Duration::from_secs(15));
    session.send_shift_tab();
    let landed = session.wait_for_since("plan", landed, Duration::from_secs(10));
    session.send_shift_tab();
    let landed = session.wait_for_since("AUTO-ALLOW", landed, Duration::from_secs(10));

    session.send("delegate the loop to a child\r");

    // The wrap-up notice, live, on the transcript -- BEFORE the run
    // otherwise completes. `wait_for` fails loudly (panicking with the
    // captured screen) if this never arrives, rather than silently timing
    // out into a false pass.
    let notice_at =
        session.wait_for_since("is nearing a budget limit", landed, Duration::from_secs(30));

    // Let the run finish naturally (the root's own follow-up turn) BEFORE
    // opening `/agents` -- the panel is a full-pane overlay
    // (`tui/view/agents.rs`), so checking it mid-run would risk the
    // transcript's own "done" text never being drawn while it is open.
    // Ordering is still proven: the notice already fired (`notice_at`,
    // above) strictly before this point.
    session.wait_for_since("done", notice_at, Duration::from_secs(30));

    // `/agents` marks the row -- opened once the run is fully settled, so
    // there is no risk of racing the panel against the transcript still
    // streaming.
    session.send("/agents\r");
    // Await `!budget` itself, NOT the panel title. `"agents ("` looks like
    // the obvious "the panel opened" marker and cannot work: `"agents "` is
    // already on screen continuously as the status line's own `/agents to
    // view` hint, and a terminal re-emits only the cells that CHANGED, so
    // the title's leading run is never rewritten and the string never
    // appears contiguously in an emission-order transcript
    // (`CONTRIBUTING.md`'s partial-redraw note). `!budget` appears nowhere
    // else in this session, so awaiting it proves both halves at once --
    // the panel rendered, and the child's row carries the tag.
    let tag_at = session.wait_for_since("!budget", notice_at, Duration::from_secs(10));
    let _ = tag_at;
    let screen = session.screen();
    assert!(
        screen.contains("!budget"),
        "the CHILD's own row must carry the `!budget` tag once it has crossed a warning -- \
         `tui/view/agents.rs`'s own `draw_tags_a_row_whose_agent_crossed_a_budget_warning` unit \
         test pins the tag text exactly; this proves the live wiring that feeds it end to end. \
         Screen:\n{screen}"
    );

    // The parent's own DURABLE log records both the crossing (a
    // `SystemNote` reason `"child_budget"`, `mailbox.rs`'s own
    // `AgentMessage::BudgetNotice` arm) and the termination (a
    // `ChildResultRecord`) -- read back through the real `sessions show`
    // subcommand against the on-disk store, not the live transcript this
    // test already checked above.
    //
    // Two sessions exist on disk at this point (root + the child's own,
    // separate session -- `conway_spawn`'s `SubagentMode::Spawn` calls
    // `SessionStore::create` for the child, `conway-runtime/src/
    // subagent.rs`'s own `SubagentMode::Spawn` arm, unlike `conway_fork`
    // which stays inside the parent's session). `SessionMeta::origin` is
    // `None` for a session nobody forked/spawned -- i.e. the root -- and
    // `Some(ForkOrigin { .. })` for the child, so filtering on it finds
    // the ROOT specifically rather than assuming list order or count.
    let conway = open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    let root_sessions: Vec<_> = sessions.iter().filter(|s| s.origin.is_none()).collect();
    assert_eq!(
        root_sessions.len(),
        1,
        "expected exactly one ROOT session (origin: None) among {} total session(s)",
        sessions.len()
    );
    let root_id = root_sessions[0].id;

    let show = run_conway(&["sessions", "show", &root_id.to_string()], &fixture);
    assert!(
        show.status.success(),
        "sessions show must succeed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let show_stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        show_stdout.contains("child_budget"),
        "the parent's own durable log must record the crossing (SystemNote reason \
         \"child_budget\") -- got: {show_stdout}"
    );
    assert!(
        show_stdout.contains("child_result"),
        "the parent's own durable log must record the child's termination \
         (ChildResultRecord) -- got: {show_stdout}"
    );

    // Board item `01M2V047Q99N4HGXF0Y42JVBB9`. Everything above proves the
    // notice ARRIVED. These prove it BOUGHT something: that what the parent
    // actually received is more than a failed tool call.
    //
    // (a) The child's own partial work. `CHILD_PARTIAL` is scripted ONLY
    // into the child's turns (see the script comment above), so its
    // presence in the ROOT's log means the handback carried it -- through
    // `terminal_account` -> `ResultBuilder::resolve` -> `AgentResult::
    // summary` -> both `conway-tools`' `agent_result_output` (the awaited
    // `conway_spawn` tool result) and `context::builder`'s
    // `child_result_text` (the `ChildResultRecord`). Against HEAD before
    // this item the child could emit no text at all and this fails.
    assert!(
        show_stdout.contains(CHILD_PARTIAL),
        "a budget-killed child's PARTIAL work must reach the parent, not be lost with the \
         child -- the parent's log must carry the child's own words ({CHILD_PARTIAL:?}), \
         got: {show_stdout}"
    );

    // (b) The derived stop note -- the half that holds even when the model
    // ignores its wrap-up notice entirely, since every input is already in
    // the runtime's hand (`conway-runtime`'s `result::budget_stop_note`).
    //
    // Asserted against ONE LINE of the output rather than the whole dump:
    // `sessions show` prints each record with `{:#?}`, which renders a
    // multi-line summary as a single escaped line, and the note itself is
    // one line by construction (`budget_stop_note`'s own unit test pins
    // that). Whole-dump `contains` would let the transcript reference below
    // pass on an unrelated occurrence -- the child's session id is already
    // in the parent's log as `AgentResult::transcript_ref` regardless of
    // this item, so only "in the note, on the same line" is evidence.
    let note_line = show_stdout
        .lines()
        .find(|line| line.contains("[partial handback -- budget exceeded]"))
        .unwrap_or_else(|| {
            panic!(
                "the handback must MARK ITSELF PARTIAL so the parent does not read it as a \
                 final answer -- got: {show_stdout}"
            )
        });
    assert!(
        note_line.contains("max_steps=5 (this session)"),
        "the derived note must name WHICH ceiling ended the child -- got: {note_line}"
    );
    // The `max_steps` equivalent of the deadline path's own
    // `interrupted_call_note` coverage in
    // `child_killed_mid_tool_call_names_the_interrupted_call`, below: WHERE
    // in the work it stopped, not only which limit it hit. `bash(` alone,
    // not the rendered arguments -- `{:#?}` escapes the quotes inside the
    // JSON args summary.
    assert!(
        note_line.contains("stopped at: bash("),
        "the `max_steps` path must name where the child stopped (its last dispatched call), \
         as the deadline path already does -- got: {note_line}"
    );
    // The resume pointer, checked against the CHILD's real session id
    // rather than a substring shape, so a note naming the wrong session
    // (or the parent's own) would fail here.
    let child_id = sessions
        .iter()
        .find(|s| s.origin.is_some())
        .expect("the spawned child must have its own session")
        .id;
    assert!(
        note_line.contains(&child_id.to_string()),
        "the derived note must point at the CHILD's own transcript so the parent can resume \
         rather than redo the work -- expected {child_id}, got: {note_line}"
    );
}

/// A child killed mid-tool-call BY ITS OWN DEADLINE (a budget dimension
/// distinct from `max_steps`) names the interrupted call in its result,
/// not only the limit it hit -- `AgentLoop::finish_cancelled`'s own
/// `interrupted_call_note` path (`crates/conway-runtime/src/agent_loop.rs`,
/// `crates/conway-runtime/src/result.rs::interrupted_call_note`).
///
/// The delay (`bash sleep 5`) is the THING UNDER TEST, not this test's own
/// synchronization: the child's `deadline_secs: 1` elapses while that real
/// subprocess is genuinely in flight, which is exactly the race the
/// board item names ("a child killed mid-tool-call"). This test's own wait
/// is `run_conway`'s ordinary `.output()` block (bounded by one-shot's
/// 300s default deadline, not by this delay) -- not a `sleep` inserted to
/// line anything up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_killed_mid_tool_call_names_the_interrupted_call() {
    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "conway_spawn",
                // No `tools` selector -- see the identical fix and its own
                // full explanation on `child_with_max_steps_five_gets_the_
                // wrap_up_notice_before_it_dies`, above: an explicit
                // `"tools": ["bash"]` here left the child with no `bash`
                // announced at all, so its scripted call was rejected
                // client-side before ever reaching the runtime. Omitting
                // `tools` gives the child the same full toolset the root
                // already uses `bash` from successfully.
                args: serde_json::json!({
                    "prompt": "run `sleep 5` in bash",
                    "budget": {"deadline_secs": 1},
                }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![
            Chunk::ToolCall {
                name: "bash",
                args: serde_json::json!({"command": "sleep 5"}),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = common::write_fixture(&mock, 40);

    // `--session <id>` pins the ROOT's own session id -- see
    // `child_with_max_steps_five_gets_the_wrap_up_notice_before_it_dies`'s
    // own comment on why `conway_spawn` makes a bare `conway.sessions(..)`
    // listing return two entries (root + the child's own separate
    // session), not one.
    let root_id = SessionId::new();

    let out = run_conway(
        &[
            "-p",
            "delegate a slow bash call",
            "--allowed-tools",
            "bash,conway_spawn",
            "--session",
            &root_id.to_string(),
        ],
        &fixture,
    );
    assert!(
        out.status.success(),
        "the ROOT run must still complete even though its child was killed -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let show = run_conway(&["sessions", "show", &root_id.to_string()], &fixture);
    assert!(show.status.success(), "sessions show must succeed");
    let show_stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        show_stdout.contains("budget: deadline="),
        "the child's own terminal reason must attribute the kill to its budget deadline, not \
         a bare \"cancelled\" -- got: {show_stdout}"
    );
    assert!(
        show_stdout.contains("interrupted mid-call: bash"),
        "the child's own result must name the INTERRUPTED CALL (bash), not only the limit it \
         hit -- got: {show_stdout}"
    );
}

// ---------------------------------------------------------------------
// Gate 8 -- a slow plugin does not end the session
// ---------------------------------------------------------------------

/// A call that runs past the configured per-call deadline, but within the
/// bounded grace (`GRACE_CEILING_FACTOR = 3`,
/// `crates/conway-tools/src/process/child_session.rs`), produces a WARNING
/// and COMPLETES -- never a kill. `timeout_ms=300`/`SLEEP_MS=400` stands in
/// for the board item's own "1 ms past the 5 s deadline" illustration at a
/// scale this suite can run 20 times without spending 20x5s -- the RATIO
/// (overshoot comfortably inside the grace window, `900ms` here) is what
/// is actually under test, not the literal millisecond counts.
///
/// The `time.sleep` inside the scripted MCP server is the thing under
/// test; this test's own wait is `run_conway`'s `.output()` (bounded by
/// one-shot's real deadline), never a `sleep` this test inserted to
/// synchronize anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_past_the_deadline_but_within_grace_warns_and_completes() {
    let script_dir = tempfile::tempdir().expect("tempdir");
    let script_path =
        mcp_fixtures::write_script(script_dir.path(), "sleep.py", mcp_fixtures::SLEEP_SERVER);
    mcp_fixtures::warm(&script_path).await;

    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "sleep",
                args: serde_json::json!({}),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    let mcp = mcp_fixtures::McpEntryCfg {
        id: "dogfood-sleep",
        command: vec![script_path.display().to_string()],
        timeout_ms: 300,
        first_call_timeout_ms: 300,
        env: vec![("SLEEP_MS".to_string(), "400".to_string())],
    };
    let fixture = mcp_fixtures::write_fixture_with_mcp(&mock.base_url, &mock.model, 40, &mcp);

    let out = run_conway(
        &[
            "-p",
            "call the sleep tool",
            "--allowed-tools",
            "sleep",
            "-v",
        ],
        &fixture,
    );
    assert!(
        out.status.success(),
        "a call inside grace must still complete the run -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("exceeded its per-call deadline"),
        "the overshoot must produce the grace WARNING (`child_session.rs`'s own \
         `tracing::warn!` text) -- stderr: {stderr}"
    );
}

/// The FIRST real call after `initialize` gets the (larger) warm-up
/// budget, not the flat ordinary per-call deadline -- proven by
/// DIFFERENCE, not by waiting out the real ~20s default
/// (`conway::plugin::DEFAULT_FIRST_CALL_TIMEOUT_MS`): `first_call_
/// timeout_ms=800` (warm-up) is set LARGER than `timeout_ms=200`
/// (ordinary), and the server sleeps a constant `500ms` on every call.
/// Call 1 (500ms sleep against an 800ms base) never overshoots its own
/// deadline at all -- no warning. Call 2 (the SAME 500ms sleep, now
/// against the 200ms ordinary base) overshoots and enters grace. If the
/// first call were governed by the flat ordinary deadline instead of its
/// own warm-up budget, call 1 would ALSO warn -- this test's own
/// discriminator is the total warning COUNT across both calls, not
/// either call in isolation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_call_gets_the_warm_up_budget_not_the_flat_ordinary_deadline() {
    let script_dir = tempfile::tempdir().expect("tempdir");
    let script_path =
        mcp_fixtures::write_script(script_dir.path(), "sleep.py", mcp_fixtures::SLEEP_SERVER);
    mcp_fixtures::warm(&script_path).await;

    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "sleep",
                args: serde_json::json!({}),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![
            Chunk::ToolCall {
                name: "sleep",
                args: serde_json::json!({}),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    let mcp = mcp_fixtures::McpEntryCfg {
        id: "dogfood-sleep",
        command: vec![script_path.display().to_string()],
        timeout_ms: 200,
        first_call_timeout_ms: 800,
        env: vec![("SLEEP_MS".to_string(), "500".to_string())],
    };
    let fixture = mcp_fixtures::write_fixture_with_mcp(&mock.base_url, &mock.model, 40, &mcp);

    let out = run_conway(
        &[
            "-p",
            "call the sleep tool twice",
            "--allowed-tools",
            "sleep",
            "-v",
        ],
        &fixture,
    );
    assert!(
        out.status.success(),
        "both calls must still complete (both are within THEIR OWN grace ceiling) -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let warnings = stderr.matches("exceeded its per-call deadline").count();
    assert_eq!(
        warnings, 1,
        "exactly ONE warning expected -- call 1 (500ms sleep against the 800ms warm-up budget) \
         must never trigger it, call 2 (the identical 500ms sleep, now against the 200ms \
         ordinary deadline) must. Got {warnings} warning(s) in stderr:\n{stderr}"
    );
}

/// A genuinely wedged server (never answers, at all) is still killed --
/// at the FULL ceiling (`base_timeout_ms * GRACE_CEILING_FACTOR`), never
/// earlier and never left hanging forever. Asserted as a BUDGET (the exact
/// ceiling the error names), not by timing the wall clock -- timing
/// assertions are exactly the flake risk this item warns about twice over.
/// `timeout_ms=100` keeps the real wall-clock cost of this test to ~300ms.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_genuinely_wedged_server_is_killed_at_the_full_ceiling() {
    let script_dir = tempfile::tempdir().expect("tempdir");
    let script_path =
        mcp_fixtures::write_script(script_dir.path(), "sleep.py", mcp_fixtures::SLEEP_SERVER);
    mcp_fixtures::warm(&script_path).await;

    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "sleep",
                args: serde_json::json!({}),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    let mcp = mcp_fixtures::McpEntryCfg {
        id: "dogfood-wedged",
        command: vec![script_path.display().to_string()],
        timeout_ms: 100,
        first_call_timeout_ms: 100,
        // Far longer than 100ms * 3 (the full ceiling) -- never answers in
        // time, by construction.
        env: vec![("SLEEP_MS".to_string(), "600000".to_string())],
    };
    let fixture = mcp_fixtures::write_fixture_with_mcp(&mock.base_url, &mock.model, 40, &mcp);

    let out = run_conway(
        &["-p", "call the sleep tool", "--allowed-tools", "sleep"],
        &fixture,
    );
    assert!(
        out.status.success(),
        "a wedged TOOL must not end the ROOT's own run -- the tool call fails, the agent \
         continues -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The kill is visible in what the MODEL was actually told: the tool
    // result content of the SECOND request (the root's follow-up turn,
    // carrying call 1's outcome).
    let requests = mock.requests();
    assert!(
        requests.len() >= 2,
        "expected at least 2 requests, got {}",
        requests.len()
    );
    let second = serde_json::to_string(&requests[1]).expect("serialize request");
    assert!(
        second.contains("timed out after 300ms"),
        "the wedged call must be killed at the FULL ceiling (100ms base * 3 = 300ms), named \
         exactly in the error the model receives -- request 2 body: {second}"
    );
}

/// **The critical assertion for gate 8.** After a session dies mid-call
/// (generation 1 of `mcp_fixtures::DIE_ONCE_SERVER` exits without ever
/// answering), the NEXT call spawns a fresh session and succeeds --
/// respawn, not a repeat of `timed out` then `session died` forever. AND,
/// simultaneously, the server-side request log proves NEITHER call was
/// ever executed twice: two distinct user-level tool calls produce exactly
/// TWO lines in `REQUEST_LOG_FILE`, written by the SERVER itself before it
/// decides whether to answer or die -- a resend the CLIENT believed was
/// safe would still show up here as a THIRD line, since the log is written
/// server-side, independent of what the client itself concluded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_call_is_ever_executed_twice_across_a_respawn() {
    let script_dir = tempfile::tempdir().expect("tempdir");
    let script_path = mcp_fixtures::write_script(
        script_dir.path(),
        "die_once.py",
        mcp_fixtures::DIE_ONCE_SERVER,
    );
    mcp_fixtures::warm(&script_path).await;
    let counter_file = script_dir.path().join("spawn_counter.txt");
    let request_log_file = script_dir.path().join("request_log.jsonl");

    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "die",
                args: serde_json::json!({"attempt": 1}),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![
            Chunk::ToolCall {
                name: "die",
                args: serde_json::json!({"attempt": 2}),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    let mcp = mcp_fixtures::McpEntryCfg {
        id: "dogfood-die-once",
        command: vec![script_path.display().to_string()],
        timeout_ms: 3_000,
        first_call_timeout_ms: 3_000,
        env: vec![
            (
                "SPAWN_COUNTER_FILE".to_string(),
                counter_file.display().to_string(),
            ),
            (
                "REQUEST_LOG_FILE".to_string(),
                request_log_file.display().to_string(),
            ),
        ],
    };
    let fixture = mcp_fixtures::write_fixture_with_mcp(&mock.base_url, &mock.model, 40, &mcp);

    let out = run_conway(
        &["-p", "call the die tool twice", "--allowed-tools", "die"],
        &fixture,
    );
    assert!(
        out.status.success(),
        "the run must complete: call 1's death is transparently respawned before call 2 -- \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let spawn_count = std::fs::read_to_string(&counter_file)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(
        spawn_count, 2,
        "exactly one respawn expected: generation 1 (dies on call 1) plus generation 2 (serves \
         call 2) -- got {spawn_count} spawns"
    );

    let request_log = std::fs::read_to_string(&request_log_file).unwrap_or_default();
    let request_count = request_log.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        request_count, 2,
        "THE CRITICAL ASSERTION: exactly two `tools/call`s must ever reach a server process -- \
         one per real user-level call. A THIRD line here would mean a resend happened after \
         the respawn, which would double a side-effecting call in production. Full log:\n{request_log}"
    );
    assert!(
        request_log.contains("\"attempt\":1") || request_log.contains("\"attempt\": 1"),
        "the logged call must be the FIRST one (never resent to generation 2) -- log:\n{request_log}"
    );
    assert!(
        request_log.contains("\"attempt\":2") || request_log.contains("\"attempt\": 2"),
        "the second, genuinely NEW call must reach generation 2 -- log:\n{request_log}"
    );

    // What the model was actually told corroborates the file evidence:
    // call 1's own tool result names the transport death; call 2's own
    // tool result is a clean success naming generation 2's pid.
    let requests = mock.requests();
    assert!(
        requests.len() >= 3,
        "expected at least 3 requests, got {}",
        requests.len()
    );
    let after_call_1 = serde_json::to_string(&requests[1]).expect("serialize");
    assert!(
        after_call_1.contains("session died"),
        "call 1's own result must show the transport death, not a silently-papered-over \
         success -- request 2 body: {after_call_1}"
    );
    let after_call_2 = serde_json::to_string(&requests[2]).expect("serialize");
    assert!(
        after_call_2.contains("pid="),
        "call 2 must succeed against the FRESH (respawned) session -- request 3 body: \
         {after_call_2}"
    );
}

/// A crash-looping server (dies on EVERY `tools/call`, every generation)
/// eventually STAYS DOWN rather than being respawned forever --
/// `conway_plugin_mcp::MAX_AUTO_RESPAWNS = 3`. Five real, distinct calls
/// against `ALWAYS_DIE_SERVER` must spawn at most four processes total (the
/// original plus three respawns), never five or six.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_looping_server_eventually_stays_down() {
    let script_dir = tempfile::tempdir().expect("tempdir");
    let script_path = mcp_fixtures::write_script(
        script_dir.path(),
        "always_dies.py",
        mcp_fixtures::ALWAYS_DIE_SERVER,
    );
    mcp_fixtures::warm(&script_path).await;
    let counter_file = script_dir.path().join("spawn_counter.txt");

    let mut turns = Vec::new();
    for i in 0..5 {
        turns.push(vec![
            Chunk::ToolCall {
                name: "boom",
                args: serde_json::json!({"attempt": i}),
            },
            Chunk::Finish("tool_calls"),
        ]);
    }
    turns.push(vec![Chunk::Text("done"), Chunk::Finish("stop")]);
    let mock = MockBackend::start(Script(turns)).await;

    let mcp = mcp_fixtures::McpEntryCfg {
        id: "dogfood-crashloop",
        command: vec![script_path.display().to_string()],
        timeout_ms: 1_000,
        first_call_timeout_ms: 1_000,
        env: vec![(
            "SPAWN_COUNTER_FILE".to_string(),
            counter_file.display().to_string(),
        )],
    };
    let fixture = mcp_fixtures::write_fixture_with_mcp(&mock.base_url, &mock.model, 40, &mcp);

    let out = run_conway(
        &["-p", "call boom five times", "--allowed-tools", "boom"],
        &fixture,
    );
    assert!(
        out.status.success(),
        "the ROOT run must still complete even though every call to the crash-looping plugin \
         fails -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let spawn_count = std::fs::read_to_string(&counter_file)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    // `MAX_AUTO_RESPAWNS + 1` (the original spawn plus at most 3 bounded
    // respawns) -- the gate's own claim ("eventually stays down rather
    // than being respawned forever"), asserted as the BEHAVIOURAL fact
    // (how many processes this host actually started), not as wording.
    // `conway_plugin_mcp::McpPluginError::SessionDied`'s `detail` text
    // does NOT distinguish "still respawning" from "budget exhausted,
    // staying down" -- every one of the five calls here reports the
    // identical "closed stdout (EOF) mid-session" regardless of which
    // case it is. That indistinguishability is real and is its own,
    // separate finding (an operator watching the transcript cannot tell
    // "still trying" from "given up" from the message alone) -- reported
    // as such, not asserted here, and not conflated with this test's own
    // claim, which is purely about the respawn COUNT being bounded.
    assert!(
        spawn_count <= 4,
        "MAX_AUTO_RESPAWNS (3) bounds the respawn count: at most 1 initial spawn + 3 respawns \
         = 4 total, even though 5 calls were attempted against a server that dies on every \
         single one. A 5th or 6th spawn would mean the respawn budget is not actually bounded. \
         Got {spawn_count}"
    );
    assert_eq!(
        spawn_count, 4,
        "this exact fixture (5 calls, all against a server that dies on every generation) \
         should exhaust the full respawn budget -- got {spawn_count}. If this is ever LESS \
         than 4, that is worth its own look (a respawn that gave up early), but was not \
         observed while writing this test."
    );
}

// ---------------------------------------------------------------------
// Exact-once under real CPU contention (`01M2XN175H315FVAYRSJYE5RA5`)
//
// The deferred half of gate 8. When the gate closed, the operator said
// "I think we can skip 7 for now" -- a deliberate deferral of the
// CPU-load reproduction this file's earlier doc discloses as "explicitly
// not attempted". This section carries it: the checkable claim is
// "no call ever executed twice" under load, and it is testable WITHOUT
// the live board, because the board these tests write to is a plain
// JSON-lines scratch store inside each test's own temp dir (the real
// ideate board is a SQLite store under the real project root; nothing
// here ever touches it, and every call names a scratch tenant besides).
//
// What these tests add over `no_call_is_ever_executed_twice_across_a_
// respawn`, above, is the CONDITION the incident actually happened in:
// every core busy. One `sh` spin loop per core (see `CpuBurners`), not a
// nested `cargo build` -- see the module doc at the top of this file for
// why a test must not build under test. The server-side execution log
// (`mcp_fixtures::BOARD_SERVER`) is the observer: written by the server
// process before a tool does anything, so a resend the CLIENT believed
// was safe shows up as an extra line the client never knew to look for,
// and the store rows are the side effects those executions had.
//
// P-15 (prove the checker fails before trusting it) is
// `the_exact_once_check_fails_against_an_injected_duplicate`, below: the
// duplicate is injected by genuinely executing the same calls a second
// time through the whole real stack -- client, server, store -- into the
// scratch tenant only, and the check must reject that run.
// ---------------------------------------------------------------------

/// The tenant every call in this section names. The scratch store starts
/// empty, so a call that fell back to ideate's default tenant (`local`)
/// would leave a row THIS verifier never expected -- and the verifier
/// treats any `local` row in the scratch store as a failure, so the
/// "never the live board's tenant" premise is checked, not assumed.
const SCRATCH_TENANT: &str = "gate7-scratch";

/// One spin loop per core, running for exactly the lifetime of this
/// guard. `sh -c 'while :; do :; done'` is the cheapest full-core burner
/// the host ships: no dependencies, no build step, ~100% of one core. One
/// per `available_parallelism` saturates the machine the same way the
/// incident's `cargo build --workspace` did -- the scheduler, not the
/// data, is what goes slow.
///
/// Killed on drop, so a panicking test never leaves burners behind.
struct CpuBurners {
    children: Vec<tokio::process::Child>,
}

impl CpuBurners {
    fn start() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let mut children = Vec::with_capacity(cores);
        for _ in 0..cores {
            let child = tokio::process::Command::new("sh")
                .arg("-c")
                .arg("while :; do :; done")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn cpu burner");
            children.push(child);
        }
        CpuBurners { children }
    }

    fn pids(&self) -> Vec<u32> {
        self.children.iter().filter_map(|c| c.id()).collect()
    }

    fn count(&self) -> usize {
        self.children.len()
    }

    /// Mean recent CPU usage across the burners, sampled with `ps` while
    /// they run. EVIDENCE, not a synchronization primitive: it goes into
    /// assertion messages so a failure report shows whether the machine
    /// was genuinely saturated when the run it is complaining about
    /// executed. A burner reading ~100 means it owned a core; a burner
    /// reading near zero would mean the OS never scheduled it, which is
    /// the one condition under which "contention" would be a fiction.
    fn mean_cpu_percent(&self) -> Option<f64> {
        let pids = self.pids();
        if pids.is_empty() {
            return None;
        }
        let list = pids
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let out = std::process::Command::new("ps")
            .args(["-o", "%cpu=", "-p", &list])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let vals: Vec<f64> = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .filter_map(|t| t.parse().ok())
            .collect();
        if vals.is_empty() {
            return None;
        }
        Some(vals.iter().sum::<f64>() / vals.len() as f64)
    }
}

impl Drop for CpuBurners {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.start_kill();
        }
    }
}

/// Everything the board fixture needs, bundled: the `[plugins].mcp[]` entry
/// plus the three on-disk artifacts the OUTSIDE verification reads.
struct Board {
    entry: mcp_fixtures::McpEntryCfg,
    store: std::path::PathBuf,
    exec_log: std::path::PathBuf,
    spawn_counter: std::path::PathBuf,
}

/// Writes [`mcp_fixtures::BOARD_SERVER`] into `dir` (the scratch board's
/// own root -- the store never lives anywhere the real board does) and
/// returns the entry config naming it. `delay_ms` and `die_on_create` are
/// the fixture's two experimental knobs (see that constant's doc).
fn board_fixture(dir: &std::path::Path, delay_ms: u64, die_on_create: u32) -> Board {
    let script_path = mcp_fixtures::write_script(dir, "board.py", mcp_fixtures::BOARD_SERVER);
    let store = dir.join("board_items.jsonl");
    let exec_log = dir.join("board_exec_log.jsonl");
    let spawn_counter = dir.join("board_spawn_counter.txt");
    Board {
        entry: mcp_fixtures::McpEntryCfg {
            id: "dogfood-board",
            command: vec![script_path.display().to_string()],
            timeout_ms: 50,
            first_call_timeout_ms: 1_000,
            env: vec![
                ("BOARD_STORE_FILE".to_string(), store.display().to_string()),
                (
                    "BOARD_EXEC_LOG_FILE".to_string(),
                    exec_log.display().to_string(),
                ),
                (
                    "BOARD_SPAWN_COUNTER_FILE".to_string(),
                    spawn_counter.display().to_string(),
                ),
                ("BOARD_DELAY_MS".to_string(), delay_ms.to_string()),
                ("BOARD_DIE_ON_CREATE".to_string(), die_on_create.to_string()),
            ],
        },
        store,
        exec_log,
        spawn_counter,
    }
}

/// One scripted `work_create` turn's tool-call chunks.
fn create_call(title: &str) -> Vec<Chunk> {
    vec![
        Chunk::ToolCall {
            name: "work_create",
            args: serde_json::json!({
                "title": title,
                "spec": format!("scratch-board evidence item: {title}"),
                "spec_format": "plan/outline",
                "tenant_id": SCRATCH_TENANT,
                "actor_human": "dan",
            }),
        },
        Chunk::Finish("tool_calls"),
    ]
}

/// Reads a JSON-lines file into values; an absent file is empty (a store
/// nobody wrote yet is a legitimate verification input, not an error).
fn read_jsonl(path: &std::path::Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("scratch board files are JSON lines"))
        .collect()
}

/// THE EXACT-ONCE CHECK -- the outside observer every test in this section
/// trusts. Reads the scratch store and the server-side execution log with
/// a fresh `std::fs` read (never the plugin's own reports, never the
/// session's transcript, never anything the SESSION wrote) and demands:
///
/// 1. exactly `expected_titles.len()` items in `tenant` -- no missing item
///    (a call that silently vanished) and no extra one (a call executed
///    twice);
/// 2. the item titles match `expected_titles` exactly, as multisets -- a
///    resent `work_create` creates a second row with the SAME title and a
///    NEW id, which this catches even though both rows are individually
///    well-formed;
/// 3. every item id is unique within the tenant;
/// 4. the execution log holds exactly one `work_create` execution for the
///    tenant per expected call, with matching titles -- the server-side
///    ground truth a client-side resend would contradict;
/// 5. NO item in the DEFAULT tenant (`local`) exists at all -- the
///    scratch-isolation premise, checked rather than assumed.
fn verify_exact_once(board: &Board, tenant: &str, expected_titles: &[&str]) -> Result<(), String> {
    let items = read_jsonl(&board.store);
    let mine: Vec<&serde_json::Value> = items
        .iter()
        .filter(|i| i.get("tenant_id").and_then(|t| t.as_str()) == Some(tenant))
        .collect();
    if mine.len() != expected_titles.len() {
        let expected = expected_titles.len();
        let found = mine.len();
        return Err(format!(
            "expected {expected} items in tenant {tenant:?}, found {found} -- a call either \
             never landed or executed more than once. Items: {items:?}"
        ));
    }
    let mut got_titles: Vec<String> = mine
        .iter()
        .map(|i| {
            i.get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect();
    got_titles.sort();
    let mut want: Vec<String> = expected_titles.iter().map(|t| t.to_string()).collect();
    want.sort();
    if got_titles != want {
        return Err(format!(
            "item titles do not match the calls that were made (a duplicate title means a \
             resent create): got {got_titles:?}, want {want:?}"
        ));
    }
    let mut ids: Vec<String> = mine
        .iter()
        .map(|i| {
            i.get("id")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect();
    let id_count = ids.len();
    ids.sort();
    ids.dedup();
    if ids.len() != id_count {
        return Err(format!("duplicate item id in tenant {tenant:?}: {ids:?}"));
    }

    let exec_entries = read_jsonl(&board.exec_log);
    let execs: Vec<&serde_json::Value> = exec_entries
        .iter()
        .filter(|e| e.get("tool").and_then(|t| t.as_str()) == Some("work_create"))
        .filter(|e| e.pointer("/params/tenant_id").and_then(|t| t.as_str()) == Some(tenant))
        .collect();
    if execs.len() != expected_titles.len() {
        return Err(format!(
            "expected {} server-side work_create executions in tenant {tenant:?}, the log has \
             {} -- any excess is a call the client executed twice. Log: {execs:?}",
            expected_titles.len(),
            execs.len(),
        ));
    }
    let mut exec_titles: Vec<String> = execs
        .iter()
        .map(|e| {
            e.pointer("/params/title")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect();
    exec_titles.sort();
    if exec_titles != want {
        return Err(format!(
            "server-side execution log titles do not match the calls made: got \
             {exec_titles:?}, want {want:?}"
        ));
    }

    let strangers: Vec<&serde_json::Value> = items
        .iter()
        .filter(|i| i.get("tenant_id").and_then(|t| t.as_str()) != Some(tenant))
        .collect();
    if !strangers.is_empty() {
        return Err(format!(
            "the scratch store must never hold an item outside tenant {tenant:?} -- a \
             `local`-tenant row here would mean a call reached the default tenant (the live \
             board's own): {strangers:?}"
        ));
    }
    Ok(())
}

/// THE OBJECTIVE HALF OF THE DEFERRED GATE, under the incident's own
/// condition: every core busy. Five DISTINCT scripted `work_create` calls
/// ride one real one-shot run against the scratch board while one burner
/// per core spins; each create sleeps 60ms server-side against a 50ms
/// ordinary per-call deadline, so every call after the first crosses its
/// deadline INTO the grace window by construction (the first rides the
/// 1000ms warm-up budget instead) -- the only path a resend could ever
/// happen on. The grace WARNING is asserted, so the run provably went
/// through that path rather than around it.
///
/// The outside verification then reads the store and the server-side
/// execution log fresh: five calls, five items, five executions, exactly
/// once each -- and nothing in the default tenant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn five_distinct_work_creates_under_full_core_contention_execute_exactly_once() {
    let script_dir = tempfile::tempdir().expect("tempdir");
    let board = board_fixture(script_dir.path(), 60, 0);
    mcp_fixtures::warm(std::path::Path::new(&board.entry.command[0])).await;

    let titles = [
        "gate7 load item 1",
        "gate7 load item 2",
        "gate7 load item 3",
        "gate7 load item 4",
        "gate7 load item 5",
    ];
    let mut turns: Vec<Vec<Chunk>> = titles.iter().map(|t| create_call(t)).collect();
    // The final in-session look at the board, then the run's own close.
    turns.push(vec![
        Chunk::ToolCall {
            name: "work_list",
            args: serde_json::json!({ "tenant_id": SCRATCH_TENANT }),
        },
        Chunk::Finish("tool_calls"),
    ]);
    turns.push(vec![Chunk::Text("done"), Chunk::Finish("stop")]);
    let mock = MockBackend::start(Script(turns)).await;
    let fixture =
        mcp_fixtures::write_fixture_with_mcp(&mock.base_url, &mock.model, 40, &board.entry);

    let burners = CpuBurners::start();
    let cpu_note = burners.mean_cpu_percent();
    let out = run_conway(
        &[
            "-p",
            "create the five board items",
            "--allowed-tools",
            "work_create,work_list",
            "-v",
        ],
        &fixture,
    );
    // Sampled while the burners were still alive, AFTER the contended run
    // they were burning through -- the evidence the contention was real.
    let cpu_after = burners.mean_cpu_percent();
    let burner_count = burners.count();
    drop(burners);

    assert!(
        out.status.success(),
        "the run must complete under contention: grace absorbs the overshoot -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The slow-call path was genuinely exercised: every ordinary call
    // (2..=5; call 1 rides the 1000ms warm-up budget) slept 60ms against a
    // 50ms deadline, so each one warned into grace. Fewer than four means
    // a call was killed at the ceiling instead -- which the store evidence
    // below would still survive, but this assertion is what pins the run
    // to the path the gate is about.
    let stderr = String::from_utf8_lossy(&out.stderr);
    let warnings = stderr.matches("exceeded its per-call deadline").count();
    assert!(
        warnings >= 4,
        "each of calls 2..=5 sleeps 60ms against its own 50ms ordinary deadline, so all four \
         must have crossed INTO grace (call 1 rides the warm-up budget and never warns). Got \
         {warnings} warning(s), burners={burner_count}, mean burner cpu before/after the run: \
         {cpu_note:?}/{cpu_after:?}. stderr:\n{stderr}"
    );

    // The outside verification: a fresh read of the files the SERVER wrote,
    // never anything the session itself reported.
    verify_exact_once(&board, SCRATCH_TENANT, &titles).unwrap_or_else(|err| {
        panic!(
            "EXACT-ONCE VIOLATION under {burner_count}-core contention (mean burner cpu \
             before/after the run: {cpu_note:?}/{cpu_after:?}): {err}"
        )
    });
}

/// P-15: the check above is only as good as its ability to FAIL. The
/// duplicate is injected the honest way -- the same two `work_create` calls
/// genuinely executed a SECOND time through the whole real stack (client,
/// server, store), against the SAME scratch store, in the scratch tenant
/// only -- which is exactly what a resent call pair would have produced.
/// The check must reject the doubled board, and must keep accepting it
/// once the duplicate is removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_exact_once_check_fails_against_an_injected_duplicate() {
    let script_dir = tempfile::tempdir().expect("tempdir");
    let board = board_fixture(script_dir.path(), 0, 0);
    mcp_fixtures::warm(std::path::Path::new(&board.entry.command[0])).await;

    let titles = ["gate7 p15 item 1", "gate7 p15 item 2"];
    // Two clean runs' worth of script, served sequentially by ONE mock: the
    // second run below replays the IDENTICAL calls against the SAME store.
    let mut turns: Vec<Vec<Chunk>> = Vec::new();
    for _ in 0..2 {
        for t in titles.iter() {
            turns.push(create_call(t));
        }
        turns.push(vec![Chunk::Text("done"), Chunk::Finish("stop")]);
    }
    let mock = MockBackend::start(Script(turns)).await;
    let fixture =
        mcp_fixtures::write_fixture_with_mcp(&mock.base_url, &mock.model, 40, &board.entry);

    // Run 1 -- clean. The check accepts it.
    let out = run_conway(
        &[
            "-p",
            "create the two board items",
            "--allowed-tools",
            "work_create",
            "-v",
        ],
        &fixture,
    );
    assert!(
        out.status.success(),
        "the clean run must complete -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    verify_exact_once(&board, SCRATCH_TENANT, &titles)
        .unwrap_or_else(|err| panic!("the CLEAN run must pass the exact-once check: {err}"));

    // THE INJECTION: run 2 executes the identical calls again -- a real
    // duplicate execution through the real client and server, landing in
    // the scratch tenant only. This is what a resent call would have done.
    let out2 = run_conway(
        &[
            "-p",
            "create the two board items AGAIN",
            "--allowed-tools",
            "work_create",
            "-v",
        ],
        &fixture,
    );
    assert!(
        out2.status.success(),
        "the injecting run must complete -- stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let verdict = verify_exact_once(&board, SCRATCH_TENANT, &titles);
    assert!(
        verdict.is_err(),
        "P-15: the check MUST fail against the injected duplicate -- it accepted four items \
         (two executed twice) as if nothing had happened. Store:\n{}\nExec log:\n{}",
        std::fs::read_to_string(&board.store).unwrap_or_default(),
        std::fs::read_to_string(&board.exec_log).unwrap_or_default(),
    );
    let reason = verdict.unwrap_err();
    assert!(
        reason.contains("duplicate title") || reason.contains("found 4"),
        "the failure must NAME the duplication (not fail for some unrelated reason): {reason}"
    );

    // Removal: put the scratch store and log back to their clean two-line
    // state and the same check accepts the same board again.
    for (file, keep) in [(&board.store, 2usize), (&board.exec_log, 2usize)] {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let kept: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(keep)
            .collect();
        std::fs::write(file, kept.join("\n") + "\n").expect("restore scratch file");
    }
    verify_exact_once(&board, SCRATCH_TENANT, &titles).unwrap_or_else(|err| {
        panic!("after removing the injected duplicate the check must pass again: {err}")
    });
}

/// The incident's literal shape, under load: the plugin process KILLED
/// mid-call (generation 1 executes `work_create`'s side effect, then exits
/// without ever answering, `BOARD_DIE_ON_CREATE=1`) while every core is
/// busy. What must hold, all at once: the item the killed call already
/// wrote stays in the store EXACTLY ONCE (a resent call would write a
/// second row with the same title); the run still completes; the next call
/// lands against the respawned session; and the server-side log shows one
/// execution per real call -- never a third.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_killed_mid_call_under_full_core_contention_never_repeats_a_side_effect() {
    let script_dir = tempfile::tempdir().expect("tempdir");
    let board = board_fixture(script_dir.path(), 0, 1);
    mcp_fixtures::warm(std::path::Path::new(&board.entry.command[0])).await;

    let titles = ["gate7 killed-call item 1", "gate7 killed-call item 2"];
    let mock = MockBackend::start(Script(vec![
        create_call(titles[0]),
        create_call(titles[1]),
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture =
        mcp_fixtures::write_fixture_with_mcp(&mock.base_url, &mock.model, 40, &board.entry);

    let burners = CpuBurners::start();
    let out = run_conway(
        &[
            "-p",
            "create two board items",
            "--allowed-tools",
            "work_create",
            "-v",
        ],
        &fixture,
    );
    let cpu_after = burners.mean_cpu_percent();
    drop(burners);
    assert!(
        out.status.success(),
        "a mid-call death must not end the run: the plugin respawns, the agent continues -- \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let spawn_count = std::fs::read_to_string(&board.spawn_counter)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(
        spawn_count,
        2,
        "exactly two server processes expected -- the original (killed mid-call) and the one \
         respawn call 2 found in place. Got {spawn_count}: {:?}\nexec log:\n{}\nstderr:\n{}",
        std::fs::read_to_string(&board.spawn_counter).unwrap_or_default(),
        std::fs::read_to_string(&board.exec_log).unwrap_or_default(),
        String::from_utf8_lossy(&out.stderr)
    );

    // What the model was told corroborates the kill: call 1's own tool
    // result names the transport death, never a papered-over success.
    let requests = mock.requests();
    assert!(
        requests.len() >= 2,
        "expected at least 2 requests, got {}",
        requests.len()
    );
    let after_call_1 = serde_json::to_string(&requests[1]).expect("serialize request");
    assert!(
        after_call_1.contains("session died"),
        "call 1's own result must show the transport death -- request 2 body: {after_call_1}"
    );

    verify_exact_once(&board, SCRATCH_TENANT, &titles).unwrap_or_else(|err| {
        panic!(
            "EXACT-ONCE VIOLATION after a mid-call kill under contention (burners sampled at \
             {cpu_after:?}% mean CPU): {err}"
        )
    });
}
