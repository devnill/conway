# Design: plugin dependencies, host surfaces, and the bundling rule

**Written 2026-08-25 against the tree at `4dcf79b`, version 0.9.0. Context:
[`INTENT.md`](INTENT.md) §7a/§7b, [`CATALOGUE.md`](CATALOGUE.md),
`docs/plugins/README.md`.**

> **This is a design record, not a plan.** Per this project's own standing
> rule (`INTENT.md` §8.8): a design document says what a feature *predicts*
> it will need, not a requirement the feature must satisfy. Everything below
> that reads as a decision is a hypothesis stated plainly enough to be
> falsified — most concretely by whoever builds `conway.ui` and finds a
> place where the shape this page assumes does not fit. §8 names what would
> falsify it.

---

## 0. What is settled coming in, and by whom

Three operator rulings are inputs to this page, not conclusions of it.

1. **`conway.ui` is first-party and bundled.** It ships linked into the
   binary, resolvable by id with no download.
2. **Bundle liberally; enable nothing.** Plugins a working developer would
   consider table stakes get bundled. Every one of them is opt-in. The
   stated contrast is Claude Code: every feature in it is useful to
   *someone*, and the cost of shipping them all switched on is bloat
   nobody chose. conway's answer is to carry the same breadth and make the
   operator name what they want.
3. **A plugin cannot be enabled without its dependencies enabled.** Not
   degraded, not silently auto-installed — refused.

Ruling 2 is not new policy. It is the tier rule
`conway::config::schema::PluginsConfig::install` already states — *"Empty by
default: no first-party plugin is ever installed unless named here… the
tier's whole point is that nothing in it runs unasked"* — with `install`'s
one deliberate exception (`default_backends`) argued in place. What is new
is the intent to grow the bundle aggressively while holding that rule.

**Ruling 1 does not weaken ruling 3, and an earlier draft of this analysis
said it did.** Bundling removes the *acquisition* cost of a dependency, not
its *enablement*. An operator who enables `conway.permissions` with
`conway.ui` unnamed is in exactly the failure ruling 3 describes; bundling
only changes the remedy from "download and trust a third party" to "add one
id." Under a liberal-bundle/strict-opt-in policy the number of installable
plugins with real inter-dependencies goes *up*, and every operator assembles
their own set by hand. That is precisely the situation dependency resolution
exists for.

---

## 1. The finding: the layer already exists, unnamed

`Plugin` has thirteen methods. **Three are consumed exclusively by
`conway-cli`:**

| Method | Only consumer | Meaningful in an embedder? |
| --- | --- | --- |
| `commands()` | `crates/conway-cli/src/tui/commands.rs:1299` | No — `CommandOutcome`'s `ForkSession`/`Checkout`/`SubmitPrompt`/`MaskRecord` are TUI-host actions |
| `description()` | `crates/conway-cli/src/tui/app/startup.rs:339` | No — its own doc: text for "the PERSON running conway… never assembled into a prompt" |
| `status_contributions()` | `crates/conway/src/builder.rs:1565` → facade | No |

So conway already has a CLI-only plugin surface. It is simply undeclared,
and undeclared is what costs:

- A plugin author gets no signal that `commands()` does nothing under `-p`.
- A plugin cannot degrade — there is no "if a UI is present, use it."
- Nothing tells an operator at install time that half of what they enabled
  is inert in their host.

**And the one existing plugin→screen path is already half-dead.**
`PluginStatusContribution` is `{ key, status, value }` — a declarative
triple, not a drawing call. `Conway::plugin_status_contributions()`
(`crates/conway/src/conway.rs:159`) exposes the collected set on the facade,
and the TUI status view never reads it. Built, exposed, unrendered: the same
built-but-unreachable defect [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md)
§0 names as this tree's recurring disease, in the exact surface this design
wants to build on.

That triple is also the ceiling this design has to break. One fixed widget,
no composition, no extension. If the host owns the widget vocabulary, every
checkbox is a core release and "display something not normally expressible"
is structurally impossible.

---

## 2. Two edges, at different heights

The layering is three deep, and conflating its two edges is what produces
cross-cutting duplication — every plugin wanting a checkbox reimplementing
a checkbox.

```
  ┌────────────────────────────────────────────┐
  │ feature plugins                             │  conway.permissions
  │   consume ui.form/1, ui.status/1            │  conway.memory, …
  └───────────────────┬────────────────────────┘
                      │  EDGE B: plugin → plugin
  ┌───────────────────▼────────────────────────┐
  │ toolkit plugin — conway.ui                  │  checkbox, select,
  │   provides ui.form/1, ui.status/1           │  multi-select, field
  └───────────────────┬────────────────────────┘
                      │  EDGE A: plugin → host
  ┌───────────────────▼────────────────────────┐
  │ host primitive — only the TUI can draw      │
  └────────────────────────────────────────────┘
```

**Edge A (plugin → host)** already exists in one direction and is closed.
`PluginManifest::required_host_caps` is checked at `ConwayBuilder::build`
and hard-fails as `PluginError::MissingHostCapability` naming both the
plugin and the cap. The ergonomics are right. Two things are wrong:

- `HostCapability` is a **closed two-variant enum** — `Subagent`,
  `PersistentTransport`. A third party can never declare a capability the
  core has not blessed, and every new host surface is a breaking enum edit.
- There is no `optional_host_caps`. `docs/plugins/inference-hooks.md:64`
  writes as though there is — *"requested
  (`required_host_caps`/`optional_host_caps`) and separately granted"* — and
  it appears **nowhere in the code**. Designed, never built, and the first
  real consumer needs it: a UI surface is present in the TUI and absent
  under `-p`.

**Edge B (plugin → plugin) does not exist at all.** `ToolCtx` hands a tool
eleven handles — `events`, `subagents`, `plugin_events`, `config`,
`context_path`, `session_discovery`, and the rest — and every one is a
*host* service. `PluginEventHandle` is emit-only fire-and-forget pub/sub,
not call-and-return. No plugin can reach another.

**The naming discipline Edge B needs already exists and is well built.**
Plugin events are `plugin_id.event_name`, enforced by
`conway_core::event_name::validate_event_name`: a plugin event *must* be
namespaced, so shadowing a core event is impossible by construction rather
than by check. That is the right model for a capability vocabulary. That
module's own doc also records the piece still missing one layer down — the
closed-vocabulary check, "does anything actually provide this name," does
not exist (§16.6 point 2). Edge B needs exactly that check, for capabilities
rather than events.

### Why this is capability negotiation and not inheritance

The authoring experience wanted is inheritance-shaped: a plugin "inherits"
UI features from `conway.ui` rather than reimplementing them. The
*mechanism* cannot be subtyping.

conway's plugin tier explicitly includes out-of-process plugins —
`conway-plugin-subprocess` and `conway-plugin-mcp` speak a wire protocol,
not Rust. A `trait UiPlugin: Plugin` is unimplementable by them, permanently
excluding subprocess plugins from every UI surface and making in-process
plugins privileged. That contradicts the rule
`crates/conway-core/src/ports/plugin.rs`'s own module doc opens with:
*"There is exactly one extension mechanism: the plugin API… nothing about
them is privileged."*

A namespaced string capability and a serialisable payload cross a pipe. A
Rust trait object does not. **This constraint decides more of this design
than any preference does**, and §8 names the condition under which it should
be revisited.

---

## 3. The three enablement points

Ruling 3 has to hold everywhere enablement happens, and there are three
places with different affordances:

| Where | Mechanism | Enforcement available |
| --- | --- | --- |
| `settings.json` `[plugins].install` | hand-edited text | `build()` only — hard error |
| `/plugin` browser toggle | `apply_plugin_toggle` writes `plugins.install` | interactive — refuse, or offer to enable both |
| `/plugin install` from marketplace | the trigger wired at `5e8d06b` | resolution time |

**The toggle-*off* direction is the sharper defect and is unguarded today.**
Enabling `conway.permissions` without `conway.ui` fails at next build —
annoying, but discoverable. Turning `conway.ui` *off* while
`conway.permissions` is still on breaks it just as badly, and the browser
today writes it to disk and prints a cheerful "conway.ui off" notice. The
operator finds out at restart. That is a live edge in the shipped binary and
the crispest acceptance criterion this work can have.

**Restart-to-apply is convenient here.**
`crates/conway-cli/src/tui/app/plugin_toggle.rs` never touches the running
session's installed plugin set — its own doc states this as deliberate. So
enforcement is one authority (a check at `build()`) plus one interactive
pre-check in the browser, never live dependency reconciliation mid-session.

---

## 4. The mechanism, minimally

**Two dependency tiers on `PluginManifest` — `requires` and `optional` —
by plugin id, with no version constraint in the first pass.**
`PluginManifest::version` is a bare `String`, and these `requires`/
`optional` edges carry no version constraint: they match on plugin id
alone. *(2026-08-29 — qualified: `semver` is now a direct dependency of
`conway-core`, added for the UNRELATED capability-CALL channel, Edge B —
`crates/conway-core/src/ports/capability.rs`, `CapabilityRegistration::
version` and `CapabilityCallHandle::call_versioned`; see §7b's own closing
entry. That crate governs a different edge, name-and-version between a
capability consumer and its provider, not the plugin-id `requires`/
`optional` edges this section is about — those remain name-only, unchanged
by that addition.)* Name-only edges cover every case currently on the
table and can be shipped without a version constraint of their own —
provided the limitation is stated out loud rather than implied away. §7
argues the case for doing versions immediately instead.

### 4a. Why two tiers, and the criterion for which one an edge gets

**One tier is not enough, and the failure is a cascade rather than an
inconvenience.** Suppose `conway.ui` requires a host drawing surface, and
`conway.permissions` requires `conway.ui`. Under `conway -p` there is no
drawing surface, so `conway.ui` cannot load, so `conway.permissions` cannot
load — and a headless run dies because the operator configured a UI plugin
months earlier. **The same `settings.json` would stop serving the TUI and
the one-shot at once.** That is the concrete form of "every surface is
first class": one configuration, portable across TUI, `-p`, and an
embedder, degrading per surface rather than refusing on the narrowest one.

**The criterion is not the author's preference, and this project has
already settled it one layer down.**
[`docs/plugins/compatibility.md`](../plugins/compatibility.md)'s
version-negotiation table splits wire points by whether they change the
outcome:

| Point class | Unsupported version | Stated reason |
| --- | --- | --- |
| **Participant** (`tool`, `permission.policy`, `context.hook`) | Refuse to load | *"a policy that silently never runs is the worst outcome"* |
| **Observer** (`observe`, `status`) | Degrade: load without the point, warn | an observer changes nothing by construction |

Dependencies inherit that split, with the question restated for this edge:

- **`requires`** — the dependent cannot perform its stated function at all
  without the dependency. Absence is refused, per ruling 3.
- **`optional`** — the dependent's function survives; only a *presentation*
  or *convenience* of it is lost. Absence degrades, and is announced.

`conway.permissions` against `conway.ui` is **optional** under this test,
and the test is what makes that answer checkable rather than a matter of
taste: without a UI the classifier still runs, the gate still narrows, the
mode is still whatever config or a flag set. What is lost is the
interactive mode picker — presentation, not function.

### 4b. An optional dependency must declare what it does without one

**Optional edges reproduce this tree's own recurring defect unless the
absence is stated.** §1 documents the live instance:
`plugin_status_contributions()` is collected, exposed on the facade, and
never rendered — built, exposed, unreachable, and silent about it. An
optional dependency that is simply absent produces the identical shape: a
feature that is gone, with nothing anywhere saying so.

So `optional` is not a bare flag. An optional edge carries **what the
dependent falls back to**, and the host announces the degradation on
whatever channel that host has: a row in the `/plugin` browser under the
TUI, a `tracing::warn!` naming both plugins under `-p` — mirroring the
observer rule's own "degrade *with a warning naming both versions*", never
degrade quietly.

**Announcement channel differs by host, and that is not a wrinkle to
paper over.** A headless run has no plugin browser to render a notice
into. Whatever this design settles on, the rule is that no surface may
degrade *silently* — not that every surface degrades identically.

**Topological resolution in `ConwayBuilder::install_selected`,** which today
walks a flat `Vec<String>`. See §5: this is not a no-op.

**Failure modes, matching ruling 3:**

- Missing **required** dependency at `build()` → the
  `MissingHostCapability` error shape, which already names both sides. A
  dependency error should name the depending plugin, the missing
  dependency, and — once versions exist — the constraint that was not
  satisfied.
- Missing **optional** dependency at `build()` → load, degrade to the
  declared fallback, announce (§4b). Never a build failure.
- Browser toggle-off of a plugin some enabled plugin **requires** →
  refused, naming the dependent, **before the write**.
  `apply_plugin_toggle` already flips its display mirror only on a
  successful write, so a refusal cannot leave the mirror claiming a state
  disk does not hold. Toggling off a merely **optional** dependency is
  allowed, and should say what the operator is giving up.
- Enabling a plugin whose bundled dependency is not yet named → the
  interactive path can offer to enable both. This is the affordance ruling 1
  buys: for a bundled dependency it is one keystroke, with no download and
  no trust decision.

**Auto-install of a non-bundled dependency is deliberately not designed
here.** It collides directly with the trust ruling the marketplace work just
settled (installing code the operator never named), and under ruling 1 it
does not arise for `conway.ui`. It becomes real the first time a
third-party plugin depends on another third-party plugin, and should be
decided then, with the trust position in hand.

---

## 5. What this breaks, stated before it is discovered

**Topological install order silently changes instruction-fragment
precedence.** `Plugin::instructions()`'s own doc fixes the current rule:
plugin fragments are injected *"in `with_plugin`/`install_selected` install
order."* Today that order is whatever the operator typed into
`[plugins].install`. Reordering the resolution pass to satisfy dependency
edges changes which plugin's paragraph outranks which, in every session,
for reasons no operator wrote down.

This is a real behaviour change hiding inside what looks like a manifest
field addition. Three ways out, none free:

1. **Resolve topologically, inject in declared order.** Keep the operator's
   `install` array as the precedence authority and use the topological order
   only for construction. Preserves today's behaviour exactly; means two
   orders exist and someone must remember which governs what.
2. **Resolve and inject topologically.** Simpler to explain — a dependency's
   text precedes its dependent's, which is arguably more correct. Changes
   existing behaviour for anyone with more than one instruction-declaring
   plugin.
3. **Make precedence explicit on the fragment** and stop deriving it from
   install order at all. Cleanest, largest, and out of scope for a
   dependency edge.

**Recommendation: (1)**, on the same argument this project uses elsewhere —
a change whose blast radius exceeds its item's stated scope should be made
deliberately, in its own item, not absorbed as a side effect. (2) may well
be right; it should be chosen, not inherited.

---

## 6. What this makes fall out for free

**Per-plugin configuration, rendered three ways from one declaration.** If a
plugin declares its config schema once, the TUI can draw an editor, an
embedder can read JSON off `PluginConfig`, and a one-shot run can take
declared defaults — no per-host authoring, and the "compatible shape with
additional parameters for in-app configuration" this work set out to reach.

This was blocked by a ruling with an undefined expiry. `[S1.5]` held
per-plugin configuration to an **embedder-only** surface *"for this first
slice"*; `PluginsConfig` is `#[serde(deny_unknown_fields)]` and actively
refuses a `[plugins.config.<id>]` key. Board item
`01M0V501HZBMWNC6AE45JJXAFK` documented the rule and stopped deliberately,
recording that whether the slice is over is an operator question.

**SETTLED 2026-08-26 — the first slice is over. Per-plugin configuration
opens, with a declared schema (the "Full" shape).** Operator ruling; see §9.
`[plugins.config.<id>]` becomes a real settings surface, a plugin declares
its config schema once, and that one declaration renders three ways: a TUI
editor, an embedder's JSON off `PluginConfig`, and declared defaults for a
one-shot run.

The decisive argument is the **cost ladder**, and it is INTENT §6's, not
this page's. §6 orders extension by deliberate cost — instruction, then
hook, then out-of-process plugin, then compiled-in — and requires that *"the
cheapest one should cover the most ground."* An embedder-only config surface
**inverts that ladder completely**: the single most expensive tier (write
Rust, link conway, recompile) is the only way to set a plugin's knob, while
the cheapest (open a file and edit a line) is actively refused. That is not
a slice that had not finished; it is the ladder upside down.

The two rejected shapes, kept:

- *Flat file only* — open `[plugins.config.<id>]` as untyped values now and
  retrofit a declared schema later onto keys already shipped. Rejected on
  §6's own defect list: an untyped key cannot be validated, so a typo is
  silently inert, which is **"a configuration option that does nothing"** —
  named in §6 as needing the same kind of gate as an instruction that
  references an unreachable capability. INTENT §8.3 compounds it: conway
  must refuse and name what changed when it cannot honour a reference
  exactly, *explicitly including "a referenced configuration that has
  drifted"* — and a declaration is what makes that refusal possible at all.
- *Stays closed* — opinionated plugins put their knobs in bespoke top-level
  config sections. Rejected because that is precisely what the plugin tier
  exists to avoid, and because it leaves the ladder inverted.

**Why §8.5 does not object to building this now.** "Build a seam when there
is a consumer for it, not in anticipation of one" is the standing objection
to a declared-schema system, and it is already satisfied: `conway.ui` (§7a)
and `conway.permissions` both need it, and `conway.trim`'s
`DEFAULT_KEEP_TURNS` was filed as reachable-to-an-embedder and
invisible-to-an-operator in board item `01M0TX5ZKQSYRBWP2HVHJ659YE` before
either existed. Three consumers, one of them already recorded as a defect.

**And the undefined expiry was itself the bug.** INTENT §8.1: *"An open
question is a failure of the spec, not a gap in the code."* A ruling whose
scope is "this first slice" with no stated end condition is that failure in
miniature — `01M0V501HZBMWNC6AE45JJXAFK` was right to stop and say so, and
right that it was not an agent's to close.

---

## 7. Open, with recommendations

**7a. Where the host/toolkit boundary sits. SETTLED 2026-08-26 — the
extensible widget tree, built narrow first.** Operator ruling; see §9. The
three altitudes were:

- *Fixed form schema owned by the host.* Simplest and safest; the toolkit
  becomes a helper library rather than a plugin. Every new widget is a core
  release, and §1's ceiling stands.
- *Extensible declarative widget tree.* The host exposes composable
  primitives plus focus and input routing; `conway.ui` composes them into
  checkbox/select/multi-select and publishes `ui.form/1`. New widgets ship
  in the plugin. A widget tree serialises, so out-of-process plugins provide
  and consume UI on the same terms as in-process ones.
- *Raw drawing surface.* A rect and input events, drawn with `ratatui`
  directly. Maximum power; welds the plugin API to one TUI library, locks
  out every out-of-process plugin, and lets a misbehaving plugin corrupt the
  screen.

**Ruled: the widget tree** — and the ruling rests on
[`INTENT.md`](INTENT.md) §8.2 rather than on this page's own preference,
which matters because it means the boundary was derived, not chosen.

§8.2's test for what may live in the core at all: *"does this encode a
judgment that two reasonable people, doing the same work, could answer
differently? If yes it is policy, and it belongs in a plugin. If no it is
mechanism, and the core may hold it"* — with the worked example *"the
ability to bind a name to something is mechanism… which names exist and
what they mean is policy."* Applied to UI, that partitions in exactly one
place: **focus, input routing, modal stacking and compositing are
mechanism** (two implementers produce the same thing), and **which widgets
exist and what they mean is policy**. That line is the widget tree's line.
The serialisation argument in §2 is now the *second* reason, not the first.

The two rejected altitudes are kept above rather than deleted, because a
decision that discards its alternatives cannot be re-examined. Each fails a
different INTENT rule, in INTENT's own words:

- *Fixed form schema* makes every new widget a core release. §6: **"If
  there is a level where the answer is 'you would have to fork conway,'
  that level is a defect."** §8.5, from the other end: "if the cheapest way
  to change behaviour is to fork the repo, that is a bug report against the
  extension surface."
- *Raw drawing surface* locks out every out-of-process plugin. §6 funds
  that tier explicitly — "a plugin in another language, running as its own
  program… the right price for someone who wants to add a capability
  without learning Rust or rebuilding the binary" — and §7c forbids the
  result outright: **"No second API, no divergence in capability."**

**The sequencing constraint, which is the operative half of this ruling.**
This page's own risk note was right: "composable primitives plus focus and
input routing" is hand-waving until someone specifies focus, input routing
and modal stacking. The ruling does **not** resolve that by specifying the
general widget tree up front. INTENT §8.5 forbids exactly that: *"build a
seam when there is a consumer for it, not in anticipation of one… nothing
is built on theory. A feature lands with a well-defined use case someone
can exercise on the day it ships."*

So the first cut ships **only the primitives `conway.ui`'s first real form
actually needs**, and focus behaviour, input routing and modal stacking are
specified by that consumer rather than ahead of it. An implementer who
finds themselves designing a widget vocabulary no shipped form exercises
has left the ruling. §8's first falsifier — "`conway.ui` needs to draw, not
declare" — is unchanged and is still the thing that would overturn this.

**7b. Versions now or name-only first. SETTLED 2026-08-29 — versioned now,
via standard semver.** Operator ruling, decision `01M189XS6Z9VKYENAHNY1B54CM`
(mechanism), which supersedes an earlier same-cycle decision,
`01M1893Q2DV773ZQ5B138W6G07`, on mechanism only — that earlier decision's
own argument for versioning the edge AT ALL is unchanged and still governs;
what it got wrong was inventing a bespoke `ui.form/1`-style major-exact
identifier instead of reaching for the crate this problem already has a
name for. Operator direction: *"I don't want to reinvent the wheel on this.
If it's okay to just use semver or pin to a specific version, that is fine
by me."* See §9 for both decision ids and the full argument.

The rejected option, kept rather than deleted (a decision that discards its
alternative cannot be re-examined): **name-only**, this page's own original
weak recommendation. It is cheaper and covers everything that was on the
table when it was written, but it fails on exactly the case this section
itself named as the falsifier — `ui.form/1` gaining a widget is precisely
when a consumer needs a version floor, and retrofitting semver onto edges
already in the wild is worse than starting with it. The recommendation
explicitly flagged its own counter-argument as strong enough to overrule,
and the operator did.

**The mechanism, for a plugin author reading this page rather than the
code.** A capability's PROVIDING side declares a `semver::Version` as a
field separate from its namespaced name — `ui.form` stays `ui.form`;
`1.0.0` is not folded into that string. A capability's CONSUMING side
declares a `semver::VersionReq` — `^1` for the ordinary floor, `=1.2.3` for
a hard pin (the operator asked specifically that pinning be available;
`VersionReq` gives it for free, which is why one type covers both cases
rather than two). Resolution is `req.matches(&version)`. A mismatch is
**refused, naming both the requirement and the version actually
installed** — never degraded, the same "not degraded, not silently
auto-installed — refused" posture §0 ruling 3 already states for a missing
dependency, applied here to a present-but-incompatible one rather than an
absent one. This needs no resolver: a capability name has exactly one
provider (a second registration for the same name is refused at
construction, not selected between), so there is no candidate set to
select among and nothing to backtrack over — `VersionReq::matches` is a
predicate over a single pair, not a search. If a second provider for the
same capability name ever exists, candidate selection becomes a real
question for the first time; that is its own item, with a real consumer,
not something this entry anticipates.

**This settles only Edge B's capability-CALL channel
(`CapabilityRegistration`/`CapabilityCallHandle`), not the plugin-id
`requires`/`optional` edges §4 describes.** Those two fields stay name-only
plugin-id lists; a `requires` entry naming a capability is still satisfied
by the mere presence of a provider, at whatever version it declares.
Whether that edge should also gain a version floor is undecided and is not
this entry's question.

**7c. Push versus pull are different machinery.** Asking a question is a
*pull* — a blocking call returning one answer. Augmenting the status bar is
a *push* — a plugin volunteering a value continuously.
`PluginStatusContribution` is the half-built push case (§1). This page has
designed for pull. Whether one capability mechanism serves both, or push
stays a separate declarative contribution, is unresolved.

**Settled 2026-08-25/26, against real code, not by argument alone (board
item `01M0Y3A8MYKKE0GMYKZE1K0QTD`): push stays separate, and it needs no
capability-mechanism machinery at all — not Edge B's call channel, not a
new dedicated push channel either.** The finding that made this decidable:
`conway.statusline` (`01M0X500861X9035QJEA82F94K`) built the push producer
end to end and proved, structurally, that the ONLY missing piece was a host
that reads `Plugin::status_contributions()` more than once. Building that
missing piece surfaced the actual answer to this open question.

*What Edge B's pull mechanism actually needed `RuntimeDeps` for.* Edge B's
capability-call channel (`RuntimeDeps::capabilities`, §2's Edge B) is
threaded through `RuntimeDeps` → `conway_runtime::agent_loop::LoopDeps` →
`conway_runtime::tools::runner::ToolBatchCtx` because a capability CALL
happens *synchronously inside a tool call's own dispatch*, deep in the agent
loop — there is no shallower seam that both exists at call time and reaches
every backend a tool call can run against. Threading anything through that
path is expensive: a sibling item that added one unrelated field to
`RuntimeDeps` had to update 26 `RuntimeDeps` literals, 5 `LoopDeps`
literals, and 2 `ToolBatchCtx` literals across three crates, none of which
has a `Default` impl.

*The push case has no equivalent need.* Its only consumer is the TUI's own
render loop, which already holds a `Conway` clone directly (`conway-cli`'s
`App`) — the exact same facade surface the pre-existing build-time snapshot
(`Conway::plugin_status_contributions`) already rides. So the live handle
this item added — `Conway::poll_plugin_status_contributions`, backed by a
`Vec<Arc<dyn Plugin>>` retained on `Conway` itself, cloned from the plugin
set *before* `PluginRegistry::from_plugins` consumes it and drops each
`Arc<dyn Plugin>` (it keeps only each plugin's `Tool`s and manifest id) —
sits as a sibling field to that snapshot, at **zero cost to `RuntimeDeps`,
`LoopDeps`, or `ToolBatchCtx` and every one of their existing construction
sites.** No channel was built either: `Plugin::status_contributions()` is
already a non-blocking, point-in-time read (the same contract
`Plugin::observe_sink`'s lossy-with-notice posture establishes for the push
side of this same trait), so a plain poll on a bounded host-side cadence —
never faster than the fastest a contributing plugin's own background loop
can produce a new value — is strictly cheaper than standing up a channel
with no blocking call on either end to justify one.

**The general answer this licenses:** whether a plugin-facing mechanism
needs `RuntimeDeps`-depth plumbing is decided by *where its consumer lives*,
not by which "kind" of mechanism it nominally is. A mechanism consumed
synchronously inside per-call tool dispatch (Edge B's pull) has no cheaper
home than `RuntimeDeps`/`LoopDeps`/`ToolBatchCtx`. A mechanism consumed only
by a host-level, out-of-band reader that already holds a facade handle
(this item's push poll) should ride the facade directly and never touch
those three types at all. Push and pull remain different machinery for that
reason — not because one is inherently synchronous and the other inherently
asynchronous, but because their *consumers* sit at different depths in the
runtime.

**7d. Host profiles may not be profiles.** It is tempting to name three
hosts — TUI, one-shot, embedder — and give each a fixed surface set. An
embedder is really "whatever surfaces that application chose to implement,"
which is a set, not a tier. Per-surface negotiation all the way down is
probably right; three named profiles are probably a documentation
convenience.

---

## 8. What would falsify this

- **`conway.ui` needs to draw, not declare.** If the first real toolkit
  cannot express what it needs as a serialisable widget tree — because focus
  behaviour, animation, or layout genuinely require imperative control — the
  §7a recommendation fails and the raw-surface option must be reconsidered,
  taking the loss of out-of-process UI with it.
- **Out-of-process plugins turn out not to matter.** The subprocess/MCP
  constraint decides §2 and §7a. If in practice every plugin worth writing
  is in-process, that constraint loses most of its force and typed traits
  become attractive again. The honest test is whether a subprocess plugin
  ever ships a UI — not whether one could.
- **No second consumer of a plugin-provided capability appears.** If
  `conway.ui` remains the only provider and `conway.permissions` the only
  consumer, Edge B was overbuilt and the capability should have been a host
  surface (Edge A) all along.
- **The toggle-off defect turns out not to bite.** If nobody ever disables a
  depended-on plugin, §3's headline acceptance criterion is theatre and the
  simpler build-time-only check was sufficient.
- **Every real edge turns out to be `required`.** §4a's two tiers are
  justified by one worked example. If, once several plugins declare
  dependencies, none of them is honestly optional — every dependent's
  stated function genuinely collapses without its dependency — then the
  `optional` tier is ceremony, and the participant/observer criterion was
  borrowed into a place it does not apply.
- **Degradation is never noticed.** §4b asserts that an announced
  degradation is meaningfully better than a silent one. If operators
  routinely run degraded without registering the notice, the announcement
  is decoration and `optional` should have been `required` with a clearer
  error.

---

## 9. Revisions

Corrections are appended here dated, never absorbed upward — matching
[`DESIGN-context-path.md`](DESIGN-context-path.md)'s own rule.

**2026-08-25 — §4 gained an `optional` dependency tier (§4a, §4b).**
Operator-raised, against the same-day first draft. As first written this
page specified `optional_host_caps` for the plugin→host edge (§2) and a
bare `requires` for the plugin→plugin edge (§4) — two tiers on one edge,
one tier on the other, with no argument for the asymmetry. The omission was
not cosmetic: with a single tier, a UI plugin that requires a drawing
surface makes every dependent unloadable under `conway -p`, so one
`settings.json` cannot serve the TUI and a headless run at once.

The amendment also **replaces what would have been an author-preference
choice with an inherited criterion.** `docs/plugins/compatibility.md`'s
participant-vs-observer split had already settled this axis for wire
points, on the principle that what changes an outcome must refuse and what
does not may degrade. The first draft did not cite it. §4a now derives the
required/optional test from it rather than inventing a parallel rule, and
§8 gains the two falsifiers that test whether the borrowing was
legitimate.

**2026-08-25 — Four "not yet built" claims above were closed by code
landing later the same day.** Verified against the tree at the time of
writing this entry; §2/§3/§4's prose above is left as written, since it
correctly describes the gap as it stood when this page was drafted.

- §2's *"`HostCapability` is a closed two-variant enum"* no longer holds:
  `HostCapability` (`crates/conway-core/src/ports/plugin.rs`) is now open
  and namespaced, gaining a `Named(String)` variant and a
  `HostCapability::named()` constructor that validates a dotted id, with
  `Subagent`/`PersistentTransport` kept as the two wire-level aliases.
- §2's *"There is no `optional_host_caps`… it appears nowhere in the
  code"* no longer holds: `PluginManifest::optional_host_caps:
  Vec<HostCapability>` is a real, `#[serde(default)]` field, mirroring
  `required_host_caps`'s refuse-vs-degrade split.
- §4's `requires`/`optional` dependency tiers, described above as this
  page's proposal, are now real fields —
  `PluginManifest::requires: Vec<String>` and
  `PluginManifest::optional: Vec<String>` — and are carried into
  `conway-plugin-subprocess::wire::WireManifest` too (its own `requires`/
  `optional` fields, `#[serde(default)]`), so an out-of-process plugin
  declares the same two tiers over the wire rather than only an in-process
  one going through `conway::plugin::PluginManifest` directly.
- §3's *"toggle-*off* is the sharper defect and is unguarded today"* no
  longer holds: `crates/conway-cli/src/tui/app/plugin_toggle.rs`'s
  `App::apply_plugin_toggle` now refuses a toggle-off that would break an
  enabled `requires` dependent, before the write, naming the dependent; a
  toggle-off of a merely `optional` dependency is still allowed, and
  annotates the browser row's own `description.you_lose` with what is
  degraded.

None of this closes §6's `[S1.5]` question or §7's open recommendations
(host/toolkit boundary, versions-now-or-later, push/pull, host profiles) —
only the four claims named above, and only as claims about what exists in
the tree today. `Plugin::hooks()` still does not exist, `conway.ui` and
`conway.permissions` still do not exist as plugins, and the embedder-only
per-plugin config restriction (§6) is unchanged — this entry does not
touch any of those.

**2026-08-26 — §7c's push/pull question is now settled; the prior entry's
"open" listing is corrected on that one point.** Board item
`01M0Y3A8MYKKE0GMYKZE1K0QTD` closed the concrete gap `conway.statusline`
(previous entry) surfaced: the host now polls `Plugin::
status_contributions()` live, per session, from `conway-cli`'s own render
loop (`Conway::poll_plugin_status_contributions`, `App::
refresh_plugin_status_contributions`). §7c above is edited in place (not
merely appended-around) with the answer and its argument, matching how §4
itself was edited in place (not just noted here) when that question was
settled — an "open, with recommendations" item, unlike §2/§3/§4's prose
elsewhere on this page, is not a claim to leave standing once superseded.
Host/toolkit boundary (§7a), versions-now-or-later (§7b), and host profiles
(§7d) remain exactly as open as before this entry.

**2026-08-26 — §7a and §6 are ruled by the operator; both sections are
edited in place.** Board item `01M0WWM0ZB6BR45XJ8HMTJWZ0Z`. Recorded here
because §7c's entry above established the rule for this page: an "open,
with recommendations" item is not a claim to leave standing once settled,
so the section itself carries the answer and this entry carries the date
and the reasoning.

Both rulings went the way this page recommended. That is worth stating
plainly, because it is also the reason to be careful: a recommendation
confirmed by the person it was written for is weak evidence, and neither
ruling rests on it. Each was re-derived from [`INTENT.md`](INTENT.md),
which is the page that outranks this one, and in both cases INTENT turned
out to constrain the answer more tightly than this page had noticed.

**Ruling 1 (§7a) — the extensible declarative widget tree, built narrow
first.** The boundary is derived from INTENT §8.2's mechanism-versus-policy
test, not from §2's serialisation argument: focus, input routing, modal
stacking and compositing are mechanism and may live in the host; which
widgets exist and what they mean is policy and belongs in a plugin. The
serialisation argument is now the second reason rather than the first. The
rejected altitudes each fail a specific INTENT rule — the fixed form schema
makes every widget a core release, which §6 calls a defect in those words;
the raw drawing surface locks out the out-of-process tier §6 explicitly
funds and produces the capability divergence §7c forbids.

*The operative half of this ruling is the sequencing constraint, and it is
new — this page did not recommend it.* This page's stated risk (that
"composable primitives plus focus and input routing" is hand-waving) is not
resolved by specifying the widget tree up front, because INTENT §8.5
forbids that: "build a seam when there is a consumer for it, not in
anticipation of one… nothing is built on theory." The first cut ships only
the primitives `conway.ui`'s first real form exercises. An implementer
designing a widget vocabulary no shipped form uses has left the ruling.

**Ruling 2 (§6) — the `[S1.5]` first slice is over; per-plugin
configuration opens with a declared schema.** The decisive argument is
INTENT §6's cost ladder ("the cheapest one should cover the most ground"),
which an embedder-only config surface inverts: the most expensive tier is
the only way to set a knob and the cheapest is refused outright. Flat
untyped values were rejected because an unvalidatable key is §6's own "a
configuration option that does nothing," and because INTENT §8.3 requires
conway to refuse and name a drifted configuration reference — impossible
without a declaration. §8.5 raises no objection here: three consumers exist
already (`conway.ui`, `conway.permissions`, and `conway.trim`'s
`DEFAULT_KEEP_TURNS`, recorded as an operator-invisible defect in
`01M0TX5ZKQSYRBWP2HVHJ659YE`).

**Still open, and deliberately not ruled in this entry:** §7b
(versions-now-or-later on capability edges) and §7d (host profiles). §7b
was put to the operator alongside these two and not answered; this page's
weak recommendation — name-only first, on the condition the limitation is
documented at the manifest field — therefore still stands as a
recommendation and not a ruling, including its own note that the
counter-argument is strong enough to overrule it. **An implementer must not
read this entry as having settled it**, and retrofitting semver onto edges
already in the wild is the cost of getting it wrong, so it should be ruled
before `ui.form/1` has a second consumer.

No code changed in this item.

**2026-08-29 — §7b is now settled, and settled twice in the same cycle; §7b
is edited in place with the answer (mirroring how §7a/§6/§7c were each
closed above), and both decision ids belong here.** First,
`01M1893Q2DV773ZQ5B138W6G07` ruled that a capability edge should carry a
version at all, closing the "name-only first" recommendation this page's
original §7b text carried. As FIRST drafted against that ruling, the
mechanism was a bespoke `ui.form/1`-style identifier with major-exact
equality — a notation invented for this one problem rather than reached
for off the shelf. That mechanism was itself superseded mid-cycle, the same
day, by `01M189XS6Z9VKYENAHNY1B54CM`, on the operator's own direction: *"I
don't want to reinvent the wheel on this. If it's okay to just use semver
or pin to a specific version, that is fine by me."* The WHY (versioning
edges at all) is `01M1893Q2DV773ZQ5B138W6G07`'s and stands; the HOW
(standard semver, `Version` on the provider, `VersionReq` on the consumer,
`req.matches(&version)`, refuse on mismatch, no resolver because one name
has one provider) is `01M189XS6Z9VKYENAHNY1B54CM`'s and is what shipped.
§7b's own text above carries the full mechanism and keeps the rejected
name-only option with its cost, matching how §7a keeps its two rejected
altitudes and §6 keeps its two rejected shapes.

`semver` is now a direct dependency of `conway-core` (`crates/conway-core/
src/ports/capability.rs`) — already present in `Cargo.lock` at 1.0.28,
pulled in transitively before this promotion, so the promotion added no
new lock entry. `CapabilityRegistration::version` (provider) and
`CapabilityCallHandle::call_versioned`'s `required: &semver::VersionReq`
parameter (consumer) are the two new surfaces; `CapabilityCallError::
VersionMismatch` is the refusal, naming both. §4's plugin-id `requires`/
`optional` edges are UNCHANGED by this entry — they remain name-only, as
`PluginManifest::requires`'s own doc (`crates/conway-core/src/ports/
plugin.rs`) now states explicitly, distinguishing that edge from the one
this entry versions.

**`call_versioned` is a forward declaration: shipped, tested, reachable
from every `Tool::invoke` via `ToolCtx::capabilities`, and NOT called by
anything in-tree yet** — no built-in plugin, no `conway-plugin-subprocess`
code, nothing in `conway-runtime`. The intended first consumer is board
item `01M0WWPA70E8YAAN981EK10D3D` (`conway.ui`, which will publish
`ui.form`), itself not yet built. `docs/plugins/hooks.md`'s point 21 Status
row carries the same label.

**2026-08-29 — board item `01M0WWPA70E8YAAN981EK10D3D` closed the forward
declaration two paragraphs above; that paragraph is left as written (it was
true when drafted) and this entry carries the correction, matching how
every other closed gap on this page is handled.** `conway.ui`
(`conway-plugin-ui`) is now built and bundled, publishing `ui.form` at
`1.0.0`; `conway-plugin-skeleton`'s `skeleton_ask` tool is the first
in-tree caller of `call_versioned`, supplying `^1`. `docs/plugins/hooks.md`
point 21's Status row and `CapabilityCallHandle::call_versioned`'s own doc
comment (`crates/conway-core/src/ports/capability.rs`) are corrected in
place alongside this entry — declaration honesty: a declaration site
describing a mechanism owns every paragraph describing it, not only the
one an item adds.

**Bundled is not enabled, unchanged by this closure.** `conway.ui` ships
per §0 ruling 1 (first-party, bundled, resolvable by id) but stays off per
ruling 2 (opt-in) exactly like every other bundle member —
`crates/conway-cli/src/first_party_plugins.rs`'s `bundle()` carries it, and
a build with no `[plugins]` section installs it not at all
(`crates/conway-cli/tests/ui_form_absent_by_default.rs`).

**§7a's sequencing constraint held, and produced a narrower shipped surface
than this page's own altitude discussion might read as implied.** The
first real form ships zero widgets beyond a single-select ask/answer
(`AskSelectRequest`/`AskSelectAnswer`), and — a disclosed scope decision,
not an oversight — no live, interactive `FormSurface` is wired into the
shipped binary in this pass: `ConwayUiPlugin::new(None)` is what every
dispatch target constructs today, TUI included, so `ui.form` degrades
identically everywhere until a real form drives what a live surface should
look like. See `conway-plugin-ui`'s own module doc ("Host requirement,
declared honestly") for the full reasoning, and this item's own completion
report for why building a rendering surface now — for a proof-of-mechanism
consumer, with no shipped form yet needing a specific widget on screen —
would have been exactly the "designing on theory" INTENT §8.5 forbids.
§8's first falsifier ("`conway.ui` needs to draw, not declare") did not
fire: the declarative request/answer shape expressed everything
`skeleton_ask` needed to ask and to receive back.
