//! Integration tests for `conway_plugin_backends::config`: key acceptance and the
//! empty-key check, exercised through both TOML and JSON deserialization.
//!
//! conway does not inspect the shape of an API key. Any non-empty value
//! parses, which is what lets an Anthropic-compatible third-party endpoint
//! (a coding-plan subscription, a self-hosted shim) be configured without
//! conway adjudicating whether the credential looks legitimate.

use conway_plugin_backends::config::AnthropicConfig;

#[test]
fn any_non_empty_key_shape_parses_via_json() {
    for key in [
        "sk-ant-oat01-abc",
        "sk-ant-api03-abc",
        "kimi-coding-plan-key",
        "an-arbitrary-third-party-token",
    ] {
        let json = format!(r#"{{"api_key": "{key}"}}"#);
        let cfg: AnthropicConfig = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("key shape must not be inspected: {key} ({e})"));
        assert_eq!(cfg.api_key.expose_secret(), key);
    }
}

#[test]
fn any_non_empty_key_shape_parses_via_toml() {
    let toml_src = r#"api_key = "sk-ant-oat01-abc""#;
    let cfg: AnthropicConfig = toml::from_str(toml_src).expect("key shape must not be inspected");
    assert_eq!(cfg.api_key.expose_secret(), "sk-ant-oat01-abc");
}

#[test]
fn standard_api_key_parses_successfully_via_json() {
    let json = r#"{"api_key": "sk-ant-api03-abc"}"#;
    let cfg: AnthropicConfig =
        serde_json::from_str(json).expect("a standard sk-ant-api* key must parse");
    assert_eq!(cfg.api_key.expose_secret(), "sk-ant-api03-abc");
}

#[test]
fn standard_api_key_parses_successfully_via_toml() {
    let toml_src = r#"api_key = "sk-ant-api03-abc""#;
    let cfg: AnthropicConfig =
        toml::from_str(toml_src).expect("a standard sk-ant-api* key must parse");
    assert_eq!(cfg.api_key.expose_secret(), "sk-ant-api03-abc");
}

#[test]
fn defaults_are_applied_when_optional_fields_are_omitted() {
    let toml_src = r#"api_key = "sk-ant-api03-abc""#;
    let cfg: AnthropicConfig = toml::from_str(toml_src).unwrap();
    assert_eq!(cfg.base_url.as_str(), "https://api.anthropic.com/");
    assert_eq!(cfg.anthropic_version, "2023-06-01");
    assert!(cfg.timeout.is_none());
    assert!(cfg.models.is_empty());
    assert_eq!(cfg.effective_timeout(), std::time::Duration::from_secs(600));
}

#[test]
fn explicit_fields_override_defaults() {
    let toml_src = r#"
        api_key = "sk-ant-api03-abc"
        base_url = "https://proxy.internal/anthropic"
        anthropic_version = "2024-01-01"
        timeout = "30s"

        [models.claude-haiku-4-5]
        stream_tools = true
    "#;
    let cfg: AnthropicConfig = toml::from_str(toml_src).unwrap();
    assert_eq!(cfg.base_url.as_str(), "https://proxy.internal/anthropic");
    assert_eq!(cfg.anthropic_version, "2024-01-01");
    assert_eq!(cfg.timeout, Some(std::time::Duration::from_secs(30)));
    assert_eq!(cfg.effective_timeout(), std::time::Duration::from_secs(30));
    assert_eq!(
        cfg.models
            .get("claude-haiku-4-5")
            .and_then(|m| m.stream_tools),
        Some(true)
    );
}

#[test]
fn empty_or_whitespace_api_key_is_rejected_with_missing_api_key() {
    for bad in ["", "   ", "\t\n"] {
        let json = format!(r#"{{"api_key": "{bad}"}}"#);
        let result: Result<AnthropicConfig, _> = serde_json::from_str(&json);
        assert!(result.is_err(), "expected rejection for {bad:?}");
    }
}

/// A key that is only whitespace is still a missing key: the trim happens
/// so `"   "` cannot masquerade as a configured credential.
#[test]
fn a_whitespace_only_key_is_treated_as_missing() {
    let json = r#"{"api_key": "   "}"#;
    let result: Result<AnthropicConfig, _> = serde_json::from_str(json);
    assert!(result.is_err(), "a whitespace-only key must be rejected");
}

/// Surrounding whitespace does not change a real key's acceptance -- only
/// emptiness is checked, not shape.
#[test]
fn a_padded_key_still_parses() {
    let json = r#"{"api_key": "  sk-ant-oat01-abc  "}"#;
    let cfg: AnthropicConfig =
        serde_json::from_str(json).expect("a padded but non-empty key must parse");
    assert_eq!(cfg.api_key.expose_secret(), "  sk-ant-oat01-abc  ");
}
