//! Acceptance 1, end to end: an operator browses a marketplace, installs a
//! plugin from it, and uses it -- against a real wiremock marketplace
//! server (never a real network host, per this item's own constraint) and
//! a real MCP server subprocess (a hand-written Python 3 stdio script, the
//! identical fixture shape `conway-plugin-claude`'s own end-to-end suite
//! uses -- see that crate's `tests/end_to_end.rs`), never a mock of
//! conway's own protocol.
//!
//! Three steps, each one this item's own machinery: **browse** (fetch the
//! marketplace manifest over HTTP), **install** (fetch every declared file
//! into conway's plugin store), **use** (the installed directory reads as
//! an ordinary Claude Code plugin directory -- `conway_plugin_claude::
//! discover` -> `conway_plugin_mcp::McpPlugin::discover` -> an actual tool
//! round trip).

use std::io::Write as _;

use conway::plugin::{ContentBlock, Plugin as _, ToolCall, ToolCtx};
use conway::AgentId;
use conway_plugin_mcp::McpPlugin;
use conway_testkit::{CollectingEventSink, FakeSubagentHost};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
    path
}

/// See `conway-plugin-claude/tests/end_to_end.rs`'s own doc: a freshly
/// exec'd binary's first run on this OS can block for seconds at ~0% CPU.
/// This test's real invocation is `python3 <installed script path>` (never
/// the script itself as a direct executable -- see the fixture setup's own
/// comment for why: an installed file's executable bit is not guaranteed by
/// this crate's own file-writer), so what needs warming is `python3`
/// itself, not the fetched script.
async fn warm_python3() {
    let child = tokio::process::Command::new("python3")
        .arg("--version")
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
        std::sync::Arc::new(FakeSubagentHost::new(agent_id)),
        std::sync::Arc::new(CollectingEventSink::new()),
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

/// The full path, end to end: browse a marketplace, install a plugin by
/// id, and its `.mcp.json` server declaration becomes a real, working
/// tool whose declared tool actually runs a round trip against the
/// spawned server.
#[tokio::test]
async fn browsing_installing_and_using_a_marketplace_plugin_works_end_to_end() {
    // A python script this test controls, written to a scratch dir the
    // marketplace itself will "serve" the bytes of over HTTP -- standing in
    // for wherever a real marketplace host would keep a plugin's files.
    let source_scratch = tempfile::tempdir().expect("source scratch dir");
    let script_source = write_script(source_scratch.path(), "acme_mcp.py", REF_MCP_SERVER);
    let script_bytes = std::fs::read_to_string(&script_source).unwrap();
    warm_python3().await;

    // Where the plugin will actually be installed -- `.mcp.json`'s own
    // `command`/`args` must name the FINAL on-disk path directly, since
    // `conway_plugin_claude` does not (yet) expand a `${CLAUDE_PLUGIN_ROOT}`
    // style variable (verified against its own `mcp.rs`: `command`/`args`
    // are used verbatim) -- exactly the same "absolute script path" shape
    // `conway-plugin-claude`'s own end-to-end fixture uses, just resolved
    // against the store path this test controls instead of a path the
    // operator typed by hand.
    let store = tempfile::tempdir().expect("plugin store");
    let installed_script_path = store.path().join("acme-tools").join("acme_mcp.py");

    let plugin_json =
        r#"{"name":"acme-tools","version":"1.0.0","description":"Acme's internal tools"}"#;
    let mcp_json = format!(
        r#"{{"mcpServers":{{"acme-search":{{"command":"python3","args":["{}"]}}}}}}"#,
        installed_script_path.display()
    );

    // Step 0: a marketplace, served over real HTTP (loopback-only,
    // wiremock -- never a real network host).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/marketplace.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{
                "name": "acme-marketplace",
                "plugins": [
                    {{
                        "id": "acme-tools",
                        "name": "Acme Tools",
                        "description": "Acme's internal tools",
                        "version": "1.0.0",
                        "files": {{
                            ".claude-plugin/plugin.json": "{base}/plugin.json",
                            ".mcp.json": "{base}/mcp.json",
                            "acme_mcp.py": "{base}/acme_mcp.py"
                        }}
                    }}
                ]
            }}"#,
            base = server.uri()
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/plugin.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(plugin_json))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/mcp.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(mcp_json.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/acme_mcp.py"))
        .respond_with(ResponseTemplate::new(200).set_body_string(script_bytes))
        .mount(&server)
        .await;

    // Step 1: BROWSE -- the operator sees what the marketplace offers
    // before installing anything.
    let marketplace_url = format!("{}/marketplace.json", server.uri());
    let manifest = conway_plugin_marketplace::fetch_marketplace(&marketplace_url)
        .await
        .expect("browse the marketplace");
    assert_eq!(manifest.plugins.len(), 1);
    assert_eq!(manifest.plugins[0].id, "acme-tools");
    assert_eq!(manifest.plugins[0].description, "Acme's internal tools");

    // Step 2: INSTALL -- fetch every declared file into conway's plugin
    // store.
    let installed =
        conway_plugin_marketplace::install_plugin(&marketplace_url, "acme-tools", store.path())
            .await
            .expect("install the plugin");
    assert_eq!(installed.id, "acme-tools");
    assert_eq!(installed.dir, store.path().join("acme-tools"));
    assert!(installed.dir.join(".claude-plugin/plugin.json").is_file());
    assert!(installed.dir.join(".mcp.json").is_file());
    assert!(installed_script_path.is_file());

    // Step 3: USE -- the installed directory is an ordinary Claude Code
    // plugin directory now. This is the SAME downstream path
    // `conway-plugin-claude`'s own end-to-end test proves for a directory
    // the operator placed by hand -- nothing about it is special-cased for
    // a marketplace-sourced one (the trust ruling's own point: identical
    // footing either way).
    let report =
        conway_plugin_claude::discover(&installed.dir).expect("discover the installed directory");
    assert_eq!(report.id, "acme-tools");
    assert_eq!(report.mcp_servers.len(), 1);
    assert_eq!(report.mcp_servers[0].name, "acme-search");

    let spec = report.mcp_servers[0].clone().into_spec(5_000);
    let plugin = McpPlugin::discover(spec)
        .await
        .expect("the installed MCP server must discover successfully");

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

    // Step 4: UNINSTALL -- leaves nothing behind (acceptance 3's other
    // half; the config-entry half is proven at the writer/App level).
    let removed =
        conway_plugin_marketplace::uninstall_plugin("acme-tools", store.path()).expect("uninstall");
    assert!(removed);
    assert!(!installed.dir.exists());
}
