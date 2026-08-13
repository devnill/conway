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
//! is tool-specific -- [`router_bundle`] and [`backend_bundle`] below are
//! exactly those other two channels.
//!
//! Resolution below matches an id against each candidate's own identity.
//! `Backend` carries an `id()` of its own (`conway_core::ports::backend`),
//! but that is a CONFIGURED INSTANCE's identity, not a KIND's -- the same
//! reason `Router` has none at all (see the paragraph below): a
//! `BackendFactory`'s own `id()` is what [`backend_bundle`] resolves
//! against, mirroring [`router_bundle`] one line over. `Router`
//! (`conway_core::ports::routing`) has NO id-bearing method at all --
//! board item 01KZFC2MD1FVNA674YJ9A19T8E answered this, settling that a
//! router's identity lives on a separate `RouterFactory` trait instead
//! (`RouterFactory::id`), never on `Router` itself: router SELECTION
//! (naming a kind) must precede router CONSTRUCTION, which needs backends
//! and a capability picture that do not exist until much later in
//! startup, well after `[plugins].install` is read. [`router_bundle`]/
//! [`backend_bundle`] below are this binary's linked `RouterFactory`/
//! `BackendFactory` lists, resolved in the SAME pass ([`install`]) as
//! [`bundle`] -- an id may name a plugin, a router factory, or a backend
//! factory, never more than one of the three, and naming more than one
//! router factory is rejected (a build has exactly one router).
//!
//! ## What makes the two backend kinds attach with no `[plugins].install` entry
//!
//! Every other candidate in this file is opt-in: absent from `wanted`
//! (`[plugins].install`), it is simply never attached, and `conway` keeps
//! working with whatever it does have (no extra tool, `MinimalRouter`
//! instead of `DeclarativeRouter`). A `[backends.<id>]` entry with no
//! matching `BackendFactory` has no such fallback -- `ConwayBuilder::build`
//! hard-errors ("no backends configured") when the backend map ends up
//! empty, and even a single unresolvable entry fails the whole build. So
//! [`build_conway`] (`main.rs`)'s `wanted` list is not `[plugins].install`
//! alone: it is `[plugins].install` UNIONED with `[plugins].
//! default_backends` (`conway::config::schema::PluginsConfig`'s own doc --
//! default `["anthropic", "openai-compat"]`, owner decision
//! 01KZHRPZ010R37411R3W1XR5TF) BEFORE `install` (this function) ever sees
//! it -- by the time an id reaches the loop below, "came from `install`"
//! and "came from `default_backends`" are indistinguishable, and both
//! resolve through the identical three-way match. This is what makes
//! `conway_plugin_backends`'s two factories attach on an ordinary
//! `settings.json` with no `[plugins]` section at all: `default_backends`
//! defaults to naming both, unioned in regardless.

use std::collections::HashSet;
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
/// `[plugins].install` entry at all, since `main.rs`'s `build_conway`
/// unions `[plugins].default_backends` into `wanted` before calling
/// [`install`] (see this module's own doc, "What makes the two backend
/// kinds attach..."). Absent BOTH ids (an operator who edited
/// `default_backends` down to `[]` or removed a specific one), a
/// `[backends.<id>]` entry naming that kind fails `build()` -- there is no
/// silent fallback, by design (GP-14): the whole point of this pair
/// shipping attached by default is that an operator has to take a
/// deliberate action to lose the capability, not merely omit one.
fn backend_bundle() -> Vec<Arc<dyn BackendFactory>> {
    vec![
        Arc::new(conway_plugin_backends::AnthropicBackendFactory),
        Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory),
    ]
}

/// Applies `wanted` (`main.rs`'s `build_conway`: `[plugins].install`
/// UNIONED with `[plugins].default_backends`, deduplicated, in that order --
/// see this module's own doc) against [`bundle`], [`router_bundle`], and
/// [`backend_bundle`] together, in one pass: for each id, in the order
/// `wanted` names them, calls `ConwayBuilder::with_plugin` for a recognized
/// plugin id, `ConwayBuilder::with_router_factory` for a recognized
/// router-factory id, or `ConwayBuilder::with_backend_factory` for a
/// recognized backend-factory id, and returns a descriptive error for the
/// first id this binary recognizes as none of the three.
///
/// GP-14: an id in `wanted` that silently did nothing would be exactly the
/// rung-1 lie CONTRIBUTING's declaration rule exists to prevent, so an
/// unknown name is a hard error here -- mirroring
/// `config::merge::validate`'s own closed-set check for
/// `tools.builtin_plugins` (that check lives in the facade because the
/// facade owns that candidate set; this one lives here because only this
/// binary knows this one -- see `PluginsConfig`'s own doc for why the
/// facade cannot perform it itself). An id resolving to MORE THAN ONE
/// router factory is also a hard error: a build has exactly one router, so
/// naming two would be a request this binary cannot honor either way, and
/// picking one silently would be exactly the kind of unstated choice GP-14
/// forbids. A backend factory carries no such cardinality limit (a build
/// has a SET of backends -- `BackendFactory::id`'s own doc).
///
/// **Also calls `ConwayBuilder::with_declined_backend_kinds`** (board item
/// 01KZHF2W8Y1KBM7PJH7R4QQJA0), unconditionally and before anything else
/// below, naming every id in [`backend_bundle`] that `wanted` does NOT
/// name -- purely diagnostic (that method's own doc): it changes no attach
/// behavior, only which of the two messages `ConwayBuilder::build` raises
/// for a `[backends.<id>]` entry naming an unresolved `kind` -- **declined**
/// (a kind this binary links but `wanted` did not select, e.g. an operator
/// removed it from `default_backends`) versus **unknown** (a kind this
/// binary has never heard of at all, e.g. a typo or an unregistered
/// third-party kind).
pub fn install(
    mut builder: ConwayBuilder,
    wanted: &[String],
) -> Result<ConwayBuilder, ConwayError> {
    // Board item 01KZHF2W8Y1KBM7PJH7R4QQJA0: every published backend-factory
    // id this binary links that `wanted` does NOT name is a DECLINED kind,
    // not an unknown one -- computed and handed to the builder before the
    // early return below, so the diagnosis is accurate even when `wanted` is
    // empty (declining both shipped dialects at once, e.g.
    // `default_backends = []`). Purely diagnostic
    // (`ConwayBuilder::with_declined_backend_kinds`'s own doc): it changes
    // nothing about which factories attach, only the message a later
    // `[backends.<id>]` entry naming a declined kind gets from `build()`.
    let backend_bundle = backend_bundle();
    let declined_backend_kinds: Vec<String> = backend_bundle
        .iter()
        .map(|f| f.id().to_string())
        .filter(|id| !wanted.iter().any(|w| w == id))
        .collect();
    builder = builder.with_declined_backend_kinds(declined_backend_kinds);
    if wanted.is_empty() {
        return Ok(builder);
    }
    let bundle = bundle();
    let router_bundle = router_bundle();
    let mut router_factories_installed: Vec<String> = Vec::new();
    for id in wanted {
        if let Some(plugin) = bundle.iter().find(|p| &p.manifest().id == id) {
            builder = builder.with_plugin(plugin.clone());
            continue;
        }
        if let Some(factory) = router_bundle.iter().find(|f| f.id() == id) {
            if let Some(already) = router_factories_installed.first() {
                return Err(ConwayError::Config {
                    path: None,
                    message: format!(
                        "plugins.install names more than one router factory ('{already}' and \
                         '{id}'); a build has exactly one router, so at most one router-factory \
                         id may appear in plugins.install."
                    ),
                });
            }
            router_factories_installed.push(id.clone());
            builder = builder.with_router_factory(factory.clone());
            continue;
        }
        if let Some(factory) = backend_bundle.iter().find(|f| f.id() == id) {
            builder = builder.with_backend_factory(factory.clone());
            continue;
        }
        let known_plugins: Vec<String> = bundle.iter().map(|p| p.manifest().id).collect();
        let known_routers: Vec<String> = router_bundle.iter().map(|f| f.id().to_string()).collect();
        let known_backends: Vec<String> =
            backend_bundle.iter().map(|f| f.id().to_string()).collect();
        return Err(ConwayError::Config {
            path: None,
            message: format!(
                "plugins.install names unknown first-party id '{id}'; linked first-party \
                 plugins: [{}]; linked router factories: [{}]; linked backend factories: [{}]. \
                 A third-party plugin is installed with ConwayBuilder::with_plugin (or a \
                 third-party router/backend with ConwayBuilder::with_router_factory/\
                 with_backend_factory) in library code and is not listed here.",
                known_plugins.join(", "),
                known_routers.join(", "),
                known_backends.join(", ")
            ),
        });
    }
    Ok(builder)
}

/// `[plugins].install` UNIONED with `[plugins].default_backends`,
/// deduplicated (a redundant explicit `install` entry for an id
/// `default_backends` already names is harmless, not a duplicate-factory
/// build error), in that order -- what [`install`] above actually resolves
/// against. Extracted so `main.rs`'s `build_conway` (the single choke
/// point every dispatch target shares) states this union in one call
/// rather than repeating the `HashSet`/order-preserving logic inline.
pub fn wanted_ids(install: &[String], default_backends: &[String]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    install
        .iter()
        .chain(default_backends.iter())
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect()
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
/// rather than on an intermediate signal.
///
/// The `with_declined_backend_kinds` call this function adds (board item
/// 01KZHF2W8Y1KBM7PJH7R4QQJA0) is covered the same way, separately, in
/// `tests/decline_backend_kind.rs`: declining a shipped dialect via
/// `[plugins].default_backends` while a `[backends.<id>]` entry still names
/// it fails the real compiled binary with a message that reads as
/// **declined**, and a kind this binary has never linked at all still fails
/// with the pre-existing **unknown-kind** message — with a third test
/// pinning that the two stderr strings are genuinely different text.
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

    /// [`wanted_ids`]'s own contract: union, order-preserving, deduplicated
    /// -- an id present in both lists appears exactly once, at `install`'s
    /// position.
    #[test]
    fn wanted_ids_unions_install_and_default_backends_deduplicated() {
        let install = vec![
            "conway.plugin_skeleton".to_string(),
            "anthropic".to_string(),
        ];
        let default_backends = vec!["anthropic".to_string(), "openai-compat".to_string()];
        assert_eq!(
            wanted_ids(&install, &default_backends),
            vec![
                "conway.plugin_skeleton".to_string(),
                "anthropic".to_string(),
                "openai-compat".to_string(),
            ]
        );
    }

    /// The default case an ordinary `settings.json` (no `[plugins]` section
    /// at all) produces: `install` empty, `default_backends` at its own
    /// default -- `wanted_ids` still names both dialects.
    #[test]
    fn wanted_ids_with_empty_install_still_names_the_default_backends() {
        let default_backends = vec!["anthropic".to_string(), "openai-compat".to_string()];
        assert_eq!(wanted_ids(&[], &default_backends), default_backends);
    }
}
