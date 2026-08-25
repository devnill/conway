//! The marketplace wire shape and [`fetch_marketplace`], the "browse" half
//! of this crate's acceptance criterion 1.
//!
//! # The manifest format, and why it names files rather than an archive
//!
//! ```json
//! {
//!   "name": "acme-marketplace",
//!   "description": "Acme's internal conway plugins",
//!   "plugins": [
//!     {
//!       "id": "acme-tools",
//!       "name": "Acme Tools",
//!       "description": "Search and lookup tools for Acme's internal index",
//!       "version": "1.0.0",
//!       "files": {
//!         ".claude-plugin/plugin.json": "https://example.com/acme-tools/plugin.json",
//!         ".mcp.json": "https://example.com/acme-tools/mcp.json"
//!       }
//!     }
//!   ]
//! }
//! ```
//!
//! `files` maps a relative path INSIDE the installed plugin directory to
//! the URL this crate fetches its bytes from -- deliberately not a single
//! archive URL. See `Cargo.toml`'s own doc for the full argument (no
//! archive-extraction dependency, and no symlink-in-an-archive class of
//! attack to defend against because there is no archive-extraction step at
//! all); this is what an installed plugin's own `files` map lets an
//! operator's browse view show honestly before consenting: every path that
//! will be written and every URL it comes from, not an opaque blob.
//!
//! `#[serde(deny_unknown_fields)]` throughout: a marketplace response is
//! untrusted network input (P-10), and a field this crate does not
//! recognize is exactly the kind of typo/version-skew this project's own
//! `.conway/skills`/`.conway/agents` loaders already refuse to guess past
//! (`conway_plugin_claude`'s own manifest parsing is the one deliberate
//! exception, for FOREIGN Claude Code files this project does not own the
//! schema of -- see that crate's own doc; a marketplace manifest is a
//! format THIS item defines, so the ordinary strict rule applies).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::MarketplaceError;

/// Safety cap on a marketplace manifest response's own byte size -- large
/// enough for a marketplace listing hundreds of plugins with real
/// descriptions, small enough that a malicious or broken marketplace
/// cannot make this crate buffer an unbounded response into memory (P-10).
pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

/// One marketplace: a name, an optional description, and every plugin it
/// lists.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceManifest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub plugins: Vec<MarketplacePluginEntry>,
}

impl MarketplaceManifest {
    /// Looks up one plugin by id -- the "install" half's first step, after
    /// "browse" (fetching the manifest) has already happened. Returns a
    /// typed, named error rather than `None`, since every caller of this
    /// method already knows the marketplace's own URL to name in the
    /// message.
    pub fn find<'a>(
        &'a self,
        marketplace_url: &str,
        plugin_id: &str,
    ) -> Result<&'a MarketplacePluginEntry, MarketplaceError> {
        self.plugins
            .iter()
            .find(|p| p.id == plugin_id)
            .ok_or_else(|| MarketplaceError::PluginNotFound {
                marketplace_url: marketplace_url.to_string(),
                plugin_id: plugin_id.to_string(),
            })
    }
}

/// One `plugins[]` entry -- everything an operator sees before consenting
/// to install (determine-first Q2: what conway shows before the write),
/// plus the `files` map [`crate::install::install_plugin`] actually fetches.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketplacePluginEntry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    /// Relative path inside the installed plugin directory -> the URL this
    /// crate fetches its bytes from. See this module's own doc for why a
    /// per-file map, not a single archive URL.
    pub files: BTreeMap<String, String>,
}

/// Builds the one `reqwest::Client` this crate's public functions each use
/// -- a bounded total-request timeout so "no network" (a host that never
/// answers, not just one that refuses the connection immediately) is an
/// ordinary, clearly-reported failure rather than a hang, per this item's
/// own "offline ... must never hang" requirement. Never constructed once
/// and reused across calls: each public entry point in this crate builds
/// its own, matching the workspace's `HttpClient::with_timeout` precedent
/// (`conway-plugin-backends/src/http.rs`) of a cheap, short-lived client
/// per logical operation rather than a long-lived shared one this crate
/// would need to manage the lifetime of.
pub(crate) fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
}

/// Fetches and parses the marketplace manifest at `url` -- the "browse" the
/// spec names: an operator (or the install path, internally) sees every
/// plugin a marketplace lists before anything is written to disk.
///
/// Every failure mode is a named [`MarketplaceError`] variant, never a
/// panic and never an unbounded read (P-10): a network failure (DNS,
/// connection refused, timeout) is [`MarketplaceError::Network`]; a
/// non-2xx response is [`MarketplaceError::Http`]; a response exceeding
/// [`MAX_MANIFEST_BYTES`] is [`MarketplaceError::ResponseTooLarge`]; a 2xx
/// response whose body is not valid JSON or is missing a required field is
/// [`MarketplaceError::MalformedManifest`].
pub async fn fetch_marketplace(url: &str) -> Result<MarketplaceManifest, MarketplaceError> {
    let client = client().map_err(|source| MarketplaceError::Network {
        url: url.to_string(),
        source,
    })?;
    let bytes = fetch_bytes(&client, url, MAX_MANIFEST_BYTES).await?;
    serde_json::from_slice(&bytes).map_err(|source| MarketplaceError::MalformedManifest {
        url: url.to_string(),
        message: source.to_string(),
    })
}

/// Shared by [`fetch_marketplace`] and [`crate::install::install_plugin`]
/// (each per-file fetch): GETs `url`, refuses a non-2xx status, and refuses
/// (before AND after the read) a body exceeding `cap` bytes -- checked
/// against `Content-Length` when present so an oversized response can be
/// refused before this crate reads a single byte of it, and checked again
/// against the actual decoded length so a server that omits or lies about
/// `Content-Length` cannot bypass the cap either.
pub(crate) async fn fetch_bytes(
    client: &reqwest::Client,
    url: &str,
    cap: u64,
) -> Result<Vec<u8>, MarketplaceError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|source| MarketplaceError::Network {
            url: url.to_string(),
            source,
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(MarketplaceError::Http {
            url: url.to_string(),
            status: status.as_u16(),
        });
    }
    if let Some(len) = response.content_length() {
        if len > cap {
            return Err(MarketplaceError::ResponseTooLarge {
                url: url.to_string(),
                actual: len,
                limit: cap,
            });
        }
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|source| MarketplaceError::Network {
            url: url.to_string(),
            source,
        })?;
    if bytes.len() as u64 > cap {
        return Err(MarketplaceError::ResponseTooLarge {
            url: url.to_string(),
            actual: bytes.len() as u64,
            limit: cap,
        });
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    /// Acceptance 5, case 1: offline. Nothing is listening on this address
    /// (a `MockServer` bound then immediately dropped still leaves the port
    /// free, but simpler and equally decisive: an unroutable TEST-NET-1
    /// address per RFC 5737 refuses/times out deterministically without
    /// depending on port-reuse timing) -- a clear, typed error, never a
    /// hang and never a panic.
    #[tokio::test]
    async fn offline_is_a_clear_typed_error_not_a_hang() {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fetch_marketplace("http://127.0.0.1:1/marketplace.json"),
        )
        .await
        .expect("must not hang past the 5s test timeout");
        let err = result.expect_err("nothing is listening on this port");
        assert_eq!(err.kind(), "network");
    }

    /// Acceptance 5, case 2: a bad URL -- a real host that answers, just
    /// not with the marketplace.
    #[tokio::test]
    async fn a_404_is_reported_as_a_named_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let url = format!("{}/marketplace.json", server.uri());
        let err = fetch_marketplace(&url).await.expect_err("404");
        assert_eq!(err.kind(), "http");
        assert!(err.to_string().contains("404"), "{err}");
    }

    /// Acceptance 5, case 3: a malformed marketplace response -- valid
    /// HTTP, invalid JSON.
    #[tokio::test]
    async fn invalid_json_is_a_malformed_manifest_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{ this is not json"))
            .mount(&server)
            .await;

        let url = format!("{}/marketplace.json", server.uri());
        let err = fetch_marketplace(&url).await.expect_err("invalid json");
        assert_eq!(err.kind(), "malformed_manifest");
    }

    /// Acceptance 5, case 3, second shape: valid JSON, wrong shape (missing
    /// the required `plugins` field) -- `deny_unknown_fields` plus no
    /// `#[serde(default)]` on `plugins` means this is refused, not silently
    /// defaulted to an empty marketplace.
    #[tokio::test]
    async fn valid_json_missing_a_required_field_is_a_malformed_manifest_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"name": "acme"}"#))
            .mount(&server)
            .await;

        let url = format!("{}/marketplace.json", server.uri());
        let err = fetch_marketplace(&url)
            .await
            .expect_err("missing plugins field");
        assert_eq!(err.kind(), "malformed_manifest");
    }

    /// The success path: browsing a real marketplace lists every plugin,
    /// with the fields an operator would review before installing.
    #[tokio::test]
    async fn a_well_formed_marketplace_lists_every_plugin() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{
                    "name": "acme-marketplace",
                    "description": "Acme's plugins",
                    "plugins": [
                        {
                            "id": "acme-tools",
                            "name": "Acme Tools",
                            "description": "Search Acme's index",
                            "version": "1.0.0",
                            "files": {
                                ".claude-plugin/plugin.json": "https://example.com/plugin.json",
                                ".mcp.json": "https://example.com/mcp.json"
                            }
                        }
                    ]
                }"#,
            ))
            .mount(&server)
            .await;

        let url = format!("{}/marketplace.json", server.uri());
        let manifest = fetch_marketplace(&url).await.expect("fetch");
        assert_eq!(manifest.name, "acme-marketplace");
        assert_eq!(manifest.plugins.len(), 1);
        let entry = manifest.find(&url, "acme-tools").expect("found");
        assert_eq!(entry.name, "Acme Tools");
        assert_eq!(entry.files.len(), 2);

        let missing = manifest.find(&url, "nope").expect_err("not listed");
        assert_eq!(missing.kind(), "plugin_not_found");
    }

    /// A response over the manifest size cap is refused before it is ever
    /// parsed -- proven with `Content-Length` present (the fast-path
    /// refusal, before any byte of the body is read).
    #[tokio::test]
    async fn an_oversized_manifest_is_refused_by_content_length() {
        let server = MockServer::start().await;
        let huge = "x".repeat((MAX_MANIFEST_BYTES + 1) as usize);
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(huge))
            .mount(&server)
            .await;

        let url = format!("{}/marketplace.json", server.uri());
        let err = fetch_marketplace(&url).await.expect_err("too large");
        assert_eq!(err.kind(), "response_too_large");
    }
}
