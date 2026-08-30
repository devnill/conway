//! Board item `01M0WWPA70E8YAAN981EK10D3D`: `skeleton_ask`
//! (`conway_plugin_skeleton::ASK_TOOL_NAME`) driven against a REAL
//! `conway_plugin_ui::ConwayUiPlugin` registration -- not a hand-rolled
//! fixture provider defined in this test file, which would only prove this
//! crate's own JSON-shape assumptions agree with themselves. This is what
//! makes this file "a real consumer exercising the capability end to end"
//! (that item's own acceptance 2), as opposed to `conway-plugin-ui`'s own
//! unit tests, which prove the PROVIDER side alone against a fixture
//! `FormSurface`.
//!
//! Three cases, matching the three outcomes `SkeletonAskTool::invoke`'s own
//! doc names as equally ordinary: `conway.ui` installed with a surface that
//! answers, `conway.ui` not installed at all, and `conway.ui` installed with
//! no drawing surface (`ConwayUiPlugin::default()` -- the exact construction
//! every shipped call site uses today). All three must leave
//! `ToolOutput::is_error` `false`: none of them is a tool failure.

use std::sync::Arc;

use conway::plugin::{
    async_trait, CapabilityCallHandle, CapabilityRegistry, ContentBlock, Plugin as _, Tool as _,
    ToolCall, ToolCtx, ToolOutput,
};
use conway::AgentId;
use conway_plugin_skeleton::{SkeletonPlugin, ASK_TOOL_NAME, PLUGIN_ID};
use conway_plugin_ui::{AskSelectAnswer, AskSelectRequest, ConwayUiPlugin, FormSurface, FormSurfaceError};
use conway_testkit::{CollectingEventSink, FakeSubagentHost};

/// A [`FormSurface`] that always answers with a fixed choice -- the SAME
/// role `conway-plugin-ui`'s own `FixedAnswerSurface` fixture plays one
/// crate over; kept separate rather than shared, since each crate's own
/// test suite compiles independently.
struct FixedAnswerSurface {
    answer: String,
}

#[async_trait]
impl FormSurface for FixedAnswerSurface {
    async fn ask_select(
        &self,
        _request: AskSelectRequest,
    ) -> Result<AskSelectAnswer, FormSurfaceError> {
        Ok(AskSelectAnswer {
            selected: self.answer.clone(),
        })
    }
}

fn ctx_with_capabilities(handle: CapabilityCallHandle) -> ToolCtx {
    let agent_id = AgentId::new();
    ToolCtx {
        capabilities: handle,
        ..ToolCtx::for_test(
            agent_id,
            std::env::temp_dir(),
            Arc::new(FakeSubagentHost::new(agent_id)),
            Arc::new(CollectingEventSink::new()),
        )
    }
}

fn ask_tool() -> Arc<dyn conway::plugin::Tool> {
    SkeletonPlugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name.as_str() == ASK_TOOL_NAME)
        .expect("SkeletonPlugin declares skeleton_ask")
}

fn call() -> ToolCall {
    ToolCall {
        call_id: "call-1".to_string(),
        name: conway::ToolName::new(ASK_TOOL_NAME),
        arguments: serde_json::json!({}),
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
        .expect("skeleton_ask always replies with a text block")
}

/// **VERIFICATION ANCHOR (acceptance 2, first half).** `conway.ui`
/// installed with a surface that answers -> `skeleton_ask` reports the
/// REAL answer that surface produced, round-tripped through the real
/// `CapabilityRegistry`/`CapabilityCallHandle` machinery -- if
/// `SkeletonAskTool` sent a payload `ConwayUiPlugin`'s own provider could
/// not parse, or read the answer back from the wrong field, this would fail
/// rather than pass on a coincidence.
#[tokio::test]
async fn skeleton_ask_reports_the_real_answer_when_conway_ui_can_answer() {
    let ui_plugin = ConwayUiPlugin::new(Some(Arc::new(FixedAnswerSurface {
        answer: "yes".to_string(),
    })));
    let registry = CapabilityRegistry::from_registrations(ui_plugin.capabilities())
        .expect("conway.ui registers exactly one capability");
    let handle = CapabilityCallHandle::new(Arc::new(registry), PLUGIN_ID);

    let output = ask_tool()
        .invoke(call(), ctx_with_capabilities(handle))
        .await
        .expect("skeleton_ask never returns a ToolError");

    assert!(!output.is_error);
    assert_eq!(text_of(&output), "skeleton ask: answered 'yes'");
}

/// **VERIFICATION ANCHOR (acceptance 3, unit-level half).** `conway.ui` not
/// installed at all (`CapabilityCallHandle::noop`, the SAME refuse-with-
/// `NotProvided` shape a build with no `ui.form` provider actually produces)
/// -> the call degrades, never fails.
#[tokio::test]
async fn skeleton_ask_degrades_without_failing_when_conway_ui_is_not_installed() {
    let handle = CapabilityCallHandle::noop(PLUGIN_ID);

    let output = ask_tool()
        .invoke(call(), ctx_with_capabilities(handle))
        .await
        .expect("skeleton_ask never returns a ToolError");

    assert!(
        !output.is_error,
        "a missing capability must degrade the reply, not fail the tool call"
    );
    let text = text_of(&output);
    assert!(
        text.starts_with("skeleton ask: no answer available"),
        "got: {text}"
    );
}

/// **VERIFICATION ANCHOR (acceptance 3, unit-level half).** `conway.ui`
/// installed, but with no drawing surface wired in -- `ConwayUiPlugin::
/// default()`, the exact construction every shipped call site
/// (`crates/conway-cli/src/first_party_plugins.rs`) uses today. The
/// compiled-binary sibling of this test
/// (`crates/conway-cli/tests/ui_form_degrades_under_one_shot.rs`) drives the
/// identical shape through the real `conway -p` binary; this one pins the
/// same behavior at the tool level, independent of the CLI's own wiring.
#[tokio::test]
async fn skeleton_ask_degrades_when_conway_ui_installs_with_no_drawing_surface() {
    let ui_plugin = ConwayUiPlugin::default();
    let registry = CapabilityRegistry::from_registrations(ui_plugin.capabilities())
        .expect("conway.ui registers exactly one capability");
    let handle = CapabilityCallHandle::new(Arc::new(registry), PLUGIN_ID);

    let output = ask_tool()
        .invoke(call(), ctx_with_capabilities(handle))
        .await
        .expect("skeleton_ask never returns a ToolError");

    assert!(!output.is_error);
    let text = text_of(&output);
    assert!(
        text.starts_with("skeleton ask: no answer available"),
        "got: {text}"
    );
}
