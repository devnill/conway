//! Integration tests for `conway_plugin_backends::model_metadata`:
//! `ModelMetadataStore::load` semantics (fixture loading, missing-path,
//! invalid-file, normalized lookup) plus a grep-style assertion that
//! `capabilities.rs` never gains a filesystem/network dependency.

use std::path::Path;

use conway_core::capabilities::ReliabilityTier;
use conway_core::ids::ModelId;
use conway_plugin_backends::config::ConfigError;
use conway_plugin_backends::model_metadata::{
    ModelMetadata, ModelMetadataStore, StructuredOutputSpec, ToolCallSupportSpec,
};

fn fixture_path() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/models.toml"
    ))
}

#[test]
fn load_reads_fixture_with_field_by_field_equality_for_two_entries() {
    let store = ModelMetadataStore::load(fixture_path()).expect("fixture must load");

    let qwen = store
        .get(&ModelId::new("qwen3-coder-30b"))
        .expect("qwen3-coder-30b entry must be present");
    assert_eq!(
        *qwen,
        ModelMetadata {
            id: "qwen3-coder-30b".to_string(),
            max_context_tokens: Some(32_768),
            tool_calling: Some(ToolCallSupportSpec::NonStreaming),
            parallel_tool_calls: Some(false),
            structured_output: Some(StructuredOutputSpec::JsonSchema),
            reasoning: Some(false),
            reliability_tier: Some(ReliabilityTier::Community),
            quantization: Some("Q4_K_M".to_string()),
        }
    );

    let sonnet = store
        .get(&ModelId::new("claude-sonnet-4-6"))
        .expect("claude-sonnet-4-6 entry must be present");
    assert_eq!(
        *sonnet,
        ModelMetadata {
            id: "claude-sonnet-4-6".to_string(),
            max_context_tokens: Some(200_000),
            tool_calling: Some(ToolCallSupportSpec::StreamingValidated),
            parallel_tool_calls: Some(true),
            structured_output: Some(StructuredOutputSpec::JsonSchema),
            reasoning: Some(true),
            reliability_tier: Some(ReliabilityTier::Verified),
            quantization: None,
        }
    );
}

#[test]
fn load_covers_every_fixture_entry_including_sparse_ones() {
    let store = ModelMetadataStore::load(fixture_path()).expect("fixture must load");
    assert_eq!(store.len(), 3);

    let glm = store
        .get(&ModelId::new("glm-5.2"))
        .expect("glm-5.2 entry must be present");
    assert_eq!(glm.reliability_tier, Some(ReliabilityTier::Community));
    assert_eq!(glm.tool_calling, Some(ToolCallSupportSpec::NonStreaming));
    // Fields omitted from the fixture table default to None.
    assert_eq!(glm.max_context_tokens, None);
    assert_eq!(glm.parallel_tool_calls, None);
    assert_eq!(glm.structured_output, None);
    assert_eq!(glm.reasoning, None);
    assert_eq!(glm.quantization, None);
}

#[test]
fn load_nonexistent_path_returns_ok_empty() {
    let path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/does-not-exist.toml"
    ));
    let store = ModelMetadataStore::load(path).expect("a missing path must not error");
    assert_eq!(store, ModelMetadataStore::empty());
    assert!(store.is_empty());
}

#[test]
fn load_syntactically_invalid_file_returns_metadata_error() {
    let path = std::env::temp_dir().join(format!(
        "conway-backends-invalid-models-{}-{:?}.toml",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&path, "this is [ not valid toml =").expect("temp file must write");

    let result = ModelMetadataStore::load(&path);
    std::fs::remove_file(&path).ok();

    match result {
        Err(ConfigError::Metadata { .. }) => {}
        other => panic!("expected Err(ConfigError::Metadata {{ .. }}), got {other:?}"),
    }
}

#[test]
fn lookup_order_falls_back_to_normalized_id() {
    let store = ModelMetadataStore::load(fixture_path()).expect("fixture must load");

    // Exact id present: matched directly.
    assert!(store.get(&ModelId::new("qwen3-coder-30b")).is_some());

    // Not present verbatim, but normalizes (lowercase, `:`/`/` -> `-`) to
    // an entry that is.
    let entry = store
        .get(&ModelId::new("Qwen3-Coder:30b"))
        .expect("normalized lookup must find qwen3-coder-30b");
    assert_eq!(entry.id, "qwen3-coder-30b");

    // Neither the exact id nor its normalized form exist: None.
    assert!(store
        .get(&ModelId::new("totally-unknown-model-xyz"))
        .is_none());
}

/// `capabilities.rs` composes `Capabilities` from already-resolved,
/// borrowed data; it must never gain a way to read the filesystem or the
/// network itself. Grepped here (rather than asserted inline in
/// `capabilities.rs`) so the check runs against the file's actual source
/// text regardless of what any individual function does at runtime.
#[test]
fn capabilities_module_has_no_filesystem_or_network_imports() {
    let source = include_str!("../src/capabilities.rs");
    for forbidden in ["reqwest", "std::fs", "tokio::fs"] {
        assert!(
            !source.contains(forbidden),
            "capabilities.rs must not reference {forbidden}"
        );
    }
}
