//! `commands/*.md` -- Claude Code's own custom-slash-command convention: a
//! Markdown file, an optional YAML frontmatter block, and a body that is a
//! PROMPT (interpolated with `$ARGUMENTS`/positional placeholders by real
//! Claude Code, then submitted as the operator's own turn).
//!
//! **Board item `01M0X1G29EZSFEWB1YAG40SE69` closes the deferral this
//! crate's own earlier item stated by name** (see [`crate`]'s own top doc,
//! "commands/*.md is ALSO out of scope" -- corrected there too):
//! `conway_core::ports::CommandOutcome::SubmitPrompt` now exists
//! (`01M0VSMF71S6VXX81YRAAF5S8Q`), so a `commands/*.md` file translates into
//! a real, invokable `conway_core::ports::Command`
//! ([`ClaudeCommand`]) -- exactly the shape
//! `conway_plugin_skeleton::FilePromptCommand` already proved out for an
//! operator-authored prompt file, applied here to a FOREIGN one.
//!
//! **Appetite: best effort, per the operator ruling this item was filed
//! under.** Get a command file invoking its prompt; no argument
//! interpolation, no frontmatter parity. Two things survive that
//! relaxation, both cheap, and both implemented here:
//!
//! 1. **Every frontmatter key this crate does not honor is NAMED**, in
//!    [`crate::UnsupportedItem`] (kind
//!    [`crate::UnsupportedKind::CommandFrontmatterKey`]), never silently
//!    dropped -- `description` is
//!    the only key this crate reads (it becomes [`ClaudeCommand`]'s own
//!    [`conway_core::ports::CommandSpec::summary`]); every other key present
//!    is named, `allowed-tools` with a stronger, PERMISSION-shaped reason
//!    (an operator who wrote a tool restriction and had it silently ignored
//!    has a permission surprise, not a fidelity one -- the operator ruling's
//!    own framing, quoted here rather than restated differently).
//! 2. **A raw `$ARGUMENTS` placeholder is refused, never submitted
//!    verbatim.** [`evaluate_body`] is the one place this is decided --
//!    see its own doc for why refusing (rather than stripping or
//!    substituting) is the chosen answer.
//!
//! **Namespacing -- reused, not invented.** [`ClaudeCommand::spec`] returns
//! a BARE name (the command file's own stem, e.g. `"config"` for
//! `commands/config.md`) -- the SAME "an author never picks their own
//! namespace" rule [`conway_core::ports::Plugin::commands`]'s own doc
//! states for every `Command` implementor, translated or not. The HOST
//! (`conway_cli::tui::commands::CommandRegistry::build`) prefixes this bare
//! name with the declaring plugin's own manifest id and validates the
//! result with `conway_core::event_name::validate_command_name` before it
//! is ever reachable -- the same shared validator
//! [`crate::hooks::HookTranslation`]'s own sibling event-name check reuses.
//! **A plugin command cannot shadow a built-in, structurally, not by a
//! check this module performs:** `CommandRegistry::build`'s own doc states
//! why (no built-in `SlashCommand` word contains the namespace separator,
//! so a namespaced full name can never equal one) -- this module's only
//! obligation is to keep emitting a BARE name, never a pre-namespaced one,
//! which it does unconditionally. What THIS module DOES guard, because a
//! failure here would fail the WHOLE plugin build rather than degrade one
//! command: `CommandRegistry::build`'s own registration check rejects an
//! empty or whitespace-containing `CommandSpec::name` outright (a
//! `CommandRegistrationError`, not a per-command skip) -- [`evaluate_body`]
//! refuses (named in [`crate::UnsupportedItem`], never registered) any
//! command whose file-stem-derived bare name would trip that check, so a
//! oddly-named foreign command file degrades to "not translated" rather
//! than to "the whole plugin fails to load."

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use conway_core::ports::{Command, CommandCtx, CommandOutcome, CommandSpec};

use crate::fsutil::read_bounded;
use crate::unsupported::UnsupportedItem;

/// One `commands/*.md` file, after translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandTranslation {
    /// `commands/<file>.md`, relative to the plugin directory -- the same
    /// relative-path convention `crate::UnsupportedItem::name` already uses
    /// for a `commands/*.md` finding.
    pub relative_path: String,
    /// This command's bare name (the file's own stem, `.md` stripped) --
    /// see this module's own top doc, "Namespacing", for why this is never
    /// pre-namespaced.
    pub bare_name: String,
    /// The frontmatter's own `description` key, if present -- the only
    /// frontmatter key this module reads (see [`Self::command`]).
    pub description: Option<String>,
    pub outcome: CommandMapOutcome,
}

/// Whether a `commands/*.md` file became a real, invokable
/// [`conway_core::ports::Command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandMapOutcome {
    /// `prompt` is the (frontmatter-stripped, normalized) body that
    /// [`CommandTranslation::command`] submits verbatim -- no
    /// interpolation, matching `CommandOutcome::SubmitPrompt`'s own v1
    /// posture exactly.
    Ready { prompt: String },
    /// This file did not become a command -- `reason` says why (empty
    /// body, unreadable file, malformed/unterminated frontmatter, a raw
    /// `$ARGUMENTS` placeholder, or a bare name that could never be typed).
    /// Always ALSO named in [`crate::UnsupportedItem`] by
    /// [`read_commands`] -- this variant is never silently dropped.
    Refused { reason: String },
}

impl CommandTranslation {
    /// `Some` only for [`CommandMapOutcome::Ready`] -- `None` for a
    /// `Refused` translation, which already named itself in
    /// [`crate::ClaudeCompatReport::unsupported`].
    pub fn command(&self) -> Option<Arc<dyn Command>> {
        let CommandMapOutcome::Ready { prompt } = &self.outcome else {
            return None;
        };
        let summary = self
            .description
            .clone()
            .unwrap_or_else(|| format!("submits {}'s own prompt", self.relative_path));
        Some(Arc::new(ClaudeCommand {
            name: self.bare_name.clone(),
            summary,
            prompt: prompt.clone(),
        }))
    }
}

/// A translated `commands/*.md` file, as a real
/// [`conway_core::ports::Command`]. `invoke` performs no I/O and cannot
/// fail -- the prompt text was already read and validated at translation
/// time ([`read_commands`]).
pub struct ClaudeCommand {
    name: String,
    summary: String,
    prompt: String,
}

#[async_trait]
impl Command for ClaudeCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: self.name.clone(),
            summary: self.summary.clone(),
        }
    }

    /// Always [`CommandOutcome::SubmitPrompt`] with this command's own
    /// (frontmatter-stripped) body, verbatim -- `ctx` is read by nothing
    /// here, the identical "v1 does no interpolation" posture
    /// `conway_plugin_skeleton::FilePromptCommand::invoke` already
    /// establishes for an operator-authored prompt file.
    async fn invoke(&self, _ctx: CommandCtx) -> CommandOutcome {
        CommandOutcome::SubmitPrompt {
            text: self.prompt.clone(),
        }
    }
}

/// Reads `<dir>/commands/*.md` (flat, no recursion -- the same scope
/// `crate::unsupported::scan_flat_markdown` already established for
/// `agents/*.md`), translating every file and appending a named
/// [`UnsupportedItem`] for every frontmatter key this module does not
/// honor and every file that does not become a command. `vec![]` when the
/// `commands` subdirectory is absent -- a plugin directory declaring no
/// commands at all is ordinary, not an error.
///
/// Files are processed in SORTED filename order -- deterministic, matching
/// `crate::unsupported::scan_flat_markdown`'s own sorted listing.
pub(crate) fn read_commands(
    dir: &Path,
    unsupported: &mut Vec<UnsupportedItem>,
) -> Vec<CommandTranslation> {
    let commands_dir = dir.join("commands");
    let Ok(entries) = std::fs::read_dir(&commands_dir) else {
        return Vec::new();
    };
    let mut file_names = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy().into_owned();
        if name.ends_with(".md") && entry.path().is_file() {
            file_names.push(name);
        }
    }
    file_names.sort();

    let mut translations = Vec::with_capacity(file_names.len());
    for file_name in file_names {
        translations.push(translate_one(&commands_dir, &file_name, unsupported));
    }
    translations
}

fn translate_one(
    commands_dir: &Path,
    file_name: &str,
    unsupported: &mut Vec<UnsupportedItem>,
) -> CommandTranslation {
    let relative_path = format!("commands/{file_name}");
    let bare_name = file_name
        .strip_suffix(".md")
        .unwrap_or(file_name)
        .to_string();

    macro_rules! refuse {
        ($reason:expr) => {{
            let reason: String = $reason;
            unsupported.push(UnsupportedItem::command(
                relative_path.clone(),
                reason.clone(),
            ));
            return CommandTranslation {
                relative_path,
                bare_name,
                description: None,
                outcome: CommandMapOutcome::Refused { reason },
            };
        }};
    }

    let content = match read_bounded(&commands_dir.join(file_name)) {
        Ok(content) => content,
        Err(err) => refuse!(format!("could not read this command file: {err}")),
    };

    let (frontmatter_src, body) = match split_frontmatter(&content) {
        Ok(parts) => parts,
        Err(reason) => refuse!(reason.to_string()),
    };

    let (description, other_keys) = match frontmatter_src {
        None => (None, Vec::new()),
        Some(src) => match parse_frontmatter(src) {
            Ok(parsed) => parsed,
            Err(err) => refuse!(format!("invalid YAML frontmatter: {err}")),
        },
    };

    for key in &other_keys {
        unsupported.push(UnsupportedItem::command_frontmatter_key(
            &relative_path,
            key,
            frontmatter_key_reason(key),
        ));
    }

    let outcome = evaluate_body(body, &bare_name);
    if let CommandMapOutcome::Refused { reason } = &outcome {
        unsupported.push(UnsupportedItem::command(
            relative_path.clone(),
            reason.clone(),
        ));
    }

    CommandTranslation {
        relative_path,
        bare_name,
        description,
        outcome,
    }
}

/// Decides whether a normalized body becomes a real command's prompt.
///
/// **The `$ARGUMENTS` decision, made here and nowhere else.** Three shapes
/// were open (the item's own spec: "strip, substitute, or refuse to
/// register such a command -- pick one, say which"). Substitution is ruled
/// out immediately -- `CommandOutcome::SubmitPrompt`'s own doc states v1
/// performs NO interpolation of any kind, so this module has no argument
/// VALUE to substitute in even if it wanted to. That leaves strip-and-run
/// versus refuse. **Refuse, chosen**: stripping `$ARGUMENTS` out of a body
/// authored expecting it to be replaced changes that body's own MEANING
/// (a sentence built around "run against $ARGUMENTS" reads as nonsense
/// with the token silently deleted, worse than not running at all) --
/// where `evaluate_body`'s sibling checks (empty body, an untypeable bare
/// name) all refuse rather than guess, this is the same posture applied to
/// the one shape the item's own spec calls out by name.
fn evaluate_body(body: &str, bare_name: &str) -> CommandMapOutcome {
    if bare_name.is_empty() || bare_name.chars().any(char::is_whitespace) {
        return CommandMapOutcome::Refused {
            reason: format!(
                "this command's file-stem-derived name {bare_name:?} is empty or contains \
                 whitespace -- `CommandRegistry::build` would reject it outright and fail the \
                 whole plugin's registration, so it is refused here instead, degrading only this \
                 one command"
            ),
        };
    }

    let normalized = normalize_body(body);
    if normalized.is_empty() {
        return CommandMapOutcome::Refused {
            reason: "this command file's body is empty -- nothing to submit".to_string(),
        };
    }
    if normalized.contains("$ARGUMENTS") {
        return CommandMapOutcome::Refused {
            reason: "this command's prompt body contains a raw \"$ARGUMENTS\" placeholder -- \
                     conway performs no argument interpolation (CommandOutcome::SubmitPrompt's \
                     own v1 posture), and submitting the placeholder text verbatim into the \
                     model's context would be worse than not registering the command at all"
                .to_string(),
        };
    }
    CommandMapOutcome::Ready { prompt: normalized }
}

/// The reason named for a frontmatter key this module does not honor.
/// `allowed-tools` gets a distinct, stronger reason (the operator ruling's
/// own framing, quoted in this module's top doc): every other key gets the
/// generic "not read" reason.
fn frontmatter_key_reason(key: &str) -> String {
    if key == "allowed-tools" {
        "an operator who wrote this expecting Claude Code's own tool restriction gets none here \
         -- conway's translated command imposes no tool restriction of any kind on the turn it \
         submits, which is a PERMISSION surprise, not merely a fidelity gap"
            .to_string()
    } else {
        format!(
            "conway's commands/*.md translation does not read Claude Code's \"{key}\" \
             frontmatter key -- named here rather than silently dropped"
        )
    }
}

/// Identical algorithm to `conway::skills::normalize_body` (duplicated for
/// the identical reason that module's own doc gives for duplicating it from
/// `agents.rs`: this crate does not depend on `conway`, the facade, in
/// production code) -- strips a single leading `\n`/`\r\n` then
/// `trim_end()`s. Internal whitespace (indentation) is left untouched.
fn normalize_body(raw: &str) -> String {
    let stripped = raw
        .strip_prefix("\r\n")
        .or_else(|| raw.strip_prefix('\n'))
        .unwrap_or(raw);
    stripped.trim_end().to_string()
}

/// Splits `content` into an optional YAML frontmatter block and the
/// remaining body. **Deliberately more permissive than `conway::skills::
/// split_frontmatter`** (this crate's own top doc, question 2: foreign
/// frontmatter is parsed permissively, not `deny_unknown_fields`-strict) --
/// a `commands/*.md` file with NO `---` block at all is ordinary (Claude
/// Code frontmatter is optional), returned as `Ok((None, content))`, never
/// an error. `Err` only for a file that OPENS a `---` block and never
/// closes it -- that shape is unambiguously broken, not merely
/// frontmatter-free, so it is refused rather than guessed at.
fn split_frontmatter(content: &str) -> Result<(Option<&str>, &str), &'static str> {
    let content = content.strip_prefix('\u{FEFF}').unwrap_or(content);
    let Some(after_open) = content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))
    else {
        return Ok((None, content));
    };

    let mut pos = 0usize;
    loop {
        if pos >= after_open.len() {
            return Err("unterminated frontmatter: no closing `---` delimiter found");
        }
        let rest = &after_open[pos..];
        let line_len = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
        let line_no_nl = rest[..line_len].trim_end_matches(['\n', '\r']);
        if line_no_nl.trim_end() == "---" {
            let yaml_src = &after_open[..pos];
            let body = &after_open[pos + line_len..];
            return Ok((Some(yaml_src), body));
        }
        pos += line_len;
    }
}

/// The frontmatter's wire shape, read PERMISSIVELY: `description` is the
/// only key this module gives meaning to; `#[serde(flatten)]` into a
/// `BTreeMap` catches every other key (sorted by key, for deterministic
/// reporting) rather than a `#[serde(deny_unknown_fields)]` struct that
/// would turn an unrecognized Claude Code key into a hard parse failure --
/// this crate's own top doc, question 2, states exactly this contrast with
/// `conway`'s own (conway-authored) frontmatter parsing.
#[derive(Debug, Default, serde::Deserialize)]
struct RawFrontmatter {
    description: Option<String>,
    #[serde(flatten)]
    other: std::collections::BTreeMap<String, serde_yaml::Value>,
}

/// Parses one YAML frontmatter block, returning its `description` (if any)
/// and every OTHER key present, sorted. An empty/whitespace-only block
/// (`---\n---\n`, a real, if unusual, Claude Code shape) parses as "no
/// description, no other keys" rather than a YAML error.
fn parse_frontmatter(yaml_src: &str) -> Result<(Option<String>, Vec<String>), String> {
    if yaml_src.trim().is_empty() {
        return Ok((None, Vec::new()));
    }
    let raw: RawFrontmatter = serde_yaml::from_str(yaml_src).map_err(|err| err.to_string())?;
    Ok((raw.description, raw.other.into_keys().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_command(dir: &Path, file_name: &str, contents: &str) {
        std::fs::create_dir_all(dir.join("commands")).unwrap();
        std::fs::write(dir.join("commands").join(file_name), contents).unwrap();
    }

    /// The headline shape: a well-formed command file (frontmatter +
    /// body) becomes a real, invokable `Command` submitting its own body.
    #[tokio::test]
    async fn a_well_formed_command_file_becomes_a_real_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command(
            dir.path(),
            "review.md",
            "---\ndescription: Review the diff\n---\n\nReview the diff for bugs.\n",
        );
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 1);
        assert!(unsupported.is_empty(), "{unsupported:?}");

        let translation = &translations[0];
        assert_eq!(translation.relative_path, "commands/review.md");
        assert_eq!(translation.bare_name, "review");
        assert_eq!(translation.description.as_deref(), Some("Review the diff"));
        assert_eq!(
            translation.outcome,
            CommandMapOutcome::Ready {
                prompt: "Review the diff for bugs.".to_string()
            }
        );

        let command = translation
            .command()
            .expect("a Ready translation must produce a Command");
        let spec = command.spec();
        assert_eq!(spec.name, "review");
        assert_eq!(spec.summary, "Review the diff");
        assert!(
            !spec.name.contains('.'),
            "the bare name must never be pre-namespaced: {spec:?}"
        );

        let ctx = CommandCtx {
            focused_agent: conway_core::ids::AgentId::new(),
            root_agent: conway_core::ids::AgentId::new(),
            session_id: conway_core::ids::SessionId::new(),
            args: "ignored, v1 does no interpolation".to_string(),
        };
        let outcome = command.invoke(ctx).await;
        assert_eq!(
            outcome,
            CommandOutcome::SubmitPrompt {
                text: "Review the diff for bugs.".to_string()
            }
        );
    }

    /// A command file with NO frontmatter at all is ordinary -- frontmatter
    /// is optional, and the whole file becomes the body.
    #[test]
    fn a_command_file_with_no_frontmatter_still_translates() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command(dir.path(), "greet.md", "Say hello.\n");
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 1);
        assert!(unsupported.is_empty());
        assert_eq!(translations[0].description, None);
        assert_eq!(
            translations[0].outcome,
            CommandMapOutcome::Ready {
                prompt: "Say hello.".to_string()
            }
        );
    }

    /// `beepboop` 1.4.0's real `commands/config.md` -- the item's own named
    /// test subject, verbatim (frontmatter: `description`, `argument-hint`,
    /// `allowed-tools`; no `$ARGUMENTS` anywhere in the body). Proves all
    /// three acceptance points on the one real fixture at once: it
    /// translates, `allowed-tools` is named with the permission-shaped
    /// reason, `argument-hint` is named too.
    const BEEPBOOP_CONFIG_MD: &str = "---\ndescription: Configure beepboop plugin settings (sounds and notifications)\nargument-hint: \"[show | enable sounds | disable sounds]\"\nallowed-tools: Read, Edit, Bash\n---\n\nManage the beepboop plugin configuration.\n\nFind the settings file and update it as directed.\n";

    #[test]
    fn beepboops_real_config_md_translates_and_names_its_ignored_frontmatter() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command(dir.path(), "config.md", BEEPBOOP_CONFIG_MD);
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 1);

        let translation = &translations[0];
        assert_eq!(translation.bare_name, "config");
        assert_eq!(
            translation.description.as_deref(),
            Some("Configure beepboop plugin settings (sounds and notifications)")
        );
        assert!(
            matches!(translation.outcome, CommandMapOutcome::Ready { .. }),
            "{translation:?}"
        );
        assert!(
            translation.command().is_some(),
            "a Ready translation must produce a real Command"
        );

        // Both non-`description` frontmatter keys are named, not silently
        // dropped -- `allowed-tools` above all (a permission surprise, per
        // the operator ruling).
        let names: Vec<_> = unsupported.iter().map(|u| u.name.as_str()).collect();
        assert!(
            names.contains(&"commands/config.md#allowed-tools"),
            "{names:?}"
        );
        assert!(
            names.contains(&"commands/config.md#argument-hint"),
            "{names:?}"
        );
        let allowed_tools = unsupported
            .iter()
            .find(|u| u.name == "commands/config.md#allowed-tools")
            .unwrap();
        assert_eq!(
            allowed_tools.kind,
            crate::unsupported::UnsupportedKind::CommandFrontmatterKey
        );
        assert!(
            allowed_tools.reason.contains("PERMISSION"),
            "allowed-tools must get the permission-shaped reason: {allowed_tools:?}"
        );
    }

    /// A raw `$ARGUMENTS` placeholder is refused, never submitted verbatim.
    #[test]
    fn a_raw_arguments_placeholder_is_refused_not_submitted() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command(
            dir.path(),
            "explain.md",
            "---\ndescription: explain something\n---\n\nExplain $ARGUMENTS in detail.\n",
        );
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 1);
        assert!(matches!(
            translations[0].outcome,
            CommandMapOutcome::Refused { .. }
        ));
        assert!(translations[0].command().is_none());
        assert_eq!(unsupported.len(), 1);
        assert_eq!(unsupported[0].name, "commands/explain.md");
        assert!(
            unsupported[0].reason.contains("$ARGUMENTS"),
            "{unsupported:?}"
        );
    }

    /// An empty body -- no frontmatter, no content -- is refused, not
    /// registered as a command that submits nothing.
    #[test]
    fn an_empty_body_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command(dir.path(), "blank.md", "");
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 1);
        assert!(matches!(
            translations[0].outcome,
            CommandMapOutcome::Refused { .. }
        ));
        assert_eq!(unsupported.len(), 1);
        assert_eq!(unsupported[0].name, "commands/blank.md");
    }

    /// A body that is only frontmatter, with nothing left after it, is
    /// also an empty body.
    #[test]
    fn a_frontmatter_only_file_is_refused_as_an_empty_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command(dir.path(), "only-meta.md", "---\ndescription: x\n---\n");
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 1);
        assert!(matches!(
            translations[0].outcome,
            CommandMapOutcome::Refused { .. }
        ));
    }

    /// Unterminated frontmatter (an opening `---` with no closing one) is
    /// refused, never guessed at.
    #[test]
    fn unterminated_frontmatter_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command(
            dir.path(),
            "broken.md",
            "---\ndescription: x\nbody with no closing delimiter\n",
        );
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 1);
        assert!(matches!(
            translations[0].outcome,
            CommandMapOutcome::Refused { .. }
        ));
        assert!(
            unsupported[0].reason.contains("unterminated"),
            "{unsupported:?}"
        );
    }

    /// Malformed YAML frontmatter is refused, named with the underlying
    /// parse error, never a panic.
    #[test]
    fn malformed_yaml_frontmatter_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command(
            dir.path(),
            "bad.md",
            "---\n: not: valid: yaml: [\n---\nBody.\n",
        );
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 1);
        assert!(matches!(
            translations[0].outcome,
            CommandMapOutcome::Refused { .. }
        ));
        assert!(unsupported[0].reason.contains("YAML"), "{unsupported:?}");
    }

    /// A file whose stem would produce an empty or whitespace-containing
    /// bare name is refused rather than reaching the host's own
    /// whole-build-failing registration check.
    #[test]
    fn a_bare_name_with_whitespace_is_refused_defensively() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command(dir.path(), "my command.md", "Do the thing.\n");
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 1);
        assert!(matches!(
            translations[0].outcome,
            CommandMapOutcome::Refused { .. }
        ));
        assert!(translations[0].command().is_none());
    }

    /// Multiple command files translate independently and in sorted order;
    /// one file's refusal does not affect a sibling's success.
    #[test]
    fn multiple_command_files_translate_independently_in_sorted_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command(dir.path(), "zeta.md", "Zeta body.\n");
        write_command(dir.path(), "alpha.md", "Alpha body.\n");
        write_command(dir.path(), "empty.md", "");
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 3);
        assert_eq!(
            translations
                .iter()
                .map(|t| t.bare_name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "empty", "zeta"]
        );
        assert!(matches!(
            translations[0].outcome,
            CommandMapOutcome::Ready { .. }
        ));
        assert!(matches!(
            translations[1].outcome,
            CommandMapOutcome::Refused { .. }
        ));
        assert!(matches!(
            translations[2].outcome,
            CommandMapOutcome::Ready { .. }
        ));
    }

    /// An absent `commands/` subdirectory is a true no-op.
    #[test]
    fn an_absent_commands_directory_reports_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert!(translations.is_empty());
        assert!(unsupported.is_empty());
    }

    /// A non-`.md` file in `commands/` is ignored entirely -- not even
    /// named, the same "not a command file" posture
    /// `crate::unsupported::scan_flat_markdown` already established.
    #[test]
    fn a_non_markdown_file_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command(dir.path(), "README.txt", "not a command");
        let mut unsupported = Vec::new();
        let translations = read_commands(dir.path(), &mut unsupported);
        assert!(translations.is_empty());
        assert!(unsupported.is_empty());
    }
}
