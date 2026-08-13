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
//! members individually.** Today it contains exactly one plugin entry --
//! `conway-plugin-skeleton`, itself a skeleton proving nothing beyond the
//! install mechanism (see that crate's own module doc). Dynamic routing,
//! context compaction, memory, skills, and MCP support are each a
//! separate, later board item (`.design/philosophy-debt.md` entry 2's own
//! sequencing note) and each adds its own entry here when it lands --
//! through `ConwayBuilder::with_backend_factory`/`with_router_factory` too,
//! not only `with_plugin`, since nothing about `[plugins].install` itself
//! is tool-specific -- `router_bundle` and `backend_bundle` below are
//! exactly those other two channels.
//!
//! Resolution below matches an id against each candidate's own identity.
//! `Backend` carries an `id()` of its own (`conway_core::ports::backend`),
//! but that is a CONFIGURED INSTANCE's identity, not a KIND's -- the same
//! reason `Router` has none at all (see the paragraph below): a
//! `BackendFactory`'s own `id()` is what `backend_bundle` resolves
//! against, mirroring `router_bundle` one line over. `Router`
//! (`conway_core::ports::routing`) has NO id-bearing method at all --
//! board item 01KZFC2MD1FVNA674YJ9A19T8E answered this, settling that a
//! router's identity lives on a separate `RouterFactory` trait instead
//! (`RouterFactory::id`), never on `Router` itself: router SELECTION
//! (naming a kind) must precede router CONSTRUCTION, which needs backends
//! and a capability picture that do not exist until much later in
//! startup, well after `[plugins].install` is read. `router_bundle`/
//! `backend_bundle` below are this binary's linked `RouterFactory`/
//! `BackendFactory` lists, resolved in the SAME pass as `bundle` by
//! [`ConwayBuilder::install_selected`] (board item
//! 01KZVZ1TDBHS7S604PQB5RZDM3) -- an id may name a plugin, a router
//! factory, or a backend factory, never more than one of the three, and
//! naming more than one router factory is rejected (a build has exactly
//! one router).
//!
//! ## What this module used to do, and does not any more
//!
//! Before board item 01KZVZ1TDBHS7S604PQB5RZDM3, this file resolved
//! `[plugins].install` UNIONED with `[plugins].default_backends` against
//! [`bundle`]/[`router_bundle`]/[`backend_bundle`] itself, in a ~70-line
//! hand-rolled loop -- the exact resolution logic every OTHER embedder had
//! to rebuild from scratch, since it lived only here. That resolution is
//! now [`ConwayBuilder::install_selected`], a facade method taking the same
//! three caller-supplied bundles this module still constructs -- so what
//! remains here is exactly what is genuinely CLI-specific: which plugin,
//! router-factory, and backend-factory crates THIS BINARY links at all.
//! [`install`] below is now three `Vec` constructions and one call.
//!
//! ## What makes the two backend kinds attach with no `[plugins].install` entry
//!
//! Every other candidate in this file is opt-in: absent from the resolved
//! id set (`[plugins].install`), it is simply never attached, and `conway`
//! keeps working with whatever it does have (no extra tool, `MinimalRouter`
//! instead of `DeclarativeRouter`). A `[backends.<id>]` entry with no
//! matching `BackendFactory` has no such fallback -- `ConwayBuilder::build`
//! hard-errors ("no backends configured") when the backend map ends up
//! empty, and even a single unresolvable entry fails the whole build. So
//! the id set `ConwayBuilder::install_selected` resolves against is not
//! `[plugins].install` alone: it is `[plugins].install` UNIONED with
//! `[plugins].default_backends` (`conway::config::schema::PluginsConfig`'s
//! own doc -- default `["anthropic", "openai-compat"]`, owner decision
//! 01KZHRPZ010R37411R3W1XR5TF) -- computed inside `install_selected` itself
//! now, from whatever `ConwayBuilder` it is called on, so "came from
//! `install`" and "came from `default_backends`" are indistinguishable by
//! the time an id is resolved, exactly as before this item. This is what
//! makes `conway_plugin_backends`'s two factories attach on an ordinary
//! `settings.json` with no `[plugins]` section at all: `default_backends`
//! defaults to naming both, unioned in regardless.

use std::sync::Arc;

use conway::plugin::Plugin;
use conway::{BackendFactory, ConwayBuilder, ConwayError, RouterFactory};

/// Every first-party plugin this binary links, in no particular order.
/// `Vec<Arc<dyn Plugin>>` rather than a `HashMap` keyed by id: the bundle
/// is tiny (one entry today), and resolving by a linear scan over each
/// candidate's own `PluginManifest::id` is the same style `conway`'s own
/// `presets::builtin_plugins()` uses for the built-in bundle -- no second
/// registry idiom introduced for a one-plugin list.
fn bundle() -> Vec<Arc<dyn Plugin>> {
    vec![Arc::new(conway_plugin_skeleton::SkeletonPlugin)]
}

/// Every first-party `RouterFactory` this binary links, in no particular
/// order -- the router-side sibling of [`bundle`], resolved against the
/// SAME `[plugins].install` list, in the same pass ([`install`]).
///
/// **First occupant, board item 01KZFC43J1J06BM4CCWKCKHSNV:**
/// `conway-plugin-routing`'s `RoutingRouterFactory` -- the capability-/
/// health-filtering `DeclarativeRouter` engine `conway` itself used to
/// compile in unconditionally, now installed by naming its published
/// `ROUTER_ID` (`"conway.routing"`) in `[plugins].install`, exactly the way
/// [`bundle`]'s skeleton plugin is named. Absent that entry, `build()`
/// falls through to `conway_core::routing::MinimalRouter` (see
/// `docs/routing.md`).
fn router_bundle() -> Vec<Arc<dyn RouterFactory>> {
    vec![Arc::new(conway_plugin_routing::RoutingRouterFactory)]
}

/// Every first-party `BackendFactory` this binary links -- the
/// backend-side sibling of [`bundle`]/[`router_bundle`], resolved against
/// the SAME id list, in the same pass ([`install`]).
///
/// **Both occupants, board item 01KZHF270T3W8GZ7NM6DSNQ4MM:**
/// `conway_plugin_backends`'s `AnthropicBackendFactory`/
/// `OpenAiCompatBackendFactory` -- the two provider-adapter dialects
/// `conway` itself used to compile in unconditionally, now installed by
/// naming their published kind ids (`conway_plugin_backends::
/// ANTHROPIC_KIND`/`OPENAI_COMPAT_KIND` -- `"anthropic"`/`"openai-compat"`,
/// unchanged from before this item) -- ordinarily with NO
/// `[plugins].install` entry at all, since `ConwayBuilder::install_selected`
/// itself unions `[plugins].default_backends` into the resolved id set (see
/// this module's own doc, "What makes the two backend kinds attach...").
/// Absent BOTH ids (an operator who edited `default_backends` down to `[]`
/// or removed a specific one), a `[backends.<id>]` entry naming that kind
/// fails `build()` -- there is no silent fallback, by design: nothing may
/// claim to be reached that isn't, and the whole point of this pair
/// shipping attached by default is that an operator has to take a
/// deliberate action to lose the capability, not merely omit one.
fn backend_bundle() -> Vec<Arc<dyn BackendFactory>> {
    vec![
        Arc::new(conway_plugin_backends::AnthropicBackendFactory),
        Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory),
    ]
}

/// Hands this binary's three linked bundles ([`bundle`], [`router_bundle`],
/// [`backend_bundle`]) to [`ConwayBuilder::install_selected`] -- the
/// facade's own resolution of `[plugins].install` UNIONED with
/// `[plugins].default_backends` against exactly those three (board item
/// 01KZVZ1TDBHS7S604PQB5RZDM3). Every dispatch target (`main.rs`'s
/// `build_conway`) shares this one call, so the TUI, one-shot `-p`,
/// `sessions`, and `routes` all see the same installed set from the same
/// config.
///
/// **This is now three `Vec` constructions and one call** -- the ~70-line
/// hand-rolled resolution this function used to perform (matching each id
/// against a candidate's own identity, the router-factory cardinality
/// check, the unknown-id error, the `with_declined_backend_kinds` call)
/// moved to `install_selected` itself, board item 01KZVZ1TDBHS7S604PQB5RZDM3
/// -- see this module's own doc, "What this module used to do, and does
/// not any more". What is left here is exactly the part that is genuinely
/// CLI-specific: which plugin/router-factory/backend-factory CRATES this
/// binary links at all.
pub fn install(builder: ConwayBuilder) -> Result<ConwayBuilder, ConwayError> {
    builder.install_selected(bundle(), router_bundle(), backend_bundle())
}

/// `install` itself is covered end-to-end in `tests/first_party_plugins.rs`,
/// which drives the real compiled binary: the empty case
/// (`skeleton_tool_is_absent_from_the_announced_set_without_plugins_install`),
/// resolution of a known id
/// (`skeleton_tool_is_present_in_the_announced_set_once_installed`), the
/// resulting tool actually running (`skeleton_tool_is_callable_from_one_shot_
/// once_installed`), a fresh install reaching a model with no
/// `[plugins].install` entry at all (`default_backends_attach_with_no_
/// plugins_install_entry_and_complete_a_one_shot_prompt`), and the
/// unknown-id hard error (`unknown_plugins_install_id_is_a_hard_error`,
/// which also pins that the error message lists the linked plugin ids, the
/// linked router factory ids, and the linked backend factory ids). Each
/// asserts on an observable outcome — the announced tool set on the wire,
/// the invoked tool's preview text, the process exit code and stderr —
/// rather than on an intermediate signal. `ConwayBuilder::install_selected`
/// itself is covered directly, against caller-supplied fakes, in
/// `crates/conway/tests/install_selected.rs`; this file's own coverage is
/// therefore the real-binary liveness proof that `install` above wires this
/// binary's three linked bundles into that method correctly, not a
/// restatement of `install_selected`'s own resolution-logic unit coverage.
///
/// The `with_declined_backend_kinds` call `install_selected` makes
/// internally (board item 01KZHF2W8Y1KBM7PJH7R4QQJA0) is covered the same
/// way, separately, in `tests/decline_backend_kind.rs`: declining a shipped
/// dialect via `[plugins].default_backends` while a `[backends.<id>]`
/// entry still names it fails the real compiled binary with a message that
/// reads as **declined**, and a kind this binary has never linked at all
/// still fails with the pre-existing **unknown-kind** message — with a
/// third test pinning that the two stderr strings are genuinely different
/// text.
///
/// This module deliberately does NOT restate that coverage as unit tests.
/// Constructing a `ConwayBuilder` here would need a stub config solely to
/// re-check what the integration suite already proves against the real
/// binary, and two earlier attempts at exactly that asserted only on
/// [`bundle`] while their names promised they exercised `install` — checks
/// that could not fail, which is the defect class CONTRIBUTING's testing
/// discipline exists to catch. The properties below are local to this
/// module and are stated as narrowly as they are checked.
#[cfg(test)]
mod tests {
    use super::*;

    /// The bundle is what `install_selected` resolves against, so an empty
    /// or mis-keyed bundle would turn every `[plugins].install` entry into
    /// an unknown-id error. This checks the wiring only; it makes no claim
    /// about `install_selected`'s own behaviour.
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

    /// Same wiring-only check, for [`backend_bundle`]: both published kind
    /// ids must be present, otherwise `[plugins].default_backends`'s own
    /// default value resolves to an unknown-id error and a fresh install
    /// cannot reach a model at all.
    #[test]
    fn backend_bundle_carries_both_published_kind_ids() {
        let bundle = backend_bundle();
        let ids: Vec<&str> = bundle.iter().map(|f| f.id()).collect();
        assert!(
            ids.contains(&conway_plugin_backends::ANTHROPIC_KIND),
            "missing '{}' in the linked backend bundle: {ids:?}",
            conway_plugin_backends::ANTHROPIC_KIND
        );
        assert!(
            ids.contains(&conway_plugin_backends::OPENAI_COMPAT_KIND),
            "missing '{}' in the linked backend bundle: {ids:?}",
            conway_plugin_backends::OPENAI_COMPAT_KIND
        );
    }

    /// Same wiring-only check, for [`router_bundle`]: the routing plugin's
    /// published `ROUTER_ID` must be present, otherwise
    /// `[plugins].install = ["conway.routing"]` resolves to an unknown-id
    /// error and an operator following `docs/routing.md` cannot install it.
    #[test]
    fn router_bundle_carries_the_routing_plugins_published_id() {
        let bundle = router_bundle();
        let ids: Vec<&str> = bundle.iter().map(|f| f.id()).collect();
        assert!(
            ids.contains(&conway_plugin_routing::ROUTER_ID),
            "missing '{}' in the linked router-factory bundle: {ids:?}",
            conway_plugin_routing::ROUTER_ID
        );
    }
}
