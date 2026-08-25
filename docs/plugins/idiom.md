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
desired."* A little extra: 27 lines, 250 words, well inside a 40-line/
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
exactly as before this plugin existed. Opt-in, like every other member of
the first-party tier.

## Where it lands, and why

`ContextBuilder::build` assembles `[0] SystemPrompt` (an agent def's own
prompt, or a session's `system_prompt_override`), then `[1]
PluginInstructions*` (every installed plugin's own fragments, in install
order), then `[1b] SkillFragments*`, then tool schemas and the
conversation. This plugin's fragment lands in `[1]` — ahead of every tool
schema and every logged turn, which is what "prepend" means against the
conversation. It lands after `[0]` only when an agent def supplies its own
system prompt, which keeps that def's own, deliberately-authored prompt
first and this plugin's generic harness orientation immediately after —
for the plugin's own primary case, a bare interactive session with no `[0]`
segment at all, `[1]` **is** the front of the whole assembled context.
Installing more than one instruction-declaring plugin orders their
fragments by `[plugins].install`/`with_plugin` order; nothing here
guarantees this fragment renders first among several.

This item does not change `ContextBuilder::build`'s assembly order. If an
operator's own sense of "prepend" turns out to mean ahead of an agent def's
own system prompt too, that is a runtime change affecting every consumer
of the context builder — a follow-up, not something this plugin does by
reordering itself.

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

## Reach: root agents only

**A forked or spawned child never sees this text.** `resolve_instructions`
and `SubagentHost::start` both give every child `instructions:
Vec::new()` unconditionally (`hooks.md` point 17's own disclosed caveat).
Part of the fragment describes how a *child* should behave — ending a turn
with `report`, reasoning about a permission denial, expecting a parent to
steer or cancel it — and a child is exactly the agent that never receives
it. This plugin does not fix that gap; it ships the content anyway, with
the limitation stated here, in `PluginDescription::you_lose`, and in the
crate's own module doc, rather than leaving it to be discovered.

## Seeing it in `/context`

`/context`'s preamble section (`crates/conway-cli/src/tui/commands.rs`)
renders every plugin-declared instruction fragment this turn's assembly
considered, named `conway.idiom.base` — its source plugin, its estimated
token cost, and (had it been withheld) which tool id made it unreachable.
