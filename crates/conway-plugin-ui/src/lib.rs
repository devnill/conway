//! `conway-plugin-ui` (`conway.ui`): a first-party plugin that both
//! PUBLISHES an Edge B capability (board item `01M0WWPA70E8YAAN981EK10D3D`,
//! `docs/vision/DESIGN-plugin-dependencies.md` §2/§7a) and, as of board item
//! `01M19NH39AE2D5AMJK0RZRQY86` (operator decision `01M19NF1C8E8CA8Y3X653Q3R23`),
//! contributes a TOOL the model calls directly.
//!
//! **The licensing consumer is the model, not another plugin -- read this
//! before anything else in this file.** The operator's own ruling: "conway.ui
//! should work as a standalone feature, making the consumer rule moot. I
//! need to be able to prompt a model to be able to interact with me in an
//! interview format." The path is model -> tool -> operator, never
//! plugin -> plugin -> operator: `AskQuestionTool` (bare name
//! [`ASK_QUESTION_TOOL_NAME`]) is what the model calls, and it is what makes
//! this crate's narrow-first posture SATISFIED rather than WAIVED (INTENT
//! §8.5) -- the previous item (`01M0WWPA70E8YAAN981EK10D3D`) shipped with no
//! live [`FormSurface`] precisely because its only consumer
//! (`conway-plugin-skeleton`'s `skeleton_ask`) was a proof-of-mechanism tool
//! with no real on-screen need; see that item's own completion report for
//! the reasoning this item's existence answers. `skeleton_ask` still exists,
//! still calls `ui.form` over Edge B, and is still a real (if secondary)
//! consumer of the capability described below -- this item does not touch
//! it.
//!
//! **What this crate publishes: exactly one capability (`ui.form`, `1.0.0`)
//! and exactly one tool ([`ASK_QUESTION_TOOL_NAME`]), both answering through
//! the identical declarative shape.** A question with a fixed, ordered list
//! of options in; one selected option back -- the *pull* shape design §7c
//! names (a blocking call, one provider, one answer), the AskUserQuestion
//! analogue named in this item's own spec. Nothing else: no checkbox, no
//! multi-select, no free-text answer alongside the options, no
//! answer-conditioned follow-up, no nested widget tree. Design §7a rules the
//! extensible declarative widget tree IN as the eventual altitude, but its
//! own operative half is the sequencing constraint -- "ship only the
//! primitives your first real form actually exercises... an implementer
//! designing a widget vocabulary no shipped form uses has left the ruling."
//! [`AskSelectRequest`]/[`AskSelectAnswer`] are that first, narrow shape,
//! shared verbatim by both the tool and the capability (the crate-private
//! `ask` function, the one place the "call the surface, refuse cleanly if
//! none is wired in" logic lives -- one implementation, never restated at
//! either call site);
//! nothing wider is built here, by design, not by omission. **This item did
//! NOT hit the first falsifier §8 names** ("`conway.ui` needs to draw, not
//! declare"): the tool a real interview needs is exactly this declarative
//! shape, unchanged.
//!
//! **Host requirement, declared honestly: NOT as a `required_host_caps`/
//! `optional_host_caps` entry.** A drawing surface exists in the TUI and
//! does not exist under `conway -p` -- but whether one exists is a property
//! of how THIS PROCESS is running, not of `settings.json`, and
//! `crate::host_caps::HostCaps::from_config` (the one production source of
//! what a host offers) derives every cap it knows about from config alone.
//! Modelling "has a drawing surface" as a host capability would need a new
//! `ConwayBuilder` injection point threaded ahead of every build -- a real,
//! separate mechanism this crate still has no shipped consumer for (INTENT
//! §8.5: build a seam when there is a consumer for it). Instead,
//! [`ConwayUiPlugin`] takes its answering [`FormSurface`] (or its absence)
//! as a plain constructor argument, exactly the shape
//! `crates/conway-cli/src/tui/gate.rs`'s `TuiGate` already establishes for
//! an analogous "the TUI can answer this; a one-shot run cannot" capability:
//! `ConwayUiPlugin::new(Some(surface))` where one is wired in,
//! `ConwayUiPlugin::new(None)` (equivalently `ConwayUiPlugin::default()`)
//! where none is.
//!
//! **A live, interactive `FormSurface` now IS wired into the shipped
//! binary -- for the TUI only.** `crates/conway-cli/src/main.rs` builds a
//! `crate::tui::form::TuiFormSurface` channel exactly when the process is
//! about to run the interactive TUI (mirroring `tui::gate::TuiGate`'s own
//! channel, built at the identical call site, for the identical reason: a
//! `PermissionGate`/`FormSurface` must be handed to `ConwayBuilder` before
//! `build()` returns, before the TUI's own `App`/`AppState` exist to answer
//! into). Every OTHER dispatch target (`-p` one-shot, `sessions`, `routes`,
//! a plugin subcommand) still constructs `ConwayUiPlugin::default()` -- see
//! `crates/conway-cli/src/first_party_plugins.rs`'s own doc for exactly
//! where that split happens. This plugin ALWAYS installs once named -- the
//! manifest declares no host capability at all -- and the degrade/refuse
//! decision happens per CALL, inside the crate-private `ask` function,
//! never at `ConwayBuilder::build` time. This is what lets a one-shot run
//! degrade rather than fail
//! (acceptance 3): the tool and the capability are always reachable, and a
//! call against either fails cleanly and namelessly-once (naming the
//! reason) rather than refusing to install in the first place, or blocking
//! forever waiting for an answer nothing under `-p` could ever produce.
//!
//! **Never a second, competing modal stack.** The TUI already owns one
//! (`crates/conway-cli/src/tui/state/modal.rs`'s `Mode` enum and its
//! `promote_next_surface` park/promote queue, built for the permission
//! prompt and since extended to the `/ask` modal, the intent-confirm card,
//! and the trust-preview card). `Mode::UiForm` is the FIFTH surface in that
//! same queue, joining its existing park/promote discipline rather than
//! building a competing one -- see that module's own doc for the priority
//! order a question now takes its place in. A model-raised question is not
//! one of `docs/vision/DESIGN-surface-coherence.md`'s three operator-invoked
//! surface kinds (ACTION/VIEW/CONFIGURATION -- that page's own inventory is
//! `/`-command surfaces an operator TYPES); it is reactive, exactly like the
//! permission prompt and the other three modal-bearing cards it now sits
//! beside, none of which that page's six rules govern either.
//!
//! **`conway::plugin` alone, no `conway-core` dependency** -- the same
//! discipline `conway-plugin-skeleton`'s own module doc states: a
//! first-party plugin gets no privileged API. Everything this crate names
//! (`Plugin`, `CapabilityProvider`, `CapabilityRegistration`,
//! `CapabilityError`, `HostCapability`, `PluginDescription`, `Tool`,
//! `ToolCtx`) is reachable through the facade a third party gets too.

use std::sync::Arc;

use conway::plugin::{
    async_trait, CapabilityError, CapabilityProvider, CapabilityRegistration, ContentBlock,
    HostCapability, PathArgs, PermissionClass, Plugin, PluginDescription, PluginManifest,
    RenderKind, Tool, ToolCall, ToolCategory, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec,
    TruncationPolicy,
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
    /// The choices offered, in display order. `FormProvider::call`
    /// refuses ([`CapabilityError`]) a request whose `options` is empty --
    /// a caller-supplied shape is untrusted input, range-checked at
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

/// The three ways [`ask`] can fail to produce an [`AskSelectAnswer`] -- the
/// ONE place this crate's "call the surface, refuse cleanly if none is
/// wired in, never forward a malformed request" logic lives (P-14: no
/// restatement). [`FormProvider::call`] (Edge B, JSON in/out) and
/// [`AskQuestionTool::invoke`] (the tool the model calls directly) both
/// delegate to [`ask`] and each format ONE of these three outcomes in
/// whatever shape their own transport needs -- a `CapabilityError` carrying
/// a machine-readable `detail.code` for the former, a plain degrade
/// sentence for the latter (skeleton_ask's own "never fail the call either
/// way" posture, restated at this crate's own tool now that it has one).
enum AskError {
    /// P-10: a caller-supplied `options` list with nothing in it -- checked
    /// BEFORE a surface is ever consulted, so an empty request never
    /// reaches [`FormSurface::ask_select`] at all.
    EmptyOptions,
    /// The degrade point (acceptance 3, `docs/vision/
    /// DESIGN-plugin-dependencies.md` §4b: "no surface may degrade
    /// silently"). This plugin ALWAYS installs -- see this crate's own
    /// module doc -- so a host with no drawing surface (every host except
    /// an interactive TUI today: see that doc) still reaches this call; it
    /// refuses immediately rather than blocking forever waiting for an
    /// answer nothing under that host could ever produce.
    NoSurface,
    /// The surface itself refused or failed (the operator cancelled, or the
    /// surface's own channel closed) -- see [`FormSurfaceError`].
    Surface(FormSurfaceError),
}

/// The one implementation of "ask a real, live surface for `request`,
/// refusing cleanly rather than forwarding a malformed or unanswerable
/// request" -- shared verbatim by [`FormProvider::call`] (Edge B) and
/// [`AskQuestionTool::invoke`] (the tool the model calls directly), so the
/// two consumption paths this crate offers can never drift on what counts
/// as empty options, what counts as "no surface," or what a surface's own
/// failure means (P-14).
async fn ask(
    surface: &Option<Arc<dyn FormSurface>>,
    request: AskSelectRequest,
) -> Result<AskSelectAnswer, AskError> {
    if request.options.is_empty() {
        return Err(AskError::EmptyOptions);
    }
    let Some(surface) = surface else {
        return Err(AskError::NoSurface);
    };
    surface.ask_select(request).await.map_err(AskError::Surface)
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
            CapabilityError::new(format!(
                "ui.form: request did not match the expected shape: {e}"
            ))
        })?;
        match ask(&self.surface, request).await {
            Ok(answer) => {
                Ok(serde_json::to_value(answer).expect("AskSelectAnswer always serializes to JSON"))
            }
            Err(AskError::EmptyOptions) => Err(CapabilityError::new(
                "ui.form: request.options must not be empty",
            )),
            Err(AskError::NoSurface) => Err(CapabilityError::with_detail(
                "ui.form: no drawing surface is available in this host; conway.ui cannot ask \
                 interactively here"
                    .to_string(),
                serde_json::json!({ "code": "no_drawing_surface" }),
            )),
            Err(AskError::Surface(e)) => Err(CapabilityError::new(e.message)),
        }
    }
}

/// The bare name `AskQuestionTool` registers under -- reachable by the
/// model as `ask_question`, the SAME unnamespaced convention every other
/// first-party plugin tool in this workspace uses (`skeleton_ping`,
/// `compose_context_path`, `search_sessions`, ...).
pub const ASK_QUESTION_TOOL_NAME: &str = "ask_question";

/// `AskQuestionTool`'s arguments -- the model-facing mirror of
/// [`AskSelectRequest`] (kept as a separate type, not a re-export of it, so
/// this tool's own `schemars::JsonSchema` derive can carry model-facing
/// field docs without leaking them into the Edge B wire shape a subprocess
/// plugin builds by hand).
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AskQuestionArgs {
    /// The question to ask the operator, shown verbatim.
    prompt: String,
    /// The choices to offer, in display order. Must not be empty --
    /// [`ask`] refuses a request with none (P-10), before ever reaching an
    /// answering surface.
    options: Vec<String>,
}

/// `ask_question`: the tool a model calls directly to interview the
/// operator -- the consumer that licenses this plugin as a standalone
/// operator-facing feature (this crate's own module doc, "The licensing
/// consumer is the model"). Presents `prompt` with `options` through
/// whatever [`FormSurface`] this plugin instance was constructed with, and
/// reports the operator's choice verbatim.
///
/// **Never fails the call for "nobody could answer it"** (mirrors
/// `conway-plugin-skeleton`'s `skeleton_ask`'s own posture exactly, restated
/// here now that this crate has a tool of its own): no live surface wired
/// in (every host except an interactive TUI today) is the SAME ordinary,
/// expected outcome as an operator who cancelled -- this tool says so in
/// plain text and moves on, `ToolOutput::is_error` stays `false` either way.
/// A malformed call (`options` empty) IS a tool error
/// (`ToolError::InvalidArguments`): that is a caller mistake, not an
/// environment the tool could not control.
struct AskQuestionTool {
    surface: Option<Arc<dyn FormSurface>>,
}

#[async_trait]
impl Tool for AskQuestionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(ASK_QUESTION_TOOL_NAME),
            description: "Asks the operator a question with a fixed list of options and \
                          returns the one they chose. Interactive hosts (the TUI) present it \
                          and collect a real answer; a host with no interactive surface (e.g. \
                          one-shot `-p`) reports that plainly instead of blocking -- this is \
                          never a tool error either way."
                .to_string(),
            schema: schemars::schema_for!(AskQuestionArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: AskQuestionArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;
        let request = AskSelectRequest {
            prompt: args.prompt,
            options: args.options,
        };
        let text = match ask(&self.surface, request).await {
            Ok(answer) => format!("operator selected: {}", answer.selected),
            Err(AskError::EmptyOptions) => {
                return Err(ToolError::InvalidArguments {
                    detail: "options must not be empty".to_string(),
                });
            }
            Err(AskError::NoSurface) => "no answer available: no interactive surface is \
                                          available in this host to ask the operator"
                .to_string(),
            Err(AskError::Surface(e)) => format!("no answer available: {e}"),
        };
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text { text }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: Vec::new(),
        })
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }
}

/// `conway.ui`: contributes the model-callable `ask_question` tool AND
/// publishes `ui.form` over Edge B for another installed plugin to call.
/// Bundled and first-party (`docs/vision/DESIGN-plugin-dependencies.md` §0
/// ruling 1), but opt-in like every other bundle member (ruling 2) --
/// naming `"conway.ui"` in `[plugins].install` is the whole of enabling it;
/// nothing about being bundled skips that step.
///
/// No command, no setting -- see [`Self::description`] for the full, tested
/// claim. Unlike this crate's own earlier shape (pinned by board item
/// `01M0WWPA70E8YAAN981EK10D3D`, "a TOOLKIT plugin in design §2's sense"),
/// this is no longer true of the tool half: `ask_question` is reachable
/// directly by the model, which is the whole point (this crate's own module
/// doc, "The licensing consumer is the model"). The capability half is
/// still toolkit-shaped -- `ui.form` is reachable only by another installed
/// plugin calling into it, never directly.
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
            tools: vec![ToolName::new(ASK_QUESTION_TOOL_NAME)],
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
    ///
    /// `costs` depends on whether THIS INSTANCE was constructed with a live
    /// [`FormSurface`] -- a plugin browser row is describing the actual
    /// installed instance, not a claim that would drift the moment a host
    /// wires one in (or out).
    fn description(&self) -> PluginDescription {
        let costs = if self.surface.is_some() {
            "none while idle; a call blocks until the operator answers or cancels, showing a \
             modal in this host's own live surface"
                .to_string()
        } else {
            "none while idle; in this host, every call refuses immediately rather than \
             blocking, since no live drawing surface is wired into this plugin instance here"
                .to_string()
        };
        PluginDescription {
            summary: "asks the operator a question with options directly (ask_question), and \
                      publishes ui.form: the same blocking ask-with-options capability another \
                      installed plugin can call into"
                .to_string(),
            you_get: format!(
                "one tool ({ASK_QUESTION_TOOL_NAME}) the model can call to interview the \
                 operator, and one capability ({FORM_CAPABILITY}, v{FORM_CAPABILITY_VERSION}) \
                 other plugins can call through `ToolCtx::capabilities`. No command and no \
                 setting of its own."
            ),
            you_lose: "the model's own ask_question tool, and a plugin that DEPENDS on ui.form \
                       loses its interactive question -- both properties of NOT installing this \
                       plugin at all, not of a dependent's own behavior"
                .to_string(),
            costs,
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(AskQuestionTool {
            surface: self.surface.clone(),
        })]
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
            description.you_get.contains(ASK_QUESTION_TOOL_NAME),
            "you_get must name the tool it actually contributes, got: {}",
            description.you_get
        );
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
        // The claim "no command of its own" is checked against this
        // plugin's REAL `Plugin::commands` -- if a future change adds one
        // without updating this text, this test fails rather than the claim
        // silently going stale.
        assert_eq!(
            plugin.tools().len(),
            1,
            "description claims exactly one tool"
        );
        assert_eq!(
            plugin.tools()[0].spec().name.as_str(),
            ASK_QUESTION_TOOL_NAME,
            "the one tool must be ask_question"
        );
        assert!(
            plugin.commands().is_empty(),
            "description claims no commands"
        );
        assert_eq!(
            plugin.capabilities().len(),
            1,
            "description claims exactly one capability"
        );
    }

    /// The manifest's own `tools` list -- checked at `ConwayBuilder::build`,
    /// independent of `Plugin::tools`'s own return -- must name
    /// `ask_question`, or naming this plugin in `[plugins].install` would
    /// leave the model with no way to reach it despite `Plugin::tools`
    /// returning it (the two lists are checked against each other
    /// elsewhere; this pins the manifest side directly).
    #[test]
    fn manifest_names_the_ask_question_tool() {
        let manifest = ConwayUiPlugin::default().manifest();
        assert_eq!(
            manifest.tools,
            vec![conway::plugin::ToolName::new(ASK_QUESTION_TOOL_NAME)]
        );
    }

    // -----------------------------------------------------------------
    // Acceptance 1: a model can call ask_question and receive the chosen
    // answer -- driven against the REAL `Tool` trait object `Plugin::tools`
    // returns, through a real `ToolCtx`, exactly the shape the runtime
    // dispatches through in production.
    // -----------------------------------------------------------------

    fn ask_question_tool(plugin: &ConwayUiPlugin) -> Arc<dyn Tool> {
        plugin
            .tools()
            .into_iter()
            .find(|t| t.spec().name.as_str() == ASK_QUESTION_TOOL_NAME)
            .expect("ConwayUiPlugin declares ask_question")
    }

    fn ask_question_call() -> ToolCall {
        ToolCall {
            call_id: "call-1".to_string(),
            name: ToolName::new(ASK_QUESTION_TOOL_NAME),
            arguments: serde_json::json!({
                "prompt": "proceed?",
                "options": ["yes", "no"],
            }),
        }
    }

    fn text_of(output: &ToolOutput) -> String {
        output
            .blocks
            .iter()
            .find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .expect("ask_question always replies with a text block")
    }

    fn test_ctx() -> ToolCtx {
        let agent_id = conway::AgentId::new();
        ToolCtx::for_test(
            agent_id,
            std::env::temp_dir(),
            Arc::new(conway_testkit::FakeSubagentHost::new(agent_id)),
            Arc::new(conway_testkit::CollectingEventSink::new()),
        )
    }

    #[tokio::test]
    async fn ask_question_reports_the_real_answer_when_a_surface_can_answer() {
        let plugin = ConwayUiPlugin::new(Some(Arc::new(FixedAnswerSurface {
            answer: "yes".to_string(),
        })));
        let output = ask_question_tool(&plugin)
            .invoke(ask_question_call(), test_ctx())
            .await
            .expect("ask_question never returns a ToolError for a well-formed call");
        assert!(!output.is_error);
        assert_eq!(text_of(&output), "operator selected: yes");
    }

    #[test]
    fn ask_question_tool_name_is_reachable_with_no_plugin_id_prefix() {
        // Model-callable tools in this workspace are named bare (no
        // namespace) -- `skeleton_ping`, `compose_context_path`, etc. --
        // pinned here so a future change cannot silently prefix this one.
        assert_eq!(ASK_QUESTION_TOOL_NAME, "ask_question");
    }

    /// **VERIFICATION ANCHOR (acceptance 3, unit-level half for the NEW
    /// consumer).** No live surface wired in -- `ConwayUiPlugin::default()`,
    /// the exact construction every non-TUI dispatch target uses -- degrades
    /// rather than fails. The compiled-binary sibling of this test
    /// (`crates/conway-cli/tests/ui_ask_question_degrades_under_one_shot.rs`)
    /// drives the identical shape through the real `conway -p` binary.
    #[tokio::test]
    async fn ask_question_degrades_without_failing_when_no_surface_is_wired_in() {
        let plugin = ConwayUiPlugin::default();
        let output = ask_question_tool(&plugin)
            .invoke(ask_question_call(), test_ctx())
            .await
            .expect("ask_question never returns a ToolError for a well-formed call");
        assert!(
            !output.is_error,
            "no interactive surface must degrade the reply, not fail the tool call"
        );
        let text = text_of(&output);
        assert!(
            text.starts_with("no answer available"),
            "expected the same degrade-sentence shape skeleton_ask uses, got: {text}"
        );
        assert!(
            text.contains("no interactive surface is available"),
            "expected the tool's own no-surface wording, got: {text}"
        );
    }

    /// A malformed call (`options` empty) is a genuine tool error -- a
    /// caller mistake, never a "nobody could answer it" degrade.
    #[tokio::test]
    async fn ask_question_refuses_an_empty_options_list_as_invalid_arguments() {
        struct PanicsIfCalled;
        #[async_trait]
        impl FormSurface for PanicsIfCalled {
            async fn ask_select(
                &self,
                _request: AskSelectRequest,
            ) -> Result<AskSelectAnswer, FormSurfaceError> {
                panic!("must not be reached for an empty-options call");
            }
        }
        let plugin = ConwayUiPlugin::new(Some(Arc::new(PanicsIfCalled)));
        let call = ToolCall {
            call_id: "call-1".to_string(),
            name: ToolName::new(ASK_QUESTION_TOOL_NAME),
            arguments: serde_json::json!({ "prompt": "proceed?", "options": [] }),
        };
        let err = ask_question_tool(&plugin)
            .invoke(call, test_ctx())
            .await
            .expect_err("empty options must be a ToolError, not a degraded reply");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    /// The `description()` claim's `costs` text depends on whether a live
    /// surface is actually wired into THIS instance -- proven by
    /// constructing both shapes and asserting the wording differs, not
    /// merely that each is non-empty.
    #[test]
    fn description_costs_reflects_whether_a_live_surface_is_wired_in() {
        let with_surface = ConwayUiPlugin::new(Some(Arc::new(FixedAnswerSurface {
            answer: "yes".to_string(),
        })));
        let without_surface = ConwayUiPlugin::default();
        assert!(with_surface.description().costs.contains("blocks until"));
        assert!(without_surface
            .description()
            .costs
            .contains("no live drawing surface"));
    }
}
