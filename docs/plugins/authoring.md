# Writing your first hook

This is the onboarding half of the set: `concepts.md` says what a hook is,
`hooks.md` says exactly what each point promises, `trust-and-security.md` and
`compatibility.md` say what you're accountable for and what conway won't
break under you. This page says how to actually build something, today,
against the tree as it stands — not the tree the design corpus describes.

**Read `concepts.md` first**, specifically "Hook-first," "Observers vs
participants," and "The value-class boundary" — this page assumes that
vocabulary and does not re-explain it. **Then read
[`trust-and-security.md`](trust-and-security.md)** before you wire anything
in for real: a hook and a plugin both run with *your own* operator
privileges the moment they're registered — there is no sandbox between a
configured hook command (or an in-process `Tool`/`ContextHook`) and
everything you yourself can touch. This page shows you how to build
something that works; it does not repeat what that thing is allowed to do
to you.

## The one thing to get straight before you start

**This section used to say the declarative surface was "decided, not
built." That was true when this page was first written and is false now —
executed, not just read, as this set's own standing rule requires.**
`hooks.rules[]` in `settings.json` dispatches all seven core events
(`hooks.md` point 13's Status row: "All SEVEN core events are real; nothing
is design-only here anymore"), and a rule's `match` field narrows a `pre_tool_use`/`post_tool_use` rule
to one tool name or a `*`-glob, exactly as `PHILOSOPHY.md` §5 spells it.
There are genuinely **two** ways to build a working hook today, not one:

1. **A declarative hook** — a `hooks.rules[]` entry naming an `event`, an
   optional tool `match`, and a `command` (an argv `Vec<String>`) to run. No
   Rust, no compiling your own crate — the shipped `conway` binary already
   does the dispatching. This is what "Ten minutes to a working hook" below
   teaches, because it genuinely is the fastest path now — it wasn't when
   this page last said otherwise.
2. **An in-process Rust hook** — implement `Tool`, `Plugin`, `ContextHook`
   and/or `ToolObserver` against the curated `conway::plugin` facade, and
   wire it in through `ConwayBuilder`. This is what "Going further: an
   in-process Rust hook" below teaches. It is still the *only* way to
   **edit** what the model sees — a declarative hook's
   `HookPermissionVerdict` can only `deny` (and only for
   `pre_tool_use`/`prompt_submitted`); it structurally cannot rewrite a
   segment the way `ContextHook::before_request` can (`concepts.md`'s
   value-class boundary table, "Context: Edit, drop, replace, mask" — no
   declarative counterpart exists for that row).

   It is also the only way to **add to the record**. `Plugin::observers`
   supplies a `ToolObserver`, called once per finished tool call with the
   call's arguments, which returns notes for the runtime to append to the
   session log. A declarative `post_tool_use` hook sees a summary payload
   without arguments and its output is discarded, so it can log elsewhere
   but cannot say anything the model will read.
   `crates/conway-plugin-stepguard` is the worked example.

What's still genuinely true from the old framing, so you don't overcorrect:
there is still no `hooks()` method on the `Plugin` trait itself — a `Plugin`
you write contributes `tools()`, `commands()`, `events()`, and
`observers`, never a `hooks`-consulted script of its own — and there is
still no
script-dispatching *plugin* (`concepts.md`'s "Language choice"); the
dispatching in mechanism 1 above is the runtime's own, built-in
`ProcessHookRunner`, not something a third-party `Plugin` implementation
provides. F8 is still
what makes mechanism 2 real, and `crates/conway/tests/plugin_surface.rs` is
still a complete, compile-guarded worked example of it — every snippet in
that section is lifted from it or from
`crates/conway/examples/minimal_session.rs`, not invented for this page.

## Ten minutes to a working hook

The fastest way to see a hook do something visible: no crate, no compiling
your own code, just three lines of JSON and a script. Write the rule
(narrowed to one tool with `match`, so it isn't the "fires on every call"
hook nobody wants), then watch it fire in a real session against a real
model.

### 1. Get a `conway` binary that dispatches hooks

**A declarative rule is only ever consulted if the binary running it has a
`HookRunner` injected** — `hooks.md` point 13 states this precisely: parsing
and validating a `hooks.rules[]` entry happens unconditionally, but
*dispatching* it does not. The shipped CLI does this for you unconditionally
(`conway-cli::build_conway` calls `ConwayBuilder::with_default_hook_runner`) — if you're working inside a
checkout of this repository and don't already have a `conway` binary:

```console
cargo build -p conway-cli --release
```

A facade-only embedder (writing their *own* thin binary rather than using
the shipped CLI) gets nothing dispatched unless they call
`ConwayBuilder::with_hook_runner`/`with_default_hook_runner` themselves — see
"Writing a Rust plugin" below for what a from-scratch binary already has to
set up, and note that a `hooks` block in that binary's config is otherwise
silently inert, not an error.

### 2. Write the rule, narrowed with `match`

A rule that fires on every tool call is not the first hook anyone wants —
`match` is what narrows one. This
rule logs one line every time (and only when) `bash` runs, in
`~/.conway/settings.json` (or `$CONWAY_CONFIG_DIR/settings.json` if
that's set — see "Where things live" below for the exact discovery order):

```json
{
  "default_role": "coder",
  "backends": { "local": { "kind": "openai-compat", "dialect": "ollama", "base_url": "http://localhost:11434/v1" } },
  "roles": { "coder": { "chain": ["local/qwen3:4b"] } },
  "hooks": {
    "rules": [
      {
        "id": "log-bash-calls",
        "event": "post_tool_use",
        "match": "bash",
        "command": ["/bin/sh", "-c", "echo bash-hook-fired >> /path/to/hook.log"],
        "timeout_ms": 3000
      }
    ]
  }
}
```

`match` is spelled `"match"` on the wire (exact tool name, or a `*`-glob
against the tool's whole name — `"fs.*"` matches every `fs.*` tool) even
though the Rust field behind it is `match_tool` (`match` is a reserved
word). It only applies to `pre_tool_use`/`post_tool_use`, the two events
that carry a tool name at all; setting it on any other event (e.g.
`session_starting`) is a load-time config error naming this rule's `id`, not
a silently-inert rule — narrower feedback than a misspelled tool name gets,
which fires never, quietly, and is the one mistake with no safety net (check
the tool's *registered* name, not the name you remember). Omitting `match`
entirely fires the rule for every occurrence of `event`, unchanged from
before this field existed — loud, and therefore self-correcting, but not
what you want for a first hook.

`post_tool_use` here is an **observation** event: it cannot deny anything,
only react after the fact (`hooks.md` point 13's Status row). A
`pre_tool_use` rule is the participant shape — its `HookAnswer.permission`
can `deny { reason }` the call outright, which is the shape
`crates/conway-cli/tests/hook_runner_wiring.rs`'s own end-to-end test
exercises. Both share the exact same `match` narrowing.

### 3. See it fire — and confirm the narrowing is real, not assumed

Run a real turn against a real backend that can call `bash` (a local Ollama
model, in this walkthrough's own evidence — no cloud credentials needed;
see the evidence transcript for the full commands), and watch the log file:

```console
$ conway -p "List the files in this directory using the bash tool (run: ls)." --allowed-tools bash
...
The files in the current directory are:
- ...
$ cat hook.log
bash-hook-fired
```

Bash is opt-in, not on by default (`docs/embedding.md`'s "Built-in plugin
selection" section) — add `"tools": {"builtin_plugins": ["conway.fs",
"conway.subagent", "conway.report", "conway.shell"]}` to `settings.json` if
`bash` isn't already in your `builtin_plugins` list, or the model has
nothing to call. `--allowed-tools bash` grants the one-shot `-p` run's own
default-deny allow-list (`docs/scripting.md`'s permission-mode table)
permission to actually run it — the hook fires downstream of that decision,
never in place of it.

**Prove the narrowing, don't just trust the field name exists.** Change
`match` to `"fs.write"` (a tool this prompt never calls) and rerun the exact
same prompt: `bash` still runs (the transcript still lists the files), but
`hook.log` stays empty — the rule parsed, validated, and was consulted, and
decided not to act, which is the *whole point* of a matcher and the exact
failure mode (`hooks.md`, the extension design) that's
otherwise indistinguishable from "the hook isn't wired at all."

## Where things live

**Correcting this page's own earlier claim: there is now a config file to
point you at.** `.conway/settings.json`'s `hooks.rules[]`, discovered by
the same walk-up-from-cwd, project-then-global precedence
`docs/getting-started.md`'s "Configure a provider" section documents for
everything else in that file (`.conway/settings.json` found by walking up
from your current directory takes precedence over `~/.conway/settings.json`
— or `$CONWAY_CONFIG_DIR/settings.json` if that variable is set,
*instead of* `~/.conway/settings.json`, not in addition to it). What's still
true from before: **there is no runtime inventory surface.** `hooks.md`
names this directly: other harnesses' documented failure mode is "a rule set
nobody can inspect," and conway's own `/plugins` display — a place to list
what hooks are active and whether each is healthy — is itself designed, not
built (`hooks.md` point 12, the extension design). Today,
"what's active" is answered by reading `settings.json`'s own `hooks.rules[]`
list directly (for a declarative hook) or your own `ConwayBuilder` call — the
`.with_plugin(..)`/`.with_context_hook(..)`/`.with_permission_gate(..)`
chain *is* the inventory for an in-process one, because it's the only place
registration happens.

## Going further: an in-process Rust hook

Everything above covers the declarative surface, which can observe and
(for `pre_tool_use`/`prompt_submitted`) deny, but never rewrite what the
model sees. When you need to **edit** context — narrow a tool's output,
fold old turns, redact something — you need the other real mechanism: an
in-process `ContextHook`, implemented against the curated `conway::plugin`
facade and wired in through `ConwayBuilder`. This is what "Writing your
first hook" originally meant before the declarative surface existed, and it
is still the only path with edit/drop/replace authority
(`concepts.md`'s value-class boundary table). It has no `match` field —
narrowing which segments it acts on is your hook's own logic, inspecting
`ContextPayload` yourself (see the cookbook's spill-to-file and compaction
examples for worked cases of exactly that).

### 1. Depend on `conway`

conway isn't published to a registry yet (`docs/embedding.md`'s own note); if
you're working inside a checkout of this repository, add a path dependency
from your own crate:

```toml
# Cargo.toml
[dependencies]
conway = { path = "../conway" }               # adjust to your layout
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

`async-trait`, `serde`, and `serde_json` are not re-exported by the facade on
purpose — they're plain data-type crates you name yourself, at the version
conway uses, so the types line up (`docs/embedding.md`'s "Writing a plugin"
section explains why).

### 2. Write the smallest hook that does something visible

A `ContextHook` that appends a marker segment to every outgoing request is
about as small as "visible" gets — you can see the segment in the assembled
payload, and later, in the transcript. This is `plugin_surface.rs`'s
`MarkerHook`, trimmed to just the append half:

```rust
use conway::plugin::{
    async_trait, ContextHook, ContextHookCtx, ContextPayload, OverflowInfo,
    PromptSegment, Provenance, Role, ContentBlock,
};

struct MyFirstHook;

#[async_trait]
impl ContextHook for MyFirstHook {
    async fn before_request(
        &self,
        _ctx: &ContextHookCtx,
        mut payload: ContextPayload) -> ContextPayload {
        payload.segments.push(PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: "my-first-hook was here".to_string(),
            }],
            Provenance::SystemNote {
                reason: "getting-started demo".to_string(),
            }));
        payload
    }

    // The trait supplies a default `on_overflow` that returns `None`
    // (give up) -- fine for a demo hook that never sees an overflow.
    async fn on_overflow(
        &self,
        _ctx: &ContextHookCtx,
        _payload: ContextPayload,
        _overflow: OverflowInfo) -> Option<ContextPayload> {
        None
    }
}
```

Every type named above (`ContextHook`, `ContextHookCtx`, `ContextPayload`,
`OverflowInfo`, `PromptSegment`, `Provenance`, `Role`, `ContentBlock`) comes
from `conway::plugin` alone — no `conway_core` import anywhere in this file,
which is the property `plugin_surface.rs` itself is a standing compile guard
for (delete one re-export from `conway::lib::pub mod plugin` and this file
stops compiling).

### 3. Confirm it fired — without a session, a backend, or a network call

The fastest possible proof that your hook does what you think: call
`before_request` directly and assert on the result. No `Conway`, no
`ConwayBuilder`, no credentials, sub-second:

```rust
use conway::plugin::{ArtifactWriteHandle, ContextHookCtx, ContextPayload};

// `ContextHookCtx` requires an `artifacts` handle even if your hook never
// writes a file. `ArtifactWriteHandle::noop` is the facade's own no-op writer, for exactly
// this case -- no `ArtifactWriter` impl to write yourself. See "Artifacts"
// below for wiring a hook that writes for real.
#[tokio::test]
async fn my_first_hook_appends_its_marker() {
    let hook = MyFirstHook;
    let agent_id = conway::AgentId::new();
    let ctx = ContextHookCtx {
        agent_id,
        // Root-first, self-inclusive -- a root agent's own path is just
        // itself.
        agent_path: vec![agent_id],
        session_id: conway::SessionId::new(),
        turn: 0,
        model: None,
        estimated_tokens: 0,
        artifacts: ArtifactWriteHandle::noop(agent_id),
        // An embedder's own correlation identifier -- `None` unless the
        // spec that created this agent set `SubagentSpec::tag`.
        tag: None,
    };
    let payload = ContextPayload { segments: vec![], tools: vec![] };

    let out = hook.before_request(&ctx, payload).await;

    assert_eq!(out.segments.len(), 1);
    assert!(matches!(out.segments[0].provenance, Provenance::SystemNote { .. }));
}
```

You will also need `tokio` with the `macros` and `rt-multi-thread` features
as a dev-dependency for `#[tokio::test]`.

This is exactly the shape of `plugin_surface.rs`'s own
`authored_hook_transforms_payloads` test. Treat it as a floor, not a ceiling —
see "Testing your hook" below for why a test at this level alone is not
enough to claim the hook is *live*.

### 4. See it fire inside a real session

A direct call proves your logic; it doesn't prove the wiring. Register the
hook through `ConwayBuilder` and drive one real turn — no API key needed,
because you point the builder at a `FakeBackend` that echoes the prompt back,
the same no-network, no-credentials shape
`crates/conway/examples/minimal_session.rs` already demonstrates end to end:

```console
cargo run -p conway --example minimal_session
```

```text
prompt -> Hello, conway!
ask    -> (ephemeral) just checking something
main-session log head: LogSeq(4) before the ask, LogSeq(4) after -> the ephemeral ask left no trace in the main session
```

To see *your* hook in that same run, add one line to that example's builder
chain:

```rust
let conway = ConwayBuilder::from_parts(minimal_config())
    .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
    .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
    .with_session_store(store.clone())
    .with_router(Arc::new(FakeRouter::single(ModelRef { .. })))
    .with_context_hook(Arc::new(MyFirstHook))   // <- the one line this adds
    .build()?;
```

The confinement root, backend routing, and event pipeline this now runs
through are the real ones — only the backend and router are fakes standing in
for a network call. This is the honest meaning of "see it in the UI" for a
facade-only author today: there is still no `/plugins` inventory panel for
*either* mechanism (`hooks.md` point 12, unchanged) — a `ContextHook`
specifically has no TUI surface of its own at all, unlike a `Plugin`'s
`commands()`, which does register a real `/{plugin_id}.{name}` the TUI can
run (see "Writing a Rust plugin" below) — so "the UI" for a `ContextHook` is
whatever your own binary prints from the session — the transcript
(`turn.text().await`), or the raw event stream (`docs/embedding.md`'s
"Consuming the event stream" section), or the session log itself. If you
build the actual `conway` CLI/TUI experience with your hook wired in,
you're building your own thin binary around `ConwayBuilder` the same way
`conway-cli` does — there is no way to load an in-process Rust plugin into
the shipped `conway` binary without recompiling it, because a plugin is
`Arc<dyn Plugin>`, linked into the binary before the process starts
(`concepts.md`'s "What exists today" list).

**Steps 1-3 of this section were executed verbatim** against a scratch
crate outside the workspace whose only conway dependency is `conway`
itself, and the test passes. Running it is what caught the missing
`artifacts` handle above: the snippet previously left that field as a
comment, which is not valid Rust, so the page did not compile as written.

## Writing a Rust plugin

**Stated plainly, because it is the single most important correction on
this page: installing a plugin requires building a binary, today, always.**
There is no runtime plugin host — every `Plugin` is `Arc<dyn Plugin>`,
linked into a specific binary in Rust before the process starts
(`concepts.md`'s "What exists today" list). `plugins.install` in
`settings.json` does not reach out and fetch a plugin by name from
anywhere; it *selects*, by id, among whatever plugins that specific binary's
own `main()` already constructed and handed to
`ConwayBuilder::install_selected` (or `with_plugin`) — a closed set fixed at
compile time, never open at runtime. An id `plugins.install` names that no
linked bundle recognizes is a hard, named config error (see below), not "not
found, so nothing happens." The honest framing for this beta is "add your
crate to a binary and build" — never "drop a file somewhere and conway
picks it up." A runtime, out-of-process plugin host is real design work with
its own item, not this page's scope:.

`conway::plugin` re-exports everything you need to implement `Tool`,
`Plugin`, and `ContextHook`: the three traits, their method-argument and
return types, and the field types of the structs you construct.
`docs/embedding.md`'s "Writing a plugin" section is the canonical list of
what's exported and why each name is there — this page doesn't duplicate
that list, only the shape of using it.

**Two new things on `Plugin` since this page was last written, worth
knowing even if this walkthrough doesn't exercise them itself:**
`Plugin::commands()` lets a plugin contribute a `/command` to the TUI,
namespaced `/{plugin_id}.{name}` (never the author's own choice of
namespace — the host prefixes it, closing the collision risk two plugins
picking the same bare name would otherwise have), and `Plugin::events()`
lets a plugin declare and fire its **own** hook event, reachable in a
`hooks.rules[].event` as `"{plugin_id}.{event_name}"` and narrowable by
`match` the identical way a core event is, gated on the plugin's own
declaration of whether its payload even carries a tool name to match
against. Both default to an empty `Vec` — every existing `Plugin`
implementor, including every snippet on this page, keeps compiling
unmodified. `crates/conway-plugin-skeleton` is the shipped worked example of
both, proven end to end rather than merely declared:
`SkeletonPlugin::commands()`/`events()` register a real `/ping` command and
a real `pong_dispatched` event, and `conway-plugin-skeleton/tests/
skeleton_end_to_end.rs` drives a real configured `hooks.rules[]` entry
that actually receives the fired event.

**A worked, executed example: your own thin binary, exactly like
`conway-cli`'s own shape**, using `ConwayBuilder::install_selected`
(`docs/embedding.md`'s "First-party plugin tier" section teaches this same
call; this page had not been updated to use it before this walkthrough) —
hand it every plugin, router, and backend factory *your* binary links, and
it resolves `plugins.install` against exactly those, calling `with_plugin`
for each id it recognizes and raising a named config error for one it
doesn't:

```rust,ignore
use std::sync::Arc;

use conway::plugin::{
    async_trait, ContentBlock, PathArgs, PermissionClass, Plugin, PluginManifest, RenderKind,
    Tool, ToolCall, ToolCategory, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec,
    TruncationPolicy,
};
use conway::{BackendFactory, ConwayBuilder, SessionSpec};
use conway_plugin_backends::OpenAiCompatBackendFactory;

struct GreetTool;

#[async_trait]
impl Tool for GreetTool {
    fn spec(&self) -> ToolSpec { /* ... */ }
    async fn invoke(&self, call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        // ... (an ordinary Tool, nothing new here -- hooks.md point 2)
    }
    fn path_args(&self) -> PathArgs { PathArgs::None }
    fn render_kind(&self) -> RenderKind { RenderKind::Structured }
}

#[derive(Default)]
struct MyFirstPlugin;

impl Plugin for MyFirstPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "my.first_plugin".to_string(),
            version: "0.1.0".to_string(),
            tools: vec![ToolName::new("greet")],
            required_host_caps: vec![],
        }
    }
    fn tools(&self) -> Vec<Arc<dyn Tool>> { vec![Arc::new(GreetTool)] }
}

#[tokio::main]
async fn main() -> conway::Result<()> {
    let conway = ConwayBuilder::discover()?
        .install_selected(
            vec![Arc::new(MyFirstPlugin) as Arc<dyn Plugin>],
            vec![], // no RouterFactory this binary links
            vec![Arc::new(OpenAiCompatBackendFactory) as Arc<dyn BackendFactory>])?
        .build()?;
    let session = conway.new_session(SessionSpec::default()).await?;
    let turn = session.prompt("Use the greet tool to greet someone named Ada.").await?;
    println!("model said -> {}", turn.text().await?);
    Ok(())
}
```

with `settings.json`'s `plugins.install: ["my.first_plugin"]` naming it.

**Optional: describe your plugin for the operator, not only the model.**
`Plugin::description()` (`hooks.md` point 19) is a separate, zero-cost-
default trait method a plugin browser (`conway`'s own `/settings` plugins
section) reads to show a summary and a you-get/you-lose/costs panel before
someone turns your plugin on:

```rust,ignore
impl Plugin for MyFirstPlugin {
    // ... manifest()/tools() unchanged from above ...
    fn description(&self) -> conway::plugin::PluginDescription {
        conway::plugin::PluginDescription {
            summary: "greets someone by name".to_string(),
            you_get: "1 tool (greet)".to_string(),
            you_lose: "nothing else".to_string(),
            costs: "none".to_string(),
        }
    }
}
```

Leaving it unimplemented is fine -- the browser renders an honest
"(no description)" rather than a blank row.

**Executed against the real thing, not sketched:** the exact shape above (the
`spec()`/`invoke()` bodies are elided here for length only — the scratch
crate's own `GreetTool` is a complete, ordinary `Tool` impl, nothing hidden)
compiled and ran as its own scratch binary outside the workspace, driven
against a real local Ollama backend, and the model called `greet` —
`AgentResult.summary` came back `"hello, Ada, from my-first-plugin!"`, this
walkthrough's own tool text, reachable only if the call actually went
through this plugin's own `Tool::invoke`. See the evidence transcript
 for the full commands, the
full unelided source, and the output.

**A gotcha this walkthrough hit, and `docs/embedding.md`'s own example
doesn't warn you about because its `install_selected` snippet is
illustrative only (`rust,ignore`, empty bundles):** `install_selected`
resolves `plugins.install` **unioned with** `plugins.default_backends`
(default `["anthropic", "openai-compat"]`) against your three bundles —
every id in that union must resolve to *something* you linked, or `build()`
fails naming the unresolved id. A binary that links only
`OpenAiCompatBackendFactory` (as above) and never touches
`plugins.default_backends` fails with `plugins.install names unknown id
'anthropic'` — not because anything about your plugin is wrong, but because
the default backend-kind list still names a dialect this binary never
linked. Either link `conway_plugin_backends::AnthropicBackendFactory` too,
or narrow `"plugins": {"default_backends": ["openai-compat"]}` in
`settings.json` to only the kind(s) your binary actually has. This was
caught by running the snippet above, not by reading `install_selected`'s
own doc comment, which states the union rule correctly but doesn't call out
this specific failure mode.

**The known boundary, stated without smoothing it.** The facade parity gap opened naming four types a plugin
author couldn't construct or match through `conway::` paths:
`SubagentSpec`, `RuntimeError`, `Fact`, and `CwdError`. That gap is now
**resolved for three of the four** — `Fact`, `CwdError`, and `SubagentError`
are exported from `conway::plugin` and pinned by name in
`plugin_surface.rs`'s `fact_and_capability_handle_errors_are_constructible_
and_matchable` test. The remaining two are **deliberately not exported**,
not overlooked, per `crates/conway/src/lib.rs`'s own doc comment on `pub mod
plugin`:

- **`SubagentSpec`** — a third-party fork/spawn goes through this crate's own
  `ForkSpec`/`SpawnSpec` instead — kept as visibly distinct types so fork and
  spawn stay two separate primitives, never blurred into one — which convert
  into `SubagentSpec` via `From` but are never themselves that type. You have
  no reason to construct or match `SubagentSpec` directly.
- **`RuntimeError`** — `ToolCtx.subagents`'s fallible methods return
  `SubagentError`, never `RuntimeError`; there is no reachable call site for
  `RuntimeError` from this facade's surface at all.

Everything else `docs/embedding.md`'s reachability table names as
implementable from a facade-only crate — `PermissionGate`, `Tool`, `Plugin`,
`ContextHook`, and, since a later item, `Backend`
via the separate `conway::backend` module — is implementable the same way:
name the trait at the facade root, name its supporting types from the
matching curated module, register through the builder.

### Spawning a child process from a `Tool`

If your `Tool::invoke` spawns a child process (`tokio::process::Command`),
you own reaping it: a child that outlives the tool call that spawned it is a
leaked, possibly-still-running process. `conway::plugin::kill_group` is the
supported way to do that — spawn with `.process_group(0)` (the child becomes
its own process-group leader, so its pid doubles as the pgid), then on your
timeout/cancellation path call `kill_group(&mut child, pgid).await`, which
SIGTERMs the whole group, gives it a grace period, and SIGKILLs-and-reaps if
it hasn't exited. This is the exact primitive `conway-plugin-subprocess` and
`conway-plugin-mcp` use for their own spawned children (board item
`01M0EKVR1BEXXS75NV2JC4HZZ9` consolidated what used to be independent
hand-copies in each of those crates plus `conway-tools` itself into this one
re-export) — reuse it rather than re-deriving the SIGTERM-then-SIGKILL
sequence yourself. Unix-only (`#[cfg(unix)]`), and only present when
`conway`'s `builtin-tools` feature is on (the default) — a binary that opts
out of default features and still spawns child processes owns its own
reaping, exactly as it would if built-in tools had never existed.

### `ToolCtx`'s handle fields

If you're writing a `Tool` rather than just a `ContextHook`, `ToolCtx` hands
you capability handles you call methods on but never name the type of:
`ctx.chdir`, `ctx.events`, `ctx.subagents`. This is deliberate, not a gap —
`docs/embedding.md`'s "Writing a plugin" section explains why those types
stay unexported. `ctx.cancel` is the one exception (`CancellationToken` is
exported, so a helper function can take `&CancellationToken`).

Unit-testing a `Tool` means constructing one of these `ToolCtx` values
yourself. `ToolCtx::for_test(agent_id, cwd, subagents, events)` does that
without you ever naming `CwdHandle`/`SubagentHandle`/`EventSinkHandle` —
pass it `Arc::new(conway::testkit::FakeSubagentHost::new(agent_id))` and
`Arc::new(conway::testkit::CollectingEventSink::new())` (behind this crate's
`testkit` feature), cloning each `Arc` first if you want to assert on it
after `invoke` runs. See `docs/embedding.md`'s "Writing a plugin" section
for the full example and the fields it defaults for you.

### Artifacts

`ContextHookCtx::artifacts` is an `ArtifactWriteHandle` — a hook can write a
file inside the agent's confinement root without reaching for the filesystem
directly:

```rust
let path = ctx.artifacts.write("spill.txt", b"overflow content".to_vec()).await?;
```

This is real and exercised end to end by `plugin_surface.rs`'s own
`authored_hook_transforms_payloads` test, which drives a hook directly against
its own `ArtifactWriter` implementation via `ArtifactWriteHandle::new(writer,
agent_id)` — write that if you want to assert on what your hook actually
wrote, the way that test's `RecordingArtifactWriter` does. `ArtifactWriteHandle
::noop(agent_id)` (the constructor used in step 3, above) is for the opposite
case: a hook under test that never calls `ctx.artifacts.write` at all. Either
way, the real containment-checked writer is supplied by the runtime when your
hook runs inside an actual session — neither constructor is what production
code ever sees.

## Testing your hook

**A unit test on your hook's transform logic is not proof the hook is
reached in a real run.** This is not a stylistic preference: nothing may
claim to be reached that isn't, and a check isn't proven until something has
been shown to fail it — sharpened by this exact codebase's own history: five separate
mechanisms shipped, documented, and unit-tested in 2026-07-30/31 while never
being called from any production path (`read:*` pattern grants inert for 12
of 13 tools, `Plugin::on_init` never invoked, prompt caching hardcoded off,
`TruncationPolicy::Artifact` silently a no-op, `LogRecord::ContextMask`
lacking any producer). Every one of them had a passing unit test. None of
them had a test proving the *host* actually called them.

What this means for you, concretely:

1. **Test the transform in isolation first** (step 3 above) — fast, no
   network, catches logic bugs cheaply.
2. **Then drive it through `ConwayBuilder::build()`** and a real turn (step 4
   above), and assert on something only your hook's presence could produce
   — the appended segment surviving into `turn.text()`'s output, or a
   changed `session_usage`, or an artifact file existing on disk. **Assert
   on the observable outcome, not an intermediate signal**: a hook that
   silently no-ops and a hook that's correctly wired but has nothing to do
   both produce "nothing changed" — pick an assertion that a broken wiring
   genuinely cannot satisfy (a worked example elsewhere in this tree: the
   `AutoAllow` deny test asserts on the persisted `ToolResult` text, not on
   gate-call count, because a correct refusal and a silent full bypass both
   produce zero gate calls).
3. **Simulate failure deliberately, don't just hope you never hit it.** The
   biggest surprise waiting for you here: `ContextHook::before_request` and
   `on_overflow` have **no `Result` in their signatures** (`hooks.md` point
   3's "On error" row). There is no sanctioned way to signal "this hook
   failed" other than returning the payload unchanged — and a panic inside
   your hook is *not* caught the way a tool's panic is (`ToolRunner` wraps
   tool invocation in `catch_unwind`; the call site for `before_request`/
   `on_overflow` inside `AgentLoop::run_inner` does not). Write a test that
   feeds your hook a payload your logic doesn't expect and confirm it
   degrades the way you intend, on purpose, rather than discovering the
   panic behavior live.

## Debugging

- **The event stream is your primary window.** `docs/embedding.md`'s
  "Consuming the event stream" section covers `conway::EventStream` in full;
  for a hook specifically, look at what changed between the request you
  handed the runtime and what actually reached the model — a hook that
  silently does nothing produces a request identical to the one you'd get
  with no hook registered at all, which is easy to miss unless you diff.
- **A hook that's wired but never runs looks identical to one that isn't
  registered.** `ContextHook` is `Option<Arc<dyn ContextHook>>` under the
  hood (`hooks.md` point 3's "When absent" row) — there is no error, no log
  line, nothing, if you forget the `.with_context_hook(..)` call. This is
  exactly why step 4 above (a real turn, not just a direct call) matters:
  step 3's test would pass unchanged even if you never wired the hook in at
  all.
- **The common failure mode named across this whole set: a matcher that
  silently matches nothing.** the extension design calls
  this out directly as another harness's documented failure — a hook
  declared against a pattern that happens to match zero calls looks, from
  the outside, identical to a hook that correctly decided not to act.

  **This is now a live hazard rather than a cautionary tale.** Configured
  `hooks.rules[]` entries take a `match` field (exact tool name, or a
  `*`-glob), and a `match` naming a tool nothing ever calls fires never,
  quietly. Two things narrow the blast radius, and neither removes it: a
  `match` on an event that carries no tool name at all
  (`session_starting`, `child_spawned`, `request_assembled`,
  `context_overflow`, `child_reported`, `prompt_submitted`) is a typed config error naming your
  rule's `id` rather than silent inertness; and omitting `match` entirely
  means *fire for every call*, which is loud and therefore self-correcting.
  A `match` that is merely misspelled is the case with no safety net —
  check the tool's registered name, not the name you remember.

  The same *shape* of mistake applies to the in-process `ContextHook` you
  are building here, with no matcher involved: double-check that the hook
  you registered is the one actually reaching `AgentLoop::run_inner` for
  the session you're testing, not a second builder instance you constructed
  and never used.
- **No `Result`, remember.** If your hook is silently doing nothing and you
  expected an error somewhere, re-read "Testing your hook" above — there is
  no error channel for `before_request`/`on_overflow` to use. "Nothing
  changed" and "my hook decided not to act" and "my hook is completely
  disconnected from this session" are three different situations that
  produce the exact same observable output unless you've built the kind of
  test that distinguishes them.

## Presenting your plugin to an operator

Everything above this chapter teaches you how to make a plugin *work*.
Nothing above it says how to make it *fit* — how it names itself, what it
tells someone deciding whether to enable it, and where its own settings
appear once enabled. That vacuum is why the twelve first-party plugin crates
that ship today (`conway-plugin-{discover, history, idiom, mcp, memory,
names, path, skills, statusline, stepguard, subprocess, trim}`) each surface
themselves differently, with no single place an operator can go to learn
what any one of them changed
([`DESIGN-surface-coherence.md`](../vision/DESIGN-surface-coherence.md)
names this finding in full). This chapter states the rules a plugin author
can follow today, and names the one piece that is a ruling rather than a
built mechanism yet.

**Written as rules with examples, not principles.** "Be consistent" is not
followable. Each rule below is stated so you can check your own plugin
against it without a judgment call. Where a rule cannot be stated that
concretely, this chapter says so rather than writing it up softly — that is
itself a finding, not a gap you're expected to close by guessing.

### Naming

**Rule: a plugin's id is `<owner>.<name>`, lowercase, no more than two
segments.** Every first-party plugin follows this already —
`conway.memory`, `conway.stepguard`, `conway.skills` — and a third party
follows the identical shape with their own owner segment instead of
`conway`, e.g. `acme.greeter`. This is not a style suggestion: `PluginManifest::id`
is the identity a `settings.json` `plugins.install`/`plugins.subprocess`/
`plugins.mcp` entry names, the identity `hooks.rules[].event` narrows
against for a plugin-declared event, and the identity every command this
plugin contributes is prefixed with (next rule) — get it wrong and every
one of those has to change together.

**Rule: a command this plugin contributes is named `<plugin-id>.<verb>`,
lowercase, one bare verb.** This is not a convention you have to remember —
`Command::spec`'s own doc states it and the host enforces it mechanically:
you supply a bare `name` (e.g. `"greet"`) to `CommandSpec`, and the host
prefixes it with your plugin's own `PluginManifest::id` before registering
it, so `/acme.greeter.greet` is what an operator types and you never choose
your own namespace (`crates/conway-core/src/ports/plugin.rs`'s own doc on
`Command::spec` and `CommandSpec::name`). Pick a verb the way you would pick
a shell subcommand name — `memory.forget`, not `memory.do_the_forgetting`.

**Rule: a status-line contribution is a short, fixed-width-friendly
fragment, not a sentence.** `Plugin::status_contributions` sits beside
other plugins' fragments and conway's own status text on one line
(`conway.statusline`'s own page describes the budget this shares).
`"mem: 12 notes"` fits; `"Memory plugin currently holding 12 saved notes for
this session"` does not — it will get truncated by whatever renders the
status line, and truncation is not this method's job to defend against.

### When to contribute a command, a settings entry, status-line text, or nothing

This is the same three-kind test
[`DESIGN-surface-coherence.md`](../vision/DESIGN-surface-coherence.md) §4
applies to the built-in TUI surface, applied to what your plugin adds to
it — a plugin is not exempt from the house rules the CLI holds itself to
just because it is optional.

- **Contribute a command (`Plugin::commands`) when your plugin does
  something now, or shows the operator something on demand.** `/ping`
  (`conway-plugin-skeleton`'s worked example) is an ACTION; a hypothetical
  `/acme.greeter.log` showing recent greetings would be a VIEW. If what
  you're building is "the operator types something and gets an immediate
  result," it's a command — not a settings toggle they have to go find
  first.
- **Contribute a settings entry when the thing being decided is
  configuration your plugin reads on every turn or every session, not
  something invoked once.** A plugin that reads its own `[plugins.config.
  <id>]` values for behaviour that changes without the operator typing
  anything (a memory retention window, a status-line refresh interval) is
  CONFIGURATION-shaped and belongs wherever conway's own configuration of
  that kind lives — see "Configuration" below for exactly where that is,
  since the answer depends on the same persistent-versus-session split
  conway's own settings menu follows.
- **Contribute status-line text (`Plugin::status_contributions`) only for a
  value an operator would want to glance at continuously without asking for
  it** — a live count, a mode, a health indicator. If the value only matters
  when explicitly requested, it belongs on a command's output instead; a
  status line that accumulates every plugin's "just in case" fragment stops
  being glanceable, which is the entire reason the budget exists.
- **Contribute nothing** when your plugin's only job is registering a tool
  for the model (`Plugin::tools`) or an instruction fragment
  (`Plugin::instructions`) — most first-party plugins are exactly this
  shape, and adding a command or a settings row with nothing operator-
  actionable behind it is surface for its own sake.

### What you owe an operator deciding whether to enable you

**Rule: override `Plugin::description()`.** Its default
(`PluginDescription::default()`, every field empty) is honest but useless —
a browser renders it as "(no description)." At minimum, fill `summary` (one
line) and `you_get` (what turning you on adds — tools, commands, an
instruction). Fill `you_lose` if there is a real cost to leaving you off
that isn't obvious from `you_get` alone, and `costs` if you do standing
work every turn or every session (a network call, a file read) rather than
only when invoked. This method is read before a plugin is enabled
(`ConwayBuilder::build`-adjacent time), by a person, never assembled into a
prompt — write it for a person, not for a model.

```rust
fn description(&self) -> PluginDescription {
    PluginDescription {
        summary: "notes that survive a restart".to_string(),
        you_get: "3 tools · /memory · an instruction telling the model \
                  when to write things down".to_string(),
        you_lose: "nothing else -- recall falls back to context".to_string(),
        costs: "a small read at the start of every turn".to_string(),
    }
}
```

**What this chapter cannot yet give you a rule for.** `description()`
covers the free-text half of what an operator is owed. The other half —
a *structured* declaration of exactly which commands, tools, settings, and
status-line contributions a plugin registers, surfaced in one place rather
than reconstructed by reading source — does not exist yet.
[`DESIGN-surface-coherence.md`](../vision/DESIGN-surface-coherence.md) §7
states the ruling that it must; no `Plugin` trait method for it is built as
of this writing, and this page will not describe one until it lands. If you
are writing a plugin today, `description()` plus accurate `commands()`/
`tools()`/`status_contributions()` implementations are everything you can
do — there is no additional method to implement.

### Configuration: your settings follow the same rule conway's do

**A plugin's own settings are subject to the identical persistent-versus-
session split `DESIGN-surface-coherence.md`'s corrected rule 1 states for
conway's own `/settings` menu.** If a value is global and persists across
restarts (a retention window, an API endpoint your plugin calls), it is
*persistent configuration* and belongs wherever conway's own persistent
configuration of that shape lives — today that means a config-file key; the
ruling anticipates a `/settings` row once the mechanism below exists. If a
value is scoped to *this session's* current use — which of several modes
your plugin is running in right now, for this conversation only — it stays
reachable the way conway's own session-scoped state does: a command, not a
buried settings toggle.

**This is a rule stated ahead of its own mechanism, and that is disclosed
rather than hidden.** Per-plugin configuration was ruled open
(`DESIGN-plugin-dependencies.md` §6, "SETTLED 2026-08-26 — the first slice
is over"): `[plugins.config.<id>]` is intended to become a real settings
surface, with a plugin declaring its config schema once and that
declaration rendering three ways (a TUI editor, an embedder's JSON, a
one-shot run's declared defaults). As of this writing, no
`Plugin`-trait method for declaring that schema exists in
`crates/conway-core/src/ports/plugin.rs`, and `ConwayConfig` has no
`[plugins.config.<id>]` key wired up to reject or accept one. The
persistent-versus-session *rule* is settled; the *mechanism* that lets a
plugin author act on it is not built. Do not invent a bespoke top-level
config section for your plugin's persistent settings in the meantime —
that is precisely the "stays closed" shape `DESIGN-plugin-dependencies.md`
§6 rejected, and it will not be forward-compatible with the declared-schema
surface once it lands.

### The compat exception

A plugin translated from Claude Code's format
([`claude-compat.md`](claude-compat.md)) is not held to this chapter's bar.
Compat is a curated on-ramp — it exists so someone arriving from Claude Code
relearns nothing on day one, not a second, permanent way to author a
plugin. A translated plugin may carry Claude Code's own naming and
structure unchanged; this chapter's naming, contribution, and description
rules apply in full to a *native* plugin — one written against
`conway::plugin` directly, first-party or third-party, from this page's
"Writing a Rust plugin" section onward.

## Where to go next

[`docs/plugins/README.md`](README.md) routes the rest of the set.
[`scripts.md`](scripts.md) covers the any-language script convention in
depth — the same `hooks.rules[].command` mechanism "Ten minutes to a
working hook" above uses, in full: any language, not just `/bin/sh`, and
the exact per-event boundary of what's dispatched today.
[`inference-hooks.md`](inference-hooks.md) covers a hook whose
decision is made by a model rather than by code.
[`cookbook.md`](cookbook.md) carries worked, end-to-end examples beyond
this page's single demo hook — five of them, each labeled
implementable-today, partially-implementable, or blocked.
