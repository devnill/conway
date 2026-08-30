//! [`MarketplaceError`] -- the one error type every fallible operation in
//! this crate returns. P-13 (fail closed, never silently open): every
//! variant here is checked and returned BEFORE this crate writes a single
//! byte to `dest_dir` in [`crate::install::install_plugin`] -- see that
//! function's own doc for the staging-then-rename mechanics that make a
//! failure partway through a multi-file install leave nothing behind
//! either.

/// Every way fetching a marketplace manifest, installing a plugin from it,
/// or removing an installed plugin can fail. Never a bare `String` or
/// `anyhow`-shaped catch-all (P-10: untrusted network input gets a named,
/// typed failure a caller can match on, not a formatted sentence a caller
/// can only display).
#[derive(Debug, thiserror::Error)]
pub enum MarketplaceError {
    /// The marketplace URL (or a per-file URL a manifest names) could not
    /// be reached at all -- DNS failure, connection refused, TLS failure,
    /// or the request timed out. Acceptance 5's "offline ... handled with a
    /// clear message" case.
    #[error("could not reach {url}: {source}")]
    Network {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    /// The remote answered, but not with success -- a 404 for a typo'd
    /// marketplace URL, a 500 from a broken marketplace host, and so on.
    /// Acceptance 5's "a bad URL" case (a URL that resolves to a real host
    /// answering with an error status, as opposed to [`Self::Network`]'s
    /// "no host answered at all").
    #[error("{url} returned HTTP {status}")]
    Http { url: String, status: u16 },
    /// The response body exceeded [`crate::manifest::MAX_MANIFEST_BYTES`]/
    /// [`crate::install::MAX_FILE_BYTES`] -- checked against
    /// `Content-Length` when the server sends one, and against the actual
    /// byte count either way, so a server that lies about its own length
    /// (or omits it) cannot bypass the cap. P-10: an unbounded read of a
    /// third-party response is exactly the "absurd size" hazard named
    /// there.
    #[error("{url}'s response is {actual} bytes, exceeding the {limit}-byte safety cap")]
    ResponseTooLarge {
        url: String,
        actual: u64,
        limit: u64,
    },
    /// The marketplace responded with a 2xx and a body that is not the
    /// shape this crate expects -- not valid JSON, or valid JSON missing a
    /// field [`crate::manifest::MarketplaceManifest`] requires. Acceptance
    /// 5's "a malformed marketplace response" case. Named separately from
    /// [`Self::Http`] because a malformed BODY on a successful status is a
    /// different failure an operator needs a different explanation for.
    #[error("{url} did not return a valid marketplace manifest: {message}")]
    MalformedManifest { url: String, message: String },
    /// The URL answered with a 2xx and a body that is not JSON at all but
    /// markup -- overwhelmingly, an operator who passed a *repository page*
    /// where this crate wants a URL pointing directly at a manifest
    /// document.
    ///
    /// **Why this is a separate variant from [`Self::MalformedManifest`]**
    /// (board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`, layer 1). The first
    /// operator to run `/plugin install` against a real Claude Code
    /// marketplace passed `https://github.com/<owner>/<repo>` and got back
    /// *"expected value at line 7 column 1"* -- a `serde_json` column
    /// reference to a `<head>` tag in GitHub's HTML. That message is
    /// accurate and completely useless: it describes the shape of a
    /// document the operator never knew was being parsed, and says nothing
    /// about what conway actually wanted. A parse error about someone
    /// else's markup is not a diagnosis.
    ///
    /// Claude Code treats a marketplace as a git repository and reads
    /// `.claude-plugin/marketplace.json` from inside it; this crate wants
    /// the manifest document itself. That difference is the whole bug, and
    /// naming it in the error is what lets an operator act. `hint` carries
    /// a concrete suggested URL when one can be derived from the input
    /// (see `crate::manifest::manifest_url_hint`), and is empty when it
    /// cannot -- never a guess presented as fact.
    #[error(
        "{url} returned a web page, not a marketplace manifest -- conway needs a URL that points \
         directly at the manifest document itself, not at a repository page{hint}"
    )]
    NotAManifestUrl { url: String, hint: String },
    /// The marketplace manifest itself parsed, but named no plugin with
    /// this id -- distinct from every error above because the manifest is
    /// well-formed; the id the caller (or the operator, mistyping it) asked
    /// for simply is not in it.
    #[error("marketplace at {marketplace_url} does not list a plugin with id '{plugin_id}'")]
    PluginNotFound {
        marketplace_url: String,
        plugin_id: String,
    },
    /// `plugin_id` is not a safe path component -- empty, containing a `/`
    /// or `\`, or a reserved component (`.`/`..`) that would let an
    /// attacker-controlled marketplace response steer where on disk this
    /// crate writes. P-10's "path traversal in a plugin name (`../../etc`)"
    /// named example, verbatim.
    #[error(
        "plugin id '{id}' is not a safe directory name -- refusing to use it as a store path \
         component"
    )]
    UnsafePluginId { id: String },
    /// One of `plugin_id`'s declared files names a relative path that
    /// would escape its own plugin directory (an absolute path, a `..`
    /// component, or a Windows drive/prefix component) -- see
    /// `crate::install::validate_relative_path` (private to that module)
    /// for the exact rule. The second half of the path-traversal example,
    /// applied to a FILE inside the plugin rather than the plugin's own
    /// directory name.
    #[error(
        "plugin '{id}' names an unsafe file path '{path}' -- refusing to write outside its own \
         plugin directory"
    )]
    UnsafeFilePath { id: String, path: String },
    /// `plugin_id` declares more files than
    /// [`crate::install::MAX_FILES_PER_PLUGIN`] -- a bound against a
    /// malicious or broken manifest asking this crate to open an unbounded
    /// number of connections for one install.
    #[error(
        "plugin '{id}' declares {count} files, exceeding the {limit}-file safety cap per install"
    )]
    TooManyFiles {
        id: String,
        count: usize,
        limit: usize,
    },
    /// `plugin_id` declares an empty `files` map -- nothing to install, and
    /// nothing `conway_plugin_claude::discover` (the intended downstream
    /// consumer of an installed directory, a dev-only dependency of this
    /// crate -- see this crate's own `Cargo.toml`) could ever read from an
    /// empty directory either.
    #[error("plugin '{id}' declares no files to install")]
    NoFiles { id: String },
    /// A local filesystem operation failed while staging or committing an
    /// install, or while removing an uninstalled plugin's directory.
    #[error("io error installing plugin '{id}': {source}")]
    Io {
        id: String,
        #[source]
        source: std::io::Error,
    },
    /// A `plugins[]` entry has neither `id` nor `name` set, so
    /// [`crate::manifest::MarketplacePluginEntry::identity`] has nothing to
    /// return -- board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`, the post-parse
    /// check `#[serde(deny_unknown_fields)]` cannot express for a type that
    /// now accepts two different entry shapes (`manifest.rs`'s own doc).
    #[error("a marketplace entry has neither an `id` nor a `name` -- conway cannot identify it")]
    MissingIdentity,
    /// `plugin_id`'s [`crate::manifest::PluginSource`] names a kind other
    /// than `git-subdir`/`github` -- board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`,
    /// layer 4/acceptance 4: most plausibly one requiring archive
    /// extraction, which this crate deliberately never adds (`Cargo.toml`'s
    /// own doc). Browsing a marketplace listing this kind still succeeds
    /// (`manifest.rs`'s own `PluginSource::Unsupported`); only an actual
    /// install attempt reaches this refusal.
    #[error(
        "plugin '{id}' names a source kind conway does not support: '{kind}' -- conway can \
         fetch git-based sources (git-subdir, github) but not one requiring archive extraction"
    )]
    UnsupportedSourceKind { id: String, kind: String },
    /// `plugin_id`'s `git-subdir` source names a URL whose scheme is not
    /// `http`/`https` -- P-10: the URL is network-supplied, untrusted input,
    /// and git's OTHER transports (`ext::`, `fd::`, a bare local path) can
    /// run an arbitrary command or open an arbitrary file descriptor rather
    /// than merely fetch a repository. Refused outright, never inferred
    /// down to "the part that looks like a URL" -- this project's own
    /// "deny-by-prefix is a seatbelt, not a boundary" lesson
    /// (`docs/plugins/trust-and-security.md`), applied here as an ALLOW-by-
    /// prefix: only `http(s)://` is accepted, everything else is refused.
    #[error(
        "plugin '{id}' names a git URL conway refuses to run git against: '{url}' -- only \
         http(s):// git remotes are supported (git's other transports, like ext:: or a local \
         path, can run an arbitrary command or read an arbitrary local file)"
    )]
    UnsafeGitUrl { id: String, url: String },
    /// `plugin_id`'s `git-subdir` source names an `http(s)://` URL whose
    /// authority embeds userinfo (`https://user:pass@host/...`). Refused
    /// OUTRIGHT rather than stripped-and-proceeded: a legitimate public
    /// marketplace has no reason to embed a credential, and the credential
    /// would otherwise survive into this crate's own error text and into
    /// `conway-cli`'s operator-facing "fetched via git from {url}"
    /// disclosure, which lands in a TUI transcript that can be copied,
    /// screen-shared, or logged (`crate::git_source`'s own doc,
    /// "credentials in a marketplace-supplied URL"). `url` here is always
    /// the REDACTED form (`crate::git_source::credentialed_url_redacted`)
    /// -- this variant never carries the credential itself.
    #[error(
        "plugin '{id}' names a git URL with embedded credentials -- conway refuses to run git \
         against a URL containing a username or password ('{url}')"
    )]
    CredentialedGitUrl { id: String, url: String },
    /// The system `git` binary could not be invoked at all -- board item
    /// `01M0Y6RYZA94BK6YXJ7X8TNEGR`, acceptance 5: refused by name, never a
    /// confusing failure partway through a clone attempt.
    #[error(
        "could not run `{program} --version` ({detail}) -- conway needs a working `git` on PATH \
         to install a git-sourced marketplace plugin"
    )]
    GitUnavailable { program: String, detail: String },
    /// The system `git` binary ran but the clone/checkout itself failed --
    /// a bad URL, an unreachable remote, a nonexistent subdirectory, or the
    /// bounded timeout this crate applies to a git invocation
    /// (`crate::git_source::GIT_TIMEOUT`) being exceeded.
    #[error("could not fetch plugin '{id}' from {url}: {detail}")]
    GitFailed {
        id: String,
        url: String,
        detail: String,
    },
}

impl MarketplaceError {
    /// A short, stable tag naming which variant this is -- useful for a
    /// caller (a transcript entry, a test assertion) that wants to branch
    /// on failure KIND without string-matching `Display` output, which this
    /// crate reserves the right to reword.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Network { .. } => "network",
            Self::Http { .. } => "http",
            Self::ResponseTooLarge { .. } => "response_too_large",
            Self::MalformedManifest { .. } => "malformed_manifest",
            Self::NotAManifestUrl { .. } => "not_a_manifest_url",
            Self::PluginNotFound { .. } => "plugin_not_found",
            Self::UnsafePluginId { .. } => "unsafe_plugin_id",
            Self::UnsafeFilePath { .. } => "unsafe_file_path",
            Self::TooManyFiles { .. } => "too_many_files",
            Self::NoFiles { .. } => "no_files",
            Self::Io { .. } => "io",
            Self::MissingIdentity => "missing_identity",
            Self::UnsupportedSourceKind { .. } => "unsupported_source_kind",
            Self::UnsafeGitUrl { .. } => "unsafe_git_url",
            Self::CredentialedGitUrl { .. } => "credentialed_git_url",
            Self::GitUnavailable { .. } => "git_unavailable",
            Self::GitFailed { .. } => "git_failed",
        }
    }
}
