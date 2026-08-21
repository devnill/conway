# conway

**A Rust agent harness for agentic coding.**

conway runs LLM-driven agents that use tools, spawn and fork child agents, and
route across multiple model backends — with explicit permissions, an
append-only session log, and no hidden context manipulation. It ships as a
library (the `conway` facade crate) and a CLI/TUI (`conway`).

License: **AGPL-3.0-only** (see [Licensing](#licensing)).

---

## What it does

- **Hierarchical agents.** An agent can **fork** (a child inherits the parent's
  full context, frozen at the fork point) or **spawn** (a child starts from a
  clean slate). Context flows one way only — there is no bleed back into the
  parent; cross-agent communication is explicit (steer / report envelopes).
- **Role-based routing with ordered fallback — capability filtering and
  breakers when you install them.** A default build resolves a role to its
  configured chain and walks that chain in order: a candidate whose backend
  refuses the request is skipped and the next one serves it, so a failed
  candidate degrades to the next rather than failing the request. A default
  build does **not** filter candidates on declared capabilities, track
  endpoint health, or open circuit breakers. Those arrive with the routing
  plugin (`crates/conway-plugin-routing`, installed by naming
  `conway.routing` in `plugins.install`), which adds pre-flight capability
  filtering on context window, tool-calling and reasoning, plus a
  per-endpoint **circuit breaker**.
- **Pluggable tools behind a permission gate.** Tools are plugins; every call
  passes an explicit `PermissionGate` (allow once / allow always / deny /
  deny-with-feedback). Tool *announcement* (what the model is told about) is
  separate from tool *execution* (what is allowed to run).
- **Ephemeral `ask`.** Fork the session, ask a throwaway question, discard the
  result — a side-question that never pollutes the live transcript.
- **Keep-alive sessions.** Opt-in multi-turn sessions that idle awaiting the
  next prompt instead of terminating.
- **Pluggable context curation (`ContextHook`).** Transform the outgoing
  request (mask records, edit the system prompt, filter the announced tools)
  or react to context overflow — script- or inference-driven. No built-in
  curation policy; no automatic compaction.
- **Append-only session log.** Every turn, tool call, and tool result is a
  record; context is assembled deterministically from the log.
- **A copy-paste-friendly TUI.** Single-column chat, a `/`-command palette with
  arrow selection, and an on-demand agent-tree panel.

## Quickstart

Requires a recent stable Rust toolchain (see `rust-version` in `Cargo.toml`).

**Run the offline example** — no config, no credentials, no network (it uses a
fake echo backend and exercises the public facade):

```console
cargo run -p conway --example minimal_session
```

```text
prompt -> Hello, conway!
ask    -> (ephemeral) just checking something
```

See [`crates/conway/examples/minimal_session.rs`](crates/conway/examples/minimal_session.rs).

For a real session — installing the binary, configuring a model provider,
and running your first prompt against it — see
[`docs/getting-started.md`](docs/getting-started.md). Once that works,
[`GUIDE.md`](GUIDE.md) is the day-to-day walkthrough: what a session
actually looks like, writing hooks, working the agent tree, recovering
from a bad turn, and the things that are not discoverable from the
screen. See [`docs/README.md`](docs/README.md) for the rest of the
documentation:
driving the TUI, scripting one-shot mode, and embedding conway as a
library. If you want to *extend* conway rather than use it —
a hook, a tool, a provider adapter — start at
[`docs/plugins/`](docs/plugins/README.md), which is the authoritative
description of what an author may rely on.

**Build the CLI:**

```console
cargo build -p conway-cli --release   # binary at target/release/conway
```

**One-shot mode** (against a configured backend — see [Configuration](#configuration)):

```console
conway -p "what time is it? use bash" --allowed-tools bash
```

`--allowed-tools` is required to enable tools in one-shot mode: with none
listed, all tool calls are denied (one-shot cannot prompt interactively, so it
fails safe).

**Interactive TUI** — run `conway` with no `-p`:

```console
conway
```

**bash is off by default** in the TUI and for a library embedder's own
`ConwayBuilder::build()` — conway's `fs`/`subagent`/`report` built-ins are
registered automatically, bash is not, and getting it requires a deliberate
opt-in. One-shot mode is unaffected (bash was already, and remains, gated by
`--allowed-tools` above). To enable it for the TUI, add `"conway.shell"` to
`settings.json`'s `tools.builtin_plugins`:

```json
{ "tools": { "builtin_plugins": ["conway.fs", "conway.subagent", "conway.report", "conway.shell"] } }
```

See [`docs/getting-started.md`](docs/getting-started.md#enabling-bash-shell-commands)
for the full explanation and the library-embedder equivalent
(`ConwayBuilder::with_builtin_plugins`).

## Configuration

conway discovers configuration with increasing precedence:

```
built-in defaults < ~/.conway/settings.json < ./.conway/settings.json < env < CLI flags
```

`settings.json` declares backends (OpenAI-compatible and Anthropic dialects,
including local servers such as Ollama), roles and their model chains, routing,
permissions, and limits. The library equivalent is
`ConwayBuilder::discover()` / `ConwayBuilder::from_config(path)`.

## Plugins

Every tool conway offers comes from a plugin, including its own. The built-ins
are written against the same `Plugin`/`Tool` traits a third party implements, so
there is nothing a built-in can do that your own plugin cannot.

| Plugin | Tools | Registered by default |
|---|---|---|
| `conway.fs` | `read`, `write`, `edit`, `glob`, `grep`, `cd` | yes |
| `conway.subagent` | `conway_fork`, `conway_spawn`, `conway_ask`, `conway_steer`, `conway_await`, `conway_cancel` | yes |
| `conway.report` | `report` | yes |
| `conway.shell` | `bash` | **no** — opt-in |

Registration is what the model is *told about*. It is separate from permission:
a registered tool still passes the gate on every call, and in one-shot mode a
tool absent from `--allowed-tools` is denied whether or not its plugin is
installed.

Change the set with `tools.builtin_plugins` in `settings.json`, which replaces
the default list rather than adding to it:

```json
{ "tools": { "builtin_plugins": ["conway.fs", "conway.subagent", "conway.report", "conway.shell"] } }
```

An embedder uses `ConwayBuilder::with_builtin_plugins`. `conway.shell` is
excluded by default because it is the one general-purpose code-execution
primitive in the set; the other three are what make conway usable out of the
box, and none of them grants arbitrary execution. See
[`docs/getting-started.md`](docs/getting-started.md#enabling-bash-shell-commands).

### First-party plugins

conway ships a second tier of plugins, maintained in this repository and
shipped alongside it, but *not* registered by default: dynamic routing
(fallback chains, health tracking, circuit breaking), context compaction,
memory, skills, and MCP support are the ones named in
[`PHILOSOPHY.md`](PHILOSOPHY.md#first-party-plugins-and-why-they-are-not-defaults),
and each lands as its own crate under `crates/` as it is built — a capability
being common does not make it neutral, so conway ships these as things you
install rather than behavior you inherit. Routing, the provider adapters,
session rewind, step-guarding, skills, memory, and MCP support (plus the
out-of-process subprocess plugin host, which the list above does not name)
are the occupants today; compaction remains unbuilt.

**The tier's shape is settled and demonstrated, with nine members shipping
today:** `crates/conway-plugin-skeleton`, a plugin that registers a single
`skeleton_ping` tool and does nothing else — it exists to prove the `Plugin`/
`Tool` mechanism below, not to be useful on its own — `crates/conway-plugin-routing`,
the declarative role-routing engine (ordered fallback chains, capability
filtering, health tracking, circuit breaking) `conway` itself used to compile
in unconditionally, now installed the same way; `crates/conway-plugin-backends`,
the Anthropic-native and OpenAI-compatible provider adapters `conway` itself
used to compile in unconditionally too; and `crates/conway-plugin-history`, `/conway.history.rewind <seq>` —
the owner's ruling that "features like /rewind, /checkout, etc are to be
plugins, to fit into the philosophy; they are not core functionality," built
via `Command::invoke`'s `CommandOutcome::ForkSession` outcome (the TUI's own
mechanism for a plugin command to fork the session driving it, without a
command ever holding a live handle onto any session); and
`crates/conway-plugin-stepguard`, repeated-tool-call detection, which the
agent loop used to carry unconditionally — `PHILOSOPHY.md` §6 leaves loop
intervention to the operator "including writing none", which is only a real
option once declining it is possible. Four more members ship alongside
those five: `crates/conway-plugin-skills` (`conway.skills`), progressive
skill disclosure — a `ContextHook` narrows full-body skill segments to a
one-line index, with a companion `read_skill` tool for the full body on
demand; `crates/conway-plugin-memory` (`conway.memory`), a mutable
`MemoryStore` the model can write to in its own words, injected into
context by a `ContextHook`; `crates/conway-plugin-subprocess`, the
out-of-process plugin host — an external program named in
`[plugins].subprocess[]` is spawned and speaks conway's own wire protocol,
gaining a tool the binary was never compiled with; and
`crates/conway-plugin-mcp`, an MCP-over-stdio *client* — an external
program named in `[plugins].mcp[]` is spawned as an MCP server, and every
tool it declares over `tools/list` attaches the same way, without a
bundle id of its own to name. Compaction, and `/checkout`/`ContextMask`,
remain separate, later work; conway-plugin-routing is not
"dynamic routing" in the learned/adaptive sense PHILOSOPHY.md describes
elsewhere — no classifier, no embedding model, ever — it is the same purely
declarative resolver conway always had, no longer compiled in by default.
With no router plugin installed, `build()` falls back to
`conway_core::routing::MinimalRouter`, an honest, config-only resolver with
no capability or health filtering — see
[`docs/routing.md`](docs/routing.md#installing-a-different-router).
**The backend plugin is the one deliberate exception to "not registered by
default"** named at the top of this section: `conway-cli` attaches both its
`BackendFactory`s without any `plugins.install` entry (see below) — a
missing router or tool plugin costs a capability, but a missing backend
leaves conway unable to reach a model at all, so this one pair ships
attached (owner decision). See
[`docs/providers.md`](docs/providers.md#where-a-backend-is-declared) for how
an operator declines a specific dialect, and how a library embedder using
`conway` alone attaches one instead.

- **Where they live.** A crate per plugin under `crates/`, exactly like the
  workspace's other crates — `cargo test --workspace` covers them the same
  way. `conway` (the facade) never depends on any of them. A `Plugin`/`Tool`/
  `Command` first-party plugin (`conway-plugin-skeleton`,
  `conway-plugin-history`) is written against `conway::plugin`, the
  identical public surface a third-party plugin author gets. A router
  plugin (`conway-plugin-routing`) and the backend plugin
  (`conway-plugin-backends`) are a narrower, different case each:
  `Router`/`HealthRegistry` and `Backend`/`BackendFactory` implementations
  are installed via their own separate identities (`RouterFactory`/
  `ConwayBuilder::with_router_factory`, `BackendFactory`/`ConwayBuilder::
  with_backend_factory`) rather than through `conway::plugin` — `conway`
  still links none of the three crates either way. Each is linked only by
  whatever binary or embedder chooses to install it.
- **How you install one.** A distinct `plugins` section, deliberately not
  folded into `tools.builtin_plugins` (that key names only the four
  compiled-in built-ins and is validated as a closed set; a first-party
  plugin is not a member of it):
  ```json
  { "plugins": { "install": ["conway.plugin_skeleton", "conway.routing", "conway.history", "conway.stepguard"] } }
  ```
  The `conway` binary links its own small bundle of first-party plugin
  crates, router factories, AND backend factories
  (`crates/conway-cli/src/first_party_plugins.rs`, `bundle`/`router_bundle`/
  `backend_bundle`) and resolves each id in `plugins.install` — UNIONED with
  `plugins.default_backends` for the backend factories, since those two
  attach without needing an `install` entry at all — against the three
  together, in one pass, for the TUI and one-shot `-p` alike. An
  unrecognized id is a hard config error naming every linked id it does
  recognize, never a silent no-op, and naming more than one router-factory id
  is rejected too (a build has exactly one router; a backend factory has no
  such limit — a build has a SET of backends). A library embedder instead
  depends on the plugin crate directly and calls `ConwayBuilder::with_plugin`
  (or, for a router/backend, `with_router_factory`/`with_backend_factory`),
  the same calls a third party makes; reading `ConwayBuilder::config().
  plugins.install`/`default_backends` first (as `conway-cli` does) is how an
  embedder offers the identical settings-driven experience.
- **What compatibility they promise.** Versioned with the workspace
  (`version.workspace = true`, same as every other crate here), not
  independently and not held to `conway-core`'s own strict-semver
  discipline — that discipline exists because third-party plugins depend on
  `conway-core`'s port surface, and a first-party plugin is, from `conway`'s
  point of view, just another consumer of the public facade, not a second
  thing granting the stability promise itself. Pre-1.0, a first-party
  plugin's own API can change in any workspace release, same as everything
  else in this tree.

See [`docs/embedding.md`](docs/embedding.md#first-party-plugin-tier) for the
full mechanism, including what an embedder does differently from `conway-cli`.

## Architecture

**[`ARCHITECTURE.md`](ARCHITECTURE.md)** is the full system overview: the
core primitives, the workspace layout, and the data flow of one turn. The
table below is the quick reference.
[`PHILOSOPHY.md`](PHILOSOPHY.md) is the other half of the picture: how the
primitives are meant to be used, and the idioms they were shaped for.

conway is a Cargo workspace of six fixed crates in a ports-and-adapters
layout — the core defines traits (ports), and session/tools are adapters —
plus an open-ended first-party plugin tier (below) that supplies routing and
backend adapters instead of either being compiled into the fixed layout.

| Crate | Responsibility |
|---|---|
| `conway-core` | Domain types and the port traits (`Backend`, `Router`, `PermissionGate`, `SessionStore`, `ContextHook`, tools). No I/O. |
| `conway-session` | The append-only session log and transcript/prefix resolution. |
| `conway-tools` | The tool plugin registry and built-in tools. |
| `conway-runtime` | The per-agent turn loop, context assembly, fork/spawn orchestration. |
| `conway` | The public facade: `ConwayBuilder`, `Conway`, `SessionHandle`, `SessionSpec`. |
| `conway-cli` | The `conway` binary: one-shot mode and the TUI. |

Library users depend on the `conway` facade crate and never reach past it into
the internal crates — the `minimal_session` example is written entirely against
the facade and serves as the API smoke test.

## Development

```console
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets
cargo fmt --all
```

## Licensing

conway is licensed under the **GNU Affero General Public License v3.0 only**
(`AGPL-3.0-only`) — see [LICENSE](LICENSE).

The AGPL is deliberate for an agent harness: if you run a modified conway as a
network service, you must make your modified source available to its users.
This is strong network-copyleft — it is well suited to a self-hosted or
source-available deployment, and it means conway is **not** intended for use as
a permissively-licensed library dependency inside closed-source software.
