# Extending conway: plugins and hooks

`docs/plugins/` is the **authoritative** description of conway's plugin and
hook architecture — what's built, what's decided but not yet built, and what
an author or a feature developer can rely on. If a page here and something
in `.design/` disagree, this page wins.

`.design/` is the reasoning archive, not a second spec. Open it when you want
to know **why** something was decided the way it was — what alternative was
considered and rejected, and what argument closed the question — never to
find out what to build; that's what this set is for.

## The set

| Page | Answers | Read if |
|---|---|---|
| [`concepts.md`](concepts.md) | What's a hook, a plugin, an observer, a participant? What may a plugin touch, and under what rule? How do fork and spawn differ for a hook evaluated by inference? How does trust work? | You're reading this set for the first time. Every other page assumes these definitions. |
| [`hooks.md`](hooks.md) — normative hook and extension-point reference | Every hook point, its exact contract, and what happens when it errors, times out, or is absent. | You're implementing a specific hook and need the contract, not the concept. |
| [`trust-and-security.md`](trust-and-security.md) — trust and security | What is an author trusted with? What does conway do and not protect against — stated bluntly, including that a trusted plugin runs with your full privileges? | You're deciding whether to install a plugin, or you're shipping one and need to know what you're accountable for. |
| [`compatibility.md`](compatibility.md) — compatibility promises | What does conway promise not to break across versions — for config files, for the not-yet-built wire protocol, for the facade itself? | You're building against this set as a normative reference and need to know what's safe to depend on. |
| [`authoring.md`](authoring.md) — your first hook | How do I actually write one in Rust, register it, and confirm it fired? | You're ready to build. Its ten-minute walkthrough has been **executed verbatim** against a crate depending only on `conway`. |
| [`scripts.md`](scripts.md) — the script convention | How would a hook fire a script in any language, and what does that cost per invocation? | You want a hook in something other than Rust. **Describes a designed convention; no script-dispatching plugin exists yet.** |
| [`inference-hooks.md`](inference-hooks.md) — hooks judged by a model | When should a hook call an LLM rather than express a static rule, and do I fork or spawn? | You're weighing an inference-evaluated hook. Read its "when NOT to use one" section first. |
| [`cookbook.md`](cookbook.md) — worked examples | What does a real hook look like end to end — spilling bulky output to a file, compaction, a permission guardrail, progressive skill disclosure, a status-line observer? | You learn faster from a worked example than from a contract. Five examples, each labeled implementable-today, partially-implementable, or blocked, with two treated explicitly as the architecture's own acceptance tests. |

## Start here: a working hook, honestly scoped

The declarative, no-Rust hook surface described in `concepts.md` — a
configuration block naming an event and a command — is **decided but not yet
built** (board item `01KZDC0RDRMMMJHX7SAFMM2Q5A`). If you came here wanting
that, there is nothing to install yet; watch that item.

What you *can* build today, in about ten minutes if you're already set up to
compile the workspace or depend on the `conway` crate: an in-process hook.
`conway::plugin` re-exports the traits you implement — `ContextHook` to
curate or mask what goes into a request, `PermissionGate` to decide whether
a tool call runs — and [`docs/embedding.md`](../embedding.md) has the
builder chain (`ConwayBuilder::with_context_hook`,
`ConwayBuilder::with_permission_gate`) that wires one in. Read
`concepts.md`'s "Hook-first" and "Observers vs participants" sections first
for the vocabulary those traits assume, then "The value-class boundary" for
what your implementation may and may not do to what it's handed.

## Everything not in this set

- **The wire transport** for an out-of-process (non-Rust) plugin —
  `.design/d1-transport.md` — is a design spike, not implemented. Nothing in
  this set describes running a plugin as a separate process, because you
  can't yet.
- **Fork and spawn as agent-delegation primitives** (the `/fork`/`/spawn`
  commands, `conway_fork`/`conway_spawn` tool calls) are a different,
  already-built topic — see [`docs/agents.md`](../agents.md). `concepts.md`
  covers the same two modes only as they apply to a hook judging by
  inference.
