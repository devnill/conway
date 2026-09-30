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

## Self-authoring skills: proposing a `SKILL.md` from what just happened

Two slices build one feature (board items `01M1YVYMGARGM2CYH06C1GXGSS`/
CATALOGUE #7): **slice 1** watches the live event stream
(`Plugin::observe_sink`) and decides, mechanically, whether a task looked
like real work (a tool-call count crossing a configurable threshold, a
tool error followed by a success, or an operator correction — never an
LLM judging its own complexity, per INTENT.md §5a). **Slice 2**, this
section, turns that decision into a proposed skill the operator can accept,
edit, or throw away, and writes a file only on an explicit `Enter`.

### The two ways a proposal happens

1. **`/conway.skills.propose` (the primary, interactive path).** Type it in
   the TUI at any point. It forks an ephemeral child with the directive
   "write a SKILL.md for the procedure just used, or say none is
   warranted," discarded the instant its answer is captured — the same
   "distillate only" semantics `/ask`'s own discard fate has, applied
   unconditionally rather than as one of three choices (there is no
   fork/pull-in fate here; the ephemeral child is never kept).
2. **The automatic trigger, for a one-shot (`conway -p`) run only.** After
   slice 1 publishes its verdict for the session's root agent, a one-shot
   run reflects the same way `/conway.skills.propose` does and shows the
   result at the terminal (a plain `y`/`N` prompt on stderr, never through
   the TUI's modal machinery, since there is no TUI in this dispatch mode).

### Exactly when the automatic trigger fires, and when it does not

**It fires only for a non-`keep_alive` root — in practice, only a one-shot
`conway -p` run (or a delegated/forked root spec that also declines
`keep_alive`).** It does **not** fire inside the interactive TUI's own
session. The reason is structural, not a missing feature: slice 1's trigger
is anchored on `Event::AgentFinished`, the one event that carries an agent
id a global observer can use — and for a `keep_alive: true` root (what the
TUI always runs), `AgentFinished` fires only when the **whole session
ends** (the operator quits), not after each prompt. By the time that event
fires, the TUI process is on its way out — too late to show a modal to an
operator who is still there. Threading true per-prompt identity through
`observe_sink` would require an `Envelope`-carrying change to
`conway-core`'s plugin port, deliberately out of scope for this feature
(named as the honest follow-up, not worked around).

This is why `/conway.skills.propose` is the primary, not merely an
alternate, path for interactive use: it is the only route that reaches an
operator who is still at the keyboard.

### The modal (or, for a one-shot run, the terminal prompt)

- **`Enter`** writes the proposal verbatim to
  `.conway/skills/<name>/SKILL.md`, creating the directory if needed.
- **`e`** opens the proposal in `$VISUAL`/`$EDITOR`/`vi` first (the same
  mechanism `Ctrl-G` uses for the input line) — the edited text replaces
  the modal's content (body/description only; the write TARGET, derived
  from the proposal's own `name:` field at propose time, does not change
  even if the edited text's `name:` line does, a deliberate guard against
  a free-text edit silently retargeting the write).
- **`Esc`** discards. Nothing is written.
- **Updating an existing skill of the same name shows a diff**, not a
  silent replacement — the same unified-diff renderer `/diff` uses. A
  brand-new skill shows its full body instead (there is nothing to diff
  against).

### Configuring it

```json
{ "plugins": { "config": { "conway.skills": { "write_approval": "ask" } } } }
```

`write_approval` takes exactly two values:

- **`ask`** (the default): show the proposal, write only on `Enter`.
- **`never`**: suppress the feature entirely — no fork, no modal, no
  tokens spent, from either surface.

**There is deliberately no `always`.** An unattended write is out of scope
for this feature on its own terms, not merely unimplemented — the spec
this shipped against says so explicitly, and `write_approval` refuses the
literal string `"always"` by name (a typo-shaped config error, not a
silently-accepted third mode) rather than mapping it to anything.

### The fork's own budget

The ephemeral child this feature forks — from either surface — is bounded:
**15 steps, and a 180-second deadline.** Both are round, plausible numbers
for "read back over what just happened and write a couple of paragraphs,"
not measured calibrations (the same "plausible, not proven" disclosure
slice 1's own `tool_call_threshold` default carries) — an operator whose
workload needs a wider bound has no config knob for it yet; state so here
rather than silently. The point is bounding the *unrequested* tokens a
proposal costs, not tuning them precisely: a reflection that goes sideways
(the model tool-calling instead of just answering, or stalling) cannot
spend an unbounded number of the operator's own tokens or wall-clock
seconds before the modal even appears.

### How a write is recorded

Every successful write is recorded as an operator-visible notice —
a transcript entry in the TUI (`conway.skills: wrote
.conway/skills/<name>/SKILL.md`), a stderr line in one-shot mode. This is
disclosed as a **deliberate scope decision**, not the originally-imagined
mechanism: there is no facade primitive that lets an external caller
(`conway-cli`, here) append a durable `LogRecord::SystemNote` to a live
session without starting a new turn — that machinery lives entirely inside
`conway-runtime`'s own agent loop, reached only from inside a turn's own
processing. Building one is a `conway-core`/`conway-runtime` change this
feature's own fence puts out of scope; the notice is real and
operator-visible, just not persisted into the session's own append-only
log the way, e.g., a budget-exceeded warning is.

### What a written skill needs to look like

Whichever surface proposes it, the written file is the complete
`SKILL.md` document — YAML frontmatter (`---`-delimited) naming `name`
(lowercase letters, digits, and hyphens only — anything else, including a
path-traversal attempt like `../../etc/x`, is refused as malformed, never
silently sanitized) and, optionally, `description`, followed by the body —
the exact shape this crate's own loader (`conway::skills::load_skill_defs`)
already expects for an operator's own `.conway/skills/<name>/SKILL.md`. A
written skill therefore shows up in the **next** session's skill index
(narrowed to a one-line entry by this plugin's own hook, if installed) with
no translation step.

### Known limitations (disclosed, not closed)

- **No re-read-before-write check.** Between the diff shown in the modal
  (or at the terminal) and the moment `Enter`/`y` actually writes, nothing
  re-reads `.conway/skills/<name>/SKILL.md` to confirm it still matches
  what the diff was computed against — a second writer racing the same
  name in that window loses silently, the same TOCTOU shape an ordinary
  shell redirect (`>`) already has. Not closed in this feature: skill
  proposals are an infrequent, operator-attended action, not a
  high-concurrency write path.
- **Not an atomic write.** The file is written with a plain
  `std::fs::write`, not a temp-file-plus-rename — a process interrupted
  mid-write (a `SIGKILL`, a full disk) can leave a truncated or partial
  `SKILL.md` rather than either the old or the new content cleanly. A
  future write path could close both of these the same way, if either
  becomes a real operator complaint rather than a theoretical one.
