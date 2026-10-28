//! Runtime configuration defaults and environment overrides.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context as _;

use crate::completion::CompletionBehavior;
use crate::path_utils::shell_home_dir;

use crate::prompt::PromptIndicators;

/// Shell configuration defaults used by the runtime.
#[derive(Debug, Clone, Default)]
pub struct ShellConfig {
    /// Prompt indicator symbol (e.g. "%", "\$", "\u276f", "\u3bb")
    pub prompt_symbol: String,
    /// Prompt template (e.g. "{user}@{host} {cwd} {symbol}")
    pub prompt_format: Option<String>,
    /// Optional right-side prompt template.
    pub right_prompt_format: Option<String>,
    /// Optional mode-specific prompt indicators.
    pub prompt_indicators: PromptIndicators,
    /// Prompt backend: "template" (legacy) or "segments" (p10k-style).
    pub prompt_style: Option<String>,
    /// Segment preset: "lean" | "classic" | "rainbow" | "pure" | "robbyrussell".
    pub segment_preset: Option<String>,
    /// Custom left-prompt segment order (overrides preset).
    pub left_prompt_elements: Option<Vec<String>>,
    /// Custom right-prompt segment order (overrides preset).
    pub right_prompt_elements: Option<Vec<String>>,
}

/// Hook script lists, shared by reference (niubash#596): the prompt loop
/// used to deep-clone a whole hook vector on every prompt only to satisfy
/// the borrow checker (`run_hook_scripts` needs `&mut self`), even though
/// hook lists are fixed at construction time and never mutated afterwards.
/// `Rc<[String]>` keeps the snapshot semantics (a hook script cannot change
/// the list mid-run) while making the per-prompt take O(1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookConfig {
    pub precmd: std::rc::Rc<[String]>,
    pub preexec: std::rc::Rc<[String]>,
    pub chpwd: std::rc::Rc<[String]>,
    pub postcmd: std::rc::Rc<[String]>,
    pub zshaddhistory: std::rc::Rc<[String]>,
    pub zshexit: std::rc::Rc<[String]>,
    pub greeting: std::rc::Rc<[String]>,
    pub title: std::rc::Rc<[String]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditorMode {
    #[default]
    Emacs,
    Vi,
}

impl EditorMode {}

#[derive(Debug, Clone, Default)]
pub struct EditorConfig {
    pub edit_mode: EditorMode,
}

/// History configuration. There is deliberately no per-session sharing
/// mode anymore (niubash#572): the retired `NIU_HISTORY_MODE`
/// shared/session/private switch was a GNU-less invention whose documented
/// behaviors did not match the implementation (an rc assignment could never
/// take effect because the value was read before the rc ran, and the
/// session mode's "builtins can observe later file updates" promise was
/// false — refresh short-circuited for every non-shared mode). History now
/// has exactly one semantics, GNU's: one live file every session appends
/// to and refreshes from, with `history -a/-n/-r` as the explicit sync
/// verbs. A `NIU_HISTORY_MODE` left in the environment is silently
/// ignored (no startup diagnostic: `-c`/script stderr stays byte-stable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryConfig {
    pub path: Option<PathBuf>,
    pub max_size: usize,
    pub ignore_space_prefixed: bool,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            path: None,
            max_size: 10000,
            ignore_space_prefixed: false,
        }
    }
}

/// Completion menu display style.
///
/// The default is [`CompletionStyle::Column`] (owner ruling 2026-10-14,
/// superseding the 2026-10-12 Gnu flip after the owner used it live): TAB
/// shows the candidates immediately as a multi-per-row grid — the look of
/// GNU's printed listing without the two-TAB dance (a first TAB that
/// completes nothing and stays silent read as "dead" at the prompt).
/// [`CompletionStyle::Gnu`] remains available as the literal GNU readline
/// behavior (`NIU_COMPLETION_STYLE=gnu` /
/// `niu config set completion.style gnu`), and the Ide popup as `ide`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionStyle {
    /// GNU readline literal: first TAB completes the common prefix (a lone
    /// candidate completes outright); an ambiguous second TAB lists the
    /// candidates columnar and repaints the prompt.
    Gnu,
    /// Multi-column popup with descriptions (VS Code style).
    Ide,
    /// Multi-column grid (like zsh `compinit`). The default: every TAB
    /// shows the candidates at once, several per row — the GNU listing
    /// look without the silent first TAB.
    #[default]
    Column,
    /// Vertical list with descriptions (like fish).
    List,
    /// Insert first match directly, Tab cycles (like bash `menu-complete`).
    Inline,
}

impl CompletionStyle {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gnu" | "readline" => Some(Self::Gnu),
            "ide" | "smart" | "popup" => Some(Self::Ide),
            "column" | "col" | "grid" => Some(Self::Column),
            "list" | "vertical" => Some(Self::List),
            "inline" | "cycle" | "menu" => Some(Self::Inline),
            _ => None,
        }
    }

    /// The canonical spelling used by the persisted config file, env values
    /// and every display surface (`niu doctor`, `niu config`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gnu => "gnu",
            Self::Ide => "ide",
            Self::Column => "column",
            Self::List => "list",
            Self::Inline => "inline",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuConfig {
    pub completion_page_size: usize,
    pub history_page_size: usize,
    pub max_entry_lines: u16,
    pub completion_style: CompletionStyle,
}

impl Default for MenuConfig {
    fn default() -> Self {
        Self {
            completion_page_size: 10,
            history_page_size: 10,
            max_entry_lines: 5,
            completion_style: CompletionStyle::default(),
        }
    }
}

impl MenuConfig {
    pub fn with_env_overrides(mut self) -> Self {
        // Resolution order (owner ruling 2026-10-14): NIU_COMPLETION_STYLE env
        // > persisted `completion.style` (config.toml) > the built-in default
        // (column). `resolve_completion_style` warns once on a bad env value.
        self.completion_style = resolve_completion_style().0;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCompletionConfig {
    pub enabled: bool,
    pub commands: Vec<String>,
    pub timeout_millis: u64,
}

impl Default for RuntimeCompletionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            commands: Vec::new(),
            timeout_millis: 1000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeWidgetConfig {
    pub enabled: bool,
    pub presets: Vec<String>,
    pub import_bindkeys: bool,
}

impl Default for NativeWidgetConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            presets: Vec::new(),
            import_bindkeys: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeWidgetBinding {
    pub widget: String,
    pub function: Option<String>,
    pub key: Option<String>,
    pub keymap: Option<String>,
    pub source_file: Option<PathBuf>,
    pub line: Option<usize>,
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutosuggestConfig {
    pub enabled: bool,
    pub strategies: Vec<String>,
    pub highlight_style: String,
    pub buffer_max_size: Option<usize>,
}

impl Default for AutosuggestConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            strategies: vec!["history".to_string()],
            highlight_style: "fg=8".to_string(),
            buffer_max_size: None,
        }
    }
}

impl AutosuggestConfig {
    pub fn with_env_overrides(mut self) -> Self {
        if let Ok(value) = std::env::var("NIU_AUTOSUGGEST_STRATEGY") {
            let strategies = parse_autosuggest_strategy_value(&value);
            if !strategies.is_empty() {
                self.strategies = strategies;
            }
        }
        if let Ok(value) = std::env::var("NIU_AUTOSUGGEST_HIGHLIGHT_STYLE") {
            if !value.trim().is_empty() {
                self.highlight_style = value;
            }
        }
        if let Ok(value) = std::env::var("NIU_AUTOSUGGEST_BUFFER_MAX_SIZE") {
            match value.trim().parse::<usize>() {
                Ok(max_size) => self.buffer_max_size = Some(max_size),
                Err(err) => log::warn!(
                    "Invalid NIU_AUTOSUGGEST_BUFFER_MAX_SIZE '{}': {}",
                    value,
                    err
                ),
            }
        }
        self
    }

    pub fn history_strategy_enabled(&self) -> bool {
        self.enabled
            && self
                .strategies
                .iter()
                .any(|strategy| strategy.eq_ignore_ascii_case("history"))
    }
}

fn parse_autosuggest_strategy_value(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.trim_matches('"')
                .trim_matches('\'')
                .to_ascii_lowercase()
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxHighlightConfig {
    pub enabled: bool,
    pub highlighters: Vec<String>,
    pub max_length: Option<usize>,
    pub styles: HashMap<String, String>,
}

impl Default for SyntaxHighlightConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            highlighters: vec!["main".to_string()],
            max_length: None,
            styles: HashMap::new(),
        }
    }
}

impl SyntaxHighlightConfig {
    pub fn with_env_overrides(mut self) -> Self {
        if let Ok(value) = std::env::var("NIU_HIGHLIGHT_HIGHLIGHTERS") {
            let highlighters = parse_arrayish_value(&value);
            if !highlighters.is_empty() {
                self.highlighters = highlighters;
            }
        }
        if let Ok(value) = std::env::var("NIU_HIGHLIGHT_MAXLENGTH") {
            match value.trim().parse::<usize>() {
                Ok(max_length) => self.max_length = Some(max_length),
                Err(err) => log::warn!("Invalid NIU_HIGHLIGHT_MAXLENGTH '{}': {}", value, err),
            }
        }
        if let Ok(value) = std::env::var("NIU_HIGHLIGHT_STYLES") {
            for (key, value) in parse_style_map_value(&value) {
                self.styles.insert(key, value);
            }
        }
        self
    }

    pub fn main_highlighter_enabled(&self) -> bool {
        self.enabled
            && self
                .highlighters
                .iter()
                .any(|highlighter| highlighter.eq_ignore_ascii_case("main"))
    }
}

fn parse_arrayish_value(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.trim_matches('"')
                .trim_matches('\'')
                .to_ascii_lowercase()
        })
        .collect()
}

fn parse_style_map_value(value: &str) -> Vec<(String, String)> {
    value
        .split(';')
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                return None;
            }
            Some((key.to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

/// Policy for the command-not-found package-search hint (niubash#249): a
/// typo must never trigger an unsolicited package-manager advertisement,
/// so the hint is off by default (GNU bash prints only the
/// "command not found" line). The only opt-in channel names niu's own
/// bundled manager (wpm); third-party managers are never recommended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandNotFoundHint {
    /// No package hint at all (the default).
    #[default]
    Off,
    /// Suggest one `wpm search --name '<word>'` line for high-confidence
    /// near-misses (Windows builds only — wpm does not exist elsewhere).
    Wpm,
}

impl CommandNotFoundHint {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Some(Self::Off),
            "wpm" => Some(Self::Wpm),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Wpm => "wpm",
        }
    }
}

/// Schema marker for the persisted user settings file
/// (`~/.niubash/config.toml`), written by `niu config set`.
pub const USER_CONFIG_SCHEMA: &str = "niubash:config@0.1.0";

/// The command-not-found hint policy key (`niu config set
/// command-not-found-hint`).
pub const COMMAND_NOT_FOUND_HINT_KEY: &str = "command-not-found-hint";

/// The TAB completion style key: `niu config set completion.style <value>`.
/// In the file this is the TOML dotted key `completion.style` (a `[completion]`
/// table with a `style` field), spelled with the dot on every command surface.
pub const COMPLETION_STYLE_KEY: &str = "completion.style";

/// The personal-rc policy key (niubash#359): in the file this is the TOML
/// dotted key `startup.rc-policy` (a `[startup]` table with an `rc-policy`
/// field). Explicit `niu setup --rc-policy` owns the write; `niu config`
/// reads and preserves it but never sets it — the doc allows no
/// environment-based implicit grant and no competing booleans.
pub const RC_POLICY_KEY: &str = "startup.rc-policy";

/// The persistent key-binding rules key (niubash#630, K4): in the file
/// this is the `[keymap]` table's `bind` string array. Each element is
/// the argument string of a GNU `bind` command (`'"\C-g": redisplay'`,
/// `'-x "\C-t": my-cmd'`), parsed by the engine's own bind parsers
/// at load — there is no second grammar. `niu config set keymap.bind`
/// appends one validated rule.
pub const KEYMAP_BIND_KEY: &str = "keymap.bind";

/// The persistent key-removal key (niubash#630, K4): the `[keymap]`
/// table's `unbind` string array. Each element names one key in bind
/// spelling (`"\C-t"`); at editor build the slot is removed outright —
/// the key goes back to unbound, not to a lower layer's binding.
pub const KEYMAP_UNBIND_KEY: &str = "keymap.unbind";

/// The `[completion]` section of config.toml.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct CompletionConfigSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style: Option<String>,
}

/// The `[startup]` section of config.toml.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct StartupConfigSection {
    #[serde(rename = "rc-policy", default, skip_serializing_if = "Option::is_none")]
    rc_policy: Option<String>,
}

/// The `[keymap]` section of config.toml (niubash#630, K4): the user's
/// persistent key-binding rules. `bind` elements are GNU bind argument
/// strings; `unbind` elements are single keyseqs. Both are raw strings
/// here — parsing happens in `crate::repl` against the engine's own
/// bind parsers, so resolve stays a pure read.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct KeymapConfigSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bind: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unbind: Option<Vec<String>>,
}

/// Which file may fill the default interactive personal-rc slot
/// (niubash#359). Persisted as `startup.rc-policy` in config.toml; every
/// read problem — missing key, unknown value, wrong type, duplicate keys,
/// malformed TOML, unreadable file — fails closed to `primary-only`, and
/// startup never repairs or rewrites that state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RcPolicy {
    /// `~/.niubashrc` is the only personal rc; `~/.bashrc` is never
    /// considered (the default).
    #[default]
    PrimaryOnly,
    /// Explicit setup opt-in: when the primary is genuinely absent,
    /// `~/.bashrc` becomes eligible for the slot. An existing primary —
    /// even empty, unreadable, a directory, or a broken symlink — owns it
    /// exclusively.
    BashrcFallback,
}

impl RcPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "primary-only" => Some(Self::PrimaryOnly),
            "bashrc-fallback" => Some(Self::BashrcFallback),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrimaryOnly => "primary-only",
            Self::BashrcFallback => "bashrc-fallback",
        }
    }
}

/// Resolve the effective personal-rc policy. Deliberately has NO
/// `NIU_`-prefixed override: the contract forbids environment-based
/// implicit grants, so only a persisted explicit-setup pick flips the
/// default. Anything unparseable fails closed to `primary-only`.
pub fn resolve_rc_policy() -> RcPolicy {
    read_user_config()
        .startup
        .and_then(|startup| startup.rc_policy)
        .and_then(|value| RcPolicy::parse(&value))
        .unwrap_or_default()
}

/// Persist `startup.rc-policy` (the `niu setup --rc-policy` writer —
/// explicit setup is the only command that may persist or revoke this
/// policy). Setting the built-in default removes the key again so the
/// file never pins a stale default. No rc file is touched: selecting the
/// fallback must not create an empty primary rc, which would suppress
/// the selected fallback (niubash#359).
pub fn set_rc_policy(policy: RcPolicy) -> anyhow::Result<PathBuf> {
    write_user_config(|doc| {
        ensure_schema(doc);
        if policy == RcPolicy::default() {
            remove_table_key(doc, "startup", "rc-policy");
            Ok(())
        } else {
            set_table_key(
                doc,
                "startup",
                "rc-policy",
                toml_edit::value(policy.as_str()),
            )
        }
    })
}

/// Where the persisted user settings live: `$NIU_CONFIG` overrides (tests,
/// portable setups); default `~/.niubash/config.toml` (the mirrors.toml
/// convention).
pub fn user_config_path() -> PathBuf {
    if let Some(value) = std::env::var_os("NIU_CONFIG") {
        let path = PathBuf::from(value);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    shell_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".niubash")
        .join("config.toml")
}

/// The parsed config.toml. Unknown keys a future version (or a hand edit)
/// added parse compatibly and are ignored.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct UserConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schema: Option<String>,
    #[serde(
        rename = "command-not-found-hint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    command_not_found_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completion: Option<CompletionConfigSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    startup: Option<StartupConfigSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keymap: Option<KeymapConfigSection>,
}

/// Read and parse the persisted user settings. Any problem — missing file,
/// unreadable file, malformed TOML — degrades to an empty config: a config
/// issue must never break the reading path.
fn read_user_config() -> UserConfigFile {
    std::fs::read_to_string(user_config_path())
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

/// Header notes for a freshly created config.toml (parsed into the new
/// document so every later surgical edit lands after them). Existing files
/// keep their own layout — these lines are never injected into one.
const USER_CONFIG_NOTES: &str = "\
# `niu config set <key> <value>` edits one key surgically; unrelated keys\n\
# and comments are preserved, and setting a key back to its default\n\
# removes the override again.\n\
# command-not-found-hint: package-search suggestion printed after\n\
#   \"command not found\". off (default) | wpm — wpm suggests one\n\
#   `wpm search --name` line for high-confidence near-misses only;\n\
#   third-party managers are never recommended (niubash#249).\n\
# completion.style: TAB completion shape. column (default) | gnu |\n\
#   ide | list | inline — `NIU_COMPLETION_STYLE` overrides this\n\
#   file (env > config > default).\n\
# startup.rc-policy: which file fills the interactive personal-rc slot\n\
#   when ~/.niubashrc is absent. primary-only (default) |\n\
#   bashrc-fallback — explicit `niu setup --rc-policy` owns this key;\n\
#   it is preserved, never set, by `niu config` (niubash#359).\n\
# keymap.bind / keymap.unbind: persistent key-binding rules\n\
#   (niubash#630). bind elements are GNU bind argument strings, e.g.\n\
#   '\"\\C-g\": redisplay' or '-x \"\\C-t\": my-cmd'; unbind elements\n\
#   name one key to remove outright, e.g. '\"\\C-t\"'. Rules apply at\n\
#   editor build after scripts and NIU_BINDKEYS, so they win over\n\
#   startup-file binds; add one with `niu config set keymap.bind`.\n";

/// The document a first-time writer starts from (header notes + schema).
fn fresh_user_config_doc() -> toml_edit::DocumentMut {
    format!("{USER_CONFIG_NOTES}\nschema = \"{USER_CONFIG_SCHEMA}\"\n")
        .parse()
        .expect("the fresh config template is valid TOML")
}

/// Keep the managed schema marker current without moving user content.
fn ensure_schema(doc: &mut toml_edit::DocumentMut) {
    doc["schema"] = toml_edit::value(USER_CONFIG_SCHEMA);
}

/// Remove `key` from `doc[table]`; drop the table itself when the removal
/// leaves it empty (a table carrying other keys — managed or unknown —
/// is preserved untouched). Handles BOTH render shapes: a `[table]` header
/// section and an inline `table = {…}` (a hand edit may use either — they
/// are the same namespace to every TOML reader).
fn remove_table_key(doc: &mut toml_edit::DocumentMut, table: &str, key: &str) {
    let emptied = match doc.get_mut(table) {
        Some(toml_edit::Item::Table(section)) => {
            section.remove(key);
            section.is_empty()
        }
        Some(toml_edit::Item::Value(toml_edit::Value::InlineTable(section))) => {
            section.remove(key);
            section.is_empty()
        }
        // A wrong-typed or absent section has no such key to remove.
        _ => false,
    };
    if emptied {
        doc.remove(table);
    }
}

/// Set `table.key = value`, preserving the section's existing shape and
/// its sibling keys: a missing section becomes a standard `[table]` header,
/// an existing header or inline section keeps its form. A wrong-typed
/// section (e.g. `table = "x"`) is a refusal, not a replacement — the
/// bytes stay exactly as they were.
fn set_table_key(
    doc: &mut toml_edit::DocumentMut,
    table: &str,
    key: &str,
    value: toml_edit::Item,
) -> anyhow::Result<()> {
    if !doc.contains_key(table) {
        doc[table] = toml_edit::table();
    }
    match doc.get_mut(table) {
        Some(toml_edit::Item::Table(section)) => {
            section[key] = value;
            Ok(())
        }
        Some(toml_edit::Item::Value(toml_edit::Value::InlineTable(section))) => {
            section.insert(
                key,
                value
                    .into_value()
                    .map_err(|error| anyhow::anyhow!("could not encode {table}.{key}: {error}"))?,
            );
            Ok(())
        }
        _ => anyhow::bail!(
            "could not save {}: `{table}` is not a table — fix it by hand; \
             niu never replaces config content it cannot read as written",
            user_config_path().display()
        ),
    }
}

/// Surgically persist one setting in config.toml (niubash#359): `edit`
/// mutates the parsed document and the result publishes atomically.
/// Unrelated keys, comments, ordering and layout survive — the pre-1.4.3
/// writer rebuilt a typed subset and dropped them, which the rc-ownership
/// contract forbids. A malformed or unreadable existing file is NEVER
/// replaced with defaults: the error reports the inability to save and
/// the bytes stay untouched.
fn write_user_config(
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> anyhow::Result<()>,
) -> anyhow::Result<PathBuf> {
    let path = user_config_path();
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(text) => text.parse::<toml_edit::DocumentMut>().map_err(|err| {
            anyhow::anyhow!(
                "could not save {}: the file is not parseable TOML ({err}); \
                 fix it by hand or remove it — niu never replaces a config it \
                 cannot read",
                path.display()
            )
        })?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => fresh_user_config_doc(),
        Err(err) => {
            return Err(err).with_context(|| {
                format!("could not save {}: cannot read the file", path.display())
            })
        }
    };
    edit(&mut doc)?;
    crate::path_utils::write_file_atomic(&path, doc.to_string().as_bytes())?;
    Ok(path)
}

/// Resolve the effective hint policy: the persisted user setting
/// (`niu config set command-not-found-hint`), overridden by
/// `NIU_COMMAND_NOT_FOUND_HINT` (the env-over-file precedence every
/// `NIU_` override uses). Any problem — missing file, unreadable TOML,
/// unknown value — degrades to the default (off): a config issue must
/// never break the command-not-found path.
pub fn resolve_command_not_found_hint() -> CommandNotFoundHint {
    let mut hint = read_user_config()
        .command_not_found_hint
        .and_then(|value| CommandNotFoundHint::parse(&value))
        .unwrap_or_default();
    if let Ok(value) = std::env::var("NIU_COMMAND_NOT_FOUND_HINT") {
        match CommandNotFoundHint::parse(&value) {
            Some(parsed) => hint = parsed,
            None => eprintln!("niubash: NIU_COMMAND_NOT_FOUND_HINT must be one of: off, wpm"),
        }
    }
    hint
}

/// Where the effective completion style came from. Display surfaces (doctor,
/// `niu config`) use it to say whether a value is the built-in default, a
/// persisted pick or an environment override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionStyleSource {
    /// No config file key and no env override.
    Default,
    /// `completion.style` in config.toml (env not set).
    ConfigFile,
    /// `NIU_COMPLETION_STYLE` set — wins over the config file.
    Env,
}

/// Resolve the effective TAB completion style: `NIU_COMPLETION_STYLE` env >
/// persisted `completion.style` (config.toml) > built-in default (column). An
/// unparsable env value warns once (the shell's only completion-style
/// diagnostic) and falls through to the lower-precedence source; an
/// unparsable file value silently falls through like every other config
/// degradation.
pub fn resolve_completion_style() -> (CompletionStyle, CompletionStyleSource) {
    let mut style = CompletionStyle::default();
    let mut source = CompletionStyleSource::Default;
    if let Some(parsed) = read_user_config()
        .completion
        .and_then(|completion| completion.style)
        .and_then(|value| CompletionStyle::parse(&value))
    {
        style = parsed;
        source = CompletionStyleSource::ConfigFile;
    }
    if let Ok(value) = std::env::var("NIU_COMPLETION_STYLE") {
        match CompletionStyle::parse(&value) {
            Some(parsed) => {
                style = parsed;
                source = CompletionStyleSource::Env;
            }
            None => eprintln!(
                "niubash: NIU_COMPLETION_STYLE must be one of: gnu, ide, column, list, inline"
            ),
        }
    }
    (style, source)
}

/// Persist `completion.style` (the `niu config set completion.style`
/// writer). Only that one key is touched; passing the current built-in
/// default removes the key again so the file never pins a stale default.
pub fn set_completion_style(style: CompletionStyle) -> anyhow::Result<PathBuf> {
    write_user_config(|doc| {
        ensure_schema(doc);
        if style == CompletionStyle::default() {
            remove_table_key(doc, "completion", "style");
            Ok(())
        } else {
            set_table_key(doc, "completion", "style", toml_edit::value(style.as_str()))
        }
    })
}

/// Persist `command-not-found-hint` (the `niu config set` writer). Only
/// that one key is touched; the built-in default removes the override.
pub fn set_command_not_found_hint(hint: CommandNotFoundHint) -> anyhow::Result<PathBuf> {
    write_user_config(|doc| {
        ensure_schema(doc);
        if hint == CommandNotFoundHint::default() {
            doc.remove(COMMAND_NOT_FOUND_HINT_KEY);
        } else {
            doc[COMMAND_NOT_FOUND_HINT_KEY] = toml_edit::value(hint.as_str());
        }
        Ok(())
    })
}

/// The string array at `table.key` in a config document, in either
/// section shape (a `[table]` header or an inline `table = {…}`); a
/// missing or wrong-typed slot reads as empty — the write path never
/// invents content it cannot see.
fn table_string_array_in_doc(doc: &toml_edit::DocumentMut, table: &str, key: &str) -> Vec<String> {
    let value = match doc.get(table) {
        Some(toml_edit::Item::Table(section)) => section
            .get(key)
            .and_then(|item| item.as_value())
            .and_then(|value| value.as_array()),
        Some(toml_edit::Item::Value(toml_edit::Value::InlineTable(section))) => {
            section.get(key).and_then(|value| value.as_array())
        }
        _ => None,
    };
    value
        .map(|array| {
            array
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Set `table.key` to a string array through [`set_table_key`], so the
/// section's shape and siblings survive; an empty array removes the
/// key again (the file never pins an empty rule list).
fn set_table_string_array(
    doc: &mut toml_edit::DocumentMut,
    table: &str,
    key: &str,
    values: &[String],
) -> anyhow::Result<()> {
    if values.is_empty() {
        remove_table_key(doc, table, key);
        return Ok(());
    }
    let mut array = toml_edit::Array::new();
    for value in values {
        array.push(value.as_str());
    }
    set_table_key(
        doc,
        table,
        key,
        toml_edit::Item::Value(toml_edit::Value::Array(array)),
    )
}

/// The raw `[keymap]` rule specs persisted in config.toml: the `bind`
/// argument strings and the `unbind` keyseqs, in file order. Parsing
/// is the loader's job (`crate::repl` against the engine's bind
/// parsers); a file problem degrades to no rules, like every read here.
pub fn resolve_keymap_rule_specs() -> (Vec<String>, Vec<String>) {
    let keymap = read_user_config().keymap;
    (
        keymap
            .as_ref()
            .and_then(|section| section.bind.clone())
            .unwrap_or_default(),
        keymap
            .and_then(|section| section.unbind)
            .unwrap_or_default(),
    )
}

/// Append one `[keymap].bind` rule (the `niu config set keymap.bind`
/// writer, niubash#630). The spec is validated by the same parser the
/// loader uses — a rule that can never apply is refused here, with the
/// parser's reason, instead of erroring at every shell startup.
/// Appending an already-present rule is a no-op; sibling keys and
/// sections are preserved like every surgical write in this file.
pub fn add_keymap_bind_rule(spec: &str) -> anyhow::Result<PathBuf> {
    crate::repl::parse_keymap_bind_rule(spec)
        .map_err(|reason| anyhow::anyhow!("invalid {KEYMAP_BIND_KEY} rule '{spec}': {reason}"))?;
    write_user_config(|doc| {
        ensure_schema(doc);
        let mut rules = table_string_array_in_doc(doc, "keymap", "bind");
        if !rules.iter().any(|rule| rule == spec) {
            rules.push(spec.to_string());
        }
        set_table_string_array(doc, "keymap", "bind", &rules)
    })
}

/// Append one `[keymap].unbind` entry (the `niu config set
/// keymap.unbind` writer, niubash#630). The entry must name a single
/// key the editor can bind — validated by the loader's parser, so a
/// chained or unparseable keyseq is refused with its reason.
pub fn add_keymap_unbind_rule(spec: &str) -> anyhow::Result<PathBuf> {
    crate::repl::parse_keymap_unbind_key(spec).map_err(|reason| {
        anyhow::anyhow!("invalid {KEYMAP_UNBIND_KEY} entry '{spec}': {reason}")
    })?;
    write_user_config(|doc| {
        ensure_schema(doc);
        let mut rules = table_string_array_in_doc(doc, "keymap", "unbind");
        if !rules.iter().any(|rule| rule == spec) {
            rules.push(spec.to_string());
        }
        set_table_string_array(doc, "keymap", "unbind", &rules)
    })
}

#[derive(Debug, Clone)]
pub struct FullConfig {
    pub shell: ShellConfig,
    pub editor: EditorConfig,
    pub history: HistoryConfig,
    pub menus: MenuConfig,
    pub aliases: HashMap<String, String>,
    pub completion_dirs: Vec<PathBuf>,
    pub completion_behavior: CompletionBehavior,
    pub winuxcmd_enabled: bool,
    pub hooks: HookConfig,
    pub autosuggest: AutosuggestConfig,
    pub syntax_highlighting: SyntaxHighlightConfig,
    pub runtime_completions: RuntimeCompletionConfig,
    pub native_widgets: NativeWidgetConfig,
}

impl Default for FullConfig {
    fn default() -> Self {
        Self {
            shell: ShellConfig {
                prompt_symbol: "%".to_string(),
                ..ShellConfig::default()
            },
            editor: EditorConfig::default(),
            history: HistoryConfig::default(),
            menus: MenuConfig::default(),
            aliases: HashMap::new(),
            completion_dirs: Vec::new(),
            completion_behavior: CompletionBehavior::default(),
            winuxcmd_enabled: true,
            hooks: HookConfig::default(),
            autosuggest: AutosuggestConfig::default(),
            syntax_highlighting: SyntaxHighlightConfig::default(),
            runtime_completions: RuntimeCompletionConfig::default(),
            native_widgets: NativeWidgetConfig::default(),
        }
    }
}

/// Return built-in defaults. User startup configuration belongs in `~/.niubashrc`.
pub fn load() -> FullConfig {
    FullConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn completion_style_parse_accepts_valid_values() {
        assert_eq!(CompletionStyle::parse("gnu"), Some(CompletionStyle::Gnu));
        assert_eq!(
            CompletionStyle::parse("readline"),
            Some(CompletionStyle::Gnu)
        );
        assert_eq!(CompletionStyle::parse("GNU"), Some(CompletionStyle::Gnu));
        assert_eq!(
            CompletionStyle::parse("column"),
            Some(CompletionStyle::Column)
        );
        assert_eq!(CompletionStyle::parse("col"), Some(CompletionStyle::Column));
        assert_eq!(
            CompletionStyle::parse("grid"),
            Some(CompletionStyle::Column)
        );
        assert_eq!(CompletionStyle::parse("list"), Some(CompletionStyle::List));
        assert_eq!(
            CompletionStyle::parse("vertical"),
            Some(CompletionStyle::List)
        );
        assert_eq!(
            CompletionStyle::parse("inline"),
            Some(CompletionStyle::Inline)
        );
        assert_eq!(
            CompletionStyle::parse("cycle"),
            Some(CompletionStyle::Inline)
        );
        assert_eq!(
            CompletionStyle::parse("menu"),
            Some(CompletionStyle::Inline)
        );
    }

    #[test]
    fn completion_style_parse_rejects_invalid() {
        assert_eq!(CompletionStyle::parse("invalid"), None);
        assert_eq!(CompletionStyle::parse(""), None);
        assert_eq!(
            CompletionStyle::parse("COLUMN"),
            Some(CompletionStyle::Column)
        );
    }

    #[test]
    fn completion_style_default_is_column() {
        // Owner ruling 2026-10-14: the factory TAB shape is the multi-per-row
        // grid (the GNU listing look, candidates immediately) — the 2026-10-12
        // Gnu flip read as "dead" at the prompt (silent first TAB when the
        // typed word already is the common prefix) and the Ide popup lost on
        // looks. Gnu remains an opt-in.
        assert_eq!(CompletionStyle::default(), CompletionStyle::Column);
    }

    #[test]
    fn completion_style_as_str_round_trips_through_parse() {
        for style in [
            CompletionStyle::Gnu,
            CompletionStyle::Ide,
            CompletionStyle::Column,
            CompletionStyle::List,
            CompletionStyle::Inline,
        ] {
            assert_eq!(CompletionStyle::parse(style.as_str()), Some(style));
        }
    }

    #[test]
    fn menu_config_default_completion_style() {
        let config = MenuConfig::default();
        assert_eq!(config.completion_style, CompletionStyle::Column);
    }

    #[test]
    fn completion_style_persists_and_env_wins_over_file() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = std::env::temp_dir().join(format!(
            "niu-config-style-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let previous_config = std::env::var_os("NIU_CONFIG");
        let previous_env = std::env::var_os("NIU_COMPLETION_STYLE");
        std::env::set_var("NIU_CONFIG", &temp);
        let result = (|| {
            // No file, no env: the built-in default.
            std::fs::remove_file(&temp).ok();
            assert_eq!(CompletionStyle::parse("gnu"), Some(CompletionStyle::Gnu));
            let (style, source) = resolve_completion_style();
            assert_eq!(style, CompletionStyle::Column);
            assert_eq!(source, CompletionStyleSource::Default);

            // Persisted file beats the default.
            set_completion_style(CompletionStyle::Ide)?;
            let (style, source) = resolve_completion_style();
            assert_eq!(style, CompletionStyle::Ide);
            assert_eq!(source, CompletionStyleSource::ConfigFile);
            // The written file parses as the documented schema and spells the
            // key as the dotted `completion.style` namespace.
            let text = std::fs::read_to_string(&temp)?;
            let parsed: UserConfigFile = toml::from_str(&text)?;
            assert_eq!(
                parsed.completion.and_then(|c| c.style).as_deref(),
                Some("ide")
            );

            // A second setter must not clobber the first key (merge write).
            set_command_not_found_hint(CommandNotFoundHint::Wpm)?;
            let text = std::fs::read_to_string(&temp)?;
            let parsed: UserConfigFile = toml::from_str(&text)?;
            assert_eq!(parsed.command_not_found_hint.as_deref(), Some("wpm"));
            assert_eq!(
                parsed.completion.and_then(|c| c.style).as_deref(),
                Some("ide")
            );

            // Env beats the file.
            std::env::set_var("NIU_COMPLETION_STYLE", "list");
            let (style, source) = resolve_completion_style();
            assert_eq!(style, CompletionStyle::List);
            assert_eq!(source, CompletionStyleSource::Env);

            // Setting the built-in default removes the key again.
            std::env::remove_var("NIU_COMPLETION_STYLE");
            set_completion_style(CompletionStyle::Column)?;
            let text = std::fs::read_to_string(&temp)?;
            let parsed: UserConfigFile = toml::from_str(&text)?;
            assert!(parsed.completion.is_none(), "default must not be pinned");

            // An unparsable file value falls through to the default, and an
            // unparsable env value warns and falls through too (tested by the
            // warning landing on stderr — the value result is what's pinned).
            std::fs::write(&temp, "[completion]\nstyle = \"winget\"").unwrap();
            let (style, source) = resolve_completion_style();
            assert_eq!(style, CompletionStyle::Column);
            assert_eq!(source, CompletionStyleSource::Default);
            Ok::<(), anyhow::Error>(())
        })();
        match previous_config {
            Some(value) => std::env::set_var("NIU_CONFIG", value),
            None => std::env::remove_var("NIU_CONFIG"),
        }
        match previous_env {
            Some(value) => std::env::set_var("NIU_COMPLETION_STYLE", value),
            None => std::env::remove_var("NIU_COMPLETION_STYLE"),
        }
        let _ = std::fs::remove_dir_all(
            temp.parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
        );
        let _ = std::fs::remove_file(&temp);
        result.unwrap();
    }

    /// niubash#630, K4: `[keymap]` rules persist by validated append —
    /// the specs round-trip through the typed read, duplicates are
    /// ignored, and sibling keys written before/after survive.
    #[test]
    fn issue630_keymap_rules_persist_append_and_roundtrip() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = std::env::temp_dir().join(format!(
            "niu-config-keymap-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let previous_config = std::env::var_os("NIU_CONFIG");
        std::env::set_var("NIU_CONFIG", &temp);
        let result = (|| {
            std::fs::remove_file(&temp).ok();
            assert_eq!(
                resolve_keymap_rule_specs(),
                (Vec::new(), Vec::new()),
                "no file, no rules"
            );
            // A sibling key first: the keymap writes must preserve it.
            set_completion_style(CompletionStyle::Ide)?;
            add_keymap_bind_rule(r#""\C-g": redisplay"#)?;
            assert_eq!(
                resolve_keymap_rule_specs().0,
                vec![r#""\C-g": redisplay"#.to_string()]
            );
            // Appending the same rule again is a no-op.
            add_keymap_bind_rule(r#""\C-g": redisplay"#)?;
            assert_eq!(resolve_keymap_rule_specs().0.len(), 1);
            add_keymap_bind_rule(r#"-x "\C-t": my-cmd"#)?;
            add_keymap_unbind_rule(r#""\C-a""#)?;
            let (bind, unbind) = resolve_keymap_rule_specs();
            assert_eq!(bind.len(), 2);
            assert_eq!(unbind, vec![r#""\C-a""#.to_string()]);
            // The typed view of the file: the section and the sibling
            // are both present, spelled as the documented namespace.
            let text = std::fs::read_to_string(&temp)?;
            let parsed: UserConfigFile = toml::from_str(&text)?;
            assert_eq!(
                parsed.completion.and_then(|c| c.style).as_deref(),
                Some("ide")
            );
            let keymap = parsed.keymap.expect("keymap section persisted");
            assert_eq!(keymap.bind.as_ref().map(Vec::len), Some(2));
            assert_eq!(keymap.unbind.as_ref().map(Vec::len), Some(1));
            Ok::<(), anyhow::Error>(())
        })();
        match previous_config {
            Some(value) => std::env::set_var("NIU_CONFIG", value),
            None => std::env::remove_var("NIU_CONFIG"),
        }
        let _ = std::fs::remove_file(&temp);
        result.unwrap();
    }

    /// niubash#630, K4: the writers validate with the loader's parser —
    /// a rule that can never apply is refused up front and nothing is
    /// written at all (no half-created config file).
    #[test]
    fn issue630_keymap_writers_refuse_unusable_rules() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = std::env::temp_dir().join(format!(
            "niu-config-keymap-refuse-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let previous_config = std::env::var_os("NIU_CONFIG");
        std::env::set_var("NIU_CONFIG", &temp);
        std::fs::remove_file(&temp).ok();
        let refused = [
            add_keymap_bind_rule("hello"),
            add_keymap_bind_rule(r#"-r "\C-t""#),
            add_keymap_bind_rule(r#""\C-g""#),
            add_keymap_unbind_rule(r#""\C-x\C-f""#),
            add_keymap_unbind_rule(""),
        ];
        match previous_config {
            Some(value) => std::env::set_var("NIU_CONFIG", value),
            None => std::env::remove_var("NIU_CONFIG"),
        }
        assert!(
            refused.iter().all(|result| result.is_err()),
            "every unusable rule is refused: {refused:?}"
        );
        assert!(!temp.exists(), "a refused rule never creates the file");
    }

    #[test]
    fn rc_policy_parse_and_defaults() {
        assert_eq!(RcPolicy::parse("primary-only"), Some(RcPolicy::PrimaryOnly));
        assert_eq!(
            RcPolicy::parse("bashrc-fallback"),
            Some(RcPolicy::BashrcFallback)
        );
        // Tolerated spellings, same namespace contract as the other keys.
        assert_eq!(
            RcPolicy::parse("  BASHRC-FALLBACK "),
            Some(RcPolicy::BashrcFallback)
        );
        assert_eq!(RcPolicy::default(), RcPolicy::PrimaryOnly);
        // Anything else fails closed at parse time too.
        assert_eq!(RcPolicy::parse("bashrc"), None);
        assert_eq!(RcPolicy::parse("fallback"), None);
        assert_eq!(RcPolicy::parse(""), None);
        assert_eq!(RcPolicy::parse("primary-only; rm -rf"), None);
        assert_eq!(RcPolicy::PrimaryOnly.as_str(), "primary-only");
        assert_eq!(RcPolicy::BashrcFallback.as_str(), "bashrc-fallback");
    }

    /// niubash#359 acceptance matrix, persistence rows: unknown/missing
    /// enum, wrong type, malformed TOML, unreadable config — every read
    /// problem fails closed to primary-only, never to fallback.
    #[test]
    fn resolve_rc_policy_fails_closed_matrix() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = std::env::temp_dir().join(format!(
            "niu-config-rcpolicy-matrix-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp.parent().unwrap()).unwrap();
        let previous = std::env::var_os("NIU_CONFIG");
        std::env::set_var("NIU_CONFIG", &temp);
        let result = {
            // No file at all.
            std::fs::remove_file(&temp).ok();
            assert_eq!(resolve_rc_policy(), RcPolicy::PrimaryOnly, "no file");
            // Missing key.
            std::fs::write(&temp, "schema = \"niubash:config@0.1.0\"\n").unwrap();
            assert_eq!(resolve_rc_policy(), RcPolicy::PrimaryOnly, "missing key");
            // Explicit primary-only.
            std::fs::write(&temp, "[startup]\nrc-policy = \"primary-only\"\n").unwrap();
            assert_eq!(
                resolve_rc_policy(),
                RcPolicy::PrimaryOnly,
                "explicit primary-only"
            );
            // Valid opt-in.
            std::fs::write(&temp, "[startup]\nrc-policy = \"bashrc-fallback\"\n").unwrap();
            assert_eq!(
                resolve_rc_policy(),
                RcPolicy::BashrcFallback,
                "valid opt-in"
            );
            // Unknown enum value.
            std::fs::write(&temp, "[startup]\nrc-policy = \"bashrc\"\n").unwrap();
            assert_eq!(resolve_rc_policy(), RcPolicy::PrimaryOnly, "unknown enum");
            // Wrong value type.
            std::fs::write(&temp, "[startup]\nrc-policy = 3\n").unwrap();
            assert_eq!(resolve_rc_policy(), RcPolicy::PrimaryOnly, "wrong type");
            // Wrong table type (the whole startup section unusable).
            std::fs::write(&temp, "startup = \"x\"\n").unwrap();
            assert_eq!(
                resolve_rc_policy(),
                RcPolicy::PrimaryOnly,
                "wrong table type"
            );
            // Duplicate key spellings (dotted + table) are malformed TOML.
            std::fs::write(
                &temp,
                "startup.rc-policy = \"bashrc-fallback\"\n[startup]\nrc-policy = \"bashrc-fallback\"\n",
            )
            .unwrap();
            assert_eq!(resolve_rc_policy(), RcPolicy::PrimaryOnly, "duplicate key");
            // Malformed TOML.
            std::fs::write(&temp, "[startup\nrc-policy = ").unwrap();
            assert_eq!(resolve_rc_policy(), RcPolicy::PrimaryOnly, "malformed toml");
            // A config DIRECTORY is unreadable as a file.
            std::fs::remove_file(&temp).unwrap();
            std::fs::create_dir_all(&temp).unwrap();
            assert_eq!(
                resolve_rc_policy(),
                RcPolicy::PrimaryOnly,
                "unreadable config"
            );
            Ok::<(), anyhow::Error>(())
        };
        match previous {
            Some(value) => std::env::set_var("NIU_CONFIG", value),
            None => std::env::remove_var("NIU_CONFIG"),
        }
        std::fs::remove_dir_all(&temp).ok();
        result.unwrap();
    }

    /// The niubash#359 writer contract: persisting one setting is surgical —
    /// comments, unknown keys, sibling sections, and the rc-policy enum all
    /// survive `niu config` writes untouched, and only explicit setup may
    /// change the policy.
    #[test]
    fn config_writes_preserve_comments_unknown_keys_and_rc_policy() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = std::env::temp_dir().join(format!(
            "niu-config-preserve-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let previous = std::env::var_os("NIU_CONFIG");
        std::env::set_var("NIU_CONFIG", &temp);
        let result = (|| {
            let original = "\n\
                # my own note, kept by hand\n\
                custom-flag = \"keep me\"\n\
                schema = \"niubash:config@0.0.9\"\n\
                [startup]\n\
                # a comment INSIDE the section, kept too\n\
                rc-policy = \"bashrc-fallback\"\n\
                [completion]\n\
                style = \"ide\"\n\
                unknown-in-section = 1\n\
            ";
            std::fs::write(&temp, original).unwrap();

            // A `niu config` write touches only its one key: the rc-policy
            // (owned by setup), the unknown keys, and the comments all
            // survive byte-for-byte.
            set_command_not_found_hint(CommandNotFoundHint::Wpm)?;
            let text = std::fs::read_to_string(&temp)?;
            assert!(text.contains("# my own note, kept by hand"), "{text}");
            assert!(text.contains("custom-flag = \"keep me\""), "{text}");
            assert!(text.contains("rc-policy = \"bashrc-fallback\""), "{text}");
            assert!(text.contains("# a comment INSIDE the section"), "{text}");
            assert!(text.contains("unknown-in-section = 1"), "{text}");
            assert_eq!(resolve_rc_policy(), RcPolicy::BashrcFallback);

            // Setting completion back to the default removes exactly that
            // key; the section's unknown key must stay (no table nuking).
            set_completion_style(CompletionStyle::Column)?;
            let text = std::fs::read_to_string(&temp)?;
            assert!(!text.contains("style = \"ide\""), "{text}");
            assert!(text.contains("unknown-in-section = 1"), "{text}");
            assert_eq!(resolve_rc_policy(), RcPolicy::BashrcFallback);

            // An inline-table section is the same namespace: the writer
            // preserves its shape and siblings instead of rewriting it.
            std::fs::write(
                &temp,
                "completion = { style = \"ide\", unknown-in-section = 1 }\n[startup]\nrc-policy = \"bashrc-fallback\"\n",
            )
            .unwrap();
            set_command_not_found_hint(CommandNotFoundHint::Off)?;
            let text = std::fs::read_to_string(&temp)?;
            assert!(
                text.contains("unknown-in-section = 1"),
                "inline sibling lost: {text}"
            );
            set_completion_style(CompletionStyle::List)?;
            let text = std::fs::read_to_string(&temp)?;
            assert!(text.contains("style = \"list\""), "{text}");
            assert!(
                text.contains("unknown-in-section = 1"),
                "inline sibling lost after set: {text}"
            );
            assert_eq!(resolve_rc_policy(), RcPolicy::BashrcFallback);
            Ok::<(), anyhow::Error>(())
        })();
        match previous {
            Some(value) => std::env::set_var("NIU_CONFIG", value),
            None => std::env::remove_var("NIU_CONFIG"),
        }
        let _ = std::fs::remove_file(&temp);
        result.unwrap();
    }

    /// `niu setup --rc-policy` is the only writer: round-trip select and
    /// revoke, no rc file involved (the path only names config.toml), and
    /// a failed persistence leaves the previous bytes AND policy intact.
    #[test]
    fn set_rc_policy_round_trips_and_fails_without_replacing() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = std::env::temp_dir().join(format!(
            "niu-config-rcpolicy-set-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let previous = std::env::var_os("NIU_CONFIG");
        std::env::set_var("NIU_CONFIG", &temp);
        let result = (|| {
            // Fresh write: the fallback opt-in persists…
            std::fs::remove_file(&temp).ok();
            let path = set_rc_policy(RcPolicy::BashrcFallback)?;
            assert_eq!(path, temp, "the writer reports the config path only");
            assert_eq!(resolve_rc_policy(), RcPolicy::BashrcFallback);
            // …and revoking via the built-in default removes the key.
            set_rc_policy(RcPolicy::PrimaryOnly)?;
            let text = std::fs::read_to_string(&temp)?;
            assert!(
                !text.contains("= \"bashrc-fallback\""),
                "the revoked key must be gone (notes comments may stay): {text}"
            );
            assert_eq!(resolve_rc_policy(), RcPolicy::PrimaryOnly);

            // Malformed config: the writer REFUSES and the bytes stay —
            // no defaults replacement, no repair (the contract's
            // atomic-publication row).
            let corrupt = "[startup\nthis is not toml";
            std::fs::write(&temp, corrupt).unwrap();
            let err = set_rc_policy(RcPolicy::BashrcFallback)
                .expect_err("a corrupt config must refuse to save");
            assert!(err.to_string().contains("not parseable TOML"), "{err:#}");
            assert_eq!(
                std::fs::read_to_string(&temp).unwrap(),
                corrupt,
                "the corrupt bytes must stay untouched"
            );
            assert_eq!(resolve_rc_policy(), RcPolicy::PrimaryOnly, "fail closed");

            // A wrong-typed startup section is a refusal too, never a
            // silent table replacement of user content.
            let wrong = "startup = \"mine\"\n";
            std::fs::write(&temp, wrong).unwrap();
            let err = set_rc_policy(RcPolicy::BashrcFallback)
                .expect_err("a wrong-typed startup must refuse to save");
            assert!(err.to_string().contains("not a table"), "{err:#}");
            assert_eq!(std::fs::read_to_string(&temp).unwrap(), wrong);
            Ok::<(), anyhow::Error>(())
        })();
        match previous {
            Some(value) => std::env::set_var("NIU_CONFIG", value),
            None => std::env::remove_var("NIU_CONFIG"),
        }
        let _ = std::fs::remove_file(&temp);
        result.unwrap();
    }

    #[test]
    fn command_not_found_hint_parse_accepts_known_values() {
        assert_eq!(
            CommandNotFoundHint::parse("wpm"),
            Some(CommandNotFoundHint::Wpm)
        );
        assert_eq!(
            CommandNotFoundHint::parse("off"),
            Some(CommandNotFoundHint::Off)
        );
        assert_eq!(
            CommandNotFoundHint::parse("none"),
            Some(CommandNotFoundHint::Off)
        );
        assert_eq!(
            CommandNotFoundHint::parse(" WPM "),
            Some(CommandNotFoundHint::Wpm)
        );
    }

    #[test]
    fn command_not_found_hint_parse_rejects_third_party_managers() {
        // niubash#249: third-party managers are not valid opt-ins — the
        // only enabled channel is niu's own wpm.
        assert_eq!(CommandNotFoundHint::parse("winget"), None);
        assert_eq!(CommandNotFoundHint::parse("scoop"), None);
        assert_eq!(CommandNotFoundHint::parse("choco"), None);
        assert_eq!(CommandNotFoundHint::parse(""), None);
    }

    #[test]
    fn command_not_found_hint_default_is_off() {
        assert_eq!(CommandNotFoundHint::default(), CommandNotFoundHint::Off);
    }

    #[test]
    fn user_config_file_round_trips_command_not_found_hint() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = std::env::temp_dir().join(format!(
            "niu-config-roundtrip-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let previous = std::env::var_os("NIU_CONFIG");
        std::env::set_var("NIU_CONFIG", &temp);
        let result = (|| {
            // Unset (no file yet): the default stands.
            std::fs::remove_file(&temp).ok();
            assert_eq!(resolve_command_not_found_hint(), CommandNotFoundHint::Off);

            set_command_not_found_hint(CommandNotFoundHint::Wpm)?;
            assert_eq!(resolve_command_not_found_hint(), CommandNotFoundHint::Wpm);
            // The written file parses as the documented schema.
            let text = std::fs::read_to_string(&temp)?;
            let parsed: UserConfigFile = toml::from_str(&text)?;
            assert_eq!(parsed.schema.as_deref(), Some(USER_CONFIG_SCHEMA));
            assert_eq!(parsed.command_not_found_hint.as_deref(), Some("wpm"));

            set_command_not_found_hint(CommandNotFoundHint::Off)?;
            assert_eq!(resolve_command_not_found_hint(), CommandNotFoundHint::Off);
            Ok::<(), anyhow::Error>(())
        })();
        match previous {
            Some(value) => std::env::set_var("NIU_CONFIG", value),
            None => std::env::remove_var("NIU_CONFIG"),
        }
        let _ = std::fs::remove_dir_all(
            temp.parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
        );
        let _ = std::fs::remove_file(&temp);
        result.unwrap();
    }

    #[test]
    fn user_config_unknown_and_invalid_values_degrade_to_off() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = std::env::temp_dir().join(format!(
            "niu-config-degrade-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp.parent().unwrap()).unwrap();
        let previous = std::env::var_os("NIU_CONFIG");
        std::env::set_var("NIU_CONFIG", &temp);
        let result = {
            // An unknown value degrades to the default, never to a hint.
            std::fs::write(&temp, "command-not-found-hint = \"winget\"").unwrap();
            assert_eq!(resolve_command_not_found_hint(), CommandNotFoundHint::Off);
            // Malformed TOML degrades too.
            std::fs::write(&temp, "[broken").unwrap();
            assert_eq!(resolve_command_not_found_hint(), CommandNotFoundHint::Off);
            Ok::<(), anyhow::Error>(())
        };
        match previous {
            Some(value) => std::env::set_var("NIU_CONFIG", value),
            None => std::env::remove_var("NIU_CONFIG"),
        }
        let _ = std::fs::remove_file(&temp);
        result.unwrap();
    }

    /// niubash#302: `HOME` alone must sandbox the config subsystem. With
    /// `NIU_CONFIG` unset, `HOME` pointed at an isolated dir and
    /// `USERPROFILE` deliberately left at a DIFFERENT dir, reads AND writes
    /// have to land under the isolated home — an explicit `HOME` override
    /// never loses to the system profile (it did before, when the resolver
    /// ranked `USERPROFILE` first on Windows, and a HOME-only sandbox
    /// silently leaked writes into the real profile).
    #[test]
    fn home_env_isolation_redirects_config_reads_and_writes() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let stamp = format!(
            "niu-config-isolation-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let isolated_home = std::env::temp_dir().join(format!("{stamp}-home"));
        let decoy_profile = std::env::temp_dir().join(format!("{stamp}-profile"));
        std::fs::create_dir_all(isolated_home.join(".niubash")).unwrap();
        std::fs::create_dir_all(decoy_profile.join(".niubash")).unwrap();

        let previous_niu_config = std::env::var_os("NIU_CONFIG");
        let previous_home = std::env::var_os("HOME");
        let previous_profile = std::env::var_os("USERPROFILE");
        std::env::remove_var("NIU_CONFIG");
        std::env::set_var("HOME", &isolated_home);
        std::env::set_var("USERPROFILE", &decoy_profile);
        let result = {
            // Write through the real writer: the file must appear under the
            // isolated home and NOWHERE near the decoy profile.
            let written =
                set_command_not_found_hint(CommandNotFoundHint::Wpm).expect("config write");
            assert_eq!(
                written,
                crate::path_utils::normalize_existing_host_path(isolated_home.clone())
                    .join(".niubash")
                    .join("config.toml"),
                "the write must land in the HOME sandbox"
            );
            assert!(
                !decoy_profile.join(".niubash").join("config.toml").exists(),
                "USERPROFILE profile was written to despite HOME isolation"
            );
            // Read back through the real resolver: the persisted value is
            // visible (so the read path resolves the same isolated home).
            assert_eq!(resolve_command_not_found_hint(), CommandNotFoundHint::Wpm);
            Ok::<(), anyhow::Error>(())
        };
        match previous_niu_config {
            Some(value) => std::env::set_var("NIU_CONFIG", value),
            None => std::env::remove_var("NIU_CONFIG"),
        }
        match previous_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match previous_profile {
            Some(value) => std::env::set_var("USERPROFILE", value),
            None => std::env::remove_var("USERPROFILE"),
        }
        let _ = std::fs::remove_dir_all(&isolated_home);
        let _ = std::fs::remove_dir_all(&decoy_profile);
        result.unwrap();
    }
}
