# D3 — Wire vocabulary, the RPC-shaped `ToolCtx`, and stability commitments

Status: design spec (board item 01KYNNA98763ZMPPKJR5DEQY4V). Written against
HEAD `6552f12` (v0.6.0 + today's provider-profile and TUI work). Transport and
framing are D1; the extension-point taxonomy is D2; trust is D4; UI templating
is D5. **This document owns the payloads**: what crosses the pipe, what shape
it has, and what conway promises about that shape over time.

All `file:line` references were read at HEAD.

---

## 0. The organizing claim

Everything below follows from one sentence:

> **Every type admitted to the wire is a promise, and the cost of a promise is
> paid by whoever has to keep it — which is conway, forever, on behalf of SDKs
> it will never see.**

So the wire is *not* "conway-core, serialized". It is a deliberately small,
separately-versioned vocabulary, most of which is a **projection** of an
internal type rather than a mirror of it. The projection is the thing that lets
`SubagentSpec` keep growing — it grew five fields since WI-066 — without
breaking a single plugin.

Two properties are load-bearing and are asserted throughout:

- **P-6 is read as "no built-in reaches a point a third party cannot", not
  "every Rust API is on the wire."** The measuring stick used everywhere below
  is the *built-in, model-facing tool*: `conway_subagent`, `conway_ask`,
  `bash`. If a remote plugin can do exactly what those do, parity holds.
  That comparison turns out to be decisive more often than it has any right to
  be, because those tools already went through the "what should a caller be
  allowed to say?" exercise (§1.3).
- **The `#[non_exhaustive]` + explicit-allow-list pattern from
  `permission_mode.rs:24-31` is replicated at the language boundary.** That
  file's whole point is that a future `ToolCategory::Deploy` is denied the day
  it is added, with nobody editing the file. The wire needs the same property
  in a language with no enums, which means the *SDK contract* must state the
  fail-closed fallback for every enum it can receive (§2.2).

---

## 1. The RPC-shaped `ToolCtx`

`ToolCtx` (`crates/conway-core/src/ports/plugin.rs:281-305`) has seven fields.
The type's own doc has named the problem since T-8: *"Not `Serialize` — it
holds trait objects (`events`, `subagents`)"* (`plugin.rs:256-259`).

### 1.1 Field-by-field disposition

| `ToolCtx` field | Wire disposition |
|---|---|
| `agent_id` | **In `tool/invoke` params**, host-supplied. Never read from a plugin-authored field (D1 §2). |
| `session_id` | **In params**, host-supplied. |
| `cwd` | **In params**, host-supplied absolute path — the same per-batch snapshot every in-process tool gets (`CwdHandle` doc, `plugin.rs:194-202`). |
| `config` | **In params**, but *only this plugin's* config map. See §1.6. |
| `cancel` | **Not a field.** It is D1's `$/cancelRequest` notification plus an SDK-local flag. §1.5. |
| `chdir` | **Not on the wire at all.** §1.5. |
| `events` | **Outbound notification `ctx/event`.** §1.2. |
| `subagents` | **Six inbound request methods `subagent/*`.** §1.3. |

`tool/invoke` params, complete:

```jsonc
{
  "ctx_token": "opaque, host-minted, per-invocation (D1 §2)",
  "call":  { "call_id": "...", "name": "read", "arguments": { } },
  "ctx": {
    "agent_id":   "01J...",           // ULID string (ids.rs: serde(transparent))
    "session_id": "01J...",
    "cwd":        "/abs/path",
    "config":     { },                // this plugin's values only
    "grants":     ["events.progress", "subagents.start", "subagents.await"],
    "limits":     { "deadline_ms": 120000, "max_output_bytes": 262144 }
  }
}
```

`grants` is the **effective** capability set for this token — the intersection
of what the plugin declared at handshake (§3) and what the operator allowed. It
is repeated per invocation rather than only at handshake because it can be
revoked live (D2 §6's `Conway::unregister`-shaped surface), and a plugin that
must guess whether a callback will be refused writes defensive code — which is
the Claude Code failure mode this document exists to avoid.

`limits` is advisory-but-real: `deadline_ms` is D1 §4's inactivity deadline and
`max_output_bytes` is D1 §8's clamp. Telling the plugin the number it is being
measured against costs one field and removes an entire class of "why did my
output get truncated" bug report.

### 1.2 `events` — the outbound, fire-and-forget capability

**Wire form: a JSON-RPC *notification*, `ctx/event`, never a request.**

```jsonc
{ "jsonrpc": "2.0", "method": "ctx/event",
  "params": { "ctx_token": "...", "event": { "event": "tool_progress", "note": "..." } } }
```

There is no `id`, therefore no reply, therefore **the plugin structurally
cannot block on the host**. That is the wire-level expression of
`EventSink::emit`'s "synchronous and non-blocking **by contract**"
(`ports/events.rs:9-14`). A request/response shape here would have quietly
converted a non-blocking contract into a round trip inside the tool's critical
path.

**Restricted variant set.** A plugin may emit exactly two variants:

- `Event::ToolProgress { call_id, note }`
- `Event::AgentProgress { note }`

Anything else — `PermissionResolved`, `AgentFinished`, `Error`, `Lagged` — is
**dropped, counted, and reported via `PluginStatusChanged`**. Reasons, in order
of severity:

1. `Envelope`'s doc restates three *delivery guarantees* at the definition site
   (`event.rs:29-35`): monotonic `seq` per session, `AgentSpawned` precedes
   every event bearing that agent id, exactly one `AgentFinished` per
   `AgentSpawned`. A plugin that can emit lifecycle variants can break all
   three, and every downstream observer, TUI, and session reconstruction is
   built on them holding.
2. The event stream is the audit trail. A plugin able to emit
   `PermissionResolved` can forge a record of a decision that never happened.
3. Parity holds: `bash` — the most privileged built-in — emits exactly
   `ToolProgress` (`conway-tools/src/shell/bash.rs:231,243`) and nothing else.
   No built-in reaches past this line either (P-6).

**`call_id` is overwritten host-side** from the `ctx_token` table, never read
from `params`. D1 §2 already forbids the plugin supplying identity; `call_id`
is identity for this purpose (it is the key `ToolCallStarted` /
`PermissionResolved` / `ToolCallFinished` correlate on).

#### The seq-mutex hazard, addressed

`EventBus::emit` **deliberately holds the seq mutex across `tx.send`**
(`conway-runtime/src/events.rs:56-79`) — assigning `seq` and publishing must be
one atomic step or two racing emitters deliver `N+1` before `N`. One `Mutex`,
shared by an entire agent tree, is therefore the tree's single serialization
point. Four rules keep off-process input away from it:

1. **No wire-backed type ever implements `EventSink`.** The `observe/1`
   bridge (D2) is a `EventBus::subscribe()` *broadcast receiver*, not a sink,
   so it can never be invoked under the mutex and it inherits the existing
   lossy-with-notice guarantee for free. This is the single most important
   sentence in this section: the hazard is avoided by construction, not by
   care.
2. **Inbound `ctx/event` is rate-limited *before* the bus is touched.** D1 §8's
   token bucket (1000/s per connection) is applied by the reader task ahead of
   the `EventSinkHandle::emit` call. The protection the tree actually needs is
   not "the sink must not block" (`BusSink::emit` cannot block) but "bounded
   rate of mutex acquisition from an untrusted source".
3. **The emit happens inline on that connection's reader task, not on a spawned
   task.** `EventBus::emit` is synchronous and non-blocking, so inline is safe;
   spawning would reorder a call's progress notes, which the `call_id`
   correlation makes visible.
4. **Outbound observer delivery never awaits.** The D1 writer's bounded `mpsc`
   is fed with `try_send`; on a full queue the envelope is **dropped with a
   counter** and a synthesized `Event::Lagged { skipped }` frame is sent to the
   plugin, so a slow plugin sees loss exactly the way a slow in-process
   subscriber does (`event.rs:198`, `ports/events.rs:11-14`). Buffer-and-drop,
   never backpressure — stated as a hard SDK requirement in both directions:
   **a plugin's own `emit` must be a non-blocking enqueue onto a bounded local
   queue that drops on full, never an inline write to stdout.**

**Test that pins it:** attach a plugin that never reads its stdin and never
emits, run N turns, and assert a wall-clock bound. If any of the four rules
regresses, that test fails. (D1 §4's "a plugin that never answers" test covers
the request path; this covers the notification path, which has no deadline to
catch it.)

### 1.3 `subagents` — all six methods

`SubagentHost` (`ports/subagent.rs`) is the full bidirectional capability. Its
RPC form is six **requests** (all need replies), all plugin→host, all carrying
`ctx_token`:

| Rust | Wire method | Params (beyond `ctx_token`) | Result |
|---|---|---|---|
| `start(parent, spec)` | `subagent/start` | `spec: WireSubagentSpec` | `{ "agent_id": "01J..." }` |
| `steer(target, text)` | `subagent/steer` | `agent_id`, `text` | `{}` |
| `await_result(target)` | `subagent/await` | `agent_id` | `{ "result": AgentResult }` |
| `cancel(target, reason)` | `subagent/cancel` | `agent_id`, `reason` | `{}` |
| `tree()` | `subagent/tree` | — | `{ "tree": WireAgentTree }` (subtree only) |
| `ask(parent, spec)` | `subagent/ask` | `spec: WireAskSpec` | `{ "outcome": AskOutcome }` |

Five rules govern all six.

**(a) `parent` is never on the wire.** `start` and `ask` take `parent: AgentId`
in Rust; the host supplies it from the `ctx_token` table. This is not
bookkeeping — a `Fork` inherits the parent's *entire context* (GP-02), so a
plugin able to name an arbitrary parent could fork a *different* agent and read
its whole conversation back through `ask`'s reply text. Cross-tree
exfiltration in one call. D1 §2 already states "the plugin never supplies
identity"; this is the case where it bites hardest.

**(b) `agent_id` in `steer`/`await`/`cancel` is authorized against a
per-token set.** The host records every `AgentId` returned by `start`/`ask`
*through this token* and refuses any other target with a typed error.

This is a **new constraint the boundary introduces**, and it should be recorded
as such: `WeakRuntimeHost::steer`/`await_result`/`cancel`
(`conway-runtime/src/subagent.rs:646,680,686`) accept **any** `AgentId` in the
runtime, with no descendant check — `steer` even walks
`tree_ref().path(target)` to find a parent for attribution, so it plainly
expects arbitrary targets. In-process that is ambient authority; across a
boundary it is exactly the kind of thing the boundary exists to make
expressible. (See §6 for the in-process finding this exposes.)

**(c) `WireSubagentSpec` is the projection, and it is *the built-in tool's own
argument set*.** See §2.4 — this is the most consequential single decision in
this document.

**(d) `subagent/await` needs no transport deadline, and that is only safe
because the port guarantees termination.** `await_result`'s contract is
explicit: *"Always terminates: the supervisor synthesizes a result on budget
exhaustion, cancellation, or task panic"* (`ports/subagent.rs:22-25`), and
`Budget::max_steps` is deliberately non-`Option` for exactly this reason
(`agent.rs:137-139`). So the host does **not** impose D1 §4's deadline on it.

But the *enclosing* `tool/invoke` does have one (120 s default), and a
legitimate child can run ten minutes. Resolution, and this is a real seam
between D1 and D3 that must be implemented deliberately:

> **An outstanding `subagent/await` bearing a `ctx_token` counts as activity on
> that token's `tool/invoke` inactivity clock.** D1 §4(c)'s absolute
> `max_total` still applies, so a plugin cannot hold a tool call forever by
> awaiting a child forever — the child's own `Budget` bounds it first.

**(e) Cancellation composes downward.** When the enclosing `tool/invoke` is
cancelled or abandoned (D1 §5):

- every outstanding callback on that token is answered with a typed
  `CtxExpired` error;
- children started through the token with `await_result: true` that have not
  yet resolved are **cancelled by the host** with reason
  `"parent tool call abandoned"`;
- children started with `await_result: false` are **not** cancelled.

The asymmetry is deliberate and matches the built-in exactly:
`wait_for_result` cancels the child when the parent tool call is cancelled
(`conway-tools/src/subagent/tools.rs:196-199`), whereas a fan-out
(`await: false`) child belongs to the *agent*, not to the tool call — the model
gets its `agent_id` back in the tool output and awaits it later with
`conway_await`. Without rule (e), abandoning a wedged plugin would leak live
agents spending tokens unsupervised — D1 §5's own complaint, one level up.

**(f) `subagent/tree` returns the calling agent's subtree, not the tree, and is
off by default.** Two reasons. First, `AgentTreeSnapshot` carries every node's
`session: SessionId` and `agent_def` name (`agent.rs:377-397`) across a tree
that may span unrelated work; a plugin's legitimate question is "did *my* child
finish?", which the subtree answers. Second — and this is why it is
capability-gated off by default rather than merely projected — **no built-in
tool calls `tree()` at all** (verified: every call site is the facade's
`Conway`/`SessionHandle`, `conway/src/conway.rs:547,648,811,904`). Shipping it
on by default would give remote plugins a surface no built-in has, which is the
*inverse* of a P-6 violation but still an unnecessary promise. A plugin that
genuinely wants tree-wide visibility asks for `observe/1`, which is declared,
operator-visible, and already the sanctioned whole-tree surface.

**`ask`'s fork-only invariant needs nothing from the wire.** As of v0.5.0 it is
a typed `RuntimeError::AskRequiresFork { mode }` enforced at the trait boundary
in every build (`error.rs:233-241`, `subagent.rs:758`) — not a
`debug_assert!`. `WireAskSpec` therefore carries **no `mode` field at all**
(mirroring `AskArgs`, `conway-tools/src/subagent/ask.rs:35-49`), and the host
constructs `SubagentMode::Fork`. The invariant is enforced twice, in the two
right places: unrepresentable on the wire, and typed at the port for every
other caller.

### 1.4 Parity or reduction? — the P-6 reconciliation

**Position: full capability parity in *kind*, projected parity in *shape*,
authority derived from the invocation rather than from the plugin.**

The concern in the brief is real — parity means every remote plugin can spawn
agents that spend tokens and run tools. Three facts resolve it without a
reduction:

1. **The authority already came from somewhere: the approved tool call.** A
   remote tool's `invoke` only runs after `ToolRunner::execute_one` →
   registry resolve → schema validate → `PermissionBroker::decide` (root check
   first, `permission.rs:486`) → and only then `tool.invoke`
   (`runner.rs:360`). Approving a tool call has *always* authorized whatever
   that tool does inside `invoke`, including subagent calls — that is the model
   an in-process Rust tool has today. The `ctx_token` dies when `invoke`
   returns (D1 §2), so the capability is exactly co-extensive with the
   approval. No new authority is created.
2. **Adding a second gate on the callback would be a GP-03 violation.** A
   `PermissionBroker::decide` call on `subagent/start` would be a second
   permission mechanism reached by a path that is not a tool call, with its own
   caching/rendering/grant semantics to invent. The sanctioned move for "this
   plugin should not spawn agents" is the tool's own `PermissionClass` and the
   operator's grant of the `subagents.*` capability at load (§3) — one
   mechanism, decided once, inspectable.
3. **Every numeric the plugin supplies is clamped host-side (P-10).**
   `Budget.max_steps`, `deadline_secs`, `max_tokens` are range-checked and
   clamped against host ceilings before a `SubagentSpec` is built — the same
   discipline `deadline_from_secs` already applies to model-supplied budgets,
   which exists because `i64::try_from(..).unwrap_or(i64::MAX)` saturated
   straight into a `Duration::seconds` overflow panic
   (`tools.rs:116-141`). Plus a host-configured **depth cap** and **live-child
   cap per token**. Clamping only ever shrinks.

Where parity is genuinely *not* full, it is because the built-in does not have
it either (`tree`, §1.3(f)) or because the field is embedder-tier (§2.4) —
never because the plugin is third-party.

**Dependency on D4, stated rather than assumed:** D4 owns whether the
`subagents.*` capability requires explicit install-time operator consent, and
whether a project-scoped `plugins.json` may declare it at all. My design
composes with either answer *without changing shape*, because the capability is
resolved once at load (§3) and then simply appears — or does not — in `grants`.
If D4 chooses default-deny, `grants` omits it and every `subagent/*` callback
is answered with a typed `CapabilityNotGranted`. **My position is that the
grant should default to "granted, declared, and visible" for a plugin the
operator installed, and default-deny for a project-scoped plugin discovered in
a cloned repo** — D1 §6 already recommends project entries require opt-in, and
this is the same line. I do not need D4 to agree for this spec to hold.

### 1.5 Two fields that are deliberately *not* on the wire

**`cancel`.** D1 owns the mechanism (`$/cancelRequest` notification, §5).
D3's contribution is a negative: **there is no `ctx/isCancelled` polling
callback, and there must not be.** `ToolCtx::cancel` is poll-only, and every
in-process consumer polls it — `bash` at 50 ms (`bash.rs:160`), the subagent
wait loop at 20 ms (`tools.rs:38`). Translating that literally would be 1 200 –
3 000 round trips per minute per in-flight call, inside the critical path, on
the pipe a wedged plugin is already failing to drain. The notification pushes
the state once; the SDK exposes it as a local flag with the same poll-shaped
API the Rust side has.

**`chdir`.** `CwdHandle` is not exposed as a callback in v1. The sanctioned way
for a remote plugin to move the agent is what `cd` does: **register a tool with
a declared path argument and let the broker check it.** `CdTool` declares
`PathArgs::Named(&["path"])` (`conway-tools/src/fs/cd.rs:39-41`), so
`check_root` resolves and contains that argument *before* `invoke` runs
(`permission.rs:355-405`). A `chdir` callback would reach `CwdHandle::set` —
whose own doc says it "performs no root/containment check"
(`plugin.rs:238-245`) — from a path that never passes the broker at all.
Containment does not actually break (a relocated cwd still makes every
subsequent relative path resolve outside the root and be denied), so this is a
minimality-and-one-mechanism decision rather than a hole. Recorded as such, and
revisitable in v2 with an explicit host-side root check.

### 1.6 `config`

Only the calling plugin's own values cross. `Runtime::new` currently hardcodes
`Arc::new(PluginConfig::default())` shared by every tool
(`runtime.rs:314`) — D1 §6 already noted this and made it moot for subprocess
plugins by delivering config through `initialize` instead. D3 confirms and adds
one rule: **the per-invocation `ctx.config` is the same map delivered at
`initialize`**, re-sent so a plugin needs no cross-call state, and it is
`PluginConfig`'s `values` map verbatim — a plain `serde_json::Map`, no
schema, no promise about its contents beyond "what the operator wrote in
`plugins.json`".

---

## 2. Wire types and their stability treatment

### 2.1 The admitted list

Three treatments:

- **M — Mirrored.** The Rust type's serde shape *is* the wire shape. Justified
  only for types already on a durable serialization path (the JSONL session log
  or the `jsonl` event stream), where conway is already committed and the wire
  adds no new promise.
- **P — Projected.** A distinct `wire::` struct, converted at the boundary. The
  Rust type stays free to change.
- **X — Excluded.** Not on the wire.

| Type | Treatment | Why |
|---|---|---|
| `ToolCall` | **M** | Already round-trip tested (`plugin.rs:552-575`); three fields, unchanged since v0.1. |
| `ToolOutput` | **M** | Ditto. `blocks`/`is_error`/`truncation`/`artifacts`. |
| `ContentBlock` | **M** | `#[non_exhaustive]`, `tag="type"`, on the log path already. |
| `TruncationPolicy` | **M** | `#[non_exhaustive]`, `tag="policy"`, on the log path. |
| `Artifact` / `ArtifactKind` | **M** | `ArtifactKind` is `#[non_exhaustive]`; `Artifact` is not — see §2.2 rule 2. |
| `ToolCategory`, `PermissionClass` | **M** | Both `#[non_exhaustive]`. The fail-closed fallback is the whole point (§2.2). |
| `Usage` | **M** | Five `u32`s; on the log path; `AgentResult` carries it. |
| `Envelope` + `Event` | **M** | The observer surface (D2 §4). `Event` is `#[non_exhaustive]` with a **pinned variant count** (`event.rs:420`) — the exact discipline this document wants everywhere. |
| `AgentResult`, `ResultStatus`, `Fact` | **M** | `subagent/await`'s reply; already the serialized form the built-in tool hands the model (`tools.rs:171`). |
| `AskOutcome` | **M** | `subagent/ask`'s reply; four fields. |
| `ToolSpec` | **P** → `WireToolSpec` | `schema` must become raw JSON (§5); `path_args` and the render template ride along. |
| `PathArgs` | **P** → `WirePathArgs` | `&'static [&'static str]` cannot cross; D1 §3.1's `Box::leak` wrinkle. §2.6. |
| `SubagentSpec` | **P** → `WireSubagentSpec` | 13 fields, 5 added recently. **§2.4.** |
| `AgentTreeSnapshot`/`AgentNode` | **P** → `WireAgentTree` | Subtree-scoped, capability-gated (§1.3f). |
| `Budget` | **P** → `WireBudget` | `deadline: DateTime<Utc>` becomes `deadline_secs: u64` — relative, clamped, no clock-skew semantics on the wire. Mirrors `BudgetArg` (`tools.rs:54-62`). |
| `ToolSelector` | **M** | `#[non_exhaustive]`, three variants, and D2 §8 already made its matcher rule the one paradigm. |
| `PluginManifest` | **P** → `WireManifest` | Gains `points`, `optional_host_caps`; `version` becomes semver-validated (§3). |
| `PluginConfig` | **M** | It is a `serde_json::Map`. There is nothing to project. |
| `ToolError` | **P** | Becomes a JSON-RPC error `code` + `data`, not a serialized enum. §2.7. |
| `PermissionMode` | **M** | Three variants, `rename_all="snake_case"`; needed by `PolicyRequest` (D2 §6). Not `#[non_exhaustive]` today — §2.2 rule 2 applies. |
| `PolicyRequest` / `PolicyVerdict` | **P** | D2 §6 owns the semantics; D3 owns only that they are projections and follow §2.2. |
| `Message`, `SamplingParams`, `ToolResult` | **X** | Backend-facing / runner-internal. A plugin never sees a `Message`; `ToolResult` is the runner's post-truncation record, and `ToolOutput` is the tool's own vocabulary. |
| `ConwayConfig`, `AgentDef` | **X** | Host configuration. A plugin has no business reading the model catalog or role definitions. |
| `PatternRule`, `PermissionFile` | **X** | D2 §10 defines a *new* rule shape for plugin-contributed rules (`deny`/`prompt` only). `PatternRule`'s prefix-plus-metacharacter-gate semantics stay internal, which is fortunate given they are currently inert for every tool but `bash` (D2 §13). |
| `PermissionRequest` | **X** | Broker-internal. `PolicyRequest` is the plugin-facing shape. |
| `Provenance` | **X**, and this resolves D2's open question 4 | `context.append/1` sends `{ role, blocks }`; **the host stamps `Provenance::Plugin { id }`**. A plugin cannot claim provenance it does not have, and `Provenance`'s doc-stated "adding a variant is a breaking wire-format change" (`provenance.rs:7`) stays an internal decision instead of becoming an SDK commitment. |
| `LogRecord`, `SessionMeta` | **X** | D2 §3 excluded `SessionStore` for good reasons; its vocabulary should not leak in the back door. |

That is **~24 admitted types**, of which 9 are projections. For comparison,
Claude Code's hook input passed 40 *fields* on one object.

### 2.2 The four stability rules

**Rule 1 — every wire enum is `#[non_exhaustive]` in Rust *and* has a
documented, fail-closed fallback in the SDK contract.**

`#[non_exhaustive]` protects Rust consumers. It does nothing for a Python SDK,
which will see a tag string it does not know and must decide something. So each
enum ships a stated fallback, chosen the way `permission_mode.rs:24-31` chooses
(match the allowed set explicitly; deny the rest):

| Enum | Unknown tag means |
|---|---|
| `ToolCategory` | Treat as `execute` — the most restricted category, and the one plan mode already denies. |
| `PermissionClass` | Treat as `dangerous`. |
| `TruncationPolicy` | Treat as the host default policy; **never** as `none`. |
| `ContentBlock` | Drop the block, count it, surface via `PluginStatusChanged`. Never render unknown content. |
| `Event` | Ignore (observers are lossy by design, `ports/events.rs:9-14`). The one place "ignore" is the right answer, because an observer changes nothing. |
| `ResultStatus` | Treat as `failed`. Not `completed`. |
| `ToolSelector` | Treat as `only([])` — selects nothing. Narrowing, never widening. |

Two of these — `PermissionMode` and `Role` — should additionally gain
`#[non_exhaustive]` in Rust. `PermissionMode` is on the wire in `PolicyRequest`
and is not currently marked; a future fourth mode would be a silent break.

**Rule 2 — every wire struct: `#[serde(default)]` on every non-identifying
field, and no `#[non_exhaustive]` on the *projection* structs.**

The in-tree precedent is exact and has tests: `AgentNode::ephemeral` and
`Event::AgentSpawned::ephemeral` both carry `#[serde(default)]` with the
backward-compat reasoning written at the field (`agent.rs:391-396`,
`event.rs:60-66`), as do `SubagentSpec::cwd` and `::root`
(`agent.rs:230-234`, `:270-274`). Additive field = `serde(default)` + a
documented default + a test that an old payload still deserializes. That is the
whole mechanism, and it already works here.

`#[non_exhaustive]` is deliberately *not* applied to the `wire::` projection
structs, for the reason `ToolCtx`'s own doc gives at `plugin.rs:261-279`: it
forbids literal construction outside the crate even with every field named,
which forces a builder. The projections are constructed in exactly one place
(the host bridge) and in test fixtures. The protection `non_exhaustive` buys is
already bought by the projection itself — the whole point is that the wire
struct changes on a different schedule from the internal one.

**Rule 3 — `deny_unknown_fields`: ON for hand-authored files, OFF for wire
frames. The provider-profile reasoning transfers, but only half of it.**

`profile.rs`'s module doc argues it precisely
(`conway-backends/src/profile.rs:24-35`): forward-compat (old file, newer
binary) is solved by `serde(default)` on every field; `deny_unknown_fields`
solves the *other* direction, where a typo silently keeps a default and changes
behavior. That reasoning is about a **hand-authored** file.

A wire frame is machine-generated, and there the second direction is not a typo
— it is a **newer peer**. Rejecting an unknown field turns a forward-compatible
additive change into a hard break, which is precisely how a plugin ecosystem
shatters on a minor release. So:

| Payload | `deny_unknown_fields` | Because |
|---|---|---|
| `plugins.json` entries | **ON** | Hand-authored. A misspelled `comand` silently defaulting is worse than a loud error naming the field — profile.rs's argument verbatim. |
| `initialize` result / `WireManifest` | **OFF** | SDK-generated, and it is the frame most likely to carry a field from a newer protocol minor. |
| every `tool/*`, `ctx/*`, `subagent/*` frame | **OFF** | Same. |

And the missing half, which is what makes OFF safe rather than sloppy:
**unknown fields are ignored but never silent.** The host counts them per
(plugin, method, field name) and reports them in `conway plugins` and in the
startup diagnostic. An operator debugging "my plugin's new option does nothing"
gets `unknown field "retry_budget" on tool/invoke result (14 times)` instead of
inferring it from behavior. This is D2 §8's "three ways a non-match is
detectable" applied to fields instead of selectors, and it keeps the loud-typo
property without paying the compat cost.

**Rule 4 — the wire vocabulary is enumerated by a test, the way `Event`
already is.**

`event.rs:420`'s `assert_eq!(variants.len(), 24)` exists "precisely so nobody
adds a variant without updating it". Replicate it:

- **A registry test** listing every admitted wire type with its treatment. A
  new type on the wire fails the test until someone adds it deliberately —
  which is a design review, not a rubber stamp.
- **Golden fixtures.** One checked-in JSON file per wire type per protocol
  minor, in `crates/<host>/tests/wire/`, asserted byte-shape-stable (field
  names and tag strings, not formatting). A field rename is then a failing test
  in the same commit, not a silent SDK break discovered by a third party. This
  is the single highest-value mechanism in this document and it costs one test
  module.
- **A "an old frame still deserializes" test per additive change**, matching
  what `ephemeral`/`cwd`/`root` already do.

### 2.3 `WireToolSpec`

```jsonc
{
  "name": "grep",
  "description": "...",
  "schema": { /* raw JSON Schema, §5 */ },
  "category": "search",             // default: "execute"    (most restrictive)
  "permission": "requires_approval",// default: "dangerous"  (most restrictive)
  "path_args": { "kind": "named", "names": ["path"] },
                                    // default: unconfinable/[]  (fail closed)
  "truncation": { "policy": "tail", "max_bytes": 16384 },  // default: host policy
  "render": null                    // D5 owns the template language
}
```

Every default is the most restrictive value, per D2 §2 and
`PathArgs::default`'s own doc (`plugin.rs:152-157`). An absent `category`
becoming `execute` means plan mode denies it, which is the correct answer for a
tool that declined to say what it does.

**In-flight addition, noticed in the working tree while this was written:**
board item 01KYT3NSWRHMPEAXVXRJ73KDYR is adding a `Tool::render_kind() ->
RenderKind` declarative method (default `ShellCommand`, the conservative
value), so `PatternRule`'s metacharacter gate stops being unconditional. It is
exactly the `path_args`-shaped move this document keeps citing — static,
declarative, fail-closed — and it belongs on `WireToolSpec` as
`"render_kind": "shell_command" | "opaque"`, **defaulting to
`"shell_command"`** so an absent or unknown value keeps the pre-existing
unconditional gating. Adding it is a protocol *minor* bump plus one golden
fixture. Recorded here so it is not discovered late; the field list above
should gain it when that item lands.

### 2.4 `SubagentSpec` — the projection, and why it is the right one

`SubagentSpec` has **13 fields** and gained five recently: `keep_alive`,
`ephemeral`, `ask_origin`, `cwd` (C1), `root` (S3). The brief calls it the
likeliest SDK-breaker. It is — *if it is mirrored*.

**Decision: `WireSubagentSpec` carries eight fields, and they are exactly the
eight the built-in `conway_subagent` tool accepts.**

| Field | On the wire? | Host-supplied value if not |
|---|---|---|
| `mode` | yes | — |
| `prompt` | yes | — |
| `agent_def` | yes | — |
| `role` | yes | — |
| `tools` | yes (`ToolSelector::Only`) | — |
| `budget` | yes (`WireBudget`, clamped) | — |
| `result_contract` | yes (raw JSON Schema) | — |
| `await_result` | yes | — |
| `cache_hint` | **no** | derived: `fork → true`, `spawn → false` |
| `keep_alive` | **no** | `false` |
| `ephemeral` | **no** | `false` (`true` + `ToolAsk` for `subagent/ask`) |
| `ask_origin` | **no** | `None` (`ToolAsk` for `subagent/ask`) |
| `cwd` | **no** | `None` → inherit the parent's |
| `root` | **no** | `None` → inherit the parent's, unchanged |

Three arguments, in increasing order of force:

1. **It is field-for-field the built-in tool's own projection.**
   `SubagentTool::invoke` builds a `SubagentSpec` from `SubagentArgs` and sets
   each excluded field to a constant with a comment explaining why
   (`conway-tools/src/subagent/tools.rs:260-290`): `keep_alive: false`
   ("an opt-in only the interactive-session facade paths ever set"),
   `ask_origin: None`, `cwd: None` ("C1 only adds `cwd` to the facade's
   `SpawnSpec`, not this tool's own args schema"), `root: None` ("GP-04:
   embedder-only"). **P-6 is satisfied exactly, not approximately**: a remote
   plugin can say precisely what the most privileged built-in delegation tool
   can say. The excluded fields are embedder-tier, which is D2 §1's tier line,
   not a plugin-tier restriction.

2. **Every field it recently gained is in the excluded set.** All five —
   `keep_alive`, `ephemeral`, `ask_origin`, `cwd`, `root` — are excluded. The
   eight admitted fields have been stable since WI-066. That is not a
   prediction about churn; it is a measurement of it. The projection is the
   *reason* `SubagentSpec` was able to grow five fields in two releases without
   anyone thinking about compatibility.

3. **`root` in particular must never be plugin-supplied.** `SubagentHost::
   start` implements an inheritance algebra where a requested root wider than
   or sideways from the parent's fails the spawn outright
   (`agent.rs:251-262`). Omitting the field means "inherit, unchanged" —
   the only safe default, and the only one the facade's `ForkSpec` can express
   either. Admitting it would put a confinement-relevant field in a plugin's
   hands to be validated, when "absent" is already the right answer.

`WireAskSpec` is narrower still — `prompt`, `budget`, `tools` — mirroring
`AskArgs` (`ask.rs:35-49`), with no `mode` field (§1.3).

### 2.5 `WireBudget`

`{ "max_steps": u32, "deadline_secs": u64, "max_tokens": u32|null }`, all
optional, all clamped host-side against configured ceilings, with
`deadline_secs > MAX_DEADLINE_SECS` (`tools.rs:126`) a typed error rather than
a saturation. Relative seconds, not an absolute `DateTime<Utc>`: a plugin in a
different process has no business asserting a wall-clock instant, and
`BudgetArg` already made this exact choice for the model-facing tool.

### 2.6 `WirePathArgs`, and the `'static` wrinkle

```jsonc
{ "kind": "none" }
{ "kind": "named",        "names": ["path", "cwd"] }
{ "kind": "unconfinable", "checkable": ["cwd"] }
```

Absent, unknown `kind`, or malformed → `{"kind":"unconfinable","checkable":[]}`.
This is the one field where a malicious manifest could try to buy an auto-allow
(D1 §9.1), and the only way it can is by *naming* its path arguments — which
are then checked by `check_root` against the confinement root. Worth the
explicit test D1 already asks for.

**New validation D3 adds, which falls straight out of having the schema and
`path_args` in the same handshake frame: every name in `names`/`checkable` must
appear as a top-level property of the tool's declared JSON Schema.** A
`path_args` naming a field the schema does not declare is a *silently never
checked* path — `check_root` skips absent arguments by design
(`permission.rs:367-372`), correctly, because `bash`'s `cwd` is optional. So a
typo'd declaration is indistinguishable from an optional argument at check
time, and must be caught at registration instead. Registration error, tool
rejected, plugin named.

On D1's open question 3 (`Box::leak` vs widening `PathArgs` to
`Cow<'static, _>`): **D3's call is `Box::leak` at connect**, bounded by a cap
on count and name length, as D1 recommends. Widening `PathArgs` is a breaking
change to a `#[non_exhaustive]` enum in the most load-bearing core port, to buy
nothing an audited leak of a few dozen short strings per process lifetime does
not already buy. `'static` is *accurate*: the registry lives for the process
(`registry.rs` builds once, immutable by design).

### 2.7 Errors

`ToolError` is **not** serialized as an enum. It becomes a JSON-RPC error with a
stable integer `code` and a `data` object. Rationale: JSON-RPC already has an
error channel, and every SDK's client library surfaces `code` idiomatically; a
second, serde-tagged error type inside `result` means every plugin author
writes a branch nobody documents.

Code ranges, reserved and documented as part of the protocol:

| Range | Meaning |
|---|---|
| `-32700..-32600` | JSON-RPC standard (parse error, invalid request, …) |
| `-32000..-32099` | Transport/host (D1): `RequestCancelled`, `CtxExpired`, `CapabilityNotGranted`, `RateLimited`, `Overloaded` |
| `1000..1099` | `ToolError` mapping: `InvalidArguments`, `Timeout`, `Cancelled`, `Io`, `Internal`, `NotFound` |
| `2000..2099` | Registration/handshake: `SchemaInvalid`, `NameCollision`, `PathArgsUndeclared`, `ProtocolMismatch`, `CapUnavailable` |

Unknown code → the host maps it to `ToolError::Internal` and the plugin maps it
to its SDK's generic error. Fail closed, never fail *open*: **no error code
maps to a success or an allow** (D1 §4's invariant, restated because it is the
one that must never regress).

---

## 3. Versioning, and the capability handshake

### 3.1 The wire schema versions independently of conway. Two levels.

**Level 1 — protocol `{ major, minor }`.** `major` covers the frame vocabulary
and envelope semantics: method names, the `ctx_token` mechanism, the id-space
rules, the error-code ranges. `minor` is additive only: new methods, new
optional fields, new capability names, new `Event` variants.

**Level 2 — per-point versions**, as D2 §11 already proposed: `tool/1`,
`permission.policy/1`, `observe/1`, `context.append/1`. One endpoint's contract
can break without moving the protocol major, and a plugin declares which points
it implements at which version.

Conway's own version appears in the handshake as **informational only**
(`host: { name, version }`), for diagnostics and bug reports. Nothing branches
on it. That is the answer to "the CHANGELOG moves for TUI polish no plugin
cares about": v0.6.0 → v0.7.0 does not move the protocol unless a wire type
changed, and the golden fixtures (§2.2 rule 4) are what prove it did not.

**Compatibility rules, all fail-closed and all typed:**

| Condition | Outcome |
|---|---|
| `plugin.major != host.major` | **Refuse.** `PluginError::Init`, naming both. |
| `plugin.minor_min > host.minor` | **Refuse.** The plugin needs a feature this host does not have. |
| `plugin.minor_min <= host.minor` | **Accept**, whatever the plugin's own minor. Unknown fields ignored-and-counted (§2.2 rule 3). |
| unknown/unsupported version of a **participant** point (`tool`, `permission.policy`, `context.append`) | **Refuse to load.** A permission policy that silently never runs is the worst outcome (D2 §11). |
| unknown/unsupported version of an **observer** point (`observe`, `status`) | **Degrade**: load without that point, warn, `PluginStatusChanged`. |

This confirms and refines D1's open question 6: refuse on major, accept on
minor, with `minor_min` making the direction explicit instead of inferred.

### 3.2 `required_host_caps` finally gets a job

It exists on `PluginManifest` (`plugin.rs:165`) and **nothing reads it** — a
compatibility story someone started and stopped. The handshake gives it
meaning:

- The host advertises a **closed, documented set of capability names** in
  `initialize`. v1 set: `events.progress`, `subagents.start`,
  `subagents.steer`, `subagents.await`, `subagents.cancel`, `subagents.ask`,
  `subagents.tree`, `tool.artifacts`, `observe.envelope`, `status.contribute`,
  `permission.policy`, `context.append`.
- The plugin declares `required_host_caps` (absence is fatal) and
  `optional_host_caps` (absence degrades). The split is new and necessary:
  today a plugin has only the fatal form, so any use of an optional feature
  forces it to be mandatory.
- **A name the host does not recognize is unsatisfiable, therefore fatal in
  `required_host_caps`.** Never "probably fine".
- **A capability the plugin *uses* but did not declare** → the callback is
  answered with `CapabilityNotGranted`, counted; repeated use degrades the
  plugin. Declaration is load-bearing because it is the operator's only
  chance to see a plugin's authority *before* it runs — which is the input D4
  needs for a consent ceremony, and the reason `conway plugins` (D1 §6) must
  print it.
- **`manifest.version` is semver-validated at registration.** Today it is a
  free-form `String`. There is no `semver` crate in the workspace and C-04 says
  do not add one for this: a ~30-line `MAJOR.MINOR.PATCH[-pre][+build]`
  validator in the host crate is sufficient, and an invalid version is a
  refusal naming the string.

### 3.3 The registration panic becomes a typed error — mechanism

`Runtime::new` `.expect()`s `PluginRegistry::from_plugins`
(`runtime.rs:302-305`). `from_plugins` correctly *returns* an error naming both
plugins and the colliding tool (`registry.rs:83-90`); the panic is entirely in
the `.expect()`. Defensible for compiled-in plugins — a duplicate name really
is a programming bug. **A direct P-10 violation the moment a tool name arrives
from a manifest off-process**, and equally for an uncompilable schema
(`registry.rs:99`) and an unserializable one (`registry.rs:90`).

Four steps, smallest possible blast radius:

1. **`PluginProcess::connect` pre-validates** everything it can see alone:
   schema compiles (bounded, §5), name charset, `path_args ⊆ schema
   properties` (§2.6), no *intra-plugin* duplicate names, size caps. Any
   failure → `PluginError::Init`, plugin does not load, named diagnostic. Fail
   closed: a plugin that fails here registers no tools.
2. **Cross-plugin collisions are resolved before `RuntimeDeps` is built.** A
   plugin cannot see its siblings, so this belongs in `ConwayBuilder::build`,
   where the collision check already conceptually lives. D2 §7 already decided
   the *policy* — nobody wins a contested name, both are announced qualified
   `{plugin_id}__{tool_name}`, a diagnostic names both, the operator may pin.
   D3 supplies the *mechanism*: the builder applies that renaming and hands
   `PluginRegistry::from_plugins` a set that is duplicate-free **by
   construction**. The existing error arm is retained (it is a real invariant)
   but becomes unreachable from the plugin path.
3. **`Runtime::try_new(deps) -> Result<Arc<Runtime>, RuntimeError>`**, with
   `Runtime::new` kept as a thin `try_new(..).expect(..)` wrapper.
   `ConwayBuilder::build` calls `try_new`. There is exactly **one** production
   call site of `Runtime::new` (`conway/src/builder.rs:407`) and a dozen test
   ones — so this is a one-line production change with zero test churn, and no
   breaking change to any public signature. That is the P-8-shaped fix.
4. **Add `RuntimeError::PluginRegistration { plugin, tool, detail }`.**
   `RuntimeError` **is** `#[non_exhaustive]` (`error.rs:187`), so this is
   additive. `registry.rs:69-76` documents that it currently smuggles
   registration failures through `ToolError::Internal` because "`RuntimeError`
   has no dedicated registration variant … out of this crate's scope to
   extend". This item is the right scope to extend it: a caller cannot
   currently distinguish "this plugin's manifest is bad" from "a tool failed at
   runtime", and an operator-facing diagnostic needs to.

**Invariant to test: no plugin-supplied manifest content can panic the host.**
Duplicate names, a 100 MB schema, a `$ref` cycle, a `path_args` naming a
nonexistent field, a non-semver version, a 10 000-tool manifest — each is a
typed refusal.

---

## 4. Schema validation is host-local. Confirmed, and why it cannot move.

`PluginRegistry::from_plugins` compiles every `ToolSpec::schema` into a
`jsonschema` validator at registration (`registry.rs:90-99`), and
`Tool::invoke`'s PRE says "`call.arguments` has already been validated"
(`plugin.rs:50-53`). The runner validates before the proposal event
(`runner.rs:268`), which is before the broker.

**A remote tool's schema is compiled locally, by the host, and the host's
validator is the only one that gates a call.** The remote side MAY re-validate;
the host never trusts, waits for, or is affected by that. Three independent
reasons, any one sufficient:

1. **`Event::ToolCallProposed` would carry unvalidated arguments.** It is
   emitted before invoke and is the audit record of what the model proposed.
   Validation after it would make the log describe a call that was never
   checked.
2. **`check_root` reads argument *shape*, and treats a wrong shape as hostile.**
   A declared path argument present with a non-string, non-null value is
   **denied**, not skipped (`permission.rs:398-405`), precisely because
   silently skipping it would let a malformed call past the check. That
   security check depends on the validator having run first, in this process.
3. **The announced schema would become a lie the audit trail records as
   truth.** A compromised plugin that validates its own arguments can accept
   calls its own advertised schema forbids, while the log shows the advertised
   schema.

Corollary already stated in §2.6: `path_args` is validated *against* that
schema at the same moment, which is only possible because both arrive in the
same handshake frame.

**Bounds (P-10).** A plugin-supplied schema is untrusted input to a schema
compiler — `$ref` cycles and deeply nested `allOf` are a known compile-time DoS
shape. So: `MAX_SCHEMA_BYTES` (256 KiB), a compile deadline, and a cap on tools
per plugin. Exceeded → tool rejected, named, plugin degraded. Never a panic,
never an unbounded compile.

### 4.1 `schemars` — the wire carries raw JSON Schema

**Decision: `WireToolSpec.schema` and `WireSubagentSpec.result_contract` are
`serde_json::Value` (raw JSON Schema), not `schemars::schema::RootSchema`.**

1. **Nothing is lost.** `RootSchema`'s serialized form *is* JSON Schema.
2. **The host already round-trips through `Value` anyway.** `registry.rs:90`
   does `serde_json::to_value(&spec.schema)` and feeds the result to
   `jsonschema::validator_for`. Carrying `Value` removes a hop *and* removes a
   failure mode: "schema is not serializable to JSON" (`registry.rs:91-96`)
   becomes unreachable for remote tools.
3. **It removes a real compatibility risk, not a theoretical one.**
   `RootSchema` is a *typed draft-07-flavored model*; MCP servers typically
   emit 2020-12. Unknown keywords survive in `SchemaObject::extensions`, but
   "survive" is doing load-bearing work there — D2 §11 flags this as "a
   compatibility risk to test explicitly, not assume". A raw `Value` has no
   such risk: the schema reaches `jsonschema` verbatim, and `jsonschema`
   supports multiple drafts.
4. **It refuses to pin `schemars 0.8` for every SDK.** Only a *Rust* SDK would
   ever be pinned (no other language has the type), and pinning it would make
   conway's own eventual `schemars` 0.8 → 1.0 migration a breaking change for
   every third-party Rust plugin. That is a large promise bought for nothing.

This answers D2's open question 2 **without changing `ToolSpec` itself** — the
projection absorbs it. Separately recommended, not required by this design:
change `ToolSpec.schema` to `serde_json::Value` in a future release, which
would remove the same seam for in-process third-party tools and delete the
`schemars` dependency from `conway-core`'s public API surface. That is a
breaking change to a non-`#[non_exhaustive]` public struct, so it is a
deliberate v0.7 item, not a drive-by.

---

## 5. What this design refuses to do

Recorded because the refusals are as load-bearing as the inclusions, and each
was actually on the table:

- **No `ctx/isCancelled` poll callback** (§1.5) — thousands of round trips per
  minute in the critical path.
- **No `chdir` callback** (§1.5) — the sanctioned path is a tool with a
  declared path argument, which the broker checks.
- **No plugin-supplied `parent`, `call_id`, `agent_id`-of-record, or
  `root`** (§1.3, §2.4) — identity and confinement are host-resolved.
- **No arbitrary `Event` emission** (§1.2) — three delivery guarantees and the
  audit trail depend on it.
- **No `Provenance` on the wire** (§2.1) — the host stamps
  `Provenance::Plugin { id }`.
- **No `RootSchema` on the wire** (§4.1).
- **No host inference callback** (a plugin calling the host's model). D2 §11
  already deferred it; D3 confirms: it would put token spend behind a callback
  with no per-call approval, no routing story, and no budget owner, and the
  sanctioned path (the plugin issues its own LLM call with its own credentials)
  already works.

---

## 6. Findings in the current tree (independent of this design)

- **`SubagentHost::steer`/`await_result`/`cancel` accept any `AgentId` in the
  runtime with no descendant check** (`conway-runtime/src/subagent.rs:646,680,
  686`). The model-facing `conway_steer`/`conway_await`/`conway_cancel` tools
  parse an arbitrary id string from model-supplied arguments and pass it
  straight through. `RuntimeError::AgentNotInSession` and the facade's
  `ensure_agent_in_session` exist for the *facade's* paths, not this one. Worth
  a separate item: a model that can name a sibling's id (they appear in tool
  output and on the event stream) can cancel or steer it.
- **`PluginManifest.version` is a free-form `String`** never parsed or
  validated (`plugin.rs:163`). Independent of plugins-over-RPC: nothing today
  would notice `version: "banana"`.
- **`PermissionMode` is not `#[non_exhaustive]`** (`permission_mode.rs:38`)
  even though `permission_mode.rs`'s own module doc builds its safety argument
  on `ToolCategory` being so. A fourth mode would be a silent break for any
  external match.
- **`Runtime::new`'s `.expect()` has exactly one production call site**
  (`conway/src/builder.rs:407`), which is what makes §3.3's `try_new` fix
  one line.

## 7. Open questions

1. **D4 — the `subagents.*` grant default.** §1.4 states my position
   (granted-and-visible for an operator-installed plugin; default-deny for a
   project-scoped one) and shows the design composes with either answer. D4
   decides; nothing here changes shape either way.
2. **Does `context.append/1` ship in v1?** §2.1 resolves its wire shape
   (`{role, blocks}`, host-stamped provenance) and thereby D2's open question 4,
   but whether the point ships at all is D2/scoping, not D3.
3. **`Event::PermissionResolved { by: Option<String> }`** — D2's open question
   3. D3's answer if it is wanted: additive field, `#[serde(default)]`, protocol
   *minor* bump, one golden fixture updated. Cheap, and the mechanism is
   already specified. Somebody still has to want it.
4. **Where the `wire::` module lives.** Recommended: inside D1's
   `conway-plugin-host` crate, **not** `conway-core`. `conway-core` should not
   grow a second serialization vocabulary, and keeping the projections in the
   host crate is what physically prevents an internal type from drifting onto
   the wire by accident.
5. **A published JSON Schema for the protocol itself.** Generating one from the
   `wire::` types (schemars is already a dependency) would let SDKs codegen and
   would make the golden fixtures machine-checkable. Real value, real
   maintenance cost; out of scope for this item but the projections are shaped
   so it stays possible.
