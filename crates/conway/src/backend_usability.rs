//! Whether a configured backend can actually serve a turn — the one place
//! that question is answered.
//!
//! Board item `01M11XSN7JK0N23XBNDFJKZB91`.
//!
//! # The gap this closes
//!
//! [`crate::builder::ConwayBuilder::build`] has exactly one check today:
//! the backend map is empty, so it hard-errors with *"no backends
//! configured"*. That check is correct as far as it goes and stops well
//! short of the cases an operator actually hits. All of these parse
//! cleanly, satisfy that check, and then fail on the first turn:
//!
//! - `api_key_env: "KIMI_API_KEY"` where `KIMI_API_KEY` is unset
//! - `base_url: "http://localhost:11434/v1"` where nothing is listening
//! - a non-empty backend map whose every entry is unusable for either reason
//!
//! # What this module is not
//!
//! **It does not change startup behaviour, print anything, or trigger any
//! flow.** It answers one question and nothing reads the answer
//! automatically — the same posture [`crate::config::locality`] takes. Two
//! consumers are expected (a first-run flow deciding whether to offer
//! guided setup, and a settings screen showing per-provider status), and
//! both call *this*: a classification vocabulary restated at a second call
//! site is exactly the kind of drift a single-implementation rule exists to
//! prevent, whose own worked example elsewhere in this tree is a doc
//! comment asserting a restatement "can never drift" while it already had.
//!
//! **It never performs inference.** Reachability is not a completion.
//! Proving a provider works by spending a real turn is a deliberate,
//! user-initiated verify step, not something a predicate does on every
//! boot.
//!
//! # Three states, because two would lie
//!
//! The interesting design constraint is that "broken" and "I cannot tell"
//! are genuinely different, and collapsing them into a boolean produces a
//! guided-setup prompt that ambushes someone whose wifi hiccuped.
//! `INTENT.md` §8.3 is the governing rule — refuse and name what changed,
//! never guess — so [`Usability`] has a third variant and
//! [`FleetUsability::should_offer_guided_setup`] deliberately answers
//! `false` when the fleet is merely undetermined.
//!
//! # Why so little is decided from `kind`
//!
//! `BackendEntry::kind` is an **open** vocabulary: a third-party kind
//! carries its own configuration in `BackendEntry::extra`, and
//! `BackendEntry`'s own doc records the explicit rejection of any shape
//! that would make built-in kinds first-class and everyone else a guest.
//! So this module does not know which kinds require a credential, and does
//! not pretend to. It decides only what is decidable without that
//! knowledge:
//!
//! - an `api_key_env` naming an unset variable is **definitely** broken —
//!   the operator declared an indirection and it does not resolve. That is
//!   true regardless of kind.
//! - an entry with no credential at all is **undetermined**, not broken —
//!   a local Ollama needs none, and a third-party kind may keep one in
//!   `extra`.
//!
//! Hardcoding "kind X needs a key" here would be both a guess and the
//! privileged-built-in asymmetry the schema deliberately removed.
//!
//! # Why only declared-local endpoints are probed
//!
//! Probing costs a connection, and this runs on the startup path. Probing
//! every remote provider on every boot would add latency and unsolicited
//! network chatter to third parties for a question a declared credential
//! already answers well enough.
//!
//! Locality is read from the **declared** `BackendEntry::local` field, never
//! inferred from `base_url` — see that field's own doc for the SSH-tunnel
//! case (`ssh -L 11434:localhost:11434 remote-box`) that defeats every
//! URL-shaped heuristic. Note this module uses the field for a *cost*
//! decision rather than a trust one, but reads it the same way for the same
//! reason: conway should not manufacture a fact out of a string nobody
//! asked it to interpret.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use crate::config::schema::{BackendEntry, ConwayConfig};

/// How long a single endpoint probe may take before it is abandoned and
/// reported as [`Undetermined::EndpointUnreachable`].
///
/// Deliberately short: every probe this module performs is against an
/// endpoint the operator declared `local`, so the honest expectation is a
/// loopback connection completing in single-digit milliseconds. A budget
/// this small cannot meaningfully delay startup even when several
/// providers are configured and none of them answers.
///
/// The value is a ceiling on *waiting*, never a claim about the endpoint: a
/// probe that exhausts it yields "could not determine", not "broken", which
/// is the whole point of [`Usability`]'s third variant.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// A definite failure: this backend cannot serve a turn, and the reason is
/// specific enough for an operator to act on without further diagnosis.
///
/// Typed rather than a formatted sentence so a caller can branch on the
/// failure KIND — a settings screen offering "set this variable" for one
/// and "start your server" for the other — matching
/// `crate::config::writer`'s and `conway_plugin_marketplace`'s existing
/// preference for named variants over strings a caller can only display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unusable {
    /// `api_key_env` names an environment variable that is unset, or set to
    /// the empty string.
    ///
    /// The variable's own name is carried because that is the actionable
    /// half: "misconfigured" tells an operator nothing, `KIMI_API_KEY`
    /// tells them exactly what to export. Acceptance 2 of this item's board
    /// entry asserts precisely that.
    CredentialVariableUnset {
        /// The variable named by `api_key_env`.
        variable: String,
    },
    /// A declared-local endpoint actively refused the connection — nothing
    /// is listening there.
    ///
    /// Distinguished from [`Undetermined::EndpointUnreachable`] because a
    /// refusal is an *answer*: the host is up and rejected us, so "the
    /// server is not running" is a fact rather than an inference. A timeout
    /// is not an answer and must not be reported as one.
    EndpointRefused {
        /// The `base_url` that refused.
        base_url: String,
    },
}

impl std::fmt::Display for Unusable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CredentialVariableUnset { variable } => write!(
                f,
                "the environment variable {variable} is not set (named by api_key_env)"
            ),
            Self::EndpointRefused { base_url } => {
                write!(f, "nothing is listening at {base_url}")
            }
        }
    }
}

/// Not broken, and not confirmed working either.
///
/// Every variant here means "answering this would require either spending a
/// turn or trusting a heuristic, and this module does neither". A fleet
/// consisting entirely of these must not trigger a guided setup — see
/// [`FleetUsability::should_offer_guided_setup`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Undetermined {
    /// The entry declares no credential at all, and this module cannot know
    /// whether this `kind` needs one.
    ///
    /// Legitimate for a local server that requires no key, and for a
    /// third-party kind keeping its credentials in `BackendEntry::extra`.
    /// See this module's own doc for why that ignorance is deliberate.
    NoCredentialDeclared {
        /// The entry's `kind`, so a caller can say which provider it is
        /// unsure about.
        kind: String,
    },
    /// A declared-local endpoint neither answered nor refused within
    /// [`DEFAULT_PROBE_TIMEOUT`], or its address could not be resolved.
    ///
    /// A timeout is genuinely ambiguous — a machine under load, a laggy
    /// interface, a server mid-restart — so it is reported as ignorance
    /// rather than as failure.
    EndpointUnreachable {
        /// The `base_url` that did not answer.
        base_url: String,
    },
    /// The entry declares a credential and is not declared local, so no
    /// probe was attempted.
    ///
    /// This is the ordinary state of a correctly configured hosted
    /// provider under [`ProbePolicy::LocalOnly`], and it is *not* a
    /// complaint. It is reported honestly rather than as `Usable` because
    /// this module has not, in fact, established that the credential is
    /// accepted — only that one is present.
    NotProbed,
    /// `base_url` is not a URL this module can extract a host and port
    /// from.
    ///
    /// Reported as undetermined rather than unusable on purpose: a `kind`
    /// this module does not know might legitimately use a non-URL address
    /// form, and refusing it outright would be exactly the built-ins-only
    /// assumption the rest of this module avoids.
    UnparseableEndpoint {
        /// The `base_url` that could not be parsed.
        base_url: String,
    },
}

impl std::fmt::Display for Undetermined {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCredentialDeclared { kind } => write!(
                f,
                "no credential declared, and conway cannot tell whether kind '{kind}' needs one"
            ),
            Self::EndpointUnreachable { base_url } => {
                write!(f, "{base_url} did not answer in time")
            }
            Self::NotProbed => write!(f, "a credential is declared; not verified"),
            Self::UnparseableEndpoint { base_url } => {
                write!(f, "could not read a host and port from {base_url}")
            }
        }
    }
}

/// One backend's status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Usability {
    /// Everything this module can check without spending a turn passed.
    Usable,
    /// Definitely cannot serve a turn.
    Unusable(Unusable),
    /// Cannot be determined here. **Never treat this as a failure** — see
    /// [`FleetUsability::should_offer_guided_setup`].
    Undetermined(Undetermined),
}

impl Usability {
    /// A short, stable tag naming which state this is — for a test
    /// assertion or a log line that wants to branch on state without
    /// matching on `Display` output, which this module reserves the right
    /// to reword. Mirrors `conway_plugin_marketplace::MarketplaceError::kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Usable => "usable",
            Self::Unusable(_) => "unusable",
            Self::Undetermined(_) => "undetermined",
        }
    }
}

/// The whole fleet's answer, and the type the first-run trigger reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FleetUsability {
    /// `[backends]` is empty. Nothing to be unsure about.
    NoBackendsConfigured,
    /// At least one backend is [`Usability::Usable`].
    AtLeastOneUsable,
    /// Every configured backend is definitely [`Usability::Unusable`].
    AllUnusable,
    /// None is usable, but at least one is undetermined — so "nothing
    /// works" has **not** been established.
    Undetermined,
}

impl FleetUsability {
    /// Should a guided provider setup be offered?
    ///
    /// True only when nothing works *and that is known*: an empty
    /// configuration, or one whose every entry is definitely broken.
    ///
    /// **[`Self::Undetermined`] answers `false`, and that is the important
    /// case.** It is the difference between "conway noticed your API key
    /// variable is unset" and "conway interrupted you with a setup wizard
    /// because a probe timed out once". The operator ruling behind this
    /// item chose the wider trigger — *no usable provider*, not merely *no
    /// backends configured* — and this method is where that width stops
    /// being reckless.
    pub fn should_offer_guided_setup(&self) -> bool {
        matches!(self, Self::NoBackendsConfigured | Self::AllUnusable)
    }
}

/// Which endpoints, if any, get a live connection attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ProbePolicy {
    /// Probe endpoints whose entry declares `local = true`. The default,
    /// and what a startup check should use — see this module's own doc for
    /// why remote endpoints are left alone.
    #[default]
    LocalOnly,
    /// Probe nothing; classify from configuration alone. Performs no I/O
    /// whatsoever, so it can be called from anywhere without a runtime
    /// consideration.
    Never,
    /// Probe every endpoint that has a parseable `base_url`, local or not.
    ///
    /// For a settings screen where the operator is *looking at* the list
    /// and expects live status, and has implicitly accepted the cost by
    /// opening it. Never appropriate on a startup path.
    All,
}

/// Classifies one backend entry.
///
/// The credential check runs first because it is free and definite: an
/// unset `api_key_env` variable makes the entry unusable no matter what a
/// probe would have said, so there is no reason to spend a connection
/// discovering that too.
///
/// **`env` is a caller-supplied map, never `std::env::vars()` read from
/// inside.** This is the hermetic-testing idiom
/// [`crate::config::merge::LoadOptions::env`] already establishes
/// workspace-wide, and it is not stylistic: `std::env::set_var` in an
/// in-process unit test races every other test thread reading process env
/// in parallel, and `crates/conway/tests/config_isolation_guard.rs` exists
/// because that exact hazard broke a suite once. A caller collects
/// `std::env::vars()` at its own call site.
pub async fn classify_entry(
    entry: &BackendEntry,
    env: &HashMap<String, String>,
    policy: ProbePolicy,
    timeout: Duration,
) -> Usability {
    // Free, definite, and kind-agnostic: a declared indirection that does
    // not resolve. An empty or whitespace-only value counts as unset —
    // `export KIMI_API_KEY=` is a variable that exists and still cannot
    // authenticate anything.
    if !entry.api_key_env.is_empty() {
        let resolved = env.get(&entry.api_key_env);
        if resolved.is_none_or(|v| v.trim().is_empty()) {
            return Usability::Unusable(Unusable::CredentialVariableUnset {
                variable: entry.api_key_env.clone(),
            });
        }
    }

    let has_credential = !entry.api_key.is_empty() || !entry.api_key_env.is_empty();

    let should_probe = match policy {
        ProbePolicy::Never => false,
        ProbePolicy::LocalOnly => entry.local,
        ProbePolicy::All => true,
    };

    if should_probe && !entry.base_url.is_empty() {
        return match probe_endpoint(&entry.base_url, timeout).await {
            EndpointProbe::Answered => Usability::Usable,
            EndpointProbe::Refused => Usability::Unusable(Unusable::EndpointRefused {
                base_url: entry.base_url.clone(),
            }),
            EndpointProbe::NoAnswer => Usability::Undetermined(Undetermined::EndpointUnreachable {
                base_url: entry.base_url.clone(),
            }),
            EndpointProbe::Unparseable => {
                Usability::Undetermined(Undetermined::UnparseableEndpoint {
                    base_url: entry.base_url.clone(),
                })
            }
        };
    }

    if has_credential {
        Usability::Undetermined(Undetermined::NotProbed)
    } else {
        Usability::Undetermined(Undetermined::NoCredentialDeclared {
            kind: entry.kind.clone(),
        })
    }
}

/// Classifies every configured backend, returning both the per-entry detail
/// (for a settings screen) and the roll-up (for a startup trigger).
///
/// Entries are probed **concurrently**, so the wall-clock cost of a fleet
/// of unreachable local endpoints is one `timeout`, not one per entry.
pub async fn classify_fleet(
    config: &ConwayConfig,
    env: &HashMap<String, String>,
    policy: ProbePolicy,
    timeout: Duration,
) -> (BTreeMap<String, Usability>, FleetUsability) {
    let mut per_entry = BTreeMap::new();

    // Genuinely concurrent, via `tokio::task::JoinSet` (feature `rt`, which
    // this crate already enables). The entry is cloned into each task
    // because a spawned task must be `'static`; a `BackendEntry` is a
    // handful of short strings, so that clone is far cheaper than the
    // connection it is about to make.
    //
    // The alternative — awaiting in a loop — would make the fleet's cost
    // N x `timeout` rather than `timeout`, and this function's own doc
    // would then be describing behaviour the code does not have.
    let mut set = tokio::task::JoinSet::new();
    for (id, entry) in &config.backends {
        let id = id.clone();
        let entry = entry.clone();
        let env = env.clone();
        set.spawn(async move {
            let usability = classify_entry(&entry, &env, policy, timeout).await;
            (id, usability)
        });
    }

    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((id, usability)) => {
                per_entry.insert(id, usability);
            }
            // A probe task panicking must not take the caller down, and
            // must not silently shrink the fleet either: the entry simply
            // does not appear, and `roll_up` then sees a smaller map. That
            // is only reachable via a bug in this module, so it is logged
            // rather than modelled as a state.
            Err(err) => {
                tracing::warn!(error = %err, "a backend usability probe task failed to join");
            }
        }
    }

    let verdict = roll_up(&per_entry);
    (per_entry, verdict)
}

/// The roll-up rule, separated so it can be tested without any I/O and
/// without constructing a whole [`ConwayConfig`].
pub fn roll_up(per_entry: &BTreeMap<String, Usability>) -> FleetUsability {
    if per_entry.is_empty() {
        return FleetUsability::NoBackendsConfigured;
    }
    if per_entry.values().any(|u| matches!(u, Usability::Usable)) {
        return FleetUsability::AtLeastOneUsable;
    }
    if per_entry
        .values()
        .all(|u| matches!(u, Usability::Unusable(_)))
    {
        return FleetUsability::AllUnusable;
    }
    FleetUsability::Undetermined
}

/// What one connection attempt established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndpointProbe {
    /// The endpoint accepted a TCP connection. It is up.
    Answered,
    /// The endpoint actively refused. Nothing is listening.
    Refused,
    /// Neither, within the budget — a timeout, or a name that would not
    /// resolve.
    NoAnswer,
    /// `base_url` yielded no host/port to connect to.
    Unparseable,
}

/// Attempts one bounded TCP connection to `base_url`'s host and port.
///
/// **A TCP connect, deliberately — not an HTTP request and emphatically not
/// a completion.** The question being answered is "is a server listening
/// there", which a connect answers exactly, with no assumption about what
/// protocol the far side speaks, no request body, and nothing for a
/// third-party kind's own API shape to disagree with.
///
/// The refused/timed-out distinction is the whole reason this returns four
/// states rather than a `bool`: a refusal is an answer from the host and a
/// timeout is silence, and this module reports the second as ignorance.
async fn probe_endpoint(base_url: &str, timeout: Duration) -> EndpointProbe {
    let Ok(parsed) = url::Url::parse(base_url) else {
        return EndpointProbe::Unparseable;
    };
    let Some(host) = parsed.host_str() else {
        return EndpointProbe::Unparseable;
    };
    let Some(port) = parsed.port_or_known_default() else {
        return EndpointProbe::Unparseable;
    };

    match tokio::time::timeout(
        timeout,
        tokio::net::TcpStream::connect((host.to_string(), port)),
    )
    .await
    {
        Ok(Ok(_stream)) => EndpointProbe::Answered,
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
            EndpointProbe::Refused
        }
        // Any other connection error (DNS failure, unreachable network,
        // permission) is ambiguous from here, and the outer timeout arm is
        // ambiguous by definition.
        Ok(Err(_)) | Err(_) => EndpointProbe::NoAnswer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A backend entry with every field at its default, so each test sets
    /// exactly the field it is about.
    ///
    /// **Per steering policy P-15:** a fixture that leaves the field under
    /// test at its default proves nothing, so every test below sets its own
    /// discriminating field explicitly rather than relying on this base.
    fn entry(kind: &str) -> BackendEntry {
        BackendEntry {
            kind: kind.to_string(),
            ..BackendEntry::default()
        }
    }

    /// A real, validated `ConwayConfig` parsed from JSON rather than
    /// hand-constructed: `ConwayConfig` deliberately has no `Default`
    /// (`default_role` has no sensible built-in), and parsing is the idiom
    /// the rest of this crate's tests already use.
    fn empty_config() -> ConwayConfig {
        serde_json::from_str(r#"{"default_role":"coder"}"#).expect("minimal config parses")
    }

    fn no_env() -> HashMap<String, String> {
        HashMap::new()
    }

    /// Binds a real listener and returns its `http://` URL plus the guard.
    /// Holding the listener keeps the port occupied.
    async fn listening() -> (String, TcpListener) {
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = l.local_addr().expect("addr");
        (format!("http://127.0.0.1:{}/v1", addr.port()), l)
    }

    /// Binds a port, learns its number, then drops the listener — so the
    /// port is known-closed rather than merely probably-closed. Picking an
    /// arbitrary high port would make the test depend on nothing else on
    /// the machine using it, which P-15 forbids.
    async fn closed_port_url() -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = l.local_addr().expect("addr").port();
        drop(l);
        format!("http://127.0.0.1:{port}/v1")
    }

    // ---- ACCEPTANCE 1: an empty backend map reports no usable provider ----

    #[test]
    fn an_empty_backend_map_reports_no_usable_provider() {
        let verdict = roll_up(&BTreeMap::new());
        assert_eq!(verdict, FleetUsability::NoBackendsConfigured);
        assert!(
            verdict.should_offer_guided_setup(),
            "an empty config is the canonical first-run case"
        );
    }

    // ---- ACCEPTANCE 2: an unset api_key_env names THE VARIABLE ----

    #[tokio::test]
    async fn an_unset_credential_variable_is_unusable_and_names_the_variable() {
        let mut e = entry("anthropic");
        e.api_key_env = "SOME_PROVIDER_KEY".to_string();

        let got = classify_entry(&e, &no_env(), ProbePolicy::Never, DEFAULT_PROBE_TIMEOUT).await;

        assert_eq!(
            got,
            Usability::Unusable(Unusable::CredentialVariableUnset {
                variable: "SOME_PROVIDER_KEY".to_string()
            })
        );
        // The actionable half: "misconfigured" would tell an operator
        // nothing. The rendered message must carry the variable's name.
        let rendered = match &got {
            Usability::Unusable(u) => u.to_string(),
            other => panic!("expected unusable, got {other:?}"),
        };
        assert!(
            rendered.contains("SOME_PROVIDER_KEY"),
            "reason must name the variable, got: {rendered}"
        );
    }

    #[tokio::test]
    async fn a_credential_variable_set_to_empty_counts_as_unset() {
        let mut e = entry("anthropic");
        e.api_key_env = "EMPTY_KEY".to_string();
        let env = HashMap::from([("EMPTY_KEY".to_string(), "   ".to_string())]);

        let got = classify_entry(&e, &env, ProbePolicy::Never, DEFAULT_PROBE_TIMEOUT).await;

        // `export EMPTY_KEY=` is a variable that exists and still cannot
        // authenticate anything, so it must not read as satisfied.
        assert_eq!(got.kind(), "unusable", "got {got:?}");
    }

    #[tokio::test]
    async fn a_set_credential_variable_is_not_reported_unusable() {
        let mut e = entry("anthropic");
        e.api_key_env = "REAL_KEY".to_string();
        let env = HashMap::from([("REAL_KEY".to_string(), "sk-abc".to_string())]);

        let got = classify_entry(&e, &env, ProbePolicy::Never, DEFAULT_PROBE_TIMEOUT).await;

        assert_eq!(got, Usability::Undetermined(Undetermined::NotProbed));
    }

    // ---- ACCEPTANCE 3: a closed port -- and WHICH state it maps to ----

    #[tokio::test]
    async fn a_declared_local_endpoint_on_a_closed_port_is_unusable_not_merely_unknown() {
        let url = closed_port_url().await;
        let mut e = entry("openai-compat");
        e.local = true;
        e.base_url = url.clone();

        let got =
            classify_entry(&e, &no_env(), ProbePolicy::LocalOnly, DEFAULT_PROBE_TIMEOUT).await;

        // Pinned deliberately: a refusal is an ANSWER from the host, so it
        // is a fact ("nothing is listening") rather than ignorance. The
        // board item asks for this distinction to be asserted rather than
        // left incidental.
        assert_eq!(
            got,
            Usability::Unusable(Unusable::EndpointRefused { base_url: url }),
            "connection refused must be Unusable, never Undetermined"
        );
    }

    #[tokio::test]
    async fn a_declared_local_endpoint_that_is_listening_is_usable() {
        let (url, _guard) = listening().await;
        let mut e = entry("openai-compat");
        e.local = true;
        e.base_url = url;

        let got =
            classify_entry(&e, &no_env(), ProbePolicy::LocalOnly, DEFAULT_PROBE_TIMEOUT).await;

        // No credential is declared, and that is correct for a local
        // server: reachability alone settles it.
        assert_eq!(got, Usability::Usable, "got {got:?}");
    }

    #[tokio::test]
    async fn a_remote_endpoint_is_not_probed_under_the_startup_policy() {
        let url = closed_port_url().await;
        let mut e = entry("anthropic");
        e.local = false; // NOT declared local
        e.base_url = url;
        e.api_key = "sk-abc".to_string();

        let got =
            classify_entry(&e, &no_env(), ProbePolicy::LocalOnly, DEFAULT_PROBE_TIMEOUT).await;

        // Even though that port is definitely closed, LocalOnly must not
        // reach out to it -- the startup path does not probe remote hosts.
        assert_eq!(got, Usability::Undetermined(Undetermined::NotProbed));
    }

    #[tokio::test]
    async fn probe_policy_all_does_reach_a_non_local_endpoint() {
        let url = closed_port_url().await;
        let mut e = entry("anthropic");
        e.local = false;
        e.base_url = url.clone();
        e.api_key = "sk-abc".to_string();

        let got = classify_entry(&e, &no_env(), ProbePolicy::All, DEFAULT_PROBE_TIMEOUT).await;

        // The settings screen's policy. Distinguishes the two policies by
        // OUTCOME rather than by inspecting which branch ran.
        assert_eq!(
            got,
            Usability::Unusable(Unusable::EndpointRefused { base_url: url })
        );
    }

    #[tokio::test]
    async fn an_unparseable_base_url_is_undetermined_never_unusable() {
        let mut e = entry("some-third-party-kind");
        e.local = true;
        e.base_url = "not-a-url".to_string();

        let got =
            classify_entry(&e, &no_env(), ProbePolicy::LocalOnly, DEFAULT_PROBE_TIMEOUT).await;

        // A kind this module does not know may legitimately use a non-URL
        // address form; refusing it outright would be the built-ins-only
        // assumption this module exists to avoid.
        assert_eq!(
            got,
            Usability::Undetermined(Undetermined::UnparseableEndpoint {
                base_url: "not-a-url".to_string()
            })
        );
    }

    #[tokio::test]
    async fn an_entry_with_no_credential_at_all_is_undetermined_not_unusable() {
        let e = entry("some-third-party-kind");

        let got = classify_entry(&e, &no_env(), ProbePolicy::Never, DEFAULT_PROBE_TIMEOUT).await;

        // conway cannot know whether this kind needs a credential -- it may
        // keep one in `extra`.
        assert_eq!(
            got,
            Usability::Undetermined(Undetermined::NoCredentialDeclared {
                kind: "some-third-party-kind".to_string()
            })
        );
    }

    // ---- ACCEPTANCE 4: one working + one broken == at least one usable ----

    #[test]
    fn one_working_and_one_broken_reports_at_least_one_usable() {
        let per_entry = BTreeMap::from([
            ("good".to_string(), Usability::Usable),
            (
                "bad".to_string(),
                Usability::Unusable(Unusable::CredentialVariableUnset {
                    variable: "NOPE".to_string(),
                }),
            ),
        ]);

        let verdict = roll_up(&per_entry);
        assert_eq!(verdict, FleetUsability::AtLeastOneUsable);
        assert!(
            !verdict.should_offer_guided_setup(),
            "a guided setup must not ambush someone whose SECOND provider is down"
        );
    }

    #[test]
    fn every_entry_definitely_broken_reports_all_unusable_and_offers_setup() {
        let per_entry = BTreeMap::from([(
            "bad".to_string(),
            Usability::Unusable(Unusable::CredentialVariableUnset {
                variable: "NOPE".to_string(),
            }),
        )]);

        let verdict = roll_up(&per_entry);
        assert_eq!(verdict, FleetUsability::AllUnusable);
        assert!(verdict.should_offer_guided_setup());
    }

    /// The constraint-1 case, and the most important test in this file.
    #[test]
    fn an_undetermined_fleet_does_not_offer_guided_setup() {
        let per_entry = BTreeMap::from([
            (
                "maybe".to_string(),
                Usability::Undetermined(Undetermined::EndpointUnreachable {
                    base_url: "http://localhost:11434/v1".to_string(),
                }),
            ),
            (
                "bad".to_string(),
                Usability::Unusable(Unusable::CredentialVariableUnset {
                    variable: "NOPE".to_string(),
                }),
            ),
        ]);

        let verdict = roll_up(&per_entry);
        assert_eq!(verdict, FleetUsability::Undetermined);
        assert!(
            !verdict.should_offer_guided_setup(),
            "'nothing works' has NOT been established -- a timed-out probe must \
             never interrupt someone with a setup wizard"
        );
    }

    // ---- ACCEPTANCE 5: bounded, never hangs ----

    /// A listener that accepts and never writes a byte. A TCP-connect probe
    /// completes on accept, so this must return promptly and report the
    /// endpoint as up.
    ///
    /// **This deliberately differs from the board item's phrasing**, which
    /// assumed a probe that waits for a reply. This module probes with a
    /// TCP connect precisely so it never has to speak a protocol a
    /// third-party kind might not implement -- so "accepts but never
    /// replies" is a server that IS listening, and saying otherwise would
    /// be a guess. The bound is asserted directly instead.
    #[tokio::test]
    async fn an_accepting_but_silent_endpoint_returns_promptly() {
        let (url, _guard) = listening().await;
        let mut e = entry("openai-compat");
        e.local = true;
        e.base_url = url;

        let started = std::time::Instant::now();
        let got =
            classify_entry(&e, &no_env(), ProbePolicy::LocalOnly, DEFAULT_PROBE_TIMEOUT).await;
        let elapsed = started.elapsed();

        assert_eq!(got, Usability::Usable);
        assert!(
            elapsed < DEFAULT_PROBE_TIMEOUT,
            "a connect must not wait for data; took {elapsed:?}"
        );
    }

    /// Every entry in a fleet gets classified, and a fleet of dead local
    /// endpoints resolves quickly.
    ///
    /// **This test does NOT verify the concurrency in `classify_fleet`, and
    /// is deliberately not named as though it does.** Per P-15 ("a coverage
    /// claim is not established until a stub has been run"), that was
    /// measured rather than assumed: replacing the `JoinSet` with a
    /// sequential loop leaves this test GREEN, because a connection to a
    /// closed loopback port is refused instantly whether or not the probes
    /// overlap. Four instant refusals take about as long serialized as
    /// concurrently.
    ///
    /// Making it discriminate would need endpoints that consume the full
    /// timeout, which cannot be produced portably with real sockets — and
    /// P-15's own rule against acceptance depending on one machine's
    /// configuration rules out a non-routable address. So the concurrency in
    /// `classify_fleet` is currently justified by reading, not by a guard.
    /// If a seam for injecting a probe outcome is ever added, this is the
    /// test that should grow teeth.
    #[tokio::test]
    async fn every_entry_in_a_fleet_is_classified_and_dead_endpoints_resolve_quickly() {
        let mut config = empty_config();
        for i in 0..4 {
            let mut e = entry("openai-compat");
            e.local = true;
            e.base_url = closed_port_url().await;
            config.backends.insert(format!("dead{i}"), e);
        }

        let started = std::time::Instant::now();
        let (per_entry, verdict) = classify_fleet(
            &config,
            &no_env(),
            ProbePolicy::LocalOnly,
            DEFAULT_PROBE_TIMEOUT,
        )
        .await;
        let elapsed = started.elapsed();

        assert_eq!(per_entry.len(), 4, "every entry must be classified");
        assert_eq!(verdict, FleetUsability::AllUnusable);
        // A loose smoke bound only -- see this test's own doc for why it
        // cannot distinguish concurrent from serialized execution.
        assert!(
            elapsed < DEFAULT_PROBE_TIMEOUT * 4,
            "a fleet of refused endpoints should resolve promptly; took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn classify_fleet_on_an_empty_config_reports_no_backends_configured() {
        let config = empty_config();
        let (per_entry, verdict) = classify_fleet(
            &config,
            &no_env(),
            ProbePolicy::Never,
            DEFAULT_PROBE_TIMEOUT,
        )
        .await;
        assert!(per_entry.is_empty());
        assert_eq!(verdict, FleetUsability::NoBackendsConfigured);
    }
}
