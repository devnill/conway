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
  context hook) into a live `Conway`. Four constructors: `discover()`
  (the CLI's own default — walks up from the current directory for
  `.conway/settings.json`, same precedence as `getting-started.md`
  describes), `from_config(path)` (an explicit path, still layered under
  the same precedence — including the ambient
  `$XDG_CONFIG_HOME/conway/settings.json`/`~/.conway/settings.json` layer,
  which always merges in regardless of `path`), `from_config_only(path)`
  (identical, except that ambient user layer is never read — `path` plus
  env plus any `CliOverrides` is the *entire* input; see "Loading config
  without the ambient user layer" below), and `from_parts(ConwayConfig)`
  (build the config yourself, no discovery, no env, no warnings — see the
  minimal example below).
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

## From `cargo add conway` to a first answer

`ConwayConfig` has fourteen fields, and hand-building one by struct literal
means naming every one of them — `ConwayConfig` has no `Default`, on
purpose (see "Discovery, not a struct literal" below). But you almost never
need to build one by hand: `ConwayBuilder::discover()` already layers a
documented, built-in default over all fourteen — the same five-source
precedence chain (default < XDG < project < env < CLI) a `settings.json`,
an environment variable, or a CLI flag all flow through — so a host with no
`~/.conway/settings.json` anywhere still gets a fully-formed, valid config
back, not an error and not a struct-literal ceremony. `crates/conway/
examples/discover_getting_started.rs` is that path, actually run, not just
described:

```console
cargo run -p conway --example discover_getting_started
```

```text
prompt -> Hello, conway!
```

The whole thing, trimmed only for the doc-comment prose the real file
carries (read `discover_getting_started.rs` itself for the reasoning behind
every line):

```rust
let conway = ConwayBuilder::discover()?
    // Per-field overrides over discover()'s own defaults -- not a second
    // config. See below for why these two are still explicit.
    .with_cli_overrides(CliOverrides {
        permission_mode: Some("deny".to_string()),
        ..CliOverrides::default()
    })
    .with_builtin_plugins(PluginSelection::None)
    .with_backend(backend)
    .with_router(Arc::new(FakeRouter::single(route)))
    .with_session_store(Arc::new(FakeStore::new()))
    .build()?;

let session = conway.new_session(SessionSpec::default()).await?;
let turn = session.prompt("Hello, conway!").await?;
println!("prompt -> {}", turn.text().await?);
```

**Two things are still explicit, deliberately — this is where "discovers"
stops and "would be guessing" starts:**

- **A `Backend`.** Nobody may be silently
  billed for a provider they never named, so there is no compiled-in
  fallback backend, ever — `with_backend` (or `with_backend_factory`, for a
  config-driven one) is mandatory. The example above injects a fake one to
  stay offline; `crates/conway/examples/real_provider_inference.rs` is the
  identical screenful with a genuine
  `conway_plugin_backends::openai_compat::OpenAiCompatBackend` wired to a
  real endpoint instead.
- **Where to route.** The built-in default document's baked-in role
  (`default_role = "coder"`, an empty chain) deliberately names no
  destination — see `ConwayBuilder`'s own module doc for why `default_role`
  has no opinion worth inventing at the core. A caller who already knows
  exactly which backend/model to use (the common embedding case) bypasses
  role/chain resolution entirely with `with_router`, as above; a caller who
  wants real role-based fallback routing declares `[roles.<alias>].chain` in
  config instead (see "Providers and roles" below) and skips `with_router`.

Everything else — session storage, limits, tool registration, headroom —
comes from `discover()`'s own layered defaults, or from the two lightweight
PER-FIELD overrides used above (`CliOverrides`, `PluginSelection`), never a
second, competing construction path.

**Permissions default to `"prompt"`, which needs a handler.** Discovery's
built-in default sets `permissions.mode = "prompt"` — the friendliest
default, "ask," rather than "deny" or "allow everything" — and `build()`
fails with a named `ConwayError::Config` if it resolves to `"prompt"` with
neither a handler nor an injected gate, rather than silently picking one
for you. Two ways to satisfy it: `ConwayBuilder::with_prompt_handler(..)`
(the direct path — hand it the one closure your host already has for "may
this proceed?", no need to implement `PermissionGate` yourself for that),
or override `permissions.mode` to `"allowlist"`/`"deny"` (what the example
above does, via `CliOverrides`, since it stays offline and has no UI to ask
through). See "Permissions" below for both, and
`crates/conway/examples/custom_permission_gate.rs` for a full
`PermissionGate` implementation when a handler closure isn't expressive
enough.

**More examples**, each runnable and each with its own doc comment
explaining what it proves:

| Example | What it shows |
| --- | --- |
| `minimal_session.rs` | The original offline smoke test: `from_parts` with a hand-built config, `ask`'s ephemerality. |
| `bare_inference.rs` | What the fourteen-field ceremony costs when you genuinely need `from_parts` (no filesystem, no ambient environment) — and why `discover_getting_started.rs` doesn't pay it. |
| `discover_getting_started.rs` | This section's screenful, actually run. |
| `custom_permission_gate.rs` | A third-party `PermissionGate` implementation, genuinely consulted during a real tool call. |
| `event_stream_consumer.rs` | Consuming `SessionHandle::events()` live instead of waiting on `TurnHandle::text`/`result`. |
| `real_provider_inference.rs` | The same screenful against a real OpenAI-compatible endpoint — opt-in only, never runs a network call unattended. |

### Discovery, not a struct literal

`ConwayConfig` deliberately has no `#[derive(Default)]`, even though
thirteen of its fourteen field types do. `default_role: RoleAlias` is the
one holdout, and it is a decision, not an oversight: a `Default` impl would
have to pick SOME role name, and any name it picked would be an opinion the
core has no business holding — conway serves a coding agent and a bare
inference call equally, and neither reading of "the default role" is more
correct than the other at this layer. Inventing one anyway (`"assistant"`,
`"default"`, anything) would be exactly the "guesses silently" failure mode
this page opens by rejecting: a caller who never thought about roles would
get an opinionated one anyway, discoverable only by reading source.

So there is no `ConwayConfig::default()`, and there does not need to be.
`ConwayBuilder::discover()` answers the same question a different, honest
way: it reads a REAL, inspectable, versioned answer (`config::merge::
default_document`'s `default_role = "coder"`, paired with an empty
`roles.coder.chain`) off the same five-source precedence chain a
`settings.json` would override — a stated, overridable convention, not a
value baked silently into a Rust `impl`. `Conway::config()`/`LoadOutcome::
warnings` make it inspectable after the fact, and an empty chain fails
routing with a NAMED `RoutingError::NoCandidate` the moment you actually
try to use it unmodified — never a silent fallback to some other model. A
caller who wants no opinion about roles at all skips the question entirely
with `with_router`, as the screenful above does.

If you genuinely need a config with no filesystem and no ambient
environment involved at all (an embedded target, a from-scratch test
fixture) — `ConwayBuilder::from_parts(ConwayConfig)` is still there, and
you still name all fourteen fields, because there is still no default
value for `default_role` to fall back to. `bare_inference.rs` is that path,
deliberately exercised so the cost is visible rather than assumed away.

## Configuring providers, roles, permissions, and confinement

### Providers and roles

Declaratively, these are the same `.conway/settings.json` shape
[`getting-started.md`](getting-started.md) documents — `ConwayConfig`
(`conway::config::schema::ConwayConfig`) is exactly what `discover()`/
`from_config()` parse it into, and what `from_parts()` takes directly if you
build one in code (`minimal_session.rs`'s own `minimal_config()` does this:
a `BTreeMap<String, RoleEntry>` for `roles`, a `BTreeMap<String,
BackendEntry>` for `backends`). To inject an already-constructed provider
instead of a config-file entry, `ConwayBuilder::with_backend(Arc<dyn
Backend>)` takes precedence over any config-derived backend with the same
`Backend::id()` — this is how `minimal_session.rs`/`discover_getting_started.rs`
both supply their fake backend with no `backends` table entry at all.

### Loading config without the ambient user layer

`discover()` and `from_config(path)` both read
`$XDG_CONFIG_HOME/conway/settings.json` (falling back to
`~/.conway/settings.json`) unconditionally, deep-merged in *before* whatever
`discover()`/`path` finds — `explicit_path` in `LoadOptions` only ever
replaces the project-scoped layer, never the user-scoped one. For a `conway`
CLI invocation that is the intended behavior: an operator's own global
settings should apply everywhere. For a host application embedding `conway`
as a library, it is usually not: the invoking user's own
`~/.conway/settings.json` (which may declare backends, credentials, or
routing that has nothing to do with your application) has no business
merging into your program's config, and neither does a test fixture that
wants to assert against exactly the config it wrote.

`ConwayBuilder::from_config_only(path)` is that seam: identical to
`from_config`, except the XDG/user layer is never read — the merge becomes
`default < path < env < CliOverrides`, four sources instead of five.
`conway::config::load_ignoring_xdg` is the underlying `config::load`
sibling, for callers who want the lower-level `LoadOutcome` (config plus
warnings) rather than a `ConwayBuilder`.

**This suppresses the XDG layer only — not `env`.** `CONWAY_*` environment
variables are how CI and container entrypoints hand a specific invocation
its credentials and overrides; they are supplied by the caller of *this*
invocation, not left over from someone else's home directory, so
`from_config_only`/`load_ignoring_xdg` merge them in exactly as `from_config`
does. A caller that also wants an env-free load already has the tool for
that: build `LoadOptions` with a hand-assembled (possibly empty) `env` map,
or use `from_parts(ConwayConfig)` directly.

### Layering flag-shaped overrides: `CliOverrides`

`conway::config::merge::CliOverrides` is the fifth and highest-precedence of
`load`'s five merge sources (default < XDG < project < env < CLI-overrides),
and the only one that isn't a settings-file-shaped table: a struct of
`Option<T>` fields shaped like command-line flags (`default_role`,
`cwd`, `permission_mode`, `allowed_tools`, `denied_tools`, `max_steps`,
`session_root`, `headroom_tokens`) that a host application constructs
programmatically — say, from its own CLI parser, a web request, or a
config UI — and layers onto whatever `discover()`/`from_config()` already
found. Pass one via `LoadOptions::cli_overrides` (before `load()` runs) or
`ConwayBuilder::with_cli_overrides` (re-applied, and fully re-validated, at
`build()` time); either way, a `Some` field wins over every other source,
and each field's `None` leaves the lower-precedence value untouched.

**The `conway` CLI binary does not use this struct.** `conway-cli` wires its
own flags (`--role-override`, `--cwd`, `--permission-mode`,
`--allowed-tools`, `--deny-tools`, etc.) directly into one-shot's/the TUI's
own construction paths instead — routing them through `CliOverrides` would
be actively breaking there (several of `conway-cli`'s flags are
non-`Option`, always-present-with-a-default clap fields, so layering them
as the highest-precedence source would make a bare `conway -p "hi"` fail
config validation; see `CliOverrides`'s own doc comment and for the full reasoning). That does not affect
an embedder: your own flags are typically `Option`-shaped from the start
(absent means "don't override"), which is exactly what this struct expects.

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
different layers, easy to conflate.

`mode = "prompt"` — discovery's own default — needs SOMETHING to answer
"may this proceed?" with, and conway ships no built-in implementation of
that decision itself (only the three modes' *selection* machinery). Two
ways to supply one, in order of how much you need to write:

- **`ConwayBuilder::with_prompt_handler(handler)`** — the direct path, for
  the common case where you have exactly one closure: `Arc<dyn
  Fn(PermissionRequest) -> BoxFuture<'static, PermissionDecision> + Send +
  Sync>` (`conway::gates::PromptHandler`). `gates::from_config` wraps it in
  a `PromptingGate` for you. Not calling this (and not calling
  `with_permission_gate` either) leaves `mode = "prompt"` failing `build()`
  with a named `ConwayError::Config` — never a silent `AllowAlways`/`DenyAll`
  substitute.
- **`ConwayBuilder::with_permission_gate(gate)`** — supply your own gate
  outright, for policy a single closure can't express (per-tool audit
  logging, an allow-list keyed off your own data). Implement
  `PermissionGate` yourself (`async fn check(&self, req: PermissionRequest)
  -> PermissionDecision`, both types facade re-exports — this is the one
  extension point that needs nothing beyond the `conway` crate; see "What's
  reachable" below). Wins unconditionally over a prompt handler if both are
  set. `crates/conway/examples/custom_permission_gate.rs` is a complete,
  runnable one.

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

### First-party plugin tier settles the shape of the tier
[`PHILOSOPHY.md`](../PHILOSOPHY.md#first-party-plugins-and-why-they-are-not-defaults)
describes: plugins written and shipped in this repository, but installed the
same way a third party's would be, never by default.

**Where they live.** One crate per plugin under `crates/`, the same layout
every other workspace crate uses — `cargo test --workspace` covers them
without special-casing. `conway` (this facade crate) does not, and must
never, depend on any of them: doing so would put a first-party plugin back
on the exact footing the tier exists to avoid. `crates/conway-plugin-skeleton`
is the worked example this item ships — a `SkeletonPlugin` registering one
`skeleton_ping` tool and one
`/conway.plugin_skeleton.ping` TUI slash command, both written entirely
against `conway::plugin`, proving nothing beyond the mechanism below and
[`docs/plugins/hooks.md`](plugins/hooks.md) point 15. `crates/conway-plugin-history`
 is this tier's first REAL, non-worked
member: `/conway.history.rewind <seq>`, forking the calling session at an
explicit, persisted sequence number via `CommandOutcome::ForkSession` (see
that variant's own doc, and [`docs/plugins/hooks.md`](plugins/hooks.md)
point 15's "Forking the calling session" subsection, for the mechanism a
plugin command uses to retarget the session that invoked it without ever
holding a live handle on any session).

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
this crate never itself acts on it. Unlike `[tui]`, which used to be
carried here on that same "wire shape only, no behavior" footing and
moved out entirely (board item
01KZVYYWZ85D1SYMCSRRZ7RAM3, "Stage 2a"): `[plugins]` stays in `ConwayConfig`
because every consumption mode resolves it the SAME way, through
`ConwayBuilder::install_selected` below; `[tui]` is presentation-only
vocabulary that only `conway-cli`'s TUI ever renders, so a headless host
linking this facade no longer has to parse or validate it at all --
`TuiSection`/`ThemeConfig`/`ThemeStyleConfig`/`StatusLineConfig` now live in
`crates/conway-cli/src/tui/config.rs` instead. [`ConwayBuilder::install_selected`] is the one
call every binary or embedder makes instead of resolving that list by hand:
hand it the plugins, router factories, and backend factories **you** link —
already constructed, as `Vec`s — and it resolves `plugins.install` (unioned
with `plugins.default_backends` for the backend arm; see "Installing a
backend" below) against exactly those three bundles, calling
`with_plugin`/`with_router_factory`/`with_backend_factory` for every id it
recognizes and raising a typed config error naming every id it does not. An
id that resolved to nothing would be a silent lie, and is never an
acceptable outcome.

`conway` (this facade crate) still maps no id to a crate itself — see
"Where they live" above. It only matches an id string against whatever you
already constructed and handed it:

```rust,ignore
let conway = ConwayBuilder::from_parts(config)
    .install_selected(
        vec![Arc::new(conway_plugin_skeleton::SkeletonPlugin)],
        vec![], // no RouterFactory this binary links
        vec![], // no BackendFactory this binary links
    )?
    .build()?;
```

`crates/conway-cli/src/first_party_plugins.rs` is the shipped version of
exactly this: the CLI binary links `conway-plugin-skeleton`,
`conway-plugin-routing`, and `conway-plugin-backends`, constructs its own
three bundles, and calls `install_selected` once for every dispatch target
(TUI, one-shot `-p`, `sessions`, `routes` — they share one `build_conway`
choke point). A library embedder wanting the same plugin depends on
`conway-plugin-skeleton` directly and writes the snippet above — there is no
facade-level shortcut that spares an embedder from linking the crate,
because that link is the whole reason the facade itself stays independent of
it. `install_selected` only spares you from re-deriving the resolution loop
that used to be `conway-cli`'s alone.

**What compatibility they promise.** Versioned with the workspace
(`version.workspace = true`, identical to every crate in this tree), not
independently, and not held to `conway-core`'s own strict-semver discipline
(ARCHITECTURE.md §2). That discipline exists because *third-party* plugins
depend on `conway-core`'s port surface; a first-party plugin is, from this
facade's point of view, just another consumer of the public `conway`/
`conway::plugin` surface, not a second thing granting the stability promise
itself. Pre-1.0, a first-party plugin's own API can change in any workspace
release, same as everything else here.

### Installing a router: `RouterFactory` and the `plugins.install` router arm

`RouterFactory` installs through the same `plugins.install` pass and
runs with the same unsandboxed privileges as any other plugin; unlike a
`BackendFactory` it receives no raw credential, only already-built
`Backend` handles it can call — see
[`docs/plugins/trust-and-security.md`](plugins/trust-and-security.md#backends-and-routers-the-same-install-pass-and-one-hands-over-more)
for what that distinction does and does not buy you. extends the same `plugins.install`
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
**construction**: `plugins.install` is read long before backends and a
capability picture exist, and building a real router needs both. A
`RouterFactory` carries the id up front and defers the fallible `build`
step to `ConwayBuilder::build()`'s own router step, once that context is
actually assembled.

**`RouterFactory` itself is facade-only implementable — filling in that
`todo!()` with a hand-written `Router` is not, today.** `RouterFactory`'s
own two methods name only `RouterBuildContext`, `RouterBundle`, and
`ConwayError`, all re-exported at `conway`'s root, so the shell above
compiles from a crate depending only on `conway`. What you put inside
`build()` to actually satisfy it — a value implementing `Router::resolve`
— is a different question (see the reachability table above): that
method's own signature names `RouteRequest`, `Route`, and `RoutingError`,
none of which the facade re-exports, so writing a *new* `Router` from
scratch still needs a direct `conway-core` dependency, same as
`conway-plugin-routing` (`crates/conway-plugin-routing/Cargo.toml`) needs
to write the one this workspace ships. A facade-only `RouterFactory` can
still be useful without writing a new `Router`: reuse or select among
routers a fuller dependency elsewhere in your own workspace already built,
and hand the chosen one back from `build()`.

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
the config-only core resolver (: this
crate no longer compiles a capability-/health-filtering router engine in at
all). A factory whose `build` returns `Err` fails the whole `build()` call
as `ConwayError::Build`, naming the factory's own id and the underlying
message — never silently swallowed, never a silent fallback to
`MinimalRouter`.

**Wiring it in, as `plugins.install`:** exactly the same shape as a
plugin id, resolved against a binary's own linked bundle of router
factories in the SAME pass as its linked plugins and backend factories —
[`ConwayBuilder::install_selected`]'s second `Vec` argument
(`crates/conway-cli/src/first_party_plugins.rs`'s `router_bundle`, beside
its existing `bundle`):

```json
{ "plugins": { "install": ["conway.routing"] } }
```

Naming more than one router-factory id in `plugins.install` is a hard
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

**No mode asymmetry**: a router installed via
`plugins.install` takes effect identically for the TUI, one-shot, and a
library embedder calling `with_router_factory` directly — all three reach
the same `ConwayBuilder::build()` router step.

## Consuming the event stream

`crates/conway/examples/event_stream_consumer.rs` is this section, actually
run and runnable (`cargo run -p conway --example event_stream_consumer`).

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
`Backend` authors have their own analogous curated module,
`conway::backend` (also below).

| Extension point | Trait itself re-exported | Every type its methods need, also re-exported | Implementable from a facade-only crate |
| --- | --- | --- | --- |
| `PermissionGate` | Yes | Yes (`PermissionRequest`, `PermissionDecision`) | **Yes** |
| `Tool` | Yes | Yes, via `conway::plugin` (`ToolSpec`, `ToolCall`, `ToolCtx`, `ToolOutput`, `ToolError`, `PathArgs`, `RenderKind`, plus the `ToolSpec`/`ToolOutput` field types) | **Yes** |
| `Plugin` | Yes | Yes, via `conway::plugin` (`PluginManifest`, plus `Tool`'s own types) | **Yes** |
| `ContextHook` (`with_context_hook`) | Yes | Yes, via `conway::plugin` (`ContextPayload`, `ContextHookCtx`, `OverflowInfo`, `PromptSegment`, `Role`, `Provenance`) | **Yes** |
| `Backend` | Yes | Yes, via `conway::backend` (`GenerateRequest`, `GenerateResponse`, `StreamChunk`, `BoxStream`, `Admission`, `check_admission`, `BackendError`, `Capabilities`, `ProbeReport`, `BackendId`, `ModelId`, plus the field types those structs are built from) | **Yes** |
| `SessionStore` | Yes | No (`SeqRange`, `StoreError`) | No |
| `Router` | Yes | No (`RouteRequest`, `Route`, `RoutingError`) | No |

**This table is about the IN-PROCESS question — can a crate depending only
on `conway` write a new `impl` of the trait itself — which is a different
question from the one the extension design answers.**
That section (dated status note, 2026-08-09) is a non-goals list for the
OUT-OF-PROCESS subprocess-plugin design; it says nothing about registering
an in-process Rust type through `ConwayBuilder`, and citing it here as
though it did was itself a standing error this page carried, now corrected.
`Backend` used to be named alongside `SessionStore`/`Router` in this
table's "not reachable" set; added
`conway::backend` specifically to make the raw trait facade-only
authorable, so that row is unconditionally **Yes** now.

`SessionStore`'s row is unchanged and stays **No**: `SeqRange`/`StoreError`
are not re-exported, so a facade-only crate cannot spell `SessionStore::
append`'s own signature. `Router`'s row is also correctly **No** for the
same narrow reason — `RouteRequest`/`Route`/`RoutingError` are not
re-exported either, so a facade-only crate cannot write `impl Router`'s
`resolve` method by hand, and (checked directly) `conway-plugin-routing`,
the one crate in this workspace that does implement `Router`, depends on
`conway-core` directly rather than only on `conway` to do it — verified by
compiling a facade-only scratch crate against each claim, not by reading
. **That is not
the whole story for `Router`, though, and reading only this row would be
misleading:** a *separate* trait, `RouterFactory` — installing, not
authoring, a router — genuinely is facade-only implementable end to end
(its own methods name only `RouterBuildContext`/`RouterBundle`/
`ConwayError`, all re-exported), and is real, tested machinery, not a
forward declaration — see "Installing a router" below, and
`crates/conway/tests/router_factory.rs`. `SessionStore` and `Router` are
both still re-exported at the trait level so you can *inject* the
workspace's own implementation or a test double built inside this
workspace via `with_session_store`/`with_router`, not so a facade-only
crate authors a wholly new one.

### Installing a backend: `BackendFactory`

**Before you register one, read
[`docs/plugins/trust-and-security.md`](plugins/trust-and-security.md#backends-and-routers-the-same-install-pass-and-one-hands-over-more).**
`with_backend_factory` runs with the same unsandboxed privileges as
`with_plugin`, and `BackendFactory::build` is additionally handed the
operator's resolved `api_key` — see that section for what that means for a
factory you didn't write yourself.

`ConwayBuilder::with_backend` takes a backend you have already constructed.
`with_backend_factory` takes something that knows *how* to construct one,
and defers that until the configuration it needs exists — the same split
`RouterFactory` makes above, and for the same reason: an install list is
read long before API keys, base URLs, and per-model overrides do.

`BackendFactory::id()` names a **kind** — a dialect, like `anthropic` — and
is not the same question `Backend::id()` answers. That one is a *configured
instance* identity, taken from the `backends.<id>` key, and two
configured backends can be the same kind under different ids. The
consequence: one registered factory can be built **once per matching
configuration entry**, where a `RouterFactory` is built at most once,
because a build has exactly one router.

```rust,ignore
use conway::{BackendBuildContext, BackendFactory, ConwayBuilder};

let conway = ConwayBuilder::from_parts(config)
    .with_backend_factory(Arc::new(MyDialectFactory))
    .build()?;
```

Precedence, stated in `with_backend_factory`'s own doc and enforced by
`build()`: an **injected** instance beats a factory-built one sharing its
`Backend::id()` — extending `with_backend`'s existing per-id rule rather
than inventing a second one. Two factories reporting the same kind id is a
**hard `build()` error naming both**, checked before any factory's `build`
runs so a duplicate never leaves one factory's side effects behind.
Registering no factories leaves `build()` behaving exactly as before.

**Reachable from configuration.** `backends.<id>.kind` is an open name, resolved against every registered
factory's own `id()` — ONLY ( removed
the temporary compiled-in fallback the predecessor item left standing;
`conway` itself compiles in no kind at all any more). A `backends.<id>`
entry naming `MyDialectFactory`'s own kind is what invokes it, with a
`BackendBuildContext` resolved from THAT entry — `id` is the entry's own
JSON key, `base_url`/`dialect` copied verbatim, `api_key` resolved the same
centralized way (literal `api_key` wins, else `api_key_env` read from the
process environment, else `None`), and `profile_file_paths` the same
discovered `.conway/profiles.toml` path list every entry receives whether
or not its kind reads it. A `kind` no registered factory claims fails
`build()` naming the offending value and every kind this build recognises —
a misspelled or unregistered kind is never silently ignored.

**The two dialects `conway` used to compile in are now `conway-plugin-backends`,
a first-party plugin — see
[`docs/providers.md`](providers.md#where-a-backend-is-declared)**. Its
`AnthropicBackendFactory`/`OpenAiCompatBackendFactory` (published kind ids
`ANTHROPIC_KIND`/`OPENAI_COMPAT_KIND`, unchanged strings `"anthropic"`/
`"openai-compat"`) are registered exactly like `MyDialectFactory` above — an
embedder using `conway` alone depends on `conway-plugin-backends` directly
and calls `with_backend_factory` for each dialect it wants, before
`build()`. The shipped `conway` binary is the one place this happens
without you writing it: `conway-cli` links the crate and hands both
factories to [`ConwayBuilder::install_selected`]'s third `Vec` argument,
which attaches by default (`plugins.default_backends`, default
`["anthropic", "openai-compat"]`, unioned into the resolved id set inside
`install_selected` itself) — the one first-party mechanism that
attaches with no `plugins.install` entry at all, since a backend, unlike
a router or a tool plugin, has no honest degenerate fallback (an install
with none attached cannot reach a model). `conway` (the facade) never does
this for you.

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
are handles you call methods on but never need to name — their types
(`CwdHandle`, `EventSinkHandle`, `SubagentHandle`) are deliberately not
exported, and constructing a `ToolCtx` by hand for a unit test no longer
requires naming any of them either. `ToolCtx::for_test(agent_id, cwd,
subagents, events)` builds a fully-wired `ToolCtx` from an `AgentId`, a
`cwd`, and `Arc<dyn SubagentHost>`/`Arc<dyn EventSink>` — pass it the port
doubles those handles wrap (`FakeSubagentHost`, `CollectingEventSink`, ...),
reachable through this crate's own `testkit` feature
(`conway::testkit::{FakeSubagentHost, CollectingEventSink}` once enabled),
already wrapped in an `Arc`:

```rust,ignore
use std::sync::Arc;
use conway::plugin::ToolCtx;
use conway::testkit::{CollectingEventSink, FakeSubagentHost};
use conway::AgentId;

let agent_id = AgentId::new();
let subagents = Arc::new(FakeSubagentHost::new(agent_id));
let events = Arc::new(CollectingEventSink::new());
let ctx = ToolCtx::for_test(agent_id, "/tmp".into(), subagents.clone(), events.clone());
// invoke the tool under test, then assert on `subagents.started()` /
// `events.events()`.
```

`subagents`/`events` are cloned before the call above because `invoke` takes
`ctx` by value, but each double lives behind the shared `Arc` — the clone
kept outside `ctx` still sees every call recorded through it. Every field
`for_test` doesn't take a parameter for is defaulted the way a test that
doesn't care about it wants (a fresh session id, an uncancelled `cancel`,
`chdir` seeded from `cwd`, a discarding `plugin_events`, an empty `config`)
— override any of those with ordinary struct-update syntax, `ToolCtx {
cancel: my_token, ..ToolCtx::for_test(..) }`, the same way you'd override a
field on any other plain struct literal.
(`cancel` was already the one exception to "never need to name the type":
its `CancellationToken` type IS exported, so a helper can take
`&CancellationToken` — unaffected by any of the above.)

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

### Writing a `Backend`

`conway::backend` re-exports everything needed to implement `Backend`:
the trait itself (already at the facade root, duplicated here for
self-sufficiency), its five methods' request/response types
(`GenerateRequest`, `GenerateResponse`, `StreamChunk`, `BoxStream`,
`ProbeReport`), the admission types (`Admission`, `check_admission`),
`BackendError`, and the field types those structs are built from
(`BackendId`, `ModelId`, `Capabilities` and its four capability enums,
`SamplingParams`, `PrefixKey`, `PromptSegment`, `ToolSpec`, `StopReason`,
`Usage`, `ContentBlock`, `ToolCall`), plus the `async_trait` attribute
macro the trait is transformed with.

`check_admission` is not optional: `Backend::admit`'s contract requires
every implementation — the trait's own default and every override —
to call it for the fits/shortfall arithmetic rather than restating
`est_tokens + headroom_tokens <= max_context_tokens` itself (one
implementation of "fits" for the whole workspace). An override that
cannot name `check_admission` cannot honour that contract.

A handful of field types one level further down (`Role`, `Provenance`,
`ToolCategory`, `PermissionClass`, `ToolName` — needed to construct a
`PromptSegment`/`ToolSpec` literal) are not re-exported a second time
inside `conway::backend`; they are already reachable at the facade root
or through `conway::plugin`, and a `Backend` author names them from
there, same as any other facade-only crate would.

`crates/conway/tests/backend_parity.rs` is a complete worked example — a
small, deterministic, no-network-I/O `Backend` written against
`conway::backend` alone (all five methods implemented, `admit` overridden
and calling `check_admission`), driven end to end by its own tests rather
than merely compiling. `crates/conway/tests/backend_surface.rs` pins the
module's export list by name, the same way `public_api_surface.rs` pins
the facade root's.

`docs/providers.md`'s ["Writing your own
adapter"](providers.md#writing-your-own-adapter) is the full authoring
page for this trait, including a worked example built against exactly
this same crate boundary and, in ["What conway cannot
enforce"](providers.md#what-conway-cannot-enforce), the obligations
steering places on an implementor that no test in this tree can check for
a crate it doesn't compile — cache hints must not change request bytes,
`admit` must call `check_admission` honestly, untrusted input must yield a
typed `BackendError` rather than panic. Read
[`docs/plugins/trust-and-security.md`](plugins/trust-and-security.md#backends-and-routers-the-same-install-pass-and-one-hands-over-more)
before shipping one: registering a `Backend` via `with_backend_factory`
carries the same unsandboxed privileges as any other plugin, plus the
operator's resolved credential.

## Next steps

- [`getting-started.md`](getting-started.md) — running conway as the CLI
  binary instead, including the config file shape `ConwayConfig` mirrors.
- [`scripting.md`](scripting.md) — the CLI's one-shot mode, if a subprocess
  is a better fit than linking the crate.
- [`permissions.md`](permissions.md) — permission modes and pattern grants
  in depth, for the config-file (`PermissionsConfig`) side of what this page
  covers programmatically.
