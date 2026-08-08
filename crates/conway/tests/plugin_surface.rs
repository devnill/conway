//! F8's liveness test (GP-14): the facade's `conway::plugin` surface is
//! *implementable* from outside the workspace, not merely nameable.
//!
//! Everything below is written the way a third-party Rust plugin author
//! would write it: `use conway::plugin::...` for the extension surface,
//! `use conway::...` for the builder/config types, and the author's own
//! `schemars`/`serde`/`serde_json` dependencies for the data types those
//! public signatures name (`ToolSpec::schema`, `ToolCall::arguments`).
//!
//! **This file must never import `conway_core`.** That is the break-the-
//! guard property, built in: if the curated export set in
//! `crates/conway/src/lib.rs`'s `pub mod plugin` is missing anything an
//! implementor of `Tool`/`Plugin`/`ContextHook` needs, this file fails to
//! COMPILE — the test cannot silently pass against a shrunken surface. The
//! negative direction is verified by hand each time this surface changes:
//! remove one re-export from `pub mod plugin` and confirm this file stops
//! compiling.
//!
//! `serde`/`serde_json`/`schemars` are the facade's own declared
//! dependencies (available to every integration test in this crate), which
//! mirrors the third-party shape honestly: those crates are part of the
//! public signatures, and a real plugin crate names them in its own
//! `Cargo.toml` — they are not what F8 curates.

// Only used by `facade_only_config` and its sole caller, both gated on
// `jsonl-store` (see that test's own doc).
#[cfg(feature = "jsonl-store")]
use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg(feature = "jsonl-store")]
use conway::config::schema::{
    AgentsConfig, BackendEntry, BackendKind, ConwayConfig, HealthSection, LimitsConfig,
    ModelsConfig, PermissionMode, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig, TuiSection,
};
use conway::plugin::{
    async_trait, Artifact, ArtifactKind, ArtifactWriteError, ArtifactWriteHandle, ArtifactWriter,
    CancellationToken, ContentBlock, ContextHook, ContextHookCtx, ContextPayload, CwdError, Fact,
    OverflowInfo, PathArgs, PermissionClass, Plugin, PluginConfig, PluginManifest, PromptSegment,
    Provenance, RenderKind, Role, SubagentError, Tool, ToolCall, ToolCategory, ToolCtx, ToolError,
    ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};
use conway::{AgentId, SessionId};
// Only used by the `jsonl-store`-gated test below; see that test's own doc.
#[cfg(feature = "jsonl-store")]
use conway::{ConwayBuilder, RoleAlias};

// ---------------------------------------------------------------------------
// A trivial third-party tool, written against `conway::plugin` alone.
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct EchoArgs {
    text: String,
}

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("echo"),
            description: "Echoes its 'text' argument back.".to_string(),
            schema: schemars::schema_for!(EchoArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        // `ctx`'s fields are exercised the way a real tool uses them:
        // method calls on the capability handles never name their types
        // (`CwdHandle`/`EventSinkHandle`/`SubagentHost` stay unexported on
        // purpose -- see `lib.rs`'s `pub mod plugin` doc). `PluginConfig`
        // and `CancellationToken` ARE exported, and are named here on
        // purpose: every re-export must appear in this file's text, or
        // dropping it from `pub mod plugin` would keep this file compiling
        // and the compile guard would be false for it.
        let _config: &Arc<PluginConfig> = &ctx.config;
        let _cancel: &CancellationToken = &ctx.cancel;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let _cwd = &ctx.cwd;
        let args: EchoArgs = serde_json::from_value(call.arguments)
            .map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text { text: args.text }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: vec![],
        })
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    fn render_kind(&self) -> RenderKind {
        // Truthful because `render` keeps the trait's default debug-dump
        // shape -- the same claim `conway-tools`' own generic guard checks.
        RenderKind::Structured
    }
}

// ---------------------------------------------------------------------------
// A trivial third-party plugin providing that tool.
// ---------------------------------------------------------------------------

struct EchoPlugin;

impl Plugin for EchoPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "test.echo".to_string(),
            version: "0.1.0".to_string(),
            tools: vec![ToolName::new("echo")],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(EchoTool)]
    }
}

// ---------------------------------------------------------------------------
// A third-party context hook (WI-126's extension point): masks segments
// carrying a marker and appends a hook-authored segment -- exercising
// `PromptSegment`/`Role`/`Provenance` construction, the thing a masking or
// system-prompt-instrumenting hook actually does.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// A third-party `ArtifactWriter` double (board item 01KZ84437RMKHP5DJX7RMHH7JY):
// an EMBEDDER-supplied capability (like `PermissionGate`/`Router`, not a
// hook-authored one -- a hook only ever RECEIVES `ContextHookCtx::artifacts`,
// already wired to the runtime's real, containment-checked writer). This
// double just proves the trait and its handle are nameable/implementable
// from a facade-only dependent, mirroring every other extension point in
// this file; the REAL containment guarantee lives in
// `conway_runtime::artifact_store`'s own tests, against the real writer.
// ---------------------------------------------------------------------------

struct RecordingArtifactWriter {
    last_write: std::sync::Mutex<Option<(String, Vec<u8>)>>,
}

#[async_trait]
impl ArtifactWriter for RecordingArtifactWriter {
    async fn write(
        &self,
        _agent_id: AgentId,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<std::path::PathBuf, ArtifactWriteError> {
        *self.last_write.lock().unwrap() = Some((name.to_string(), bytes));
        Ok(std::path::PathBuf::from(name))
    }
}

struct MarkerHook;

#[async_trait]
impl ContextHook for MarkerHook {
    async fn before_request(&self, ctx: &ContextHookCtx, mut payload: ContextPayload) -> ContextPayload {
        let _ = (ctx.turn, ctx.estimated_tokens);
        payload.segments.retain(|segment| {
            !segment.content.iter().any(|block| match block {
                ContentBlock::Text { text } => text.contains("MASK-ME"),
                _ => false,
            })
        });
        payload.segments.push(PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: "hook was here".to_string(),
            }],
            Provenance::SystemNote {
                reason: "test hook instrumentation".to_string(),
            },
        ));
        payload
    }

    async fn on_overflow(
        &self,
        _ctx: &ContextHookCtx,
        payload: ContextPayload,
        overflow: OverflowInfo,
    ) -> Option<ContextPayload> {
        let _ = overflow.shortfall_tokens;
        Some(payload)
    }
}

// ---------------------------------------------------------------------------
// Registration through `ConwayBuilder`, end to end, with no `conway_core`
// test double anywhere: a real config-file backend (never contacted --
// `build()` does no network I/O), the default JSONL store pointed at a
// tempdir, the built-in deny gate, and the facade-compiled router.
// ---------------------------------------------------------------------------

// Only used by the `jsonl-store`-gated test below; see that test's own doc.
#[cfg(feature = "jsonl-store")]
fn facade_only_config(session_root: std::path::PathBuf, metadata_path: std::path::PathBuf) -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        RoleEntry {
            chain: vec!["anthropic/claude-sonnet-4-6".to_string()],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    let mut backends = BTreeMap::new();
    backends.insert(
        "anthropic".to_string(),
        BackendEntry {
            kind: BackendKind::Anthropic,
            api_key: "sk-ant-api03-not-a-real-key".to_string(),
            ..BackendEntry::default()
        },
    );
    let permissions = PermissionsConfig {
        mode: PermissionMode::Deny,
        ..PermissionsConfig::default()
    };
    ConwayConfig {
        default_role: RoleAlias::new("coder"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig {
            root: session_root,
            ..SessionConfig::default()
        },
        limits: LimitsConfig::default(),
        permissions,
        backends,
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig {
            metadata_path,
            probe_on_startup: false,
        },
        tui: TuiSection::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
    }
}

/// The acceptance criterion: a `Tool`, a `Plugin`, and a `ContextHook`
/// implemented with no `conway_core` import, registered through
/// `ConwayBuilder::with_plugin`/`with_context_hook`, and `build()` --
/// which runs the plugin-registration and duplicate-manifest-id paths for
/// real -- succeeds.
///
/// Gated on `jsonl-store`: this test never calls `with_session_store`, so
/// `build()` synthesizes the default `JsonlSessionStore` pointed at `dir`
/// (`facade_only_config`'s own doc, above) -- without that feature
/// `build_default_store` is `Err(ConwayError::Build)` and the `.expect`
/// below panics. **Not** gated on any backend feature: `construct_backend`
/// has no cfg-gated path left to fail on for `kind = "anthropic"` (board
/// item: retire the backend compile-time feature flags) -- this test used
/// to carry a now-removed `#[cfg(feature = "anthropic")]` for exactly that
/// reason, which masked the real (`jsonl-store`) dependency; corrected as
/// part of that item's own verification pass, which found this test failed
/// under `--no-default-features`/`builtin-tools`-only for the true reason
/// above, not the stale one.
#[cfg(feature = "jsonl-store")]
#[test]
fn plugin_tool_and_hook_register_through_the_builder() {
    let dir = tempfile::tempdir().expect("tempdir");
    let metadata_path = dir.path().join("models.json");
    std::fs::write(
        &metadata_path,
        r#"{"models":{"anthropic/claude-sonnet-4-6":{"max_context_tokens":200000,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}}}"#,
    )
    .expect("write models.json fixture");
    let config = facade_only_config(dir.path().join("sessions"), metadata_path);

    ConwayBuilder::from_parts(config)
        .with_plugin(Arc::new(EchoPlugin))
        .with_context_hook(Arc::new(MarkerHook))
        .build()
        .expect("a facade-only plugin/tool/hook must register and build");
}

/// The authored objects behave as declared, exercised directly: the
/// plugin's manifest agrees with its tool's spec, and the tool's static
/// declarations are the ones a permission broker would read.
#[test]
fn authored_plugin_and_tool_are_self_consistent() {
    let plugin = EchoPlugin;
    let manifest = plugin.manifest();
    let tools = plugin.tools();
    assert_eq!(tools.len(), 1);
    let spec = tools[0].spec();
    assert_eq!(manifest.tools, vec![spec.name.clone()]);
    assert_eq!(spec.category, ToolCategory::Read);
    assert_eq!(spec.permission, PermissionClass::Safe);
    assert_eq!(tools[0].path_args(), PathArgs::None);
    assert_eq!(tools[0].render_kind(), RenderKind::Structured);
    // The trait's default `render` is available through the facade surface.
    assert_eq!(
        tools[0].render(&serde_json::json!({"text": "hi"})),
        "echo({\"text\":\"hi\"})"
    );
    // `Artifact`/`ArtifactKind` are part of the authoring surface (a tool
    // emitting a file artifact constructs them into `ToolOutput.artifacts`)
    // and must be NAMED here, for the same reason as the handle types in
    // `invoke`: an export this file never names is an export the compile
    // guard does not cover.
    let output = ToolOutput {
        blocks: vec![],
        is_error: false,
        truncation: TruncationPolicy::None,
        artifacts: vec![Artifact {
            id: "out-1".to_string(),
            kind: ArtifactKind::File,
            path: None,
            media_type: None,
            bytes: None,
            label: "example".to_string(),
        }],
    };
    assert_eq!(output.artifacts.len(), 1);
    assert_eq!(output.artifacts[0].kind, ArtifactKind::File);
}

/// C3: `Fact`, `CwdError`, and `SubagentError` are constructible/matchable
/// from a facade-only dependent, the same "named, not just re-exported"
/// property every other type in this file proves. `Fact` is the report
/// tool's own typed-fact output shape (already half-reachable via
/// `AgentResult.facts: Vec<Fact>` before this item, but not nameable to
/// declare a local variable of the type); `CwdError`/`SubagentError` are
/// the two `ToolCtx` capability-handle error types (`ctx.chdir`/
/// `ctx.subagents`) a tool's `invoke` needs to match on to turn a handle
/// failure into a `ToolError` without going through `conway_core` directly.
#[test]
fn fact_and_capability_handle_errors_are_constructible_and_matchable() {
    let fact = Fact {
        key: "reviewed_files".to_string(),
        value: serde_json::json!(["a.rs", "b.rs"]),
        source: Some("review-tool".to_string()),
    };
    assert_eq!(fact.key, "reviewed_files");
    assert_eq!(fact.source.as_deref(), Some("review-tool"));

    let cwd_err = CwdError::Poisoned;
    assert!(matches!(cwd_err, CwdError::Poisoned));
    assert_eq!(
        cwd_err.to_string(),
        "cwd handle's lock was poisoned by a panic in a prior `set` call"
    );

    let agent = AgentId::new();
    let subagent_err = SubagentError::UnknownAgent { agent };
    let mapped: ToolError = subagent_err.into();
    assert!(
        matches!(mapped, ToolError::InvalidArguments { .. }),
        "SubagentError::UnknownAgent is a model-correctable mistake, not host infrastructure"
    );
}

/// The hook surface is drivable, not just nameable: `ContextPayload`,
/// `ContextHookCtx`, and `OverflowInfo` are all constructible from facade
/// paths, and both hook methods run their transforms for real.
#[tokio::test]
async fn authored_hook_transforms_payloads() {
    let hook = MarkerHook;
    let writer = Arc::new(RecordingArtifactWriter {
        last_write: std::sync::Mutex::new(None),
    });
    let ctx = ContextHookCtx {
        agent_id: AgentId::new(),
        session_id: SessionId::new(),
        turn: 0,
        model: None,
        estimated_tokens: 42,
        artifacts: ArtifactWriteHandle::new(writer.clone(), AgentId::new()),
    };
    let payload = ContextPayload {
        segments: vec![
            PromptSegment::new(
                Role::User,
                vec![ContentBlock::Text {
                    text: "keep me".to_string(),
                }],
                Provenance::UserPrompt,
            ),
            PromptSegment::new(
                Role::User,
                vec![ContentBlock::Text {
                    text: "MASK-ME".to_string(),
                }],
                Provenance::UserPrompt,
            ),
        ],
        tools: vec![],
    };

    let out = hook.before_request(&ctx, payload).await;
    assert_eq!(out.segments.len(), 2, "masked segment dropped, hook segment appended");
    assert!(matches!(out.segments[1].provenance, Provenance::SystemNote { .. }));

    let retry = hook
        .on_overflow(
            &ctx,
            ContextPayload::default(),
            OverflowInfo {
                max_context_tokens: 100,
                headroom_tokens: 10,
                required_tokens: 200,
                shortfall_tokens: 100,
            },
        )
        .await;
    assert!(retry.is_some());

    // C-family liveness: `ContextHookCtx::artifacts` is drivable, not just
    // nameable -- a hook can actually call `.write()` through the facade
    // type and observe the write reach the underlying `ArtifactWriter`.
    let written = ctx.artifacts.write("spill.txt", b"overflow content".to_vec()).await;
    assert_eq!(written.unwrap(), std::path::PathBuf::from("spill.txt"));
    assert_eq!(
        writer.last_write.lock().unwrap().as_ref(),
        Some(&("spill.txt".to_string(), b"overflow content".to_vec()))
    );
}
