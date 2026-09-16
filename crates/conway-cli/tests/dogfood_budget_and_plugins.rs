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

mod common;

// A sibling top-level module, not nested inside `common` (`common/mod.rs`
// is out of this writer's fence) -- see its own top doc.
#[path = "common/mcp_fixtures.rs"]
mod mcp_fixtures;

use std::time::Duration;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::pty::PtySession;
use common::{open_conway, run_conway};
use conway::SessionFilter;

const LANDED: &str = "Type a message, or / for commands";

// ---------------------------------------------------------------------
// Gate 7 -- children get their budget warning
// ---------------------------------------------------------------------

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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_with_max_steps_five_gets_the_wrap_up_notice_before_it_dies() {
    let mut turns = vec![vec![
        Chunk::ToolCall {
            name: "conway_spawn",
            args: serde_json::json!({
                "prompt": "keep running `echo still going` in a loop and report back",
                "budget": {"max_steps": 5},
                "tools": ["bash"],
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
    for _ in 0..6 {
        turns.push(vec![
            Chunk::ToolCall {
                name: "bash",
                args: serde_json::json!({"command": "echo still going"}),
            },
            Chunk::Finish("tool_calls"),
        ]);
    }
    // The root's own follow-up turn once the child's terminal result comes
    // back as `conway_spawn`'s own tool output.
    turns.push(vec![Chunk::Text("done"), Chunk::Finish("stop")]);

    let mock = MockBackend::start(Script(turns)).await;
    let fixture = common::write_fixture(&mock, 40);

    // `--allowed-tools` is a ONE-SHOT-only gate (`conway-cli/src/oneshot.rs`
    // reads `cli.allowed_tools`; the interactive TUI path never does) --
    // the interactive equivalent, matching `tui_permission_mode.rs`'s own
    // precedent, is cycling permission mode to AutoAllow (Prompt -> Plan ->
    // AutoAllow, two `Shift-Tab`s) so neither the root's `conway_spawn` nor
    // the child's own `bash` calls ever block on an unanswered permission
    // prompt this test would otherwise hang on.
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
    let notice_at = session.wait_for_since(
        "is nearing a budget limit",
        landed,
        Duration::from_secs(30),
    );

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
    session.wait_for_since("agents (", notice_at, Duration::from_secs(10));
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
    let conway = open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    assert_eq!(sessions.len(), 1, "expected exactly one root session");
    let sid = sessions[0].id;

    let show = run_conway(&["sessions", "show", &sid.to_string()], &fixture);
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
                args: serde_json::json!({
                    "prompt": "run `sleep 5` in bash",
                    "budget": {"deadline_secs": 1},
                    "tools": ["bash"],
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

    let out = run_conway(
        &["-p", "delegate a slow bash call", "--allowed-tools", "bash,conway_spawn"],
        &fixture,
    );
    assert!(
        out.status.success(),
        "the ROOT run must still complete even though its child was killed -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let conway = open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    assert_eq!(sessions.len(), 1, "expected exactly one root session");
    let sid = sessions[0].id;

    let show = run_conway(&["sessions", "show", &sid.to_string()], &fixture);
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
    let script_path = mcp_fixtures::write_script(script_dir.path(), "sleep.py", mcp_fixtures::SLEEP_SERVER);
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
        &["-p", "call the sleep tool", "--allowed-tools", "sleep", "-v"],
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
    let script_path = mcp_fixtures::write_script(script_dir.path(), "sleep.py", mcp_fixtures::SLEEP_SERVER);
    mcp_fixtures::warm(&script_path).await;

    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall { name: "sleep", args: serde_json::json!({}) },
            Chunk::Finish("tool_calls"),
        ],
        vec![
            Chunk::ToolCall { name: "sleep", args: serde_json::json!({}) },
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
        &["-p", "call the sleep tool twice", "--allowed-tools", "sleep", "-v"],
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
    let script_path = mcp_fixtures::write_script(script_dir.path(), "sleep.py", mcp_fixtures::SLEEP_SERVER);
    mcp_fixtures::warm(&script_path).await;

    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall { name: "sleep", args: serde_json::json!({}) },
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

    let out = run_conway(&["-p", "call the sleep tool", "--allowed-tools", "sleep"], &fixture);
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
    assert!(requests.len() >= 2, "expected at least 2 requests, got {}", requests.len());
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
    let script_path =
        mcp_fixtures::write_script(script_dir.path(), "die_once.py", mcp_fixtures::DIE_ONCE_SERVER);
    mcp_fixtures::warm(&script_path).await;
    let counter_file = script_dir.path().join("spawn_counter.txt");
    let request_log_file = script_dir.path().join("request_log.jsonl");

    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall { name: "die", args: serde_json::json!({"attempt": 1}) },
            Chunk::Finish("tool_calls"),
        ],
        vec![
            Chunk::ToolCall { name: "die", args: serde_json::json!({"attempt": 2}) },
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
            ("SPAWN_COUNTER_FILE".to_string(), counter_file.display().to_string()),
            ("REQUEST_LOG_FILE".to_string(), request_log_file.display().to_string()),
        ],
    };
    let fixture = mcp_fixtures::write_fixture_with_mcp(&mock.base_url, &mock.model, 40, &mcp);

    let out = run_conway(&["-p", "call the die tool twice", "--allowed-tools", "die"], &fixture);
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
    assert!(requests.len() >= 3, "expected at least 3 requests, got {}", requests.len());
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
        env: vec![("SPAWN_COUNTER_FILE".to_string(), counter_file.display().to_string())],
    };
    let fixture = mcp_fixtures::write_fixture_with_mcp(&mock.base_url, &mock.model, 40, &mcp);

    let out = run_conway(&["-p", "call boom five times", "--allowed-tools", "boom"], &fixture);
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
    assert_eq!(
        spawn_count, 4,
        "at most MAX_AUTO_RESPAWNS (3) respawns on top of the original spawn -- 4 total -- \
         even though 5 calls were attempted; a 5th or 6th spawn would mean the respawn budget \
         is not actually bounded. Got {spawn_count}"
    );

    let requests = mock.requests();
    assert!(requests.len() >= 6, "expected at least 6 requests, got {}", requests.len());
    let last = serde_json::to_string(&requests[5]).expect("serialize");
    assert!(
        last.contains("gave up auto-respawning after 3 attempts"),
        "once the respawn budget is exhausted, the failure must say so explicitly rather than \
         attempting a new spawn -- request 6 body: {last}"
    );
}
