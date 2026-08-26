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
unless a directory is named here.** A discovery failure — the directory
missing, a malformed `.claude-plugin/plugin.json`/`.mcp.json`, or the
translated MCP server itself failing discovery — fails the **whole build**,
naming the offending entry's own `id`, mirroring `mcp.md`'s own posture.

## What appears named, but does NOT run — read this before assuming otherwise

This is the equally-prominent half of this page, by design (nothing here
may claim to be reached that isn't).

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
- **`skills/<name>/SKILL.md` — still not imported by THIS layer.** conway's
  own skill loader (`crates/conway/src/skills.rs`) is no longer
  single-root: `skills::load_skill_defs_from_roots` (board item
  `01M0X1EH2GW5DKY9XD1EZ78S3F`) accepts an ORDERED list of roots — the
  operator's own `.conway/skills` always shadows a plugin's on a name
  collision, and a plugin root's own malformed `SKILL.md` is skipped rather
  than failing the whole load. Reading a second directory is now possible
  in the loader, and (board item `01M0XRE2N96ATHEXJ1617E133P`)
  `ConwayBuilder::with_extra_skill_dir` is a real, callable seam that reaches
  it through an actual build; this layer just does not yet CALL that seam
  with a plugin's own `skills/` directory, so nothing changes for an
  operator naming a `[plugins].claude_compat[]` entry today. That wiring —
  the translation step, not the loader capability or its seam — is a
  separate, deferred item. Every `skills/<name>/SKILL.md` directory found is
  still named in the report.
- **`agents/*.md` — still not imported by THIS layer,** for the identical
  reason: `agents::load_agent_defs_from_roots` exists and (board item
  `01M0XRE2N96ATHEXJ1617E133P`) `ConwayBuilder::with_extra_agent_dir` is a
  real, callable seam onto it, but this layer does not yet call it with a
  plugin's own `agents/` directory. Named in the report, never read for
  content. (An earlier version of this paragraph pointed at an
  `AgentsConfig::extra_dirs` config field; that field was retired in favor
  of the builder method above so the agents and skills halves of this
  capability stay symmetric — see `AgentsConfig`'s own doc.)
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
  registrations; a CALLER appends them into its own `[hooks].rules[]`
  before `ConwayBuilder::build`. **As shipped (board item
  `01M0XBZNBPXEESX8VNTJDKNG0J`), `conway-cli`'s own
  `[plugins].claude_compat[]` install path (`claude_compat_plugins.rs`)
  now performs that append**: naming a directory in `settings.json` gets
  you both its MCP servers running *and* its mapped hooks dispatching,
  with no hand-copying of `{event, matcher}` into your own
  `[hooks].rules[]` required. Every appended rule keeps `on_failure:
  "deny"` — the CLI never chooses a foreign plugin's own outage posture
  for you — and the CLI reports, on startup, which registered hooks *can
  deny* a real tool call (`pre_tool_use`) versus which are
  observation-only, so naming a directory here is never presented as
  merely "hooks registered." **This does not change the payload-shape
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
| `SubagentStart` | **approximate** | `child_spawned` | open |
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

**The approximate pair's own known divergence:** conway's `child_reported`
fires once per agent that has a parent, for both an ordinary completion
*and* a supervisor-synthesized terminal result (a panic, or a task still
unresponsive past its grace window) — whether Claude Code's own
`SubagentStop` fires for that second, synthesized case the same way is
unverified. Mapped, labelled, not chased further, per the operator ruling's
own best-effort-and-disclosed appetite; a beepboop smoke test is what
surfaces whether it actually bites in practice.

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
`unsupported: Vec<UnsupportedItem>` names every `commands/*.md`, every
`skills/<name>`, every `agents/*.md`, and every unmapped hook event, each
with its own reason — never a single "N things skipped" count. `/plugin`
(the TUI listing) surfaces the same names on the directory's own row,
bounded to a few names with an honest "+N more" tail for a very large
directory rather than an unbounded line or a silent truncation with no
indication anything was cut.

## Foreign frontmatter is read permissively, deliberately

Every file this layer reads (`plugin.json`, `.mcp.json`, `hooks.json`) is
parsed as a `serde_json::Value` and only the fields actually used are read
— an unrecognized Claude Code key is simply never looked at, never a
`deny_unknown_fields`-style hard failure. **This is deliberately NOT how
conway's own `.conway/skills`/`.conway/agents` frontmatter is parsed** —
`crates/conway/src/skills.rs`/`agents.rs` reject an unknown key outright,
and that strictness is untouched by this item: it catches an operator's
own typo in a file conway itself defines the shape of, which a Claude Code
plugin author's file is not.

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
