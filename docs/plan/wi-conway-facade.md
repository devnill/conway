## Size Assessment

**Right size (8 work items).** The facade has five separable concerns (crate/feature skeleton, config loading, gates+presets, agent-def loading, builder/assembly) plus the handle surface split across three delivery groups (Slice 1 core, Group 2 subagent, Group 3 resume). No sub-module split is warranted: all items share one crate-level public API and no internal interfaces exist between them beyond `ConwayConfig` and `Arc<Runtime>`.

Assumptions stated where the module spec is silent are marked `ASSUMPTION` in Implementation Notes.

---

# WI-096: conway crate skeleton, feature flags, error type, and public re-export surface

**complexity:** Medium

## scope
- `crates/conway/Cargo.toml` (create)
- `crates/conway/src/lib.rs` (create)
- `crates/conway/src/error.rs` (create)
- `crates/conway/tests/public_api_surface.rs` (create)
- `.github/workflows/feature-matrix.yml` (create)

## depends
- MODULE:conway-core (types + ports)
- MODULE:conway-runtime (`Runtime`, `RuntimeDeps`)
- MODULE:conway-backends (`AnthropicBackend`, `OpenAiCompatBackend`)
- MODULE:conway-session (`JsonlSessionStore`)
- MODULE:conway-routing (`DeclarativeRouter`, `BreakerRegistry`)
- MODULE:conway-tools (`builtin_plugins`)

## criteria
- [machine] `crates/conway/Cargo.toml` declares `[features]` with `default = ["anthropic", "openai-compat", "builtin-tools", "jsonl-store"]`.
- [machine] Each of the four default features maps to exactly one optional dependency: `anthropic` → `conway-backends/anthropic`, `openai-compat` → `conway-backends/openai-compat`, `builtin-tools` → `dep:conway-tools`, `jsonl-store` → `dep:conway-session`.
- [machine] Feature-matrix build passes for all of: `--no-default-features`; `--no-default-features --features anthropic`; `--no-default-features --features openai-compat`; `--no-default-features --features builtin-tools,jsonl-store`; `--all-features`; default. Each invocation is `cargo check -p conway` and must exit 0.
- [machine] `crates/conway/src/lib.rs` re-exports from `conway_core`: `AgentId`, `SessionId`, `LogRecord`, `LogSeq`, `SessionMeta`, `SessionFilter`, `AgentResult`, `ResultStatus`, `Event`, `Envelope`, `Budget`, `AgentDef`, `RoleAlias`, `ModelRef`, `ContextReport`, `AgentTreeSnapshot`, `Provenance`, and the port traits `Backend`, `Plugin`, `Tool`, `PermissionGate`, `SessionStore`, `Router`, `HealthRegistry`.
- [machine] `crates/conway/tests/public_api_surface.rs` contains a test that names every re-exported item in a `use conway::{...};` statement and fails to compile if any is removed.
- [machine] No `pub use conway_runtime::` statement anywhere in `crates/conway/src/**` except `pub use conway_runtime::{ContextReport, AgentTreeSnapshot, EventStream}` if and only if those types originate in `conway-runtime` rather than `conway-core`; verified by a `grep`-based test asserting the count of `pub use conway_runtime::` lines is ≤ 1.
- [machine] `ConwayError` in `error.rs` derives `thiserror::Error` and `Debug`, is `#[non_exhaustive]`, and has variants `Config{path: Option<PathBuf>, message: String}`, `Io(#[from] std::io::Error)`, `Backend(#[from] conway_core::BackendError)`, `Store(#[from] conway_core::StoreError)`, `Routing(#[from] conway_core::RoutingError)`, `Runtime(#[from] conway_core::RuntimeError)`, `AgentDef{path: PathBuf, message: String}`, `Build{message: String}`, `UnsupportedFeature{feature: &'static str, message: String}`.
- [machine] `pub type Result<T> = std::result::Result<T, ConwayError>;` is exported from the crate root.
- [machine] `cargo clippy -p conway --all-features -- -D warnings` exits 0.

## notes
**Objective:** Establish the `conway` crate as the single stable public API surface: dependency wiring, feature flags, the crate-level error type, and the curated re-export list. Everything downstream in this module builds on this skeleton.

**Implementation Notes:**
- `Cargo.toml` dependencies: `conway-core` (required), `conway-runtime` (required), `conway-backends` (optional, `default-features = false`), `conway-session` (optional), `conway-routing` (required), `conway-tools` (optional), plus `serde`, `serde_json`, `toml`, `serde_yaml`, `thiserror`, `directories` (XDG resolution), `tokio` (features `rt`, `sync`), `async-trait`, `futures-core`.
- `openai-compat`/`anthropic` features must both enable the shared `dep:conway-backends` and the corresponding backend sub-feature. Use `anthropic = ["dep:conway-backends", "conway-backends/anthropic"]`.
- With `--no-default-features`, the crate must still compile and expose `ConwayBuilder` with `with_backend`/`with_session_store` — an embedder supplies its own adapters. Any code path that names a concrete adapter must be behind `#[cfg(feature = ...)]`.
- `UnsupportedFeature` is returned when config names a backend kind whose feature is disabled at compile time. Message format: `"backend kind '{kind}' requires the '{feature}' cargo feature, which was not enabled at build time"`.
- Module declarations in `lib.rs`: `pub mod config; pub mod agents; pub mod gates; pub mod presets; mod builder; mod conway; mod session_handle; mod event_stream; mod error;` with `pub use builder::ConwayBuilder; pub use conway::Conway; pub use session_handle::{SessionHandle, SessionSpec, TurnHandle}; pub use event_stream::EventStream; pub use error::{ConwayError, Result};`. Files owned by other work items are created there; this item creates `lib.rs` with the module declarations and empty/stub module files are NOT created here — instead `lib.rs` declares them and the item's acceptance is checked with all sibling items' files present (CI matrix runs after the module is complete). To keep this item independently compilable, `lib.rs` initially declares only `mod error;` plus the re-export block, and each sibling item adds its own `mod` line to `lib.rs`.
- ASSUMPTION: `lib.rs` is a shared file. To avoid concurrent overlap, every sibling item that adds a `mod` declaration to `lib.rs` declares `crates/conway/src/lib.rs` (modify) and depends on WI-096. Ordering among siblings is enforced by their own dependency edges where they touch it; see each item's `depends`.
- The feature matrix workflow runs the six `cargo check` invocations listed in criteria as a matrix job.

---

# WI-097: Config schema, discovery, precedence merge, and OAuth-token rejection

**complexity:** High

## scope
- `crates/conway/src/config/mod.rs` (create)
- `crates/conway/src/config/schema.rs` (create)
- `crates/conway/src/config/discovery.rs` (create)
- `crates/conway/src/config/merge.rs` (create)
- `crates/conway/src/config/model_metadata.rs` (create)
- `crates/conway/tests/config_precedence.rs` (create)
- `crates/conway/tests/config_oauth_rejection.rs` (create)
- `crates/conway/tests/fixtures/config/` (create)

## depends
- WI-096
- MODULE:conway-core (`RoutingConfig`, `BackendConfig`, `Budget`, `ModelRef`)

## criteria
- [machine] `config::ConwayConfig` deserializes the full TOML schema in Implementation Notes without error; a round-trip test (`toml::from_str` → `toml::to_string` → `toml::from_str`) produces an equal value.
- [machine] `config::load(LoadOptions) -> Result<ConwayConfig>` exists and performs no network I/O; a test asserts this by running `load` in an environment where all outbound sockets are unavailable (`LoadOptions.model_metadata_refresh = false` is the only supported default) — verified structurally by a test asserting `crates/conway/src/config/` contains no `reqwest`/`hyper`/`TcpStream` identifier (grep-based assertion).
- [machine] Precedence test: with a default value D, an XDG file value X, a project file value P, an env var E, and a CLI override C set for the same key, `load` returns C. Removing sources one at a time in that order yields E, then P, then X, then D. Test covers at least these keys: `default_role`, `backends.<id>.base_url`, `limits.max_steps`, `permissions.mode`.
- [machine] Env var mapping test: `CONWAY_DEFAULT_ROLE`, `CONWAY_BACKENDS__ANTHROPIC__API_KEY`, `CONWAY_LIMITS__MAX_STEPS` are read and mapped to the corresponding fields; unknown `CONWAY_*` vars are ignored without error.
- [machine] Discovery test: `discover()` returns project config from `<cwd>/.conway/conway.toml`, and walks parent directories up to the filesystem root, taking the nearest match; a test with a nested temp dir asserts the nearest ancestor wins.
- [machine] Discovery test: when `XDG_CONFIG_HOME` is set, `$XDG_CONFIG_HOME/conway/conway.toml` is read; when unset, the platform default from `directories::ProjectDirs` is used.
- [machine] OAuth rejection: parsing a config whose `backends.<id>.api_key` (or the resolved value of `api_key_env`) begins with `sk-ant-oat` returns `Err(ConwayError::Config{..})` whose message contains all of: `"sk-ant-oat"`, `"Anthropic subscription OAuth"`, `"Terms of Service"`, `"not supported"`. Test asserts each substring.
- [machine] OAuth rejection applies regardless of backend kind and regardless of whether the value came from file, env, or CLI override — three separate test cases.
- [machine] Config referencing an unknown role alias in `default_role` returns `Err(ConwayError::Config)` naming the unknown alias and listing the defined aliases.
- [machine] Unknown TOML keys are rejected: `ConwayConfig` and all nested structs use `#[serde(deny_unknown_fields)]`; a test with a typo'd key (`max_step`) returns an error containing `max_step`.
- [machine] `model_metadata::load(path) -> Result<ModelMetadata>` reads a local JSON file and returns `Ok(ModelMetadata::empty())` when the file does not exist (missing metadata is not an error).
- [machine] `model_metadata::refresh(url, dest) -> Result<()>` exists, is gated behind `#[cfg(feature = "metadata-refresh")]`, and is never called from `config::load` (grep-based assertion).

## notes
**Objective:** Implement configuration as a pure, network-free, deterministic function of five ordered sources, with fail-loud validation and mandatory rejection of Anthropic subscription OAuth tokens.

**Implementation Notes:**

TOML schema (`ConwayConfig`) — this is the binding shape:

```toml
default_role = "coder"           # RoleAlias, must exist in [roles]
cwd = "."                        # optional, PathBuf

[session]
root = ".conway/sessions"        # PathBuf
fsync = "interval"               # "always" | "interval" | "never"
fsync_interval_ms = 200          # u64

[limits]
max_steps = 40                   # u32
max_tokens = 0                   # u32, 0 = unlimited
deadline_secs = 0                # u64, 0 = none
max_parallel_tools = 4           # u32

[permissions]
mode = "prompt"                  # "prompt" | "allowlist" | "deny"
allowed_tools = []               # Vec<String>, used when mode = "allowlist"
denied_tools  = []               # Vec<String>

[backends.anthropic]
kind = "anthropic"               # "anthropic" | "openai-compat"
api_key = ""                     # optional; mutually exclusive with api_key_env
api_key_env = "ANTHROPIC_API_KEY"
base_url = ""                    # optional override

[backends.local]
kind = "openai-compat"
dialect = "ollama"               # "openai" | "ollama" | "vllm-hermes" | "lm-studio" | "llamacpp-server"
base_url = "http://localhost:11434/v1"
api_key_env = ""
stream_tools = false             # optional bool, per-backend default

[roles.coder]
chain = ["local/qwen3-coder-80b", "anthropic/claude-sonnet-4-6"]

[health]
transport_failures_to_open = 3
open_duration = "30s"
probe_interval = "15s"
probe_timeout = "2s"

[agents]
dir = ".conway/agents"           # PathBuf

[models]
metadata_path = ".conway/models.json"   # local file, never fetched implicitly
```

- `[roles]` and `[health]` deserialize directly into `conway_core::RoutingConfig`. Do not duplicate the types.
- `LoadOptions { cwd: PathBuf, explicit_path: Option<PathBuf>, env: HashMap<String,String>, cli_overrides: CliOverrides }`. Tests inject `env` rather than mutating process env — this makes precedence tests parallel-safe. `load()` with a default `LoadOptions` reads `std::env::vars()`.
- `CliOverrides` is a struct of `Option<T>` fields mirroring the subset of keys the CLI exposes: `default_role`, `model` (a `ModelRef` pin), `permission_mode`, `allowed_tools`, `denied_tools`, `max_steps`, `session_root`, `cwd`. It is defined here (not in `conway-cli`) so the library is the source of truth (C-03).
- Merge is field-wise and shallow-per-leaf: a lower-precedence source's value survives unless a higher-precedence source sets that exact leaf. Tables merge by key union (`[backends.x]` in XDG and `[backends.y]` in project yields both). Arrays replace wholesale (do not concatenate `chain` or `allowed_tools`).
- Env var naming: `CONWAY_` prefix, `__` (double underscore) as the table separator, single `_` preserved within a key, uppercase. `CONWAY_BACKENDS__LOCAL__BASE_URL` → `backends.local.base_url`. Values are parsed as TOML scalars; on parse failure, treat as a string. Arrays via env use comma separation.
- OAuth check runs in `merge::validate` after the final merged value is computed and after `api_key_env` resolution, so a token injected via env is also caught. Check is `value.starts_with("sk-ant-oat")` (covers `sk-ant-oat01-...` and future suffixes). Error message template: `"Anthropic subscription OAuth tokens (sk-ant-oat*) are not supported: using a Claude subscription token through a third-party harness is prohibited by Anthropic's Terms of Service and has been technically blocked since February 2026. Use an API key (sk-ant-api*) from console.anthropic.com instead. (backend: {backend_id}, source: {source})"`.
- Validation order in `merge::validate`, fail on the first error: (1) OAuth token rejection; (2) `default_role` exists in `[roles]`; (3) every `ModelRef` in every `chain` has the form `<backend_id>/<model>` and `<backend_id>` exists in `[backends]`; (4) `permissions.mode = "allowlist"` requires non-empty `allowed_tools`; (5) `fsync = "interval"` requires `fsync_interval_ms > 0`; (6) `api_key` and `api_key_env` are not both non-empty for the same backend.
- `ModelMetadata` is `{ models: HashMap<String, ModelMetadataEntry> }` with `ModelMetadataEntry { max_context_tokens: u32, tool_calling: String, reasoning: bool, reliability_tier: String }`. It is passed to `conway-backends`' `CapabilityProbe` by the builder (WI-100).
- Fixtures live under `tests/fixtures/config/` as `xdg.toml`, `project.toml`, `oauth_token.toml`, `unknown_key.toml`, `bad_role.toml`.

---

# WI-098: Built-in permission gates and preset plugin/backend registration

**complexity:** Low

## scope
- `crates/conway/src/gates.rs` (create)
- `crates/conway/src/presets.rs` (create)
- `crates/conway/tests/gates.rs` (create)

## depends
- WI-096
- WI-097 (consumes `config::PermissionsConfig`)
- MODULE:conway-core (`PermissionGate`, `PermissionRequest`, `PermissionDecision`)
- MODULE:conway-tools (`builtin_plugins`)

## criteria
- [machine] `gates::AllowListGate::new(allowed: Vec<String>, denied: Vec<String>) -> AllowListGate` exists and implements `conway_core::PermissionGate`.
- [machine] `AllowListGate` returns `AllowOnce` for a tool name present in `allowed` and absent from `denied`.
- [machine] `AllowListGate` returns `DenyWithFeedback{message}` for a tool absent from `allowed`; the message contains the tool name and the literal text `"not in the allow list"`.
- [machine] `AllowListGate` returns `DenyWithFeedback` for a tool present in both `allowed` and `denied` — deny wins. Test asserts this precedence explicitly.
- [machine] `AllowListGate` supports the glob form `tool_name(arg_pattern)` and bare `tool_name`: entry `bash(git *)` allows a `bash` call whose `arguments["command"]` matches the glob `git *`, and denies `rm -rf /`. Test covers a match, a non-match, and an entry without parentheses matching any arguments.
- [machine] `gates::DenyAllGate` implements `PermissionGate` and always returns `Deny{reason}` where reason is the constant `"all tool use is denied by DenyAllGate"`.
- [machine] `gates::PromptingGate::new(handler)` where `handler: Arc<dyn Fn(PermissionRequest) -> BoxFuture<'static, PermissionDecision> + Send + Sync>` implements `PermissionGate` by delegating to the handler unchanged; a test with a handler returning `AllowAlways{scope: Session}` asserts the identical decision is returned.
- [machine] `gates::from_config(&config::PermissionsConfig, prompt_handler: Option<...>) -> Result<Arc<dyn PermissionGate>>` returns `AllowListGate` for mode `allowlist`, `DenyAllGate` for `deny`, and `PromptingGate` for `prompt`; returns `Err(ConwayError::Config)` when mode is `prompt` and `prompt_handler` is `None`, with a message naming that a prompt handler is required.
- [machine] `presets::builtin_plugins() -> Vec<Arc<dyn Plugin>>` is gated on `#[cfg(feature = "builtin-tools")]` and returns the vector from `conway_tools::builtin_plugins()` unchanged (length and manifest ids equal).
- [machine] `presets::default_permissions_for_one_shot() -> PermissionsConfig` returns mode `allowlist` with empty `allowed_tools`.

## notes
**Objective:** Ship the three built-in `PermissionGate` implementations named in §4.3 plus the preset registration helpers, so the CLI and embedders never reimplement permission plumbing.

**Implementation Notes:**
- Glob matching uses the `globset` crate. Parse each allow/deny entry once at construction into `(tool_name, Option<GlobMatcher>)`. Malformed globs return `Err` from `AllowListGate::try_new`; `new` is a panicking convenience only if entries are static — prefer a single fallible `AllowListGate::new(...) -> Result<Self>` and no panicking variant.
- Argument matching target: for tools with a single primary string argument the matcher applies to the first schema property in declaration order; for `bash` this is `command`. ASSUMPTION (module spec is silent): the matched value is `req.arguments` serialized to a compact JSON string when the first property is not a string, so a pattern always has something to match; document this in the rustdoc.
- `AllowListGate` never returns `AllowAlways` — one-shot mode is stateless (`-p` MUST NOT prompt). `DenyWithFeedback` (not `Deny`) is used for allow-list misses so the model can adapt and the denial appears in structured output.
- All three gates are `Send + Sync + 'static` and hold no mutable state; `PromptingGate` holds only the handler `Arc`.
- `presets` contains no logic beyond delegation and defaults — no plugin is privileged (GP-03).

---

# WI-099: Agent definition loader (markdown + YAML frontmatter)

**complexity:** Medium

## scope
- `crates/conway/src/agents.rs` (create)
- `crates/conway/tests/agent_defs.rs` (create)
- `crates/conway/tests/fixtures/agents/` (create)

## depends
- WI-096
- WI-097 (consumes `config::AgentsConfig.dir`)
- MODULE:conway-core (`AgentDef`, `ToolSelector`, `ModelRef`, `RoleAlias`)

## criteria
- [machine] `agents::load_agent_defs(dir: &Path) -> Result<HashMap<String, AgentDef>>` exists and reads every `*.md` file in `dir` non-recursively.
- [machine] A well-formed file (fixture `reviewer.md`) parses into an `AgentDef` whose `name = "reviewer"`, `role = Some("coder")`, `tools` contains `read` and `grep`, `model = Some(ModelRef)`, `max_steps = Some(20)`, `result_contract = Some(JsonSchema)`, and whose `system_prompt` equals the markdown body verbatim with the frontmatter block and its delimiters removed and exactly one leading newline trimmed.
- [machine] Missing `dir` returns `Ok(empty map)`, not an error.
- [machine] A file with no `---` frontmatter delimiter returns `Err(ConwayError::AgentDef{path, message})` where message contains `"missing YAML frontmatter"` and `path` is the offending file.
- [machine] A file with an unterminated frontmatter block returns `Err(ConwayError::AgentDef)` with message containing `"unterminated frontmatter"`.
- [machine] A file with invalid YAML in the frontmatter returns `Err(ConwayError::AgentDef)` whose message includes the underlying YAML error text and the line number.
- [machine] A file whose frontmatter omits the required `name` key returns `Err(ConwayError::AgentDef)` with message containing `"missing required field 'name'"`.
- [machine] A file whose frontmatter `name` differs from the file stem returns `Err(ConwayError::AgentDef)` naming both values.
- [machine] Two files producing the same `name` (impossible given the stem rule, but checked defensively for case-insensitive filesystems) return `Err(ConwayError::AgentDef)` containing `"duplicate agent definition"`.
- [machine] An unknown frontmatter key returns `Err(ConwayError::AgentDef)` naming the key (`deny_unknown_fields`).
- [machine] `result_contract` given as an inline YAML mapping is converted to `serde_json::Value` and validated as a JSON Schema draft 2020-12 document; an invalid schema returns `Err(ConwayError::AgentDef)` containing `"invalid result_contract"`.
- [machine] An empty markdown body (frontmatter only) returns `Err(ConwayError::AgentDef)` containing `"empty system prompt"`.
- [machine] Fixture directory contains at least: `reviewer.md` (valid, all fields), `minimal.md` (valid, only `name`), `no_frontmatter.md`, `unterminated.md`, `bad_yaml.md`, `missing_name.md`, `name_mismatch.md`, `bad_contract.md`, `empty_body.md`.

## notes
**Objective:** Load agent definitions from `.conway/agents/*.md` into `conway_core::AgentDef` values, failing loudly and specifically on every malformation.

**Implementation Notes:**

Frontmatter schema (`#[serde(deny_unknown_fields)]`):

```yaml
---
name: reviewer            # String, REQUIRED, must equal the file stem
role: coder               # Option<RoleAlias>
tools: [read, grep, bash] # Option<Vec<String>> -> ToolSelector::Explicit; absent -> ToolSelector::Inherit
model: anthropic/claude-sonnet-4-6   # Option<ModelRef>, "<backend>/<model>"
max_steps: 20             # Option<u32>
result_contract:          # Option<serde_json::Value>, a JSON Schema object
  type: object
  required: [verdict]
  properties:
    verdict: { type: string }
---
```

- Parsing algorithm, in order: (1) read file to `String`; (2) require the content to begin with `---` followed by a newline (a UTF-8 BOM and leading blank lines are stripped first); (3) find the next line that is exactly `---` (after trimming trailing whitespace); (4) YAML-parse the slice between; (5) the remainder is the body. If step 2 fails → `missing YAML frontmatter`; if step 3 fails → `unterminated frontmatter`.
- Body normalization: strip a single leading `\n` (or `\r\n`) after the closing delimiter, then `trim_end()`. Do not otherwise alter whitespace — the body is a system prompt and indentation is meaningful.
- `ToolSelector::Inherit` when `tools` is absent is the documented default; an explicit empty list means *no tools*, which is distinct and must be preserved (test both).
- JSON Schema validation uses `jsonschema` crate's compile step; only compilation is checked here, not instance validation (instance validation is `conway-runtime`'s job).
- File ordering: sort entries by file name before processing so error reporting is deterministic across platforms.
- Files not ending in `.md` are ignored silently. Subdirectories are ignored (non-recursive) — ASSUMPTION, since the spec says `.conway/agents/*.md`.
- Per §9 this is Group 2 track I. It is implemented here as a standalone pure function so it can land independently; WI-100 wires the loaded map into `RuntimeDeps.agent_defs`.

---

# WI-100: ConwayBuilder assembly and `Conway::new_session`

**complexity:** High

## scope
- `crates/conway/src/builder.rs` (create)
- `crates/conway/src/conway.rs` (create)
- `crates/conway/src/lib.rs` (modify)
- `crates/conway/tests/builder.rs` (create)

## depends
- WI-096
- WI-097
- WI-098
- WI-099
- MODULE:conway-runtime (`Runtime::new(RuntimeDeps)`, `Runtime::start_root(RootSpec)`)
- MODULE:conway-backends (`AnthropicBackend::new`, `OpenAiCompatBackend::new`, `CapabilityProbe`)
- MODULE:conway-session (`JsonlSessionStore::open`)
- MODULE:conway-routing (`DeclarativeRouter::new`, `BreakerRegistry::new`, `CapabilityIndex`)
- MODULE:conway-tools (`builtin_plugins`)

## criteria
- [machine] `ConwayBuilder` exposes exactly these methods with these signatures: `from_config(impl AsRef<Path>) -> Result<Self>`, `discover() -> Result<Self>`, `from_parts(ConwayConfig) -> Self`, `with_backend(self, Arc<dyn Backend>) -> Self`, `with_plugin(self, Arc<dyn Plugin>) -> Self`, `with_permission_gate(self, Arc<dyn PermissionGate>) -> Self`, `with_session_store(self, Arc<dyn SessionStore>) -> Self`, `with_router(self, Arc<dyn Router>) -> Self`, `with_cli_overrides(self, CliOverrides) -> Self`, `build(self) -> Result<Conway>`.
- [machine] `build()` with a config naming an `anthropic` backend while the `anthropic` feature is disabled returns `Err(ConwayError::UnsupportedFeature{feature: "anthropic", ..})`.
- [machine] `build()` with no backends configured and none injected returns `Err(ConwayError::Build)` with message containing `"no backends"`.
- [machine] `build()` with no session store configured and the `jsonl-store` feature disabled and none injected returns `Err(ConwayError::Build)` containing `"no session store"`.
- [machine] Injection precedence: a backend injected via `with_backend` with the same `BackendId` as a config-derived one replaces the config-derived one; a test asserts the injected instance is the one present in the final registry.
- [machine] A `with_permission_gate` call overrides `permissions.mode` from config; a test with config mode `deny` plus an injected `AllowListGate` asserts the injected gate is used.
- [machine] With `builtin-tools` enabled and no `with_plugin` calls, the built `Conway`'s plugin registry contains exactly the manifests from `presets::builtin_plugins()`; with `builtin-tools` disabled it is empty.
- [machine] `with_plugin` with a manifest id equal to a built-in's id returns an error from `build()` containing `"duplicate plugin id"` (deduplication is not silent).
- [machine] `build()` performs no network I/O: a test using a fake backend asserts `CapabilityProbe` is only invoked when `config.models.probe_on_startup = true` (default `false`), and that with the default the `CapabilityIndex` is populated solely from `models.metadata_path`.
- [machine] `Conway::new_session(SessionSpec) -> Result<SessionHandle>` creates a session via `SessionStore::create`, starts a root agent via `Runtime::start_root`, and returns a handle whose `id()` equals the created `SessionId` and whose `root()` equals the returned `AgentId`.
- [machine] `SessionSpec::default()` produces `{ agent_def: None, role: None (falls back to config.default_role), cwd: config.cwd, budget: from config.limits, labels: vec![] }`; a test asserts each defaulted field.
- [machine] `Conway::explain_routing(&RoleAlias) -> ExplainReport` delegates to `conway_routing::RoutingExplain` and returns a report whose chain equals the configured chain for that role.
- [machine] End-to-end test with `conway-core` fakes (`FakeBackend`, `FakeStore`, `FakeGate`): `ConwayBuilder::from_parts(cfg).with_backend(fake).with_session_store(fake).with_permission_gate(fake).build()?.new_session(SessionSpec::default()).await?` succeeds with zero network and zero filesystem writes outside a temp dir.

## notes
**Objective:** Turn a validated `ConwayConfig` plus optional injected ports into a live `Runtime`, and expose session creation. This is the wiring layer — it contains no agent logic.

**Implementation Notes:**
- Internal builder state: `ConwayBuilder { config: ConwayConfig, cli_overrides: CliOverrides, backends: Vec<Arc<dyn Backend>>, plugins: Vec<Arc<dyn Plugin>>, gate: Option<Arc<dyn PermissionGate>>, store: Option<Arc<dyn SessionStore>>, router: Option<Arc<dyn Router>>, prompt_handler: Option<...> }`.
- `build()` validation and construction order — implement exactly this sequence, returning on the first error:
  1. Apply `cli_overrides` to `config` via `config::merge::apply_cli` (config-level validation, including OAuth rejection, has already run in `load`; re-run `merge::validate` after applying overrides so a CLI-supplied `sk-ant-oat*` key is caught).
  2. Load model metadata from `config.models.metadata_path` (missing file → empty).
  3. Construct config-derived backends. For each `[backends.<id>]`: match `kind`; if the corresponding cargo feature is off → `UnsupportedFeature`. Resolve `api_key_env` at this point; a named-but-unset env var → `ConwayError::Config` naming the variable.
  4. Merge injected backends over config-derived ones, keyed by `BackendId`. Error if the merged set is empty.
  5. Build `CapabilityIndex` from model metadata; if `config.models.probe_on_startup`, additionally run `CapabilityProbe` per backend and overlay probed results over file-derived ones (probe failure is a warning, not an error — it degrades to file metadata).
  6. Build `BreakerRegistry` from `config.health`.
  7. Router: use injected if present, else `DeclarativeRouter::new(config.routing(), health.clone(), capability_index)`.
  8. Store: injected, else `JsonlSessionStore::open(config.session.root)` under `#[cfg(feature = "jsonl-store")]`, else `Build{"no session store"}`.
  9. Gate: injected, else `gates::from_config(&config.permissions, prompt_handler)`.
  10. Plugins: `presets::builtin_plugins()` ++ injected; error on duplicate manifest ids.
  11. Agent defs: `agents::load_agent_defs(&config.agents.dir)`.
  12. `Runtime::new(RuntimeDeps{ store, router, health, backends, plugins, gate, agent_defs, event_bus })`.
- `Conway { rt: Arc<Runtime>, config: Arc<ConwayConfig> }`. Cheap `Clone`.
- `SessionSpec { agent_def: Option<String>, role: Option<RoleAlias>, cwd: Option<PathBuf>, budget: Option<Budget>, labels: Vec<String> }` with `Default`. `new_session` resolves `None` fields from config at call time, not at builder time, so a single `Conway` can serve differently-configured sessions.
- `new_session` sequence: build `SessionMeta` → `store.create(meta)` → `Runtime::start_root(RootSpec{ session, agent_def, role, cwd, budget })` → construct `SessionHandle`. If `start_root` fails after `create` succeeded, the session file remains (append-only; an empty session is valid and inspectable) — do not attempt rollback.
- `discover()` calls `config::load` with `LoadOptions::default()` and `cwd = std::env::current_dir()?`.
- `lib.rs` modification: add `mod builder; mod conway;` and the corresponding `pub use`. This item is the first to modify `lib.rs` after WI-096; WI-101 modifies it next and depends on this item.

---

# WI-101: SessionHandle core surface, TurnHandle, and EventStream

**complexity:** Medium

## scope
- `crates/conway/src/session_handle.rs` (create)
- `crates/conway/src/event_stream.rs` (create)
- `crates/conway/src/lib.rs` (modify)
- `crates/conway/tests/session_handle.rs` (create)

## depends
- WI-100
- MODULE:conway-runtime (`Runtime::prompt`, `::subscribe`, `::tree`, `::context_report`)
- MODULE:conway-session (`TranscriptResolver`)

## criteria
- [machine] `SessionHandle` exposes `id() -> SessionId`, `root() -> AgentId`, `prompt(impl Into<String>) -> Result<TurnHandle>` (async), `events() -> EventStream`, `tree() -> AgentTreeSnapshot`, `context_report(AgentId) -> Result<ContextReport>` (async), `transcript(AgentId) -> Result<Vec<LogRecord>>` (async).
- [machine] `SessionHandle` is `Clone + Send + Sync` — asserted by a `fn assert<T: Clone + Send + Sync>()` compile-time test.
- [machine] `prompt` delegates to `Runtime::prompt(self.root, text)` with no transformation of the text; a fake-runtime test asserts the received string equals the input byte-for-byte, including leading/trailing whitespace.
- [machine] `events()` returns a stream of `Envelope` filtered to `envelope.session == self.id()`; a test publishing envelopes for two sessions asserts only this session's are yielded.
- [machine] `events_from(seq: LogSeq) -> EventStream` exists and replays persisted envelopes with `seq >= seq` from the session log before switching to live broadcast, with no duplicates and no gaps at the junction; a test with 10 persisted and 5 live envelopes asserts exactly 15 in monotonically increasing `seq` order.
- [machine] A slow consumer yields `Event::Lagged{skipped}` rather than stalling; a test that fills the broadcast buffer asserts a `Lagged` envelope is observed and that subsequent envelopes continue to arrive.
- [machine] `EventStream` implements `futures_core::Stream<Item = Envelope>` and is `Send + Unpin`.
- [machine] `TurnHandle` exposes `text() -> Result<String>` (async; concatenates all `TextDelta` for the turn), `result() -> Result<AgentResult>` (async; resolves on `AgentFinished`), and `events() -> EventStream` (scoped to this turn's `agent_id`).
- [machine] `TurnHandle::result()` resolves even when the turn ends in `BudgetExceeded` or `Cancelled`; two tests, one per status.
- [machine] `context_report` delegates to `Runtime::context_report` and returns segments in the fixed §5.3 order; a test asserts the first segment's provenance is `AgentDef` or `SystemNote` and that provenance is `Some` for every segment.
- [machine] `transcript(agent)` returns the *effective* transcript via `TranscriptResolver` (ancestry-resolved), not only the agent's own records; a test with a forked fixture session asserts the inherited prefix is present.
- [machine] `transcript` for an unknown `AgentId` returns `Err(ConwayError::Runtime)` naming the agent id.

## notes
**Objective:** Deliver the Slice 1 consumer-facing handle: prompt in, events and text out, plus the GP-10 introspection reads. All methods are thin delegations to `Runtime`.

**Implementation Notes:**
- `SessionHandle { rt: Arc<Runtime>, session: SessionId, root: AgentId, store: Arc<dyn SessionStore> }`. All fields `Arc`/`Copy`; `Clone` is cheap.
- `EventStream` wraps `tokio_stream::wrappers::BroadcastStream<Envelope>` plus an optional replay prefix. Implement as an enum state machine `{ Replaying{ buf: VecDeque<Envelope>, live: BroadcastStream }, Live(BroadcastStream) }`; on `Replaying` exhaustion, transition to `Live` and drop any live envelope whose `seq` is `<=` the last replayed `seq` (dedup at the junction).
- Filtering by session happens in `poll_next` — do not create a per-session broadcast channel; the runtime owns one bus.
- `BroadcastStream`'s `RecvError::Lagged(n)` maps to a synthesized `Envelope` carrying `Event::Lagged{skipped: n}` with the last-seen `seq` and the handle's session/agent ids. If `conway-core` does not define `Event::Lagged`, flag it to the architect rather than inventing a local variant — §8 names `Event::Lagged{skipped}` as part of the guarantee, so it belongs in `conway-core`.
- `TurnHandle { session: SessionId, agent: AgentId, stream: EventStream }`. `text()` drains the stream accumulating `TextDelta` until `TurnFinished` for that agent; `result()` drains until `AgentFinished`. Both consume the handle's internal stream; calling `text()` then `result()` on the same handle must not deadlock — implement by buffering the terminal `AgentResult` in the handle when `text()` encounters it.
- No method on `SessionHandle` may take `&mut self`; all state changes go through the runtime.
- `lib.rs` modification: add `mod session_handle; mod event_stream;` and `pub use`. Depends on WI-100 to serialize the edit.

---

# WI-102: SessionHandle subagent surface (fork, spawn, steer, await, cancel)

**complexity:** Medium

## scope
- `crates/conway/src/session_handle.rs` (modify)
- `crates/conway/src/subagent_spec.rs` (create)
- `crates/conway/src/lib.rs` (modify)
- `crates/conway/tests/session_handle_subagent.rs` (create)

## depends
- WI-101
- MODULE:conway-runtime (`impl SubagentHost for Runtime`)
- MODULE:conway-core (`SubagentSpec`, `SubagentMode`, `Budget`, `ToolSelector`)

## criteria
- [machine] `SessionHandle` gains `fork(from: AgentId, spec: ForkSpec) -> Result<AgentId>`, `spawn(from: AgentId, spec: SpawnSpec) -> Result<AgentId>`, `steer(target: AgentId, text: impl Into<String>) -> Result<()>`, `await_agent(target: AgentId) -> Result<AgentResult>`, `cancel(target: AgentId, reason: &str) -> Result<()>` — all `async`.
- [machine] `ForkSpec` and `SpawnSpec` each convert into `conway_core::SubagentSpec` via `From`, producing `mode: Fork` and `mode: Spawn` respectively; a test asserts the mode and every field mapping.
- [machine] `SpawnSpec` requires `agent_def`: the field is non-`Option` (`agent_def: String`), so a spawn without one is a compile error rather than a runtime error. A compile-fail test (`trybuild`) asserts construction without `agent_def` does not compile.
- [machine] `ForkSpec::agent_def` is `Option<String>` and `cache_hint` defaults to `true`; `SpawnSpec` has no `cache_hint` field.
- [machine] All five methods delegate to the corresponding `SubagentHost` method on `Runtime` with unmodified arguments; a fake-`SubagentHost` test asserts argument identity for each.
- [machine] `await_agent` on a budget-exhausted child returns `Ok(AgentResult{status: BudgetExceeded, ..})` rather than `Err`; on a hard-cancelled child returns `Ok(AgentResult{status: Cancelled, ..})`. Two tests.
- [machine] `await_agent` on an unknown `AgentId` returns `Err(ConwayError::Runtime)` naming the id.
- [machine] `fork`/`spawn` reject a `from` agent that does not belong to this session with `Err(ConwayError::Runtime)` containing `"agent does not belong to session"`.
- [machine] `Budget` is mandatory on both specs: `ForkSpec`/`SpawnSpec` `Default` supplies the session's configured budget, and there is no way to construct a spec with `budget: None` (field type is `Budget`, not `Option<Budget>`).
- [machine] No fork/spawn *logic* lives in this file: a grep-based test asserts `session_handle.rs` contains no reference to `SessionStore`, `TranscriptResolver`, `ContextBuilder`, or `AgentTree`.

## notes
**Objective:** Expose the fork/spawn primitives at the library level before any tool exposes them (decision 2, API-first). Pure delegation to `SubagentHost`.

**Implementation Notes:**

```rust
pub struct ForkSpec {
    pub directive: String,               // becomes the fork_directive record
    pub agent_def: Option<String>,       // overrides the forker's system prompt
    pub role: Option<RoleAlias>,
    pub tools: Option<ToolSelector>,     // intersected with the forker's set by the runtime
    pub budget: Budget,
    pub cache_hint: bool,                // default true
    pub result_contract: Option<serde_json::Value>,
}

pub struct SpawnSpec {
    pub prompt: String,
    pub agent_def: String,               // REQUIRED — type-enforced
    pub role: Option<RoleAlias>,
    pub tools: Option<ToolSelector>,
    pub budget: Budget,
    pub result_contract: Option<serde_json::Value>,
}
```

- `From<ForkSpec> for SubagentSpec` maps `directive` → `prompt`, sets `mode: Fork`, passes `cache_hint` through. `From<SpawnSpec>` maps `prompt` → `prompt`, `agent_def` → `Some(AgentDefRef)`, sets `mode: Spawn`, `cache_hint: false`.
- `Default` for both specs cannot supply `agent_def` for `SpawnSpec`, so `SpawnSpec` does not derive `Default`; provide `SpawnSpec::new(agent_def, prompt)` with builder-style setters instead. `ForkSpec::new(directive)` mirrors this.
- Session-ownership check: call `self.rt.tree()` and verify `from` is present in the snapshot rooted at `self.root` before delegating. This check is in the facade because the runtime's `SubagentHost` is session-agnostic.
- Per §9 this is Group 2 track G; it depends on the runtime's `SubagentHost` implementation existing. It modifies `session_handle.rs` created by WI-101, hence the strict dependency edge.
- T-1 (fork overflow policy) is unresolved in the architecture. Do not add an `on_overflow` field. Surface whatever typed error the runtime returns unchanged; note this in rustdoc as a known open question.

---

# WI-103: Session resume, listing, and fork-from on `Conway`

**complexity:** Medium

## scope
- `crates/conway/src/conway.rs` (modify)
- `crates/conway/tests/resume.rs` (create)

## depends
- WI-100
- WI-102
- MODULE:conway-session (`SessionIndex`, `SessionStore::list`, `::meta`, `::children`, `::fork`)
- MODULE:conway-runtime (tree reconstruction from a resumed session)

## criteria
- [machine] `Conway::resume(sid: SessionId) -> Result<SessionHandle>` exists and returns a handle whose `id()` equals `sid` and whose `root()` equals the root agent recorded in the session header.
- [machine] `resume` on a nonexistent `SessionId` returns `Err(ConwayError::Store)` naming the id.
- [machine] A resumed handle's `transcript(root)` returns the same records as before the process restart; test writes a session via `new_session` + `prompt`, drops the `Conway`, rebuilds it, resumes, and asserts record equality.
- [machine] A resumed handle's `tree()` reconstructs all agents from the session index, including children created before the restart; a fixture with one root and two children asserts three nodes.
- [machine] A resumed handle's `prompt` appends to the existing log — the new record's `seq` equals `head + 1` from before the resume.
- [machine] `resume` on a session whose last log line is truncated/partial succeeds (truncate-and-warn), emitting a warning through the event stream rather than failing; test uses a fixture with a half-written trailing line.
- [machine] `Conway::sessions(SessionFilter) -> Result<Vec<SessionMeta>>` delegates to `SessionStore::list` and returns results unmodified; a test with three sessions and a label filter asserts the filtered subset.
- [machine] `Conway::fork_from(sid: SessionId, at: LogSeq, spec: ForkSpec) -> Result<SessionHandle>` creates a new session via `SessionStore::fork(sid, at, meta)` and returns a handle on the child; a test asserts the child's `SessionMeta.origin == Some(ForkOrigin{parent: sid, at_seq: at, mode: Fork})`.
- [machine] `fork_from` with `at` greater than the parent's `head` returns `Err(ConwayError::Store)` containing both the requested seq and the parent's head.
- [machine] `fork_from` copies zero records: a test asserts the child's session file contains exactly one line immediately after the call.
- [machine] `fork_from` with `at` equal to `0` is valid and produces a child with an empty inherited prefix.

## notes
**Objective:** Complete the `Conway` surface with resume, listing, and CLI-independent `--fork-from` support, so no CLI capability lacks a library equivalent (C-03, GP-05).

**Implementation Notes:**
- `resume` sequence: `store.meta(sid)?` → read the header to recover `agent_id`, `agent_def`, `role`, `cwd`, `budget` → `Runtime::resume_root(ResumeSpec{ session, meta })` (if `conway-runtime` exposes only `start_root`, flag the gap to the architect rather than reconstructing agent state in the facade) → construct `SessionHandle`.
- Tree reconstruction reads `store.children(sid)` transitively and hands the resulting shape to the runtime; the facade must not build `AgentTreeSnapshot` itself — it passes the child `SessionMeta` list and the runtime owns the tree type.
- `fork_from` builds the child `SessionMeta` from the parent's meta (inheriting `cwd`, `role` unless `spec.role` overrides, `agent_def` unless `spec.agent_def` overrides) plus `origin`. It reuses `ForkSpec` from WI-102 rather than defining a parallel type — this is why the dependency on WI-102 exists.
- `fork_from` is the session-level analogue of `SessionHandle::fork`: the former forks a *stored* session at an arbitrary seq (offline, no running parent); the latter forks a *live* agent at its head. Both call `SessionStore::fork`; only the latter goes through `SubagentHost`. Document this distinction in rustdoc — it is the most likely point of confusion in the public API.
- Truncated-trailing-line handling belongs to `conway-session`; the facade only ensures the warning reaches the event stream as `Event::Error{fatal: false}`.
- Per §9 this is Group 3 track K.

---

## Coverage Statement

**Module:** conway (facade), crate `crates/conway`

**Work items:** WI-096, WI-097, WI-098, WI-099, WI-100, WI-101, WI-102, WI-103

**Coverage:** These eight work items collectively implement 100% of the facade module's scope: config discovery/parsing/merging (WI-097), agent & skill definition loading (WI-099), backend construction from config and default plugin registration (WI-100), built-in permission gates (WI-098), the `ConwayBuilder`/`Conway`/`SessionHandle` public surface (WI-096, WI-100, WI-101, WI-102, WI-103), and the crate's feature-flag and stable-API discipline (WI-096). No scope is intentionally excluded. Two items are deferred-group deliverables per §9 and are sequenced accordingly: WI-102 (Group 2 track G, requires `SubagentHost`) and WI-103 (Group 3 track K, requires tree reconstruction). Slice 1 is satisfied by WI-096 through WI-101 alone. `SkillDef` loading is not separately itemized: the module spec names only `agents::load_agent_defs` under Provides and `conway-core` owns the `SkillDef` type; if skill files require a distinct loader, that is an unresolved gap in the module spec and is flagged to the architect rather than silently invented.

**Provides implemented by:**
| Provides | Work item(s) |
|---|---|
| `ConwayBuilder::{from_config, discover, with_backend, with_plugin, with_permission_gate, with_session_store, with_router, build}` | WI-100 |
| Feature flags `default = [anthropic, openai-compat, builtin-tools, jsonl-store]`, independently disableable backends | WI-096 |
| Stable public API / no runtime-internal re-exports | WI-096 |
| `Conway::new_session` | WI-100 |
| `Conway::explain_routing` | WI-100 |
| `Conway::resume`, `Conway::sessions`, `Conway::fork_from` | WI-103 |
| `SessionHandle::{id, root, prompt, events, tree, context_report, transcript}`, `TurnHandle`, `EventStream` | WI-101 |
| `SessionHandle::{fork, spawn, steer, await_agent, cancel}`, `ForkSpec`, `SpawnSpec` | WI-102 |
| `config::{ConwayConfig, load, merge}`, config precedence, no-network guarantee, model metadata local file + explicit refresh | WI-097 |
| `sk-ant-oat*` rejection at config parse (C-02, GP-09) | WI-097 (parse/load path), WI-100 (re-validated after CLI overrides) |
| `agents::load_agent_defs` (markdown + YAML frontmatter) | WI-099 |
| `gates::{AllowListGate, DenyAllGate, PromptingGate}` | WI-098 |
| `presets::builtin_plugins` | WI-098 (definition), WI-100 (registration) |
| `ConwayError` / `Result` | WI-096 |

**Requires consumed by:**
| Requires | Work item(s) |
|---|---|
| MODULE:conway-core — types, ports, `AgentDef`, `RoutingConfig`, `Event`, `Envelope`, `AgentResult`, `SubagentSpec` | WI-096, WI-097, WI-098, WI-099, WI-101, WI-102 |
| MODULE:conway-runtime — `Runtime::new/start_root/prompt/subscribe/tree/context_report`, `impl SubagentHost` | WI-100, WI-101, WI-102, WI-103 |
| MODULE:conway-backends — `AnthropicBackend`, `OpenAiCompatBackend`, `CapabilityProbe`, `ModelMetadata` | WI-096 (feature wiring), WI-100 (construction) |
| MODULE:conway-session — `JsonlSessionStore::open`, `TranscriptResolver`, `SessionIndex`, `SessionStore::fork` | WI-100, WI-101, WI-103 |
| MODULE:conway-routing — `DeclarativeRouter`, `BreakerRegistry`, `CapabilityIndex`, `RoutingExplain` | WI-100 |
| MODULE:conway-tools — `builtin_plugins()` | WI-098, WI-100 |

**Validation:** File scope is non-overlapping except for `crates/conway/src/lib.rs` (WI-096 → WI-100 → WI-101 → WI-102, a strict chain), `crates/conway/src/session_handle.rs` (WI-101 → WI-102), and `crates/conway/src/conway.rs` (WI-100 → WI-103) — each pair is sequenced by a dependency edge. The dependency graph is a DAG with maximum depth 5 (096→097→098→100→101→102→103 is the longest chain at depth 7; WI-098 and WI-099 are parallel siblings after WI-097).