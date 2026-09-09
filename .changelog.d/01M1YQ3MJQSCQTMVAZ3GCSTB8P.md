### Fixed

- **A single slow MCP/subprocess-plugin call no longer kills the whole plugin session the instant its per-call deadline elapses** — board item `01M1YQ3MJQSCQTMVAZ3GCSTB8P`. `ChildSession` (the shared round-trip machinery under `conway-plugin-mcp` and `conway-plugin-subprocess`) now warns and waits again for the SAME pending response, up to a fixed, bounded multiple of the original deadline, before giving up; the original request is never resent, and a genuinely hung or malicious server is still killed once the full ceiling elapses. A new `first_call_timeout_ms` (`McpPluginSpec`/`[plugins].mcp[]`/`[plugins].claude_compat[]`) gives an MCP session's first real `tools/call` after `initialize` a larger, operator-tunable warm-up budget, since it commonly pays a one-time cost (opening a connection, priming a cache) an already-warm call never pays again.

### Changed

- **`docs/plugins/mcp.md`** now documents the full startup/first-call/per-call timeout tiers and the bounded grace-before-kill every tier shares.
