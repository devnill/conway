# `conway.web`: a web fetch (and search) tool

The first-party network-fetch plugin, shipped by `crates/conway-plugin-web`.
Depends on [`concepts.md`](concepts.md) for vocabulary and
[`trust-and-security.md`](trust-and-security.md) for the broader trust
model this plugin sits inside — this page covers only what is specific to
this one plugin: exactly what it reaches, what it refuses by default, and
what an operator should and should not assume about the guard.

## What this is for

"Read this documentation page and tell me what changed" is an ordinary
request a developer makes several times a day, and every comparable
harness (Claude Code, Codex, OpenCode, Cursor, Hermes, Antigravity) answers
it with a built-in fetch tool. Without `conway.web`, the model can only
reach a URL by shelling out to `curl` — which requires `bash` to be
installed AND approved, and `bash` is off by default, so a fresh conway
install cannot fetch anything at all.

## The trust posture, stated plainly

**Installing `conway.web` gives the model outbound network reach, with
YOUR OWN connectivity, from a fresh install.** This is a materially
different trust posture from every other first-party plugin in this
workspace — `conway.checkpoint` reads/writes files the agent could already
reach through `write`/`edit`; `conway.trim` narrows context the agent
already produced. `conway.web`'s `web_fetch` reaches somewhere the agent
could not otherwise reach at all. That is exactly why it is **opt-in, not
in the default opinion set** — see `crates/conway-plugin-web/src/lib.rs`'s
own module doc for the tier reasoning — and why both tools default to
`PermissionClass::RequiresApproval`: a fresh install still prompts for the
first call, and an operator's own permission gate (`docs/permissions.md`)
remains the actual control point, not this plugin's own guard.

**What conway.web protects you from, and what it does not.** The SSRF
guard below (`web_fetch` only) refuses reaching an address inside your own
private network topology by accident — the class of mistake, not malice,
this tool exists to guard against. It is NOT a content filter: a URL on
the open internet that the guard allows through is fetched and its
(reduced-to-text) content is handed to the model exactly as returned,
unvetted for anything else. It does not sandbox the network call in any
OS-level sense the way `conway.confine` sandboxes a shell's filesystem
writes — there is no kernel-enforced boundary here, only the classification
logic described below, run in-process before every request.

## `web_fetch`: what is and is not blocked, precisely

`web_fetch` refuses:

- Any scheme other than `http`/`https` (`file://`, `ftp://`, ... all
  refused).
- A URL with no host.
- An IP-literal target that classifies as loopback, any RFC 1918 private
  range, link-local (`169.254.0.0/16` — **this includes the cloud-metadata
  endpoint `169.254.169.254`** most cloud providers serve instance
  credentials from), carrier-grade NAT (`100.64.0.0/10`), broadcast,
  multicast, RFC 5737 documentation space, or `0.0.0.0/8` — for both IPv4
  and the IPv6 analogues (`::1`, `::`, unique-local `fc00::/7`, link-local
  `fe80::/10`, multicast `ff00::/8`), **including an IPv4 address embedded
  in an IPv6 literal** (both the IPv4-mapped `::ffff:a.b.c.d` form and the
  deprecated IPv4-compatible `::a.b.c.d` form).
- **Every alternate numeric encoding of an IPv4 address the WHATWG URL
  Standard recognizes** — decimal (`http://2130706433/`), hex
  (`http://0x7f000001/`), octal (`http://0177.0.0.1/`), and partial-dotted
  forms all normalize to the real address before this guard ever sees it,
  because URL parsing itself (the `url` crate) does that normalization —
  so these are not a bypass.
- A domain name that resolves (via ordinary DNS) to any address in the
  ranges above, or that fails to resolve at all.

**Fails closed.** A host this guard cannot classify — a name whose DNS
resolution errors, or that resolves to zero addresses — is refused, never
allowed through on the absence of evidence.

**What this does NOT cover — read this before assuming complete
protection:**

- **DNS-rebinding.** The guard resolves a domain name once, classifies
  that answer, and then hands the ORIGINAL hostname (not a pinned IP
  address) to the HTTP client, which resolves it again to actually
  connect. A name that answers a safe address at guard time and a private
  one moments later at connect time is not caught. Closing this fully
  needs the HTTP client to connect to the exact address the guard already
  vetted; that is not implemented.
- **6to4 (`2002::/16`) and Teredo (`2001:0000::/32`) tunneling addresses**,
  which can themselves encapsulate an arbitrary IPv4 address, are not
  unwrapped. Neither is **NAT64 (`64:ff9b::/96`)**, which embeds an IPv4
  address in its low 32 bits the same way the two embeddings above do.
- **Content is not filtered or sanitized** beyond stripping HTML markup to
  text — see "The trust posture", above.

An operator whose `domains` permission rule (below) is the only line of
defense should not assume this guard closes every network-topology trick;
it closes what a URL/IP-literal/ordinary-DNS-resolution analysis can see,
which is the bar every comparable harness's own fetch tool sets, not a
stronger one.

## Redirects, truncation, and content reduction

Every redirect hop is re-guarded exactly like the initial URL — a first
request to a safe host that redirects to a private address is refused at
the redirect, not silently followed. Bounded to `max_redirects` hops
(default 5); exceeding the bound is a refused call, not a hang.

The response body is read up to `max_bytes` (default 200,000; a call's own
`max_bytes` argument may only narrow this, never raise it) — read stops at
the network layer once the bound is hit, not merely truncated after a full
download. An `html`-typed response is reduced to text: tags stripped,
`<script>`/`<style>` content dropped entirely, common entities decoded,
whitespace collapsed. This is a lightweight reducer, not a browser-grade
HTML/CSS engine — no DOM, no layout, no visibility rules.

## `web_search`: present only when configured

`web_search` is registered **only when an operator configures a search
provider** — a tool that would fail every call because nothing is
configured must not be announced to the model at all. One provider is
implemented: Brave (`GET
https://api.search.brave.com/res/v1/web/search`). No key is bundled; you
supply your own, named by an environment variable (never written to
`settings.json` itself, the same `api_key_env` indirection a backend's own
credential already uses).

## Configuring it: `[plugins.config."conway.web"]`

```json
{ "plugins": {
    "install": ["conway.web"],
    "config": { "conway.web": {
      "max_bytes": 100000,
      "max_redirects": 3,
      "search": { "provider": "brave", "api_key_env": "BRAVE_API_KEY" }
    } }
} }
```

All three keys are optional and independently defaulted; any other key
under `conway.web`'s own table is refused by name, never silently dropped.
See `crates/conway-plugin-web/src/lib.rs`'s own module doc for the exact
validation each key gets.

## Permission rules: `domains`

`web_fetch` renders as the bare URL (not a JSON dump), so a structured
`domains` permission rule can compare it against a host list:

```json
{ "select": { "tools": ["web_fetch"] }, "when": { "domains": ["docs.rs"] }, "then": "allow" }
```

allows `docs.rs` and any subdomain of it without a prompt; a fetch of any
other host still prompts (or is denied, if a `deny` rule names it). See
[`docs/permissions.md`](../permissions.md)'s own `domains` section for the
full matching rule and its fail-closed behavior on both the allow and
deny/prompt sides.

## What this plugin does NOT build

- **No browser automation, no JavaScript execution, no screenshots.**
  `web_fetch` performs one GET; nothing here renders a page.
- **No caching layer, no crawling.** One URL per `web_fetch` call.
- **No bundled search provider key.** An operator who wants `web_search`
  supplies their own key.

## Installing it

```json
{ "plugins": { "install": ["conway.web"] } }
```

Not in the default opinion set — a deliberate choice, not an oversight; see
"The trust posture", above.
