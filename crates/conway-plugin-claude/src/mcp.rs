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
/// The bare name of the plugin-root variable -- [`PLUGIN_ROOT_TOKEN`] is the
/// `${...}` interpolation form of this same name, and they must not drift.
const PLUGIN_ROOT_TOKEN_NAME: &str = "CLAUDE_PLUGIN_ROOT";

/// Substitutes `${CLAUDE_PLUGIN_ROOT}` in one translated `.mcp.json` string.
///
/// Uses `hooks`' own [`PLUGIN_ROOT_TOKEN`] rather than a second spelling of
/// the same literal (steering P-14): hook commands and MCP argvs are the two
/// places a Claude Code plugin writes this token, and a translation that
/// resolved it in one and not the other is exactly the defect this fixes.
///
/// **Found by the operator, 2026-08-30.** `.mcp.json` shipping
/// `"args": ["${CLAUDE_PLUGIN_ROOT}/bin/ideate-mcp"]` -- the ordinary shape,
/// since a plugin cannot know its own absolute install path -- was passed to
/// the spawner verbatim, so `sh` was handed a literal `${CLAUDE_PLUGIN_ROOT}`
/// path, failed `No such file or directory`, and exited before writing a byte
/// of protocol. conway reported that as `session died: closed stdout (EOF)
/// mid-session` and refused to start at all.
fn subst_plugin_root(raw: &str, dir: &Path) -> String {
    raw.replace(crate::hooks::PLUGIN_ROOT_TOKEN, &dir.display().to_string())
}

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
        let mut argv = vec![subst_plugin_root(command, dir)];
        let mut malformed_args = false;
        if let Some(args) = entry.get("args") {
            match args.as_array() {
                Some(items) => {
                    for item in items {
                        match item.as_str() {
                            Some(s) => argv.push(subst_plugin_root(s, dir)),
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
                            Some(s) => env.push((k.clone(), subst_plugin_root(s, dir))),
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
        // Claude Code exports `CLAUDE_PLUGIN_ROOT` into an MCP server's
        // environment as well as interpolating it, and launchers rely on
        // BOTH: ideate's `bin/ideate-mcp` is spawned through the
        // interpolated argv but then reads
        // `${CLAUDE_PLUGIN_ROOT:-<derived>}` at runtime to locate its own
        // build output. Substituting without exporting fixes the spawn and
        // leaves the runtime read to the fallback -- which happens to work
        // there, and would not in a launcher that has no fallback.
        //
        // A plugin that declares the variable in its own `env` block wins:
        // that is an explicit statement about its own layout, and silently
        // overwriting it would be conway substituting its opinion for the
        // plugin author's.
        if !env.iter().any(|(k, _)| k == PLUGIN_ROOT_TOKEN_NAME) {
            env.push((
                PLUGIN_ROOT_TOKEN_NAME.to_string(),
                dir.display().to_string(),
            ));
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

    /// **Board finding, operator-reported 2026-08-30.** A `.mcp.json` that
    /// locates its own launcher with `${CLAUDE_PLUGIN_ROOT}` -- the ordinary
    /// shape, since a plugin cannot know its absolute install path -- must
    /// have that token resolved before the argv reaches the spawner.
    ///
    /// This is the exact ideate manifest that stopped conway starting:
    /// `sh` received a literal `${CLAUDE_PLUGIN_ROOT}/bin/ideate-mcp`, failed
    /// `No such file or directory`, and exited before speaking any protocol,
    /// which surfaced as `session died: closed stdout (EOF) mid-session`.
    ///
    /// `hooks.rs` resolved the token for hook commands from the start; this
    /// path never did. Note that `tests/end_to_end.rs` in
    /// `conway-plugin-marketplace` DOCUMENTED the gap and worked around it by
    /// writing an absolute path into its fixture -- so the end-to-end test
    /// passed against a manifest shape no real plugin uses. A test that
    /// avoids the untranslated shape cannot fail on it.
    #[test]
    fn plugin_root_token_resolves_in_command_args_and_env() {
        let dir = tempfile::tempdir().expect("plugin dir");
        write_mcp_json(
            dir.path(),
            r#"{"mcpServers":{"ideate":{
                 "command":"sh",
                 "args":["${CLAUDE_PLUGIN_ROOT}/bin/ideate-mcp"],
                 "env":{"IDEATE_HOME":"${CLAUDE_PLUGIN_ROOT}/state"}}}}"#,
        );

        let (servers, unsupported) = read_mcp_servers(dir.path()).expect("read");
        assert!(unsupported.is_empty(), "{unsupported:?}");
        assert_eq!(servers.len(), 1);
        let root = dir.path().display().to_string();

        assert_eq!(
            servers[0].command,
            vec!["sh".to_string(), format!("{root}/bin/ideate-mcp")],
            "the argv handed to the spawner must carry a real path, never the \
             literal token -- sh cannot open a file called `${{CLAUDE_PLUGIN_ROOT}}`"
        );
        assert!(
            servers[0]
                .env
                .iter()
                .any(|(k, v)| k == "IDEATE_HOME" && *v == format!("{root}/state")),
            "env values carry the token too: {:?}",
            servers[0].env
        );
        // Exported as well as interpolated: a launcher that resolves its own
        // root at runtime (ideate's does) needs the variable to exist.
        assert!(
            servers[0]
                .env
                .iter()
                .any(|(k, v)| k == "CLAUDE_PLUGIN_ROOT" && *v == root),
            "CLAUDE_PLUGIN_ROOT must be exported to the server: {:?}",
            servers[0].env
        );
    }

    /// A plugin that declares `CLAUDE_PLUGIN_ROOT` itself keeps its own value
    /// -- conway interpolates and exports, but does not overrule an explicit
    /// statement by the plugin author about its own layout.
    #[test]
    fn an_explicitly_declared_plugin_root_is_not_overwritten() {
        let dir = tempfile::tempdir().expect("plugin dir");
        write_mcp_json(
            dir.path(),
            r#"{"mcpServers":{"x":{"command":"true",
                 "env":{"CLAUDE_PLUGIN_ROOT":"/explicitly/chosen"}}}}"#,
        );
        let (servers, _) = read_mcp_servers(dir.path()).expect("read");
        let roots: Vec<_> = servers[0]
            .env
            .iter()
            .filter(|(k, _)| k == "CLAUDE_PLUGIN_ROOT")
            .collect();
        assert_eq!(roots.len(), 1, "exactly one binding, never a duplicate");
        assert_eq!(roots[0].1, "/explicitly/chosen");
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
        // The plugin's own declared pair survives verbatim...
        assert!(
            servers[0]
                .env
                .iter()
                .any(|(k, v)| k == "API_KEY" && v == "secret"),
            "{:?}",
            servers[0].env
        );
        // ...alongside the CLAUDE_PLUGIN_ROOT conway now exports for every
        // translated server (see `plugin_root_token_resolves_in_command_args_
        // and_env`). This assertion used to be an exact-vector equality, which
        // is why adding the export surfaced here: the extra binding is the
        // intended new contract, not a regression.
        assert_eq!(
            servers[0].env.len(),
            2,
            "the declared pair plus the exported root, nothing else: {:?}",
            servers[0].env
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
