# Inference-evaluated hooks: judging with a model instead of code

**This page documents a designed, decided capability with no code behind it
yet.** No hook registration surface exists at all in the tree today
(`hooks.md` point 13's status row: `Plugin` has no `hooks()` method), so
there is nowhere for anything this page describes to attach. It's still
worth writing now rather than after the fact — per decision
`01KYTNTRAGX2H72HF4R69XACEX`, an inference-evaluated hook is a **first-class
supported shape**, not a workaround, and the four open questions it left
(how fork-vs-spawn is declared, cost and attribution, determinism, mask
production) were settled by decision `01KYTP2QYE00FJSQAQQ0E37JZP` before this
set was written. What follows states those settled decisions, sourced from
the decision record and `.design/extension-architecture.md` §16, never
invented for this page.

## What they are

A hook is allowed to be evaluated **by inference** rather than by code — it
issues its own LLM call to decide, running as a subagent instead of a
function. `concepts.md`'s "Fork vs spawn, for inference-evaluated hooks"
section owns the base vocabulary; this page is the deeper how-to on top of
it. The clearest example the design corpus works through is a permission
policy that classifies a tool call from its own inference call rather than a
fixed pattern list — but the shape generalizes to any hook point that reads
text and returns a decision: a compaction hook judging what to summarize, a
context-mask hook deciding what's safe to durably exclude.

## Choosing fork or spawn

This is the author's choice to make, and it's sharper than it looks:

- **Fork** — the hook's subagent inherits the calling agent's entire
  ancestry context as an immutable prefix (GP-02). Informed: the hook sees
  everything the parent saw. Expensive, in both token cost and exposure.
- **Spawn** — a clean slate. The hook's subagent gets none of the parent's
  transcript. Cheap, and *structurally* cannot leak it — there's nothing to
  leak.

Frame it as "judge with full context" versus "judge in isolation." A
permission classifier deciding whether one specific tool call should
proceed almost never needs the whole conversation; a compaction-decision
hook plausibly does, because "what's safe to summarize" is a question about
the conversation as a whole.

**State the security asymmetry plainly, because it's the reason the default
below is what it is.** An inference-evaluated hook is already reading
attacker-reachable text — the tool call, the surrounding segments, all of it
can carry indirect injection riding on content a hostile source produced.
Forking multiplies that exposure twice over: the hook's own classifier now
sees strictly more attacker-reachable text than a spawned equivalent would,
*and* the hook's own output (a deny reason, a mask) is itself a channel back
into the model's context — so a wider input widens what an attacker's
injected content can try to launder back out through the hook's own
verdict.

**The declaration surface, default, and override rule — taken from decision
`01KYTP2QYE00FJSQAQQ0E37JZP`, not invented here:**

- A hook declares `subagent_mode: Fork | Spawn` **per registration**, not
  per plugin. One plugin may register a classifier needing no ancestry
  alongside a compaction hook wanting the whole conversation; a single
  manifest-wide flag would force one of them to the wrong default.
- **`Spawn` is the default.**
- `Fork` requires a separately granted **`hook.fork`** capability, following
  `subagent.spawn`'s exact shape: default off, never implied by trust,
  requested (`required_host_caps`/`optional_host_caps`) and separately
  granted.
- An operator **may refuse** a requested `hook.fork` — the hook fails to
  register if declared required, or is skipped with a status change if
  optional.
- An operator **may never force `Fork` onto a hook that declared `Spawn`**,
  and the runtime may never silently downgrade a declared `Fork` to `Spawn`
  and run it anyway. A hook that structurally needs the full conversation,
  given a spawned view instead, would answer a different question with
  apparent confidence rather than failing loudly — the same "never guessed
  at" standard the rest of this architecture holds permission decisions to.

**The precedent this reuses already ships**, outside any hook:
`crates/conway/src/intent.rs`'s classifier for the TUI's natural-language
`/fork`/`/spawn` command runs as an ephemeral, zero-tool
`SubagentMode::Spawn` child (`intent.rs:250`) — a judge with no ancestry and
no tool access, deciding one narrow question from a prompt alone. It is not
a hook and predates this design; it's cited because it's the identical shape
`Spawn` mode would reuse once a hook registration surface exists to attach
it to.

## Cost and bounds

An inference hook issues a **real LLM call**. Three things to know about
what that costs and who pays for it:

- **Bounding.** The same declared-`timeout_ms` clamp that already governs
  every inference-evaluated hook design applies uniformly — a 60 s default,
  operator-raisable to a configured maximum. This was never
  permission-specific; it's a property of issuing an LLM call from inside a
  hook, which decision `01KYTP2QYE00FJSQAQQ0E37JZP` confirmed applies to
  every inference-evaluated hook kind, not only a permission classifier.
- **Attribution — whose budget it spends.** For an in-process hook running
  the zero-tool judge shape above, the spend lands under the hook's own
  ephemeral child session, never folded into the calling agent's own
  `session_usage` (the same reason that accessor already excludes an
  inherited fork prefix — "the parent's own prior conversation, not this
  agent's," one level up: a hook's own inference call is not the calling
  agent's conversation either). The decision requires every
  inference-evaluated hook's spawn to carry a stable **role tag** (the
  `role: Some("guard")` shape `intent.rs`'s own precedent already uses,
  generalized) so "what did my guardrails cost me" is answerable separately
  from "what did my agents cost me," by grouping cost rollups on that tag.
  For a **remote** hook, there is and will be no conway-side accounting at
  all — the plugin spends its own credentials.
- **A decision-bearing call cannot stall the session by emitting progress
  forever, and this is the sharpest finding the settling decision made.**
  The general rule elsewhere in this architecture is that a call's deadline
  resets on progress notifications (`hooks.md`'s failure-semantics section).
  Applied naively to a decision-bearing hook, that rule has a hole: a hook
  emitting a liveness ping every couple of seconds while never actually
  deciding would be certified *healthy* forever — no timeout ever fires, no
  `on_failure` ever runs, and every tool call in the session stalls at that
  hook indefinitely, because it produces neither an allow nor a failure,
  just nothing, on and on. **Decision: decision-bearing calls are excluded
  from the progress-reset rule entirely.** A decision call's deadline is the
  clamped `timeout_ms`, flat, never extended by a progress notification on
  that call's own token — a hook may still emit progress (useful for a
  future liveness display), it simply carries no deadline-reset semantics
  for that call. The rejected alternative — a separate, smaller
  `max_total` for decision calls — was considered and dropped: it only
  delays the same failure to a later, arbitrarily-chosen point, and the
  smallest `max_total` that's actually correct for this call class already
  equals `timeout_ms`, so excluding progress-reset produces the right
  behavior without inventing a second number an operator has to reason
  about.

## The recursion rule

**A hook's own machinery does not re-enter that hook.** Stated generally:
no hook of any kind is invoked for a call made by an agent that hook's own
machinery spawned. Without this, an inference hook that (mistakenly or by
design) gives its judge agent tool access produces a child whose own tool
calls re-enter the same hook, which spawns another judge, which spawns
another child — a loop bounded by nothing but each individual agent's own
step budget, not by anything that recognizes the chain.

**Why the zero-tool judge pattern is the mechanism that actually closes
this, not merely a stylistic choice.** The reference shape — `ephemeral:
true`, `tools: Only(vec![])`, `keep_alive: false` — closes the loop two
ways at once: zero tools means the judge produces zero tool-call proposals,
so there's nothing to re-enter the hook chain with in the first place; and
`SubagentHost::start` is a host callback the judge's spawn goes through, not
a tool call, so the spawn itself never re-enters either. `intent.rs:254` is
the shipped precedent for this exact shape, even though — worth repeating —
it isn't itself a hook.

**But the zero-tool convention is a convention, not an enforced boundary,
which is exactly why an explicit invariant exists on top of it.** A policy
that spawns its judge with `tools: All` instead would produce a child whose
calls genuinely do re-enter `decide` — nothing about the zero-tool shape
*prevents* that, it just happens to be what every current instance chooses.
The settled invariant, checked on `agent_path` (already in scope wherever a
hook decision is made): **no hook evaluation happens for a call made by an
agent that hook's own spawn produced.** It's implemented once, generically,
keyed on `agent_path` the same way for every hook kind — not reimplemented
per kind, which is exactly the kind of duplication this project's own
sanitizer history shows drifts apart over time.

## When NOT to use one

An inference hook costs a real model call and adds real latency to every
decision it touches — everything in "Cost and bounds" above is a cost paid
on the path of a tool call actually running, not background work. **A
static rule that expresses the same intent is faster, cheaper, deterministic,
and works when the plugin is dead.** A pattern rule in `permissions.json`
(`hooks.md` point 6) that denies a specific command prefix costs nothing at
decision time, never times out, and keeps working if every plugin process on
the machine has crashed — an inference hook, by construction, cannot make
either of those last two claims.

Reach for inference only when the decision genuinely cannot be expressed
declaratively — when the judgment depends on reading and understanding
content, not matching a pattern against it. "Deny any `bash` call containing
`rm -rf /`" is a pattern match; write it as one. "Deny a commit message that
plausibly leaks a customer's data" is a judgment; that's the shape inference
hooks exist for.

## Where to go next

[`concepts.md`](concepts.md)'s "Fork vs spawn" section — the base
vocabulary this page assumes and builds on, and the value-class boundary
governing what any hook (inference-evaluated or not) may do to what it's
handed. [`hooks.md`](hooks.md) point 14 — the normative status row for
fork/spawn declaration, and point 8 for the composed permission-policy point
an inference hook would most often attach to. [`authoring.md`](
authoring.md) — the one path that's actually buildable today, and where
"Testing your hook"/"Debugging" apply just as much to a future
inference-evaluated hook as to the in-process ones you can write now.
