# Philosophy debt

[`PHILOSOPHY.md`](../PHILOSOPHY.md) is written in the present tense throughout,
including for a few things the code does not do yet. That is deliberate: the
page states the shape the system is meant to have, and the code is then built to
match it, rather than the page trailing the code and describing whatever
happened to get built.

This file is the ledger that makes that method safe. Every present-tense claim
on that page which is not yet true is listed here, with what exists today and
what would make the claim true. **A claim that is not in this list is expected to
be true right now.**

The exemption is narrow and applies only to that page's prose. It does not
extend to `docs/`, to `CHANGELOG.md`, to doc comments, or to config keys and
their defaults, where the rule in
[`CONTRIBUTING.md`](../CONTRIBUTING.md#2-nothing-may-claim-to-be-reached-that-isnt)
applies in full. A settings key that exists and does nothing is a defect no
matter what this file says.

Clear an entry by building it, or by amending the page. Both are legitimate; a
claim that survives here unexamined for long is neither.

**Cite entries by title, not by number.** Clearing one renumbers everything
below it, so a number written down elsewhere goes stale silently and points at
the wrong entry rather than at nothing. Soft cancellation was cleared on
2026-08-07 and the entries below it moved up one: routing 5→4, path confinement
6→5, the subagent tool split 7→6. Board specs written before that date cite the
old numbers. The subagent tool split itself (then entry 6) was cleared later
the same day; nothing was below it, so no further renumbering resulted.
"Routing moved out of the core" (then entry 4) was cleared on 2026-08-08
(board item 01KZFC43J1J06BM4CCWKCKHSNV) and the one entry below it moved up
one: path confinement 5→4. Board specs written before that date (including
this item's own, which cites "entry 5") mean the entry that was numbered 5
when they were written. "Backends as plugins" (then entry 3) was cleared on
2026-08-08 (board item 01KZHF270T3W8GZ7NM6DSNQ4MM) and the one entry below
it moved up one: path confinement 4→3. Board specs written before that date
cite the old number.

---

## 1. Declarative hooks

**Claimed:** [Extending conway](../PHILOSOPHY.md#5-extending-conway) presents a
configured hook surface as a working extension rung between settings and Rust
plugins: a `hooks` block naming events and commands, a matcher on tool name,
structured input on stdin, decisions carried by exit status, and `pre_tool_use`
able to allow, deny, or deny with a reason the model reads. The page also says
the event vocabulary is open, with plugins declaring events of their own that
sit at the same level as the core's.

**Exists today: the configured rung ships, for four of its seven events.** A
`[hooks]` block in `settings.json` is discovered on the existing precedence
chain, parsed, and validated. Four events then dispatch for real:
`pre_tool_use`, plus the three OBSERVATION-ONLY events `post_tool_use`,
`session_starting` and `child_spawned` (board item
01KZS019NHG11RVQYSVT7RG0P5), which fire at `ToolRunner`'s own
`ToolCallFinished` seam, at `Runtime::start_root`, and at the single
`SubagentHost::start` both subagent modes share.

**The two tiers differ in kind, deliberately.** `pre_tool_use` fails CLOSED —
a broken hook denies the call, because that is the safe direction for a
permission decision. The three observation events cannot deny at all and
fail OPEN: `ObservationDispatcher::dispatch` returns `()`, so a hook that
errors or times out is logged and the operation it observed is unaffected.
Whether `child_spawned` may ever deny is an open question, deliberately
deferred and recorded at its dispatch site rather than settled by the shape of
a return type.

A rule whose `event` is `pre_tool_use` is dispatched for
real — `ConwayBuilder::build` filters `[hooks].rules[]` to the enabled
`pre_tool_use` entries, and `PermissionBroker::decide` invokes each through an
injected `HookRunner` at the deny tier, ahead of the mode gate, the grant cache,
pattern allows and `AutoAllow`, so a hook denial is a real denial under every
permission mode. `conway_tools::hook_runner::ProcessHookRunner` spawns the
command as a one-shot subprocess with the documented JSON payload on stdin, and
the denial reason reaches the model in the tool result. No Rust and no rebuild
are involved on that path.

The in-process Rust seams remain alongside it, unchanged and still Rust-only:
`ContextHook` (`before_request` and `on_overflow`) is a core port with no
default implementation, and the permission gate is a consumer-supplied trait.
They are no longer the whole of what exists.

**The embedder asymmetry is deliberate.** `conway-cli`'s `build_conway` calls
`ConwayBuilder::with_default_hook_runner` unconditionally, so a `pre_tool_use`
rule in a `settings.json` driving the CLI fires. A third party linking the
facade directly gets no dispatch until it calls `with_hook_runner` or
`with_default_hook_runner` itself. `HookRunner` is an optional port like
`with_permission_gate` and `with_context_hook` — not called at all is the
default, and `conway-runtime` must not reach a concrete runner on its own
(decision 01KZT642CEZ20K92DYWBTPE2XZ). A rule declared by an embedder that
never injects a runner parses, validates, and is never consulted.

**Needed to make it true:**

- Dispatch at the three events still left: prompt submitted, request assembled,
  and child reported. A rule carrying one of those parses, validates, and does
  nothing. The runner port they will reuse already exists; each needs only its
  own dispatch call site.
- A tool-name matcher. `HookEntry` is `id`, `event`, `command`, `timeout_ms`,
  `enabled` — there is no matcher field, so a `pre_tool_use` rule fires for
  *every* tool call and a hook wanting to act on one tool must filter for
  itself from the payload.
- Registration for plugin-declared events, which is the part with no precedent
  in the design being borrowed from and the part most likely to be deferred into
  never. The namespace rule itself is settled and encoded —
  `conway_core::event_name::validate_event_name` implements it — but **nothing
  calls it**, on either the subscriber or the declaration side, and no plugin
  can declare an event. Still needed: a way for an operator to discover what is
  hookable given what they have installed, and a payload contract the plugin
  defines rather than the core guessing at. Retrofitting an open vocabulary onto
  a closed one means changing the config shape after people have written hooks
  against it.
- The remaining half of the security properties the page asserts. Failing closed
  ships: a hook that errors, times out, or returns an unparseable answer denies.
  **Operator visibility does not** — no surface lists the active hook rules, so
  revocation today means editing `enabled` in the config file rather than
  revoking a rule the operator can see.

<!-- claim-check
entry: Declarative hooks
claim: the three remaining events -- prompt submitted, request assembled, child reported -- dispatch nothing
paths: crates/conway/src crates/conway-runtime/src crates/conway-tools/src crates/conway-cli/src
absent: "(prompt_submitted|request_assembled|child_reported)"
-->

<!-- claim-check
entry: Declarative hooks
claim: the three observation-only events ARE dispatched -- the shipped half, which must not silently regress
paths: crates/conway-runtime/src/observation.rs
present: pub const SESSION_STARTING
-->

<!-- claim-check
entry: Declarative hooks
claim: there is no tool-name matcher; a pre_tool_use rule fires for every tool call
paths: crates/conway/src/config/schema.rs
absent: pub matcher
-->

<!-- claim-check
entry: Declarative hooks
claim: the plugin-event namespace rule is encoded but nothing calls it, on either side
paths: crates/conway/src crates/conway-runtime/src crates/conway-tools/src crates/conway-cli/src crates/conway-plugin-routing/src crates/conway-plugin-backends/src
absent: validate_event_name\(
-->

<!-- claim-check
entry: Declarative hooks
claim: no operator surface lists the active hook rules, so revocation means editing the config file
paths: crates/conway-cli/src/tui/view/settings.rs
absent: [Hh]ook
-->

<!-- claim-check
entry: Declarative hooks
claim: pre_tool_use IS dispatched -- the shipped half, which must not silently regress
paths: crates/conway/src/builder.rs
present: rule\.event == "pre_tool_use"
-->

**Already met, and not to be rebuilt:** a liveness test per the discipline in
[`CONTRIBUTING.md`](../CONTRIBUTING.md#3-a-check-is-not-established-until-it-has-been-shown-to-fail)
covers the shipped path — `crates/conway-cli/tests/hook_runner_wiring.rs`
drives the real compiled binary with a real `settings.json` and asserts the
denial on the persisted tool result. A second event's dispatch owes its own.

**The lineage is the shape, not the vocabulary.** The familiar part is naming an
event and a command, stdin carrying structured input, and the exit status
carrying the decision. The events themselves come from conway's primitives, and
copying another harness's event names would import a model conway does not have.

**Open, and worth settling before building:** whether a hook may *modify* a
request or only allow, deny, and observe. Modification reopens the provenance
question that [The primitives](../PHILOSOPHY.md#1-the-primitives) leans on, and
the answer should be deliberate rather than emergent.

---

## 2. The first-party plugin tier

**Claimed:** [Extending conway](../PHILOSOPHY.md#5-extending-conway) describes a
tier between the core and the ecosystem: plugins written and maintained in this
repository, shipped with it, and not installed by default. It names dynamic
routing, compaction, memory, skills, and MCP support.
[Decisions conway leaves to you](../PHILOSOPHY.md#6-decisions-conway-leaves-to-you)
then leans on it, telling a reader that several deferred decisions already have
a plausible answer they can install.

**Exists today (board item 01KZDC3JQ7W4DY1MG6MBCVB2DV, plus
01KZFC43J1J06BM4CCWKCKHSNV and 01KZHF270T3W8GZ7NM6DSNQ4MM): the shape, and
three occupants.** The page names five capabilities in this tier, not six —
dynamic routing, context compaction, memory, skills, and MCP support (below,
where each occupant is accounted for) — and one of them, dynamic routing, is
now built. The other four — context compaction, memory, skills, MCP
support — are not. `conway-plugin-skeleton` proves the mechanism and is not
itself one of the five. `conway-plugin-backends` is a third occupant that is
not one of the five either — provider adapters, a different page claim,
already cleared (see the renumbering note at the top of this file). Four
decisions settle the shape, each stated where a reader will find it
(`docs/embedding.md`'s "First-party plugin tier" section is the fullest
account):

- **Where they live:** one crate per plugin under `crates/`, same layout as
  every other workspace crate (`cargo test --workspace` covers them without
  special-casing). `conway` (the facade) does not, and must never, depend
  on any of them. A `Plugin`/`Tool` first-party crate is written against
  `conway::plugin`, the identical public surface a third party gets
  (`conway-plugin-skeleton`). A `RouterFactory` first-party crate is a
  narrower, deliberately different case (`conway-plugin-routing`): `Router`/
  `HealthRegistry` implementations are excluded from the `conway::plugin`
  tier outright (`crates/conway/src/lib.rs`'s own "Deliberately NOT here"
  list, §13.5) since a router genuinely needs the routing/capability domain
  `conway::plugin`'s curated surface does not (and should not) carry, so
  this one depends on `conway-core` directly instead — the SAME facade
  independence still holds (`conway` links neither crate), just not the
  identical-to-third-party-authoring claim for this specific extension
  point. A `BackendFactory` first-party crate (`conway-plugin-backends`)
  takes the same `conway-core`-direct shape as routing, for the same
  reason (code relocated wholesale from below the facade, not rewritten) —
  but unlike routing this is a choice, not a structural requirement:
  `Backend`/`BackendFactory` are NOT on `crates/conway/src/lib.rs`'s
  "Deliberately NOT here" exclusion list the way `Router`/`HealthRegistry`
  are, and `crates/conway-thirdparty-backend` (board item
  01KZHF3E1ZG3AZ7F7HHVY324T9, never installed and not itself a member of
  this tier — a stand-in for a repository that does not exist) proves the
  facade-only path is genuinely sufficient: written the way a stranger
  outside this repository would write one, naming exactly one workspace
  crate in its `[dependencies]` — `conway` itself. So there are three
  authoring cases, not two: facade-only (`Plugin`/`Tool`, and, provably,
  `Backend`/`BackendFactory`), and `conway-core`-direct by structural
  necessity (`Router`/`HealthRegistry` only, since the facade's curated
  surface deliberately excludes them).
- **How one is installed:** a new, distinct `[plugins].install` key in
  `settings.json` (`conway::config::schema::PluginsConfig`), deliberately
  NOT folded into `tools.builtin_plugins` (that key is a closed,
  compile-time-known candidate set the facade itself validates; a
  first-party plugin is never a member of it). The facade carries the wire
  shape but never itself acts on it — `ConwayBuilder::config()` (new) lets
  whatever binary or embedder links a given plugin crate read the list and
  call `with_plugin` (or, plausibly, `with_backend`/`with_router` — nothing
  about the mechanism is tool-specific) before `build()`.
  `crates/conway-cli/src/first_party_plugins.rs` is the shipped instance:
  one bundle, resolved for the TUI and one-shot `-p` alike through the
  single `build_conway` choke point both share. A second, later key sits
  beside `install`: `[plugins].default_backends` (same `PluginsConfig`,
  board item 01KZHF270T3W8GZ7NM6DSNQ4MM, decision 01KZHRPZ010R37411R3W1XR5TF),
  default `["anthropic", "openai-compat"]`. It is neither `install` nor a
  member of `tools.builtin_plugins` — a third shape, not a second instance
  of either. The asymmetry is deliberate, and stated in the schema doc
  itself: every other first-party mechanism (a `Plugin`, a `RouterFactory`)
  has an honest absent-configuration fallback (`MinimalRouter` when no
  router factory is installed; a missing tool is simply not offered), so
  `install` staying empty by default costs nothing — nothing in the tier
  runs unasked. A backend has no such fallback: a fresh install with no
  backend attached cannot reach a model at all, a materially worse failure
  mode than a degraded router, so this one first-party pair ships attached
  by default and an operator opts OUT by removing an id from
  `default_backends`, rather than opting in.
- **Versioning:** with the workspace (`version.workspace = true`, same as
  every crate here), not independently, and not held to `conway-core`'s own
  strict-semver discipline. That discipline exists to protect an external
  consumer whose only promise is a published crate version; a first-party
  crate has no such gap to bridge regardless of which surface it touches —
  whether only `conway::plugin` (`conway-plugin-skeleton`) or
  `conway-core`'s port surface directly (`conway-plugin-routing`,
  `conway-plugin-backends`, both `conway-core`-direct per the authoring-case
  bullet above), it is built and tested in the same workspace, on the same
  commit, as the crate it depends on, so a breaking change there and its
  first-party consumer land together rather than needing two negotiated
  versions.
- **Discovery:** `README.md`'s "First-party plugins" section now describes
  what exists (the mechanism, plus all three occupants — skeleton, routing,
  backends) rather than what is planned.

**`crates/conway-plugin-skeleton`, `crates/conway-plugin-routing`, and
`crates/conway-plugin-backends` are the three members so far. One of them
fulfills one of the page's five named capabilities; the other two do not
fall under the named five at all.** The skeleton registers a single
`skeleton_ping` tool that echoes its argument back — enough to prove the
`Plugin`/`Tool` mechanism (absent by default, installable via
`[plugins].install` or `with_plugin`, callable from the TUI, one-shot, and
a library embedder) end to end, and nothing more; it is not itself one of
the five. `conway-plugin-routing` (board item 01KZFC43J1J06BM4CCWKCKHSNV)
IS dynamic routing, the first of the five to be built: the declarative
`Router`/`HealthRegistry` engine `conway` itself used to compile in
unconditionally — ordered fallback chains, capability filtering, and health
tracking with circuit breaking, exactly what the page's own "Routing"
account ([Extending conway](../PHILOSOPHY.md#5-extending-conway)) describes
and nothing more — relocated wholesale behind the SAME `[plugins].install`
mechanism via a second installable identity, `RouterFactory`
(`ConwayBuilder::with_router_factory`), since router *selection* has to be
nameable before backends exist to build one against. The page never asks
for a classifier, an embedding model, or any other learned component here —
that vocabulary appears nowhere on it
(`grep -n "learned\|adaptive\|classifier\|embedding model" PHILOSOPHY.md`
returns nothing). `conway-plugin-routing`'s own permanent commitment to
staying purely declarative (GP-07,
`crates/conway-plugin-routing/src/lib.rs`) is a stronger guarantee than the
page asks for, not a shortfall against it, and reading it as one (as an
earlier version of this entry did) imported a stricter definition of
"dynamic" the page itself does not state. One piece named by an earlier
version of this entry, `HealthProber` (a background per-endpoint liveness
loop that would have fed a second, independent `Probe` breaker), is no
longer deferred — it was retired outright (board item
01KZ802GSF692EKYKQ2TTVCJB8), not built: it had no production call site
anywhere in the tree, and the Transport breaker alone already catches a
dead endpoint recovering (its `HalfOpen` state derives from the clock at
read time, so the very next real request retries it), so wiring the
prober would only have shaved latency off that one request — an
optimization with no measured baseline to justify shipping it. Nothing
about the page's own "Routing" account depended on that piece existing.
`conway-plugin-backends`
(board item 01KZHF270T3W8GZ7NM6DSNQ4MM, formerly this ledger's own
"Backends as plugins" entry, cleared the same day) is the third member and
NOT one of the five capabilities this page names — provider adapters, a
different page claim this page states separately (the "Backends are
plugins too" passage and [The default set](../PHILOSOPHY.md#the-default-set),
both under [Extending conway](../PHILOSOPHY.md#5-extending-conway)) — and it
is the ONE member of the tier that attaches without any `[plugins].install`
entry at all, through the second, default-on key described above. Context
compaction, memory, skills, and MCP support are unchanged by any of the
three: four of the page's five named capabilities still ship in no form,
and each is separate, later work.

**Sequencing note, resolved.** The prior note here said routing (then this
ledger's entry 4) was the one to build first, as the hardest test of the
plugin surface now that the tier had a place to put it. That happened: it
found the gap the `RouterFactory` port item closed (router construction
needs backends and a capability picture that do not exist when
`[plugins].install` is first read, which a bare `Plugin`/`Tool` never
needed) before the remaining plugins are written against a surface that
would otherwise have turned out to be inadequate.

**This entry replaces an earlier, weaker one** about shipping "example
implementations" for the deferred decisions. That was the same idea before it
had a name, and the page now commits to considerably more than examples.

---

<!-- claim-check
entry: The first-party plugin tier
claim: four of the five named capabilities -- compaction, memory, skills, MCP -- are unbuilt, so nothing installs one
paths: crates/conway-cli/src crates/conway/src
absent: conway\.(compaction|memory|skills|mcp)
-->

## 3. Path confinement moves into `conway.fs`

**Claimed:** [Constraining a child](../PHILOSOPHY.md#constraining-a-child-its-tool-set)
says limits on reach belong to the plugin that performs the operation, that
`conway.fs` takes a root confining every path it reads or writes, and that the
guarantee is exact because one plugin does both the checking and the opening.
[Decisions conway leaves to you](../PHILOSOPHY.md#6-decisions-conway-leaves-to-you)
adds that a plugin reaching anything else is expected to confine its own
operations the same way.

**Exists today:** confinement is a harness-level mechanism sitting above every
tool. `CanonicalRoot` (`conway-core`'s `containment.rs`) is the std-only,
symlink- and dotdot-aware resolution primitive; `AgentRoot` and
`PermissionBroker` are one layer up, in `conway-runtime`'s `permission.rs`,
not `conway-core` — the broker checks a root against a tool's declared
`path_args` ahead of the gate and the allow-always cache.
`SubagentSpec::root` (`conway-core`) carries a root to children, and
`--root` plus `ConwayBuilder::with_root` expose it.

**Why relocating improves it rather than merely moving it.** The current design
has a TOCTOU window that follows directly from the split: the broker checks a
path, and the tool opens it later, across a task boundary, so a symlink created
in between defeats the check. A root enforced inside `conway.fs` can resolve and
open as one operation. The scope of the promise also stops overreaching. A
harness-level root appears to cover every tool while only covering the ones that
declare path arguments, whereas a root on `conway.fs` covers exactly what
`conway.fs` does.

**Needed to make it true:**

- Root handling moves into the `conway.fs` tools, using open-relative operations
  so the check and the use are one step.
- `PathArgs` and the broker's pre-gate root check retire, along with
  `SubagentSpec::root`, unless the item below keeps a version of it.
- `--root` and `with_root` either become configuration for `conway.fs` or go
  away in favour of it.

<!-- claim-check
entry: Path confinement moves into conway.fs
claim: confinement is still harness-level -- PathArgs and the broker's pre-gate root check have not retired
paths: crates/conway-core/src/ports
present: PathArgs
-->

<!-- claim-check
entry: Path confinement moves into conway.fs
claim: CanonicalRoot still lives in conway-core, which is also why that crate still does I/O
paths: crates/conway-core/src/containment.rs
present: canonicalize\(\)
-->

<!-- claim-check
entry: Path confinement moves into conway.fs
claim: conway.fs does not yet enforce a root of its own
paths: crates/conway-tools/src/fs
absent: (AgentRoot|CanonicalRoot)
-->

**The open question, and it is the real one: per-child narrowing.**
`SubagentSpec::root` is parent-set and narrowing-only, so a parent can spawn a
child confined to a subtree of its own reach. Ordinary plugin configuration is
not per-agent, so moving the root into `conway.fs` loses that unless something
replaces it. Two shapes:

1. A special case, where `conway.fs` reads a root that a parent may narrow when
   spawning. Cheap, and it re-privileges one plugin in the way GP-style
   reasoning across this repository keeps arguing against.
2. A general capability, where a parent may narrow any plugin's configuration
   for a child, with narrowing semantics the plugin defines. More work, and it
   serves the memory plugin scoping its store, a routing plugin restricting a
   child's models, and cases not yet thought of.

Shape 2 is the better fit for a system whose whole argument is that built-ins
hold no privileges. It should be decided before the move, because shape 1 is
hard to walk back once `conway.fs` has a bespoke path for it.

**Watch for the inversion.** Relocating confinement must not read as caring less
about containment. The claim is that a boundary belongs to whatever can enforce
it, and that a root spanning plugins advertises a promise no single plugin can
keep. If `conway.fs`'s root ends up harder to reach than `--root` was, the
argument stops being honest.

---

## Removed rather than kept

One claim was deleted from
[Knowing what happened](../PHILOSOPHY.md#7-knowing-what-happened) when the hooks
section moved to the present tense: "a capability that exists ahead of its
consumer says so and defaults off." It is a real project promise and it holds
everywhere the exemption above does not reach, but stating it on a page that is
itself written ahead of the code would have been a contradiction a reader could
catch inside a single document. It belongs in
[`CONTRIBUTING.md`](../CONTRIBUTING.md#2-nothing-may-claim-to-be-reached-that-isnt),
where it now lives alone.
