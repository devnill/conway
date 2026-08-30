//! Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`, acceptance 1: **the real
//! incompatibility, asserted against real bytes -- and, once the ruling
//! landed, the real ACCEPTANCE, asserted against the identical bytes.**
//!
//! # Why this file exists
//!
//! `01M0VR96Y87FF2BVNTBSC6GEYR` shipped as "Install a plugin from a Claude
//! Code marketplace" and its own end-to-end test
//! ([`end_to_end.rs`](end_to_end.rs)) passes -- against a manifest **this
//! project wrote in this project's own format**. The first operator to
//! point `/plugin install` at a real Claude Code marketplace got a
//! `serde_json` parse error about GitHub's HTML.
//!
//! The gap between those two facts was a test that had never been written:
//! no test in this crate had ever fed it a manifest conway did not author.
//! This file closes that gap by fixing the fixture in place, not by adding
//! a second one: `fixtures/claude-code-marketplace.json` is unchanged real
//! bytes; what changed is what these tests assert ABOUT them.
//!
//! # The fixture is real, not representative
//!
//! `fixtures/claude-code-marketplace.json` is the verbatim body of
//! `https://raw.githubusercontent.com/devnill/claude-marketplace/HEAD/.claude-plugin/marketplace.json`,
//! fetched 2026-08-26 -- the exact document the operator's failing command
//! was reaching for. It is checked in rather than fetched at test time
//! because a test that needs the network is a test that fails for reasons
//! unrelated to the thing it asserts (and this crate's own suite is
//! otherwise network-free by construction, per `end_to_end.rs`'s "never a
//! real network host" constraint).
//!
//! # This file's own history, kept rather than deleted
//!
//! Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`'s ruling adopted the Claude Code
//! schema (its three candidates were named as options ahead of that
//! ruling: adopt, document-the-limitation, or narrow to a plain `git
//! clone` -- the FIRST is what happened). Before that ruling landed, this
//! file's own tests asserted REFUSAL against this exact fixture -- a
//! deliberate characterization test, proving the incompatibility was a
//! fact CI re-checked rather than a code reading that could rot. The
//! tests below are their direct descendants, rewritten into assertions
//! about ACCEPTANCE now that the fetcher exists (`crate::git_source`,
//! reached through `crate::install::install_entry`) -- exactly what that
//! prior version of this file said its own successor would have to do.
//! `git status -- crates/conway-plugin-marketplace/tests/claude_code_manifest.rs`
//! before this commit is the record of what changed and why.

use std::collections::BTreeSet;

use conway_plugin_marketplace::manifest::{fetch_marketplace, MarketplaceManifest, PluginSource};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The verbatim bytes of a real, published Claude Code marketplace
/// manifest. See this file's own doc for provenance.
const REAL_CLAUDE_CODE_MANIFEST: &str = include_str!("fixtures/claude-code-marketplace.json");

/// The shape of the real document, asserted directly, so that a later
/// reader can see what conway's parser now has to represent without going
/// to the network -- and so that a fixture silently replaced by a
/// different file fails here rather than producing a confusing failure
/// downstream. Every number in this test is a fact about the published
/// document, not a design choice of conway's.
#[test]
fn the_real_manifest_has_the_claude_code_shape_this_crate_now_understands() {
    let doc: serde_json::Value =
        serde_json::from_str(REAL_CLAUDE_CODE_MANIFEST).expect("fixture is valid JSON");

    assert!(doc.get("owner").is_some(), "real manifests carry `owner`");
    assert!(
        doc.get("metadata").is_some(),
        "real manifests carry `metadata`"
    );

    let plugins = doc["plugins"].as_array().expect("`plugins` is an array");
    assert_eq!(plugins.len(), 7, "fixture pins the published plugin count");

    assert!(
        plugins.iter().all(|p| p.get("id").is_none()),
        "no real entry has an `id` -- identity is `name`"
    );
    assert!(
        plugins.iter().all(|p| p.get("files").is_none()),
        "no real entry has a `files` map -- it names a `source` instead"
    );
    assert!(
        plugins.iter().all(|p| p.get("source").is_some()),
        "every real entry has a `source`"
    );

    // `source` is not one shape but a tagged family -- both kinds this
    // fixture uses are git-based, which is exactly what board item
    // `01M0Y6RYZA94BK6YXJ7X8TNEGR`'s ruling scoped this crate's fetcher to.
    let kinds: BTreeSet<&str> = plugins
        .iter()
        .filter_map(|p| p["source"]["source"].as_str())
        .collect();
    assert_eq!(
        kinds,
        ["git-subdir", "github"].into_iter().collect(),
        "both published source kinds are git-based"
    );
}

/// The direct parse: conway's own type, fed the real document, now
/// SUCCEEDS -- the inverse of what this test asserted before the ruling
/// landed. Asserted at the type level rather than through HTTP so success
/// is attributable to the SCHEMA alone.
#[test]
fn conways_manifest_type_now_parses_the_real_document() {
    let manifest: MarketplaceManifest = serde_json::from_str(REAL_CLAUDE_CODE_MANIFEST)
        .expect("conway's schema must now represent a real Claude Code marketplace");

    assert_eq!(manifest.name, "marketplace");
    assert_eq!(
        manifest.owner.as_ref().map(|o| o.name.as_str()),
        Some("Dan Singer")
    );
    assert_eq!(
        manifest.metadata.as_ref().map(|m| m.version.as_str()),
        Some("3.0.11")
    );
    assert_eq!(manifest.plugins.len(), 7);

    let beepboop = manifest
        .find("https://example.invalid/marketplace.json", "beepboop")
        .expect("identified by `name`, not `id`");
    assert_eq!(
        beepboop.source,
        Some(PluginSource::GitSubdir {
            url: "https://github.com/devnill/beepboop".to_string(),
            path: "plugin".to_string(),
        })
    );
    assert!(beepboop.files.is_empty());

    let ideate = manifest
        .find("https://example.invalid/marketplace.json", "ideate")
        .expect("a second real entry, the `github` source kind");
    assert_eq!(
        ideate.source,
        Some(PluginSource::Github {
            repo: "ideate-ai/ideate".to_string(),
        })
    );
}

/// The operator's actual path, end to end over HTTP: a real manifest served
/// at a URL, fetched by the same function `/plugin install` calls.
///
/// This is the one that would have caught the original bug, and now proves
/// its fix: browsing a real, published Claude Code marketplace succeeds.
/// Whether any ONE of its plugins can then be INSTALLED is a further step
/// (`crate::git_source`, exercised by `crate::install`'s own tests against
/// a stub `git`, never a real network host here) -- this test's own scope
/// is "browse", the half `fetch_marketplace` performs.
#[tokio::test]
async fn fetching_a_real_claude_code_marketplace_now_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.claude-plugin/marketplace.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(REAL_CLAUDE_CODE_MANIFEST, "application/json"),
        )
        .mount(&server)
        .await;

    let url = format!("{}/.claude-plugin/marketplace.json", server.uri());
    let manifest = fetch_marketplace(&url)
        .await
        .expect("a real Claude Code marketplace is installable today");

    assert_eq!(manifest.plugins.len(), 7);
    assert!(manifest.find(&url, "beepboop").is_ok());
}
