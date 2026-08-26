//! Edge B (`docs/vision/DESIGN-plugin-dependencies.md` §2): the plugin ->
//! plugin capability CALL channel -- request/response, addressed by a
//! namespaced [`super::HostCapability`] name, dynamic and JSON-serialisable
//! rather than a Rust trait shaped around any one capability's own methods.
//!
//! **Why this is a distinct channel from [`super::PluginEventHandle`], not a
//! reuse of it.** `PluginEventHandle`/`PluginEventEmitter` are emit-only,
//! fire-and-forget: a plugin fires an event and gets nothing back, and zero
//! or many hooks may react -- the *push* shape design §7c names. A
//! capability call is the opposite shape: exactly one provider, one
//! request, one answer (or one error) -- a blocking call, the *pull* shape
//! that same section names. Conflating the two would either force every
//! event subscriber to answer (breaking the "an observer changes nothing"
//! posture `docs/plugins/compatibility.md` already rests its degrade rule
//! on) or force a capability call to tolerate zero-or-many answers (making
//! "the" answer ambiguous). Design §7c leaves whether the push case
//! (`PluginStatusContribution`, the half-built push mechanism design §1
//! records) should ever share machinery with this one explicitly
//! UNRESOLVED -- this module builds pull only, and settles nothing about
//! push.
//!
//! **Dynamic and serialisable, not a typed Rust trait per capability.**
//! conway's plugin tier includes out-of-process plugins speaking a wire
//! protocol (`conway-plugin-subprocess`, `conway-plugin-mcp`); a
//! capability-specific Rust trait -- `trait UiPlugin { fn checkbox(&self,
//! ...) -> ...; }` -- is unimplementable by them, exactly the defect design
//! §2 names for that shape and rejects. [`CapabilityProvider::call`] and
//! [`CapabilityCallHandle::call`] both take and return `serde_json::Value`
//! -- the SAME shape [`super::Tool::invoke`]'s `ToolCall::arguments` /
//! `ToolOutput` content already crosses a wire in (`conway-plugin-
//! subprocess`'s `WireTool` / `RawToolResult`), so a provider that forwards
//! over an out-of-process wire is exactly as reachable through this trait
//! as an in-process one is: a host-side proxy object implementing
//! [`CapabilityProvider`] by serialising `payload`, writing it across its
//! own transport, and parsing the JSON answer back is the SAME shape
//! `SubprocessTool` already is for [`super::Tool`]. Nothing about
//! [`CapabilityProvider`] privileges an in-process implementor over one
//! that does exactly that.
//!
//! **What this module does NOT build.** No capability is registered here --
//! no `conway.ui`, no `ui.form/1`. This is the channel only; see
//! `docs/vision/DESIGN-plugin-dependencies.md` for what is deliberately out
//! of scope. The STATIC "does anything installed provide this name" check
//! (the `requires`/`optional`-vs-`provides` closed-vocabulary gate) lives in
//! `crates/conway/src/builder.rs`, not here -- it operates over plain
//! `PluginManifest`/`Plugin::capabilities()` data at `ConwayBuilder::build`
//! time and needs none of this module's runtime dispatch machinery.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::HostCapability;

/// An application-level failure a [`CapabilityProvider`] returns from its
/// own [`CapabilityProvider::call`] -- distinct from [`CapabilityCallError`],
/// which additionally covers "nothing is registered for this name at all",
/// a case a provider itself never observes (only
/// [`CapabilityRegistry::call`] does, before a request ever reaches a
/// provider).
///
/// Plain data, `Serialize`/`Deserialize` -- deliberately, the same reason
/// `conway-plugin-subprocess`'s `WireToolError` is: an out-of-process
/// provider constructs this from whatever its own wire answer carries, so
/// it must be a shape a non-Rust implementation can actually produce, never
/// a Rust-only error type only an in-process provider could construct.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CapabilityError {
    /// A short, human-readable failure reason -- logged/traced, and shown
    /// verbatim by a caller with nowhere more specific to put it.
    pub message: String,
    /// Arbitrary structured detail a provider wants a caller able to branch
    /// on programmatically. `Value::Null` (this type's implicit default via
    /// [`Self::new`]) when a provider has nothing beyond `message`.
    #[serde(default)]
    pub detail: serde_json::Value,
}

impl CapabilityError {
    /// Builds a [`CapabilityError`] carrying `message` and no structured
    /// detail (`detail: Value::Null`) -- the common case for a fixture
    /// provider, and for a first real provider that has not yet found a
    /// reason to attach structured detail.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            detail: serde_json::Value::Null,
        }
    }

    /// Builds a [`CapabilityError`] carrying both `message` and structured
    /// `detail` a caller can branch on without string-matching `message`.
    pub fn with_detail(message: impl Into<String>, detail: serde_json::Value) -> Self {
        Self {
            message: message.into(),
            detail,
        }
    }
}

/// One capability a plugin makes callable by ANOTHER plugin -- the runtime
/// half of [`super::Plugin::capabilities`]'s declaration, exactly as
/// [`super::Plugin::tools`] is the runtime half of
/// `PluginManifest::tools`'s static one. Object-safe (no generic
/// parameters, `#[async_trait]`), so a host can hold `Arc<dyn
/// CapabilityProvider>` regardless of whether the plugin behind it is
/// in-process or a wire-forwarding proxy for an out-of-process one -- see
/// this module's own doc.
#[async_trait]
pub trait CapabilityProvider: Send + Sync + 'static {
    /// Answers one call with `payload`, or fails with a [`CapabilityError`]
    /// this provider constructs itself.
    ///
    /// PRE: the calling [`CapabilityRegistry`] has already matched this
    /// provider to the requested capability name -- `payload` carries no
    /// name, only whatever request shape this capability itself defines
    /// (undefined by this module; a future capability's own doc states its
    /// own request/response shape, the same way `Tool::spec`'s JSON Schema
    /// states a tool's).
    async fn call(&self, payload: serde_json::Value) -> Result<serde_json::Value, CapabilityError>;
}

/// One [`super::Plugin::capabilities`] entry: the [`HostCapability`] name
/// this plugin is registering a live provider for, paired with the
/// provider itself.
pub struct CapabilityRegistration {
    pub capability: HostCapability,
    pub provider: Arc<dyn CapabilityProvider>,
}

impl std::fmt::Debug for CapabilityRegistration {
    // Manual impl: `Arc<dyn CapabilityProvider>` carries no `Debug` bound --
    // mirrors `PluginEventHandle`'s own manual `Debug` exactly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilityRegistration")
            .field("capability", &self.capability)
            .field("provider", &"<dyn CapabilityProvider>")
            .finish()
    }
}

/// A capability call's outcome when it does NOT reach a provider's own
/// [`CapabilityProvider::call`] at all, plus the wrapped [`CapabilityError`]
/// when it does but the provider itself refused. Plain data (`Clone`,
/// `PartialEq`) a caller can match on and log -- no wrapped trait object.
#[derive(Clone, Debug, PartialEq)]
pub enum CapabilityCallError {
    /// `capability` is not a well-formed namespaced name -- the SAME shape
    /// [`HostCapability::named`] enforces, checked by
    /// [`CapabilityCallHandle::call`] before a registry lookup is even
    /// attempted, so a typo can never be silently indistinguishable from
    /// "no one provides this" ([`Self::NotProvided`]).
    MalformedName { capability: String, reason: String },
    /// No installed plugin registered a runtime [`CapabilityProvider`] for
    /// `capability`. This is the RUNTIME counterpart of the STATIC
    /// `missing_required_dependency`/`missing_optional_dependencies` check
    /// `crates/conway/src/builder.rs` runs at `ConwayBuilder::build` --
    /// that check, not this variant, is what should stop a `requires`-tier
    /// consumer from ever reaching a live call that produces this in the
    /// first place; a `optional`-tier consumer reaching this at runtime is
    /// the ordinary, expected degrade-and-decide-for-yourself case.
    NotProvided { capability: String },
    /// The registered provider itself answered with a [`CapabilityError`]
    /// -- the channel's own failure path, distinct from "nothing is even
    /// listening" above.
    Provider {
        capability: String,
        error: CapabilityError,
    },
}

impl std::fmt::Display for CapabilityCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapabilityCallError::MalformedName { capability, reason } => {
                write!(f, "malformed capability name '{capability}': {reason}")
            }
            CapabilityCallError::NotProvided { capability } => {
                write!(f, "no installed plugin provides capability '{capability}'")
            }
            CapabilityCallError::Provider { capability, error } => {
                write!(
                    f,
                    "capability '{capability}' provider failed: {}",
                    error.message
                )
            }
        }
    }
}

impl std::error::Error for CapabilityCallError {}

/// The host-side dispatcher [`CapabilityCallHandle`] calls through --
/// object-safe (no generic parameters, `#[async_trait]`), so a caller can
/// hold `Arc<dyn CapabilityHost>` regardless of which concrete registry
/// backs it. [`CapabilityRegistry`] is the one production implementation
/// this crate ships; a test may supply its own fixture instead (the SAME
/// "a port, and one production fallback, and a test may substitute its
/// own" shape every other port in this module already uses).
#[async_trait]
pub trait CapabilityHost: Send + Sync + 'static {
    async fn call(
        &self,
        capability: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, CapabilityCallError>;
}

/// Two [`CapabilityRegistration`]s declaring the SAME capability name --
/// refused at [`CapabilityRegistry::from_registrations`] construction, fail
/// closed, never "last one wins". A silently shadowed provider is exactly
/// the unreachable-but-installed defect
/// `docs/vision/DESIGN-plugin-dependencies.md` §1 documents one layer up;
/// resolving the ambiguity by iteration order would reintroduce it here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateCapabilityProvider {
    pub capability: String,
}

impl std::fmt::Display for DuplicateCapabilityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "more than one installed plugin registered a provider for capability '{}'",
            self.capability
        )
    }
}

impl std::error::Error for DuplicateCapabilityProvider {}

/// The in-memory `capability name -> provider` lookup, built once from
/// every installed plugin's [`super::Plugin::capabilities`] and consulted
/// by every [`CapabilityCallHandle::call`] for the lifetime of a build --
/// the same "build once, query many, no rebuild-on-every-call" shape
/// `crate::ports::capability_index::CapabilityIndex` establishes for an
/// unrelated lookup (`(backend, model) -> Capabilities`).
#[derive(Clone, Default)]
pub struct CapabilityRegistry {
    providers: HashMap<String, Arc<dyn CapabilityProvider>>,
}

// A manual `Debug` rather than a derive: `Arc<dyn CapabilityProvider>` is a
// trait object and cannot be `Debug` without adding that bound to the trait,
// which would exclude out-of-process implementors that have nothing useful to
// print. The registry's *keys* are the part a reader wants anyway, and they
// are sorted so the output is stable across runs -- `HashMap` iteration order
// is not.
impl std::fmt::Debug for CapabilityRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names: Vec<&str> = self.providers.keys().map(String::as_str).collect();
        names.sort_unstable();
        f.debug_struct("CapabilityRegistry")
            .field("capabilities", &names)
            .finish()
    }
}

impl CapabilityRegistry {
    /// Builds a registry from every installed plugin's capability
    /// registrations, keyed by [`HostCapability::as_wire_str`]. Refuses
    /// (rather than silently keeping the first or last) when two
    /// registrations share a capability name -- see
    /// [`DuplicateCapabilityProvider`]'s own doc.
    pub fn from_registrations(
        registrations: impl IntoIterator<Item = CapabilityRegistration>,
    ) -> Result<Self, DuplicateCapabilityProvider> {
        let mut providers: HashMap<String, Arc<dyn CapabilityProvider>> = HashMap::new();
        for registration in registrations {
            let key = registration.capability.as_wire_str().to_string();
            if providers
                .insert(key.clone(), registration.provider)
                .is_some()
            {
                return Err(DuplicateCapabilityProvider { capability: key });
            }
        }
        Ok(Self { providers })
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[async_trait]
impl CapabilityHost for CapabilityRegistry {
    async fn call(
        &self,
        capability: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, CapabilityCallError> {
        match self.providers.get(capability) {
            Some(provider) => {
                provider
                    .call(payload)
                    .await
                    .map_err(|error| CapabilityCallError::Provider {
                        capability: capability.to_string(),
                        error,
                    })
            }
            None => Err(CapabilityCallError::NotProvided {
                capability: capability.to_string(),
            }),
        }
    }
}

/// A [`CapabilityHost`] bound to ONE calling plugin -- the `ToolCtx`-facing
/// capability a tool's own [`super::Tool::invoke`] gets, in place of a raw
/// `Arc<dyn CapabilityHost>`. Mirrors [`super::PluginEventHandle`]'s own
/// shape: [`Self::caller_plugin_id`] is baked in for tracing/audit
/// provenance (which plugin issued this call), never for authorization --
/// nothing here restricts WHICH capability a caller may name, unlike
/// `PluginEventHandle::emit`'s namespace-forgery guard, because a
/// capability call is calling INTO another plugin's own declared surface,
/// not asserting this plugin's own namespace.
#[derive(Clone)]
pub struct CapabilityCallHandle {
    host: Arc<dyn CapabilityHost>,
    caller_plugin_id: String,
}

impl std::fmt::Debug for CapabilityCallHandle {
    // Manual impl: `Arc<dyn CapabilityHost>` carries no `Debug` bound --
    // mirrors `PluginEventHandle`'s own manual `Debug` exactly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilityCallHandle")
            .field("caller_plugin_id", &self.caller_plugin_id)
            .field("host", &"<dyn CapabilityHost>")
            .finish()
    }
}

impl CapabilityCallHandle {
    /// Wraps `host`, baking `caller_plugin_id` in for tracing/audit
    /// provenance -- see this type's own doc for why that is NOT an
    /// authorization boundary.
    pub fn new(host: Arc<dyn CapabilityHost>, caller_plugin_id: impl Into<String>) -> Self {
        Self {
            host,
            caller_plugin_id: caller_plugin_id.into(),
        }
    }

    /// A handle that refuses every call with
    /// [`CapabilityCallError::NotProvided`] -- the default for a `ToolCtx`
    /// fixture that does not exercise this capability, mirroring
    /// `ContextPathHandle::noop`'s "unconditional, no I/O, refuses rather
    /// than silently succeeds" shape (that type's own doc: a default here
    /// may not perform I/O, and a refusal performs none). A test that DOES
    /// exercise a capability call supplies a real [`CapabilityHost`]
    /// instead (typically a [`CapabilityRegistry`] built from one or more
    /// fixture [`CapabilityProvider`]s).
    pub fn noop(caller_plugin_id: impl Into<String>) -> Self {
        Self::new(Arc::new(NoopCapabilityHost), caller_plugin_id)
    }

    /// This handle's own caller plugin id -- tracing/audit provenance only,
    /// see this type's own doc.
    pub fn caller_plugin_id(&self) -> &str {
        &self.caller_plugin_id
    }

    /// Calls `capability` with `payload`. Validates `capability`'s shape
    /// with [`HostCapability::named`] (reusing, not reimplementing, the
    /// SAME `crate::event_name::validate_event_name` check that gates
    /// [`super::HostCapability`] itself and `PluginEventHandle::emit`'s
    /// namespace check -- "one vocabulary, not two") before ever reaching
    /// the underlying [`CapabilityHost`], so a malformed name fails as
    /// [`CapabilityCallError::MalformedName`] rather than a registry
    /// lookup miss indistinguishable from "no one provides this".
    pub async fn call(
        &self,
        capability: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, CapabilityCallError> {
        if let Err(reason) = HostCapability::named(capability) {
            return Err(CapabilityCallError::MalformedName {
                capability: capability.to_string(),
                reason,
            });
        }
        self.host.call(capability, payload).await
    }
}

/// The private implementation behind [`CapabilityCallHandle::noop`]. Not
/// itself exported -- mirrors `ports::context_path`'s own private
/// `NoopContextPathHost` exactly.
struct NoopCapabilityHost;

#[async_trait]
impl CapabilityHost for NoopCapabilityHost {
    async fn call(
        &self,
        capability: &str,
        _payload: serde_json::Value,
    ) -> Result<serde_json::Value, CapabilityCallError> {
        Err(CapabilityCallError::NotProvided {
            capability: capability.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dependency-free async-test helper -- `conway-core` has no `tokio`/
    /// `futures-executor` dependency, even in dev-deps (see `ports::plugin`'s
    /// own copy of this exact helper for the precedent this mirrors). Every
    /// future this module's tests drive either does no real `.await`ing
    /// internally, or (the out-of-process fixture below) does ordinary
    /// BLOCKING `std::process`/`std::io` calls inside an `async fn` body --
    /// still no genuine suspension point a real executor would need to
    /// schedule around -- so a single poll with a no-op waker always
    /// resolves `Ready`. Not a general-purpose executor.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn noop(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);

        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        loop {
            if let Poll::Ready(val) = fut.as_mut().poll(&mut cx) {
                return val;
            }
        }
    }

    static_assertions::assert_impl_all!(CapabilityCallHandle: Send, Sync, Clone);
    static_assertions::assert_impl_all!(CapabilityRegistry: Send, Sync, Clone);
    static_assertions::assert_impl_all!(CapabilityError: Send, Sync, Clone);
    static_assertions::assert_impl_all!(CapabilityCallError: Send, Sync, Clone);

    /// A fixture provider that echoes `payload` back wrapped in
    /// `{"echoed": payload}` -- proves a real call reaches a real provider
    /// and the response is exactly what that provider returned, not some
    /// default.
    struct EchoProvider;

    #[async_trait]
    impl CapabilityProvider for EchoProvider {
        async fn call(
            &self,
            payload: serde_json::Value,
        ) -> Result<serde_json::Value, CapabilityError> {
            Ok(serde_json::json!({ "echoed": payload }))
        }
    }

    /// A fixture provider that always fails -- the untested-failure-path
    /// concern this whole channel's own item doc calls out: "a channel
    /// whose failure path is untested is a channel with an untested half".
    struct AlwaysFailsProvider;

    #[async_trait]
    impl CapabilityProvider for AlwaysFailsProvider {
        async fn call(
            &self,
            _payload: serde_json::Value,
        ) -> Result<serde_json::Value, CapabilityError> {
            Err(CapabilityError::with_detail(
                "acme.fixture.fail always fails",
                serde_json::json!({ "code": "always_fails" }),
            ))
        }
    }

    fn registry_with(
        capability: &str,
        provider: Arc<dyn CapabilityProvider>,
    ) -> CapabilityRegistry {
        CapabilityRegistry::from_registrations([CapabilityRegistration {
            capability: HostCapability::named(capability).unwrap(),
            provider,
        }])
        .unwrap()
    }

    #[test]
    fn a_real_call_reaches_the_registered_provider_and_returns_its_answer() {
        let registry = registry_with("acme.fixture.echo", Arc::new(EchoProvider));
        let handle = CapabilityCallHandle::new(Arc::new(registry), "acme.consumer");
        let answer = block_on(handle.call("acme.fixture.echo", serde_json::json!({ "n": 1 })))
            .expect("echo provider answers");
        assert_eq!(answer, serde_json::json!({ "echoed": { "n": 1 } }));
    }

    #[test]
    fn an_error_returned_by_the_provider_surfaces_as_capabilitycallerror_provider() {
        let registry = registry_with("acme.fixture.fail", Arc::new(AlwaysFailsProvider));
        let handle = CapabilityCallHandle::new(Arc::new(registry), "acme.consumer");
        let err = block_on(handle.call("acme.fixture.fail", serde_json::json!({})))
            .expect_err("AlwaysFailsProvider must fail every call");
        match err {
            CapabilityCallError::Provider { capability, error } => {
                assert_eq!(capability, "acme.fixture.fail");
                assert_eq!(error.message, "acme.fixture.fail always fails");
                assert_eq!(error.detail, serde_json::json!({ "code": "always_fails" }));
            }
            other => panic!("expected Provider, got {other:?}"),
        }
    }

    #[test]
    fn calling_an_unregistered_capability_is_not_provided_not_a_panic() {
        let registry = CapabilityRegistry::default();
        let handle = CapabilityCallHandle::new(Arc::new(registry), "acme.consumer");
        let err = block_on(handle.call("acme.nobody.home", serde_json::json!(null)))
            .expect_err("nothing registered this capability");
        assert_eq!(
            err,
            CapabilityCallError::NotProvided {
                capability: "acme.nobody.home".to_string()
            }
        );
    }

    #[test]
    fn a_malformed_capability_name_fails_before_ever_reaching_the_host() {
        let handle = CapabilityCallHandle::new(Arc::new(CapabilityRegistry::default()), "acme");
        let err = block_on(handle.call(".bad", serde_json::json!(null)))
            .expect_err("a leading separator is malformed");
        assert!(matches!(err, CapabilityCallError::MalformedName { .. }));
    }

    #[test]
    fn noop_handle_refuses_every_call_with_no_io() {
        let handle = CapabilityCallHandle::noop("acme");
        let err = block_on(handle.call("acme.anything", serde_json::json!(null)))
            .expect_err("noop always refuses");
        assert!(matches!(err, CapabilityCallError::NotProvided { .. }));
    }

    #[test]
    fn duplicate_provider_registration_is_refused_not_last_one_wins() {
        let err = CapabilityRegistry::from_registrations([
            CapabilityRegistration {
                capability: HostCapability::named("acme.dup").unwrap(),
                provider: Arc::new(EchoProvider),
            },
            CapabilityRegistration {
                capability: HostCapability::named("acme.dup").unwrap(),
                provider: Arc::new(AlwaysFailsProvider),
            },
        ])
        .expect_err("two providers for the same capability name must be refused");
        assert_eq!(err.capability, "acme.dup");
    }

    #[test]
    fn an_out_of_process_fixture_provider_reaches_the_channel_on_identical_terms() {
        // A `CapabilityProvider` that speaks to a REAL child process over its
        // stdin/stdout, exactly the shape `conway-plugin-subprocess`'s own
        // `SubprocessTool` already is for `Tool::invoke` (serialize a JSON
        // request, write it across a real OS pipe, read and parse a JSON
        // response back) -- proving this channel does not privilege an
        // in-process implementor: the SAME `CapabilityCallHandle::call` call
        // site above, unchanged, reaches this provider too.
        //
        // `cat` is used deliberately (not a purpose-built helper binary --
        // this test cannot assume one is built) as the child: it echoes
        // stdin to stdout verbatim, so a correct answer proves bytes made a
        // full round trip through a genuine separate process, not that the
        // provider merely returned its input without ever touching the
        // pipe -- this provider wraps the echoed bytes in `{"child_echoed":
        // ...}` on the RUST side, only after reading them back from `cat`,
        // so a test double that skipped the subprocess entirely could not
        // produce this exact shape by accident.
        struct SubprocessEchoProvider;

        #[async_trait]
        impl CapabilityProvider for SubprocessEchoProvider {
            async fn call(
                &self,
                payload: serde_json::Value,
            ) -> Result<serde_json::Value, CapabilityError> {
                use std::io::Write;
                use std::process::Stdio;

                let mut child = std::process::Command::new("cat")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .map_err(|e| CapabilityError::new(format!("spawn cat: {e}")))?;

                let request = serde_json::to_vec(&payload)
                    .map_err(|e| CapabilityError::new(e.to_string()))?;
                child
                    .stdin
                    .take()
                    .expect("piped stdin")
                    .write_all(&request)
                    .map_err(|e| CapabilityError::new(format!("write to cat: {e}")))?;

                let output = child
                    .wait_with_output()
                    .map_err(|e| CapabilityError::new(format!("wait for cat: {e}")))?;
                if !output.status.success() {
                    return Err(CapabilityError::new("cat exited non-zero"));
                }
                let echoed: serde_json::Value = serde_json::from_slice(&output.stdout)
                    .map_err(|e| CapabilityError::new(format!("parse cat's echo: {e}")))?;
                Ok(serde_json::json!({ "child_echoed": echoed }))
            }
        }

        let registry = registry_with(
            "acme.fixture.subprocess_echo",
            Arc::new(SubprocessEchoProvider),
        );
        let handle = CapabilityCallHandle::new(Arc::new(registry), "acme.consumer");
        let answer = block_on(handle.call(
            "acme.fixture.subprocess_echo",
            serde_json::json!({ "greeting": "hello from the wire" }),
        ))
        .expect("a real child process answers through the same channel");
        assert_eq!(
            answer,
            serde_json::json!({ "child_echoed": { "greeting": "hello from the wire" } })
        );
    }
}
