# Plugin and hook concepts

The mental model the rest of this set assumes. Read this before doc 2 (the
per-point reference), doc 3 (trust in depth), doc 4 (authoring), or doc 5
(the cookbook) — none of them re-explain what a hook is, what an observer or
a participant is, what a plugin may touch, or how trust works.

**Status of this page.** conway's plugin architecture is being documented
ahead of being fully built, deliberately — per the amended GP-14, a labeled
forward declaration is a respectable state, and the alternative (silence
until the last piece lands) would leave feature development without the
normative spec it needs to build against. Every section below says plainly
which parts exist in the tree today and which are decided-but-not-yet-built.
Where something is unbuilt, it names the board item tracking it. Nothing here
is aspirational in the sense of "might happen" — everything unbuilt has
already been decided; what remains is implementation.

**What exists today, in one list**, so you don't have to extract it from the
labels below: a plugin is an in-process `Arc<dyn Plugin>` (built-in or
supplied via `ConwayBuilder::with_plugin`/`conway::plugin`) that registers
tools; `ContextHook` (`before_request`/`on_overflow`) and `PermissionGate`
are real, invoked ports with no built-in policy — you supply your own; and
project-file trust (keyed on path + content digest) is real and enforced.
Everything else this page describes — hooks as a generalized registration
surface, the observer/participant point vocabulary, an out-of-process
transport, script-dispatched hooks, and digest-keyed *plugin* trust — is
decided design, not yet implemented. See
[`docs/permissions.md`](../permissions.md#limits) for the same boundary
stated from the operator's side: "conway's only extension mechanism today is
in-process."

## Hook-first

The primary extension mechanism is the hook, not the tool. A **plugin** is
the unit of registration — the thing you install, trust, and grant
capabilities to — and **hooks** are where its behavior attaches: a hook
observes a moment (a tool about to run, a request about to be sent) or
decides an outcome for one. A plugin that provides only tools is still just
a plugin whose only registered surface happens to be `tool/1`.

This centering is a correction, not the original design. Earlier material in
`.design/` centered tool-provision — "a plugin gives conway new tools" — and
treated hooks as one extension point among several. The operator redirected
that (decision `01KYTNTRAGX2H72HF4R69XACEX`): hooks are the primary authoring
surface, and tool-provision is one hook-adjacent capability, not the center.
If you're carrying a mental model from that earlier material, or from a
tool-plugin system in general, put the tool at the edge and the hook at the
center.

**Not yet implemented as a generalized surface.** Today, `Plugin::manifest()`
declares identity and `Plugin::tools()` declares tools
(`crates/conway-core/src/ports/plugin.rs`) — there is no `hooks()` method and
no way for a `Plugin` to register a hook through the trait itself.
`ContextHook` and `PermissionGate` are real, but they are separate ports the
embedder wires in directly (`ConwayBuilder`), not something a `Plugin`
registers. The first rung of hook-first — a declarative `hooks` block that
fires a configured command on a named event — is open, board item
`01KZDC0RDRMMMJHX7SAFMM2Q5A`.

## Observers vs participants

Every hook is one of exactly two structural kinds, and the difference is not
a matter of degree:

- **Observers** receive information and return nothing. A run does not wait
  on one, so it may lag, fail, or be entirely absent without changing
  anything about what happened. The shape itself forbids a denial — there is
  no reply channel for one to travel back on, not a rule that happens to
  forbid using one. `EventSink` (`crates/conway-core/src/ports/events.rs`)
  is the built example: `emit` is synchronous, non-blocking by contract, and
  a slow consumer is dropped from delivery (`Event::Lagged`) rather than
  allowed to stall the runtime.
- **Participants** return a value the runtime acts on. They are bounded in
  time, fail closed when they error or time out, and compose under a rule
  that makes **registration order unobservable** — running participant A
  before B or B before A must never change the outcome. `ContextHook` and
  `PermissionGate` are the built examples: a request is genuinely reshaped by
  what `before_request` returns, and a tool call genuinely does not run
  without a decision from the gate.

**Why order-independence matters.** The moment an outcome depends on
registration order, config authored by different parties becomes an arms
race over who gets to run last or claim the highest priority — and the
result silently depends on who edited most recently, not on what any single
rule actually says. This is why nothing in this architecture uses priority
numbers.

**Not yet implemented: the generalized point vocabulary and composition.**
The design names discrete "points" a plugin registers against —
`tool/1`, `permission.policy/1`, `observe/1`, `context.hook/1`
(`.design/extension-architecture.md` §4) — and specifies how *more than one*
participant's answers compose. Today there is exactly one `PermissionGate`
implementation per embedder (not a composed chain of policies) and exactly
one optional `ContextHook` (not a composed set of context hooks), so the
composition rule is written down but has nothing yet to compose. The remote
transport those points would run over is design-only
(`.design/d1-transport.md`), and doc 2 of this set is where the per-point
contracts, once built, will live.

## The value-class boundary

The single most important table in this set. It appears here, once; every
other page in the set references it rather than restating it.

| Value | What a plugin may do |
|---|---|
| Tool **arguments** | Never rewritten, by anything |
| Permission **verdicts** | Narrow only — veto, never widen |
| **Context** | Edit, drop, replace, mask |

These three rules look like one rule ("plugins are restricted") stated three
times. They are not — each has a different reason, and the third row is the
one a reader consistently gets wrong by pattern-matching on the first two.

- **Arguments.** The permission cache key is computed by digesting a call's
  canonicalized arguments (`CacheKey::for_call`,
  `crates/conway-runtime/src/permission.rs`). Rewrite arguments *after* a
  decision and the authorized bytes no longer match the executed bytes;
  rewrite them *before* a decision and a human's "always allow", granted
  against what they saw, now silently covers arguments they never saw. There
  is no assignment of "what was authorized" that survives either direction,
  so the rule is not "rewrite carefully" — it's "don't." The sanctioned
  alternative is `Deny { reason }`: the model re-proposes, and the new call
  enters with one clean authorship.
- **Verdicts.** A permission policy is expected to be evaluated by inference
  in the general case, and an inference-evaluated policy reads text that may
  be attacker-influenced (the tool call, the surrounding context). "May only
  narrow, never widen" is a property of *what kind of value a verdict is* —
  an authority grant — not a configuration flag a particular deployment
  could flip. Composing multiple narrowing verdicts is safe in a way
  composing multiple widening ones never could be: the tree can only get more
  restrictive as more parties weigh in, never less.
- **Context is different, and it is safe for a reason that is not
  guessable from the first two rows.** A plugin that can already *append* to
  an agent's context can already say anything an editor of that context
  could say — the security line was crossed the moment `context.append`-
  shaped capability existed, not when edit/drop/replace was added on top of
  it. Deleting or replacing what's already there can only make an agent see
  **less** of what actually happened; it cannot make the agent believe
  something that was never said. What still binds context is a different
  property than "never rewrite" — **provenance**: every edit must be
  attributable and every persisted exclusion reversible (a durable mask is
  un-masked by a second record, never by mutating the first). A reader who
  infers "strict everywhere" from the first two rows will wrongly conclude
  context must be locked down the same way; it is exactly as free as the
  first two rows are strict, and for a stated reason, not an oversight.

**What exists today.** `ContextHook::before_request` genuinely can edit and
drop segments in-process — this is not a forward declaration, it's exercised
in `crates/conway-core/src/ports/plugin.rs`'s own tests. `PermissionGate`
genuinely never widens what the built-in rule floor already denies (there is
exactly one decision path; nothing sits between the check and the tool
call). **Not yet implemented:** a remote (out-of-process) plugin reaching
context with edit/drop/replace parity to the in-process hook — the wire
point that would carry it, `context.hook/1`, is specified only for append
and whole-segment exclude; a same-target *replace* primitive for remote
plugins is an open gap in the specification itself, board item
`01KZ844ZXZMVRWC7ZANT7PSM6X`.

## Fork vs spawn, for inference-evaluated hooks

A hook is allowed to be evaluated by inference — it can issue its own LLM
call to decide, running as a subagent rather than as pure code. When it does,
the author chooses one of the same two modes [`docs/agents.md`](../agents.md)
describes for agent delegation, but for a different purpose: not "hand off
work" but "judge this."

- **Fork** — the hook's subagent inherits the calling agent's entire
  ancestry context as an immutable prefix. Informed: the hook sees everything
  the parent saw. Expensive, and — because an inference-evaluated hook is
  already reading attacker-reachable text — a fork widens what an attacker's
  injected content can try to influence, and the hook's own output (a deny
  reason, a mask) is itself a channel back into the model's context, so a
  wider input widens what can be laundered back out through it too.
- **Spawn** — a clean slate. The hook's subagent gets none of the parent's
  transcript. Cheap, and *structurally* cannot leak it, because there is
  nothing to leak.

Frame the choice as "judge with full context" (fork) vs "judge in isolation"
(spawn) and pick the cheaper one unless the hook's job genuinely requires the
history — a permission classifier almost never does; a compaction-decision
hook plausibly does.

**Declaration surface and default — decided, not invented for this page**
(decision `01KYTQTYHW9BNKEEDFEJME90PG`, settling one of the four questions
decision `01KYTNTRAGX2H72HF4R69XACEX` left open): a hook declares
`subagent_mode: Fork | Spawn` **per registration**, not per plugin — one
plugin may register a classifier that needs no ancestry alongside a
compaction hook that wants the whole conversation, and a single manifest-wide
flag would force one of them to the wrong default. **`Spawn` is the
default.** `Fork` requires a separately granted `hook.fork` capability
(following the same shape as `subagent.spawn`'s capability gate): an operator
may refuse a requested `hook.fork`, but may never force `Fork` onto a hook
that declared `Spawn`, and may never silently downgrade a declared `Fork` to
`Spawn` and run it anyway — a hook that structurally needs the full
conversation, given a spawned view instead, would answer a different
question with apparent confidence rather than failing loudly.

The precedent for defaulting an inference judge to a clean slate already
exists in the shipped tree, outside any hook: `crates/conway/src/intent.rs`'s
natural-language classifier for `/fork` and `/spawn` runs as an ephemeral,
zero-tool `SubagentMode::Spawn` child — a judge with no ancestry and no
ability to wander into tool calls, deciding one narrow question from the
prompt alone. It is not a hook, but it is the identical shape a hook's
`Spawn` mode reuses.

**Not yet implemented.** No hook registration surface exists yet
(see "Hook-first" above), so there is nowhere for a `subagent_mode` field to
attach; the `hook.fork` capability is decided but has no code representing
it. Tracked under `01KZDC0RDRMMMJHX7SAFMM2Q5A`.

## Language choice

A hook may fire a script written in any language, not only Rust. This does
not add a second extension mechanism (GP-03 stays satisfied): the script
surface is provided *by a plugin* — one that dispatches to a configured
script per event — so a script-backed hook is still, from the runtime's
point of view, an ordinary hook registered by an ordinary plugin. The script
path layers on top of the one extension mechanism; it does not sit beside
it.

State the cost honestly, because "any language" reads as free and isn't:
spawning a process per invocation costs roughly 10–50 ms for a shell script
and 200–400 ms for a Python one, and that cost compounds across a batch of
tool calls running in parallel. Fine for a hook that fires occasionally —
wrong for one wired to every tool call.

**Not yet implemented.** No script-dispatching plugin exists in the tree.
Tracked under `01KZDC0RDRMMMJHX7SAFMM2Q5A`, the same item that would add the
declarative `hooks` configuration surface a script-backed hook would be
declared through.

## Trust, in one paragraph

Enough to make the rest of this set readable; doc 3 has the detail. Trust is
granted to a specific subject — in the full design, `(kind, id,
content-digest)` — never to a directory: there is no "trust this folder"
operation anywhere in the design or the code. A deny always applies,
regardless of trust; an allow always requires it. Editing trusted content
**de-trusts it silently** rather than re-prompting, on purpose — a prompt
that fires on every `git pull` trains an operator to press "yes" without
reading it, which makes the prompt a latency tax rather than a control.

**What exists today** is narrower than the full design and is real, tested
code: `TrustStore` (`crates/conway/src/config/trust.rs`) implements exactly
one trust subject kind, a project-scoped `permissions.json`, keyed on
`(absolute path, content digest)` — not yet the full `(kind, id, digest)`
triple, because the `id` axis exists to distinguish multiple subjects of the
*same* kind (multiple plugins), and plugins are not a loadable kind yet. A
content edit changes the digest and de-trusts the file exactly as described
above; this is exercised by that module's own tests.

**Not yet implemented:** a `plugin` trust kind. conway's only extension
mechanism today is in-process (`Arc<dyn Plugin>`, linked into the binary in
Rust before the process starts), so there is nothing an on-disk,
digest-checked trust record could gate — trusting the binary and trusting
its plugins are the same act today. [`docs/permissions.md`](../permissions.md#limits)
states this from the operator-facing side. No board item names the plugin-
trust build specifically yet; it depends on the out-of-process transport
(`.design/d1-transport.md`), which is itself design-only.

## Glossary

- **Plugin** — the unit of registration: an implementor of the `Plugin` trait
  (`crates/conway-core/src/ports/plugin.rs`), in-process today, out-of-process
  in the design. Declares an identity and, today, its tools.
- **Manifest** — a plugin's static identity: `PluginManifest { id, version,
  tools, required_host_caps }`.
- **Hook** — an attachment point where a plugin's behavior runs. Concrete,
  built examples: `ContextHook`, `PermissionGate`. See "Hook-first" above for
  what's still design.
- **Point** — the design's name for a named, wire-addressable hook (`tool/1`,
  `permission.policy/1`, `context.hook/1`, `observe/1`). Not yet built; see
  "Observers vs participants" above.
- **Observer** — a hook kind that receives information and returns nothing;
  cannot deny or alter a run by construction, not by rule. See `EventSink`.
- **Participant** — a hook kind that returns a value the runtime acts on;
  bounded, fail-closed, composed order-independently.
- **Capability** — a named permission a plugin requests from the host
  (`PluginManifest::required_host_caps`) and the host separately grants,
  never implied by trust alone. Declared today; not yet consumed anywhere in
  the tree — no code reads `required_host_caps` to gate anything.
- **Trust subject** — the specific thing a trust decision is made about: in
  the full design, `(kind, id, content-digest)`; built today for exactly one
  kind, `(absolute path, content-digest)` for a project's `permissions.json`.
  Never a directory.
- **Digest** — a `blake3` content hash of the exact bytes a trust decision
  covers (`content_digest`, `crates/conway/src/config/trust.rs`). Changing
  the bytes changes the digest, which is how an edit silently de-trusts.
- **Narrowing** — a permission verdict may only make an outcome more
  restrictive than the floor already in force; it may never grant something
  the floor denies.
- **Verdict** — the outcome of a permission decision. Built today as
  `PermissionDecision` (`crates/conway-core/src/agent.rs`), returned by a
  single `PermissionGate`; the design's composed multi-policy verdict
  (`PolicyVerdict`) has no code yet, per "Observers vs participants" above.
- **Segment** — one unit of an assembled request's content —
  `PromptSegment` (`crates/conway-core/src/segment.rs`) — the thing a context
  hook edits, drops, or replaces.
- **Provenance** — the recorded origin of a segment or a log record —
  `Provenance` (`crates/conway-core/src/provenance.rs`) — what makes a
  context edit attributable rather than anonymous.
- **Confinement root** — the filesystem boundary a session can be started
  under (`--root`), independent of and stricter than any permission rule.
  See [`docs/permissions.md`](../permissions.md#confinement) for what it does
  and does not guarantee.

## Where to go next

[`docs/plugins/README.md`](README.md) routes you to the rest of the set —
doc 2 for the per-point contracts this page deliberately left out, doc 3 for
trust and the compatibility promise in full, doc 4 to write your first hook,
doc 5 for worked examples.
