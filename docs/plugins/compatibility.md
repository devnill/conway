# Compatibility

What an author may rely on across conway versions, and what they may not.
This is contract documentation, not narrative — the amended P-11's ban on
version references in `docs/` forbids "added in 0.x" feature-history, not
stating the compatibility rules themselves, and this page is entirely the
latter.

Depends on [`concepts.md`](concepts.md) for vocabulary. Most of what this
page states concretely applies to a wire protocol that does not exist yet
(`.design/d1-transport.md` is a design spike, not implemented) — every claim
below says plainly whether it describes something enforced in the tree today
or a decided rule for the transport once it lands, the same labeling
discipline [`hooks.md`](hooks.md) uses throughout.

## Versioning

**The rule that makes forward compatibility work, stated once, for both
scopes below: unknown enum tags fail to the most restrictive value, never to
a default that happens to be permissive.** "Use versioning" is not this
page's content; the table under each scope is.

### Config files, as shipped today

A hand-authored file — `settings.json`, `permissions.json`, an agent def's
frontmatter, a provider profile — is read by conway *before* the model or
any plugin sees it, and the two directions of change need opposite
treatment:

- **An old file, a newer binary.** Every optional field carries
  `#[serde(default)]` with a conservative, documented default, so a file
  written before a field existed keeps parsing unchanged. `PermissionFile`'s
  own doc states this for `deny`/`rules`
  (`crates/conway-core/src/permission_pattern.rs`); `profile.rs`'s module
  doc states the identical reasoning for provider profiles
  (`crates/conway-backends/src/profile.rs`).
- **A newer file (or a typo), an older binary.** `#[serde(deny_unknown_fields)]`
  turns an unrecognized field into a loud, named error instead of a silently
  ignored one. This is deliberately the *opposite* choice from a wire frame
  (below): a hand-authored file is small, edited by a human, and a
  misspelled key defaulting to "off" and running anyway is worse than a
  refusal naming the field. `crates/conway/src/config/schema.rs`,
  `crates/conway/src/agents.rs`'s frontmatter, and
  `crates/conway-backends/src/profile.rs`'s `ProfileRaw` all set it; the
  crate's own `config_precedence.rs` test suite pins the behavior with named
  typo cases (`typo_d_key_is_rejected_by_deny_unknown_fields`,
  `typo_d_health_key_is_rejected_by_deny_unknown_fields`).

**A known gap in this rule, checked fresh against the tree at the time of
writing: `PermissionFile` and `TrustFile` do not set
`#[serde(deny_unknown_fields)]`.** `PermissionFile`
(`crates/conway-core/src/permission_pattern.rs`) and the internal
`RawPermissionFile` its own loader parses through carry no
`deny_unknown_fields` attribute; neither does `TrustFile` or
`TrustedRecord` (`crates/conway/src/config/trust.rs`). A misspelled key —
`"denys"` instead of `"deny"` — is silently accepted as an unrecognized
field, `deny` falls back to its `#[serde(default)]` empty vector, and **zero
deny rules install, with no error surfaced anywhere.** This is the loud-typo
property every other hand-authored config surface in this tree already has,
missing from exactly the two files whose typo has a security consequence
rather than a merely cosmetic one. No board item names this gap
specifically — searched and confirmed absent. If filed, the item is: *add
`#[serde(deny_unknown_fields)]` to `PermissionFile` (and its internal
`RawPermissionFile` parse path) and to `trust.rs`'s `TrustFile`/
`TrustedRecord`, with a regression test asserting a misspelled `denys` key
is a loud load error rather than a silently-empty deny list — following the
exact precedent `config_precedence.rs`'s existing typo tests already set for
`settings.json`.*

**The general fallback pattern — explicit match, deny by default — already
ships, just not yet at the config-file-typo layer above.**
`PermissionMode::allows_category` (`crates/conway-core/src/
permission_mode.rs`) matches plan mode's allowed set explicitly and denies
everything else, including a `ToolCategory` variant that doesn't exist yet;
the module's own doc states why the match is spelled allow-list-then-deny
rather than the inverse: *"a future `ToolCategory::Deploy` is blocked in
plan mode the day it is added, without anyone remembering to update this
file."* This is the shipped instance of "unknown fails to the most
restrictive value" the wire-level rule below generalizes.

### The wire protocol, once it exists — decided design, not yet code

`.design/d3-wire-vocabulary.md` §2.2/§3.1 settles the rules a future
out-of-process plugin transport must follow. Concrete, and cited here so a
future implementation is built against a decision, not a vibe:

| Enum | Unknown tag means, per the design |
|---|---|
| `ToolCategory` | `execute` — the category plan mode already denies |
| `PermissionClass` | `dangerous` |
| `TruncationPolicy` | the host's default policy, never `none` |
| `ContentBlock` | drop the block, count it, surface it via a status change |
| `Event` | ignore — the one point where "ignore" is correct, because an observer changes nothing by construction |
| `ResultStatus` | `failed`, never `completed` |
| `ToolSelector` | `only([])` — selects nothing; narrowing, never widening |

And the version-negotiation table for the handshake itself:

| Condition | Outcome |
|---|---|
| `plugin.major != host.major` | Refuse to load, naming both |
| `plugin.minor_min > host.minor` | Refuse — the plugin needs a feature this host does not have |
| `plugin.minor_min <= host.minor` | Accept, whatever the plugin's own minor; unknown fields ignored-and-counted, never rejected |
| Unknown/unsupported version of a **participant** point (`tool`, `permission.policy`, `context.hook`) | Refuse to load — a policy that silently never runs is the worst outcome |
| Unknown/unsupported version of an **observer** point (`observe`, `status`) | Degrade: load without that point, warn |

`major` covers the frame vocabulary and envelope semantics (method names,
error-code ranges); `minor` is additive only — new methods, new optional
fields, new capability names, new `Event` variants. Conway's own version
appears in a future handshake as informational only (`host: { name,
version }`), never branched on — a TUI-only release does not have to move
the protocol, and nothing here is size-of-conway-version-shaped.

**None of the wire-level table above is enforced by any code today.** No
`initialize` handshake, no `WireManifest`, no per-point version negotiation
exists in the tree — `concepts.md`'s "Hook-first" section and
[`hooks.md`](hooks.md) point 1 both state the same thing from their own
angles. It is documented here, concretely, because a reader building
against this reference needs the rule the same way `hooks.md` documents
`permission.policy/1`'s contract ahead of its implementation: labeled,
decided, and precise enough to implement without re-litigating.

**Decision `01KYTP2QYE00FJSQAQQ0E37JZP`** (settling the hook-first
redirect's four open questions) bears on this page only through what it
already settled elsewhere in the set: `concepts.md`'s "Fork vs spawn"
section states the resulting per-registration `subagent_mode` declaration
and its `hook.fork` capability gate. It adds nothing further to the
versioning rules above; restated here only so the check is on record rather
than silently skipped.

## The promise

- **`conway-core`'s port surface** — `Plugin`, `Tool`, `ContextHook`,
  `PermissionGate`, and every type named in their method signatures — is
  what `ARCHITECTURE.md` §2/§3.8 calls out by name: *"`conway-core` is
  deliberately small and slow-moving under strict semver discipline...
  because it is the surface third-party plugins depend on."* This is the
  surface an in-process plugin author is actually building against, and it
  is the one this project is most deliberate about not letting churn.
- **`conway::plugin`'s export set** (landed as board item F8,
  `crates/conway/src/lib.rs`) is a curated re-export, not a second surface
  with its own promise: every name in it is sourced from `conway-core`
  (`conway_core::ports`, `conway_core::content`, `conway_core::error`,
  `conway_core::agent::Fact`, `conway_core::segment::PromptSegment`,
  `conway_core::provenance::Provenance`) plus the external `async_trait`
  macro re-export that makes `use conway::plugin::*` sufficient to write an
  implementation. It is covered by the *same* discipline as the types it
  re-exports, not a weaker one invented for the module — `crates/conway/
  tests/plugin_surface.rs` and `plugin_builtin_parity.rs` pin the set by
  compiling against it, so a regression here is a build failure, not a
  silent drift.
- **conway is pre-1.0, and that is a real, stated limit on this promise, not
  a footnote.** `CHANGELOG.md` states the project "adheres to Semantic
  Versioning" — and semver's own text is explicit that a pre-1.0 major
  (`0.x`) carries no compatibility guarantee at all; anything may change in
  any `0.x` release. `docs/embedding.md`'s own statement about a first-party
  plugin crate generalizes past that one case: *"Pre-1.0, [an API] can
  change in any workspace release, same as everything else here."*
  `conway::plugin` is "everything else" in that sentence — nothing in this
  page promises a *version-number*-level compatibility guarantee. What the
  strict-semver-discipline language above actually promises is narrower and
  real: `conway-core`'s port surface is *edited* more carefully and changes
  less often than the rest of the tree, because the project has said so
  explicitly and treats a change there as costing every plugin author, not
  because a `0.x` bump is contractually barred from breaking it.
- **Everything not named above is explicitly unstable**: `conway-cli`'s
  flags, `conway-runtime`'s internals (never re-exported at all —
  `crates/conway/src/lib.rs`'s own doc states "no type from `conway-runtime`
  is re-exported here"), and any first-party plugin crate's own API
  (`conway-plugin-skeleton`, `conway-plugin-routing`) — all versioned with
  the workspace, all free to change in any release pre-1.0.

## Config file strictness

Stated fully under "Versioning" above; restated here in one line because
it's the sentence an author skimming for "can I add a field to my own
plugin's config and not break existing installs" wants without reading the
whole section: **a hand-authored file rejects a field it does not
recognize; a wire frame (once one exists) tolerates one, silently but
countedly, because the file in front of a human punishes a typo and the
frame between two programs must survive a newer peer.**

## Deprecation

**No point, field, or capability in this set has actually been retired
yet**, and there is no board item ruling a formal deprecation *procedure*
(a warning period, a required runtime notice, a minimum number of minor
releases before removal) — searched and confirmed absent. What follows is
the policy this page can state with confidence from what's already ruled
elsewhere, not an exercised procedure:

- **Retiring a point, a field, or a capability name is a breaking change by
  definition**, and the versioning rules above put a breaking change behind
  a major bump only — never a minor, regardless of how unused the thing
  being removed appears to be. A minor release only ever adds.
- **A retirement is announced the same way any user-facing change is, per
  the amended P-11: a `CHANGELOG.md` entry under the section for the
  release that removes it.** P-11 is explicit that version references and
  "added in 0.x"-shaped history do not belong in `docs/` — `CHANGELOG.md` is
  release metadata, the one place that framing is correct, and it stays the
  single place a reader looks for "what changed and when," never inline in
  a page like this one.
- **The closest live precedent for a surface moving out from under a
  consumer** is not a deprecation but a relocation: `conway-routing`'s
  engine moved from a mandatory workspace crate into the installable
  first-party plugin tier (board item `01KZFC43J1J06BM4CCWKCKHSNV`), and
  `conway_backends::config::Dialect`'s five-variant convenience enum was kept
  working for every existing call site rather than replaced outright when
  provider profiles became data (`crates/conway-backends/src/profile.rs`'s
  own doc: *"it is not deprecated, but it can no longer name a provider this
  crate doesn't already ship code for"*) — the shipped instinct, in both
  cases, is to keep an old surface compiling and route it onto the new
  mechanism rather than break it outright, even without a formally ruled
  deprecation window forcing that choice.

If a stronger commitment — a stated minimum warning period, a runtime
deprecation notice a plugin author can detect programmatically — is wanted,
it needs its own decision; this section states what already follows from
P-11 and the versioning rules above, not a ruling this page is positioned to
make on its own.
