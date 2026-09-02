# `conway.idiom`: prepends a short conway-idioms instruction fragment

The first-party plugin for `Plugin::instructions()` (board item
`01M0VR3BKW5N3V3WS28H7FV8ZK`), installed by `crates/conway-plugin-idiom`.
Depends on [`concepts.md`](concepts.md) for vocabulary and on
[`hooks.md`](hooks.md) point 17 for `Plugin::instructions()`'s own contract,
precedence, and reach.

## What this is, in one sentence

A plugin that prepends a short, conway-specific instruction fragment near
the front of a session's assembled context — the operator's own framing:
*"this is a plugin which prepends a custom system prompt. Currently we send
minimal data, and the purpose of this is to add a little extra if
desired."* A little extra: 28 lines, 275 words, well inside a 40-line/
400-word budget measured against Pi's own system-prompt template
(`docs/vision/INTENT.md`'s citation of Pi as conway's extension-surface
reference).

## Why this exists

`App::session_spec` (the interactive TUI's own session construction path)
sets `role`/`keep_alive`/`tools`/`model` and never sets `agent_def` or
`system_prompt_override`. `SessionSpec::system_prompt_override`'s own doc
states the consequence directly: no system-prompt segment at all when
`agent_def` is also absent. A bare interactive session therefore sends the
model tool schemas and the conversation, and nothing telling it what
harness it is in — what fork and spawn are, how an agent ends, that the
tool set is configuration-dependent. `Plugin::instructions()` already
existed as a mechanism (board item `01M0K5MD59YZRSHE31JKZKFRMY`); this
plugin is its first content-bearing occupant beyond a tool's own
`when-to-use` note.

## What installing it costs

```json
{ "plugins": { "install": ["conway.idiom"] } }
```

Uninstalled, nothing changes: no instruction fragment is contributed, and a
bare interactive session keeps sending no system-prompt segment at all,
exactly as before this plugin existed. Opt-in at the harness level, like
every other member of the first-party tier — but **installed by default at
first-run** by the `conway` binary's own guided setup (board item
`01M1FS34GNZEVZP4ZBVC90VD6J`, decision `01M1FQFP5D0R3M9GC8R8Z24F5N`,
2026-09-01): a fresh operator gets this fragment without asking for it by
name, alongside five siblings — see
[`docs/getting-started.md`](../getting-started.md#installing-a-first-party-plugin)
for the full set and how to remove any one of them. That is a property of
the binary's first run, not of `ConwayBuilder::build()` or the harness's
own defaults, which are unchanged by this item.

## Where it lands, and why

`ContextBuilder::build` renders every `InstructionFragment` at one of two
positions relative to `[0] SystemPrompt` (an agent def's own prompt, or a
session's `system_prompt_override`/one-shot `--system-prompt` override):
`BeforeSystemPrompt` or `AfterSystemPrompt` (`hooks.md` point 17 has the
full ordering contract). This plugin's shipped base fragment
(`conway.idiom.base`) declares `BeforeSystemPrompt` with `order: -100` —
it renders **genuinely first**, ahead of even a curated `AgentDef`'s own,
deliberately-authored prompt, and ahead of everything else in the
assembled context when there is no `[0]` at all (a bare interactive
session, this plugin's own primary case). Installing more than one
`BeforeSystemPrompt`-declaring plugin orders their fragments by `order`
first, then `[plugins].install`/`with_plugin` install order for a tie;
nothing here guarantees `conway.idiom.base` renders first among several
against a third-party plugin that also declares `BeforeSystemPrompt` with
a lower `order`.

The operator's own project/global fragments (`conway.idiom.operator.
project`/`conway.idiom.operator.global`, described below) stay at the
`AfterSystemPrompt` default — an agent def's own prompt still precedes an
operator's own standing instructions, which is the ordering an operator
authoring both would expect.

**This used to be argued the other way.** Before `InstructionFragment::
position` existed, every fragment rendered at a single fixed slot after
`[0]`, so the base fragment's own doc argued (correctly, for what the
runtime could deliver at the time) that landing there was "the right
place" and that landing ahead of an agent def's own prompt "does not change
`ContextBuilder::build`'s assembly order... a follow-up, not something this
plugin does by reordering itself." The fragment position/order/scope item
(process record `01M1FQ36PCW2J19AP219GKZH3R`) is that follow-up: the
runtime now lets a fragment say which side of `[0]` it wants, and this
plugin's base fragment says "ahead."

## What the fragment covers, and what it deliberately does not name as a tool dependency

Fork vs. spawn, how an agent ends (`report`, or plain text for a
`report`-less interactive root), configuration-dependent tools, context
scarcity, permissions, budgets, and steering — see
`crates/conway-plugin-idiom/fragments/idiom.md` for the fragment's own
exact text.

`tool_ids` is empty, deliberately. `ContextBuilder::build`'s reachability
check withholds a fragment's text **entirely** when any id in its
`tool_ids` is not among the turn's announced tools — one missing tool
would silently drop the whole paragraph. The fragment names
`conway_fork`/`conway_spawn`/`report` in prose, but nothing in it requires
the model to be able to call any specific one for the rest of the text to
still hold — and an interactive root specifically never has `report`
(`App::session_spec`'s own `ToolSelector::Except(vec!["report".into()])`),
so naming `report` in `tool_ids` would make the fragment vanish from the
one session type this plugin exists for.

## Reach: every agent, root or child — a ruling, not a bare description

**A forked or spawned child sees this text too, not the root alone.**
`SubagentHost::start` now resolves a fork/spawn child's `AgentSpec.
instructions` through the same `resolve_instructions` function
`start_root`/`resume_root` already call — board item
`01M0VSKA76NSEHDSH25XJGJ2J5`'s ruling, argued at that function's own doc
(`crates/conway-runtime/src/runtime/root.rs`): a plugin instruction
fragment is harness configuration keyed to tool reachability (the
pre-existing `tool_ids` gate, unchanged by this ruling), not transcript
context, so fork/spawn's "whole transcript vs. empty transcript" split
does not govern it — the same way it already does not govern
`plugin_config`, which narrows-and-inherits from the parent for spawn
exactly as for fork, predating this ruling.

Part of the fragment describes how a *child* should behave — ending a turn
with `report`, reasoning about a permission denial, expecting a parent to
steer or cancel it — and a child is exactly the agent most likely to need
it. That is now the audience that receives it, stated here, in
`PluginDescription::you_get`, and in the crate's own module doc, rather
than left to be discovered.

Before this ruling, the absence was *disclosed* (this page, `hooks.md`
point 17, the fragment's own shipped text, `PluginDescription::you_lose`)
but never *decided* — nobody had argued whether a child SHOULD receive it.
The board item argued it in full; this page, and the other three sites
just named, are the record of that decision, corrected to match.

## An operator's own standing instructions (board item `01M0VR4GMGSZ2682T908JCGVFG`)

Beyond the shipped fragment above, this plugin also reads an operator's own
`instructions.md` — house conventions, what this repository is, how an
operator wants the model to behave — the file-based lever Pi's `AGENTS.md`/
`SYSTEM.md` establish as precedent, applied here rather than a new
`[plugins]` config key: `PluginsConfig` is `#[serde(deny_unknown_fields)]`
with exactly four fields and no per-plugin operator configuration surface
exists anywhere in conway yet (`conway-plugin-trim`'s own bundle entry
names the identical gap), so a config key would be a schema change
contending with other work, for text that has no reason to be a TOML value
in the first place.

**Location, and why it is one file at each of two scopes, not a search
path.** `<project>/.conway/instructions.md`, matching the direct-`cwd`-join
convention `.conway/agents/`/`.conway/skills/` already use (never walked up
an ancestor chain, unlike Pi's own multi-directory merge — conway's project
file convention never walks upward for `.conway/*`, so a search path would
be new shape for this file alone). And, additionally, the SAME directory
`conway::config::discovery::user_config_path` resolves `settings.json`
into, filename swapped, at zero cost in new dependencies or schema — that
is `<home>/.conway/instructions.md` when `CONWAY_CONFIG_DIR` is unset, and
`<CONWAY_CONFIG_DIR>/instructions.md` when it is set (board item
`01M0W5Q569F0T97HSEP6F0MPCR`, closing an isolation gap identical in shape
to the one board item `01M0VV6CVSZM4XH8J4G6EBV5E3` closed for
`settings.json` itself: an operator or embedder relocating conway's
user-config layer relocates this file with it too, not only
`settings.json`). Both files are read when
present, and **both are additive — neither one's presence disables the
other**, unlike `settings.json`'s project-overrides-user merge: an operator
who has authored both a house-wide preference and a per-project convention
gets both, as two separately named fragments (`conway.idiom.operator.
project`, `conway.idiom.operator.global`) so `/context` shows each one's
own token cost rather than one opaque combined number. (When a project
genuinely lives at the operator's own home directory, the two paths name
the same file; the global fragment collapses away rather than injecting
the same text twice.)

**Missing is silent; unreadable is not.** No file at either scope,
or a file that is empty/whitespace-only, contributes nothing and is not an
error — exactly conway's pre-existing behavior. A file that exists but
cannot be read cleanly — a permissions error, the path naming a directory,
invalid UTF-8 — fails the build loudly instead, naming the path, the same
tier a malformed `.conway/skills/*/SKILL.md` already fails at. A file the
operator wrote and conway silently ignored is exactly the failure mode this
project cares most about.

**Reaches a forked or spawned child too, on the identical footing as the
shipped fragment above** — board item `01M0VSKA76NSEHDSH25XJGJ2J5`'s
ruling applies uniformly to every `Plugin::instructions()` fragment this
plugin declares, operator-authored or shipped alike; there is no separate
rule for the operator's own text, and no flag to opt a child out of it.

**Provenance, stated rather than fixed.** Every fragment this plugin
contributes — including an operator's own project/global text — is
stamped `Provenance::Skill { name }` once assembled
(`crates/conway-runtime/src/context/builder.rs`), the SAME stamp an
operator-authored `.conway/skills` body gets. Plugin attribution lives
only in the parallel `ContextReport::instruction_fragments` list, a
side-channel, not durable provenance — so an operator's own words, merely
read by this plugin, are attributed in the durable log to "a skill" and in
`/context`'s report to `conway.idiom`, wrong in both directions. Not fixed
here: a `Provenance::Operator` variant is a persisted wire-format change
(precedent: `Provenance::CommandPrompt`, added the same day for a
different feature) and is its own decision.

**Replacing, not adding, is still the flag's job.** `--system-prompt`/
`--append-system-prompt` (`crates/conway-cli/src/cli.rs`) reach
`SessionSpec::system_prompt_override`, which REPLACES the whole `[0]
SystemPrompt` segment — the answer for an operator who wants to replace
the system prompt outright, on the one-shot path. This plugin's file is
additive, alongside every other declared fragment, and does not attempt to
answer "replace" a second way.

## Seeing it in `/context`

`/context`'s preamble section (`crates/conway-cli/src/tui/commands.rs`)
renders every plugin-declared instruction fragment this turn's assembly
considered — `conway.idiom.base` always, plus `conway.idiom.operator.
project`/`conway.idiom.operator.global` whenever the corresponding file
exists — each with its source plugin, its estimated token cost, and (had
it been withheld) which tool id made it unreachable.
