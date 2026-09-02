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
**designed-not-built** with the tracking item noted for the gap. A labeled forward declaration is a respectable
state; an unlabeled one is a trap. **The implemented rows now outnumber the designed ones** — the tree ships
tool registration, one `ContextHook`, one `PermissionGate`, file-based
permission rules, declarative script hooks, plugin commands, plugin-fired
events, and the two observer-class wire points (`observe/1`,
`status.declare/1`); what still reads designed-not-built is the composed
inference-evaluated policy chain, a remote `context.hook/1` transport, and a
plugin-declared tool-hide selector, plus the TTL sweep that would age a
`status.declare/1` contribution out once its declared `ttl_ms` passes (the
render path that displays those contributions, and the startup snapshot
that populates it, both shipped — see point 12 below). The `subagent_mode`
field is not in that "still awaiting work" list: it is a **deliberate
absence**, abandoned 2026-08-27 alongside the inference-evaluated hook it
would have belonged to (decision record `01M128AP39WXE01BBZV4RENC4M`) — see
point 14.

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
| Status | **Implemented** or **designed-not-built**, with the tracking item noted for the latter |

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
| Status | **Implemented**, in-process (`Plugin::manifest`/`Plugin::tools`, `crates/conway-core/src/ports/plugin.rs`, exercised by `crates/conway/tests/plugin_surface.rs` and `crates/conway/tests/plugin_builtin_parity.rs`). **The wire projection is now ALSO implemented, as a thin, disclosed slice**: `tool.spec/1` (`conway_plugin_subprocess::wire::WireManifest`/`WireTool`) is a real, one-shot-exec request a `SubprocessPlugin` sends once at `SubprocessPlugin::discover`, projecting `PluginManifest`/`ToolSpec` over JSON — see [`subprocess-plugins.md`](subprocess-plugins.md) for the full contract. Narrower than the design's own persistent-connection shape (disclosed on that page) |

**`required_host_caps` is now consulted at registration.** A plugin declares
what it needs (a `Vec<HostCapability>` -- an OPEN, `#[non_exhaustive]` enum
in `crates/conway-core/src/ports/plugin.rs`: two core-blessed bare names
plus a shape-checked `Named(String)` catch-all, not a free-form
`Vec<String>` the host never validates, since a malformed name still fails
to parse); the host separately grants, never implied by trust alone. The
`conway` builder consults the field once per installed plugin at the
manifest-validation seam (right where the duplicate-plugin-id check already
runs, `crates/conway/src/builder.rs`), comparing each declared cap against
what THIS host offers (`conway::HostCaps::from_config`); a cap the host does
NOT offer is a `PluginError::MissingHostCapability` naming both the plugin
and the cap, and the plugin is refused -- the narrowing direction (a plugin
declares what it needs; the host refuses to load it if the host can't
provide it). Empty `required_host_caps` (the common case -- "needs nothing
the host might lack") is always satisfied. The two core-blessed variants
each map to something real: `subagent` (fork/spawn a child session;
required by the `conway.subagent` built-in, offered by the `conway` runtime
which always provides a `SubagentHost`) and `persistent_transport` (the
persistent NDJSON `tool/1` channel; offered iff at least one
`[plugins].subprocess[]` entry is configured `persistent` -- a plugin
requiring it against a one-shot-only host is refused). A plugin may also
declare any OTHER well-formed name via the open `Named` variant; nothing
this host builds offers one today, so it is refused at this same gate. The
wire projection carries the field too: `conway_plugin_subprocess::wire::
WireManifest::required_host_caps` (`#[serde(default)]`). A MALFORMED cap
tag still fails closed AT PARSE, unchanged; a WELL-FORMED but
previously-unknown tag now parses (resolving to `HostCapability::Named`)
and is refused HERE, at this registration gate, not at parse -- the
fail-closed guarantee did not weaken, it moved and got sharper, naming both
the plugin and the cap either way. The glossary entry in `concepts.md` now
states it is consumed and open ("consulted at registration, never implied
by trust alone"). This is no longer a declared field with zero consumers;
it is a capability gate wired into the build.

**`optional_host_caps` -- label: carried on the wire and honoured
(degrade-and-announce), no first-party producer today** (operator ruling,
harness gap review 2026-09-01 finding 9). The sibling field rides the SAME
per-plugin loop right after the check above: a cap the host does NOT offer
never refuses the build -- the plugin loads degraded, and the degradation
is always announced (`ConfigWarning { code: OptionalHostCapabilityMissing }`
plus a `tracing::warn!`, naming both). See
[`subprocess-plugins.md`](subprocess-plugins.md)'s "Host capabilities"
section for the full mechanism; no shipped plugin, first-party or
otherwise, sets it to a non-empty list today.

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
| Status | **Implemented**, in-process. **A thin, disclosed slice of the wire form is now ALSO implemented**: `tool/1` (`conway_plugin_subprocess::SubprocessTool::invoke`) spawns the plugin's own command fresh per call, projecting `ToolCall`/`ToolOutput`/`ToolError` over JSON — see [`subprocess-plugins.md`](subprocess-plugins.md). **Narrower than this row's own "RPC-shaped `ToolCtx` projection"**: a subprocess tool receives only `{tool, call_id, arguments}`, never `ToolCtx` itself (no `cwd`, no `chdir`, no `subagents`, no `plugin_events` reach a subprocess) — this host enforces `ctx.cancel` and the timeout on the subprocess's behalf, from the outside, rather than projecting those capabilities across the wire |

**`ToolCtx.subagents` never lets a tool act as a different agent.** It is a
`SubagentHandle` (`crates/conway-core/src/ports/mod.rs`) bound to
`ToolCtx.agent_id` at construction, not a raw `Arc<dyn SubagentHost>` a tool
could redirect — closing the exact cross-tree exfiltration shape an earlier fix already closed.

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
| Ordering | Exactly one `ContextHook` per embedder (`ConwayBuilder::with_context_hook` takes one `Arc<dyn ContextHook>`, not a `Vec`) — but composition across *multiple* hooks IS reachable as of board item `01KZRZZP6A4A27R3EN0HQAENBS`: a configured `request_assembled` script hook (point 13) is evaluated against the SAME pre-edit payload this Rust hook sees, and the two compose under §16.3's union rule (exclusions union, appends concatenate) rather than chaining. Two or more script hooks on the same event compose identically. See point 13's own entry for the script-hook side |
| Status | **Implemented.** `ContextHook::before_request` (`crates/conway-core/src/ports/plugin.rs`), invoked from `AgentLoop::run_inner` (`crates/conway-runtime/src/agent_loop.rs`), exercised end-to-end and by that port's own unit tests (`before_request_can_drop_a_segment`). Still registered and invoked exactly as before this item -- `01KZRZZP6A4A27R3EN0HQAENBS` added a SIBLING (script) editor at the same seam, never a second Rust-hook registration path |

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
conway never reads it.** an
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
| Ordering | One Rust hook, same as `before_request` -- but, as of board item `01KZRZZP6A4A27R3EN0HQAENBS`, composing with every configured `context_overflow` script hook (point 13) the SAME way: both are evaluated against the same pre-edit payload and their edits union rather than chain. The runtime, not any hook, bounds the retry: `MAX_OVERFLOW_ATTEMPTS = 2` (`crates/conway-runtime/src/agent_loop.rs`) caps how many times `route_and_attempt` will call `on_overflow`/dispatch `context_overflow` for one turn — a hook that keeps returning a still-too-large payload cannot hang the turn |
| Status | **Implemented** — and genuinely invoked, which was not always true. See the boundary below |

**The exact boundary on when this fires, stated precisely because it is easy
to overstate.** `on_overflow` fires **only** when the router or the attempt
engine rejects with `RoutingError::ContextTooLarge` — the case where *every*
candidate in the resolved chain was rejected **solely** on the headroom gate
(`conway_plugin_routing::router::DeclarativeRouter::resolve`'s own module
doc, closing an earlier open question about that exact boundary). A candidate that fails on headroom **and**
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

**Two designed extensions to this point -- one now built, one still not.**
The redirect decided `before_request` should also
be able to propose a *durable* mask — a `target_seq` set the runtime diffs
against `LogRecord::ContextMask` and persists — alongside its existing
ephemeral, per-request edit. `LogRecord::ContextMask`
(`crates/conway-core/src/log.rs`) is real, persisted, and consumed by
`apply_context_mask` (`crates/conway-session/src/resolver.rs`), but **still
has no producer anywhere in the tree** — board item `01KZRZZP6A4A27R3EN0HQAENBS`
considered and explicitly declined to become one (decision
`01KZWVWY05Z8T309YS2YZXXHJ6`'s reopening condition is a measured demand the
existing primitives cannot serve, not a rediscovery of the gap). No hook,
Rust or script, can express "mask this durably" today; every edit at points
3/4/13 is ephemeral, scoped to one request, and invisible on the next turn
unless repeated. This remains closer in scope to
`01KZ844ZXZMVRWC7ZANT7PSM6X` (the `context.hook/1` replace gap) than to
anything with its own tracker.

**The OTHER extension -- a configured SCRIPT able to make the identical kind
of ephemeral edit `ContextHook` already could, append-only -- is now built**
(board item `01KZRZZP6A4A27R3EN0HQAENBS`): see point 13's `request_assembled`/
`context_overflow` entries and `crates/conway-runtime/src/context/
script_hook.rs`. `crate::context::script_hook`'s own module doc states in
writing how its in-memory `AppliedContextEdit` relates to `ContextMask`'s
vocabulary (the ephemeral, per-request analogue of the same "fold away,
reversibly" idea) precisely so the durable-mask gap above and this
now-built ephemeral one converge onto one vocabulary later rather than
diverging into two.

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
| Ordering | Two-stage composition (the extension design, confirmed live in `PermissionBroker::decide`): **admission** — a `deny`/`prompt` rule installs from any file, trusted or not (narrowing has no failure mode worth gating on trust); an `allow` rule from a *project* file installs only once its exact bytes match a recorded trust decision (`TrustStore`, see `concepts.md`'s "Trust" section) — then **most-restrictive-wins** over the admitted set: any `deny` beats every `prompt` beats every `allow`. Registration order (which file, which rule within a file) never changes the outcome, only which single matching rule is reported |
| Status | **Implemented**, including the structured `{ select, when, then }` form. This corrects the item spec that produced this document: it named the structured rule form (historical label "F12") as pending at writing time. That item is now **done** — `Rule`/`Select`/`When`/`Then` are real, exercised by `permission_pattern::f12_tests` and the real-stack seam `crates/conway/tests/structured_rule_seam.rs`, and documented operator-facing in `docs/permissions.md`'s "The structured `rules` array" section |

**A second correction, same shape.** The item spec also named the
convergence of conway's three control-character sanitizers (historical label "F2/F3") as pending. It too is **done** — a
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
| Status | **designed-not-built**, but with real, tested guard code already in place ahead of the producer: `PatternOrigin::Plugin` (`crates/conway-core/src/permission_pattern.rs`) exists as a variant, is exercised by `crates/conway-runtime/tests/permission_broker.rs` (proving the allow-rejection holds even though nothing constructs this variant outside tests today), and its own doc names the reason precisely — "the invariant rests on a guard, not on the absence of a transport." **Nothing tracks building this producer.** The declarative `hooks` charter does not build it — none of its nine children gives `Plugin` a `rules()` method or constructs `PatternOrigin::Plugin` outside a test; that charter's own spec is scoped to the script-hook surface, a different axis. **Correction: this row's forward reference is now only partially right.** `pre_tool_use` IS a real, dispatched capability (point 13 below) — but it does NOT produce `PatternOrigin::Plugin` rules, and its answer shape is not "allow/deny/deny-with-feedback": `HookPermissionVerdict` is `no_opinion` (proceed) or `deny { reason }` ONLY, with no `allow` variant at all, by construction (a hook may only narrow, never widen). `pre_tool_use` dispatch feeds `PermissionBroker::decide` directly, as an independent narrowing-only chain step (exactly one narrowing chain, never configurable policy branching), never through this row's `Plugin::rules()`/`PatternOrigin::Plugin` producer, which remains exactly as unbuilt as before this item |

### 8. Composed inference-evaluated permission policy — `permission.policy/1`

| Field | Value |
|---|---|
| Kind | Participant |
| Receives | Design only: a `PolicyRequest` carrying `agent_id`, `agent_path`, `session`, `tool`, `category`, raw `arguments`, `rendered`, `render_kind`, `call_id`, `cwd`, `root`, `mode`, `must_reach_gate` |
| May return | Design only: a `NarrowingPolicy` returns `Deny { reason } | Abstain` (no `Allow` variant exists on its type); a `DecidingPolicy` returns `Deny { reason } | Abstain | Allow { reason }` — a type-level split, not a runtime flag, specifically because "may only narrow" must be a property of the return type an inference-evaluated policy cannot talk its way around |
| On error | Design only FOR THIS POINT'S OWN inference-evaluated chain: `on_failure`, default `Deny` — **never `Allow`**. **Correction: this exact `on_failure: Deny | Prompt`, default-`Deny`, never-`Allow` shape is now BUILT — for point 13's declarative `pre_tool_use` hook, not for this point's own composed chain** (board item `01M0X1AH44SNMK5TZ507K30QNP`, `conway_core::hook::HookOnFailure`). See point 13's own "On error" row |
| On timeout | Design only: a declared `timeout_ms`, clamped by an operator-configured maximum (design default 60 s) |
| On garbage | Design only FOR THIS POINT'S OWN chain: an unparseable verdict is treated as `on_failure`, never guessed at. **Correction: also now true of point 13's declarative `pre_tool_use` hook** — an unparseable `HookAnswer` is `HookFailure::UnparseableAnswer`, routed through the SAME per-registration `on_failure` policy as any other runner failure, never given a silently-guessed verdict of its own |
| When absent | The WIRE projection (built): a plugin that does not declare `permission.policy/1` in its `initialize/1` `points` array loads normally and contributes no wire policy — the host advertises the point but does not require it (advertising != requiring; the participant refusal is version-gated, not presence-gated). The full inference-evaluated CHAIN (design only): `PermissionBroker::decide`'s real ordering (below) still has no per-call `PolicyRequest` step — the wire projection is installed as static `PatternOrigin::Plugin` deny/prompt rules, not as a composed chain |
| Ordering | Wire projection (built): the host advertises `permission.policy/1` in `initialize/1`, exchanges a static per-tool narrowing declaration once at session open (after `initialize/1`, before any `tool/1` call), and installs each declared rule as a `PatternOrigin::Plugin` deny or prompt rule on the `PermissionBroker` (abstain rules install nothing). The operator's own `permissions.json` / `PermissionMode` still wins — the wire policy is **advisory-under-enforcement, narrowing only**: there is no `Allow` variant on the wire verdict enum (`deny` / `prompt` / `abstain`), so a plugin can never widen. The operator's deny fires at `decide`'s step 2 (deny-pattern) before plan-mode and all allow paths; a plugin `prompt` forces the gate (sets `must_reach_gate`) even under `AutoAllow`. Full inference-evaluated CHAIN (design only): most-restrictive-wins, admission gated by trust for the allow half only (identical two-stage shape to point 6) |
| Status | **wire projection built; composed inference-evaluated chain design-only.** Board item `01M03VKJG7JJ0JEKY265WA7MJ7` (`permission.policy/1`) delivered the wire projection over the persistent NDJSON transport: the host advertises the point in `initialize/1`, a plugin that declares it at a supported version (1) exchanges a static per-tool narrowing declaration once at session open, an unsupported version is REFUSED at discover with a typed `HandshakeRefused` naming the mismatch (participant rule, never silently never-run), a malformed answer fails closed (`HandshakeMalformed`), and the declared rules are surfaced via `Plugin::permission_rules` and installed by the `conway` facade as `PatternOrigin::Plugin` deny/prompt rules on the `PermissionBroker`. The `conway-runtime` inline `plugin_permission_subordination_*` tests pin the subordination boundary (operator deny beats plugin abstain/prompt; plugin prompt forces the gate under `AutoAllow`; no `Allow` variant exists on the verdict type). What REMAINS design-only is the full inference-evaluated `PolicyRequest`/`NarrowingPolicy`/`DecidingPolicy` trait from the rows above — a per-call, argument-aware, inference-evaluated policy chain with trust-gated admission. The wire projection built here is the static, per-tool narrowing declaration only; it is not that composed chain and does not track building it. **Correction: this row's `on_failure` vocabulary (design only, above) is now only partially right, the same way point 7's own correction reads.** Board item `01M0X1AH44SNMK5TZ507K30QNP` built `on_failure: Deny | Prompt`, default `Deny`, never `Allow` — but for point 13's declarative `pre_tool_use` hook (`HookEntry::on_failure`/`conway_core::hook::HookOnFailure`), a per-REGISTRATION, non-inference-evaluated policy over a hook's OWN runner outage, not a per-CALL, argument-aware, inference-evaluated `PolicyRequest`/`NarrowingPolicy`/`DecidingPolicy` step. Nothing about the composed chain this row otherwise describes exists yet; only the narrower on/off shape — Deny-or-Prompt on a script hook's own outage — is real |

### 9. Remote context-editing parity — `context.hook/1`

| Field | Value |
|---|---|
| Kind | Participant |
| Receives | Design only: the wire projection of `ContextHookCtx` plus `ContextPayload` |
| May return | Design only: `{ appends, excludes, durable_excludes }`, composed across every hook — in-process and remote alike — under the union rule (§16.3, below) |
| On error / timeout / garbage / absent | Design only — inherits the same shapes as points 3/4 above, projected onto a wire boundary that does not exist yet |
| Ordering | Design only: exclusion is a set union (order-independent); a same-target *replace* collision between two hooks fails to exclusion rather than picking a winner |
| Status | **designed-not-built** for the REMOTE wire transport specifically -- unchanged by board item `01KZRZZP6A4A27R3EN0HQAENBS`, which deliberately did not build this point (see that item's own text: shipping full replacement for shell scripts first, ahead of this point settling, would have created a FOURTH inconsistent surface rather than resolving the existing three). Named replacement for the retired `context.append/1` point, which the redirect superseded because it gave a remote plugin strictly *less* than the in-process `ContextHook` already gives (append-only, no edit/drop) — the exact built-in/third-party inversion this project forbids. Even the *specification* of `context.hook/1` has an acknowledged gap: it can append and exclude but has no same-target **replace** primitive, tracked as `01KZ844ZXZMVRWC7ZANT7PSM6X`, still `open` as of this writing. **What DID change:** the append/exclude vocabulary this point specifies is now genuinely IMPLEMENTED for the in-process SCRIPT-hook surface (point 13's `request_assembled`/`context_overflow`), using the identical `{appends, excludes}` shape (`conway_core::hook::ContextDelta`) this point's own "May return" row names, minus `durable_excludes` (still design-only everywhere, per point 4's own disclosure). That converges one MORE of the three surfaces `01KZ844ZXZMVRWC7ZANT7PSM6X` must eventually reconcile (in-process trait, declarative script hook, remote plugin) onto one vocabulary rather than three -- it neither closes nor widens that item's own replace-primitive gap, which remains exactly as open as before |

### 10. Tool-announcement hiding as a plugin-declared selector — `context.tools/1`

| Field | Value |
|---|---|
| Kind | Declarative |
| Receives | Design only: nothing live — a selector naming tools this plugin's own declaration hides from announcement |
| May return | Design only: a `ToolSelector`-shaped exclusion set |
| On error / timeout / garbage | Design only: a selector matching nothing is a registration error, per the extension design's own row for this point |
| When absent | Today: the only way to narrow `ContextPayload.tools` is an embedder-supplied `ContextHook::before_request` doing it imperatively (point 3) — there is no way for a `Plugin`, as such, to *declare* a static hide-list the way this point would let it |
| Ordering | Design only: a set union over every plugin's declared hides |
| Status | **designed-not-built.** No `Plugin` method for declaring a tool-hide selector exists. **Nothing tracks it.** The declarative `hooks` charter does not build a `Plugin`-declared tool-hide selector among its nine children — it is a plugin-reachable extension point, not the script-hook surface the charter decomposed |

### 11. Event observation by a plugin — `observe/1`

| Field | Value |
|---|---|
| Kind | Observer |
| Receives | An `Event` (`crates/conway-core/src/event.rs`), filtered by a selector the plugin declares at engagement (`["*"]` for every event, or a named-tag set). The host serializes each matching `Event` to a no-`id` `observe/1` notification and writes it to the plugin's stdin over the persistent NDJSON transport |
| May return | Nothing — the point has no reply channel, structurally. This is the one place "the shape itself forbids a denial" (see `concepts.md`'s "Observers vs participants") is easiest to see: there is no return type to smuggle a decision through. The engagement exchange (`PersistentObserveRequest`/`PersistentObserveAnswer`) is the ONLY round trip; everything after it is one-way host→plugin |
| On error | Ignored — an observer cannot fail the run by construction. A write that times out or hits a closed stdin sets a `broken` flag on the sink and the host stops forwarding to that plugin, but the session itself stays alive (`tool/1` still answers, proven by `a_subscribed_observe_plugin_receives_events_and_stays_alive`) |
| On timeout | Each per-write forward is bounded by a timeout; a write that exceeds it breaks the sink (observer-local degrade), never the session |
| On garbage | An unknown `Event` variant is IGNORED — the one enum-versioning case where "ignore" is the *right* answer, per the extension design, because an observer changes nothing by definition. The selector filter drops it before it reaches the wire if the tag is not in the named set; `["*"]` forwards it and the plugin's own reader is responsible |
| When absent | `EventSink::emit` (`crates/conway-core/src/ports/events.rs`) is real and invoked constantly; the embedder-level subscription (`conway::EventStream`, `crates/conway/src/event_stream.rs`) remains the primary consumer. A slow consumer is dropped from delivery and sees `Event::Lagged { skipped }` on its next successful receive rather than stalling the runtime — that guarantee is real and tested, for the embedder's own stream AND now for a plugin's `observe/1` sink (the host's bus→sink forwarding task uses the identical lossy-with-notice discipline, see the Status row) |
| Ordering | Independent by construction — multiple subscribers never interact; each engaged plugin has its own bounded channel and writer task |
| Status | **Implemented** as the wire half over the persistent NDJSON transport (board item `01M03VKQ738DTGHHK2C4RWXC0E`), AFTER `initialize/1` and `permission.policy/1`. The host advertises `observe/1` at version 1 in `initialize/1`; a plugin that declares it at a SUPPORTED version (1) engages via `PersistentObserveRequest`/`PersistentObserveAnswer` (a `["*"]` or named-tag selector); a plugin that declares it at an UNSUPPORTED version DEGRADES — the host WARNS and LOADS WITHOUT the point, it does NOT refuse (the observer rule, the OPPOSITE of `permission.policy/1`'s participant REFUSAL — see `an_unsupported_observe_version_degrades_not_refuses`); a plugin that declares NEITHER point loads normally and contributes no observe sink (presence-gated, proven by `a_plugin_declaring_neither_point_loads_normally_with_no_observer_surfaces`). After engagement, the `conway` facade's `build` spawns a bus→sink forwarding task per `Plugin::observe_sink()` that drains `EventBus::subscribe()` and calls `EventSink::emit` — the SAME lossy-with-notice delivery `EventStream` uses (`Event::Lagged` passthrough from the broadcast channel, PLUS a second-level bounded `mpsc` channel with drop+warn so a plugin that blocks its own read loop can NEVER stall the host turn — the bounded queue fills, the host drops the event with a `tracing::warn`, and the host turn proceeds). Wire types: `conway_plugin_subprocess::wire::{PersistentObserveRequest, ObserveSelector, build_observe_notification, parse_persistent_observe_response}` (`crates/conway-plugin-subprocess/src/wire.rs`); session plumbing: `PersistentSession::request_observe`/`observe_sink` (`crates/conway-plugin-subprocess/src/session.rs`); trait surface: `Plugin::observe_sink` (`crates/conway-core/src/ports/plugin.rs`). **What REMAINS design-only:** no IN-PROCESS `Plugin` subscribes to the event stream through `observe_sink` today (the trait method defaults to `None`; only `SubprocessPlugin` overrides it), and there is no operator-facing render path for observed events — the wire half, the trait surface, and the facade forwarding are built; an in-process observer and any TUI surface built on top are not |

### 12. UI status contribution — `status.declare/1` / `status/1`

| Field | Value |
|---|---|
| Kind | Declarative (`status.declare/1`) + Observer output (`status/1`) |
| Receives / May return / failure modes | A plugin declares per-key `{ max_len, ttl_ms }` via `status.declare/1` at engagement, then PUSHES `status/1` notifications as no-`id` inbound NDJSON lines on its stdout: `{ "op": "status/1", "key": "..", "status": "..", "value": ".." }`. The host's reader routes no-`id` lines to a bounded notification channel (NOT `kill_all(MalformedFrame)`, the pre-observer posture), and a handler task parses each `status/1` line into a `WireStatusContribution { key, status, value }` and stores it by `key` in the session's shared status map. An unknown `ResultStatus` tag DEGRADES to `Failed { error: "unknown status tag: <tag>" }` (the compatibility table's `ResultStatus` row, never `Completed`) so the degradation is auditable in the value itself. A stale value expires at snapshot time and the render path never calls a plugin or blocks on one — the snapshot is a polled `HashMap` read, not a live call |
| On error / timeout / garbage | A `status/1` line that fails to parse is dropped with a `tracing::warn` (never kills the session — the notification channel is observer-class). A FULL notification channel drops the line with a warn (bounded, drop+warn, never blocks the host turn — the SAME discipline `observe/1`'s bounded sink uses, for the SAME reason: an observer that blocks its own read loop must not stall the host turn). An unknown `ResultStatus` tag degrades to `Failed`, never to a session error |
| When absent | `conway-cli`'s status line still reads only conway's own computed state; a plugin that does not declare `status.declare/1` contributes nothing (`Plugin::status_contributions` defaults to an empty `Vec`, and the host never sends an engagement request for the point — presence-gated, same rule as `observe/1`) |
| Status | **Implemented** as the wire half over the persistent NDJSON transport (board item `01M03VKQ738DTGHHK2C4RWXC0E`), AFTER `initialize/1`, `permission.policy/1`, and `observe/1`. The host advertises `status.declare/1` at version 1 in `initialize/1`; a plugin that declares it at a SUPPORTED version (1) engages via `PersistentStatusDeclareRequest`/`PersistentStatusDeclareAnswer` (per-key `StatusDeclaration { key, max_len, ttl_ms }`); a plugin that declares it at an UNSUPPORTED version DEGRADES — the host WARNS and LOADS WITHOUT the point, it does NOT refuse (the observer rule, the OPPOSITE of `permission.policy/1`'s participant REFUSAL — see `an_unsupported_status_declare_version_degrades_not_refuses`); a plugin that declares NEITHER point loads normally and contributes no status (presence-gated). After engagement, the host's reader routes no-`id` stdout lines to a bounded `mpsc` notification channel (a SECOND reader task is NOT spawned — the EXISTING reader gained a no-`id` arm that `try_send`s into the channel, preserving the single-reader invariant), and a handler task drains the channel, parsing `status/1` lines into `WireStatusContribution` and storing them by `key`. `Plugin::status_contributions` returns a live snapshot of that map (polled — proven by `a_status_declaring_plugin_surfaces_contributions_with_unknown_degraded_to_failed`, which polls until both a known `Completed` and an unknown-tag-degraded-to-`Failed` contribution surface). Wire types: `conway_plugin_subprocess::wire::{PersistentStatusDeclareRequest, StatusDeclaration, WireStatusContribution, parse_status_notification, parse_persistent_status_declare_response}` (`crates/conway-plugin-subprocess/src/wire.rs`); session plumbing: `PersistentSession::request_status_declare`/`status_contributions` (`crates/conway-plugin-subprocess/src/session.rs`); trait surface: `Plugin::status_contributions` + `PluginStatusContribution` (`crates/conway-core/src/ports/plugin.rs`); facade wiring: `ConwayBuilder::build` collects the snapshot into `Conway::plugin_status_contributions` (`crates/conway/src/builder.rs`, `crates/conway/src/conway.rs`). **The TUI render path shipped (board item `01M0X1B7Z41J57N6YP2JFZ2AZW`):** `App::new` copies the snapshot into `AppState::plugin_status_contributions` at startup (`crates/conway-cli/src/tui/app/startup.rs`), and `crates/conway-cli/src/tui/view/status.rs` reads it to render each contribution on the status line — `conway-cli`'s TUI status line now DOES read `plugin_status_contributions`; it is not merely POLLED, it is RENDERED. **What REMAINS design-only:** the `ttl_ms`/`max_len` declarations are still only CARRIED by the wire type and stored on the session — no TTL sweep runs anywhere in the pipeline, so the snapshot does not yet EXPIRE a stale value (`crates/conway-plugin-subprocess/src/session.rs`'s own doc: "the ttl/expiry RENDER path itself stays design-only"); a plugin that stops updating a key leaves its last-pushed value on screen indefinitely rather than aging it out. This row does not over-claim in either direction: the plugin PUSHES status, the host POLLS and now RENDERS it, but nothing yet ages a stale value out |

### 13. Declarative script-fired hooks — the `hooks` configuration block

| Field | Value |
|---|---|
| Kind | Declarative (registration) wrapping whatever kind the named event actually is (Observer for a logging hook, Participant for `pre_tool_use`) |
| Receives / May return | **All NINE core events are real; nothing is design-only here anymore** (board item `01KZRZZP6A4A27R3EN0HQAENBS` added `context_overflow`, the ninth). A rule's command receives `{"name":"<event>","payload":{...}}` on stdin (`conway_core::hook::HookInvocation`/`HookEvent`) and answers on stdout with a JSON `conway_core::hook::HookAnswer`, whose `permission` field (`HookPermissionVerdict`) may be `"no_opinion"` (proceed, the default) or `{"deny":{"reason":...}}` (refuse the call) — read only by `pre_tool_use` and `prompt_submitted`, the two events that can deny. **`request_assembled` and `context_overflow` additionally read `HookAnswer.context`** (`conway_core::hook::ContextDelta { appends, excludes }`) -- every OTHER event ignores that field entirely. **There is no `"allow"` shape — the type has no such variant, by construction** (a hook may only narrow a permission verdict, never widen one), **and there is no "replace" shape either — `ContextDelta` has only `appends`/`excludes`**, by the identical construction argument, applied to context instead of permission (`crates/conway-runtime/src/context/script_hook.rs`'s own module doc has the full type-level proof). **A rule may additionally narrow WHICH occurrences of `pre_tool_use`/`post_tool_use` it fires for**, via `match`: an exact tool name (`"bash"`, `"fs.write"`) or a `*`-glob against the tool's whole name (`"fs.*"`), checked by `conway_core::hook::tool_matcher_matches`. Absent `match` (the default) fires the rule for every occurrence of `event`, unchanged from before this field existed. `match` on any event that carries no tool name (`session_starting`, `child_spawned`, `request_assembled`, `context_overflow`, `child_reported`, `prompt_submitted`) is a load-time config error naming the rule's `id` (`crate::config::merge::validate`), never silently ignored. **Evolution contract (board item finding 9, harness gap review 2026-09-01):** `HookInvocation`/`HookEvent`/`HookAnswer`/`ContextDelta` are `#[non_exhaustive]` structs (construct one via its own `::new`, never a Rust struct literal, from outside `conway-core`) and `HookPermissionVerdict`/`HookOnFailure`/`HookOrigin` are `#[non_exhaustive]` enums that may gain a variant in a future release without that being a breaking change for a compiled plugin binary; a hand-rolled `match` on any of the three enums (rather than reading `serde_json`/JSON directly, which already tolerates an unrecognized shape gracefully) must carry a wildcard arm, and — for `HookPermissionVerdict` specifically — that arm must treat an unrecognized verdict as `deny`, the same fail-closed posture `PermissionBroker` itself gives one. |
| On error / timeout / garbage / absent | **`prompt_submitted`: unconditionally fail-closed** — this event carries no `on_failure` field at all; a missing/unexecutable command, a timeout, a nonzero exit, or unparseable stdout is `HookFailure`, treated as a denial, exactly as before. **`pre_tool_use`: fail-closed BY DEFAULT, per registration, via `HookEntry::on_failure`/`PreToolUseHookSpec::on_failure` (board item `01M0X1AH44SNMK5TZ507K30QNP`)** — a missing/unexecutable command, a timeout, a nonzero exit, or stdout that fails to parse as `HookAnswer` are ALL `HookFailure` (`conway_core::error::HookFailure`), and `PermissionBroker::pre_tool_use_hook_denial` resolves that failure through THIS registration's own `on_failure` policy: `Deny` (the default, and today's exact byte-for-byte behavior for a registration that never sets the field) denies outright; `Prompt` forces the operator's own gate instead — never a denial, and never `Allow` (`HookOnFailure` has no such variant, the identical structural guarantee `HookPermissionVerdict` already gives a hook's own verdict). **This is a DIFFERENT fact from a hook's own explicit `{"deny":{"reason":...}}` verdict, and the two now take different paths, not merely different rendered text** — see `conway_core::hook::HookOnFailure`'s own doc and `docs/vision/DESIGN-permission-modes.md` §3a/§3c for the full argument; `on_failure` is consulted ONLY when the runner itself could not be reached, never for a hook that ran and had an opinion. **The remaining seven events fail OPEN**: `post_tool_use`, `session_starting`, `child_spawned`, `request_assembled`, `context_overflow`, `child_reported` — a failing hook is logged (`tracing::warn`) and the thing it observed/would have edited is unaffected. For `request_assembled`/`context_overflow` specifically, "unaffected" means the pre-hook payload is used exactly as if that hook had answered with an empty `ContextDelta` -- there is no partial application of a failed hook's answer, because there is no answer to partially apply. |
| Ordering | `pre_tool_use`/`prompt_submitted`: rules are consulted in the order `ConwayBuilder::build` filtered them from `hooks.rules[]`; the FIRST denying hook wins (order-independent for the boolean outcome — a `deny` beats a `no_opinion` however many hooks run, so which hook happens to be checked first only changes which hook's `reason` is reported, never whether the call is denied). The observation events (`post_tool_use`, `session_starting`, `child_spawned`, `child_reported`): every subscribed, matcher-satisfying hook runs, in configured order; a failure never stops a later hook from running. The context-editing events (`request_assembled`, `context_overflow`): identical dispatch order and failure isolation, but every ANSWERING hook's `ContextDelta` also COMPOSES (§16.3's union rule below) -- exclusions union across every hook that answered (including a Rust `ContextHook` at the same seam, points 3/4), and appends concatenate in the SAME configured order, each attributed to its own hook id via `Provenance::SystemNote { reason: "script_hook:<id>" }`. |
| Status | **All NINE core events are DISPATCHED.** `pre_tool_use`; the observation-only events `post_tool_use`, `session_starting`, `child_spawned`, `child_reported`; the context-editing events `request_assembled`, `context_overflow` (board item `01KZRZZP6A4A27R3EN0HQAENBS`); and `prompt_submitted`, which may DENY but may never MODIFY. **`prompt_submitted` fires at BOTH submission sites** — `Runtime::start_root` for a session's first prompt and `Runtime::prompt` for a follow-up — before the text reaches the agent loop, with `{text, agent_id, session, first_prompt}`. It fails CLOSED like `pre_tool_use`, and a denial surfaces to the CALLER as `RuntimeError::PromptDenied`, never to a model as a tool error, since there is no model turn yet to report into. **It cannot rewrite a word of what the user typed, and that is a TYPE guarantee rather than an unwired path:** the dispatch reads only `HookPermissionVerdict`, whose whole vocabulary is `no_opinion` and `deny { reason }` — no variant and no field can carry replacement text back, and `HookAnswer.context` is ignored here (the extension design: the user's own words are the one thing in the pipeline nothing gets to launder). **The observation tier (`post_tool_use`/`session_starting`/`child_spawned`/`child_reported`) cannot deny and fails OPEN, which is the opposite of `pre_tool_use` and is deliberate:** the thing it observes has already happened, so breaking a working operation because a logging script timed out would be the wrong direction. `conway_runtime::hook_dispatch::HookDispatcher::dispatch` returns `()`, so a failing observation hook is logged via `tracing::warn` and cannot propagate; `post_tool_use` fires at `ToolRunner`'s `ToolCallFinished` seam with `{call_id, tool, is_error, preview, agent_id, agent_path, session}` and honors `match` against `tool`; `session_starting` fires ONCE per `Runtime::start_root` (never per turn, and never on `resume_root`) with `{agent_id, session, cwd}`; `child_spawned` fires at the single `SubagentHost::start` that BOTH fork and spawn share, with `{child_id, parent, caller, mode, session}`; and `child_reported` fires for every terminal `AgentResult` that crosses back to a parent — both a normal completion (`AgentLoop::finish`) and a supervisor-synthesized one (`conway_runtime::supervisor`: a panic, or a task unresponsive past its grace window) — with `{agent_id, parent, session, result}`, gated on the same publish-race winner `Event::AgentFinished` already uses at each site so it fires exactly once per agent; it NEVER fires for a root's own finish, since a root has no parent for a result to cross back to.

**`request_assembled` and `context_overflow` are the CONTEXT-EDITING tier, not observation-only, as of board item `01KZRZZP6A4A27R3EN0HQAENBS` (correcting this doc's own earlier claim that `request_assembled` "structurally cannot" edit).** They still fail OPEN, per hook -- the property that changed is which field of a SUCCESSFUL answer is read, not the failure posture. Dispatched through `conway_runtime::hook_dispatch::HookDispatcher::dispatch_context`, a SIBLING method to `dispatch` that reads `HookAnswer.context` (a `conway_core::hook::ContextDelta`) instead of discarding it, applies every answering hook's delta append-only
(`conway_runtime::context::script_hook::apply_script_deltas` -- exclude by segment id, append a new `Provenance::SystemNote`-stamped segment attributed to the hook's own configured id, never an in-place edit), and runs the RESULT through the identical tool-call/result coherence guard (`conway_runtime::context::hook_guard::ensure_hook_payload_coherent`, board item `01M00RGARPESWXYAVY960KDE7S`) the Rust `ContextHook` path already goes through before a request can be sent -- there is no second, unguarded way for a script's edit to reach a request. `request_assembled` fires ONCE per turn, from `AgentLoop::run_inner`, after `ContextBuilder::build` and (if registered) `ContextHook::before_request`'s own edit, and before that turn's route/attempt call; `context_overflow` is the script-hook counterpart of `ContextHook::on_overflow` (point 4), firing from `AgentLoop::route_and_attempt` at the IDENTICAL trigger boundary that hook already observes (`RoutingError::ContextTooLarge` only, never a mixed `RoutingError::NoCandidate` rejection -- unwidened by this event's addition). Both payloads are a SUMMARY (`{agent_id, agent_path, session, turn, ..., segment_count, total_tokens_est}`) PLUS per-segment METADATA (`segments: [{id, role, provenance, tokens_est}, ...]`) — never the full assembled segment CONTENT, a bounded-cost decision this event's own item made explicitly (`crate::hook_dispatch::HookDispatcher::dispatch_context`'s own doc has the reasoning); an id is what `ContextDelta::excludes` needs to name a target, and role/provenance is enough for a policy decision without ever reading the transcript. An existing `request_assembled` rule written purely for observation (its answer never sets `context`) is UNCHANGED: applying an empty `ContextDelta` is a no-op, so this is additive, not a breaking change to a shipped config surface. **Whether `child_spawned` may ever DENY a spawn is an open question, deliberately deferred** and recorded at its dispatch site rather than answered by the shape of a return type. **`pre_tool_use` and `post_tool_use` alone honor `match`** — `PreToolUseHookSpec::matcher`/`crate::hook_dispatch::HookSpec::matcher`, checked against `AuthorizedCall::tool`/the payload's `"tool"` field respectively — every other event (including `request_assembled`/`context_overflow`) carries no tool name for a matcher to narrow against. The `pre_tool_use` half is otherwise unchanged: `conway_runtime::permission::PermissionBroker::decide` invokes an injected `Arc<dyn HookRunner>` (`ConwayBuilder::with_hook_runner` — mirroring `with_permission_gate`/`with_context_hook`; not called at all is still the default for a third-party embedder, and a `pre_tool_use` rule with no runner injected parses, validates, and is silently never consulted. `conway-cli` DOES inject one, via the `builtin-tools`-gated convenience `ConwayBuilder::with_default_hook_runner` which supplies `conway_tools::hook_runner::ProcessHookRunner` — so a rule written in a `settings.json` driving the CLI fires. The CLI's opt-in is not inherited by an embedder linking `conway` directly) once per enabled `hooks.rules[]` entry whose `event == "pre_tool_use"`, at the SAME tier as a `deny` pattern rule — before the mode gate, the cache, pattern-allow grants, and `AutoAllow`, so a denying hook is enforced under every permission mode including `AutoAllow` (the one mode with no human in the loop to catch what a downstream-of-the-gate check would have missed). `Plugin::manifest()`/`Plugin::tools()` is no longer the whole `Plugin` trait: `Plugin::hooks()` now exists (board item `01M129QW0GV90QTQS6B3BY3DAR`) — a plugin registers a hook rule directly, through the SAME `with_plugin` surface its tools already use, reaching `PermissionBroker::decide` (for `pre_tool_use`) or the observation/deny-only dispatcher (every other event) at the IDENTICAL tier a config-declared `[hooks].rules[]` entry always has. `ConwayBuilder::config_mut` — the whole-config escape hatch the claude-compat translation used before this method existed — is REMOVED; `crates/conway-cli/src/claude_compat_plugins.rs` now wraps its translated registrations as a real `Plugin` and attaches it via `with_plugin`, the same seam its MCP half already used. Provenance is structural, not merely a stderr warning: a plugin-registered hook's dispatched id is host-namespaced with its own declaring plugin's manifest id (an author never picks its own namespace, mirroring event/command namespacing), and `Conway::active_deny_capable_hook_rules`'s review list reports its `origin` as `"plugin '<id>'"` rather than the operator-authored `"settings.json (merged config)"` label -- see `conway_core::ports::plugin::Plugin::hooks`'s own doc for the full registration-surface contract. Separately, and NOT part of that item: there is no `subagent_mode` field and no `hook.fork` capability anywhere in the tree, and — unlike `hooks()` — this is a **deliberate absence**, not a pending one: the inference-evaluated hook modality those two would belong to was abandoned 2026-08-27 (decision record `01M128AP39WXE01BBZV4RENC4M`, `docs/vision/DESIGN-permission-modes.md` §8, `docs/plugins/inference-hooks.md`) for want of any consumer — see point 14's own Status row. The `HookRunner` port/`ProcessHookRunner` implementation is the general script-runner mechanism `pre_tool_use` dispatch was the FIRST consumer of, not the last — `request_assembled`/`child_reported` reused the identical runner and fail-open contract the three earlier observation events already established, adding only their own two dispatch call sites; `context_overflow` (board item `01KZRZZP6A4A27R3EN0HQAENBS`) reuses the SAME runner again, through the new `dispatch_context` method rather than a third runner implementation. All events remain tracked under the same declarative `hooks` charter.

**Trust posture for the `Plugin::hooks()` registration surface specifically: see `docs/plugins/trust-and-security.md`'s "Plugin-registered hooks" section.** Short version: a plugin-registered `pre_tool_use`/`prompt_submitted` rule reaches the IDENTICAL tier an operator's own `[hooks].rules[]` deny reaches — before the mode gate, the cache, pattern allows, and `AutoAllow` — and it can deny a real tool call or a submitted prompt with no operator authorship at all beyond having installed the plugin. `HookOrigin::Plugin` is what keeps that rule inspectable rather than indistinguishable from one the operator wrote.

**The config shape, settled:** a flat `hooks.rules[]` list keyed by a per-entry `event` field, with `command` as an argv `Vec<String>`. `PHILOSOPHY.md` §5 illustrated a nested, event-keyed shape with a single `run` shell string; the page was corrected to match this one rather than the reverse, because a `run` string only becomes a command once something decides where the words break, and deciding that means predicting a shell — exactly what §1 of the same page rejects. `match` is spelled as §5 spells it (`"match"` on the wire; the Rust field is `match_tool` only because `match` is a reserved word). **A script runner is not a second extension mechanism:** the design is explicit that a script-dispatching hook is itself an ordinary `Plugin` whose own implementation happens to shell out per event, layered on top of the one mechanism, never beside it |

### 14. Fork/spawn declaration for an inference-evaluated hook

| Field | Value |
|---|---|
| Kind | Declarative |
| Receives / May return | Design only: a per-hook-registration `subagent_mode: Fork | Spawn` field, defaulting to `Spawn`. `Fork` additionally requires a granted `hook.fork` capability, following `subagent.spawn`'s exact shape — default off, never implied by trust, requested and separately granted |
| On error | Design only: an operator may refuse a requested `hook.fork` (the hook fails to register if declared required, or is skipped with a status change if optional); an operator may never force `Fork` onto a hook that declared `Spawn`, and a runtime may never silently downgrade a declared `Fork` to `Spawn` and run it anyway — "never guessed at" |
| On timeout | Design only: §16.2's decision-bearing-call exclusion applies — see "Failure semantics" below |
| Ordering | Not applicable — a per-registration field, not a composed value |
| Status | **ABANDONED, 2026-08-27 — a deliberate absence, not a pending gap.** Decision record `01M128AP39WXE01BBZV4RENC4M`: the one named consumer for an inference-evaluated hook, `docs/vision/DESIGN-permission-modes.md`'s `conway.permissions` permission guard, was cancelled outright after a 48-case corpus test — a local model was not reliable enough to gate tool calls, and the failure was shown NOT to be a model-size problem (both a 4B-class and a 14.8B model missed the same paradigm case, `git reset --hard` disambiguated only by `cwd`, 100% of the time). With that consumer gone, nothing in the tree names or needs an inference-evaluated hook modality, so `subagent_mode` and `hook.fork` are not tracked as future work. **This is a different fact from point 13's `hooks()` method**, which a hook registration surface — board item `01M129QW0GV90QTQS6B3BY3DAR` — has now BUILT, for the unrelated, already-shipped claude-compat consumer; that surface registers one-shot script hooks only and carries no `subagent_mode` field, so its arrival does not revive this row. `crates/conway/src/intent.rs`'s `classify` function remains the one shipped precedent for the zero-tool judge shape a future differently-scoped design might reuse — a zero-tool, `SubagentMode::Spawn` judge deciding one narrow question from a prompt alone, with no ancestry and no tool access — but it is not a hook and backs an unrelated feature (the TUI's natural-language `/fork`/`/spawn` intent classifier) that predates and outlives this decision. **What remains open, and is not decided by this abandonment:** a differently-scoped design — pattern rules as the actual gate, a model consulted only as an additional narrowing check for residual cases — is recorded as a follow-up and explicitly **not** a recommendation; and two spec questions (what fail-closed feels like live, whether `AUTO-ALLOW` misleads while a guard runs or has died) were never answered and are not resolved by this decision either. Should a future, differently-scoped proposal need this modality, it is new work carrying a real consumer, not a revival of this row |

**Correction to the design corpus this document must make explicit, per the
brief that produced it:** any future inference-evaluated hook running in
`Fork` mode will inherit the parent's `agent_def` — and never that def's
`result_contract` — because that is now how *every* fork in the tree behaves
(`Runtime::start`'s Fork arm, `crates/conway-runtime/src/subagent.rs`,
`def_was_inherited`; a settled design decision). This post-dates every design document
discussing hook fork/spawn and is not itself hook-specific — it is a
correction to the fork primitive generally, which a hook's `Fork` mode would
simply inherit once built.

### 15. TUI slash-command declaration — `Plugin::commands()` / `Command::invoke`

| Field | Value |
|---|---|
| Kind | Declarative (`Plugin::commands()`, consulted once, at TUI startup) + Participant (`Command::invoke`, an operator-triggered call the host runs and shows the result of) |
| Receives | `Command::spec()` is consulted with nothing live, at registry construction, and returns a [`CommandSpec`] (`name`, `summary`). `Command::invoke` receives a [`CommandCtx`]: `focused_agent`, `root_agent`, `session_id` (the CALLING session's own id), and `args` (everything typed after the command word, verbatim — the same "consume the remainder verbatim" rule every other slash command's free-text argument follows) |
| May return | A [`CommandOutcome`]: `Output(Vec<String>)` (lines appended to the transcript verbatim, each its own entry), `Error(String)` (shown as an ordinary `Notice`, the same severity a failing built-in command gets), `ForkSession { at_seq, directive }` (asks the HOST to fork the calling session at `at_seq` and drive the resulting child; see this point's own "Forking the calling session" subsection below), `MaskRecord { target_seq, excluded }` (board item 01KZY8QRAVVVKCRBZ6HAEGW3GG — asks the HOST to append a `LogRecord::ContextMask` against the CALLING session; see "Masking a record and checking out another session" below), `Checkout { target }` (same item — asks the HOST to fork a NAMED, already-existing session at ITS OWN head and drive the child; the one variant that lets a command name a session other than the one that invoked it), or `SubmitPrompt { text }` (board item 01M0VSMF71S6VXX81YRAAF5S8Q — asks the HOST to submit `text` as a new turn on the CALLING agent, as if the operator had typed it; see "Submitting a prompt" below) |
| On error | `invoke` returning `CommandOutcome::Error` is not a failure of the *host* — it is the command's own reported outcome, rendered as a `Notice` and nothing more. A **panic** inside `invoke` is isolated: the host runs it inside a `tokio::spawn`, and a panicking task cannot bring down the process or the TUI's render/input loop — its `JoinError` is converted into an ordinary `CommandOutcome::Error` naming the panic, delivered through the same reply channel a normal return uses. A `ForkSession`/`Checkout`/`SubmitPrompt` whose target sequence/session/agent is invalid or already mid-turn becomes the identical `Notice`-shaped failure, never a panic |
| On timeout | None imposed. A command that never completes leaves its reply channel silent forever, but never blocks anything else — see "When absent"/Ordering below for why this is structural, not a convention an implementation must remember |
| On garbage | Not applicable to `invoke` (it receives typed Rust values, not wire input). At *registration*, a malformed `CommandSpec::name` (empty, containing whitespace, or failing `conway::plugin::validate_command_name` once namespaced) is a **named, install-time error** — the TUI refuses to start rather than installing a command that could never be typed or that malforms its own namespace |
| When absent | No `Plugin::commands()` override means no commands (the trait's own default returns `Vec::new()`) — every existing `Plugin` implementor, built-in or third-party, keeps compiling and behaving identically. With the declaring plugin not installed at all, its command's full name is simply unknown — `commands::parse` recognizes the *shape* of a plugin-looking word (containing conway-core's event/command namespace separator, `.`) but resolution against the installed registry happens only in `execute`, so an uninstalled plugin's command produces the ordinary "unknown command" notice, never a stub or a special case |
| Ordering | **The render/input loop never calls a plugin, and never blocks on one — the same hard-won property point 12 (`status.declare/1`/`status/1`) establishes for the status line, reused here for the same reason.** `commands::execute` resolves a command (a synchronous `HashMap` lookup) and returns an `Effect::RunPluginCommand` describing it, without ever calling `invoke`; `App` (`conway-cli`) spawns the actual call on its own task, off the `select!` loop that drives rendering and key handling, and receives the reply on a channel exactly like `/ask`'s own modal-answer plumbing (`ModalAskOutcome`/`run_modal_ask`). A hanging command therefore degrades to "the operator doesn't see a reply yet," never to a frozen terminal. Applying a `ForkSession` reply (`App::apply_plugin_command_done`) DOES run on that loop, same as `host.fork`/`host.resume` already do for the built-in commands that swap sessions — the property that must never block is `Command::invoke` itself, already complete by the time a reply exists |
| Status | **Implemented.** `conway_core::ports::plugin::{Command, CommandCtx, CommandOutcome, CommandSpec}` and `Plugin::commands()`'s default (`crates/conway-core/src/ports/plugin.rs`); dispatch through `conway_cli::tui::commands::{SlashCommand::Plugin, CommandRegistry, Host::resolve_command}` and `conway_cli::tui::app::App::spawn_plugin_command`/`apply_plugin_command_done`. `conway-plugin-skeleton`'s `SkeletonPingCommand` (`/{plugin id}.ping`) is the worked example of `Output`. `ForkSession` and `conway-plugin-history`'s real `/conway.history.rewind <seq>` consumer each landed separately; `MaskRecord`/`Checkout` and their real consumers, `/conway.history.mask <seq> [unmask]`/`/conway.history.checkout <session-id>`, land with board item 01KZY8QRAVVVKCRBZ6HAEGW3GG. `SubmitPrompt` and its real consumer, `conway-plugin-skeleton`'s file-backed `FilePromptCommand`, land with board item 01M0VSMF71S6VXX81YRAAF5S8Q — see "Submitting a prompt" below |

**Why this is narrower than a hook that can touch a live session, and
deliberately so.** [`CommandCtx`] carries read-only identity and the raw
argument text — nothing that reaches a live `Conway`/`SessionHandle`, and
never will. Unlike every OTHER point in this table, `Plugin`/`Command` live
in `conway-core`, which structurally cannot depend on `conway` (the facade
crate one layer up, where session-manipulation capability like `Conway::
fork_from` lives) without a dependency cycle — a command can never hold a
live handle onto its own session, let alone another one.

### Forking the calling session — `CommandOutcome::ForkSession`

The gap this point originally disclosed — "a plugin command cannot fork,
resume, steer, or swap the session the TUI is driving" — blocked `/rewind`
being a plugin, which the owner ruled it must be ("features like /rewind,
/checkout, etc are to be plugins, to fit into the philosophy; they are not
core functionality"). This closes exactly the slice `/rewind` needs — fork
the calling session at a sequence, drive the child — and nothing wider
(YAGNI at the time; `/checkout`/`ContextMask` were named as a later item
with their own consumer, and board item 01KZY8QRAVVVKCRBZ6HAEGW3GG is that
item — see "Masking a record and checking out another session" below).

**The shape: an outcome variant the host acts on, not a handle the plugin
exercises.** Two designs were weighed: (1) hand `Command::invoke` a
`conway-core`-native handle that can fork/retarget directly (mirroring
[`ToolCtx::subagents`]'s `SubagentHandle`), or (2) add a third
`CommandOutcome` variant asking the HOST to retarget. (2) was chosen: it
keeps the plugin declarative, leaves the host in control of its own focus,
composes with this point's own "the render/input loop never blocks on a
plugin" rule, and is a strictly smaller capability to hand out — a request
the host can refuse or reinterpret, never a capability the plugin exercises
itself. This is the SAME declare/return-an-effect shape point 12's
`status.declare/1` already uses.

**Bound to the invoking session, structurally.** `CommandOutcome::
ForkSession { at_seq, directive }` carries NO session identifier of its own
— there is no field through which a command could name a session other than
the one it was invoked from. `conway_cli::tui::app::App` resolves `at_seq`/
`directive` against the SAME `CommandCtx::session_id` it captured when it
spawned the invocation, never against whatever session it happens to be
driving by the time the reply arrives — the two can legitimately differ
(an operator's `/resume` racing a slow plugin command). This is the same "a
command acts on its own session, never one it names" property that `SubagentHandle`'s own precedent already established for
tools, applied to commands the only way it can be given this variant carries
no live handle at all: by construction of the type, not a runtime check that
could be forgotten. `conway_core::ports::plugin::tests::
fork_session_outcome_carries_no_session_field_at_all` is the unit-level
proof; `conway_cli::tui::app::tests::
a_fork_session_outcome_is_resolved_against_the_invoking_session_even_if_the_host_has_since_moved_on`
is the adversarial, real-`Conway` proof (a `/resume`-race simulation whose
correct-target log stays untouched, and where forking the WRONG session
directly is shown to fail for a distinguishable, concrete reason —
`StoreError::SeqOutOfRange`).

**What the host does, mechanically:** `Conway::fork_from(session_id, at_seq,
ForkSpec::new(directive))` — zero-copy by reference (`SessionStore::fork`'s
own O(1) contract), so the PARENT session's log is never mutated — then
swaps its own driven `SessionHandle` for the returned child and
resubscribes its event stream, the same mechanism `SlashCommand::Resume`'s
`Effect::Resumed` already uses for an unrelated reason (swapping which
session the TUI drives).

**`/rewind`'s fork-and-drive half is now buildable as a pure plugin; its own
"which sequence" half is a separate, disclosed gap this item does not
close.** A command whose operator syntax names a `LogSeq` directly (`/acme.
rewind 42`) needs nothing more than `ctx.args.parse()` and this variant — the
`conway-plugin-skeleton`-shaped fixture `conway_core::ports::plugin::tests::
RewindCommand`/`conway_cli::tui::app::tests::RewindCommandFixture` prove
exactly that path end to end. A command whose operator syntax is natural
language ("rewind to before I asked about X") needs to RESOLVE that request
against the session's own history first — and `CommandCtx` grants no way to
read a transcript at all, on purpose (this point's own narrowing: no live
`Conway`/`SessionHandle`, ever). That resolution problem belongs to
`/rewind`'s own item, not this one — it may need a
further, separately-justified read-only capability (a history-browsing port,
scoped the same "conway-core-native, bound at construction" way this item's
own precedent demands), or it may turn out `/rewind`'s UI can defer sequence
selection to the operator some other way (an explicit `@seq` argument, a
picker built from data the TUI already has). Either way, nothing about
resolving a target reaches `conway-cli` internals, and nothing here widens
`CommandCtx` beyond the one new read-only field this item adds.

**Resolved, this way, by the follow-up item (`conway-plugin-history`,
`/conway.history.rewind <seq>`):** the second option above — an explicit
`<seq>` argument, never free text, and NO new `CommandCtx`/`conway-core`
capability. The "is a seq even operator-visible" question that item was
also asked to settle came back "no, not before this item": the live event
stream's own `Envelope::seq` is a per-connection renumbering, not the
persisted `LogSeq` `fork_from` accepts, so the status line's `session <id>`
field now widens to `session <id>@<seq>` once the head is known, reusing
`session_ref.rs`'s own `<session-id>[@<seq>]` notation and the ALREADY-
EXISTING `Conway::session_head` facade call (`conway-cli`'s own host-side
addition — `crates/conway-cli/src/tui/app.rs`'s `refresh_session_head`, no
new port). Resolving free text against history remains the disclosed,
unbuilt gap this paragraph always named.

### Masking a record and checking out another session — `CommandOutcome::MaskRecord`/`CommandOutcome::Checkout`

Board item 01KZY8QRAVVVKCRBZ6HAEGW3GG ("`/checkout` and a reachable
`ContextMask`") is `conway-plugin-history`'s SECOND and THIRD commands,
added beside `/rewind` rather than as a parallel mechanism — both reuse
`ForkSession`'s own "the host performs the effect, the plugin only
declares it" shape, and both close gaps that item's own predecessor named
but deliberately did not build.

**`/conway.history.mask <seq> [unmask]` gives `LogRecord::ContextMask` its
first real producer.** Before this item, that record type was persisted
and READ by `conway-session`'s fork-ancestry resolver
(`TranscriptResolver::apply_context_mask`), but nothing in the tree
appended one outside a test — `ARCHITECTURE.md` §3.5 said so plainly. The
command parses `ctx.args` as a bare `LogSeq` plus an optional trailing
`unmask` token and returns `CommandOutcome::MaskRecord { target_seq,
excluded }`, bound to the CALLING session exactly like `ForkSession` —
same structural argument, same doc precedent, no field to name a different
session. The host resolves it with `Conway::mask_record`, a plain
`SessionStore::append` — masking (and un-masking) is itself an ordinary,
reversible, append-only write, never a mutation of the targeted record.

**The scope decision that item settled: still fork-prefix-only.** A mask
only ever affects what a FUTURE fork of the masked session inherits — never
the owning session's own later turns; `ContextBuilder`/`TranscriptResolver`
for a session's OWN live assembly is unchanged by this item. Widening it
to reach a session's own assembly was considered and rejected: the
per-request, append-only script-hook edit path (`request_assembled`/
`ContextDelta`, landed separately) already lets an operator-configured
script exclude a segment from THIS session's own next request, through
`ContextHook`, without touching the `TranscriptResolver`/`ContextBuilder`
hot path a persisted-mask widening would have to. Building a second
mechanism for the identical effect would have been duplication, not a new
capability.

**`/conway.history.checkout <session-id>` is the one case `ForkSession`
structurally cannot express.** `ForkSession` can only ever fork the
CALLING session — deliberately, per its own doc above. Checking out
means moving to a DIFFERENT, already-existing session, so
`CommandOutcome::Checkout { target }` deliberately widens what a command
can name: `target` is a `SessionId` the command read from its own typed
argument, not the invoking one. Nothing else about the narrowing this
point establishes is loosened — a command still cannot read another
session's content, steer it, or act on it beyond "hand me a fresh fork of
it to drive." `/checkout` always FORKS `target` at ITS OWN current head
rather than attaching to it live (`PHILOSOPHY.md` §1: a finished session
is forkable at any point, and forking is the safer default — it keeps
`target`'s own log append-only-untouched and needs no "two live agents
driving one session" concurrency story). The host resolves it with
`Conway::session_head(target)` then the SAME `Conway::fork_from` call
`ForkSession` uses, then swaps its driven `SessionHandle` for the child —
`target` itself is never written to and stays listed exactly as before.

### Submitting a prompt — `CommandOutcome::SubmitPrompt`

Board item 01M0VSMF71S6VXX81YRAAF5S8Q ("No command can submit a prompt")
closed the one gap every variant above left standing: a command could act
on session HISTORY (fork it, mask a record in it, check another one out),
but nothing could START A NEW TURN — put text into the conversation as if
the operator had typed it. That is what a prompt-template command's entire
job is (`/review-this`, `/explain`, the shape most of Claude Code's own
`commands/*.md` plugins are built on — see
[`compatibility.md`](compatibility.md) for the import-time gap this closes,
though wiring `commands/*.md` itself to this remains a separate, unbuilt
follow-up).

**Provenance is the load-bearing question, and it is answered by a new,
dedicated `Provenance` variant — never `UserPrompt`.** Text a command
submits is authored by conway (a plugin's own template or logic), not
typed by the operator, even though the model reads it in the identical
`Role::User` position an operator's own turn would occupy. The host stamps
the resulting `LogRecord::UserTurn` with `Provenance::CommandPrompt {
command }` — this project's own "context provenance is mandatory and
persisted, and a feature that obscures where context came from is
rejected" rule applied literally: a turn conway generated is not a turn
the operator typed, and the durable log — `/context`'s own provenance
rendering included — can tell the two apart (`command` is the full
`plugin_id.bare_name` that produced it). This is a persisted wire-format
addition to `Provenance` (`#[serde(tag = "type", ...)]`): every record
written before this variant existed still decodes exactly as it always
has (its own tag is one of the original twelve); an OLDER binary reading a
NEWER log containing a `command_prompt`-tagged record fails to decode
that one record, the identical forward-compatibility cost every prior
addition to that enum (`MergedAsk`, `ChildResult`, `Memory`) already paid.

**A port variant, not a TUI-only renderer effect — decided by this
project's own "no capability may exist in only one mode" rule, not by
convenience.** `conway::SessionHandle::prompt_command(agent,
text, command)` is the facade primitive that fulfils this outcome,
reachable by ANY caller holding a `SessionHandle` — the TUI's `App`, a
one-shot `conway <plugin-id>.<command>` invocation, or a bare library
embedder with no TUI in the loop at all — never a method only `conway-cli`
can reach. It stamps `Provenance::CommandPrompt` and otherwise runs the
SAME `Runtime::prompt` machinery an ordinary operator turn uses
(persist-before-act, the live `Event::UserTurn` twin, the `prompt_notify`
wake).

**v1 performs no interpolation of any kind.** `text` is a literal string
this crate never parses, templates, or substitutes into — no
`{{args}}`/`$ARGUMENTS`/positional-placeholder syntax exists anywhere in
this port. A `Command::invoke` implementation that wants to fold
`CommandCtx::args` into the submitted text does so itself, with ordinary
Rust string building; there is no template language for this crate to
parse untrusted argument text through -- this project's own discipline of
range-checking untrusted input at the boundary favors the smaller slice
over building a template language ahead of a real consumer that needs
one.

**Bound to the invoking agent AND session, structurally, exactly like
`ForkSession`/`MaskRecord` above.** No field on `SubmitPrompt` names a
session or agent other than the ones the command was invoked from —
`conway_cli::tui::app::App` resolves the submission against the SAME
`CommandCtx::focused_agent`/`CommandCtx::session_id` it captured when it
spawned the invocation, never against whatever the host happens to be
driving by the time the reply arrives. Targeting `focused_agent` (not
`root_agent`) matches "as if the operator had typed it" literally: an
ordinary typed message targets whichever agent the operator is currently
looking at, and this variant does too. Never swaps the driven session —
submitting a prompt never changes which session is driven, exactly like
`MaskRecord`.

**The in-flight guard.** `App::apply_plugin_command_done`'s own
`SubmitPrompt` arm refuses (a `Notice`, nothing appended) rather than
silently racing a second turn onto the SAME agent the TUI is currently
watching mid-turn — composing with, not fighting, `state.rs`'s own
`turn_started_at.is_some()` guard (the fix for the adjacent
wedged-status-bar defect, board 01M0VQ650R31MGTXD8E225RRFH): `turn_started_at`
is `Some` only between a real `TurnStarted` and `TurnFinished` for the
focused agent, exactly the "a turn is in flight" predicate that fix
already established. Scoped to the focused agent because that is the only
agent `AppState` tracks turn-in-flight state for at all today — a target
agent the operator has since navigated away from has no tracked state to
consult (a disclosed, pre-existing limit, not a silent gap), so the
submission proceeds in that case: `Runtime::prompt`'s own concurrent-call
contract (durable append either way, never lost, never corrupted) makes
that the safe direction to fail open in.

**Trust posture: see `docs/plugins/trust-and-security.md`'s "TUI slash
commands: no permission gate at all, by design" section**, which now
covers `SubmitPrompt` alongside `ForkSession`/`MaskRecord`/`Checkout` —
the short version is that this widens WHAT a command can cause (a new
agent turn) but not WHO can cause it: the operator who typed the command
word could have typed the identical text themselves in the next keystroke.

**A real, file-backed consumer, not a speculative grant.**
`conway-plugin-skeleton`'s `FilePromptCommand` reads a markdown file's
content once, at construction, and returns `CommandOutcome::SubmitPrompt`
carrying that file's own body verbatim — this crate's own proof that a
markdown file becomes a typeable command with no Rust beyond a handful of
lines, and the smallest honest instance of the capability this point
ships. `crates/conway-plugin-skeleton/tests/file_prompt_command.rs` is the
end-to-end proof, driven entirely through the library API (no TUI): the
file's own body lands as a real, readable-back `LogRecord::UserTurn`
stamped `Provenance::CommandPrompt`, and the turn runs to completion.
`crates/conway-cli/src/tui/app/plugin_cmd.rs`'s own test module carries the
TUI-path proof (submission, the in-flight guard, and its falsification)
through a real `App`.

**Verification.** `crates/conway/tests/context_mask_producer.rs` masks a
real record, forks, and asserts on the forked child's ASSEMBLED SEGMENTS
(`GenerateRequest::segments`, what a model would actually receive) that
the masked turn is absent while a sibling turn survives — shown, in a
second test, to be present when the same scenario runs without the mask.
`crates/conway-cli/src/tui/app/plugin_cmd.rs`'s own test module carries
the real-plugin, real-`Conway` proofs for both commands through the TUI
path, mirroring `/rewind`'s own anchor there exactly.
`crates/conway-cli/tests/checkout_and_mask_plugin.rs` is the headless
`conway conway.history.checkout <id>`/`conway conway.history.mask <seq>`
proof, including a `/checkout` test reading the checked-out-FROM session's
own `.jsonl` file as raw bytes, before and after, to confirm they are
identical.

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
`crates/conway/tests/architecture_invariants.rs`'s `t9_tui_has_no_parser_bypasses`, unaffected by this point). `parse` recognizes
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
| On timeout | None imposed on `emit` itself; a subscribed hook's own timeout is the SAME `timeout_ms` every `hooks.rules[]` entry declares (point 13) |
| On garbage | At *registration* (`ConwayBuilder::build`), a malformed declaration (an empty bare name, or two events landing on the identical namespaced full name — from the same plugin or two different ones) is a **named, build-time error**, mirroring point 15's identical registration-time refusal for a malformed `CommandSpec::name` |
| When absent | No `Plugin::events()` override means no declared events (the trait's own default returns `Vec::new()`) — every existing `Plugin` implementor, built-in or third-party, keeps compiling and behaving identically. A `hooks.rules[].event` naming no installed plugin's declared event parses, validates, and is silently never dispatched — the SAME tolerance a typo'd core event name has always had |
| Ordering | Every hook subscribed to the SAME namespaced event name runs, in configured order, exactly like point 13's five observation events (this is literally the same dispatch table, unioned) — a failure never stops a later hook from running |
| Status | **Implemented.** `conway_core::ports::{EventDecl, PluginEventEmitter, PluginEventHandle}` and `Plugin::events()`'s default (`crates/conway-core/src/ports/plugin.rs`); `conway_runtime::hook_dispatch::declared_plugin_events` (namespacing/validation) and `impl PluginEventEmitter for HookDispatcher` (dispatch, reusing point 13's own fan-out); `ConwayBuilder::build` unions the result into the SAME dispatch table `hooks.rules[]` already feeds. `conway-plugin-skeleton`'s `pong_dispatched` event is the worked example: `SkeletonPlugin::events()` declares it, `SkeletonPingTool::invoke` fires it unconditionally on every call, and `conway-plugin-skeleton/tests/skeleton_end_to_end.rs`'s `a_configured_hook_fires_when_the_skeletons_declared_event_is_dispatched` proves a real configured `hooks.rules[]` entry actually receives it. |

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
rule additionally
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
decide what `hooks.rules[]` may actually dispatch — one implementation,
not a parallel "validate" path and a separate "enumerate" path that could
drift apart. No `conway-cli` surface lists this yet (mirroring point 13's
own disclosed gap for `hooks.rules[]` visibility) — the mechanism exists
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

### 17. Instruction fragment declaration — `Plugin::instructions()`

| Field | Value |
|---|---|
| Kind | Declarative (`Plugin::instructions()`, consulted at `ConwayBuilder::build` for duplicate-name validation and at `ConwayBuilder::build` for plugin attribution; the reachability check itself runs per-turn, in `ContextBuilder::build` — see "Where the reachability check runs" below) |
| Receives | Consulted with nothing live; returns zero or more [`InstructionFragment`] (`name`, `text`, `tool_ids: Vec<ToolName>`, `position: FragmentPosition`, `order: i16`, `scope: FragmentScope`, `agent_def: Option<String>`) |
| May return | Any number of fragments, including none (the trait's own zero-cost default). No shape constraint on `text` beyond being a `String` — this point ships no markdown-file loader; that is a plugin-author convention (see below), not a mechanism this point enforces. `position`/`order`/`scope`/`agent_def` all default to today's pre-existing behavior (`AfterSystemPrompt`, `0`, `All`, `None`) via `InstructionFragment::new`, so a plugin that never calls the `with_*` builders behaves exactly as it did before those fields existed |
| On error | Not applicable — `instructions()` cannot fail; it is a pure, synchronous, in-process call, like `Plugin::tools()`/`Plugin::commands()` |
| On timeout | None — synchronous, in-process, no I/O boundary |
| On garbage | A `name` colliding with another installed plugin's fragment name (or another fragment of the SAME plugin's own) is a **named, build-time error** at `ConwayBuilder::build`, mirroring point 16's identical registration-time refusal for a colliding event name. A `tool_ids` entry naming a tool no installed plugin provides is NOT a build-time error (see below) |
| When absent | No `Plugin::instructions()` override means no declared fragments — every existing `Plugin` implementor, built-in or third-party, keeps compiling and behaving identically; a build with no instruction-declaring plugin injects no new segment |
| Ordering | Every fragment renders at one of two positions relative to `[0] SystemPrompt` (the base idiom an `AgentDef` carries, or a one-shot `--system-prompt`/`--append-system-prompt` override — both occupy `[0]`): `BeforeSystemPrompt` fragments render AHEAD of `[0]`; `AfterSystemPrompt` fragments (the default — every fragment declared before `position` existed keeps this exact placement) render where they always have, AFTER `[0]` and BEFORE an operator's own directory-authored skills (`AgentDef.skills`). Within a position, fragments are stable-sorted by `(order, install index)` — lower `order` first, a tie keeping `with_plugin`/`install_selected` install order. The seam (`ContextBuilder::build`) owns this precedence; no call site invents its own order. Deterministic (a plain numeric sort, no hashing) so the assembled prefix stays byte-identical turn over turn (prompt-cache economics) |
| Reaches | **Every agent — root, forked, or spawned (board item `01M0VSKA76NSEHDSH25XJGJ2J5`, a RULING, not merely an observed behavior) — by DEFAULT.** `SubagentHost::start` resolves a fork/spawn child's `AgentSpec.instructions` through the SAME `resolve_instructions` root/resume already call, unconditionally, with no per-mode branch — the reachability check just above (`tool_ids`) still gates what actually lands in a child's assembled context, exactly as it does for root. **A fragment may narrow this with `scope`**: `FragmentScope::All` (the default) reaches every agent exactly as before; `RootOnly`/`ChildrenOnly` restrict it to one or the other, keyed on the STRUCTURAL fact of whether the agent has a parent (`AgentLoop::parent.is_none()`, never inferred from tools or role) — a scoped-away fragment is dropped before rendering and recorded with `skipped_by_scope: true` in `ContextReport::instruction_fragments`, never silently withheld. `agent_def`, when `Some`, additionally requires `[0]`'s own agent-def name to match exactly. **There is deliberately no ROLE selector** — no first-party consumer needs one, and threading a role into `ContextInput` for a selector nothing uses would be unbuilt theory; a future item that needs one has this line to update. **The argument for reach-by-default, in brief** (full version at `resolve_instructions`'s own doc, `crates/conway-runtime/src/runtime/root.rs`): the two-primitive rule (fork = whole parent TRANSCRIPT, spawn = empty transcript) governs the conversation a child starts with, not every configuration channel `AgentSpec` carries — an instruction fragment is never appended to the log, is resolved fresh every turn from install-time plugin state, and is gated purely by whether THIS turn's announced tools satisfy `tool_ids` (plus, now, `scope`/`agent_def`), the same shape `system_prompt`/`tools`/`plugin_config` already have. Giving a spawned child the same fragments a root gets by default is therefore not a third, blurred "partial inheritance" primitive: fork and spawn remain byte-identical in what they do to the log, and this one function is now called identically for both. |
| Status | **Implemented.** `conway_core::ports::plugin::{InstructionFragment, FragmentPosition, FragmentScope, Plugin::instructions}` (`crates/conway-core/src/ports/plugin.rs`); collection + duplicate-name check in `ConwayBuilder::build` (`crates/conway/src/builder.rs`); position/order sort, scope/agent_def filter, and per-turn reachability check in `ContextBuilder::build` (`crates/conway-runtime/src/context/builder.rs`); `/context`'s preamble section (`crates/conway-cli/src/tui/commands.rs`). Board item `01M0K5MD59YZRSHE31JKZKFRMY`; position/order/scope extension: process record `01M1FQ36PCW2J19AP219GKZH3R`. |

**Why this point exists at all, stated once here rather than only in the
struct's own doc.** Before this point, a plugin could already put a
paragraph into a context by mutating the assembled request from inside
point 3 (`ContextHook::before_request`) — `conway.skills`'s own
`SkillIndexHook` does exactly this, to NARROW a segment, not author one.
That is expressible but not legible: the text lives in Rust, not a file;
there is no way to ask what instruction conway is running with short of
reading every hook; nothing states which paragraph outranks which when
several disagree; and nothing connects a paragraph to the tool calls it
assumes. This point makes the declaration DATA a host can inspect, order,
and check — not a second injection path alongside point 3.

**Where the reachability check runs, and why it is split across two
places.** The obligation ("an instruction may only name a capability that
is actually reachable", decision `01M0K4S2S1NBW63KNF1NEY5XT3`) has TWO
distinct failure classes, checked at two different times:

- **Duplicate fragment names** are a build-time, configuration-INDEPENDENT
  fact (the same set of names is either unique or is not, regardless of
  which tools an operator happens to have installed) — checked once, at
  `ConwayBuilder::build`, the same tier as point 16's duplicate-event-name
  check.
- **An unreachable `tool_ids` entry** is configuration-DEPENDENT: a
  fragment can name a tool that exists somewhere in this repository but is
  not among the tools THIS operator's `plugins.install` actually resolved
  to — a fact no CI grep can see. This is checked per turn, in
  `ContextBuilder::build`, against that turn's own resolved
  `ContextInput.tools`. An unreachable fragment's text is WITHHELD (never
  injected as a segment, so the model can never try a tool that is not
  there and fail silently, forever) and recorded in
  `ContextReport::instruction_fragments` with the missing tool id named, so
  `/context`'s preamble section renders the omission inline
  (`⚠ names <tool> — not installed`) rather than only warning once in a
  log line that scrolls away.

A fragment naming a tool id its OWN declaring plugin also provides
(`Plugin::tools`) can NEVER fail the second check: both are contributed by
the same `Arc<dyn Plugin>`, installed through the same `with_plugin` call,
so they ship and leave together by construction. Reachability is therefore
STRUCTURAL for that common case and CHECKED only for the genuinely
cross-plugin (or entirely-missing) case.

**Convention, not enforcement: text lives in a markdown file.** Nothing in
`InstructionFragment` forces `text` to come from a file rather than a Rust
string literal — a `String` cannot tell the difference. The convention is
`include_str!("../fragments/foo.md")` for a fragment compiled into a
plugin, or a genuine file read at construction time for a plugin
distributed as data alongside a compiled binary. The two differ on
removability: `include_str!` bakes the text into the binary, so there is no
file an operator can delete to disable ONE fragment without uninstalling
the whole plugin — that is the one kind of fragment that genuinely needs a
settings UI to disable (deferred; the `/settings` instructions section is
pending a fuller settings design, per decision `01M0K5K8DCRVR523P54DZF4BY3`).
A files-beside-the-plugin convention keeps every fragment removable with no
UI at all — the file IS the control surface — which is why it is the
recommended shape even though `include_str!` remains legal.

**Relationship to point 3's `conway.skills` — not folded together.**
`conway.skills` narrows a `Provenance::Skill` segment `AgentDef.skills`
already put there (operator-authored, loaded from a directory,
`crates/conway/src/skills.rs`); this point AUTHORS a `Provenance::Skill`
segment in the first place, sourced from a plugin. They share the
rendering machinery (`conway_runtime::context::SkillFragment`,
`Provenance::Skill`) once resolved — both are, at that point, "a named text
fragment injected into context" — but the SOURCING differs
(capability-authored vs. operator-authored) and so does the LIFETIME: a
skill outlives any plugin (it is the operator's own file); an instruction
fragment does not (it ships and leaves with `with_plugin`). This is a
deliberate non-merge, argued rather than assumed.

**Trust posture: see `docs/plugins/trust-and-security.md`'s "Instruction
fragments" section.** Short version: installing the plugin is still the
entire control (nothing new is gated), and this grants no capability point
3 did not already have — a `ContextHook` could already inject arbitrary
text; this is a narrower, declarative, legible way to do one specific thing
point 3 could already do arbitrarily.

### 18. Context-path composition — `ToolCtx::context_path` (`ContextPathHost`)

| Field | Value |
|---|---|
| Kind | Participant (a `Tool` calls it during `Tool::invoke`; each method returns a value the tool acts on) |
| Receives | `ToolCtx::context_path: ContextPathHandle`, a handle bound to the calling tool call's own `session_id`, on every dispatched `Tool::invoke` — not a hook a plugin implements, a capability every tool already holds. Three methods: `default_path()` (this session's current context path), `resolve_records(&[RecordRef])` (resolve records from ANY session, honestly, through the same masked read the per-turn assembly itself uses), `set_head(ValidatedPath)` (freeze a derived path as this session's new head) |
| May return | `default_path`/`set_head` return `Result<_, PathError>`, narrowed to this session by construction — no parameter through which a call could name a different one; `resolve_records` returns `Result<BTreeMap<RecordRef, Arc<LogRecord>>, PathError>` and may name any session |
| On error | `PathError` (`UnresolvableNode`, `PrefixChainTooDeep`, `WouldOrphan`, ...) is an ordinary typed error a tool matches on and reports back as an `is_error` `ToolOutput` — never a panic, never a partial write (`set_head` either fully persists a `ContextPathSet` record or does not run at all) |
| On timeout | None at this point — whatever the underlying `SessionStore`/`PathStore` implementation's own I/O timeout is (unchanged by this point) |
| On garbage | Not applicable — every argument is a typed Rust value (`SessionId`, `RecordRef`, `ValidatedPath`) constructed by the calling tool, not parsed from wire bytes at this boundary |
| When absent | No tool that never calls `ctx.context_path` is affected — the field is populated unconditionally on every `ToolCtx` (like `subagents`/`chdir`), at zero cost to a tool that never reads it |
| Ordering | Not applicable — no multiple-registration composition; one runtime-wide `ContextPathHost` implementation, narrowed per call |
| Status | **Implemented.** `conway_core::ports::context_path::{ContextPathHost, ContextPathHandle}` (`crates/conway-core/src/ports/context_path.rs`); production implementation `conway_runtime::context::RuntimeContextPathHost` (`crates/conway-runtime/src/context/path_host.rs`), wired into `LoopDeps`/`ToolBatchCtx` in `runtime.rs`/`agent_loop.rs`/`tools/runner.rs`; first production consumer `conway.path`'s `compose_context_path` tool (`crates/conway-plugin-path`). Board item `01M0PEFMG96SVBBD5D2E06H34A`, decision `01M0K4QT6MBXPD6PXMBBBD2P7B`. |

**Why this is a `ToolCtx` field, not a `Curator` capability.** Decision
`01M0K4QT6MBXPD6PXMBBBD2P7B` (cited in the board item that built this
point): `CurateCtx` carries `model: Option<ModelId>` as a sizing
IDENTIFIER only, and a `Curator` runs per-turn BEFORE routing — inference
there would be re-entrant. Composing a path from an operator's stated
intent needs a MODEL to have already interpreted that intent, so the
composing capability belongs at a `Tool` call, where inference is already
in flight, not at the curator seam. `CurateCtx` is unchanged by this
point.

**Why a new handle, not a widened `PathStore` re-export.** `PathStore`
(point coverage: none — it is engine-internal by a separate, standing
decision, board item `01M0EMCK55628YJXGBQY8YGXHE`) is deliberately not
re-exported through `conway::plugin`. This point does not reopen that
decision: `ContextPathHost` is a narrow, purpose-built capability
(`default_path`/`resolve_records`/`set_head` — not `PathStore`'s own
`put`/`get`/`selections_referencing`) that its production implementation
backs with `PathStore`/`SessionStore`/`TranscriptResolver` internally,
mirroring `SubagentHandle`'s identical "narrow handle over a host trait,
never the raw port" shape for fork/spawn (point 2's own `ToolCtx::
subagents` field).

**Trust posture: see `docs/plugins/trust-and-security.md`'s "Composing a
context path" section.** Short version: an ordinary gated `Tool` call
(unlike a `Command`, point 15 — this point's calls go through
`PermissionGate`/`PermissionBroker` like any other tool); `resolve_records`
may read any session's records (the same cross-session reach point 15's
`Checkout` and `Curator`'s own §11.5 read surface already establish is not
a new capability, since any record in the store is something conway itself
already produced); `default_path`/`set_head` can only ever change what the
CALLING session renders next, never another session's.

**This point resolves a reference the model ALREADY holds.** Point 20,
immediately below, is where a model gets a reference to a session it
neither started nor spawned in the first place — `resolve_records` here has
no way to answer "which session is that."

### 19. Operator-facing description — `Plugin::description()`

| Field | Value |
|---|---|
| Kind | Declarative (`Plugin::description()`, consulted by a plugin browser -- `crates/conway-cli/src/tui/view/settings.rs`'s `/settings` plugins section -- never by the model, never by context assembly) |
| Receives | Consulted with nothing live; returns one [`PluginDescription`] (`summary`, `you_get`, `you_lose`, `costs`, every field a plain `String`) |
| May return | Any text, including every field empty (the trait's own zero-cost default) |
| On error | Not applicable -- `description()` cannot fail; it is a pure, synchronous, in-process call, like `Plugin::tools()`/`Plugin::manifest()` |
| On timeout | None -- synchronous, in-process, no I/O boundary |
| On garbage | Not applicable -- no external input crosses this point |
| When absent | No `Plugin::description()` override means every field renders empty; the browser shows an honest fallback (`"(no description)"`/`"(none given)"`/`"none"`) rather than inventing text a plugin never supplied |
| Ordering | Not applicable -- one description per plugin, no composition |
| Status | **Implemented.** `conway_core::ports::plugin::{PluginDescription, Plugin::description}` (`crates/conway-core/src/ports/plugin.rs`); rendered by `crates/conway-cli/src/tui/view/settings.rs`'s plugins section; written by `crates/conway-cli/src/tui/app/plugin_toggle.rs`'s `App::apply_plugin_toggle` via `conway::config::writer::set_plugin_installed`. Board item `01M0KARX71A64NTSYTDBVANVPF`. |

**Two audiences, two types -- argued, not assumed.** Point 17
(`Plugin::instructions()`) ships text for the MODEL; this point ships text
for the PERSON deciding whether to turn a plugin on. The choice was
between a field on `PluginManifest`, an addition to `InstructionFragment`,
or a new trait method -- this point is the third, for two reasons: adding
a required field to `PluginManifest` would have forced every one of the
three dozen existing struct-literal construction sites across this
workspace (most with no operator-facing browser to describe themselves
for at all) to invent placeholder text just to keep compiling, where a
trait method with a default costs them nothing; and cardinality differs
from `InstructionFragment` (one description per plugin, matching
`PluginManifest`'s own one-per-plugin identity, vs. zero-or-many
instruction fragments -- several first-party plugins ship zero fragments
today but still have something to tell an operator).

**Text lives as a Rust literal, not a loaded file -- the opposite of point
17's own convention, deliberately.** That convention exists so an
operator can delete ONE instruction fragment's file to disable a
MODEL-facing behavior with no recompile. A description has no equivalent
removability need: it never changes what the model does, and there is no
"keep the plugin, lose only its description" state anyone would want.

**"You get / you lose / costs" is the operator's own framing, kept
literally** -- it names what CHANGES, the actual question when deciding
whether to flip a toggle, rather than a generic "what is this" blurb.

### 20. Cross-session discovery — `ToolCtx::session_discovery` (`SessionDiscoveryHost`)

| Field | Value |
|---|---|
| Kind | Participant (a `Tool` calls it during `Tool::invoke`; the one method returns a value the tool reports back) |
| Receives | `ToolCtx::session_discovery: SessionDiscoveryHandle`, on every dispatched `Tool::invoke` -- not a hook a plugin implements, a capability every tool already holds, mirroring point 18's `context_path` exactly. One method: `search(SessionSearchQuery) -> Result<SessionSearchResult, StoreError>`. Cross-session by construction -- unlike `context_path`, no session is baked in, since a search is never "about" one session. |
| May return | `SessionSearchResult`: the matches found (session id, project key, cwd, created time, and -- only in content-search mode -- the specific `(seq, snippet)` pairs), plus `projects_scanned`/`sessions_considered`/`sessions_content_scanned`/`records_scanned`/`truncated` -- what was searched and what it cost, always populated, never optional |
| On error | `StoreError`, an ordinary typed error a tool matches on and reports back as an `is_error` `ToolOutput` -- never a panic |
| On timeout | None at this point -- whatever the underlying `SessionStore`'s own I/O timeout is |
| On garbage | Not applicable -- every argument is a typed Rust value (`SessionSearchQuery`) constructed by the calling tool, not parsed from wire bytes at this boundary |
| When absent | No tool that never calls `ctx.session_discovery` is affected -- the field is populated unconditionally on every `ToolCtx`, at zero cost to a tool that never reads it |
| Ordering | Not applicable -- one runtime-wide `SessionDiscoveryHost` implementation, no per-call narrowing |
| Status | **Implemented.** `conway_core::ports::discovery::{SessionDiscoveryHost, SessionDiscoveryHandle, SessionSearchQuery, SessionSearchResult}` (`crates/conway-core/src/ports/discovery.rs`); production implementation `conway::discovery_host::FsSessionDiscoveryHost` (`crates/conway/src/discovery_host.rs` -- NOT in `conway-runtime`, see below), wired into `RuntimeDeps`/`LoopDeps`/`ToolBatchCtx` in `builder.rs`/`runtime.rs`/`agent_loop.rs`/`tools/runner.rs`; first production consumer `conway.discover`'s `search_sessions` tool (`crates/conway-plugin-discover`). Board item `01M0PS8J3AK7Z7253Z3E3RD3GY`. |

**Why the production implementation lives in the `conway` facade crate, not
`conway-runtime` (unlike point 18's `RuntimeContextPathHost`).**
`crates/conway/tests/architecture_invariants.rs`'s T4 pins `conway-runtime`
to depend on `conway-core` alone -- no adapter edge, `conway-session`
included. `SessionSearchScope::AllProjects` genuinely needs adapter
machinery (`conway_session::discovery`, opening a `JsonlSessionStore` per
sibling project directory under the central sessions root) that has no
adapter-free equivalent the way point 18's `resolve_default_path`/
`write_head` do, so `FsSessionDiscoveryHost` is built where the
`conway-session` edge already legitimately exists -- gated behind
`jsonl-store`, exactly like `build_default_store`. Its `CurrentProject`
scope needs no adapter at all (it reuses the already-built `store: Arc<dyn
SessionStore>` through the generic port trait alone), so it works
identically whether `jsonl-store` is on or an embedder injected their own
store.

**Why a new port, not a widened `ContextPathHost`.** Bolting a `search`
method onto point 18's port would fuse two genuinely separate capabilities
-- composition and discovery -- into one, repeating one layer down the
exact scope-doubling cherry-pick (`01M0KZ6J0DF6XR1TVSDH2KDPRX`) correctly
avoided at the tool level. A new, narrow port keeps each seam answering
exactly one question, mirroring `ContextPathHost`'s own "narrow,
purpose-built capability" precedent.

**Reach: a directory listing over one root, never a crawler or a
registry.** Decision `01M0QK8J757ZH6R06WYJ0PQGEM` moved sessions to a
central, project-keyed root specifically so `SessionSearchScope::
AllProjects` would need neither. `conway_session::discovery::
list_project_keys` does exactly one `read_dir` over that root.

**Trust posture: see `docs/plugins/trust-and-security.md`'s "Finding a
session" section.** Short version: an ordinary gated `Tool` call, same
footing as point 18; `search` never writes anything, ever; content search
(`SessionSearchQuery::text` set) is the only mode that reads a record body,
and it is bounded by `max_sessions`, stated in the result.

### 21. Plugin-to-plugin capability calls — `Plugin::capabilities()` / `CapabilityProvider` (`conway_core::ports::capability`)

| Field | Value |
|---|---|
| Kind | Participant — one provider, one request, one answer (the *pull* shape), the opposite of point 11's `observe/1` (emit-only, zero-or-many, no reply channel by construction). Design §7c (`docs/vision/DESIGN-plugin-dependencies.md`) names why the two are deliberately not one mechanism |
| Receives | `payload: serde_json::Value` only — no capability name (the calling `CapabilityRegistry` has already matched it before a provider is ever reached) and no `ToolCtx`; `CapabilityCallHandle::caller_plugin_id` is carried for tracing/audit only, never checked as an authorization input |
| May return | `Result<serde_json::Value, CapabilityError>` — `CapabilityError { message, detail }` is plain, `Serialize`/`Deserialize` data, so an out-of-process provider (a `SubprocessPlugin` forwarding proxy) can construct it from whatever its own wire answer carries |
| On error | The provider's own `CapabilityError`, wrapped as `CapabilityCallError::Provider` and returned to the caller — a typed value a calling `Tool::invoke` matches on, never a panic that escapes into the caller |
| On timeout | Not applicable at this port — no port-level deadline exists; a slow provider blocks its caller for exactly as long as an ordinary awaited call would, on the caller's own task |
| On garbage | A malformed `capability` name fails `HostCapability::named`'s validation and is refused by `CapabilityCallHandle::call` before a registry lookup is even attempted — `CapabilityCallError::MalformedName`, kept structurally distinct from `NotProvided` so a typo can never read as "nothing installed provides this" |
| When absent | No installed plugin registered a provider for the named capability → `CapabilityCallError::NotProvided`, an ordinary typed error, never a stall or a panic. The STATIC counterpart — whether anything installed satisfies a `requires`/`optional` declaration at all — is checked once, at `ConwayBuilder::build`, before any call is possible; this row is the RUNTIME path a `requires`-tier caller should never actually reach |
| Version | A provider declares a `semver::Version` (`CapabilityRegistration::version`), a field separate from its `HostCapability` name, never folded into the name string. A consumer that calls `CapabilityCallHandle::call_versioned` (rather than the unversioned `::call`) supplies its own `semver::VersionReq` — `^1` for an ordinary floor, `=1.2.3` for a hard pin — checked as `req.matches(&version)` before the call ever reaches `CapabilityProvider::call`. A mismatch **refuses** as `CapabilityCallError::VersionMismatch { capability, required, available, available_declared }`, naming both `required` and `available` (`available_declared` distinguishes an author-declared `available` from the `0.0.0` fallback a missing/unparseable manifest version degrades to — see that variant's own doc), never degrading (decision `01M189XS6Z9VKYENAHNY1B54CM`, `docs/vision/DESIGN-plugin-dependencies.md` §7b/§9). No candidate selection: the Ordering row below already means one capability name has exactly one provider, so there is nothing to select among — `VersionReq::matches` is a predicate over a single pair. **`call_versioned` now has an in-tree caller** — see the Status row below |
| Ordering | Exactly one provider per capability name, enforced at construction: `CapabilityRegistry::from_registrations` refuses to build (`DuplicateCapabilityProvider`) if two installed plugins register the same name — never "first installed wins" or "last installed wins" |
| Status | **Both the unversioned and the versioned channel are implemented and have real consumers.** `conway_core::ports::capability::{CapabilityProvider, CapabilityRegistry, CapabilityHost, CapabilityCallHandle, CapabilityRegistration, CapabilityError, CapabilityCallError}`; a plugin registers via `Plugin::capabilities() -> Vec<CapabilityRegistration>`, and a caller reaches the channel through `ToolCtx::capabilities: CapabilityCallHandle`, present on every dispatched `Tool::invoke`. The unversioned `::call` has its real consumer in `conway-plugin-subprocess` (board item `01M0WWNHQQYN1EVTH8WPZ33EBF`), which forwards this generically for a configured subprocess plugin declaring `provides` (`docs/plugins/subprocess-plugins.md`'s "Providing a capability" section) — its own declared version is borrowed from that plugin's `PluginManifest::version`, parsed as semver, degrading to `0.0.0` when that string is not valid semver (never a panic on plugin-supplied input). This module registers no capability of its own — it is the channel only, exactly as its own module doc states. **`CapabilityCallHandle::call_versioned` (versioning, decision `01M189XS6Z9VKYENAHNY1B54CM`) is no longer a bare forward declaration** (board item `01M0WWPA70E8YAAN981EK10D3D`): `conway-plugin-ui` (`conway.ui`) is the first FIRST-PARTY plugin to register a capability — `ui.form`, at `1.0.0` — and `conway-plugin-skeleton`'s `skeleton_ask` tool is the first in-tree caller of `call_versioned` itself, supplying `^1`. Every OTHER production call site (including every `conway-plugin-subprocess`-forwarded call) still goes through the unversioned `::call`; that is a fact about what has adopted versioning so far, not a limitation of the mechanism. |

**The property that makes this a different trust shape from every gated `Tool`
call in this document, stated exactly.** A `Tool` call is proposed by the
MODEL and passes through `PermissionGate`/`PermissionBroker` before it
runs — there is an operator gate between the caller and the code that
executes. A capability call has neither: no model proposal step, no
permission check, nothing an operator sees before or after it runs. The
calling plugin names a `HostCapability` — a string, matched by
`CapabilityRegistry::call` — never an implementation, a crate, or a
declaring plugin's id. **Installing one plugin can therefore cause a
SECOND, unrelated plugin's code to run**, purely because the first named a
capability the second happens to provide; nothing about the mechanism
requires the two to be authored by the same party, know about each other,
or be reviewed together.

**Trust posture: see `docs/plugins/trust-and-security.md`'s "Plugin-to-plugin
capability calls" section.**

**Whether `Plugin::hooks()` and `conway.statusline` also warrant their own
numbered points here, considered and declined (board item
`01M12XR5QBS9S0G89TXA73C1KC`, finding `01M12WKMKJVE26EA6SYGB0PNZQ`).**
`Plugin::hooks()` is not a new extension point in this catalogue's sense —
it is a new PRODUCER of point 13's existing `hooks` charter, already
documented in that point's own Status row; a second point describing the
identical dispatch mechanism a second time would fork one fact into two
entries that could drift against each other. `conway.statusline` is a
shipped PLUGIN, not a new `Plugin` trait method — like the other nine
first-party plugins `docs/plugins/README.md` lists, it is a CONSUMER of an
existing point (`status.declare/1`, point 12) plus its own config surface,
and no first-party plugin gets its own numbered entry in this list; giving
`conway.statusline` one while `conway.fs`, `conway.subagent`, and the rest
have none would be inconsistent, not more complete. Both surfaces get a
trust-page entry instead (this item's actual deliverable), which is a
different obligation from a spot in this catalogue.

## The permission decision ordering

This is the ordering most likely to be reasoned about incorrectly, so it is
stated as a numbered pipeline twice: what `PermissionBroker::decide`
(`crates/conway-runtime/src/permission.rs`) **actually does today**, and,
separately, what the design's policy-chain overlay would add if points 7 and
8 above were ever built. Do not conflate the two.

### Today, as shipped

1. **Gate-routing for unconfinable calls under a root** (`PermissionBroker::
   check_root`, narrowed since "Retire the harness-level confinement root
   once `conway.fs` enforces its own"). This no longer checks a
   `PathArgs::Named` tool's own declared path arguments at all — `conway.fs`
   does that itself, open-relative, inside `read`/`write`/`edit`/`cd`/
   `glob`/`grep`, AFTER this decision returns `Allow` (see
   `conway_tools::fs::beneath`). What remains here: `AgentRoot::Broken`
   still denies immediately — before the cache, patterns, `AutoAllow`, or the
   gate are ever consulted; and `PathArgs::Unconfinable` (e.g. `bash`'s own
   free-form `command`, which belongs to a different plugin `conway.fs` has
   no jurisdiction over) still forces every later step past the
   cache/pattern/`AutoAllow` shortcuts, straight to the gate, under a
   configured root — a gate-routing policy, not a containment check; its
   OWN `checkable` sub-arguments (e.g. `bash`'s `cwd`) are still walked and
   denied here directly.
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
6. **Pattern-allow rules** (point 6's `then: allow`) — refused unconditionally
   for a tool whose `RenderKind` is `ShellCommand` (e.g. `bash`; see
   `docs/permissions.md`'s Limits section), matched by prefix as written for
   every other tool — consulted only if `must_reach_gate` is still false.
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

the extension design specifies where a composed
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

- **Context editing (points 3, 4, 9, 13).** Composition across *multiple*
  hooks is REACHABLE as of board item `01KZRZZP6A4A27R3EN0HQAENBS`: a Rust
  `ContextHook` (points 3/4) and any number of configured `request_assembled`/
  `context_overflow` script hooks (point 13) can all be registered on the
  same event at once. The rule this item implements, exactly as specified:
  **every hook is evaluated independently against the same pre-hook payload,
  never against another hook's output** (chaining is rejected outright — it
  is exactly the "no fixed point under two rewriters" hazard restated for
  context) — `conway_runtime::context::script_hook`'s own module doc and
  `crates/conway-runtime/tests/context_hook_scripts.rs`'s
  `a_rust_context_hook_and_a_script_hook_coexist_on_the_same_turn` test
  exercise this directly. Exclusion composes as a set union — two hooks
  excluding the same segment is not a conflict, `{X} ∪ {X} = {X}` — and
  addition (append) stays independent and attributed (every appended segment
  carries `Provenance::SystemNote { reason: "script_hook:<id>" }` naming the
  hook that added it), with declaration order observable only as
  *presentation* order among non-conflicting appends, never a semantic
  tie-break. **What is NOT built:** a same-target *replace* collision between
  two hooks, because neither the in-process `ContextHook` (point 3's own
  `ContextPayload`) nor the script-hook vocabulary (`ContextDelta`) can
  express a "replace" edit at all — there is no target-plus-new-value shape
  for two hooks to collide over, so the "fails to exclusion" resolution point
  9 and the extension design describe remains reachable only once
  `01KZ844ZXZMVRWC7ZANT7PSM6X` (the `context.hook/1` replace gap) ships a
  replace primitive for SOME surface to collide through.
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
| Policy chain (8, designed) | `on_failure`, default `Deny`, **never `Allow`** — design only for THIS point's own composed, per-call, inference-evaluated chain; the same `on_failure: Deny | Prompt` shape is now BUILT one row below, for point 13's declarative `pre_tool_use` hook only (see that row, and point 8's own "Correction" above) | Clamped `timeout_ms`, design default 60 s; **excluded from any progress-reset rule** — a decision-bearing call's deadline never extends on a progress notification, so a hook that emits progress forever while never deciding cannot stall the session (§16.2d) | Design only | Design only | Design only | No chain exists; unaffected | Design only |
| `context.hook/1` (9, designed) | Design only | Design only, same clamped-timeout shape as 8 | Design only | Design only | Design only | No wire transport exists | Design only |
| Declarative hooks (13) | `prompt_submitted`: unconditionally fail-closed (no `on_failure` field exists for it). **`pre_tool_use`: fail-closed BY DEFAULT, per registration, via `HookEntry::on_failure` (`HookOnFailure::Deny | HookOnFailure::Prompt`, default `Deny`, never `Allow` — board item `01M0X1AH44SNMK5TZ507K30QNP`)** — `Deny` denies outright (byte-identical to before this field existed); `Prompt` forces the operator's own gate instead of denying, never a widening. Logged and swallowed (fail-open) for the remaining seven events, INCLUDING `request_assembled`/`context_overflow` -- a failing context-editing hook contributes no `ContextDelta`, exactly as if it had answered with an empty one | `pre_tool_use`: routed through the SAME per-registration `on_failure` policy as every other runner failure — a timeout is just another `HookFailure`. `prompt_submitted`: fail-closed, same as error. Every other event: logged and swallowed | Runner-reported (`HookFailure`), never a raw process panic reaching the caller | Unparseable `HookAnswer` stdout is a `HookFailure`, handled identically to any other runner error — for `pre_tool_use`, also routed through `on_failure` like any other failure, never a silently-guessed verdict of its own | An unrecognized `HookPermissionVerdict`/`ContextDelta` shape fails to deserialize (`HookAnswer`'s own `deny_unknown_fields` posture on `permission`) — see `conway_core::hook`'s own tests; a malformed INDIVIDUAL `ContextDelta::appends` item (well-formed JSON, wrong per-item shape) is skipped with a `tracing::warn!`, never applied and never failing the rest of that hook's own valid work (`conway_runtime::context::script_hook::apply_script_deltas`'s own tests) | A `hooks` block is parsed, deny-unknown-fields-strict, and validated whether present or absent. **All NINE core events now dispatch** — see point 13's Status row, which is normative — so a rule naming any of them DOES run, given an injected runner; a `match` on an event carrying no tool name is a load-time config error, never a silently-inert rule | Design only |

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
(`trust-and-security.md`); this page states only what each point's
failure table needs ("de-trusted") and defers the mechanism. The wire
transport itself was designed and never built; this page
describes points and contracts, which are transport-independent, not frame
formats.
