# conway documentation

Task-oriented documentation for using conway — installing it, driving it
interactively or from a script, and embedding it as a library. Start with
[`getting-started.md`](getting-started.md) if you haven't run conway yet;
everything else assumes that page's setup.

## Start here

| Page | Answers | Read if |
| --- | --- | --- |
| [`getting-started.md`](getting-started.md) | How do I install conway, configure a model provider, and run my first prompt? | You're setting conway up for the first time. |
| [`GUIDE.md`](../GUIDE.md) | What does a working session actually look like — hooks, the agent tree, recovering from a bad turn, and the things that aren't on the screen? | You're installed and running, and want the practical path rather than a reference. Lives in the repository root beside `README.md`. |

## Driving conway

| Page | Answers | Read if |
| --- | --- | --- |
| [`interactive.md`](interactive.md) | How do I use the TUI — keys, slash commands, the permission prompt, the status line? | You're running `conway` with no `-p` flag, as a human at a terminal. |
| [`scripting.md`](scripting.md) | How does `-p`/`--print` behave — exit codes, output formats, permissions with nobody there to answer a prompt? | You're calling conway from a script or another program as a subprocess. |
| [`embedding.md`](embedding.md) | How do I depend on the `conway` crate directly — the builder chain, a minimal example, what's actually reachable from outside the workspace? | You're linking conway into a host application (an IDE, a service) instead of running it as a subprocess. |

## Concepts

| Page | Answers | Read if |
| --- | --- | --- |
| [`agents.md`](agents.md) | What's the difference between fork and spawn, and how do I create, steer, and inspect a child agent? | You want more than one agent working on a problem. |
| [`sessions.md`](sessions.md) | What gets persisted, how do resume and fork-from-disk work, and what's the full `conway sessions` command reference? | You need to resume, branch from, or inspect a session's history. |
| [`providers.md`](providers.md) | How do I point conway at Anthropic, an OpenAI-compatible server, or a provider conway doesn't already know about? | You're configuring a backend beyond the minimal example in `getting-started.md`. |
| [`routing.md`](routing.md) | How does conway pick which configured model actually serves a request, and how do I see why? | You have more than one model configured and need to control or debug which one runs. |
| [`permissions.md`](permissions.md) | What do the permission modes, the prompt, pattern grants, project trust, and `--root` confinement actually guarantee — and not guarantee? | You're deciding what an agent is allowed to touch. |
| [`tools.md`](tools.md) | What are the built-in tools, what's each one's category, permission class, and truncation policy, and which ones can `--root` actually confine? | You're writing `--allowed-tools`/`--deny-tools`, a permission rule, or a `PermissionGate`. |

## Extending conway

| Page | Answers | Read if |
| --- | --- | --- |
| [`plugins/`](plugins/README.md) | How does conway's plugin and hook architecture work — what's built today, what's decided but not yet built, and where do I start? Start with [`plugins/concepts.md`](plugins/concepts.md) for the mental model. | You're writing a plugin, a hook, or a policy that attaches to conway rather than just using it. |

## Background

| Page | Answers | Read if |
| --- | --- | --- |
| [`whitepaper.md`](whitepaper.md) | Why is conway shaped this way — the failure modes it's built against, the primitives fork/spawn and non-compaction rest on, and what they buy you? | You want the reasoning behind conway's design, not just how to drive it. |
| [`vision/`](vision/README.md) | What is conway *for*, how far along is it, and what is being worked on next? | You're contributing to conway's direction rather than using it. Not a user-facing page. |
| [`dogfooding.md`](dogfooding.md) | How do I turn friction I hit while using conway into a board item, in about the same number of keystrokes as shrugging and moving on? | You're using conway on its own tree and something's awkward — recording it now beats reconstructing it from memory later. Not a user-facing page. |

For the system-level picture — the crate layout, the core primitives, and
the data flow of one turn — see [`/ARCHITECTURE.md`](../ARCHITECTURE.md).
