# conway

**A Rust agent harness for agentic coding.**

conway runs LLM-driven agents that use tools, spawn and fork child agents, and
route across multiple model backends — with explicit permissions, an
append-only session log, and no hidden context manipulation. It ships as a
library (the `conway` facade crate) and a CLI/TUI (`conway`).

License: **AGPL-3.0-only** (see [Licensing](#licensing)).

> Status: `0.2.0` is the current release. See [CHANGELOG.md](CHANGELOG.md).

---

## What it does

- **Hierarchical agents.** An agent can **fork** (a child inherits the parent's
  full context, frozen at the fork point) or **spawn** (a child starts from a
  clean slate). Context flows one way only — there is no bleed back into the
  parent; cross-agent communication is explicit (steer / report envelopes).
- **Capability-based routing.** Requests route to a model by role and by the
  model's declared capabilities (context window, tool-calling, reasoning),
  with per-transport and per-probe **circuit breakers** and an ordered
  fallback chain.
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

## Configuration

conway discovers configuration with increasing precedence:

```
built-in defaults  <  ~/.conway/settings.json  <  ./.conway/settings.json  <  env  <  CLI flags
```

`settings.json` declares backends (OpenAI-compatible and Anthropic dialects,
including local servers such as Ollama), roles and their model chains, routing,
permissions, and limits. The library equivalent is
`ConwayBuilder::discover()` / `ConwayBuilder::from_config(path)`.

## Architecture

**[`ARCHITECTURE.md`](ARCHITECTURE.md)** is the full system overview, and
**[`docs/crates/`](docs/crates/)** has a detailed doc for each crate. The table
below is the quick reference.

conway is a Cargo workspace of eight crates in a ports-and-adapters layout —
the core defines traits (ports), and backends/session/tools are adapters.

| Crate | Responsibility |
|---|---|
| `conway-core` | Domain types and the port traits (`Backend`, `Router`, `PermissionGate`, `SessionStore`, `ContextHook`, tools). No I/O. |
| `conway-backends` | Backend adapters: Anthropic and OpenAI-compatible (OpenAI, Ollama, …) dialects. |
| `conway-routing` | Capability-based router, circuit breakers, health probes. |
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
