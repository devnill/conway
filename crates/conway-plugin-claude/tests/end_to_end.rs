//! Acceptance 2, end to end: an operator points conway at a Claude Code
//! plugin directory on disk, and its `.mcp.json` server declaration's tools
//! become real, invokable `conway::plugin::Tool`s -- the SAME
//! `conway_plugin_mcp::McpPlugin::discover` path an operator-authored
//! `[plugins].mcp[]` entry already uses, fed a translated
//! `McpPluginSpec` instead of a hand-written one. Against a REAL MCP
//! server (a hand-written Python 3 stdio script, the identical fixture
//! shape `conway-plugin-mcp`'s own end-to-end suite uses), never a mock of
//! conway's own protocol.

use std::io::Write as _;
use std::sync::Arc;

use conway::plugin::{ContentBlock, Plugin as _, ToolCall, ToolCtx};
use conway::AgentId;
use conway_plugin_mcp::McpPlugin;
use conway_testkit::{CollectingEventSink, FakeSubagentHost};

const REF_MCP_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "acme-search", "version": "0.1"},
        }
    }

def tools_list(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "tools": [
                {
                    "name": "search",
                    "description": "Search acme's index.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"query": {"type": "string"}},
                        "required": ["query"],
                    },
                },
            ]
        }
    }

def tools_call(rid, params):
    name = params.get("name", "")
    args = params.get("arguments", {})
    if name == "search":
        q = args.get("query", "")
        result = {"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": f"found: {q}"}],
            "isError": False,
        }}
    else:
        result = {"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": f"unknown tool: {name}"}],
            "isError": True,
        }}
    return result

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        resp = initialize(rid)
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        resp = tools_list(rid)
    elif method == "tools/call":
        resp = tools_call(rid, req.get("params", {}))
    else:
        resp = {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": f"method not found: {method}"}}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

fn write_script(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create fixture script");
    f.write_all(contents.as_bytes())
        .expect("write fixture script");
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x fixture script");
    }
    path
}

/// Executes `path` once, discarding the result, before the timed assertion
/// -- see `conway-plugin-mcp`'s own `tests/common::warm` for the measured
/// first-exec tax this sidesteps (a freshly written script's first exec on
/// this OS can block for seconds at ~0% CPU).
async fn warm(path: &std::path::Path) {
    let child = tokio::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        let _ = child.wait().await;
    }
}

fn ctx() -> ToolCtx {
    let agent_id = AgentId::new();
    ToolCtx::for_test(
        agent_id,
        std::env::temp_dir(),
        Arc::new(FakeSubagentHost::new(agent_id)),
        Arc::new(CollectingEventSink::new()),
    )
}

fn call(tool: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "call-1".to_string(),
        name: conway::ToolName::new(tool),
        arguments,
    }
}

fn first_text(out: &conway::plugin::ToolOutput) -> String {
    for b in &out.blocks {
        if let ContentBlock::Text { text } = b {
            return text.clone();
        }
    }
    panic!("expected at least one Text block, got {:?}", out.blocks);
}

/// The full path, end to end: a directory shaped like a real Claude Code
/// plugin (`.claude-plugin/plugin.json` + `.mcp.json`) is discovered,
/// translated into an `McpPluginSpec`, and that spec becomes a real,
/// working plugin whose declared tool actually runs a round trip against
/// the spawned server.
#[tokio::test]
async fn a_claude_plugin_directorys_mcp_server_declaration_becomes_a_real_working_tool() {
    let plugin_dir = tempfile::tempdir().expect("tempdir");
    let root = plugin_dir.path();
    std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
    std::fs::write(
        root.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"acme-tools","version":"1.0.0","description":"Acme's internal tools"}"#,
    )
    .unwrap();

    let script = write_script(root, "acme_mcp.py", REF_MCP_SERVER);
    warm(&script).await;
    std::fs::write(
        root.join(".mcp.json"),
        format!(
            r#"{{"mcpServers":{{"acme-search":{{"command":"{}"}}}}}}"#,
            script.display()
        ),
    )
    .unwrap();

    // Step 1: the translation layer reads the directory conway itself never
    // fetched -- purely local disk I/O.
    let report = conway_plugin_claude::discover(root).expect("discover the plugin directory");
    assert_eq!(report.id, "acme-tools");
    assert_eq!(report.mcp_servers.len(), 1);
    assert_eq!(report.mcp_servers[0].name, "acme-search");

    // Step 2: the translated declaration becomes a REAL McpPluginSpec, fed
    // to the exact same discovery path `conway-cli`'s own `mcp_plugins.rs`
    // uses for an operator-authored `[plugins].mcp[]` entry.
    let spec = report.mcp_servers[0].clone().into_spec(5_000);
    let plugin = McpPlugin::discover(spec)
        .await
        .expect("the translated MCP declaration must discover successfully");

    // Step 3: the server's own declared tool is really there, and really
    // invokable -- "have its MCP server declarations work," acceptance 2's
    // own wording, proven by an actual round trip, not merely a manifest
    // check.
    let manifest = plugin.manifest();
    assert_eq!(manifest.tools, vec![conway::ToolName::new("search")]);

    let tools = plugin.tools();
    assert_eq!(tools.len(), 1);
    let output = tools[0]
        .invoke(
            call("search", serde_json::json!({"query": "widgets"})),
            ctx(),
        )
        .await
        .expect("tool invocation must succeed");
    assert!(!output.is_error);
    assert_eq!(first_text(&output), "found: widgets");
}

/// Acceptance 5, demonstrated against the SAME directory shape: every
/// non-MCP thing in a real-shaped plugin directory is named, not silently
/// dropped, even while the MCP half above works end to end.
#[test]
fn everything_the_directory_cannot_use_is_named_by_the_same_discover_call() {
    let plugin_dir = tempfile::tempdir().expect("tempdir");
    let root = plugin_dir.path();
    std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
    std::fs::write(
        root.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"acme-tools"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("commands")).unwrap();
    std::fs::write(root.join("commands").join("triage.md"), "").unwrap();
    std::fs::create_dir_all(root.join("hooks")).unwrap();
    std::fs::write(
        root.join("hooks").join("hooks.json"),
        r#"{"hooks":{"Notification":[{"hooks":[{"type":"command","command":"notify-send hi"}]}]}}"#,
    )
    .unwrap();

    let report = conway_plugin_claude::discover(root).expect("discover");
    let names: Vec<_> = report.unsupported.iter().map(|u| u.name.as_str()).collect();
    assert!(names.contains(&"commands/triage.md"));
    assert!(names.contains(&"Notification"));
    assert_eq!(report.unsupported.len(), 2, "{:?}", report.unsupported);
}
