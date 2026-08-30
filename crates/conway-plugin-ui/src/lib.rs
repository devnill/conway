//! `conway-plugin-ui` (`conway.ui`): the first bundled plugin to PROVIDE an
//! Edge B capability (board item `01M0WWPA70E8YAAN981EK10D3D`,
//! `docs/vision/DESIGN-plugin-dependencies.md` §2/§7a), and the first
//! in-tree caller of [`conway::plugin::CapabilityCallHandle::call_versioned`]
//! (`conway-plugin-skeleton`'s `skeleton_ask` tool is that consumer -- see
//! this crate's own tests and that crate's own module doc for the
//! end-to-end proof).
//!
//! **What this crate publishes: exactly one capability, `ui.form`, at
//! `1.0.0`.** A question with a fixed, ordered list of options in; one
//! selected option back -- the *pull* shape design §7c names (a blocking
//! call, one provider, one answer), the AskUserQuestion analogue named in
//! this item's own spec. Nothing else: no checkbox, no multi-select, no
//! nested widget tree. Design §7a rules the extensible declarative widget
//! tree IN as the eventual altitude, but its own operative half is the
//! sequencing constraint -- "ship only the primitives your first real form
//! actually exercises... an implementer designing a widget vocabulary no
//! shipped form uses has left the ruling." [`AskSelectRequest`]/
//! [`AskSelectAnswer`] are that first, narrow shape; nothing wider is built
//! here, by design, not by omission.
//!
//! **Host requirement, declared honestly: NOT as a `required_host_caps`/
//! `optional_host_caps` entry.** A drawing surface exists in the TUI and
//! does not exist under `conway -p` -- but whether one exists is a property
//! of how THIS PROCESS is running, not of `settings.json`, and
//! `crate::host_caps::HostCaps::from_config` (the one production source of
//! what a host offers) derives every cap it knows about from config alone.
//! Modelling "has a drawing surface" as a host capability would need a new
//! `ConwayBuilder` injection point threaded ahead of every build -- a real,
//! separate mechanism nothing here has a shipped consumer for yet (INTENT
//! §8.5: build a seam when there is a consumer for it). Instead,
//! [`ConwayUiPlugin`] takes its answering [`FormSurface`] (or its absence)
//! as a plain constructor argument, exactly the shape
//! `crates/conway-cli/src/tui/gate.rs`'s `TuiGate` already establishes for
//! an analogous "the TUI can answer this; a one-shot run cannot" capability:
//! `ConwayUiPlugin::new(Some(surface))` where one is wired in,
//! `ConwayUiPlugin::new(None)` (equivalently `ConwayUiPlugin::default()`)
//! where none is. `crates/conway-cli/src/first_party_plugins.rs` passes
//! `None` for both the TUI and one-shot builds today -- **no live,
//! interactive `FormSurface` is wired into the shipped binary in this
//! pass** (a disclosed scope decision; see this item's own completion
//! report for the reasoning: no shipped form yet needs a specific
//! rendering, and building one for a proof-of-mechanism consumer would be
//! exactly the "designing on theory" INTENT §8.5 forbids). Either way, this
//! plugin ALWAYS installs -- the manifest declares no host capability at
//! all -- and the degrade/refuse decision happens per CALL, inside
//! [`FormProvider::call`], never at `ConwayBuilder::build` time. This is
//! what lets `conway.permissions`-shaped consumer degrade rather than fail
//! the whole run under `-p` (acceptance 3): the capability is always
//! reachable, and a call against it fails cleanly and namelessly-once
//! (naming the reason) rather than refusing to install in the first place.
//!
//! **Never a second, competing modal stack.** The TUI already owns one
//! (`crates/conway-cli/src/tui/state/modal.rs`'s `Mode` enum and its
//! `promote_next_surface` park/promote queue, built for the permission
//! prompt and since extended to the `/ask` modal, the intent-confirm card,
//! and the trust-preview card). This crate does not add a fifth surface to
//! that queue: with no live `FormSurface` wired in (see above), there is
//! nothing here that would need to.
//!
//! **`conway::plugin` alone, no `conway-core` dependency** -- the same
//! GP-03 discipline `conway-plugin-skeleton`'s own module doc states: a
//! first-party plugin gets no privileged API. Everything this crate names
//! (`Plugin`, `CapabilityProvider`, `CapabilityRegistration`,
//! `CapabilityError`, `HostCapability`, `PluginDescription`) is reachable
//! through the facade a third party gets too.

use std::sync::Arc;

use conway::plugin::{
    async_trait, CapabilityError, CapabilityProvider, CapabilityRegistration, HostCapability,
    Plugin, PluginDescription, PluginManifest, Tool,
};
use serde::{Deserialize, Serialize};

/// This plugin's manifest id -- the string an operator names in
/// `[plugins].install` (`settings.json`) to enable it. Bundled, never
/// enabled by default (`docs/vision/DESIGN-plugin-dependencies.md` §0
/// ruling 2): a build with no `[plugins]` section installs this plugin NOT
/// AT ALL, the SAME tier rule `PluginsConfig::install`'s own doc states for
/// every first-party candidate.
pub const PLUGIN_ID: &str = "conway.ui";

/// The one capability this plugin publishes -- `ui.form`, resolved through
/// [`conway::plugin::HostCapability::named`], the SAME open, namespaced
/// vocabulary `PluginManifest::required_host_caps`/`::optional_host_caps`
/// already validate through (Edge A and Edge B share one vocabulary, not
/// two).
pub const FORM_CAPABILITY: &str = "ui.form";

/// This registration's declared version -- standard semver, a field
/// separate from [`FORM_CAPABILITY`]'s name string (decision
/// `01M189XS6Z9VKYENAHNY1B54CM`, `crate::ports::capability`'s own module
/// doc in `conway-core`): `ui.form` stays `ui.form`; `1.0.0` is this
/// constant. A consumer supplies its own `semver::VersionReq` (`^1` for the
/// ordinary floor) to
/// [`conway::plugin::CapabilityCallHandle::call_versioned`] -- see
/// `conway-plugin-skeleton`'s `skeleton_ask` tool for the worked example.
pub const FORM_CAPABILITY_VERSION: &str = "1.0.0";

/// One question posed through `ui.form`: `prompt` plus a non-empty,
/// ordered list of `options`, presented verbatim by whatever answers it.
/// Plain, `Serialize`/`Deserialize` data -- the SAME shape an
/// out-of-process caller crosses a wire with (`docs/vision/
/// DESIGN-plugin-dependencies.md` §2's own argument for why Edge B is
/// dynamic JSON rather than a typed Rust trait per capability): a
/// subprocess plugin builds this exact JSON object by hand, with no
/// dependency on this crate at all.
///
/// **This is the whole of the v1 request shape -- no checkbox, no
/// multi-select, no free-text answer.** `conway-plugin-skeleton`'s
/// `skeleton_ask` tool (this item's own first real consumer) asks a single
/// fixed question with two options; that is the entire shipped use case,
/// and it is the only reason this shape has the fields it has and no
/// others -- see this crate's own module doc, "What this crate publishes".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AskSelectRequest {
    /// The question text, shown verbatim.
    pub prompt: String,
    /// The choices offered, in display order. [`FormProvider::call`]
    /// refuses ([`CapabilityError`]) a request whose `options` is empty --
    /// P-10: a caller-supplied shape is untrusted input, range-checked at
    /// the boundary, never a panic.
    pub options: Vec<String>,
}

/// `ui.form`'s answer: the one option string the caller chose, verbatim
/// from [`AskSelectRequest::options`] -- never an index, so a consumer
/// never has to re-resolve an answer against the request it sent to know
/// what was picked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AskSelectAnswer {
    pub selected: String,
}

/// A failure [`FormSurface::ask_select`] returns -- plain data (not
/// `Serialize`, unlike [`CapabilityError`]: a [`FormSurface`] is always
/// in-process, wired in by whatever host constructed this plugin, never
/// forwarded across a wire the way a [`CapabilityProvider`] itself might
/// be).
#[derive(Clone, Debug, PartialEq)]
pub struct FormSurfaceError {
    pub message: String,
}

impl FormSurfaceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for FormSurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FormSurfaceError {}

/// **Edge A, minimally: the one thing a host that can actually draw a
/// question implements.** Mechanism (design §7a's INTENT §8.2 test:
/// "focus, input routing, modal stacking... is mechanism"), not policy --
/// this trait says nothing about HOW a question is drawn, only that
/// something in the host CAN present `options` and return the one the
/// operator chose. `conway.ui` itself never draws anything; it is the
/// plugin-side half of the `ui.form` capability, and this trait is the
/// seam a host plugs an answering mechanism into.
///
/// **No implementation of this trait ships in this crate.** See this
/// crate's own module doc, "Host requirement, declared honestly", for why
/// no live, interactive implementation is wired into the shipped binary in
/// this pass, and `FormProvider::call`'s own doc for what happens when
/// [`ConwayUiPlugin`] holds `None` instead of a real one.
#[async_trait]
pub trait FormSurface: Send + Sync + 'static {
    /// Presents `request` and returns the operator's choice, or a
    /// [`FormSurfaceError`] if the surface could not collect one (e.g. the
    /// operator cancelled, or the surface's own channel closed).
    async fn ask_select(
        &self,
        request: AskSelectRequest,
    ) -> Result<AskSelectAnswer, FormSurfaceError>;
}

/// [`ConwayUiPlugin::capabilities`]'s one registration -- answers `ui.form`
/// by delegating to `surface` when one is present, refusing cleanly
/// (never blocking, never panicking) when it is not.
struct FormProvider {
    surface: Option<Arc<dyn FormSurface>>,
}

#[async_trait]
impl CapabilityProvider for FormProvider {
    async fn call(&self, payload: serde_json::Value) -> Result<serde_json::Value, CapabilityError> {
        // P-10: `payload` is caller-supplied (another plugin's own JSON
        // construction, potentially relayed from an out-of-process
        // subprocess plugin) -- range/shape-checked here, never trusted
        // structurally, and a malformed shape maps to a typed
        // `CapabilityError`, never a panic.
        let request: AskSelectRequest = serde_json::from_value(payload).map_err(|e| {
            CapabilityError::new(format!("ui.form: request did not match the expected shape: {e}"))
        })?;
        if request.options.is_empty() {
            return Err(CapabilityError::new(
                "ui.form: request.options must not be empty",
            ));
        }
        // The degrade point (acceptance 3, `docs/vision/
        // DESIGN-plugin-dependencies.md` §4b: "no surface may degrade
        // silently"). This plugin ALWAYS installs -- see this crate's own
        // module doc -- so a host with no drawing surface (today, every
        // host: see that doc) still reaches this call; it refuses
        // immediately, naming the reason in both `message` and a
        // machine-readable `detail.code`, rather than blocking forever
        // waiting for an answer nothing can ever produce.
        let Some(surface) = &self.surface else {
            return Err(CapabilityError::with_detail(
                "ui.form: no drawing surface is available in this host; conway.ui cannot ask \
                 interactively here"
                    .to_string(),
                serde_json::json!({ "code": "no_drawing_surface" }),
            ));
        };
        let answer = surface
            .ask_select(request)
            .await
            .map_err(|e| CapabilityError::new(e.message))?;
        Ok(serde_json::to_value(answer).expect("AskSelectAnswer always serializes to JSON"))
    }
}

/// `conway.ui`: publishes `ui.form` over Edge B. Bundled and first-party
/// (`docs/vision/DESIGN-plugin-dependencies.md` §0 ruling 1), but opt-in
/// like every other bundle member (ruling 2) -- naming `"conway.ui"` in
/// `[plugins].install` is the whole of enabling it; nothing about being
/// bundled skips that step.
///
/// Contributes no tool, no command, no setting -- see
/// [`Self::description`] for the full, tested claim. This is a TOOLKIT
/// plugin in design §2's sense: the value it adds is reachable only by
/// ANOTHER installed plugin calling into `ui.form`, never directly by the
/// model or the operator.
pub struct ConwayUiPlugin {
    surface: Option<Arc<dyn FormSurface>>,
}

impl ConwayUiPlugin {
    /// `surface` is `Some` when the constructing host can actually present
    /// a question and collect an answer (see this crate's own module doc
    /// for why no production call site passes `Some` yet), `None`
    /// otherwise -- every call into `ui.form` then refuses immediately with
    /// a named reason rather than blocking on an answer nothing can ever
    /// produce.
    pub fn new(surface: Option<Arc<dyn FormSurface>>) -> Self {
        Self { surface }
    }
}

impl Default for ConwayUiPlugin {
    /// The construction every shipped call site uses today
    /// (`crates/conway-cli/src/first_party_plugins.rs`) -- no drawing
    /// surface wired in. See this crate's own module doc.
    fn default() -> Self {
        Self::new(None)
    }
}

impl Plugin for ConwayUiPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            // Versioned WITH the workspace -- see this crate's own
            // Cargo.toml doc comment.
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![],
            // Deliberately empty -- see this crate's own module doc,
            // "Host requirement, declared honestly", for why "needs a
            // drawing surface" is NOT expressed as a host capability.
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    /// A capability claim, not a convention -- `description_matches_what_
    /// this_plugin_actually_contributes` (this module's own test) checks
    /// `you_get`/`you_lose` against what [`Self::tools`]/
    /// [`Self::capabilities`] actually return, so this text cannot drift
    /// from this plugin's real behavior the way `docs/vision/
    /// DESIGN-surface-coherence.md` §7 warns an implicit, unchecked
    /// contribution listing does.
    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "publishes ui.form: a blocking ask-with-options capability another \
                      installed plugin calls into"
                .to_string(),
            you_get: format!(
                "one capability ({FORM_CAPABILITY}, v{FORM_CAPABILITY_VERSION}) other plugins \
                 can call through `ToolCtx::capabilities`. No tool, no command, and no setting \
                 of its own -- nothing here is reachable by the model or the operator directly."
            ),
            you_lose: "nothing on its own -- a plugin that DEPENDS on ui.form loses its \
                       interactive question if this plugin is not installed, but that is a \
                       property of the DEPENDENT, not of this one"
                .to_string(),
            costs: "none while idle; in this build, every call refuses immediately rather than \
                    blocking, since no host wires a live drawing surface into this plugin yet \
                    (see this crate's own module doc)"
                .to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn capabilities(&self) -> Vec<CapabilityRegistration> {
        vec![CapabilityRegistration::new(
            HostCapability::named(FORM_CAPABILITY)
                .expect("\"ui.form\" is a well-formed namespaced capability name"),
            FORM_CAPABILITY_VERSION,
            Arc::new(FormProvider {
                surface: self.surface.clone(),
            }),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::plugin::{CapabilityCallError, CapabilityCallHandle, CapabilityRegistry};

    /// A [`FormSurface`] fixture that always answers with a fixed choice --
    /// the SAME role `capability.rs`'s own `EchoProvider` fixture plays one
    /// layer down: proving a REAL call reaches a REAL provider and the
    /// registered surface's own answer (not a default) comes back, without
    /// needing a live terminal to do it.
    struct FixedAnswerSurface {
        answer: String,
    }

    #[async_trait]
    impl FormSurface for FixedAnswerSurface {
        async fn ask_select(
            &self,
            request: AskSelectRequest,
        ) -> Result<AskSelectAnswer, FormSurfaceError> {
            assert!(
                !request.options.is_empty(),
                "FormProvider must never forward an empty-options request to a surface"
            );
            Ok(AskSelectAnswer {
                selected: self.answer.clone(),
            })
        }
    }

    fn registry_from(plugin: &ConwayUiPlugin) -> CapabilityRegistry {
        CapabilityRegistry::from_registrations(plugin.capabilities())
            .expect("conway.ui registers exactly one capability, never a duplicate")
    }

    fn ask_payload() -> serde_json::Value {
        serde_json::to_value(AskSelectRequest {
            prompt: "proceed?".to_string(),
            options: vec!["yes".to_string(), "no".to_string()],
        })
        .unwrap()
    }

    // -----------------------------------------------------------------
    // Acceptance 2 (first half): a real consumer, ask-and-answer end to
    // end, when a surface IS present.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn ask_and_answer_round_trips_through_a_real_registered_provider() {
        let plugin = ConwayUiPlugin::new(Some(Arc::new(FixedAnswerSurface {
            answer: "yes".to_string(),
        })));
        let handle = CapabilityCallHandle::new(Arc::new(registry_from(&plugin)), "acme.consumer");
        let required = semver::VersionReq::parse("^1").expect("valid semver req");

        let answer = handle
            .call_versioned(FORM_CAPABILITY, &required, ask_payload())
            .await
            .expect("a satisfied ^1 requirement against a 1.0.0 provider must succeed");
        let answer: AskSelectAnswer = serde_json::from_value(answer).expect("well-formed answer");
        assert_eq!(answer.selected, "yes");
    }

    // -----------------------------------------------------------------
    // Acceptance 3: no surface -> refuses immediately, never blocks,
    // never panics, names the reason.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn no_surface_refuses_immediately_naming_the_reason() {
        let plugin = ConwayUiPlugin::default();
        let handle = CapabilityCallHandle::new(Arc::new(registry_from(&plugin)), "acme.consumer");
        let required = semver::VersionReq::parse("^1").expect("valid semver req");

        let err = handle
            .call_versioned(FORM_CAPABILITY, &required, ask_payload())
            .await
            .expect_err("no surface must refuse, not hang or panic");
        match err {
            CapabilityCallError::Provider { capability, error } => {
                assert_eq!(capability, FORM_CAPABILITY);
                assert_eq!(
                    error.detail,
                    serde_json::json!({ "code": "no_drawing_surface" }),
                    "the reason must be machine-readable, not only prose"
                );
            }
            other => panic!("expected Provider (a refusal from ui.form itself), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_empty_options_request_is_refused_before_reaching_the_surface() {
        // P-10: a malformed caller-supplied shape maps to a typed error,
        // never a panic and never a call into the surface with nothing to
        // present.
        struct PanicsIfCalled;
        #[async_trait]
        impl FormSurface for PanicsIfCalled {
            async fn ask_select(
                &self,
                _request: AskSelectRequest,
            ) -> Result<AskSelectAnswer, FormSurfaceError> {
                panic!("must not be reached for an empty-options request");
            }
        }
        let plugin = ConwayUiPlugin::new(Some(Arc::new(PanicsIfCalled)));
        let handle = CapabilityCallHandle::new(Arc::new(registry_from(&plugin)), "acme.consumer");
        let payload = serde_json::to_value(AskSelectRequest {
            prompt: "proceed?".to_string(),
            options: vec![],
        })
        .unwrap();

        let err = handle
            .call(FORM_CAPABILITY, payload)
            .await
            .expect_err("empty options must be refused");
        assert!(matches!(err, CapabilityCallError::Provider { .. }));
    }

    // -----------------------------------------------------------------
    // Acceptance 2 (second half): a mismatched VersionReq is refused
    // naming both the requirement and the version actually installed.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn a_version_requirement_ui_form_does_not_satisfy_is_refused_naming_both() {
        let plugin = ConwayUiPlugin::default();
        let handle = CapabilityCallHandle::new(Arc::new(registry_from(&plugin)), "acme.consumer");
        let required = semver::VersionReq::parse("^2").expect("valid semver req");

        let err = handle
            .call_versioned(FORM_CAPABILITY, &required, ask_payload())
            .await
            .expect_err("ui.form is 1.0.0; ^2 must not match it");
        match err {
            CapabilityCallError::VersionMismatch {
                capability,
                required: named_required,
                available,
                available_declared,
            } => {
                assert_eq!(capability, FORM_CAPABILITY);
                assert_eq!(named_required, required);
                assert_eq!(available, semver::Version::new(1, 0, 0));
                assert!(available_declared, "conway.ui declares a real version");
            }
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // conway.ui registers ui.form at exactly 1.0.0.
    // -----------------------------------------------------------------

    #[test]
    fn conway_ui_registers_ui_form_at_one_dot_zero_dot_zero() {
        let plugin = ConwayUiPlugin::default();
        let registrations = plugin.capabilities();
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].capability.as_wire_str(), FORM_CAPABILITY);
        assert_eq!(registrations[0].version, semver::Version::new(1, 0, 0));
        assert!(registrations[0].version_declared);
    }

    // -----------------------------------------------------------------
    // Acceptance 1: manifest declares no host capability, so refusing to
    // enable this plugin is purely a `[plugins].install` config decision,
    // never a build-time capability negotiation -- the manifest-level half
    // of that acceptance criterion. `crates/conway-cli/tests/
    // ui_form_absent_by_default.rs` (a real compiled-binary run) owns the
    // config-level half: absent from `install`, `ui.form` is unreachable.
    // -----------------------------------------------------------------

    #[test]
    fn manifest_declares_no_host_capability_requirement() {
        let manifest = ConwayUiPlugin::default().manifest();
        assert_eq!(manifest.id, PLUGIN_ID);
        assert!(manifest.required_host_caps.is_empty());
        assert!(manifest.optional_host_caps.is_empty());
        assert!(manifest.requires.is_empty());
        assert!(manifest.optional.is_empty());
    }

    // -----------------------------------------------------------------
    // Build item 4 / acceptance 7: the contribution declaration is a
    // checked claim, not prose that can drift from this plugin's actual
    // behavior (`docs/vision/DESIGN-surface-coherence.md` §7/§10).
    // -----------------------------------------------------------------

    #[test]
    fn description_matches_what_this_plugin_actually_contributes() {
        let plugin = ConwayUiPlugin::default();
        let description = plugin.description();
        assert!(!description.summary.is_empty());
        assert!(
            description.you_get.contains(FORM_CAPABILITY),
            "you_get must name the capability it actually registers, got: {}",
            description.you_get
        );
        assert!(
            description.you_get.contains(FORM_CAPABILITY_VERSION),
            "you_get must name the version it actually declares, got: {}",
            description.you_get
        );
        // The claim "no tool, no command of its own" is checked against
        // this plugin's REAL `Plugin::tools`/`Plugin::commands` -- if a
        // future change adds one without updating this text, this test
        // fails rather than the claim silently going stale.
        assert!(plugin.tools().is_empty(), "description claims no tools");
        assert!(plugin.commands().is_empty(), "description claims no commands");
        assert_eq!(
            plugin.capabilities().len(),
            1,
            "description claims exactly one capability"
        );
    }
}
