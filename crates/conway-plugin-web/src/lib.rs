//! `conway.web`: a first-party `web_fetch` tool (GET an `http(s)` URL,
//! reduced to readable text, bounded, and guarded against reaching a
//! private network -- see `fetch`'s own module doc for the full SSRF
//! coverage), plus an optional `web_search` tool that exists only when an
//! operator configures a search provider.
//!
//! # Why this is opt-in, not in the default opinion set
//!
//! Every other first-party plugin in this workspace (`conway.checkpoint`,
//! `conway.trim`, `conway.stepguard`, ...) reads or writes state the agent
//! could already reach through tools it already has -- a checkpoint is a
//! shadow copy of a file the agent already wrote; a trim curator narrows
//! context the agent already produced. `conway.web` is different in kind,
//! not degree: it is the ONE first-party plugin whose tool reaches the
//! open network with the OPERATOR's own connectivity, from a fresh
//! install, the moment it is installed -- there is no local sandbox
//! boundary the way there is for `bash`'s command text (the operator's own
//! permission gate is the only control point, and `PermissionClass::
//! RequiresApproval`, this crate's own default, means a fresh install
//! still prompts for the first call). That is exactly the "wanted, not
//! required" tier the project reserves for a capability every comparable
//! harness ships but this project declines to hand an operator unasked --
//! `docs/plugins/web.md` states the trust posture in full; this doc
//! comment states the one-line reasoning for the tier placement itself.
//!
//! # Installing it
//!
//! ```json
//! { "plugins": { "install": ["conway.web"] } }
//! ```
//!
//! # Configuring it: `[plugins.config."conway.web"]`
//!
//! [`WebPlugin::configure`] is the real, wired seam
//! (`conway_core::ports::Plugin::configure`; `conway_plugin_trim::
//! TrimPlugin::configure` is that method's own first implementor, and this
//! crate is its second) that reaches this table -- resolved by whatever
//! binary/embedder calls `ConwayBuilder::install_selected`
//! (`crates/conway-cli/src/first_party_plugins.rs`'s `bundle` is
//! `conway-cli`'s own choke point). Three keys, each optional and
//! independently defaulted:
//!
//! ```json
//! { "plugins": { "install": ["conway.web"], "config": { "conway.web": {
//!     "max_bytes": 100000,
//!     "max_redirects": 3,
//!     "search": { "provider": "brave", "api_key_env": "BRAVE_API_KEY" }
//! } } } }
//! ```
//!
//! - `max_bytes` (integer, `>= 1`): [`fetch::FetchConfig::max_bytes`]'s
//!   default is 200,000; a call's own `max_bytes` argument may only narrow
//!   this, never raise it.
//! - `max_redirects` (integer, `0..=255`): [`fetch::FetchConfig::
//!   max_redirects`]'s default is 5.
//! - `search` (object, `{"provider": ..., "api_key_env": ...}`): absent by
//!   default, which means `web_search` is NOT REGISTERED at all -- see
//!   `search`'s own module doc, "declaration-honesty": a tool that would
//!   fail every call because no provider is configured must not be
//!   announced to the model. `provider` names one of [`search::
//!   SearchProvider`]'s recognized values (`"brave"` today); `api_key_env`
//!   names an ENVIRONMENT VARIABLE this tool reads the actual key from at
//!   call time -- the key's VALUE is never written to `settings.json`
//!   itself, the same indirection `crates/conway/src/config/schema.rs`'s
//!   `BackendEntry::api_key_env` already establishes for a backend's own
//!   credential.
//!
//! Any other key under `conway.web`'s own table is refused BY NAME
//! (`conway_core::ports::PluginConfigureError::UnknownKey`), never
//! silently dropped -- the same discipline `TrimPlugin::configure`
//! establishes.
//!
//! # What this crate does NOT build
//!
//! No browser automation, no JavaScript execution, no screenshots. No
//! caching layer, no crawling -- one URL per `web_fetch` call. No bundled
//! search provider key -- an operator who wants `web_search` supplies
//! their own.

mod fetch;
mod search;

use std::sync::Arc;

use conway::plugin::{Plugin, PluginConfigureError, PluginDescription, PluginManifest, Tool};

pub use fetch::{FetchConfig, WebFetchTool};
pub use search::{SearchConfig, SearchProvider, WebSearchTool};

/// This plugin's manifest id.
pub const PLUGIN_ID: &str = "conway.web";

/// `conway.web`'s own persistent configuration -- see this crate's own
/// module doc, "Configuring it", for the `[plugins.config."conway.web"]`
/// wire shape [`WebPlugin::configure`] validates and applies.
#[derive(Debug, Clone, Default)]
pub struct WebPluginConfig {
    pub fetch: FetchConfig,
    /// Absent by default: no search provider configured, so `web_search`
    /// is not registered at all -- see `search`'s own doc.
    pub search: Option<SearchConfig>,
}

// `Default` is exactly the derived one: `FetchConfig`'s own default and no
// configured search provider.

/// `conway.web`'s plugin type -- see this crate's own module doc.
pub struct WebPlugin {
    config: WebPluginConfig,
}

impl WebPlugin {
    pub fn new(config: WebPluginConfig) -> Self {
        Self { config }
    }
}

impl Default for WebPlugin {
    fn default() -> Self {
        Self::new(WebPluginConfig::default())
    }
}

impl Plugin for WebPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: self.tools().iter().map(|t| t.spec().name).collect(),
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "a web_fetch tool (GET an http(s) URL, reduced to readable text, \
                      SSRF-guarded), plus an optional web_search behind a configured provider"
                .to_string(),
            you_get: match &self.config.search {
                Some(search) => format!(
                    "2 tools: web_fetch (GET an http(s) URL, readable text, bounded) and \
                     web_search (via {})",
                    search.provider.name()
                ),
                None => "1 tool: web_fetch (GET an http(s) URL, readable text, bounded) -- \
                         web_search is not registered until a search provider is configured"
                    .to_string(),
            },
            you_lose: "nothing else off by default -- installing this plugin adds tools, it \
                       never removes any"
                .to_string(),
            costs: "outbound network reach with the operator's own connectivity, on every \
                    web_fetch/web_search call -- gated by the ordinary permission prompt \
                    (RequiresApproval by default) unless the operator writes an allow rule"
                .to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> =
            vec![Arc::new(WebFetchTool::new(self.config.fetch.clone()))];
        if let Some(search) = &self.config.search {
            tools.push(Arc::new(WebSearchTool::new(search.clone())));
        }
        tools
    }

    /// See this crate's own module doc, "Configuring it", for the wire
    /// shape and the reasoning; see [`conway_core::ports::Plugin::
    /// configure`]'s own doc for the seam this is the second real
    /// implementor of.
    fn configure(&mut self, value: &serde_json::Value) -> Result<(), PluginConfigureError> {
        let object = value
            .as_object()
            .ok_or_else(|| PluginConfigureError::NotAnObject {
                actual: json_value_kind(value).to_string(),
            })?;
        let mut fetch = self.config.fetch.clone();
        let mut search = self.config.search.clone();
        for (key, raw) in object {
            match key.as_str() {
                "max_bytes" => {
                    let n = raw
                        .as_u64()
                        .ok_or_else(|| PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: "must be a JSON integer".to_string(),
                        })?;
                    if n == 0 {
                        return Err(PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: "must be >= 1".to_string(),
                        });
                    }
                    fetch.max_bytes = n as usize;
                }
                "max_redirects" => {
                    let n = raw
                        .as_u64()
                        .ok_or_else(|| PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: "must be a JSON integer".to_string(),
                        })?;
                    fetch.max_redirects =
                        u8::try_from(n).map_err(|_| PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: format!("must fit in a u8 (0-255), got {n}"),
                        })?;
                }
                "search" => {
                    let search_obj =
                        raw.as_object()
                            .ok_or_else(|| PluginConfigureError::InvalidValue {
                                key: key.clone(),
                                message:
                                    "must be a JSON object with \"provider\" and \"api_key_env\""
                                        .to_string(),
                            })?;
                    let provider_name = search_obj
                        .get("provider")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| PluginConfigureError::InvalidValue {
                            key: "search.provider".to_string(),
                            message: "required, must be a string".to_string(),
                        })?;
                    let provider = SearchProvider::parse(provider_name).ok_or_else(|| {
                        PluginConfigureError::InvalidValue {
                            key: "search.provider".to_string(),
                            message: format!(
                                "unrecognized provider {provider_name:?}; supported: brave"
                            ),
                        }
                    })?;
                    let api_key_env = search_obj
                        .get("api_key_env")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| PluginConfigureError::InvalidValue {
                            key: "search.api_key_env".to_string(),
                            message: "required, must be a string naming an environment \
                                      variable"
                                .to_string(),
                        })?
                        .to_string();
                    for other_key in search_obj.keys() {
                        if other_key != "provider" && other_key != "api_key_env" {
                            return Err(PluginConfigureError::UnknownKey {
                                key: format!("search.{other_key}"),
                            });
                        }
                    }
                    search = Some(SearchConfig {
                        provider,
                        api_key_env,
                    });
                }
                other => {
                    return Err(PluginConfigureError::UnknownKey {
                        key: other.to_string(),
                    });
                }
            }
        }
        self.config = WebPluginConfig { fetch, search };
        Ok(())
    }
}

fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_id_matches_the_published_constant() {
        let plugin = WebPlugin::default();
        assert_eq!(plugin.manifest().id, PLUGIN_ID);
    }

    #[test]
    fn description_is_non_empty() {
        let plugin = WebPlugin::default();
        let d = plugin.description();
        assert!(!d.summary.is_empty());
        assert!(!d.you_get.is_empty());
        assert!(!d.costs.is_empty());
    }

    /// Acceptance 3, at the plugin level: with no `search` configured,
    /// `web_search` is absent entirely -- not present-but-erroring, ABSENT
    /// from the returned `Vec` -- catches an implementation that always
    /// registers both tools and relies on `web_search` failing at call
    /// time instead.
    #[test]
    fn web_search_is_absent_by_default_and_present_once_configured() {
        let plugin = WebPlugin::default();
        let names: Vec<String> = plugin
            .tools()
            .iter()
            .map(|t| t.spec().name.to_string())
            .collect();
        assert_eq!(names, vec!["web_fetch".to_string()]);

        let mut configured = WebPlugin::default();
        configured
            .configure(&serde_json::json!({
                "search": { "provider": "brave", "api_key_env": "BRAVE_API_KEY" }
            }))
            .unwrap();
        let names: Vec<String> = configured
            .tools()
            .iter()
            .map(|t| t.spec().name.to_string())
            .collect();
        assert_eq!(
            names,
            vec!["web_fetch".to_string(), "web_search".to_string()]
        );
    }

    #[test]
    fn configure_refuses_an_unknown_top_level_key_by_name() {
        let mut plugin = WebPlugin::default();
        let err = plugin
            .configure(&serde_json::json!({ "not_a_real_key": 1 }))
            .unwrap_err();
        match err {
            PluginConfigureError::UnknownKey { key } => assert_eq!(key, "not_a_real_key"),
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    #[test]
    fn configure_refuses_an_unrecognized_search_provider_by_name() {
        let mut plugin = WebPlugin::default();
        let err = plugin
            .configure(&serde_json::json!({
                "search": { "provider": "not-a-real-provider", "api_key_env": "X" }
            }))
            .unwrap_err();
        match err {
            PluginConfigureError::InvalidValue { key, message } => {
                assert_eq!(key, "search.provider");
                assert!(message.contains("not-a-real-provider"));
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn configure_refuses_an_unknown_key_inside_the_search_table() {
        let mut plugin = WebPlugin::default();
        let err = plugin
            .configure(&serde_json::json!({
                "search": {
                    "provider": "brave",
                    "api_key_env": "BRAVE_API_KEY",
                    "extra_field": true
                }
            }))
            .unwrap_err();
        match err {
            PluginConfigureError::UnknownKey { key } => assert_eq!(key, "search.extra_field"),
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    #[test]
    fn configure_applies_max_bytes_and_max_redirects() {
        let mut plugin = WebPlugin::default();
        plugin
            .configure(&serde_json::json!({ "max_bytes": 1234, "max_redirects": 2 }))
            .unwrap();
        assert_eq!(plugin.config.fetch.max_bytes, 1234);
        assert_eq!(plugin.config.fetch.max_redirects, 2);
    }

    #[test]
    fn configure_rejects_a_non_object_value() {
        let mut plugin = WebPlugin::default();
        let err = plugin
            .configure(&serde_json::json!("not an object"))
            .unwrap_err();
        assert!(matches!(err, PluginConfigureError::NotAnObject { .. }));
    }
}
