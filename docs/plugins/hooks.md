# Hooks and extension points: the normative reference

Every point a plugin or an embedder can attach behavior to, its exact
contract, and what the runtime does when the thing on the other end errors,
times out, answers garbage, or is absent. Depends on
[`concepts.md`](concepts.md) for vocabulary (hook, plugin, observer,
participant, value-class, fork/spawn, trust subject) — this page does not
redefine any of those terms, only applies them point by point.

**This is the document an implementation is built against.** Its hardest
requirement is truth: every claim below is checked against the tree as of
this writing, cited by symbol name (never a line number — this codebase's
line numbers drift), and every point is labeled **Implemented** or
**designed-not-built** with the board item tracking the gap. A labeled forward declaration is a respectable
state; an unlabeled one is a trap. **Most of the rows below read designed-not-built.** That is the
correct outcome of checking, not a defect in this document — the tree ships
tool registration, one `ContextHook`, one `PermissionGate`, and file-based
permission rules; almost everything else `.design/extension-architecture.md`
describes is decided design, not yet code.

## How to read a point's table

Every point gets all nine fields:

| Field | Meaning |
|---|---|
| Kind | Observer (fire-and-forget, no reply channel, cannot deny) / Participant (returns a value the runtime acts on, bounded, fail-closed) / Declarative (a static assertion consulted at a decision point, never invoked live) |
| Receives | The exact payload, field by field |
| May return | The exact permitted values, and what is *not* permitted |
| On error | What the runtime does when the implementation returns `Err`, panics, or is malformed |
| On timeout | Which timeout, its default, whether it is clamped |
| On garbage | Malformed input or an unknown enum variant crossing this point |
| When absent | Behavior with nothing registered |
| Ordering | Whether more than one registration composes, and how |
| Status | **Implemented** or **designed-not-built**, with the board item for the latter |

## The points

### 1. Tool declaration — `Plugin::manifest()` / `Tool::spec()`

| Field | Value |
|---|---|
| Kind | Declarative |
| Receives | Nothing live — consulted once, at registry construction (`PluginRegistry::from_plugins`, `crates/conway-runtime/src/tools/registry.rs`) |
| May return | A `PluginManifest { id, version, tools, required_host_caps }` and, per tool, a `ToolSpec { name, description, schema, category, permission }` (`crates/conway-core/src/content.rs`) |
| On error | A schema that fails to compile (`jsonschema::validator_for`) fails registry construction with a `RuntimeError::Tool(ToolError::Internal)` naming the tool — the whole registry does not partially build |
| On timeout | Not applicable — synchronous, in-process, no I/O |
| On garbage | An unparseable schema is a registration error, not a runtime one; there is no live-request path where a schema can arrive malformed |
| When absent | A `Plugin` with no `tools()` entries contributes nothing; a session with no plugins beyond the built-ins registers only those |
| Ordering | Tool names must be unique across every registered plugin; a collision is a build-time error (`ConwayBuilder::build`) naming both plugins, never a silent last-registration-wins |
| Status | **Implemented.** `Plugin::manifest`/`Plugin::tools` (`crates/conway-core/src/ports/plugin.rs`), exercised by `crates/conway/tests/plugin_surface.rs` and `crates/conway/tests/plugin_builtin_parity.rs`. In-process only — the wire projection `WireManifest`/`tool.spec/1` for an out-of-process plugin is design only (`.design/extension-architecture.md` §4, §6.2), no board item yet names its implementation |

**`required_host_caps` is declared and consulted nowhere.** Every constructor
of `PluginManifest` in the tree — every built-in tool
(`crates/conway-tools/src/{fs,shell,subagent,report}/mod.rs`), the first-party
plugin skeleton (`crates/conway-plugin-skeleton/src/lib.rs`), and every test
fixture — passes `required_host_caps: vec![]` or `Vec::new()`, and no code
anywhere reads the field to gate anything. This is a declared field with zero
consumers, not a partially-wired capability system: the glossary entry in
`concepts.md` already states it ("Declared today; not yet consumed anywhere
in the tree"), and this document confirms it by exhaustive grep, not
inference. It needs a forward-declaration label of its own distinct from the hook-first
surface it will eventually gate through — no board item currently names
"consume `PluginManifest::required_host_caps`" specifically; the closest
tracked work is the declarative `hooks` surface, `01KZDC0RDRMMMJHX7SAFMM2Q5A`,
which is where a capability grant/request handshake would first need one.

### 2. Tool execution — `Tool::invoke`

| Field | Value |
|---|---|
| Kind | Participant |
| Receives | `ToolCall { call_id, name, arguments }` (already schema-validated against this tool's own `spec().schema` by the caller) and `ToolCtx { agent_id, session_id, cwd, chdir, cancel, events, subagents, config }` |
| May return | `Result<ToolOutput, ToolError>` — `ToolOutput { blocks, is_error, truncation, artifacts }` on success; a typed `ToolError` (`InvalidArguments`, `Timeout`, `Cancelled`, `Io`, `Internal`, `NotFound`, …) on failure |
| On error | Model-visible feedback, never an abort. `ToolRunner` (`crates/conway-runtime/src/tools/runner.rs`) catches a panic per call and turns it into a `ToolOutcome` with `is_error: true` — a misbehaving tool degrades one call, not the run |
| On timeout | No trait-level deadline exists; a tool honors `ctx.cancel` cooperatively. `bash`, the one built-in tool with genuinely long-running invocations, enforces its own `DEFAULT_TIMEOUT_MS` (120 s) internally — that is a `bash`-specific policy, not a port-level one every `Tool` inherits |
| On garbage | `PRE: call.arguments has already been validated` — a `Tool::invoke` implementation is entitled to assume schema-valid arguments, but it must still not panic on adversarial values within that schema (`ToolCtx`'s own doc; `Tool::render`'s doc states the identical rule for its own untrusted-input surface) |
| When absent | The tool is not registered; a call naming it never resolves (`ToolRunner`'s registry lookup fails before invocation, distinct from a running tool erroring) |
| Ordering | Not applicable — one call resolves to exactly one tool by name; there is no composition question for execution itself |
| Status | **Implemented**, in-process. The wire form (`tool/1`, the RPC-shaped `ToolCtx` projection in `.design/extension-architecture.md` §6.1) is design only |

**`ToolCtx.subagents` never lets a tool act as a different agent.** It is a
`SubagentHandle` (`crates/conway-core/src/ports/mod.rs`) bound to
`ToolCtx.agent_id` at construction, not a raw `Arc<dyn SubagentHost>` a tool
could redirect — closing the exact cross-tree exfiltration shape board item
`01KYTP0PGKJ4VCJP5TD39A1WHF` fixed.

### 3. Context curation — `ContextHook::before_request`

| Field | Value |
|---|---|
| Kind | Participant |
| Receives | `ContextHookCtx { agent_id, agent_path, session_id, turn, model, estimated_tokens, artifacts, tag }` and a `ContextPayload { segments: Vec<PromptSegment>, tools: Vec<ToolSpec> }` — the just-assembled request, before routing |
| May return | An edited `ContextPayload` — see "The value-class boundary" below. Returning the payload unchanged is always valid; the trait's own doc states this explicitly |
| On error | Not applicable at the trait level — `before_request` has no `Result` in its signature, so an implementation that wants to signal a failure can only do so by returning the payload unchanged or by panicking, and a panic here is not caught the way a tool's panic is (this call sits in `AgentLoop::run_inner`, not behind `ToolRunner`'s per-call `catch_unwind`) |
| On timeout | No dedicated deadline. The call is awaited inside the agent's own turn, bounded only by `self.spec.budget.deadline` if the embedder set one (`AgentLoop::run_inner`'s `tokio::select!` against `route_attempt_fut`) — an agent with no configured deadline has no bound on this call at all |
| On garbage | Not applicable — `ContextPayload` is a Rust value the hook constructs directly, in-process; there is no wire encoding to be malformed yet |
| When absent | The runtime holds `Option<Arc<dyn ContextHook>>` (`LoopDeps::context_hook`) and never invokes anything when it is `None` — not even a no-op call. Behavior is byte-identical to a build with no `ContextHook` machinery at all |
| Ordering | Exactly one `ContextHook` per embedder (`ConwayBuilder::with_context_hook` takes one `Arc<dyn ContextHook>`, not a `Vec`) — composition across *multiple* hooks is specified (§16.3's union rule, below) but unreachable today: there is nothing yet to compose |
| Status | **Implemented.** `ContextHook::before_request` (`crates/conway-core/src/ports/plugin.rs`), invoked from `AgentLoop::run_inner` (`crates/conway-runtime/src/agent_loop.rs`), exercised end-to-end and by that port's own unit tests (`before_request_can_drop_a_segment`) |

**Reads `docs/agents.md`'s tool-announcement-vs-execution split into
practice.** `ContextPayload.tools` is what the model is *told* it may call
this turn; narrowing it here hides a tool from the model entirely for the
turn, but never bypasses `PermissionGate` for a call the model still manages
to propose against an un-hidden tool — `PluginRegistry::specs`'s own doc
states it plainly: "announcement and execution are independent gates, and
neither implies the other." `ToolBatchCtx`
(`crates/conway-runtime/src/tools/runner.rs`) resolves a call by name against
the *whole* registry and carries no selector at all — a `ContextHook` that
hides a tool from announcement narrows what the model is offered, never what
it is capable of calling if it names the tool anyway (that gate is
`PermissionGate`, point 5 below, and the confinement root, neither of which
consults `AgentSpec::tools`/`ContextPayload.tools` at execution time).

**`ContextHookCtx.agent_path` tells a hook where in the tree it is running,
not just who it is.** It is the root→this-agent chain, root first and
including the agent's own id (a root agent's path is `vec![agent_id]`) — the
identical shape and ordering as `PermissionRequest.agent_path` (point 5
below), both populated from the same `AgentLoop::agent_path` field. A hook
that wants to behave differently for a top-level agent than for a subagent
four levels down reads `ctx.agent_path.len()` (or walks it directly) rather
than needing a second, redundant lookup against the live tree.

**`ContextHookCtx.tag` is the embedder's own correlation identifier, and
conway never reads it.** Board item `01KZQJ03ZQ22MPM9H2TW1350ZF`: an
embedder mapping conway agents onto its own domain objects (a file, a job, a
node in its own tool) sets `SubagentSpec::tag` when it creates the agent
(`Some(String)`, or `None` for a root agent or a child whose caller left it
unset), and reads it back here — on the child's very first turn, closing the
race a post-hoc side table keyed on the freshly-minted `AgentId` would have:
by the time `SubagentHost::start` returns an id to register against, a
`keep_alive`-less child may already have run its first turn. Threaded
through unread by `AgentSpec::tag` — this is the first field in this table
conway carries but never branches, matches, or compares against for any
decision (routing, permission, budget, or logging); contrast `role`, which
`DeclarativeRouter` resolves against, and `SessionMeta::ask_origin`, which
gates whether a `result_contract` may attach. Not yet exposed on the
facade's `ForkSpec`/`SpawnSpec` — an embedder wanting one today constructs a
`SubagentSpec` (or calls `SubagentHost::start` directly) rather than going
through those two convenience builders.

### 4. Context overflow retry — `ContextHook::on_overflow`

| Field | Value |
|---|---|
| Kind | Participant |
| Receives | The same `ContextHookCtx` (now with `model: Some(..)` always set — a route was already chosen), the still-too-large `ContextPayload`, and `OverflowInfo { max_context_tokens, headroom_tokens, required_tokens, shortfall_tokens }` |
| May return | `Option<ContextPayload>` — `Some(smaller_payload)` for the runtime to re-estimate and retry; `None` (the trait's own default) to give up |
| On error | Not applicable at the trait level, same as `before_request` — no `Result` in the signature |
| On timeout | Same as `before_request`: no dedicated deadline, bounded only by the agent's own budget if one is configured |
| On garbage | Not applicable, same reasoning as `before_request` |
| When absent | `None` registered, or a hook whose `on_overflow` returns `None` on its first call, both fall straight through to a hard `RoutingError::ContextTooLarge` — identical to what `run_inner` would produce with no `ContextHook` machinery at all |
| Ordering | One hook, same as `before_request`. The runtime, not the hook, bounds the retry: `MAX_OVERFLOW_ATTEMPTS = 2` (`crates/conway-runtime/src/agent_loop.rs`) caps how many times `route_and_attempt` will call `on_overflow` for one turn — a hook that keeps returning a still-too-large payload cannot hang the turn |
| Status | **Implemented** — and genuinely invoked, which was not always true. See the boundary below |

**The exact boundary on when this fires, stated precisely because it is easy
to overstate.** `on_overflow` fires **only** when the router or the attempt
engine rejects with `RoutingError::ContextTooLarge` — the case where *every*
candidate in the resolved chain was rejected **solely** on the headroom gate
(`conway_plugin_routing::router::DeclarativeRouter::resolve`'s own module
doc, decision `01KYXS3PTYVATWR58JR95AZJYN`, closing board item
`01KYXNAHN64YMADZPQDQC0CPTJ`). A candidate that fails on headroom **and**
something else — a missing tool-calling capability, an unhealthy endpoint —
is a *mixed* failure, still an ordinary capability skip, and disqualifies the
whole request from `ContextTooLarge`: resolution falls back to
`RoutingError::NoCandidate` instead, and `AgentLoop::route_and_attempt`'s own
destructure (`let RoutingError::ContextTooLarge { .. } = routing_err else {
return Err(routing_err.into()) }`) means **no hook fires for `NoCandidate`,
ever** — the turn fails with the router's own error, unmodified. This is a
recorded, deliberate consequence rather than an oversight: a hook
cannot always shrink a request back under the window, only the specific case
where size was the *only* thing wrong. Before the admission cluster landed
(2026-08-01), `DeclarativeRouter` folded every all-rejected outcome into
`NoCandidate` unconditionally, so this hook was reachable only via
`AttemptEngine::execute`'s own backstop check
(`conway_core::ports::check_admission`, `crates/conway-core/src/ports/backend.rs`)
— a route the router itself admitted but whose real backend still rejected on
size. Both paths reach `on_overflow` identically today; only the router-side
gap is new.

**Two designed extensions to this point, neither built.** The redirect (§16.4
of `.design/extension-architecture.md`) decided `before_request` should also
be able to propose a *durable* mask — a `target_seq` set the runtime diffs
against `LogRecord::ContextMask` and persists — alongside its existing
ephemeral, per-request edit. `LogRecord::ContextMask`
(`crates/conway-core/src/log.rs`) is real, persisted, and consumed by
`apply_context_mask` (`crates/conway-session/src/resolver.rs`), but **has no
producer anywhere in the tree** — confirmed by search across
`conway-runtime`, `conway-tools`, and the facade. No hook can express "mask
this durably" today; every `ContextHook` edit is ephemeral, scoped to one
request, and invisible on the next turn unless the hook repeats it. No board
item currently names this specific gap (durable-mask production from a
hook) — it is closer in scope to `01KZ844ZXZMVRWC7ZANT7PSM6X` (the
`context.hook/1` replace gap) than to anything with its own tracker, and
should get one before doc 3 or 4 promises the durable form as available.

### 5. Tool-call permission decision — `PermissionGate::check`

| Field | Value |
|---|---|
| Kind | Participant |
| Receives | `PermissionRequest { agent_id, agent_path, tool, category, arguments, rendered, call_id, render_kind }` (`crates/conway-core/src/agent.rs`) |
| May return | `PermissionDecision::{AllowOnce, AllowAlways { scope }, Deny { reason }, DenyWithFeedback { message }}` |
| On error | Not applicable at the trait level — `check` returns `PermissionDecision` directly, no `Result`. A gate implementation that cannot decide has no sanctioned "error" reply; it must pick one of the four variants |
| On timeout | **None, by contract.** `PermissionGate::check`'s own doc states the gate "may block indefinitely: the runtime holds the tool call pending... and emits `Event::PermissionRequested` while it waits" — a human may be on the other end. `PermissionBroker`'s own module doc states it plainly: "It never imposes a timeout on the gate." Gate cancellation (e.g. process shutdown) is expected to surface as `Deny { reason: "cancelled" }`, per the port's own doc — but this is a convention an implementation follows, not something the trait enforces structurally; `conway-cli`'s `TuiGate` (`crates/conway-cli/src/tui/gate.rs`) does follow it |
| On garbage | Not applicable — a `PermissionDecision` is a Rust enum an in-process implementation constructs directly |
| When absent | **Cannot be absent.** `ConwayBuilder::build` requires a gate to exist — either injected via `with_permission_gate` or derived from config (`gates::from_config`); there is no "no gate registered, default to allow/deny" fallback anywhere |
| Ordering | **Exactly one `PermissionGate` per embedder.** `with_permission_gate` takes one `Arc<dyn PermissionGate>`, replacing any config-derived selection wholesale — not a composed chain. `crates/conway/src/gates.rs` ships three trivial implementations (`AllowListGate`, `DenyAllGate`, `PromptingGate`) for testing/embedding, and `conway-cli`'s `TuiGate`/oneshot gate are the two real ones |
| Status | **Implemented**, as a single, embedder-supplied port. See point 7 below for what does not exist: a *composed*, plugin-contributed policy chain feeding into this decision |

### 6. Operator-authored permission rules — `permissions.json`, flat and structured

| Field | Value |
|---|---|
| Kind | Declarative |
| Receives | Nothing live — loaded from `<project>/.conway/permissions.json` and the global config file at startup / `/trust permissions`, via `Conway::load_permission_files` |
| May return | A flat `{ "allow": [...], "deny": [...] }` string list, and/or a structured `{ "rules": [{ select, when, then }] }` array (`Rule`/`Select`/`When`/`Then`, `crates/conway-core/src/permission_pattern.rs`) — both desugar into one internal `Rule` and are evaluated by the same path (`PatternRule::to_rule`) |
| On error | A malformed JSON entry is dropped, fail-closed (`ParsedPermissionFile`'s loader: `serde_json::from_value::<Rule>` returning `Err` silently skips that one entry rather than rejecting the file). A structurally *sound but semantically inert* rule — `command_prefix` on a `Structured`-rendering tool, `paths_under` on an `Unconfinable` tool, an unresolvable `paths_under` prefix — is a typed `RuleRegistrationError` (`PathsUnderOnUnconfinedTool`, `PathsUnderPrefixUncanonicalizable`, `CommandPrefixOnStructuredTool`), surfaced as a red transcript notice at startup and on `/trust permissions`, never installed silently |
| On timeout | Not applicable — synchronous file load, no I/O beyond a local read |
| On garbage | Same as "on error" above — dropped or refused, never guessed at |
| When absent | No file, no rules of that kind. The permission decision falls through to `PermissionGate::check` (point 5) for every call |
| Ordering | Two-stage composition (`.design/extension-architecture.md` §5.5, confirmed live in `PermissionBroker::decide`): **admission** — a `deny`/`prompt` rule installs from any file, trusted or not (narrowing has no failure mode worth gating on trust); an `allow` rule from a *project* file installs only once its exact bytes match a recorded trust decision (`TrustStore`, see `concepts.md`'s "Trust" section) — then **most-restrictive-wins** over the admitted set: any `deny` beats every `prompt` beats every `allow`. Registration order (which file, which rule within a file) never changes the outcome, only which single matching rule is reported |
| Status | **Implemented**, including the structured `{ select, when, then }` form. This corrects the item spec that produced this document: it named the structured rule form (board `01KYTJD6CJ1CHJBXZ0GFYMV5MT`, "F12") as pending at writing time. That item is now **done** — `Rule`/`Select`/`When`/`Then` are real, exercised by `permission_pattern::f12_tests` and the real-stack seam `crates/conway/tests/structured_rule_seam.rs`, and documented operator-facing in `docs/permissions.md`'s "The structured `rules` array" section |

**A second correction, same shape.** The item spec also named the
convergence of conway's three control-character sanitizers (board
`01KYTJE5TSJBF01598F3BKJP1X`, "F2/F3") as pending. It too is **done** — a
single shared `conway_core::text::sanitize_control_chars` now backs the
`rendered` seam every `PermissionRequest.rendered` value passes through
(`runner::sanitize_rendered`) and `ToolOutcome::error`'s construction alike,
per `CHANGELOG.md`'s entry for that item. It is not itself a hook or
extension point — no row above documents it — but it bears on point 5's
`PermissionRequest.rendered` field and point 6's `command_prefix` matching,
both of which read the now-single-source sanitized form rather than one of
three previously-divergent copies.

### 7. Plugin-contributed permission rules — `PatternOrigin::Plugin`

| Field | Value |
|---|---|
| Kind | Declarative |
| Receives | N/A — no live call site exists |
| May return | By design: `deny`/`prompt` rules only, never `allow`. `PermissionBroker::remember_pattern_rule` (`crates/conway-runtime/src/permission.rs`) structurally rejects `Then::Allow` paired with `PatternOrigin::Plugin` — a guard at the broker boundary, not an absence-of-transport accident |
| On error | N/A |
| On timeout | N/A |
| On garbage | N/A |
| When absent | This is the current, only state: no `Plugin` implementor has any way to submit a rule. `Plugin::tools()`/`Plugin::manifest()` are the entire trait surface (point 1) — there is no `Plugin::rules()` method or equivalent |
| Ordering | N/A |
| Status | **designed-not-built**, but with real, tested guard code already in place ahead of the producer: `PatternOrigin::Plugin` (`crates/conway-core/src/permission_pattern.rs`) exists as a variant, is exercised by `crates/conway-runtime/tests/permission_broker.rs` (proving the allow-rejection holds even though nothing constructs this variant outside tests today), and its own doc names the reason precisely — "the invariant rests on a guard, not on the absence of a transport." Tracked under the same umbrella as the declarative `hooks` surface, `01KZDC0RDRMMMJHX7SAFMM2Q5A`. **Correction (board item 01KZS00JP5QNBJSSHNFP9C47GM): this row's forward reference is now only partially right.** `pre_tool_use` IS a real, dispatched capability (point 13 below) — but it does NOT produce `PatternOrigin::Plugin` rules, and its answer shape is not "allow/deny/deny-with-feedback": `HookPermissionVerdict` is `no_opinion` (proceed) or `deny { reason }` ONLY, with no `allow` variant at all, by construction (decision 01KZRZAFD8T3GX407MZC8P1W1E — a hook may only narrow, never widen). `pre_tool_use` dispatch feeds `PermissionBroker::decide` directly, as an independent narrowing-only chain step (exactly one narrowing chain, never configurable policy branching), never through this row's `Plugin::rules()`/`PatternOrigin::Plugin` producer, which remains exactly as unbuilt as before this item |

### 8. Composed inference-evaluated permission policy — `permission.policy/1`

| Field | Value |
|---|---|
| Kind | Participant |
| Receives | Design only: a `PolicyRequest` carrying `agent_id`, `agent_path`, `session`, `tool`, `category`, raw `arguments`, `rendered`, `render_kind`, `call_id`, `cwd`, `root`, `mode`, `must_reach_gate` (`.design/extension-architecture.md` §5.3) |
| May return | Design only: a `NarrowingPolicy` returns `Deny { reason } | Abstain` (no `Allow` variant exists on its type); a `DecidingPolicy` returns `Deny { reason } | Abstain | Allow { reason }` — a type-level split, not a runtime flag, specifically because "may only narrow" must be a property of the return type an inference-evaluated policy cannot talk its way around |
| On error | Design only: `on_failure`, default `Deny` — **never `Allow`** |
| On timeout | Design only: a declared `timeout_ms`, clamped by an operator-configured maximum (design default 60 s) |
| On garbage | Design only: an unparseable verdict is treated as `on_failure`, never guessed at |
| When absent | Today: nothing is affected, because nothing consults this point — `PermissionBroker::decide`'s real ordering (below) has no policy-chain step at all |
| Ordering | Design only: most-restrictive-wins, admission gated by trust for the allow half only (identical two-stage shape to point 6) |
| Status | **designed-not-built.** No `NarrowingPolicy`/`DecidingPolicy` trait exists anywhere in the workspace. `PermissionBroker::decide`'s real, live ordering has exactly eight steps and none of them is a policy chain — see "The permission decision ordering" below. No dedicated board item names building this composed chain distinctly from the declarative `hooks` surface; `01KZDC0RDRMMMJHX7SAFMM2Q5A` is the closest tracked work and is the umbrella this document cites for it |

### 9. Remote context-editing parity — `context.hook/1`

| Field | Value |
|---|---|
| Kind | Participant |
| Receives | Design only: the wire projection of `ContextHookCtx` plus `ContextPayload` (`.design/extension-architecture.md` §16.5) |
| May return | Design only: `{ appends, excludes, durable_excludes }`, composed across every hook — in-process and remote alike — under the union rule (§16.3, below) |
| On error / timeout / garbage / absent | Design only — inherits the same shapes as points 3/4 above, projected onto a wire boundary that does not exist yet |
| Ordering | Design only: exclusion is a set union (order-independent); a same-target *replace* collision between two hooks fails to exclusion rather than picking a winner |
| Status | **designed-not-built.** Named replacement for the retired `context.append/1` point, which the redirect superseded because it gave a remote plugin strictly *less* than the in-process `ContextHook` already gives (append-only, no edit/drop) — the exact built-in/third-party inversion this project forbids. Even the *specification* of `context.hook/1` has an acknowledged gap: it can append and exclude but has no same-target **replace** primitive, tracked as board item `01KZ844ZXZMVRWC7ZANT7PSM6X`, still `open` as of this writing |

### 10. Tool-announcement hiding as a plugin-declared selector — `context.tools/1`

| Field | Value |
|---|---|
| Kind | Declarative |
| Receives | Design only: nothing live — a selector naming tools this plugin's own declaration hides from announcement |
| May return | Design only: a `ToolSelector`-shaped exclusion set |
| On error / timeout / garbage | Design only: a selector matching nothing is a registration error, per `.design/extension-architecture.md` §4's own row for this point |
| When absent | Today: the only way to narrow `ContextPayload.tools` is an embedder-supplied `ContextHook::before_request` doing it imperatively (point 3) — there is no way for a `Plugin`, as such, to *declare* a static hide-list the way this point would let it |
| Ordering | Design only: a set union over every plugin's declared hides |
| Status | **designed-not-built.** No `Plugin` method for declaring a tool-hide selector exists; tracked under `01KZDC0RDRMMMJHX7SAFMM2Q5A` alongside the rest of the generalized point vocabulary |

### 11. Event observation by a plugin — `observe/1`

| Field | Value |
|---|---|
| Kind | Observer |
| Receives | Design only: an `Envelope`/`Event` (`crates/conway-core/src/event.rs`), filtered by a declared selector |
| May return | Nothing — the point has no reply channel, structurally. This is the one place "the shape itself forbids a denial" (see `concepts.md`'s "Observers vs participants") is easiest to see: there is no return type to smuggle a decision through |
| On error | Ignored — an observer cannot fail the run by construction |
| On timeout | Not applicable |
| On garbage | Design only: an unknown `Event` variant is ignored — the one enum-versioning case where "ignore" is the *right* answer, per `.design/extension-architecture.md` §6.3, because an observer changes nothing by definition |
| When absent | Today: `EventSink::emit` (`crates/conway-core/src/ports/events.rs`) is real and invoked constantly, but it is an **embedder-level** subscription (`conway::EventStream`, `crates/conway/src/event_stream.rs`), not something a `Plugin` implements or registers through the `Plugin` trait. A slow consumer is dropped from delivery and sees `Event::Lagged { skipped }` on its next successful receive rather than stalling the runtime — that guarantee is real and tested today, for the embedder's own stream |
| Ordering | Independent by construction — multiple subscribers never interact |
| Status | **designed-not-built**, specifically as a *plugin*-reachable point. The underlying mechanism (`EventSink`, lossy-with-notice delivery) is implemented and load-bearing; what is missing is any way for an in-process or remote `Plugin` to subscribe to it the way an embedder's `EventStream` already can. Tracked under `01KZDC0RDRMMMJHX7SAFMM2Q5A` |

### 12. UI status contribution — `status.declare/1` / `status/1`

| Field | Value |
|---|---|
| Kind | Declarative (`status.declare/1`) + Observer output (`status/1`) |
| Receives / May return / failure modes | Entirely design (`.design/extension-architecture.md` §8): a plugin declares per-key `{ max_len, ttl_ms }`, then pushes `StatusContribution { key, value }`; a stale value expires at snapshot time and the render path never calls a plugin or blocks on one |
| Status | **designed-not-built.** No status-line plugin surface exists in the tree; `conway-cli`'s status line reads only conway's own computed state |

### 13. Declarative script-fired hooks — the `hooks` configuration block

| Field | Value |
|---|---|
| Kind | Declarative (registration) wrapping whatever kind the named event actually is (Observer for a logging hook, Participant for `pre_tool_use`) |
| Receives / May return | **All SEVEN core events are real; nothing is design-only here anymore.** A rule's command receives `{"name":"<event>","payload":{...}}` on stdin (`conway_core::hook::HookInvocation`/`HookEvent`) and answers on stdout with a JSON `conway_core::hook::HookAnswer`, whose `permission` field (`HookPermissionVerdict`) may be `"no_opinion"` (proceed, the default) or `{"deny":{"reason":...}}` (refuse the call) — read only by `pre_tool_use` and `prompt_submitted`, the two events that can deny; every other event ignores it. **There is no `"allow"` shape — the type has no such variant, by construction** (decision 01KZRZAFD8T3GX407MZC8P1W1E: a hook may only narrow a permission verdict, never widen one). **A rule may additionally narrow WHICH occurrences of `pre_tool_use`/`post_tool_use` it fires for**, via `match` (board item 01KZYAWQ6011Q6CJVG6CCMQPF1): an exact tool name (`"bash"`, `"fs.write"`) or a `*`-glob against the tool's whole name (`"fs.*"`), checked by `conway_core::hook::tool_matcher_matches`. Absent `match` (the default) fires the rule for every occurrence of `event`, unchanged from before this field existed. `match` on any event that carries no tool name (`session_starting`, `child_spawned`, `request_assembled`, `child_reported`, `prompt_submitted`) is a load-time config error naming the rule's `id` (`crate::config::merge::validate`), never silently ignored. |
| On error / timeout / garbage / absent | **`pre_tool_use`/`prompt_submitted`: fail-closed.** A missing/unexecutable command, a timeout, a nonzero exit, or stdout that fails to parse as `HookAnswer` are ALL `HookFailure` (`conway_core::error::HookFailure`), and the consuming dispatch treats every one of them as a denial — the runner's own failure signal, not a second weaker fail-closed implementation layered on top. **The five observation events fail OPEN**: `post_tool_use`, `session_starting`, `child_spawned`, `request_assembled`, `child_reported` — a failing hook is logged (`tracing::warn`) and the thing it observed is unaffected. |
| Ordering | `pre_tool_use`/`prompt_submitted`: rules are consulted in the order `ConwayBuilder::build` filtered them from `[hooks].rules[]`; the FIRST denying hook wins (order-independent for the boolean outcome — a `deny` beats a `no_opinion` however many hooks run, so which hook happens to be checked first only changes which hook's `reason` is reported, never whether the call is denied). The five observation events: every subscribed, matcher-satisfying hook runs, in configured order; a failure never stops a later hook from running. |
| Status | **All SEVEN core events are DISPATCHED.** `pre_tool_use` (board item 01KZS00JP5QNBJSSHNFP9C47GM); the observation-only events `post_tool_use`, `session_starting`, `child_spawned` (board item 01KZS019NHG11RVQYSVT7RG0P5), `request_assembled`, `child_reported` (board item 01KZYAXSGDS8AP7YK1CN7H680G); and `prompt_submitted` (board item 01KZS01ZBNEY12DBDNW2Y861SQ), which may DENY but may never MODIFY. **`prompt_submitted` fires at BOTH submission sites** — `Runtime::start_root` for a session's first prompt and `Runtime::prompt` for a follow-up — before the text reaches the agent loop, with `{text, agent_id, session, first_prompt}`. It fails CLOSED like `pre_tool_use`, and a denial surfaces to the CALLER as `RuntimeError::PromptDenied`, never to a model as a tool error, since there is no model turn yet to report into. **It cannot rewrite a word of what the user typed, and that is a TYPE guarantee rather than an unwired path:** the dispatch reads only `HookPermissionVerdict`, whose whole vocabulary is `no_opinion` and `deny { reason }` — no variant and no field can carry replacement text back, and `HookAnswer.context` is ignored here (`.design/extension-architecture.md` §5.8: the user's own words are the one thing in the pipeline nothing gets to launder). **The observation tier cannot deny and fails OPEN, which is the opposite of `pre_tool_use` and is deliberate:** the thing it observes has already happened (or, for `request_assembled`, has been decided but not yet sent), so breaking a working operation because a logging script timed out would be the wrong direction. `conway_runtime::hook_dispatch::HookDispatcher::dispatch` returns `()`, so a failing observation hook is logged via `tracing::warn` and cannot propagate; `post_tool_use` fires at `ToolRunner`'s `ToolCallFinished` seam with `{call_id, tool, is_error, preview, agent_id, agent_path, session}` and honors `match` against `tool`; `session_starting` fires ONCE per `Runtime::start_root` (never per turn, and never on `resume_root`) with `{agent_id, session, cwd}`; `child_spawned` fires at the single `SubagentHost::start` that BOTH fork and spawn share, with `{child_id, parent, caller, mode, session}`; `request_assembled` fires ONCE per turn, from `AgentLoop::run_inner`, after `ContextBuilder::build` and (if registered) `ContextHook::before_request`'s own edit, and before that turn's route/attempt call, with a SUMMARY payload `{agent_id, agent_path, session, turn, model_pin, segment_count, total_tokens_est, tokenizer}` — never the full assembled segment content, a performance/privacy decision this event's own item does not make unilaterally; and `child_reported` fires for every terminal `AgentResult` that crosses back to a parent — both a normal completion (`AgentLoop::finish`) and a supervisor-synthesized one (`conway_runtime::supervisor`: a panic, or a task unresponsive past its grace window) — with `{agent_id, parent, session, result}`, gated on the same publish-race winner `Event::AgentFinished` already uses at each site so it fires exactly once per agent; it NEVER fires for a root's own finish, since a root has no parent for a result to cross back to. **Both `request_assembled` and `child_reported` are observation-only, like their three siblings, not a lesser version of something else**: `request_assembled` sits at the exact seam `ContextHook::before_request` already edits the assembled request at, so it would be reasonable to expect it to edit too — it structurally cannot, for the identical reason `prompt_submitted` cannot rewrite the prompt (this dispatch tier discards `HookAnswer.context`/`permission` unread). A configured script editing assembled context append-only, without breaking the prompt cache, is a SEPARATE, still-open board item (`01KZRZZP6A4A27R3EN0HQAENBS`) this one does not build and does not foreclose. **Whether `child_spawned` may ever DENY a spawn is an open question, deliberately deferred** and recorded at its dispatch site rather than answered by the shape of a return type. **`pre_tool_use` and `post_tool_use` alone honor `match`** — `PreToolUseHookSpec::matcher`/`crate::hook_dispatch::HookSpec::matcher`, checked against `AuthorizedCall::tool`/the payload's `"tool"` field respectively — the other five events carry no tool name for a matcher to narrow against. The `pre_tool_use` half is otherwise unchanged: `conway_runtime::permission::PermissionBroker::decide` invokes an injected `Arc<dyn HookRunner>` (`ConwayBuilder::with_hook_runner` — mirroring `with_permission_gate`/`with_context_hook`; not called at all is still the default for a third-party embedder, and a `pre_tool_use` rule with no runner injected parses, validates, and is silently never consulted. `conway-cli` DOES inject one, via the `builtin-tools`-gated convenience `ConwayBuilder::with_default_hook_runner` which supplies `conway_tools::hook_runner::ProcessHookRunner` — board item 01KZVTTP492R3BDY33FAGYWDNW — so a rule written in a `settings.json` driving the CLI fires. The CLI's opt-in is not inherited by an embedder linking `conway` directly) once per enabled `[hooks].rules[]` entry whose `event == "pre_tool_use"`, at the SAME tier as a `deny` pattern rule — before the mode gate, the cache, pattern-allow grants, and `AutoAllow`, so a denying hook is enforced under every permission mode including `AutoAllow` (the one mode with no human in the loop to catch what a downstream-of-the-gate check would have missed). `Plugin::manifest()`/`Plugin::tools()` is still the whole `Plugin` trait otherwise; there is still no `hooks()` method, no `subagent_mode` field, and no `hook.fork` capability anywhere in the tree. `01KZRZY1MNM872BZ6AKEBG3SKE`'s `HookRunner` port/`ProcessHookRunner` implementation is the general script-runner mechanism `pre_tool_use` dispatch was the FIRST consumer of, not the last — `request_assembled`/`child_reported` reuse the identical runner and fail-open contract the three earlier observation events already established, adding only their own two dispatch call sites, exactly as `01KZYAXSGDS8AP7YK1CN7H680G`'s own scope stated. All events remain tracked under the umbrella `01KZDC0RDRMMMJHX7SAFMM2Q5A`. **The config shape decision (board item 01KZYAWQ6011Q6CJVG6CCMQPF1), recorded here because it affects every rule an operator writes:** `PHILOSOPHY.md` §5 is the 1.0 specification and this shipped schema converges toward it field-by-field, not via a wholesale reshape onto the page's illustrative nested `{"pre_tool_use": [...], ...}` JSON. `[hooks].rules[]` stays a FLAT list keyed by a per-entry `event` field (unchanged), and `command` stays an argv `Vec<String>` rather than the page's single `run` shell string (a deliberate, previously-recorded divergence predating this item — see `schema::HookEntry::command`'s own doc). What converges is `match`, spelled exactly as the page spells it (`"match"` on the wire; the Rust field is `match_tool` only because `match` is a reserved word) — the smallest change that turns "fires for every tool call, unusable for the page's own canonical example (run the formatter after a write)" into parity, without a breaking reshape of every existing `[hooks]` block. A full move to the page's nested shape remains available as later, purely additive work; nothing here forecloses it. **A script runner is not a second extension mechanism:** the design is explicit that a script-dispatching hook is itself an ordinary `Plugin` whose own implementation happens to shell out per event, layered on top of the one mechanism, never beside it |

### 14. Fork/spawn declaration for an inference-evaluated hook

| Field | Value |
|---|---|
| Kind | Declarative |
| Receives / May return | Design only: a per-hook-registration `subagent_mode: Fork | Spawn` field, defaulting to `Spawn`. `Fork` additionally requires a granted `hook.fork` capability, following `subagent.spawn`'s exact shape — default off, never implied by trust, requested and separately granted |
| On error | Design only: an operator may refuse a requested `hook.fork` (the hook fails to register if declared required, or is skipped with a status change if optional); an operator may never force `Fork` onto a hook that declared `Spawn`, and a runtime may never silently downgrade a declared `Fork` to `Spawn` and run it anyway — "never guessed at" |
| On timeout | Design only: §16.2's decision-bearing-call exclusion applies — see "Failure semantics" below |
| Ordering | Not applicable — a per-registration field, not a composed value |
| Status | **designed-not-built.** No hook registration surface exists at all (point 13), so there is nowhere for a `subagent_mode` field to attach. `crates/conway/src/intent.rs`'s `classify` function is the one shipped precedent for the shape `Spawn` mode would reuse — a zero-tool, `SubagentMode::Spawn` judge deciding one narrow question from a prompt alone, with no ancestry and no tool access — but it is not a hook; it backs the TUI's natural-language `/fork`/`/spawn` intent classifier, an unrelated feature that predates this design and happens to need the same shape |

**Correction to the design corpus this document must make explicit, per the
brief that produced it:** any future inference-evaluated hook running in
`Fork` mode will inherit the parent's `agent_def` — and never that def's
`result_contract` — because that is now how *every* fork in the tree behaves
(`Runtime::start`'s Fork arm, `crates/conway-runtime/src/subagent.rs`,
`def_was_inherited`; decision `01KZHEWXDZWPWMEAQ01XY2RDCB`, board
`01KZGXYSEKMVM4GVG4ZBWC0WSC`). This post-dates every design document
discussing hook fork/spawn and is not itself hook-specific — it is a
correction to the fork primitive generally, which a hook's `Fork` mode would
simply inherit once built.

### 15. TUI slash-command declaration — `Plugin::commands()` / `Command::invoke`

| Field | Value |
|---|---|
| Kind | Declarative (`Plugin::commands()`, consulted once, at TUI startup) + Participant (`Command::invoke`, an operator-triggered call the host runs and shows the result of) |
| Receives | `Command::spec()` is consulted with nothing live, at registry construction, and returns a [`CommandSpec`] (`name`, `summary`). `Command::invoke` receives a [`CommandCtx`]: `focused_agent`, `root_agent`, and `args` (everything typed after the command word, verbatim — the same "consume the remainder verbatim" rule every other slash command's free-text argument follows) |
| May return | A [`CommandOutcome`]: `Output(Vec<String>)` (lines appended to the transcript verbatim, each its own entry) or `Error(String)` (shown as an ordinary `Notice`, the same severity a failing built-in command gets) |
| On error | `invoke` returning `CommandOutcome::Error` is not a failure of the *host* — it is the command's own reported outcome, rendered as a `Notice` and nothing more. A **panic** inside `invoke` is isolated: the host runs it inside a `tokio::spawn`, and a panicking task cannot bring down the process or the TUI's render/input loop — its `JoinError` is converted into an ordinary `CommandOutcome::Error` naming the panic, delivered through the same reply channel a normal return uses |
| On timeout | None imposed. A command that never completes leaves its reply channel silent forever, but never blocks anything else — see "When absent"/Ordering below for why this is structural, not a convention an implementation must remember |
| On garbage | Not applicable to `invoke` (it receives typed Rust values, not wire input). At *registration*, a malformed `CommandSpec::name` (empty, containing whitespace, or failing `conway::plugin::validate_command_name` once namespaced) is a **named, install-time error** — the TUI refuses to start rather than installing a command that could never be typed or that malforms its own namespace |
| When absent | No `Plugin::commands()` override means no commands (the trait's own default returns `Vec::new()`) — every existing `Plugin` implementor, built-in or third-party, keeps compiling and behaving identically. With the declaring plugin not installed at all, its command's full name is simply unknown — `commands::parse` recognizes the *shape* of a plugin-looking word (containing conway-core's event/command namespace separator, `.`) but resolution against the installed registry happens only in `execute`, so an uninstalled plugin's command produces the ordinary "unknown command" notice, never a stub or a special case |
| Ordering | **The render/input loop never calls a plugin, and never blocks on one — the same hard-won property point 12 (`status.declare/1`/`status/1`) establishes for the status line, reused here for the same reason.** `commands::execute` resolves a command (a synchronous `HashMap` lookup) and returns an `Effect::RunPluginCommand` describing it, without ever calling `invoke`; `App` (`conway-cli`) spawns the actual call on its own task, off the `select!` loop that drives rendering and key handling, and receives the reply on a channel exactly like `/ask`'s own modal-answer plumbing (`ModalAskOutcome`/`run_modal_ask`). A hanging command therefore degrades to "the operator doesn't see a reply yet," never to a frozen terminal |
| Status | **Implemented.** `conway_core::ports::plugin::{Command, CommandCtx, CommandOutcome, CommandSpec}` and `Plugin::commands()`'s default (`crates/conway-core/src/ports/plugin.rs`); dispatch through `conway_cli::tui::commands::{SlashCommand::Plugin, CommandRegistry, Host::resolve_command}` and `conway_cli::tui::app::App::spawn_plugin_command`/`apply_plugin_command_done`. `conway-plugin-skeleton`'s `SkeletonPingCommand` (`/{plugin id}.ping`) is the worked example. Board item 01KZYBFTK4QPB45AJT9M57P60W |

**Why this is narrower than a hook that can touch a live session, and
deliberately so.** [`CommandCtx`] carries read-only identity and the raw
argument text — nothing that reaches a live `Conway`/`SessionHandle`. Unlike
every OTHER point in this table, `Plugin`/`Command` live in `conway-core`,
which structurally cannot depend on `conway` (the facade crate one layer up,
where session-manipulation capability like `Conway::fork_from` lives) without
a dependency cycle. The natural bridge — mirroring how [`ToolCtx::subagents`]
narrows fork/spawn into a running tool call via a `conway-core`-native port —
would need a NEW port of that shape (a "which session is this command allowed
to touch, and how" capability), threaded through `conway-runtime`/
`ConwayBuilder`, that no item has built. Rather than design that capability
speculatively, this point ships the largest grant possible without it — read
identity, print output — and the gap is disclosed, not silently worked
around: a command needing more than this is, in this project's own words
about the tool surface (point 7's own precedent), "a bug report against the
plugin API," not a reason to reach past `conway-core`'s own layering.

**Namespacing is mandatory, and shadowing a built-in is impossible by
construction, not merely checked.** `CommandRegistry::build` (the one
constructor) always registers a command as `{plugin manifest id}.{command's
own bare name}`, via `conway::plugin::validate_command_name` — the EXACT
same rule `conway_core::event_name::validate_event_name` already enforces
for plugin-declared events (§16.6), reused rather than reinvented: both share
one implementation, differing only in which noun their error text names. No
built-in TUI command word (`help`, `quit`, `fork`, `spawn`, `steer`, `tree`,
`context`, `why`, `resume`, `settings`, `exit`) contains that separator, so
no plugin, however it names itself or its command, can ever produce a full
name equal to a bare built-in's — a plugin declaring a command named `help`
registers cleanly as its OWN, separately reachable `/{its id}.help`, never as
`/help`. What registration DOES refuse, with a named error: two commands (one
plugin declaring the same name twice, or two plugins landing on the identical
full name) colliding with EACH OTHER — the collision this scheme cannot rule
out structurally.

**Everything goes through `commands::parse`.** `SlashCommand::Plugin { full_name,
args }` is an ordinary variant of the same closed enum every built-in command
is (there is no second, parser-bypassing surface for this point — see
`crates/conway/tests/architecture_invariants.rs`'s `t9_tui_has_exactly_
the_four_known_parser_bypasses`, unaffected by this point). `parse` recognizes
only the SHAPE of a plugin command (a command word containing the namespace
separator) — staying pure and state-free, consistent with every other arm —
and defers resolving whether that name is actually installed to `execute`,
the one place with a `CommandRegistry` to resolve against.

**Discovery.** A declared command appears in the `/` palette
(`conway_cli::tui::view::palette::matches`/`draw_overlay`, merged with the
built-in table at call time from `AppState::plugin_commands`) — the SAME
surface an operator already uses to find every built-in command. `/help`
(`conway_cli::tui::view::help`) stays keybindings-only by long-standing
convention (T7/V4: "`/help` does not list slash commands; see `/` for
those") — a plugin command is discoverable exactly the way a built-in one
is, through the one surface that already lists commands, not duplicated into
a second listing that could drift from it.

### 16. Plugin-declared custom event — `Plugin::events()` / `PluginEventHandle::emit`

| Field | Value |
|---|---|
| Kind | Declarative (`Plugin::events()`, consulted once, at `ConwayBuilder::build`) + Observer (`PluginEventHandle::emit`, fire-and-forget, no reply channel, cannot deny — dispatched through the identical `HookDispatcher::dispatch` point 13's five observation events use) |
| Receives | `Plugin::events()` is consulted with nothing live and returns zero or more [`EventDecl`] (`name`, `summary`, `carries_tool_name`). `PluginEventHandle::emit(bare_name, payload)` — reachable from `ToolCtx::plugin_events`, bound at construction to the resolved tool's own declaring plugin id — takes a BARE name (never the namespaced form) and an arbitrary `serde_json::Value` payload the plugin itself defines; there is no core-imposed shape beyond "valid JSON", mirroring `HookEvent::payload`'s own "event-specific, decided by whoever wires a concrete event" contract |
| May return | Not applicable — `emit` returns nothing, and a plugin cannot observe whether any hook was actually subscribed |
| On error | `emit`'s only failure mode is a malformed assembled name (only reachable via an empty `bare_name`, since the declaring plugin's own id is validated once at registration) — dropped silently, never a panic, matching `HookDispatcher::dispatch`'s own posture for a failing hook downstream of it |
| On timeout | None imposed on `emit` itself; a subscribed hook's own timeout is the SAME `timeout_ms` every `[hooks].rules[]` entry declares (point 13) |
| On garbage | At *registration* (`ConwayBuilder::build`), a malformed declaration (an empty bare name, or two events landing on the identical namespaced full name — from the same plugin or two different ones) is a **named, build-time error**, mirroring point 15's identical registration-time refusal for a malformed `CommandSpec::name` |
| When absent | No `Plugin::events()` override means no declared events (the trait's own default returns `Vec::new()`) — every existing `Plugin` implementor, built-in or third-party, keeps compiling and behaving identically. A `[hooks].rules[].event` naming no installed plugin's declared event parses, validates, and is silently never dispatched — the SAME tolerance a typo'd core event name has always had |
| Ordering | Every hook subscribed to the SAME namespaced event name runs, in configured order, exactly like point 13's five observation events (this is literally the same dispatch table, unioned) — a failure never stops a later hook from running |
| Status | **Implemented.** `conway_core::ports::{EventDecl, PluginEventEmitter, PluginEventHandle}` and `Plugin::events()`'s default (`crates/conway-core/src/ports/plugin.rs`); `conway_runtime::hook_dispatch::declared_plugin_events` (namespacing/validation) and `impl PluginEventEmitter for HookDispatcher` (dispatch, reusing point 13's own fan-out); `ConwayBuilder::build` unions the result into the SAME dispatch table `[hooks].rules[]` already feeds. `conway-plugin-skeleton`'s `pong_dispatched` event is the worked example: `SkeletonPlugin::events()` declares it, `SkeletonPingTool::invoke` fires it unconditionally on every call, and `conway-plugin-skeleton/tests/skeleton_end_to_end.rs`'s `a_configured_hook_fires_when_the_skeletons_declared_event_is_dispatched` proves a real configured `[hooks].rules[]` entry actually receives it. Board item 01KZS03BFE720EQZG7Q2768N2H |

**The declaration/firing split, and why an undeclared-but-fired (or
declared-but-never-fired) event is a defect, not a shrug.** `PHILOSOPHY.md`
§5: "An event a plugin declares and never fires is the same defect as a tool
that does nothing, and is treated as one." `Plugin::events()` ships only the
declaration; nothing enforces that a plugin author who declares an event
also calls `emit` for it anywhere — that discipline is on the plugin author,
exactly as "a tool that does nothing" is never mechanically detectable
either. What IS enforced structurally: `PluginEventHandle::emit` can only
ever produce a name under the SAME plugin id the handle was constructed
with (`conway_core::ports::PluginEventHandle`'s own doc) — there is no
parameter through which a call could fire under a different plugin's
namespace, so an operator wiring a hook to `"acme.routing.candidate_chosen"`
is trusting a name only the plugin whose manifest id is `acme` can ever
actually produce.

**Namespacing, and why the "plugin id containing the separator" exclusion
this point's own validator used to state no longer holds.**
`declared_plugin_events` always registers an event as `{plugin manifest
id}.{event's own bare name}`, via `conway_core::event_name::
validate_event_name` — the SAME shared validator point 15 uses for command
names (`conway::plugin::validate_command_name`), one implementation,
differing only in which noun the error text names. An earlier draft of this
rule (`.design/extension-architecture.md` §16.6 point 3) additionally
excluded a plugin id that itself contains the separator, reasoning from a
SUBSCRIBER-side hazard (recovering `id` by splitting `name` on the first
`.` could misattribute an event to the wrong plugin). This item is the
"whichever item first validates a `PluginManifest` at registration time"
that section named as owing the resolution, and it resolves the OTHER way:
every real built-in plugin id in this workspace (`conway.fs`,
`conway.shell`, `conway.plugin_skeleton`, ...) already contains the
separator, so the exclusion would make this point unreachable for all of
them. `validate_event_name`'s own doc comment (`crates/conway-core/src/
event_name.rs`) carries the disclosed reasoning for why the hazard cannot
occur on the declaration side by construction (`id` is always supplied out
of band, never recovered by splitting), and why a genuine full-name
collision is still caught — as a duplicate, at `declared_plugin_events` —
regardless of why two declarations landed on the same string.

**Discovery: no new registry, the SAME mechanism point 15 already
established for commands.** `declared_plugin_events(plugins: &[Arc<dyn
Plugin>])` is a free function, not a method on a live `Conway`/`Runtime` —
an embedder already holding the exact plugin list it is about to hand
`ConwayBuilder` can call it directly, before `build()`, and read back every
plugin event's full namespaced name, one-line `summary`, and
`carries_tool_name`. `ConwayBuilder::build` calls this SAME function to
decide what `[hooks].rules[]` may actually dispatch — one implementation,
not a parallel "validate" path and a separate "enumerate" path that could
drift apart. No `conway-cli` surface lists this yet (mirroring point 13's
own disclosed gap for `[hooks].rules[]` visibility) — the mechanism exists
and is reachable; a TUI/settings presentation of it is later, additive
work.

**`match` on a plugin event.** A rule's `match` (point 13) narrows which
occurrences of an event fire a hook, and only makes sense against a payload
that names a tool. `EventDecl::carries_tool_name` is the plugin's own
declaration of whether that is true for ITS event — the plugin, not the
core, is the one party that actually knows its payload's shape. A `match`
paired with a plugin event whose own declaration says `carries_tool_name:
false` is a named, build-time error (`ConwayBuilder::build`), the identical
class of defect point 13's `merge::validate` check already gives a CORE
event without a tool name — just discoverable only once the installed
plugin set is known, since `merge::validate` itself has no access to it.

**Why observation-only, never a second deny-capable tier.** `PHILOSOPHY.md`
§5's own routing/compaction examples ("a routing plugin can offer a point
before it commits to a candidate") could plausibly want to deny, not merely
observe. This point ships only the observation tier — reusing point 13's
`HookDispatcher::dispatch` exactly, fails open, cannot deny — deliberately:
a second, deny-capable tier for plugin events raises the identical class of
open questions point 13's own `child_spawned` section defers (what does the
plugin see when its own request is denied — a `Result`, a silent no-op? does
every caller need new error handling?), and answering those by the shape of
`emit`'s return type, ahead of a real consumer that needs it, is exactly the
trap this document's own point 14 and `PluginManifest`'s retired `on_init`
warn against.

## The permission decision ordering

This is the ordering most likely to be reasoned about incorrectly, so it is
stated as a numbered pipeline twice: what `PermissionBroker::decide`
(`crates/conway-runtime/src/permission.rs`) **actually does today**, and,
separately, what the design's policy-chain overlay would add if points 7 and
8 above were ever built. Do not conflate the two.

### Today, as shipped

1. **Root confinement** (`PermissionBroker::check_root`). A `Denied` root
   decision returns immediately — before the cache, patterns, `AutoAllow`, or
   the gate are ever consulted. `MustReachGate` (an unconfinable call under a
   configured root) does not deny; it sets an accumulator that forces every
   later step past the cache/pattern/`AutoAllow` shortcuts, straight to the
   gate.
2. **Deny-pattern rules** (`PermissionBroker::deny_matches`, point 6's
   `then: deny`). Checked immediately after the root floor, before the mode
   gate, the cache, pattern allows, and `AutoAllow` — a deny rule beats every
   one of those unconditionally, regardless of mode or trust.
3. **Plan-mode denial.** If the session is in `PermissionMode::Plan` and the
   call's category is one plan mode does not permit, deny — checked above
   every allow path so a plan-mode session cannot be talked out of its
   denial by a cache hit or a pattern grant.
4. **Prompt-pattern rules** (`PermissionBroker::prompt_matches`, point 6's
   `then: prompt`). A match sets the same `must_reach_gate` accumulator step
   1 can set — it forces the call to the human gate, skipping the cache,
   pattern-allow, and `AutoAllow` shortcuts, including under `AutoAllow`
   mode, the one mode this step matters most in (there is no human already
   in the loop to catch what the rule would have caught).
5. **Cache** — a prior `AllowAlways` grant covering this call, consulted only
   if `must_reach_gate` is still false.
6. **Pattern-allow rules** (point 6's `then: allow`), gated by the
   metacharacter check on the tool's `RenderKind`, consulted only if
   `must_reach_gate` is still false.
7. **`AutoAllow` mode** — if none of the above resolved the call and the
   session mode is `AutoAllow`, allow. Consulted only if `must_reach_gate` is
   still false.
8. **`PermissionGate::check`** (point 5) — the human, or whatever gate
   implementation the embedder supplied. Reached whenever nothing above
   resolved the call, or whenever `must_reach_gate` was set by step 1 or 4.

Composition within a step is most-restrictive-wins and registration order
never changes the outcome — which deny/prompt rule matched first only
changes *which one is reported*, never *whether* the call is refused or
forced to the gate.

### The honest limit

**A trusted, allow-eligible pattern grant (step 6) or `AutoAllow` (step 7)
short-circuits a human prompt that would otherwise have happened at step 8.**
Read plainly, that is a widening — the call that would have reached a human
under `PermissionMode::Prompt` with no pattern grant installed does not reach
one once a grant exists. The reason this is acceptable is **not** the
ordering by itself: it rests on the type split between narrowing and
widening authority (point 6's admission stage — only a *trusted* `allow`
enters the evaluation set at all) plus the fact that every widening grant is
an **operator's own** act (an interactive "always allow," or an explicit
trust decision on a project file's `allow` half) — never something a plugin
or an untrusted file can install on the operator's behalf. The ordering
enforces that once a widening grant legitimately exists it cannot be
overridden by a *later*, weaker mechanism; it does not, by itself, establish
that the grant should have existed. Do not read this pipeline as a stronger
guarantee than that.

### The policy-chain overlay, if points 7 and 8 are ever built

`.design/extension-architecture.md` §5.1 specifies where a composed
`NarrowingPolicy`/`DecidingPolicy` chain would sit, argued from the same
`decide` this section just described: a **deny** half would run for every
call, including one that must already reach the gate — inserted as a new
step between plan-mode denial and prompt-pattern rules, i.e. before the cache
— because "root confinement outranks everything and cannot be widened" only
holds if a policy's deny is checked *before* any allow shortcut, not folded
in beside it. An **allow** half would run only when `must_reach_gate` is
false, only from a `DecidingPolicy`, only if at least one policy allowed and
none denied, and would never be cached — a policy's allow is authority that
does not outlive the call that earned it. **None of this exists.** It is
documented here because a reader building against this reference needs to
know the two invariants a future implementation would have to preserve
(never widen the root; a veto beats every allow path), not because either
half is live.

## Determinism and composition

- **Context editing (points 3, 4, 9).** Today there is exactly one
  `ContextHook`, so composition across *multiple* hooks is unreachable — the
  question does not yet arise in the running system. The design's answer,
  settled by `01KYTP2QYE00FJSQAQQ0E37JZP` and specified in
  `.design/extension-architecture.md` §16.3, for when it does: **every hook
  is evaluated independently against the same pre-hook payload, never
  against another hook's output** (chaining is rejected outright — it is
  exactly the "no fixed point under two rewriters" hazard restated for
  context). Exclusion (drop/mask) composes as a set union — two hooks masking
  the same record is not a conflict, `{X} ∪ {X} = {X}` — and addition
  (append) stays independent and attributed, with declaration order
  observable only as *presentation* order among non-conflicting appends,
  never a semantic tie-break. A same-target *replace* collision between two
  hooks has no principled order-independent merge and none is invented: it
  fails to exclusion (the segment is masked, neither replacement wins), named
  in a diagnostic — the less-informative outcome winning a genuine
  disagreement, never an arbitrary one.
- **Permission rules and verdicts (points 6, 7, 8).** Two-stage composition,
  live for point 6 today and specified identically for the still-unbuilt
  points 7/8: admission by trust (only a narrowing rule/verdict, or a
  trusted-and-granted widening one, enters the evaluation set), then
  most-restrictive-wins over the admitted set. Registration order never
  changes the outcome — this is why there are no priority numbers anywhere
  in the permission system: priorities invite an arms race over who edited a
  config file last.
- **Tool registration (point 1).** A set, keyed by name; a name collision
  across plugins is a build-time error, not a composition rule — there is
  nothing to compose, only a uniqueness constraint to enforce.
- **Observers (point 11 and `EventSink` generally).** Independent by
  construction; a slow or absent observer changes nothing about any other
  observer's delivery.

## Failure semantics

Fail-closed as a pattern, not a single prose assertion — every row below
either denies, drops, or falls through to the strictest available default.

| Point | Error | Timeout | Crash / panic | Malformed response | Unknown enum variant | Absent | De-trusted |
|---|---|---|---|---|---|---|---|
| Tool execution (2) | Model-visible `ToolError`, never an abort | No port-level deadline; cooperative via `ctx.cancel` | Caught per call by `ToolRunner`, becomes an error `ToolOutcome` | N/A — arguments pre-validated | N/A | Call fails to resolve, distinct from a running tool erroring | N/A (tools are not a trust subject today) |
| `before_request` (3) | No `Result` in the signature; a panic here is not caught the way a tool's is | None dedicated; bounded only by the agent's own configured budget deadline, if any | Propagates — not isolated per hook the way tool panics are | N/A, in-process Rust value | N/A | Skipped entirely; behavior is pre-hook | N/A |
| `on_overflow` (4) | Same as `before_request` | Same as `before_request`; retry count separately bounded (`MAX_OVERFLOW_ATTEMPTS = 2`) | Same as `before_request` | N/A | N/A | Falls through to hard `ContextTooLarge` | N/A |
| `PermissionGate::check` (5) | N/A — always returns a `PermissionDecision` | **None, by contract** — may block indefinitely; cancellation is expected to answer `Deny{"cancelled"}`, per-implementation | Not isolated — this is host-tier code the embedder supplied | N/A, in-process Rust value | N/A | Cannot be absent — `build()` requires one | N/A |
| Permission rules (6) | Malformed entry dropped, fail-closed | N/A, synchronous file load | N/A | Same as error | N/A (rules are not a wire enum yet) | No rules of that kind; falls through to the gate | Editing a trusted file's bytes de-trusts it silently — its `allow` half stops installing until re-trusted; `deny`/`prompt` are unaffected (they never needed trust) |
| Plugin rules (7, designed) | `on_failure`-shaped, design only | Design only, clamped `timeout_ms` | Design only | Design only, never guessed at | Design only | No producer exists; contributes nothing | Design only |
| Policy chain (8, designed) | `on_failure`, default `Deny`, **never `Allow`** | Clamped `timeout_ms`, design default 60 s; **excluded from any progress-reset rule** — a decision-bearing call's deadline never extends on a progress notification, so a hook that emits progress forever while never deciding cannot stall the session (§16.2d) | Design only | Design only | Design only | No chain exists; unaffected | Design only |
| `context.hook/1` (9, designed) | Design only | Design only, same clamped-timeout shape as 8 | Design only | Design only | Design only | No wire transport exists | Design only |
| Declarative hooks (13) | Fail-closed for `pre_tool_use`/`prompt_submitted` (the two that can deny); logged and swallowed (fail-open) for the five observation events | `pre_tool_use`/`prompt_submitted`: fail-closed, same as error. Observation events: logged and swallowed | Runner-reported (`HookFailure`), never a raw process panic reaching the caller | Unparseable `HookAnswer` stdout is a `HookFailure`, handled identically to any other runner error | An unrecognized `HookPermissionVerdict`/`ContextDelta` shape fails to deserialize (`HookAnswer`'s own `deny_unknown_fields` posture on `permission`) — see `conway_core::hook`'s own tests | A `[hooks]` block is parsed, deny-unknown-fields-strict, and validated (board item 01KZRZW5CWMVQ0GPRT4GX4RV5G) whether present or absent. **All seven core events now dispatch** — see point 13's Status row, which is normative — so a rule naming any of them DOES run, given an injected runner; a `match` on an event carrying no tool name is a load-time config error, never a silently-inert rule | Design only |

## Constraints this document keeps

**No built-in is privileged.** No point above claims a built-in reaches
something a third-party plugin cannot. Where a built-in genuinely does — an
embedder's own `ContextHook`/`PermissionGate`, which are host-tier code chosen
by whoever constructs `ConwayBuilder`, not extension-tier plugins — that tier
distinction is stated, never smoothed into "plugins can do this too."

**No invented composition rules.** No point here invents a composition rule
beyond what a recorded decision already settled. A genuine multi-party merge
(context replace collisions, point 9) is stated as deliberately unsolved,
rather than solved by an unstated tie-break.

**Fail-closed is stated, never assumed.** Every "on garbage" row says
explicitly what happens.

**No version numbers inline.** No conway version appears anywhere in this
document; historical framing routes to `CHANGELOG.md` at the repository root
rather than to a version string here, because a version inline goes stale
silently and a changelog entry does not.

## Out of scope

Tutorials and worked examples — see doc 4
([`authoring.md`](authoring.md)) and doc 5
([`cookbook.md`](cookbook.md)). Trust in depth — see doc 3
(`trust-and-security.md`, `01KYTP78T0NR20A9HV93D7E3AE`); this page states only what each point's
failure table needs ("de-trusted") and defers the mechanism. The wire
transport itself — `.design/d1-transport.md` is a closed design spike
(`01KYNN7K02MS59GCM5PKC1JDW9`, done as design, not as code); this page
describes points and contracts, which are transport-independent, not frame
formats.
