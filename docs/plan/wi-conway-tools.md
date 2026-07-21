## Size Assessment

**Right size — 7 work items (WI-061…WI-067).** The module has four independent plugin families plus a shared harness and an aggregator. No sub-module split is warranted; `SubagentPlugin` is a thin wrapper, not a sub-system.

**Assumptions stated up front (all flagged to `MODULE:conway-core`):**
1. `TruncationPolicy` is assumed to have variants `None`, `Head { max_bytes: usize }`, `Tail { max_bytes: usize }`, `HeadTail { max_bytes: usize }`. If conway-core names them differently, use conway-core's names with the same semantics; do not add variants from this crate.
2. `EventSink` is assumed to expose `fn emit(&self, event: Event)` (or an equivalent single-method emit) usable from a non-async context. Progress is reported as `Event::ToolProgress { call_id, note }`.
3. `ToolError` is assumed to have at least `Cancelled`, `Io`, `InvalidArguments`, `Host`. Map per the notes; if a variant is missing, use the closest existing one and do not add ad-hoc string errors.
4. `ContentBlock::Text(String)` exists. All tool output in this module is text blocks only.
5. conway-core does **not** ship a `FakeSubagentHost`; WI-061 defines one locally.

---

# WI-061: conway-tools crate scaffold, shared tool helpers, and test harness

**complexity:** Medium

**scope:**
- `crates/conway-tools/Cargo.toml` (create)
- `crates/conway-tools/src/lib.rs` (create)
- `crates/conway-tools/src/common.rs` (create)
- `crates/conway-tools/src/registry.rs` (create)
- `crates/conway-tools/src/testing.rs` (create)
- `crates/conway-tools/src/fs/mod.rs` (create)
- `crates/conway-tools/src/shell/mod.rs` (create)
- `crates/conway-tools/src/subagent/mod.rs` (create)
- `crates/conway-tools/src/report/mod.rs` (create)
- `crates/conway-tools/tests/common_helpers.rs` (create)

**depends:** MODULE:conway-core

**criteria:**
- [machine] `cargo build -p conway-tools` succeeds; `cargo tree -p conway-tools` contains no `conway-runtime`, `conway-session`, `conway-routing`, or `conway-backends` entry.
- [machine] `crates/conway-tools/src/lib.rs` declares `pub mod common; pub mod fs; pub mod shell; pub mod subagent; pub mod report; mod registry;` and re-exports `pub use registry::builtin_plugins;`.
- [machine] `common::resolve_path(&ToolCtx, &str) -> Result<PathBuf, ToolError>` exists; unit test: relative input `"a/b"` with `ctx.cwd = /tmp/x` yields `/tmp/x/a/b`; absolute input `/etc/hosts` is returned unchanged; input containing a NUL byte yields `Err(ToolError::InvalidArguments)`.
- [machine] `common::parse_args::<T: DeserializeOwned>(&ToolCall) -> Result<T, ToolError>` exists; unit test: malformed arguments yield `Err(ToolError::InvalidArguments)` whose message contains the serde error text.
- [machine] `common::text_output(String, TruncationPolicy) -> ToolOutput` and `common::error_text(String) -> ToolOutput` exist; unit test asserts `error_text` sets `is_error == true`, one `ContentBlock::Text`, `truncation == TruncationPolicy::None`, and empty `artifacts`.
- [machine] `common::check_cancel(&ToolCtx) -> Result<(), ToolError>` returns `Err(ToolError::Cancelled)` when the token is cancelled; unit test covers both branches.
- [machine] `registry::builtin_plugins() -> Vec<Arc<dyn Plugin>>` exists and compiles (returns an empty vec at this stage; populated by WI-067).
- [machine] `testing::FakeSubagentHost` implements `conway_core::SubagentHost`; unit test asserts a `start` call records the `SubagentSpec` and returns the scripted `AgentId`, and `await_result` returns the scripted `AgentResult`.
- [machine] `testing::RecordingEventSink` implements `EventSink` and exposes `events() -> Vec<Event>`; `testing::test_ctx(cwd: PathBuf) -> (ToolCtx, TestHandles)` builds a `ToolCtx` wired to `FakeSubagentHost` + `RecordingEventSink` + a fresh `CancellationToken`.
- [machine] `tests/common_helpers.rs` passes and exercises `resolve_path`, `parse_args`, `check_cancel` against a `tempfile::TempDir`.

**notes:**

**Objective:** Establish the crate, its dependency boundary, the helper layer every tool uses, and the in-crate test doubles that let every subsequent work item be unit-tested with zero runtime.

**Implementation Notes:**
- `Cargo.toml` dependencies: `conway-core` (path), `async-trait`, `serde`, `serde_json`, `schemars`, `tokio` (features `fs`, `process`, `io-util`, `macros`, `rt`, `time`, `sync`), `thiserror`, `regex`, `ignore`, `globset`, `nix` (unix, features `signal`, `process`). Dev-deps: `tempfile`, `tokio` (`rt-multi-thread`, `test-util`). Feature `test-fakes` gates `testing` for external use; `testing` is also compiled under `cfg(test)`.
- Error discipline, applied crate-wide: **model-recoverable** conditions (file not found, no regex match, non-zero exit code, ambiguous edit) return `Ok(ToolOutput { is_error: true, .. })` so the model can adapt. **Host/infrastructure** conditions (cancellation, permission-denied by the OS, spawn failure, unreachable `SubagentHost`) return `Err(ToolError::…)`. Every subsequent work item follows this rule; do not deviate.
- `resolve_path` performs **no** containment or escape checks (GP-08: no sandboxing here). It joins relative paths onto `ctx.cwd`, rejects NUL bytes, and returns. It does not canonicalize (canonicalizing would fail for not-yet-created files).
- Module root files `fs/mod.rs`, `shell/mod.rs`, `subagent/mod.rs`, `report/mod.rs` are created as compiling stubs containing only a doc comment; later work items add submodule declarations and plugin types to them.
- `FakeSubagentHost` fields: `Mutex<Vec<(AgentId, SubagentSpec)>> started`, `Mutex<Vec<(AgentId,String)>> steers`, `Mutex<Vec<(AgentId,String)>> cancels`, and script maps `next_agent_id: AgentId`, `results: HashMap<AgentId, AgentResult>`. `await_result` on an unknown id returns `Err(RuntimeError::…)`. Provide `FakeSubagentHost::with_result(agent_id, AgentResult)` builder. It contains no fork logic — it is a recorder.
- `TestHandles` exposes `Arc<FakeSubagentHost>`, `Arc<RecordingEventSink>`, and the `CancellationToken` so tests can cancel mid-invoke.

---

# WI-062: FsPlugin core file tools — read, write, edit

**complexity:** High

**scope:**
- `crates/conway-tools/src/fs/read.rs` (create)
- `crates/conway-tools/src/fs/write.rs` (create)
- `crates/conway-tools/src/fs/edit.rs` (create)
- `crates/conway-tools/tests/fs_core.rs` (create)

**depends:** 061

**criteria:**
- [machine] `ReadTool`, `WriteTool`, `EditTool` each implement `conway_core::Tool`; `spec().name` is `"read"`, `"write"`, `"edit"` respectively.
- [machine] `spec().category` is `ToolCategory::Read` for `read` and `ToolCategory::Edit` for `write` and `edit`.
- [machine] Each `spec().schema` is the JSON Schema below verbatim in shape (required fields, types, defaults); a test deserializes each schema and asserts the `required` array and property names.
- [machine] read: on a 5-line temp file, output is exactly `     1\t<line1>\n…     5\t<line5>` (line number right-aligned to width 6, then a TAB); `offset: 3, limit: 2` yields only lines 3 and 4 with their original numbers.
- [machine] read: nonexistent path → `Ok(ToolOutput { is_error: true })` whose text contains the path; a file whose first 8192 bytes contain `0x00` → `is_error: true` with text `"binary file; not read"`.
- [machine] read: `spec()` output declares `TruncationPolicy::Head { max_bytes: 65_536 }` on the returned `ToolOutput`.
- [machine] write: writing to `dir/does/not/exist/f.txt` creates parent dirs and the file; a second write replaces content; output text is `wrote <N> bytes to <abs path>`; `is_error == false`.
- [machine] write: the target file is never observed partially written — test asserts a temp sibling file matching `.*.conway.tmp` does not remain after success.
- [machine] edit: `old_string` occurring exactly once is replaced and every other byte of the file is unchanged (test compares full file bytes).
- [machine] edit: zero occurrences → `is_error: true`, text contains `"old_string not found"`; two occurrences with `replace_all` absent/false → `is_error: true`, text contains `"found 2 occurrences"`; two occurrences with `replace_all: true` → both replaced, output text `edited <path>: 2 replacement(s)`.
- [machine] edit: `old_string == new_string` → `is_error: true`, text contains `"old_string and new_string are identical"`.
- [machine] Cancellation: a pre-cancelled `ToolCtx` makes each of the three tools return `Err(ToolError::Cancelled)` without touching the filesystem (test asserts target file is not created).

**notes:**

**Objective:** Implement the three mutating/reading file tools of `FsPlugin` with fully determined schemas, output formats, and error semantics.

**Implementation Notes:**

Schemas (JSON Schema draft 2020-12, generated with `schemars` from a `#[derive(Deserialize, JsonSchema)]` args struct per tool):

```jsonc
// read
{ "type":"object",
  "properties": {
    "path":   {"type":"string", "description":"File path, absolute or relative to cwd"},
    "offset": {"type":"integer","minimum":1,"description":"1-based first line to read"},
    "limit":  {"type":"integer","minimum":1,"description":"Max lines to read; default 2000"}
  }, "required":["path"], "additionalProperties": false }

// write
{ "type":"object",
  "properties": {
    "path":    {"type":"string"},
    "content": {"type":"string"}
  }, "required":["path","content"], "additionalProperties": false }

// edit
{ "type":"object",
  "properties": {
    "path":        {"type":"string"},
    "old_string":  {"type":"string"},
    "new_string":  {"type":"string"},
    "replace_all": {"type":"boolean","default":false}
  }, "required":["path","old_string","new_string"], "additionalProperties": false }
```

- **read**: `tokio::fs::read` the whole file; binary sniff on the first 8192 bytes (any `0x00` ⇒ binary). Decode as UTF-8 lossy. Apply `offset` (default 1) and `limit` (default 2000). Format each line as `format!("{:>6}\t{}", n, line)` where `n` is the absolute 1-based line number. If lines remain after `limit`, append a final line `… (<K> more lines; use offset/limit)`. Empty file ⇒ text `(empty file)`, `is_error: false`. Returned `truncation: TruncationPolicy::Head { max_bytes: 65_536 }`.
- **write**: `create_dir_all(parent)`, write to `parent/.{filename}.conway.tmp`, `flush`, `sync_all`, then `tokio::fs::rename` over the target (atomic on the same filesystem). On any error after temp creation, remove the temp file before returning. `truncation: TruncationPolicy::None`.
- **edit**: read file as UTF-8 (invalid UTF-8 ⇒ `is_error: true`, `"file is not valid UTF-8"`). Match semantics are **literal byte-exact substring matching**, never regex, never whitespace-normalized. Count with `matches(old).count()`. Rules in order: identical strings ⇒ error; count == 0 ⇒ error listing the path; count > 1 && !replace_all ⇒ error text `found 2 occurrences of old_string in <path>; add surrounding context to make it unique, or set replace_all: true`; else `replace` (or `replacen(old,new,1)`). Write back via the same atomic temp+rename routine as `write` (factor it into `fs::write::atomic_write(path, &str)` and call it from `edit`). `truncation: TruncationPolicy::None`.
- Cancellation check via `common::check_cancel` at the start of `invoke` and, for `read`, again after the file read completes.
- Rendered permission one-liner (`ToolSpec` rendering field, if present in core): `read <path>`, `write <path> (<N> bytes)`, `edit <path>`.

---

# WI-063: FsPlugin search tools — glob, grep — and the FsPlugin assembly

**complexity:** High

**scope:**
- `crates/conway-tools/src/fs/glob.rs` (create)
- `crates/conway-tools/src/fs/grep.rs` (create)
- `crates/conway-tools/src/fs/mod.rs` (modify)
- `crates/conway-tools/tests/fs_search.rs` (create)

**depends:** 062

**criteria:**
- [machine] `GlobTool` and `GrepTool` implement `Tool`; `spec().name` is `"glob"` / `"grep"`; both declare `ToolCategory::Search`.
- [machine] `FsPlugin` implements `conway_core::Plugin`; `FsPlugin::new()` exists; `manifest().id == "conway.fs"`; `tools()` returns exactly 5 tools whose names, sorted, are `["edit","glob","grep","read","write"]`.
- [machine] `fs/mod.rs` declares `pub mod read; pub mod write; pub mod edit; pub mod glob; pub mod grep;` and re-exports the five tool types plus `FsPlugin`.
- [machine] glob: in a temp tree containing `a.rs`, `sub/b.rs`, `sub/c.txt`, pattern `**/*.rs` returns exactly `a.rs` and `sub/b.rs` as paths relative to the search root, one per line.
- [machine] glob: files under `.git/` and paths matched by a root `.gitignore` are excluded (test creates `.gitignore` with `target/` and a `target/x.rs` file; result omits it).
- [machine] glob: results are ordered by file mtime descending, ties broken by lexicographic path (test sets mtimes explicitly).
- [machine] glob: zero matches → `is_error: false` with text `no files matched <pattern>`; more than `limit` matches → the first `limit` lines plus a final `… (<K> more matches)` line.
- [machine] grep: pattern `fn \w+` over a temp tree returns lines formatted `<relative path>:<1-based line>:<line text>`; results grouped by path, paths in the same order as the glob walk order.
- [machine] grep: `case_insensitive: true` matches differing case; invalid regex → `is_error: true` with text containing the regex parse error; binary files (NUL in first 8192 bytes) are skipped silently.
- [machine] grep: `glob` argument filters candidate files (test asserts `*.rs` excludes a matching `.txt` file); zero matches → `is_error: false`, text `no matches for <pattern>`.
- [machine] Cancellation: with a token cancelled after the walk starts, both tools return `Err(ToolError::Cancelled)` (test uses a tree of ≥200 files and cancels from another task; assert the error variant).

**notes:**

**Objective:** Implement the two search tools and assemble the complete `FsPlugin`.

**Implementation Notes:**

Schemas:

```jsonc
// glob
{ "type":"object",
  "properties":{
    "pattern":{"type":"string","description":"Glob pattern, e.g. **/*.rs"},
    "path":{"type":"string","description":"Search root; default cwd"},
    "limit":{"type":"integer","minimum":1,"default":1000}
  },"required":["pattern"],"additionalProperties":false }

// grep
{ "type":"object",
  "properties":{
    "pattern":{"type":"string","description":"Rust regex"},
    "path":{"type":"string","description":"Search root; default cwd"},
    "glob":{"type":"string","description":"Only search files matching this glob"},
    "case_insensitive":{"type":"boolean","default":false},
    "max_results":{"type":"integer","minimum":1,"default":200}
  },"required":["pattern"],"additionalProperties":false }
```

- Both tools walk with `ignore::WalkBuilder::new(root).hidden(false).git_ignore(true).git_global(false).parents(false)` and an explicit filter dropping any path component equal to `.git`. Walk is single-threaded (`build()` not `build_parallel()`) so ordering is deterministic; the walk runs inside `tokio::task::spawn_blocking` and the blocking closure polls a cloned `CancellationToken` every 64 entries, aborting with `ToolError::Cancelled`.
- glob matching: `globset::GlobBuilder::new(pattern).literal_separator(true).build()` matched against the **path relative to the search root**. Collect `(relative_path, mtime)`, sort by `mtime` descending then `relative_path` ascending, truncate to `limit`.
- grep: compile `regex::RegexBuilder::new(pattern).case_insensitive(flag).build()`. For each candidate file: skip if the first 8192 bytes contain `0x00`; read to string lossily; emit one output line per matching line. Stop after `max_results` matches and append `… (result limit <max_results> reached)`. No context lines in MVP; do not add a `context` argument.
- Both return `truncation: TruncationPolicy::Head { max_bytes: 32_768 }`.
- `FsPlugin::manifest()` — `PluginManifest { id: "conway.fs", version: env!("CARGO_PKG_VERSION"), tools: [names], required_host_caps: [] }`. `on_init` uses the default (no-op).
- `PermissionClass` per tool: `read`/`glob`/`grep` are the read class; `write`/`edit` are the mutate class. Use whatever conway-core names these; do not invent a class.

---

# WI-064: ShellPlugin — bash with streaming output and process-group cancellation

**complexity:** High

**scope:**
- `crates/conway-tools/src/shell/bash.rs` (create)
- `crates/conway-tools/src/shell/mod.rs` (modify)
- `crates/conway-tools/tests/shell_bash.rs` (create)

**depends:** 061

**criteria:**
- [machine] `BashTool` implements `Tool`; `spec().name == "bash"`; `spec().category == ToolCategory::Execute`; `ShellPlugin` implements `Plugin` with `manifest().id == "conway.shell"` and `tools()` returning exactly one tool named `"bash"`.
- [machine] `echo hi` yields `is_error: false` and output text containing `hi` and `exit code: 0`.
- [machine] `sh -c 'echo out; echo err 1>&2; exit 3'`-equivalent command yields `is_error: true`, text containing both `out` (under a `stdout:` section) and `err` (under a `stderr:` section) and `exit code: 3`.
- [machine] Returned `ToolOutput.truncation == TruncationPolicy::HeadTail { max_bytes: 30_000 }`.
- [machine] Streaming: running `printf 'a\nb\nc\n'` causes the `RecordingEventSink` to contain at least 3 `Event::ToolProgress` events whose `note` values are `a`, `b`, `c` in order, each carrying the invoking `call_id`.
- [machine] Process-group kill on cancel: command `bash -c 'sleep 300 & sleep 300'` is cancelled via `ctx.cancel` after 200 ms; `invoke` returns `Err(ToolError::Cancelled)` within 3 s, and a post-test assertion using `kill(-pgid, 0)` (or `nix::sys::signal::kill(Pid::from_raw(-pgid), None)`) reports `ESRCH`, proving the whole group — including the backgrounded child — is dead.
- [machine] Timeout: `timeout_ms: 300` with `sleep 5` returns `Ok(is_error: true)` within 3 s, text containing `timed out after 300ms`, and the process group is dead by the same `kill(-pgid, 0)` check.
- [machine] `cwd` argument overrides `ctx.cwd`; test asserts `pwd` output equals the supplied directory. Absent `cwd`, the process runs in `ctx.cwd`.
- [machine] Non-unix: the file gates the process-group logic behind `#[cfg(unix)]`; a `#[cfg(not(unix))]` `invoke` returns `Ok(is_error: true)` with text `bash tool requires a unix host`. `cargo check` passes on the host target.

**notes:**

**Objective:** Implement the single `bash` tool: streamed, cancellable, process-group-killing command execution.

**Implementation Notes:**

Schema:

```jsonc
{ "type":"object",
  "properties":{
    "command":{"type":"string","description":"Shell command executed with bash -c"},
    "timeout_ms":{"type":"integer","minimum":1,"default":120000},
    "cwd":{"type":"string","description":"Working directory; default the agent cwd"}
  },"required":["command"],"additionalProperties":false }
```

- Spawn: `tokio::process::Command::new("/bin/bash").arg("-c").arg(&command).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())` plus `std::os::unix::process::CommandExt::process_group(0)` so the child becomes its own process-group leader. Record `pgid = child.id()`.
- Reading: take `stdout`/`stderr`, wrap each in `tokio::io::BufReader::new(..).lines()`, and drive both plus `child.wait()` in a `tokio::select!` loop. Each completed line is (a) pushed to the corresponding accumulator `Vec<String>` and (b) emitted immediately as `Event::ToolProgress { call_id: call.id.clone(), note: line.clone() }` through `ctx.events`. Do not buffer output before emitting — line-at-a-time emission is the contract tested above. A final partial line without a trailing newline is included in the accumulator and emitted.
- Termination paths, all funnelled through one `kill_group(pgid)` helper: send `SIGTERM` to `-pgid` via `nix::sys::signal::kill(Pid::from_raw(-(pgid as i32)), Signal::SIGTERM)`, wait up to 2 s for `child.wait()`, then send `SIGKILL` to `-pgid` and `wait()` again. Always reap the child; never leave a zombie.
  - `ctx.cancel` cancelled ⇒ `kill_group`, then `Err(ToolError::Cancelled)`. Output already streamed as events is not re-returned.
  - `timeout_ms` elapsed (`tokio::time::timeout` around the whole loop) ⇒ `kill_group`, then `Ok(ToolOutput { is_error: true, .. })` containing the partial output collected so far plus `timed out after <N>ms`.
- Output text format, exactly:
  ```
  stdout:
  <stdout lines, verbatim, or "(empty)">

  stderr:
  <stderr lines, verbatim, or "(empty)">

  exit code: <N>
  ```
  `is_error = exit_code != 0`. A signal-terminated child reports `exit code: signal <N>` and `is_error: true`.
- `truncation: TruncationPolicy::HeadTail { max_bytes: 30_000 }` — the tool does **not** truncate its own output; it declares the policy and the runtime enforces and records it.
- No sandboxing, no command allow/deny list, no argument sanitization (GP-08; the `PermissionGate` is the control point). Rendered one-liner: the first 120 chars of `command`.

---

# WI-065: ReportPlugin — explicit AgentResult finalization

**complexity:** Low

**scope:**
- `crates/conway-tools/src/report/report_tool.rs` (create)
- `crates/conway-tools/src/report/mod.rs` (modify)
- `crates/conway-tools/tests/report.rs` (create)

**depends:** 061

**criteria:**
- [machine] `ReportTool` implements `Tool`; `spec().name == "report"`; `spec().category == ToolCategory::Think`; `ReportPlugin` implements `Plugin` with `manifest().id == "conway.report"` and exactly one tool.
- [machine] A valid call produces `is_error: false` and a single `ContentBlock::Text` whose contents parse as JSON with top-level key `conway_report` containing keys `version` (== 1), `summary`, `facts`, `artifacts`, `structured`.
- [machine] `facts` and `artifacts` in the emitted JSON round-trip to `Vec<conway_core::Fact>` / `Vec<conway_core::Artifact>` via `serde_json::from_value` (test asserts deserialization succeeds).
- [machine] Parsed `Artifact`s are also placed on `ToolOutput.artifacts`; test asserts `output.artifacts.len()` equals the number supplied.
- [machine] `summary` longer than 2000 characters → `Ok(is_error: true)` with text containing `summary exceeds 2000 characters`; nothing is emitted on `artifacts`.
- [machine] Omitted `facts`/`artifacts`/`structured` default to `[]`, `[]`, `null` respectively; test asserts the emitted JSON contains those defaults.
- [machine] `ToolOutput.truncation == TruncationPolicy::None`.
- [machine] The module contains no reference to `SessionStore`, session logging, or `AgentResult` construction — verified by a test asserting `include_str!("report_tool.rs")` contains neither `"SessionStore"` nor `"AgentResult {"`.

**notes:**

**Objective:** Give an agent a tool to explicitly declare its terminal result instead of the runtime inferring one from trailing text.

**Implementation Notes:**

Schema:

```jsonc
{ "type":"object",
  "properties":{
    "summary":{"type":"string","maxLength":2000,
               "description":"Required, bounded terminal summary of this agent's work"},
    "facts":{"type":"array","items":{"type":"object",
             "properties":{"key":{"type":"string"},"value":{"type":"string"},
                           "source":{"type":"string"}},
             "required":["key","value"]},"default":[]},
    "artifacts":{"type":"array","items":{"type":"object",
             "properties":{"kind":{"type":"string"},"path":{"type":"string"},
                           "value":{"type":"string"}},
             "required":["kind"]},"default":[]},
    "structured":{"description":"Free-form JSON validated by the runtime against result_contract"}
  },"required":["summary"],"additionalProperties":false }
```

- The tool **does not** construct an `AgentResult` and **does not** write the session log. It emits a canonical, versioned JSON envelope; the runtime recognizes the `report` tool by name and lifts the payload into the agent's `AgentResult`. Envelope:
  ```json
  {"conway_report":{"version":1,"summary":"…","facts":[…],"artifacts":[…],"structured":null}}
  ```
  Serialize with `serde_json::to_string` (compact, not pretty) so the runtime parse is exact.
- `facts`/`artifacts` items are deserialized into `conway_core::Fact` / `conway_core::Artifact` during argument parsing; a shape mismatch is `Err(ToolError::InvalidArguments)` (host-level: the schema was violated), while the length check on `summary` is model-recoverable and returns `is_error: true`.
- `structured` is passed through untouched; this tool performs no schema validation against `result_contract` — that is the runtime's job.
- Honor cancellation with a `common::check_cancel` at entry, for uniformity, though the tool does no I/O.

---

# WI-066: SubagentPlugin — conway_subagent / steer / await / cancel

**complexity:** Medium

**scope:**
- `crates/conway-tools/src/subagent/tools.rs` (create)
- `crates/conway-tools/src/subagent/mod.rs` (modify)
- `crates/conway-tools/tests/subagent.rs` (create)

**depends:** 061, MODULE:conway-core providing `SubagentHost` + `SubagentSpec`

**criteria:**
- [machine] Four tools implement `Tool` with names `"conway_subagent"`, `"conway_steer"`, `"conway_await"`, `"conway_cancel"`; all declare `ToolCategory::Delegate`. `SubagentPlugin` implements `Plugin`, `manifest().id == "conway.subagent"`, `tools()` returns exactly those four.
- [machine] **No fork/spawn logic.** Test asserts `include_str!("tools.rs")` contains none of: `"SessionStore"`, `"TranscriptResolver"`, `"ContextBuilder"`, `"conway_runtime"`, `"fork("`. Test asserts `tools.rs` is under 400 lines.
- [machine] `conway_subagent{mode:"fork", prompt:"p"}` against `FakeSubagentHost` results in exactly one recorded `start(parent = ctx.agent_id, spec)` where `spec.mode == SubagentMode::Fork`, `spec.prompt == "p"`, `spec.cache_hint == true`.
- [machine] `mode:"spawn"` with `agent_def` recorded as `SubagentMode::Spawn`, `cache_hint == false`; `mode:"spawn"` **without** `agent_def` → `Ok(is_error: true)` with text `agent_def is required for mode "spawn"`, and `FakeSubagentHost` records zero `start` calls.
- [machine] `mode` value other than `"fork"`/`"spawn"` → `Err(ToolError::InvalidArguments)`; zero `start` calls.
- [machine] `await` defaulting: with `await` omitted, the tool calls `await_result` and returns the scripted `AgentResult` serialized as JSON text; test asserts the text parses and `agent_id` matches.
- [machine] `await: false` returns immediately with text parsing to `{"agent_id":"<id>"}` and `is_error: false`; `FakeSubagentHost` records zero `await_result` calls.
- [machine] `is_error` mapping from `ResultStatus`: `Completed` → false; `Failed`, `Rejected`, `Cancelled`, `BudgetExceeded` → true. One test case per variant.
- [machine] Budget defaults: with `budget` omitted, the recorded `spec.budget` equals `Budget { max_steps: 40, deadline: 10 min, max_tokens: None }` unless overridden by `ctx.config` keys `subagent.max_steps` / `subagent.deadline_secs` / `subagent.max_tokens`; test covers both the default and a config override.
- [machine] Cancellation while awaiting: cancelling `ctx.cancel` during a blocked `await_result` causes `invoke` to return `Err(ToolError::Cancelled)` **and** `FakeSubagentHost` to record one `cancel(child_id, _)` call.
- [machine] `conway_steer{agent_id,text}` calls `SubagentHost::steer` once with those exact values and returns `is_error: false`; `conway_cancel{agent_id, reason?}` calls `cancel` with the supplied reason or the default `"cancelled by parent agent"`; `conway_await{agent_id}` calls `await_result` and applies the same serialization + `is_error` mapping.
- [machine] A `RuntimeError` from any `SubagentHost` method surfaces as `Err(ToolError::Host)` carrying the underlying message (not as `is_error: true`).

**notes:**

**Objective:** Expose the delegation primitives to the model as a pure wrapper over `ToolCtx::subagents`. This work item is the mechanical enforcement of the API-first decision: the tool layer holds zero delegation logic.

**Implementation Notes:**

Schemas:

```jsonc
// conway_subagent
{ "type":"object",
  "properties":{
    "mode":{"enum":["fork","spawn"]},
    "prompt":{"type":"string","description":"Fork: the fork directive. Spawn: the whole task."},
    "agent_def":{"type":"string","description":"Agent definition name; required when mode=spawn"},
    "role":{"type":"string","description":"Role alias for routing"},
    "budget":{"type":"object","properties":{
        "max_steps":{"type":"integer","minimum":1},
        "deadline_secs":{"type":"integer","minimum":1},
        "max_tokens":{"type":"integer","minimum":1}},"additionalProperties":false},
    "tools":{"type":"array","items":{"type":"string"},
             "description":"Restrict the child's tool set to these names"},
    "result_contract":{"type":"object","description":"JSON Schema the child's structured result must satisfy"},
    "await":{"type":"boolean","default":true,
             "description":"false returns the agent_id immediately for fan-out"}
  },"required":["mode","prompt"],"additionalProperties":false }

// conway_steer   {"agent_id":string, "text":string}            required: both
// conway_await   {"agent_id":string}                            required: agent_id
// conway_cancel  {"agent_id":string, "reason":string?}          required: agent_id
```

- Body of `conway_subagent::invoke`, in full:
  1. `check_cancel`; `parse_args`.
  2. Validate `mode == Spawn ⇒ agent_def.is_some()` (model-recoverable error).
  3. Build `SubagentSpec { mode, prompt, agent_def, role, tools: tools.map(ToolSelector::Names), budget, cache_hint: mode == Fork, result_contract }`.
  4. `let child = ctx.subagents.start(ctx.agent_id.clone(), spec).await.map_err(ToolError::Host)?;`
  5. If `!await_flag` ⇒ return `text_output(json!({"agent_id": child}))`.
  6. Else `tokio::select!` on `ctx.subagents.await_result(child)` vs `ctx.cancel.cancelled()`. On cancel: `ctx.subagents.cancel(child, "parent tool cancelled")` (ignore its error), return `Err(ToolError::Cancelled)`.
  7. Serialize the `AgentResult` with `serde_json::to_string`, set `is_error` by the status mapping, return.
- `truncation` for all four tools: `TruncationPolicy::Tail { max_bytes: 16_384 }` — when a child's serialized result is oversized, the tail (summary/facts/status) is the part that must survive. Applied by the runtime.
- `AgentId` parsing from the string arguments uses `AgentId::from_str`; a malformed id is `Err(ToolError::InvalidArguments)`.
- Do not implement fan-out aggregation, tournaments, or retry here. The composite pattern (N `await:false` spawns → N `conway_await` → one `report`) is a model-level behavior with no supporting code in this crate.
- `agent_def` is passed through as `AgentDefRef` by name only; this crate never loads or resolves agent definitions.

---

# WI-067: builtin_plugins() registry and cross-plugin integration tests

**complexity:** Low

**scope:**
- `crates/conway-tools/src/registry.rs` (modify)
- `crates/conway-tools/tests/builtins.rs` (create)

**depends:** 063, 064, 065, 066

**criteria:**
- [machine] `builtin_plugins() -> Vec<Arc<dyn Plugin>>` returns exactly four plugins with `manifest().id` values, sorted: `["conway.fs","conway.report","conway.shell","conway.subagent"]`.
- [machine] The union of `tools()` across all returned plugins has exactly 11 entries whose names, sorted, are `["bash","conway_await","conway_cancel","conway_steer","conway_subagent","edit","glob","grep","read","report","write"]`.
- [machine] No two tools across all built-in plugins share a name (test asserts the name set length equals the tool count).
- [machine] Every tool's `spec().schema` is valid JSON Schema — test asserts each schema deserializes as a `serde_json::Value` object containing `"type": "object"` and a `"properties"` object.
- [machine] Every tool's `spec().description` is non-empty and ≤ 1024 characters.
- [machine] Cancellation conformance: for every built-in tool, invoking with a pre-cancelled `ToolCtx` and minimal valid arguments returns `Err(ToolError::Cancelled)` — a single table-driven test covering all 11 tools.
- [machine] Truncation conformance: for every built-in tool, a successful invocation returns a `ToolOutput` whose `truncation` matches the policy declared in this crate's documentation table (test table asserts the exact variant per tool).
- [machine] `cargo test -p conway-tools` passes; `cargo tree -p conway-tools` contains no `conway-runtime`.

**notes:**

**Objective:** Assemble the four built-in plugins into the single registration entry point the facade consumes, and enforce the crate-wide conformance rules with one test suite.

**Implementation Notes:**
- `registry.rs`:
  ```rust
  pub fn builtin_plugins() -> Vec<Arc<dyn Plugin>> {
      vec![
          Arc::new(crate::fs::FsPlugin::new()),
          Arc::new(crate::shell::ShellPlugin::new()),
          Arc::new(crate::subagent::SubagentPlugin::new()),
          Arc::new(crate::report::ReportPlugin::new()),
      ]
  }
  ```
  Order in the vec is registration order, not sorted; the sorted assertion is on ids.
- The truncation conformance table (authoritative for this crate):

  | tool | policy |
  |---|---|
  | read | `Head { max_bytes: 65_536 }` |
  | write, edit, report | `None` |
  | glob, grep | `Head { max_bytes: 32_768 }` |
  | bash | `HeadTail { max_bytes: 30_000 }` |
  | conway_subagent, conway_steer, conway_await, conway_cancel | `Tail { max_bytes: 16_384 }` |

- The cancellation conformance test builds each tool's minimal valid arguments from a fixture table (temp dir paths for fs tools, `true` for bash, a `FakeSubagentHost`-known agent id for subagent tools) and asserts the `Cancelled` variant. This is the machine check for the boundary rule "every tool honors `ToolCtx::cancel` cooperatively."
- No plugin is privileged: `builtin_plugins` returns plain `Arc<dyn Plugin>` values with no side channel to the runtime. If a future built-in needs a capability, it is added to `ToolCtx` in conway-core, not here.

---

## Coverage Statement

**Module:** conway-tools
**Work items:** WI-061, WI-062, WI-063, WI-064, WI-065, WI-066, WI-067

**Coverage:** These seven work items collectively implement 100% of the conway-tools scope: the four built-in plugins (`fs`, `shell`, `subagent`, `report`), the shared helper layer, the in-crate test doubles, and the `builtin_plugins()` registration entry point, with tests in scope throughout. Nothing in the module scope is excluded. Explicitly *not* implemented here, consistent with the module's "not responsible for" clause: permission decisions (tools only declare `PermissionClass`/`ToolCategory`), sandboxing or worktree isolation (GP-08), MCP, session-log writing, and `AgentResult` construction.

**Provides implemented by:**
| Provides | Work item(s) |
|---|---|
| `FsPlugin` — `read`, `write`, `edit` | WI-062 |
| `FsPlugin` — `glob`, `grep`, plugin assembly (`Plugin` impl, manifest, 5 tools) | WI-063 |
| `ShellPlugin` — `bash`, `ToolCategory::Execute`, `ToolCtx::events` streaming, `TruncationPolicy::HeadTail` | WI-064 |
| `SubagentPlugin` — `conway_subagent`, `conway_steer`, `conway_await`, `conway_cancel`, `ToolCategory::Delegate`, pure `SubagentHost` wrapper | WI-066 |
| `ReportPlugin` — `report{summary, facts, artifacts, structured}` | WI-065 |
| `builtin_plugins() -> Vec<Arc<dyn Plugin>>` | WI-061 (signature + stub), WI-067 (populated + conformance tests) |

**Requires consumed by:**
| Requires (`MODULE:conway-core`) | Work item(s) |
|---|---|
| `Plugin`, `PluginManifest` | WI-061 (registry signature), WI-063, WI-064, WI-065, WI-066, WI-067 |
| `Tool`, `ToolSpec`, `ToolCall`, `ToolCategory`, `PermissionClass` | WI-062, WI-063, WI-064, WI-065, WI-066 |
| `ToolCtx` (`cwd`, `cancel`, `events`, `subagents`, `config`, `agent_id`) | WI-061 (helpers + test ctx), consumed by WI-062, WI-063, WI-064, WI-065, WI-066 |
| `ToolOutput`, `ContentBlock`, `Artifact`, `TruncationPolicy` | WI-061, WI-062, WI-063, WI-064, WI-065, WI-066, WI-067 |
| `ToolError` | WI-061 (mapping policy), all tool items |
| `EventSink`, `Event::ToolProgress` | WI-061 (`RecordingEventSink`), WI-064 |
| `SubagentHost`, `SubagentSpec`, `SubagentMode`, `Budget`, `ToolSelector`, `AgentResult`, `ResultStatus`, `AgentId` | WI-061 (`FakeSubagentHost`), WI-066 |
| `Fact`, `Artifact` | WI-065 |

**Group assignment (§9):** WI-061, WI-062, WI-063, WI-064 are Group 1 track D (required by Slice 1). WI-065 may land with Group 1 (no runtime dependency). WI-066 and WI-067 are Group 2 track G — WI-066 codes only against the conway-core `SubagentHost` trait and is unit-testable against `FakeSubagentHost` before the runtime implementation exists.