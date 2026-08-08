//! WI-099: agent definition loader — non-recursive `*.md` discovery,
//! well-formed parsing (all `AgentDef` fields), and fail-loud, path-naming
//! errors for every documented malformation.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use conway::agents::load_agent_defs;
use conway::{AgentDef, ConwayError};
use conway_core::agent::ToolSelector;
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};

// ---------------------------------------------------------------------
// End-to-end: a `result_contract` declared in a real, on-disk agent-def
// file is enforced when a subagent is spawned FROM THAT DEF -- board item
// wiring `AgentDef.result_contract` into `subagent.rs`'s `SubagentHost::
// start`. Everything below drives the real loader (`load_agent_defs`) and
// the real runtime (`conway_runtime::runtime::Runtime`), never a
// hand-constructed `AgentDef` fed straight to `AgentSpec` -- see this
// file's own module doc note (this is exactly the distinction the
// coordinator's guidance calls out: a hand-built `AgentDef` proves only
// that the field is populated, not that anything reads it).
mod result_contract_via_def {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use conway_core::agent::{
        AgentDefRef, Budget, PermissionDecision, ResultStatus, SubagentSpec,
    };
    use conway_core::capabilities::{
        CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
    };
    use conway_core::content::{ContentBlock, StopReason, Usage};
    use conway_core::error::RoutingError;
    use conway_core::fakes::{FakeGate, FakeHealth, FakeStore, ScriptedBackend, ScriptedTurn};
    use conway_core::ids::{AgentId, BackendId, ModelId, RoleAlias};
    use conway_core::log::LogRecord;
    use conway_core::ports::{
        Backend, GenerateResponse, HealthRegistry, Plugin, Router, SessionStore, SubagentHost,
    };
    use conway_core::capabilities::HeadroomPolicy;
    use conway_core::routing::{Route, RouteRequest, RoutingReason};
    use conway_runtime::events::EventBus;
    use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};

    use super::{dir_with_fixtures, load_agent_defs};

    fn caps_ok() -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::Streaming { validated: true },
            cache: CacheMode::None,
            parallel_tool_calls: true,
            structured_output: StructuredOutput::None,
            max_context_tokens: 1_000_000,
            reasoning: false,
            reliability_tier: ReliabilityTier::Verified,
        }
    }

    fn text_response(text: &str) -> GenerateResponse {
        GenerateResponse {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            },
        }
    }

    /// Routes `"parent"` and `"child"` roles to distinct backends, so the
    /// parent root agent and the def-spawned child (sharing one `Runtime`)
    /// never contend over the same `ScriptedBackend`'s script queue --
    /// mirrors `conway-runtime`'s `tests/result_contract.rs::RoleRouter`.
    struct RoleRouter {
        parent: Route,
        child: Route,
    }

    impl Router for RoleRouter {
        fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError> {
            if req.role.as_str() == "child" {
                Ok(vec![self.child.clone()])
            } else {
                Ok(vec![self.parent.clone()])
            }
        }
    }

    /// Builds a `Runtime` with `agent_defs` loaded from the given fixture
    /// file names via the REAL loader (`conway::agents::load_agent_defs`),
    /// and a `parent`/`child` `ScriptedBackend` pair wired through
    /// `RoleRouter`. Returns `(runtime, parent_agent_id, store)` with the
    /// parent root agent already started; `store` is the same store instance
    /// injected into the runtime, kept for direct log inspection (`Runtime`
    /// exposes no accessor for it).
    async fn build_runtime_with_def(
        label: &str,
        fixture: &str,
        child_script: Vec<ScriptedTurn>,
    ) -> (Arc<Runtime>, AgentId, Arc<dyn SessionStore>) {
        let dir = dir_with_fixtures(label, &[fixture]);
        let agent_defs = load_agent_defs(&dir).expect("real def file loads");
        assert!(
            !agent_defs.is_empty(),
            "fixture must parse into at least one AgentDef"
        );

        let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
        let health: Arc<dyn HealthRegistry> = Arc::new(FakeHealth::new());

        let parent_backend = Arc::new(
            ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("parent turn"))])
                .with_id(BackendId::new("parent-backend"))
                .with_capabilities(caps_ok()),
        );
        let child_backend = Arc::new(
            ScriptedBackend::new(child_script)
                .with_id(BackendId::new("child-backend"))
                .with_capabilities(caps_ok()),
        );
        let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
        backends.insert(parent_backend.id(), parent_backend);
        backends.insert(child_backend.id(), child_backend);

        let router = Arc::new(RoleRouter {
            parent: Route {
                backend: BackendId::new("parent-backend"),
                model: ModelId::new("m"),
                params: Default::default(),
                reason: RoutingReason::AliasPrimary {
                    alias: RoleAlias::new("parent"),
                },
            },
            child: Route {
                backend: BackendId::new("child-backend"),
                model: ModelId::new("m"),
                params: Default::default(),
                reason: RoutingReason::AliasPrimary {
                    alias: RoleAlias::new("child"),
                },
            },
        });

        let runtime = Runtime::new(RuntimeDeps {
            store: store.clone(),
            router,
            health,
            backends,
            plugins: Vec::<Arc<dyn Plugin>>::new(),
            gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
            agent_defs,
            event_bus: EventBus::new(1024),
            headroom: Arc::new(HeadroomPolicy::default()),
        });

        let parent = runtime
            .start_root(RootSpec {
                session: None,
                agent_def: None,
                role: Some(RoleAlias::new("parent")),
                tools: None,
                budget: Budget::default(),
                cwd: PathBuf::from("/tmp"),
                root: None,
                prompt: Some("go".to_string()),
                keep_alive: false,
                model: None,
            })
            .await
            .unwrap();

        (runtime, parent, store)
    }

    /// The main defect-fix test: `contract_child.md`'s `result_contract`
    /// (`{required: [ok]}`) is loaded through the real on-disk def path and
    /// applied to a child spawned via `AgentDefRef("contract_child")` with
    /// NO call-site `result_contract` of its own. The child never emits a
    /// `structured` value (no `report` call in either scripted turn), so
    /// its `structured` is treated as `null` and fails the contract both
    /// times: first failure -> corrective retry (exactly one
    /// `result_contract_violation` `SystemNote`, exactly like the
    /// call-site-only path `conway-runtime`'s `tests/result_contract.rs`
    /// already covers); second failure -> terminal `Rejected { missing }`.
    /// Before this item's fix, `subagent.rs` never read
    /// `AgentDef.result_contract` at all, so this child would instead
    /// complete normally on its first turn -- see this item's
    /// break-the-guard note for the confirmed failure.
    #[tokio::test]
    async fn def_result_contract_is_enforced_end_to_end_retry_then_reject() {
        let (runtime, parent, store) = build_runtime_with_def(
            "def-contract-e2e",
            "contract_child.md",
            vec![
                ScriptedTurn::Respond(text_response("child turn 1")),
                ScriptedTurn::Respond(text_response("child turn 2")),
            ],
        )
        .await;

        let spec = SubagentSpec::spawn(
            "do the child's work",
            AgentDefRef("contract_child".to_string()),
            Budget::default(),
        );
        assert!(
            spec.result_contract.is_none(),
            "this test's whole point is a call site that supplies NO contract of its own"
        );

        let child = SubagentHost::start(&*runtime, parent, parent, spec)
            .await
            .unwrap();
        let result = SubagentHost::await_result(&*runtime, parent, child)
            .await
            .unwrap();

        match &result.status {
            ResultStatus::Rejected { missing } => {
                // The child never calls `report`, so `structured` resolves
                // to `null`; the def's `{type: object, required: [ok]}`
                // fails the TYPE check before `required` is even reached
                // (`validate_result_contract`'s own null-treatment, exactly
                // like `conway-runtime`'s
                // `a_missing_structured_value_entirely_is_treated_as_null_and_fails_an_object_contract`)
                // -- `missing` therefore names the type mismatch, not the
                // `ok` property by name. What matters here is that
                // rejection happened at all: before this item's fix,
                // `subagent.rs` never read `AgentDef.result_contract`, so a
                // spawn with no call-site contract of its own would have no
                // contract to fail and would instead `Completed` on its
                // very first turn.
                assert!(!missing.is_empty(), "missing must be non-empty: {missing:?}");
            }
            other => panic!(
                "expected the def-declared result_contract to reject this child's undeclared \
                 structured output, got {other:?} -- if this is Completed, the def's \
                 result_contract was never applied at all"
            ),
        }
        assert_eq!(
            result.steps_taken, 2,
            "one corrective retry must have been spent (2 turns: fail, retry, fail again)"
        );

        let records = store
            .read(&result.transcript_ref, conway_core::ids::SeqRange::full())
            .await
            .unwrap();
        let violation_notes = records
            .iter()
            .filter(|r| {
                matches!(r, LogRecord::SystemNote { reason, .. } if reason == "result_contract_violation")
            })
            .count();
        assert_eq!(
            violation_notes, 1,
            "exactly one corrective retry must have been spent before the terminal rejection"
        );
    }

    /// Precedence: an EXPLICIT call-site `result_contract` (permissive:
    /// matches anything, including the child's `null` structured output)
    /// must win over the def's own stricter contract (`required: [ok]`,
    /// which the child's plain-text-only turn would fail). The child
    /// backend is scripted with exactly ONE turn -- if the def's contract
    /// won instead, validation would fail and the loop would need a
    /// second corrective turn the script does not provide, which would
    /// surface as something other than a clean `Completed` on the first
    /// turn (this is the discriminating observable, not a log message).
    #[tokio::test]
    async fn call_site_result_contract_wins_over_the_defs_when_both_are_set() {
        let (runtime, parent, _store) = build_runtime_with_def(
            "def-contract-precedence",
            "contract_child.md",
            vec![ScriptedTurn::Respond(text_response("child turn 1 only"))],
        )
        .await;

        let mut spec = SubagentSpec::spawn(
            "do the child's work",
            AgentDefRef("contract_child".to_string()),
            Budget::default(),
        );
        // The empty JSON Schema matches every instance, including the
        // `null` this child's structured output resolves to -- deliberately
        // permissive so ONLY the precedence rule (not contract strictness)
        // determines the outcome.
        spec.result_contract =
            Some(serde_json::from_value(serde_json::json!({})).expect("empty schema compiles"));

        let child = SubagentHost::start(&*runtime, parent, parent, spec)
            .await
            .unwrap();
        let result = SubagentHost::await_result(&*runtime, parent, child)
            .await
            .unwrap();

        assert_eq!(
            result.status,
            ResultStatus::Completed,
            "the call site's permissive contract must have been the one enforced, not the \
             def's stricter `required: [ok]` contract"
        );
        assert_eq!(
            result.steps_taken, 1,
            "zero retries: the permissive call-site contract passed on the first attempt"
        );
    }
}

/// Each test gets its own scratch directory (no external `tempfile`
/// dependency, matching `tests/support/mod.rs`'s existing convention) so
/// fixtures with deliberately conflicting/broken content never interfere
/// with each other or with parallel test threads.
fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "conway-agent-defs-test-{label}-{}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agents")
}

/// Copies the named fixtures (by file name, e.g. `"reviewer.md"`) into a
/// fresh scratch directory and returns that directory's path.
fn dir_with_fixtures(label: &str, names: &[&str]) -> PathBuf {
    let dir = scratch_dir(label);
    for name in names {
        let content = fs::read_to_string(fixtures_dir().join(name))
            .unwrap_or_else(|err| panic!("read fixture {name}: {err}"));
        fs::write(dir.join(name), content).unwrap_or_else(|err| panic!("write {name}: {err}"));
    }
    dir
}

fn load_single(label: &str, name: &str) -> Result<HashMap<String, AgentDef>, ConwayError> {
    let dir = dir_with_fixtures(label, &[name]);
    load_agent_defs(&dir)
}

fn expect_agent_def_error(
    result: Result<HashMap<String, AgentDef>, ConwayError>,
    file_name: &str,
) -> String {
    match result {
        Ok(defs) => panic!("expected an error, got Ok({defs:?})"),
        Err(ConwayError::AgentDef { path, message }) => {
            assert_eq!(
                path.file_name().and_then(|n| n.to_str()),
                Some(file_name),
                "error path should name the offending file"
            );
            message
        }
        Err(other) => panic!("expected ConwayError::AgentDef, got {other:?}"),
    }
}

#[test]
fn missing_dir_returns_ok_empty_map() {
    let defs = load_agent_defs(Path::new("/does/not/exist/conway-agents")).unwrap();
    assert!(defs.is_empty());
}

#[test]
fn well_formed_fixture_parses_all_fields() {
    let defs = load_single("reviewer", "reviewer.md").unwrap();
    let def = defs.get("reviewer").expect("reviewer entry present");

    assert_eq!(def.name, "reviewer");
    assert_eq!(def.role, Some(RoleAlias::new("coder")));
    assert_eq!(
        def.tools,
        ToolSelector::Only(vec!["read".to_string(), "grep".to_string()])
    );
    assert_eq!(
        def.model,
        Some(ModelRef {
            backend: BackendId::new("anthropic"),
            model: ModelId::new("claude-sonnet-4-6"),
        })
    );
    assert_eq!(def.max_steps, Some(20));
    assert!(def.result_contract.is_some());
    assert_eq!(def.skills, vec!["review-checklist".to_string()]);
    assert_eq!(
        def.description,
        Some("Reviews diffs for correctness and style.".to_string())
    );

    let expected_prompt = "You are a careful, thorough code reviewer. Read the diff, check for\n\
correctness, security issues, and style violations. Report findings\n\
concisely.";
    assert_eq!(def.system_prompt, expected_prompt);
}

#[test]
fn minimal_fixture_parses_with_only_name() {
    let defs = load_single("minimal", "minimal.md").unwrap();
    let def = defs.get("minimal").expect("minimal entry present");

    assert_eq!(def.name, "minimal");
    assert_eq!(def.description, None);
    assert_eq!(def.role, None);
    assert_eq!(def.tools, ToolSelector::All);
    assert_eq!(def.model, None);
    assert_eq!(def.max_steps, None);
    assert_eq!(def.result_contract, None);
    assert!(def.skills.is_empty());
    assert_eq!(def.system_prompt, "Minimal system prompt.");
}

#[test]
fn no_frontmatter_errors() {
    let result = load_single("no-frontmatter", "no_frontmatter.md");
    let message = expect_agent_def_error(result, "no_frontmatter.md");
    assert!(message.contains("missing YAML frontmatter"), "{message}");
}

#[test]
fn unterminated_frontmatter_errors() {
    let result = load_single("unterminated", "unterminated.md");
    let message = expect_agent_def_error(result, "unterminated.md");
    assert!(message.contains("unterminated frontmatter"), "{message}");
}

#[test]
fn invalid_yaml_error_includes_underlying_text_and_line_number() {
    let result = load_single("bad-yaml", "bad_yaml.md");
    let message = expect_agent_def_error(result, "bad_yaml.md");
    assert!(message.contains("invalid YAML frontmatter"), "{message}");
    assert!(message.contains("line"), "{message}");
}

#[test]
fn missing_name_errors() {
    let result = load_single("missing-name", "missing_name.md");
    let message = expect_agent_def_error(result, "missing_name.md");
    assert!(
        message.contains("missing required field 'name'"),
        "{message}"
    );
}

#[test]
fn name_stem_mismatch_errors_naming_both_values() {
    let result = load_single("name-mismatch", "name_mismatch.md");
    let message = expect_agent_def_error(result, "name_mismatch.md");
    assert!(message.contains("someone_else"), "{message}");
    assert!(message.contains("name_mismatch"), "{message}");
}

#[test]
fn bad_result_contract_errors() {
    let result = load_single("bad-contract", "bad_contract.md");
    let message = expect_agent_def_error(result, "bad_contract.md");
    assert!(message.contains("invalid result_contract"), "{message}");
}

#[test]
fn empty_body_errors() {
    let result = load_single("empty-body", "empty_body.md");
    let message = expect_agent_def_error(result, "empty_body.md");
    assert!(message.contains("empty system prompt"), "{message}");
}

#[test]
fn unknown_frontmatter_key_names_the_key() {
    let dir = scratch_dir("unknown-key");
    fs::write(
        dir.join("bogus.md"),
        "---\nname: bogus\nnope: true\n---\nBody.\n",
    )
    .unwrap();
    let result = load_agent_defs(&dir);
    let message = expect_agent_def_error(result, "bogus.md");
    assert!(message.contains("nope"), "{message}");
}

#[test]
fn non_md_files_and_subdirectories_are_ignored() {
    let dir = scratch_dir("ignored");
    fs::write(dir.join("README.txt"), "not an agent def").unwrap();
    fs::create_dir_all(dir.join("nested")).unwrap();
    fs::write(
        dir.join("nested").join("hidden.md"),
        "---\nname: hidden\n---\nBody.\n",
    )
    .unwrap();

    let defs = load_agent_defs(&dir).unwrap();
    assert!(defs.is_empty());
}

#[test]
fn multiple_valid_files_load_and_key_by_name() {
    let dir = dir_with_fixtures("multi", &["reviewer.md", "minimal.md"]);
    let defs = load_agent_defs(&dir).unwrap();
    assert_eq!(defs.len(), 2);
    assert!(defs.contains_key("reviewer"));
    assert!(defs.contains_key("minimal"));
}

#[test]
fn explicit_empty_tools_list_means_no_tools() {
    let dir = scratch_dir("empty-tools");
    fs::write(
        dir.join("notools.md"),
        "---\nname: notools\ntools: []\n---\nBody.\n",
    )
    .unwrap();
    let defs = load_agent_defs(&dir).unwrap();
    let def = defs.get("notools").unwrap();
    assert_eq!(def.tools, ToolSelector::Only(Vec::new()));
}
