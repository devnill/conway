# Architecture: conway

This is the system overview for conway: what it is, how the workspace is
laid out, the primitives it's built from, and the data flow of one turn.
For task-oriented documentation on using conway, see
[`docs/README.md`](docs/README.md); this page is the level below that —
how the pieces fit together, for a reader who wants the whole-system
picture before diving into a specific crate's source.

---

## 1. What conway is

conway is an embeddable Rust agent harness for agentic coding: it runs
LLM-driven agents that call tools, fork and spawn child agents, and route
across multiple model backends, with explicit permissions, an append-only
session log, and no hidden context manipulation.

It ships as **one library** (`conway`, the facade crate) that serves **three
consumption modes equally** — there is no mode that is "more native" than the
others, because all three are written against the same public API:

- **Embeddable Rust library.** A host application (for example an IDE)
  depends on the `conway` crate directly: `ConwayBuilder` assembles a
  `Conway`, which hands out `SessionHandle`s. Fully async, event-streamed, no
  process boundary.
- **Interactive TUI.** `conway-cli`'s terminal shell — a single-column,
  copy-paste-friendly conversation view, a live `/`-command palette, and an
  on-demand agent-tree panel. It is a thin consumer of the same facade API
  the library embedder uses.
- **One-shot `-p` / `--print`.** A scriptable, non-interactive mode: prompt
  from argv or stdin, streamed output, `--output-format text|json|jsonl`,
  strict stdout purity (only model output on stdout, diagnostics on stderr),
  stable exit codes. Because one-shot mode cannot prompt an operator for
  permission, it fails closed: tools are denied unless explicitly named via
  `--allowed-tools`.

## 2. The workspace: 7 crates, ports-and-adapters

conway is a Cargo workspace of seven crates laid out as ports-and-adapters
(hexagonal architecture). `conway-core` defines the domain types and every
port trait; every other crate is either an adapter implementing those ports,
or a consumer wiring adapters together.

```
conway-core        domain types + port traits. No tokio-net. No I/O *except*
                    `containment.rs` (see below). Also
                    where `MinimalRouter` lives — the config-only `Router`
                    a default build resolves roles with (§3.3); there is no
                    dedicated routing crate, and no dedicated backend-adapter
                    crate, in this fixed layout.
conway-testkit     Test doubles for every `conway-core` port trait
                    (`FakeBackend`, `FakeStore`, `FakeGate`, `FakeRouter`,
                    `FakeHealth`, `FakeSubagentHost`, `CollectingEventSink`).
                    Depends on `conway-core`, never the reverse. Not linked by
                    default: `conway`'s facade forwards it behind its own
                    `testkit` feature, so a crate depending only on `conway`
                    can reach it by opting in, the same way this workspace's
                    own test suites always could.
conway-session     The append-only session log; transcript/prefix resolution.
conway-tools       The tool/plugin registry and built-in tools.
conway-runtime     The agent loop, context assembly, fork/spawn orchestration.
conway             The public facade: ConwayBuilder, Conway, SessionHandle.
conway-cli         The `conway` binary: one-shot mode and the TUI.
```

**One forward declaration remains in that first row**, labeled at its
declaration site in `conway-core` itself:

- **`conway-core` does I/O today, in exactly one file.**
  `crates/conway-core/src/containment.rs` calls `std::fs::canonicalize` when
  constructing a `CanonicalRoot` and again in its walk-up loop — a
  symlink-aware containment check cannot be pure computation. Exactly one
  file, pinned by a CI guard
  (`crates/conway/tests/architecture_invariants.rs`, T2) that fails if a
  second one starts. It closes when confinement moves into `conway.fs`, per
  §3.4 and `PHILOSOPHY.md` §1; that file's own module doc records the four
  questions that has to answer first.

(A second forward declaration used to sit here: `conway-core` shipped test
doubles behind `feature = "fakes"`, unreachable outside this workspace
because the facade enabled that feature only under `[dev-dependencies]`.
Board item 01KZVYWNA24EYMPVW3NPGBW51M closed it by extracting the doubles
into `conway-testkit`, the row added above, and made them reachable through
`conway`'s own forwarded `testkit` feature.)

This is the fixed core layout; it does not include the first-party plugin
tier (§2b), whose crate count grows independently of it — notably
`conway-plugin-routing` (install id `conway.routing`), the capability-/
health-filtering `DeclarativeRouter` engine that used to be a mandatory
crate in this table (`conway-routing`) until moved it out to the plugin tier, and
`conway-plugin-backends`, the Anthropic-native and OpenAI-compatible
provider adapters (OpenAI, Ollama, vLLM/Hermes, LM Studio, llama.cpp server
dialects) that used to be the mandatory `conway-backends` crate above until moved it out the same way.

Dependency direction is **strictly downward**; `conway-core` depends on
nothing else in the workspace. This is the **default build** — no routing
plugin installed, no backend plugin linked by `conway` itself, and no edge
from `conway` or `conway-runtime` to any routing or backend-adapter crate,
because there isn't one:

```
conway-cli ──> conway ──> conway-runtime ──┬─> conway-session ─┐
                                            └─> conway-tools ───┴─> conway-core
```

Installing the routing plugin (`plugins.install = ["conway.routing"]`,
§3.3) adds exactly **one** edge to this graph, and it touches neither
`conway` nor `conway-runtime` — only `conway-cli` gains it, because linking
the plugin crate to populate `first_party_plugins::router_bundle()` is what
makes `RoutingRouterFactory` nameable in `plugins.install` at all. The two
provider-adapter dialects are the same shape, one layer over: `conway-cli`
links `conway-plugin-backends` to populate `first_party_plugins::
backend_bundle()`, but — unlike routing — needs no `plugins.install` entry
at all to attach both (`conway::config::schema::PluginsConfig::
default_backends`'s own doc explains why a backend, unlike every other
first-party mechanism, ships attached by default):

```
conway-cli ──> conway-plugin-routing ──> conway-core
conway-cli ──> conway-plugin-backends ─> conway-core
```

`conway-cli` may carry these edges while remaining forbidden from depending
on the internal engine crates above (`conway-runtime`, `conway-session`,
`conway-core`, `conway-tools` — machine-checked by `no_forbidden_deps`,
`crates/conway-cli/tests/cli_surface.rs`, whose FORBIDDEN list keeps the two
retired crate names `conway-routing`/`conway-backends` as dead strings)
because a first-party **plugin** crate is a different tier entirely (§2b):
it is meant to be linked by exactly one binary, the same way a third party
would link it, not an internal implementation detail `conway` itself
assembles — that list was never meant to, and does not, cover the plugin
tier. A library embedder wanting the same router or backend adapters links
`conway-plugin-routing`/`conway-plugin-backends` directly and calls
`ConwayBuilder::with_router_factory`/`with_backend_factory`; no edge through
`conway-cli` is involved at all.

There are no cycles. The one place a cycle would naturally appear — the
fork/spawn *tool*, which lives in `conway-tools`, needing to drive the
runtime that lives above it — is broken by the **`SubagentHost` port**:
defined in `conway-core`, implemented by `conway-runtime`, and handed to
tools through `ToolCtx`. A tool never depends on `conway-runtime` directly;
it depends only on the trait object it's handed.

This shape exists so:

- An embedder can depend on `conway` alone and never pull in `clap` or a TUI
  renderer.
- A third-party plugin author depends on `conway-core` — a small, slow-moving
  crate with strict semver discipline — not on the whole harness.
- **Which backend you talk to is runtime configuration, not a build-time
  choice.** `conway` itself depends on neither adapter (: `conway-plugin-backends` is a first-party
  plugin, §2b); the shipped `conway-cli` binary links it and attaches both
  `BackendFactory`s by default (`PluginsConfig::default_backends`), so the
  harness still ships adapters for the common API flavours (Anthropic
  native, OpenAI-compatible) out of the box, and a `backends.<id>.kind`
  entry in settings selects among whatever kinds are registered — there is
  no `anthropic`/`openai-compat` cargo feature to recompile for, and no
  closed built-in set: `kind` is an open name resolved against every
  registered `BackendFactory`.
- Every port trait has a fake/test-double implementation available behind a
  feature flag, so the runtime and tools are testable end-to-end with zero
  network.

## 2b. The first-party plugin tier

A second, open-ended set of crates sits alongside the seven above: plugins
written and shipped in this repository but never installed unless asked for
(`PHILOSOPHY.md`'s "First-party plugins, and why they are not defaults").
Members today:

- **`crates/conway-plugin-skeleton`** — the first, and a worked example (one
  `skeleton_ping` tool) proving the mechanism rather than a real capability.
- **`crates/conway-plugin-routing`** — the capability-/health-filtering
  `DeclarativeRouter` engine `conway` itself used to compile in
  unconditionally, relocated here and installed by naming its
  `RouterFactory::id()` (`ROUTER_ID = "conway.routing"`) in `plugins.install`.
  See §3.3 for exactly what it adds over the default `MinimalRouter`.
- **`crates/conway-plugin-backends`** — the Anthropic-native and
  OpenAI-compatible provider adapters. The one member attached by default
  (`PluginsConfig::default_backends`), because a harness that cannot reach a
  model is inert rather than unopinionated.
- **`crates/conway-plugin-history`** — `/conway.history.rewind <seq>`, which
  forks the calling session at a sequence number and hands the TUI the child.
  It exists to prove that `/rewind`-class features genuinely are plugins.
- **`crates/conway-plugin-stepguard`** — repeated-tool-call detection, which
  the agent loop used to carry unconditionally. It is the first consumer of
  the `ToolObserver` port (§3.9), and the reason that port exists:
  `PHILOSOPHY.md` §6 leaves loop intervention to the operator "including
  writing none", which is only true once declining it is possible.
- **`crates/conway-plugin-skills`** (`conway.skills`) — progressive skill
  disclosure: a `ContextHook` narrows full-body `Provenance::Skill` segments
  to a one-line index, and a companion `read_skill` tool returns the full
  body on demand.
- **`crates/conway-plugin-memory`** (`conway.memory`) — a mutable
  `MemoryStore` the model can write to in its own words, injected into
  context by a `ContextHook`. A rework of an earlier label-based curator;
  see the crate's own module doc for why that design was replaced.
- **`crates/conway-plugin-path`** (`conway.path`) — the `compose_context_path`
  tool a model calls to compose what a session sends as context on its next
  turn: bring specific records in from another session, leave specific
  records of this session's own history out, or both. Refuses (never
  silently patches) a composition that would strand a tool call or its
  result.
- **`crates/conway-plugin-discover`** (`conway.discover`) — the
  `search_sessions` tool that feeds `conway.path` immediately above: finds a
  session or record the model does not already hold a reference to.
  Metadata-only by default; a `text` argument turns it into a bounded
  content scan. Meant to be installed alongside `conway.path`, though
  nothing enforces that pairing.
- **`crates/conway-plugin-trim`** (`conway.trim`) — a `Curator` that omits
  tool call/result round-trips older than a configurable turn window: the
  smallest honest curator over the path/curation machinery above, which had
  no production consumer before it.
- **`crates/conway-plugin-names`** (`conway.names`) — operator-chosen,
  renameable names for agents: three commands
  (`/conway.names.rename`/`.unname`/`.list`) over a small `AgentNames`
  store shared with the TUI's own name reads, so a rename is visible to
  `/agents` and to `/steer` immediately, with no reload.
- **`crates/conway-plugin-subprocess`** — the out-of-process plugin host: an
  external program named in `[plugins].subprocess[]` is spawned and speaks
  conway's own wire protocol (`tool/1`, `permission.policy/1`, `observe/1`,
  `status.declare/1`) over a persistent NDJSON channel, gaining a tool the
  binary was never compiled with.
- **`crates/conway-plugin-mcp`** — an MCP-over-stdio *client*: an external
  program named in `[plugins].mcp[]` is spawned as an MCP server, and every
  tool it declares over `tools/list` attaches as an ordinary
  `conway::plugin::Tool`. A sibling transport to the subprocess host above,
  not a layering on it — the two speak different wire protocols.

Compaction remains separate, later work — the sole member of this list not
yet written; see `PHILOSOPHY.md` §6's own "Where the tree is today" note.

The layout is one crate per plugin, under `crates/` like everything else.
A single crate holding several would couple members that are meant to be
independently installable, and would defer a naming question a growing tier
has to answer anyway. A directory outside `crates/` was the other candidate
and is worse than it looks: `cargo test --workspace` walks `crates/*`, so
members would drop out of the suite silently — a coverage gap that goes
unnoticed precisely because nothing fails.

Most such crates depend on `conway` (the facade) — the identical public
surface a third-party plugin author gets, and how `conway-plugin-skeleton`
does it. `conway-plugin-routing` is the deliberate exception: it implements
`Router`/`HealthRegistry`/`RoutingExplainer`, surfaces `conway::plugin`'s own
module doc excludes from the third-party `Plugin`/`Tool` tier, so it depends
on `conway-core` directly instead — `RouterFactory` (§3.3) is a separate,
root-level installable mechanism built for exactly that: a router kind
needs the full routing/capability domain the facade does not curate down
to. Either way, `conway` itself depends on none of the plugin tier, in
either direction, ever. That asymmetry is the whole point: a first-party
plugin is written and reviewed here, but from the facade's own point of
view it is indistinguishable from a plugin nobody at this project wrote.
The one place a first-party plugin crate IS linked into a shipped binary is
`conway-cli` (`src/first_party_plugins.rs`), behind a config key
(`plugins.install`) distinct from the built-in selection §2 describes —
a `Plugin` via `bundle()`/`ConwayBuilder::with_plugin`, or, for
`conway-plugin-routing` specifically, a `RouterFactory` via
`router_bundle()`/`ConwayBuilder::with_router_factory`. A library embedder
instead links the crate directly and calls the matching `ConwayBuilder`
method itself. See `docs/embedding.md`'s "First-party plugin tier" section
for the full mechanism.

## 3. Core primitives

### 3.1 Fork vs. spawn

conway has exactly two ways to create a child agent, and they are genuinely
distinct primitives — there is no partial-inheritance mode.

- **Fork** — a child inherits the parent's *entire* effective context at the
  fork point: a literal, immutable, ordered prefix, frozen at that sequence
  number, plus an appended directive. This is O(1) on disk: forking writes
  one header line and copies zero records. Siblings forked at the same point
  share one memoized in-memory prefix allocation.
- **Spawn** — a clean-slate child. It inherits no parent context. Naming an
  agent definition (a system prompt / skill) is optional, not required:
  omitting one means the child inherits the spawning session's own role, and
  transitively its model routing, the same way a roleless fork inherits its
  forker's role.

After a fork, the two sessions are independent append-only logs: prompting
the parent never reaches the child and vice versa. Cross-agent communication
after that point is explicit — steer messages (applied at turn boundaries)
and terminal results, never implicit context bleed.

The ephemeral **`/ask`** primitive (`SessionHandle::ask`, §3.6) is built
directly on fork: it starts a new agent from the current head, drives one turn
on it, and returns a handle to that child — the parent's own transcript is
never touched.

### 3.2 The append-only session log and context assembly

Every turn, tool call, and tool result is written to an append-only session
log (`conway-session`, `JsonlSessionStore`) — one JSONL file per session,
never mutated or deleted in place, crash-tolerant on read. The log is the
source of truth; in-memory agent state is a cache over it.

Context for a turn is assembled **deterministically** from the log by
`conway-runtime`'s `ContextBuilder`, in three conceptual layers:

- **Records** — the raw, persisted `LogRecord`s (user turns, assistant turns,
  tool results, fork directives, parent steers, ...), each carrying a typed
  `Provenance` describing where it came from.
- **Segments** — records resolved into an ordered `Vec<PromptSegment>` in a
  fixed order (static content first: system prompt, tool schemas; then the
  inherited prefix for a fork child; then the turn's own volatile records).
  This fixed ordering is what makes cache hits possible for
  implicit-prefix-caching backends — ordering is an economics mechanism, not
  a correctness one.
- **Wire messages** — segments translated into the concrete request shape a
  given `Backend` adapter expects, with cache hints attached where the
  backend's `CacheMode` supports them.

Fork inheritance and context assembly compose: a fork child's "inherited"
prefix is resolved once at fork time by `conway-session`'s
`TranscriptResolver` (walking the ancestry chain, bounded at the fork
sequence) and is then simply replayed into every one of the child's turns
unchanged.

### 3.3 Routing: `MinimalRouter` by default, an installable plugin for the rest

**Content-blindness is a `Router`-port guarantee, not a routing-crate one.**
`Router::resolve` (`conway_core::ports::routing`) takes a `RouteRequest`
that carries no prompt-bearing field at all, for any implementation of the
port — the guarantee holds **by construction**, not by convention or an
audit of one crate's code paths: there is no code path reachable through
`Router` that can see request text, because the type handed to it cannot
carry any. That is not a claim that conway can never route on content — it
makes the router's decision a pure function of role, capabilities, and
health, with content-aware policy living one layer up, in role
*selection*: the caller picks the role, whether that is the host, an agent
definition, or a plugin spawning with a role it chose. `ContextHook` cannot
do this today — `ContextPayload` carries only `segments` and `tools`, and
`AgentSpec::role` is fixed before `Router::resolve` runs — so a hook reads
the routing outcome (`ContextHookCtx::model`) rather than steering it.
Widening that surface is future work.

**A default build** resolves a role alias (`"planner"`, `"coder"`,
`"fast"`, ...) with `conway_core::routing::MinimalRouter`: a config-only
resolver that walks `roles.<alias>.chain` in order — the first entry
carries `RoutingReason::AliasPrimary`, every entry after it
`RoutingReason::Fallback` — paired with `AlwaysClosedHealthRegistry`,
which reports every breaker `Closed` and records nothing. Stated plainly,
what a default build does **NOT** do: no capability filtering, no health
tracking, no circuit breaking — every configured candidate is treated as
eligible, so an unregistered model or a dead endpoint is discovered only
when the request is actually attempted, not skipped in advance.

Capability matching (each candidate's declared `Capabilities` — context
window, tool-calling support, reasoning — checked against the turn's
requirements) and a circuit breaker per endpoint — **transport**, fed by
connection failures — are what installing `conway-plugin-routing` (§2b)
adds, not something a default build has. (A second, independent **probe**
breaker fed by a periodic background liveness check used to be planned
alongside it; it was retired rather than wired — — because the transport breaker alone already
detects a recovered endpoint on the next real request, so the prober would
only have shaved latency off that one request, an optimization with no
measured baseline to justify it.) `plugins.install = ["conway.routing"]`
resolves
`RoutingRouterFactory` (`ROUTER_ID = "conway.routing"`) from
`conway-cli`'s `router_bundle()`, which builds a `DeclarativeRouter` plus a
real `BreakerRegistry` in `MinimalRouter`/`AlwaysClosedHealthRegistry`'s
place. Once installed, when the primary candidate for a role is
unavailable, the router walks the ordered fallback chain, recording health
observations as it goes. Selection precedence inside `ConwayBuilder::build`
is exact: an injected `with_router` wins unconditionally; else a registered
`with_router_factory` wins; else `MinimalRouter`. See
[`docs/routing.md`](docs/routing.md) for the full mechanism, including
exactly what each configuration does and does not do.

`conway routes explain` answers in **both** configurations: every candidate
either resolver returns carries a `RoutingReason`. Absent the plugin, the
answer is honestly degenerate (`capabilities: None`, every breaker
`Closed`, one entry per configured chain candidate) rather than invented —
installed, it is capability- and health-filtered. `ExplainReport` and the
field types it's built from (`ExplainEntry`, `EntryOutcome`,
`CapabilitySummary`, `BreakerSnapshot`) live in `conway_core::routing`, so
producing one never requires depending on `conway-plugin-routing`'s own
filtering logic.

### 3.4 Tools as plugins, behind a permission gate

Every tool — built-in or third-party — implements the same `Plugin`/`Tool`
traits from `conway-core`; nothing about the built-ins (filesystem, shell,
subagent, report) is privileged. A `Plugin` declares the `Tool`s it provides;
each `Tool` declares its JSON-schema `ToolSpec`, a `ToolCategory`, and a
`PermissionClass`.

Tool **announcement** — what the model is told exists and may be called — is
a distinct concern from tool **execution** — what is actually allowed to
run. Every proposed tool call passes through an explicit `PermissionGate`
(`AllowOnce`, `AllowAlways{scope}`, `Deny`, or `DenyWithFeedback` — the last
of which lets the model see and adapt to a denial reason rather than just
failing silently). The gate is always implemented by the consumer; conway
ships an allow-list gate (used by `-p`), a deny-all gate, and an interactive
prompting gate (the TUI).

A spawned child may additionally carry a **confinement root**
(`SubagentSpec::root`). Enforcement is split by `Tool::path_args`. For a
`PathArgs::Named` tool — `conway.fs`'s `read`, `write`, `edit`, `cd`, `glob`,
and `grep` — the root is enforced inside the plugin itself, open-relative
(`conway_tools::fs::beneath`, `cap_std::fs::Dir`): `resolve` returns a path
meaningful only relative to a `Dir` opened at the canonical root, and the
`Dir`'s own methods re-verify every path component at call time, so the
containment check and the filesystem open are the same syscall sequence —
a symlink created inside the root between a check and an open can no longer
defeat it, closing a TOCTOU gap an earlier, harness-only check left open.
`PermissionBroker::check_root` no longer walks a `Named` tool's declared
path arguments at all; that call still reaches the gate, the `AllowAlways`
cache, pattern grants, and `AutoAllow` mode first, and `conway.fs`'s own
check runs after, refusing the call regardless of what the gate decided.

`glob` and `grep` get a real but weaker version of the same guarantee: they
validate their search *root* through `resolve` plus a probe `Dir::open_dir`,
closing the window on the root argument itself, but the walk that follows
(`crate::fs::walk_files`, the `ignore` crate) does not integrate with `Dir`
— it relies on `WalkBuilder`'s `follow_links(false)` default instead. See
`beneath.rs`'s own "What this does NOT close" section for the reasoning.

`PathArgs::Unconfinable` is the one case the broker still checks directly,
because no plugin-level root check can reach it: `bash`'s `cwd` argument is
declared `checkable` and remains root-checked by `PermissionBroker::check_root`
exactly as before; its `command` string is declared unconfinable, not
enforced (the string runs verbatim via `/bin/bash -c`, and the broker cannot
parse shell), so **an agent holding `bash` is not confined by root alone.**
The composition that IS a real guarantee is root *plus* a tool set that
excludes `bash` (`SubagentSpec::tools`/`ToolSelector`, narrowing a child's
announced tools at spawn) — a confined child is a child spawned without
`bash`. See [`docs/permissions.md`](docs/permissions.md) and
[`docs/tools.md`](docs/tools.md) for the full mechanism and these limits
stated in full.

### 3.5 `ContextHook` — pluggable context and tool curation

`ContextHook` is a port trait (`conway-core`) invoked once per assembled
request, before it is routed: `before_request` receives the segments and the
announced tool set together as one `ContextPayload` and may edit or drop
segments, narrow the announced tools, or leave the payload unchanged. An
optional `on_overflow` method fires only when the already-hooked payload
still doesn't fit the routed model's context window, giving the hook one more
chance to shrink it before the runtime raises a hard error.

**No hook is registered by default, and conway-core ships no curation
policy.** With `LoopDeps::context_hook` set to `None` — the default — the
runtime never invokes anything, not even a no-op pass-through; behavior is
byte-for-byte what it was before the hook existed. There is no automatic
compaction anywhere in the harness. A consumer that wants masking,
system-prompt instrumentation, tool-announcement narrowing, or
overflow-time summarization supplies its own hook — which may be a pure
script or may itself issue an LLM call, since `before_request` is async.

**A hook's returned payload is re-checked for tool-call/result coherence at
one seam, not at each call site.** `ContextBuilder::build` already guarantees
a rendered context never carries a tool call without its answering result;
nothing re-checked that guarantee once a hook had edited the payload, so a
hook dropping half a pair shipped a request every provider rejects outright.
A hook enters the runtime through exactly one place, `Runtime::
set_context_hook`, which wraps whatever it is given in `GuardedContextHook`:
`LoopDeps::context_hook` holds `Arc<GuardedContextHook>`, never a bare
`Arc<dyn ContextHook>`, so there is no unwrapped path for a call site to use
by mistake. The wrapper's `before_request` and `on_overflow` delegate to the
wrapped hook and then run the same coherence check on whatever it returns —
covering both existing methods and any the trait gains later — and refuse
with a typed error naming the orphaned `call_id`s and which method produced
them, rather than repairing the payload itself: a hook's edit is a deliberate
act, and guessing which half of an orphaned pair to drop would be guessing at
intent the hook never stated
(`crates/conway-runtime/src/context/hook_guard.rs`).

A related, narrower mechanism is the **out-of-context record mask**
(`LogRecord::ContextMask`): a persisted, reversible overlay naming another
record in the same session as excluded. Its only effect is on
**fork-prefix resolution** — `conway-session`'s `TranscriptResolver`
applies it when computing what a *new* fork child inherits. It does not
touch the owning session's own future turns: a session's own context
assembly reads its own records directly, unfiltered by any mask on them.
No **built-in** surface constructs a `ContextMask` record.
`conway_plugin_history`'s `/conway.history.mask <seq> [unmask]` command
does, through `Conway::mask_record` — but that is an opt-in first-party
**plugin** (`[plugins].install = ["conway.history"]`), not core and not a
built-in. With the plugin uninstalled, nothing in core or the CLI can
append one, so "not reachable through any built-in surface" stays
literally true — precisely because a plugin is not a built-in.

### 3.6 Keep-alive sessions and `/ask`

By default, a session's root agent task terminates once its first turn
reaches a natural `Completed` state; a second `SessionHandle::prompt` call on
the same handle would silently run no turn. **Keep-alive** (`SessionSpec::
keep_alive = true`) opts a session out of that: the root agent idles,
awaiting its next prompt, instead of terminating — this is what lets the TUI
run a second chat message in the same process. A keep-alive session emits
exactly one `AgentFinished` for its whole lifetime (on cancel, deadline, or
budget exhaustion), not one per turn.

**`/ask`** (`SessionHandle::ask`) is the ephemeral side-question primitive
built on fork (§3.1): it starts a new agent from the current head, whose own
log is marked `SessionMeta::ephemeral = true`, then drives one turn on it
with the given text. The child inherits the full context and tool set, so
tool calls it makes are real, but its transcript never touches the parent's
session and is excluded from default session listings — a throwaway
question never pollutes the live conversation.

### 3.7 Reasoning support at the wire layer

conway carries reasoning/extended-thinking as a first-class, per-dialect
wire concern in `conway-plugin-backends`: extended-thinking budget and
reasoning-effort request parameters are mapped per backend dialect, and for
Anthropic specifically, thinking-block **signature round-trip** is preserved
across multi-turn tool-call loops (including `redacted_thinking` blocks),
since a dropped or malformed signature invalidates the request. This is
wire-layer plumbing, not policy — whether/how much a model reasons is a
routing/config decision, not something the harness imposes.

### 3.8 The extension model

The same shape recurs at every extension point: `conway-core` defines a
port, the core ships mechanism behind it, and the policy is the consumer's
to write.

- **Tools** (§3.4) are plugins. The filesystem, shell, subagent, and report
  tools implement the same `Plugin`/`Tool` traits a third party would, so
  there is nothing a built-in can do that a third-party plugin cannot.
- **Permissions** (§3.4) are a `PermissionGate` the consumer supplies.
  conway ships an allow-list, a deny-all, and an interactive gate, and
  privileges none of them.
- **Context curation** (§3.5) is a `ContextHook`. None is registered by
  default and there is no compaction anywhere in the harness, so a consumer
  that wants condensing, masking, or tool narrowing writes that policy
  itself.
- **Routing** (§3.3) resolves roles; selecting the role belongs to the
  caller, which is where content-aware policy lives.

`conway-core` is deliberately small and slow-moving under strict semver
discipline (§2), because it is the surface third-party plugins depend on.
Keeping opinions out of it is what keeps that surface stable: a policy baked
into the core becomes a behavior every extension has to accommodate or work
around, and those accumulate.

The reach of these ports is not uniform yet, and §3.3 records one current
limit: a `ContextHook` can read the routing outcome but cannot steer it.
Widening port surfaces where they are narrower than the model implies is
ongoing work, and the tool-facing types are serialization-ready, which
leaves cheaper plugin hosts (subprocess, WASM) as a layered addition rather
than a redesign.

### 3.9 `ToolObserver` — loop-intervention policy, outside the core

`ToolObserver` is a port (`conway-core`) invoked once per finished tool call,
after its result is durable and before the next turn's context is assembled.
It receives the call — including the `arguments`, which the `post_tool_use`
payload does not carry, and which any "has this exact call happened before"
policy needs — and returns an `ObserverAnswer` describing what to record. The
runtime performs it.

**Declare an effect, do not perform one.** An observer is handed no
`SessionStore`, no event bus, and no agent handle; the only thing it can cause
is a `SystemNote` appended to the session it was called about, plus events
fired through the same `PluginEventHandle` a plugin's own tools already use
(so under its own `plugin_id.` namespace, never a core event and never another
plugin's). This is the same shape `ContextHook` (returns an edited payload)
and `CommandOutcome::ForkSession` (returns a request to fork) already use: the
smallest capability that does the job, bounded by the return type rather than
by the plugin's restraint.

**Observation only, fail-open.** The call has already run its side effects, so
an observer cannot deny, cancel, or alter it, and a panicking observer is
contained rather than failing the batch — the same posture `post_tool_use`
takes, for the same reason. Policy that wants to *stop* something wants
`PermissionGate` or a `pre_tool_use` hook, both of which run beforehand.

**No observer is registered by default**, and with none installed the loop's
observer pass does not execute at all. That emptiness is the design rather
than an optimization: `PHILOSOPHY.md` §6 leaves loop intervention to the
operator "including writing none", which is not a real option while the core
ships one. `conway-plugin-stepguard` (§2b) is the first consumer, and carries
the repeated-call detection the agent loop used to hold itself.

## 4. Data flow of one turn

```
prompt
  │
  ▼
persist            Runtime appends LogRecord::UserTurn to the session log
  │                 (persist-before-act: the log is truth, memory is cache)
  ▼
context build       ContextBuilder resolves records -> ordered, provenance-
                     tagged PromptSegments (fixed order: static, then
                     inherited prefix, then volatile)
  │
  ▼
route                Router::resolve(role, required capabilities, est_tokens)
                     -> an ordered candidate list (§3.3: capability- and
                     breaker-aware only when the routing plugin is
                     installed; a default build's `MinimalRouter` treats
                     every configured candidate as eligible)
  │
  ▼
attempt / backend call   Walk the candidate list; call Backend::generate or
                     ::stream (choice depends on the model's declared
                     tool-calling support); record health observations
  │
  ▼
tool run             Proposed tool calls pass PermissionGate; permitted
                     calls run concurrently, bounded, cancellable
  │
  ▼
append records       Assistant turn + tool results are appended to the log
  │
  ▼
next turn            Loop back to context build until the model returns no
                     tool calls, or a budget (steps/deadline/tokens) trips
```

Steer messages from a parent agent are only ever applied at a **turn
boundary** — the mailbox is drained after a turn's tool batch completes and
before the next turn's context is assembled. No code path injects into a
context mid-generation.

## 5. Where to go next

- [`docs/README.md`](docs/README.md) indexes conway's task-oriented
  documentation — installing conway, driving it interactively or from a
  script, and embedding it as a library.
- [`PHILOSOPHY.md`](PHILOSOPHY.md) covers how these primitives are meant to be
  used: how to shape an agent tree, when to fork versus spawn, and what conway
  deliberately leaves to you.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) is the discipline applied when changing
  any of it: what "done" means, and the rules a change is held to.
- Each crate's own source carries its implementation-level documentation;
  `cargo doc --workspace --no-deps --open` builds it.
