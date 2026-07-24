# conway docs

Start at [`/ARCHITECTURE.md`](../ARCHITECTURE.md) — the current, maintained
architectural overview: what conway is, the 8-crate workspace and its
dependency direction, the core primitives (fork/spawn, the append-only
session log and context assembly, capability-based routing, tools-as-plugins
behind a permission gate, `ContextHook`, keep-alive sessions, `/ask`,
wire-layer reasoning support), and the data flow of one turn.

## Per-crate docs

`docs/crates/` holds one design doc per workspace crate, each covering that
crate's scope, the ports it provides or consumes, and implementation notes
that don't belong at the architecture-overview level:

- [`docs/crates/conway-core.md`](crates/conway-core.md) — domain types and
  port traits.
- [`docs/crates/conway-backends.md`](crates/conway-backends.md) — Anthropic
  and OpenAI-compatible backend adapters.
- [`docs/crates/conway-routing.md`](crates/conway-routing.md) — capability
  routing, circuit breakers, health probes.
- [`docs/crates/conway-session.md`](crates/conway-session.md) — the
  append-only session log and transcript/prefix resolution.
- [`docs/crates/conway-tools.md`](crates/conway-tools.md) — the plugin/tool
  registry and built-in tools.
- [`docs/crates/conway-runtime.md`](crates/conway-runtime.md) — the agent
  loop, context assembly, fork/spawn orchestration.
- [`docs/crates/conway.md`](crates/conway.md) — the public facade.
- [`docs/crates/conway-cli.md`](crates/conway-cli.md) — the `conway` binary
  (one-shot mode and the TUI).

All eight are written; each covers that crate's responsibility and
boundary, its public interfaces, key types and invariants, and links back
up to `/ARCHITECTURE.md` and across to sibling crate docs.

## Historical planning material

conway's inception-era planning documents (frozen at 0.1.0) have been
retired: their durable design content — types, interfaces, invariants,
data flow — was audited and migrated into `/ARCHITECTURE.md` and
`docs/crates/*.md`, and their planning-era rationale ("why we chose X")
lives in this project's ideate decision/journal records instead.
