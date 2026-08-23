# Extending conway: plugins and hooks

`docs/plugins/` is the **authoritative and only** description of conway's
plugin and hook architecture — what's built, what's decided but not yet
built, and what an author or a feature developer can rely on. There is no
second copy of it: this set used to sit alongside a separate design corpus,
and the corpus is gone, because two documents describing the same thing means
one of them is quietly wrong.

Where a page here says something is **design only**, that is the design — not
a summary pointing somewhere else.

## The set

| Page | Answers | Read if |
|---|---|---|
| [`concepts.md`](concepts.md) | What's a hook, a plugin, an observer, a participant? What may a plugin touch, and under what rule? How do fork and spawn differ for a hook evaluated by inference? How does trust work? | You're reading this set for the first time. Every other page assumes these definitions. |
| [`hooks.md`](hooks.md) — normative hook and extension-point reference | Every hook point, its exact contract, and what happens when it errors, times out, or is absent. | You're implementing a specific hook and need the contract, not the concept. |
| [`trust-and-security.md`](trust-and-security.md) — trust and security | What is an author trusted with? What does conway do and not protect against — stated bluntly, including that a trusted plugin runs with your full privileges? | You're deciding whether to install a plugin, or you're shipping one and need to know what you're accountable for. |
| [`compatibility.md`](compatibility.md) — compatibility promises | What does conway promise not to break across versions — for config files, for the not-yet-built wire protocol, for the facade itself? | You're building against this set as a normative reference and need to know what's safe to depend on. |
| [`authoring.md`](authoring.md) — your first hook | How do I actually write one in Rust, register it, and confirm it fired? | You're ready to build. Its ten-minute walkthrough has been **executed verbatim** against a crate depending only on `conway`. |
| [`subprocess-plugins.md`](subprocess-plugins.md) — the subprocess plugin host | How do I add a tool to a `conway` binary I already built, in a language that isn't Rust, with a `settings.json` edit and no rebuild? | You want a plugin that isn't Rust, or you're evaluating what naming a command in `[plugins].subprocess` actually trusts. |
| [`memory.md`](memory.md) — `conway.memory` | What does the model-writable memory store do, where does it live on disk, and what happens if that directory can't be opened? | You want the model to remember things across turns and across separate `conway` invocations, or you're deciding whether its fail-closed startup behavior blocks you. |
| [`path.md`](path.md) — `conway.path` | What does the `compose_context_path` tool let a model do to a session's future context, what does it report afterward, and how does it avoid silently undoing an earlier exclusion? | You want an operator's stated intent ("forget that dead end", "bring in what we found in that other session") to actually change what a later turn sees, or you're evaluating what this new capability can and cannot read/write. |
| [`discover.md`](discover.md) — `conway.discover` | What does the `search_sessions` tool let a model find that it did not already hold a reference to, what does a search cost, and how wide can it reach? | You want an operator's stated intent ("what did we work out yesterday") to name a session the model never started or spawned, or you're evaluating what this reaches and what it costs before it runs. |
| [`skills.md`](skills.md) — `conway.skills` | What does progressive skill disclosure narrow, and what does `read_skill` cost? | You have full-body skills in context and want to try narrowing them to a one-line index. |
| [`mcp.md`](mcp.md) — the MCP client | How do I bring an existing MCP server's tools into conway, and what is conway's own MCP client (not server) posture? | You have an MCP server already and want its tools available to the model, or you're evaluating what naming one in `[plugins].mcp` actually trusts. |
| [`scripts.md`](scripts.md) — the script convention | How would a hook fire a script in any language, and what does that cost per invocation? | You want a hook in something other than Rust. **Describes a designed convention; no script-dispatching plugin exists yet.** |
| [`inference-hooks.md`](inference-hooks.md) — hooks judged by a model | When should a hook call an LLM rather than express a static rule, and do I fork or spawn? | You're weighing an inference-evaluated hook. Read its "when NOT to use one" section first. |
| [`cookbook.md`](cookbook.md) — worked examples | What does a real hook look like end to end — spilling bulky output to a file, compaction, a permission guardrail, progressive skill disclosure, a status-line observer? | You learn faster from a worked example than from a contract. Five examples, each labeled implementable-today, partially-implementable, or blocked, with two treated explicitly as the architecture's own acceptance tests. |

## Start here: a working hook, honestly scoped

The declarative, no-Rust hook surface described in `concepts.md` — a
configuration block naming an event and a command — is **decided but not yet
built**. If you came here wanting
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

## Adding a tool without a rebuild

A **thin, disclosed slice** of the out-of-process transport now exists:
[`subprocess-plugins.md`](subprocess-plugins.md) — spawn a command named in
`settings.json`, no Rust required, no rebuild of the binary. Read that page
before assuming the wire transport is still "designed and never built": the
two points it wires (`tool.spec/1`, `tool/1`) are real; everything else the
full design describes (a persistent connection, `permission.policy/1`,
`context.hook/1`, `observe/1`, a `plugin` trust subject) is not, and that
page's own "What's left" section names each gap.

## Five shipped first-party plugins

Five capabilities beyond the mechanism itself now ship, each installable
with a one-line `settings.json` edit and no rebuild:

- [`memory.md`](memory.md) — `conway.memory`, a mutable store the model can
  write to (`remember`/`forget`/`list_memories`), injected into context by a
  `ContextHook`. Durable at `<cwd>/.conway/memory` once installed — a memory
  survives a process restart — and **fails the CLI's startup, loudly, rather
  than silently falling back**, if that directory cannot be opened; read its
  own page before installing it.
- [`skills.md`](skills.md) — `conway.skills`, progressive skill disclosure:
  narrows full-body skill context to a one-line index plus a `read_skill`
  tool.
- [`mcp.md`](mcp.md) — the MCP-over-stdio **client**: brings an existing MCP
  server's tools into conway. Not an MCP server — conway does not expose
  itself over MCP.
- [`path.md`](path.md) — `conway.path`, a tool (`compose_context_path`) a
  model calls to compose what a session sends as context on its NEXT turn —
  bring specific records in from another session, leave specific records of
  this session's own history out, or both. Reports what it brought in and
  whether the change falls inside the cached portion of context; refuses
  (never silently patches) a composition that would strand a tool call or
  its result.
- [`discover.md`](discover.md) — `conway.discover`, a tool (`search_sessions`)
  a model calls to find a session or record it does not already hold a
  reference to — one it neither started this turn nor was handed a
  `transcript_ref` for. Metadata-only by default; a `text` argument turns it
  into a bounded content scan. Reports what it searched and what that cost.
  Install alongside `conway.path`: this tool finds, `compose_context_path`
  composes.

## Everything not in this set

- **The persistent-connection remote-plugin transport**, and every point
  beyond `tool.spec/1`/`tool/1` — see [`subprocess-plugins.md`](subprocess-plugins.md)'s
  own "What's left" section for the itemized gap.
- **Fork and spawn as agent-delegation primitives** (the `/fork`/`/spawn`
  commands, `conway_fork`/`conway_spawn` tool calls) are a different,
  already-built topic — see [`docs/agents.md`](../agents.md). `concepts.md`
  covers the same two modes only as they apply to a hook judging by
  inference.
