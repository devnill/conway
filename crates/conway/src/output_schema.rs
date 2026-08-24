//! Compiling a caller-supplied JSON Schema document into the
//! `schemars::schema::RootSchema` shape [`crate::SessionSpec::result_contract`]
//! (and `AgentDef`/`ForkSpec`/`SpawnSpec`'s identically-typed fields)
//! require.
//!
//! This is the one place a schema arriving as a bare `serde_json::Value` --
//! from a file `conway-cli`'s `--output-schema` read, or from anywhere else
//! an embedder's own schema originates -- gets validated and converted. It
//! generalizes the compile-and-classify step `crate::agents::
//! compile_result_contract` already applies to an agent def's own
//! frontmatter-declared `result_contract`: that helper stays private and
//! `AgentDef`-specific (its errors are `FacadeError::AgentDef`, carrying the
//! `.md` file's own path); this one is the public entry point for a schema
//! from any source, so its errors are the source-agnostic
//! `FacadeError::Config` instead.

use crate::error::{FacadeError, Result};

/// Validates `value` compiles as a JSON Schema document (draft 2020-12,
/// `jsonschema`'s default when no `$schema` keyword is present -- the same
/// compile-only pattern `crate::agents::compile_result_contract` and
/// `conway-plugin-backends`' `tool_calls::validate::SchemaValidator::compile`
/// both already use) and, on success, deserializes it into the
/// `schemars::schema::RootSchema` shape [`crate::SessionSpec::result_contract`]
/// requires. Deliberately permissive on the deserialize step: unrecognized
/// keywords land in `SchemaObject::extensions` rather than failing (`RootSchema`
/// derives a plain, non-`deny_unknown_fields` `Deserialize`), so a schema
/// written for a validator with a broader vocabulary than `schemars` models
/// still round-trips.
///
/// Both failure modes -- `value` does not compile as a schema at all, or it
/// compiles but does not deserialize into `RootSchema`'s shape (fails only
/// for a document that is not even a JSON *object*, e.g. a bare `true`/
/// `false` boolean schema, which `jsonschema` accepts as valid but
/// `RootSchema` cannot represent) -- report as [`FacadeError::Config`] with
/// `path: None`, naming the underlying error's own message. The caller
/// (`conway-cli`'s `oneshot::resolve_session`, for `--output-schema`) is
/// expected to wrap this in its own usage-error framing that names the flag
/// and the file it read `value` from; this function itself has no file to
/// name.
pub fn compile_output_schema(value: serde_json::Value) -> Result<schemars::schema::RootSchema> {
    jsonschema::validator_for(&value).map_err(|err| FacadeError::Config {
        path: None,
        message: format!("invalid JSON Schema: {err}"),
    })?;
    serde_json::from_value(value).map_err(|err| FacadeError::Config {
        path: None,
        message: format!("invalid JSON Schema: {err}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_a_valid_object_schema() {
        let schema = compile_output_schema(serde_json::json!({
            "type": "object",
            "required": ["answer"],
            "properties": {"answer": {"type": "string"}}
        }))
        .expect("valid schema compiles");
        let round_tripped = serde_json::to_value(&schema).unwrap();
        assert_eq!(round_tripped["required"], serde_json::json!(["answer"]));
    }

    #[test]
    fn rejects_a_document_that_does_not_compile_as_a_schema() {
        // `type` naming something that is not a valid JSON Schema type
        // keyword value -- `jsonschema::validator_for` rejects this at
        // compile time, before any instance is ever checked against it.
        let err = compile_output_schema(serde_json::json!({"type": "not-a-real-type"}))
            .expect_err("malformed schema must not silently compile");
        assert!(matches!(err, FacadeError::Config { .. }));
        assert!(
            err.to_string().contains("invalid JSON Schema"),
            "error should say the document is not a valid schema: {err}"
        );
    }

    #[test]
    fn round_trips_permissively_through_extensions() {
        // A schema keyword `schemars`' `RootSchema` does not model by name
        // (still a valid JSON Schema document) lands in `extensions` rather
        // than failing the deserialize -- see this module's doc.
        let schema = compile_output_schema(serde_json::json!({
            "type": "object",
            "unevaluatedProperties": false
        }))
        .expect("unrecognized-but-valid keywords must not fail compilation");
        let round_tripped = serde_json::to_value(&schema).unwrap();
        assert_eq!(round_tripped["unevaluatedProperties"], false);
    }
}
