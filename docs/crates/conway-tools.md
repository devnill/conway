# conway-tools

`conway-tools` is the home of conway's **only extension mechanism**: the
`Plugin`/`Tool` traits from [`conway-core`](conway-core.md), plus the four
built-in plugins that implement them with no special privilege. This is the
primary reference for "individual plugins" documentation — see
[`/ARCHITECTURE.md §3.4`](/ARCHITECTURE.md) for how tools-as-plugins fits
the whole system.

## Responsibility and boundary

This crate provides **no privileged capability**. Every built-in plugin —
filesystem, shell, subagent, report — is a plain `Arc<dyn Plugin>` built
from the exact same `Plugin`/`Tool` traits a third party would implement,
and every runtime interaction goes through the ports handed down in
`ToolCtx` (`events`, `subagents`, `cancel`, `config`). `conway-tools` MUST
NOT depend on `conway-runtime`, `conway-session`, `conway-routing`, or
`conway-backends` — this is a boundary rule, not just a preference: it is
what proves a tool cannot reach outside the sandboxed interface `ToolCtx`
defines.

```
fs        cd, read, write, edit, glob, grep (FsPlugin)
shell     bash                              (ShellPlugin)
subagent  conway_subagent, conway_ask,
          conway_steer, conway_await,
          conway_cancel                    (SubagentPlugin)
report    report                            (ReportPlugin)
```

`registry::builtin_plugins()` is the single registration entry point the
[`conway`](conway.md) facade consumes — it returns the four plugins above,
in registration order, each a plain `Arc<dyn Plugin>` with no side channel
to the runtime. If a future built-in needs a new capability, it is added
to `ToolCtx` in `conway-core`, not smuggled in here.

`common.rs` holds the shared helper layer (argument parsing, path
resolution, output construction, cooperative cancellation) every tool in
this crate builds on, and `testing.rs` (behind `feature = "test-fakes"` or
`cfg(test)`) provides in-crate test doubles — `FakeSubagentHost`,
`RecordingEventSink`, `test_ctx` — that let every tool be unit-tested with
zero runtime.

## The public `Plugin`/`Tool` API third parties implement

A third-party plugin author implements exactly the two traits defined in
`conway-core` (see [`conway-core`](conway-core.md) for the full
signatures):

```rust
pub trait Plugin: Send + Sync + 'static {
    fn manifest(&self) -> PluginManifest;   // id, semver, tools, required_host_caps
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
}

#[async_trait]
pub trait Tool: Send + Sync + 'static {
    fn spec(&self) -> ToolSpec;             // name, description, JSON schema, category, permission
    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError>;
}
```

`ToolCtx` is the entirety of what a tool can reach: `agent_id`,
`session_id`, `cwd`, a `CancellationToken`, an `EventSinkHandle` for
progress reporting, `Arc<dyn SubagentHost>` (the same trait object the
developer API's `SessionHandle::fork`/`spawn` calls — a plugin has no more
delegation power than the public API), and the plugin's own `PluginConfig`.
A plugin transported in-process (the current, MVP shape — a first-party
crate or a consumer's own `Arc<dyn Plugin>`) never sees more than this.

### The error discipline every built-in tool follows

Applied crate-wide, and a good template for a third-party tool: distinguish
**model-recoverable** conditions from **host/infrastructure** conditions.

- **Model-recoverable** — file not found, no regex match, a non-zero shell
  exit code, an ambiguous edit — return `Ok(ToolOutput { is_error: true,
  .. })`. The model sees the failure as a normal tool result and can adapt
  (try a different path, a narrower pattern, and so on).
- **Host/infrastructure** — cancellation, permission denied at the OS
  level, a spawn failure, an unreachable `SubagentHost` — return
  `Err(ToolError::..)`. These are conditions the model has no useful way
  to react to; they surface as a harness-level error instead of a tool
  result.

`ToolOutput` also carries a `TruncationPolicy` the tool declares (the
runtime enforces it and records the truncation in the log — see
[`conway-runtime`](conway-runtime.md)) and any `Artifact`s the call
produced.

## The `PermissionGate` model: announcement vs. execution

Tool **announcement** — what the model is told exists and may call — and
tool **execution** — what is actually allowed to run — are two distinct
concerns in conway, not one:

- **Announcement** is controlled by whatever assembles the tool set offered
  to the model for a turn: ordinarily every registered tool, but
  narrowable per-turn by a [`ContextHook`](conway-core.md) (`conway-core`,
  0.2.0) editing `ContextPayload::tools`, or per-agent-definition by
  `AgentDef::tools`/`SubagentSpec::tools` (a `ToolSelector`). A tool that
  is never announced is simply not offered to the model this turn — not
  silently denied.
- **Execution** is gated by `PermissionGate` (`conway-core::ports::
  permission`), a trait every consumer (CLI, IDE, embedder) implements —
  `conway-tools` and `conway-core` ship no privileged bypass:

  ```rust
  #[async_trait]
  pub trait PermissionGate: Send + Sync + 'static {
      async fn check(&self, req: PermissionRequest) -> PermissionDecision;
  }
  ```

  Every proposed tool call — announced or not, built-in or third-party —
  passes through the gate before `Tool::invoke` runs. `PermissionDecision`
  has four cases: `AllowOnce`, `AllowAlways { scope: PermissionScope }`
  (`Session | Agent | AgentSubtree`), `Deny { reason }`, and
  `DenyWithFeedback { message }` — the last of which lets the *model* see
  and adapt to the denial reason (e.g. "you may not write outside
  `/workspace`") rather than the call simply failing silently the way a
  bare `Deny` would appear to the model. The gate may block indefinitely
  while waiting on a human decision; the runtime emits
  `Event::PermissionRequested` while it does, and gate cancellation
  (e.g. the host process shutting down) surfaces as `Deny { reason:
  "cancelled" }`, never as a hang.

  A tool that was narrowed out of announcement never reaches
  `PermissionGate` at all this turn — it wasn't offered — but that is
  strictly a UX/context-budget concern, not a permission bypass: the gate
  still governs every call the model *does* propose regardless of what was
  announced.

**For embedders: narrowing is not a capability boundary.** A `ToolSelector`
(and likewise a `ContextHook` that edits `ContextPayload::tools`) narrows
*announcement* only. Tool dispatch resolves a proposed call by name against
the whole `PluginRegistry` the agent was built with, so a tool that is
registered but not selected stays fully reachable if the model happens to
name it exactly — an unannounced-but-registered name is a real call, not an
"unknown tool" error. If you need a hard guarantee ("this agent can never
touch the filesystem"), get it one of two ways: don't register those tools
into that agent's plugin set at all (simplest and unconditional), or run a
deny-by-default `PermissionGate` that allows only what you intend. Do not
rely on the selector for it.

`ToolSpec::permission: PermissionClass` (`Safe | RequiresApproval |
Dangerous`, `conway-core`) is a static, coarse-grained hint a gate
implementation typically uses to decide its default posture (e.g. an
allow-list gate auto-allows `Safe` tools and always prompts for
`Dangerous` ones) — it is advisory context for the gate, not a bypass of
it. See [`conway`](conway.md) for the concrete gate implementations
conway ships (an allow-list gate used by `-p`, a deny-all gate, and the
TUI's interactive prompting gate).

## The built-in plugins

### `FsPlugin` — `cd`, `read`, `write`, `edit`, `glob`, `grep`

| tool | category | permission | behavior |
|---|---|---|---|
| `cd` | `Move` | `Safe` | Changes the working directory subsequent tool calls resolve relative paths against. |
| `read` | `Read` | `Safe` | `cat -n`-style file reading with binary sniffing and offset/limit windowing. |
| `write` | `Edit` | `RequiresApproval` | Atomic whole-file replacement via a sibling temp file plus rename. |
| `edit` | `Edit` | `RequiresApproval` | Literal, byte-exact substring replacement (no regex, no fuzzy match — ambiguous matches are a model-recoverable error). |
| `glob` | `Search` | `Safe` | Gitignore-aware glob pattern matching over a directory tree, results ordered mtime-descending. |
| `grep` | `Search` | `Safe` | Regex content search over a directory tree, gitignore-aware. |

The five file tools respect `.gitignore` where applicable and operate
relative to `ToolCtx::cwd` — there is no sandbox/worktree logic anywhere in
this crate (a general conway design principle). Path confinement, when an
agent has a confinement root, is enforced once by `conway-runtime`'s
`PermissionBroker` against every tool's declared `Tool::path_args` — not by
this plugin, and not by the `PermissionGate` implementation either (a gate
still governs execution as always, but the root check runs *before* any
gate is consulted; see [`conway-runtime`](conway-runtime.md)'s "Permission
brokering" section for the full mechanism and its limits).

**`cd`** is the exception worth its own paragraph, because its effect is
not immediate the way every other tool's is. It resolves its one `path`
argument the same way every other file tool does (relative joins onto
`ToolCtx::cwd`, absolute as-is), verifies the target exists and is a
directory (a nonexistent path or a file target is a model-recoverable
error, cwd left unchanged — `Tool::invoke` never calls `CwdHandle::set`
without that check passing first), and then calls
`ToolCtx::chdir.set(path)`. Because `ToolRunner::run_batch` snapshots the
`chdir` cell into `ToolCtx::cwd` exactly once, before dispatching any call
in a batch (`conway-runtime`, S1), **a `cd` takes effect starting the next
batch of tool calls, never the one it was issued in** — a `cd` alongside a
`read` in the same batch does not redirect that `read`. For a one-off
("run this one command somewhere else, then come back") use the per-call
`cwd` argument `bash`/`glob`/`grep` already accept instead — the `(cd X &&
cmd)` subshell idiom, which applies immediately because it's a fresh
`ToolCtx` field read, not a persistent move. `cd` also never changes where
a session started (`SessionMeta::cwd`): a resumed session always returns to
its original spawn directory. Deliberately out of scope: `cd -`,
`pushd`/`popd`, a directory stack, `PATH`-style search, and a `pwd` tool —
shell affordances the model doesn't need (`ToolCtx::cwd` is already handed
to every tool, and the current directory is already visible in the TUI
status line).

`cd` itself performs no containment check, and deliberately so — but that
does **not** mean its target is unchecked. `CdTool` declares
`PathArgs::Named(&["path"])`, and `conway-runtime`'s permission broker
checks every declared path argument of every tool against the agent's
confinement root before any allow path is consulted. So a `cd` to a path
outside a confined agent's root is **denied by the broker**, exactly like a
`read` or a `write` would be, without `cd` containing a line of
root-specific code. That is the design working as intended: confinement is
enforced at the one chokepoint every tool call passes through, not
re-implemented per tool (GP-08 — the harness's responsibility ends at the
permission model).

Two things follow. A `cd` inside the root is still just a `cd` — cwd is not
the security boundary, so moving around within the root is unremarkable. And
an unconfined agent (no root) is unaffected: the broker's check is a no-op
there, and `cd` behaves exactly as it did before confinement existed.

### `ShellPlugin` — `bash`

One tool, `bash` (category `Execute`, permission `Dangerous`): streamed,
cancellable, process-group-killing command execution. Output streams
incrementally via `ToolCtx::events`; cancelling the tool's
`CancellationToken` kills the whole process group, not just the immediate
child, so a shell pipeline or backgrounded subprocess can't outlive
cancellation.

`BashTool` overrides `Tool::render` to return the bare `command` string
(falling back to the trait's generic `bash(args)` rendering if `command` is
missing or not a string — untrusted, model-supplied arguments must never
panic a render) rather than accepting the trait's default `name(args)`
one-liner. This is load-bearing, not cosmetic: `conway-runtime`'s
`PermissionRequest::rendered` (permission prompt text, the
`PermissionRequested` event, and `conway_core::permission_pattern::
PatternRule` prefix matching) is built from whatever `Tool::render`
returns, and a rule like `bash:git status` is only checkable-by-reading —
the entire reason V2 pattern grants use prefixes over regex — when the
rendered text IS the command a person would type, not a JSON dump of the
call's arguments.

**What confinement gives you here, stated plainly, not reassuringly.**
`BashTool` declares `Tool::path_args() -> PathArgs::Unconfinable { checkable:
&["cwd"] }`: its `cwd` argument is resolved and checked against a
confinement root exactly like any other tool's path argument, but its
`command` string is **declared unconfinable, not enforced** — it is handed
to `/bin/bash -c` verbatim, and the broker cannot parse shell. `cd ..`,
`$HOME/x`, `$(echo /etc)/passwd`, `exec 3</etc/passwd`, a shell function, and
a heredoc all reach paths a root-check-on-the-string could never rule out;
extracting paths from the command text and concluding "none outside the
root, therefore allow" would be the same shape of bug as the metacharacter
gate fixed in 0.5.0 — a transformation of untrusted input whose *failure to
find* something becomes an authorization. So it is never attempted: a
root never auto-allows `bash`'s command, and **an agent holding `bash` is
not confined by root alone.** See
[`conway-runtime`](conway-runtime.md)'s "Permission brokering" section for
what root does and does not guarantee, and the composition (root plus a
tool set that excludes `bash`) that actually is a jail.

### `SubagentPlugin` — `conway_subagent`, `conway_ask`, `conway_steer`, `conway_await`, `conway_cancel`

A pure, zero-delegation-logic wrapper over `ToolCtx::subagents`: every tool
here does argument parsing, exactly one `SubagentHost` call — the same
port the developer API's `SessionHandle::fork`/`spawn` calls — and result
shaping. This is the mechanical proof that the subagent tool has no
privileged access the public API lacks.

- **`conway_subagent`** (category `Delegate`, permission `Dangerous` —
  starting a child grants it the transitive capability to make arbitrary
  tool calls itself, one hop removed, the same risk class as `bash`).
  Takes a `mode` (`fork` or `spawn`), a `prompt`, an optional `agent_def`
  (naming one for `spawn` gives the child a clean-slate system prompt and
  tool set from that def; omitting it means the child inherits the caller's
  own role/model instead — the earlier "`agent_def` required for spawn" rule
  is relaxed), optional `role`/`tools`/`budget`/`result_contract`, and an
  `await` flag. **`await: false` is the fan-out primitive**: the call
  returns `{ "agent_id": ... }` immediately without blocking, so a parent
  can start several children back-to-back and later `conway_await` each
  one — this is what makes an N-way tournament/fan-out pattern practical
  rather than serial.
- **`conway_ask`** (category `Delegate`, permission `Dangerous` — like
  `conway_subagent`, the fork child inherits at most the caller's requested
  tool set, so arbitrary tool calls are one hop away). **Fork-only**: takes a
  `prompt`, an optional `budget`, and an optional `tools` list (no
  `mode`/`agent_def`/`role` — the fork already inherits the caller's context,
  agent_def, and role; GP-02). `tools` narrows the ephemeral child's tool set
  to the named tools (`ToolSelector::Only`, the same selector
  `conway_subagent`'s `tools` arg produces) — e.g.
  `{"prompt": "summarize the diff", "tools": ["read"]}` restricts the child
  to read-only inspection. The arg is narrowing-only: it can restrict, never
  widen, the tool set the child would otherwise inherit. Runs
  the prompt in an **ephemeral fork** of the caller and returns the child's
  **full** concatenated reply text (not the truncated `AgentResult::summary`;
  GP-01), plus an `EphemeralSessionRef` artifact naming the child's session for
  provenance (P-2). The child is marked `ephemeral`, so it never appears in the
  TUI `/agents` panel or default session listings, while staying attached to
  the live `AgentTree`. The intended composition (P-1: `ask` is fork+await-text,
  not a third primitive) is `conway_ask` → `conway_subagent { mode: spawn,
  prompt: <the returned brief> }`: the model drafts/curates context for a fresh
  spawn out-of-band, then spawns with it, keeping the curation reasoning out of
  the orchestrator's context window.
- **`conway_steer`** (category `Delegate`, permission `RequiresApproval`)
  — sends a text message to a running child; delivered at the child's next
  turn boundary (see [`conway-runtime`](conway-runtime.md)'s mailbox).
- **`conway_await`** — blocks on `SubagentHost::await_result` for a
  previously started child; always terminates (the supervisor synthesizes
  a result on budget exhaustion, cancellation, or panic), so a parent's
  pending call can never hang indefinitely.
- **`conway_cancel`** — cancels a running child by id, with a reason.

  All three take a model-supplied `agent_id` naming the target, but that id
  is not, by itself, authorization to act on it: `control.rs` always passes
  `ctx.agent_id` — the runtime-assigned identity of the agent actually
  dispatching the call — as the `caller` half of `SubagentHost::steer`/
  `await_result`/`cancel`'s signature, and `conway-runtime`'s
  `impl SubagentHost for Runtime` rejects with `RuntimeError::
  AgentNotInSubtree` unless `agent_id` names `ctx.agent_id` itself or one of
  its descendants — see that trait's own doc and
  [`conway-runtime`](conway-runtime.md)'s "descendancy-checked at this trait
  boundary" note for why this cannot be bypassed by any other caller.

### `ReportPlugin` — `report`

One tool, `report` (category `Think`, permission `Safe`): explicit
terminal-result declaration. It emits a canonical, versioned JSON envelope
and does **no delegation, no session logging, and no result finalization
itself** — deliberately, per the crate boundary rule: lifting the
envelope's payload into the agent's terminal `AgentResult` is
`conway-runtime`'s job (the runtime recognizes the `report` tool by name),
because `conway-tools` must not depend on `conway-runtime` or
`conway-session`. See [`conway-runtime`](conway-runtime.md) for the lift.

## How it fits the whole

`conway-tools` depends only on [`conway-core`](conway-core.md).
[`conway-runtime`](conway-runtime.md) hosts the tool registry
(`tools::registry`) that resolves an announced `Vec<ToolSpec>` against
`Plugin`/`Tool` implementations, runs `PermissionGate::check` before
`Tool::invoke`, and implements the `SubagentHost` port every subagent tool
call reaches. The [`conway`](conway.md) facade is where `builtin_plugins()`
is registered by default and where concrete `PermissionGate`
implementations (allow-list, deny-all, interactive) live. See
[`/ARCHITECTURE.md §3.4`](/ARCHITECTURE.md) for the system-level picture of
tools-as-plugins behind a permission gate.
