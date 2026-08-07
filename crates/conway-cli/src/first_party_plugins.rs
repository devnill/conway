//! The first-party plugin tier's install mechanism for the CLI binary
//! (board item 01KZDC3JQ7W4DY1MG6MBCVB2DV): every first-party plugin crate
//! this BINARY happens to link, resolved against `[plugins].install`
//! (`conway::config::schema::PluginsConfig`) before `ConwayBuilder::build`.
//!
//! `conway` (the facade) does not, and must never, depend on any of these
//! crates -- see that field's own doc for why. This module is the one
//! place a first-party plugin crate IS linked into a shipped binary: behind
//! this file, never inside the facade itself. A library embedder wanting
//! one of these plugins depends on its crate directly and calls
//! `ConwayBuilder::with_plugin`, exactly as this module does internally --
//! `conway-plugin-skeleton`'s own `tests/skeleton_end_to_end.rs` is that
//! embedder-shaped usage, written against the identical crate this module
//! links.
//!
//! **This bundle is a worked example, not a commitment to any of its
//! members individually.** Today it contains exactly one entry --
//! `conway-plugin-skeleton`, itself a skeleton proving nothing beyond the
//! install mechanism (see that crate's own module doc). Dynamic routing,
//! context compaction, memory, skills, and MCP support are each a
//! separate, later board item (`.design/philosophy-debt.md` entry 2's own
//! sequencing note) and each adds its own entry here when it lands --
//! plausibly through `ConwayBuilder::with_backend`/`with_router` too, not
//! only `with_plugin`, since nothing about `[plugins].install` itself is
//! tool-specific.
//!
//! Those two are not equally cheap, and an earlier draft of this comment
//! said they were. Resolution below matches an id against each candidate's
//! own `PluginManifest::id`. `Backend` carries an `id()` of its own
//! (`conway_core::ports::backend`), so a backend arm is close to
//! mechanical. `Router` (`conway_core::ports::routing`) has NO id-bearing
//! method at all, so a `[plugins].install` entry has nothing to match a
//! router candidate against: board item 01KZDC5BJWSWZZJQ7HHS11S97H must
//! give one an identity before it can be installed this way. That is more
//! than a second branch, and it is worth knowing before that item is
//! scoped.

use std::sync::Arc;

use conway::plugin::Plugin;
use conway::{ConwayBuilder, ConwayError};

/// Every first-party plugin this binary links, in no particular order.
/// `Vec<Arc<dyn Plugin>>` rather than a `HashMap` keyed by id: the bundle
/// is tiny (one entry today), and resolving by a linear scan over each
/// candidate's own `PluginManifest::id` is the same style `conway`'s own
/// `presets::builtin_plugins()` uses for the built-in bundle -- no second
/// registry idiom introduced for a one-plugin list.
fn bundle() -> Vec<Arc<dyn Plugin>> {
    vec![Arc::new(conway_plugin_skeleton::SkeletonPlugin)]
}

/// Applies `wanted` (`ConwayBuilder::config().plugins.install`, read by the
/// caller before this is called) against [`bundle`]: calls
/// `ConwayBuilder::with_plugin` for every recognized id, in the order
/// `wanted` names them, and returns a descriptive error for the first one
/// this binary does not recognize.
///
/// GP-14: an id in `[plugins].install` that silently did nothing would be
/// exactly the rung-1 lie CONTRIBUTING's declaration rule exists to
/// prevent, so an unknown name is a hard error here -- mirroring
/// `config::merge::validate`'s own closed-set check for
/// `tools.builtin_plugins` (that check lives in the facade because the
/// facade owns that candidate set; this one lives here because only this
/// binary knows this one -- see `PluginsConfig`'s own doc for why the
/// facade cannot perform it itself).
pub fn install(
    mut builder: ConwayBuilder,
    wanted: &[String],
) -> Result<ConwayBuilder, ConwayError> {
    if wanted.is_empty() {
        return Ok(builder);
    }
    let bundle = bundle();
    for id in wanted {
        match bundle.iter().find(|p| &p.manifest().id == id) {
            Some(plugin) => {
                builder = builder.with_plugin(plugin.clone());
            }
            None => {
                let known: Vec<String> = bundle.iter().map(|p| p.manifest().id).collect();
                return Err(ConwayError::Config {
                    path: None,
                    message: format!(
                        "plugins.install names unknown first-party plugin '{id}'; linked \
                         first-party plugins: [{}]. A third-party plugin is installed with \
                         ConwayBuilder::with_plugin in library code and is not listed here.",
                        known.join(", ")
                    ),
                });
            }
        }
    }
    Ok(builder)
}

/// `install` itself is covered end-to-end in `tests/first_party_plugins.rs`,
/// which drives the real compiled binary: the empty case
/// (`skeleton_tool_is_absent_from_the_announced_set_without_plugins_install`),
/// resolution of a known id
/// (`skeleton_tool_is_present_in_the_announced_set_once_installed`), the
/// resulting tool actually running (`skeleton_tool_is_callable_from_one_shot_
/// once_installed`), and the unknown-id hard error
/// (`unknown_plugins_install_id_is_a_hard_error`). Each asserts on an
/// observable outcome — the announced tool set on the wire, the invoked
/// tool's preview text, the process exit code and stderr — rather than on an
/// intermediate signal.
///
/// This module deliberately does NOT restate that coverage as unit tests.
/// Constructing a `ConwayBuilder` here would need a stub config solely to
/// re-check what the integration suite already proves against the real
/// binary, and two earlier attempts at exactly that asserted only on
/// [`bundle`] while their names promised they exercised `install` — checks
/// that could not fail, which is the defect class CONTRIBUTING's testing
/// discipline exists to catch. The one property below is local to this
/// module and is stated as narrowly as it is checked.
#[cfg(test)]
mod tests {
    use super::*;

    /// The bundle is what `install` resolves against, so an empty or
    /// mis-keyed bundle would turn every `[plugins].install` entry into an
    /// unknown-id error. This checks the wiring only; it makes no claim
    /// about `install`'s own behaviour.
    #[test]
    fn bundle_carries_the_skeleton_plugin_under_its_published_id() {
        let found = bundle()
            .iter()
            .any(|p| p.manifest().id == conway_plugin_skeleton::PLUGIN_ID);
        assert!(
            found,
            "the linked bundle must contain the skeleton plugin under its published id, \
             otherwise `[plugins].install = [\"{}\"]` resolves to an unknown-id error",
            conway_plugin_skeleton::PLUGIN_ID
        );
    }
}
