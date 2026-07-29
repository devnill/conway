# conway

`conway` is the embeddable facade: it assembles [`conway-core`](conway-core.md)'s
ports and domain types together with the concrete
[`conway-runtime`](conway-runtime.md), [`conway-backends`](conway-backends.md),
[`conway-session`](conway-session.md), [`conway-routing`](conway-routing.md),
and [`conway-tools`](conway-tools.md) implementations behind one stable
public API. This is the crate an embedder (for example an IDE) depends on
directly, and the crate [`conway-cli`](conway-cli.md) is built on. See
[`/ARCHITECTURE.md §1`](/ARCHITECTURE.md) for how the three consumption
modes (embeddable library, TUI, one-shot `-p`) all sit equally on this one
API.

## Responsibility and boundary

`conway`'s public surface is expressed in terms of `conway-core`'s domain
types and port traits, plus this crate's own
`ConwayBuilder`/`Conway`/`SessionHandle` wrappers — **no type from
`conway-runtime` is re-exported**; `conway-runtime` is an implementation
detail behind the facade, enforced by a grep-based test
(`tests/public_api_surface.rs::at_most_one_runtime_reexport`) that the crate
performs at most **one** direct `pub use conway_runtime::` re-export (the
count is zero in practice). The one
exception outside the `conway-core` re-export set is `ExplainReport` and
its field types (`ExplainEntry`, `EntryOutcome`, `CapabilitySummary`,
`BreakerSnapshot`), re-exported from [`conway-routing`](conway-routing.md)
because `Conway::explain_routing`'s return type is defined there and
duplicating it would fork the type.

## `ConwayBuilder` → `Conway` → `SessionHandle`

- **`ConwayBuilder`** (`builder.rs`) assembles a validated `ConwayConfig`
  plus optional injected ports into a live `Conway`. It is a pure wiring
  layer with no agent logic. Construction: `ConwayBuilder::discover()`
  (XDG/project config discovery) or `::from_config(path)` or
  `::from_parts(config)`, then optional overrides —
  `with_backend`, `with_plugin`, `with_permission_gate`,
  `with_context_hook`, `with_session_store`, `with_router`,
  `with_cli_overrides` — then `build()`. `build()` is synchronous even
  though the default session store (`JsonlSessionStore::open`) and the
  optional startup capability probe are both real `async` I/O: it bridges
  that gap by running the one `async` call to completion on a fresh OS
  thread with its own throwaway current-thread `tokio` runtime, rather
  than `Handle::current().block_on(..)` (which would panic if `build()`
  were ever called from inside an existing tokio context).
- **`Conway`** (`conway.rs`) is the live, assembled facade over one
  `conway-runtime::Runtime`, constructed exclusively via `ConwayBuilder::
  build`. Its surface: `new_session(SessionSpec) -> SessionHandle`,
  `resume(SessionId) -> SessionHandle`, `sessions(SessionFilter)`,
  `session_head`, `fork_from` (see `/ask` below), `explain_routing(&RoleAlias)
  -> ExplainReport` (the `conway routes explain` data source), the three
  ephemeral-`/ask` lifecycle ops `promote`/`pull_in`/`purge` (see `/ask`
  below), `sweep_stale_modal_asks` (the TUI's crash-residue reaper, same
  section), plus `config()`/`warnings()` for introspecting the merged
  config.
- **`SessionHandle`** (`session_handle.rs`) is the consumer-facing surface
  over one running session: `prompt`, `ask` (see below), `events`/
  `events_from(seq)`/`agent_events(agent)` (the last replays and then live-
  tails one specific agent's own transcript — what the TUI uses to switch
  the shown conversation when the focused agent changes), `tree`,
  `context_report`/`context_report_at`, `transcript`, `fork`/`spawn`
  (taking `ForkSpec`/`SpawnSpec`), `steer`, `await_agent`, `cancel`. Every
  method is a thin delegation to `Runtime` —
  no method takes `&mut self`; every state change routes through the
  runtime, never through local mutation.
- **`TurnHandle`**, returned by `prompt`/`ask`, exposes `text()` (await the
  turn's final text), `result()` (await the terminal `AgentResult`), and
  `events()` (an `EventStream` scoped to this turn).

`SessionSpec` (`session_handle.rs`) is the request shape for
`new_session`: agent definition, role, budget, and `keep_alive` (see
[`conway-runtime`](conway-runtime.md) for keep-alive semantics — a
long-lived host like the TUI sets this so the root agent idles between
prompts instead of terminating after its first turn).

## Fork/spawn request shapes

`ForkSpec`/`SpawnSpec` (`subagent_spec.rs`) are the library-consumer-facing
request types for `SessionHandle::fork`/`::spawn`. They are kept as two
distinct types rather than one type with a mode flag specifically so the
fork/spawn distinction is visible at the call site. `SpawnSpec.agent_def` is
`Option<String>` — naming one gives the child a clean-slate system prompt
and tool set from that def; leaving it unset (`SpawnSpec::new`, no
`.agent_def(...)` call) means the spawned child inherits the *spawning*
session's role, and transitively its model routing, exactly like a roleless
fork inherits its forker's role (a recorded design decision that relaxes the
0.1.0 "agent_def mandatory for spawn" rule). Both convert via `From` into
`conway_core::agent::SubagentSpec`, the type `SubagentHost::start` actually
consumes — this module contains no fork/spawn logic of its own, only the
request shape and that conversion.

**`SpawnSpec.cwd: Option<PathBuf>` (C1)** scopes the spawned child to its own
working directory instead of unconditionally inheriting the spawning
session's — the motivating case is an embedder (Kepler) scoping a
drill-down explorer child to one region of a codebase. Set it via
`SpawnSpec::cwd(path)`; `None` (the `SpawnSpec::new` default) preserves the
pre-existing "child inherits the parent's cwd" behavior unchanged.

- **Absolute path:** used as-is.
- **Relative path:** resolved against the PARENT's cwd at spawn time (the
  child has no cwd of its own yet to resolve against) — not the child's own
  eventual cwd, and not the process's current working directory.
- **Nonexistent resolved path:** the spawn fails fast, with a clear error
  naming the offending path (`RuntimeError::Tool(ToolError::Internal{..})`,
  `conway-runtime`'s established "closest fit" error surface for a rejected
  spec — see `subagent.rs`'s own doc), rather than starting a child whose
  tools would silently fail on every relative path.
- **Grandchildren:** a child spawned with `cwd: None` inherits its
  IMMEDIATE parent's (possibly-overridden) cwd, not the root's — the same
  "immediate parent, not root" rule the inherited-transcript machinery
  already follows at fork depth ≥ 2.
- **No sandbox claim:** this governs relative-path resolution only (every
  filesystem tool resolves its relative arguments against
  `conway_core::ports::ToolCtx::cwd`, which this field ultimately sets). An
  absolute path a tool is given, or a `..` that walks back out, still
  escapes it — the permission gate remains the actual enforcement layer;
  `cwd` is defense in depth on top of it, not a replacement for it.

Deliberately **not** a field on `ForkSpec`: a fork inherits the forker's
ENTIRE context (GP-02), so a `ForkSpec.cwd` override would be incoherent
with the context the child actually sees — the child's own transcript would
keep describing the forker's directory while its tools silently resolved
somewhere else. `conway_core::agent::SubagentSpec::cwd` (the type both specs
convert into) does carry the field regardless of mode — `ForkSpec`'s own
`From` impl simply always maps it to `None` — but only `SpawnSpec` exposes
it as a request-shape option.

## The `/ask` ephemeral fork

`SessionHandle::ask(text) -> TurnHandle` is the ephemeral side-question
primitive: it forks the session's root agent at its **current head** into a
fresh child marked `SessionMeta::ephemeral: true` (set once, at creation),
then drives one turn on that child with the given text. Post-B2 the child
goes through the runtime's own subagent machinery (`SubagentHost::start`,
the same path `SessionHandle::fork` and the `conway_ask` tool use), so it
attaches as a **proper fork child of the asker** — `kind: Fork`, a real
`parent` link, `inherited_upto` at the fork point — and `AgentTree::attach`
emits `Event::AgentSpawned { ephemeral: true, .. }` on the live bus (the
TUI's `/agents` panel shows the node with an `(ephemeral)` marker while the
ask runs). The child inherits the full context and tool set — tool calls it
makes are real — but its transcript never touches the parent session, and
ephemeral sessions are excluded from default `sessions()`/`SessionFilter`
results (visible only by direct `SessionId` lookup or `SessionFilter {
include_ephemeral: true, .. }`) — a throwaway question never pollutes the
live conversation or its catalog listing. The returned `TurnHandle`'s
`agent()` names the child; `text()` drains its single turn to the finished
reply.

An ephemeral ask child has exactly **three fates**, all facade lifecycle
ops on the child's `AgentId` (each live-checked against `Runtime::tree()`
and guarded before anything is mutated; P-1 — these are lifecycle
operations on existing agents, not new subagent primitives):

- **`Conway::promote(agent) -> SessionId`** (keep — the TUI modal's `[f]`
  fork fate): the one-way ephemeral→persistent flip, performed atomically
  as durable header rewrite (`SessionStore::set_ephemeral`, the single
  sanctioned header mutation) → live-tree flag flip → `Event::AgentPromoted`
  emission, in that failure-ordered sequence so the three views can never
  split-brain. No re-parenting, no record rewriting: the child's whole
  transcript, origin, and provenance survive verbatim (P-2).
- **`Conway::pull_in(child)`** (the modal's `[p]` fate): merges the child's
  turns into its parent's log — the child's `ForkDirective` head record
  materialized as a `UserTurn` re-stamped `Provenance::MergedAsk { from:
  child_session }`, its `Assistant` records copied verbatim — then purges
  the child via `SessionStore::remove`. Refuses (before any parent
  mutation) when the child is unknown, still running, non-ephemeral, has
  children of its own, or its parent is no longer live.
- **`Conway::purge(agent)`** (the modal's `[esc]` discard fate, and the
  forced fate when the TUI quits with the modal open): deletes the child's
  session outright, merging nothing — the single user-explicit exception to
  mandatory provenance retention (P-2/GP-10). Live-checked, terminal-only,
  and guarded by the store's own ephemeral-only/no-children matrix.

`fork_child.rs` factors out the sequence `Conway::fork_from` needs —
`SessionStore::fork` (the zero-copy, one-header-write operation from
[`conway-session`](conway-session.md)) followed by `Runtime::resume_root`
(re-registering the child as a live, drivable agent, resolving its
inherited prefix) — into one shared implementation. `/ask` itself no longer
uses it: B2 moved the ask flow onto `SubagentHost::start` so the child
attaches as a proper fork child (above).

**`ask_origin` and the crash-residue sweep.** `SessionMeta::ask_origin`
(`Option<AskOrigin>`, `#[serde(default)]` so pre-existing headers decode as
`None`) distinguishes the two ephemeral-ask paths at creation:
`AskOrigin::ModalAsk` (stamped by `SessionHandle::ask`, the TUI's modal
`/ask`) vs `AskOrigin::ToolAsk` (stamped by the `conway_ask` tool). The tag
exists for exactly one consumer: `Conway::sweep_stale_modal_asks()`, which
the TUI runs once at startup to purge modal-`/ask` leftovers whose agent is
not live (a crashed TUI leaves no modal that will ever show the answer, so
no user will ever choose a fate). `ToolAsk` children are **never** swept —
their transcripts are referenced by `EphemeralSessionRef` artifacts in the
calling agent's persisted tool output, and purging one would leave that
artifact dangling.

## NL intent classification (`classify_agent_intent`)

`Conway::classify_agent_intent(parent, default_recipe, text) -> Result<AgentIntent>`
is the facade capability the TUI's `/fork`/`/spawn` free-text path routes
through (C1): it runs an EPHEMERAL, tool-less, one-turn spawn under the
declarative `intent` role alias, parses the reply strictly, and returns an
`AgentIntent { recipe, agent_def, prompt }` — then purges the classifier
session before returning, on every exit path. `default_recipe` is the
caller's command default (`Fork` for `/fork`, `Spawn` for `/spawn`); every
degraded path (unconfigured `[roles.intent]` role, unparseable reply,
invalid recipe, empty prompt) returns a verbatim passthrough `AgentIntent`
carrying that recipe, the raw text, and no agent def — a classifier
failure can never break the command. Other errors (store I/O, backend
failure inside the turn) propagate as `ConwayError::IntentClassification`.
The full design — session shape, prompt-prefix system prompt, the P-10
reply-validation policy, and every disclosed residual — lives in the
`intent` module's doc (`crates/conway/src/intent.rs`); the TUI's
confirmation card (the trust gate, P-10) is documented in
[`conway-cli`](conway-cli.md)'s NL intent section.

## Config discovery

`config/` implements a deterministic, network-free, five-source precedence
merge: **default < XDG < project < env (`CONWAY_*`) < CLI**. `discovery.rs`
does pure filesystem/XDG path discovery (walking from a start directory up
to the filesystem root looking for `<dir>/.conway/settings.json`, nearest
match wins) with no parsing. `merge.rs` does the actual layering — on
`serde_json::Value` (tables union by key, arrays/scalars replace wholesale)
— plus `CONWAY_*` environment-variable mapping and `ConwayConfig` semantic
validation, including mandatory rejection of Anthropic subscription
OAuth-style tokens (`sk-ant-oat*`, which are not valid API keys) and the
headroom sanity checks described in [`conway-core`](conway-core.md)/
[`conway-routing`](conway-routing.md). Only the final merged document is
deserialized with `#[serde(deny_unknown_fields)]`, which is what makes
unknown-key rejection a meaningful fail-loud check on the *result* of
layering five sources rather than on each source individually (an
individual source may legitimately omit almost everything).

`agents.rs` loads `.conway/agents/*.md` — markdown with a YAML frontmatter
block — into `conway_core::config::AgentDef` values; it resolves the
frontmatter verbatim (including the `skills` name list) without touching
the runtime or wiring a live tool registry.

## Configuration schema (`settings.json`)

The facade owns the complete, binding config shape (`config::schema::ConwayConfig`,
deserialized with `#[serde(deny_unknown_fields)]`). Every field has a
`#[serde(default)]` **except** `default_role`, which has no sensible built-in
default (the binding config always sets it). The file is JSON at
`.conway/settings.json` (project) or the XDG config dir (user); the shape:

```jsonc
{
  "default_role": "coder",           // RoleAlias — required; must exist in "roles"
  "cwd": ".",                        // PathBuf — agent working directory

  "session": {
    "root": ".conway/sessions",      // PathBuf — session-log directory
    "fsync": "interval",             // "always" | "interval" | "never"
    "fsync_interval_ms": 200         // u64
  },
  "limits": {
    "max_steps": 40,                 // u32
    "max_tokens": 0,                 // u32 — 0 = unlimited
    "deadline_secs": 0,              // u64 — 0 = none
    "max_parallel_tools": 4          // u32
  },
  "permissions": {
    "mode": "prompt",                // "prompt" | "allowlist" | "deny"
    "allowed_tools": [],             // [String] — used when mode = "allowlist"
    "denied_tools": []               // [String]
  },
  "backends": {                      // map<name, BackendEntry>
    "anthropic": {
      "kind": "anthropic",           // "anthropic" | "openai-compat"
      "api_key": "",                 // optional; mutually exclusive with api_key_env
      "api_key_env": "ANTHROPIC_API_KEY"
    },
    "local": {
      "kind": "openai-compat",
      "dialect": "ollama",           // "openai" | "ollama" | "vllm-hermes" | "lm-studio" | "llamacpp-server"
      "base_url": "http://localhost:11434",
      "stream_tools": null           // Option<bool> — per-model tool-call streaming override
    }
  },
  "routing": { /* RoutingSection */ },
  "roles": {                         // map<alias, RoleEntry>
    "coder": { /* chain of backend/model refs + optional per-role headroom */ }
  },
  "health": {                        // HealthSection
    "default_headroom_tokens": 0     // u32 — reasoning-headroom reservation (see conway-core / conway-routing)
  },
  "agents": { /* AgentsConfig */ },
  "models": { /* ModelsConfig — per-model capability metadata */ }
}
```

Anthropic subscription OAuth tokens (`sk-ant-oat*`) are rejected during
validation (they are not valid API keys). `deny_unknown_fields` applies to the
final merged document, so any unknown key across the five layered sources
fails loud. (The 0.1.0 planning docs described this shape as TOML; 0.2.0
migrated the config file to JSON.)

## `PermissionGate` implementations

`gates.rs` ships three built-in `PermissionGate` implementations covering
the full space described in [`conway-tools`](conway-tools.md)'s
announcement-vs-execution model, all `Send + Sync + 'static` and stateless:

- **`AllowListGate`** — stateless allow/deny by tool name and argument
  glob; this is what backs `-p --allowed-tools` (one-shot mode fails
  closed by default, since it cannot prompt an operator).
- **`DenyAllGate`** — always denies.
- **`PromptingGate`** — delegates to an embedder-supplied handler; this is
  what [`conway-cli`](conway-cli.md)'s TUI uses for its interactive
  permission prompts.

`gates::from_config` selects one from `config::schema::PermissionsConfig`.
`presets.rs` provides the built-in plugin set (`conway-tools`'s `fs`,
`shell`, `subagent`, `report`) behind the `builtin-tools` feature — with it
disabled, this crate has no `conway-tools` dependency at all, and the
function does not exist rather than silently returning an empty vector.
No plugin registered this way is privileged over one an embedder supplies
via `ConwayBuilder::with_plugin`.

## Event stream

`EventStream` (`event_stream.rs`) is a facade-owned type — not a
re-export of `conway-runtime`'s own stream alias — wrapping the runtime's
already lag-normalized broadcast stream (`EventBus::subscribe` folds a
lagging subscriber's dropped-message condition into a synthesized
`Event::Lagged` envelope before this type ever sees it) with a
session/agent filter and an optional replay-from-sequence prefix.

Two event classes deliberately bypass that session/agent filter, exactly
like `Event::Lagged`: **`Event::AgentSpawned` and `Event::AgentFinished`**.
Tree lifecycle is a global concern — a subagent is spawned and finishes on
its own freshly-minted session, so a subscriber scoped to any single
session (the TUI's root `handle.events()`, or a per-turn `TurnHandle`)
would otherwise never observe another agent's spawn/finish at all. That
passthrough is what lets the `/agents` panel and the inline subagent
activity populate. Consumers that treat "an `AgentFinished` reached me" as
"my own agent finished" (e.g. `TurnHandle::text`/`result`) therefore check
the finished `AgentResult`'s `agent_id` themselves rather than assuming it.

**`record_to_event` and the `UserTurn` live twin.** `SessionHandle::
events_from`/`agent_events`'s replay batch is built by `session_handle.rs`'s
`record_to_event`, which maps each persisted `LogRecord` to the `Event` its
live counterpart would have produced. Most record kinds still fall back to
`Event::AgentProgress { note }` (free text, no faithful `Event` shape), but
`LogRecord::UserTurn` now maps faithfully to `Event::UserTurn { text, prov
}` — the exact event `conway-runtime` emits live for the same occurrence
(`Runtime::prompt`, `Runtime::start_root` with an initial prompt, and a
`Spawn` subagent with a non-empty prompt). Because the replay batch and the
live bus can now both carry the identical `Event::UserTurn` for the same
occurrence (the same subscribe-before-read race `AgentFinished`/
`ToolCallFinished` already had to handle), `EventStream`'s junction-dedup
(`has_live_twin`) treats `UserTurn` exactly like those two: a race duplicate
is dropped by content match, not by chance. `ForkDirective`/`ParentSteer`
remain on the `AgentProgress` fallback — see [`conway-core`](conway-core.md)'s
own note on why that's a disclosed scope decision, not an oversight.

## How it fits the whole

`conway` depends on every other workspace crate. It is the sole integration
surface: [`conway-cli`](conway-cli.md) builds on it and nothing else in the
workspace; an embedder depends on `conway` alone and never needs to know
`conway-runtime`, `conway-session`, or the backend adapters exist as
separate crates. See [`/ARCHITECTURE.md §1–2`](/ARCHITECTURE.md) for why
the three consumption modes are all written against this exact API.
