//! Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`, acceptance 1: **the real
//! incompatibility, asserted against real bytes.**
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
//! The gap between those two facts is a test that was never written: no
//! test in this crate had ever fed it a manifest conway did not author. So
//! the incompatibility was a *code reading* -- true, but the kind of true
//! that rots. This file makes it a fact that CI re-checks.
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
//! # What these tests will do when the ruling lands
//!
//! **They are written to fail loudly, not to quietly keep passing.**
//! Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR` carries three candidate rulings;
//! under ruling (a) -- adopt the Claude Code schema -- every assertion here
//! about *refusal* becomes wrong, and that is the point: whoever implements
//! (a) must come here and rewrite these into assertions about acceptance.
//! Under rulings (b) and (c) they stay exactly as they are and become the
//! standing proof of a deliberate limitation. Either way the next person
//! reads this file before they change behaviour, which is the whole job of
//! a characterization test.

use conway_plugin_marketplace::manifest::{fetch_marketplace, MarketplaceManifest};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The verbatim bytes of a real, published Claude Code marketplace
/// manifest. See this file's own doc for provenance.
const REAL_CLAUDE_CODE_MANIFEST: &str = include_str!("fixtures/claude-code-marketplace.json");

/// The shape of the real document, asserted directly, so that a later
/// reader can see *what* conway is incompatible with without going to the
/// network -- and so that a fixture silently replaced by a different file
/// fails here rather than producing a confusing failure downstream.
///
/// Every number in this test is a fact about the published document, not a
/// design choice of conway's.
#[test]
fn the_real_manifest_has_the_shape_conways_parser_does_not_accept() {
    let doc: serde_json::Value =
        serde_json::from_str(REAL_CLAUDE_CODE_MANIFEST).expect("fixture is valid JSON");

    // Two top-level fields conway's `deny_unknown_fields` has no home for.
    assert!(doc.get("owner").is_some(), "real manifests carry `owner`");
    assert!(
        doc.get("metadata").is_some(),
        "real manifests carry `metadata`"
    );

    let plugins = doc["plugins"].as_array().expect("`plugins` is an array");
    assert_eq!(plugins.len(), 7, "fixture pins the published plugin count");

    // The entry shape differs in kind, not in detail: not one entry carries
    // either of the two fields `MarketplacePluginEntry` requires, and every
    // entry carries the one field it has no place for.
    assert!(
        plugins.iter().all(|p| p.get("id").is_none()),
        "no real entry has an `id` -- identity is `name`"
    );
    assert!(
        plugins.iter().all(|p| p.get("files").is_none()),
        "no real entry has a `files` map -- conway requires one"
    );
    assert!(
        plugins.iter().all(|p| p.get("source").is_some()),
        "every real entry has a `source` -- conway has no field for it"
    );

    // And `source` is not one shape but a tagged family, none of which is a
    // per-file URL map. This is layer 4 of the board item: the half that
    // needs a fetcher this crate deliberately does not have.
    let kinds: std::collections::BTreeSet<&str> = plugins
        .iter()
        .filter_map(|p| p["source"]["source"].as_str())
        .collect();
    assert_eq!(
        kinds,
        ["git-subdir", "github"].into_iter().collect(),
        "both published source kinds need git, which this crate does not do"
    );
}

/// The direct parse: conway's own type, fed the real document.
///
/// Asserted at the type level rather than through HTTP so the failure is
/// attributable to the *schema* and nothing else.
#[test]
fn conways_manifest_type_refuses_the_real_document() {
    let err = serde_json::from_str::<MarketplaceManifest>(REAL_CLAUDE_CODE_MANIFEST)
        .expect_err("conway's schema cannot represent a real Claude Code marketplace");

    // `deny_unknown_fields` reports the first unknown field it reaches.
    // Asserting the message names `owner` pins *which* incompatibility bites
    // first, so that a future change that fixes only the entry shape does
    // not silently leave this passing for a different reason than it does
    // today.
    let message = err.to_string();
    assert!(
        message.contains("owner"),
        "expected the first refusal to name the `owner` field, got: {message}"
    );
}

/// The operator's actual path, end to end over HTTP: a real manifest served
/// at a URL, fetched by the same function `/plugin install` calls.
///
/// This is the one that would have caught the bug. `end_to_end.rs` proves
/// the happy path against a conway-authored manifest; nothing proved what
/// happened when the document came from somebody else.
#[tokio::test]
async fn fetching_a_real_claude_code_marketplace_fails_as_a_malformed_manifest() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.claude-plugin/marketplace.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(REAL_CLAUDE_CODE_MANIFEST, "application/json"),
        )
        .mount(&server)
        .await;

    let url = format!("{}/.claude-plugin/marketplace.json", server.uri());
    let err = fetch_marketplace(&url)
        .await
        .expect_err("a real Claude Code marketplace is not installable today");

    // NOT `not_a_manifest_url`: the URL was right and the body was real
    // JSON. The failure is the schema, which is precisely the distinction
    // layer 1's new error variant exists to preserve -- fixing the URL
    // guidance must not paper over the deeper incompatibility.
    assert_eq!(
        err.kind(),
        "malformed_manifest",
        "expected a schema refusal, got: {err}"
    );
}
