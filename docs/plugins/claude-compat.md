# Claude Code plugin compatibility: reading a directory you already have

The Claude Code plugin directory compatibility layer (board item
`01M0VR89FB1F3Q4FQ8852K2A5E`), shipped by `crates/conway-plugin-claude`.
Depends on [`concepts.md`](concepts.md) for vocabulary and reads naturally
alongside [`mcp.md`](mcp.md) — the MCP half of this page is that same
client, fed a translated declaration instead of a hand-written one.

## What this is, in one sentence

You point conway at a Claude Code plugin directory you already have on disk
— `conway` never downloads one — and it reads that directory fresh every
time it starts (**read-at-runtime**, never converted into a durable
`settings.json` entry), translating what it can and naming, individually,
everything it cannot.

## What works, fully, end to end

**MCP server declarations translate and run with no fidelity caveat.** A
directory's own `.mcp.json` (`{"mcpServers": {"<name>": {"command", "args",
"env"}}}`) is translated into a real `conway_plugin_mcp::McpPluginSpec` and
discovered through the identical `conway_plugin_mcp::McpPlugin::discover` ->
`ConwayBuilder::with_plugin` path an operator-authored `[plugins].mcp[]`
entry already uses. This is the one pairing that is a genuine structural
match — both sides are stdio JSON-RPC with a `command`/`env` declaration —
and it is the only kind translated here that needs no disclosure beyond
this section: an MCP server declared in a Claude Code plugin behaves
exactly as one hand-written in `[plugins].mcp[]` would. **It is not,
however, the only kind this layer wires to actually run any more.**
`hooks/hooks.json` and `commands/*.md` both now dispatch too — see the next
section for exactly what each does and does not cover, including the
payload-shape gap a dispatched hook still has against real Claude Code and
the best-effort, non-parity ruling that governs a dispatched command.

```json
{
  "plugins": {
    "claude_compat": [
      { "id": "acme-tools", "dir": "/path/to/acme-claude-plugin", "timeout_ms": 5000 }
    ]
  }
}
```

Empty by default (`[plugins].claude_compat = []`): **nothing is ever read
unless a directory is named here.** A discovery failure reading the
directory itself — the directory missing, a malformed
`.claude-plugin/plugin.json`/`.mcp.json` — fails the **whole build**, naming
the offending entry's own `id`, mirroring `mcp.md`'s own posture.

## A dead MCP server degrades the entry — it does not brick the session

**Board item `01M1AMSDE035HAG23TE6XPEF9R`, an operator-reported startup
failure, 2026-08-30.** Installing a real Claude Code plugin (`ideate`) and
restarting used to refuse to start conway *at all*: the plugin's own MCP
server died on its first launch (a missing runtime, a first-launch build
that never finished, an upstream bug — any of these is ordinary for a
third-party server, not exceptional), and that ONE dead subprocess failed
the whole build with `[plugins].claude_compat entry 'ideate': mcp server
'ideate': session died: closed stdout (EOF) mid-session`. The trap: `/plugin`
is how an operator disables or removes a plugin, and `/plugin` is
unreachable when the process that would show it never starts — the only
recovery was hand-editing `settings.json`, knowing the file existed, its
schema, and which key to cut.

**Ruling: a translated MCP server that fails discovery degrades that ONE
server and announces it — it does not fail the build.** conway starts
without that server's tools; the directory's own hooks and commands (an
independent declaration, read from separate files, never contingent on the
MCP server's own liveness) still attach exactly as if the server had
succeeded. Every degraded server is reported on stderr, unconditionally,
and as a `ConfigWarning` (`WarningCode::McpServerFailed`) on
`Conway::warnings()` — the same accessor a non-interactive run already
prints from and the TUI already renders into its own transcript, so the
failure is stated once, prominently, not buried in scrollback. The message
names the entry, the specific server, the underlying error, and the one
live recovery: `/plugin uninstall <entry-id>` — reachable now, for the
first time, because the session actually started.

**Why this does not weaken conway's own rule that a deny/prompt permission
rule must fail closed, never silently open.** An MCP server contributes
tools ONLY — `conway_plugin_mcp::McpPlugin`'s own `Plugin` implementation
carries no `hooks`/`permission_evaluator` override. A server that never
came up narrows what the model can call; it does not silently drop or
misapply a permission rule, which is the one thing that rule actually
forbids. Contrast the SAME directory's `hooks/hooks.json` rules: those ARE
permission-relevant, are discovered independently (a pure, local file read
— `conway_plugin_claude::discover` never spawns anything), and keep the
existing hard-fail posture on a genuine directory-read problem. The
distinction is by CLASS, not by which entry it happens to be: a tool-only
surface degrades; a permission/hook-contributing one still fails closed.

**What this does NOT cover.** An operator-authored `[plugins].mcp[]` entry
(`mcp.md`) and `[plugins].install` (a closed, compiled-in candidate set)
both keep their existing hard-fail posture, unchanged by this item. The
identical "tools only, degrade instead" argument could be made for
`[plugins].mcp[]` — same wire protocol, same contribution shape — but
extending the ruling there is a separate, undone widening this item does
not make on its own account (`crates/conway-cli/src/mcp_plugins.rs`'s own
doc states this explicitly). `[plugins].install` failing to load a
first-party plugin is conway's own defect, not a third party's, and stays
loud on purpose.

**Known gap, disclosed rather than silently deferred:** `/plugin`'s own
listing does not yet mark a degraded entry's row as failed — today it
shows the same "N mcp server(s) translated" count `rows_from_claude_compat`
always has, regardless of whether that server actually attached. Removal
already works regardless (`/plugin uninstall <id>` operates on
`settings.json` and the plugin store, never on the live installed-plugin
set), and the failure is still fully stated on stderr/`Conway::warnings()`
per the above — but the row itself does not yet distinguish "declared" from
"declared and actually running." Closing that gap means threading
`Conway::warnings()` (filtered by `WarningCode::McpServerFailed` and
correlated by entry id) into the state `rows_from_claude_compat` reads, a
small, separate change to `crates/conway-cli/src/tui/state.rs`/
`tui/app/startup.rs`/`tui/view/plugins.rs` this item did not make.

## What runs, with real caveats — read this before assuming full parity

**Renamed from "does NOT run"** (board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K`):
every kind below except `.claude-plugin/plugin.json` itself now genuinely
reaches a running `conway` process in some form — the heading calling all
of it "does NOT run" would now be actively wrong, not merely stale. This
is still the equally-prominent half of this page, by design (nothing here
may claim MORE than what actually reaches a running process, either):
every caveat, gap, and best-effort disclosure that keeps a translated kind
from matching real Claude Code exactly lives here, named, not glossed over.

- **`commands/*.md` — wired all the way to a running `conway` process,
  best effort (board items `01M0X1G29EZSFEWB1YAG40SE69`,
  `01M0XRCAFD7DD7N64RNRM3P8W9`).** Most command files translate into a
  real `conway_core::ports::Command` returning `CommandOutcome::
  SubmitPrompt` — `ClaudeCompatReport::command_registrations()` hands back
  the ready-to-install list. **That list now actually reaches a running
  process, not only `conway-plugin-claude`'s own library-level tests:**
  `conway-cli`'s `first_party_plugins::installed_plugins` — the SAME
  re-derivation the TUI's `CommandRegistry::build` and the `conway
  <plugin-id>.<command>` external subcommand both already read for every
  other plugin's own commands — folds in `claude_compat_plugins::
  command_plugins`, so a translated command shows up in the slash
  palette, dispatches through the ordinary `<plugin-id>.<name>` path,
  cannot shadow a built-in, and — through the identical `SessionHandle::
  prompt_command` path `conway_plugin_skeleton::FilePromptCommand`
  already proved out for an operator-authored prompt file — submits its
  prompt for real. Proven through the compiled binary, not the library API
  alone: `crates/conway-cli/tests/claude_compat_commands.rs` drives a real
  `conway` process end to end, mirroring `conway-plugin-claude`'s own
  `tests/commands_dispatch.rs` (which proves the library-API half of the
  identical claim, against `beepboop`'s real `commands/config.md`).
  **Best effort, not parity, by explicit operator ruling:** v1 performs no
  `$ARGUMENTS`/argument interpolation of any kind, so a command whose body
  contains a raw `$ARGUMENTS` placeholder is refused rather than submitted
  verbatim — named in the operator-visible report like anything else this
  layer cannot use. Every frontmatter key besides `description` (which
  becomes the command's own one-line summary) is named, not silently
  honored, even on a command that otherwise translates — `allowed-tools`
  above all: an operator who wrote a Claude Code tool restriction and had
  it silently ignored has a *permission* surprise, not merely a fidelity
  gap. A translated command's own name is always bare (never
  pre-namespaced) — the host prefixes it with the declaring plugin's own
  id and validates the result with `validate_command_name`, the same
  structural guarantee that already makes shadowing a built-in impossible
  for every other plugin command, translated or not.
- **`skills/<name>/SKILL.md` — imported two ways at once (board item
  `01M1DG5TTF6NHW2RXJRZ8ZPE7K`, reversing the earlier "out of scope"
  ruling).** First, as a real, invokable **slash command**: a skill is
  translated the identical way a `commands/*.md` file is (bare name, the
  host prefixing it with the declaring plugin's own id), so `ideate`'s own
  `refine` skill resolves as `/ideate.refine` — and, because real Claude
  Code names it `/ideate:refine` (a colon, not conway's own `.`),
  `conway-cli`'s slash-command parser accepts a leading `:` as an input
  ALIAS for `.`, so typing it exactly the way Claude Code itself would have
  you type it also resolves. Second, as a genuine **context-injectable
  skill**: `ConwayBuilder::with_extra_skill_dir` (a real, callable seam
  that already existed, just uncalled) is now called with each entry's own
  `skills/` directory, and `crates/conway/src/skills.rs`'s own lenient
  loader learned a third-party-tolerant fallback shape (no `name`
  frontmatter key required — the directory IS the identity, real Claude
  Code's own convention — and unrecognized keys tolerated rather than
  rejected), so a plugin's skill is selectable by name from `AgentDef.
  skills`, exactly like an operator's own `.conway/skills` entry, and
  always loses a name collision against one. **Cross-references survive
  by construction, not by rewriting prose:** a real `SKILL.md` tells its
  own reader to open a sibling file "relative to the plugin root" — every
  translated skill's own submitted prompt (and every context-injected
  skill body) is prefixed with one line naming this plugin's own absolute
  root directory, so a model reading it can resolve that reference with
  its own Read tool. Every `SKILL.md` frontmatter key besides
  `description` is named, not silently honored, mirroring `commands/*.md`'s
  own posture exactly (`allowed-tools` above all: a permission surprise,
  not a fidelity gap). A directory that fails to translate (unreadable,
  malformed/unterminated frontmatter, an empty body) is still named in the
  report.
- **`agents/*.md` — imported as real `AgentDef`s (same board item).**
  `ConwayBuilder::with_extra_agent_dir` (also a real, previously-uncalled
  seam) is now called with each entry's own `agents/` directory;
  `crates/conway/src/agents.rs`'s own lenient loader learned the identical
  kind of third-party-tolerant fallback (identity is always the file's own
  stem; `tools:` accepted as EITHER a YAML list — conway's own convention
  — OR a comma-separated string — real Claude Code's own convention, e.g.
  `tools: Read, Edit, Write, Bash, Grep, Glob`; `${CLAUDE_PLUGIN_ROOT}` in
  the body substituted with the plugin's own absolute root, the identical
  token `hooks.json`/`.mcp.json` commands already use). **How it is
  invoked is not a new mechanism**: an `AgentDef` is already "a prompt
  inserted into a session, like a system prompt" (`SpawnSpec.agent_def`),
  so `/spawn @worker` starts a fresh child running `ideate`'s own real
  `worker.md` as its system prompt — the same command an operator's own
  `.conway/agents/*.md` entry already answers to. **The safety ruling on
  `tools:`, escalated and decided, load-bearing:** a Claude Code tool name
  (`Read`, `Edit`, ...) is never conway's own (`read`, `edit`, ..., lower
  case) — every declared name is matched case-insensitively against
  conway's known first-party tool names; anything that does not resolve is
  DROPPED and named (never silently included), and a `tools:` declaration
  that resolves to zero known names still degrades to "this agent gets NO
  tools," never to "this agent gets every tool" — a translation gap only
  ever narrows what an agent can do, never widens it. `model:`'s own real
  convention is a bare alias (`model: sonnet`/`opus`), not conway's
  `<backend>/<model>` wire shape; an unparseable value is simply dropped
  (not a permission concern, so no safety consequence) and the agent falls
  back to its role's own default model. A file that fails to translate, or
  whose own declared tool restriction had something dropped, is named in
  the report — `AgentToolRestriction` is its own distinct
  `UnsupportedKind`, permission-shaped, separate from a whole-file failure.
- **`hooks/hooks.json` — event names are matched, and (board item
  `01M0X1FCQ80C9ET97HENXSAW2K`) a mapped rule now translates into a real,
  dispatchable `[hooks].rules[]`-shaped registration.**
  `ClaudeCompatReport::hook_registrations()` turns every `Mapped` rule into
  a `conway_plugin_claude::HookRegistration` — the shell command wrapped
  as `["/bin/sh", "-c", <command>]` (never a guessed word-split: the whole
  string, unmodified, handed to a real shell) with `${CLAUDE_PLUGIN_ROOT}`
  already resolved to the discovered directory's own absolute path (every
  real `hooks.json` this layer has been checked against uses that token in
  every command; leaving it unresolved would make a registration dispatch
  reliably and fail "no such file" just as reliably). See the coverage
  table below for exactly which Claude Code events that covers.
  **What this still does NOT mean:** the two sides' JSON *payload* shapes
  differ even where the event name lines up (a Claude Code hook script
  reads `tool_name`/`tool_input` on stdin; conway's dispatcher sends its
  own `HookInvocation`/`HookEvent` shape) — "dispatches" is not the same
  claim as "behaves identically to running under real Claude Code." **And
  this crate itself still never mutates a `HooksConfig`** — it hands back
  registrations; a CALLER installs them as a real `Plugin` before
  `ConwayBuilder::build`. **As shipped (board item
  `01M0XBZNBPXEESX8VNTJDKNG0J`, re-wired onto a real registration seam by
  board item `01M129QW0GV90QTQS6B3BY3DAR`), `conway-cli`'s own
  `[plugins].claude_compat[]` install path (`claude_compat_plugins.rs`)
  now wraps every mapped rule as a `Plugin` whose `hooks()` returns them
  and attaches it via `ConwayBuilder::with_plugin`** — the SAME seam its
  MCP half already uses, not an append into `[hooks].rules[]`: naming a
  directory in `settings.json` gets you both its MCP servers running *and*
  its mapped hooks dispatching, with no hand-copying of `{event, matcher}`
  into your own `[hooks].rules[]` required. Every registered rule keeps
  `on_failure: "deny"` — the CLI never chooses a foreign plugin's own
  outage posture for you — and the CLI reports, on startup, which
  registered hooks *can deny* a real tool call (`pre_tool_use`) versus
  which are observation-only, so naming a directory here is never
  presented as merely "hooks registered." A registered hook's dispatched
  id is also host-namespaced with its declaring plugin's own manifest id
  (never the bare id the translation itself assigned), which is what makes
  it distinguishable from an operator-authored `[hooks].rules[]` entry on
  the `/settings` review list. **This does not change the payload-shape
  caveat two paragraphs up**: a dispatched hook script still receives
  conway's own `HookInvocation`/`HookEvent` payload on stdin, not Claude
  Code's `tool_name`/`tool_input` shape — wiring dispatch makes the
  registration real, it does not make conway behave identically to Claude
  Code for whatever that script does with what it reads.

### Coverage table: which of a Claude Code plugin's own hooks actually run

Every event `beepboop` 1.4.0 or `ideate` 3.2.2 declares (25 measured from
`beepboop`'s own `hooks.json`, 7 from `ideate`'s, six shared) — status is one
of **maps** (exact, dispatches), **approximate** (dispatches, one known
semantic divergence), or **declined** (named in `unsupported`, never
dispatches). Fail-open/fail-closed is the conway event's own posture,
inherited by a translated rule that lands on it
(`crates/conway/src/config/schema.rs`'s own `HooksConfig` doc has the
authoritative per-event disclosure).

| Claude Code event | Status | conway event | Fail posture |
| --- | --- | --- | --- |
| `SessionStart` | maps | `session_starting` | open |
| `UserPromptSubmit` | maps | `prompt_submitted` | closed (may deny) |
| `PreToolUse` | maps | `pre_tool_use` | closed (may deny) |
| `PostToolUse` | maps | `post_tool_use` | open |
| `SubagentStart` | maps — narrowed to `Spawn`-mode children only, see below | `child_spawned` | open |
| `SubagentStop` | **approximate** | `child_reported` | open — see below |
| `PermissionRequest` | declined | — | — |
| `Notification` | declined | — | — |
| `Stop` | declined | — | — |
| `StopFailure` | declined | — | — |
| `PostToolUseFailure` | declined | — | — |
| `PreCompact` | declined (no compaction mechanism yet) | — | — |
| `PostCompact` | declined | — | — |
| `SessionEnd` | **declined and settled** — operator ruling, `docs/vision/DESIGN-permission-modes.md` §9; not reopened by this item | — | — |
| `TaskCreated` | declined | — | — |
| `TaskCompleted` | declined | — | — |
| `TeammateIdle` | declined (no teammate concept) | — | — |
| `InstructionsLoaded` | declined | — | — |
| `ConfigChange` | declined | — | — |
| `WorktreeCreate` | declined (no worktree concept) | — | — |
| `WorktreeRemove` | declined | — | — |
| `CwdChanged` | declined | — | — |
| `FileChanged` | declined | — | — |
| `Elicitation` | declined | — | — |
| `ElicitationResult` | declined | — | — |

**`SubagentStart` used to be the second approximate pair — fixed, not just
disclosed (board item `01M129Y98V4C1050QBPPMY37X0`).** Conway creates a
child agent in one of two modes: `Spawn` (a clean child with no ancestry —
the shape Claude Code's own Task tool creates, and the shape a plugin
author writing a `SubagentStart` hook is picturing) and `Fork` (the current
conversation continues in a child that inherits its context — the shape
`/ask`, both the modal command and the `conway_ask` tool, use). Before this
item, a translated `SubagentStart` rule fired for BOTH: a plugin's hook
reacted to every `/ask`, a thing its author never had in mind — for
`beepboop` specifically, an audible sound on a keystroke that is not
"starting a subagent" to the operator at all. Fixed using data already
being sent to the hook and previously discarded: `child_spawned`'s own
payload always carried `"mode"` (`Fork`/`Spawn`), and a translated
`SubagentStart` rule now sets `spawn_only`, a per-hook filter
(`conway_core::ports::PluginHookRule::spawn_only`,
`conway_runtime::hook_dispatch::HookSpec::spawn_only`) that only lets it
see the `Spawn` occurrences. **conway's `child_spawned` event ITSELF is
unchanged** — it still fires for both modes, unconditionally, at the same
single `SubagentHost::start` call site; an operator-authored
`[hooks].rules[]` entry (or any other plugin's own `child_spawned`
subscription) still sees every child, fork included, exactly as before.
Only the ONE translated `SubagentStart` rule narrows.
**Considered and rejected: leaving this approximate and only improving the
disclosure wording.** That would have kept the operator-visible bug (every
`/ask` still triggering the hook) in exchange for a clearer label — a
worse outcome than actually fixing it, given the data needed to fix it
was already in hand.

**The one divergence still known and NOT fixed here, for `child_reported`
specifically:** conway's `child_reported` fires once per agent that has a
parent, for both an ordinary completion *and* a supervisor-synthesized
terminal result (a panic, or a task still unresponsive past its grace
window) — whether Claude Code's own `SubagentStop` fires for that second,
synthesized case the same way is unverified. **Considered and rejected:
narrowing `child_reported` to fire only for an ordinary completion.**
Unlike `SubagentStart`'s divergence, there is no field in `child_reported`'s
own payload (`{agent_id, parent, session, result}`) that tells the two
cases apart — an ordinary completion can ALSO carry a `Failed`/
`Cancelled`/`BudgetExceeded` result, the identical shapes a synthesized
panic/timeout produces — so narrowing here would need a NEW payload field
invented for this alone, not a fix using data already in hand. And the
cost would land exactly where visibility matters most: a plugin watching
`child_reported` to know when a child is done would silently stop hearing
about the crashes and timeouts — the cases most worth surfacing — while
still hearing about ordinary success. Kept firing for both, mapped,
labelled, not chased further, per the operator ruling's own
best-effort-and-disclosed appetite; a beepboop smoke test is what surfaces
whether it actually bites in practice.

Every `declined` row above is also named individually, with its own reason,
in `ClaudeCompatReport::unsupported` (`UnsupportedKind::Hook`) — never
folded into a single count.

- **`.claude-plugin/plugin.json` — read for identity/description only.**
  There is no counterpart in conway to a plugin manifest as a *file
  format* — `PluginManifest` is a Rust struct a `Plugin` trait method
  returns, not a declaration a directory can carry. This layer reads
  `name`/`version`/`description` and nothing else; every other key is
  simply never looked at (never a hard parse failure — see "Foreign
  frontmatter" below).

## Where the full list of what did not import lives

`conway_plugin_claude::discover` returns a `ClaudeCompatReport` whose
`unsupported: Vec<UnsupportedItem>` names every `commands/*.md`,
`skills/<name>`, or `agents/*.md` file that did NOT translate, every
ignored `commands/*.md`/`skills/<name>` frontmatter key (even on a file
that otherwise translated), every `agents/*.md` tool restriction dropped
for lacking a conway counterpart (even on a file that otherwise
translated), and every unmapped hook event — each with its own reason,
never a single "N things skipped" count. `/plugin` (the TUI listing)
surfaces the same names on the directory's own row, bounded to a few names
with an honest "+N more" tail for a very large directory rather than an
unbounded line or a silent truncation with no indication anything was cut.

## Foreign frontmatter is read permissively, deliberately

Every file this layer reads (`plugin.json`, `.mcp.json`, `hooks.json`) is
parsed as a `serde_json::Value` and only the fields actually used are read
— an unrecognized Claude Code key is simply never looked at, never a
`deny_unknown_fields`-style hard failure. **This is deliberately NOT how
conway's own `.conway/skills`/`.conway/agents` frontmatter is parsed** —
`crates/conway/src/skills.rs`/`agents.rs` reject an unknown key outright
for the operator's OWN `.conway/skills`/`.conway/agents` root, and that
strictness is untouched: it catches an operator's own typo in a file
conway itself defines the shape of, which a Claude Code plugin author's
file is not. A THIRD-PARTY root (what `with_extra_skill_dir`/
`with_extra_agent_dir` add) is different: both loaders now try that strict
shape first, then fall back to a permissive one tolerant of real Claude
Code conventions (no `name` key, `tools:` as a comma-separated string,
...) — see this page's own "What actually reaches a running process"
section, above, for the full shape.

## Trust — read this before you name a directory

**No new trust mechanism.** Everything a `[plugins].claude_compat[]`
entry's directory declares — every MCP server's own `command` — runs with
your own privileges, unsandboxed, the identical footing
[`mcp.md`](mcp.md)/[`subprocess-plugins.md`](subprocess-plugins.md) already
establish. Naming a directory here is exactly as trusted as naming a
command directly. See [`trust-and-security.md`](trust-and-security.md) for
the fuller argument.

## What conway does NOT do here

- **No downloading, ever, in THIS crate.** `conway_plugin_claude` reads a
  directory already on the operator's own filesystem; nothing in it makes a
  network call (`crates/conway-plugin-claude` depends on no HTTP client of
  any kind), and this page's own scope stays a directory an operator
  already has. **A sibling item now fetches one for you** —
  [`marketplace.md`](marketplace.md) browses a marketplace and installs a
  plugin's files into conway's own plugin store, then writes the exact
  `[plugins].claude_compat[]` entry this page describes, pointing at where
  it landed. That item does not change anything on THIS page: an installed
  marketplace plugin is, on disk and in `settings.json`, indistinguishable
  from a directory the operator cloned or typed the path to by hand — same
  entry shape, same read-at-runtime translation, same trust footing.
- **No config writer, in THIS crate.** `conway_plugin_claude` itself never
  writes `settings.json` — delete the `[plugins].claude_compat[]` entry
  yourself and the translation vanishes; nothing here was ever persisted by
  this crate. This is the "read-at-runtime, not translate-and-write"
  decision, argued in full in `conway_plugin_claude`'s own crate-level doc:
  a translate-and-write approach would need a real array-entry config
  writer, which did not exist when this item shipped
  (`crates/conway/src/config/writer.rs` used to patch one id via a
  hand-rolled text edit only, never parse-and-reserialize an array of
  objects). **That writer exists now** (`conway::config::
  set_claude_compat_entry`, [`marketplace.md`](marketplace.md)'s own doc) —
  built for the marketplace-install item, which needed to write an `{id,
  dir}` object into this exact array. `conway_plugin_claude` itself still
  calls no writer of any kind; the marketplace item's own CLI wiring
  (`crates/conway-cli/src/tui/app/marketplace.rs`) is what calls it, kept
  entirely outside this crate.
