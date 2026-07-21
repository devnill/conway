## Size Assessment

**Right size — 7 work items (WI-111…WI-117).** The module has four Provides (interactive TUI, `-p` one-shot, `sessions` subcommands, `routes explain`) plus a stable exit-code/stream contract that must be integration-tested. Decomposition axis: **command surface first (shared skeleton), then one renderer per mode, then read-only subcommands, then session-continuity flags** — matching the §9 slice order (Slice 1 → Group 2/H → Group 3/K,M).

One assumption stated up front, because the module spec does not settle it: **integration tests exercise the compiled binary against a local mock HTTP server speaking the OpenAI-compatible SSE dialect**, configured through a temp `conway.toml`. This avoids adding any CLI-only test hook (which would violate GP-05 / C-03), and keeps the fake in the test crate rather than in the binary.

---

# WI-111: CLI skeleton, clap command surface, and exit-code contract

## Complexity
Medium

## Scope
- `crates/conway-cli/Cargo.toml` (create)
- `crates/conway-cli/src/main.rs` (create)
- `crates/conway-cli/src/cli.rs` (create)
- `crates/conway-cli/src/exit.rs` (create)
- `crates/conway-cli/src/diag.rs` (create)
- `crates/conway-cli/src/oneshot.rs` (create — stub)
- `crates/conway-cli/src/tui/mod.rs` (create — stub)
- `crates/conway-cli/src/commands/mod.rs` (create)
- `crates/conway-cli/src/commands/sessions.rs` (create — stub)
- `crates/conway-cli/src/commands/routes.rs` (create — stub)
- `crates/conway-cli/tests/cli_surface.rs` (create)

## Depends
- MODULE:conway-facade

## Criteria
- [machine] `crates/conway-cli/Cargo.toml` declares `[[bin]] name = "conway"`, and its `[dependencies]` contain exactly one workspace crate: `conway`. A test (`cli_surface.rs::no_forbidden_deps`) parses the manifest and asserts none of `conway-runtime`, `conway-backends`, `conway-session`, `conway-routing`, `conway-core`, `conway-tools` appear.
- [machine] `conway --help` exits 0 and lists subcommands `sessions` and `routes`; `conway sessions --help` lists `list`, `show`, `tree`, `export`; `conway routes --help` lists `explain`.
- [machine] `conway --nonexistent-flag` exits **2** and writes nothing to stdout.
- [machine] `conway sessions show` with no `<id>` exits **2**, stdout empty, stderr non-empty.
- [machine] `exit.rs` exports `pub enum ExitCode` with discriminants `Completed=0, AgentFailed=1, Usage=2, PermissionDenied=3, NoHealthyBackend=4, BudgetExceeded=5, Interrupted=130` and `impl ExitCode { pub fn code(self) -> i32; pub fn from_result(r: &AgentResult) -> ExitCode; pub fn from_error(e: &ConwayError) -> ExitCode; }`.
- [machine] Unit tests in `exit.rs` cover every mapping row in the Implementation Notes table (one assertion per row).
- [machine] Every stub `run` entry point returns `Ok(ExitCode::Usage)` after writing `not implemented` to **stderr**; a test asserts stdout is empty for `conway sessions list`.
- [machine] `cargo clippy -p conway-cli -- -D warnings` passes.

## Notes

**Objective:** Establish the binary, the complete clap command/flag surface for the whole module (declared once, here, so no later work item edits `cli.rs`), the exit-code contract, and the stdout/stderr discipline. All command handlers are stubs that later work items fill in.

**Implementation Notes:**

Clap derive, `clap = { version = "4", features = ["derive", "env"] }`.

```rust
// cli.rs
#[derive(Parser)]
#[command(name = "conway", version)]
pub struct Cli {
    #[arg(short = 'p', long = "print", value_name = "PROMPT", num_args = 0..=1,
          default_missing_value = "")]
    pub print: Option<String>,          // present => one-shot; empty => read stdin
    #[arg(long, value_enum, default_value = "text")]
    pub output_format: OutputFormat,    // Text | Json | Jsonl
    #[arg(long, value_delimiter = ',')] pub allowed_tools: Vec<String>,
    #[arg(long, value_delimiter = ',')] pub deny_tools: Vec<String>,
    #[arg(long, value_enum, default_value = "allowlist")] pub permission_mode: PermissionMode,
    #[arg(long)] pub role_override: Option<String>,
    #[arg(long)] pub model: Option<String>,
    #[arg(long)] pub session: Option<String>,
    #[arg(long)] pub resume: Option<String>,
    #[arg(long, value_name = "SID[@SEQ]")] pub fork_from: Option<String>,
    #[arg(long, value_name = "PATH")] pub config: Option<PathBuf>,
    #[arg(long, value_name = "DIR")] pub cwd: Option<PathBuf>,
    #[arg(short, long, action = ArgAction::Count)] pub verbose: u8,
    #[command(subcommand)] pub command: Option<Command>,
}

pub enum Command {
    Sessions(SessionsArgs),   // List{ limit, label, json } | Show{ id, json } | Tree{ id } | Export{ id, out: Option<PathBuf> }
    Routes(RoutesArgs),       // Explain{ role, json }
}
```

Dispatch in `main.rs`: if `command.is_some()` → `commands::{sessions,routes}::run`; else if `print.is_some()` → `oneshot::run`; else → `tui::run`. `main` is `#[tokio::main]`, returns `std::process::ExitCode`, and never uses `?` at top level — every error is converted via `ExitCode::from_error`, printed to stderr by `diag::error`, and returned as a code.

`diag.rs`: `pub fn error(msg)`, `pub fn warn(msg)`, `pub fn info(msg)` — all write to `stderr` only; `info` is suppressed unless `verbose >= 1`. No function in `diag` may take a stdout handle. This is the mechanism enforcing "stdout carries only program output."

Exit-code mapping table (unit-tested):

| input | code |
|---|---|
| `ResultStatus::Completed` | 0 |
| `ResultStatus::Failed{..}` (non-permission cause) | 1 |
| `ResultStatus::Rejected{..}` | 1 |
| clap parse error / config load/parse error / bad `--fork-from` syntax / unknown role | 2 |
| `ConwayError` whose terminal cause is a permission denial (hard `Deny`, not `DenyWithFeedback`) | 3 |
| `RoutingError::NoCandidate{..}` or fallback chain exhausted with all attempts transport/server errors | 4 |
| `ResultStatus::BudgetExceeded` | 5 |
| `ResultStatus::Cancelled` **and** a SIGINT was observed | 130 |
| `ResultStatus::Cancelled` with no SIGINT | 1 |

Precedence when several apply: 2 > 130 > 3 > 4 > 5 > 1 > 0. Usage errors are detected before the runtime starts, so they cannot race; SIGINT outranks a status the runtime produced *because of* the interrupt.

Config is loaded through `conway::ConwayBuilder::discover()` when `--config` is absent, `from_config(path)` when present. `--cwd` is applied by `std::env::set_current_dir` before builder construction.

---

# WI-112: `-p` one-shot mode — streaming renderers, allow-list gate, SIGINT

## Complexity
High

## Scope
- `crates/conway-cli/src/oneshot.rs` (modify)
- `crates/conway-cli/src/render/mod.rs` (create)
- `crates/conway-cli/src/render/text.rs` (create)
- `crates/conway-cli/src/render/json.rs` (create)
- `crates/conway-cli/src/render/jsonl.rs` (create)
- `crates/conway-cli/src/signal.rs` (create)

## Depends
- WI-111
- MODULE:conway-facade

## Criteria
- [machine] `render/mod.rs` exports `pub trait Renderer { fn on_event(&mut self, env: &Envelope) -> io::Result<()>; fn finish(&mut self, result: Option<&AgentResult>) -> io::Result<()>; }` and `pub fn make(format: OutputFormat, out: Box<dyn Write + Send>) -> Box<dyn Renderer>`.
- [machine] Unit test: `TextRenderer` fed `[TextDelta("he"), TextDelta("llo"), TurnFinished, AgentFinished{Completed}]` writes exactly `hello\n` and nothing else.
- [machine] Unit test: `TextRenderer::on_event` calls `flush()` after every `TextDelta` (verified with a writer that records flush calls; assertion: flush count == TextDelta count).
- [machine] Unit test: `JsonlRenderer` writes exactly one line per `Envelope`, each line parses as JSON with keys `seq`, `ts`, `session`, `agent`, `event`, and contains no byte `0x1b`.
- [machine] Unit test: `JsonRenderer` writes nothing on non-terminal events and exactly one JSON object (the `AgentResult`) on `finish`.
- [machine] Unit test: with `--permission-mode allowlist --allowed-tools read,glob`, `oneshot::build_gate` produces an `AllowListGate` that returns `DenyWithFeedback` for `bash` and `AllowOnce` for `read`; with `--deny-tools bash` and no allow-list, `bash` yields `DenyWithFeedback` and `read` yields `AllowOnce`; with `--permission-mode deny`, every tool yields `DenyWithFeedback`.
- [machine] Unit test: `oneshot::run` never constructs a `PromptingGate` (assert by construction — `oneshot.rs` contains no reference to `PromptingGate`; enforced by a `grep`-style source assertion test).
- [machine] `signal.rs` exports `pub fn install() -> SigintWatch` with `SigintWatch::hits() -> u8`; unit test asserts the second delivery invokes the abort callback.
- [machine] `cargo clippy -p conway-cli -- -D warnings` passes.

## Notes

**Objective:** Implement the one-shot execution path — the Slice 1 milestone. Read the prompt, build the session, consume the `EventStream`, render incrementally, map termination to an exit code.

**Implementation Notes:**

`oneshot::run(cli: &Cli, conway: Conway) -> Result<ExitCode>` steps, in order:

1. **Prompt source.** `--print <text>` non-empty → use it. `--print` empty/absent-value → read `stdin` to end (if stdin is a TTY and empty, exit 2 with `no prompt provided`).
2. **Gate.** Build from flags *before* the session exists, via `conway::gates::AllowListGate`. Rules: `permission_mode=deny` → deny-all. Otherwise allow-list = `--allowed-tools` if non-empty, else "all tools"; then subtract `--deny-tools`. Denials MUST be `DenyWithFeedback { message }` where message names the tool and the flag that would permit it (e.g. `tool "bash" is not permitted in -p mode; pass --allowed-tools bash`). Never `Deny`.
3. **Session.** `conway.new_session(SessionSpec{ role_override, model_pin, cwd, .. })`. `--model` sets a pin; `--role-override` sets the root role alias. Unknown role/model → exit 2 before any output.
4. **Subscribe before prompting.** `let mut events = handle.events();` MUST be obtained before `handle.prompt(text)` — otherwise early envelopes are lost.
5. **Render loop.** `while let Some(env) = events.next().await { renderer.on_event(&env)?; ... }` — terminate on `Event::AgentFinished` whose `agent == handle.root()`. On `Event::Lagged{skipped}` write a stderr warning (never stdout).
6. **Finish.** `renderer.finish(result.as_ref())`, flush, then `ExitCode::from_result`.

**Event→render mapping (binding):**

| Event | `text` | `json` | `jsonl` |
|---|---|---|---|
| `TextDelta{text}` | write text verbatim, flush | — | envelope line |
| `ThinkingDelta` | — (suppressed) | — | envelope line |
| `ToolCallProposed` / `ToolCallStarted` / `ToolCallFinished` | one-line note to **stderr** | — | envelope line |
| `PermissionResolved{decision: Denied}` | one-line note to **stderr** | (appears in result) | envelope line |
| `ModelDecision` | stderr, only when `verbose >= 1` | — | envelope line |
| `BackendDegraded` | stderr | — | envelope line |
| `Error{fatal:true}` | stderr | — | envelope line |
| `AgentFinished{result}` | trailing `\n` if last write wasn't one | serialize `result` to stdout | envelope line |

Rationale for text mode: stdout is the assistant's text and nothing else, so `conway -p "…" > out.txt` yields clean content. All three renderers write through a `BufWriter` that is flushed after every `on_event`; buffering across events is the bug the module spec forbids.

**SIGINT.** `signal.rs` spawns a `tokio::signal::ctrl_c()` loop. First hit: set `sigint_seen`, call `handle.cancel(root, "sigint")`, keep consuming the event stream (so a terminal `AgentResult` is still rendered and persisted); if no `AgentFinished` arrives within 5s, stop rendering and return 130 anyway. Second hit: flush stdout, then `std::process::exit(130)` immediately. On Windows, `ctrl_c` semantics are identical; no `signal_unix` types leak into `oneshot.rs`.

`oneshot.rs` must not exceed the responsibility of a renderer driver: no permission prompting, no tool logic, no direct routing/session access.

---

# WI-113: One-shot integration test suite (exit codes, stdout purity, streaming, SIGINT)

## Complexity
High

## Scope
- `crates/conway-cli/tests/common/mod.rs` (create)
- `crates/conway-cli/tests/common/mock_backend.rs` (create)
- `crates/conway-cli/tests/oneshot.rs` (create)
- `crates/conway-cli/tests/fixtures/conway.toml.tmpl` (create)

## Depends
- WI-112

## Criteria
- [machine] `common::MockBackend::start(script) -> MockHandle` serves an OpenAI-compatible `/v1/chat/completions` endpoint on an ephemeral port, emitting SSE chunks from a scripted list, and `/v1/models` returning the configured model id.
- [machine] `common::run_conway(args, cfg) -> Output` writes `conway.toml` from the template into a `tempfile::TempDir` (pointing `base_url` at the mock), runs the binary via `assert_cmd`, and returns captured stdout/stderr/status.
- [machine] Test `text_streams_only_assistant_text`: a two-delta script yields stdout exactly `hello world\n`, and stderr is not asserted to be empty (diagnostics allowed there).
- [machine] Test `stdout_purity`: with `-v -v` and a script producing a tool call plus a denial, stdout contains no line beginning with `conway:` and no `0x1b` byte.
- [machine] Test `jsonl_line_by_line`: every stdout line parses via `serde_json::from_str::<serde_json::Value>` and has `seq`/`agent`/`event` keys; `seq` values are strictly increasing; no line contains `0x1b`.
- [machine] Test `jsonl_streams_incrementally`: the child's stdout yields a parseable line **before** the mock server has sent its final SSE chunk (mock holds the last chunk behind a barrier for 500 ms; test reads a line with a 2 s timeout).
- [machine] Test `json_single_object`: stdout parses as exactly one JSON object with a `status` field.
- [machine] Test `exit_0_completed`, `exit_1_failed`, `exit_2_bad_flag`, `exit_2_bad_config`, `exit_4_no_backend` (mock refuses connections), `exit_5_budget` (`max_steps=1` config + tool-calling script) each assert the exact status code.
- [machine] Test `exit_3_permission_termination`: `--permission-mode deny` with a script that only emits tool calls terminates with code 3 and, under `--output-format jsonl`, stdout contains a `PermissionResolved` envelope with a denied decision.
- [machine] Test `unlisted_tool_gets_feedback`: `--allowed-tools read` with a `bash` call produces a jsonl `PermissionResolved` denial envelope whose message names `bash`, and the run does not hang.
- [machine] Test `sigint_graceful` (unix-gated): sends `SIGINT` mid-stream via `nix::sys::signal::kill`, asserts exit 130 within 10 s and that already-emitted stdout is retained.
- [machine] Test `sigint_double_aborts` (unix-gated): two `SIGINT`s 200 ms apart yield exit 130 within 2 s.
- [machine] `cargo test -p conway-cli` passes with no network access beyond loopback.

## Notes

**Objective:** Lock the CLI's externally observable contract — exit codes, stream shape, stdout purity, signal behavior — as executable tests against the real binary. These tests are the acceptance evidence for the Slice 1 milestone.

**Implementation Notes:**

Dev-dependencies: `assert_cmd`, `predicates`, `tempfile`, `serde_json`, `tokio` (rt + macros), `hyper` or `axum` for the mock, `nix` (unix-only, for signals).

`MockBackend` script type:

```rust
pub enum Chunk { Text(&'static str), ToolCall{ name: &'static str, args: serde_json::Value },
                 Finish(&'static str), Delay(Duration), Hang }
pub struct Script(pub Vec<Vec<Chunk>>);   // outer = one entry per successive request
```

The mock records received request bodies so a test may assert the streaming path was used. `Hang` holds the connection open, used by the SIGINT tests.

`conway.toml.tmpl` placeholders: `{{BASE_URL}}`, `{{MODEL}}`, `{{MAX_STEPS}}`. It configures a single `openai-compat` backend with dialect `ollama`, a single role `default` chaining to it, and `.conway/sessions` inside the temp dir.

Do not assert on exact stderr text — only on stdout content, exit status, and structural properties. Stderr wording is presentational and may change; asserting it would create brittle tests without protecting a contract.

Tests must be deterministic: no sleeps used as synchronization except the deliberate `Delay`/barrier in the mock, and every read from the child's stdout has an explicit timeout that fails the test rather than hanging CI.

---

# WI-114: Interactive TUI — ratatui shell, streaming render, permission prompts

## Complexity
High

## Scope
- `crates/conway-cli/src/tui/mod.rs` (modify)
- `crates/conway-cli/src/tui/app.rs` (create)
- `crates/conway-cli/src/tui/view.rs` (create)
- `crates/conway-cli/src/tui/input.rs` (create)
- `crates/conway-cli/src/tui/gate.rs` (create)
- `crates/conway-cli/src/tui/state.rs` (create)

## Depends
- WI-111
- MODULE:conway-facade

## Criteria
- [machine] `tui::run(cli, conway) -> Result<ExitCode>` enters the alternate screen with raw mode enabled and restores the terminal on every exit path, including panic (a panic hook installed in `run` restores the terminal before re-raising); test asserts the hook is installed by invoking `tui::install_panic_hook` and checking the restore closure ran.
- [machine] `state.rs` exports `pub struct AppState` with `apply(&mut self, env: &Envelope)` and is unit-tested with no terminal: feeding `[AgentSpawned(root), TextDelta("a"), TextDelta("b"), ToolCallProposed, PermissionRequested, PermissionResolved, ToolCallFinished, AgentFinished]` yields a transcript containing one assistant message `"ab"`, one completed tool-call entry, and a tree with one node in `Finished` state.
- [machine] Unit test: `AppState::apply` on `Event::Lagged{skipped}` appends a visible transcript notice rather than panicking or dropping silently.
- [machine] Unit test: `AppState::apply` on `AgentSpawned{parent: Some(p)}` attaches the node under `p`; on an unknown parent it attaches under root and records a diagnostic entry (no panic).
- [machine] `gate.rs` implements `conway_core::PermissionGate` (obtained through the `conway` facade re-export) by sending a request over an `mpsc` channel and awaiting a `oneshot` reply; unit test drives it headlessly: `AllowOnce`, `AllowAlways{Session}`, `Deny`, and channel-drop → `Deny{reason:"cancelled"}`.
- [machine] Unit test: the render pass is pure — `view::draw(&AppState, &mut TestBackend Frame)` produces a non-empty buffer and mutates no state (`AppState` passed as `&`).
- [machine] `tui::run` uses no `conway-runtime`/`conway-session`/`conway-routing` symbol (enforced by the WI-111 dependency test).
- [human] Terminal-UX review sign-off (product owner): the three-pane layout (agent tree left, transcript centre/right, input line bottom) is legible at 80×24 and at 200×50, streaming text does not flicker, and the permission prompt is unmistakably distinct from ordinary transcript output.
- [human] Product owner confirms that a permission prompt during streaming output does not lose or reorder already-rendered assistant text.

## Notes

**Objective:** The interactive mode's shell: terminal lifecycle, application state derived from the identical `EventStream`, the render pass, key handling, and the in-TUI `PermissionGate`. Slash commands are WI-115.

**Implementation Notes:**

Dependencies: `ratatui`, `crossterm`, `tokio`, `tui-textarea` (or a hand-rolled single-line input — either is acceptable; if hand-rolled it must support left/right/home/end/backspace/word-delete).

Architecture — three tasks joined by channels, so no rendering happens on the runtime's thread:

```
event task:  EventStream ──Envelope──▶ mpsc ─┐
input task:  crossterm EventStream ──Key──▶ mpsc ─┼─▶ app loop ─▶ view::draw
gate:        PermissionGate::check ──Req──▶ mpsc ─┘        (60 fps cap / redraw-on-change)
```

The app loop is `tokio::select!` over the three receivers plus a 16 ms redraw tick; it redraws only when a `dirty` flag is set.

**Layout.** `Layout::horizontal([Constraint::Length(28), Constraint::Min(0)])`; the right column splits vertically into transcript (`Min(0)`) and input (`Length(3)`). When width < 60, the tree pane is hidden and reachable only via `/tree`.

**AppState** holds: `transcript: Vec<Entry>` (`Entry::{User, Assistant{text}, Tool{call_id, name, status, preview}, Notice{text}, Permission{..}}`), `tree: AgentTreeView`, `last_model_decision: Option<ModelDecision>` (this is what `/why` will read), `input: String`, `mode: Mode::{Normal, AwaitingPermission(PendingPrompt)}`, `scroll`.

`TextDelta` appends to the trailing `Assistant` entry, creating one if the last entry is not `Assistant`. This is the only place text is coalesced — it must produce the same visible text a `TextRenderer` would produce for the same event sequence (the module's "same events, different renderer" invariant).

**Permission prompt.** `Mode::AwaitingPermission` renders a bordered block over the bottom of the transcript pane showing `req.rendered`, the tool name, category, and the `agent_path` (so the user sees *which* subagent asked). Keys: `y` = `AllowOnce`, `a` = `AllowAlways{Session}`, `n` = `Deny{reason:"user denied"}`, `Esc` = `DenyWithFeedback{"user declined; try another approach"}`. Only one prompt is displayed at a time; concurrent requests queue in arrival order.

**Keys (normal mode):** `Enter` submits the input as `SessionHandle::prompt` (or, if it starts with `/`, dispatches to the command handler — the dispatch hook is defined here, handlers land in WI-115), `Ctrl-C` = graceful cancel of the running turn (second within 2 s exits with 130), `Ctrl-D` on an empty input exits 0, `PgUp`/`PgDn` scroll the transcript.

Exit code from the TUI is 0 for a normal quit, 130 for a double `Ctrl-C`, and `ExitCode::from_error` for a fatal startup error.

---

# WI-115: TUI slash commands

## Complexity
Medium

## Scope
- `crates/conway-cli/src/tui/commands.rs` (create)
- `crates/conway-cli/src/tui/app.rs` (modify)

## Depends
- WI-114
- MODULE:conway-facade

## Criteria
- [machine] `commands::parse(input: &str) -> Result<SlashCommand, ParseError>` is unit-tested for every command listed in the Implementation Notes table, including one malformed case per command producing `ParseError` with a message naming the expected form.
- [machine] Unit test: `/steer a7 hold on` parses to `Steer{ target: "a7", text: "hold on" }` with the text preserving internal whitespace.
- [machine] Unit test: `/fork a7 review the diff` and `/spawn reviewer review the diff` parse to `Fork`/`Spawn` variants with the agent-def and prompt split correctly; `/spawn` with no prompt is a `ParseError`.
- [machine] Unit test: an unknown command `/nope` yields `ParseError` and the app appends a `Notice` entry — it must not be sent to the model as a prompt.
- [machine] Unit test (headless, fake `SessionHandle` seam): each command maps to exactly one facade call — `/steer`→`steer`, `/fork`→`fork`, `/spawn`→`spawn`, `/context`→`context_report`, `/tree`→`tree`, `/resume`→`Conway::resume`, `/why`→ reads `AppState::last_model_decision` and makes **no** facade call.
- [machine] Unit test: `/context a7` renders each `ContextReport` segment as one line containing the segment id, a provenance label, and the token estimate; a report with zero segments renders an explicit "empty context" line rather than nothing.
- [machine] Unit test: `/why` before any `ModelDecision` renders "no routing decision yet" instead of panicking.
- [human] Product owner reviews the rendered output of `/tree`, `/context`, and `/why` against a live multi-agent session and confirms each answers its question without scrolling on an 80×24 terminal.

## Notes

**Objective:** Implement the interactive command surface listed in Provides. Commands are the interactive-mode equivalent of the facade's introspection API; none of them may reach past `SessionHandle`/`Conway`.

**Implementation Notes:**

| Input | Variant | Action |
|---|---|---|
| `/steer <agent> <text…>` | `Steer` | `handle.steer(agent, text)`; on `Ok`, append a `Notice` "steer queued for `<agent>`"; the runtime's `SteerQueued` event is what confirms it landed |
| `/tree` | `Tree` | render `handle.tree()` into the transcript as an indented snapshot (also always visible in the left pane; this pins a point-in-time copy) |
| `/context <agent>` | `Context` | `handle.context_report(agent).await`, render segments in order |
| `/why` | `Why` | render `AppState::last_model_decision`: role, chosen backend/model, `RoutingReason` rendered per variant, attempt number |
| `/fork <agent> <directive…>` | `Fork` | `handle.fork(agent, ForkSpec{ prompt: directive, ..default })` |
| `/spawn <agent_def> <prompt…>` | `Spawn` | `handle.spawn(root, SpawnSpec{ agent_def, prompt, ..default })` |
| `/resume <sid>` | `Resume` | `conway.resume(sid)`, replace the active handle, resubscribe events, reset `AppState` from `handle.transcript(root)` |
| `/help` | `Help` | render the table above |
| `/quit` | `Quit` | exit 0 |

Parsing rule: split on the first whitespace for the command word; commands taking a trailing free-text argument consume the remainder verbatim (no re-tokenization, no quote handling). Agent ids are accepted as either full ULIDs or unique prefixes; an ambiguous prefix is a `ParseError` listing the candidates.

Every command is executed asynchronously and its failure becomes a `Notice` entry with the error's `Display`. A failing slash command must never terminate the TUI.

`app.rs` changes are confined to: (a) routing input beginning with `/` to `commands::parse` + `commands::execute`, and (b) adding the `last_model_decision` update on `Event::ModelDecision`. No other behavior in `app.rs` changes.

---

# WI-116: `sessions` and `routes explain` subcommands

## Complexity
Medium

## Scope
- `crates/conway-cli/src/commands/sessions.rs` (modify)
- `crates/conway-cli/src/commands/routes.rs` (modify)
- `crates/conway-cli/src/commands/fmt.rs` (create)
- `crates/conway-cli/tests/subcommands.rs` (create)

## Depends
- WI-111
- WI-113
- MODULE:conway-facade

## Criteria
- [machine] `conway sessions list` prints one row per session with columns `ID  CREATED  ROLE  STATUS  ORIGIN` (origin blank for roots, `fork@<seq> <parent-prefix>` for children), exits 0, and prints only the header when there are no sessions (never an error).
- [machine] `conway sessions list --json` emits one JSON array; every element parses and contains `id`, `created`, `status`.
- [machine] `conway sessions show <id>` prints the resolved transcript one record per block; `--json` emits one `LogRecord` JSON object per line, each parseable.
- [machine] `conway sessions show <unknown-id>` exits 2 with an empty stdout.
- [machine] `conway sessions tree <id>` prints an ASCII tree using `├─`/`└─`/`│`; integration test builds a parent with two forked children (through the CLI's own one-shot mode plus a fixture store) and asserts both children appear indented one level under the parent.
- [machine] `conway sessions export <id>` writes the full ancestry-resolved JSONL to stdout (or to `--out <path>`); every line parses as JSON; a re-run is byte-identical (deterministic).
- [machine] `conway routes explain <role>` prints, in order: the resolved chain with position numbers, each candidate's `RoutingReason`, and each endpoint's current breaker state; exits 0.
- [machine] `conway routes explain <role> --json` emits one JSON object with keys `role`, `chain`, `skipped`, `health`.
- [machine] `conway routes explain <unknown-role>` exits 2, stdout empty, stderr names the role and lists the configured roles.
- [machine] Integration test asserts stdout of every subcommand above contains no `0x1b` byte when stdout is not a TTY.
- [machine] `sessions.rs` and `routes.rs` reference only `conway::{Conway, SessionMeta, LogRecord, ExplainReport}` — no other workspace crate (covered by the WI-111 dependency test).

## Notes

**Objective:** The read-only introspection subcommands. These are pure formatters over `Conway::sessions`, `SessionHandle::transcript`, and `Conway::explain_routing`.

**Implementation Notes:**

`fmt.rs` holds the shared rendering helpers so `sessions` and `routes` produce consistent output: `pub fn table(headers: &[&str], rows: Vec<Vec<String>>) -> String` (left-aligned, two-space gutter, columns sized to the widest cell, truncating cells over 48 chars with `…`), `pub fn tree_lines<T>(root, children_fn, label_fn) -> Vec<String>`, `pub fn ts(dt: DateTime<Utc>) -> String` (RFC 3339, seconds precision), `pub fn id_short(id) -> String` (first 8 ULID chars).

Colour: use `crossterm::style` only when `std::io::stdout().is_terminal()`; when piped, emit plain text. This is what makes the no-ANSI criterion machine-checkable.

`sessions tree <id>` reconstructs from `SessionMeta.origin` links obtained via `Conway::sessions(SessionFilter::all())` — the CLI does not read the index file directly (that is `conway-session`'s business, reached through the facade). If `<id>` is a child, the tree is rooted at `<id>`, not at its ancestor.

`sessions export` differs from `show --json` in one respect: `export` emits the *ancestry-resolved* record sequence (what the agent actually saw), `show` emits the same. If the facade does not expose a resolved-transcript accessor distinct from `SessionHandle::transcript`, use `transcript` for both and note in `--help` that export is transcript-resolved; do **not** add a facade-bypassing read.

`routes explain` renders `RoutingReason` variants as: `PinnedByApi` → "pinned by API"; `PinnedByAgentDef` → "pinned by agent definition"; `AliasPrimary{alias}` → "primary for role `<alias>`"; `Fallback{position, after}` → "fallback #N after: <failures>"; `CapabilitySkip{skipped, missing}` → "skipped `<ref>`: missing <caps>"; `HealthSkip{skipped, breaker}` → "skipped `<ref>`: <breaker> breaker open". A candidate rendered without a reason is a bug and must fail a unit test.

Integration tests reuse `tests/common/mod.rs` from WI-113 for the temp-config + mock-backend harness; they add no new harness code.

---

# WI-117: Session continuity flags — `--session`, `--resume`, `--fork-from`

## Complexity
Medium

## Scope
- `crates/conway-cli/src/oneshot.rs` (modify)
- `crates/conway-cli/src/session_ref.rs` (create)
- `crates/conway-cli/tests/continuity.rs` (create)

## Depends
- WI-112
- WI-113
- MODULE:conway-facade

## Criteria
- [machine] `session_ref.rs` exports `pub fn parse_fork_ref(s: &str) -> Result<(SessionId, Option<LogSeq>), ParseError>`; unit-tested for `01J…`, `01J…@142`, `01J…@` (error), `@142` (error), `01J…@abc` (error), and an invalid ULID (error). Each error message names the expected form `<session-id>[@<seq>]`.
- [machine] Unit test: supplying more than one of `--session`, `--resume`, `--fork-from` is a usage error (exit 2) naming the conflicting flags; clap `conflicts_with_all` is acceptable as the mechanism.
- [machine] Integration test `resume_continues_transcript`: run `-p "remember X"`, capture the session id from `sessions list`, then run `-p "what did I say" --resume <sid>`; assert the mock server's second request body contains the first turn's text (proving the transcript was resolved and resent) and that the run exits 0.
- [machine] Integration test `resume_unknown_session` exits 2 with empty stdout.
- [machine] Integration test `fork_from_creates_child`: after a first run, `-p "…" --fork-from <sid>@1` exits 0 and `conway sessions tree <sid>` then shows exactly one child whose origin seq is 1.
- [machine] Integration test `fork_from_without_seq_uses_head`: `--fork-from <sid>` produces a child whose `origin.at_seq` equals the parent's head at fork time.
- [machine] Integration test `fork_from_seq_beyond_head` exits 2 (not 1) with a message naming the parent's head.
- [machine] Integration test `session_flag_sets_id`: `--session <new-id>` creates a session with that id; reusing an existing id without `--resume` exits 2.
- [machine] All continuity tests assert stdout purity (streamed assistant text only, under `--output-format text`).

## Notes

**Objective:** Wire the three session-continuity flags into one-shot mode, so `-p` can continue or branch prior work. This is Group 3 track K's CLI surface.

**Implementation Notes:**

Resolution happens in `oneshot::run` step 3 (session construction), replacing the unconditional `new_session` with:

```rust
let handle = match (&cli.resume, &cli.fork_from, &cli.session) {
    (Some(sid), None, None) => conway.resume(parse_sid(sid)?).await?,
    (None, Some(r),  None)  => { let (parent, at) = parse_fork_ref(r)?;
                                 let p = conway.resume(parent).await?;
                                 let child = p.fork(p.root(), ForkSpec{ at, prompt: text.clone(), ..default }).await?;
                                 /* handle bound to the child agent */ }
    (None, None, s)         => conway.new_session(SessionSpec{ id: s.map(parse_sid).transpose()?, .. }).await?,
    _ => return usage_error("--session, --resume and --fork-from are mutually exclusive"),
};
```

`--fork-from` semantics: the fork's directive **is** the `-p` prompt; the CLI must not send an additional `prompt()` call after forking, or the directive would be duplicated in the child's context. All other one-shot behavior (renderer, gate, SIGINT, exit code) is unchanged — the only thing these flags alter is which `SessionHandle` the renderer drives, which preserves the module's "identical handle, identical stream" invariant.

Every failure mode here is a **usage** error (exit 2), not an agent failure (exit 1): unknown session, malformed ref, seq beyond head, duplicate session id, conflicting flags. The agent never starts, so no agent status exists to report.

`parse_sid` accepts a full ULID only — prefix matching is an interactive affordance (WI-115) and is deliberately not offered in `-p`, where scripts need unambiguous inputs.

---

## Coverage Statement

**Module:** conway-cli
**Work items:** WI-111, WI-112, WI-113, WI-114, WI-115, WI-116, WI-117

**Coverage:** These seven work items collectively implement 100% of the conway-cli scope: the binary and its complete command surface (WI-111), one-shot streaming mode with all documented flags and the full exit-code/stdout/SIGINT contract (WI-112, verified by WI-113, extended by WI-117), interactive ratatui mode with permission prompts and slash commands (WI-114, WI-115), and the read-only `sessions`/`routes` subcommands (WI-116). Nothing in the module scope is excluded. The `--fork-from`/`--resume` flags are declared in WI-111 (so `--help` is complete from the first commit) and made functional in WI-117, matching the §9 sequencing that places fork/resume in Group 3.

**Provides implemented by:**
- Interactive REPL/TUI (agent-tree pane, streamed text, permission prompts) → WI-114
- Slash commands `/steer`, `/tree`, `/context`, `/why`, `/fork`, `/spawn`, `/resume` → WI-115
- `conway -p` prompt from argv or stdin → WI-112
- `--output-format text|json|jsonl` → WI-112 (renderers), WI-113 (contract tests)
- `--allowed-tools`, `--deny-tools`, `--permission-mode` → WI-112 (gate construction), WI-113 (denial-visibility tests)
- `--role-override`, `--model` → WI-111 (flag surface), WI-112 (applied to `SessionSpec`)
- `--session`, `--resume`, `--fork-from` → WI-111 (declaration), WI-117 (behavior)
- Exit-code contract (0/1/2/3/4/5/130) → WI-111 (`ExitCode` mapping), WI-112 (SIGINT path), WI-113 (per-code integration tests)
- `conway sessions list|show|tree|export` → WI-116
- `conway routes explain <role>` → WI-116

**Requires consumed by:**
- MODULE:conway-facade — `ConwayBuilder::{discover, from_config, build}` → WI-111
- MODULE:conway-facade — `Conway::new_session`, `SessionHandle::{prompt, events, root}`, `EventStream`/`Envelope` → WI-112, WI-114
- MODULE:conway-facade — `gates::AllowListGate` → WI-112
- MODULE:conway-facade — `PermissionGate` (implemented by the TUI gate), `SessionHandle::{tree, context_report, steer, fork, spawn, transcript}` → WI-114, WI-115
- MODULE:conway-facade — `Conway::{sessions, resume, explain_routing}`, `SessionMeta`, `LogRecord`, `ExplainReport` → WI-116, WI-117
- MODULE:conway-facade — `AgentResult`, `ResultStatus`, `ConwayError` (for exit-code mapping) → WI-111

**Boundary compliance:** no work item lists a dependency on `conway-runtime`, `conway-backends`, `conway-session`, or `conway-routing`; WI-111's manifest test enforces this mechanically for the life of the crate.

**File-scope disjointness:** `cli.rs` is created once (WI-111) and never modified again — the full flag surface is declared up front specifically so the four downstream items can proceed in parallel without contending for it. `oneshot.rs` is created in WI-111 and modified by WI-112 then WI-117 (both sequenced by dependency edges). `tui/app.rs` is created in WI-114 and modified by WI-115 (sequenced). `tests/common/mod.rs` is created in WI-113 and only read by WI-116 and WI-117.

**Parallelism:** after WI-111, three tracks run concurrently — {WI-112 → WI-113 → WI-117}, {WI-114 → WI-115}, and {WI-116, gated on WI-113 only for the test harness}. Maximum dependency depth is 4.