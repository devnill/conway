# conway documentation

Task-oriented documentation for using conway — installing it, driving it
interactively or from a script, and embedding it as a library. Start with
[`getting-started.md`](getting-started.md) if you haven't run conway yet;
everything else assumes that page's setup.

## Start here

| Page | Answers | Read if |
| --- | --- | --- |
| [`getting-started.md`](getting-started.md) | How do I install conway, configure a model provider, and run my first prompt? | You're setting conway up for the first time. |

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

For the system-level picture — the crate layout, the core primitives, and
the data flow of one turn — see [`/ARCHITECTURE.md`](../ARCHITECTURE.md).
