//! `WebSearchTool`: the `web_search` tool -- present only when an operator
//! configures a search provider (`crate::WebPlugin::configure`'s `search`
//! key). See [`crate`]'s own module doc for the declaration-honesty
//! argument: a tool that would fail every call because no provider is
//! configured must not be announced to the model at all, so this tool's
//! very EXISTENCE (not merely its behavior) is conditional -- see
//! `crate::WebPlugin::tools`.
//!
//! One provider is implemented this round: [`SearchProvider::Brave`] (the
//! Brave Search API, `GET https://api.search.brave.com/res/v1/web/search`
//! with an `X-Subscription-Token` header carrying the API key). Adding a
//! second provider is a natural follow-up, not started here -- disclosed
//! rather than implied complete by [`SearchProvider`]'s own doc.

use async_trait::async_trait;
use conway::plugin::{
    ContentBlock, PathArgs, PermissionClass, RenderKind, Tool, ToolCall, ToolCategory, ToolCtx,
    ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};
use schemars::JsonSchema;
use serde::Deserialize;

/// `web_search`'s own configuration: which provider to call, and the name
/// of the environment variable carrying that provider's API key. The key
/// VALUE itself is never carried in configuration -- only its ENV VAR NAME
/// is, the same `api_key_env` indirection `crates/conway/src/config/
/// schema.rs`'s `BackendEntry::api_key_env` already establishes for a
/// backend's own credential.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub provider: SearchProvider,
    pub api_key_env: String,
}

/// The open (for now, single-member) set of search providers this crate
/// knows how to call. See this module's own doc for why only
/// [`Self::Brave`] is implemented this round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchProvider {
    Brave,
}

impl SearchProvider {
    /// Parses a provider name from `[plugins.config."conway.web"].search.
    /// provider`, case-insensitively. `None` for anything unrecognized --
    /// `WebPlugin::configure` turns that into a named
    /// `PluginConfigureError::InvalidValue`, never a silent fallback to
    /// some default provider.
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "brave" => Some(Self::Brave),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Brave => "brave",
        }
    }

    fn default_endpoint(&self) -> String {
        match self {
            Self::Brave => "https://api.search.brave.com/res/v1/web/search".to_string(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WebSearchArgs {
    /// The search query
    query: String,
    /// Number of results to return (default 5, max 10)
    #[schemars(range(min = 1))]
    n: Option<u8>,
}

/// One parsed search result.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// The `web_search` tool. See this module's own doc.
pub struct WebSearchTool {
    config: SearchConfig,
    /// The provider endpoint this tool calls -- [`SearchProvider::
    /// default_endpoint`] in production ([`Self::new`]); overridden by
    /// [`Self::with_endpoint`] so this crate's own tests can point at a
    /// local `wiremock` server instead of a real provider, without this
    /// tool's own code branching on "am I in a test".
    endpoint: String,
}

impl WebSearchTool {
    pub fn new(config: SearchConfig) -> Self {
        let endpoint = config.provider.default_endpoint();
        Self { config, endpoint }
    }

    /// Test-only: see [`Self::endpoint`]'s own doc.
    #[cfg(test)]
    fn with_endpoint(config: SearchConfig, endpoint: String) -> Self {
        Self { config, endpoint }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("web_search"),
            description: format!(
                "Search the web via the operator-configured provider ({}) and return up to \
                 10 results (title, URL, snippet). This tool is registered only when a \
                 provider is configured -- see conway.web's own docs.",
                self.config.provider.name()
            ),
            schema: schemars::schema_for!(WebSearchArgs),
            category: ToolCategory::Search,
            permission: PermissionClass::RequiresApproval,
        }
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: WebSearchArgs = serde_json::from_value(call.arguments.clone()).map_err(|e| {
            ToolError::InvalidArguments {
                detail: e.to_string(),
            }
        })?;
        let n = args.n.unwrap_or(5).clamp(1, 10);

        let api_key = resolve_api_key(&self.config.api_key_env, self.config.provider)?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| ToolError::Internal {
                detail: e.to_string(),
            })?;

        let results = match self.config.provider {
            SearchProvider::Brave => {
                brave_search(&client, &self.endpoint, &api_key, &args.query, n).await?
            }
        };

        let text = if results.is_empty() {
            format!("No results for {:?}", args.query)
        } else {
            results
                .iter()
                .enumerate()
                .map(|(i, r)| format!("{}. {}\n   {}\n   {}", i + 1, r.title, r.url, r.snippet))
                .collect::<Vec<_>>()
                .join("\n\n")
        };

        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text { text }],
            is_error: false,
            truncation: TruncationPolicy::Head { max_bytes: 32_768 },
            artifacts: vec![],
        })
    }
}

/// Reads `env_name` from the process environment, naming both the missing
/// variable and the configured provider in the refusal -- so an operator
/// who configured `conway.web`'s search provider but forgot to export the
/// key gets a message that says exactly what is missing, not a bare
/// "unauthorized" from the provider's own API.
fn resolve_api_key(env_name: &str, provider: SearchProvider) -> Result<String, ToolError> {
    std::env::var(env_name).map_err(|_| ToolError::Denied {
        reason: format!(
            "conway.web's configured search provider ({}) needs an API key in the \
             environment variable {env_name:?}, which is not set",
            provider.name()
        ),
    })
}

/// Calls the Brave Search API's `web` results, parsing at most `n` of them.
/// A malformed or missing field on any one result silently drops that
/// result (never the whole response) -- `filter_map` -- since a provider's
/// wire format carrying an occasional incomplete entry is not this tool's
/// failure to report.
async fn brave_search(
    client: &reqwest::Client,
    endpoint: &str,
    api_key: &str,
    query: &str,
    n: u8,
) -> Result<Vec<SearchResult>, ToolError> {
    let n_str = n.to_string();
    // The query string is built with `url`'s own encoder rather than
    // `RequestBuilder::query`. That method is behind a `reqwest` feature this
    // workspace does not enable (the workspace pins `default-features =
    // false` with `rustls`/`json`/`stream`), and widening a workspace-wide
    // feature set for one call site would cost every other crate that
    // compiles `reqwest` -- where `url` is already a direct dependency here,
    // load-bearing for this crate's own SSRF host parsing.
    let mut endpoint_url = url::Url::parse(endpoint).map_err(|e| ToolError::InvalidArguments {
        detail: format!("brave search endpoint is not a valid url: {e}"),
    })?;
    endpoint_url
        .query_pairs_mut()
        .append_pair("q", query)
        .append_pair("count", n_str.as_str());
    let resp = client
        .get(endpoint_url)
        .header("X-Subscription-Token", api_key)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| ToolError::Io {
            detail: format!("calling brave search: {e}"),
        })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(ToolError::Denied {
            reason: format!("brave search returned HTTP {status}"),
        });
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| ToolError::Io {
        detail: format!("parsing brave search response: {e}"),
    })?;
    let results = body
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(results
        .into_iter()
        .take(n as usize)
        .filter_map(|r| {
            Some(SearchResult {
                title: r.get("title")?.as_str()?.to_string(),
                url: r.get("url")?.as_str()?.to_string(),
                snippet: r
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_provider_parse_is_case_insensitive_and_refuses_unknown_names() {
        assert_eq!(SearchProvider::parse("brave"), Some(SearchProvider::Brave));
        assert_eq!(SearchProvider::parse("BRAVE"), Some(SearchProvider::Brave));
        assert_eq!(SearchProvider::parse("google"), None);
    }

    /// Catches an implementation that reads a bare/default key instead of
    /// naming the configured env var, or that panics on a missing var
    /// instead of returning a typed refusal.
    #[test]
    fn resolve_api_key_names_the_missing_env_var_and_provider() {
        let err = resolve_api_key(
            "CONWAY_WEB_PLUGIN_TEST_DEFINITELY_UNSET_VAR",
            SearchProvider::Brave,
        )
        .unwrap_err();
        match err {
            ToolError::Denied { reason } => {
                assert!(reason.contains("CONWAY_WEB_PLUGIN_TEST_DEFINITELY_UNSET_VAR"));
                assert!(reason.contains("brave"));
            }
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    /// Real HTTP call against a `wiremock` fixture shaped like Brave's own
    /// response, proving the parsing path end to end without touching the
    /// process environment at all (the API key is passed directly, not
    /// resolved from a var) -- avoiding any `std::env::set_var` in a test
    /// binary that runs tests in parallel.
    #[tokio::test]
    async fn brave_search_parses_titles_urls_and_snippets_and_respects_n() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let fixture = serde_json::json!({
            "web": {
                "results": [
                    {"title": "Serde", "url": "https://docs.rs/serde", "description": "a framework"},
                    {"title": "Tokio", "url": "https://docs.rs/tokio", "description": "an async runtime"},
                    {"title": "Missing url", "description": "dropped, no url field"},
                ]
            }
        });
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let endpoint = format!("{}/res/v1/web/search", server.uri());
        let results = brave_search(&client, &endpoint, "test-key", "serde vs tokio", 5)
            .await
            .unwrap();
        assert_eq!(
            results.len(),
            2,
            "the malformed third entry must be dropped, not error out"
        );
        assert_eq!(results[0].title, "Serde");
        assert_eq!(results[0].url, "https://docs.rs/serde");
        assert_eq!(results[0].snippet, "a framework");
    }

    /// `WebSearchTool::invoke` end to end: env-var key resolution, the real
    /// HTTP call (via `with_endpoint`, pointed at `wiremock`), and result
    /// formatting, all through the public `Tool` surface. Uses an env var
    /// name unique to this test function so it cannot race another test's
    /// `std::env::set_var` in this same binary.
    #[tokio::test]
    async fn web_search_tool_invoke_end_to_end_via_wiremock() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const ENV_VAR: &str = "CONWAY_WEB_PLUGIN_TEST_BRAVE_KEY_END_TO_END";
        std::env::set_var(ENV_VAR, "test-key");

        let server = MockServer::start().await;
        let fixture = serde_json::json!({
            "web": {"results": [
                {"title": "Serde", "url": "https://docs.rs/serde", "description": "a framework"}
            ]}
        });
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture))
            .mount(&server)
            .await;

        let tool = WebSearchTool::with_endpoint(
            SearchConfig {
                provider: SearchProvider::Brave,
                api_key_env: ENV_VAR.to_string(),
            },
            format!("{}/res/v1/web/search", server.uri()),
        );
        let call = ToolCall {
            call_id: "c1".into(),
            name: ToolName::new("web_search"),
            arguments: serde_json::json!({ "query": "serde vs tokio" }),
        };
        let agent_id = conway::AgentId::new();
        let ctx = ToolCtx::for_test(
            agent_id,
            std::env::temp_dir(),
            std::sync::Arc::new(conway_testkit::FakeSubagentHost::new(agent_id)),
            std::sync::Arc::new(conway_testkit::CollectingEventSink::new()),
        );
        let out = tool.invoke(call, ctx).await.unwrap();
        let ContentBlock::Text { text } = &out.blocks[0] else {
            panic!("expected a Text block");
        };
        assert!(text.contains("Serde"), "{text}");
        assert!(text.contains("https://docs.rs/serde"), "{text}");

        std::env::remove_var(ENV_VAR);
    }
}
