### Fixed

- **An MCP plugin whose session dies mid-use now recovers on the very next tool call instead of failing every call for the rest of the process's life** — board item `01M1YQ4WB3334VF70YCJRXCCZG`. `McpPlugin` transparently respawns a fresh child from the original spec and re-runs the handshake, bounded by `conway_plugin_mcp::MAX_AUTO_RESPAWNS` (3 respawns per plugin, for its whole lifetime) after which `SessionDied` surfaces permanently again, exactly as before; the call that found the session dead always fails closed and is never re-sent to the fresh child, since a killed process may already have completed the work.

### Changed

- **A respawned MCP server whose `tools/list` no longer matches the tool set registered at discovery now fails the call with a typed `McpPluginError::ToolSetChanged` naming the added/removed tools**, instead of silently adopting a different tool set.
- **An MCP plugin respawn is now visible on the status line / `/context`**, not only in a `tracing::warn!`, via the same `Plugin::status_contributions` path every other plugin's contributions already surface through.
- **`docs/plugins/mcp.md`** now documents the bounded auto-respawn behaviour in place of the old "no automatic reconnect" claim.
