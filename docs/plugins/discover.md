# `conway.discover`: the tool a model calls to find a session it does not already hold a reference to

The first-party plugin for `SessionDiscoveryHost` (board item
`01M0PS8J3AK7Z7253Z3E3RD3GY`), installed by `crates/conway-plugin-discover`.
Depends on [`concepts.md`](concepts.md) for vocabulary, on
[`hooks.md`](hooks.md) point 20 for the `ToolCtx::session_discovery`
contract this plugin's tool calls through, on
[`trust-and-security.md`](trust-and-security.md)'s "Finding a session" section
for what it can and cannot reach, and on [`path.md`](path.md) — this tool
finds a `(session, seq)` pair; `conway.path`'s `compose_context_path` is what
actually brings it onto a context path.

## What this is, in one sentence

A model-callable tool, `search_sessions`, that finds a session or a specific
record the model does not already hold a reference to — one it neither
started this turn nor was handed a `session_id`/`transcript_ref` for by a
completed `conway_fork`/`conway_spawn`/`conway_ask` call — and reports what it
searched and what that cost.

## Why this exists

`compose_context_path` (`conway.path`) takes resolved `(session, seq)`
references, which is correct: the operator states intent in ordinary
language, a MODEL resolves it, and by the time the tool is called the
resolving is done (decision `01M0K4QT6MBXPD6PXMBBBD2P7B`). But a model can
only resolve intent into a reference it already holds — its own current
session, or a completed subagent's `transcript_ref`. "Bring in what we
worked out about the retry logic yesterday" names neither. Cherry-pick
(`01M0KZ6J0DF6XR1TVSDH2KDPRX`) shipped `compose_context_path`'s composing half
without this — a deliberate, disclosed follow-up, not an oversight — and this
plugin is what closes it.

**Not a widened `compose_context_path`, and not a `Curator`.** Cramming
search into the composing tool would have doubled that tool's own scope,
exactly the mistake its own design avoided once already; this is a
genuinely separate capability, its own plugin. And a `Curator` cannot
interpret "yesterday's retry logic conversation" at all — `CurateCtx` carries
`model: Option<ModelId>` as a sizing identifier only, never a callable
backend (the same finding that kept `conway.path` out of the `Curator` port).

## What installing it costs

```json
{ "plugins": { "install": ["conway.discover", "conway.path"] } }
```

Uninstalled, nothing changes: no `search_sessions` tool is announced, and
`ToolCtx::session_discovery` sits unused on every other tool exactly as it
did before this plugin existed. Opt-in, like every other member of the
first-party tier — see `PHILOSOPHY.md`'s "First-party plugins, and why they
are not defaults" for why nothing in this tier is on by default. Install
alongside `conway.path`: this tool only finds a `(session, seq)` pair,
`compose_context_path` is what brings it onto a path — installed alone,
`conway.discover` can still report what it found, but the model has nothing
to hand the finding to.

## What the model actually sends

- `scope` — `"current_project"` (default) or `"all_projects"`. Almost always
  the default: "yesterday" or "the other session" nearly always means this
  same project's own other sessions. `"all_projects"` is an explicit
  widening the model chooses, never a default it falls into.
- `label` / `agent_def` — exact match against a session's own metadata.
- `text` — a plain, case-insensitive substring. **Omitting it searches
  METADATA ONLY** (which sessions exist, when, labeled how) with zero record
  content ever read. Supplying it turns this into a bounded CONTENT scan.
- `max_sessions` — the most sessions this call will ever open and read,
  in either mode, most-recent-first. The price of the call, stated before it
  runs.

## What you see afterwards

Every match: the session id, its project, its working directory, when it was
created, and (in content-search mode) the specific matching records — a
`(session, seq)` and a short snippet, ready to hand to
`compose_context_path`'s `include` list. And, always, what was actually
searched and what it cost: how many project directories, how many sessions'
metadata, how many sessions' content, how many records — plus whether more
existed beyond `max_sessions` (`truncated`), so a caller who needs to look
further back knows to raise the bound and ask again rather than assume
exhaustion from a short result.

## Search surface: metadata always, content only when asked, bounded either way

Listing sessions is not the same as searching them, and full free-text search
over every record ever logged is exactly the "quietly reads a thousand
records" cost this project's standing rule forbids (the price of a curation
decision must be knowable in advance). This tool resolves the tension by
making the choice explicit and the cost visible rather than picking one
extreme: `text` omitted is a metadata-only listing (cheap, always bounded by
session count, never opens a record); `text` supplied is a real scan, bounded
by `max_sessions`, and the reply states exactly what was read.

No new index was built for this. `conway_session::SessionIndex` already
holds session HEADERS (id, agent, creation time, labels — never record
content); metadata search reuses it through the ordinary `SessionStore::list`
surface every session store already provides. A scan over a bounded set was
judged sufficient for content search; building a durable, cross-session
content index was judged a materially larger commitment than this item's
own question, and was not attempted.

## Reach: a directory listing over one root, never a crawler or a registry

`scope: "all_projects"` became answerable without either mechanism because
sessions now live under one central, project-keyed root (`~/.conway/sessions/
<project-key>/` by default, or `$CONWAY_CONFIG_DIR/sessions/<project-key>/`).
Machine-wide discovery is one `read_dir` over that root's own subdirectories,
never a filesystem crawl and never a side table anything else must keep in
sync. A project whose `[session].root` was explicitly configured away from
the central default is invisible to `"all_projects"` scope for its OWN
sessions — it never wrote there, so there is nothing to list. That is the
central-root decision's own disclosed edge, not a new gap this plugin opens.
