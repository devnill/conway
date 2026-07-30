# D1 — Transport, wire framing, and plugin process lifecycle

Status: design, not implemented. Scope: how an out-of-process plugin talks to
conway and how its process is managed. Out of scope: which extension points
exist (D2), the RPC-shaped `ToolCtx` and message vocabulary (D3), trust (D4),
UI templating (D5).

All file:line references are against HEAD `9a8d882` (v0.6.0).

> **Partly superseded by D6** (`extension-architecture.md`), the synthesis.
> Where this document and D6 disagree, **D6 wins**.
>
> - **`Plugin::on_init` as the handshake hook (§203) is void.** §645 raised
>   its zero call sites as an open question; the answer was to *remove* the
>   method rather than wire it (D6 §11.6). A plugin does its connect work in
>   its own constructor, before `with_plugin`.
>
> Kept unedited as the record of the reasoning, not as current guidance.

---

## 0. Mechanism: confirmed, with its rejected alternatives

**Subprocess + JSON-RPC 2.0 over stdio holds for conway.** Not re-surveyed —
confirmed against this codebase, on three specific grounds:

1. **The port surface already fits.** `ToolCall` and `ToolOutput` are
   `Serialize + Deserialize` and round-trip-tested
   (`crates/conway-core/src/ports/plugin.rs:552-575`). `PluginManifest`,
   `ToolSpec`, `ContentBlock`, `Artifact`, `TruncationPolicy` likewise. The
   only non-serializable thing in the tool path is `ToolCtx`'s two trait
   objects, which that type's own doc already names as the T-8 limitation
   (`ports/plugin.rs:256-259`).
2. **The dependency budget is already paid.** `tokio` with `process` +
   `io-util`, and `nix` with `signal` + `process`, are existing workspace
   dependencies in use by `crates/conway-tools/Cargo.toml:32-35`.
   `serde_json` is universal. C-04's zero-new-dependency path is not a
   compromise here; it is the whole implementation.
3. **The failure model matches P-10's existing one.** The runtime already
   treats tool output as untrusted, sanitizes it
   (`crates/conway-runtime/src/tools/runner.rs:409`), catches panics per call
   (`runner.rs:180-215`), and turns tool failure into model-visible feedback
   rather than an abort (`runner.rs:307-309`). A misbehaving child slots into
   that model without inventing a new one.

Rejected, and why (for the record):

| Alternative | Rejected because |
| --- | --- |
| **WASM / Component Model** | Async host-callbacks (which conway *requires* — see §2) stabilized only with WASI 0.3 in Feb 2026; WASI 1.0 is not final. Helix declined it this year on maturity grounds; Zed ships it only behind a deliberately narrow API. `wasmtime` is a very large dependency against C-04. Revisit when WASI 1.0 is final. |
| **C ABI + `libloading`** | No memory-safety boundary at all — a plugin bug is a host segfault, which defeats the point of `catch_unwind` per call. Rust has no stable ABI, so every plugin must be recompiled against the host's exact compiler. Unconditionally out. |
| **Embedded scripting (Lua/JS/Python)** | Picks a language for third parties, ships an interpreter into the dependency tree, and still needs a sandbox story. Strictly more cost than a subprocess for strictly less isolation. |
| **SDK-over-IPC (a socket, a broker daemon)** | Same protocol problem plus a rendezvous problem (paths, permissions, cleanup on crash) that stdio pipes solve for free by being lifetime-bound to the child. |
| **Two pipes, host-defined binary framing** | No debuggability, no ecosystem, and every plugin author writes a framer. JSON-RPC's whole value here is that it is boring and already implemented in every language. |

---

## 1. Framing

### Decision: newline-delimited JSON (NDJSON), one JSON-RPC 2.0 object per line

- A **frame** is exactly one compact JSON value followed by `\n`.
- A trailing `\r` is stripped before parsing (a CRLF-writing plugin works).
- **Empty lines are ignored**, not errors.
- **Compact serialization is mandatory on both sides.** JSON's own grammar
  guarantees no unescaped newline inside a serialized value — the codebase
  already asserts exactly this invariant for its own stream
  (`crates/conway-core/src/event.rs:439`: `assert!(!json.contains('\n'))`).
  So "robust to embedded newlines" is not a real advantage of
  `Content-Length` for a JSON-only wire; it is only an advantage for a
  *pretty-printing* producer, which the contract forbids.
- **Batches (top-level JSON arrays) are rejected.** JSON-RPC 2.0 permits
  them; conway does not. A batch of 100k tiny requests is an amplification
  vector for no benefit, and LSP made the same call.

**Rejected: LSP-style `Content-Length` headers.** Its one genuine advantage
is that you learn a frame's size before reading it, so you can refuse an
oversized frame without buffering it. That advantage is small here because
conway's response to an oversized frame is to kill the child anyway (§8), and
it costs: a hand-rolled header parser (the workspace has `httparse`, but only
as a `conway-cli` **dev**-dependency — production use would be a new
dependency against C-04), the loss of `tail -f | jq` debuggability, and
divergence from the JSONL precedent this project already sets
(`crates/conway-cli/src/render/jsonl.rs`, one flat object per line).

**Rejected: length-prefixed binary / MessagePack / CBOR.** New dependency, no
human-readable wire, no ecosystem win.

### stderr is reserved for plugin diagnostics

- The child's **stdout is protocol only**; **stderr is diagnostics only**. A
  plugin logs to stderr and cannot corrupt the stream by doing so. This is
  stated as a hard requirement in the plugin contract, because "someone added
  a `println!`" is the single most likely real-world plugin bug.
- The host reads stderr line-by-line on its own task, the same shape
  `crates/conway-tools/src/shell/bash.rs:201` already uses.
- **Where it goes:** to `tracing` under target `conway::plugin`, with
  `plugin_id` and `pid` fields, at `warn!`. Deliberately **not** to the
  `EventSink` and **not** into any `ToolOutput`. `EventSink::emit` is
  contractually non-blocking fan-out for agent-visible events
  (`crates/conway-core/src/ports/events.rs:9-14`); a chatty plugin would
  drown the TUI, and routing stderr into tool output would make
  plugin-controlled text model-visible without passing the truncation
  accounting.
- The host keeps a bounded **ring buffer** of the last 64 lines / 8 KiB per
  plugin. It is attached (truncated) to the host-side error when a call fails
  or the child dies, and shown by `conway plugins` (§6).
- Every stderr line that can reach a terminal is control-character sanitized
  with the same rule as `runner.rs:409` (Unicode `Cc` → U+FFFD). Child stderr
  is attacker-controlled bytes headed for the operator's terminal.
- The child's **stdin is the protocol input**; it is never the host's
  terminal. The child gets `Stdio::piped()` on all three fds.

---

## 2. Bidirectionality

### The problem

`ToolCtx` carries two *capabilities*: `events: EventSinkHandle` and
`subagents: Arc<dyn SubagentHost>` (`ports/plugin.rs:300-303`). A remote tool
calling `ctx.subagents.start(...)` is a **plugin→host call**. So the wire is
not request/response in one direction.

### Decision: JSON-RPC 2.0's symmetry, confirmed, with disjoint id namespaces

JSON-RPC 2.0 is symmetric by construction — both endpoints are simultaneously
client and server. It is sufficient, and no framing extension is needed.

- **Frame kinds** are distinguished structurally: `method` + `id` = request;
  `method`, no `id` = notification; `id`, no `method` = response
  (`result` xor `error`).
- **Id namespaces are disjoint by origin, not by value.** The host allocates
  ids for host→plugin requests; the plugin allocates ids for plugin→host
  requests. A response is always routed to the side that *sent* the matching
  request, so the two id spaces cannot collide even if both sides pick `1`.
  This is stated explicitly because assuming a single shared id space is the
  classic bug in symmetric JSON-RPC implementations. For debuggability the
  host's own ids are strings `"h:<counter>"`; plugin ids are echoed back
  **verbatim, preserving JSON type** as the spec requires.
- **One reader task per child** owns the child's stdout and is the only
  demultiplexer. It dispatches:
  - response → look up `id` in the pending map, `oneshot::send`; unknown id →
    log, count, drop (never panic);
  - request → spawn a handler task holding a concurrency permit; reply when
    done;
  - notification → dispatch, no reply;
  - anything else (missing `jsonrpc`, non-object, array) → protocol error,
    counted, dropped (see §8 for the thresholds).
- **One writer task per child** owns the child's stdin, fed by a bounded
  `mpsc`. Concurrent host tasks therefore cannot interleave partial frames,
  and a plugin that stops draining stdin produces backpressure that surfaces
  as a call deadline (§4), not unbounded host memory.
- **Correlation of a callback to its invocation: an opaque `ctx_token`.**
  Every host→plugin `tool/invoke` carries a `ctx_token` minted per
  invocation. Every plugin→host request must carry the same token. The host
  resolves the token against a per-invocation table holding the live
  `ToolCtx`, and **drops the entry when `invoke` returns**.
  - A callback bearing an unknown, expired, or foreign token is answered with
    a typed error and serviced no further.
  - **The plugin never supplies identity.** `agent_id`, `session_id`, `cwd`,
    and the confinement root are read from the host's table, never from
    `params`. A plugin cannot act as another agent by asking to.
  - The *shape* of what may be called through a token is D3's; that it is
    token-scoped and host-resolved is D1's.
- **Concurrency caps** (P-10): `max_inflight` host→plugin requests per
  connection (default: `4 × max_parallel_tools`), and
  `max_inflight_callbacks` inbound (default 32). Exceeding the inbound cap
  answers with an error rather than queueing.
- **Ordering:** none across ids. A plugin may answer out of order. Within one
  direction, frames are FIFO because there is exactly one writer.

---

## 3. Process lifecycle

### 3.1 Spawn timing — eager, at startup, before `ConwayBuilder::build()`

This is forced by the code, not chosen for latency:
`PluginRegistry::from_plugins` is **synchronous, infallible-by-panic at the
call site, and eager** — it calls `plugin.tools()` and `tool.spec()` and
compiles every JSON Schema at construction
(`crates/conway-runtime/src/tools/registry.rs:79-115`), and `Runtime::new`
`.expect()`s it (`crates/conway-runtime/src/runtime.rs:302-305`). `Plugin::
manifest()` and `Plugin::tools()` are sync and infallible
(`ports/plugin.rs:27-29`). A subprocess plugin's tool list and schemas must
therefore exist **before the runtime does**.

**Shape:** an async constructor, `PluginProcess::connect(spec).await ->
Result<Arc<dyn Plugin>, PluginError>`, called by the embedder/CLI from the
main async runtime, whose result is handed to the existing
`ConwayBuilder::with_plugin` (`crates/conway/src/builder.rs:196`). No change
to any `conway-core` port. The handshake result is cached in the struct, so
the sync `manifest()`/`tools()` are pure reads.

Rejected alternatives:

- **Lazy spawn on first use.** Impossible without either (a) a static
  declaration file listing tools and schemas — a second source of truth that
  can silently disagree with the running plugin, which is precisely the
  doc/code drift class this project keeps finding; or (b) making
  `Plugin::tools()` async and fallible, a breaking change to conway-core's
  most load-bearing port under §2's semver discipline. Independently, §7
  shows the latency saved is one 5–50 ms spawn against a multi-hundred-ms
  model round trip — negligible.
- **Handshaking inside `ConwayBuilder::build()`'s existing sync/async
  bridge** (`crates/conway/src/builder.rs:743-760`). **Specifically rejected,
  and this is the subtle one:** that helper runs the future on a throwaway
  current-thread runtime on a scoped thread that is then joined and dropped.
  A `tokio::process::Child` created there loses its reactor and child-reaper
  when that runtime drops — the process is orphaned and its pipes go dead. A
  long-lived child must be spawned on the runtime that outlives it.
- **`Plugin::on_init` as the handshake hook.** It is synchronous, and a
  workspace-wide grep shows it has **zero call sites** — the "called once at
  startup" contract in `ports/plugin.rs:31-37` is currently unimplemented, so
  nothing may be built on it.

**Handshake:** host sends `initialize` (protocol version, host capabilities,
this plugin's config values, cwd); plugin responds with its `PluginManifest`
plus a full `ToolSpec` per tool and each tool's `path_args` declaration.
Bounded at 10 s; on failure or timeout the host kills the child and `connect`
returns `PluginError::Init`. Fail closed: a plugin that does not handshake
registers no tools. Connect **pre-validates** every schema (compiles it) and
checks for tool-name collisions, because a collision reaching
`PluginRegistry::from_plugins` panics the process at `runtime.rs:303`; the
facade must surface it as a build error instead.

**Mechanical wrinkle to resolve in implementation:** `PathArgs::Named` holds
`&'static [&'static str]` (`ports/plugin.rs:143`), but a remote tool's path
argument names arrive at runtime. Recommendation: `Box::leak` the
handshake-declared names once per tool at connect time (bounded — once per
tool per process lifetime, with a cap on count and length); `'static` is
genuinely accurate since the registry lives for the process. The alternative
— widening `PathArgs` to `Cow<'static, _>` — is a breaking conway-core change
and should be D3's call if it wants one.

### 3.2 Supervision, restart, backoff

Per-plugin state: `Ready | Draining | Restarting | Unhealthy { reason }`,
plus restart count, `next_retry_at`, and last exit status.

- **Child death** (EOF on stdout, exit, or kill): every pending host→plugin
  call fails **immediately** with `ToolError::Internal` carrying the exit
  status and the tail of the stderr ring buffer. Nothing is left hanging —
  the same "always terminates" discipline `SubagentHost::await_result`
  commits to (`crates/conway-core/src/ports/subagent.rs:22-25`).
- **No automatic retry of a failed or timed-out call.** Tool calls are not
  idempotent; a replayed call may repeat a side effect. Backoff applies to
  *connections*, never to calls. The model sees a tool error, which is
  model-visible feedback exactly like a denial (`runner.rs:307-309`).
- **Restart is lazy and backoff-gated.** No background respawn loop. The
  next invocation after `next_retry_at` triggers a respawn attempt; calls
  arriving earlier get a fast typed failure. Backoff: 250 ms, doubling, 30 s
  cap, jittered. Budget: 5 restarts in 60 s → `Unhealthy`, stop restarting.
  (Rejected: an eager background respawn loop — it churns processes in an
  idle session for a plugin nobody is calling.)
- **On restart, re-handshake and verify the tool set is identical** (names +
  schemas + `path_args`). A mismatch → permanently `Unhealthy`; do not serve
  calls against schemas the registry did not compile. (Rejected: hot-reloading
  the registry — it is built once and immutable by design, and changing the
  announced tool set mid-session would break the stable-ordering contract the
  `ToolRegistry` provenance hash depends on, `registry.rs:117-119`.)
- **Unhealthy plugins are skipped, not unregistered.** Calls to their tools
  return a typed error immediately. The tool stays in the registry and stays
  announced, for the same stability reason. An embedder that wants to stop
  announcing it has the supported surface already: `ContextHook::
  before_request` narrows announced tools (`ports/plugin.rs:337-344`), and
  the host exposes health for exactly that purpose.

### 3.3 `HealthRegistry` is **not** reused

`HealthRegistry` is keyed by `EndpointId` and speaks `BreakerState` /
`Observation` about model endpoints; `Router::resolve` reads it as a filter
for model selection (`crates/conway-core/src/ports/routing.rs:14-37`).
Putting plugin processes in it would mean either synthesizing fake
`EndpointId`s that the router could then try to send *model traffic* to, or
widening a core port for an unrelated subject. **Plugin health is a separate,
host-owned structure** that reuses the breaker *pattern* and shares no code
and no keyspace.

### 3.4 Shutdown, kill, orphan prevention

**Reuse `bash.rs`'s existing process-group discipline. Do not reinvent it.**

- Spawn with `.process_group(0)` (`bash.rs:188`) so the child's pid is its
  own pgid.
- Shutdown ladder: `shutdown` request → `SHUTDOWN_GRACE` (2 s, deliberately
  the same number as `bash.rs:162`'s `TERM_GRACE`) → close stdin (EOF is the
  second, language-agnostic signal) → `kill(-pgid, SIGTERM)` → grace →
  `kill(-pgid, SIGKILL)` → `child.wait()` to reap. That is exactly
  `bash.rs:286-295`'s `kill_group`.
- **One implementation, not two.** `kill_group` is currently private inside
  `conway-tools`' `unix` module. Recommendation: lift it to
  `conway_tools::process::kill_group` (public) and have the plugin host call
  it — no new dependency, no layering inversion (`conway-tools → conway-core`
  only). If that is unacceptable, the fallback is a verbatim copy carrying
  the same "DUPLICATED, DELIBERATELY … must change in the same commit"
  doc-comment discipline the codebase already uses at
  `crates/conway-runtime/src/permission.rs:153-164`. What is **not**
  acceptable is a second, subtly different kill path for a
  security-relevant routine.
- **`Drop` guard:** `Drop` cannot await. It signals `SIGTERM` to the group and
  `tokio::spawn`s the reap; with no runtime available (process teardown) it
  falls back to a synchronous `kill(-pgid, SIGKILL)` via `nix`, which is a
  raw syscall and safe in `Drop`.
- **Host SIGKILL:** nothing can run, so the only surviving defense is a
  contract — **a plugin MUST exit when its stdin reaches EOF**, which the
  kernel delivers when the host's fds are reaped. Stated as a hard
  requirement in the plugin contract. (Rejected: Linux `PR_SET_PDEATHSIG` —
  not portable to macOS, which is this project's development platform, and it
  interacts poorly with process groups. The stdin-EOF contract covers both
  platforms.)

---

## 4. Timeouts, and the permission-contract asymmetry

### The tension, precisely

`PermissionGate::check` "may block indefinitely… Gate cancellation surfaces
as `PermissionDecision::Deny { reason: "cancelled" }`, never as a hang"
(`crates/conway-core/src/ports/permission.rs:11-15`), and `PermissionBroker`
"never imposes a timeout on the gate"
(`crates/conway-runtime/src/permission.rs:7-9`). A human at a prompt takes as
long as they take. A blanket transport timeout **breaks that contract**.
No timeout lets a wedged plugin hang a tool call forever.

### Resolution: the timeout is a property of the extension point, not of the transport

The transport ships three primitives and **no default policy**.

**(a) Connection liveness — always on, every call kind, including permission.**
The host sends a `$/ping` request after 10 s of connection idleness whenever
anything is pending, and requires a response within 5 s. This is the move
that dissolves the tension: it distinguishes *"alive and deliberately still
working — a human is at a prompt"* from *"wedged or dead"*. A plugin blocked
on a human answers pings forever and is never cut off. A wedged plugin fails
a ping, the connection is poisoned, and everything pending fails closed.
**Health probing is not a decision timeout**, and it is the only clock that
applies to a permission-shaped call.

**(b) Per-call-kind deadline, chosen by the extension-point adapter.**

- `Deadline::Bounded(Duration)` — the default for tool invocation. **Default
  120 s**, deliberately matching `bash`'s `DEFAULT_TIMEOUT_MS = 120_000`
  (`bash.rs:18`): a remote tool is the same order of thing as a shell
  command, and 120 s is already the number this codebase teaches.
  Overridable per plugin and per tool in `plugins.json`.
  **Explicitly not Claude Code's 600 s.**
- `Deadline::Unbounded` — available only to extension points whose port
  contract sanctions indefinite blocking (today: a permission gate). Still
  covered by (a) liveness and by §5 cancellation.

**(c) Inactivity is primary; total is a backstop.** A call's deadline resets
on any progress notification from the plugin bearing that call's token — the
same streaming shape `bash.rs:231-249` already implements by emitting
`ToolProgress` per line. An absolute `max_total` (default 10 min for tool
invocation) prevents a plugin from holding a call forever by spamming
progress.

### Fail closed, without exception

- A timed-out `tool/invoke` yields `ToolError::Timeout { after_secs }`
  (already exists, `crates/conway-core/src/error.rs:76-77`) — an error. Never
  a success, never an allow.
- If a decision-bearing call ever times out, the result is
  `Deny { reason: … }` — the identical shape gate cancellation already takes
  (`ports/permission.rs:13-15`).
- **Invariant, and it should have a test that drives a plugin which simply
  never answers: there is no code path in which a transport-level failure
  produces an allow.**

### Doing better than the reference point

The documented complaint about Claude Code's HTTP hooks is 600 s inherited
with no retry and no backoff. This design's answer is not retries — retries
of a non-idempotent tool call are worse than the disease. It is: a **5×
shorter default**, **progress-based extension** so legitimate long work is
not cut off, **liveness pings** so "wedged" and "slow" are different
conditions with different responses, and **backoff at the connection level**
where retrying is actually safe.

---

## 5. Cancellation across the boundary

### The facts

`ToolCtx::cancel` is a poll-only `Arc<AtomicBool>` with a parent chain
(`ports/plugin.rs:451-504`); it does not cross a process boundary. `runner.rs`
bridges the awaitable `tokio_util` token to it by spawning a watcher
(`runner.rs:339-347`) and races `cancelled()` against `invoke` with a biased
`tokio::select!` (`runner.rs:313-317`).

### Mechanism

1. On the host side, `RemoteTool::invoke` spawns a watcher that polls
   `ctx.cancel.is_cancelled()` every **50 ms** — reuse `bash.rs:160`'s
   `POLL_INTERVAL` so there is one cancellation-latency number in this
   codebase, not two. Ancestor cancellation needs nothing extra:
   `is_cancelled` already walks the chain (`ports/plugin.rs:483-494`).
2. On observation, the host sends a `$/cancelRequest` **notification** naming
   the host request id (JSON-RPC's own convention; LSP does exactly this).
3. A cooperating plugin answers the cancelled request with a JSON-RPC error
   carrying a `RequestCancelled` code; the host maps it to
   `ToolError::Cancelled`.

### If the plugin ignores cancel — closing the drop-does-not-kill gap

**In-process, `runner.rs`'s `select!` drops the losing `invoke` future and the
work stops because it was a future in this process. Dropping a future does
not kill a subprocess.** That difference is closed in two explicit tiers:

1. **Abandonment (frees the host, does not stop the plugin).** After
   `CANCEL_GRACE` (2 s — again `bash.rs:162`'s number) the host stops waiting,
   resolves the call as `ToolError::Cancelled`, and marks the request id
   *abandoned*; a late response for an abandoned id is dropped, logged, and
   counted. **Abandonment alone must never be presented as cancellation** —
   the child is still running.
2. **Escalation to the process.** On the first abandoned call, the connection
   is marked `Draining`: no new calls are dispatched to it, and after
   `DRAIN_GRACE` (5 s) — or immediately if a second call is also abandoned —
   the child is killed via the §3.4 process-group ladder and restarted under
   the §3.2 backoff.

   Stated plainly, because it is a deliberate liveness sacrifice: *a plugin
   that ignores cancellation is not merely slow; it is a process doing
   unsupervised, unattributable work with the host's privileges, and the only
   mechanism that actually stops it is a signal to its process group.*

A cancelled `Deadline::Unbounded` call follows the same path: a permission
plugin that ignores cancellation gets killed, and the decision fails closed to
`Deny` — which is exactly what `ports/permission.rs:13-15` already promises.

Rejected: **cooperative cancel only** (it makes cancellation a lie across a
process boundary). Rejected: **kill on every cancel** (a plugin that answers
`RequestCancelled` promptly must survive, or correct behavior costs a
respawn).

---

## 6. Discovery and configuration

### Decision: adopt conway's two-scope layering; reject the scattering

Plugins are declared in **one filename, in two scopes**, mirroring
`permission_file_paths` (`crates/conway/src/config/discovery.rs:65-83`)
exactly:

1. project — `<nearest ancestor .conway>/plugins.json`
2. global — `<XDG or ~/.conway>/plugins.json`

Same `discover()`-then-`xdg_config_path()` precedence, same "a missing file at
either level is not an error" rule. Project entries override global entries
**by plugin id**.

Plus one non-file source that already exists and is unchanged:
`ConwayBuilder::with_plugin(Arc<dyn Plugin>)` (`builder.rs:196`). A connected
subprocess plugin *is* an `Arc<dyn Plugin>`, so the embedder path needs no new
API.

**Explicitly rejected: Claude Code's scattering** — hooks across user
settings, project settings, local settings, enterprise policy, and per-project
MCP files, with no runtime way to list what is active. Two scopes, one
filename, one precedence rule, and a mandatory inventory command (below).

**Rejected: a `[plugins]` table inside `settings.json`.** `settings.json` is
merged across five sources including environment variables and CLI flags
(`crates/conway/src/config/mod.rs:4-8`). An env-var-injectable *executable
path* is a privilege-escalation surface. A separate, file-sourced-only file
with **no env override** is the safer shape, and `permissions.json` already
sets that precedent.

### Loading reports; it does not decide

A project-scoped `.conway/plugins.json` is checked-in, potentially hostile
content in a freshly cloned repo. That is D4's problem, but D1 must not
foreclose it. The loader therefore returns, per entry, `{ id, spec, origin:
PathBuf, scope: Project | Global }` and **makes no execution decision itself**.
Non-binding recommendation to D4: project-scoped entries should require
explicit opt-in.

### "What is loaded right now, and from where" — required, not optional

- `Conway::plugins() -> Vec<PluginStatus>`, one row per plugin: id, version,
  **origin** (the file path, or `"injected via with_plugin"`), scope,
  transport (`in-process` | `subprocess`), the command line as spawned, pid,
  state (`Ready`/`Draining`/`Restarting`/`Unhealthy{reason}`), restart count,
  tools provided, and the tail of the stderr ring buffer.
- CLI: `conway plugins`, a new variant alongside `Sessions` and `Routes`
  (`crates/conway-cli/src/cli.rs:78-83`) — the "inspect what the system
  decided" subcommand pattern already exists.
- TUI: `/plugins`, on the same principle `/settings` already applies to
  grants: *"An operator must be able to see what they have granted; a rule
  set nobody can inspect is a trap."*
  (`crates/conway-runtime/src/permission.rs:419-422`.)
- Startup: one `info` line per loaded plugin with id + origin, through the
  stderr-only diagnostic discipline (`crates/conway-cli/src/diag.rs`).

### Per-plugin config

A plugin's config values come from its own `plugins.json` entry and are
delivered in the `initialize` handshake. Known asymmetry worth recording:
`Runtime::new` hardcodes `Arc::new(PluginConfig::default())` shared by every
tool (`crates/conway-runtime/src/runtime.rs:314`) — there is no per-plugin
config plumbing today. D1 does not fix that; the handshake makes it moot for
subprocess plugins, whose config arrives from the loader rather than through
`ToolCtx::config`.

---

## 7. Startup cost and per-call overhead

**Confirmed: conway's plugins are long-lived processes, so per-call cost is
not a spawn.** Per call, the host pays:

- serializing a `ToolCall` (typically a few hundred bytes) — tens of
  microseconds;
- one pipe write and one pipe read — a couple of context switches,
  ~10–100 µs;
- deserializing the response.

Host-attributable overhead is **well under 1 ms per call**, dominated
entirely by the plugin's own work.

One spawn (5–50 ms) is paid once per plugin per session, at startup, and
connects run concurrently across plugins. Against the multi-hundred-millisecond
model round trip the runtime already awaits before dispatching any tool call
(for scale, this codebase's own capability probe budgets 5 s,
`crates/conway/src/builder.rs:126`), that is not a cost worth designing
around.

Contrast: a per-invocation-spawn model pays 5 spawns (25–250 ms) for a
5-tool batch. Conway pays zero, and `run_batch` dispatches up to
`max_parallel_tools` concurrently over one multiplexed connection
(`runner.rs:141`), so batch size does not multiply process cost at all.

**Consequence to record:** amortized startup plus sub-millisecond per-call
overhead is the *second, independent* reason to reject lazy spawning — the
first being `PluginRegistry`'s synchronous construction (§3.1).

---

## 8. Payload limits and P-10 posture

### Limits, decided deliberately

- **`MAX_FRAME_BYTES` = 4 MiB**, both directions, hard. Exceeded → connection
  poisoned → child killed and restarted; pending calls fail closed.
  Resyncing on `\n` after a truncated oversized frame would be exactly the
  frame-confusion a length-prefixed protocol exists to avoid, so conway does
  not attempt it.
- **Unparseable (but in-bounds) line** → log, count, drop. Graded, not fatal:
  a stray `println!` is the likeliest real plugin bug and killing the process
  for it is disproportionate. Threshold: 16 consecutive or 64 total malformed
  frames on a connection → poison. A dropped line that happened to be a
  response simply lets that call hit its deadline (§4).
- **Tool output is not capped by the transport.** It is capped by the
  mechanism that already exists: the tool declares a `TruncationPolicy`, and
  `runner.rs::apply_truncation` (`runner.rs:447`) enforces it and records a
  `TruncationRecord` in the log. A remote tool declares its policy on the
  wire like any other tool.
  **Explicitly rejected: Claude Code's 10 KB cap with out-of-band spill to a
  file the user fetches manually.** Conway's version is strictly better on
  three counts — the limit is per-tool-declared rather than global, the
  elision is inline and self-describing (`"… (N bytes omitted)"`,
  `runner.rs:533`), and the truncation is auditable in the session log.
  Genuinely large output has a typed escape hatch already:
  `ToolOutput::artifacts` (`content.rs:149-156`), not a path smuggled in a
  string.
- **Guard against a plugin declaring `TruncationPolicy::None` on a 4 MiB
  payload:** the host clamps a remote tool's declared policy against a
  configurable `max_output_bytes` (default 256 KiB) before handing
  `ToolOutput` back to the runner. Clamping only ever shrinks.
- **Notification rate:** a token bucket per connection (1000/s sustained).
  Excess is **dropped with a counter, not queued** — the same philosophy
  `EventSink` already commits to for slow consumers, surfacing as
  `Event::Lagged { skipped }` (`ports/events.rs:9-14`,
  `event.rs:198`) rather than a second, contradictory backpressure model.

### P-10 posture

- **No `unwrap`/`expect`/panic on any byte from the child.** Every decode is a
  `Result`; every unknown id, unknown method, or malformed frame is
  logged, counted, and dropped.
- The reader task is panic-wrapped so a bug in it poisons that connection
  rather than aborting the process — mirroring `runner.rs:180-215`.
- No child-supplied number ever sizes an allocation or indexes without a
  bound.
- Every child-originated string that can reach a terminal is control-character
  sanitized per `runner.rs:409`'s rule.
- The child never supplies identity (§2).

---

## 9. The v0.6.0 invariants this design must not break

### 9.1 The non-delegable root check stays above everything

The dispatch sequence is unchanged: `ToolRunner::execute_one` → registry
resolve → schema validate → `PermissionBroker::decide` (root check **first**,
`permission.rs:486`) → and only then `resolved.tool.invoke(...)`
(`runner.rs:360`). **A remote tool is an `Arc<dyn Tool>` sitting at that last
step.** The entire process boundary lives *inside* `RemoteTool::invoke`.
Therefore:

- No frame in this protocol can reach the broker, the gate, or the root check.
  No `params` field names an agent id, session id, cwd, or root.
- A plugin's declared `path_args` is an *input to* the check that can only
  make a call **more** gated, never less. `Named` is the only variant that
  can permit an auto-allow, and only after every named path is verified inside
  the root (`permission.rs:363-407`).
- **A remote tool that declares nothing must default to
  `Unconfinable { checkable: &[] }`** (`ports/plugin.rs:152-157`) — never
  `PathArgs::None`. Treating a plugin's silence as "no paths, therefore
  nothing to check" would unconfine it. This is the one place a malicious
  manifest could try to buy itself an auto-allow, and the only way it can is
  by naming its path arguments — which are then checked. Worth an explicit
  test.
- The transport installs no `PermissionGate`. Gates are injected by the
  embedder (`with_permission_gate`, `builder.rs:203`), never discovered from
  `plugins.json`. Whether a plugin may *be* a gate is D2/D4's question.

### 9.2 `Tool::path_args` is the worked example for D3

`path_args` was made declarative — argument *names*, not computed paths —
specifically so it survives an RPC boundary without a second round trip, and
its own doc comment says so: *"A method that inspected `args` to compute paths
would need an extra RPC round trip for an out-of-process plugin; a field-name
list survives the wire intact"* (`ports/plugin.rs:95-98`). That is the shape
D3 should reach for at every extension point: **static, declarative metadata
delivered once at handshake, not a callback per call.** Every callback D3 adds
is a round trip inside a tool call's critical path, and a place a wedged
plugin can stall the host.

---

## Open questions for a human or the researcher

1. **Where does the host crate live?** Recommended: a new workspace member
   `conway-plugin-host` (conway-core + tokio + serde_json + nix; zero new
   external dependencies), depended on by the `conway` facade and by nothing
   in `conway-core`/`conway-runtime`. Alternative: fold it into
   `conway-tools`, which already has the exact tokio/nix feature set.
2. **Lifting `kill_group`** out of `conway-tools`' private `unix` module to
   `pub` — needs sign-off, since the alternative (a documented verbatim copy)
   is worse for a security-relevant routine.
3. **`PathArgs`' `'static` lifetime.** `Box::leak` at connect (recommended,
   D1-local) versus widening `PathArgs` to `Cow<'static, _>` (a breaking
   conway-core change, and D3's call).
4. **Windows.** Everything in §3.4 is unix. Process-group kill and
   `PR_SET_PDEATHSIG` have no direct equivalents; job objects do. `bash.rs`
   already ships a `#[cfg(not(unix))]` "requires a unix host" degradation
   (`bash.rs:117-121`). Is unix-only acceptable for v1 of the plugin host?
5. **`Plugin::on_init` has zero call sites.** Should this design also wire it
   up, or leave the dead contract alone and note it? D1 does not depend on it
   either way.
6. **Should the `initialize` handshake carry the host's protocol version and
   negotiate down**, or refuse on mismatch? Refusing is simpler and fails
   closed; negotiating is friendlier to a plugin ecosystem that will outlive
   any single conway version. Recommend refuse-on-major-mismatch,
   accept-on-minor.
