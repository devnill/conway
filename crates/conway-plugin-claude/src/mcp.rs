//! `.mcp.json` -- Claude Code's own MCP server declaration file, at the
//! plugin directory's root. **The one pairing the spec calls a MATCH, and
//! the proof this whole approach works**: both sides are stdio JSON-RPC
//! with a `command`/`env` declaration; translating one is close to a field
//! rename.
//!
//! Claude Code's own shape (the same `.mcp.json` convention used for
//! project-level MCP config):
//! ```json
//! { "mcpServers": { "<name>": { "command": "prog", "args": ["a"], "env": {"K":"V"} } } }
//! ```
//! `conway_plugin_mcp::McpPluginSpec::command` is a single argv `Vec<String>`
//! (program, then arguments) -- never a shell string -- so `command` and
//! `args` here fold onto that one vector as `[command, ...args]`.

use std::path::Path;

use conway_plugin_mcp::McpPluginSpec;

use crate::error::ClaudeCompatError;
use crate::fsutil::read_bounded;
use crate::unsupported::UnsupportedItem;

/// One `.mcp.json` server entry, translated. Structurally a thin rename of
/// [`McpPluginSpec`] minus the timeout (an operator-configured, per-`[plugins].
/// claude_compat[]`-entry value this crate does not read from the plugin
/// directory at all, since Claude Code's own `.mcp.json` carries none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslatedMcpServer {
    pub name: String,
    pub command: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl TranslatedMcpServer {
    /// Builds the [`McpPluginSpec`] `conway_plugin_mcp::McpPlugin::discover`
    /// takes -- the literal next step that makes this server's tools real,
    /// exactly as `crates/conway-cli/src/mcp_plugins.rs` does for an
    /// operator-authored `[plugins].mcp[]` entry.
    pub fn into_spec(self, timeout_ms: u64) -> McpPluginSpec {
        McpPluginSpec {
            config_id: self.name,
            command: self.command,
            timeout_ms,
            env: self.env,
        }
    }
}

/// Reads `<dir>/.mcp.json`, translating every well-formed server entry and
/// collecting a named [`UnsupportedItem`] for every entry this crate cannot
/// translate (missing/non-string `command`, per P-13: a malformed entry is
/// reported, never silently dropped from the list). `Ok((vec![], vec![]))`
/// when the file is simply absent -- a plugin directory naming no MCP
/// servers at all is ordinary, not an error.
pub(crate) fn read_mcp_servers(
    dir: &Path,
) -> Result<(Vec<TranslatedMcpServer>, Vec<UnsupportedItem>), ClaudeCompatError> {
    let path = dir.join(".mcp.json");
    if !path.is_file() {
        return Ok((Vec::new(), Vec::new()));
    }
    let text = read_bounded(&path)?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|source| ClaudeCompatError::MalformedJson {
            path: path.clone(),
            source,
        })?;

    let mut servers = Vec::new();
    let mut unsupported = Vec::new();

    let Some(map) = value.get("mcpServers").and_then(|v| v.as_object()) else {
        return Ok((servers, unsupported));
    };
    for (name, entry) in map {
        let Some(command) = entry.get("command").and_then(|v| v.as_str()) else {
            unsupported.push(UnsupportedItem::mcp_server(
                name,
                "no string \"command\" field -- conway's MCP client needs an argv-shaped \
                 command to spawn",
            ));
            continue;
        };
        let mut argv = vec![command.to_string()];
        let mut malformed_args = false;
        if let Some(args) = entry.get("args") {
            match args.as_array() {
                Some(items) => {
                    for item in items {
                        match item.as_str() {
                            Some(s) => argv.push(s.to_string()),
                            None => malformed_args = true,
                        }
                    }
                }
                None => malformed_args = true,
            }
        }
        if malformed_args {
            unsupported.push(UnsupportedItem::mcp_server(
                name,
                "\"args\" is present but is not an array of strings",
            ));
            continue;
        }
        let mut env = Vec::new();
        let mut malformed_env = false;
        if let Some(env_value) = entry.get("env") {
            match env_value.as_object() {
                Some(pairs) => {
                    for (k, v) in pairs {
                        match v.as_str() {
                            Some(s) => env.push((k.clone(), s.to_string())),
                            None => malformed_env = true,
                        }
                    }
                }
                None => malformed_env = true,
            }
        }
        if malformed_env {
            unsupported.push(UnsupportedItem::mcp_server(
                name,
                "\"env\" is present but is not an object of string values",
            ));
            continue;
        }
        servers.push(TranslatedMcpServer {
            name: name.clone(),
            command: argv,
            env,
        });
    }
    Ok((servers, unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_mcp_json(dir: &Path, contents: &str) {
        std::fs::write(dir.join(".mcp.json"), contents).unwrap();
    }

    #[test]
    fn translates_command_args_and_env_onto_one_argv_vector() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_mcp_json(
            dir.path(),
            r#"{"mcpServers":{"acme-search":{"command":"acme-mcp","args":["--flag","v"],"env":{"API_KEY":"secret"}}}}"#,
        );
        let (servers, unsupported) = read_mcp_servers(dir.path()).unwrap();
        assert!(unsupported.is_empty(), "{unsupported:?}");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "acme-search");
        assert_eq!(
            servers[0].command,
            vec![
                "acme-mcp".to_string(),
                "--flag".to_string(),
                "v".to_string()
            ]
        );
        assert_eq!(
            servers[0].env,
            vec![("API_KEY".to_string(), "secret".to_string())]
        );
    }

    #[test]
    fn an_absent_mcp_json_is_a_true_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (servers, unsupported) = read_mcp_servers(dir.path()).unwrap();
        assert!(servers.is_empty());
        assert!(unsupported.is_empty());
    }

    #[test]
    fn a_server_missing_command_is_reported_not_silently_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_mcp_json(dir.path(), r#"{"mcpServers":{"broken":{"args":["x"]}}}"#);
        let (servers, unsupported) = read_mcp_servers(dir.path()).unwrap();
        assert!(servers.is_empty());
        assert_eq!(unsupported.len(), 1);
        assert!(unsupported[0].name.contains("broken"));
    }

    #[test]
    fn multiple_servers_all_translate() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_mcp_json(
            dir.path(),
            r#"{"mcpServers":{"one":{"command":"a"},"two":{"command":"b"}}}"#,
        );
        let (servers, unsupported) = read_mcp_servers(dir.path()).unwrap();
        assert!(unsupported.is_empty());
        assert_eq!(servers.len(), 2);
    }
}
