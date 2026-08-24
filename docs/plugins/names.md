# `conway.names`: name an agent, then steer it by that name

The first-party plugin for operator-chosen agent names (board item
`01M0TV5BSE98S16SFYECG9G9WP`, decision `01M0TV3ZZBDKSSV7MD0FW3FSY7`),
installed by `crates/conway-plugin-names`. Depends on
[`concepts.md`](concepts.md) for vocabulary (plugin, command,
`[plugins].install`).

## What it is for

In a tree of forty agents, steering one means finding it first. The
`/agents` panel already prints a short, copyable id per row, and that solves
*telling rows apart* — but an id is not a handle you remember. This plugin
lets you name the two or three agents you are actually steering:

```
/conway.names.rename scout
/steer scout check whether the merge is clean
```

A name is an **addressing affordance**. It sits **alongside** the agent's
id, never instead of it: every id, prefix, and short id that worked before
still works, and an agent you never named behaves exactly as it always did.
Nothing auto-generates a name for every spawn — a screen full of invented
tokens beside a column that already disambiguates would be a trade, not an
improvement.

## Install it

```json
{ "plugins": { "install": ["conway.names"] } }
```

Not installed by default. Every first-party plugin in the shipped bundle is
opt-in (see [`README.md`](README.md) and `crates/conway-cli/src/first_party_plugins.rs`);
this one is no exception, and whether it should become the first exception
is a question for the operator, not for this page.

## The commands

| command | what it does |
| --- | --- |
| `/conway.names.rename <name>` | names the **focused** agent |
| `/conway.names.rename <agent-id> <name>` | names the agent with that **full** id |
| `/conway.names.unname` | forgets the focused agent's name |
| `/conway.names.unname <agent-id>` | forgets that agent's name |
| `/conway.names.list` | every name stored, across every session and project |

**Why the two-argument form wants a FULL id, not the short one on the
panel.** A plugin command cannot see the agent tree — its invocation context
carries the focused agent, the root, the session id, and your argument text,
and nothing else — so it cannot expand a short id the way the host's own
resolver can. `/tree` prints every agent's full 26-character id for exactly
this kind of use. In practice you rarely need it: focus the agent (the
`/agents` panel, `/context <agent>`) and use the one-argument form.

## What a name may be

One word — no whitespace — at most 48 characters, and not itself a valid
agent id. Each rule exists because breaking it would make the name
unusable rather than merely ugly:

- **No whitespace**, because `/steer <agent> <text>` splits on it. A name
  with a space could be stored but never typed at the agent it names.
- **At most 48 characters**, because the name is one token in a panel row
  that already carries an indent, a status marker, an id, a label, and a
  recipe. A longer name is not a large name, it is a broken row.
- **Not a valid ULID**, because the resolver matches a full agent id first,
  so such a name could never resolve to its own agent.

## How a name resolves

Every agent-targeted command — `/steer`, `/context`, `/fork @<agent>` — goes
through one resolver, which tries, in order:

1. a **full agent id**;
2. an **exact name**, among the agents in this session's tree;
3. an **id prefix**, among the same agents.

Exact matches come before approximate ones, which is why a deliberate name
is never shadowed by an accidental prefix collision with some other agent's
id.

**Two agents may share a name.** The store does not refuse a duplicate: it
cannot know which agents are on screen, and refusing `scout` today because
something in another project was called `scout` last year would be a worse
failure than the one it prevents. When two agents *in the live tree* share a
name, the resolver reports an ambiguity naming both candidates — the same
message shape an ambiguous id prefix already produces. `/conway.names.rename`
also tells you at the time that a name is already in use.

**A name only resolves to an agent in this session's tree.** The store is
global, so a name belonging to an agent you cannot see falls through to the
ordinary `no agent matches` error rather than resolving to something
off-screen.

## Where names are stored

One JSON file, `agent-names.json`, in the same directory as your
`settings.json` — `~/.conway/`, or `$CONWAY_CONFIG_DIR` when that is set.
Names follow the operator, not the checkout, exactly like the TUI's input
history file and unlike the project-local `conway.memory` store.

It is keyed by the bare agent id with no project or session partition,
because an agent id is a ULID: already unique across every project, session,
and process. A per-project directory would add a key that carries no
information the id does not already determine.

**Two costs, stated rather than discovered later:**

- A name **persists across a restart** but does **not travel** with a
  session log copied to another machine. Making it travel would require a
  record in the session log, which would mean changing conway's core; the
  decision behind this plugin declined that trade.
- Nothing prunes. An entry outlives the agent it names, because this plugin
  has no way to know which agents still exist, and a finished agent is still
  addressable. What keeps that honest is that removal is first-class
  (`/conway.names.unname`) and the whole store is visible
  (`/conway.names.list`) — nothing accumulates out of sight.

**Fail-closed, once selected.** If `agent-names.json` exists but cannot be
parsed, `conway` refuses to start and names the file, rather than starting
with an empty store and overwriting your file on the next rename. Fix or
delete the file, or remove `"conway.names"` from `[plugins].install`. With
the plugin unselected, no file is opened or created at all.

## Uninstalled, nothing changes

This is the point of shipping naming as a plugin. With `"conway.names"`
absent from `[plugins].install`:

- the `/agents` panel draws the identical row it drew before this plugin
  existed;
- the resolver is the identical two-pass id/prefix resolver;
- no file is created beside your `settings.json`;
- `conway-core` contains no naming machinery at all — the `AgentNames`
  trait and its filesystem implementation both live in
  `crates/conway-plugin-names`, and the CLI names that trait directly
  because it already links the crate in order to install it.

That last point is the structural claim worth checking rather than
believing: `grep -rn "AgentNames\|agent_name" crates/conway-core/src`
returns nothing.
