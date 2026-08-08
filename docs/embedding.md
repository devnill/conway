# Embedding conway

`conway` (the crate, not the CLI binary) is the embeddable facade over the
agent harness: `ConwayBuilder` → `Conway` → `SessionHandle` → `TurnHandle`,
the same chain the `conway` CLI itself is built on. This page covers that
chain, a minimal runnable example, configuring providers/roles/permissions/
confinement programmatically, and — checked against the facade's actual
re-exports, not its intent — what a host application can and can't reach
today. For running conway as a subprocess instead, see
[`getting-started.md`](getting-started.md) and
[`scripting.md`](scripting.md).

conway is not published to a registry; depend on it by path within this
workspace (or vendor it):

```toml
// Cargo.toml
[dependencies]
conway = { path = "../conway" }
```

## The facade

- **`ConwayBuilder`** assembles a validated config plus any ports you inject
  (backends, plugins, a permission gate, a session store, a router, a
  context hook) into a live `Conway`. Three constructors: `discover()`
  (the CLI's own default — walks up from the current directory for
  `.conway/settings.json`, same precedence as `getting-started.md`
  describes), `from_config(path)` (an explicit path, still layered under
  the same precedence), and `from_parts(ConwayConfig)` (build the config
  yourself, no discovery, no env, no warnings — see the minimal example
  below).
- **`Conway`** is the built harness: `new_session`/`resume`/`fork_from` open
  a `SessionHandle`; it also owns permission-pattern grant/revoke, config
  warnings, and `explain_routing`.
- **`SessionHandle`** is a live, cheap-to-clone handle onto one running
  session: `prompt(text)` returns a `TurnHandle`; `ask`, `fork`, `spawn`,
  `steer`, `await_agent`, `cancel`, `cancel_with`, and `tree()` are the
  subagent surface; `events()`/`events_from(seq)` are the event stream.
  `cancel` always stops immediately (propagating to the whole subtree);
  `cancel_with(target, reason, CancelMode)` is the primitive it delegates
  to, and `CancelMode::Graceful` is the only way to reach the turn-boundary
  form — it cannot reach a `keep_alive` agent idling between turns, since
  that wait never drains the mailbox a graceful cancel is delivered
  through (see [`agents.md`](agents.md)'s control-surface table).
- **`TurnHandle`** is one prompt in flight: `text().await` concatenates the
  reply as it streams in; `result().await` resolves once the agent's turn
  reaches a terminal `AgentResult` — including `BudgetExceeded`/`Cancelled`,
  never as an error; `events()` gives you the raw stream for that turn's
  agent instead.

`build()` is synchronous even though config/store loading underneath it does
real I/O — it bridges that with a throwaway single-purpose runtime on a
fresh thread. Calling it from inside an existing `tokio` task works, but
briefly blocks that task; if you care, run it via `spawn_blocking` instead.

## A minimal example

`crates/conway/examples/minimal_session.rs` is a complete, runnable example
— it imports only facade re-exports plus `conway_core::fakes` (a dev-only
test double), so it needs no config file, no credentials, and no network:

```console
cargo run -p conway --example minimal_session
```

```text
prompt -> Hello, conway!
ask    -> (ephemeral) just checking something
main-session log head: LogSeq(4) before the ask, LogSeq(4) after -> the ephemeral ask left no trace in the main session
```

The shape that matters, trimmed:

```rust
let conway = ConwayBuilder::from_parts(minimal_config())
    .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
    .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
    .with_session_store(store.clone())
    .with_router(Arc::new(FakeRouter::single(ModelRef { .. })))
    .build()?;

let session = conway.new_session(SessionSpec::default()).await?;
let turn = session.prompt("Hello, conway!").await?;
println!("prompt -> {}", turn.text().await?);
let _ = turn.result().await?;
```

For a **real** session — a real backend, real capability-based routing, a
real store — drop the fake wiring and start from `ConwayBuilder::discover()`
or `from_config(path)` instead; those bring everything `conway new_session`
would need, the same construction the CLI itself uses (see
`crates/conway-cli/src/main.rs`'s `build_conway`).

**One default you must override before `build()` will succeed on an
unmodified config:** `[permissions].mode` defaults to `"prompt"`, and there
is no `ConwayBuilder::with_prompt_handler` — `build()` calls
`gates::from_config` with `prompt_handler: None` unconditionally, and a
`"prompt"` mode with no handler is a `ConwayError::Config`, not a silent
fallback. Either set `permissions.mode` to `"allowlist"` or `"deny"` in your
config, or call `.with_permission_gate(..)` yourself (see below) — one of
the two is mandatory for `build()` to succeed with a default-shaped config.

## Configuring providers, roles, permissions, and confinement

### Providers and roles

Declaratively, these are the same `.conway/settings.json` shape
[`getting-started.md`](getting-started.md) documents — `ConwayConfig`
(`conway::config::schema::ConwayConfig`) is exactly what `discover()`/
`from_config()` parse it into, and what `from_parts()` takes directly if you
build one in code (`minimal_config()` in the example above does this: a
`BTreeMap<String, RoleEntry>` for `[roles]`, a `BTreeMap<String,
BackendEntry>` for `[backends]`). To inject an already-constructed provider
instead of a config-file entry, `ConwayBuilder::with_backend(Arc<dyn
Backend>)` takes precedence over any config-derived backend with the same
`Backend::id()` — this is how the minimal example supplies its fake backend
with no `[backends]` table at all.

### Permissions

`PermissionsConfig` (`conway::config::schema`):
`mode` (`"prompt"` | `"allowlist"` | `"deny"`), `allowed_tools`,
`denied_tools` — `build()` turns that into an `AllowListGate`/`DenyAllGate`/
`PromptingGate` via `gates::from_config`, but **only when you never call
`with_permission_gate` yourself**. The CLI's own `-p` and TUI paths always
call `with_permission_gate` (each needs behavior `gates::from_config` can't
express from config alone — `-p`'s stricter fail-closed default, the TUI's
real interactive channel), so `gates::from_config` is what an embedder gets
for free from a plain config file, not what conway's own binary actually
runs on. **This is a distinct type from `conway::PermissionMode`** (the
facade's top-level re-export of `conway_core::permission_mode::
PermissionMode` — `Prompt`/`Plan`/`AutoAllow`, the *runtime* behavior mode a
live `Conway` is in, read via `Conway::permission_mode()`/set via
`Conway::set_permission_mode()`): same word, two different enums at two
different layers, easy to conflate. To supply your own gate outright —
including a real interactive prompt handler, which conway ships no built-in
implementation of — implement `PermissionGate` yourself (`async fn
check(&self, req: PermissionRequest) -> PermissionDecision`, both types
facade re-exports — this is the one extension point that needs nothing
beyond the `conway` crate; see "What's reachable" below) and pass it to
`ConwayBuilder::with_permission_gate`.

### Confinement

`ConwayBuilder::with_root(path)` is the library form of the
CLI's `--root`; the same danger and the same rule apply. These two settings
are easy to conflate, and mixing them up is the mistake
most likely to cost you real damage — read this before you set either one.

- **`cwd`** (`SessionSpec::cwd`, or `ConwayConfig::cwd` when a session
  doesn't override it) sets the root agent's own working directory: where
  the agent *works*, and where a relative tool argument starts from. It is
  **not** a security boundary. It never limits what a tool call can reach —
  an agent whose `cwd` is `/home/alice/project` can still read or write
  `/etc/passwd` if a tool call names that absolute path.
- **`ConwayBuilder::with_root(path)`** confines the root agent — and, by
  inheritance, every subagent it forks or spawns — to that directory: any
  tool call whose path argument resolves outside it is denied before your
  `PermissionGate` is ever consulted. This **is** the security boundary. A
  subagent can only narrow its inherited root further, never widen it.

Never calling `with_root` leaves every root agent this `Conway` starts
**unconfined**: it can reach anywhere your process's own user account can
reach, byte-for-byte identical to every invocation before this method
existed. Call it whenever you want a hard guarantee that conway cannot touch
anything outside a directory tree, regardless of what a tool call asks for
or what your gate grants.

**When you set a root, `cwd` must resolve inside it.** `Conway::new_session`
verifies the session's own working directory sits inside the confinement
root before it will start; a `cwd` outside it is a typed error, not a
silent widening of the root:

```rust
let conway = ConwayBuilder::from_config(".conway/settings.json")?
    .with_root("/home/alice/project")
    .build()?;
```

### Built-in plugin selection (bash is opt-in)

`build()` no longer registers all four `conway-tools` built-ins
unconditionally. `fs`, `subagent`, and `report` are still on by default;
`conway.shell` (bash — arbitrary shell command execution) is not, and
getting it requires a deliberate act. This is filtered by manifest id
through `PluginSelection`, the same mechanism a bundle of your own
third-party plugins could use for identical selection UX — it is not a
bash-specific switch:

```rust
pub enum PluginSelection {
    All,
    None,
    Only(Vec<String>),
    AllExcept(Vec<String>),
}
```

Two ways to opt in, in increasing order of precedence:

- **Config**: `ConwayConfig::tools.builtin_plugins` (`Vec<String>` of
  manifest ids) defaults to `["conway.fs", "conway.subagent",
  "conway.report"]`. Add `"conway.shell"` to it — in a loaded
  `settings.json`, or in a `ConwayConfig` you build by hand for
  `from_parts()` — to include bash.
- **Builder**: `ConwayBuilder::with_builtin_plugins(selection)` overrides
  the config-derived selection outright, e.g.
  `.with_builtin_plugins(PluginSelection::All)` for "every built-in,
  exactly like before this default changed."

Not calling `with_builtin_plugins` at all is the common case and reads the
config-derived selection instead — so a plain `ConwayBuilder::discover()?
.build()?` on an unmodified config gets `fs`/`subagent`/`report` only.
**A plugin injected via `with_plugin` is never filtered by this** — calling
`with_plugin` is already the explicit, per-plugin declaration this
mechanism exists to require of built-ins too, so an already-explicit call
gains no new hoop to jump through.

### First-party plugin tier

Board item 01KZDC3JQ7W4DY1MG6MBCVB2DV settles the shape of the tier
[`PHILOSOPHY.md`](../PHILOSOPHY.md#first-party-plugins-and-why-they-are-not-defaults)
describes: plugins written and shipped in this repository, but installed the
same way a third party's would be, never by default.

**Where they live.** One crate per plugin under `crates/`, the same layout
every other workspace crate uses — `cargo test --workspace` covers them
without special-casing. `conway` (this facade crate) does not, and must
never, depend on any of them: doing so would put a first-party plugin back
on the exact footing the tier exists to avoid. `crates/conway-plugin-skeleton`
is the worked example this item ships — a `SkeletonPlugin` registering one
`skeleton_ping` tool, written entirely against `conway::plugin`, proving
nothing beyond the mechanism below.

**How one is installed — deliberately a distinct key from
`tools.builtin_plugins`.** That key names a *closed* candidate set (the four
`conway-tools` built-ins this crate itself compiles in) and is validated as
one — an unrecognized id is a hard config error precisely because the full
set is known here, at compile time. A first-party plugin is never a member
of that set, so folding the two together would make `builtin_plugins`'s
existing name a lie in the other direction the day a real first-party
plugin lands in it. Instead:

```json
{ "plugins": { "install": ["conway.plugin_skeleton"] } }
```

`ConwayConfig::plugins.install` (`PluginsConfig`) carries this list, but
this crate never itself acts on it — the same relationship `[tui]` already
has with `conway-cli`'s TUI. Whatever binary or embedder actually links a
given first-party plugin crate reads the list itself, via
[`ConwayBuilder::config`], and calls `with_plugin` (or, for a future
plugin-supplied backend or router, `with_backend`/`with_router` — nothing
about this mechanism is tool-specific) for every id it recognizes, before
calling `build()`:

```rust,ignore
let wanted = builder.config().plugins.install.clone();
if wanted.iter().any(|id| id == conway_plugin_skeleton::PLUGIN_ID) {
    builder = builder.with_plugin(Arc::new(conway_plugin_skeleton::SkeletonPlugin));
}
let conway = builder.build()?;
```

`crates/conway-cli/src/first_party_plugins.rs` is the shipped version of
exactly this: the CLI binary links `conway-plugin-skeleton`, resolves
`plugins.install` against its own small bundle for every dispatch target
(TUI, one-shot `-p`, `sessions`, `routes` — they share one `build_conway`
choke point), and raises a typed config error for an id it does not
recognize rather than installing nothing silently (GP-14). A library
embedder wanting the same plugin depends on `conway-plugin-skeleton`
directly and writes the snippet above — there is no facade-level shortcut
that spares an embedder from linking the crate, because that link is the
whole reason the facade itself stays independent of it.

**What compatibility they promise.** Versioned with the workspace
(`version.workspace = true`, identical to every crate in this tree), not
independently, and not held to `conway-core`'s own strict-semver discipline
(ARCHITECTURE.md §2). That discipline exists because *third-party* plugins
depend on `conway-core`'s port surface; a first-party plugin is, from this
facade's point of view, just another consumer of the public `conway`/
`conway::plugin` surface, not a second thing granting the stability promise
itself. Pre-1.0, a first-party plugin's own API can change in any workspace
release, same as everything else here.

### Installing a router: `RouterFactory` and the `[plugins].install` router arm

Board item 01KZFC2MD1FVNA674YJ9A19T8E extends the same `[plugins].install`
key to router selection. `Router` itself has no identity method — a router
that ships as an installable component instead names itself through a
small, separate factory trait, `conway::RouterFactory`:

```rust,ignore
use conway::{RouterBuildContext, RouterBundle, RouterFactory};
use conway_core::error::ConwayError;

struct MyRouterFactory;

impl RouterFactory for MyRouterFactory {
    fn id(&self) -> &str {
        "my.dynamic_router"
    }

    fn build(&self, ctx: RouterBuildContext<'_>) -> Result<RouterBundle, ConwayError> {
        // `ctx.routing`, `ctx.headroom`, and `ctx.backends` are everything
        // a router genuinely needs — build a `Router` (and the
        // `HealthRegistry` it shares state with) from them here.
        todo!()
    }
}
```

This exists because router **selection** must precede router
**construction**: `[plugins].install` is read long before backends and a
capability picture exist, and building a real router needs both. A
`RouterFactory` carries the id up front and defers the fallible `build`
step to `ConwayBuilder::build()`'s own router step, once that context is
actually assembled.

**Wiring it in, as a library embedder:**

```rust,ignore
let conway = ConwayBuilder::discover()?
    .with_router_factory(Arc::new(MyRouterFactory))
    .build()?;
```

`ConwayBuilder::with_router` (an already-built `Router`) still wins
UNCONDITIONALLY over a registered factory — it is never wrapped, inspected,
or validated, and a factory set alongside it is then never even invoked.
Absent an injected router, a registered factory is invoked instead; absent
both, `build()` falls through to `conway_core::routing::MinimalRouter` —
the config-only core resolver (board item 01KZFC43J1J06BM4CCWKCKHSNV: this
crate no longer compiles a capability-/health-filtering router engine in at
all). A factory whose `build` returns `Err` fails the whole `build()` call
as `ConwayError::Build`, naming the factory's own id and the underlying
message — never silently swallowed, never a silent fallback to
`MinimalRouter`.

**Wiring it in, as `[plugins].install`:** exactly the same shape as a
plugin id, resolved against a binary's own linked bundle of router
factories in the SAME pass as its linked plugins
(`crates/conway-cli/src/first_party_plugins.rs`'s `router_bundle`, beside
its existing `bundle`):

```json
{ "plugins": { "install": ["conway.routing"] } }
```

Naming more than one router-factory id in `[plugins].install` is a hard
config error — a build has exactly one router. `conway-cli`'s own linked
router-factory bundle carries one first-party occupant today,
`conway-plugin-routing`'s `RoutingRouterFactory` (published id
`"conway.routing"`) — the same capability-/health-filtering
`DeclarativeRouter` engine `conway` itself used to compile in
unconditionally before this crate existed, see
[`routing.md`](routing.md#installing-a-different-router). An id an operator
names that this binary does not recognize as either a plugin or a router
factory is a hard error listing both known sets, mirroring the plugin-only
unknown-id error this tier already raised.

**No mode asymmetry** (GP-05/C-03): a router installed via
`[plugins].install` takes effect identically for the TUI, one-shot, and a
library embedder calling `with_router_factory` directly — all three reach
the same `ConwayBuilder::build()` router step.

## Consuming the event stream

`SessionHandle::events()` and `TurnHandle::events()` both return
`conway::EventStream`, which implements `futures_core::Stream<Item =
Envelope>` — the facade itself depends on `futures-core` only, so add
`futures` (or `futures-util`) as your own dependency to get `.next()` and
drive it in a loop, exactly like `conway-cli`'s own one-shot renderer does
(`crates/conway-cli/src/oneshot.rs`, `use futures::StreamExt`):

```rust
let mut events = session.events();
let turn = session.prompt("…").await?;
while let Some(envelope) = events.next().await {
    match envelope.event {
        Event::TextDelta { text } => print!("{text}"),
        Event::ToolCallProposed { tool, .. } => eprintln!("tool call: {tool}"),
        Event::AgentFinished { result, .. } if envelope.agent == session.root() => break,
        _ => {}
    }
}
```

A few things worth knowing before you build a host UI on this:

- **Subscribe before you act.** `SessionHandle::prompt`/`ask` and
  `TurnHandle` construction all take out the broadcast subscription first,
  then perform the action — so the turn's own first events can never be
  missed by a subscribe-after-append race. Do the same in your own code if
  you build anything lower-level than `prompt`/`ask`.
- **Lifecycle events bypass your session/agent filter, deliberately.**
  `Event::AgentSpawned`/`AgentFinished`/`AgentPromoted` are always forwarded
  regardless of which session or agent a stream is scoped to (tree lifecycle
  is a global concern — a subagent's own spawn/finish is stamped under its
  *own*, freshly-minted session id, not its parent's). A consumer building a
  "this agent only" view must check `envelope.agent`/`result.agent_id`
  itself for these three variants — `TurnHandle::text`/`result` do exactly
  that internally, as the example above does.
- **`Event::Lagged { skipped }` can arrive on any stream.** The underlying
  bus is a bounded broadcast channel; a slow consumer sees a synthesized
  `Lagged` envelope instead of silently missing events. Handle it (at
  minimum, log it) rather than assuming every event you'd expect arrives.
- **`events_from(seq)`/`agent_events(agent)`** replay persisted history from
  a point, then transparently continue live — useful for reattaching a UI to
  a session that was already running, without a gap or (within a bounded,
  disclosed residual window) a duplicate at the junction.

## What's reachable from the library, and what isn't

Every extension point below is a `conway_core::ports` trait, and every one
of those *traits* is re-exported at the facade's crate root
(`conway::{Backend, ContextHook, HealthRegistry, PermissionGate, Plugin,
Router, SessionStore, Tool}`). A re-exported trait is only implementable
if every type its methods name is also reachable; the authoring surface
for the three traits plugin authors implement lives in the curated
`conway::plugin` module (below), so that `use conway::plugin::...` plus
the facade root is the whole surface — no `conway-core` dependency.

| Extension point | Trait itself re-exported | Every type its methods need, also re-exported | Implementable from a facade-only crate |
| --- | --- | --- | --- |
| `PermissionGate` | Yes | Yes (`PermissionRequest`, `PermissionDecision`) | **Yes** |
| `Tool` | Yes | Yes, via `conway::plugin` (`ToolSpec`, `ToolCall`, `ToolCtx`, `ToolOutput`, `ToolError`, `PathArgs`, `RenderKind`, plus the `ToolSpec`/`ToolOutput` field types) | **Yes** |
| `Plugin` | Yes | Yes, via `conway::plugin` (`PluginManifest`, plus `Tool`'s own types) | **Yes** |
| `ContextHook` (`with_context_hook`) | Yes | Yes, via `conway::plugin` (`ContextPayload`, `ContextHookCtx`, `OverflowInfo`, `PromptSegment`, `Role`, `Provenance`) | **Yes** |
| `Backend` | Yes | No (`GenerateRequest`, `GenerateResponse`, `BackendError`, `StreamChunk`, `ProbeReport`, `ModelId`, `Capabilities`, …) | No |
| `SessionStore` | Yes | No (`SeqRange`, `StoreError`) | No |
| `Router` | Yes | No (`RouteRequest`, `Route`, `RoutingError`) | No |

The last three rows are deliberate, not gaps: the extension architecture
rejects plugin implementations of `Backend`, `SessionStore`, `Router`,
`HealthRegistry`, `SubagentHost`, and `EventSink` with stated reasons
(`.design/extension-architecture.md` §13.5 — two of those ports are
structurally uncrossable by an async RPC boundary; the rest are policy).
Those traits are re-exported so you can *inject* the workspace's own
implementations or a test double written inside this workspace, not so
third parties write new ones against the facade.

### Writing a plugin

`conway::plugin` re-exports everything needed to implement `Tool`,
`Plugin`, and `ContextHook`: the three traits, their method-argument and
return types, the field types of the structs you construct (`ToolSpec`,
`ToolOutput`, `PluginManifest`, `PromptSegment`), the two capability
handle types the built-in tools themselves name in helper signatures
(`PluginConfig`, `CancellationToken`), and the `async_trait` attribute
macro all three traits are transformed with. Two data-type crates are
part of the public signatures but are *not* re-exported — name them in
your own `Cargo.toml`, at the same version conway uses, so the types line
up: `schemars` (`ToolSpec::schema`) and `serde_json`
(`ToolCall::arguments`, `Tool::render`'s argument).

`ToolCtx`'s remaining fields (`chdir`, `events`, `subagents`)
are handles you call methods on but never need to name — their types are
deliberately not exported, and constructing a `ToolCtx` by hand is
test-fixture work, served by `conway-core`'s `fakes` feature inside this
workspace rather than by the authoring surface. (`cancel` is the
exception: its `CancellationToken` type IS exported, so a helper can take
`&CancellationToken`.)

Register what you write through the builder:

```rust,ignore
let conway = ConwayBuilder::from_parts(config)
    .with_plugin(Arc::new(MyPlugin))
    .with_context_hook(Arc::new(MyHook))
    .build()?;
```

`crates/conway/tests/plugin_surface.rs` is a complete worked example —
a trivial `Tool`, `Plugin`, and `ContextHook` written against
`conway::plugin` alone (it imports no `conway-core` path, and fails to
compile if the export set ever shrinks), registered through
`ConwayBuilder` exactly as above.

## Next steps

- [`getting-started.md`](getting-started.md) — running conway as the CLI
  binary instead, including the config file shape `ConwayConfig` mirrors.
- [`scripting.md`](scripting.md) — the CLI's one-shot mode, if a subprocess
  is a better fit than linking the crate.
- [`permissions.md`](permissions.md) — permission modes and pattern grants
  in depth, for the config-file (`PermissionsConfig`) side of what this page
  covers programmatically.
