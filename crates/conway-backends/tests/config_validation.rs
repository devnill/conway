//! Integration tests for `conway_backends::config`: the `sk-ant-oat*`
//! rejection-at-parse-time requirement (C-02, GP-09) and standard-key
//! acceptance, exercised through both TOML and JSON deserialization.

use conway_backends::config::{AnthropicConfig, ConfigError};

#[test]
fn subscription_oauth_token_is_rejected_at_parse_time_via_json() {
    let json = r#"{"api_key": "sk-ant-oat01-abc"}"#;
    let result: Result<AnthropicConfig, _> = serde_json::from_str(json);
    let err = result.expect_err("a sk-ant-oat* key must fail to deserialize");
    let message = err.to_string();
    assert!(
        message.contains("sk-ant-oat"),
        "error message missing `sk-ant-oat` substring: {message:?}"
    );
    assert!(
        message.contains("subscription OAuth tokens are not supported"),
        "error message missing explanation substring: {message:?}"
    );
}

#[test]
fn subscription_oauth_token_is_rejected_at_parse_time_via_toml() {
    let toml_src = r#"api_key = "sk-ant-oat01-abc""#;
    let result: Result<AnthropicConfig, _> = toml::from_str(toml_src);
    let err = result.expect_err("a sk-ant-oat* key must fail to deserialize");
    let message = err.to_string();
    assert!(message.contains("sk-ant-oat"), "{message:?}");
    assert!(
        message.contains("subscription OAuth tokens are not supported"),
        "{message:?}"
    );
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

#[test]
fn subscription_token_prefix_check_ignores_leading_trailing_whitespace() {
    let json = r#"{"api_key": "  sk-ant-oat01-abc  "}"#;
    let result: Result<AnthropicConfig, _> = serde_json::from_str(json);
    assert!(result.is_err(), "trimmed key must still be rejected");
}

#[test]
fn config_error_display_matches_the_exact_specified_message() {
    assert_eq!(
        ConfigError::SubscriptionTokenRejected.to_string(),
        "Anthropic subscription OAuth tokens (sk-ant-oat*) are not supported: subscription OAuth tokens are not supported by conway; use a standard API key (sk-ant-api*) from console.anthropic.com"
    );
}
