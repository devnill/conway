# Installing a plugin from a marketplace

**Two manifest shapes are understood: conway's own, and a real, published
Claude Code marketplace — and a real Claude Code marketplace itself comes
in two `source` shapes.** Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR` (ruling
2026-08-29) closed the gap this page used to carry as a warning: an
operator who passed a real `https://github.com/<owner>/<repo>` Claude Code
marketplace got a `serde_json` parse error about GitHub's own HTML, because
conway's manifest reader understood only its own format and never resolved
a repository URL to the document Claude Code actually keeps
(`.claude-plugin/marketplace.json`). The very next operator attempt —
installing ideate's OWN marketplace, hours later — found a THIRD manifest
shape and a gap in the repository-URL resolution the first item had only
half-wired; board item `01M1A9J9C9YRH3YPTGD335HZPZ` closed those:

- **A repository URL is now actually resolved, not merely diagnosed.**
  `/plugin install https://github.com/<owner>/<repo> <plugin>` reads
  `.claude-plugin/marketplace.json` from inside it directly — conway GETs
  the resolved raw-content URL itself, so an operator pointed at a
  repository page never sees one at all. **A bare `<owner>/<repo>` GitHub
  shorthand — Claude Code's own `/plugin marketplace add owner/repo` shape
  — resolves identically.** See "Passing a repository URL or shorthand",
  below.
- The real schema parses: `owner`/`metadata` top-level objects are
  tolerated, and a `plugins[]` entry identified by `name` (rather than
  `id`) naming a `source` (rather than a `files` map) is accepted
  alongside conway's own shape — see "The two manifest formats", below.
- A `git-subdir`/`github` source actually FETCHES, by invoking the system
  `git` binary (never a git library — see "Fetching a git-sourced entry",
  below).
- **A plain-STRING `source` (`"./"`, meaning "this repository IS the
  plugin") now fetches too** — ideate's own real marketplace uses exactly
  this shape, resolved against whichever repository the marketplace itself
  was reached through. See "A relative source", below.
- **No internal error text reaches an operator on any of the above paths.**
  An `owner/repo` shorthand used to reach `reqwest`'s own request-builder
  failure and surface literally as "builder error"; it no longer does.

`crates/conway-plugin-marketplace/tests/claude_code_manifest.rs` and
`tests/ideate_manifest.rs` assert acceptance against real, published
manifests (the second one committed the same day this gap was found), so
these claims cannot quietly go stale the way the old warning eventually
would have.

The network-reaching half of the plugin feature (board items
`01M0VR96Y87FF2BVNTBSC6GEYR` and `01M0Y6RYZA94BK6YXJ7X8TNEGR`), shipped by
`crates/conway-plugin-marketplace` and wired at
`crates/conway-cli/src/tui/app/marketplace.rs`. Depends on
[`claude-compat.md`](claude-compat.md) — an installed marketplace plugin is,
on disk and in `settings.json`, an ordinary
`[plugins].claude_compat[]` entry, nothing more — and on
[`trust-and-security.md`](trust-and-security.md) for the trust ruling this
page does not re-argue, including the git-cloning-specific note that
page's "Fetching a git-sourced entry is still a network trust boundary"
section adds.

## What this is, in one sentence

`conway` fetches a marketplace's manifest over HTTP, fetches a chosen
plugin's declared files into its own plugin store, and writes a
`[plugins].claude_compat[]` entry naming where it landed — the three pieces
of machinery [`claude-compat.md`](claude-compat.md) explicitly says it does
NOT have ("No downloading, ever" / "No config writer"): this page's item is
where both now exist, layered on top of that read-at-runtime translation
rather than replacing it.

## The two manifest formats

**Conway's own — a files-map entry, identified by `id`:**

```json
{
  "name": "acme-marketplace",
  "description": "Acme's internal conway plugins",
  "plugins": [
    {
      "id": "acme-tools",
      "name": "Acme Tools",
      "description": "Search and lookup tools for Acme's internal index",
      "version": "1.0.0",
      "files": {
        ".claude-plugin/plugin.json": "https://example.com/acme-tools/plugin.json",
        ".mcp.json": "https://example.com/acme-tools/mcp.json"
      }
    }
  ]
}
```

`files` maps a relative path inside the installed plugin directory to the
URL its bytes are fetched from — **deliberately not a single archive URL**.
This shape never extracts an archive (`.tar.gz`/`.zip`): that would be a
genuinely new dependency (`tar`/`zip`, neither of which is in this
workspace's lock), and a symlink inside an extracted archive pointing
outside the extraction root is a real safety surface a first cut should
not take on casually. Fetching each file individually means there is no
archive-extraction step, and therefore no symlink-in-an-archive class of
attack to defend against at all for this shape — see
`conway-plugin-marketplace/Cargo.toml`'s own doc for the full argument.
This is **kept, not the only shape understood**: it is what lets a
conway-native marketplace exist with no git remote of its own.

**A real, published Claude Code marketplace — identified by `name`,
naming a `source` instead of `files`:**

```json
{
  "name": "marketplace",
  "owner": { "name": "Dan Singer" },
  "metadata": { "description": "A Claude Code plugin marketplace", "version": "3.0.11" },
  "plugins": [
    {
      "name": "beepboop",
      "source": { "source": "git-subdir", "url": "https://github.com/devnill/beepboop", "path": "plugin" },
      "description": "Plays sounds on Claude Code hook events",
      "version": "1.4.0"
    },
    {
      "name": "ideate",
      "source": { "source": "github", "repo": "ideate-ai/ideate" },
      "description": "a composable process framework for Claude Code",
      "version": "3.0.0"
    }
  ]
}
```

(Verbatim, real bytes: `crates/conway-plugin-marketplace/tests/fixtures/
claude-code-marketplace.json`, fetched from `devnill/claude-marketplace`
2026-08-26.) `owner`/`metadata` are read but not required, and neither
object is `#[serde(deny_unknown_fields)]` — any field of either beyond what
conway reads is simply ignored, the same "foreign format, read
permissively" posture [`claude-compat.md`](claude-compat.md) already uses
for every file IT reads. A `plugins[]` entry's own `#[serde(deny_unknown_
fields)]` is dropped too, for the identical reason: this is Claude Code's
schema, not conway's, so an unrecognized field is never looked at rather
than refused. Only conway's own top-level container fields
(`name`/`description`/`owner`/`metadata`/`plugins`) stay strict, since a
typo there is far more likely a conway-native marketplace author's mistake
than an unmodeled corner of Claude Code's schema.

`source` is a tagged family. Two kinds fetch, both git-based:

- **`git-subdir`** — a repository URL plus the subdirectory inside it that
  is this plugin's own root.
- **`github`** — an `owner/repo` pair; the whole repository is this
  plugin's own root. conway builds `https://github.com/<repo>.git` itself
  — the `repo` string is never passed to `git` as a URL directly.

**Any other `source` kind parses (so browsing a marketplace that lists one
still works) but refuses BY NAME the moment an install is attempted** —
most plausibly one requiring archive extraction, which this crate still
never adds (`Cargo.toml`'s own doc has the up-to-date argument).

### A relative source — `"source": "./"`

Board item `01M1A9J9C9YRH3YPTGD335HZPZ`: ideate's own real marketplace
(`https://github.com/ideate-ai/ideate`, fetched 2026-08-30, committed as
`crates/conway-plugin-marketplace/tests/fixtures/ideate-marketplace.json`)
names its `ideate` entry with `"source": "./"` — a plain JSON STRING, not
an object naming `git-subdir`/`github`:

```json
{
  "name": "ideate-marketplace",
  "plugins": [
    { "name": "ideate", "source": "./", "version": "3.2.2" }
  ]
}
```

This is likely the COMMONEST real-world shape, not an edge case: a
repository that publishes a marketplace listing only itself has no reason
to spell out an object pointing back at its own clone URL when a bare
`"./"` says the same thing more simply. conway's manifest reader used to
call the object-shaped lookup on this value unconditionally, which — given
a *string* rather than an object — looked for a `source` field *inside*
the string and reported `missing field `source``, an accurate-sounding but
useless error about the exact value it was trying to read.

A relative source names no git remote of its own — it means "the
repository the marketplace manifest was itself reached through". conway
resolves it against the literal `marketplace_url` the install was
requested with (a repository URL, an `owner/repo` shorthand, or a
`raw.githubusercontent.com` URL — all three recognized). **A marketplace
reached through anything else (an arbitrary HTTP host with no known git
remote) refuses this resolution by name** rather than guessing one —
conway has no general "what git remote served this HTTP URL" mechanism.

## Fetching a git-sourced entry

`git-subdir`/`github` sources are fetched by invoking the **system `git`
binary** — never a git library: `git2` never entered this workspace's
lock, matching the ruling's own words, "invoke the system git (no crate
enters the lock; refuse legibly if git is absent)". If `git` cannot be
run at all, the install is refused by name (`git_unavailable`), never a
confusing failure partway through a clone.

**A git checkout is untrusted content too, closed the same way as a
files-map entry's paths:**

- The clone runs bounded by a timeout (120s), so an unreachable remote is
  an ordinary reported failure, never a hang.
- A `git-subdir` URL that is not `http://`/`https://` is refused before
  `git` is ever invoked (`unsafe_git_url`) — git's OTHER transports
  (`ext::<command>`, `fd::<n>`, a bare local path) can run an arbitrary
  command or read an arbitrary local file, and the URL comes directly from
  the marketplace's own response, untrusted network input.
- Every entry in the checked-out plugin root is walked before a single
  byte is copied into conway's own plugin store; a symlink ANYWHERE in
  that tree refuses the whole install. A git checkout cannot be
  archive-traversed (there is no archive-extraction step for it either),
  but it is a narrower version of the same hazard class, not an absent
  one — see "Path safety", below, and
  [`trust-and-security.md`](trust-and-security.md)'s own note on this.

## Passing a repository URL or shorthand

Claude Code treats a marketplace as a git repository and reads
`.claude-plugin/marketplace.json` from inside it; `/plugin install`/`/plugin
marketplace add` also accept a bare `<owner>/<repo>` shorthand, assuming
GitHub. conway now does the identical resolution itself, for both forms,
**before ever sending a request** — board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`
was supposed to make hunting for the raw manifest URL unnecessary and only
wired the git-FETCH half (a `source` *inside* an already-parsed manifest);
`01M1A9J9C9YRH3YPTGD335HZPZ` finished the job for the top-level marketplace
URL itself:

- `https://github.com/<owner>/<repo>` (no deeper path — `/tree/...`,
  `/blob/...`, and similar are left unresolved, since the manifest's
  location is not mechanically derivable from those) resolves to
  `https://raw.githubusercontent.com/<owner>/<repo>/HEAD/.claude-plugin/marketplace.json`
  and conway GETs that directly. **An operator pointed at a repository page
  never sees one at all** — the "returned a web page, not a manifest"
  refusal this section used to describe as the normal outcome for this
  input no longer fires for it.
- `<owner>/<repo>` (no scheme) resolves the identical way.
- Anything already an absolute `http(s)://` URL — the overwhelmingly common
  case, a manifest document named directly — is used completely unchanged.
- Anything else is refused before a single request is attempted, naming
  exactly what conway accepts, rather than reaching the HTTP client with a
  string it cannot build a request from at all. **This used to surface as
  `reqwest`'s own internal "builder error" text** for an `owner/repo`
  shorthand used directly — an implementation detail, not a diagnosis. It
  no longer does, on any path: a defense-in-depth check at the actual HTTP
  call site catches the identical failure mode a second time, for anything
  this resolution step does not special-case (a per-file URL a marketplace's
  own `files` map declares, say).

conway's older "usually at ..." suggestion for a repository page mistaken
for a manifest still exists, but only fires for a shape this resolution
does not cover — a deeper GitHub path serving HTML, or a non-GitHub host
that answers with markup. This does not fetch the suggested URL to confirm
it exists — a wrong suggestion is worse than none, so it is offered as a
guess, never a claim.

Every failure this crate can hit is a named, typed error, never a panic and
never an unbounded read: offline (DNS failure, connection refused, or a
request that would otherwise hang — bounded by a 20-second client timeout so
"no network" is an ordinary reported failure, never a hang), a non-2xx HTTP
status, a response over a byte-size cap (checked against `Content-Length`
before any byte is read, and against the actual length either way, so a
server that lies about or omits its own length cannot bypass the cap), a
repository page mistaken for a manifest, an unresolvable/malformed URL, and
a malformed manifest (invalid JSON, or missing a required field) are each
their own `MarketplaceError` variant.

## Where a fetched artifact lives, and what installing writes

A plugin installed from **identity** `acme-tools` — its own `id` for a
conway-native entry, its own `name` for a real Claude Code entry (there is
no `id` on that shape) — lands at
`<config dir>/plugins/marketplace/acme-tools`, alongside the
`settings.json` the matching entry is written into (`~/.conway/`, or
`$CONWAY_CONFIG_DIR`) — and `settings.json` gains:

```json
{
  "plugins": {
    "claude_compat": [
      { "id": "acme-tools", "dir": "/home/you/.conway/plugins/marketplace/acme-tools" }
    ]
  }
}
```

The array-of-**objects** config writer this needed
(`conway::config::set_claude_compat_entry`) did not exist before this item —
[`claude-compat.md`](claude-compat.md)'s own "No config writer" section
explains why one is materially harder than the array-of-strings writer
`set_plugin_installed` already had (`plugins.install`): the operator's own
comments-as-keys convention, unrelated top-level sections, and their own key
order all have to survive the write, so it is built from the identical
hand-rolled scanner/splicer `set_plugin_installed` uses — never a
parse-mutate-reserialize round trip — matching an existing element by its
`id` member rather than by a bare string.

**Never a partial install.** Every declared file is fetched into a staging
directory first; only once every one has landed does the plugin's real
directory get replaced by a single `rename`. A failure partway through
(network, an unsafe path, too many files) removes the staging directory and
writes nothing to `settings.json` — the config write only happens after
installation has already fully succeeded. If the config write itself then
fails (a separate, later step), the just-fetched artifact is removed again
rather than left as an orphan nothing tracks.

## Path safety

The resolved identity and every relative path a plugin declares — in
`files`, or `git-subdir`'s own `path` — are validated before anything is
written: it must be a single, ordinary path component (non-empty, no
`/`/`\`, no `..`, not itself `.` or hidden); a declared file's own relative
path is refused outright — never partially accepted — unless every one of
its components is an ordinary path segment (no absolute path, no `..`, no
bare `.`, no Windows drive/prefix component). This is the "path traversal
in a plugin name (`../../etc`)" hazard, closed at the boundary, not
inferred past — the SAME check, one implementation, for both a files-map
entry's declared path and a git-subdir entry's own subdirectory.

**A git checkout adds one more check a files-map entry never needed**: a
symlink anywhere in the checked-out plugin root refuses the whole install,
before a single byte is copied out of the checkout. See "Fetching a
git-sourced entry", above.

## Uninstalling

Removes the `[plugins].claude_compat[]` entry from `settings.json` first,
then the artifact's own directory — leaving neither behind, and in that
order deliberately: an orphan directory with no config entry naming it is
harmless (an operator can find and remove it by hand); an orphan config
entry naming a directory that no longer exists would fail the *next* config
load outright (`claude_compat_plugins::install`'s own "an unresolvable entry
fails the whole build" contract) — a worse failure than a stray directory.
Removing a plugin never installed is a reported no-op, never an error.

## Triggering it: `/plugin install`/`/plugin uninstall`

Board item `01M0WB5W5DX844HSJQG3JP23X0` wired the interactive trigger the
listing item (`01M0VR5RCCB8NDGG2JEQW8X7XR`) had scoped out: `/plugin` still
opens the read-only listing bare, and now also takes an action —

```
/plugin install <manifest-url> <plugin-id>
/plugin uninstall <plugin-id>
```

**Smallest honest v1: a URL argument, not a browsable catalogue.** An
operator must already know the marketplace URL and the plugin id (from the
marketplace's own listing page, a README, etc.) — this cannot fetch a
marketplace's manifest just to let an operator pick from it interactively.
A browsable catalogue is a real, larger follow-up, not built here.

Both forms extend the existing `SlashCommand::Plugins` variant (the one
surface that owns plugin listing keeps owning plugin install/uninstall too,
rather than gaining a competing command) and reach the identical, already-
tested `App::apply_marketplace_install`/`apply_marketplace_uninstall`
methods described above — install is awaited directly inside `App::submit`
(not spawned off the render loop the way a plugin command or `/ask` is:
`app/marketplace.rs`'s own module doc explains why splitting the tested
method into a spawn-safe half was judged not worth the drift risk for a
fetch already bounded by the 20-second client timeout above). Uninstall
touches no network at all, so it always runs inline. Once installed, the
plugin appears in `/plugin`'s own listing with a `claude-compat` origin —
on the NEXT restart, exactly like a toggled compiled-in plugin or a hand-
edited `[plugins].claude_compat[]` entry (config changes here are never
live-applied to a running session).

`env`/`cwd` — the one thing this trigger needed that the ordinary slash-
command dispatch machinery (`commands::Host`) does not carry — are
resolved once, at `App::new`, from the same ambient sources the TUI
already reads for its history file and permission-file loading, and parked
as two `App` fields rather than threaded as a new `App::new` parameter
(which would have forced every one of this crate's ~40 existing `App::new`
call sites to name a value almost none of them touch).

## Trust — read this before installing anything

**Settled, not re-argued here.** A fetched artifact is checked against
**nothing** — no digest, no allow-list, no prompt — the identical footing
`[hooks].rules[].command` and every other plugin transport already have
(decision `01M0VS2M8FC25QYCATQ8PKQ73Y`, `trust-and-security.md`'s own
marketplace-ruling section). Fetching bytes over the network rather than an
operator copying them by hand changes nothing about that footing: "a
marketplace-sourced artifact is not safer than a command path the operator
typed by hand" is the ruling's own wording. **The operator's decision to
install IS the control point** — which is why installing discloses, in the
one transcript entry the action produces, the plugin's name, description,
version, and either every file it writes and the URL each comes from (a
files-map entry) or where it was cloned from (a git-sourced entry), plus
the destination directory and the unsandboxed-privilege caveat, all before
anything an operator would need to walk back is already running.

**Cloning a git repository is a network trust boundary too, stated
explicitly rather than left implicit** — `docs/plugins/trust-and-security.md`'s
own "Fetching a git-sourced entry is still a network trust boundary"
section is the fuller statement, cross-referenced back here: conway now
runs the system `git` binary against a URL a marketplace's own response
names, on the operator's own machine, with the operator's own credentials
and network access. This is not a NEW class of trust decision — it sits on
the identical footing the paragraph above already states for a files-map
fetch — but "conway now clones arbitrary third-party git repositories on
operator command" is a materially different-SOUNDING sentence from "conway
fetches a JSON file," and this project's documentation rule requires stating a new trust
surface explicitly rather than letting a reader infer it from an
unrelated paragraph.

## What this does NOT do

- **No browsable marketplace catalogue.** `/plugin install` takes a URL and
  a plugin id the operator already knows — it cannot fetch a marketplace's
  manifest and let an operator pick from a rendered list of what it offers.
  See "Triggering it", above.
- **No digest check, no allow-list, no trust prompt**, and none is coming
  from this mechanism — see "Trust", above.
- **No archive extraction, ever** — a `.tar.gz`/`.zip`-requiring source
  kind refuses by name rather than being fetched. **Git cloning IS now
  built** (`git-subdir`/`github` sources, via the system `git` binary) —
  see "The two manifest formats" and "Fetching a git-sourced entry",
  above; this bullet is corrected from an earlier "no git clone, no
  archive extraction" claim that named both, which stopped being true
  for the first half.
- **No non-interactive (`conway <plugin-id>.<command>`-style) trigger.**
  `/plugin install`/`uninstall` are TUI-only, like every other `/plugin`
  action — a non-interactive trigger was considered and rejected here: the
  CLI's `external_subcommand` surface dispatches an ALREADY-installed
  plugin's own command, a different question from installing one in the
  first place, and one-shot mode has no equivalent to `App`'s `env`/`cwd`
  fields or transcript to disclose into.
