# Installing a plugin from a marketplace

The network-reaching half of the plugin feature (board item
`01M0VR96Y87FF2BVNTBSC6GEYR`), shipped by `crates/conway-plugin-marketplace`
and wired at `crates/conway-cli/src/tui/app/marketplace.rs`. Depends on
[`claude-compat.md`](claude-compat.md) — an installed marketplace plugin is,
on disk and in `settings.json`, an ordinary
`[plugins].claude_compat[]` entry, nothing more — and on
[`trust-and-security.md`](trust-and-security.md) for the trust ruling this
page does not re-argue.

## What this is, in one sentence

`conway` fetches a marketplace's manifest over HTTP, fetches a chosen
plugin's declared files into its own plugin store, and writes a
`[plugins].claude_compat[]` entry naming where it landed — the three pieces
of machinery [`claude-compat.md`](claude-compat.md) explicitly says it does
NOT have ("No downloading, ever" / "No config writer"): this page's item is
where both now exist, layered on top of that read-at-runtime translation
rather than replacing it.

## The manifest format

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
No git clone, no `.tar.gz`/`.zip` extraction: either would be a genuinely new
dependency (`git2`/`tar`/`zip`, none of which are in this workspace's lock),
and a symlink inside an extracted archive pointing outside the extraction
root is a real safety surface a first cut should not take on casually.
Fetching each file individually means
there is no archive-extraction step, and therefore no symlink-in-an-archive
class of attack to defend against at all — see
`conway-plugin-marketplace/Cargo.toml`'s own doc for the full argument.
`#[serde(deny_unknown_fields)]` throughout: a marketplace response is
untrusted network input, and this is a format conway itself defines the
shape of (unlike `claude-compat.md`'s own deliberately permissive parsing of
a FOREIGN file format).

Every failure this crate can hit is a named, typed error, never a panic and
never an unbounded read: offline (DNS failure, connection refused, or a
request that would otherwise hang — bounded by a 20-second client timeout so
"no network" is an ordinary reported failure, never a hang), a non-2xx HTTP
status, a response over a byte-size cap (checked against `Content-Length`
before any byte is read, and against the actual length either way, so a
server that lies about or omits its own length cannot bypass the cap), and a
malformed manifest (invalid JSON, or missing a required field) are each
their own `MarketplaceError` variant.

## Where a fetched artifact lives, and what installing writes

A plugin installed from id `acme-tools` lands at
`<config dir>/plugins/marketplace/acme-tools` — alongside the
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

`plugin_id` and every relative path a plugin declares in `files` are
validated before anything is written: a plugin id must be a single, ordinary
path component (non-empty, no `/`/`\`, no `..`, not itself `.` or hidden);
a declared file's own relative path is refused outright — never partially
accepted — unless every one of its components is an ordinary path segment
(no absolute path, no `..`, no bare `.`, no Windows drive/prefix component).
This is the "path traversal in a plugin name (`../../etc`)" hazard, closed
at the boundary, not inferred past.

## Uninstalling

Removes the `[plugins].claude_compat[]` entry from `settings.json` first,
then the artifact's own directory — leaving neither behind, and in that
order deliberately: an orphan directory with no config entry naming it is
harmless (an operator can find and remove it by hand); an orphan config
entry naming a directory that no longer exists would fail the *next* config
load outright (`claude_compat_plugins::install`'s own "an unresolvable entry
fails the whole build" contract) — a worse failure than a stray directory.
Removing a plugin never installed is a reported no-op, never an error.

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
version, every file it writes and the URL each comes from, the destination
directory, and the unsandboxed-privilege caveat, all before anything an
operator would need to walk back is already running.

## What this does NOT do

- **No slash command or menu yet.** `App::apply_marketplace_install`/
  `apply_marketplace_uninstall` are real, tested, end-to-end-correct
  methods with no interactive TUI trigger wired to them yet — deliberately
  scoped out of this item rather than rushed; see this item's own
  completion report for exactly what a follow-up (a `SlashCommand` variant
  mirroring `/ask`'s async-effect pipeline) would touch.
- **No digest check, no allow-list, no trust prompt**, and none is coming
  from this mechanism — see "Trust", above.
- **No git clone, no archive extraction** — see "The manifest format",
  above.
