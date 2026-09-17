### Fixed

- **`conway routes explain`, `conway sessions`, `conway tools list`, and `conway plugin list`/`install`/`remove` no longer refuse to run when a configured `[plugins].mcp[]` entry crashes at startup** — board item `01M2PJCT90G2010KGCJ4YSFREM`. A single crashing MCP plugin used to fail `main.rs`'s one dispatch choke point for every command alike, including the six that never start an agent or call a tool — exactly the operator most likely to reach for `routes explain` in the first place, to debug why their session will not start. Each of those six commands now degrades instead: it prints a `conway: warning:` line naming the failing entry and starts without that entry's tools, so the introspection you asked for actually runs. The TUI, one-shot `-p`, and every plugin-contributed command are unaffected — a session that might actually propose a tool call still refuses to start over a crashing MCP entry, exactly as before.

### Changed

- **`docs/plugins/mcp.md`** now documents the introspection-only degrade path alongside the existing hard-fail posture, and states that a tool-name collision across two plugins is always fatal regardless of which command triggered it.
