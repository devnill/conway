//! Integration tests for `--output-schema`, board item
//! `01M02862GPDP6WXH8G0AVAA9BC` -- driven against the real, compiled
//! `conway` binary, exactly as `tests/oneshot_persona_and_budget.rs` drives
//! `--agent`/`--system-prompt`/the budget flags.
//!
//! **The mechanism, in one sentence:** `--output-schema` becomes this run's
//! `result_contract` (the same schema-checked-at-finish machinery
//! `conway_fork`/`conway_spawn`'s own `result_contract` argument already
//! uses for a child agent, `conway-runtime`'s `result.rs`), checked against
//! whatever `structured` value the `report` tool call ends the run with --
//! never a backend-native structured-output/JSON-mode request field (no
//! adapter in this workspace has one). Enforcement is therefore IDENTICAL
//! for every backend/model: this mock stands in for all of them.
//!
//! Every request-shaped assertion below reads the real wire request the
//! mock backend received (`mock.requests()`), or the real process exit
//! code with a known request count -- never a parsed-flag assertion alone.
//! [`schema_rejected_after_one_corrective_retry_then_fails_named`] is the
//! load-bearing negative case: it feeds the mock a `structured` value that
//! never satisfies the schema and asserts the run fails in a NAMED way
//! (`ResultStatus::Rejected`, exit code 1, `missing` reasons in the JSON
//! output) rather than handing back a `Completed` result wrapping
//! unvalidated text.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture, Fixture};
use serde_json::Value;

/// Writes a minimal `.conway/output_schema/<name>.json` file inside
/// `fixture`'s own temp dir (the process cwd `common::command` sets for the
/// spawned binary) and returns the path `--output-schema` should be given
/// (relative to that cwd, matching how an operator would invoke it).
fn write_schema(fixture: &Fixture, name: &str, schema: &Value) -> String {
    let dir = fixture.dir.path().join(".conway").join("output_schema");
    std::fs::create_dir_all(&dir).expect("create .conway/output_schema");
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(schema).unwrap()).expect("write schema file");
    format!(".conway/output_schema/{name}.json")
}

fn write_agent_def(fixture: &Fixture, name: &str, frontmatter_extra: &str, body: &str) {
    let dir = fixture.dir.path().join(".conway").join("agents");
    std::fs::create_dir_all(&dir).expect("create .conway/agents");
    let content = format!("---\nname: {name}\n{frontmatter_extra}\n---\n{body}\n");
    std::fs::write(dir.join(format!("{name}.md")), content).expect("write agent def");
}

/// The `content` string of every `role: "system"` message in `request`'s
/// `messages` array, concatenated -- mirrors `oneshot_persona_and_budget.
/// rs`'s identical helper.
fn system_message_text(request: &Value) -> String {
    request["messages"]
        .as_array()
        .expect("request must carry a messages array")
        .iter()
        .filter(|m| m["role"] == "system")
        .map(|m| m["content"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

fn report_call(structured: Value) -> Vec<Chunk> {
    vec![
        Chunk::ToolCall {
            name: "report",
            args: serde_json::json!({
                "summary": "structured result",
                "structured": structured,
            }),
        },
        Chunk::Finish("tool_calls"),
    ]
}

fn plain_text_turn(text: &'static str) -> Vec<Chunk> {
    vec![Chunk::Text(text), Chunk::Finish("stop")]
}

fn answer_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["answer"],
        "properties": {"answer": {"type": "string"}}
    })
}

fn run_json(args: &[&str], fixture: &Fixture) -> (std::process::Output, Option<Value>) {
    let out = run_conway(args, fixture);
    let value = if out.stdout.is_empty() {
        None
    } else {
        let text = String::from_utf8(out.stdout.clone()).expect("stdout is utf8");
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "--output-format json must write exactly one JSON object, got: {text:?}"
        );
        Some(serde_json::from_str(lines[0]).expect("stdout line is valid JSON"))
    };
    (out, value)
}

// ---------------------------------------------------------------------
// Positive path: a report call whose `structured` satisfies the schema.
// ---------------------------------------------------------------------

/// A `structured` result satisfying `--output-schema` reaches the caller,
/// unedited, as `AgentResult.structured` -- driven through the real binary
/// against a mock provider, per this item's own verification anchor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conforming_structured_result_completes_and_is_returned_unedited() {
    let mock = MockBackend::start(Script(vec![
        report_call(serde_json::json!({"answer": "42"})),
        plain_text_turn("done"),
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);
    let schema_path = write_schema(&fixture, "answer", &answer_schema());

    let (out, value) = run_json(
        &[
            "-p",
            "hi",
            "--output-format",
            "json",
            "--output-schema",
            &schema_path,
            "--allowed-tools",
            "report",
        ],
        &fixture,
    );

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value = value.expect("json output");
    assert_eq!(value["status"]["status"], "completed");
    assert_eq!(
        value["structured"],
        serde_json::json!({"answer": "42"}),
        "a conforming structured result must reach the caller unedited: {value}"
    );
    assert_eq!(
        mock.requests().len(),
        2,
        "exactly two turns: the report call, then the plain-text turn that triggers the \
         finish-boundary contract check"
    );
}

/// `--output-schema`'s own instruction text (telling the model to conclude
/// via `report`'s `structured` argument, matching the schema) reaches the
/// real wire request's system prompt -- this is the "emulate" half of the
/// stated backend-agnostic strategy: conway never uses a backend's native
/// structured-output mode, so the model's only signal is what it reads in
/// its own system prompt plus, on a first mismatch, the corrective
/// `SystemNote`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_schema_instruction_reaches_the_system_prompt() {
    let mock = MockBackend::start(Script(vec![
        report_call(serde_json::json!({"marker_field_xyz": "x"})),
        plain_text_turn("done"),
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);
    let schema = serde_json::json!({
        "type": "object",
        "required": ["marker_field_xyz"],
        "properties": {"marker_field_xyz": {"type": "string"}}
    });
    let schema_path = write_schema(&fixture, "marker", &schema);

    let out = run_conway(
        &[
            "-p",
            "hi",
            "--output-schema",
            &schema_path,
            "--allowed-tools",
            "report",
        ],
        &fixture,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let system_text = system_message_text(&requests[0]);
    assert!(
        system_text.contains("marker_field_xyz"),
        "the schema itself must reach the model's system prompt, got: {system_text:?}"
    );
    assert!(
        system_text.contains("report"),
        "the instruction must direct the model to the `report` tool, got: {system_text:?}"
    );
}

// ---------------------------------------------------------------------
// VERIFICATION ANCHOR: a non-conforming structured result fails NAMED,
// after exactly one corrective retry -- never a silent `Completed` wrapping
// unvalidated text.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_rejected_after_one_corrective_retry_then_fails_named() {
    let mock = MockBackend::start(Script(vec![
        // First attempt: `structured` has no `answer` key at all.
        report_call(serde_json::json!({"wrong_field": true})),
        // Plain-text turn: triggers the finish-boundary contract check,
        // which fails and grants one corrective retry (a `SystemNote`,
        // never surfaced to this mock, but the loop continues).
        plain_text_turn("let me try again"),
        // Second attempt: still non-conforming.
        report_call(serde_json::json!({"still_wrong": 1})),
        // Second plain-text turn: the retry has already been spent, so
        // this failure is terminal.
        plain_text_turn("still working"),
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);
    let schema_path = write_schema(&fixture, "answer", &answer_schema());

    let (out, value) = run_json(
        &[
            "-p",
            "hi",
            "--output-format",
            "json",
            "--output-schema",
            &schema_path,
            "--allowed-tools",
            "report",
        ],
        &fixture,
    );

    // Named failure, not success: exit code 1 (`AgentFailed` -- `exit.rs`'s
    // `ResultStatus::Rejected` arm), never 0.
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value = value.expect("json output");
    assert_eq!(
        value["status"]["status"], "rejected",
        "a non-conforming structured result must terminate as Rejected, not Completed: {value}"
    );
    let missing = value["status"]["missing"]
        .as_array()
        .expect("Rejected carries a `missing` array");
    assert!(
        !missing.is_empty(),
        "the rejection must name what failed, not just that it failed: {value}"
    );
    let missing_text = missing
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        missing_text.contains("answer"),
        "the rejection reasons must name the missing `answer` property: {missing_text}"
    );

    // Exactly one corrective retry: four requests (call, plain-text check,
    // retry call, second plain-text check) -- never fewer (would mean no
    // retry was granted) and never more (would mean it kept retrying
    // forever instead of terminating).
    assert_eq!(
        mock.requests().len(),
        4,
        "must grant exactly one corrective retry before terminating"
    );
}

// ---------------------------------------------------------------------
// Precedence with --agent: the call-site schema (--output-schema) always
// wins over the named agent def's own `result_contract`.
// ---------------------------------------------------------------------

/// An agent def declaring its OWN `result_contract` (requiring
/// `from_agent_def`), combined with `--output-schema` (requiring a
/// DIFFERENT field, `from_flag`): a `structured` result satisfying only
/// the FLAG's schema still completes -- proving `--output-schema` replaced
/// the agent def's own contract outright rather than merging with or
/// losing to it. Mirrors `conway-runtime`'s already-established
/// call-site-over-agent-def precedence for a forked/spawned child's
/// contract (`subagent.rs`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_schema_wins_over_the_agent_defs_own_result_contract() {
    let mock = MockBackend::start(Script(vec![
        report_call(serde_json::json!({"from_flag": "x"})),
        plain_text_turn("done"),
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);
    write_agent_def(
        &fixture,
        "has_own_contract",
        "result_contract:\n  type: object\n  required: [from_agent_def]\n  properties:\n    \
         from_agent_def: { type: string }",
        "Produce structured output.",
    );
    let schema_path = write_schema(
        &fixture,
        "flag_schema",
        &serde_json::json!({
            "type": "object",
            "required": ["from_flag"],
            "properties": {"from_flag": {"type": "string"}}
        }),
    );

    let out = run_conway(
        &[
            "-p",
            "hi",
            "--agent",
            "has_own_contract",
            "--output-schema",
            &schema_path,
            "--allowed-tools",
            "report",
        ],
        &fixture,
    );

    assert!(
        out.status.success(),
        "a structured result satisfying ONLY --output-schema's contract (not the agent def's \
         own) must still complete, proving the call-site contract replaced the def's rather \
         than merging with it -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        mock.requests().len(),
        2,
        "no retry should have been spent: the effective contract was --output-schema's alone"
    );
}

// ---------------------------------------------------------------------
// Usage errors: malformed input and unsupported combinations.
// ---------------------------------------------------------------------

/// A `--output-schema` path that does not exist is a usage error naming
/// the path, with zero requests reaching the mock -- the same
/// "prove-via-zero-requests" bar `oneshot_persona_and_budget.rs`'s budget
/// tests set.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_schema_missing_file_is_a_usage_error_with_zero_requests() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(
        &["-p", "hi", "--output-schema", "does-not-exist.json"],
        &fixture,
    );

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does-not-exist.json"),
        "stderr must name the path: {stderr}"
    );
    assert!(mock.requests().is_empty(), "no request must have been sent");
}

/// A `--output-schema` file that does not compile as a JSON Schema document
/// is a usage error, not a run that starts with an unenforceable
/// "contract".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_schema_malformed_schema_is_a_usage_error_with_zero_requests() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    // A `type` naming something that is not a real JSON Schema type
    // keyword value -- `jsonschema::validator_for` rejects this at compile
    // time.
    let schema_path = write_schema(&fixture, "bad", &serde_json::json!({"type": "not-a-type"}));

    let out = run_conway(&["-p", "hi", "--output-schema", &schema_path], &fixture);

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(mock.requests().is_empty(), "no request must have been sent");
}

/// `--output-schema` combined with `--resume`/`--fork-from` is a usage
/// error, not a silent drop: neither facade path accepts a caller-supplied
/// result-contract override today (see `oneshot::resolve_session`'s own
/// doc for why).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_schema_is_a_usage_error_with_resume_and_fork_from() {
    let mock = MockBackend::start(Script(vec![
        report_call(serde_json::json!({"answer": "42"})),
        plain_text_turn("done"),
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);
    let schema_path = write_schema(&fixture, "answer", &answer_schema());

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success());
    let sid = conway::SessionId::new(); // never created; the guard fires first either way

    let resume_out = run_conway(
        &[
            "-p",
            "hi",
            "--output-schema",
            &schema_path,
            "--resume",
            &sid.to_string(),
        ],
        &fixture,
    );
    assert_eq!(resume_out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&resume_out.stderr).contains("--resume"));

    let fork_out = run_conway(
        &[
            "-p",
            "hi",
            "--output-schema",
            &schema_path,
            "--fork-from",
            &sid.to_string(),
        ],
        &fixture,
    );
    assert_eq!(fork_out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&fork_out.stderr).contains("--fork-from"));
}
