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
//!
//! **Narrow coercion of a stringified argument (board item
//! `01M23SDCE6T85Z48CRQ8NBY6PV`, ruled -- not open).** A model occasionally
//! wraps a nested-object argument in quotes -- e.g. `conway_spawn`'s
//! `budget` sent as the STRING `"{\"max_steps\": 5}"` instead of the object
//! itself -- which trips this validator (the string matches neither branch
//! of the declared `anyOf: [object, null]`) even though the model's
//! *intent* is completely unambiguous. [`SchemaValidator::validate`]
//! attempts exactly one narrow repair before giving up: when validation
//! fails, it reads the value at the failing `instance_path`, and IF that
//! value is a string AND the string parses as JSON AND the PARSED value's
//! own shape is an object or `null` (never a bare scalar or array -- see
//! [`try_coerce`]'s own "shape gate" doc for why parse-then-revalidate
//! alone is not narrow enough) AND substituting the parsed value at that
//! same path makes the WHOLE document validate against the SAME compiled
//! schema that rejected it, the parsed value is used instead. This can
//! never change meaning -- a value that validates against the tool's own
//! declared schema is, by construction, a value the tool already promised
//! to accept -- so it is wire-format normalization, not core inventing a
//! substitute for something the model did not say. A string that parses as
//! JSON but does NOT then validate, or whose parsed shape is a scalar or
//! array, is left alone and reported as the ordinary `ToolArgumentsInvalid`
//! below; so is a string that does not parse as JSON at all. Every firing
//! is logged (`tracing::info!`, naming the tool and the argument path, and
//! -- on a streaming backend -- surfaced durably as `Event::
//! ToolArgumentCoerced`, `StreamChunk::ToolArgumentCoerced`'s own doc) so
//! it is countable rather than silent -- see `docs/agents.md`'s "When a
//! model's tool call is malformed" section for the full three-step policy
//! this is step 1 of.

use std::collections::HashMap;

use conway_core::content::ToolSpec;
use conway_core::error::BackendError;
use conway_core::ids::ToolName;
use serde_json::Value;

/// Attempts the narrow coercion described in this module's own doc: `at` is
/// a JSON Pointer (RFC 6901, e.g. `/budget`) into `arguments` naming the
/// value schema validation rejected. Returns the coerced document only when
/// ALL of: the value at `at` is a string; that string parses as JSON; the
/// PARSED value's own shape is an object or `null` (see this fn's "shape
/// gate" note below); and the resulting document validates against
/// `validator`. `at == ""` names the whole document (`Value::pointer("")`
/// is the RFC 6901 convention for "the document itself"), which
/// `Value::pointer`/`pointer_mut` already handle without a special case.
///
/// **Shape gate (board item `01M23SDCE6T85Z48CRQ8NBY6PV`, review round 2 --
/// closes a real widening the first draft of this function had).** Parse-
/// then-revalidate alone is NOT sufficient to keep this narrow: verified
/// empirically against this workspace's own vendored `jsonschema` 0.48.2,
/// `{"max_steps": "5"}` against `{"type":"integer"}` coerces to the number
/// `5`, and `{"flag": "true"}` against `{"type":"boolean"}` coerces to the
/// boolean `true` -- both are numeric/boolean-STRING coercion, which the
/// operator's own ruling explicitly forbids ("No numeric-string coercion,
/// no missing-field defaulting, no shape guessing"), even though both
/// mechanically satisfy "parses as JSON, then validates." A bare stringified
/// scalar is exactly the "shape guessing" the ruling names: unlike a
/// stringified OBJECT (THE EVIDENCE's own `budget: "{\"max_steps\": 5}"`,
/// a well-known LLM tool-calling artifact -- a model that learned a
/// stringify-structured-args convention from a different framework), a
/// stringified scalar is at least as likely to be a genuine, intentional
/// string value in some OTHER schema shape, so accepting it by construction
/// is closer to guessing than to normalizing a wire-format artifact. This
/// function therefore coerces only when the PARSED value's own top-level
/// shape is [`Value::Object`] or [`Value::Null`] -- never [`Value::Array`],
/// [`Value::String`], [`Value::Number`], or [`Value::Bool`] -- deliberately
/// narrower than "any shape schema-valid at this path," matching exactly
/// the shape THE EVIDENCE's own `budget: Option<BudgetArg>` (`anyOf:
/// [object, null]`) needs and no more.
fn try_coerce(validator: &jsonschema::Validator, arguments: &Value, at: &str) -> Option<Value> {
    let offending = arguments.pointer(at)?;
    let raw = offending.as_str()?;
    let parsed: Value = serde_json::from_str(raw).ok()?;
    if !matches!(parsed, Value::Object(_) | Value::Null) {
        return None;
    }
    let mut coerced = arguments.clone();
    if at.is_empty() {
        coerced = parsed;
    } else {
        *coerced.pointer_mut(at)? = parsed;
    }
    validator.validate(&coerced).ok()?;
    Some(coerced)
}

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

    /// Validates `arguments` against the compiled schema for `name`,
    /// attempting the narrow coercion this module's own doc describes when
    /// validation fails. `ToolCallAccumulator::finish` already checks
    /// `name` against the known-tool set before calling this (to produce
    /// the exact "unknown tool" message it owns); the `unknown tool` branch
    /// below is a defensive fallback, not the primary path for that
    /// criterion. On a failure coercion cannot repair, the error is
    /// [`BackendError::ToolArgumentsInvalid`] (never the bare
    /// [`BackendError::ToolParse`]) -- it carries `arguments` and
    /// `call_id` so `AttemptEngine` (`conway-runtime`) can hand the model a
    /// corrective `ToolResult` naming what was wrong (board item
    /// `01M23SDCE6T85Z48CRQ8NBY6PV` step 2).
    pub(crate) fn validate(
        &self,
        name: &ToolName,
        call_id: &str,
        arguments: Value,
    ) -> Result<Validated, BackendError> {
        let validator = self
            .validators
            .get(name)
            .ok_or_else(|| BackendError::ToolParse {
                detail: format!("unknown tool `{name}`"),
            })?;
        let Err(err) = validator.validate(&arguments) else {
            return Ok(Validated::AsIs(arguments));
        };
        let at = err.instance_path().as_str().to_string();
        let detail = format!(
            "tool `{name}`: arguments failed schema validation at `{}`: {err}",
            err.schema_path()
        );
        if let Some(coerced) = try_coerce(validator, &arguments, &at) {
            return Ok(Validated::Coerced {
                value: coerced,
                argument_path: at,
            });
        }
        Err(BackendError::ToolArgumentsInvalid {
            tool: name.clone(),
            call_id: call_id.to_string(),
            arguments: Box::new(arguments),
            argument_path: at,
            detail,
        })
    }
}

/// [`SchemaValidator::validate`]'s outcome on the success path: whether
/// `arguments` already validated as given, or coercion fired to produce a
/// replacement -- distinguished so the caller (`ToolCallAccumulator::
/// finish`) can record EACH firing (tool, call, argument path) for its own
/// callers to surface durably (`StreamChunk::ToolArgumentCoerced` ->
/// `Event::ToolArgumentCoerced`, board item `01M23SDCE6T85Z48CRQ8NBY6PV`
/// step 1's "every firing is logged... countable rather than silent"
/// requirement) rather than silently returning the same `Ok(Value)` either
/// way.
#[derive(Debug)]
pub(crate) enum Validated {
    AsIs(Value),
    Coerced { value: Value, argument_path: String },
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
            .validate(&name, "call_1", serde_json::json!({"path": "a.txt"}))
            .is_ok());

        let err = validator
            .validate(&name, "call_1", serde_json::json!({}))
            .unwrap_err();
        match err {
            BackendError::ToolArgumentsInvalid { detail, tool, .. } => {
                assert_eq!(tool, ToolName::new("read"));
                assert!(
                    detail.contains("required") || detail.contains('/'),
                    "{detail}"
                );
            }
            other => panic!("expected ToolArgumentsInvalid, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tool_name_is_tool_parse() {
        let validator = SchemaValidator::compile(&[]).unwrap();
        let err = validator
            .validate(&ToolName::new("nope"), "call_1", serde_json::json!({}))
            .unwrap_err();
        match err {
            BackendError::ToolParse { detail } => assert!(detail.contains("unknown tool")),
            other => panic!("expected ToolParse, got {other:?}"),
        }
    }

    fn budget_tool() -> ToolSpec {
        tool(
            "conway_spawn",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string"},
                    "budget": {
                        "anyOf": [
                            {
                                "type": "object",
                                "properties": {"max_steps": {"type": "integer"}},
                                "additionalProperties": false
                            },
                            {"type": "null"}
                        ]
                    }
                },
                "required": ["prompt"],
                "additionalProperties": false
            }),
        )
    }

    /// THE EVIDENCE, reproduced directly against `SchemaValidator`: a
    /// stringified `budget` (`"{\"max_steps\": 5}"`, a JSON string
    /// containing an object rather than the object itself) is coerced to
    /// the parsed object, not rejected.
    #[test]
    fn a_stringified_object_argument_is_coerced() {
        let validator = SchemaValidator::compile(&[budget_tool()]).unwrap();
        let name = ToolName::new("conway_spawn");
        let outcome = validator
            .validate(
                &name,
                "call_1",
                serde_json::json!({
                    "prompt": "do the thing",
                    "budget": "{\"max_steps\": 5}"
                }),
            )
            .expect("an unambiguously stringified object must be coerced, not rejected");
        match outcome {
            Validated::Coerced {
                value,
                argument_path,
            } => {
                assert_eq!(
                    value,
                    serde_json::json!({"prompt": "do the thing", "budget": {"max_steps": 5}}),
                    "the coerced document must carry the PARSED object, not the raw string"
                );
                assert_eq!(argument_path, "/budget");
            }
            Validated::AsIs(_) => panic!("expected Coerced, got AsIs"),
        }
    }

    /// A stringified `null` (the OTHER allowed shape) is also coerced.
    #[test]
    fn a_stringified_null_argument_is_coerced() {
        let validator = SchemaValidator::compile(&[budget_tool()]).unwrap();
        let name = ToolName::new("conway_spawn");
        let outcome = validator
            .validate(
                &name,
                "call_1",
                serde_json::json!({"prompt": "do the thing", "budget": "null"}),
            )
            .expect("a stringified null must be coerced");
        assert!(matches!(
            outcome,
            Validated::Coerced { value, .. } if value == serde_json::json!({"prompt": "do the thing", "budget": null})
        ));
    }

    /// The narrow half of the decision: a string that parses as JSON but
    /// does NOT then validate against the schema it was rejected by (here,
    /// `budget: "5"` parses to the JSON number `5`, which is not an object)
    /// is never coerced -- it is reported exactly like any other
    /// malformation.
    #[test]
    fn a_parseable_but_non_validating_string_is_not_coerced() {
        let validator = SchemaValidator::compile(&[budget_tool()]).unwrap();
        let name = ToolName::new("conway_spawn");
        let err = validator
            .validate(
                &name,
                "call_1",
                serde_json::json!({"prompt": "do the thing", "budget": "5"}),
            )
            .unwrap_err();
        match err {
            BackendError::ToolArgumentsInvalid {
                tool,
                call_id,
                argument_path,
                detail,
                ..
            } => {
                assert_eq!(tool, ToolName::new("conway_spawn"));
                assert_eq!(call_id, "call_1");
                assert_eq!(argument_path, "/budget");
                assert!(detail.contains("conway_spawn"), "{detail}");
                assert!(detail.contains("budget"), "{detail}");
            }
            other => panic!("expected ToolArgumentsInvalid, got {other:?}"),
        }
    }

    /// A string that is not JSON at all is likewise left alone.
    #[test]
    fn a_non_json_string_is_not_coerced() {
        let validator = SchemaValidator::compile(&[budget_tool()]).unwrap();
        let name = ToolName::new("conway_spawn");
        assert!(validator
            .validate(
                &name,
                "call_1",
                serde_json::json!({"prompt": "do the thing", "budget": "not json at all {"}),
            )
            .is_err());
    }

    // -----------------------------------------------------------------
    // The shape gate (review round 2, CRITICAL): a stringified SCALAR
    // mechanically satisfies "parses as JSON, then validates" just as
    // readily as a stringified object does -- these three are exactly the
    // widening the review verified empirically against this workspace's
    // own vendored jsonschema, and are what stop it from coming back.
    // -----------------------------------------------------------------

    fn scalar_tool(schema_json: serde_json::Value) -> ToolSpec {
        tool(
            "t",
            serde_json::json!({
                "type": "object",
                "properties": {"field": schema_json},
                "required": ["field"],
                "additionalProperties": false
            }),
        )
    }

    /// `{"field": "5"}` against `{"type":"integer"}`: `"5"` parses to the
    /// JSON number `5`, which DOES validate against `integer` -- and must
    /// still not be coerced. No numeric-string coercion.
    #[test]
    fn a_stringified_integer_is_not_coerced() {
        let validator =
            SchemaValidator::compile(&[scalar_tool(serde_json::json!({"type": "integer"}))])
                .unwrap();
        let name = ToolName::new("t");
        let err = validator
            .validate(&name, "call_1", serde_json::json!({"field": "5"}))
            .unwrap_err();
        assert!(matches!(err, BackendError::ToolArgumentsInvalid { .. }));
    }

    /// `{"field": "true"}` against `{"type":"boolean"}`: `"true"` parses to
    /// the JSON boolean `true`, which DOES validate -- still not coerced.
    #[test]
    fn a_stringified_boolean_is_not_coerced() {
        let validator =
            SchemaValidator::compile(&[scalar_tool(serde_json::json!({"type": "boolean"}))])
                .unwrap();
        let name = ToolName::new("t");
        let err = validator
            .validate(&name, "call_1", serde_json::json!({"field": "true"}))
            .unwrap_err();
        assert!(matches!(err, BackendError::ToolArgumentsInvalid { .. }));
    }

    /// `{"field": "[1,2,3]"}` against `{"type":"array"}`: `"[1,2,3]"` parses
    /// to a JSON array, which DOES validate -- still not coerced (the shape
    /// gate allows only object/null, never array).
    #[test]
    fn a_stringified_array_is_not_coerced() {
        let validator =
            SchemaValidator::compile(&[scalar_tool(serde_json::json!({"type": "array"}))]).unwrap();
        let name = ToolName::new("t");
        let err = validator
            .validate(&name, "call_1", serde_json::json!({"field": "[1,2,3]"}))
            .unwrap_err();
        assert!(matches!(err, BackendError::ToolArgumentsInvalid { .. }));
    }

    // A stringified plain STRING (e.g. `"\"hello\""`, which parses to the
    // JSON string `"hello"`) has no test here: any raw string already
    // satisfies `{"type": "string"}` on its own (a JSON string containing
    // escaped quote characters is still a string), so that case never
    // reaches `validator.validate`'s failure branch at all -- there is
    // nothing for coercion to fire (or fail to fire) against.
}
