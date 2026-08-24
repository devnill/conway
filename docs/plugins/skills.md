# `conway.skills`: progressive skill disclosure

The first-party plugin for narrowing full-body skill context, installed by
`crates/conway-plugin-skills`. Ports `docs/plugins/cookbook.md` example 4
into an installable, off-by-default plugin — that cookbook example is the
worked-out design; this page is the shipped form. Depends on
[`concepts.md`](concepts.md) for vocabulary and [`hooks.md`](hooks.md) point
3 for the `ContextHook::before_request` contract this plugin's hook runs
through.

## What this is, in one sentence

A [`ContextHook`](hooks.md) that narrows every full-body
`Provenance::Skill { name }` context segment down to a one-line
`name: description (call read_skill(name="...") for the full document)`
index entry, paired with a `read_skill` tool that returns the full document
on demand.

## What installing it costs

```json
{ "plugins": { "install": ["conway.skills"] } }
```

Uninstalled, `ContextBuilder`'s full-body `Skill` segments reach the model
completely unchanged — the runtime's context hook is simply never set. This
is not a recommended default: the crate's own module doc calls it "a
plausible efficiency win, not a measured one" (restated from the cookbook
example it ports). Installing it demonstrates the architecture does not
stand in the way of progressive disclosure; it does not claim the trade-off
is worth it for you.

## How it reaches skill bodies — no privileged channel

Both halves (the hook and the `read_skill` tool) share one
`Arc<HashMap<String, SkillDef>>`, loaded via the same public
`conway::skills::load_skill_defs` function the facade's own builder uses. A
third-party plugin could load the identical map the identical way — this
plugin reaches nothing runtime-internal.

## What it deliberately does not do

- **It never hard-fails an unknown skill name.** `read_skill(name="typo")`
  returns `is_error: true` with a "no such skill" message a model can read
  and recover from — never a crash, never a denied call.
- **It never drops or masks an unindexed skill segment.** A
  `Provenance::Skill` segment whose name is not in this plugin's own map is
  left **completely unchanged** rather than narrowed or removed — fail
  *safe* means "leave the model with what it already had," the opposite
  direction from a hook that spills bulky output to a file.
- **It does not load skills from the runtime's own directory scan.** Its map
  is its own copy, built once at plugin-construction time
  (`SkillsPlugin::from_dir`); it does not read `Runtime.skills` or any
  runtime-internal state.

## Its limits, stated plainly

- **A description-less skill still narrows**, just without the `: description`
  clause (`name (call read_skill(name="...") for the full document)`) — there
  is no third state for "skip narrowing this one".
- **No cache-hint tuning**, the same limitation `conway.memory`'s hook
  shares: narrowing happens after `ContextBuilder::build`, and no shipped
  `ContextHook` sets `PromptSegment::cache_hint`.
- **`SkillsPlugin` has no `Default`.** An empty skills map would narrow
  nothing and serve only "no such skill" replies — a uselessly-installed
  plugin rather than a sensible default — so it must be constructed
  explicitly via `SkillsPlugin::new` or `SkillsPlugin::from_dir`.

## Installing it

```json
{ "plugins": { "install": ["conway.skills"] } }
```

The CLI wires this from `.conway/skills` under your working directory (the
same directory `ConwayBuilder::build` itself loads skills from) — see
`crates/conway-cli/src/first_party_plugins.rs`'s `bundle()`. A missing
skills directory yields an empty-skills plugin (narrows nothing, serves "no
such skill" for every call); a malformed `SKILL.md` fails the whole build
loudly rather than silently degrading this plugin.

An embedder with its own skills directory:

```rust,ignore
let plugin = conway_plugin_skills::SkillsPlugin::from_dir(&skills_dir)?;
let conway = ConwayBuilder::from_parts(config)
    .with_plugin(Arc::new(plugin))
    .build()?;
```

## Trust

No new trust mechanism — `read_skill` is `PermissionClass::Safe` (a pure
read of an already-configured skill file), and the hook only rewrites
already-assembled context, the same seam every other `ContextHook` runs
through. See [`trust-and-security.md`](trust-and-security.md) for what a
trusted plugin can and cannot do more generally.
