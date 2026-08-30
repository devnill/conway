//! Board item `01M1A9J9C9YRH3YPTGD335HZPZ`, defect 1 (and its acceptance
//! 2): **a THIRD real Claude Code manifest shape, found within hours of
//! `01M0Y6RYZA94BK6YXJ7X8TNEGR` shipping the first two** -- a `source`
//! that is a plain STRING, not an object naming `git-subdir`/`github`.
//!
//! # Why this file exists
//!
//! The operator's very next attempt after `01M0Y6RYZA94BK6YXJ7X8TNEGR`
//! landed -- installing ideate's OWN marketplace -- hit
//! `"source": "./"`. conway's custom [`PluginSource`] `Deserialize` looked
//! for a `source` field INSIDE that string (there is none; a string has no
//! fields) and reported `missing field `source`` -- an accurate-sounding
//! but useless error about the very value it was trying to read. This file
//! is `claude_code_manifest.rs`'s own direct sibling: a real manifest,
//! fixed in place, whose own tests below assert what conway does with it.
//!
//! # The fixture is real, not representative
//!
//! `fixtures/ideate-marketplace.json` is the verbatim body of
//! `https://raw.githubusercontent.com/ideate-ai/ideate/HEAD/.claude-plugin/marketplace.json`,
//! fetched 2026-08-30 -- the exact document the operator's failing command
//! (`/plugin install https://github.com/ideate-ai/ideate ideate`) was
//! reaching for. Checked in rather than fetched at test time, for the
//! identical reason `claude_code_manifest.rs`'s own doc gives: a test that
//! needs the network fails for reasons unrelated to what it asserts.

use conway_plugin_marketplace::manifest::{fetch_marketplace, MarketplaceManifest, PluginSource};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The verbatim bytes of ideate's own, real, published Claude Code
/// marketplace manifest. See this file's own doc for provenance.
const REAL_IDEATE_MANIFEST: &str = include_str!("fixtures/ideate-marketplace.json");

/// The shape of the real document, asserted directly against the raw JSON
/// -- so a later reader (or a fixture silently replaced by a different
/// file) sees exactly what conway's parser has to represent, independent
/// of whether conway's own types currently succeed at it.
#[test]
fn the_real_ideate_manifest_names_a_plain_string_source() {
    let doc: serde_json::Value =
        serde_json::from_str(REAL_IDEATE_MANIFEST).expect("fixture is valid JSON");

    assert_eq!(doc["plugins"].as_array().expect("array").len(), 1);
    let entry = &doc["plugins"][0];
    assert_eq!(entry["name"], "ideate");
    assert!(entry.get("id").is_none(), "identity is `name`, not `id`");
    assert!(
        entry["source"].is_string(),
        "the defect this file exists for: `source` is a bare string, not an object naming a \
         `git-subdir`/`github` kind -- got: {:?}",
        entry["source"]
    );
    assert_eq!(entry["source"], "./");
}

/// **The discriminating observable, established by inspection rather than
/// by running the pre-fix code (this crate's own worker builds no cargo --
/// see the wave briefing): `serde_json::Value::get` returns `None` for
/// every index against a `Value::String` (there is no field to get), which
/// is exactly what the pre-fix `PluginSource::Deserialize` called
/// unconditionally before ever checking whether `value` was a string at
/// all. That `None` fed `serde::de::Error::missing_field("source")` --
/// the literal defect report's own reproduction, verbatim.** This test
/// asserts the POST-fix behavior: parsing the same real document now
/// succeeds, and the plugin's `source` is
/// [`PluginSource::RelativePath`] naming the exact string the manifest
/// declared. The build lane must confirm the "before" half by running this
/// test against a checkout that predates `PluginSource::RelativePath` (or
/// by reverting just that variant/branch) and observing it fail with a
/// `missing field `source`` message -- named here so that confirmation is
/// a mechanical rerun, not a fresh investigation.
#[test]
fn conways_manifest_type_now_parses_ideates_own_real_document() {
    let manifest: MarketplaceManifest = serde_json::from_str(REAL_IDEATE_MANIFEST)
        .expect("conway's schema must now represent a plain-string `source`");

    assert_eq!(manifest.name, "ideate-marketplace");
    assert_eq!(manifest.plugins.len(), 1);

    let ideate = manifest
        .find("https://raw.githubusercontent.com/ideate-ai/ideate/HEAD/.claude-plugin/marketplace.json", "ideate")
        .expect("identified by `name`, not `id`");
    assert_eq!(
        ideate.source,
        Some(PluginSource::RelativePath {
            path: "./".to_string()
        })
    );
    assert!(ideate.files.is_empty());
}

/// The operator's actual "browse" path, end to end over HTTP: a real
/// self-hosted marketplace served at a URL, fetched by the same function
/// `/plugin install` calls. This is the half of the operator's report that
/// does not depend on GitHub-URL resolution (`resolve_marketplace_url`'s
/// own tests in `manifest.rs` cover that half in isolation, since it is a
/// pure string transform with nothing to mock): whatever URL conway
/// actually GETs, the BODY still has to parse, and this proves it does.
#[tokio::test]
async fn fetching_ideates_own_marketplace_now_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.claude-plugin/marketplace.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(REAL_IDEATE_MANIFEST, "application/json"),
        )
        .mount(&server)
        .await;

    let url = format!("{}/.claude-plugin/marketplace.json", server.uri());
    let manifest = fetch_marketplace(&url)
        .await
        .expect("ideate's own real, self-hosted marketplace is browsable today");

    assert_eq!(manifest.plugins.len(), 1);
    let entry = manifest.find(&url, "ideate").expect("found");
    assert_eq!(
        entry.source,
        Some(PluginSource::RelativePath {
            path: "./".to_string()
        })
    );
}
