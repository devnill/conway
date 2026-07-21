//! Thin wrapper over the [`jsonschema`] crate: compiles each [`ToolSpec`]'s
//! JSON Schema once (at [`SchemaValidator::compile`], invoked from
//! `ToolCallAccumulator::new`) and validates accumulated tool-call
//! arguments against it at `finish` time.
//!
//! **Choice of validator (documented per the work item's instruction to
//! record the decision):** the [`jsonschema`] crate is used directly rather
//! than a hand-rolled required/type checker. `ToolSpec.schema` is a
//! `schemars::schema::RootSchema`, which `Serialize`s to a standard JSON
//! Schema document (with `$schema`, `definitions`, etc. — schemars 0.8
//! defaults to draft-07); `jsonschema::validator_for` accepts that
//! `serde_json::Value` directly with no shape translation needed. This
//! gives full JSON Schema semantics (nested `properties`, `oneOf`, `enum`,
//! numeric bounds, …) for free and, critically, a real `schema_path()` on
//! failure — the "failing schema path" wording in this item's acceptance
//! criteria maps directly onto `ValidationError::schema_path()`, which a
//! hand-rolled required/type walker would have to reinvent. The crate is
//! pulled in with `default-features = false` (no `resolve-http`/
//! `resolve-file`/TLS features) since this module never resolves external
//! `$ref`s over the network or filesystem — only the schema embedded in
//! each `ToolSpec`.
//!
//! A schema that fails to compile is a [`BackendError::BadRequest`]: a
//! malformed tool registration is a caller/registration bug, not a
//! stream-parsing failure, so it must never be reported as `ToolParse`. A
//! schema that compiles but rejects a particular argument value at
//! validation time IS a `ToolParse`, since that failure only becomes
//! observable once the stream has produced a complete (but invalid) call.

use std::collections::HashMap;

use conway_core::content::ToolSpec;
use conway_core::error::BackendError;
use conway_core::ids::ToolName;
use serde_json::Value;

/// One compiled `jsonschema::Validator` per registered tool name.
pub(crate) struct SchemaValidator {
    validators: HashMap<ToolName, jsonschema::Validator>,
}

impl SchemaValidator {
    /// Compiles every `spec.schema` once. A schema that is not
    /// JSON-serializable, or that fails to compile as a JSON Schema
    /// document, is a `BadRequest` naming the offending tool.
    pub(crate) fn compile(specs: &[ToolSpec]) -> Result<Self, BackendError> {
        let mut validators = HashMap::with_capacity(specs.len());
        for spec in specs {
            let schema_value =
                serde_json::to_value(&spec.schema).map_err(|err| BackendError::BadRequest {
                    detail: format!(
                        "tool `{}`: schema is not serializable to JSON: {err}",
                        spec.name
                    ),
                })?;
            let validator = jsonschema::validator_for(&schema_value).map_err(|err| {
                BackendError::BadRequest {
                    detail: format!("tool `{}`: schema failed to compile: {err}", spec.name),
                }
            })?;
            validators.insert(spec.name.clone(), validator);
        }
        Ok(Self { validators })
    }

    /// Validates `arguments` against the compiled schema for `name`.
    /// `ToolCallAccumulator::finish` already checks `name` against the
    /// known-tool set before calling this (to produce the exact "unknown
    /// tool" message it owns); the `unknown tool` branch below is a
    /// defensive fallback, not the primary path for that criterion.
    pub(crate) fn validate(&self, name: &ToolName, arguments: &Value) -> Result<(), BackendError> {
        let validator = self
            .validators
            .get(name)
            .ok_or_else(|| BackendError::ToolParse {
                detail: format!("unknown tool `{name}`"),
            })?;
        validator
            .validate(arguments)
            .map_err(|err| BackendError::ToolParse {
                detail: format!(
                    "tool `{name}`: arguments failed schema validation at `{}`: {err}",
                    err.schema_path()
                ),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::content::{PermissionClass, ToolCategory};

    fn tool(name: &str, schema_json: serde_json::Value) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(name),
            description: "test tool".into(),
            schema: serde_json::from_value(schema_json).expect("valid RootSchema JSON"),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    #[test]
    fn compiles_and_validates_a_required_property_schema() {
        let spec = tool(
            "read",
            serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        );
        let validator = SchemaValidator::compile(&[spec]).unwrap();
        let name = ToolName::new("read");
        assert!(validator
            .validate(&name, &serde_json::json!({"path": "a.txt"}))
            .is_ok());

        let err = validator
            .validate(&name, &serde_json::json!({}))
            .unwrap_err();
        match err {
            BackendError::ToolParse { detail } => {
                assert!(detail.contains("read"), "{detail}");
                assert!(
                    detail.contains("required") || detail.contains('/'),
                    "{detail}"
                );
            }
            other => panic!("expected ToolParse, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tool_name_is_tool_parse() {
        let validator = SchemaValidator::compile(&[]).unwrap();
        let err = validator
            .validate(&ToolName::new("nope"), &serde_json::json!({}))
            .unwrap_err();
        match err {
            BackendError::ToolParse { detail } => assert!(detail.contains("unknown tool")),
            other => panic!("expected ToolParse, got {other:?}"),
        }
    }
}
