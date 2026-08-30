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
//!
//! **Versioning (decision `01M189XS6Z9VKYENAHNY1B54CM`, which supersedes
//! `01M1893Q2DV773ZQ5B138W6G07` on mechanism only -- that item's argument
//! for versioning edges AT ALL still governs).** A capability edge carries
//! a version, expressed in standard semver via the `semver` crate, not a
//! bespoke notation and not a version folded into the name string:
//! [`CapabilityRegistration::version`] is a [`semver::Version`] the
//! PROVIDER declares as a field separate from its
//! [`super::HostCapability`] name (`ui.form` stays `ui.form`; `1.0.0` is
//! this field); [`CapabilityCallHandle::call_versioned`] takes a
//! [`semver::VersionReq`] the CONSUMER supplies at the call site (`^1` for
//! the ordinary floor, `=1.2.3` for a hard pin -- `VersionReq` gives pinning
//! for free, which is why one type covers both). Resolution is
//! `req.matches(&version)`; a mismatch is refused
//! ([`CapabilityCallError::VersionMismatch`]), never degraded, naming both
//! the requirement and the version actually installed -- the same
//! "not degraded, not silently auto-installed -- refused" posture
//! `docs/vision/DESIGN-plugin-dependencies.md` §0 ruling 3 already states
//! for a missing dependency, applied here to a present-but-incompatible
//! one. See `docs/vision/DESIGN-plugin-dependencies.md` §7b/§9 for the
//! full argument, including why this needs no resolver: one capability
//! name has exactly one provider ([`DuplicateCapabilityProvider`] refuses a
//! second registration at construction), so there is no candidate set to
//! select among and nothing to backtrack over -- `VersionReq::matches` is a
//! predicate over a single pair, not a search.

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
    /// This registration's declared version, standard semver
    /// (`crate::ports::capability`'s own module doc: decision
    /// `01M189XS6Z9VKYENAHNY1B54CM`) -- a field separate from `capability`,
    /// never folded into the name string (`ui.form` at `1.0.0`, not
    /// `"ui.form/1.0.0"`). Checked against a caller's own
    /// `semver::VersionReq` by [`CapabilityCallHandle::call_versioned`];
    /// [`CapabilityCallHandle::call`] (unversioned) ignores this field
    /// entirely, exactly as it always has.
    pub version: semver::Version,
    /// Whether [`Self::version`] is an author-declared value this
    /// registration's own caller supplied, or
    /// [`Self::from_declared_version_or_unversioned`]'s own `0.0.0`
    /// fallback for a declaration that was absent or did not parse as
    /// semver. [`Self::new`] always sets this `true` -- a hand-written
    /// literal that failed to parse panics before this field is ever
    /// assigned, so every `CapabilityRegistration` built that way carries a
    /// REAL declared version by construction. Consulted by
    /// [`CapabilityCallHandle::call_versioned`] so a
    /// [`CapabilityCallError::VersionMismatch`] can say WHICH kind of
    /// mismatch this is, rather than let the `0.0.0` fallback read as a
    /// provider that declared and shipped that exact version (see that
    /// variant's own doc for the concrete confusion this heads off).
    pub version_declared: bool,
    pub provider: Arc<dyn CapabilityProvider>,
}

impl CapabilityRegistration {
    /// Builds a registration from a version LITERAL (`"1.0.0"`), for a
    /// caller that has a hard-coded version string in its own source and
    /// would otherwise need to add `semver` to its own `Cargo.toml` merely
    /// to spell `semver::Version::new(1, 0, 0)` -- most workspace crates
    /// constructing a fixture `CapabilityRegistration` are in exactly that
    /// position; only `conway-core` (this field's own home) needs `semver`
    /// as a direct dependency (C-04, and the zero-`Cargo.lock`-diff premise
    /// this item's own acceptance criteria rest on).
    ///
    /// **Panics on a malformed literal.** This constructor is for a version
    /// written BY HAND in source -- a programmer error if it does not
    /// parse, exactly like an invalid literal in any other `::new`. It must
    /// never be used on an operator- or plugin-supplied string: that caller
    /// has untrusted input and must call `semver::Version::parse` itself
    /// and handle the `Err` (P-10) -- see
    /// [`Self::from_declared_version_or_unversioned`] for that case.
    pub fn new(
        capability: HostCapability,
        version: &str,
        provider: Arc<dyn CapabilityProvider>,
    ) -> Self {
        Self {
            capability,
            version: semver::Version::parse(version).unwrap_or_else(|e| {
                panic!("CapabilityRegistration::new: malformed semver literal '{version}': {e}")
            }),
            version_declared: true,
            provider,
        }
    }

    /// Builds a registration from a version string this caller did NOT
    /// write by hand -- `declared_version` is relayed from somewhere else
    /// (an out-of-process plugin's own manifest, for instance), so it is
    /// untrusted and may not be valid semver at all
    /// (`PluginManifest::version`'s own doc: "a bare string", never
    /// guaranteed semver). A malformed or absent version degrades to
    /// `0.0.0` rather than panicking or refusing registration outright
    /// (P-10: untrusted input maps to a typed, in-range value, never a
    /// panic) -- `0.0.0` satisfies no requirement with a non-zero major
    /// version (`VersionReq::parse("^1")` never matches `0.0.0`), so a
    /// provider that has not adopted semver for this capability can still
    /// register, but a REAL version requirement against it refuses rather
    /// than silently matching by accident of the default chosen here.
    pub fn from_declared_version_or_unversioned(
        capability: HostCapability,
        declared_version: &str,
        provider: Arc<dyn CapabilityProvider>,
    ) -> Self {
        let (version, version_declared) = match semver::Version::parse(declared_version) {
            Ok(version) => (version, true),
            Err(_) => (semver::Version::new(0, 0, 0), false),
        };
        Self {
            capability,
            version,
            version_declared,
            provider,
        }
    }
}

impl std::fmt::Debug for CapabilityRegistration {
    // Manual impl: `Arc<dyn CapabilityProvider>` carries no `Debug` bound --
    // mirrors `PluginEventHandle`'s own manual `Debug` exactly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilityRegistration")
            .field("capability", &self.capability)
            .field("version", &self.version)
            .field("version_declared", &self.version_declared)
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
    /// Something registered `capability`, but its declared
    /// [`CapabilityRegistration::version`] does not satisfy the caller's
    /// own `semver::VersionReq` -- [`CapabilityCallHandle::call_versioned`]'s
    /// own failure path, checked BEFORE the call ever reaches
    /// [`CapabilityProvider::call`] (this module's doc: decision
    /// `01M189XS6Z9VKYENAHNY1B54CM`, "not degraded, not silently
    /// auto-installed -- refused", applied to a present-but-incompatible
    /// version rather than a missing dependency). Distinct from
    /// [`Self::NotProvided`]: someone DOES provide `capability`, just not
    /// at a version `required` accepts -- naming both `required` and
    /// `available` is the whole point of this variant existing separately
    /// from a bare "no match" boolean.
    ///
    /// **`available_declared` distinguishes two different failures that
    /// would otherwise both print `available` as if it were a real shipped
    /// version.** When `true`, `available` is what the provider actually
    /// declared. When `false`, `available` is
    /// [`CapabilityRegistration::from_declared_version_or_unversioned`]'s
    /// own `0.0.0` fallback for a declaration that was absent or did not
    /// parse as semver (`PluginManifest::version`'s own contract: "a bare
    /// string", never guaranteed semver) -- WITHOUT this field, `required:
    /// '^1', available: '0.0.0'` reads as a provider that regressed to
    /// version zero, when the truth is it never declared a parseable
    /// version at all; conflating those two is exactly the
    /// "plausible-looking dead end" a misleading error produces (this
    /// module's own item history: the tilde-formatting bug this same
    /// cycle fixed for the same reason).
    VersionMismatch {
        capability: String,
        required: semver::VersionReq,
        available: semver::Version,
        available_declared: bool,
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
            CapabilityCallError::VersionMismatch {
                capability,
                required,
                available,
                available_declared: true,
            } => {
                write!(
                    f,
                    "capability '{capability}' requires version '{required}', but the \
                     installed provider offers '{available}'"
                )
            }
            CapabilityCallError::VersionMismatch {
                capability,
                required,
                available_declared: false,
                ..
            } => {
                write!(
                    f,
                    "capability '{capability}' requires version '{required}', but the \
                     installed provider declared no usable version (its declared \
                     version was absent or did not parse as semver, so it is treated \
                     as unversioned and satisfies no non-zero version requirement)"
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

    /// The registered provider's declared [`CapabilityRegistration::version`]
    /// for `capability`, or `None` when nothing is registered for that name
    /// -- [`CapabilityCallHandle::call_versioned`]'s own lookup, split out
    /// from [`Self::call`] so a version requirement can be checked BEFORE a
    /// call is ever dispatched. A plain `fn`, not `async`: every production
    /// implementation ([`CapabilityRegistry`]) answers this from an
    /// in-memory map populated once at construction, the same reason
    /// [`CapabilityRegistry::len`]/[`CapabilityRegistry::is_empty`] are
    /// synchronous too; nothing about "what version did this provider
    /// declare at registration" requires I/O.
    fn provided_version(&self, capability: &str) -> Option<semver::Version>;

    /// Whether [`Self::provided_version`]'s answer (when `Some`) is an
    /// author-declared version, or
    /// [`CapabilityRegistration::from_declared_version_or_unversioned`]'s
    /// own `0.0.0` fallback -- see
    /// [`CapabilityCallError::VersionMismatch`]'s own doc for why this
    /// distinction exists and what conflating it would misread as.
    /// Meaningless, and never consulted, when [`Self::provided_version`]
    /// answers `None`.
    fn version_is_declared(&self, capability: &str) -> bool;
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
    /// Each registered provider's own declared [`CapabilityRegistration::
    /// version`], keyed by the SAME capability name `providers` is keyed by
    /// -- a second map rather than a `(provider, version)` tuple in
    /// `providers` itself, so [`Self::version_of`]/[`Self::provided_version`]
    /// can answer without touching the trait-object map at all.
    versions: HashMap<String, semver::Version>,
    /// Each registration's own [`CapabilityRegistration::version_declared`],
    /// keyed the same way as `versions` -- backs
    /// [`CapabilityHost::version_is_declared`].
    version_declared: HashMap<String, bool>,
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
        let mut versions: HashMap<String, semver::Version> = HashMap::new();
        let mut version_declared: HashMap<String, bool> = HashMap::new();
        for registration in registrations {
            let key = registration.capability.as_wire_str().to_string();
            if providers
                .insert(key.clone(), registration.provider)
                .is_some()
            {
                return Err(DuplicateCapabilityProvider { capability: key });
            }
            versions.insert(key.clone(), registration.version);
            version_declared.insert(key, registration.version_declared);
        }
        Ok(Self {
            providers,
            versions,
            version_declared,
        })
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// The registered provider's declared version for `capability`, or
    /// `None` when nothing is registered for that name -- the same answer
    /// [`CapabilityHost::provided_version`] gives, exposed directly on this
    /// concrete type for a caller that already holds a `CapabilityRegistry`
    /// rather than a `dyn CapabilityHost`.
    pub fn version_of(&self, capability: &str) -> Option<&semver::Version> {
        self.versions.get(capability)
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

    fn provided_version(&self, capability: &str) -> Option<semver::Version> {
        self.version_of(capability).cloned()
    }

    fn version_is_declared(&self, capability: &str) -> bool {
        self.version_declared.get(capability).copied().unwrap_or(false)
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

    /// Calls `capability` with `payload`, exactly like [`Self::call`], but
    /// first checks `required` (a consumer's own `semver::VersionReq`)
    /// against the registered provider's declared
    /// [`CapabilityRegistration::version`] -- this module's doc: decision
    /// `01M189XS6Z9VKYENAHNY1B54CM`. Resolution is `required.matches(&
    /// available)`: no candidate search, because a capability name has
    /// exactly one provider ([`DuplicateCapabilityProvider`] refuses a
    /// second registration for the same name at construction), so there is
    /// nothing to select among.
    ///
    /// Failure shapes, checked in this order:
    /// 1. A malformed `capability` name -> [`CapabilityCallError::
    ///    MalformedName`], identical to [`Self::call`], before any host
    ///    lookup.
    /// 2. Nothing registered for `capability` at all -> [`CapabilityCallError::
    ///    NotProvided`] -- the SAME variant an unversioned [`Self::call`]
    ///    would reach for the identical reason, so "no version answered
    ///    this call" is never confused with "no one is even listening"
    ///    ([`CapabilityCallError::VersionMismatch`]'s own doc draws this
    ///    same line).
    /// 3. Something registered, but its version does not satisfy
    ///    `required` -> [`CapabilityCallError::VersionMismatch`], naming
    ///    both `required` and the version actually installed, refused
    ///    rather than degraded -- the call never reaches
    ///    [`CapabilityProvider::call`] in this case.
    ///
    /// A satisfied requirement proceeds through the SAME [`Self::call`]
    /// dispatch this handle already had -- one implementation of the
    /// actual call, never a second copy behind the version check.
    ///
    /// **No in-tree caller yet.** This method is reachable -- every
    /// dispatched `Tool::invoke` gets a `ToolCtx::capabilities:
    /// CapabilityCallHandle` that exposes it -- and is exercised by this
    /// module's own tests, but nothing in `conway-runtime`, no built-in
    /// plugin, and no `conway-plugin-subprocess` code calls it today; every
    /// production call site still goes through the unversioned
    /// [`Self::call`]. The intended first consumer is board item
    /// `01M0WWPA70E8YAAN981EK10D3D` (`conway.ui`, which will publish
    /// `ui.form` and is not yet built). Forward-declared ahead of that
    /// consumer deliberately (this method's own doc names why); not yet
    /// wired to one.
    pub async fn call_versioned(
        &self,
        capability: &str,
        required: &semver::VersionReq,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, CapabilityCallError> {
        if let Err(reason) = HostCapability::named(capability) {
            return Err(CapabilityCallError::MalformedName {
                capability: capability.to_string(),
                reason,
            });
        }
        match self.host.provided_version(capability) {
            None => Err(CapabilityCallError::NotProvided {
                capability: capability.to_string(),
            }),
            Some(available) if required.matches(&available) => {
                self.host.call(capability, payload).await
            }
            Some(available) => {
                let available_declared = self.host.version_is_declared(capability);
                Err(CapabilityCallError::VersionMismatch {
                    capability: capability.to_string(),
                    required: required.clone(),
                    available,
                    available_declared,
                })
            }
        }
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

    fn provided_version(&self, _capability: &str) -> Option<semver::Version> {
        None
    }

    fn version_is_declared(&self, _capability: &str) -> bool {
        false
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
        // Version-agnostic tests (everything above [`Self::call`], not
        // [`Self::call_versioned`]) don't care what version is on record,
        // so this fixes one arbitrary value rather than making every
        // pre-existing caller supply its own.
        registry_with_version(capability, semver::Version::new(1, 0, 0), provider)
    }

    /// Like [`registry_with`], but with an explicit declared version -- the
    /// helper the version-resolution tests below use so the version under
    /// test is visible at each call site rather than hidden behind a fixed
    /// default.
    fn registry_with_version(
        capability: &str,
        version: semver::Version,
        provider: Arc<dyn CapabilityProvider>,
    ) -> CapabilityRegistry {
        CapabilityRegistry::from_registrations([CapabilityRegistration {
            capability: HostCapability::named(capability).unwrap(),
            version,
            version_declared: true,
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
                version: semver::Version::new(1, 0, 0),
                version_declared: true,
                provider: Arc::new(EchoProvider),
            },
            CapabilityRegistration {
                capability: HostCapability::named("acme.dup").unwrap(),
                version: semver::Version::new(1, 0, 0),
                version_declared: true,
                provider: Arc::new(AlwaysFailsProvider),
            },
        ])
        .expect_err("two providers for the same capability name must be refused");
        assert_eq!(err.capability, "acme.dup");
    }

    // -- Capability-edge versioning (decision `01M189XS6Z9VKYENAHNY1B54CM`) --

    #[test]
    fn a_satisfied_version_requirement_resolves_and_reaches_the_provider() {
        let registry = registry_with_version(
            "acme.fixture.versioned",
            semver::Version::new(1, 3, 0),
            Arc::new(EchoProvider),
        );
        let handle = CapabilityCallHandle::new(Arc::new(registry), "acme.consumer");
        let required = semver::VersionReq::parse("^1").expect("valid semver req");
        let answer = block_on(handle.call_versioned(
            "acme.fixture.versioned",
            &required,
            serde_json::json!({ "n": 1 }),
        ))
        .expect("^1 is satisfied by an installed 1.3.0 provider");
        assert_eq!(answer, serde_json::json!({ "echoed": { "n": 1 } }));
    }

    #[test]
    fn an_unsatisfied_version_requirement_refuses_naming_the_requirement_and_available() {
        let registry = registry_with_version(
            "acme.fixture.versioned",
            semver::Version::new(2, 0, 0),
            Arc::new(EchoProvider),
        );
        let handle = CapabilityCallHandle::new(Arc::new(registry), "acme.consumer");
        let required = semver::VersionReq::parse("^1").expect("valid semver req");
        let err = block_on(handle.call_versioned(
            "acme.fixture.versioned",
            &required,
            serde_json::json!(null),
        ))
        .expect_err("^1 does not accept an installed 2.0.0 provider");
        match err {
            CapabilityCallError::VersionMismatch {
                capability,
                required: named_required,
                available,
                available_declared,
            } => {
                assert_eq!(capability, "acme.fixture.versioned");
                assert_eq!(named_required, required);
                assert_eq!(available, semver::Version::new(2, 0, 0));
                assert!(
                    available_declared,
                    "registry_with_version registers a real declared version"
                );
            }
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
        // The provider itself must never be reached on a version refusal --
        // if it had been, this would be `Ok`, not `Err`, since
        // `EchoProvider::call` never fails.
    }

    #[test]
    fn an_exact_pin_requirement_accepts_that_version_and_refuses_the_next_patch() {
        let required = semver::VersionReq::parse("=1.2.3").expect("valid semver req");

        let pinned = registry_with_version(
            "acme.fixture.pinned",
            semver::Version::new(1, 2, 3),
            Arc::new(EchoProvider),
        );
        let handle = CapabilityCallHandle::new(Arc::new(pinned), "acme.consumer");
        block_on(handle.call_versioned(
            "acme.fixture.pinned",
            &required,
            serde_json::json!(null),
        ))
        .expect("=1.2.3 accepts an installed 1.2.3 provider");

        let next_patch = registry_with_version(
            "acme.fixture.pinned",
            semver::Version::new(1, 2, 4),
            Arc::new(EchoProvider),
        );
        let handle = CapabilityCallHandle::new(Arc::new(next_patch), "acme.consumer");
        let err = block_on(handle.call_versioned(
            "acme.fixture.pinned",
            &required,
            serde_json::json!(null),
        ))
        .expect_err("=1.2.3 is a hard pin and must refuse 1.2.4, not just older versions");
        assert!(matches!(err, CapabilityCallError::VersionMismatch { .. }));
    }

    #[test]
    fn a_version_requirement_against_nothing_registered_is_not_provided_not_a_version_mismatch() {
        let registry = CapabilityRegistry::default();
        let handle = CapabilityCallHandle::new(Arc::new(registry), "acme.consumer");
        let required = semver::VersionReq::parse("^1").expect("valid semver req");
        let err = block_on(handle.call_versioned(
            "acme.nobody.home",
            &required,
            serde_json::json!(null),
        ))
        .expect_err("nothing registered this capability at any version");
        assert_eq!(
            err,
            CapabilityCallError::NotProvided {
                capability: "acme.nobody.home".to_string()
            }
        );
    }

    #[test]
    fn an_undeclared_fallback_version_mismatch_names_itself_not_a_regressed_provider() {
        // The concrete case a subprocess plugin manifest produces: a
        // `PluginManifest::version` string that is legitimate under that
        // field's own "bare string" contract but is not valid semver
        // (`"beta-3"`, standing in for anything
        // `semver::Version::parse` refuses) -- `CapabilityRegistration::
        // from_declared_version_or_unversioned` degrades this to the
        // `0.0.0` sentinel rather than panicking or refusing registration.
        let registration = CapabilityRegistration::from_declared_version_or_unversioned(
            HostCapability::named("acme.fixture.undeclared").unwrap(),
            "beta-3",
            Arc::new(EchoProvider),
        );
        assert!(
            !registration.version_declared,
            "an unparseable declared_version must not read as author-declared"
        );
        let registry = CapabilityRegistry::from_registrations([registration]).unwrap();
        let handle = CapabilityCallHandle::new(Arc::new(registry), "acme.consumer");
        let required = semver::VersionReq::parse("^1").expect("valid semver req");
        let err = block_on(handle.call_versioned(
            "acme.fixture.undeclared",
            &required,
            serde_json::json!(null),
        ))
        .expect_err("the 0.0.0 fallback satisfies no non-zero version requirement");
        match &err {
            CapabilityCallError::VersionMismatch {
                available,
                available_declared,
                ..
            } => {
                assert_eq!(*available, semver::Version::new(0, 0, 0));
                assert!(
                    !*available_declared,
                    "the 0.0.0 sentinel is a fallback, not something the provider declared"
                );
            }
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
        // The Display text must not phrase the fallback as though the
        // provider itself declared and shipped version '0.0.0' -- exactly
        // the "plausible-looking dead end" an operator reading this error
        // must not be sent down.
        let rendered = err.to_string();
        assert!(
            !rendered.contains("offers '0.0.0'"),
            "must not read as an author-declared version, got: {rendered}"
        );
        assert!(
            rendered.contains("declared no usable version"),
            "must name the actual failure, got: {rendered}"
        );
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
