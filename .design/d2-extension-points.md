# D2 — Extension-point taxonomy: what a non-Rust plugin can participate in

Status: design spec (board item 01KYNN8SKK10WKQAKBTRYXWY9J). Written against
HEAD `9a8d882` (v0.6.0). Transport/framing is D1, wire vocabulary D3, trust
D4, UI template language D5.

> **Partly superseded by D6** (`extension-architecture.md`), the synthesis.
> Where this document and D6 disagree, **D6 wins** — it reconciled the five
> specs against each other and revised two of this document's proposals.
>
> - **§4's `Plugin::on_init` as "the connect" is void.** `on_init` had zero
>   call sites and has since been *removed* from the trait (D6 §11.6). Any
>   design here that leans on it needs a different hook.
> - **§6's single verdict enum plus `may_allow: bool` is rejected** in favour
>   of a type split, `NarrowingPolicy` / `DecidingPolicy` (D6 §5.2).
> - **§10 and §13's "pattern grants are structurally inert for every tool
>   except `bash`" is false as of `68ea9b1`** (2026-07-30), which gave every
>   tool a `RenderKind` declaration and taught the matcher to apply the
>   metacharacter gate only to `ShellCommand` renders. See the local status
>   notes at §10 and §13 for what changed and why the finding mattered more
>   than its own wording suggested.
> - **§1's R1 ("one authority per value: veto or narrow, never rewrite") is
>   scoped, not widened, by the 2026-07-30 hooks/scripts redirect.** Read in
>   isolation R1 sounds like it binds every value a participant touches,
>   including context — and D6 §9 (`updatedInput`) reads the same way. It
>   does not: R1 governs `ToolCall::arguments` and permission verdicts.
>   **Context is not an authority grant**, and `ContextHook::before_request`
>   has permitted editing/dropping a segment since it was written; R1 was
>   never meant to forbid that, only argument rewriting and verdict
>   widening. D6 §5.9 states the three-value-class boundary this bullet is
>   the pointer to, and D6 §16 is the fuller redirect record.
>
> Kept unedited as the record of the reasoning, not as current guidance.

---

## 1. The organizing principle

Two structurally different kinds of extension point, and the difference is not
a matter of degree:

- **Observers** receive information and return nothing. They cannot change
  behavior, so they may lag, fail, or be absent without changing the run.
  They live *beside* the facade — the fan-out already exists
  (`conway-runtime/src/events.rs`, `EventBus`) and already leaves the process
  (`conway-cli/src/render/jsonl.rs`).
- **Participants** return a value the runtime acts on. They must live *behind*
  the facade, because the return path needs a `Runtime` handle the CLI cannot
  hold. They are bounded in time, fail closed, and compose under a rule that
  makes registration order unobservable.

Three rules follow, and every decision below is an application of one of them:

- **R1 — One authority per value.** A participant may *veto* a value or
  *narrow* it monotonically. It may never *rewrite* a value some other check
  has already read. (This is what forbids `updatedInput`; §9.)
- **R2 — Declarative metadata beats a round trip.** Anything that is a static
  property of the extension (which tools it wants, which events, which tools
  it hides, what its timeout is) is declared once at registration and
  evaluated in-process, with a fail-closed default. This is exactly the shape
  `Tool::path_args` was given in v0.6.0 (`conway-core/src/ports/plugin.rs:107`,
  default `PathArgs::Unconfinable`) and it generalizes.
- **R3 — Policy is not a floor.** The permission broker's root check sits
  above all four allow paths deliberately (`conway-runtime/src/permission.rs:476`).
  No extension point may be placed above it, beside it, or in a position that
  can widen it. A guarantee implemented as plugin policy fails open when the
  plugin is absent; therefore guarantees stay in the harness and plugins get
  the space *below* the floor.

### The tier line, and how it satisfies P-6

P-6 ("no privileged API") is read **within a tier**, and the tiers are
host-vs-extension, not builtin-vs-third-party:

- **Host ports** are implemented by whoever constructs `ConwayBuilder` — the
  embedder. They are in-process, unbounded, and hold authority by
  construction: `Backend`, `SessionStore`, `Router`, `HealthRegistry`,
  `PermissionGate`, `ContextHook`, `EventSink`, `SubagentHost`. The embedder is
  the harness for this purpose. Choosing the gate is not an API, it is
  ownership.
- **Extension points** are implemented by plugins. Every plugin — a Rust
  `Arc<dyn Plugin>` handed to `with_plugin`, or a remote process behind D1's
  transport — reaches exactly the same points with exactly the same authority
  and the same failure semantics. There is no point a built-in plugin can
  reach that a third-party plugin cannot.

GP-03 is satisfied because every extension point is *implemented on top of*
an existing host port and its decision flows through that port: the policy
chain lives inside `PermissionBroker` (which is what `PermissionGate` feeds),
plugin tools are `Tool`s in the one `PluginRegistry`, observers are fed by the
one `EventBus` behind `EventSink`. There is no second mechanism and no path
that goes around a port.

---

## 2. The extension-point table

Point ids are proposed; D3 owns the wire spelling.

| Point | Direction | Kind | Receives | May return | On error / timeout / garbage / absent |
|---|---|---|---|---|---|
| `tool/1` | host → ext → host | **Participant** | `ToolCall { call_id, name, arguments }` + an RPC-shaped `ToolCtx` subset (`agent_id`, `session_id`, `cwd`, `cancel`, `config`) | `ToolOutput { blocks, is_error, truncation, artifacts }` | Model-visible tool error (`ToolOutcome::error`), never an abort. Matches today's runner, which already converts a panicking tool into an error outcome (`runner.rs:205`). Absent → the tool is not registered; a call to it is `unknown tool`. |
| `tool.spec/1` | ext → host, registration only | **Declarative** | — | `ToolSpec { name, description, schema, category, permission }` + `render` template + `path_args` | Malformed → the tool is rejected at registration, named in a diagnostic. `path_args` absent → `Unconfinable { checkable: [] }`. `category`/`permission` absent → `Execute` / `Dangerous` (most restrictive). |
| `permission.policy/1` | host → ext → host | **Participant** | `PolicyRequest` (§6) — raw `arguments`, `rendered`, tool, category, agent path, session, cwd, root, mode, `must_reach_gate` | `Deny { reason } \| Abstain \| Allow { reason }` — and `Allow` only if the operator set `may_allow` for this policy, and only when `!must_reach_gate` | `on_failure`, default **`Deny`** (operator may set `abstain`). Never `Allow`. Absent → contributes nothing; the floor is unaffected because the floor is not policy. |
| `permission.rules/1` | ext → host, registration only | **Declarative** | — | a rule set (§10), effects restricted to `deny` / `prompt` | Unparseable rule → **rejected, named**, plugin marked degraded. Never guessed at (`PatternRule::parse`'s own rule). |
| `observe/1` | host → ext | **Observer** | `Envelope` (`seq`, `ts`, `session`, `agent`, `event`), filtered by the declared event selector | *nothing* — the point has no reply channel, structurally | Ignored. Slow → `Event::Lagged` then dropped delivery (the §8 guarantee, already implemented). Dead → unsubscribed + `PluginStatusChanged`. The run never waits. |
| `status/1` | ext → host | **Observer output** | — (pushed by the plugin) | `StatusContribution { key, value, ttl }` — last-known-value, TTL-expiring | Stale contribution expires; the UI renders without it. The render path never calls a plugin and never blocks on one. |
| `context.append/1` | host → ext → host | **Participant (additive)** | `ContextHookCtx { agent_id, session_id, turn, model, estimated_tokens }` | `Vec<PromptSegment>` to **append**, each stamped `Provenance::Plugin { id }` | Contribution dropped, turn proceeds with the unmodified payload, `PluginStatusChanged`. Dropping an addition removes authority, so this direction is the closed one. |
| `context.tools/1` | ext → host, registration only | **Declarative** | — | a tool selector naming tools this plugin hides from announcement | Declarative, so it cannot fail at turn time. A selector matching nothing is a registration error (§8). Note announcement is not a permission boundary either way (`tools/registry.rs:132`). |

Everything in that table is either a `Tool`, an `EventSink` consumer, or a
decision fed into `PermissionBroker` / the context assembly — i.e. four of the
ten existing ports, reached through their existing call sites.

---

## 3. The ten ports: reachable, and excluded with reasons

**Reachable from outside Rust**

| Port | How |
|---|---|
| `Plugin` | The unit of registration. A remote extension is *one* `Plugin` from the runtime's point of view: `manifest()` is the handshake, `tools()` is its tool list, `on_init` is the connect. No new registration mechanism (GP-03). |
| `Tool` | `ToolCall` and `ToolOutput` are already fully serializable and the plugin doc already names this as the T-8 path (`ports/plugin.rs:256`). Needs an RPC-shaped `ToolCtx` (D1/D3). |
| `PermissionGate` | **Indirectly only.** A plugin does not implement the gate; it registers a `PermissionPolicy` consulted inside `PermissionBroker`. See §6 for why the indirection is load-bearing rather than fussy. |
| `ContextHook` | **Indirectly and partially.** A plugin contributes appended segments (dynamic) and hidden tools (static). Arbitrary segment rewriting/dropping stays a host capability. |
| `EventSink` | **Indirectly.** A plugin cannot *be* an `EventSink`; it subscribes behind the host's one sink. |

**Excluded, and why — the exclusions are as load-bearing as the inclusions**

- **`SessionStore` — no.** It is the persistence hot path (`append` per record)
  and the audit trail. Putting it behind a pipe means every appended log record
  blocks on IPC, a plugin crash is data loss, and — worse — the record of what
  happened becomes extension-controlled, which makes every trust claim in D4
  unverifiable. `fork` is additionally O(1)-by-design and load-bearing for
  tournament patterns (`ports/session.rs:49`); a remote implementation would
  silently make that a per-call network cost.
- **`Router` — no, structurally.** `Router::resolve` is a **synchronous**
  `fn` (`ports/routing.rs:30`). A synchronous port cannot be crossed by an
  async RPC without blocking a runtime thread; that is a structural exclusion,
  not a preference. Its contract is also unenforceable across a boundary:
  "MUST be pure with respect to request content" and "MUST NOT mutate breaker
  state" are guarantees a remote process cannot be held to. Routing policy is
  already declarative config in `conway-routing` — that is the right shape for
  it.
- **`HealthRegistry` — no, same reason.** Both methods are synchronous
  (`ports/routing.rs:35-36`), consulted per routing decision, and one of them
  *mutates* breaker state. A remote registry fails open into "everything is
  healthy," which is the worst possible failure for a circuit breaker.
- **`Backend` — no, and it needs no help.** Conway already speaks HTTP to
  backends. "A non-Rust backend" is already solved by running a server that
  speaks a dialect `conway-backends` supports; wrapping the streaming
  `BoxStream<StreamChunk>` path in a second plugin transport buys nothing and
  puts provider credentials in a third place. If a genuinely new dialect is
  needed, that is a `conway-backends` change or an embedder-supplied
  `Arc<dyn Backend>` — host tier.
- **`SubagentHost` — not implementable.** It is the runtime's cycle-breaker
  (`ports/subagent.rs`); implementing it means being the runtime. Note the
  *other* direction: plugin tools want to **call** it (`ToolCtx.subagents`).
  That is a host-callback surface, and D1 owns whether the transport is
  bidirectional. **Disclosed asymmetry:** until the callback channel exists, a
  remote tool can emit progress but cannot fork/spawn/ask, while a Rust tool
  can. That is a transport limitation, not a privilege one, and it closes when
  D1 lands duplex calls. It must be *disclosed in the plugin docs*, not
  discovered.
- **`EventSink` as an implementable port — no.** `emit` is synchronous and
  "non-blocking **by contract**" (`ports/events.rs:11`). A remote sink cannot
  honor that contract; a plugin that could would be able to stall every
  producer in the runtime. Observers subscribe behind the host's sink instead,
  where the existing lossy-with-notice guarantee already protects the runtime.

---

## 4. The `Event` enum as the observer surface

`Event` is already the right surface: `#[non_exhaustive]`, `serde(tag =
"event")`, flattened into `Envelope` as exactly one JSON object per line, with
three delivery guarantees restated at the definition site
(`conway-core/src/event.rs:29-35`), and the file already anticipates an
external consumer ("A future ACP shim filters `agent == root` … nothing in
this enum precludes that", `event.rs:49`).

**The observer point is `Envelope`, not `Event`.** The envelope's
`seq`/`session`/`agent` are what make the stream reconstructible; an observer
that receives bare events cannot order them or attribute them.

### Genuine gaps — five additions, not thirty

Claude Code's ~30 hook events mostly map onto events conway already has:
`SessionStart`/`SessionEnd` ≈ `AgentSpawned`/`AgentFinished` (which also carry
`parent` and `kind: SubagentMode`, so `SubagentStart`/`SubagentStop` need
nothing new); `UserPromptSubmit` ≈ `UserTurn`; `PreToolUse`/`PostToolUse` ≈
`ToolCallProposed` / `PermissionRequested` / `PermissionResolved` /
`ToolCallStarted` / `ToolCallFinished`; `Stop` ≈ `TurnFinished`.

Deliberately **not** added:

- `PreCompact`/`PostCompact` — conway has no compaction. The nearest concept is
  `ContextHook::on_overflow`, which is a hook, not an event, and fires only
  when the assembled payload still overflows the routed model's window.
  Inventing compaction events would describe a feature that does not exist.
- `Stop` as a *blocking* point — conway has no "prevent the agent from
  stopping" concept, and should not acquire one at the plugin tier.
  `result_contract` and steering already cover the legitimate want.
- `ConfigChange` — config is load-time. A live-reload event would document a
  feature that does not exist.

Added, each because something in this design or an existing operator-visible
state change is otherwise unobservable:

1. **`CwdChanged { cwd }`** — v0.6.0 shipped `cd` and a mutable `CwdHandle`
   (`ports/plugin.rs:246`), and *nothing on the stream says the agent moved*.
   An observer reconstructing state from the stream cannot know the cwd, and
   the status line (use case 4) needs it directly. Emit at the same point the
   next batch snapshots it, so the event and the observable effect agree.
2. **`PermissionModeChanged { mode }`** — `PermissionBroker::set_mode`
   (`permission.rs:303`) can move a live session into `AutoAllow`, and the
   stream records nothing. A session's audit trail that cannot show when
   prompting was turned off is not an audit trail.
3. **`PermissionGrantChanged { change }`** — `remember_pattern` and
   `revoke_all_grants` create and destroy durable authority
   (`permission.rs:314`, `:430`) invisibly. `PermissionResolved { Cached }`
   shows a grant being *used*, never one being *made*.
4. **`PluginStatusChanged { plugin, status, detail }`** — required by this
   design: fail-closed denials, timeouts, transport drops, selector mismatches
   and name collisions must be attributable. A remote plugin dying is
   currently invisible; with this design, a plugin dying can *deny tool calls*,
   which must never be a mystery.
5. **`AgentSpawned { root }`** (a field, additive with `#[serde(default)]`,
   not a variant) — an observer cannot tell whether an agent is confined, or
   to what. Same serde-default technique as `ephemeral` (`event.rs:66`).

Costs to state plainly: `event.rs`'s `all_variants()` count assertion
(`event.rs:420`) must move with each addition — that assertion exists to force
this conversation — and each addition is a D3 wire-vocabulary commitment.

---

## 5. Facade reachability, and the re-exports this design implies

**Finding (current tree, independent of this design): `Tool` is re-exported
but not implementable through the facade.** `conway/src/lib.rs:60` exports
`Plugin` and `Tool`; `lib.rs:56` exports only `ToolCategory` and `Usage` from
`content`. An external crate therefore cannot name `ToolSpec`, `ToolCall`,
`ToolOutput`, `ToolCtx`, or `ToolError` — i.e. it cannot write the trait's
method signatures. GP-03's single extension mechanism is, today, unreachable
from outside the workspace. Fixing this is the minimal enabling change for
everything in this document and is worth doing on its own.

**Decision: one curated `pub mod plugin` on the facade**, following the
existing precedent for punching a hole (`pub mod permission_pattern`,
`lib.rs:53`, added for exactly this reason). One module, documented as
"everything you need to implement an extension," rather than doubling
`lib.rs`'s flat re-export list. Contents:

- from `conway_core::content`: `ToolSpec`, `ToolCall`, `ToolCategory`,
  `PermissionClass`, `ContentBlock`, `TruncationPolicy`, `Artifact`,
  `ArtifactKind`
- from `conway_core::ports`: `Tool`, `Plugin`, `ToolCtx`, `ToolOutput`,
  `PathArgs`, `PluginManifest`, `PluginConfig`, `PluginInitCtx`,
  `CancellationToken`, `CwdHandle`, `EventSink`, `EventSinkHandle`,
  `SubagentHost`, `ContextHook`, `ContextPayload`, `ContextHookCtx`,
  `OverflowInfo`
- from `conway_core::error`: `ToolError`, `PluginError`
- new: `PermissionPolicy`, `PolicyRequest`, `PolicyVerdict`,
  `PolicyRegistration`, `ExtensionSelector`, `StatusContribution`

`ContextHook` in particular is currently unreachable even though
`ConwayBuilder::with_context_hook` exists (`builder.rs:214`) — the builder
accepts a type no external caller can name.

---

## 6. The participant-vs-facade problem: resolved on the broker, not the gate

Three candidate resolutions were on the table: a `set_permission_gate` setter,
a channel-based gate like `TuiGate`, or a composite gate dispatching to
registered plugins.

**Decision: none of the three. The gate slot is not touched. Late binding goes
on `PermissionBroker`, as a policy chain, mirroring `set_context_hook`.**

The decisive argument is structural and reads straight off `decide`
(`permission.rs:465-592`): there are four ways to return `Allow` and **only one
reaches `gate.check`**. A composite `PermissionGate` — however elegant —
**cannot see** a call answered by the cache, by a pattern grant, or by
`AutoAllow`. A guardrail plugin installed as a composite gate would therefore
evaporate in precisely the modes where it matters most. The composition rules
this design needs (a veto that beats `AutoAllow`; an allow that is
subordinate to the root check) are not expressible inside a single
`PermissionGate` implementation. They are expressible exactly one place: the
broker, which is where the ordering already lives.

Secondary reasons: `PermissionGate`'s contract says it "may block
indefinitely" because a human is on the other end (`ports/permission.rs:11`);
a policy must be *bounded*. Two different temporal contracts should not share
a trait. And this is purely additive — no `RuntimeDeps` change, no breaking
change to any existing caller, the same reasoning `set_context_hook` records
for itself (`runtime.rs:377-387`).

**Shape.** `PermissionBroker` gains `policies: RwLock<Vec<RegisteredPolicy>>`,
read fresh per decision exactly as `LoopDeps::context_hook` is
(`agent_loop.rs:522`), with `Runtime::set_permission_policies` /
`register_permission_policy` as the late-binding hatch. Facade surface, for
P-8: `ConwayBuilder::with_permission_policy(...)` (library, one-shot) and
`Conway::register_permission_policy(...)` / `unregister(...)` (TUI
`/settings`, live).

**Where the chain runs inside `decide`.** The order is the whole point:

```
1. root check                   NON-DELEGABLE FLOOR (unchanged, still first)
2. plan-mode denial             NON-DELEGABLE FLOOR (unchanged)
3. policy chain — DENY half     runs for every call, including must_reach_gate
                                and including AutoAllow sessions
4. cache hit                    (skipped when must_reach_gate)
5. pattern grant                (skipped when must_reach_gate)
6. AutoAllow                    (skipped when must_reach_gate)
7. policy chain — ALLOW half    only if !must_reach_gate, only if some policy
                                allowed and none denied; never cached
8. gate.check                   the human
```

One invocation per matching policy per call; the deny half is applied at step
3 and the allow half remembered for step 7.

Two properties this ordering buys, stated as invariants a test can pin:

- **A policy can never widen the root.** When `check_root` returns
  `MustReachGate`, the allow half is skipped entirely, so an unconfinable call
  under a root still reaches the *human* gate. A policy's allow is exactly as
  authoritative as `AutoAllow` and lives in the same slot — never above it.
- **A policy veto survives every allow path.** A `Deny` at step 3 beats the
  cache, pattern grants, and `AutoAllow` alike — the same reasoning plan mode
  already gets (`permission.rs:502-509`).

**What a policy receives.** `PolicyRequest` carries `agent_id`, `agent_path`,
`session`, `tool`, `category`, **raw `arguments`**, `rendered`, `call_id`,
`cwd`, `root: Option<PathBuf>`, `mode`, `must_reach_gate`. It gets both the
raw arguments and the rendered string, and the contract says plainly:
`rendered` is sanitized and lossy (`runner.rs:396`, `sanitize_rendered`) and
**must not be the basis of a security decision** — the 0.5.0 laundering bug is
the worked example, and `check_root` reads `arguments` for exactly this reason
(`permission.rs:328-331`).

**What a policy may return.** `Deny { reason } | Abstain | Allow { reason }`.

- `Allow` requires the operator to have set `may_allow: true` at registration.
  **Default `false`** — a plugin cannot grant authority the operator did not
  knowingly delegate.
- A policy's `Allow` is **per call and never cached, never a grant**. Caching
  it would make the plugin's authority outlive the plugin, keyed by an
  argument digest the policy may not have inspected. Grants belong to the
  operator.
- No policy may return `AllowAlways` or install a `PatternRule`.

**Bounded.** Each policy declares `timeout_ms` at registration, clamped by an
operator-configured maximum (default 60s, generous because an
inference-evaluated policy issues its own LLM call). Exceeding it yields
`on_failure` (default `Deny`) plus `PluginStatusChanged`.

**Absence.** A `required: true` plugin whose load fails prevents startup;
otherwise the plugin is simply absent and its policy contributes nothing.
`required` makes absence *loud*; it does not make plugin policy a floor.
**Nothing does. Guarantees live in the harness.**

**Overlap with board item 01KYKPAW2AFYE284WCC894T87J** (a permission-policy
plugin port): `PermissionPolicy` as specified here *is* that item's port. D2
does not ask for a second mechanism — it asks that item to land this trait
shape (verdict set, `may_allow` default false, bounded, no grants) and this
position in `decide`. If that item lands first with a different shape, D2's
permission story reduces to "the remote bridge implements that port."

---

## 7. Ordering, composition, and name collisions

**Ordering: unspecified, because composition is commutative.** For every
permission-shaped point, composition is **most-restrictive-wins**, adopted
explicitly and by name:

> any `Deny` beats every `Allow`; `Allow` requires at least one allow and zero
> denies; all-`Abstain` falls through to the next step. Registration order
> cannot change the outcome.

This is better than specifying an order, because it removes the question. No
priority numbers: priorities invite an arms race between config authored by
different parties and make the result depend on who edited last.

The **only** place order is observable is `context.append/1` segment order,
and there it is **declaration order** — deterministic and stated.

`context.tools/1` hides are a set **union** (also commutative). Observers are
independent and their order is unspecified by construction.

**Name collisions: neither panic nor first-wins.**

Today: `PluginRegistry::from_plugins` correctly returns an error naming both
plugins and the tool (`tools/registry.rs:83-90`), and then `Runtime::new`
`.expect()`s it into a **panic** (`runtime.rs:304`). Fine for compiled-in
plugins — a registration bug. A P-10 violation the moment a remote plugin's
declared tool name can trigger it. Claude Code's answer (first-registered
silently wins) is a documented footgun and additionally makes the result
depend on load order.

**Decision: qualified identity, bare name only when unambiguous, nobody wins a
contested name.**

- Every plugin-provided tool has a stable qualified identity
  `{plugin_id}__{tool_name}` (`plugin_id` sanitized to `[A-Za-z0-9_-]`, the
  intersection of what mainstream providers accept in a tool name). The
  qualified form always resolves at dispatch.
- The **announced** name is the bare `tool_name` when exactly one plugin
  claims it.
- When two or more plugins claim it, **neither gets the bare name.** Both are
  announced qualified, a `PluginStatusChanged` diagnostic names both plugins
  and the tool, and the operator may pin the bare name to one plugin in config.
- Registration never panics and never silently drops a tool.

Properties: deterministic (independent of load order — the improvement over
first-wins); no silent shadowing (the improvement over Claude Code); no
built-in privilege, since a built-in that collides also loses the bare name
(P-6); and the model always has a callable tool either way. The cost is
honest: a collision changes the name the model sees for a previously-working
tool — but only at startup, loudly, and with a config fix available.

---

## 8. Matchers: one paradigm, non-matches detectable

**Chosen paradigm: exact name, or a single trailing `*` prefix wildcard.
Nothing else.** This is not new: it is `ToolSelector`'s existing rule,
implemented in `conway-core/src/agent.rs:363` and documented at
`agent.rs:341-344` ("an entry ending in `*` is a prefix match on the tool
name, otherwise it is exact equality"). `ToolSelector` is already
`#[non_exhaustive]` and already the codebase's answer to "which tools."

Rejected: regex — `permission_pattern`'s module doc argues the case against it
for permission surfaces better than I can (`git .*` reads tight, but `.`
matches `;`). Rejected: globs — `conway/src/gates.rs` uses `globset` for
*argument* patterns, which is already a second paradigm in the tree and should
be treated as a wart to converge, not a precedent to spread. Rejected
absolutely: Claude Code's mixed paradigm (exact unless special characters
appear, then regex), whose failure mode is a pattern that silently matches
nothing.

Selector shape (`ExtensionSelector`, `#[non_exhaustive]`): tool patterns
(above rule), `categories: Vec<ToolCategory>` (enum, no strings), and event
patterns matched against the serde tag with the same rule
(`tool_call_*`, `permission_*`). A point with no selector is never consulted.

**Three ways a non-match is detectable, not silent:**

1. **Parse-time rejection.** A pattern containing any character outside
   `[A-Za-z0-9_.-]` plus at most one trailing `*` is rejected at registration,
   with the offending character named. This is what makes Claude Code's
   "special characters silently switch paradigms" structurally impossible.
2. **Resolution-time verification.** After all plugins register (one phase, in
   `ConwayBuilder::build`, where the collision check already lives), every
   selector is resolved against the known universe — registered tool names and
   the `Event` tag set. A pattern matching **zero** known targets is a
   **registration error for a participant** (fail closed: a policy that
   silently never runs is the worst outcome) and a **warning +
   `PluginStatusChanged` for an observer**.
3. **Inspectability.** The host exposes the resolved match set per plugin
   (`conway plugins explain`-shaped), so an operator can see what a pattern
   actually selected rather than inferring it from behavior. Precedent:
   `PermissionBroker::active_patterns` exists for exactly this reason —
   "a rule set nobody can inspect is a trap" (`permission.rs:418`).

An empty selector on a participant point is a registration error, not a
no-op.

---

## 9. Argument rewriting (`updatedInput`): forbidden

**No participant may mutate `ToolCall::arguments`.** Four independent reasons,
each sufficient:

1. **The grant cache would have no defensible key.** `CacheKey::for_call`
   digests canonicalized arguments (`permission.rs:219`). Rewrite *after* a
   decision and the authorized bytes differ from the executed bytes. Rewrite
   *before* and a human's `AllowAlways`, granted against the original
   arguments, now covers rewritten ones (or the reverse — a grant the human
   made goes stale invisibly). There is no assignment of "which arguments were
   authorized" that survives both cases. The question in the brief has no good
   answer, which is itself the answer.
2. **It reintroduces the 0.5.0 bug class by construction.** `check_root` reads
   `arguments` and explicitly **not** `rendered`, because a safe-looking
   transformation sitting between evidence and check laundered exactly this
   (`permission.rs:328-331`). Argument rewriting *is* a transformation sitting
   between the model's proposal and every check that reads the proposal.
3. **The human approved a different string.** `rendered` is derived from
   arguments (`runner.rs:396`) and is what the operator saw in the prompt and
   what appears in `Event::PermissionRequested`. A rewrite after render means
   the approved sentence does not describe the executed call.
4. **No fixed point.** Schema validation happens before the proposal event
   (`runner.rs:268`). A rewrite requires re-validation, re-render,
   re-root-check — the whole pipeline re-entered — and with two rewriting
   plugins there is no guarantee of convergence and no commutativity.

**The sanctioned alternative already exists.** A plugin that wants different
arguments returns `Deny { reason }` — or, better, the runtime surfaces the
policy's reason the way `DenyWithFeedback { message }` already does
(`agent.rs:485`): model-visible feedback saying what to call instead. The
model re-proposes, and the new proposal enters the pipeline from the top with
one authority for its arguments. Cost: one model round trip. Benefit: "which
arguments were authorized" always has exactly one answer — the ones in
`Event::ToolCallProposed`.

Generalized as R1 (§1): a participant may veto or monotonically narrow; it may
never rewrite a value another check has already read. The same rule forbids a
`UserPromptSubmit`-style participant that edits the user's prompt text; the
sanctioned point for adding context is `context.append/1`, which is additive
and attributable.

---

## 10. General rules for tool use

**Finding, verified in the current tree: pattern grants are structurally inert
for every tool except `bash`.** `PatternRule::matches` gates on
`contains_shell_metacharacters(rendered)` before any prefix comparison, and
that gate includes `(`, `)`, `{`, `}` (`permission_pattern.rs:47`). Only
`bash` overrides `Tool::render` (`conway-tools/src/shell/bash.rs:132`); every
other tool uses the default `format!("{}({})", name, args)`
(`ports/plugin.rs:83`), whose output always contains parentheses and, for any
object-shaped argument set, braces. So `read({"path":"a.txt"})` can never
satisfy any rule — and because the gate is applied to the wildcard case too
(`permission_pattern.rs:130`), **even `read:*` never matches.** The grant
language today is a prefix on a rendered string, and for non-`bash` tools that
string is a JSON dump the gate rejects by construction.

> **Status (2026-07-30), `68ea9b1`.** This finding no longer holds, and D4 §1
> carries the fuller note: the fix landed the same day as, and hours before,
> a permission-consent fix (`d917ba2`) that D4 §1's own threat model cited
> this finding to bound. That made the pre-fix threat *larger* than stated,
> not merely stale — the dangerous direction for a threat model to be wrong
> in. `Tool::render_kind() -> RenderKind`
> (`ShellCommand` | `Structured`) is now consulted by
> `PatternRule::matches_render`, and the metacharacter gate applies only to
> `ShellCommand`. `read:*` now matches. The "different predicate" this
> section goes on to propose (`paths_under`, `command_prefix` scoped by
> render kind) is still unbuilt — F12 in `extension-architecture.md` §12 —
> but the premise that the *existing* prefix language could not reach
> non-`bash` tools at all is gone.

So "all reads under `./src` are fine, writes prompt" is not expressible today
and cannot be made expressible by extending the prefix language. It needs a
different predicate — and v0.6.0 shipped the machinery for it:
`Tool::path_args` (declarative path argument names) plus
`CanonicalRoot::contains` and `resolve_like_the_tool_will`
(`permission.rs:165`).

**Proposed rule shape** — a small, closed set, evaluated in-process by the
broker:

```
Rule { select: ExtensionSelector, when: Condition, then: Effect }

Condition ::= paths_under(prefix)      // over the tool's declared path_args,
                                       // resolved exactly as check_root does;
                                       // an Unconfinable tool NEVER satisfies
                                       // it (fail closed, same asymmetry)
            | command_prefix(s)        // today's PatternRule semantics,
                                       // metacharacter gate intact,
                                       // for shell-shaped renders only
            | category_in([..])
            | always

Effect    ::= allow | prompt | deny
```

The example becomes:

```
{ select: tools ["read","grep","glob"], when: paths_under("./src"), then: allow }
{ select: categories [Edit, Delete],    when: always,               then: prompt }
```

Composition is the same most-restrictive-wins rule (§7): `deny` beats
`prompt` beats `allow`, and an `allow` rule is honored only where `AutoAllow`
would be — below the floors, never for `must_reach_gate`.

**Who may author which effect.** A **plugin-contributed** rule may only be
`deny` or `prompt`. `allow` rules come from operator-owned config — the
existing `PermissionFile` (`conway-core/src/permission_pattern.rs`) is the
right home. Rationale: an allow rule is a durable grant, grants belong to the
operator, and a plugin shipping "allow all bash" would be a supply-chain
escalation with no prompt anywhere. A plugin may *suggest* allow rules; the
host presents them for one-time operator acceptance into the operator's own
file. That consent ceremony is D4's.

**Why rules and not only a live policy.** A rule set crosses the wire once at
registration and is evaluated in-process: no per-call RPC, no timeout risk, no
fail-open window, and it works when the plugin is dead. This is R2, and it is
the same reasoning `Tool::path_args` records for choosing declarative argument
*names* over a computed path (`ports/plugin.rs:93-98`). The live
`permission.policy/1` point remains the escape hatch for genuinely dynamic
decisions — use case 1 needs it; a "reads under ./src" rule does not.

---

## 11. The four use cases

**1 — Inference-evaluated permission gate.**
Point: `permission.policy/1`. Registration: selector
`{ tools: ["bash","write","edit"], categories: [Execute, Edit, Delete] }`,
`timeout_ms: 20000`, `on_failure: deny`, `may_allow: true` (which the operator
must set explicitly — that is the informed consent for "a model may approve my
tool calls"). Receives `PolicyRequest` including raw `arguments` and `root`.
Returns `Deny { reason } | Abstain | Allow { reason }`. Its allow is honored at
step 7 only, is never cached, never becomes a grant, and is skipped entirely
when `must_reach_gate` — so it cannot widen a confinement root. Its deny fires
at step 3, so it still guards an `AutoAllow` session. Timeout or crash denies
the calls it selected, attributed via `PluginStatusChanged`. It issues its own
LLM call in its own process with its own credentials (a host inference
callback is deliberately not in v1 — D1/D3).

**2 — MCP shim.**
Point: `tool/1` + `tool.spec/1`; the shim is one `Plugin` fronting one MCP
server. `ToolSpec` ↔ MCP `Tool` is as close as it looks, with three real
seams:
- `name`↔`name`, `description`↔`description` — clean.
- `schema: schemars::schema::RootSchema` ↔ `inputSchema` — a JSON Schema value
  deserializes into `RootSchema` and unknown keywords survive in
  `SchemaObject::extensions`, and the registry compiles it with
  `jsonschema::validator_for` (`tools/registry.rs:99`). Workable, but the
  draft-07-flavored typed model vs. MCP's typical 2020-12 is a compatibility
  risk to test explicitly, not assume. (A future `ToolSpec.schema:
  serde_json::Value` would remove the seam entirely; that is a D3 call.)
- MCP has **no** `category`, **no** `permission` class, and **no** `path_args`.
  The shim must synthesize all three, and the default is the most restrictive
  one: `category: Execute` (so plan mode denies it), `permission: Dangerous`,
  `path_args: Unconfinable { checkable: [] }`. This is `Tool::path_args`'s own
  fail-closed default generalized to the whole spec.
- **Server-declared hints never reduce restriction.** MCP `readOnlyHint` /
  `destructiveHint` are claims by the extension about itself; honoring them
  would let a server declare `readOnly` to walk past plan mode. Only operator
  config may lower a tool's classification.
- Outputs: MCP content blocks → `ContentBlock` (text and image map directly;
  resource links need a `ContentBlock`/`Artifact` decision from D3),
  `isError` → `ToolOutput.is_error`, `TruncationPolicy` has no MCP analogue so
  the shim declares the host default. MCP prompts/resources/roots are out of
  scope for v1: only `tools/*` maps.

**3 — Plugin interface compatibility.**
Compatibility attaches to **the point set plus the manifest**, not to the
plugin as a whole: each point is independently versioned (`tool/1`,
`permission.policy/1`, …) and `PluginManifest` declares which points it
implements at which version. `PluginManifest.required_host_caps` already
exists for this and is currently **inert** — nothing in the tree reads it (only
`vec![]` literals in tests). This design gives it its job: named host
capabilities a plugin requires. Also: `PluginManifest.version` is a free-form
`String` and should be semver-validated at registration.
Failure rule, mirroring the observer/participant asymmetry that runs through
this whole document: a plugin declaring an **unknown or unsupported version of
a participant point fails to load** (a permission policy that silently does not
run is the worst failure mode); an unknown **observer** point degrades with a
warning. Handshake failure = plugin absent, loudly; startup is refused only
when the plugin is `required: true`.

**4 — Status line / UI instrumentation.**
Points: `observe/1` (in) + `status/1` (out). The plugin subscribes to a
declared event selection and pushes `StatusContribution { key, value, ttl }`;
the host keeps a last-known-value registry the UI reads. D5 owns the template
language; D2's placement decision is the discipline around it: **the render
path never calls a plugin and never waits for one.** A dead plugin's fragment
expires by TTL rather than freezing a frame. This use case needs no runtime
change at all — it is an `EventSink` subscriber plus a host-side registry —
but it is the main consumer of the new `CwdChanged` /
`PermissionModeChanged` / `PermissionGrantChanged` events (§4), without which
a status line cannot show cwd, mode, or grants.

---

## 12. Open questions

1. **Duplex transport (D1).** The host-callback surface — a remote tool
   emitting progress, and later fork/spawn/ask — depends on whether D1's
   transport is bidirectional. Until it is, remote tools are strictly less
   capable than Rust tools (§3). Needs D1's answer before the `tool/1` payload
   is frozen.
2. **`ToolSpec.schema` type (D3).** Keep `schemars::schema::RootSchema` and
   accept the MCP round-trip risk, or widen to `serde_json::Value` and move
   validation entirely to the compiled validator? Affects every plugin, so it
   is a D3 wire commitment, not a D2 preference.
3. **Deny attribution on the event stream.** `Event::PermissionResolved`
   carries no "who decided." With plugin policies able to deny, an optional
   `by: Option<String>` field (additive, `#[serde(default)]`) is wanted. D3's
   call.
4. **`Provenance::Plugin { id }`.** `context.append/1` needs an attributed
   provenance variant; `Provenance` is `#[non_exhaustive]` but its own doc
   says adding a variant "is a breaking wire-format change and must be treated
   as such" (`provenance.rs:7`). Needs an explicit decision, not a drive-by.
5. **Operator consent for suggested allow rules (D4).** §10 hands the
   install-time acceptance ceremony to D4.
6. **Interaction with board item 01KYKPAW2AFYE284WCC894T87J.** Whichever lands
   first defines `PermissionPolicy`; §6 states the shape D2 needs.

## 13. Findings in the current tree (independent of this design)

- **Pattern grants are inert for every tool but `bash`** — including `read:*`.
  §10, verified against `permission_pattern.rs:47`/`:126` and
  `ports/plugin.rs:83`.

  > **Status (2026-07-30), `68ea9b1`.** Fixed. See §10's status note for
  > what changed and why this one mattered more than a typical stale
  > finding.
- **`Tool` is re-exported from the facade but cannot be implemented through
  it** — its parameter types are not exported. §5.
- **`ConwayBuilder::with_context_hook` accepts a type no external caller can
  name** — `ContextHook` is not re-exported. §5.
- **`PluginManifest.required_host_caps` is inert** — declared, never read. §11.
- **`Runtime::new` panics on a duplicate tool name** — correct for compiled-in
  plugins, a P-10 violation for remote ones. §7.
