# Migrating from Claude Code's `settings.json`

You have a working `~/.claude/settings.json` and want a conway
`settings.json` that reflects the same choices. This page walks through
that, key by key, against a real file — not a synthetic example.

**Operator ruling, 2026-08-25: migrate only what fits conway's philosophy
cleanly. Where a plugin is the right home, say so.** This is **not a
compatibility layer**, and it must not become one. `INTENT.md` §2 and the
plugin tier's own rule — nothing runs unasked, the core stays agnostic —
mean importing another tool's configuration model wholesale is the
failure this page exists to avoid, not the goal it's chasing. Every key
below lands in exactly one of three buckets:

- **Maps** — an existing conway config surface expresses the same idea,
  under a different name and often a different syntax.
- **Plugin** — the right home is outside conway's core, and a board item
  already owns building it. Cited by id below.
- **Declined** — conway does not have this machinery, and the reasoning
  for not adding it is stated, not just asserted.

Two of the declined entries are **settled, not open questions**: see
"Already ruled" below before you read anything into their absence from
the rest of this page.

If you haven't set conway up at all yet, read
[`getting-started.md`](getting-started.md) first — this page assumes you
already have a `default_role`/`backends`/`roles` triple working and is
about carrying *policy* over, not about the first install.

## Already ruled — not reopened here

Two keys were, in an earlier draft of the item behind this page, open
questions for the operator to decide. Both are now settled
(`docs/vision/DESIGN-permission-modes.md` §9) and are **not** re-argued on
this page:

- **`env` — declined.** Conway's `[plugins].mcp[].env` is a scoped,
  additive env surface (an MCP server's own credentials, named
  explicitly), but a *global* env-injection key, the way Claude Code's
  `env` works, is not coming back — see the citation note at the bottom
  of this page for exactly which scoped surface this compares against.
  Commit `ae318f7` (and the predecessor
  it followed) removed the explicit `env: &HashMap` that used to thread
  through `main()`; `crates/conway/tests/config_isolation_guard.rs` exists
  specifically to keep that removal from being silently reintroduced. A
  global env key would be exactly that reintroduction, so it stays out.
- **`hooks.SessionEnd` — declined.** Not a candidate for growing conway's
  hook-event vocabulary. An earlier framing floated "file an item if the
  absence hurts" for this one; that framing is withdrawn.

## The real file, and what changed since the spec was written

This page was written against the operator's actual `~/.claude/
settings.json`, read fresh rather than trusted from an earlier pass. Two
things in the originating board item's own settings table turned out to
be stale once checked against the real file:

1. **`effortLevel: "high"` is present in the real file and was not in the
   spec's declined list at all.** It's bucketed below (declined — conway
   has no reasoning-effort/thinking-budget key at the `settings.json`
   level; see the table).
2. **`permissions.allow` does not map to `permissions.allowed_tools`.**
   The spec's table said it did. It doesn't, for anyone actually running
   the `conway` binary: `settings.json`'s `permissions.mode`/
   `allowed_tools`/`denied_tools` are consulted only by
   `gates::from_config`, the fallback `ConwayBuilder::build()` uses when
   *no* gate has been supplied — and both the TUI and `-p` one-shot mode
   **always** supply their own gate
   (`docs/permissions.md`'s "Permission modes" section says this
   explicitly). The field parses, sits in the file, and does nothing for
   an ordinary `conway` user. The actual target for a durable Claude-Code-
   style allow rule is a **`permissions.json`** file (project- or
   global-scoped) — a sibling of `settings.json`, not a key inside it. The
   worked example below uses the correct target throughout.

Everything else in the spec's table checked out.

## Bucket table

| Claude Code key | Bucket | Where it lands / why not |
| --- | --- | --- |
| `permissions.allow` (7 rules) | Maps — **but see above** | `~/.conway/permissions.json` (or the project one), not `settings.json`. Worked example below; only 1 of the 7 real rules actually survives the trip. |
| `model` | Maps | `default_role` + `roles.<alias>.chain`. Worked example below. |
| `enabledPlugins` (6) | Maps, per-entry (uneven) | `[plugins].claude_compat[]` for a directory-sourced one; `/plugin install <marketplace-repo-url> <plugin-name>` now installs the two GitHub-marketplace-sourced ones for real (board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`, 2026-08-29); no equivalent for the Claude-Code-official one. See "Plugins and marketplaces" below — this is the entry the spec's table most oversimplified. |
| `extraKnownMarketplaces` (3) | Maps, imperfectly | conway has no persistent "known marketplaces" registry at all — `/plugin install <url> <plugin-id>` names a manifest URL directly, every time (`docs/plugins/marketplace.md`, "Smallest honest v1: a URL argument, not a browsable catalogue"). A named, reusable marketplace alias is a Claude Code concept with no conway counterpart; see below. |
| `statusLine.command` | Plugin | Board item `01M0X500861X9035QJEA82F94K` — "A command-driven status line as a plugin." Conway's own `StatusLineConfig` is `{ fields: Vec<String> }` over a closed ten-variant vocabulary; a shell-command status line is opinionated output-formatting logic that belongs outside the core. |
| `permissions.defaultMode` ("auto") | Plugin | Board item `01M0X4YDNVP7TZ0PVSRJ0388SS` — "Plugin-declared permission modes." Conway ships exactly three modes (`Prompt`/`Plan`/`AutoAllow`, cycled with `/settings`); a fourth, "prompt me only in edge cases," is the item that would add a mode beyond those three, and it's explicitly a plugin's job, not the core's. |
| `env` | **Declined (settled)** | See "Already ruled," above. |
| `hooks.SessionEnd` | **Declined (settled)** | See "Already ruled," above. |
| `effortLevel` | Declined | No reasoning-effort or thinking-budget key exists in `settings.json` at any level. `RoleEntry`/routing carries a `params: SamplingParams` sub-table (`temperature`, `top_p`, `max_tokens`, `stop`, `seed`, `extra`), but `ConwayConfig::routing()` never populates it from anything in `settings.json` — the documented schema doesn't reach it (`crates/conway/src/config/schema.rs`'s own module doc says exactly this). Nothing to map to; this key evaporates on migration. |
| `tui` | Declined | conway has one TUI, not a `fullscreen`/other mode toggle. |
| `voice` / `voiceEnabled` | Declined | No voice input/output surface exists. |
| `verbose` | Declined | No global verbosity flag; conway's transcript detail is not gated by a single switch this shape. |
| `teammateMode` | Declined | conway has no "teammate" concept (tmux-pane-per-agent orchestration) at the config level. |
| `inputNeededNotifEnabled` | Declined | No desktop/OS notification integration. |
| `agentPushNotifEnabled` | Declined | Same as above. |
| `skipAutoPermissionPrompt` | Declined | No such toggle — conway's permission modes (`Prompt`/`Plan`/`AutoAllow`) already express "skip the prompt," and `AutoAllow` is the honest name for what this key would otherwise silently rename. |
| `autoMode.environment` (24 entries) | Declined | This is Claude Code's ambient trust/environment briefing (org policy, protected branches, sensitive-data heuristics, etc.), injected into every session. Conway has no equivalent ambient-context injection surface, and per the settled `env` ruling above, adding one is not on the table — it would be the identical global-injection shape under a different name. |

**Naming each declined key individually, rather than a summary "and
everything else conway doesn't have," matches the compatibility layer's
own established posture** (`docs/plugins/claude-compat.md`): name,
individually, everything you cannot carry over. A guide that silently
omits a key an operator relies on is worse than one that says plainly
"conway has no equivalent."

## Worked example 1: the real seven `permissions.allow` rules

These are the operator's actual entries, in file order:

```json
"permissions": {
  "allow": [
    "Bash(for id in af2eb7f6bb2e323aa a68747af77c8dc099)",
    "Bash(do echo \"=== $id ===\")",
    "Read(//private/tmp/claude-501/-Users-dan-code-ideate2/tasks/**)",
    "Read(//Users/dan/code/ideate2/**)",
    "Bash(done)",
    "Bash(ls -la /Users/dan/code/ideate2/agents/*.md)",
    "Bash(for id in a9d6e4f1c1e0ea483 a28112d10db1c775f)"
  ]
}
```

Translating these one at a time is the honest part of this page — most of
them do not survive the trip, and the reason is not "not implemented
yet," it's a deliberate design choice on conway's side.

**Rules 1, 2, 5, 7 — the `for`/`do`/`done` loop fragments.** These are not
a policy at all; they're Claude Code's own remembered-exact-text capture
of three lines from one past shell command, kept as three separate
"always allow" entries. **None of the four has a conway equivalent, of
any kind, because a durable allow grant does not exist for `bash` in
conway — not a narrower one, none**
(`docs/permissions.md`'s Limits section: "A durable pattern grant does
not exist for `bash` (or any tool whose rendering is a shell command) at
all"). Writing `{"select":{"tools":["bash"]},"when":{"command_prefix":
"for id in"},"then":"allow"}` into `permissions.json` parses, installs,
and authorizes **nothing** — the loader emits an operator-visible notice
naming it as inert, on purpose, because an earlier version of this
mechanism tried to be clever about which shell commands were safe to
pattern-match and measured a 68% false-positive rate doing it. What's
left for a `bash` call in conway: allow it once (`[y]`) or grant the exact
call for the session (`[a]`), narrow it with a `deny`/`prompt` rule (those
still work), or confine what it can reach with `--root`. There is no
config-file equivalent of "always allow this exact command, forever."
**Declined — not because conway lacks the feature, but because conway
removed it deliberately and documents why.**

**Rule 6 — `Bash(ls -la /Users/dan/code/ideate2/agents/*.md)`.** Same
fate as above, for the same reason: it's a `bash` allow rule, and no
`bash` allow rule ever authorizes anything in conway. **Declined.**

**Rule 3 — `Read(//private/tmp/claude-501/-Users-dan-code-ideate2/tasks/**)`.**
This is a `Read` grant, and `Read` allow grants *do* work in conway — but
this particular one is scoped to a path under `/private/tmp/claude-501/…`,
an ephemeral per-session scratch directory tied to one specific, already-
finished agent session. Conway's `paths_under` rule canonicalizes its
prefix against the filesystem at load time; a prefix that doesn't resolve
on disk either fails to register (`deny`/`prompt`) or silently never
matches (`allow`) — either way, carrying this rule forward buys nothing,
because the directory it names is very likely already gone. **Declined —
not a category loss, a "this was already stale" observation.** If you
have a *durable* scratch-file location you read from repeatedly, name
that directory instead; don't carry an old ephemeral one forward.

**Rule 4 — `Read(//Users/dan/code/ideate2/**)`.** This is the one rule
that survives cleanly: a real, persistent project directory, granted read
access. Its conway shape, in the structured `rules` array (the flat
`read:*` form has no way to scope by path — only the structured form
does):

```json
// ~/.conway/permissions.json  (or $CONWAY_CONFIG_DIR/permissions.json)
{
  "rules": [
    {
      "select": { "tools": ["read"] },
      "when": { "paths_under": "/Users/dan/code/ideate2" },
      "then": "allow"
    }
  ]
}
```

This is a *global* file, so it needs no trust ceremony — trust exists to
stop a cloned repository's own `.conway/permissions.json` from silently
granting itself permissions the moment you open it; a file that is your
own, at `~/.conway/` (or `$CONWAY_CONFIG_DIR`), is exempt by design
(`docs/permissions.md`'s Trust section: "asking you to trust your own
file is theater").

**Net result: 1 of the operator's real 7 `permissions.allow` rules
translates. 6 do not** — 4 because they were never a real policy (loop
fragments), 1 because `bash` allow grants are categorically inert in
conway, and 1 because it was already pointing at a directory that no
longer exists. That ratio is the honest headline for this section, not
an edge case.

## Worked example 2: `model`

```json
"model": "opus[1m]"
```

Conway has no single `model` key — a model choice is `default_role`
naming a role, and that role's `chain` naming one or more
`"backend/model"` pairs to try in order:

```json
{
  "default_role": "coder",
  "backends": {
    "anthropic": {
      "kind": "anthropic",
      "api_key_env": "ANTHROPIC_API_KEY"
    }
  },
  "roles": {
    "coder": { "chain": ["anthropic/claude-opus-4-1[1m]"] }
  }
}
```

The `[1m]` suffix convention is real in conway (`docs/providers.md`'s
`k3[1m]` example: "The `[1m]` suffix is literal — part of the id the
provider expects"), but **the exact model id string is provider-specific
and this page does not assert one** — check what your `anthropic` backend
actually accepts (or your `.conway/models.json`, if you maintain one)
before trusting the literal string above.

## Plugins and marketplaces — the entry the spec's table oversimplified

The real `enabledPlugins`/`extraKnownMarketplaces` pair is six plugin
entries against three marketplace sources:

```json
"enabledPlugins": {
  "rust-analyzer-lsp@claude-plugins-official": true,
  "beepboop@marketplace": true,
  "cyberbrain@marketplace": true,
  "superpowers@claude-plugins-official": false,
  "kg@kg-marketplace": true,
  "ideate@ideate-marketplace": true
},
"extraKnownMarketplaces": {
  "marketplace": { "source": { "source": "github", "repo": "devnill/claude-marketplace" } },
  "kg-marketplace": { "source": { "source": "directory", "path": "/Users/dan/code/knowledge-graph/plugin" } },
  "ideate-marketplace": { "source": { "source": "github", "repo": "ideate-ai/ideate" } }
}
```

conway has two, unrelated ways to bring in an already-Claude-Code-shaped
plugin, and neither one has a "known marketplaces registry" the way
Claude Code does:

- **A directory already on disk** → `[plugins].claude_compat[]`, naming
  `{id, dir}` directly. **This is the only one of the six entries with a
  clean translation**: `kg@kg-marketplace` resolves, via
  `extraKnownMarketplaces.kg-marketplace`, to a plain local directory
  (`/Users/dan/code/knowledge-graph/plugin`), so it becomes:

  ```json
  { "plugins": { "claude_compat": [
    { "id": "kg", "dir": "/Users/dan/code/knowledge-graph/plugin" }
  ] } }
  ```

  **Verified against the actual directory, not assumed**: it contains
  `.claude-plugin/`, `commands/`, and `hooks/` — **no `.mcp.json`**.
  `docs/plugins/claude-compat.md` is explicit that only `.mcp.json` server
  declarations are wired to actually run; `commands/*.md` and
  `hooks/hooks.json` entries are named in the operator-visible discovery
  report and never executed. So this translation is honest about its
  ceiling: adding this entry to `settings.json` gets you a report line
  naming what `kg` declares, not a running equivalent of whatever it did
  inside Claude Code.

- **A marketplace fetch** → `/plugin install <manifest-url> <plugin-id>`,
  which then writes its own `[plugins].claude_compat[]` entry for you
  (`docs/plugins/marketplace.md`). **This section used to say conway could
  not reach `beepboop@marketplace`, `cyberbrain@marketplace`, or
  `ideate@ideate-marketplace` at all without someone standing up a
  conway-shaped manifest URL first. Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`
  (2026-08-29) corrected that**: conway's manifest reader now understands
  a real, published Claude Code marketplace directly — `owner`/`metadata`
  tolerated, a `name`+`source`-identified entry accepted, and a
  `git-subdir`/`github` source fetched by invoking the operator's own
  `git` binary. `extraKnownMarketplaces.marketplace` names
  `devnill/claude-marketplace` as a GitHub repo; conway resolves that
  SAME repo URL to `.claude-plugin/marketplace.json` (the document Claude
  Code itself reads), so:

  ```
  /plugin install https://github.com/devnill/claude-marketplace beepboop
  /plugin install https://github.com/devnill/claude-marketplace cyberbrain
  ```

  install `beepboop`/`cyberbrain` for real, identified by `name` (there is
  no conway-native `id` on a real Claude Code entry) and fetched via git,
  not a conway-authored `files` map. `ideate@ideate-marketplace` is the
  same shape one level removed: `extraKnownMarketplaces.ideate-marketplace`
  names `ideate-ai/ideate` itself as the marketplace repo, so
  `/plugin install https://github.com/ideate-ai/ideate ideate` is the
  equivalent lookup against THAT repo's own manifest.

  **What is still a real gap, stated precisely rather than implied
  away**: `git` must actually be on the operator's own `PATH` (refused by
  name, `git_unavailable`, if not); a source kind requiring archive
  extraction still refuses by name rather than installing (not a
  limitation either of the two marketplaces above hits, since both
  publish only `git-subdir`/`github` sources); and there is still no
  persistent "known marketplaces" registry or alias — the full repository
  URL is typed every time, exactly as `extraKnownMarketplaces` (3) above
  already notes.

- **`rust-analyzer-lsp@claude-plugins-official`** — sourced from Claude
  Code's own built-in official marketplace, with no directory or URL
  named anywhere in the operator's file at all. No conway equivalent
  exists to point at. This one is a straightforward loss, not a
  translation gap: conway has no LSP-status integration surface for this
  plugin to attach to even if the artifact were reachable.

- **`superpowers@claude-plugins-official`: `false`** — disabled in Claude
  Code. conway's `[plugins].claude_compat[]` has no enable/disable toggle
  — an entry's presence *is* "installed," so the conway equivalent of "an
  operator wanted this at some point but currently doesn't want it" is
  simply: don't write the entry. There's nothing to migrate for a
  disabled plugin either way.

## Full worked target `settings.json`

Assembling only the entries that actually translate (this is what "no new
core config keys" looks like in practice — every key below already
existed in `schema.rs` before this page was written):

```json
{
  "default_role": "coder",
  "backends": {
    "anthropic": {
      "kind": "anthropic",
      "api_key_env": "ANTHROPIC_API_KEY"
    }
  },
  "roles": {
    "coder": { "chain": ["anthropic/claude-opus-4-1[1m]"] }
  },
  "tools": {
    "builtin_plugins": ["conway.fs", "conway.subagent", "conway.report", "conway.shell"]
  },
  "plugins": {
    "claude_compat": [
      { "id": "kg", "dir": "/Users/dan/code/knowledge-graph/plugin" }
    ]
  }
}
```

(`tools.builtin_plugins` gains `"conway.shell"` because the operator's
real file leans on `bash` heavily — six of seven `permissions.allow`
entries name it — and bash is off by default in conway; see
[`getting-started.md`](getting-started.md#enabling-bash-shell-commands).
Carrying the *intent* "I use bash" forward requires this line even though
none of the individual `Bash(...)` rules themselves survive.)

Plus the separate `~/.conway/permissions.json` from worked example 1,
above.

## What you lose

This is most of the point of this page, not a footnote:

- **Every durable `bash` allow grant, without exception.** conway
  deliberately does not offer a way to say "always allow this shell
  command" — six of the operator's seven `permissions.allow` entries were
  exactly that, and none of them come back in any form. The nearest
  substitutes (`[y]`/`[a]` per-call, `deny`/`prompt` narrowing,
  `--root` confinement) each give up something the original rule had:
  either you're asked again, or you're accepting a boundary broader than
  the rule you started with.
- **A shell-command status line.** `statusLine.command` is filed as its
  own plugin item, not built yet — until it lands, conway's status line
  is the fixed ten-field vocabulary described in `interactive.md`, not
  arbitrary shell output.
- **A fourth permission mode.** `permissions.defaultMode` beyond conway's
  three (`Prompt`/`Plan`/`AutoAllow`) is filed as its own plugin item, not
  built yet.
- **Global env injection.** Settled declined — see "Already ruled." If
  you relied on `env` to set something every session needs, that has to
  move to wherever you launch `conway` from (your shell profile, a
  wrapper script) — conway will not do it for you at the config layer.
- **A `SessionEnd` hook.** Settled declined. If you had a
  `SessionEnd`-driven integration (the operator's real file runs a
  knowledge-graph ingest script on session end), there is no conway hook
  event to attach it to, and none is coming.
- **Reasoning-effort control from `settings.json`.** `effortLevel` has no
  landing spot at all — not maps, not plugin, just absent from the config
  surface at this layer.
- **TUI presentation preferences, notifications, and voice** (`tui`,
  `verbose`, `teammateMode`, `voice`/`voiceEnabled`,
  `inputNeededNotifEnabled`, `agentPushNotifEnabled`,
  `skipAutoPermissionPrompt`) — none of these surfaces exist in conway.
- **The 24-entry `autoMode.environment` trust briefing.** No ambient
  environment-briefing injection surface exists, and per the `env` ruling
  above, none is being added under a different name either.
- **A reusable marketplace alias.** `extraKnownMarketplaces` lets you
  register a source once and refer to it by short name repeatedly;
  conway's `/plugin install` takes a full manifest URL every time, with no
  persistent registry to shorten that.
- **Most of a Claude Code plugin, even for the one directory that does
  translate.** `kg`'s `commands/` and `hooks/` are named in a report, not
  run — only an `.mcp.json` (which `kg` doesn't have) would actually wire
  anything.

## Verification

**Followed against a scratch config directory, not asserted from the
table above.** With `CONWAY_CONFIG_DIR` pointed at an empty temporary
directory, I wrote the `settings.json` and `permissions.json` this page
describes and read them back:

```console
$ export CONWAY_CONFIG_DIR=<scratch dir>/conway-migrate-verify
$ mkdir -p "$CONWAY_CONFIG_DIR"
$ cat > "$CONWAY_CONFIG_DIR/settings.json" <<'EOF'
{ ... the full worked target above, verbatim ... }
EOF
$ cat > "$CONWAY_CONFIG_DIR/permissions.json" <<'EOF'
{ "rules": [ { "select": {"tools":["read"]}, "when": {"paths_under": "/Users/dan/code/ideate2"}, "then": "allow" } ] }
EOF
$ python3 -m json.tool "$CONWAY_CONFIG_DIR/settings.json" >/dev/null && echo settings-ok
$ python3 -m json.tool "$CONWAY_CONFIG_DIR/permissions.json" >/dev/null && echo permissions-ok
settings-ok
permissions-ok
```

(Actually run, not transcribed from memory: both files were written to a
temporary `$CONWAY_CONFIG_DIR`, parsed back with `python3 -m json.tool`,
and printed to confirm the bytes on disk match what this page shows —
`settings-ok`/`permissions-ok` above is the real output, not illustrative.)

Both files are well-formed JSON and every key/shape in them was checked
by hand against `crates/conway/src/config/schema.rs` (`ConwayConfig`,
`BackendEntry`, `RoleEntry`, `ToolsConfig`, `PluginsConfig`,
`ClaudeCompatPluginEntry` — every one of these structs carries
`#[serde(deny_unknown_fields)]`, so a wrong key name is a hard load
error, not a silent no-op) and `crates/conway-core/src/
permission_pattern.rs` (`Select`, `When`, `Then`, `Rule`,
`PermissionFile`) field-for-field.

**What this does not verify, and could not without the binary**: this
lane runs no `cargo build`/`test`/`check` — disk is the constraint for
this wave, and this item is scoped docs-only. I did not, and could not,
actually run `conway` against this config to confirm it loads, resolves
the role, or that the `permissions.json` rule installs and matches a real
`read` call. The struct-level cross-check above is the honest ceiling of
what this page's own verification claims: syntactically and structurally
correct against the schema as read, not exercised end to end. An operator
following this page should expect the first real run to be where any
remaining surprise — a typo this cross-check missed, a model id string
that isn't quite right — actually surfaces.

## Table entries the spec asked to be checked, and what changed

For the record, since the originating board item asked this to be
verified rather than trusted:

- **Stale**: the spec's `permissions.allow (7 rules)` → `permissions.allowed_tools + pattern rules` row. Corrected above — the real target is `permissions.json`, and `settings.json`'s own `permissions.allowed_tools` is inert for anyone running the `conway` binary.
- **Missing**: `effortLevel` was present in the real file and absent from the spec's declined list. Added above.
- **Confirmed accurate**: the `env` and `hooks.SessionEnd` decline reasons, the `statusLine.command` and `permissions.defaultMode` plugin citations (item ids filled in above — the spec named the mechanisms but not the ids), the `tui`/`verbose`/`teammateMode`/`voice`/`voiceEnabled`/`inputNeededNotifEnabled`/`agentPushNotifEnabled`/`skipAutoPermissionPrompt`/`autoMode.environment` (24 entries, confirmed by count) declined list, and the seven-rule count for `permissions.allow`.
- **One citation imprecision, not a ruling change**: the spec cites conway's scoped `env` as "`[hooks].env` (`schema.rs:1020`)." Line 1020 is real, but it's `McpPluginEntry.env` (`[plugins].mcp[].env`), not a `[hooks]` field — `HookEntry` itself has no `env` field at all; its own doc comment says so directly ("a `[hooks].rules[]` entry ... has no `env` field at all, and its command inherits the parent's environment whole, with no additive mechanism of any kind"). This does not change the `env` ruling itself, which stands regardless of which scoped surface it's compared against.
