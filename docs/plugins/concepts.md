# Plugin and hook concepts

The mental model the rest of this set assumes. Read this before doc 2 (the
per-point reference), doc 3 (trust in depth), doc 4 (authoring), or doc 5
(the cookbook) — none of them re-explain what a hook is, what an observer or
a participant is, what a plugin may touch, or how trust works.

**Status of this page.** conway's plugin architecture is being documented
ahead of being fully built, deliberately — a labeled
forward declaration is a respectable state, and the alternative (silence
until the last piece lands) would leave feature development without the
normative spec it needs to build against. Every section below says plainly
which parts exist in the tree today and which are decided-but-not-yet-built.
Where something is unbuilt, it names the item tracking it. Nothing here
is aspirational in the sense of "might happen" — everything unbuilt has
already been decided; what remains is implementation.

**What exists today, in one list**, so you don't have to extract it from the
labels below: a plugin is an in-process `Arc<dyn Plugin>` (built-in or
supplied via `ConwayBuilder::with_plugin`/`conway::plugin`, or selected by
id via `ConwayBuilder::install_selected`) that registers tools, and — more
recently — may
also register TUI `commands()` and declare/fire its own `events()`;
`ContextHook` (`before_request`/`on_overflow`) and `PermissionGate` are
real, invoked ports with no built-in policy — you supply your own; project-
file trust (keyed on path + content digest) is real and enforced; and, as of a later item, a **declarative** `hooks.rules[]`
block in `settings.json` — no Rust required — really is dispatched: all
seven core events, narrowable to one tool by a `match` field, given a
`HookRunner` injected (`docs/plugins/authoring.md`'s "Ten minutes to a
working hook" walks through it end to end); and a THIN, disclosed slice of
the out-of-process transport (`tool.spec/1`/`tool/1` only, one-shot exec —
[`subprocess-plugins.md`](subprocess-plugins.md) is the normative
reference) really does let a shipped binary gain a tool it was never
compiled with. What's still decided design, not
implemented: the generalized observer/participant point vocabulary and
composition rule (today there is exactly one `ContextHook` and one
`PermissionGate` per embedder, never a composed set), the transport's
own persistent-connection shape and every point beyond `tool.spec/1`/`tool/1`
(`permission.policy/1`, `context.hook/1`, `observe/1`), a *plugin*-authored
script-dispatching hook (the dispatching
above is the runtime's own built-in `ProcessHookRunner`, not something a
third-party `Plugin` provides). Digest-keyed *plugin* trust was considered
and declined rather than left as still-design — see "Trust, in one
paragraph" below for the conclusion. **Stale as of this transport's own
thin slice landing**, flagged here rather than silently left wrong:
[`docs/permissions.md`](../permissions.md#limits)'s "conway's only extension
mechanism today is in-process" claim (stated from the operator's side) no
longer covers a `[plugins].subprocess` entry, which is a real, out-of-process
`Tool` source now — that page's own correction is separate, later work.

## Hook-first

The primary extension mechanism is the hook, not the tool. A **plugin** is
the unit of registration — the thing you install, trust, and grant
capabilities to — and **hooks** are where its behavior attaches: a hook
observes a moment (a tool about to run, a request about to be sent) or
decides an outcome for one. A plugin that provides only tools is still just
a plugin whose only registered surface happens to be `tool/1`.

This centering is a correction, not the original design. Earlier design
material centered tool-provision — "a plugin gives conway new tools" — and
treated hooks as one extension point among several. The operator redirected
that: hooks are the primary authoring
surface, and tool-provision is one hook-adjacent capability, not the center.
If you're carrying a mental model from that earlier material, or from a
tool-plugin system in general, put the tool at the edge and the hook at the
center.

**Partially implemented as a generalized surface — and this correction
matters more here than anywhere else in the set.** `Plugin::manifest()`
declares identity, `Plugin::tools()` declares tools, `Plugin::commands()`
declares TUI slash commands, and `Plugin::events()` declares hook events a
plugin may itself fire (`crates/conway-core/src/ports/plugin.rs`) — but
there is still no `hooks()` method, and still no way for a `Plugin` to
register a *script-dispatched* hook through the trait itself; `ContextHook`
and `PermissionGate` are real, separate ports the embedder wires in directly
(`ConwayBuilder`), not something a `Plugin` registers. **The first rung of
hook-first that this section used to call open is now built**: a
declarative `hooks.rules[]` block in `settings.json` fires a configured
command on a named event, narrowed to one tool by `match`, dispatching all seven core events plus any
plugin-declared one (`docs/plugins/hooks.md` point 13's Status row is
normative; `docs/plugins/authoring.md` is the executed walkthrough). What
remains open under the same umbrella tracking item:
a *plugin* registering a hook through the
`Plugin` trait itself (as opposed to an operator writing a `hooks.rules[]`
entry by hand), and the generalized observer/participant composition rule
the next section describes.

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
— and specifies how *more than one*
participant's answers compose. Today there is exactly one `PermissionGate`
implementation per embedder (not a composed chain of policies) and exactly
one optional `ContextHook` (not a composed set of context hooks), so the
composition rule is written down but has nothing yet to compose. The remote
transport those points would run over is design-only, and doc 2 of this set is where the per-point
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
plugins is an open gap in the specification itself.

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
(settling one of the four questions left open): a hook declares
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
it. Tracked under the same declarative `hooks` charter as the rest of this section.

## Language choice

A hook may fire a script written in any language, not only Rust. **The
user-visible capability this section describes is now real, built
differently than this section originally proposed — a reconciliation, not
a claim that stands unchanged.** The design's original shape was a
script-dispatching *plugin*: an ordinary `Arc<dyn Plugin>` whose own
implementation shells out per event, so a script-backed hook would still be,
from the runtime's point of view, an ordinary hook registered by an ordinary
plugin. **What actually shipped is
the runtime's own built-in `HookRunner` port**
(`conway_tools::hook_runner::ProcessHookRunner`) consulted directly by a
`hooks.rules[]` entry's `command` — no `Plugin` in between at all. The
observable result for an author is identical to what this section always
promised (write a script in any language, name it in config, it runs on the
event); the mechanism underneath is not the one originally sketched.
`docs/plugins/authoring.md`'s "Ten minutes to a working hook" is the
executed walkthrough of the shipped path.

State the cost honestly, because "any language" reads as free and isn't:
spawning a process per invocation costs roughly 10–50 ms for a shell script
and 200–400 ms for a Python one, and that cost compounds across a batch of
tool calls running in parallel. Fine for a hook that fires occasionally —
wrong for one wired to every tool call.

**Still not yet implemented, and this is the part of the original design
that remains open:** a script-dispatching *plugin* in the originally-sketched
sense — a third-party `Plugin` whose own `tools()`/`commands()`/`events()`
happen to be backed by a script, distinct from the runtime's own built-in
runner. Nothing today lets a `Plugin` implementor delegate its *own* trait
methods to an external script the way `hooks.rules[]` delegates a core
event; a plugin author still writes Rust. Tracked under the same declarative `hooks` charter.

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

**Still not implemented, and now a considered position rather than "nothing
to gate yet":** a `plugin` trust kind. A subprocess plugin
(`[plugins].subprocess`, [`subprocess-plugins.md`](subprocess-plugins.md))
and an MCP-over-stdio plugin (`[plugins].mcp`, [`mcp.md`](mcp.md)) are both
real, out-of-process artifacts now — the "a `plugin` kind nothing can yet
consume would be a capability with nothing behind it" argument that used to
justify leaving this open no longer applies verbatim, since something COULD
now consume a digest for it. Board item `01KZHVFCN6ZEAXV7K5JHRQN1YB` was
reopened on that basis and worked to a conclusion: a `plugin` trust kind was
considered and DECLINED, because a digest check gated onto only the
out-of-process transports — while `[hooks].rules[].command` stays
permanently ungated — would assert a distinction (plugins reviewed, hooks
not) that the identical unsandboxed, full-privilege execution underneath
both does not support; see
[`trust-and-security.md`](trust-and-security.md#what-trust-is) for the full
reasoning. The operator's own review of what they typed into
`[plugins].subprocess[]` or `[plugins].mcp[]` is the whole control point
today, on the identical footing `[hooks].rules[].command` already has — see
[`subprocess-plugins.md`](subprocess-plugins.md)'s own "Trust" section.
[`docs/permissions.md`](../permissions.md#limits)'s "trusting the binary and
trusting its plugins are the same act" is now stale for the in-process case
specifically (a subprocess plugin is trusted separately, by naming its
command, not by trusting the binary) — that page's own correction is
separate, later work.

## Glossary

- **Plugin** — the unit of registration: an implementor of the `Plugin` trait
  (`crates/conway-core/src/ports/plugin.rs`), in-process (compiled in) or, as
  a thin, one-shot-exec slice, out-of-process
  (`conway_plugin_subprocess::SubprocessPlugin`, itself an in-process `Plugin`
  that answers `manifest`/`tools` by spawning a subprocess — see
  [`subprocess-plugins.md`](subprocess-plugins.md)); the FULL out-of-process
  design (a persistent connection, every point) remains design only. Declares
  an identity and, today, its tools, its TUI
  `commands()`, and the hook `events()` it may itself fire.
- **Manifest** — a plugin's static identity: `PluginManifest { id, version,
  tools, required_host_caps }`.
- **Hook** — an attachment point where behavior runs. Concrete, built
  examples: `ContextHook`, `PermissionGate`, and, declaratively, a
  `hooks.rules[]` entry naming a core or plugin-declared event. See
  "Hook-first" above for what's still design.
- **Point** — the design's name for a named, wire-addressable hook (`tool/1`,
  `permission.policy/1`, `context.hook/1`, `observe/1`). Not yet built; see
  "Observers vs participants" above.
- **Observer** — a hook kind that receives information and returns nothing;
  cannot deny or alter a run by construction, not by rule. See `EventSink`.
- **Participant** — a hook kind that returns a value the runtime acts on;
  bounded, fail-closed, composed order-independently.
- **Capability** — a named permission a plugin requests from the host
  (`PluginManifest::required_host_caps`) and the host separately grants,
  never implied by trust alone. Consumed: the `conway` builder consults
  `required_host_caps` at registration (the manifest-validation seam),
  comparing each declared cap against `conway::HostCaps::from_config` and
  refusing a plugin whose cap the host lacks with
  `PluginError::MissingHostCapability`. The cap set is an **open**,
  `#[non_exhaustive]` `HostCapability` enum, not a free-form `Vec<String>`
  the host never validates: two core-blessed bare names, `subagent`
  (offered by the `conway` runtime's always-present `SubagentHost`) and
  `persistent_transport` (offered iff a `[plugins].subprocess[]` entry is
  configured `persistent`), plus a shape-checked, catch-all `Named(String)`
  variant for anything else a plugin declares (a malformed name still fails
  to parse; only a well-formed, previously-unknown one succeeds). Opening
  the vocabulary widened what PARSES, not what a host GRANTS — a cap the
  host does not offer, whether one of the two core names or an open one,
  still refuses the plugin at this same gate. Empty `required_host_caps`
  means "needs nothing the host might lack."
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
