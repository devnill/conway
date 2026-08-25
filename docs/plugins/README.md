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
| [`idiom.md`](idiom.md) — `conway.idiom` | What does the prepended conway-idioms instruction fragment actually say, where does it land relative to an agent def's own system prompt, and why does a subagent never see it? | You want a bare interactive session to carry any harness orientation at all, or you're evaluating what this fragment assumes about which tools are announced. |
| [`skills.md`](skills.md) — `conway.skills` | What does progressive skill disclosure narrow, and what does `read_skill` cost? | You have full-body skills in context and want to try narrowing them to a one-line index. |
| [`names.md`](names.md) — `conway.names` | What does naming an agent actually change, and how does a name interact with the id and short-id it sits alongside? | You are steering more than a couple of agents by id and want a handle you remember instead. |
| [`mcp.md`](mcp.md) — the MCP client | How do I bring an existing MCP server's tools into conway, and what is conway's own MCP client (not server) posture? | You have an MCP server already and want its tools available to the model, or you're evaluating what naming one in `[plugins].mcp` actually trusts. |
| [`claude-compat.md`](claude-compat.md) — Claude Code plugin compatibility | I have a Claude Code plugin directory already on disk — what does conway actually do with it, and what does it name but not use? | You want to point conway at an existing Claude Code plugin, or you're deciding whether its MCP-only, read-at-runtime scope is enough for what you have. |
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

## Ten shipped first-party plugins

**The membership rule for this section:** every id
[`first_party_plugins::bundle()`](../../crates/conway-cli/src/first_party_plugins.rs)
resolves to a `Plugin` candidate gets a bullet below — that function, not
this page, is the source of truth for what the shipped binary can install
through `[plugins].install`; re-derive the list yourself with
`grep 'Arc::new(conway_plugin_' crates/conway-cli/src/first_party_plugins.rs`
whenever this section is in doubt. A bullet links a dedicated page when one
exists; where none does yet, the bullet says so rather than silently
omitting the id. `conway.routing` installs the same way but is resolved by
`bundle`'s sibling `router_bundle()` (it implements `RouterFactory`, not
`Plugin`), and the two backend ids come from another sibling,
`backend_bundle()` — both outside `bundle()` itself, so both are outside
this section's rule; see [`docs/routing.md`](../routing.md) for the former.

**What `[plugins].install` decides, and what it does not.** Naming an id
here decides *whether* that plugin runs; nothing here decides *how* it
behaves once it does. Every id below ships with an opinionated default and
no `settings.json` field of its own — `PluginsConfig` accepts `install`
plus the backends exception and rejects any other key outright
(`#[serde(deny_unknown_fields)]`), so the absence is not something waiting
to be filled in later. Per-agent plugin configuration is a real, separate
mechanism (`conway_core::ports::PluginConfig`, narrowed down a subagent
tree via `Plugin::narrowable_keys`, `conway.fs`'s `root` key its one
production consumer today), but it is reachable only from embedding code,
deliberately, for this first slice — `[S1.5]`, cited at
`crates/conway-tools/src/subagent/tools.rs` and across
`crates/conway/src/subagent_spec.rs`. `first_party_plugins::bundle()`'s own
module doc makes the matching claim from the install side: it is "a worked
example, not a commitment to any of its members individually" — the list
below decides which plugins ship, not what an operator may tune about any
one of them from outside code.

Ten capabilities beyond the mechanism itself now ship, each installable
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
- [`idiom.md`](idiom.md) — `conway.idiom`, prepends a short conway-idioms
  instruction fragment (fork vs. spawn, how an agent ends,
  configuration-dependent tools, context, permissions, budgets, steering)
  near the front of a session's assembled context — what makes a bare
  interactive session, which otherwise sends no system-prompt segment at
  all, carry any harness orientation. Reaches every forked or spawned
  child too, not the root alone (board item `01M0VSKA76NSEHDSH25XJGJ2J5`'s
  ruling — see `idiom.md`'s own "Reach" section for the argument).
- [`names.md`](names.md) — `conway.names`, operator-chosen, renameable
  names for agents: `/conway.names.rename`/`.unname`/`.list` over a store
  shared with the TUI's own `/agents` panel, so a rename is visible
  immediately, with no reload.
- `conway.trim` — a `Curator` that omits tool call/result round-trips older
  than 8 turns, keeping context small as a session grows long. Fully wired
  into the shipped binary, same footing as the four above with dedicated
  pages — no dedicated page in this set yet; see `conway-plugin-trim`'s own
  crate-level doc for the full design. The 8-turn window is an instance of
  the rule stated above, not an exception argued separately: no
  `settings.json` field reaches it, and that crate's own doc adds the one
  fact specific to this constant — it is a curation heuristic an operator
  has no feedback loop to tune, not a budget.
- `conway.history` — `/conway.history.rewind <seq>`/`.mask`/`.checkout`:
  forks the calling session at a sequence number, masks a record out of
  future context, or checks out a prior session as the active one. No
  dedicated page in this set yet; see `conway-plugin-history`'s own
  crate-level doc.
- `conway.stepguard` — repeated-tool-call detection, moved out of the agent
  loop and into the plugin tier so declining it (`PHILOSOPHY.md` §6) is
  something an operator can actually do. No dedicated page in this set yet;
  see `conway-plugin-stepguard`'s own crate-level doc.
- `conway.plugin_skeleton` — **not operator-facing.** A worked example
  proving the install mechanism end to end (one `skeleton_ping` tool, one
  custom event); it performs no real work of its own, and exists so a
  third-party plugin author has a real, shipped, first-party-tier plugin to
  point at instead of a description of one. Listed here for completeness
  against `bundle()`, not as a capability to install. See
  `conway-plugin-skeleton`'s own crate-level doc.

## The MCP client — first-party, but not a `[plugins].install` id

[`mcp.md`](mcp.md) — the MCP-over-stdio **client**: brings an existing MCP
server's tools into conway. Not an MCP server — conway does not expose
itself over MCP. Deliberately excluded from the count above: MCP attaches
through `[plugins].mcp[]` (naming a command to spawn), a config surface
separate from `[plugins].install`, and `first_party_plugins::bundle()` never
resolves it — `crates/conway-cli/src/mcp_plugins.rs` is its own choke
point.

## Claude Code plugin compatibility — first-party, but not a `[plugins].install` id either

[`claude-compat.md`](claude-compat.md) — reads a Claude Code plugin
directory the operator already has on disk (no downloading) and translates
what it can. **Only its MCP server declarations are wired to actually
run** — everything else it finds (`commands/*.md`, `skills/`, `agents/*.md`,
most hook events) is named in an operator-visible report, never silently
imported. Deliberately excluded from the "ten shipped first-party plugins"
count above and from the MCP section immediately above this one: it
attaches through its own `[plugins].claude_compat[]` config surface,
resolved by `crates/conway-cli/src/claude_compat_plugins.rs`, a fourth
choke point neither `first_party_plugins::bundle()` nor
`crates/conway-cli/src/mcp_plugins.rs` ever touches.

## Everything not in this set

- **The persistent-connection remote-plugin transport**, and every point
  beyond `tool.spec/1`/`tool/1` — see [`subprocess-plugins.md`](subprocess-plugins.md)'s
  own "What's left" section for the itemized gap.
- **Fork and spawn as agent-delegation primitives** (the `/fork`/`/spawn`
  commands, `conway_fork`/`conway_spawn` tool calls) are a different,
  already-built topic — see [`docs/agents.md`](../agents.md). `concepts.md`
  covers the same two modes only as they apply to a hook judging by
  inference.
