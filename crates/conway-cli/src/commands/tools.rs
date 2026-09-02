//! `conway tools list`: a pure formatter over `Conway::tool_specs` /
//! `Conway::tool_plugin_count`. Scripts drive `conway -p` with
//! `--allowed-tools`/`--deny-tools` (`cli.rs`'s own doc on those flags), but
//! until this subcommand existed the only place naming the exact registered
//! tool names was the interactive TUI's own panel -- this makes the
//! allow-list vocabulary self-describing from a headless shell too.
//!
//! **Registration, not confinement, and not the permission gate.** This
//! reads the same `ToolSpec` set the router/backend would be offered for a
//! turn (`PluginRegistry::specs(None)`, unfiltered by any one agent's own
//! `ToolSelector`) -- it does not simulate `--allowed-tools`/`--deny-tools`
//! filtering, and it does not simulate `--root` confinement (module doc on
//! `list`'s `root` parameter, below, has the full disclosure for why the
//! list itself never shrinks under `--root`).

use std::path::Path;

use clap::{Args, Subcommand};
use conway::plugin::{PathArgs, PermissionClass, ToolCategory};
use conway::Conway;

use crate::exit::ExitCode;

#[derive(Args, Debug)]
pub struct ToolsArgs {
    #[command(subcommand)]
    pub action: ToolsAction,
}

#[derive(Subcommand, Debug)]
pub enum ToolsAction {
    /// Print every tool this process registered: name, category,
    /// permission class, and the first sentence of its description, one
    /// per line, sorted by name.
    List {
        /// Print the full `ToolSpec` list (name, description, JSON Schema,
        /// category, permission class) as one JSON array instead of the
        /// human-readable table.
        #[arg(long)]
        json: bool,
    },
}

/// `root` is `cli.root.as_deref()` -- `main.rs`'s own already-parsed
/// `--root` flag, forwarded rather than re-read. **Announcement and
/// confinement are separate questions** (this module's own doc): a `--root`
/// confines which PATHS a tool call may resolve to, not which TOOL NAMES
/// exist, so the printed list is identical with or without `--root` --
/// only an extra footnote line is added, naming which of the listed tools
/// [`Conway::tool_path_args`] declares as path-confinable
/// (`PathArgs::Named`) at all, so the operator can see what the root
/// actually covers without hunting through `docs/tools.md`'s table by hand.
pub async fn run(
    args: &ToolsArgs,
    conway: &Conway,
    root: Option<&Path>,
) -> conway::Result<ExitCode> {
    match &args.action {
        ToolsAction::List { json } => list(conway, *json, root),
    }
}

fn list(conway: &Conway, json: bool, root: Option<&Path>) -> conway::Result<ExitCode> {
    // `Conway::tool_specs` -> `Runtime::tool_specs` -> `PluginRegistry::
    // specs(None)` already returns its `Vec` sorted lexicographically by
    // `ToolName` (that method's own doc, kept for provenance-hash
    // stability) -- this renderer relies on that ordering rather than
    // re-sorting a second time.
    let specs = conway.tool_specs();

    if json {
        println!(
            "{}",
            serde_json::to_string(&specs).expect("tool specs always serialize")
        );
        return Ok(ExitCode::Completed);
    }

    let name_w = specs
        .iter()
        .map(|s| s.name.to_string().chars().count())
        .max()
        .unwrap_or(0);
    let cat_w = specs
        .iter()
        .map(|s| category_str(s.category).len())
        .max()
        .unwrap_or(0);
    let perm_w = specs
        .iter()
        .map(|s| permission_str(s.permission).len())
        .max()
        .unwrap_or(0);
    for spec in &specs {
        println!(
            "{name:<name_w$}  {cat:<cat_w$}  {perm:<perm_w$}  {desc}",
            name = spec.name,
            cat = category_str(spec.category),
            perm = permission_str(spec.permission),
            desc = first_sentence(&spec.description),
            name_w = name_w,
            cat_w = cat_w,
            perm_w = perm_w,
        );
    }
    println!(
        "{} tools registered from {} plugins",
        specs.len(),
        conway.tool_plugin_count()
    );

    if let Some(root) = root {
        let confinable: Vec<&str> = specs
            .iter()
            .filter(|s| matches!(conway.tool_path_args(&s.name), Some(PathArgs::Named(_))))
            .map(|s| s.name.as_str())
            .collect();
        let names = if confinable.is_empty() {
            "(none)".to_string()
        } else {
            confinable.join(", ")
        };
        println!("--root {} confines: {names}", root.display());
    }

    Ok(ExitCode::Completed)
}

/// `ToolCategory::default()`-free: every declared variant renders its own
/// name, and the `#[non_exhaustive]` wildcard exists only for this crate's
/// forward compatibility with a variant added upstream, not expected to be
/// exercised by this module's own unit tests -- matching
/// `commands::routes`'s identical `render_reason`/`render_token_fidelity`
/// precedent for a `#[non_exhaustive]` core enum.
fn category_str(category: ToolCategory) -> &'static str {
    match category {
        ToolCategory::Read => "read",
        ToolCategory::Edit => "edit",
        ToolCategory::Delete => "delete",
        ToolCategory::Move => "move",
        ToolCategory::Search => "search",
        ToolCategory::Execute => "execute",
        ToolCategory::Think => "think",
        ToolCategory::Fetch => "fetch",
        ToolCategory::Delegate => "delegate",
        _ => "unknown",
    }
}

/// Same shape as [`category_str`], for `PermissionClass`.
fn permission_str(permission: PermissionClass) -> &'static str {
    match permission {
        PermissionClass::Safe => "safe",
        PermissionClass::RequiresApproval => "requires_approval",
        PermissionClass::Dangerous => "dangerous",
        _ => "unknown",
    }
}

/// The first sentence of `description`, split at the first `". "` --
/// present rather than absent for a genuinely multi-sentence description
/// (e.g. `conway.shell`'s bash tool: "Execute a shell command with bash
/// -c. If a confinement root is..."). A description with no internal
/// `". "` (most built-ins: a single clause, often with no trailing period
/// at all -- e.g. `fs.glob`'s "Find files matching a glob pattern,
/// gitignore-aware") is returned whole, unmodified.
fn first_sentence(description: &str) -> &str {
    match description.find(". ") {
        Some(idx) => &description[..=idx],
        None => description,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_category_and_permission_renders_a_specific_string() {
        assert_eq!(category_str(ToolCategory::Read), "read");
        assert_eq!(category_str(ToolCategory::Edit), "edit");
        assert_eq!(category_str(ToolCategory::Delete), "delete");
        assert_eq!(category_str(ToolCategory::Move), "move");
        assert_eq!(category_str(ToolCategory::Search), "search");
        assert_eq!(category_str(ToolCategory::Execute), "execute");
        assert_eq!(category_str(ToolCategory::Think), "think");
        assert_eq!(category_str(ToolCategory::Fetch), "fetch");
        assert_eq!(category_str(ToolCategory::Delegate), "delegate");

        assert_eq!(permission_str(PermissionClass::Safe), "safe");
        assert_eq!(
            permission_str(PermissionClass::RequiresApproval),
            "requires_approval"
        );
        assert_eq!(permission_str(PermissionClass::Dangerous), "dangerous");
    }

    #[test]
    fn first_sentence_splits_a_genuinely_multi_sentence_description() {
        let desc = "Execute a shell command with bash -c. If a confinement \
                     root is set, the working directory is checked against it.";
        assert_eq!(
            first_sentence(desc),
            "Execute a shell command with bash -c."
        );
    }

    #[test]
    fn first_sentence_returns_a_single_clause_description_whole() {
        let desc = "Find files matching a glob pattern, gitignore-aware";
        assert_eq!(first_sentence(desc), desc);
    }
}
