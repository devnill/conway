# Design: surface coherence — one home per thing

**Written 2026-08-29 against the tree at `2409e1d`, version 0.9.0. Context:
[`INTENT.md`](INTENT.md) §7a/§7b, `crates/conway-cli/src/tui/commands.rs`,
`crates/conway-cli/src/tui/view/settings.rs`.**

> **This is a design record, not a plan.** Per this project's own standing
> rule (`INTENT.md` §8.8): a design document says what a feature *predicts*
> it will need, not a requirement the feature must satisfy. This page
> transcribes an operator ruling made in one sitting on 2026-08-29 — it is
> not deriving the ruling, and where §11 below finds a question the ruling
> does not answer, that is recorded as open rather than settled here.
> §12 names what would falsify it.

Board item `01M11XYADTHKBM4GZ5HD5JVQ3B`. **This item writes documents only.**
It does not rename a command, move anything into `/settings`, or change any
rendering. The build work this page implies is a later refine pass's job,
decomposed from this page's findings.

---

## 0. What is settled coming in, and by whom

Two operator rulings from earlier cycles are inputs to this page, not
conclusions of it:

- **No capability may exist in only one mode.** Every mode (TUI, one-shot,
  embedder) reaches the same capability set. This page is about the TUI's
  slash-command surface specifically; it does not relitigate which
  capabilities exist, only where each one's home is inside the surface that
  already has it.
- **Policy lives in hooks and plugins, not in the unopinionated core.** The
  harness stays unopinionated; an application built from it does not have
  to.

What this page adds is the operator interview held 2026-08-29, working from
a finding that the TUI's eighteen slash commands mixed five things that read
as different "kinds" of surface, with no stated rule for which kind gets
which home. Everything below §1 is that interview's output, including two
places where the operator corrected an answer given earlier in the same
sitting — recorded as corrections, not smoothed into a single clean
narrative (§5, §6).

---

## 1. The premise: opinionation is layered

**"Unopinionated" is a property of the harness, not a gag order on the CLI.**
This is the premise that makes the rest of this page possible. Before it was
named, "unopinionated" was being applied uniformly across the whole tree —
harness, CLI, and plugins alike — which made every ordinary CLI convention
(a settings menu, a fixed vocabulary of slash commands, a naming rule) look
like a violation of conway's own founding idea, when it is not: `INTENT.md`
§7a already draws exactly this line — *"An opinion in the binary is not an
opinion in the core. That is the whole distinction"* — and describes the
shipped `conway` binary as *"a fully-equipped coding agent, opinionated on
purpose, with every opinion visible and removable."*

The layering, stated explicitly for this page's purpose:

1. **The harness** (`conway`, `conway-core`) is unopinionated: it answers
   *what must exist for conway to work at all*, and stays narrow.
2. **The CLI** (`conway-cli`) is a legitimately opinionated *application*
   built from the harness. It gets to have a settings menu, a fixed slash
   vocabulary, and house rules about where things live — the same way any
   well-designed terminal tool does. Opinion here is not a defect; an
   application with no opinions about its own surface is unfinished, not
   pure.
3. **Plugins** add a further, optional layer of opinion on top of both,
   exactly as the policy-lives-in-plugins rule above already states.

This extends the interactive-first, every-mode-reachable rule and the
policy-lives-in-hooks-and-plugins-not-core rule from §0 — it does not
contradict either. The mode-parity rule is about *capability* parity across
modes, not about the TUI's own internal furniture; the policy rule is about
where *policy over the model's behaviour* lives, not about where a settings
menu entry lives. A house style for `conway-cli`'s own command surface is
neither.

---

## 2. The mental model

**One obvious home per thing; where it lives is apparent without exploring;
menus stay shallow.**

Two things this rejects directly, because both were present in the tree
this session worked from (§3 below is the direct account):

- **Deep nesting.** A menu whose answer to "where do I turn off X" requires
  descending through categories to find it has already lost — the operator
  is exploring, not recalling.
- **The same capability reachable two ways with neither one visibly
  subordinate.** A capability appearing once as a menu entry and once as a
  top-level command, with nothing marking one as a doorway into the other,
  is not two conveniences; it is one capability with two claimed homes,
  which is the same failure as no home at all, expressed differently.

---

## 3. The surface inventory this session worked from

A convention document that does not show the mess it was written against
loses its argument within a release. This is the inventory the session
worked from — gathered 2026-08-27 — with the named inconsistencies that
made "unopinionated" look like it was being violated everywhere:

| Command | Usage | Kind mixed in |
| --- | --- | --- |
| `/ask` | `/ask <text>` | do-something-now |
| `/agents` | `/agents` | show-me-something |
| `/settings` | `/settings` | open-a-menu |
| `/plugin` | `/plugin [install ... \| uninstall ...]` | show-me-something **and** takes a subcommand |
| `/trust` | `/trust permissions` | do-something-now |
| `/steer` | `/steer <agent> <text>` | do-something-now |
| `/cancel` | `/cancel <agent> [<reason>]` | do-something-now |
| `/context` | `/context [<agent>]` | show-me-something |
| `/tree` | `/tree` | show-me-something |
| `/why` | `/why` | show-me-something |
| `/fork` | `/fork [<text>] \| @<agent> <directive>` | do-something-now |
| `/spawn` | `/spawn [@<agent_def>] [<prompt>]` | do-something-now |
| `/resume` | `/resume <session-id>` | do-something-now |
| `/model` | `/model <backend/model>` | change-a-setting |
| `/role` | `/role <alias>` | change-a-setting |
| `/help` | `/help` | show-me-something |
| `/quit` | `/quit` | do-something-now |
| `/exit` | `/exit` | alias of `/quit`, nothing else |

Five kinds were mixed across these eighteen commands: do-something-now,
show-me-something, change-a-setting, open-a-menu, take-a-subcommand. §4
below is the finding that two of these are not kinds at all.

**The named inconsistencies, carried into the ruling below rather than
resolved by inspection alone:**

- `/model` and `/role` sit outside `/settings` while the display preferences
  (`show_reasoning`, `show_timestamps`, `tool_preview_lines`) sit inside it
  — apparently the same kind of thing, homed two different ways.
- `/plugin` is both a listing (`show-me-something`) and a verb
  (`install`/`uninstall` subcommands), while `/settings` is only ever a
  menu — two commands that look structurally alike are not.
- `/settings`' plugins section is a single shortcut row into `/plugin`, not
  an independent listing — the exact "same capability, two claimed homes"
  shape §2 names, mitigated by making one of the two an admitted doorway.
- `/exit` and `/quit` are aliases of each other and nothing else in the
  eighteen has one.
- `/plugin` (singular) and `/agents` (plural) look like an unexplained
  spelling inconsistency sitting side by side.

Twelve first-party plugin crates — `conway-plugin-{discover, history, idiom,
mcp, memory, names, path, skills, statusline, stepguard, subprocess, trim}`
— ship today, each surfacing itself differently, with no single place an
operator can go to learn what enabling one of them actually changes.
(`conway-plugin-skeleton` is excluded from this count deliberately: its own
module doc names it *"a worked, non-default example plugin proving the
tier's shape end to end"*, not an operator-facing capability — thirteen
plugin crates exist, twelve of them ship a real capability.)

### 3a. Verified against the tree, 2026-08-29 — no drift found

The inventory above was gathered 2026-08-27, two days before this ruling.
Re-reading `crates/conway-cli/src/tui/commands.rs` for this page found no
drift: all eighteen entries above still parse (`commands.rs`'s own
`describe`/`builtin_commands` functions, lines 331–533), including the pair
this project has already been burned by once. `commands.rs`'s own module
doc (lines 280–290) records the exact hazard this project's own standing
rule about verification warns about — *a grep's negative result is a claim
about the grep, not about the code* — from board
item `01M0RW29F2ATVGCV0R8H0GQEYH`: an earlier, independent palette array
drifted from the real parser, and a naive grep over `parse`'s `match` arms
made `/exit`/`/quit` look unlisted when they were not, because both live in
one disjunctive arm, `"/quit" | "/exit" =>`, which a line-oriented grep for
`"/exit" =>` alone does not match. This page's own inventory was checked by
reading `describe`'s exhaustive match and `builtin_commands`'s explicit
hand-written `/exit` row (`commands.rs:525–533`) directly, not by grepping
for command literals — the failure mode this exact command pair already
demonstrated once.

One thing worth recording precisely, since it bears on §6: the plugins
shortcut in `/settings` is not merely a *proposal* to fix — it already
exists, built and tested, at `crates/conway-cli/src/tui/view/settings.rs`
under a section the file's own module doc titles "Plugins: one home, not
two" (board item `01M0VR5RCCB8NDGG2JEQW8X7XR`). It renders as a `"plugins"`
group — the same `MenuNode::Group` primitive as `"display"`, `"tool
output"`, and `"permissions"`, the menu's three genuine configuration
sections — containing a static counts row and one leaf reading *"open the
full plugin listing -- /plugin (Enter)"*. §6 is about exactly this
implementation, not a hypothetical one.

That same file's own module doc (its "Providers" section) independently
flags the precise question this session exists to answer: providers
(`backends.<id>`) concluded the *opposite* of plugins — `/settings` owns
provider add/remove directly, with no `/provider` sibling command at all —
and the file says outright, *"which one is the house style for a THIRD
future settings category is exactly the question that [the surface-
coherence] session should rule on, not something this item decides for
it."* §4's rules answer it: provider management is *global, persistent
configuration* (a `backends.<id>` entry is exactly rule 1's "global,
persistent configuration"), so it belongs in `/settings` directly with no
doorway needed; plugin install/enable is *action* (§4's ACTION kind, on the
strength of §6's "installing and enabling are actions"), so `/plugin`
remains its one home and `/settings` may only hold a doorway into it. The
two conclude oppositely not because one is right and one is wrong, but
because a provider and a plugin are different *kinds* of thing under this
session's own taxonomy. This is not new work asked of this item; it closes
a question a previous item's own doc had explicitly left open for this one.

---

## 4. Three surface kinds, not five

Two of the five kinds §3 found are not kinds of surface at all:

- **"Open a menu" is not a kind — it is how CONFIGURATION is *presented*.**
  A menu is what a configuration surface with more than one setting looks
  like; it is not a fourth thing sitting beside actions and views.
- **"Take a subcommand" is not a kind — it is the *verb form* of an
  action.** A command that accepts `install`/`uninstall` is still doing
  something now; the subcommand is how it disambiguates which thing.

What remains, once those two collapse into the others:

- **ACTION** — do something now. `/fork` `/spawn` `/cancel` `/resume`
  `/ask` `/steer` `/plugin`.
- **VIEW** — show me something. `/tree` `/context` `/why` `/agents`.
- **CONFIGURATION** — change a setting. `/settings`, and everything reached
  through it.

These three examples are illustrative, not an exhaustive re-classification
of all eighteen commands — the ruling names these specific commands as its
worked examples. Applying the same test to the remainder is mechanical
rather than a new judgment call, and is recorded for completeness: `/trust`
(do something now — extend a trust decision) and `/quit` are ACTION;
`/model` and `/role` are CONFIGURATION under the persistent/session-scoped
split §5 states below (their home is top-level precisely *because* they are
the session-scoped half of that split, not because they are a fourth kind).
`/help` does not classify cleanly under any of the three — it shows the
command surface itself rather than any session state, which makes it
VIEW-shaped by the letter of the test, but it is also the one command that
exists to describe the other seventeen rather than to do anything a session
has. **This is left open in §11** rather than forced into VIEW here.

---

## 5. The six rules

1. **Global, persistent configuration's home is `/settings`. Session-scoped
   state — what this session is currently using — is a top-level command.**
   Actions and views are top-level.
2. **Exactly one home per capability.** A second entry point is permitted
   only when it is visibly a DOORWAY into that home — rendered as
   navigation out of the current surface, never as a peer entry sitting
   alongside real ones.
3. **Menus are one level deep.** A group needing a second level becomes its
   own command.
4. **Only an action command may take a subcommand.** A command that is only
   a menu takes none.
5. **No aliases.** `/quit` or `/exit`, not both.
6. **Naming: a view over a collection is plural; an action or configuration
   command is singular.** This makes `/agents` and `/plugin` both correct
   rather than an inconsistency: `/agents` is a VIEW over a collection
   (plural), `/plugin` is an ACTION (singular) — the apparent spelling
   mismatch §3 lists is not one.

### Rule 1 was corrected by the operator, mid-session

An earlier ruling in the same sitting moved `/model` and `/role` into
`/settings` and was **overruled** later the same session. The corrected
rule, as stated above: `/settings` holds **global, persistent**
configuration; **session-scoped state — what this session is currently
using — stays a top-level command.** `/model` and `/role` remain outside
the settings menu; the *default* model and *default* role live inside it,
labelled as defaults.

**Why the first attempt failed, stated plainly rather than as a pragmatic
exception (that framing was explicitly withdrawn):** the first pass
classified `/model` and `/role` as "configuration" because its working
taxonomy had no way to separate *persistent* configuration from *session
state*. Once "configuration" was one undifferentiated bucket, both looked
like they belonged in the one place configuration lives. Collapsing the two
made a correct surface look like a defect — `/model` and `/role` were never
misplaced; the taxonomy classifying them was incomplete. The commands were
never in rule 1's scope at all, once rule 1's own scope was stated
correctly; there is no exception here to explain away, only a rule that was
missing a distinction it needed from the start.

**A concrete gap this correction surfaces, checked against the tree for
this page rather than left as an assertion:** `crates/conway/src/config/
schema.rs`'s `ConwayConfig` already carries `default_role: RoleAlias` as a
persistent, top-level config field — the "default role, labelled as a
default" half of the corrected rule has somewhere to live today. The
model-selection half does not: there is no `default_model` field anywhere
in the schema. Model selection today runs through `RoutingSection`
(`schema.rs:438`), which carries only `default_headroom_tokens` — routing
policy, not a scalar "which backend/model does a session start on." A build
item surfacing "default model" inside `/settings` per the corrected rule 1
has a real design decision in front of it (a new scalar field, or something
routing-shaped) that this page does not resolve — see §11.

---

## 6. The reopened ruling: `/settings`' plugins group

`/settings`' plugins group was made a shortcut into `/plugin` under a prior
"one home, not two" ruling (board item `01M0VR5RCCB8NDGG2JEQW8X7XR`,
recorded in `crates/conway-cli/src/tui/view/settings.rs`'s own module doc,
§3a above). **The operator named this exact pair as the most apparent
example of fragmentation** in the tree this session reviewed.

**Its intent is upheld.** `/plugin` remains the one home for plugin
management, because installing and enabling are ACTIONS (rule 1's third
sentence, §4's ACTION kind) — there is still exactly one place a plugin is
installed, uninstalled, or toggled, and `/settings` does not get a second,
independent listing.

**Its implementation is rejected.** An entry that reads as a settings group
while actually being a shortcut is precisely the ambiguity rule 2 forbids.
Concretely, against the code described in §3a: the plugins section renders
as a `MenuNode::Group` labelled `"plugins"`, styled identically to
`"display"`, `"tool output"`, and `"permissions"` — the menu's three
sections that *are* real, in-place configuration. An operator scanning the
menu sees four groups that look alike; only the third row inside the fourth
one, on closer reading, reveals that the whole group is navigation rather
than a setting. That is a settings group wearing a doorway's job, not a
doorway — the exact shape rule 2 names. It must render as an unmistakable
doorway: distinguishable at the level a person scans a menu at, not only at
the level they read a row's full label.

This page does not specify what that rendering looks like — a distinct
visual treatment, a different primitive than `MenuNode::Group`, a
top-of-menu placement outside the four sections entirely — because
specifying it is exactly the kind of implementation decision this item's
scope excludes (see the item's own "SCOPE" note: this item does not move
anything or change any rendering). What this page settles is that the
current implementation does not meet rule 2, and that the fix is a rendering
change, not a re-litigation of which surface owns plugin management.

The page must show this reverses something, and it does: a previous ruling
(`01M0VR5RCCB8NDGG2JEQW8X7XR`) is not wrong about *where* plugin management
lives; it is wrong about *how the shortcut announces itself*, and that
second half is corrected here.

---

## 7. What a first-party plugin owes an operator

A `description()` readable before enabling, and a declaration of what it
contributes — commands, tools, settings, status-line text — surfaced in one
place, so someone can learn what enabling a plugin changed without reading
its source.

**Verified against the tree: half of this already exists.** `Plugin::
description()` (`crates/conway-core/src/ports/plugin.rs:219`) already
returns a `PluginDescription { summary, you_get, you_lose, costs }` — free
text, readable before enabling, exactly the first half of what this section
asks for. What does not exist is the second half: a **structured**
declaration of what a plugin contributes (which commands, which tools,
which settings, which status-line text) surfaced **in one place**. Today
that inventory is implicit — reconstructable only by reading each of the
twelve first-party crates' own `Plugin::manifest()`/`commands()`/
`status_contributions()` implementations, which is the "entirely ad hoc
surfacing" §3 names as the finding. Twelve first-party crates ship today
with no such place.

---

## 8. Familiarity means convergence

**A paradigm earns deference when several independent terminal coding
harnesses land on it. One tool's choice earns none.**

The operator named four: Claude Code, OpenCode, Hermes, and "Py" (dictated).
This gives the rule a test: **before deferring to an existing paradigm, ask
whether several harnesses do this, or just one.**

An earlier version of this same ruling, made in the same sitting, framed
familiarity against Claude Code alone, making one vendor the reference.
**That framing is corrected here.** In a field whose patterns are
demonstrably unsettled (`INTENT.md`'s own "unopinionation is a bet on
volatility" posture — the cycle's steering ruling 5), independent
implementations arriving at the same answer is the best available evidence
that the answer fits the problem, rather than that one vendor happened to
ship first.

**What familiarity does NOT license.** conway's idioms and primitives
differ deliberately — fork versus spawn, the context tree, distillate, the
permission model. Where the difference buys something, conway keeps it.
Where it buys nothing, sameness wins. Convergence is a reason to *match a
paradigm*; it is never a reason to *adopt a capability* conway had not
otherwise chosen to have.

### 8a. The roster, verified

The roster was dictated and its spellings were unverified going in. Each
name below was checked this run (2026-08-29), against the live internet,
independently of this project's own prior citations, and cross-checked
against them where they existed.

- **Claude Code** — Anthropic's own CLI. Not independently re-verified here;
  it is this project's own point of departure and needs no external source.
- **OpenCode** — confirmed. `https://opencode.ai/`, fetched this run: *"The
  open source AI coding agent"*, delivered as a terminal interface, IDE
  extension, and desktop app; installable via `curl -fsSL
  https://opencode.ai/install | bash` among other package managers. Spelled
  as one word, both capitals: `OpenCode`.
- **Hermes** — confirmed, and already load-bearing elsewhere in this
  project. `INTENT.md` §7b already cites [Hermes Agent's own
  repository](https://github.com/NousResearch/hermes-agent) by name as a
  reference for skills, persistent memory, and a learning loop shipped as
  harness features. Independently re-confirmed this run at
  `https://hermes-agent.nousresearch.com/` (Nous Research; MIT license;
  installable via `curl -fsSL https://hermes-agent.nousresearch.com/
  install.sh | bash`; current version `v0.20.6` at fetch time). **One
  caveat worth stating rather than silently absorbing:** Hermes Agent
  positions itself as a multi-surface personal agent first — *"Telegram,
  Discord, Slack, WhatsApp, Signal, Email, CLI — and a growing list of
  platforms. One agent, one memory, every surface"* — with the terminal as
  one surface among several, not a dedicated terminal coding harness the
  way Claude Code and OpenCode are. It still ships coding-agent
  characteristics directly relevant to this convergence test — isolated
  subagents with their own terminals, a skills system, a self-evolution
  loop over its own skills and prompts — so it is kept in the roster, with
  this distinction on the record rather than papered over.
- **"Py" is Pi** (`https://pi.dev`) — **confirmed, not merely inferred.**
  The dictated "Py" was a mishearing of **Pi**, a real, actively developed
  "minimal agent harness" by Earendil Inc. — confirmed this run via the
  product site (*"Pi is a minimal agent harness. Adapt Pi to your workflows,
  not the other way around"*; installable via `curl -fsSL
  https://pi.dev/install.sh | sh` or `npm install -g
  @earendil-works/pi-coding-agent`, currently at version `0.84.4` on the npm
  registry) and the GitHub organisation `earendil-works` (repo `pi`,
  described as *"AI agent toolkit: unified LLM API, agent loop, TUI, coding
  agent CLI"*). Two pieces of evidence settle it, neither available to this
  page's first pass: the operator's own earlier words in the same session —
  *"I've used pie and Hermes a bit"* — are the same pairing that later
  reappeared as "Py … and Hermes," and `INTENT.md` §7b already cites both
  by name and URL, [Pi](https://pi.dev) as *"the reference [for] the shape
  of a lightweight core with good extension surfaces: four tools, a short
  system prompt, tree-structured sessions, and an explicit list of things
  it refuses to own,"* and [Hermes Agent](https://github.com/NousResearch/hermes-agent)
  as the harness-features reference cited above. The same project, already
  load-bearing in this tree under its correct spelling, is the only serious
  match a homophone search for "Py"/"Pai"/"Pi" surfaces, and the operator's
  own prior "pie" removes the last reason to treat the match as coincidence.

A convention page that misnames its own evidence loses its argument, and
the rule does not depend on any single roster entry being right — it
depends on the *test* (do several independent harnesses converge, not one).
That said, §8a's roster is now fully confirmed: Claude Code needs no
external source, and OpenCode, Hermes, and Pi are each verified by name.

---

## 9. The six rejected alternatives, each kept with its cost

A decision that discards its alternatives cannot be re-examined. Six were
weighed and set aside while reaching §5 and §8; each is kept here with the
cost it lost on, not deleted once the ruling was made.

1. **Claude Code as the sole familiarity reference.** Cost: makes conway an
   imitation of the tool the operator is leaving, and lets any Claude Code
   behaviour be defended as "familiar" regardless of whether anyone else
   does it. §8's convergence test replaces this.
2. **No familiarity input at all.** Cost: internally consistent, unfamiliar
   to everyone arriving. A surface with no external reference accumulates
   its own idiosyncrasies just as surely as one that copies a single vendor
   — its own kind of accumulation, not a neutral default.
3. **Moving `/model` and `/role` into `/settings`.** Cost: internal
   consistency under an incomplete taxonomy, at the price of diverging from
   a paradigm every comparable tool shares, on two of the most frequently
   typed commands. This was rule 1's own first attempt, corrected in §5.
4. **Keeping `/model` and `/role` top-level as a documented *exception*.**
   Cost: an exception invites more exceptions, and misdescribes the
   situation — §5 states plainly that these commands were never inside
   rule 1's scope to begin with, once rule 1's own scope was stated
   correctly, so labelling them an exception would misrepresent a taxonomy
   gap as a deliberate carve-out.
5. **A deep, categorised menu tree.** Cost: discoverability by exploration,
   which §2's mental model explicitly does not want — the whole point of
   "one obvious home per thing, apparent without exploring" is that an
   operator should not have to descend a category tree to find a setting.
6. **A mechanical predicate over the six rules, checked the way
   `scripts/board-claims.md` checks other capability claims.** Cost:
   automatic drift detection, traded away deliberately — §10 states this
   trade and why it was made, in full.

---

## 10. Enforced or documented — both, on a principled line

**The six rules in §5 are DOCUMENTED, not mechanically gated.** This
project's own declaration-honesty rule governs claims about what the
software *does*; house style is not such a claim — a slash command's home is
not a capability declaration in that sense, it is a furniture choice. **No
predicate is added to `scripts/board-claims.md` for the six rules.** This was
considered and
explicitly rejected, at a real, named cost: automatic drift detection. §9's
sixth rejected alternative names that cost in full. Adding one anyway would
legislate house style into a gate and make every new surface a gate
negotiation — a tax this project's own steering has repeatedly rejected
elsewhere for the same reason (INTENT §8.5's standing objection to building
seams ahead of a proven need applies here too: no consumer of a mechanical
six-rules gate exists, and the six rules themselves are new enough that
premature automation would freeze an interpretation before it has been
tested against a second real surface).

**The plugin contribution listing (§7) IS gated, and belongs to whatever
item builds it — not to this documentation item.** It tells an operator what
enabling a plugin changed, which is a capability claim in the full sense the
declaration-honesty rule governs: describing a plugin's contribution
incorrectly (claiming a command, tool, or setting the plugin does not
actually register) is exactly the kind of "nothing may claim to be reached
that isn't" defect that rule exists to catch. That half is code with a test.
This item does not build it; §7 states what it must contain, not how it is
checked.

---

## 11. Open — genuinely, not settled here

- **`/help`'s kind.** §4 could not place it cleanly in ACTION, VIEW, or
  CONFIGURATION — it shows the command surface itself rather than any
  session state. This may mean `/help` is a fourth, infrastructural
  exception to the three-kinds taxonomy rather than a member of it, or it
  may mean VIEW's definition should be read to include "shows the surface
  a person can act on," not only "shows session state." The ruling does not
  say which, and this page does not choose.
- **The doorway's concrete rendering.** §6 states that `/settings`' plugins
  group must render as an unmistakable doorway rather than a peer group,
  but does not specify what that rendering is. This is deliberately left to
  the build item that implements it — but it is worth naming explicitly as
  unanswered rather than implying a specific fix was chosen and merely
  omitted.
- ~~**Where the "default model" the corrected rule 1 asks `/settings` to
  show actually lives.**~~ **CLOSED, board item
  `01M18Q7P25DTSKQJDJJCC3E800`, 2026-08-30 (see §13).** Resolved as a
  DERIVED read over `roles.<default_role>.chain` (`ConwayConfig::
  default_model`), not a new `default_model` scalar beside `default_role`
  — the rejected alternative and its cost are recorded at that method's
  own declaration site (`crates/conway/src/config/schema.rs`). `/settings`'
  "defaults" section now shows both: `default role` as a settable,
  cyclable leaf; `default model` as a read-only row computed from it.
- **Whether an operator may override a plugin-declared default independent
  of the plugin's own on/off state**, and other second-order questions
  about how the persistent/session-scoped split in rule 1 interacts with
  per-plugin configuration once that lands (`DESIGN-plugin-dependencies.md`
  §6's "settled 2026-08-26 — the first slice is over" ruling opens
  `[plugins.config.<id>]`, but no `Plugin::config_schema()`-shaped method
  exists in `crates/conway-core/src/ports/plugin.rs` today, confirmed by
  reading the trait for this page). The presentation chapter's
  "Configuration" section in `docs/plugins/authoring.md` states the rule
  this page settles (a plugin's own settings follow the same persistent/
  session-scoped split conway's do) without claiming the underlying
  mechanism is built.

---

## 12. What would falsify this

- **A future surface needs a fourth kind.** If a command genuinely cannot
  be classified as ACTION, VIEW, or CONFIGURATION — and `/help` (§11) is
  the first candidate — the three-kinds finding in §4 is incomplete, not
  merely under-specified.
- **The doorway rendering rule turns out to be unbuildable within
  `MenuNode`'s existing primitives.** §6 asserts the plugins shortcut can be
  made unmistakable without becoming a fifth surface kind of its own; if the
  primitive genuinely cannot express "this group is navigation, not
  content" without inventing a new node kind, rule 3 ("menus are one level
  deep") may need to be read to allow the exception it currently forbids.
- **A fifth independent harness converges on a paradigm conway rejected.**
  §8's convergence test is falsified the moment conway holds a position that
  several independent harnesses have since converged against — the test
  obligates re-examination, not just adoption on the way in.

---

## 13. Revisions

Corrections are appended here dated, never absorbed upward — matching
[`DESIGN-context-path.md`](DESIGN-context-path.md)'s own rule.

This page is written directly from the 2026-08-29 operator interview. It
already records, in its own body rather than here, that the ruling it
transcribes was itself corrected twice during that single sitting — rule 1
(§5) and the familiarity input (§8). A page that shows its own correction
history, including corrections that happened before the page existed, is
more trustworthy than one that presents a clean answer arrived at on the
first try.

**2026-08-30 — §8a's "Py" hedge removed.** §8a originally reported "Py" as
"not confirmed as spelled," reasoning to Pi as the likely match but stating
plainly that this was an inference from a dictated homophone. Two pieces of
evidence not available to that first pass settle the question: the
operator's own earlier words in the same session, *"I've used pie and
Hermes a bit"* — the same pairing that later reappeared as "Py … and
Hermes" — and `INTENT.md` §7b's pre-existing citation of both Pi and Hermes
Agent by name and URL. §8a now names Pi as confirmed rather than inferred;
§12's corresponding falsifier ("Py resolves to something other than Pi") is
removed as resolved. The Hermes caveat (a multi-surface agent, not a
dedicated terminal coding harness) is unchanged — it was never in question.

**2026-08-30 (board item `01M18Q7P25DTSKQJDJJCC3E800`).** §11's "where the
default model lives" open question is closed, not struck: "default model"
is a derived read over `roles.<default_role>.chain`
(`ConwayConfig::default_model`), never a second, independently-settable
`default_model` scalar — the rejected alternative (a `default_model`
field beside `default_role`) and its cost (a second source of truth for
model selection, exactly the "one implementation" drift this project's own
safety-critical-resolution-logic rule exists to prevent) are recorded at
that method's own declaration site, not only here. `/settings` now has a
sixth top-level group, "defaults", implementing rule 1's own words from
§5: the default role as a settable, cyclable leaf; the default model as a
read-only row computed from it, both labelled as defaults.
