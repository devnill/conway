//! Whether a role's configured chain is entirely local, per
//! `BackendEntry::local`.
//!
//! **What this module is not:** it does not enforce anything. It answers
//! one question — "if every candidate in this role's chain were tried,
//! would every one of them be a backend declared local?" — and nothing
//! reads the answer automatically. Board item `01M0WX4MB7JETFBRZE3AEQNSV3`'s
//! scope fence is explicit: no routing behaviour changes here, and a chain
//! that falls through from a local candidate to a non-local one still does
//! so; a consumer (e.g. a permission guard, per
//! `docs/vision/DESIGN-permission-modes.md` §4/§5) is the thing that would
//! act on [`role_is_local`]'s answer, by refusing to run rather than by the
//! router silently reordering or dropping candidates.
//!
//! `local` itself is a **declared** property, not one this module infers
//! from `base_url` — see `BackendEntry::local`'s own doc comment for the
//! exact predicate and the case (an SSH tunnel presenting a remote server as
//! `localhost`) that defeats every URL-shaped heuristic this module
//! deliberately does not attempt.

use conway_core::ids::{ModelRef, RoleAlias};

use crate::config::schema::ConwayConfig;
use crate::error::{FacadeError, Result};

/// True iff `role`'s configured chain is non-empty and every candidate in
/// it names a backend declared `local == true`.
///
/// Errors — deliberately mirroring `config::merge::validate`'s own chain
/// checks (step 2), so a caller who has already run `config::load`/
/// `config::validate` on `config` will never observe any of these in
/// practice — cover exactly the ways a chain entry can fail to resolve to a
/// backend at all:
///
/// - `role` is not configured under `[roles]`.
/// - a chain entry is not a valid `"backend/model"` reference.
/// - a chain entry names a backend absent from `[backends]`.
///
/// An **empty** chain is also an error rather than a vacuous `Ok(true)`:
/// "every candidate is local" holding because there are zero candidates
/// would be true in exactly the sense that matters least to a caller asking
/// this question, which is whether inference through this role can
/// actually stay on the machine — a role nothing can route through answers
/// that question with silence, not `true`.
pub fn role_is_local(config: &ConwayConfig, role: &RoleAlias) -> Result<bool> {
    let entry = config
        .roles
        .get(role.as_str())
        .ok_or_else(|| FacadeError::Config {
            path: None,
            message: format!("role '{role}' is not defined in [roles]"),
        })?;

    if entry.chain.is_empty() {
        return Err(FacadeError::Config {
            path: None,
            message: format!(
                "role '{role}' has an empty chain; locality is undefined for a role with no \
                 candidates to check"
            ),
        });
    }

    for raw in &entry.chain {
        let model_ref: ModelRef = raw.parse().map_err(|_| FacadeError::Config {
            path: None,
            message: format!(
                "role '{role}': chain entry '{raw}' is not a valid 'backend/model' reference"
            ),
        })?;
        let backend = config
            .backends
            .get(model_ref.backend.as_str())
            .ok_or_else(|| FacadeError::Config {
                path: None,
                message: format!(
                    "role '{role}': chain entry '{raw}' names unknown backend '{}'",
                    model_ref.backend
                ),
            })?;
        if !backend.local {
            return Ok(false);
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two-backend, mixed-chain case the item's own instructions call
    /// out by name: one local candidate, one not. `role_is_local` must
    /// answer `false` -- ANY non-local candidate in the chain disqualifies
    /// the whole role, not just the ones actually reached at routing time
    /// (this module never resolves health/capability -- see the module
    /// doc for why that is deliberate).
    #[test]
    fn mixed_chain_is_not_local() {
        let json = r#"
        {
          "default_role": "coder",
          "backends": {
            "ollama": {
              "kind": "openai-compat",
              "dialect": "ollama",
              "base_url": "http://localhost:11434/v1",
              "local": true
            },
            "anthropic": {
              "kind": "anthropic",
              "api_key_env": "ANTHROPIC_API_KEY"
            }
          },
          "roles": {
            "coder": { "chain": ["ollama/qwen3:4b", "anthropic/claude-sonnet-4-6"] }
          }
        }
        "#;
        let cfg: ConwayConfig = serde_json::from_str(json).expect("must parse");
        let role = RoleAlias::new("coder");
        assert!(!role_is_local(&cfg, &role).expect("must resolve"));
    }

    /// Every candidate declared local -> the role is local.
    #[test]
    fn all_local_chain_is_local() {
        let json = r#"
        {
          "default_role": "guard",
          "backends": {
            "ollama": {
              "kind": "openai-compat",
              "dialect": "ollama",
              "base_url": "http://localhost:11434/v1",
              "local": true
            },
            "llama-cpp": {
              "kind": "openai-compat",
              "dialect": "llama-cpp",
              "base_url": "http://localhost:8000/v1",
              "local": true
            }
          },
          "roles": {
            "guard": { "chain": ["ollama/qwen3:4b", "llama-cpp/phi-4-mini"] }
          }
        }
        "#;
        let cfg: ConwayConfig = serde_json::from_str(json).expect("must parse");
        let role = RoleAlias::new("guard");
        assert!(role_is_local(&cfg, &role).expect("must resolve"));
    }

    /// A backend with no `local` key at all defaults to NOT local (safe
    /// default -- see `BackendEntry::local`'s own doc) -- so a chain naming
    /// it is not local either, even though its `base_url` happens to say
    /// `localhost`. This is the module doc's own point made concrete: this
    /// module never reads `base_url`, only the declared field.
    #[test]
    fn undeclared_local_defaults_to_not_local_even_with_localhost_base_url() {
        let json = r#"
        {
          "default_role": "coder",
          "backends": {
            "maybe-local": {
              "kind": "openai-compat",
              "dialect": "ollama",
              "base_url": "http://localhost:11434/v1"
            }
          },
          "roles": {
            "coder": { "chain": ["maybe-local/qwen3:4b"] }
          }
        }
        "#;
        let cfg: ConwayConfig = serde_json::from_str(json).expect("must parse");
        let role = RoleAlias::new("coder");
        assert!(!role_is_local(&cfg, &role).expect("must resolve"));
    }

    /// A `local: true` key on a backend entry now lands in the typed field,
    /// not in the `extra` catch-all -- the exact acceptance criterion this
    /// item exists to satisfy (previously: accepted, parsed into `extra`,
    /// and meant nothing).
    #[test]
    fn local_key_is_typed_not_captured_into_extra() {
        let json = r#"
        {
          "kind": "openai-compat",
          "dialect": "ollama",
          "base_url": "http://localhost:11434/v1",
          "local": true
        }
        "#;
        let entry: crate::config::schema::BackendEntry =
            serde_json::from_str(json).expect("must parse");
        assert!(entry.local, "local: true must set the typed field");
        assert!(
            entry.extra.get("local").is_none(),
            "local must not also land in the extra catch-all"
        );
    }

    #[test]
    fn unknown_role_is_an_error() {
        let json = r#"
        {
          "default_role": "coder",
          "roles": { "coder": { "chain": [] } }
        }
        "#;
        let cfg: ConwayConfig = serde_json::from_str(json).expect("must parse");
        let role = RoleAlias::new("nonexistent");
        let err = role_is_local(&cfg, &role).unwrap_err().to_string();
        assert!(err.contains("nonexistent") && err.contains("is not defined"));
    }

    #[test]
    fn empty_chain_is_an_error_not_vacuous_true() {
        let json = r#"
        {
          "default_role": "coder",
          "roles": { "coder": { "chain": [] } }
        }
        "#;
        let cfg: ConwayConfig = serde_json::from_str(json).expect("must parse");
        let role = RoleAlias::new("coder");
        let err = role_is_local(&cfg, &role).unwrap_err().to_string();
        assert!(err.contains("coder") && err.contains("empty chain"));
    }

    #[test]
    fn chain_naming_unknown_backend_is_an_error() {
        let json = r#"
        {
          "default_role": "coder",
          "roles": { "coder": { "chain": ["ghost/some-model"] } }
        }
        "#;
        let cfg: ConwayConfig = serde_json::from_str(json).expect("must parse");
        let role = RoleAlias::new("coder");
        let err = role_is_local(&cfg, &role).unwrap_err().to_string();
        assert!(err.contains("ghost") && err.contains("unknown backend"));
    }
}
