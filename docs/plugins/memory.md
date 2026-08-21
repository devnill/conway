# `conway.memory`: a store the model can write to, in its own words

The first-party plugin for `conway::plugin::MemoryStore` (board item
`01M09P2T8E5M292WMSMS64CVC4`), installed by `crates/conway-plugin-memory`.
Depends on [`concepts.md`](concepts.md) for vocabulary (plugin, `ContextHook`,
provenance) and on [`hooks.md`](hooks.md) point 3/4 for the `before_request`
contract this plugin's hook runs through.

## Read this before you install it: durable once selected, but fail-closed

**The CLI wires this plugin to the durable `conway::memory::FsMemoryStore`
when `"conway.memory"` is named in `[plugins].install`** (board item
`01M09V3S2AQYB2VK6MANFRH1JM`, `crates/conway-cli/src/first_party_plugins.rs`'s
`resolve_memory_store`), at `<cwd>/.conway/memory` — alongside the session
store's own `.conway/sessions` default. A memory `remember`ed in one `conway`
invocation is recalled by a later, separate invocation from the same
directory, exactly like session history already is.

**Opening that store is unconditional, not best-effort, once selected.** If
the directory cannot be opened (permissions, a read-only filesystem, or a
binary built without the facade's `jsonl-store` feature), `conway` refuses to
start and reports why on stderr — it never silently falls back to a
non-durable store. If you hit this, either fix the directory/filesystem
problem the message names, or remove `"conway.memory"` from
`[plugins].install` to run without it.

**Unselected, nothing durable is even attempted.** With no
`"conway.memory"` entry in `[plugins].install`, the bundle's (unattached)
candidate is backed by `InMemoryMemoryStore` instead — no I/O, and nothing
that could fail a build you never asked to use this plugin at all.

**Exactly one store instance per process.** Both places this binary needs an
installed `conway.memory`'s backing store (the initial install, and the
TUI/plugin-subcommand's independent re-derivation of the installed set) share
the SAME `Arc<dyn MemoryStore>`, resolved once — never two independent
`FsMemoryStore`s racing over one directory (see `FsMemoryStore::put_lock`'s
own doc, `crates/conway-session/src/memory_store.rs`, for why that would
matter).

An embedder wanting a DIFFERENT root, or per-project isolation, still
constructs `conway::memory::FsMemoryStore::open(root)` itself (behind the
facade's `jsonl-store` feature, on by default) and passes it to
`MemoryPlugin::new` directly rather than going through `[plugins].install` at
all — see "Installing it" below.

## What this is, in one sentence

A mutable, model-writable store of freeform text (`remember`, `forget`,
`list_memories`) that a [`ContextHook`](hooks.md) injects near the front of
every assembled request, so something remembered in one turn — or one
session — can resurface in a later one that shares the same store instance.

## What installing it costs

```json
{ "plugins": { "install": ["conway.memory"] } }
```

Uninstalled, nothing changes: no `remember`/`forget`/`list_memories` tool is
offered, and the context hook is never registered. Opt-in, exactly like every
other member of the CLI's first-party bundle
(`crates/conway-cli/src/first_party_plugins.rs`) — nothing in this tier runs
unasked.

## What it deliberately does not do

- **No summarisation.** `remember`'s text is stored verbatim — no model
  call, no imposed structure, no editing of what you asked it to keep. If you
  want a distillation, write one and pass it to `remember` yourself (or have
  the model do so).
- **No per-session or per-project scoping.** One store is shared globally —
  `list()` takes no scoping parameter. A whole-session label used to be the
  unit ("this conversation is recallable"); that was tried and failed (see
  the crate's own module doc for the five-count postmortem) and is not
  coming back as a per-project variant. An embedder wanting isolation
  constructs one `FsMemoryStore` per project root and installs a separate
  `MemoryPlugin` per `ConwayBuilder` — no port change required.
- **No curation of which session records reach the model.** That is a
  different capability (`conway_core::ports::Curator`), a different seam,
  and untouched by this plugin. Memory *injects* authored text that was
  never a logged record anywhere; it does not *select* which existing
  records survive onto a resolved path.
- **No cache-hint tuning.** Injected memory segments join the static tier of
  the assembled request (cache-friendly by position, at the front) but this
  hook does not set `PromptSegment::cache_hint` — no shipped `ContextHook`
  does, today.

## Its limits, stated plainly

- **Durability: see the top of this page.** Durable at `<cwd>/.conway/memory`
  once `"conway.memory"` is in `[plugins].install`; in-process only
  (`InMemoryMemoryStore`) when it is not, or when an embedder passes that
  type in directly.
- **Injection budget, not a growth-control mechanism.** The hook stops
  injecting (never truncates mid-memory) once either `MemoryConfig::
  max_memories` (default 64) or `MemoryConfig::max_bytes` (default 16384,
  summing `Memory::text.len()`) would be exceeded. This bounds one turn's
  injected text; it does not bound how many memories the store can hold —
  `forget` is the actual answer to unbounded growth, not the cap.
- **A store read failure fails open.** If `store.list()` errors, the hook
  injects nothing that turn rather than failing the request — there is no
  `Failed` outcome for a `ContextHook` to report through.
- **Global visibility, once written.** Any caller sharing the same store
  instance sees every memory in `list()` — there is no per-caller
  partition. Removal is the only way to keep it from surfacing.
- **`list_memories` elides very long text for display only** (past 240
  characters per entry) — the stored memory itself is never touched by
  that truncation.

## Installing it

The off-by-default CLI wiring (durable, at `<cwd>/.conway/memory`):

```json
{ "plugins": { "install": ["conway.memory"] } }
```

An embedder wanting a different root, or per-project isolation, wires the
crate directly rather than going through `[plugins].install` at all:

```rust,ignore
let store = conway::memory::FsMemoryStore::open(root).await?;
let plugin = conway_plugin_memory::MemoryPlugin::new(
    Arc::new(store),
    conway_plugin_memory::MemoryConfig::default(),
);
let conway = ConwayBuilder::from_parts(config)
    .with_plugin(Arc::new(plugin))
    .build()?;
```

`FsMemoryStore` lives in `conway-session` and is re-exported through the
facade as `conway::memory::FsMemoryStore`, behind the `jsonl-store` feature
(on by default for the `conway` crate).

## Using it, once installed

Three tools appear: `remember(text)`, `forget(id)`, `list_memories()`. A
model (or a script driving the same tool surface) calls `remember` to keep a
fact, `list_memories` to find an id worth forgetting, and `forget` to retire
it. There is no separate "recall" tool — recall is automatic: every
assembled request gets whatever the injection budget allows, oldest-first,
with no action required.

## Trust

No new trust mechanism. `remember`/`forget` are `PermissionClass::
RequiresApproval` (they mutate a shared store); `list_memories` is
`PermissionClass::Safe`. The store itself is exactly as trusted as any other
in-process plugin state — see [`trust-and-security.md`](trust-and-security.md)
for what a trusted plugin can and cannot do with your session.
