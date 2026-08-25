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
            Self::PluginNotFound { .. } => "plugin_not_found",
            Self::UnsafePluginId { .. } => "unsafe_plugin_id",
            Self::UnsafeFilePath { .. } => "unsafe_file_path",
            Self::TooManyFiles { .. } => "too_many_files",
            Self::NoFiles { .. } => "no_files",
            Self::Io { .. } => "io",
        }
    }
}
