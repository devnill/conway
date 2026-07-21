## Addendum — revised WI-097 (headroom config surface)

The headroom surface folds into the config work item; no new work item is required and no other item's scope changes. Delta only, plus the full revised item for clarity.

**Delta summary:** `crates/conway/src/config/schema.rs` gains `RoutingSection.default_headroom_tokens` and `RoleConfig.headroom_tokens`; `merge.rs` gains resolution + validation; `tests/config_precedence.rs` gains a per-role-override test; a new `tests/config_headroom.rs` carries the validation cases. Complexity stays High. Dependencies unchanged.

---

# WI-097 (revised): Config schema, discovery, precedence merge, headroom, and OAuth-token rejection

**complexity:** High

## scope
- `crates/conway/src/config/mod.rs` (create)
- `crates/conway/src/config/schema.rs` (create)
- `crates/conway/src/config/discovery.rs` (create)
- `crates/conway/src/config/merge.rs` (create)
- `crates/conway/src/config/model_metadata.rs` (create)
- `crates/conway/tests/config_precedence.rs` (create)
- `crates/conway/tests/config_oauth_rejection.rs` (create)
- `crates/conway/tests/config_headroom.rs` (create)
- `crates/conway/tests/fixtures/config/` (create)

## depends
- WI-096
- MODULE:conway-core (`RoutingConfig`, `BackendConfig`, `Budget`, `ModelRef`)

## criteria

*(all criteria from the original WI-097 remain unchanged; the following are added)*

- [machine] `[routing] default_headroom_tokens` deserializes into `RoutingSection.default_headroom_tokens: u32` and defaults to `16000` when the key is absent; a test with an empty config asserts the default.
- [machine] `[roles.<alias>] headroom_tokens` deserializes into `RoleConfig.headroom_tokens: Option<u32>` and defaults to `None`.
- [machine] `ConwayConfig::headroom_for(&RoleAlias) -> u32` returns the role's `headroom_tokens` when set, otherwise `routing.default_headroom_tokens`. Test: config with `default_headroom_tokens = 16000`, role `planner` with `headroom_tokens = 40000`, role `fast` without an override → `headroom_for("planner") == 40000` and `headroom_for("fast") == 16000`.
- [machine] `headroom_for` on an alias absent from `[roles]` returns the global default rather than erroring.
- [machine] Headroom participates in the full precedence chain independently at each level: with global default D, XDG `default_headroom_tokens` X, project `default_headroom_tokens` P, env `CONWAY_ROUTING__DEFAULT_HEADROOM_TOKENS` E, and CLI override C, `headroom_for` on a role with no override returns C, then E, then P, then X, then D as sources are removed in that order.
- [machine] A per-role `headroom_tokens` set in a lower-precedence source is NOT overridden by a higher-precedence source's `default_headroom_tokens`; test asserts XDG `roles.planner.headroom_tokens = 40000` wins over project `routing.default_headroom_tokens = 8000` for the `planner` role, while the `fast` role gets `8000`.
- [machine] Env override for a per-role value works: `CONWAY_ROLES__PLANNER__HEADROOM_TOKENS=30000` yields `headroom_for("planner") == 30000`.
- [machine] Validation: `default_headroom_tokens == 0` returns `Err(ConwayError::Config)` whose message contains `"default_headroom_tokens"` and `"must be greater than 0"`.
- [machine] Validation: any role's `headroom_tokens == 0` returns `Err(ConwayError::Config)` naming the role alias and containing `"must be greater than 0"`.
- [machine] Validation: `load` returns `Ok` but records a warning when an effective headroom value is `>=` the smallest `max_context_tokens` among all models reachable through any configured role chain, per model metadata. The returned `LoadOutcome.warnings` contains a warning whose text includes the role alias (or `"routing.default_headroom_tokens"` for the global value), the headroom value, the offending `ModelRef`, and that model's `max_context_tokens`.
- [machine] Warning, not error: a test with `headroom_tokens = 200000` against a model whose metadata declares `max_context_tokens = 32768` asserts `load` returns `Ok`, `warnings.len() == 1`, and the resulting `ConwayConfig` retains the value `200000` unmodified (no clamping).
- [machine] No warning is emitted when model metadata is empty/absent — headroom cannot be checked without context sizes; test with a missing `models.metadata_path` asserts `warnings.is_empty()`.
- [machine] Warnings are deterministic and ordered: a config with two offending roles produces two warnings sorted by role alias; test asserts exact ordering across two runs.
- [machine] Fixture set additionally contains `headroom_zero.toml`, `headroom_role_override.toml`, and `headroom_exceeds_context.toml` (paired with a `models.json` fixture declaring a 32768-token model).

## notes

**Objective:** Implement configuration as a pure, network-free, deterministic function of five ordered sources, with fail-loud validation, mandatory rejection of Anthropic subscription OAuth tokens, and an explicit headroom surface (tokens reserved for model output and reasoning in the context-window gate) resolvable globally and per role.

**Implementation Notes:**

TOML schema (`ConwayConfig`) — binding shape, with the headroom additions in `[routing]` and `[roles.<alias>]`:

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
allowed_tools = []
denied_tools  = []

[backends.anthropic]
kind = "anthropic"
api_key = ""
api_key_env = "ANTHROPIC_API_KEY"
base_url = ""

[backends.local]
kind = "openai-compat"
dialect = "ollama"
base_url = "http://localhost:11434/v1"
api_key_env = ""
stream_tools = false

[routing]
default_headroom_tokens = 16000  # u32, > 0. Tokens reserved for model output +
                                 # reasoning; the context-window gate treats a model's
                                 # usable input budget as max_context_tokens - headroom.

[roles.coder]
chain = ["local/qwen3-coder-80b", "anthropic/claude-sonnet-4-6"]

[roles.planner]
chain = ["anthropic/claude-sonnet-4-6"]
headroom_tokens = 40000          # Option<u32>, > 0 when present. Overrides
                                 # routing.default_headroom_tokens for this role only.

[health]
transport_failures_to_open = 3
open_duration = "30s"
probe_interval = "15s"
probe_timeout = "2s"

[agents]
dir = ".conway/agents"

[models]
metadata_path = ".conway/models.json"
```

- `[routing]` is a new section owned by the facade. If `conway_core::RoutingConfig` already carries a headroom field, deserialize into it directly and do not duplicate; if it does not, `ConwayConfig` owns `RoutingSection` and converts into `RoutingConfig` in `ConwayConfig::routing()`, passing headroom through as part of that conversion. Flag to the architect if `RoutingConfig` has no place to carry headroom — the value must reach `conway-routing`'s capability filter (`RequiredCaps::min_context`), and inventing a facade-local side channel would break the §8 contract.
- Headroom resolution is a *read-time* function (`headroom_for`), not a merge-time flattening. Do not materialize per-role effective values during merge — the global default and the per-role override remain separately addressable so that a higher-precedence source can change one without clobbering the other. This is what makes the precedence criteria above hold.
- Merge semantics for headroom follow the general rule already specified: leaf-wise, so `roles.planner.headroom_tokens` and `routing.default_headroom_tokens` are independent leaves. `[roles.*]` tables merge by key union; `chain` arrays replace wholesale.
- Env mapping follows the existing convention: `CONWAY_ROUTING__DEFAULT_HEADROOM_TOKENS`, `CONWAY_ROLES__<ALIAS_UPPER>__HEADROOM_TOKENS`. Alias case is normalized by uppercasing the configured alias for comparison; an env var naming an alias absent from `[roles]` is ignored (consistent with the existing "unknown `CONWAY_*` vars are ignored" rule).
- `CliOverrides` gains `headroom_tokens: Option<u32>`, which sets `routing.default_headroom_tokens` (there is no CLI form for a per-role override — a CLI-supplied value is a session-wide floor). Document this asymmetry in the field's rustdoc.
- **Validation order in `merge::validate`** — the headroom checks slot in as steps 7 and 8, after all existing checks, so an OAuth token or an unresolvable `ModelRef` still fails first:
  1. OAuth token rejection (`sk-ant-oat*`).
  2. `default_role` exists in `[roles]`.
  3. Every `ModelRef` in every `chain` has the form `<backend_id>/<model>` and `<backend_id>` exists in `[backends]`.
  4. `permissions.mode = "allowlist"` requires non-empty `allowed_tools`.
  5. `fsync = "interval"` requires `fsync_interval_ms > 0`.
  6. `api_key` and `api_key_env` are not both non-empty for the same backend.
  7. **Hard error:** `routing.default_headroom_tokens > 0`, and every present `roles.<alias>.headroom_tokens > 0`. Message templates: `"routing.default_headroom_tokens must be greater than 0"` and `"roles.{alias}.headroom_tokens must be greater than 0"`.
  8. **Warning only:** for each role, compute `headroom_for(alias)` and compare against `min(max_context_tokens)` over the models in that role's `chain` that appear in the loaded `ModelMetadata`. If `headroom >= that minimum`, push a warning: `"headroom for role '{alias}' is {headroom} tokens, which is not less than the smallest context window in its chain ({model_ref} = {max_context} tokens); every request routed to that model will be rejected by the context-window gate"`. When the offending value came from the global default rather than a role override, substitute `"routing.default_headroom_tokens"` for `"headroom for role '{alias}'"` and emit it once per offending role (deduplicate identical messages). Models absent from metadata are skipped — unknown context size is not a warning.
- **`load` return type changes** to accommodate warnings: `config::load(LoadOptions) -> Result<LoadOutcome>` where `LoadOutcome { config: ConwayConfig, warnings: Vec<ConfigWarning> }` and `ConfigWarning { code: WarningCode, message: String }` with `WarningCode::{HeadroomExceedsContext}` (`#[non_exhaustive]`). `ConwayConfig` derefs are not used; callers take `outcome.config`. WI-100's `ConwayBuilder::from_config`/`discover` must forward `outcome.warnings` — they surface as `Event::Error{fatal: false}` on the first session's event stream, matching the truncated-log warning path in WI-103. This is a signature change to a symbol WI-100 consumes; WI-100's dependency on WI-097 already sequences it, and its criterion "`build()` performs no network I/O" is unaffected.
- Warning computation reads `ModelMetadata` from `config.models.metadata_path`, so `model_metadata::load` runs inside `config::load` (locally, no network — the existing no-network criterion still holds). A missing metadata file yields an empty map and therefore zero headroom warnings.
- Headroom is a *config* concern only in this item. Applying it (computing `usable_input = max_context_tokens - headroom` and gating/routing on it) belongs to `conway-routing` and `conway-runtime`; nothing in `crates/conway/src/config/` may perform that arithmetic beyond the validation comparison above.

---

## Coverage Statement (amended)

**Module:** conway (facade), crate `crates/conway`

**Work items:** WI-096, WI-097 (revised), WI-098, WI-099, WI-100, WI-101, WI-102, WI-103

**Coverage:** Unchanged from the original statement, with one addition to the Provides mapping: the headroom config surface (global `[routing] default_headroom_tokens`, per-role `[roles.<alias>] headroom_tokens`, `ConwayConfig::headroom_for`, precedence, and validation) is implemented by WI-097 and forwarded by WI-100. No other work item's scope, file set, or dependency edges change. File scope remains non-overlapping; the DAG is unchanged.

**Provides — addition:**
| Provides | Work item(s) |
|---|---|
| Headroom config surface: `routing.default_headroom_tokens`, `roles.<alias>.headroom_tokens`, `ConwayConfig::headroom_for`, headroom precedence and validation | WI-097 |
| `LoadOutcome.warnings` propagation to the event stream | WI-097 (production), WI-100 (forwarding) |

**Open item flagged to the architect:** `conway_core::RoutingConfig` must carry headroom for it to reach `conway-routing`'s capability filter (`RequiredCaps::min_context`). If it does not, that is a gap in the `conway-core` module spec, not something the facade may work around — §8's `RouteRequest`/`RequiredCaps` contract is the only sanctioned channel.