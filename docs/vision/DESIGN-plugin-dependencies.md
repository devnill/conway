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
`PluginManifest::version` is a bare `String` and there is **no `semver`
crate anywhere in the workspace**. Name-only edges cover every case
currently on the table and can be shipped without that dependency —
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

This is currently blocked by a ruling with an undefined expiry. `[S1.5]`
holds per-plugin configuration to an **embedder-only** surface *"for this
first slice"*; `PluginsConfig` is `#[serde(deny_unknown_fields)]` and
actively refuses a `[plugins.config.<id>]` key. Board item
`01M0V501HZBMWNC6AE45JJXAFK` documented the rule and stopped deliberately,
recording that whether the slice is over is an operator question.

**It is now forced rather than optional.** A bundled, opt-in toolkit whose
consumers configure themselves through it cannot function under an
embedder-only config surface. This page does not decide `[S1.5]`; it records
that the first design requiring the answer has arrived.

---

## 7. Open, with recommendations

**7a. Where the host/toolkit boundary sits.** Three altitudes:

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

**Recommendation: the widget tree**, on the serialisation argument in §2.
Its risk is that "composable primitives plus focus and input routing" is
hand-waving until someone specifies focus, input routing, and modal
stacking — which is where this recommendation is most likely to be wrong,
and the first thing an implementer should attack.

**7b. Versions now or name-only first.** Name-only is much cheaper and
covers everything currently on the table. But `ui.form/1` gaining a widget
is exactly the case where a consumer needs a floor, and retrofitting semver
onto edges already in the wild is worse than starting with it. **Weak
recommendation: name-only first**, on the condition that the limitation is
documented at the manifest field rather than left to be inferred — and
noting the counter-argument is strong enough that the operator may
reasonably overrule it.

**7c. Push versus pull are different machinery.** Asking a question is a
*pull* — a blocking call returning one answer. Augmenting the status bar is
a *push* — a plugin volunteering a value continuously.
`PluginStatusContribution` is the half-built push case (§1). This page has
designed for pull. Whether one capability mechanism serves both, or push
stays a separate declarative contribution, is unresolved.

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
