# The conway extension architecture

Status: **design, not implemented.** This document is the synthesis of the
five-slice extension spike — D1 through D5 — into one architecture. Where
those five disagreed, this document decides, and the losing option is
recorded as a rejected alternative rather than left standing beside the
winner.

Written against HEAD `68ea9b1` plus two things in flight at the time of
writing: the removal of `Plugin::on_init` (§11.6) and D4 §11's `allow`/`deny`
asymmetry for `permissions.json` (§7.4). Both are treated as landing, not
hypothetical.

## 0. How to read this, and why it exists

The five source specs are still worth reading for their argument; they are
committed at `.design/d1-transport.md` through `d5-template-
instrumentation.md`. This document is the one that is *true* where they
conflict.

**If you are about to re-open a settled question, read §11 first.** That
section is a ledger of every disagreement found between the five slices, the
decision, and the argument that lost. It exists because the most expensive
failure mode for a design spike is not being wrong — it is being re-argued
from scratch by the next person who reads only one of the five documents.
This is the same job the note at `crates/conway-core/src/ports/plugin.rs`
does for `ToolCtx`'s T-8 limitation, at a larger scale.

> **Status (2026-07-30) — a project-level redirect.** The operator redirected
> the extension architecture on four axes: hooks as the primary authoring
> surface, scripts as a user-chosen language layered on the plugin core,
> inference-evaluated hooks running as subagents (fork or spawn), and a wider
> context-manipulation surface for plugins. **The headline finding is that the
> fourth axis is not a relaxation of this document's rules** —
> `ContextHook::before_request` already permits a hook to edit or drop a
> segment, and is already `async` so an inference-driven hook can issue its
> own LLM call. §4's `context.append/1` (append-only, for *remote* plugins)
> had fallen behind that in-process capability, which is the exact
> built-in/third-party inversion GP-03 and P-6 forbid. **Read the new §16
> before re-opening**: `context.append/1` (§4, §5.8, §7.3, §7.6, §12), §5.8's
> scope, §13.6, or §8.3's shell-command-status closure — all five are marked
> superseded in place, and §16 also settles four questions the redirect left
> open (hook fork/spawn declaration, cost and attribution for
> inference-evaluated hooks, determinism across two context-editing plugins,
> and where `ContextMask` gets produced).

### Where this document lives, and why it is not in `ARCHITECTURE.md`

`ARCHITECTURE.md` opens by stating that it describes the system **as
committed**. Nothing in this document is committed. Folding an unimplemented
architecture into the file whose contract is "this is what is here" would
manufacture exactly the doc/code drift this project keeps finding and fixing,
and it would do it in the one file a new reader is told to start from.

So this lives in `.design/`, as a sibling to the five slices it
reconciles. **The correct time to move content into `ARCHITECTURE.md` is
per-phase, as each phase of §12 lands** — a `## Extensions` section in
`ARCHITECTURE.md` describing what actually exists, plus a pointer here for
the reasoning. Adding that pointer now would assert a section that does not
exist yet.

---

## 1. The problem

conway's extension story today is one trait, `Plugin`, holding `Tool`s, and
one way to install it: hand an `Arc<dyn Plugin>` to `ConwayBuilder::
with_plugin` at build time, from Rust, compiled into the binary. Three
distinct wants push past that:

1. **Extensions in languages other than Rust.** Compiled-in plugins mean
   trusting a plugin and trusting the binary are the same act. That is a
   fine model for built-ins and an impossible one for an ecosystem.
2. **Richer rules for tool use than the grant language can express.** The
   grant vocabulary is a prefix over a *rendered string*. "All reads under
   `./src` are fine; writes prompt" cannot be said in it — not because the
   syntax is small, but because a prefix over a rendering is the wrong
   predicate for a question about paths. This is the want that matters most,
   it is the one with the least to do with plugins, and §9.5 shows it is
   answerable without any transport at all.
3. **Instrumentation surfaces** — a status line that can show something
   conway does not compute.

The architecture answers all three with **one registration mechanism, one
composition rule, one trust record, and one wire vocabulary.** The single
most important structural claim is that these are not four systems that
happen to cooperate: every extension point is implemented on top of an
existing port and its decision flows through that port's existing call site.
There is no second mechanism and no path that goes around a port.

---

## 2. The mechanism: subprocess + JSON-RPC 2.0 over stdio

**Decision: an out-of-process plugin is a long-lived child process speaking
newline-delimited JSON-RPC 2.0 on stdio; stdout is protocol, stderr is
diagnostics, and the child must exit on stdin EOF.**

Three grounds, all measured against this codebase rather than against the
general question:

1. **The port surface already fits.** `ToolCall` and `ToolOutput` are
   `Serialize + Deserialize` and round-trip-tested
   (`crates/conway-core/src/ports/plugin.rs`, the `ToolCall`/`ToolOutput`
   round-trip tests). The only non-serializable thing in the tool path is
   `ToolCtx`'s two trait objects — which that type's own doc has named as the
   T-8 limitation since it was written.
2. **The dependency budget is already paid.** `tokio` with `process` +
   `io-util`, `nix` with `signal` + `process`, and `serde_json` are all
   existing workspace dependencies. Zero new external dependencies.
3. **The failure model matches the one that already exists.** The runtime
   already treats tool output as untrusted, sanitizes it, catches panics per
   call, and turns tool failure into model-visible feedback rather than an
   abort (`crates/conway-runtime/src/tools/runner.rs`). A misbehaving child
   slots into that model without inventing a new one.

### Rejected alternatives

| Alternative | Rejected because |
| --- | --- |
| **WASM / Component Model** | Async host callbacks — which conway *requires*, because `ToolCtx` carries capabilities a tool calls back into — stabilized only with WASI 0.3 in Feb 2026, and WASI 1.0 is not final. `wasmtime` is a very large dependency. **Deferred, not rejected**; see §13.1 for the trigger. |
| **C ABI + `libloading`** | No memory-safety boundary at all, so a plugin bug is a host segfault, defeating the per-call `catch_unwind` the runner already does. Rust has no stable ABI. Unconditionally out. |
| **Embedded scripting (Lua/JS/Python)** | Picks a language for third parties, ships an interpreter, and still needs a sandbox story. Strictly more cost than a subprocess for strictly less isolation. |
| **SDK-over-IPC (socket, broker daemon)** | Same protocol problem plus a rendezvous problem — paths, permissions, cleanup on crash — that stdio pipes solve for free by being lifetime-bound to the child. |
| **Host-defined binary framing** | No debuggability, no ecosystem, and every plugin author writes a framer. |
| **LSP-style `Content-Length` headers** | Its one real advantage is knowing a frame's size before buffering it, which is small here because conway's response to an oversized frame is to kill the child anyway. It costs a hand-rolled header parser (a new production dependency), `tail -f \| jq` debuggability, and divergence from the JSONL precedent this project already sets in `conway-cli/src/render/jsonl.rs`. |
| **Lazy spawn on first use** | Impossible without either a static declaration file (a second source of truth that can silently disagree with the running plugin) or making `Plugin::tools()` async and fallible (a breaking change to the most load-bearing core port). Independently: the latency saved is one 5–50 ms spawn against a multi-hundred-millisecond model round trip. |

### The shape of the boundary, in one sentence

`PluginProcess::connect(spec).await -> Result<Arc<dyn Plugin>, PluginError>`,
called by the embedder from the main async runtime, handing its result to the
existing `ConwayBuilder::with_plugin`. **No change to any `conway-core`
port.** A remote tool is an `Arc<dyn Tool>` sitting at the last step of the
existing dispatch sequence, and the entire process boundary lives *inside*
`RemoteTool::invoke`.

That placement is the load-bearing fact behind every security claim in this
document. The dispatch sequence is unchanged:

```
ToolRunner::execute_one
  -> registry resolve
  -> schema validate            (host-local, always; §6.4)
  -> PermissionBroker::decide   (root check FIRST; permission.rs:500)
  -> resolved.tool.invoke(...)  <- the process boundary is in here
```

No frame in this protocol can reach the broker, the gate, or the root check.

---

## 3. The house style: declarative self-assertion

conway has now reached this shape three independent times, and it is worth
naming as the house style rather than rediscovering it a fourth time.

> **A tool or plugin asserts a static property about itself. The harness
> consults the assertion at the decision point. The default is the value that
> produces *more* checking. The assertion may only ever add checks or narrow
> authority, never remove or widen. And a generic, registry-wide test proves
> the assertion is truthful for every implementor.**

The three instances:

| Instance | Assertion | Default | Consulted at |
| --- | --- | --- | --- |
| `Tool::path_args` | which argument names carry filesystem paths | `Unconfinable { checkable: &[] }` | `PermissionBroker::check_root` |
| `Tool::render_kind` (landed in `68ea9b1`) | whether `render`'s output could reach a shell | `ShellCommand` | `PatternRule::matches_render` |
| The wire vocabulary (§6) | which points, versions, and capabilities a plugin implements | most restrictive / unsatisfiable | handshake, `PermissionBroker`, the dispatcher |

**Never a name check.** Not once. The permission layer never asks "is this
tool called `bash`." This is why `render_kind` had to be a *new* declaration
rather than a reuse of `path_args`: `report` declares
`PathArgs::Unconfinable` because its `artifacts[].path` is nested inside an
array and the top-level-only vocabulary cannot express it — a reason with
nothing to do with shell safety. Reusing `path_args` as the shell-safety
signal would have left `report:*` permanently inert for an unrelated reason,
which is the same category error one layer down. `report` now declares
`Unconfinable` and `Structured` simultaneously, which is the case that proves
the two must be orthogonal.

**The defaults are not uniformly-spelled, and that is correct.**
`path_args` defaults to `Unconfinable` and `render_kind` defaults to
`ShellCommand`. Read as enum names those look like opposite directions. Read
as *effects* they are the same direction: both produce more checking for a
tool that declined to declare. **The rule is about the effect, never the
spelling.**

**The fourth clause is the admission criterion.** A declaration without a
generic guard is a convention, not a mechanism.
`render_kind_is_consistent_with_whether_render_is_overridden` sweeps the
whole registry and enforces that a tool may declare `Structured` only if its
`render` output is byte-identical to the trait default; it was verified
load-bearing by injecting a lying override and watching it fail with a
diagnostic naming both the risk and the fix. `path_args` gets the equivalent
in §6.5's schema cross-check. The wire vocabulary gets §6.3's registry test
plus golden fixtures. **Any future instance of this pattern must ship its
guard in the same commit as the declaration.**

---

## 4. The extension-point table

Two structurally different kinds, and the difference is not one of degree:

- **Observers** receive information and return nothing. They cannot change
  behavior, so they may lag, fail, or be absent without changing the run.
  They are fed by the one `EventBus` behind `EventSink`.
- **Participants** return a value the runtime acts on. They are bounded in
  time, fail closed, and compose under a rule that makes registration order
  unobservable.

| Point | Kind | Receives | May return | On error / timeout / garbage / absent |
|---|---|---|---|---|
| `tool/1` | Participant | `ToolCall` + the RPC-shaped `ToolCtx` (§6.1) | `ToolOutput` | Model-visible tool error, never an abort. Absent: the tool is not registered. |
| `tool.spec/1` | Declarative | — | `WireToolSpec` (§6.2) | Malformed: rejected at registration, plugin named. Absent fields take the most restrictive value. |
| `permission.policy/1` | Participant | `PolicyRequest` (§5.3) | **Narrowing:** `Deny \| Abstain`. **Deciding:** `Deny \| Abstain \| Allow` | `on_failure`, default `Deny`. **Never `Allow`.** Absent: contributes nothing; the floor is unaffected because the floor is not policy. |
| `permission.rules/1` | Declarative | — | rule strings and structured rules, effects restricted to `deny`/`prompt` (§5.4) | Unparseable rule: rejected, named, plugin degraded. Never guessed at. |
| `observe/1` | Observer | `Envelope`, filtered by the declared selector | *nothing* — the point has no reply channel, structurally | Ignored. Slow: `Event::Lagged` then dropped delivery. Dead: unsubscribed + `PluginStatusChanged`. The run never waits. |
| `status.declare/1` | Declarative | — | per-key `{ max_len, ttl_ms }` (§8.2) | Malformed: key rejected, named. |
| `status/1` | Observer output | — (pushed) | `StatusContribution { key, value }` | Stale value expires at snapshot time; the UI renders without it. The render path never calls a plugin and never blocks on one. |
| `context.append/1` | Participant (additive) | `ContextHookCtx` | `Vec<{role, blocks}>` to **append**, host-stamped `Provenance::Plugin { id }` | Contribution dropped, turn proceeds unmodified, `PluginStatusChanged`. Dropping an addition removes authority, so this direction is the closed one. |
| `context.tools/1` | Declarative | — | a selector naming tools this plugin hides from announcement | Declarative, so it cannot fail at turn time. A selector matching nothing is a registration error. |

Every row is a `Tool`, an `EventSink` consumer, or a decision fed into
`PermissionBroker` / context assembly — four of the ten existing ports,
reached through their existing call sites.

> **Status (2026-07-30) — `context.append/1` superseded, not modified in
> place.** It gave a remote plugin strictly less than `ContextHook` already
> gives an in-process one (edit/drop, not just append), which is the
> built-in/third-party inversion §4's own "no point a built-in can reach that
> a third-party cannot" claim exists to forbid. Replaced by `context.hook/1`,
> specified in §16.5, which reaches wire parity with `ContextHook` and
> composes multiple hooks' proposals under §16.3's order-independent rule
> rather than leaving open, as this row's original wording did, what happens
> when more than one hook contributes in the same turn.

### Ports deliberately not reachable

- **`SessionStore`** — it is the persistence hot path and the audit trail.
  Behind a pipe, every appended record blocks on IPC, a plugin crash is data
  loss, and the record of what happened becomes extension-controlled, which
  makes every trust claim in §7 unverifiable. `fork` is additionally O(1)
  by design and load-bearing for tournament patterns.
- **`Router`, `HealthRegistry`** — both are *synchronous* `fn`s consulted per
  routing decision, and one mutates breaker state. A synchronous port cannot
  be crossed by an async RPC without blocking a runtime thread. That is a
  structural exclusion, not a preference. A remote health registry fails open
  into "everything is healthy," the worst possible failure for a breaker.
- **`Backend`** — conway already speaks HTTP to backends. A non-Rust backend
  is already solved by running a server that speaks a supported dialect.
- **`SubagentHost`** — implementing it means being the runtime. The *other*
  direction (plugin tools calling it) is §6.1's callback surface.
- **`EventSink` as an implementable port** — `emit` is synchronous and
  non-blocking **by contract**. A remote sink cannot honor that; one that
  could would stall every producer in the runtime. Observers subscribe behind
  the host's sink instead.
- **`PermissionGate`** — see §5.1. A plugin never *is* the gate.

### The tier line, and P-6

"No privileged API" is read **within a tier**, and the tiers are
host-vs-extension, not builtin-vs-third-party.

- **Host ports** are implemented by whoever constructs `ConwayBuilder` — the
  embedder. Choosing the gate is not an API; it is ownership.
- **Extension points** are implemented by plugins. Every plugin — a Rust
  `Arc<dyn Plugin>` or a remote process — reaches exactly the same points
  with exactly the same authority and the same failure semantics.

**There is no point a built-in plugin can reach that a third-party plugin
cannot.** §7.3 is where this is hardest to hold, and it is held by keying
every reduction on the operator's grant rather than on the plugin's
provenance — with the identical mechanism applying to compiled-in code.

---

## 5. Permissions: where extensions attach, and how they compose

### 5.1 The binding point is `PermissionBroker`, not `PermissionGate`

Three candidate resolutions were on the table: a `set_permission_gate`
setter, a channel-based gate like `TuiGate`, or a composite gate dispatching
to registered plugins.

**Decision: none of the three. The gate slot is not touched. Late binding
goes on `PermissionBroker` as a policy chain, mirroring `set_context_hook`.**

The decisive argument reads straight off `decide`
(`crates/conway-runtime/src/permission.rs:479-608`): there are **four ways to
return `Allow` and only one reaches `gate.check`** — the cache
(`permission.rs:543`), a pattern grant (`:560`), `AutoAllow` (`:571`), and
the gate (`:592`). A composite `PermissionGate`, however elegant, **cannot
see** three of them. A guardrail plugin installed as a composite gate would
evaporate in precisely the modes where it matters most.

Secondary: `PermissionGate`'s contract says it may block indefinitely because
a human is on the other end. A policy must be bounded. Two different temporal
contracts should not share a trait.

**Where the chain runs inside `decide`:**

```
1. root check                 NON-DELEGABLE FLOOR (unchanged, still first)
2. plan-mode denial           NON-DELEGABLE FLOOR (unchanged)
3. policy chain - DENY half   runs for every call, including must_reach_gate
                              and including AutoAllow sessions
4. cache hit                  (skipped when must_reach_gate)
5. pattern grant              (skipped when must_reach_gate)
6. AutoAllow                  (skipped when must_reach_gate)
7. policy chain - ALLOW half  only if !must_reach_gate, only from a Deciding
                              policy, only if one allowed and none denied;
                              never cached
8. gate.check                 the human
```

Two invariants a test can pin:

- **A policy can never widen the root.** When `check_root` returns
  `MustReachGate`, the allow half is skipped entirely, so an unconfinable
  call under a root still reaches the *human*. A policy's allow is exactly as
  authoritative as `AutoAllow` and lives in the same slot, never above it.
- **A policy veto survives every allow path.** A `Deny` at step 3 beats the
  cache, pattern grants, and `AutoAllow` alike — the same treatment plan mode
  already gets.

> **Status (2026-07-31), board item 01KYTMH9JX21CGSE2Y6E2KP8SJ.** The first
> invariant above was **true only vacuously** for the agent an operator
> actually talks to. `Runtime::start_root`'s `RootSpec` had no `root` field
> at all until this item, so `AgentRoot::reconstruct` always produced
> `Unconfined` for a session's root agent, `check_root` never returned
> `MustReachGate` for it, and "a policy can never widen the root" held only
> because there was no root to widen — the same emptiness §7.5 disclosed for
> its own containment count. `RootSpec::root` (`--root` /
> `ConwayBuilder::with_root`) makes a root agent confinable, which makes this
> invariant **actually enforced**, not just unfalsified, whenever an operator
> sets one. The default remains `Unconfined` — deliberately not changed by
> this item — so the invariant is still vacuous for every invocation that
> does not opt in. See `docs/permissions.md`'s "Confinement" section for the
> mechanism.

> **Status (2026-07-31), board item 01KYTP1D3XWEZPW4AKPH54FNB3.** Steps 3 and
> 7 above (`policy chain`, `NarrowingPolicy`/`DecidingPolicy`) remain
> aspirational — no such trait exists in the workspace yet. What DOES now
> exist, shipped by this item, is the narrower slice of step 3 this section's
> own §5.4 called "the single most important reconciliation": a
> `prompt`-effect rule, expressed in TODAY's `PatternRule` vocabulary (not
> the future `{select, when, then}` structured form), that actually forces
> `gate.check`. Before this item, `must_reach_gate` was set EXCLUSIVELY by
> `check_root` (step 1) — nothing else in `decide` could raise it, so this
> section's own §2020 worked example (`{"categories":["edit","delete"],
> "then":"prompt"}`) was inert in every mode, including `AutoAllow`, the one
> mode it matters most in. `must_reach_gate` is now the broker-level
> accumulator this document already implied it should be: `PermissionBroker`
> gained a `prompt_patterns` set, structurally identical to the existing
> `deny_patterns` (no `GrantScope`, matched via `PatternRule::matches_deny`
> for the identical anti-evasion reason §5.6 gives for `deny`), checked as a
> new step **between plan-mode denial and the cache** — i.e. exactly where
> step 3's "policy chain - DENY half" sits above, minus the general `Policy`
> trait. A match ORs onto `must_reach_gate` (never clears it), so it forces
> the call past steps 4–6 exactly as `check_root`'s own `MustReachGate`
> already did, and the two invariants above are unaffected: `check_root`'s
> root-forced case cannot be weakened by an OR-only accumulator, and this
> pinned by the existing
> `unconfinable_bash_command_always_reaches_the_gate_for_a_confined_root_agent`
> test. `deny` still beats `prompt` beats `allow` (a `deny_patterns` match
> returns before `prompt_patterns` is ever consulted), and registration
> order among rules within a step remains unobservable. Attribution — which
> rule forced a given ask — is a deliberate non-goal of this slice; see
> `CHANGELOG.md`'s entry for this item for why that gap is safe to leave
> open for now. (`crates/conway-runtime/src/permission.rs`,
> `crates/conway/src/conway.rs`)

**Implementation constraint, stated because getting it wrong deadlocks:**
the chain lives in `RwLock<Vec<RegisteredPolicy>>` and must be **cloned with
the guard dropped before any `.await`**, exactly as `LoopDeps::context_hook`
is read fresh per turn. `decide` holds no guard across an await today; it
must not start.

### 5.2 Narrowing and Deciding policies are different types

**Decision: `PermissionPolicy` splits into two registration kinds whose
verdict types differ.**

```
NarrowingPolicy::evaluate(&PolicyRequest) -> Deny { reason } | Abstain
DecidingPolicy::evaluate(&PolicyRequest)  -> Deny { reason } | Abstain | Allow { reason }
```

A `NarrowingPolicy` has **no `Allow` variant to return.** Not a boolean it
must not set — a variant that does not exist in its return type.

**Rejected alternative (D2 §6's original shape): one verdict enum plus a
`may_allow: bool` defaulting to false.** It is one flag away from wrong, the
flag lives in a config file, and the thing it guards is "may a program
approve my tool calls." §9.1 shows why this matters concretely: an
inference-evaluated policy reads attacker-controlled text, so "this may only
narrow" has to be a property of the type rather than of an operator's
setting. Splitting the type also makes the dangerous registration greppable
at the call site, and turns the trust record's `may_allow` column into a
*derived* fact rather than an independent one.

Rules that apply to both kinds:

- A policy's `Allow` is **per call, never cached, never a grant.** Caching it
  would make the plugin's authority outlive the plugin, keyed by an argument
  digest the policy may not have inspected. Grants belong to the operator.
- No policy may return `AllowAlways` or install a `PatternRule`.
- **A `DecidingPolicy`'s allow half is not consulted for a call to a tool
  provided by the same plugin.** One condition in the chain loop, keyed on
  `plugin_id`, not configurable. The deny and abstain halves *are* still
  consulted from the same plugin — deny is narrowing, and a plugin refusing
  its own dangerous tool under conditions only it understands is useful and
  harmless. The exclusion is exactly as wide as the danger.
- **Policies are not consulted for calls made by an agent a policy spawned.**
  See §9.1; this is the recursion break and it needs a test.
- Each policy declares `timeout_ms`, clamped by an operator-configured
  maximum (default 60 s, generous because an inference-evaluated policy
  issues its own LLM call). Exceeding it yields `on_failure` plus
  `PluginStatusChanged`.

### 5.3 What a policy receives

`PolicyRequest` carries `agent_id`, `agent_path`, `session`, `tool`,
`category`, **raw `arguments`**, `rendered`, **`render_kind`**, `call_id`,
`cwd`, `root: Option<PathBuf>`, `mode`, `must_reach_gate`.

`render_kind` is on this list because `AuthorizedCall` now carries it
(`runner.rs:297`) and a policy reasoning about a rendering needs to know
whether that rendering is shell-shaped. Without it, a policy would have to
infer it from the tool name — the one thing the house style forbids.

The contract says plainly: **`rendered` is sanitized and lossy and must not
be the basis of a security decision.** The v0.5.0 laundering bug is the
worked example, and `check_root` reads `arguments` for exactly this reason.

### 5.4 Rules: one language, two levels of expressiveness

**Decision: the flat string form landing now in `permissions.json` is the
surface syntax for the structured form. Both parse into one internal `Rule`,
evaluated by one evaluator.**

This is the single most important reconciliation in the permission area,
because the alternative — D4 §11's flat `{allow, deny}` strings shipping
now and D2 §10's `{select, when, then}` shipping later — is two rule
languages, two parsers, and two composition stories in one file.

```
Rule { select, when, then }

select ::= tools([pattern...])       // exact, or a single trailing `*`
         | categories([ToolCategory...])
when   ::= paths_under(prefix)       // over the tool's declared path_args,
                                     // resolved exactly as check_root does;
                                     // an Unconfinable tool NEVER satisfies
                                     // it (fail closed, same asymmetry)
         | command_prefix(s)         // today's PatternRule semantics; a
                                     // registration error for a tool whose
                                     // render_kind is Structured (§11.7)
         | category_in([...])
         | always
then   ::= allow | prompt | deny
```

`"bash:cargo test"` in the `allow` array **is** `Rule { select:
tools(["bash"]), when: command_prefix("cargo test"), then: allow }`. The
string form stays the ergonomic default and keeps working forever; the
structured form is the additive superset. `parse_rules` gains a second arm,
not a second home.

**Who may author which effect.** A plugin-contributed rule may only be `deny`
or `prompt`. `allow` rules come from operator-owned config. An allow rule is
a durable grant, grants belong to the operator, and a plugin shipping "allow
all bash" would be a supply-chain escalation with no prompt anywhere. A
plugin may *suggest* allow rules; §7.5 owns the ceremony.

**Why rules and not only a live policy.** A rule set crosses the wire once at
registration and is evaluated in-process: no per-call RPC, no timeout risk,
no fail-open window, and it works when the plugin is dead. The live
`permission.policy/1` point remains the escape hatch for genuinely dynamic
decisions; "reads under `./src`" is not one.

### 5.5 Composition is two stages, not one rule stated twice

D2 said "most-restrictive-wins." D4 said "allow requires trust; deny applies
immediately." **These are not the same rule and they are not in conflict —
they are two stages of one pipeline, and stating them as one rule is how they
would drift.**

**Stage 1 — admission (trust).** A rule or verdict enters the evaluation set
iff *either* it narrows (`deny`, `prompt`, a `NarrowingPolicy`'s verdict)
*or* its author is trusted **and** the operator granted the widening
(`allow`, a `DecidingPolicy`). Untrusted `allow` rules are not overridden
later; they never enter.

**Stage 2 — composition (most-restrictive-wins).** Over the admitted set:
any `deny` beats every `prompt` beats every `allow`; `allow` requires at
least one allow and zero denies; all-abstain falls through to the next step
of §5.1's ordering. **Registration order cannot change the outcome**, which
is why there are no priority numbers — priorities invite an arms race between
config authored by different parties and make the result depend on who edited
last.

The only place order is observable is `context.append/1` segment order, and
there it is **declaration order**, deterministic and stated. `context.tools/1`
hides are a set union. Observers are independent by construction.

> **Status (2026-07-30).** `context.append/1`'s successor, `context.hook/1`
> (§16.5), keeps declaration order as the tie-break for **append** ordering
> among non-conflicting contributions only. It is never a semantic tie-break:
> exclusion composes by set union (order-independent, §16.3), and a
> same-target content-replacement collision between two hooks fails to
> exclusion rather than to whichever hook was declared last.

### 5.6 The deny half's matching rule, and its honest limit

An `allow` rule refuses any command containing shell metacharacters, because
a prefix match on a chained command authorizes the chain. **A `deny` rule
must not inherit that gate** — inverted, it would mean `deny bash:curl` stops
`curl x` but not `curl x; y`, i.e. adding a metacharacter would *defeat* the
rule.

- **Deny compares the prefix without the metacharacter gate.** A
  metacharacter disqualifies an allow and does not disqualify a deny.
- Composition is §5.5 stage 2: any deny beats every allow.

The limit, said plainly rather than papered over: `deny bash:git push` does
not catch `foo; git push`. **Prefix matching is not a containment boundary in
either direction.** What makes the composition sound anyway is the other
half — a command containing metacharacters can never be auto-allowed, so the
chained form always reaches the human. A deny rule is a seatbelt for the
obvious case. Anything that must not happen belongs in the confinement root
or in a capability not granted. Overselling deny-by-prefix would be the same
mistake as MCP's `readOnlyHint`.

### 5.7 `*` means two different things now, and a reader must know

`render_kind` split the grant language's wildcard semantics, and this is
worth stating explicitly because it will otherwise be discovered by
surprise:

- For a **`ShellCommand`** tool, `bash:*` means *any metacharacter-free
  invocation*. `git status && rm -rf /` still reaches the operator.
- For a **`Structured`** tool, `read:*` means *any invocation*, full stop —
  the gate does not apply, because the rendering is a JSON dump no shell will
  ever see.

This is correct in both cases and the asymmetry is a conservatism on `bash`,
not a hole for the others: for `*` there is no prefix to ride past, so
gating `bash:*` is stricter than the operator's own stated intent. It is a
deliberate keep.

### 5.8 Argument rewriting is forbidden

**No participant may mutate `ToolCall::arguments`.** Four independent
reasons, each sufficient:

1. **The grant cache would have no defensible key.** `CacheKey::for_call`
   digests canonicalized arguments. Rewrite *after* a decision and the
   authorized bytes differ from the executed bytes. Rewrite *before* and a
   human's `AllowAlways`, granted against the original arguments, now covers
   rewritten ones. There is no assignment of "which arguments were
   authorized" that survives both cases.
2. **It reintroduces the v0.5.0 bug class by construction.** `check_root`
   reads `arguments` and explicitly not `rendered`, because a safe-looking
   transformation sitting between evidence and check laundered exactly this.
   Argument rewriting *is* such a transformation.
3. **The human approved a different string.** `rendered` is derived from
   arguments and is what the operator saw and what
   `Event::PermissionRequested` recorded.
4. **No fixed point.** Schema validation happens before the proposal event. A
   rewrite requires re-validation, re-render, re-root-check, and with two
   rewriting plugins there is no guarantee of convergence.

The sanctioned alternative already exists: return `Deny { reason }`, surfaced
as model-visible feedback the way `DenyWithFeedback` already is. The model
re-proposes and the new proposal enters from the top with one authority for
its arguments. Cost: one model round trip. Benefit: "which arguments were
authorized" always has exactly one answer.

Generalized: **a participant may veto or monotonically narrow; it may never
rewrite a value another check has already read.** The same rule forbids a
participant that edits the user's prompt text; the sanctioned point for
adding context is `context.append/1`, which is additive and attributable.

> **Status (2026-07-30) — this section's scope narrowed, not its content.**
> Read literally, "no participant may mutate" and the "generalized" paragraph
> above sound like they cover every value a participant sees, including
> context — and `context.append/1`'s "additive... closed direction" framing
> reads the same way. **They do not, and never should have read that way.**
> This section governs exactly two value classes: `ToolCall::arguments` and
> permission verdicts (`PermissionOutcome`/`PolicyVerdict`). `ContextHook::
> before_request` has permitted editing and dropping a segment since it was
> written, and the redirect widens that to remote plugins (§16.4, §16.5). See
> §5.9 for the three-row boundary and the reasoning for each row, and §16.3
> for what changes when more than one plugin edits context in the same turn.

### 5.9 The value-class boundary

> **Status (2026-07-30) — new subsection**, written because §5.8's "never
> rewrite" and D2 §1's R1 ("one authority per value") both read, out of
> context, as if they bound *everything* a participant touches. They do not.
> Three value classes appear across this document and they behave under
> three different rules; conflating them is the exact misreading this
> subsection forecloses. Nothing above is edited by adding this — see the
> status note directly above and D2's own top-of-file banner for the two
> places that misreading is now corrected in place.

| Value class | May a participant... | Why |
|---|---|---|
| **Tool call arguments** (`ToolCall::arguments`) | Never rewritten, by anything, at any point. Veto (`Deny`) only. | §5.8's four reasons stand unchanged: `CacheKey::for_call` digests them, so a rewrite desynchronizes authorized bytes from executed bytes in either direction; `check_root` and schema validation both read `arguments` specifically, before `Event::ToolCallProposed`, so a rewrite would need re-validation, re-render, re-root-check, and with two rewriting plugins there is no guarantee of convergence — no fixed point under two rewriters, and none is needed: `Deny{reason}` plus a model re-proposal gets a caller a different call with one clean authorship. |
| **Permission verdicts** (`PermissionOutcome`, `PolicyVerdict`) | Narrow only (`Deny`/`Abstain`), never widen. Exactly one allow path exists (§5.2 step 7, a `DecidingPolicy` only), below every floor, per call, never cached, always attributed. | A verdict is an authority grant, and an authority grant composed from more than one source needs order-independent composition (§5.5) with a trust act gating any widening (§5.5 stage 1). This is what D2 §1's R1 ("one authority per value") actually describes. |
| **Context** (`ContextPayload`'s segments and tool announcements, and — new — the persisted `ContextMask`, §16.4) | May be edited, dropped, replaced, or masked, by a hook — and always could be, in-process, since `ContextHook::before_request` was written. **Not an authority grant, so R1 does not bind it.** | **The security line was crossed by `context.append/1`'s original *append*, not by edit/drop.** A participant that can already inject arbitrary text into an agent's context can already say anything an editor of that context could say; deleting or replacing a line can only make an agent see *less* of what actually happened, never make it believe something invented. What still binds context, and is a *different* property than R1, is **provenance** (P-2, GP-10): every edit must be attributable and inspectable, and the persisted form must be reversible — which is why `ContextMask` is append-only and un-masking is a second record, never a mutation. Safety here comes from visibility, not from veto-or-narrow. |

**What this table does not relax.** Arguments and verdicts keep every rule
§5.2–§5.5 and §5.8 state; nothing about context's flexibility licenses
touching either. A hook is never handed a `ToolCall` to edit under cover of
"context manipulation" — arguments are not part of `ContextPayload`, and no
port hands a context hook one. The boundary is drawn on **type**, not on
**intent**, which is what keeps it checkable rather than aspirational.

---

## 6. The wire vocabulary and its stability guarantees

> **Every type admitted to the wire is a promise, and the cost of a promise
> is paid by whoever has to keep it — which is conway, forever, on behalf of
> SDKs it will never see.**

The wire is therefore *not* "conway-core, serialized." It is a deliberately
small, separately-versioned vocabulary, most of which is a **projection** of
an internal type rather than a mirror of it. The projection is what let
`SubagentSpec` grow five fields in two releases without breaking anything.

### 6.1 The RPC-shaped `ToolCtx`

| `ToolCtx` field | Wire disposition |
|---|---|
| `agent_id`, `session_id`, `cwd` | **In `tool/invoke` params**, host-supplied. Never read from a plugin-authored field. |
| `config` | **In params**, *only this plugin's* values, the same map delivered at `initialize`. |
| `cancel` | **Not a field.** A `$/cancelRequest` notification plus an SDK-local flag. |
| `chdir` | **Not on the wire at all.** |
| `events` | **Outbound notification `ctx/event`.** |
| `subagents` | **Six inbound request methods `subagent/*`.** |

```jsonc
{
  "ctx_token": "opaque, host-minted, per-invocation",
  "call": { "call_id": "...", "name": "read", "arguments": { } },
  "ctx": {
    "agent_id": "01J...", "session_id": "01J...", "cwd": "/abs/path",
    "config": { },
    "grants": ["subagent.spawn"],
    "limits": { "deadline_ms": 120000, "max_output_bytes": 262144 }
  }
}
```

**`ctx_token` is the whole identity mechanism.** Every host-to-plugin
`tool/invoke` carries one, minted per invocation; every plugin-to-host
request must carry the same token; the host resolves it against a
per-invocation table holding the live `ToolCtx` and **drops the entry when
`invoke` returns**. A callback bearing an unknown, expired, or foreign token
is answered with a typed error and serviced no further.

**The plugin never supplies identity.** `agent_id`, `session_id`, `cwd`, the
confinement root, `call_id`, and a subagent's `parent` are read from the
host's table, never from `params`. `parent` is the case where this bites
hardest: a `Fork` inherits the parent's entire context, so a plugin able to
name an arbitrary parent could fork a *different* agent and read its whole
conversation back through `ask`'s reply text. Cross-tree exfiltration in one
call.

`grants` is **advisory disclosure, not enforcement** (§7.3). It is sent
because a plugin that must guess whether a callback will be refused writes
defensive code, and that is the failure mode this design exists to avoid.

**`events` is a notification, never a request.** No `id`, therefore no reply,
therefore the plugin structurally cannot block on the host — the wire-level
expression of `EventSink::emit`'s non-blocking contract. A plugin may emit
exactly `ToolProgress` and `AgentProgress`; anything else is dropped,
counted, and reported. Three reasons: `Envelope`'s three delivery guarantees
would be breakable; the event stream is the audit trail and a plugin able to
emit `PermissionResolved` could forge a decision that never happened; and
parity holds, because `bash` — the most privileged built-in — emits exactly
`ToolProgress` and nothing else.

**The seq-mutex hazard is avoided by construction, not by care.**
`EventBus::emit` deliberately holds the seq mutex across `tx.send`
(`crates/conway-runtime/src/events.rs:56-79`) because assigning `seq` and
publishing must be one atomic step. That one mutex is an entire agent tree's
serialization point. Four rules keep untrusted input away from it:

1. **No wire-backed type ever implements `EventSink`.** The `observe/1`
   bridge is an `EventBus::subscribe()` broadcast receiver, so it can never
   be invoked under the mutex and it inherits the lossy-with-notice guarantee
   for free.
2. Inbound `ctx/event` is rate-limited (token bucket, 1000/s per connection)
   *before* the bus is touched.
3. The emit happens inline on that connection's reader task, not a spawned
   task — spawning would reorder a call's progress notes.
4. Outbound observer delivery uses `try_send` on a bounded queue and **drops
   with a counter** on full, synthesizing an `Event::Lagged { skipped }` to
   the plugin. Buffer-and-drop, never backpressure, in both directions.

**Two fields are deliberately absent.** There is no `ctx/isCancelled` polling
callback: `bash` polls at 50 ms and the subagent wait loop at 20 ms, which
would be 1 200–3 000 round trips per minute per in-flight call, inside the
critical path, on the pipe a wedged plugin is already failing to drain. And
there is no `chdir` callback: the sanctioned way for a remote plugin to move
the agent is what `cd` does — register a tool with a declared path argument
and let the broker check it. A `chdir` callback would reach `CwdHandle::set`,
whose own doc says it performs no containment check, from a path that never
passes the broker.

### 6.2 Admitted types

Three treatments: **M** mirrored (the Rust serde shape *is* the wire shape,
justified only for types already on a durable serialization path), **P**
projected (a distinct `wire::` struct converted at the boundary), **X**
excluded.

**M:** `ToolCall`, `ToolOutput`, `ContentBlock`, `TruncationPolicy`,
`Artifact`/`ArtifactKind`, `ToolCategory`, `PermissionClass`, `Usage`,
`Envelope`+`Event`, `AgentResult`/`ResultStatus`/`Fact`, `AskOutcome`,
`ToolSelector`, `PluginConfig`, `PermissionMode`.

**P:** `ToolSpec` → `WireToolSpec`; `PathArgs` → `WirePathArgs`;
`SubagentSpec` → `WireSubagentSpec`; `AgentTreeSnapshot` → `WireAgentTree`;
`Budget` → `WireBudget`; `PluginManifest` → `WireManifest`; `ToolError` → a
JSON-RPC error code + data; `PolicyRequest`/`PolicyVerdict`.

**X:** `Message`, `SamplingParams`, `ToolResult`, `ConwayConfig`, `AgentDef`,
`PatternRule` as a *type* (§11.8), `PermissionRequest`, `Provenance`,
`LogRecord`, `SessionMeta`.

Two projections carry the weight:

**`WireToolSpec`** — every default is the most restrictive value:

```jsonc
{
  "name": "grep", "description": "...",
  "schema": { /* raw JSON Schema, §6.4 */ },
  "category":    "search",            // default "execute"     (plan mode denies it)
  "permission":  "requires_approval", // default "dangerous"
  "path_args":   { "kind": "named", "names": ["path"] },
                                      // default unconfinable/[]
  "render_kind": "structured",        // default "shell_command" (§3)
  "truncation":  { "policy": "tail", "max_bytes": 16384 },
  "render":      null
}
```

**`WireSubagentSpec` carries eight fields, and they are exactly the eight the
built-in `conway_subagent` tool accepts** — `mode`, `prompt`, `agent_def`,
`role`, `tools`, `budget`, `result_contract`, `await_result`. Excluded and
host-supplied: `cache_hint` (derived), `keep_alive` (`false`), `ephemeral`
(`false`), `ask_origin` (`None`), `cwd` (`None` → inherit), `root` (`None` →
inherit).

Three arguments in increasing force: it is field-for-field the built-in
tool's own projection, so **P-6 is satisfied exactly rather than
approximately**; every field `SubagentSpec` recently gained is in the
excluded set, which is a measurement of churn rather than a prediction about
it; and `root` in particular must never be plugin-supplied, because
`SubagentHost::start` implements an inheritance algebra where a requested
root wider than or sideways from the parent's fails the spawn outright —
"absent" is already the right answer.

`WireAskSpec` is narrower still (`prompt`, `budget`, `tools`) and carries
**no `mode` field at all**, so `ask`'s fork-only invariant is
unrepresentable on the wire *and* typed at the port for every other caller.

### 6.3 The four stability rules

**Rule 1 — every wire enum is `#[non_exhaustive]` in Rust *and* has a
documented, fail-closed fallback in the SDK contract.** `#[non_exhaustive]`
protects Rust consumers and does nothing for a Python SDK, which will see a
tag it does not know and must decide something.

| Enum | Unknown tag means |
|---|---|
| `ToolCategory` | `execute` — the most restricted, the one plan mode denies |
| `PermissionClass` | `dangerous` |
| `RenderKind` | `shell_command` — the gate stays applied |
| `TruncationPolicy` | the host default policy; **never** `none` |
| `ContentBlock` | drop the block, count it, report it. Never render unknown content |
| `Event` | ignore — the one place "ignore" is right, because an observer changes nothing |
| `ResultStatus` | `failed`. Not `completed` |
| `ToolSelector` | `only([])` — selects nothing. Narrowing, never widening |

**Rule 2 — `#[serde(default)]` on every non-identifying field of every wire
struct**, with a documented default and a test that an old payload still
deserializes. `#[non_exhaustive]` is deliberately *not* applied to the
`wire::` projection structs: it forbids literal construction outside the
crate even with every field named, and the projections are constructed in
exactly one place. The protection it would buy is already bought by the
projection existing.

**Rule 3 — `deny_unknown_fields` ON for hand-authored files, OFF for wire
frames.** For a hand-authored file, a misspelled `comand` silently defaulting
is worse than a loud error. For a wire frame, an unknown field is not a typo
— it is a **newer peer**, and rejecting it turns a forward-compatible
additive change into a hard break. So: ON for `plugins.json`, `trust.json`,
`permissions.json`; OFF for every `initialize`/`tool/*`/`ctx/*`/`subagent/*`
frame. **And the missing half that makes OFF safe rather than sloppy:
unknown fields are ignored but never silent** — counted per (plugin, method,
field) and reported in `conway plugins`, so an operator debugging "my
plugin's new option does nothing" gets `unknown field "retry_budget" on
tool/invoke result (14 times)`.

**Rule 4 — the wire vocabulary is enumerated by a test**, the way `Event`
already pins its variant count precisely so nobody adds one without updating
it. A registry test listing every admitted type with its treatment; **golden
fixtures**, one checked-in JSON file per wire type per protocol minor,
asserted byte-shape-stable; and an "old frame still deserializes" test per
additive change. The golden fixtures are the single highest-value mechanism
here and they cost one test module.

### 6.4 Schema validation is host-local and cannot move

`PluginRegistry::from_plugins` compiles every schema into a `jsonschema`
validator at registration; the runner validates before the proposal event,
which is before the broker. **A remote tool's schema is compiled locally by
the host, and the host's validator is the only one that gates a call.** The
remote side MAY re-validate; the host never trusts, waits for, or is affected
by that. Three independent reasons:

1. `Event::ToolCallProposed` would otherwise carry unvalidated arguments, and
   it is the audit record of what the model proposed.
2. `check_root` reads argument *shape* and treats a wrong shape as hostile —
   a declared path argument present with a non-string, non-null value is
   **denied**, not skipped. That check depends on the validator having run
   first, in this process.
3. The announced schema would become a lie the audit trail records as truth.

**The wire carries raw JSON Schema (`serde_json::Value`), not
`schemars::schema::RootSchema`.** Nothing is lost — `RootSchema`'s serialized
form *is* JSON Schema. The host already round-trips through `Value` anyway
(`registry.rs:90`), so carrying `Value` removes a hop and makes "schema is
not serializable to JSON" unreachable for remote tools. It removes a real
compatibility risk: `RootSchema` is a typed draft-07-flavored model and MCP
servers typically emit 2020-12. And it refuses to pin `schemars 0.8` for
every Rust SDK, which would make conway's own eventual 0.8 → 1.0 migration a
breaking change for every third-party plugin.

**Bounds.** A plugin-supplied schema is untrusted input to a schema compiler,
and `$ref` cycles are a known compile-time DoS shape: `MAX_SCHEMA_BYTES`
(256 KiB), a compile deadline, a cap on tools per plugin. Exceeded: tool
rejected, named, plugin degraded. Never a panic.

### 6.5 `path_args` is cross-checked against the schema

**Every name in `names`/`checkable` must appear as a top-level property of
the tool's declared JSON Schema.** A `path_args` naming a field the schema
does not declare is a *silently never checked* path, because `check_root`
skips absent arguments by design (correctly — `bash`'s `cwd` is optional). So
a typo'd declaration is indistinguishable from an optional argument at check
time and must be caught at registration instead. Registration error, tool
rejected, plugin named. This is the `path_args` instance of §3's fourth
clause, and it is only possible because the schema and the declaration arrive
in the same handshake frame.

`PathArgs::Named` holds `&'static [&'static str]` while a remote tool's names
arrive at runtime. **Resolution: `Box::leak` at connect**, bounded by a cap
on count and name length. `'static` is genuinely accurate — the registry is
built once and is immutable by design, living for the process. Widening
`PathArgs` to `Cow<'static, _>` is a breaking change to a
`#[non_exhaustive]` enum in the most load-bearing core port, to buy nothing
an audited leak of a few dozen short strings does not already buy.

### 6.6 Errors

`ToolError` is **not** serialized as an enum; it becomes a JSON-RPC error
with a stable integer `code` and a `data` object, because JSON-RPC already
has an error channel and every SDK surfaces `code` idiomatically.

| Range | Meaning |
|---|---|
| `-32700..-32600` | JSON-RPC standard |
| `-32000..-32099` | Transport/host: `RequestCancelled`, `CtxExpired`, `CapabilityNotGranted`, `RateLimited`, `Overloaded` |
| `1000..1099` | `ToolError`: `InvalidArguments`, `Timeout`, `Cancelled`, `Io`, `Internal`, `NotFound` |
| `2000..2099` | Registration: `SchemaInvalid`, `NameCollision`, `PathArgsUndeclared`, `ProtocolMismatch`, `CapUnavailable` |

Unknown code maps to `ToolError::Internal`. **No error code maps to a success
or an allow.** That invariant deserves a test that drives a plugin which
simply never answers.

### 6.7 Versioning

**Level 1 — protocol `{ major, minor }`.** `major` covers the frame
vocabulary and envelope semantics; `minor` is additive only. **Level 2 —
per-point versions** (`tool/1`, `permission.policy/1`, …), so one endpoint's
contract can break without moving the protocol major.

Conway's own version appears in the handshake as **informational only**.
Nothing branches on it, which is the answer to "the CHANGELOG moves for TUI
polish no plugin cares about."

| Condition | Outcome |
|---|---|
| `plugin.major != host.major` | **Refuse.** `PluginError::Init`, naming both. |
| `plugin.minor_min > host.minor` | **Refuse.** |
| `plugin.minor_min <= host.minor` | **Accept**, whatever the plugin's own minor. |
| unknown version of a **participant** point | **Refuse to load.** A permission policy that silently never runs is the worst outcome. |
| unknown version of an **observer** point | **Degrade**: load without it, warn, `PluginStatusChanged`. |

### 6.8 Timeouts, and the asymmetry that dissolved

D1 carved out `Deadline::Unbounded` for extension points whose port contract
sanctions indefinite blocking, naming the permission gate. **D2 then decided
no plugin is ever a gate (§5.1), so that carve-out has no host-to-plugin
user.** The tension D1 identified was real for a design in which plugins
could be gates; that design lost, and what remains is:

- **Every host-to-plugin call is `Deadline::Bounded`.** No exceptions.
  `tool/invoke` defaults to **120 s**, deliberately matching `bash`'s
  `DEFAULT_TIMEOUT_MS` — a remote tool is the same order of thing as a shell
  command, and 120 s is the number this codebase already teaches. Explicitly
  not Claude Code's 600 s. `permission.policy` uses the policy's declared
  `timeout_ms`, clamped.
- **`Deadline::Unbounded` survives with exactly one user, in the other
  direction**: the plugin-to-host `subagent/await` callback. That is safe
  *only* because `await_result`'s port contract guarantees termination — the
  supervisor synthesizes a result on budget exhaustion, cancellation, or task
  panic, and `Budget::max_steps` is deliberately non-`Option` for this
  reason. An outstanding `subagent/await` counts as activity on its token's
  enclosing `tool/invoke` inactivity clock, and the absolute `max_total`
  still applies.
- **Connection liveness is always on, for every call kind.** A `$/ping` after
  10 s of idleness whenever anything is pending, answered within 5 s. This is
  the move that distinguishes *alive and deliberately still working* from
  *wedged or dead*. **Health probing is not a decision timeout.**
- **Inactivity is primary; total is a backstop.** A call's deadline resets on
  progress notifications bearing that call's token — the same streaming shape
  `bash` already implements per line. An absolute `max_total` (10 min for
  tool invocation) prevents a plugin holding a call forever by spamming
  progress.

> **Status (2026-07-30) — scoped, not changed, for a call class that did not
> exist to worry about when this was written.** "A call's deadline resets on
> progress notifications" as stated applies to every host-to-plugin call,
> which on inspection includes a **decision-bearing** one
> (`permission.policy/1`, and the redirect's inference-evaluated context
> hooks, §16.2). A hook that emits progress every 2 s while never deciding is
> judged healthy by this rule forever, `on_failure` never fires, and every
> tool call in the session stalls at that hook. §16.2d decides this: **a
> decision-bearing call is excluded from the progress-reset rule entirely**
> — its deadline is `timeout_ms` (clamped), flat, never extended by progress
> on that call's token. `max_total` stays exactly as stated above, scoped to
> tool invocation, because a decision call never needed one: excluding it
> from progress-reset already makes its effective `max_total` equal to
> `timeout_ms`, which is the correct number for a call class with no
> legitimate reason to run long.

**Fail closed, without exception.** A timed-out `tool/invoke` yields
`ToolError::Timeout` — an error, never a success, never an allow. A
decision-bearing call that times out yields `Deny`, the identical shape gate
cancellation already takes. **There is no code path in which a
transport-level failure produces an allow**, and that deserves the test named
in §6.6.

### 6.9 Cancellation across the boundary

`ToolCtx::cancel` is a poll-only `Arc<AtomicBool>` with a parent chain; it
does not cross a process boundary, and **dropping a future does not kill a
subprocess.** That gap is closed in two explicit tiers:

1. **Abandonment.** After `CANCEL_GRACE` (2 s — `bash.rs`'s number) the host
   stops waiting and resolves the call as `ToolError::Cancelled`, marking the
   id abandoned. **Abandonment alone must never be presented as
   cancellation** — the child is still running.
2. **Escalation.** On the first abandoned call the connection is marked
   `Draining`; after `DRAIN_GRACE` (5 s), or immediately on a second
   abandonment, the child is killed via the process-group ladder and
   restarted under backoff.

Stated plainly because it is a deliberate liveness sacrifice: *a plugin that
ignores cancellation is not merely slow; it is a process doing unsupervised,
unattributable work with the host's privileges, and the only mechanism that
actually stops it is a signal to its process group.*

Cancellation composes downward. When an invocation is cancelled or abandoned,
every outstanding callback on that token is answered `CtxExpired`; children
started through the token with `await_result: true` are **cancelled by the
host**; children started with `await_result: false` are **not** — they belong
to the *agent*, not to the tool call, exactly as the built-in behaves.
Without that asymmetry, abandoning a wedged plugin would leak live agents
spending tokens unsupervised.

### 6.10 Process lifecycle

**Spawn is eager, at startup, before `ConwayBuilder::build()`** — forced by
the code, not chosen for latency. `PluginRegistry::from_plugins` is
synchronous and eager, and `Runtime::new` builds it, so a subprocess plugin's
tool list and schemas must exist before the runtime does.

**Reuse `bash.rs`'s process-group discipline; do not reinvent it.** Spawn
with `.process_group(0)`; shutdown ladder is `shutdown` request → 2 s grace →
close stdin (EOF, the language-agnostic second signal) → `kill(-pgid,
SIGTERM)` → grace → `kill(-pgid, SIGKILL)` → `wait()` to reap. That is
`bash.rs`'s `kill_group` verbatim, and **one implementation, not two** — it
should be lifted to `conway_tools::process::kill_group` rather than copied,
because a second subtly-different kill path for a security-relevant routine
is not acceptable.

Against host SIGKILL nothing can run, so the only surviving defense is a
contract: **a plugin MUST exit when its stdin reaches EOF.** Stated as a hard
requirement. (`PR_SET_PDEATHSIG` is Linux-only and this project develops on
macOS; the stdin-EOF contract covers both.)

**Restart is lazy and backoff-gated** — no background respawn loop, which
would churn processes in an idle session for a plugin nobody is calling. The
next invocation after `next_retry_at` triggers a respawn; earlier calls get a
fast typed failure. 250 ms doubling to a 30 s cap, jittered; 5 restarts in
60 s means `Unhealthy`. **On restart, re-handshake and verify the tool set is
identical**; a mismatch is permanently `Unhealthy`, because serving calls
against schemas the registry did not compile is not an option and the
registry is immutable by design. **Unhealthy plugins are skipped, not
unregistered** — the tool stays announced, for the same stability reason, and
`ContextHook::before_request` is the supported way to stop announcing it.

**No automatic retry of a failed or timed-out call.** Tool calls are not
idempotent. Backoff applies to *connections*, never to calls.

**`HealthRegistry` is not reused.** It is keyed by `EndpointId` and speaks
about model endpoints; putting plugin processes in it means either
synthesizing fake `EndpointId`s the router could send *model traffic* to, or
widening a core port for an unrelated subject.

### 6.11 Payload limits

- **`MAX_FRAME_BYTES` = 4 MiB**, both directions, hard. Exceeded: connection
  poisoned, child killed and restarted, pending calls fail closed. Resyncing
  on `\n` after a truncated oversized frame would be exactly the frame
  confusion a length-prefixed protocol exists to avoid.
- **Unparseable in-bounds line**: log, count, drop. A stray `println!` is the
  likeliest real plugin bug and killing the process for it is
  disproportionate. 16 consecutive or 64 total malformed frames poisons the
  connection.
- **Tool output is not capped by the transport.** It is capped by the
  mechanism that already exists — the tool's declared `TruncationPolicy`,
  enforced by the runner, with a `TruncationRecord` in the log. The host
  clamps a remote tool's declared policy against `max_output_bytes` (256 KiB)
  so a plugin cannot declare `None` on a 4 MiB payload. **Clamping only ever
  shrinks.**
- **Batches (top-level JSON arrays) are rejected.** JSON-RPC 2.0 permits
  them; conway does not. A batch of 100k tiny requests is an amplification
  vector for no benefit.

### 6.12 Registration never panics

`PluginRegistry::from_plugins` correctly *returns* an error naming both
plugins and the colliding tool; the panic is entirely in `Runtime::new`'s
`.expect()` (`runtime.rs:302-304`). Defensible for compiled-in plugins — a
duplicate name really is a programming bug. **A direct failure-posture
violation the moment a tool name arrives from a manifest off-process.**

Four steps, smallest possible blast radius:

1. **`PluginProcess::connect` pre-validates** everything visible to one
   plugin alone: schema compiles, name charset, `path_args ⊆ schema
   properties`, no intra-plugin duplicates, size caps.
2. **Cross-plugin collisions are resolved before `RuntimeDeps` is built**, in
   `ConwayBuilder::build`. **Qualified identity, bare name only when
   unambiguous, nobody wins a contested name:** every plugin tool has a
   stable `{plugin_id}__{tool_name}` that always resolves; the announced name
   is the bare one when exactly one plugin claims it; when two claim it,
   **neither** gets the bare name, both are announced qualified, a diagnostic
   names both, and the operator may pin. Deterministic regardless of load
   order (the improvement over first-wins), no silent shadowing, and no
   built-in privilege — a built-in that collides also loses the bare name.
3. **`Runtime::try_new(deps) -> Result<Arc<Runtime>, RuntimeError>`**, with
   `Runtime::new` kept as a thin wrapper. There is exactly **one** production
   call site of `Runtime::new` (`conway/src/builder.rs:407`), so this is a
   one-line production change with zero test churn.
4. **`RuntimeError::PluginRegistration { plugin, tool, detail }`.**
   `RuntimeError` is `#[non_exhaustive]`, so this is additive. `registry.rs`
   currently smuggles registration failures through `ToolError::Internal` and
   documents that it does so because `RuntimeError` has no registration
   variant.

**Invariant to test: no plugin-supplied manifest content can panic the
host.** Duplicate names, a 100 MB schema, a `$ref` cycle, a `path_args`
naming a nonexistent field, a non-semver version, a 10 000-tool manifest —
each a typed refusal.

### 6.13 Discovery and configuration

Plugins are declared in **one filename, in two scopes**, mirroring
`permission_file_paths` exactly: project `<nearest ancestor .conway>/
plugins.json`, then global `<XDG or ~/.conway>/plugins.json`. Project entries
override global by plugin id. A missing file at either level is not an error.
Plus `ConwayBuilder::with_plugin`, unchanged.

**Rejected: a `[plugins]` table in `settings.json`.** `settings.json` merges
five sources including environment variables, and an env-var-injectable
*executable path* is a privilege-escalation surface. `permissions.json`
already sets the separate-file precedent.

**Rejected: Claude Code's scattering** — hooks across user, project, local,
and enterprise settings plus per-project MCP files, with no runtime way to
list what is active. Two scopes, one filename, one precedence rule, and a
mandatory inventory command.

**The loader reports; it does not decide.** It returns `{ id, spec, origin,
scope }` per entry and makes no execution decision. §7 does that.

**"What is loaded right now, and from where" is required, not optional.**
`Conway::plugins() -> Vec<PluginStatus>`, a `conway plugins` subcommand
alongside `Sessions` and `Routes`, and a TUI `/plugins` panel — on the same
principle `/settings` already applies to grants: *an operator must be able to
see what they have granted; a rule set nobody can inspect is a trap.*

---

## 7. The trust model

One sentence: **trust is granted to a (kind, id, content-digest) subject,
never to a directory; a deny is always in force and an allow always requires
trust; a change to trusted content de-trusts silently rather than prompting;
and the capability vocabulary names only what the host actually mediates.**

### 7.1 Threat model, and what is achievable today

The attacker authors a repository the operator clones. They control every
byte under the checkout, which includes `.conway/plugins.json`,
`.conway/permissions.json`, `.conway/profiles.toml`, and
`.conway/settings.json`. They also control the source the model reads, so
indirect prompt injection is a *second* channel that can drive the model
toward calling whatever the first channel authorized.

They do **not** control the operator's global `~/.conway`, the conway binary,
the operator's keystrokes, or any digest recorded before the hostile content
existed.

**What they can achieve today with no plugins involved:** `PermissionFile`
has exactly one field, `allow`. The entire project-scoped permissions file is
a grant file, it is loaded at TUI startup, and every rule is installed at
`PermissionScope::Session`, which `GrantScope::covers` answers `true` for any
requester in the tree (`crates/conway-cli/src/tui/app.rs:200-216`). A cloned
repo ships pattern grants into the operator's live session before the
operator has typed anything. Bounded by the metacharacter gate to a
metacharacter-free `bash:` prefix — which still leaves `{"allow":
["bash:npm run build"]}` in a repo that also controls `package.json`. **That
is arbitrary code execution with no prompt, from a clone, on current main**,
and §7.4 is the shippable answer that depends on no plugin work at all.

> **Status (2026-07-30), `d917ba2`.** No longer true — this also **contradicts
> this same document's own §12 F10/F11 rows**, marked `✅ DONE` against the
> same commit, and its own HEAD banner. Project `allow` rules now require a
> recorded trust decision before install (F10), and the bound this paragraph
> used to size the "no plugins involved" attack — "metacharacter-free `bash:`
> prefix" — was itself wrong by the time it was written: `68ea9b1`, earlier
> the same day, made pattern grants work for every tool via `RenderKind`, not
> just `bash`. Left in place as the record of what the threat looked like
> before either fix; see D4 §1's parallel note, which states this more fully
> and flags it as an understatement in the permissive direction.

**What plugins add** is an inversion: until now, trusting a plugin and
trusting the binary were the same act. A plugin declared in a file makes
*cloning a repo* the act that decides what code runs.

### 7.2 What trust is, and what it is not

**Trust is not a sandbox boundary, and conway is not acquiring one.**
Isolation belongs in tools and in the deployment, not in the harness; a real
sandbox (seccomp, landlock, a container runtime) is a large new dependency
plus a platform matrix conway does not have. This design adds **zero new
dependencies** — `blake3` is already a workspace dependency, already used by
the broker's `CacheKey`.

> A trusted plugin runs as a subprocess with the operator's full privileges:
> their filesystem, their network, their credentials, their ability to exec.
> Conway cannot and does not constrain that. **The decision to trust is the
> entire control at the process level, and it is binary.**

Everything §7.3 calls a capability governs what a plugin can make *conway*
do — not what it can do to the machine.

**The trust subject is `(kind, id, digest)`, not a directory.** There is no
"trust this folder" act; a project appears as a *key* selecting which
subjects apply, never as a grantee. Two kinds in v1: `plugin` (digesting both
the declaring `plugins.json` entry **and** the artifact it names, kept
separately because they change for different reasons) and `permission_file`
(digesting the bytes). `profiles.toml` is **excluded deliberately, not by
oversight**: a hostile profile can malform requests but `Profile` carries no
`base_url`, so it cannot redirect credentials to an attacker's host. Adding
it later is one line in the `kind` enum.

**Trust lives in one global-only file, `<XDG or ~/.conway>/trust.json`**,
deliberately breaking the project-then-global precedent. That precedent
exists for *content the project legitimately authors*; a trust record is not
content, it is a **decision about** content, and a decision whose subject can
author it is not a decision. No env-var override, for the same reason
`plugins.json` has none.

**Scope decides whether it loads; the record decides what it can do.** A
global-scope entry is authored by the operator and is trusted by authorship —
asking an operator to trust their own file is theater that teaches people to
click through. A project-scoped entry requires a record. **Capabilities are
explicit in both cases**: "I put this in my config" is not "I want it
spawning agents."

**A change to trusted content de-trusts. It never prompts.**

```
digest(subject) != recorded digest
   => Untrusted{ DigestChanged }
   => the plugin is NOT loaded / the allow rules are NOT installed
   => one notice line in the transcript, one row in /plugins
   => the session starts, degraded, and keeps working
```

The obvious alternative — re-prompt on change — has a worse failure mode than
the flaw it fixes: **a prompt that fires on every `git pull` trains the
operator to press `y`.** Any design whose safety depends on a human reading
the twentieth identical modal of the week has already failed. **There is no
modal on this path, at startup or ever.** The only prompt-shaped surface is
one the operator opens on purpose, and it shows a *diff against the trusted
digest* rather than a yes/no.

The cost, stated: a plugin you depend on can stop being available after a
pull. Three things bound it — the notice is not silent; `required: true`
converts "runs without it" into "refuses to start" for a plugin whose absence
is unacceptable, so the operator picks which failure they get; and it is a
de-trust, not a deletion, so caps and classifications survive and re-trusting
is one confirmation rather than a reconfiguration. **conway spends
availability to buy the property that authority never silently follows
content across a change.**

### 7.3 Capabilities name only what the host mediates

| cap | admits | enforced by |
|---|---|---|
| `tool` | registering tools | registration |
| `observe` | `observe/1` subscription | the subscription is the host's to make |
| `status` | `status.declare/1` + `status/1` | the registry is host-side |
| `context.append` | appended segments | the assembly is host-side |
| `context.tools` | announcement hiding | the announcement is host-side |
| `permission.rules` | contributing `deny`/`prompt` rules | the rule set is host-evaluated |
| `permission.policy` | joining the policy chain as a `NarrowingPolicy` | the chain is inside `PermissionBroker` |
| `permission.policy.allow` | joining as a `DecidingPolicy` (§5.2) | the chain is inside `PermissionBroker` |
| `subagent.spawn` | `start`/`ask`/`steer`/`await`/`cancel` | the guarded handle (below) |
| `subagent.tree` | the subtree snapshot | the guarded handle |

> **Status (2026-07-30) — new capability, `hook.fork`.** The redirect's
> inference-evaluated hooks run as subagents and must declare `Fork` or
> `Spawn` (§16.1). `hook.fork` follows `subagent.spawn`'s exact shape here:
> default off, never implied by trust, requested in the manifest and
> separately granted. `context.append` above is superseded (§16.5) by
> `context.hook/1`; its capability name and row are retired with it, not
> repurposed.

**Deliberately absent: `fs.read`, `fs.write`, `net`, `exec`.** A plugin is a
separate OS process with the operator's privileges. Conway has no mechanism
by which `"net": "none"` could be made true. Putting those words in a
capability list would be *worse* than omitting them, because an operator
reading `net: none` in a review surface would reasonably conclude the plugin
cannot reach the network — **a false belief manufactured by the very surface
meant to inform them. A declared-but-unenforced capability is documentation,
not a control, and this design refuses to let documentation sit in the
control's slot.**

Self-reported intent is still useful for review, so the manifest carries a
segregated `disclosures` map (free text: `"network": "calls
api.example.com to classify commands"`) rendered under a header that says
verbatim *"Self-reported by the plugin. Conway does not verify or enforce
these."*

**Request vs grant.** `required_host_caps` (existing field, currently inert)
means caps without which the plugin refuses to run — any one not granted and
the plugin fails to load, naming the cap. `optional_host_caps` (new) degrade.
The **granted** set lives in the trust record and nowhere else. **A manifest
can only request.** The effective set is the intersection; a cap granted but
not requested does nothing — a grant is a ceiling, never a floor. `/plugins`
shows the delta, which is the most interesting column in the whole surface,
because *"what does this want that I did not give it"* is the question a
reviewer actually has.

#### The guarded handle: how the reduction is keyed on the grant, not the plugin

The tempting implementation is: remote plugins get a reduced `ToolCtx`
without `subagents`; built-ins get the full one. **That is a privileged API
by definition** — a field present for compiled-in code and absent for
third-party code, keyed on which it is — and both are on the same side of the
tier line. So the key changes:

> **The reduction is keyed on the operator's grant, and the identical
> mechanism applies to a compiled-in `Arc<dyn Plugin>`.**

`ToolCtx.subagents` stops being a bare `Arc<dyn SubagentHost>` and becomes a
per-plugin guarded handle. Every method checks the calling plugin's granted
caps and returns `RuntimeError::CapabilityDenied { plugin, cap }` when
`subagent.spawn` is absent.

> **Status (2026-08-04), board item 01KZ59SXNQ3BRXP49V4JW10N72 (C1).**
> `ToolCtx.subagents` is now a `SubagentHandle` rather than a bare
> `Arc<dyn SubagentHost>` — but keyed on the calling AGENT's identity, not on
> a plugin's granted capabilities. No grant is consulted and
> `CapabilityDenied` does not exist. The capability mechanism described here
> is still unbuilt and would now extend an existing handle rather than
> introduce one. See `d4-trust-model.md` §7's status note.

**The mechanism costs no new `ToolCtx` field and involves no name check.**
`RegisteredTool` already carries `plugin_id`
(`crates/conway-runtime/src/tools/registry.rs:23`, currently used only for
the duplicate diagnostic), and `ToolCtx` is constructed at
`runner.rs:353` with `resolved` in scope. The guarded handle is constructed
per plugin at registry build and handed to that plugin's tools. The registry
supplies the identity; the tool never asserts it.

The built-in subagent tool ships in a plugin whose manifest requests
`subagent.spawn` and which is **seeded** with that grant. That seed is the
one place the word "built-in" appears, and it appears as **a default value in
an inspectable, revocable config table, not as a branch in code.** The
distinction is thin, and here is why it is the right thin line: revoke
`subagent.spawn` from `builtin.subagent` and `/fork` stops working, through
exactly the same code path that stops a third-party plugin. A grant seeded in
config is visible and revocable; a branch in code is neither. **The substance
of "no code path privileges a built-in" survives intact.**

Cost, stated: built-ins hold `subagents` unconditionally today, so this is a
behavior change that depends on the seed existing. If the seed were dropped,
`conway_subagent` would break loudly with a typed error naming the cap —
which is the correct failure and is worth a test that pins it.

**The wire schema is uniform for every plugin; the dispatcher enforces.** The
callback method exists on the wire regardless of grant, and calling it
without the cap returns a typed `capability_denied`. Two structural reasons:
a schema that differs by grant makes a plugin's code path depend on a fact it
cannot discover, and the failure is "method not found," indistinguishable
from a version mismatch; and if the schema is the enforcement, the
enforcement lives in schema generation, a place with no natural test and no
single call site. One checked dispatcher is one place to test and one place
to audit.

**`ctx.grants` is therefore advisory disclosure, not enforcement.** The
guarded handle is the only enforcement point, and it is hit identically by
the in-process call and by the RPC bridge.

### 7.4 The shippable half: project-scoped `permissions.json`

Everything above applies to a mechanism that does not exist yet. §7.1
established the same threat is live on main today. **This half depends on no
plugin work at all**, in order of value:

> **Status (2026-07-30), `d917ba2`.** Items 1, 2, and 4 below shipped this
> commit — see §12's status block for detail and the two residuals it left
> behind. **Item 3 (scope narrowing) is the one still open**, tracked as F13.

1. **A project-scoped `permissions.json` is a trust subject.** Its `allow`
   list installs only if the digest matches a record; otherwise it is skipped
   with one notice. The global file is unaffected.
2. **Add the `deny` half** (§5.6), applying immediately and regardless of
   trust, from either scope. `deny` is additive and `#[serde(default)]`, so
   every existing file keeps parsing unchanged.
3. **Stop installing project rules at `Session` scope by default.** A project
   file's grants should default to the narrowest scope that makes them
   useful; `Session` should be the operator's explicit choice, not the
   loader's default. Recorded with its compatibility cost: it narrows
   behavior for anyone relying on the current default, and the narrowing
   direction is the safe one.
4. **Origin tracking on `active_patterns()`**, without which none of the
   above is inspectable.

The current loader's silence is correct where it is silent and wrong where it
is not: every failure today is silent and narrowing, which is right for a
corrupt file. **Skipping an untrusted file is the same kind of narrowing and
belongs on the same path — but it deserves a notice, because unlike a corrupt
file it is a state the operator can resolve.**

### 7.5 Circularity, escalation, and the honest residue

**(a) Direct self-authorization — prevented.** §5.2's same-plugin allow
exclusion.

**(b) Mutual authorization — detected, not prevented.** Plugin A allows B's
tools; B allows A's. No `plugin_id` check catches this, and no structural
mechanism can — mutual back-scratching is indistinguishable from two
independent policies that happen to agree. Bounded by: two explicit operator
grants of `permission.policy.allow`; **attribution on the event stream**
(`Event::PermissionResolved { by: Option<String> }`, additive, and this
design upgrades it from "wanted" to **required** — an allow nobody can
attribute is an audit trail with a hole exactly where the interesting event
is); and never being cached, so it is re-decided and re-attributed per call.

**(c) Self-induced authorization — the honest residue.** Plugin A's tool
spawns a subagent; the child calls a built-in tool; A's policy allows it. Not
self-authorization by the letter, but it is by the spirit. Bounded only by
`permission.policy.allow` + attribution + the root floor + `subagent.spawn`
being a *separate* grant. This is the strongest argument for keeping those
two grants independent: holding both is what makes this reachable, and
requiring two deliberate operator acts is proportionate.

**Escalation is narrow-only, with exactly one operator-enabled exception**,
and it is worth being precise that it *is* a widening: a `DecidingPolicy`'s
allow at step 7 short-circuits a human prompt that would otherwise have
happened. Four independent containments, and the count is the justification:
two operator acts; it sits below both floors and is skipped outright when
`must_reach_gate`; it is per call, never cached, never a grant; it is
attributed.

> **Status (2026-07-31), board item 01KYTMH9JX21CGSE2Y6E2KP8SJ.** Two of
> those four ("it sits below both floors" and "skipped outright when
> `must_reach_gate`") were, for the agent an operator actually talks to,
> **vacuously true, not enforced**: `RootSpec` (`Runtime::start_root`'s
> parameter) had no `root` field, so a root agent's own `AgentRoot` was
> always `Unconfined`, the root floor never applied to it, and
> `must_reach_gate` could never fire for it either — those two containments
> held only because there was nothing for a `DecidingPolicy`'s allow to
> escape past — a different failure shape from a check that runs but answers
> wrong: this was a check that never reached the agent in question at all.
> `RootSpec::root` (`--root` / `ConwayBuilder::with_root`)
> makes both containments real whenever an operator configures a root — the
> default remains `Unconfined`, so they stay vacuous for every invocation
> that does not opt in. The count in this section's own justification ("four
> independent containments") is therefore now conditionally true rather than
> unconditionally true; read "it sits below both floors" as "it sits below
> both floors, when a floor exists to sit below."

**Self-declared classification may only tighten.** A remote plugin's declared
`category`/`permission` is a claim about itself. Honoring a claim of `Read`
on a tool that executes is a direct plan-mode bypass, and plan mode is the
mode an operator selects when they want a guarantee. So: undeclared or
newly-seen defaults to `Execute`/`Dangerous`; a declaration may make a tool
**more** gated, never less; **only the operator may lower a
classification**, via `tool_class` in the trust record. Cost, stated: an MCP
shim's read-only tools are plan-mode-denied until the operator classifies
them. Mitigated by placing `tool_class` in the trust record so the
classification happens **in the same review, in the same act** as trusting
the plugin, with the plugin's declarations shown as a pre-filled suggestion.

**Suggested allow rules — the ceremony.** `/plugins rules <id>` shows the
plugin's suggested rules **verbatim, in the same surface syntax the
operator's own file uses** (which is possible precisely because §5.4 made the
string form the surface syntax of the structured form). Accepting transcribes
them into the operator's own `permissions.json` with a comment naming the
origin plugin and its digest. **From that moment they are
operator-authored**: they survive the plugin being revoked, they appear in
`active_patterns()`, and they are visible in a `git diff`. The trade-off must
be stated rather than hidden — **the rules outlive the plugin** — which is
why the origin column in §7.4 item 4 is not optional.

**Never widenable, under any grant:** the confinement root, plan mode's
denial, argument rewriting, and `AllowAlways`/pattern-grant installation.

### 7.6 Plugin strings are untrusted *inbound*, not only outbound

**Finding:** `PermissionOutcome::Deny { rendered_error }` flows to
`ToolOutcome::error` (`runner.rs:307-309`), which wraps it in
`ContentBlock::Text` with `is_error: true`. That block enters the model's
context. `sanitize_rendered` is applied only to `rendered` (`runner.rs:396`),
**not to this path.** So a denial reason is model-visible text that has
passed through no filter at all. With plugin policies returning `Deny {
reason }`, that becomes a prompt-injection channel from an extension straight
into the model's context — a uniquely well-positioned one, because the model
reads a denial while deciding what to do next and is primed to treat it as
instruction-shaped.

Rules for every plugin-supplied string that can reach the model:

1. **Attribute and delimit; never concatenate.** A policy reason renders as
   `permission denied by policy "<plugin_id>":` followed by the reason in a
   delimited block. **The plugin cannot forge the harness's own voice.**
   Content filtering is unwinnable; *provenance* is cheap and makes "conway
   says you should now run X" unavailable to a plugin.
2. **Cap the length** (1 KiB) with an explicit elision. A denial reason is
   not a payload channel.
3. **Apply control-character sanitization to this path too** — a present-tree
   gap independent of plugins, since an operator-authored gate's message
   already reaches the transcript unsanitized. Fix at `ToolOutcome::error`'s
   construction rather than at each producer.
4. **`context.append/1` provenance must be rendered, not merely recorded.** A
   segment attributed only in the log is attributed to the auditor, not to
   the reader who is about to act on it.

   > **Status (2026-07-30).** `context.append/1` is superseded by
   > `context.hook/1` (§16.5); this rule carries over unchanged to the
   > successor and now applies additionally to a durable `ContextMask`'s
   > reason string (§16.4) — a mask a reader cannot see the reason for is the
   > same attribution failure one step removed.
5. **`StatusContribution` never reaches the model.** Screen only. Stated so
   it cannot drift into the context assembly later.

### 7.7 Failure posture

**A plugin that crashes, hangs, returns garbage, is untrusted, or is absent
must degrade, never authorize.**

| failure | result |
|---|---|
| process crashes | policies contribute `on_failure` (default `Deny`); its tools become `unknown tool` errors; never an allow |
| hangs | the point's timeout fires — the deadline is the host's, not the plugin's — then `on_failure` |
| returns garbage | frame dropped and counted; the call hits its deadline; `on_failure` |
| untrusted / de-trusted / never loaded | contributes nothing |
| `trust.json` missing | every project-scoped subject is untrusted |
| `trust.json` corrupt | **treated as empty, with a loud diagnostic** — never partially applied |
| artifact digest uncomputable | untrusted |
| granted-cap lookup fails for any reason | cap absent |

**The structural guarantee behind the whole table:** a plugin allow exists
only as a *return value* at step 7 of `decide`. Absence produces no return
value, so control falls through to `gate.check` — the human. There is no
default-allow branch keyed on "no policies registered," and there must never
be one. **A test should pin exactly that: with zero policies registered,
`decide`'s behavior is byte-identical to today's.**

**Undecidable is untrusted.** `Containment::Undecidable` is fused with
`Outside` everywhere in this codebase because "can't check" is never "allow."
The trust layer adopts the identical rule verbatim.

**The second reason never to put a guarantee in a plugin.** The first is that
a guarantee implemented as plugin policy fails open when the plugin is
absent. Trust adds an independent one: a plugin's availability now depends on
a **file digest**, so any guarantee living in a plugin can be revoked by an
attacker touching a byte in the repo. **Guarantees stay in the harness.**

---

## 8. The UI templating surface

### 8.1 What is traded, and what is preserved

Today, config supplies an enum selector from a closed set and no config value
ever reaches the screen. That is two properties wearing one coat:

- **P-A — the host owns every value.** A `{ctx_pct}` reference resolves to a
  number the host computed; config cannot supply the number. **Preserved
  exactly.** There is no construct that substitutes a config-supplied value.
- **P-B — no config-authored *text* reaches the screen.** **Deliberately
  broken.** Literal text between placeholders renders. This is the feature,
  and it is the entire cost.

The price of breaking P-B, paid explicitly: a sanitizer at the literal-text
boundary; a hard floor in the width priority order; an unmodified
forced-`mode` invariant; and a user-scope-only rule for the template key.

### 8.2 The language — two constructs, two escapes, nothing else

```
{name}        variable reference; name is [a-z0-9_.]+
[ ... ]       group: renders only if EVERY variable inside resolved
              non-empty; otherwise the group and its literals vanish
{{  }}        literal brace
[[  ]]        literal bracket
anything else literal text
```

**No conditionals, no operators, no functions, no width specs, no nesting, no
styling markup.** Groups do not nest. A group with zero variable references
is a syntax error.

**Why the group earns its place when nothing else does.** Today's assembly
gets separator elision for free — `model` is omitted before the first
`ModelDecision` and no dangling ` | ` appears. A flat one-construct template
renders `" · ctx 0%"` with an orphan separator in exactly that case. **The
pressure to fix that is precisely the pressure that grows a template language
into a programming language**: the next request after "silently wrong output"
is `{model?}`, then `{if model}`, then expressions. One non-nesting bracket
pair absorbs that pressure permanently and cannot be built on. It is the
cheapest possible fixed point.

The acceptance test for expressiveness is reproducing today's `tokens` field
exactly:

```
{tokens_total} tok[ ({cache_pct}% cached)]
```

**The template does not replace the field list.** It defines new *named*
fields the existing closed-set `fields` list selects, each declaring a
*ladder* of progressively shorter rungs:

```json
"tui": { "status_line": {
  "fields": ["session","mode","model","ctx","cache","activity","hint"],
  "custom": { "cache": {
    "template": ["{tokens_total} tok[ ({cache_pct}% cached)]", "{cache_pct}%c"],
    "style": "status_dim", "priority": "telemetry" } } } }
```

**Rejected: replacing `fields` with one whole-line template string.** It
destroys four working properties at once, each bought with an adversarial
review finding: **the ladder** (a single flat string has no rungs, so a
narrow terminal falls straight through to the explicit-truncation *last
resort* used as the normal case); **`mode`'s survival guarantee** (it is last
in `drop_priority` and its ladder never ends empty, so `AUTO-ALLOW` is never
removed); **the forced-`mode` rule** (it operates on a list of *names*, so
keeping `fields` a list of names keeps it working unmodified from both the
file and env paths); and **per-field omission semantics**.

**Styling is not in the template.** A custom field declaration may name **one
theme slot** for its entire text, from the closed set of `Theme`'s own field
names, falling back to the base style on an unknown name. This keeps every
`Color` literal inside `theme.rs`, which is what the existing grep guard
enforces — a template naming colors directly would need a color parser
outside `theme.rs`, a violation in spirit even with a different needle
string. Want two colors? Define two custom fields. That is the deliberate cap
and it is the difference between *arranging host values* and *authoring a
rendering language*.

**Templates are user-scope only.** Template keys are accepted from the
XDG/home `settings.json` and ignored **with a diagnostic** in a project-
scoped `.conway/settings.json`. Project `settings.json` outranks the user's
own file today, which is harmless while `[tui.status_line]` can only reorder
a closed set of names — and *not* harmless the moment it can carry text. **A
repo you clone should not be able to write sentences into your status bar.**
The `fields` list itself stays project-settable: still a closed-set selector,
and the forced-`mode` rule neutralizes the only abuse.

### 8.3 The variable namespace, and `status/1`

Two tiers: **session variables** (meaningful for any consumer of a session)
live on the facade as a `conway::instrument::Vars` snapshot, so library
embedders and the one-shot CLI reach the same values the TUI does; **view
variables** (`spinner`, `elapsed_s`, `mode`) are TUI-owned and overlay it.
Putting `spinner_frame` on the facade to satisfy the parity principle
literally would be worse than stating the boundary.

Most of the catalogue exists today with zero plumbing. **`cache_pct` is the
one to lead with** — the line already renders `tokens (n% cached)`, the
arithmetic already exists, cache economics are central to conway's O(1)-fork
design, and no competitor surfaces it.

Deliberately out, each because a variable naming a feature that does not
exist documents a feature that does not exist: **cost in USD** (nothing in
the workspace prices a token; it needs a price table, a currency, a staleness
policy, and an answer to "priced when" — its own board item); **lines
added/removed** (no diff ledger); **rate-limit quota** (per-request error
classification is not a quota model); **vim mode**; **output style**. And
**PR number / repo host / worktree** are the *canonical* `status/1` plugin —
saying so is more useful than half-implementing them in the host.

**A shell-command status variable is rejected permanently, not deferred.**
The v0.3.0 record deferred it; this closes it, because the two things it
bought are now served separately and better — **arrangement** by the
template, **arbitrary data** by `status/1`. A `status/1` plugin whose
implementation happens to be a script delivers the identical capability while
being registered, named, versioned, TTL-bounded, trust-gated, and supervised
with restart counts and a stderr ring buffer. A config string that execs has
none of that. Re-adding it would be strictly redundant *and* strictly less
safe.

> **Status (2026-07-30) — the *closure*'s reasoning is superseded; the
> permanent "no" above is not reopened.** The v0.3.0 record's original
> objection to a shell-command status variable was that arbitrary command
> exec from config is too large a trust surface for a bare config string —
> and that objection is now directly answered, for the general case, by
> `d917ba2`'s trust model: a project file's authority requires a recorded,
> digest-keyed trust decision before it runs anything. The redirect's
> "scripts as a platform" axis reconciles with this cleanly rather than
> reopening it: **the script runner is itself a plugin** — it implements
> `status.declare/1`/`status/1` (or `context.hook/1`, or `tool/1`) like any
> other out-of-process plugin, and its own implementation happens to dispatch
> to a configured script per event. The script surface therefore layers ON
> TOP of the Rust port (GP-03's own sanctioned shape for lower-barrier
> surfaces), not beside it, and there is still exactly one extension
> mechanism and one trust ceremony. What stays permanently closed is a raw
> shell-command *string* accepted straight out of `settings.json` with no
> registration, no digest, and no supervision — that is the thing this
> section actually forbids, and the redirect gives no reason to reopen it.

**`status/1` gains a registration half, `status.declare/1`** — a plugin
declares its keys with `max_len` and default `ttl_ms` at handshake. D2 gave
`status/1` no declaration and D5 assumed one (it needs `max_len` at
registration); §11.4 resolves that in D5's favor, and the declaration pays
for itself twice: the `status` capability is granted at trust time and a
granted capability implies a declaration to grant, and it is what lets
`{plugin.*}` references be validated at all (§8.4).

### 8.4 The four obligations

**Width.** Two layers on top of the existing accounting, which already sums
real display columns via `Span::width()`. **Ingest cap:** the registry
truncates on store to the declared `max_len`, default 24, hard cap 64 —
**in `char`s, not columns**, a deliberate stated compromise, because
`conway-runtime` has no `unicode-width` and adding one for this is not
justified; char-capping bounds memory and pathological input, and
column-accurate accounting happens at render where ratatui is present.
**Render floor:** a custom field's `drop_priority` defaults to `ambient` — it
gives up space *before* `cwd` — and a declaration may raise it to `telemetry`
and **no higher**. `orientation`, `activity`, `hint`, and `mode` are
unreachable from config. **The invariant a test must pin: no configuration of
custom fields can cause `mode` to lose a rung earlier than it does today, or
`hint` to be dropped earlier than it does today.**

**Sanitization.** Applied at three boundaries: `status/1` ingest, before
storage; **template literal text at parse time** (new, and the direct cost of
breaking P-B); and host variables that are not host-authored — `focused_model`
arrives off the wire in `Event::ModelDecision`, `cwd_display` from CLI args,
`lineage` embeds `agent_def` names.

**The v0.5.0 laundering lesson, restated for this context, because a future
reader will otherwise "helpfully" get it backwards.** That bug was sanitizing
*before a security check*, so the sanitizer laundered the evidence.
Sanitizing at ingest is safe **here precisely because nothing downstream of a
status variable makes a security decision on it** — it is display-only, by
construction, in every source. That is the discriminating property and it
must be restated at the call site.

**Latency — structural, not a rule to remember.** `status_line_spans` is pure
and takes no handle to anything. The registry's last-known values are
**snapshotted into `AppState` by the event loop**, the same place
`focused_model` and `git_branch` already land, as one
`HashMap<String, String>` field. **The render path therefore cannot *name*
the registry, let alone lock it.** And **TTL expiry is evaluated at snapshot
time, on the event loop, not at render time** — otherwise
`status_line_spans` becomes clock-dependent for *which variables exist* and
two renders of the same `AppState` could differ, breaking the existing test
seam the whole status-line test module is built on.

**Failure.** *A variable that is known but unavailable resolves to the empty
string. Its enclosing group elides. A field whose every rung renders empty is
omitted, exactly as `git` is when there is no repo. Never a panic, never a
block, never a placeholder that could be mistaken for a value.* Plugin
absent, plugin dead, TTL expired, contribution never sent, `focused_model`
before the first `ModelDecision` — all one behavior, the one the existing
ladder already implements.

**Unknown is loud; unavailable is quiet.** `{ctx_pc}` renders `⟨?ctx_pc⟩` in
the error style, in place — loud, local, self-documenting, and it does not
destroy the rest of the line. `{plugin.gh.pr}` when the plugin is dead
resolves to empty. **A typo is a mistake; an absent plugin is a normal
state**, and conflating them is what makes silent matcher no-ops a documented
complaint elsewhere. Three detection routes for unknown names: load-time
validation against the closed host set; the render-time marker; and
`conway status-line explain`, which prints the resolved template, every
variable it references, its current value, and its source.

Because `status.declare/1` exists, the `plugin.*` namespace gets a **fourth**
route the host namespace does not need: after all plugins register, every
`plugin.*` reference resolves against the declared key universe. Unknown is a
warning plus `PluginStatusChanged` — observer-grade, not fatal, because a
plugin may legitimately be absent this run.

### 8.5 Which surfaces open

**Exactly two. The status line, and the one-shot summary line. Everything
else closed; the transcript closed permanently.** Every surface opened is a
surface to sanitize, width-budget, priority-order, and test.

The one-shot summary line is where the parity principle stops being a slogan:
the *session* tier is facade-level, and a `--status-format "<template>"` flag
proves it with one renderer and one variable set, needing no width budget, no
theme, and no ladder. **Constraint: it writes to stderr, never stdout** —
stdout carries only the assistant's raw text, verbatim, so `conway -p "…" >
out.txt` yields clean content, and a summary line on stdout would break that
guarantee for a cosmetic feature.

**Transcript entry prefixes are closed permanently, and this is the strongest
"no" here.** The transcript's entire job is attribution: who said what. Making
`you>` and the assistant marker config-authored turns provenance labeling
into a spoofing surface, on the one surface where spoofing matters, and
project config participates in that. It is also the surface a clean-copy
invariant protects with a test, and a config-authored prefix lands in the
user's clipboard.

Also closed: the sticky prompt breadcrumb (its content is one thing and its
whole design is about not lying about *which* prompt); the scroll footer
(templating buys renaming a recovery affordance); and `/agents` panel rows —
the most *tempting*, because they are genuinely tabular, and closed for a
specific reason: `hop_label` is deliberately shared with the status line's
`lineage` field so the breadcrumb and the panel can never disagree about how
an agent came to exist. **Templating one side reintroduces exactly the drift
that sharing was built to prevent.** Revisit only if a user asks, and then
template *both* from one definition.

---

## 9. The four walkthroughs

### 9.1 A spawned agent using inference to evaluate permission gates

**Point:** `permission.policy/1`, registered as a **`NarrowingPolicy`**
(§5.2). Registration declares `ExtensionSelector { tools: ["bash","write",
"edit"], categories: [Execute, Edit, Delete] }`, `timeout_ms: 20_000`,
`on_failure: Deny`.

**Late binding — verified end to end.** The policy binds to
`PermissionBroker::decide`, not `PermissionGate`. Confirmed against the
shipped code rather than asserted:

- `runner.rs:307` is the single call site: `broker.decide(&perm_ctx,
  &authorized).await`. Every tool call in the system passes it.
- `decide` (`permission.rs:479-608`) has four `Allow` returns —
  cache `:551`, pattern `:568`, `AutoAllow` `:579`, gate `:607` — and only
  the last is downstream of `gate.check` at `:592`. **A composite
  `PermissionGate` would be invisible to three of the four.** The policy runs
  at step 3 (deny) and step 7 (allow), so it sees all four.

**What it returns.** `Deny { reason } | Abstain`. It has no `Allow` variant.

**The security constraint — expressed structurally, and the argument is
sharper than "may only narrow."** The text being judged (the tool arguments
the model proposed) is attacker-controlled: a hostile repo drives the model
via indirect injection, and the arguments carry that influence into the
classifier's prompt. So the requirement is that inference can never turn a
denial into an allow. **`decide`'s ordering makes that impossible
independently of the policy's type**: every deny path — the root check at
`:500`, plan mode at `:525`, and the policy chain's own deny half — **returns
early.** By the time step 7 could run, no denial exists to overturn. The only
thing a step-7 allow can convert is a *prompt* into an *allow*.

That is still a widening, so the type split closes it: a `NarrowingPolicy`
cannot reach step 7 at all. **Two independent guarantees, one structural in
the ordering and one structural in the type**, which is why the inference
case is expressible here rather than merely permitted.

**Recursion — avoided two ways, both measurable.** The policy classifies by
running one ephemeral spawn in the shape `crates/conway/src/intent.rs`
already uses: `SubagentHost::start(parent, SubagentSpec { mode: Spawn,
ephemeral: true, keep_alive: false, role: Some("guard"), tools:
Some(ToolSelector::Only(vec![])), budget: max_steps 2, .. })`
(`intent.rs:254` is the zero-tools line). Then:

1. **Zero tools means zero `ToolCallProposed`, so zero re-entry into
   `decide`.** The classifier must answer from the prompt alone.
2. **`SubagentHost::start` is a host callback, not a tool call**, so the
   spawn itself never enters `decide` either.

**But nothing in (1) is enforced, and that is a hole this walkthrough
closes.** A policy that spawns with `ToolSelector::All` produces a child
whose tool calls re-enter `decide`, which re-consults the same policy, which
spawns again. `Budget::max_steps` bounds each agent and not the chain. So:

> **Invariant: policies are not consulted for a call made by an agent that a
> policy spawned.** Checked on `PermissionCtx.agent_path`, which is already in
> scope at `decide`. Needs a test that registers a policy which deliberately
> spawns a tool-bearing child and asserts the chain is skipped for that
> child's calls.

**Deployment split, which the specs left implicit.** An in-process policy
(embedder-supplied, or a built-in plugin) can use conway's own models through
`SubagentHost`, as above. A **remote** policy cannot: there is no host
inference callback and there will not be one (§13.3), so it issues its own
LLM call with its own credentials. Both are the same point with the same
verdict type; only the source of the inference differs, and `/plugins` shows
which.

**Timeout or error.** `timeout_ms` clamped against the operator maximum;
expiry yields `on_failure` (default `Deny`) plus `PluginStatusChanged`. Two
things bound the resulting availability cost. First, `Deny` is *stricter than
absence* — an absent policy falls through to the human gate — so the default
is deliberately the strict one, on the reasoning that a guardrail which fails
should not silently stop guarding. Second, the liveness ping (§6.8)
distinguishes *slow* from *wedged*, so a policy legitimately waiting on its
own model call is not cut off, while a dead one is. The operator can set
`on_failure: abstain` explicitly and see that they did, in `/plugins`.

**Verdict: works. One revision** (the Narrowing/Deciding type split, §5.2 —
D2's single-enum-plus-boolean lost) **and one new invariant** (no policy
evaluation inside a policy's own subtree).

> **Status (2026-07-30).** This walkthrough's `mode: Spawn` choice, read
> alongside `crates/conway/src/intent.rs:250`'s identical choice for its own
> zero-tool judge, is not incidental — it is now the *decided* default for
> every inference-evaluated hook, not just this one (§16.1), on the same
> security-asymmetry argument this walkthrough already makes for why the
> classifier gets zero tools. The no-recursion invariant generalizes the same
> way: "no hook evaluation inside a hook's own subtree," checked once,
> generically, on `PermissionCtx.agent_path` (§16.2c) — not reimplemented per
> hook kind. And the 60 s `timeout_ms` clamp this section's "Timeout or
> error" paragraph describes is, as of the redirect, non-extendable: a
> decision-bearing call does not get the progress-reset §6.8 grants a tool
> invocation (§16.2d).

### 9.2 MCP shims

**What someone writes:** one plugin fronting one MCP server — a subprocess
speaking JSON-RPC to conway on stdio and MCP to the server. Roughly: connect
to the server, call `tools/list`, translate each entry into a
`WireToolSpec`, answer `tool/invoke` by forwarding to `tools/call`, and
translate the result.

**The `ToolSpec` ↔ MCP `Tool` mapping is as close as it looks**, with four
seams, three of which this architecture already closed:

| conway | MCP | seam |
|---|---|---|
| `name`, `description` | `name`, `description` | clean |
| `schema` | `inputSchema` | **closed by §6.4**: the wire carries raw JSON Schema, so MCP's 2020-12 reaches `jsonschema` verbatim. The `schemars` draft-07-flavored round trip — the one thing D2 flagged as "a compatibility risk to test explicitly, not assume" — is gone by construction. |
| `category`, `permission` | *nothing* | The shim must synthesize both, and the default is the most restrictive: `Execute` / `Dangerous`. Only the operator may lower it (§7.5). |
| `path_args`, `render_kind` | *nothing* | Synthesized. `Unconfinable { checkable: [] }`, and `Structured`. |

**`render_kind` is where the shim gets something concrete from work that
landed after these specs were written.** An MCP shim tool does not override
`render`, so it truthfully declares `Structured` and passes the generic
consistency guard. That means an operator's `docserver__search:*` grant
**actually grants** — before `68ea9b1` it would have been inert for the same
reason `read:*` was, because the default JSON rendering's `(){}` trip the
metacharacter gate. The single most common operator want for an MCP shim
("stop asking me about this read-only search tool") is now expressible.
§5.7's semantic split is the thing a shim author must know.

**Server-declared hints never reduce restriction.** MCP's `readOnlyHint` /
`destructiveHint` are claims by the extension about itself; honoring them
would let a server declare `readOnly` to walk past plan mode. They are
rendered in the review surface as *suggestions* and applied only by an
operator's `tool_class` entry.

**Outputs.** Text and image content map directly to `ContentBlock`; `isError`
maps to `ToolOutput.is_error`; `TruncationPolicy` has no MCP analogue so the
shim declares the host default. **A `resource_link` becomes an `Artifact`,
not a `ContentBlock::Text` containing a URI** — a URI in text is a path
smuggled in a string, which is exactly the shape `ToolOutput::artifacts`
exists to replace. An embedded `resource` with inline contents becomes a
`ContentBlock`, subject to truncation like anything else. (D3 flagged this as
undecided; deciding it is this document's job.)

**Two MCP spec facts that matter and point the same way.**
`sampling/createMessage` is **deprecated as of the 2026-07-28 spec
(SEP-2577)**, so any design in which servers call back for inference builds
on a closing door. This architecture does not depend on it in either
direction: conway does not offer sampling to servers (§13.3 refuses a host
inference callback for its own reasons), and a conway shim does not need
servers to offer it. And **SEP-2260 now restricts server-initiated requests
to occur only while the server is processing a client call** — which is
*exactly* §6.1's `ctx_token` lifetime rule, arrived at independently for
independent reasons. Two protocols converging on the same discipline is a
strong signal the discipline is right; record it so the next reader does not
treat the token's short life as conway-specific fussiness.

MCP prompts, resources, and roots are out of scope for v1; only `tools/*`
maps. MCP `roots` is tempting because conway has confinement roots, but
conway's root is host-owned and may only narrow, so exposing it to a server
would be a disclosure rather than a control.

**Verdict: works, cheap, no revision required.** The one thing to verify
rather than assume is 2020-12 schema handling end to end, and §6.4 already
removed the mechanism that would have made it hard.

### 9.3 Plugin interface compatibility

This was an illustration, not a requirement. **Reporting what becomes cheap
and what stays expensive; not picking one.**

**Cheap.**

- **Consuming MCP servers** — §9.2. One shim process per server, a few
  hundred lines, no conway change beyond the plugin host. Residual friction:
  the classification act (§7.5), which is one operator review per server.
- **Reading MCP server configs** (`.mcp.json`-shaped) — command, args, env is
  the same shape as `plugins.json`. A translator is trivial.
- **`required_host_caps` across versions** — cheap *under this design*,
  because §6.7 and §7.3 give the inert field a job. The cost is the closed
  capability-name set, which is a forever promise, and that is why §7.3 keeps
  it as short as it is.

**Moderate.**

- **Exposing conway as an MCP server.** The vocabularies serialize already.
  The hard part is not the wire — it is that conway's tools require a
  `ToolCtx` with an `agent_id`, a `session_id`, and a `SubagentHost`. They are
  meaningless outside an agent, so exposing conway's *thirteen tools* over MCP
  would mean handing out a runtime-less `ToolCtx`: a category error. What is
  coherently exposable is **agents** — one or two MCP tools shaped like
  `conway_subagent` ("run an agent with this prompt and contract"), which is
  really the ACP shim `event.rs` already anticipates in a comment. Moderate,
  and a genuinely different product decision from everything else here.

**Expensive, and the expense is not parsing.**

- **Reading Claude Code's hook formats.** Their model is a shell command
  invoked per event with JSON on stdin, ~30 event names, and settings
  scattered across user/project/local/enterprise files plus per-project MCP
  files. A translation layer is writable as a third-party plugin — it execs a
  hook per event — but **it re-acquires everything this design rejected**:
  per-invocation spawn cost, no cancellation, no supervision, no capability
  model, and a discovery story with no way to list what is active. It is
  mechanically possible and deliberately not a host feature.
  One hard incompatibility to flag: their matcher paradigm is *exact unless
  special characters appear, then regex*, whose failure mode is a pattern
  that silently matches nothing. §5.4's one paradigm cannot represent that
  faithfully, so a translator must **reject such patterns loudly** rather than
  approximate them. That is the right call and it means some existing configs
  will not translate.

### 9.4 Status line / UI instrumentation, end to end

A plugin declaring a variable to a glyph on screen, ten steps:

1. **Declare.** `WireManifest` carries `optional_host_caps: ["status",
   "observe"]`, an `observe/1` selector (`{ events: ["tool_call_*"] }`), and
   `status.declare/1`: `{ "pr": { "max_len": 12, "ttl_ms": 300000 } }`.
2. **Trust and grant.** Global-scope entry: trusted by authorship. Project-
   scope: needs a `trust.json` subject or it does not load. **Caps are
   explicit either way** — the operator grants `status` and `observe`.
3. **Subscribe.** The host subscribes the plugin to `EventBus::subscribe()`
   — a broadcast receiver, never an `EventSink` (§6.1 rule 1) — filtered by
   the declared selector. The selector is resolved against the known `Event`
   tag set at build; matching zero tags is a **warning** for an observer, and
   would be a **registration error** for a participant.
4. **Contribute.** `status/contribute` notification: `{ "key": "pr", "value":
   "#412 ✓" }`. **No `ctx_token`** — this is not inside a tool invocation, so
   identity comes from the connection, per "the plugin never supplies
   identity."
5. **Ingest.** Sanitize (control chars → U+FFFD), truncate to the declared
   `max_len` in chars, stamp `expires_at = now + ttl`, store last-known-value
   keyed `(plugin_id, key)`.
6. **Snapshot.** The event loop copies live values into
   `AppState.plugin_status_vars`. **TTL is evaluated here**, on the event
   loop, so the render path stays clock-free about which variables exist.
7. **Configure.** In the operator's *user-scope* `settings.json` only:
   `custom: { "pr": { "template": ["PR {plugin.gh.pr}", "{plugin.gh.pr}"],
   "style": "status_dim", "priority": "telemetry" } }`, and `"pr"` added to
   `fields`.
8. **Validate.** Load-time validation skips `plugin.*` names (plugins
   register after config load) — and then, after registration, resolves every
   `plugin.*` reference against the declared key universe (§8.4). Unknown:
   warning + `PluginStatusChanged`.
9. **Render.** `field_ladder` yields the two rungs; `ladder_width` sums real
   display columns; the field's `telemetry` priority makes it give up space
   before `orientation`, `activity`, `hint`, and `mode`. The pinned
   invariant: no custom-field configuration makes `mode` lose a rung earlier
   than today.
10. **Fail.** Plugin dead, TTL expired, or never contributed: the value is
    empty, the `[...]` group elides, both rungs render empty, the field is
    omitted — exactly like `git` with no repo. Never a panic, never a block.

**Obligations:** width (5 + 9), sanitization (5, plus the template-literal
boundary at parse), latency (6, and it is structural — the render function
cannot name the registry), failure (10). All four met.

**Verdict: works, with one addition** — `status.declare/1`, the registration
half D5 assumed and D2 did not create (§11.4).

**Disclosed asymmetry, carried forward deliberately:** a `status/1` plugin
can write values and cannot *read* host variables. Everything it needs is
reconstructible from the event stream, so v1 says no. It is a real ergonomic
cost, and the fix is cheap later because `Vars` is already a snapshot type —
moving it to core makes it a wire commitment, which is the reason not to do
it casually.

### 9.5 The want that matters more: general rules for tool use

**The ask:** express a richer policy than a prefix on a rendered string,
without writing Rust. The canonical example: *all reads under `./src` are
fine; writes prompt.*

**Today this is not expressible, and the reason is not syntax.** The grant
language's only predicate is a prefix over a *rendering*. For `read`, the
rendering is `read({"path":"/etc/shadow"})`. A prefix like
`read:read({"path":"./src` is a JSON-substring match: brittle, dependent on
serde key ordering, and semantically wrong — it does not resolve `..`, so
`./src/../../../etc/shadow` matches the prefix and escapes the intent. Until
`68ea9b1`, `read:*` did not match at all; now it does, but it is *all* reads,
unconstrained by path. Extending the prefix language cannot fix this, because
a prefix over a rendering is the wrong predicate for a question about paths.

**Under this architecture, in `permissions.json`, no Rust:**

```jsonc
{
  "rules": [
    { "tools": ["read","grep","glob"], "when": { "paths_under": "./src" }, "then": "allow" },
    { "categories": ["edit","delete"],                                     "then": "prompt" },
    { "tools": ["bash"], "when": { "command_prefix": "curl" },             "then": "deny" }
  ],
  "allow": ["bash:cargo test"],
  "deny":  ["bash:ssh"]
}
```

`paths_under` is evaluated by taking the tool's **declared `path_args`
names** and resolving each argument exactly as `check_root` does, through
`resolve_like_the_tool_will` (`permission.rs:172`). So `../../../etc/shadow`
does not pass, and an `Unconfinable` tool **never** satisfies the condition —
fail closed, the same asymmetry the root check already uses.

**Is it materially better? Yes, and the reason is one sentence:** it changes
the predicate from a string prefix over a rendering to a **resolved-path
containment over declared arguments**, which is the same predicate the
confinement root already uses and already trusts. It is not a new security
primitive; it is the existing one made expressible in config. **No new
trusted code** — which is the strongest available form of "materially
better," and the reason to prefer it over any richer rule language.

**The ceiling, stated honestly rather than hidden:**

- `command_prefix` is still a prefix and still not a boundary. `deny bash:git
  push` does not catch `foo; git push` (§5.6).
- `paths_under` is only as good as the tool's `path_args` declaration — but
  that is a *narrowing-only* self-declaration (declaring names can only add
  checks), which is precisely why it is safe to trust. Same argument as
  `render_kind`, and the same generic guard obligation (§6.5).
- "Reads under `./src` but not `./src/secrets`" needs a second `deny` rule.
  That works, because deny beats allow — but it is two rules, not an
  exclusion operator, and there will be no exclusion operator.
- Content-dependent policy ("allow bash if it does not touch the network")
  is **not** expressible here and should not be. That is
  `permission.policy/1`'s job, and it costs a live participant with a
  timeout, a failure mode, and an operator grant. Keeping the rule language
  incapable of it is what keeps it evaluable in-process, with no RPC, no
  fail-open window, and correct behavior when the plugin is dead.

**And the largest practical point: none of this needs the transport.**
§5.4's rule form, `paths_under`, the `deny` half, and origin tracking are all
in-process changes to `conway-core` and `conway-runtime`. Phases 0–2 of §12
deliver the entire "general rules" want with no plugin host at all. If the
spike produces one shippable thing, it should be that.

**Shipped form (board item F12).** The `rules` array parses into one
internal `Rule { select, when, then }`, evaluated by one evaluator — the
flat form desugars into the same `Rule` (`PatternRule::to_rule`) and is
byte-identical to its structured equivalent (`matches_render` vs
`matches_allow_render` are the same primitives; proven by a matrix test in
`permission_pattern::f12_tests` and a real-stack seam in
`crates/conway/tests/structured_rule_seam.rs`). The grammar:

- `select ::= tools([pattern...]) | categories([ToolCategory...])` — a
  tool name with an optional trailing `*` wildcard, or a category set.
- `when ::= paths_under(prefix) | command_prefix(s) | category_in([...]) | always`
  — `paths_under` resolves declared path arguments via
  `resolve_like_the_tool_will` + `CanonicalRoot::contains` (the SAME path
  `check_root` uses, no new trusted code); `command_prefix` is the shell
  token-wise prefix the flat form uses; `category_in` matches the call's
  declared category.
- `then ::= allow | prompt | deny`.

The five traps, resolved as stated above and pinned by real-path seam tests
(real tools → real `ToolRunner` → real `PermissionBroker`, asserting on
observable gate-reach outcomes): (1) `paths_under` reads `call.arguments`, never the sanitized
`call.rendered`; (2) `PathArgs::Unconfinable` never satisfies `paths_under`
(fail closed); (3) `command_prefix` on a `Structured`-rendering tool is a
typed registration error surfaced in `PermissionLoadReport`, not a silent
inert rule; (4) the allow-side metacharacter gate applies to every `when`
(unchanged from the flat form — a chained command never auto-allows);
(5) `deny`/`prompt` install from every file unconditionally (narrowing has
no trust precondition), `allow` only from operator-owned (trusted/global)
config. Composition is the two stages §5.5 already states: deny/prompt
admit unconditionally at stage 1, allow only when trusted; stage 2 is
most-restrictive-wins (deny beats prompt beats allow), with no priority
numbers and no order dependence.

---

## 10. Failure posture, in one table

| what fails | what happens |
|---|---|
| plugin never handshakes | registers no tools; `PluginError::Init`; startup refused only if `required: true` |
| plugin crashes mid-session | pending calls fail immediately with the exit status and stderr tail; policies contribute `on_failure`; lazy backoff-gated restart |
| plugin ignores cancellation | abandoned after 2 s (host freed), connection `Draining`, killed via process group after 5 s |
| plugin never answers | inactivity deadline, then `ToolError::Timeout`. A decision-bearing call becomes `Deny` |
| plugin floods notifications | token bucket drops with a counter and a synthesized `Lagged`, never backpressure |
| plugin sends a 5 MiB frame | connection poisoned, child killed, pending calls fail closed |
| plugin sends a stray `println!` | logged, counted, dropped; 16 consecutive poisons the connection |
| plugin declares a colliding tool name | nobody gets the bare name; both announced qualified; a diagnostic names both |
| plugin declares an uncompilable schema | tool rejected at connect, named; plugin degraded; **never a panic** |
| plugin declares `path_args` naming a nonexistent field | registration error (§6.5) |
| plugin declares nothing about paths | `Unconfinable { checkable: [] }` — never `None` |
| plugin is untrusted or de-trusted | not loaded; one notice; the session starts degraded |
| `trust.json` corrupt | treated as empty, loudly; never partially applied |
| a status contribution goes stale | expires at snapshot; the group elides; the field is omitted |

**The one invariant above all of these: there is no code path in which a
transport-level failure, a trust failure, or a plugin failure produces an
allow.**

---

## 11. Settled questions — the reconciliation ledger

Read this before re-opening anything. Each entry is a disagreement found
between the five slices, the decision, and what lost.

### 11.1 D1's `Deadline::Unbounded` vs D2's participant classification

**Conflict.** D1 carved out `Deadline::Unbounded` for extension points whose
port contract sanctions indefinite blocking, naming the permission gate. D2
then decided no plugin is ever a gate, and that its permission participant
must be *bounded*. D1's primitive was left with no host-to-plugin user.

**Decided.** Every **host-to-plugin** call is `Deadline::Bounded`, no
exceptions. `Deadline::Unbounded` survives with exactly one user in the
**other** direction — the plugin-to-host `subagent/await` callback — safe
only because `await_result`'s port contract guarantees termination. Liveness
pings apply everywhere. **Lost:** the reading in which a plugin could hold an
unbounded host-to-plugin decision open. §6.8.

**Note for a future reader:** the tension D1 described was real. It did not
survive D2's decision, which means the *reasoning* in D1 §4 is still correct
and its *conclusion* now applies to a case that does not exist. Do not
resurrect the carve-out by pointing at D1.

### 11.2 D3's `ToolCtx` shape vs D4's capability model

**Conflict.** D3 argued full parity in kind, projected in shape, with a
`grants` array in the per-invocation `ctx` and `CapabilityNotGranted` on the
wire. D4 rejected a reduced `ToolCtx` outright and instead keyed reduction on
the operator's grant applied identically to built-ins, requiring
`ToolCtx.subagents` itself to become a guarded handle in Rust.

> **Status (2026-08-04), board item 01KZ59SXNQ3BRXP49V4JW10N72 (C1).** The
> premise "a compiled-in third-party plugin would hold an unguarded
> `SubagentHost`" is now partly false: every tool, compiled-in or remote,
> holds a `SubagentHandle` that confines it to its own subtree by
> construction. That closes the cross-tree hole independently of any grant
> mechanism, so the "unguarded" case below is now specifically "unguarded
> with respect to CAPABILITY GRANTS" — a tool can still spend tokens and
> spawn freely within its own subtree without any operator grant. The
> conflict this section resolves is therefore still live for grants, and the
> resolution still stands; only the severity of the D3-only scenario drops.

**Decided: they are the same answer, and D4's is strictly stronger, so D4's
mechanism is the enforcement point.** D3's stops at the wire. If only D3
landed, a compiled-in third-party plugin would hold an unguarded
`SubagentHost` while a remote one is checked — which is the same privileged-
API violation D4 identified, relocated from "built-in vs third-party" to
"in-process vs remote."

So: **the guarded handle is the single enforcement point, hit identically by
the in-process call and the RPC bridge. `ctx.grants` is retained as advisory
disclosure, not enforcement.** The wire schema stays uniform (D4's own
position, and D3's reason 1 is the better argument for it: a missing method
is indistinguishable from a version mismatch). **Lost:** wire-level
enforcement as the mechanism, and schema-level omission as an alternative.
§7.3. This closes D3 OQ1 and D4 OQ1 together.

### 11.3 D2's most-restrictive-wins vs D4's allow-requires-trust

**Conflict.** They look like one rule stated twice. They are not.

**Decided: two stages of one pipeline, and stating them as one rule is how
they would drift.** Stage 1 is **admission** (trust): a widening enters the
evaluation set only if its author is trusted and the operator granted it; a
narrowing always enters. Stage 2 is **composition** (most-restrictive-wins)
over what was admitted, order-independent. An untrusted `allow` is never
"overridden later" — it never enters. §5.5.

### 11.4 D5's plugin variables vs D2's `status/1`

**Conflict.** D5 requires a `max_len` declared **at registration**; D2's
`status/1` is push-only with no registration surface at all. D5 built on a
point D2 did not fully create.

**Decided in D5's favor: `status.declare/1` exists.** It is required anyway,
because §7.3 makes `status` a granted capability and a granted capability
implies something to grant. It pays for itself twice by giving the
`plugin.*` namespace a validation route D5 could not otherwise have (§8.4).
D5's TTL-at-snapshot-time refinement is adopted over D2's looser
"contribution expires." **Lost:** push-only `status/1` with no declaration.

### 11.5 D3 and D4 on `subagent.spawn`

**Conflict, twice over.** (a) D3's v1 capability set has six names
(`subagents.start/steer/await/cancel/ask/tree`); D4 has one
(`subagent.spawn`). (b) D3's position is that the grant defaults to
granted-and-visible for an operator-installed plugin; D4's is default-off,
never implied by trust.

**Decided (a): two capabilities, not one and not six.** `subagent.spawn`
covers `start`/`ask`/`steer`/`await`/`cancel` — the last three are meaningless
without `start`, because targets are already authorized against a per-token
set of agents this token started. `subagent.tree` stays separate, because it
is *read* of other agents' existence rather than spawn, and because **no
built-in tool calls `tree()` at all**, so shipping it on would give plugins a
surface no built-in has. D4's naming convention wins. **Lost:** D3's six-name
split and D4's single name.

> **Status (2026-07-30), board item 01KYTP0PGKJ4VCJP5TD39A1WHF.** "No built-in
> tool calls `tree()` at all" is a true, and still-accurate, statement about
> what the built-in tools DO — but it is not a statement about what any tool
> (built-in or third-party) COULD do, and that gap was live: `tree()` sat on
> the `SubagentHost` trait object every tool holds via `ToolCtx::subagents`
> unconditionally, took no caller, and returned the runtime-wide tree to
> whichever tool called it. Composed with `start`/`ask` (also unguarded —
> `start`/`ask` took only `parent` and acted on it directly, with nothing
> checking the caller was entitled to act there), this was cross-tree
> exfiltration in one call, reachable in-process regardless of this
> section's wire-level capability gating (which governs only OUT-of-process
> plugins, not the in-process trait boundary every built-in tool already
> crosses). All three now enforce "caller may act only within its own
> subtree" at the trait boundary itself (`ensure_own_subtree`, the same
> mechanism board item 01KYT8TS0EBKJHYNJRF6S88NRH already added for
> `steer`/`await`/`cancel`), so the reasoning above about the WIRE-level
> `subagent.tree` capability being unnecessary now rests on a port that
> itself enforces subtree confinement, not on "no built-in happens to call
> it."

**Decided (b): D4. Default off, never implied by trust.** Trust answers "may
this code run as me"; "may this code spend my tokens and start agents that
run tools" is a different question with a different answer for most plugins.
Collapsing them would give an MCP shim for a documentation server the same
authority as an orchestration plugin. **Lost:** D3's convenience position.
The cost is one extra operator act per orchestrating plugin, mitigated
because `required_host_caps` makes it a loud load failure naming the cap.

**Does D3's wire design compose with default-off?** Yes, and better under the
unified answer: the method exists on the wire always, the guarded handle
refuses, the error is typed in D3's own `-32000..` range, and `ctx.grants`
tells the plugin up front so it need not probe.

### 11.6 `Plugin::on_init` is gone

D1 rejected it as the handshake hook and asked (OQ5) whether to wire it up.
**It is being removed from `conway-core` as this is written**, on the
grounds that a hook that silently never runs is worse than an absent one.

**Consequences, so nobody rebuilds on it:** `Plugin` has exactly two methods,
`manifest()` and `tools()`. Connect is `PluginProcess::connect`, which runs
*before* the `Arc<dyn Plugin>` exists. **D2's proposed facade re-export list
must drop `PluginInitCtx`**, which will not exist. D2's table line describing
`on_init` as "the connect" is superseded.

### 11.7 D2's "shell-shaped renders" vs the landed `RenderKind`

D2 §10 wrote `command_prefix` as applying "for shell-shaped renders only"
with no mechanism for deciding what that means. `render_kind` landed after
D2 and is that mechanism.

**Decided:** `command_prefix` applies iff `render_kind == ShellCommand`.
For a `Structured` tool the condition is unsatisfiable, and per D2's own rule
that a participant rule which can never match is a registration error, **it
is a registration error, not a silent never-match.** Additionally,
`PolicyRequest` carries `render_kind`, because `AuthorizedCall` now does
(`runner.rs:297`) and a policy would otherwise have to infer it from the tool
name — the one thing §3 forbids.

### 11.8 D3 excludes `PatternRule` from the wire; D4's ceremony needs rules on the wire

**Conflict.** D3 marks `PatternRule`/`PermissionFile` **X — excluded**. D4's
suggested-allow-rule ceremony requires a plugin to transmit rules "verbatim,
in the same wire form the operator's own file uses."

**Decided: both, because they are about different things.** The wire carries
rule **strings in the operator's own surface syntax** (`"bash:cargo test"`),
never a serialized `PatternRule` type. The host parses them with the same
`parse_rules` the file loader uses. This satisfies D3's exclusion (no Rust
type promise) and D4's requirement (the suggestion and the operator's file
are byte-identical), and it is only coherent because §5.4 made the string
form the surface syntax of the structured form rather than a separate
language.

### 11.9 D5's project-scope rule has nowhere to attach in D4

D5 defers project-scoped templates "behind D4's workspace-trust ceremony,"
but D4's `kind` enum has only `plugin` and `permission_file` — no settings
kind.

**Decided:** v1 keeps D5's rule as written (project templates ignored with a
diagnostic). The future unlock is a third trust kind, `settings_template`,
which is one line in the enum, exactly as D4 says adding `profiles.toml`
would be. Not v1, and recorded so it is a decision rather than a dangling
reference.

### 11.10 Three sanitizers, and one of them is semantically different

D4 and D5 both want `sanitize_rendered` hoisted; D5 counts three copies.

**The tree actually holds two U+FFFD *replacers* — `runner.rs:413` and the
documented hand-copy at `permission_pattern.rs:317` — and one *dropper*,
`header.rs:232`, which filters control chars out entirely.** That is a
semantic divergence, not just duplication, and it matters: replacing
preserves char count (so `truncate_chars_with_ellipsis`'s budget arithmetic
is unchanged) while dropping does not.

**Decided: one function, `conway_core::text::sanitize_control`, replacing
with U+FFFD, called by all three** — plus the `status/1` ingest, the template
literal boundary, and `ToolOutcome::error`'s construction. `header.rs` moving
from drop to replace is strictly safer for its own truncation math. **Lost:**
the drop semantics, and the idea that hoisting is only a de-duplication.

---

## 12. Follow-on work — proposed, not created

**These are not on the board.** Turning a spike into a cycle is the user's
call. Sizes are XS/S/M/L/XL relative to this codebase's usual work items.

**The shape worth noticing before reading the list: phases 0–2 deliver most
of the user-visible value with zero transport work.** Working grants, real
rules, the deny half, and the policy port need no plugin host. The transport
is the expensive part and it is not on the critical path for §9.5, the want
that matters most.

> **Status, 2026-07-30.** F10 and F11 shipped the same day this plan was
> written (`d917ba2`), along with three other spike-discovered bugs
> (`b17cab7`, `b00b18f`, `674bb65`). **F12 — the §9.5 deliverable — is now
> unblocked**, since F11 was its stated prerequisite. Phase 1's remainder is
> F12, F13, F14.
>
> Two residuals from that batch are not items below and are recorded here so
> they are not lost:
> - **`EventBus.seqs` reclaims only `conway_ask` modal children.**
>   `conway_subagent` builds spawn *and* fork with `ephemeral: false`, so
>   those counters still leak. The fix is to have `resume_root` reseed the
>   counter from the persisted transcript's last `seq`, which makes any
>   finished session prunable.
> - **`deny` rules are not listed in `/settings`.** An operator can audit
>   what a repo *granted* them but not what it *denied* them.
>   `active_deny_permission_patterns()` exists and is tested but is unwired
>   to the view. Same shape as the trap F11 closed, smaller blast radius.

### Phase 0 — enabling changes and bugs, each shippable alone

| # | Item | Size |
|---|---|---|
| F1 | `Runtime::try_new` + `RuntimeError::PluginRegistration`; `Runtime::new` becomes a thin wrapper. One production call site (`builder.rs:407`). | S |
| F2 | Hoist `sanitize_control` to `conway-core::text`; converge `runner.rs:413`, `permission_pattern.rs:317`, `header.rs:232` (§11.10). | S |
| F3 | Apply it at `ToolOutcome::error`'s construction, closing the deny-reason path into model context (§7.6). | S |
| F4 | `PermissionMode` and `Role` become `#[non_exhaustive]`. | XS |
| F5 | Sanitize `focused_model` before it reaches a `Span`. | XS |
| F6 | `git_branch` / `cwd_display` async refresh, on the event loop, never the render path. Depends on F17's `CwdChanged`. | M |
| F7 | Replace the theme grep guard's fixed file list with a directory walk over `view/`. | S |
| F8 | Facade `pub mod plugin` — the curated re-export (minus `PluginInitCtx`). Today `Tool` is exported but not implementable, and `with_context_hook` accepts a type no external caller can name. **Worth doing on its own; enabling for everything else.** | S |
| F9 | `PluginManifest.version` semver-validated at registration (~30 lines, no new dependency). | S |

### Phase 1 — operator-facing permissions, no plugins needed

| # | Item | Size |
|---|---|---|
| F10 | ✅ **DONE** (`d917ba2`) — `permissions.json` `deny` half + trust gate on project `allow` (§7.4 items 1–2). Trust is keyed on (path, blake3 digest), so editing a file de-trusts it. | — |
| F11 | ✅ **DONE** (`d917ba2`) — origin tracking on `active_patterns()` via `PatternOrigin`. **This unblocks F12.** Also enabled per-rule revocation, which was not in this plan and turned out to depend on it. | M |
| F12 | **The structured rule form** (§5.4): flat strings become sugar; `paths_under` via `resolve_like_the_tool_will`; `command_prefix` a registration error for `Structured` tools. **This is the §9.5 deliverable.** | L |
| F13 | Project rule scope narrowing (§7.4 item 3). Behavior change; needs a call. | S |
| F14 | Widen `suggested_rule` / the `[p]` offer to consult `render_kind`, so the UI's offer surface matches the evaluation surface (§13, bug 3). | S |

### Phase 2 — the policy chain, still no transport

| # | Item | Size |
|---|---|---|
| F15 | `PermissionPolicy` port: `NarrowingPolicy`/`DecidingPolicy` split, chain at steps 3/7, lock-dropped-before-await, `Runtime::set_permission_policies`, `ConwayBuilder::with_permission_policy`, the no-recursion invariant (§9.1), and the "zero policies ⇒ byte-identical" test. | L |
| F16 | `Event::PermissionResolved { by: Option<String> }`, additive. | S |
| F17 | The five new events: `CwdChanged`, `PermissionModeChanged`, `PermissionGrantChanged`, `PluginStatusChanged`, `AgentSpawned { root }`. Each moves the variant-count assertion, which exists to force this conversation. | M |
| F18 | `GuardedSubagentHost` per plugin, built from `RegisteredTool.plugin_id`, with the built-in seed grant. Behavior change; needs the human sign-off §14.3 asks for. | M |

### Phase 3 — the transport

| # | Item | Size |
|---|---|---|
| F19 | `conway-plugin-host` crate: NDJSON framing, symmetric JSON-RPC, disjoint id spaces, reader/writer tasks, the `ctx_token` table. | XL |
| F20 | Process lifecycle: `.process_group(0)`, shutdown ladder, `Drop` guard, backoff, `Unhealthy`. Requires lifting `kill_group` to `conway_tools::process` (needs sign-off). | L |
| F21 | The `wire::` module, golden fixtures, and the vocabulary registry test. | L |
| F22 | `plugins.json` discovery, `Conway::plugins()`, `conway plugins`, TUI `/plugins`. | L |
| F23 | `trust.json`, digests, `conway plugins diff`, `conway trust list`, the review surface at the **back** of the TUI surface queue. | L |

### Phase 4 — the points

| # | Item | Size |
|---|---|---|
| F24 | `tool/1` + `tool.spec/1` over the transport — the first real remote plugin. | L |
| F25 | `observe/1` + `status.declare/1` + `status/1` + the host-side status registry. | M |
| F26 | The template language, `ResolvedField`, `Theme::slot`, `conway::instrument::Vars`, `--status-format`. | L |
| F27 | `permission.policy/1` + `permission.rules/1` over the transport (F15 ∘ F19). | M |
| F28 | `context.append/1`, with rendered provenance. | M |
| F29 | An MCP shim in-tree as the reference implementation, written against the public surface like a third party would. | M |

> **Status (2026-07-30).** F28 (`context.append/1`) is superseded by
> `context.hook/1` (§16.5) — the item is the same shape (a new point over the
> transport) but a wider contract: edit/drop parity with `ContextHook`,
> composed per §16.3, plus the durable-mask producer work §16.4 names as its
> own real gap (a `target_seq` per segment, and `Provenance` on
> `LogRecord::ContextMask`). Sizing moves from M to L.

---

## 13. What this architecture does **not** do

An honest boundary beats an optimistic one.

**13.1 WASM / the Component Model is deferred, not rejected.** The blocker is
specific and checkable: conway *requires* async host callbacks, which
stabilized only with WASI 0.3 in Feb 2026, and **WASI 1.0 is not final**.
`wasmtime` is also a very large dependency against a zero-new-dependency
posture (C-04).

**The trigger is empirical, not a version number.** Revisit when all of these
hold, rather than when a spec body publishes a "1.0":

1. stable async host-function support in `wasmtime`,
2. working resource limiting (memory, fuel/epoch) for untrusted guests,
3. at least one production Rust host demonstrating the async-callback pattern
   conway needs.

Stated this way deliberately: a milestone-name trigger can fail to fire on a
technicality if "1.0" slips or ships with caveats, and WASI's timelines have
slipped before. A capability trigger cannot.

> **Correction (2026-07-30).** An earlier draft of this section claimed "one
> major editor declined it this year on maturity grounds; another ships it
> only behind a deliberately narrow API," and used that as ecosystem evidence
> for the deferral. **The first half was uncited and could not be
> corroborated** on review; the one editor found using WASM for plugins (Zed)
> *ships* it, behind a versioned WIT-defined API — which is the second clause,
> not the first. The claim is withdrawn. The deferral stands on the three
> checkable conditions above, which do not depend on it. Recorded rather than
> silently deleted, because an unsourced external claim used as evidence for a
> decision is exactly the kind of thing that should leave a scar.

**The subprocess design does not
foreclose it** — the wire vocabulary (§6) is transport-independent, and the
`wire::` projections would become the component interface almost unchanged.
Revisit when WASI 1.0 is final; do not revisit sooner on enthusiasm.

**13.2 conway does not sandbox plugin processes.** Isolation belongs in tools
and in the deployment, not in the harness. **Trust is the entire control at
the process level, and it is binary.** A trusted plugin runs with the
operator's filesystem, network, credentials, and ability to exec. The
capability vocabulary governs what a plugin can make *conway* do, never what
it can do to the machine — which is why `fs.read`, `net`, and `exec` are
deliberately absent from it (§7.3), and why self-reported intent lives in a
segregated `disclosures` field under a header saying conway does not verify
it.

**13.3 No host inference callback.** A plugin does not get to call conway's
model. It would put token spend behind a callback with no per-call approval,
no routing story, and no budget owner. The sanctioned path — the plugin
issues its own LLM call with its own credentials — already works. (Note the
symmetry with MCP's deprecation of `sampling/createMessage`; both directions
of that door are closing, for related reasons.)

**13.4 No argument rewriting**, ever, by anything (§5.8).

> **Status (2026-07-30) — scope stated explicitly.** This sentence and §5.8
> govern `ToolCall::arguments` and permission verdicts only (§5.9's
> value-class boundary). It does not extend to context, which `ContextHook`
> already permitted to be edited/dropped when this was written, and which the
> redirect widens to remote plugins (§16.4, §16.5).

**13.5 No plugin implementations of `SessionStore`, `Router`,
`HealthRegistry`, `Backend`, `SubagentHost`, or `EventSink`** (§4). Two of
those are structural (synchronous ports cannot be crossed by async RPC); the
rest are decisions with stated reasons.

**13.6 No compaction events, no blocking `Stop` point, no `ConfigChange`.**
conway has no compaction, no "prevent the agent from stopping" concept, and
config is load-time. **A hook naming a feature that does not exist documents
a feature that does not exist.**

> **Status (2026-07-30) — the compaction clause is superseded; the other two
> stand.** conway still ships no compaction *policy* — no built-in decides
> what to drop. What it now has is a stated *path* to one: §16.4 gives
> `LogRecord::ContextMask` a producer (a hook proposing a durable exclusion,
> composed per §16.3, persisted by the runtime as the delta), which is the
> mechanism a compaction plugin would need and previously had nowhere to
> attach. No blocking `Stop` point and no `ConfigChange` remain exactly as
> stated — the redirect did not touch either.

**13.7 Unix only, in v1.** Process-group kill has no direct Windows
equivalent (job objects are the analogue). `bash.rs` already ships a
`#[cfg(not(unix))]` degradation, and this design follows it rather than
inventing a second story.

**13.8 No hot reload.** The tool registry is built once and immutable by
design; changing the announced tool set mid-session would break the
stable-ordering contract the provenance hash depends on. A restarted plugin
that presents a different tool set becomes permanently `Unhealthy`.

**13.9 No plugin marketplace, no code signing, no artifact fetching.** Trust
is a digest of what is already on disk. Conway does not download plugins and
does not verify publishers.

**13.10 Prefix matching is not a containment boundary — in either
direction.** `deny bash:git push` does not catch `foo; git push`. The
confinement root is the boundary; rules are a seatbelt.

**13.11 The transcript is permanently closed to templating** (§8.5), and no
project-scoped config may author screen text (§8.2). Project templates behind
a trust ceremony are a possible v2 and would need a third trust kind
(§11.9).

**13.12 No shell-command status variables**, permanently, not deferred
(§8.3).

**13.13 Disclosed asymmetries** — real, deliberate, and to be documented in
the plugin docs rather than discovered:

- A `status/1` plugin can write values and cannot read host variables (§9.4).
- A remote tool cannot `chdir`; it registers a tool with a declared path
  argument instead (§6.1).
- A remote tool cannot emit arbitrary events — only `ToolProgress` and
  `AgentProgress`, which is exactly what `bash` emits (§6.1).
- A plugin can never install a grant or return `AllowAlways` (§5.2).
- `subagent/tree` returns the calling agent's subtree, not the tree, and is
  off by default — because no built-in has whole-tree visibility either
  (§11.5).

> **Status (2026-07-30), board item 01KYTP0PGKJ4VCJP5TD39A1WHF.** The first
> half of this bullet was aspirational until now, not yet implemented: the
> in-process `SubagentHost::tree()` (`conway-core`'s port every built-in AND
> third-party tool calls through `ToolCtx::subagents`, distinct from this
> wire-level `subagent/tree` capability D3/D4 describe) took no caller at
> all and returned the WHOLE runtime tree to anyone holding the trait
> object — reachable from any tool, not gated on the `subagent.tree`
> capability this section describes. Composed with `start`/`ask` (also
> unguarded — see §11.5's own status note), this was cross-tree
> exfiltration in one call. `SubagentHost::tree` now takes a `caller:
> AgentId` and returns exactly that caller's own subtree (itself, plus every
> descendant), making this bullet's first clause true in-process for the
> first time. The wire-level default-off gating this section decides is
> unaffected — that remains a separate, not-yet-implemented decision about
> the OUT-of-process capability grant, layered on top of an in-process
> contract that now actually holds.

---

## 14. Open questions for a human

1. **The built-in seed grant** (§7.3). Seeding `subagent.spawn` for
   built-in-scope plugins is the thin line the no-privileged-API property
   survives on. It deserves explicit sign-off, not a designer's assertion.
2. **Lifting `kill_group`** out of `conway-tools`' private `unix` module to
   `pub`. The alternative is a documented verbatim copy, which is worse for a
   security-relevant routine.
3. **Project rule scope narrowing** (§7.4 item 3) is a behavior change for
   existing users. Does it ship with the trust gate or separately?
4. **Multi-operator / shared-home hosts.** `granted_by` is recorded but
   nothing enforces it. Is a trust record granted by another user on a shared
   machine honored, ignored, or a warning?
5. **Digesting an artifact that is not a single file.** A `node`/`python`
   entrypoint whose real code is an adjacent tree defeats `artifact_digest`.
   Options: digest a declared file list, require a lockfile, or accept and
   document the limit. Recommendation: document the limit in v1 and surface
   "this entry's artifact is an interpreter" as a review-surface warning —
   pretending otherwise would be its own `readOnlyHint`.
6. **Where the host crate lives.** Recommended: a new workspace member
   `conway-plugin-host`, depended on by the `conway` facade and by nothing in
   `conway-core`/`conway-runtime`, so an internal type physically cannot
   drift onto the wire. Alternative: fold it into `conway-tools`, which
   already has the exact tokio/nix feature set.
7. **A published JSON Schema for the protocol itself.** Generating one from
   the `wire::` types would let SDKs codegen and make the golden fixtures
   machine-checkable. Real value, real maintenance cost. Out of scope here;
   the projections are shaped so it stays possible.

---

## 15. Bugs in the current tree — separate from the architecture

These are **broken now**, not merely absent. Already found and filed during
the spike (referenced, not re-filed): the registration-time `.expect()` panic
(`runtime.rs:302-304`); `PluginManifest.version` never parsed;
`PermissionMode` not `#[non_exhaustive]`; `focused_model` reaching a `Span`
unsanitized; `sanitize_rendered` duplicated; `git_branch` never refreshing;
the theme grep guard's fixed file list; a cloned repo installing pattern
grants at `Session` scope with no consent; denial-reason text reaching the
model unsanitized; a subagent's tool selector not inheriting-narrowing from
its parent; `required_host_caps` inert; `SubagentHost::steer`/`await_result`/
`cancel` accepting any `AgentId` with no descendant check. (`Plugin::on_init`
having zero call sites is being fixed as this is written.)

> **Status (2026-07-30) — this list is stale, and it is the list a reader
> trusts to say what is actually broken, which makes its staleness the most
> dangerous single thing in this document.** Four of the entries above are
> fixed:
> - **A cloned repo installing pattern grants at `Session` scope with no
>   consent** — fixed, `d917ba2`. See §7.1's and §7.4's status notes.
> - **`SubagentHost::steer`/`await_result`/`cancel` accepting any `AgentId`
>   with no descendant check** — fixed, `674bb65`. `Runtime::ensure_own_subtree`
>   now runs before all three, and each takes `caller: AgentId` as its first
>   parameter so it has something to check against
>   (`crates/conway-core/src/ports/subagent.rs`). D3 §1.3(b) and §6 carry the
>   fuller note, including a gap this fix's shape exposes on the wire side.
> - **`Plugin::on_init` having zero call sites** — the parenthetical undersold
>   this even at the time; it did not get "fixed" in the sense of wired up, it
>   was **removed**, `b17cab7`, the same reasoning D6 §11.6 already gives.
> - **`EventBus.seqs` is never pruned** (item 2 below) — partially fixed,
>   `b00b18f`. See that item's own note; the fix covers `conway_ask` modal
>   children only.

**Surfaced by this synthesis and not previously filed:**

1. **The three control-character sanitizers are not three copies of one rule
   — one of them is semantically different.** `runner.rs:413` and
   `permission_pattern.rs:317` **replace** control chars with U+FFFD;
   `crates/conway-cli/src/tui/view/header.rs:232` **drops** them. Replacing
   preserves char count, which `truncate_chars_with_ellipsis` immediately
   relies on; dropping does not. The de-duplication (F2) must therefore
   *change* `header.rs`'s behavior, not merely re-point it — which makes it a
   slightly larger change than "hoist a function," and is exactly the kind of
   drift three hand-copies produce.

2. **`EventBus.seqs` is never pruned.**
   `crates/conway-runtime/src/events.rs:36` holds
   `Mutex<HashMap<SessionId, u64>>` with an `entry().or_insert(0)` and no
   removal anywhere. A long-lived embedder creating many sessions grows it
   without bound — and it is the one mutex an entire agent tree serializes
   through, so it is also the map you least want to grow. Small, real, and
   unrelated to plugins.

   > **Status (2026-07-30), `b00b18f` — PARTIAL.** Reclaims one case: a
   > session reaching its terminal `AgentFinished` while still flagged
   > `ephemeral` (safe because `promote_agent` flips that flag before
   > emitting, per that commit's own reasoning about `resume_root` reusing
   > `SessionId` on resume). This covers `conway_ask` modal children. It does
   > **not** cover `conway_subagent` spawn or fork, which build with
   > `ephemeral: false`, so those counters still leak — recorded as a
   > residual in §12's status block and tracked at board item
   > `01KYTJFV0ZCP8E03Z4AZ8SGPA7`.

3. **The grant the UI *offers* and the grant the evaluator *honors* are now
   governed by different gates.** `suggested_rule`
   (`permission_pattern.rs:560`) takes no `RenderKind` and so applies the
   metacharacter gate unconditionally, while `matches_render` no longer does.
   After `68ea9b1`, a hand-added `read:*` in `permissions.json` works, but the
   `[p]` offer still declines for any `read` call whose rendering contains
   `{`. The behavior was flagged as a follow-up when `render_kind` landed;
   the *architectural* framing is the new part — **the offer surface and the
   evaluation surface have diverged, and an operator's fastest path to a
   grant is now narrower than the grant language.** F14.

4. **`PluginRegistry::from_plugins` returns on the first duplicate**
   (`crates/conway-runtime/src/tools/registry.rs:82-90`), so a manifest with
   ten collisions reports one and the operator fixes them one restart at a
   time. Acceptable for compiled-in plugins; inadequate for §6.12's "nobody
   wins a contested name," which needs the full collision set to announce
   both sides of every collision in one pass.

**And one non-bug worth recording so nobody "fixes" it:**
`RegisteredTool.plugin_id` (`registry.rs:23`) is currently used only for the
duplicate diagnostic and looks dead-ish. **It is the reason §7.3's guarded
handle needs no new `ToolCtx` field and no name check.** Do not remove it;
build on it.

---

## 16. The 2026-07-30 redirect — hooks, scripts, and a wider context surface

**Status: design, not implemented — same status as the rest of this
document.** This section settles four questions the redirect left open and
indexes the five places elsewhere in this document, D2, and D5's synthesis
that its decisions supersede. Written in the same voice as §11: a ledger, not
a rewrite. Nothing above §16 is edited by anything below it; every claim here
either adds a status note at its own site (already placed) or is new content
that did not exist to contradict before.

### 16.0 The four axes, and the headline finding

The operator redirected this architecture on four axes, verbatim:

1. "i want to have this primarily driven by hooks"
2. "language choice should be a user option. like in claude code, hooks can
   fire scripts which can be used as a platform"
3. "i'd like a convention for hooks to be handled by inference via subagent,
   or potentially in the calling agent (subagents could be for[k] or spawn)"
4. "We should have a wide surface of configuration that can be set via plugin
   to allow different functionality, including context manipulation. While we
   have strict rules, we should be somewhat flexible to allow plugins to
   bypass them (e.g. compaction could be implemented)."

**Axis 4 is not what it reads as.** Read alone, "allow plugins to bypass
[strict rules]" sounds like a relaxation. It is not: `ContextHook::
before_request` (`crates/conway-core/src/ports/plugin.rs`) has permitted a
hook to "edit/drop a segment ... to apply an ad hoc exclusion mirroring
WI-125's persisted `ContextMask`" since its own doc comment was written, and
is explicitly `async` "so an inference-driven hook can issue its own LLM call
to decide." **Both halves of axis 4 — context manipulation and
inference-evaluated hooks — already existed for an in-process `ContextHook`
implementor.** What axis 4 actually corrects is that `context.append/1`
(§4), the point a *remote* plugin reaches, is append-only — strictly weaker
than what an in-process implementation already holds. **A built-in (or any
in-process plugin) holding a capability a third-party remote plugin cannot
reach is the exact inversion GP-03 and P-6 forbid**, and it is this
document's own §4/§7 that drifted into it, not the shipped code. §16.5's
supersession 1 is the fix; §5.9 states the boundary that drift crossed, and
the one it did not.

Axes 1–3 are new surface, not corrections. "Primarily driven by hooks" and
"scripts as a platform" describe a lower-barrier authoring surface (a script
invoked by a small, well-known set of hook events) that GP-03 already
anticipates: "lower-barrier surfaces (declarative skills/hooks) may be
layered ON TOP of the plugin core; they are additions over the stable
interface, never replacements." The layering is literal: **a script runner is
itself a plugin** — one that implements `tool/1`/`context.hook/1`/
`permission.policy/1` like any other out-of-process plugin, whose own
implementation happens to dispatch to a configured script per event rather
than embed logic directly. §16.5 supersession 5 works through the specific
case (statusLine) this reconciles; the same shape generalizes to every hook
point without adding a second extension mechanism.

### 16.1 Q1 — How a hook declares fork vs spawn

**Decision.** A **per-hook-registration field**, not a manifest-wide flag:
`subagent_mode: Fork | Spawn`, alongside the existing per-hook `timeout_ms`/
`on_failure` (§5.2's registration shape). **Default `Spawn`.** `Fork` is a
request the plugin makes in its registration and is gated by a new
capability, `hook.fork`, following `subagent.spawn`'s exact shape (§7.3,
§11.5(b)): default off, never implied by trust, requested
(`required_host_caps`/`optional_host_caps`) and separately granted. An
operator may always refuse a requested `hook.fork` (the hook fails to
register if declared required, or is skipped with `PluginStatusChanged` if
optional — the existing capability-absence shape, §7.7) but **may never force
`Fork` on a hook that declared `Spawn`.**

**Why `Spawn` defaults.** The security asymmetry is exactly what a fork
primitive implies: a forked child inherits the *entire* ancestry context, and
an inference-evaluated hook is already reading attacker-reachable text (§9.1
— indirect injection can ride in on the segments themselves). Forking
multiplies that exposure twice: the hook's classifier sees strictly more
attacker-reachable text, and its own output (a deny or mask reason, §7.6) is
itself a channel back into the model's context, so a wider input widens what
an attacker can try to launder back out through it. This is also not a new
pattern invented for this decision — it is the shape every zero-tool judge in
this codebase already uses: `crates/conway/src/intent.rs:250`
(`mode: SubagentMode::Spawn`) and §9.1's own walkthrough. Matching the
shipped precedent for "classify from text, no ancestry needed" is not the
novelty; `Fork` as a default would have been, and it is the choice that
needed the argument.

**Why per-registration, not per-plugin.** One plugin may register more than
one hook — a `permission.policy/1` classifier needing no ancestry, and a
compaction-decision hook plausibly wanting the whole conversation to decide
what to condense. A manifest-wide flag forces the safer hook up to the
riskier default, or the legitimately-needing-context hook down to `Spawn`,
producing silently wrong compaction decisions. Scoping the field to the
registration keeps each hook's endowment exactly what its own job needs — the
narrowing self-declaration, checked, shape §3 already requires of `path_args`
and `render_kind`.

**Enforcement point.** Per P-1 ("mode restrictions on a primitive are
enforced at the trait boundary... not only at a tool callsite — a tool-layer
guard leaves the trait impl bypassable from any other caller"), the check
belongs in the **guarded handle** (§7.3, §11.2) that already gates
`subagent.spawn`: it refuses (typed `CapabilityDenied`, never a silent
coercion) a `Fork`-mode spawn from a registration lacking `hook.fork`.

**Rejected alternatives.**
- **`Fork` as the default.** Rejected on the security asymmetry above;
  nothing in the redirect's own wording asks for it either — "subagents could
  be fork or spawn" states that the primitive choice exists, not which one
  should default.
- **A single manifest-wide flag.** Rejected: forces one endowment on hooks
  that legitimately need different ones (above).
- **Enforce only at the hook's own inference call site.** Rejected per P-1,
  verbatim: a callsite-only guard is bypassable by any other caller,
  including a future remote-plugin SDK that never goes through whatever
  library enforced it client-side.
- **Let an operator widen `Spawn` → `Fork` by grant.** Rejected: the same
  shape as "never widenable, under any grant" (§7.5's list — the confinement
  root, plan mode, argument rewriting, `AllowAlways`). Forcing more context
  into a hook the author's own code never accounted for is not authority an
  operator can consent to on the plugin's behalf — the plugin's inference
  call and its output channel (§7.6) were written against a specific
  endowment, and changing that endowment out from under it makes its
  behavior on the new input unaudited by anyone, the opacity GP-10 exists to
  prevent.
- **Silent coercion (refuse `Fork`, downgrade to `Spawn`, run anyway).**
  Rejected: matches "never guessed at" (§4's failure table for
  `permission.rules/1`). A hook that structurally needs the full conversation
  to reason correctly, given only a spawned view instead, will not fail
  loudly — it will answer a different question with apparent confidence. A
  typed refusal, applying the hook's own declared `on_failure`, is the
  failure that cannot be mistaken for success.

**Consequence for existing sections.** §7.3 gains a `hook.fork` capability
row (placed). §5.2's registration fields gain `subagent_mode`. §9.1's
`mode: Spawn` choice is now stated as this decision, not merely an example
(placed).

### 16.2 Q2 — Cost and attribution for inference-evaluated hooks

**a) Bounding — confirmed, generalized.** §5.2's 60 s default `timeout_ms`
clamp, operator-raisable to a configured maximum, applies uniformly to every
inference-evaluated hook registration, not only `permission.policy/1`. The
reasoning was never permission-specific ("generous because an
inference-evaluated policy issues its own LLM call") — it is a property of
issuing an LLM call from inside a hook, which the redirect now permits for
context hooks too.

**b) Attribution — mostly already correct; one real gap.** For an in-process
hook using `SubagentHost::start` (the §9.1 shape), the spend already lands
under the ephemeral child's own `SessionId`, never folded into the calling
agent's `session_usage` — confirmed against `crates/conway/src/
session_handle.rs`: that accessor sums only `[0, head)` of the resolved
agent's *own* session records, and a spawned/forked child is a distinct
session by construction, the same reason it excludes an inherited prefix
("the PARENT's own prior conversation, not this agent's" — one level up, a
hook's own inference call is not the calling agent's conversation either).
**The gap is not double-counting; it is that a hook's ephemeral spend is
currently indistinguishable, in any rollup, from ordinary agent-tree work.**
§9.1 already tags its classifier `role: Some("guard")`; the fix is to require
every inference-evaluated hook's spawn to carry a stable role tag, and to
make cost rollups group by role, so "what did my guardrails cost me" is
answerable separately from "what did my agents cost me" without new plumbing
beyond the tag that already exists. For a **remote** hook (§13.3: no host
inference callback, ever), there is and will be no conway-side accounting at
all — the plugin spends its own credentials, exactly as §13.3 already
establishes for permission policies, generalized here to every
inference-evaluated hook kind rather than left implicit.

**c) Recursion — composes, with the mechanism named generically.** §9.1's
invariant ("policies are not consulted for a call made by an agent that a
policy spawned," checked on `PermissionCtx.agent_path`) is not actually
permission-specific in its mechanism — it holds because every
inference-evaluated hook uses the identical zero-tool judge shape
(`ephemeral: true`, `tools: Only(vec![])`, `keep_alive: false`), so the same
check, generalized from "no policy inside a policy's own subtree" to **"no
hook of any kind is invoked for a call made by an agent that hook's own
machinery spawned,"** composes without new mechanism. It should be
*implemented* once, generically, keyed on `agent_path` the same way, rather
than duplicated per hook kind — duplication is how two copies drift, the same
failure category §11.10 found in the sanitizers.

**d) The §6.8 interaction — closed by exclusion, not by a smaller
`max_total`.** The hole is real: §6.8 states "a call's deadline resets on
progress notifications" for every host-to-plugin call, and separately names
`max_total` (10 min) as a backstop scoped to tool invocation only. A
decision-bearing hook emitting progress every 2 s while never deciding is
judged *healthy* by that rule forever; `on_failure` never fires; every tool
call in the session stalls at the policy (or context-hook) step.

**Decision: exclude decision-bearing calls from the progress-reset rule
entirely.** A decision call's deadline is `timeout_ms` (clamped, §16.2a),
flat, never extended by a progress notification on that call's token. `$/
ping` connection-liveness probing still runs and still distinguishes a
wedged connection from a dead one; it is orthogonal and unaffected. A
decision-bearing hook may still emit progress (useful for `/plugins`
liveness display) — it simply carries no deadline-reset semantics for that
call.

**Rejected alternative — a separate, smaller `max_total` for decision
calls.** Considered and rejected on inspection: a `max_total` only *delays*
the same failure to a later, arbitrarily-chosen wall clock — a hook spamming
empty progress is still treated as healthy for the entire `max_total` window,
so it does not detect "never decides" any earlier or more reliably than the
flat `timeout_ms` already does. The smallest `max_total` that is actually
correct for this call class is `max_total == timeout_ms`, which is exactly
what excluding progress-reset produces, without inventing and defending a
second number an operator has to reason about alongside the first. The only
case a `max_total` genuinely earns its keep — `bash`, and remote `tool/
invoke` generally — is one where legitimate multi-minute progress is common
and expected; a permission or context decision has no such case, per the
prompt's own framing: a permission decision has no legitimate reason to take
ten minutes.

**Consequence for existing sections.** §6.8's progress-reset sentence is
scoped by a status note (placed). §9.1's "Timeout or error" paragraph gains
one sentence (placed): the clamped timeout is non-extendable, not merely a
ceiling.

### 16.3 Q3 — Determinism when two plugins edit context

§5.8's "no fixed point under two rewriters" is exactly the hazard here,
restated for a case whose conclusion is not "forbid it": **sequential
chaining** — each hook seeing the previous hook's already-edited output — is
not order-independent in general. Two hooks that both touch overlapping
content (one summarizing a segment, one dropping it) produce a different
result depending on which runs first, exactly the property §5.5 protects for
permission composition ("registration order cannot change the outcome...
priorities invite an arms race between config authored by different
parties").

**Decision: every hook is evaluated independently against the same pre-hook
payload — never against another hook's output — and their proposals are
composed by the runtime under one fixed, order-independent rule per proposal
kind.**

- **Exclusion (drop/mask) is a set union.** Each hook returns the set of
  segments/`target_seq`s it wants excluded, computed from the original
  assembly. The applied result is the union of every hook's exclusion set —
  commutative and associative by construction, the shape §5.5 already uses
  for `deny` composition. This directly answers the two named
  sub-questions: **two hooks masking the same record is not a conflict** —
  union is idempotent, `{X} ∪ {X} = {X}` — and **every hook sees the
  original payload, never an earlier hook's output**; chaining is exactly
  what is rejected below.
- **Addition (append) is unchanged**: independent, attributed
  (`Provenance::Plugin { id }`), and — restated, not changed, from §5.5 —
  declaration order is observable only here, as presentation order among
  non-conflicting appends. It was never a semantic tie-break and stays
  exactly that.
- **Content replacement of the same target by two different hooks has no
  principled order-independent merge, and none is invented.** A generic
  merge algorithm in core to reconcile two plugins' conflicting rewrites of
  one segment is precisely the "sophisticated policy" GP-11 says core must
  not own. **Decision: a same-target replace collision fails to exclusion**
  — the segment is masked rather than either party's replacement winning by
  an unstated tie-break — with both hooks named in a `PluginStatusChanged`-
  shaped diagnostic. This reuses §5.5's own shape (`deny` beats `allow`)
  translated to context: the less-informative outcome wins a genuine
  disagreement, not an arbitrary one, and it degrades toward the direction
  that is always safe to reverse (masking already is — §16.4).

**Rejected alternatives.**
- **Sequential chaining, declaration order.** Rejected as the direct source
  of order-dependence — chaining is precisely what "no fixed point under two
  rewriters" describes, restated for *n* > 1 hooks rather than resolved by
  adding more of them.
- **Priority numbers.** Rejected on §5.5's own stated reason, unchanged: they
  invite an arms race between config authored by different parties and make
  the outcome depend on who edited last.
- **Last-write-wins on a same-target collision.** Rejected: depends on an
  unstated, effectively arbitrary iteration order, and silently discards one
  party's edit with no record — the unattributed loss P-2/GP-10 exist to
  prevent.
- **A generic *n*-way content merge.** Rejected per GP-11: a merge algorithm
  is exactly the cross-cutting policy machinery core is not supposed to own;
  if two plugins need to cooperate on the same segment, that cooperation is
  the plugin authors' problem to solve, not core's to solve for them.

**Consequence for existing sections.** §5.5's order-observability sentence
and §4's `context.append/1` row both carry status notes (placed). The
successor point (§16.5) specifies this composition rule from the start
rather than retrofitting it.

### 16.4 Q4 — Where `ContextMask` gets produced

`LogRecord::ContextMask { seq, ts, target_seq, excluded }`
(`crates/conway-core/src/log.rs`) is real, persisted, append-only, and
reversible (a second record with `excluded: false` un-masks). It is consumed
by `apply_context_mask` (`crates/conway-session/src/resolver.rs`) and has
**no producer anywhere** in `conway-runtime`, `conway-tools`, or the facade —
confirmed by search across the workspace; the only other references are the
type's own definition and its mention inside `ContextHook::before_request`'s
doc comment.

**Decision: `before_request` gains the producer; no new method. Its return
contract splits into two tiers, and the runtime — never the hook — is the
sole writer to `SessionStore`.**

1. **Ephemeral exclusion** — the existing, already-shipped contract: a hook
   edits/drops a segment in its returned `ContextPayload` for *this request
   only*. Unchanged.
2. **Durable exclusion** (new) — a hook may additionally return a set of
   `target_seq`s (§16.3's union composition applies) it wants durably masked,
   with a reason string. The runtime computes the **delta** against what
   `apply_context_mask` already shows as excluded for that session and
   appends `ContextMask` records only for genuinely new exclusions or
   un-exclusions. This is what answers "every turn re-computes the same
   exclusion": the fix is diffing, once, at the site that already owns the
   write path — not a new call site.

**Why not a new method or a new wire point.** The inputs a mask decision
needs — the assembled payload, `ContextHookCtx` — are exactly `before_
request`'s existing inputs; a second point needs the same handshake, pays the
same wire promise a second time (§6's epigraph: "every type admitted to the
wire is a promise... paid by conway forever"), and reopens "which points,
when" (§1) for no new information. `SessionStore` stays off-limits to plugins
(§4 — audit-trail integrity, IPC on the hot path, a plugin crash becoming
data loss): a hook **returns a proposal**, the shape `permission.policy/1`
and `context.append/1` already use; the host is what writes.

**A concrete gap this decision surfaces, not solved by it.**
`PromptSegment.id` is a `SegmentId`, generated fresh per assembly
(`PromptSegment::new`) — **not** the `LogSeq` `ContextMask::target_seq`
needs. Checking `Provenance`'s ten variants (`crates/conway-core/src/
provenance.rs`): only `Inherited { seq_range }` and `ParentSteer
{ parent_seq }` carry any `LogSeq` at all; `UserPrompt`, `AgentDef`, `Skill`,
`ToolRegistry`, `ForkDirective`, `ToolResult`, `SystemNote`, `MergedAsk` do
not. **A hook cannot express "durably mask this segment" today, because the
payload handed to it does not reliably carry the log-seq identity a mask
needs to target.** This is implementation work the decision *requires*:
`PromptSegment` (or a host-side companion map handed alongside
`ContextPayload`) needs to expose the originating `target_seq` per segment
where one exists — most `Volatile`-tier, log-derived segments have one; a
handful of synthetic segments may not, and "no `target_seq` available" must
be a distinct, representable state, not an error. Named as a follow-on in the
report, not solved here.

**Also worth naming: `LogRecord::ContextMask` has no `Provenance`/
attribution field.** Every other meaningful `LogRecord` variant (`UserTurn`,
`Assistant`, `ToolCallRecord`, `ParentSteer`, `SystemNote`) carries `prov:
Provenance`; `ContextMask` does not. That was consistent while the only
producer was a human/operator action (WI-125's original design); it stops
being consistent the moment a plugin can propose one — "who masked what, and
when" (the record's own doc comment) needs "and *by what*," per P-2/GP-10,
once the answer can be a third party's code rather than the harness itself.

**Rejected alternatives.**
- **A distinct RPC point / trait method for mask proposals.** Rejected above
  — no new information, doubles the wire promise, reopens the surface-area
  question.
- **Give hooks direct `SessionStore` access.** Rejected outright, per §4,
  verbatim.
- **No dedup — persist a mask every turn regardless of change.** Rejected:
  this *is* the named failure ("every turn re-computes the same
  exclusion"), and it works against GP-10's actual goal — an inspectable
  log, not a noisy one. A hook returning the same answer every turn was never
  non-deterministic; the missing piece was diffing, not determinism.
- **Single-tier (always durable, no ephemeral option).** Rejected: forces
  every existing, already-shipped ad hoc `before_request` edit into a
  permanent log entry, a strictly larger behavior change than the redirect
  asks for — and the existing doc comment ("an ad hoc exclusion mirroring
  WI-125's persisted `ContextMask`") already treats the ad hoc form as a
  deliberate lightweight mirror of the persisted one, implying both were
  always meant to coexist, not merge into one.

### 16.5 The five supersessions — index

Each is marked additively at its own location; this table is a map, not a
duplicate of the reasoning it points at.

| # | What's superseded | Where marked | Replacement / correction |
|---|---|---|---|
| 1 | `context.append/1`, append-only | §4 (table), §5.8, §7.3, §7.6 rule 4, §12 F28, §5.5 | `context.hook/1`, specified below |
| 2 | §5.8/§13.4's "no rewriting, ever, by anything" read as covering context | §5.8, §13.4 | Scoped explicitly to arguments and permission verdicts; context is a third value class (§5.9) |
| 3 | D2 §1's R1 ("one authority per value") read as binding context | `d2-extension-points.md` top banner | Context is not an authority; R1 governs arguments and verdicts (§5.9) |
| 4 | §13.6 "no compaction events" | §13.6 | No compaction *policy* still ships; a producer path for the persisted exclusion now does (§16.4) |
| 5 | §8.3's shell-command status-variable closure, and the v0.3.0 deferral it closed | §8.3 | The scripts axis answers the original trust objection (`d917ba2`'s digest-keyed trust decisions); the script runner is itself a plugin (GP-03), so the surface layers on top of, not beside, the one extension mechanism |

**`context.hook/1` — the named replacement for supersession 1, specified.**
Mirrors `ContextHook::before_request`'s shape at the wire (§6.1's projection
discipline): receives the projected `ContextHookCtx` + `ContextPayload`;
returns `{ appends: [...], excludes: [segment identifiers], durable_excludes:
[{ target_seq, reason }] }`, composed across every hook — in-process and
remote alike, per GP-03/P-6, no point a built-in reaches that a third party
cannot — under §16.3's union rule and §16.4's ephemeral/durable split.
`context.append/1` is retired the way `Plugin::on_init` was (§11.6): not
wired up differently, removed, because a point strictly weaker than the
in-process capability it was supposed to give parity with is worse than
absent — it is a promise of parity the design did not keep.
