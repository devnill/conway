//! `PluginRegistry`: the runtime's compiled view over the injected
//! `Arc<dyn Plugin>` set (WI-079, architecture §4.2).
//!
//! Every tool's JSON Schema is compiled exactly once, at construction time
//! ([`PluginRegistry::from_plugins`]), so [`super::runner::ToolRunner`] never
//! re-parses a schema per call. A schema that fails to compile, or two
//! plugins registering the same tool name, are both construction-time
//! errors — a malformed plugin set is a registration bug, not something a
//! running agent should discover mid-batch.

use std::collections::HashMap;
use std::sync::Arc;

use conway_core::agent::ToolSelector;
use conway_core::content::ToolSpec;
use conway_core::error::{RuntimeError, ToolError};
use conway_core::ids::ToolName;
use conway_core::ports::{Plugin, Tool};

/// One registered tool: its owning plugin's id, the `Tool` implementation,
/// and its compiled schema validator.
struct RegisteredTool {
    plugin_id: String,
    tool: Arc<dyn Tool>,
    /// Captured once at registration: `Tool::spec()` rebuilds the whole
    /// spec (including its schema) on every call, which is wasted work on
    /// the per-dispatch hot path (cycle-1 review M1).
    spec: ToolSpec,
    validator: jsonschema::Validator,
}

/// The compiled, queryable view over every tool contributed by the
/// injected plugin set. Built once and shared (behind an `Arc`) by
/// [`super::runner::ToolRunner`].
pub struct PluginRegistry {
    tools: HashMap<ToolName, RegisteredTool>,
}

/// A tool resolved from the registry: everything [`super::runner::ToolRunner`]
/// needs to validate, authorize, and describe one call, without re-hashing
/// the registry per field access. Crate-internal — external callers only
/// ever see [`ToolSpec`] via [`PluginRegistry::specs`].
pub(crate) struct ResolvedTool<'a> {
    pub(crate) tool: Arc<dyn Tool>,
    pub(crate) spec: &'a ToolSpec,
    validator: &'a jsonschema::Validator,
}

impl ResolvedTool<'_> {
    /// Validates `arguments` against this tool's compiled schema. On
    /// failure, the message names the failing instance's JSON pointer
    /// (`ValidationError::instance_path`) so the caller sees exactly which
    /// part of the call was invalid, not just that validation failed.
    pub(crate) fn validate(&self, arguments: &serde_json::Value) -> Result<(), String> {
        self.validator.validate(arguments).map_err(|err| {
            format!(
                "tool `{}`: arguments failed schema validation at `{}`: {err}",
                self.spec.name,
                err.instance_path()
            )
        })
    }
}

impl PluginRegistry {
    /// Compiles every tool from every plugin. Two tools sharing a name
    /// across any pair of plugins is a construction-time error naming both
    /// plugin ids and the colliding tool name; so is a schema that fails to
    /// serialize or fails to compile as JSON Schema.
    ///
    /// `RuntimeError` has no dedicated "registration" variant (its
    /// non-exhaustive set is fixed by `conway-core`, out of this crate's
    /// scope to extend), so these construction-time failures are carried as
    /// `RuntimeError::Tool(ToolError::Internal { detail })` — the detail
    /// string is what actually names the plugins/tool per the criterion,
    /// not the variant tag.
    pub fn from_plugins(plugins: Vec<Arc<dyn Plugin>>) -> Result<Self, RuntimeError> {
        let mut tools: HashMap<ToolName, RegisteredTool> = HashMap::new();
        for plugin in plugins {
            let plugin_id = plugin.manifest().id;
            for tool in plugin.tools() {
                let spec = tool.spec();
                if let Some(existing) = tools.get(&spec.name) {
                    return Err(RuntimeError::Tool(ToolError::Internal {
                        detail: format!(
                            "duplicate tool `{}`: registered by plugin `{}` and plugin `{}`",
                            spec.name, existing.plugin_id, plugin_id
                        ),
                    }));
                }
                let schema_value = serde_json::to_value(&spec.schema).map_err(|err| {
                    RuntimeError::Tool(ToolError::Internal {
                        detail: format!(
                            "tool `{}`: schema is not serializable to JSON: {err}",
                            spec.name
                        ),
                    })
                })?;
                let validator = jsonschema::validator_for(&schema_value).map_err(|err| {
                    RuntimeError::Tool(ToolError::Internal {
                        detail: format!("tool `{}`: schema failed to compile: {err}", spec.name),
                    })
                })?;
                tools.insert(
                    spec.name.clone(),
                    RegisteredTool {
                        plugin_id: plugin_id.clone(),
                        tool,
                        spec,
                        validator,
                    },
                );
            }
        }
        Ok(Self { tools })
    }

    /// Every registered tool's spec, in lexicographic order by name, so the
    /// `ToolRegistry` provenance hash (`ContextBuilder`, WI-077) is stable
    /// across runs. `selector: None` behaves as `ToolSelector::All`.
    ///
    /// **This is registration-time filtering, not announcement or execution
    /// filtering.** `ToolSelector` (`AgentSpec::tools`) fixes which tools
    /// this AGENT may ever see, for its whole run; two further, distinct
    /// filters apply downstream, per-turn, to this method's already-narrowed
    /// result:
    /// - **Announcement** (WI-126): `conway_core::ports::ContextHook::
    ///   before_request` may further narrow the `tools` this method returns
    ///   before it reaches the router/backend for a given turn -- hiding a
    ///   tool from the model entirely, so it can never propose calling it.
    ///   See `AgentLoop::run_inner`, the `announced_tools` local.
    /// - **Execution**: `conway_core::ports::PermissionGate` (via
    ///   `PermissionBroker`/`ToolRunner`) decides whether a call the model
    ///   actually proposes is allowed to run, regardless of what was
    ///   announced. A tool this method returns, and a hook did not filter
    ///   out, can still be denied at call time -- announcement and
    ///   execution are independent gates, and neither implies the other.
    pub fn specs(&self, selector: Option<&ToolSelector>) -> Vec<ToolSpec> {
        let mut names: Vec<&ToolName> = self
            .tools
            .keys()
            .filter(|name| selector.is_none_or(|s| s.selects(name)))
            .collect();
        names.sort();
        names
            .into_iter()
            .map(|name| self.tools[name].spec.clone())
            .collect()
    }

    /// Resolves one tool by name, or `None` if no plugin registered it.
    pub(crate) fn resolve(&self, name: &ToolName) -> Option<ResolvedTool<'_>> {
        self.tools.get(name).map(|registered| ResolvedTool {
            tool: registered.tool.clone(),
            spec: &registered.spec,
            validator: &registered.validator,
        })
    }
}
