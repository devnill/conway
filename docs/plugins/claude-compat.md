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

**MCP server declarations, and only MCP server declarations.** A
directory's own `.mcp.json` (`{"mcpServers": {"<name>": {"command", "args",
"env"}}}`) is translated into a real `conway_plugin_mcp::McpPluginSpec` and
discovered through the identical `conway_plugin_mcp::McpPlugin::discover` ->
`ConwayBuilder::with_plugin` path an operator-authored `[plugins].mcp[]`
entry already uses. This is the one pairing that is a genuine structural
match — both sides are stdio JSON-RPC with a `command`/`env` declaration —
and it is the only kind this item wires to actually run.

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

- **`commands/*.md` — not wired.** conway now has a capability that could,
  in principle, submit a command file's own prompt into a session
  (`SessionHandle::prompt_command`) — but connecting a Claude Code command
  file to it is a separate, deferred item, not something this layer's own
  absence of the capability ever blocked. Every `commands/*.md` file found
  is named in the operator-visible report; none of them run anything.
- **`skills/<name>/SKILL.md` — not imported, at all.** conway's own skill
  loader (`crates/conway/src/skills.rs`) reads exactly one hardcoded root,
  `.conway/skills`; there is no mechanism to read a second directory. Making
  that multi-rooted is a real, separate change (a public config field
  widened, the loader touched, its own test coverage) that this item
  deliberately did not take on — a directory-read layer that imports MCP
  correctly and says plainly that skills are not yet translated is worth
  more than one that ships a partial, half-working skill import. Every
  `skills/<name>/SKILL.md` directory found is named in the report.
- **`agents/*.md` — not imported, at all,** for the identical reason:
  `AgentsConfig::dir` is a single `PathBuf`, not a list of roots to search.
  Named in the report, never read for content.
- **`hooks/hooks.json` — event names are matched, nothing is wired to
  dispatch.** `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, and
  `SessionStart` each have a same-named conway event
  (`pre_tool_use`/`post_tool_use`/`prompt_submitted`/`session_starting`);
  `Stop`, `SubagentStop`, `Notification`, `PreCompact`, and `SessionEnd`
  have none (`PreCompact` specifically because conway has no compaction
  mechanism yet — the one first-party capability still unbuilt). **Even a
  matched event name is reported, never auto-installed as a running
  `[hooks].rules[]` entry**: the two sides' JSON *payload* shapes differ
  even where the event name lines up (a Claude Code hook script reads
  `tool_name`/`tool_input` on stdin; conway's dispatcher sends its own
  `HookInvocation`/`HookEvent` shape), so a foreign script, handed conway's
  payload unmodified, would not understand it — silently wiring such a rule
  into a running session's dispatch table would be exactly the kind of
  "looks installed, never meaningfully answers" failure this whole layer
  exists to prevent. If you want a matched hook live, copy its `{event,
  matcher}` pairing into your own `[hooks].rules[]` and adapt the script to
  conway's payload shape yourself — recorded here, not automated.
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
