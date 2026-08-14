//! Golden-file and behavioral tests for an earlier item's `ContextBuilder`.
//!
//! Golden comparisons serialize a `GoldenSegment` projection (ordinal,
//! role, provenance, cache_hint, content_sha) that deliberately omits
//! `id` — see `context/builder.rs`'s module doc for why `PromptSegment::id`
//! is unsuitable for byte-equal golden files even though it is now
//! deterministic. Regenerate with `UPDATE_GOLDEN=1 cargo test -p
//! conway-runtime --test context_golden`.

use std::collections::BTreeSet;
use std::sync::Arc;

use conway_core::capabilities::CacheMode;
use conway_core::content::{
    ContentBlock, PermissionClass, Role, StopReason, ToolCategory, ToolResult, ToolSpec, Usage,
};
use conway_core::ids::{AgentId, LogSeq, ModelId, SeqRange, SessionId, ToolName};
use conway_core::log::LogRecord;
use conway_core::provenance::Provenance;
use conway_core::segment::{CacheTtl, PromptSegment};
use conway_runtime::context::{
    ContextBuilder, ContextInput, HeadSegment, InheritedPrefix, SkillFragment, SystemPromptSpec,
};

// ---------------------------------------------------------------------
// Fixed identifiers (never `AgentId::new()`/`SessionId::new()`): these
// values are embedded verbatim inside `Provenance` fields that golden
// files serialize, so they must be stable across runs. Derived from the
// well-known example ULID `01ARZ3NDEKTSV4RRFFQ69G5FAV` by varying only
// trailing Crockford-base32 characters.
// ---------------------------------------------------------------------

fn agent_root() -> AgentId {
    "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap()
}

fn agent_forker() -> AgentId {
    "01ARZ3NDEKTSV4RRFFQ69G5FAW".parse().unwrap()
}

fn agent_fork_child() -> AgentId {
    "01ARZ3NDEKTSV4RRFFQ69G5FAX".parse().unwrap()
}

fn agent_spawn_child() -> AgentId {
    "01ARZ3NDEKTSV4RRFFQ69G5FAY".parse().unwrap()
}

fn agent_steer_from() -> AgentId {
    "01ARZ3NDEKTSV4RRFFQ69G5FAZ".parse().unwrap()
}

fn session_parent() -> SessionId {
    "01ARZ3NDEKTSV4RRFFQ69G5FBV".parse().unwrap()
}

fn ts() -> chrono::DateTime<chrono::Utc> {
    "2026-07-20T00:00:00Z".parse().unwrap()
}

fn sample_tool(name: &str) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(name),
        description: format!("{name} tool"),
        schema: schemars::schema_for!(std::collections::BTreeMap<String, String>),
        category: ToolCategory::Read,
        permission: PermissionClass::Safe,
    }
}

// ---------------------------------------------------------------------
// Fixtures — one per golden case.
// ---------------------------------------------------------------------

fn root_simple_input() -> ContextInput {
    ContextInput {
        agent_id: agent_root(),
        turn: 0,
        model: ModelId::new("claude-sonnet-4-6"),
        cache_mode: CacheMode::None,
        system_prompt: Some(SystemPromptSpec {
            agent_def: "reviewer".into(),
            text: "You are a careful reviewer.".into(),
        }),
        skills: vec![SkillFragment {
            name: "diff-review".into(),
            text: "Review diffs for races.".into(),
        }],
        tools: vec![sample_tool("read"), sample_tool("write")],
        inherited: None,
        head: HeadSegment::Prompt {
            text: "Please review src/lib.rs".into(),
        },
        own: Arc::from(Vec::new()),
        cache_ttl: CacheTtl::FiveMinutes,
    }
}

fn fork_inherited_input() -> ContextInput {
    let records: Vec<LogRecord> = vec![
        LogRecord::UserTurn {
            seq: LogSeq(0),
            ts: ts(),
            text: "Investigate the failing test".into(),
            prov: Provenance::UserPrompt,
        },
        LogRecord::Assistant {
            seq: LogSeq(1),
            ts: ts(),
            content: vec![ContentBlock::Text {
                text: "Looking now.".into(),
            }],
            model: "anthropic/claude-sonnet-4-6".parse().unwrap(),
            route_reason: serde_json::json!({"AliasPrimary": {"alias": "coder"}}),
            usage: Usage::default(),
            stop: StopReason::EndTurn,
        },
        LogRecord::ToolResultRecord {
            seq: LogSeq(2),
            ts: ts(),
            result: ToolResult {
                call_id: "tc_1".into(),
                tool: ToolName::new("read"),
                blocks: vec![ContentBlock::Text {
                    text: "file contents".into(),
                }],
                is_error: false,
                truncated: None,
            },
        },
    ];
    ContextInput {
        agent_id: agent_fork_child(),
        turn: 0,
        model: ModelId::new("claude-sonnet-4-6"),
        cache_mode: CacheMode::ExplicitBreakpoints {
            max_breakpoints: 4,
            ttls: vec![CacheTtl::FiveMinutes],
        },
        system_prompt: Some(SystemPromptSpec {
            agent_def: "reviewer".into(),
            text: "You are a careful reviewer.".into(),
        }),
        skills: vec![SkillFragment {
            name: "diff-review".into(),
            text: "Review diffs for races.".into(),
        }],
        tools: vec![sample_tool("read"), sample_tool("write")],
        inherited: Some(InheritedPrefix {
            from: session_parent(),
            seq_range: SeqRange::new(LogSeq(0), Some(LogSeq(3))),
            records: Arc::from(records),
        }),
        head: HeadSegment::ForkDirective {
            text: "Now review the diff for races".into(),
            by: agent_forker(),
        },
        own: Arc::from(Vec::new()),
        cache_ttl: CacheTtl::FiveMinutes,
    }
}

fn spawn_clean_input() -> ContextInput {
    ContextInput {
        agent_id: agent_spawn_child(),
        turn: 0,
        model: ModelId::new("claude-sonnet-4-6"),
        cache_mode: CacheMode::None,
        system_prompt: Some(SystemPromptSpec {
            agent_def: "triage".into(),
            text: "You triage failing CI jobs.".into(),
        }),
        skills: vec![SkillFragment {
            name: "ci-triage".into(),
            text: "Look at logs first.".into(),
        }],
        tools: vec![sample_tool("read")],
        inherited: None,
        head: HeadSegment::Prompt {
            text: "Diagnose the failing pipeline run #482".into(),
        },
        own: Arc::from(Vec::new()),
        cache_ttl: CacheTtl::FiveMinutes,
    }
}

fn steer_and_toolresults_input() -> ContextInput {
    let own_records = vec![
        LogRecord::Assistant {
            seq: LogSeq(1),
            ts: ts(),
            content: vec![ContentBlock::ToolUse {
                call_id: "tc_2".into(),
                name: ToolName::new("write"),
                arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
            }],
            model: "anthropic/claude-sonnet-4-6".parse().unwrap(),
            route_reason: serde_json::json!({"AliasPrimary": {"alias": "coder"}}),
            usage: Usage::default(),
            stop: StopReason::ToolUse,
        },
        LogRecord::ToolResultRecord {
            seq: LogSeq(2),
            ts: ts(),
            result: ToolResult {
                call_id: "tc_2".into(),
                tool: ToolName::new("write"),
                blocks: vec![ContentBlock::Text {
                    text: "wrote 1 file".into(),
                }],
                is_error: false,
                truncated: None,
            },
        },
        LogRecord::ParentSteer {
            seq: LogSeq(3),
            ts: ts(),
            text: "skip the tests dir".into(),
            from: agent_steer_from(),
            parent_seq: LogSeq(150),
            prov: Provenance::ParentSteer {
                from: agent_steer_from(),
                parent_seq: LogSeq(150),
            },
        },
    ];
    ContextInput {
        agent_id: agent_root(),
        turn: 1,
        model: ModelId::new("claude-sonnet-4-6"),
        cache_mode: CacheMode::ExplicitBreakpoints {
            max_breakpoints: 1,
            ttls: vec![CacheTtl::FiveMinutes],
        },
        system_prompt: Some(SystemPromptSpec {
            agent_def: "reviewer".into(),
            text: "You are a careful reviewer.".into(),
        }),
        skills: vec![SkillFragment {
            name: "diff-review".into(),
            text: "Review diffs for races.".into(),
        }],
        tools: vec![sample_tool("read"), sample_tool("write")],
        inherited: None,
        head: HeadSegment::Prompt {
            text: "Please review src/lib.rs".into(),
        },
        own: Arc::from(own_records),
        cache_ttl: CacheTtl::FiveMinutes,
    }
}

// ---------------------------------------------------------------------
// Golden-file harness
// ---------------------------------------------------------------------

#[derive(serde::Serialize)]
struct GoldenSegment {
    ordinal: usize,
    role: Role,
    provenance: Provenance,
    cache_hint: Option<conway_core::segment::CacheHint>,
    content_sha: String,
}

fn project(segments: &[PromptSegment]) -> Vec<GoldenSegment> {
    segments
        .iter()
        .enumerate()
        .map(|(ordinal, segment)| {
            let content_json = serde_json::to_vec(&segment.content).unwrap();
            GoldenSegment {
                ordinal,
                role: segment.role,
                provenance: segment.provenance.clone(),
                cache_hint: segment.cache_hint.clone(),
                content_sha: blake3::hash(&content_json).to_hex().to_string(),
            }
        })
        .collect()
}

fn assert_golden(name: &str, segments: &[PromptSegment]) {
    let projected = project(segments);
    let json = serde_json::to_string_pretty(&projected).unwrap() + "\n";
    let path = format!("{}/tests/golden/{name}.json", env!("CARGO_MANIFEST_DIR"));

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &json).expect("write golden file");
    }

    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden file {path}; run with UPDATE_GOLDEN=1"));
    assert_eq!(json, expected, "golden mismatch for {name}");
}

fn discriminant(provenance: &Provenance) -> &'static str {
    match provenance {
        Provenance::UserPrompt => "user_prompt",
        Provenance::AgentDef { .. } => "agent_def",
        Provenance::Skill { .. } => "skill",
        Provenance::ToolRegistry { .. } => "tool_registry",
        Provenance::Inherited { .. } => "inherited",
        Provenance::ForkDirective { .. } => "fork_directive",
        Provenance::ParentSteer { .. } => "parent_steer",
        Provenance::ToolResult { .. } => "tool_result",
        Provenance::SystemNote { .. } => "system_note",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------
// Golden tests
// ---------------------------------------------------------------------

#[test]
fn context_root_simple() {
    let input = root_simple_input();
    let (segments, report) = ContextBuilder::new().build(&input).unwrap();

    let order: Vec<&str> = segments
        .iter()
        .map(|s| discriminant(&s.provenance))
        .collect();
    assert_eq!(
        order,
        vec!["agent_def", "skill", "tool_registry", "user_prompt"]
    );
    assert_eq!(report.segments.len(), segments.len());

    assert_golden("context_root_simple", &segments);
}

#[test]
fn context_fork_inherited() {
    let input = fork_inherited_input();
    let (segments, _report) = ContextBuilder::new().build(&input).unwrap();

    let order: Vec<&str> = segments
        .iter()
        .map(|s| discriminant(&s.provenance))
        .collect();
    assert_eq!(
        order,
        vec![
            "agent_def",
            "skill",
            "tool_registry",
            "inherited",
            "inherited",
            "inherited",
            "fork_directive",
        ]
    );

    let inherited_ranges: Vec<SeqRange> = segments
        .iter()
        .filter_map(|s| match &s.provenance {
            Provenance::Inherited { from, seq_range } => {
                assert_eq!(*from, session_parent());
                Some(*seq_range)
            }
            _ => None,
        })
        .collect();
    assert_eq!(inherited_ranges.len(), 3);
    let mut expected_start = LogSeq(0);
    for range in &inherited_ranges {
        assert_eq!(range.start, expected_start);
        expected_start = range.end.unwrap();
    }
    assert_eq!(
        expected_start,
        LogSeq(3),
        "union must cover exactly 0..at_seq"
    );

    assert_golden("context_fork_inherited", &segments);
}

#[test]
fn context_spawn_clean() {
    let input = spawn_clean_input();
    let (segments, _report) = ContextBuilder::new().build(&input).unwrap();

    assert!(
        !segments
            .iter()
            .any(|s| matches!(s.provenance, Provenance::Inherited { .. })),
        "spawn must carry no Inherited segment"
    );
    assert_eq!(segments.len(), 4);
    assert_eq!(segments[3].provenance, Provenance::UserPrompt);

    assert_golden("context_spawn_clean", &segments);
}

#[test]
fn context_with_steer_and_toolresults() {
    let input = steer_and_toolresults_input();
    let (segments, report) = ContextBuilder::new().build(&input).unwrap();

    let order: Vec<&str> = segments
        .iter()
        .map(|s| discriminant(&s.provenance))
        .collect();
    assert_eq!(
        order,
        vec![
            "agent_def",
            "skill",
            "tool_registry",
            "user_prompt",
            // assistant turn — see builder.rs module doc for the
            // documented `system_note` interpretation gap.
            "system_note",
            "tool_result",
            "parent_steer",
        ]
    );
    assert_eq!(report.segments.len(), segments.len());

    assert_golden("context_with_steer_and_toolresults", &segments);
}

// ---------------------------------------------------------------------
// Behavioral tests
// ---------------------------------------------------------------------

#[test]
fn cache_hints_off_for_implicit_prefix_and_none() {
    for cache_mode in [
        CacheMode::None,
        CacheMode::ImplicitPrefix {
            min_prefix_tokens: 256,
        },
    ] {
        let mut input = fork_inherited_input();
        input.cache_mode = cache_mode;
        let (segments, _) = ContextBuilder::new().build(&input).unwrap();
        assert!(segments.iter().all(|s| s.cache_hint.is_none()));
    }
}

#[test]
fn cache_hint_placement_with_inherited_prefix_marks_a_and_b() {
    let input = fork_inherited_input(); // ExplicitBreakpoints { max_breakpoints: 4, .. }
    let (segments, _) = ContextBuilder::new().build(&input).unwrap();

    let a = segments
        .iter()
        .rposition(|s| matches!(s.provenance, Provenance::ToolRegistry { .. }))
        .unwrap();
    let b = segments
        .iter()
        .rposition(|s| matches!(s.provenance, Provenance::Inherited { .. }))
        .unwrap();

    let marked: BTreeSet<usize> = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.cache_hint.as_ref().is_some_and(|h| h.breakpoint))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(marked, BTreeSet::from([a, b]));
}

#[test]
fn cache_hint_placement_without_inherited_prefix_marks_only_a() {
    let mut input = root_simple_input();
    input.cache_mode = CacheMode::ExplicitBreakpoints {
        max_breakpoints: 4,
        ttls: vec![CacheTtl::FiveMinutes],
    };
    let (segments, _) = ContextBuilder::new().build(&input).unwrap();

    let a = segments
        .iter()
        .rposition(|s| matches!(s.provenance, Provenance::ToolRegistry { .. }))
        .unwrap();
    let marked: BTreeSet<usize> = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.cache_hint.as_ref().is_some_and(|h| h.breakpoint))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(marked, BTreeSet::from([a]));
}

#[test]
fn breakpoint_trim_keeps_b_over_a() {
    let mut input = fork_inherited_input();
    input.cache_mode = CacheMode::ExplicitBreakpoints {
        max_breakpoints: 1,
        ttls: vec![CacheTtl::FiveMinutes],
    };
    let (segments, _) = ContextBuilder::new().build(&input).unwrap();

    let b = segments
        .iter()
        .rposition(|s| matches!(s.provenance, Provenance::Inherited { .. }))
        .unwrap();
    let marked: BTreeSet<usize> = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.cache_hint.as_ref().is_some_and(|h| h.breakpoint))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        marked,
        BTreeSet::from([b]),
        "only breakpoint B should survive trimming to 1"
    );
}

fn identity_tuples(
    segments: &[PromptSegment],
) -> Vec<(
    conway_core::ids::SegmentId,
    Role,
    Vec<ContentBlock>,
    Provenance,
)> {
    segments
        .iter()
        .map(|s| (s.id, s.role, s.content.clone(), s.provenance.clone()))
        .collect()
}

#[test]
fn cache_neutrality_holds_for_every_golden_case() {
    for mut input in [
        root_simple_input(),
        fork_inherited_input(),
        spawn_clean_input(),
        steer_and_toolresults_input(),
    ] {
        let (mut with_cache, _) = ContextBuilder::new().build(&input).unwrap();
        conway_core::segment::strip_cache_hints(&mut with_cache);

        input.cache_mode = CacheMode::None;
        let (without_cache, _) = ContextBuilder::new().build(&input).unwrap();

        assert_eq!(
            identity_tuples(&with_cache),
            identity_tuples(&without_cache),
            "stripping hints must equal building with caching disabled"
        );
    }
}

#[test]
fn prefix_key_stable_across_siblings_and_sensitive_to_model() {
    let base = fork_inherited_input();
    let (segments_a, _) = ContextBuilder::new().build(&base).unwrap();
    let key_a = conway_runtime::context::prefix_key(&base.model, &segments_a);

    // A "sibling": different agent identity and different post-boundary
    // content (the fork directive text), same static+inherited prefix.
    let mut sibling = base.clone();
    sibling.agent_id = agent_forker();
    sibling.head = HeadSegment::ForkDirective {
        text: "a completely different directive".into(),
        by: agent_forker(),
    };
    let (segments_b, _) = ContextBuilder::new().build(&sibling).unwrap();
    let key_b = conway_runtime::context::prefix_key(&sibling.model, &segments_b);
    assert_eq!(
        key_a, key_b,
        "siblings differing only after B share a PrefixKey"
    );

    let mut different_model = base.clone();
    different_model.model = ModelId::new("claude-haiku-4-5");
    let (segments_c, _) = ContextBuilder::new().build(&different_model).unwrap();
    let key_c = conway_runtime::context::prefix_key(&different_model.model, &segments_c);
    assert_ne!(
        key_a, key_c,
        "a differing model_id must change the PrefixKey"
    );
}

#[test]
fn context_report_matches_segments_and_uses_heuristic_estimator() {
    let input = steer_and_toolresults_input();
    let (segments, report) = ContextBuilder::new().build(&input).unwrap();

    assert_eq!(report.segments.len(), segments.len());
    for (entry, segment) in report.segments.iter().zip(segments.iter()) {
        assert_eq!(entry.segment, segment.id);
        assert_eq!(&entry.provenance, &segment.provenance);
        assert_eq!(entry.tokens_est, segment.tokens_est.unwrap());
        assert!(entry.estimated);
    }
    assert_eq!(report.tokenizer, conway_runtime::context::TOKEN_ESTIMATOR);
    assert_eq!(
        report.total_tokens_est,
        report.segments.iter().map(|e| e.tokens_est).sum::<u32>()
    );
    assert_eq!(report.agent_id, input.agent_id);
    assert_eq!(report.turn, input.turn);
}
