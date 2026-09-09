# `conway.idiom`: prepends a short conway-idioms instruction fragment

The first-party plugin for `Plugin::instructions()` (board item
`01M0VR3BKW5N3V3WS28H7FV8ZK`), installed by `crates/conway-plugin-idiom`.
Depends on [`concepts.md`](concepts.md) for vocabulary and on
[`hooks.md`](hooks.md) point 17 for `Plugin::instructions()`'s own contract,
precedence, and reach.

## What this is, in one sentence

A plugin that prepends two short, conway-specific instruction fragments near
the front of a session's assembled context — a session-static environment
block (`conway.idiom.environment`, below) genuinely first, then this
plugin's original idioms primer (`conway.idiom.base`) immediately behind
it — the operator's own framing for the latter: *"this is a plugin which
prepends a custom system prompt. Currently we send minimal data, and the
purpose of this is to add a little extra if desired."* A little extra: the
idioms primer is 40 lines, 357 words (raw source, `<!-- tools: -->` markers
included), at the 40-line/400-word budget measured against Pi's own
system-prompt template (`docs/vision/INTENT.md`'s citation of Pi as
conway's extension-surface reference); the environment block (board item
`01M1FSTQT952QYM014G65EVW25`) is a single short sentence.

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

Uninstalled, nothing changes: neither instruction fragment is contributed,
and a bare interactive session keeps sending no system-prompt segment at
all, exactly as before this plugin existed. Opt-in at the harness level,
like every other member of the first-party tier — but **installed by
default at first-run** by the `conway` binary's own guided setup (board
item `01M1FS34GNZEVZP4ZBVC90VD6J`, decision `01M1FQFP5D0R3M9GC8R8Z24F5N`,
2026-09-01): a fresh operator gets both fragments without asking for
either by name, alongside five siblings — see
[`docs/getting-started.md`](../getting-started.md#installing-a-first-party-plugin)
for the full set and how to remove any one of them. That is a property of
the binary's first run, not of `ConwayBuilder::build()` or the harness's
own defaults, which are unchanged by this item.

**Token cost, per turn:** the environment block is one short sentence, and
— because it is computed exactly once at plugin construction and never
changes for the life of the session (see "The environment block" below) —
it is the cheapest possible thing to have sit ahead of the rest of the
context on a prompt cache: after the first turn, it costs nothing extra to
re-send, because the bytes are identical to what the cache already holds.
The idioms primer costs its unconditional body plus whichever of its
bash/report/conway_fork-gated sentences this turn's own tool set makes
reachable (board item `01M1FSRJJAB3ZYZXED4SVT2ZSF`) — never the full
~360-word raw source at once, only the parts that actually apply. Plus
whatever an operator's own `instructions.md` file(s) cost, when either
exists. `/context`'s preamble section names `conway.idiom.environment`,
`conway.idiom.base`, `conway.idiom.operator.project`, and
`conway.idiom.operator.global` separately, each with its own exact token
cost.

## Where it lands, and why

`ContextBuilder::build` renders every `InstructionFragment` at one of two
positions relative to `[0] SystemPrompt` (an agent def's own prompt, or a
session's `system_prompt_override`/one-shot `--system-prompt` override):
`BeforeSystemPrompt` or `AfterSystemPrompt` (`hooks.md` point 17 has the
full ordering contract). This plugin ships TWO `BeforeSystemPrompt`
fragments, ordered against each other by `order`: the environment block
(`conway.idiom.environment`) declares `order: -200`, and the idioms primer
(`conway.idiom.base`) declares `order: -100` — so the environment block
renders **genuinely first**, immediately followed by the idioms primer,
both ahead of even a curated `AgentDef`'s own, deliberately-authored
prompt, and ahead of everything else in the assembled context when there
is no `[0]` at all (a bare interactive session, this plugin's own primary
case). Installing more than one `BeforeSystemPrompt`-declaring plugin
orders their fragments by `order` first, then `[plugins].install`/
`with_plugin` install order for a tie; nothing here guarantees either of
this plugin's own two fragments renders first among several against a
third-party plugin that also declares `BeforeSystemPrompt` with a lower
`order`.

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

## The environment block (board item `01M1FSTQT952QYM014G65EVW25`)

A model running inside conway was previously never told what harness it is
in, what directory it is working from, what OS it is on, or what day it
is. `conway.idiom.environment` closes that gap with a single sentence,
naming exactly four facts:

- **`cwd`** — the session's own working directory, absolute, verbatim.
- **`os`/arch** — `std::env::consts::OS`/`ARCH` (e.g. `macos aarch64`,
  `linux x86_64`).
- **`date`** — the session's start date (`YYYY-MM-DD`), explicitly labeled
  "session start" in the sentence itself.
- **`git`** — the current branch (or, in detached `HEAD` state, a short
  commit sha), when `cwd` is inside a git work tree; omitted entirely,
  never guessed at, when it is not.

**It is a snapshot, not a clock.** Every one of these facts is computed
exactly once, when the plugin is constructed (at session/agent-loop
startup), and never again — `Plugin::instructions()` returns the
byte-identical rendered text every time it is called for the life of that
plugin instance. **A session that happens to cross midnight keeps
reporting its ORIGINAL start date**, not the date the model's clock would
now read; conway does not re-derive this block mid-session, on a fork, or
on a spawn (a forked/spawned child inherits its own copy, computed at ITS
OWN construction, immediately after the parent's — normally the same
instant, for all practical purposes).

**Why "session-static" is the entire design, not an oversight.** An
`InstructionFragment` declared `BeforeSystemPrompt` with a very negative
`order` sits ahead of everything else in the assembled context — which
means it sits ahead of everything a prompt cache keys its cached prefix
on. If this block's text changed from one turn to the next, EVERY turn
would invalidate the cache for the entire rest of the context behind it,
turn after turn, defeating the purpose of a prefix cache existing at all.
Keeping this block byte-identical across a session's whole lifetime is
what lets the rest of the context still benefit from caching.

**Deliberately excluded, each for a reason tied to the property above (or
to declaration honesty):**

- **Clean/dirty git status.** This changes on nearly every turn (the
  model's own edits dirty the tree) — exactly the kind of per-turn churn
  this block exists to avoid. If you need this, `bash`/`git status` is the
  live answer, not a prompt fragment.
- **Model, context window, or budget headroom.** Not knowable at plugin
  construction time (construction precedes route resolution), and already
  covered by a different, per-turn mechanism — see
  [`interactive.md`](../interactive.md#runway-notices)'s "runway notices."
  Restating it here would be a second, potentially stale source of truth
  for the same number.
- **A tool list.** The wire-level tool schema announcement already states
  exactly which tools this turn can call.
- **Hostname, username, or any other environment variable.** No consumer
  needs either; nothing here is added "just in case."

**No new dependency, no subprocess.** Git facts come from parsing
`.git/HEAD` directly (plain text, `std::fs` only) — never a `git`
subprocess call, never the `git2` crate. A linked worktree checkout's own
`.git` is a FILE (`gitdir: <path>`, not a directory) naming where its REAL
`HEAD` actually lives; this plugin follows that pointer, so a worktree
checkout correctly reports the worktree's own current branch, not the
main checkout's.

## What the fragment covers, and how per-part gating replaced the tool_ids trap

Fork vs. spawn, ending a turn, configuration-dependent tools, context
scarcity, permissions, budgets, and steering — see
`crates/conway-plugin-idiom/fragments/idiom.md` for the fragment's own
exact text. Those bullets are the fragment's unconditional **body**
(`InstructionFragment::text`): always rendered, regardless of which tools
this turn's session announces.

**The budgets bullet's promise is made true by the runtime, in the same
change that wrote it** (declaration honesty): the fragment tells the model
it will be warned at 50/75/90% of the context window and within 20% of any
budget's limit, and `conway_runtime::runway` (`crates/conway-runtime/src/
runway.rs`) is the mechanism that actually sends those warnings — see
[`interactive.md`](../interactive.md#runway-notices) for what one looks
like on the wire and [`scripting.md`](../scripting.md#budget-flags) for the
non-interactive framing.

**Board item `01M1FSRJJAB3ZYZXED4SVT2ZSF` closed the gap this section used
to disclose here.** Before that item, `InstructionFragment` had exactly one
whole-fragment `tool_ids` list, and `ContextBuilder::build`'s reachability
check withheld a fragment's text **entirely** the moment any one id in it
was unreachable — so this plugin's base fragment had to leave `tool_ids`
empty, deliberately, even though its prose named `conway_fork`/
`conway_spawn`/`report` throughout: naming `report` would have made the
WHOLE fragment vanish for the interactive root this plugin exists for
(`App::session_spec`'s own `ToolSelector::Except(vec!["report".into()])`
excludes it there). That meant the fragment could only ever describe those
tools, never actually tell the model to use one.

`InstructionFragment` now also carries `parts: Vec<InstructionPart>` —
zero or more conditional sentences beyond the body, each independently
gated on its own tool ids, rendered into the SAME segment as the body
(joined by a blank line) only when every id it names is reachable this
turn. `fragments/idiom.md` uses the markdown convention this crate's
parser (`parse_fragment_markdown`) implements: a paragraph immediately
preceded by an HTML comment `<!-- tools: id1, id2 -->` is a conditional
part; every other paragraph is body. Three sentences that used to have no
way to become genuinely actionable are now real parts:

- `<!-- tools: bash -->` — verify with a tool call before claiming done;
  run the relevant tests with `bash`.
- `<!-- tools: report -->` — you are a child: finish by calling `report`
  with a result.
- `<!-- tools: conway_fork -->` — when the window is filling, fork the
  remaining exploration to a child and keep only its distillate.

Each renders only for a session that actually has the tool it names, and
is withheld — recorded, never silently — otherwise, exactly the same
per-turn discipline the old whole-fragment check had, now at finer grain.
An operator's own `instructions.md` (below) can use the identical `<!--
tools: -->` convention to gate their own sentences the same way.

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

**Location, and why the project scope is a short, bounded walk, not an
open-ended search path.** At project scope, this plugin walks up from `cwd`
through each ancestor directory — nearest first — taking the first
`.conway/instructions.md` it finds, and stops climbing at the enclosing git
repository root (the nearest ancestor, including `cwd` itself, that
contains a `.git` entry) rather than continuing to the filesystem root.
Someone who launches conway from a subdirectory of an already-configured
repository — `docs/`, a package inside a monorepo — now sees the same
project instructions a launch from the repository root would have. When
`cwd` is not inside a git repository at all, the walk is `cwd` alone: the
direct `.conway/agents/`/`.conway/skills/`-style `cwd`-join this plugin
itself used before, and still the only behavior those two other
conventions have. If that walk finds no `.conway/instructions.md`
anywhere, it tries the identical directory list again for `AGENTS.md` —
the standing-instructions filename several other harnesses already
converge on — before giving up; `.conway/instructions.md` wins whenever
both exist anywhere on the walk, even one nearer to `cwd` than a farther
`.conway/instructions.md` that wins, since conway's own file is the more
specific declaration. `CLAUDE.md` is deliberately not read: ruled out as
single-vendor, unlike `AGENTS.md`'s multi-harness convergence. No
`@import` directive and no per-directory rule file either — an operator
who wants to say two different things still says them in one file.

Global scope is unaffected by any of the above: it is still exactly one
file, the SAME directory `conway::config::discovery::user_config_path`
resolves `settings.json` into, filename swapped, at zero cost in new
dependencies or schema — that
is `<home>/.conway/instructions.md` when `CONWAY_CONFIG_DIR` is unset, and
`<CONWAY_CONFIG_DIR>/instructions.md` when it is set (board item
`01M0W5Q569F0T97HSEP6F0MPCR`, closing an isolation gap identical in shape
to the one board item `01M0VV6CVSZM4XH8J4G6EBV5E3` closed for
`settings.json` itself: an operator or embedder relocating conway's
user-config layer relocates this file with it too, not only
`settings.json`). Both scopes are read when
present, and **both are additive — neither one's presence disables the
other**, unlike `settings.json`'s project-overrides-user merge: an operator
who has authored both a house-wide preference and a per-project convention
gets both, as two separately named fragments (`conway.idiom.operator.
project`, `conway.idiom.operator.global`) so `/context` shows each one's
own token cost rather than one opaque combined number — the project
fragment carries that name whether its text came from `.conway/
instructions.md` or the `AGENTS.md` fallback; only the provenance path
underneath it differs. (When a project genuinely lives at the operator's
own home directory, the two paths name the same file; the global fragment
collapses away rather than injecting the same text twice.)

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

**Provenance: the shipped fragment and your own text are tagged apart**
(board item `01M1FSNBRE5XJ0GQ04RT5HZ1PS`). Every fragment this plugin
contributes carries an `authored_by` marker
(`InstructionFragment::authored_by`), and `ContextBuilder::build`
(`crates/conway-runtime/src/context/builder.rs`) reads it when it stamps
the assembled segment's provenance:

- The shipped base fragment (`conway.idiom.base`) is stamped
  `Provenance::PluginInstruction { plugin_id: "conway.idiom", name }` —
  this crate wrote every word of it.
- An operator's own project/global text is stamped `Provenance::Operator
  { name, path }` instead, naming the exact file (a project's
  `.conway/instructions.md`, its `AGENTS.md` fallback, or
  `<home>/.conway/instructions.md` at global scope) the words came from.

Before this item, every fragment this plugin contributed — the shipped
paragraph and an operator's own text alike — was stamped
`Provenance::Skill { name }`, the SAME stamp an operator-authored
`.conway/skills` body gets. That made the durable log lie in both
directions: an operator's own words read back as "a skill," and this
plugin's own shipped paragraph was indistinguishable from operator prose in
the one place a session's durable record actually lives. `/context`'s
preamble section (below) already named which PLUGIN declared a fragment,
but that was a side-channel, not durable provenance — it could not say a
fragment's *words* were never the plugin's own. Fixed now: an operator's
own instructions are durably, per-segment provenance as theirs, not the
plugin's.

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
considered — `conway.idiom.environment` and `conway.idiom.base` always,
plus `conway.idiom.operator.project`/`conway.idiom.operator.global`
whenever the corresponding file exists — each with its source plugin, its
estimated token cost, and, when one or more of the idioms primer's
conditional parts (board item `01M1FSRJJAB3ZYZXED4SVT2ZSF`; the
environment block declares none) were withheld this turn, how many and
which tool ids made them unreachable. A fragment naming an unreachable
part can still have rendered a real segment — its body, or another
reachable part — so this line describes what was left out, not
necessarily "nothing was sent."

The per-segment listing further down `/context`'s output (and `conway
sessions show`/`export`'s headless render of the same session log) names
each segment's own provenance label, and the two now read apart: the two
shipped fragments render as `plugin:conway.idiom/conway.idiom.environment`
and `plugin:conway.idiom/conway.idiom.base`, while your own project/global
text renders as
`operator:instructions.md` (or `operator:AGENTS.md` when the project
fragment came from the fallback) — the file's own basename, not "a skill"
and not merely "`conway.idiom`". Nothing here is attributed to a skill anymore;
a directory-authored `.conway/skills` body keeps its own, separate
`skill:<name>` label, unaffected.
