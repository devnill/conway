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

---

## 1. Declarative hooks

**Claimed:** [Extending conway](../PHILOSOPHY.md#5-extending-conway) presents a
configured hook surface as a working extension rung between settings and Rust
plugins: a `hooks` block naming events and commands, a matcher on tool name,
structured input on stdin, decisions carried by exit status, and `pre_tool_use`
able to allow, deny, or deny with a reason the model reads. The page also says
the event vocabulary is open, with plugins declaring events of their own that
sit at the same level as the core's.

**Exists today:** the in-process Rust seams only. `ContextHook` (`before_request`
and `on_overflow`) is a core port with no default implementation, and the
permission gate is a consumer-supplied trait. Both require Rust and a rebuild,
which is exactly the cost the hooks rung claims to remove.

**Needed to make it true:**

- A `hooks` block in `settings.json`, discovered on the existing precedence
  chain, with per-event lists and a tool-name matcher.
- Dispatch at the events the core emits: `pre_tool_use`, `post_tool_use`, prompt
  submitted, request assembled, child forked or spawned, child reported, session
  started.
- Registration for plugin-declared events, which is the part with no precedent
  in the design being borrowed from and the part most likely to be deferred into
  never. It needs a namespace so a plugin's events cannot collide with the
  core's or each other, a way for an operator to discover what is hookable given
  what they have installed, and a payload contract the plugin defines rather
  than the core guessing at. Design it alongside the core events; retrofitting
  an open vocabulary onto a closed one means changing the config shape after
  people have written hooks against it.
- Subprocess execution with a documented stdin payload per event, and a
  documented protocol for the response, including how a denial reason reaches
  the model.
- `pre_tool_use` wired into the permission broker rather than beside it, so a
  hook denial is a real denial on every path.
- The security properties the page asserts, which are not free: fails closed
  when a hook errors, times out, or is unreadable; every active hook rule
  visible in the operator surface; each individually revocable.
- A liveness test per the discipline in
  [`CONTRIBUTING.md`](../CONTRIBUTING.md#3-a-check-is-not-established-until-it-has-been-shown-to-fail),
  driving a production entry point and asserting on the observable outcome. A
  security-bearing hook that silently never fires is the worst rung of the harm
  ladder.

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

**Exists today (board item 01KZDC3JQ7W4DY1MG6MBCVB2DV): the shape, and one
worked skeleton — none of the six named capabilities yet.** Four decisions
settle the shape, each stated where a reader will find it
(`docs/embedding.md`'s "First-party plugin tier" section is the fullest
account):

- **Where they live:** one crate per plugin under `crates/`, same layout as
  every other workspace crate (`cargo test --workspace` covers them without
  special-casing). `conway` (the facade) does not, and must never, depend
  on any of them — a first-party plugin is written against `conway::plugin`,
  the identical public surface a third party gets.
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
  single `build_conway` choke point both share.
- **Versioning:** with the workspace (`version.workspace = true`, same as
  every crate here), not independently, and not held to `conway-core`'s own
  strict-semver discipline — that discipline exists because *third-party*
  plugins depend on `conway-core`'s port surface, and a first-party plugin
  is just another consumer of the public facade from `conway`'s own point
  of view.
- **Discovery:** `README.md`'s "First-party plugins" section now describes
  what exists (the mechanism, plus the one worked skeleton) rather than
  what is planned.

**`crates/conway-plugin-skeleton` is the one member so far, and it is not a
real capability.** It registers a single `skeleton_ping` tool that echoes
its argument back — enough to prove the mechanism (absent by default,
installable via `[plugins].install` or `with_plugin`, callable from the TUI,
one-shot, and a library embedder) end to end, and nothing more. Dynamic
routing, compaction, memory, skills, and MCP support are unchanged by this
item: none of the six ships in any form yet, and each is separate, later
work.

**Sequencing note, unchanged.** Routing (entry 4) is still the one to build
first, and not because it is most wanted. It is the hardest test of the
plugin surface now that the tier has a place to put it, so it will find
whatever is missing there before the remaining plugins are written against a
surface that turns out to be inadequate.

**This entry replaces an earlier, weaker one** about shipping "example
implementations" for the deferred decisions. That was the same idea before it
had a name, and the page now commits to considerably more than examples.

---

## 3. Backends as plugins

**Claimed:** [Extending conway](../PHILOSOPHY.md#5-extending-conway) states that
a backend is a plugin on the same surface as everything else, that there is no
privileged inference path and no blessed provider list, and that a provider
conway has never heard of is a plugin you install rather than a patch you
submit. This is the most consequential aspirational claim on the page, because
it is the one a reader is most likely to act on.

**Exists today:** `conway-backends` ships Anthropic and OpenAI-compatible
dialects as a non-optional workspace crate, selected at runtime by a
`[backends.<id>].kind` entry in settings. An embedder can supply their own
through `ConwayBuilder::with_backend`, which is real and is the honest version
of the claim for library users. What does not exist is the declarative path: a
third party cannot ship a backend as an installable plugin the way they can ship
a tool.

**Needed to make it true:** the open question comes first, and it decides the
size of the work. Can the plugin surface carry a backend today, or does it only
carry tools? If the former, this is a formalization: a declaration path,
discovery, and documentation. If the latter, it is an extension-surface
build-out, and it should be scoped as one rather than discovered halfway
through.

Either way the capability story has to come with it, since the page leans on
declared capabilities (context window, tool-calling, streaming, caching
mechanism) being what lets routing stay declarative rather than special-casing
vendors. A plugin-supplied backend that cannot declare those is not on the same
surface as a built-in, and the claim would still be false in the way that
matters.

**The default-set framing raises the stakes.**
[The default set](../PHILOSOPHY.md#the-default-set) now presents the shipped
Anthropic and OpenAI dialects as *plugins that happen to be installed*, swappable
and wrappable through the same surface a third party uses. Today they are a
non-optional workspace crate selected by a config `kind` field, which is a
different thing wearing the same description. The page's `coreutils` argument
only holds if the swap is real, so this entry is now load-bearing for a claim
about conway's structure and not just for a convenience.

**Note the asymmetry while it stands.** The embedder path works and the
declarative path does not, so the claim is true for someone writing Rust against
the facade and false for someone configuring a binary. That is precisely the
split the hooks rung exists to close, and the two entries should probably be
sequenced together.

---

## 4. Routing moved out of the core

**Claimed:** [Extending conway](../PHILOSOPHY.md#5-extending-conway) and
[Decisions conway leaves to you](../PHILOSOPHY.md#6-decisions-conway-leaves-to-you)
both say the core resolves a role to a model and stops, with ordered fallback,
capability filtering, health tracking, circuit breaking, and headroom policy
living in a routing plugin that is not installed by default.

**Exists today:** the opposite arrangement. `conway-routing` is a workspace
crate the facade depends on, and `DeclarativeRouter` is wired in as the default.
All of it is in the core app: 3,904 lines across the router (848), the health
prober (676), capability filtering (665), the two circuit breakers (572),
routing config (521), explain (300), and failure classification (280).

The seam is genuinely there, which is what makes this a relocation rather than a
redesign. `Router` is a port trait in `conway-core`, `ConwayBuilder::with_router`
already lets an embedder replace the whole thing, and the port contract already
requires a `RoutingReason` per candidate. What is missing is a declarative
install path, which is the identical gap entry 3 describes for backends. Both
should be solved once.

**Admission is half landed, and the half that is missing is the consuming
half.** Board item 01KZDC4DKVC4JC3W4KN1WMC43N added `Backend::admit`, the
`Admission` numbers it returns, `BackendError::ContextTooLarge`, and one shared
`check_admission` that every dialect calls. Both shipped adapters implement it
over their own wire bodies. So the page's "a backend answers admissible or not
and says so with numbers" is true today.

The page's next clause is not: "a router uses that answer to pass over a
candidate that cannot take the request." **Nothing calls `admit` outside tests.**
The live request path still admits through `conway_routing`'s own pre-existing
`context_shortfall`, so the tree currently holds two implementations of the same
arithmetic — which P-14 forbids as a steady state and which this entry's work is
what resolves. `admit` is labelled at its declaration site as not yet consumed,
naming this item.

The design question deliberately left for here, rather than improvised there:
how `RoutingError::ContextTooLarge` and `BackendError::ContextTooLarge` relate
at the call site. Settle that before deleting `capability.rs`'s copy.

**Needed to make it true:**

- Split the crate. The core keeps the `Router` port, the `RoutingReason` /
  `ModelRef` / `Observation` / breaker-state vocabulary, and a minimal resolver
  that maps a role to a model. Everything else moves.
- Emit attempt outcomes as something a plugin can observe, since breakers move
  out and still have to see every attempt.
- Keep the router in-process. A breaker observing every attempt across a
  subprocess boundary is a round trip per attempt, so this stays a Rust plugin
  regardless of what cheaper plugin hosts arrive later.
- Decide where `conway routes explain` renders from, given the detail it can
  show now depends on which router is installed.
- Move admission to the backend, not to the router and not left in core
  (**ruled**, see below).


**Admission belongs to the backend (ruled), as a port method rather than a
plugin of its own.** The question of who decides whether a request fits was open
when this entry was written. `Backend` grows a method answering admissible or
not with numbers; each backend plugin implements it; the router consumes
verdicts to skip candidates it cannot use; the core keeps the behavioural half,
which is that a refusal reaches the caller rather than being worked around by
trimming or escalating.

**A standalone admission plugin was considered and rejected.** It would still
need a measurement from whichever backend the request is headed to, so the port
has to exist either way. What remains after adding that port is a headroom rule
and a comparison, and a policy layer sitting on top of per-candidate verdicts is
what the router already is. The second thing in that position would do less than
the first.

**What is irreducibly per-backend**, and the reason this pushes down rather than
being shared wholesale:

- Tokenization. Anthropic, the OpenAI dialects, and a local llama.cpp server
  count differently, so one shared estimator is wrong for at least two of them.
- What draws on the window. Whether a reasoning budget comes from the same
  allowance, how tool schemas are counted, how cache blocks figure. Provider
  semantics, not arithmetic.
- The refusal itself. What overflow looks like coming back, and whether it is
  distinguishable from a rate limit or a malformed request.
- Calibration. Every response reports actual input tokens, so a backend can
  compare its own estimate against that and correct its bias. A shared estimator
  can only average two dialects' errors together.

**What stays shared, and why it has to.** The headroom arithmetic and the fit
comparison are identical everywhere, and
[safety-bearing code](../CONTRIBUTING.md#5-safety-bearing-code) requires one
implementation of a computation a guard depends on rather than a restatement per
callsite. So: a shared helper both plugins call, with the tokenizer as the
injected seam. The failure this avoids is two backend plugins growing slightly
different notions of "fits," one of which quietly omits a check. Headroom the
number is configuration, a default plus a per-role override, read by whichever
backend is answering.

**Do not put a count-tokens API call on the admission path.** Anthropic offers
one, and it is a network round trip per check on the hot path. Local estimation
with the dialect's own tokenizer, reconciled after the fact against reported
usage, gets the accuracy without spending a request to learn whether a request
can be made.

**Two consequences for the split.** `capability.rs` (665 lines) is doing
backend-shaped work inside the routing crate today and should move to the
backend side rather than travelling with the router. And the estimator gains an
owner it has never had: `heuristic-chars4` is currently never compared against
the `input_tokens` the provider reports on the very next response, so its bias is
unmeasured in both directions, causing false rejections and admitting requests
that should have been refused. Closing that loop is a correctness improvement
this relocation makes available rather than a requirement of it.

**Resolves an open tension rather than creating one.** Routing was the largest
concentration of default policy in the system and the one subsystem that shipped
an opinion rather than a seam. Moving it out is the change that makes the
"mechanism in core, policy outside" claim uniformly true instead of true with an
exception nobody had written down.

**Deletes the health prober for free.** The health prober (676 lines, nothing
constructs it, gated behind a baseline that cannot be measured because no
benchmark harness exists) leaves the core with the rest of the crate. Deleting
it outright remains the better answer, but it stops being core debt either way.

---

## 5. Path confinement moves into `conway.fs`

**Claimed:** [Constraining a child](../PHILOSOPHY.md#constraining-a-child-its-tool-set)
says limits on reach belong to the plugin that performs the operation, that
`conway.fs` takes a root confining every path it reads or writes, and that the
guarantee is exact because one plugin does both the checking and the opening.
[Decisions conway leaves to you](../PHILOSOPHY.md#6-decisions-conway-leaves-to-you)
adds that a plugin reaching anything else is expected to confine its own
operations the same way.

**Exists today:** confinement is a harness-level mechanism sitting above every
tool. `AgentRoot`/`CanonicalRoot` live in `conway-core`'s `containment.rs`,
`SubagentSpec::root` carries a root to children, `PermissionBroker` checks it
against a tool's declared `path_args` ahead of the gate and the allow-always
cache, and `--root` plus `ConwayBuilder::with_root` expose it.

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
