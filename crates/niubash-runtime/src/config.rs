//! Runtime configuration defaults and their single sources of truth.
//!
//! Since 1.5.0 niubash has no `config.toml`. Every setting resolves from
//! exactly one place:
//!
//! * GNU-shaped knobs live in readline/rc: `bind`/`set`/`shopt` lines in
//!   `~/.niubashrc`, or `$INPUTRC` / `~/.inputrc`.
//! * niu-proprietary knobs live in `NIU_*` environment variables, normally
//!   exported from the rc.
//!
//! [`migrate_legacy_config`] is the one-time bridge for a pre-1.5.0
//! `~/.niubash/config.toml`; nothing on the runtime path reads that file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

impl EditorMode {
    /// Parse readline's `editing-mode` value (the engine normalizes it to
    /// lowercase before storing it).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "emacs" => Some(Self::Emacs),
            "vi" => Some(Self::Vi),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Emacs => "emacs",
            Self::Vi => "vi",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EditorConfig {
    pub edit_mode: EditorMode,
}

/// History configuration. There is deliberately no per-session sharing
/// mode anymore (niubash#572): the retired `NIU_HISTORY_MODE`
/// shared/session/private switch was a GNU-less invention whose documented
/// behaviors did not match the implementation. History now has exactly one
/// semantics, GNU's: one live file every session appends to and refreshes
/// from, with `history -a/-n/-r` as the explicit sync verbs.
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
/// behavior (`NIU_COMPLETION_STYLE=gnu`), and the Ide popup as `ide`.
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

    /// The canonical spelling used by env values and every display surface
    /// (`niu doctor`, `niu config`).
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

/// How TAB completion interacts with the line while candidates are on
/// display (niubash#631): the *interaction* axis, orthogonal to
/// [`CompletionStyle`], which is the *presentation* axis.
///
/// The default is [`CompletionInteraction::Cycle`] (owner ruling
/// 2026-10-28): TAB puts the first candidate into the line and further
/// TABs cycle through the candidate set; Enter accepts the candidate in
/// the line and runs the command; Esc rolls the line back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionInteraction {
    /// zsh menu-completion form: TAB cycles candidates into the line.
    #[default]
    Cycle,
    /// TAB completes the common prefix / opens the presentation menu,
    /// Enter confirms the pick, and Esc keeps what was inserted.
    Insert,
}

impl CompletionInteraction {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cycle" => Some(Self::Cycle),
            "insert" => Some(Self::Insert),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cycle => "cycle",
            Self::Insert => "insert",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuConfig {
    pub completion_page_size: usize,
    pub history_page_size: usize,
    pub max_entry_lines: u16,
    pub completion_style: CompletionStyle,
    pub completion_interaction: CompletionInteraction,
}

impl Default for MenuConfig {
    fn default() -> Self {
        Self {
            completion_page_size: 10,
            history_page_size: 10,
            max_entry_lines: 5,
            completion_style: CompletionStyle::default(),
            completion_interaction: CompletionInteraction::default(),
        }
    }
}

impl MenuConfig {
    pub fn with_env_overrides(mut self) -> Self {
        self.completion_style = resolve_completion_style().0;
        self.completion_interaction = resolve_completion_interaction().0;
        self
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
/// so the hint is off by default. The only opt-in channel names niu's own
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

/// Where an effective setting came from. `rc` lines are not a separate
/// source at read time: an rc `export NIU_*` is observed as [`Env`] after
/// the shell mirrors it into the process environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingSource {
    /// No override in effect.
    Default,
    /// A `NIU_*` environment variable.
    Env,
    /// A `$INPUTRC` / `~/.inputrc` readline variable.
    Inputrc,
}

impl SettingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Env => "env",
            Self::Inputrc => "inputrc",
        }
    }
}

/// User-facing config keys. The dotted spellings are retained from the
/// config.toml era so existing muscle memory keeps working; `niu config set`
/// no longer exists.
pub const COMMAND_NOT_FOUND_HINT_KEY: &str = "command-not-found-hint";
pub const COMPLETION_STYLE_KEY: &str = "completion.style";
pub const COMPLETION_INTERACTION_KEY: &str = "completion.interaction";
pub const RC_POLICY_KEY: &str = "startup.rc-policy";
pub const EDITOR_MODE_KEY: &str = "editing-mode";
pub const KEYSEQ_TIMEOUT_KEY: &str = "keyseq-timeout";

/// The environment variables behind the niu-proprietary knobs.
pub const COMMAND_NOT_FOUND_HINT_ENV: &str = "NIU_COMMAND_NOT_FOUND_HINT";
pub const COMPLETION_STYLE_ENV: &str = "NIU_COMPLETION_STYLE";
pub const COMPLETION_INTERACTION_ENV: &str = "NIU_COMPLETION_INTERACTION";
pub const RC_POLICY_ENV: &str = "NIU_RC_POLICY";

/// readline's `keyseq-timeout` default, in milliseconds.
pub const DEFAULT_KEYSEQ_TIMEOUT_MS: u64 = 500;

/// Which file may fill the default interactive personal-rc slot
/// (niubash#359). Persisted as the `NIU_RC_POLICY` environment variable;
/// every read problem — unknown value or absence — fails closed to
/// `primary-only`, and startup never repairs or rewrites that state.
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

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Resolve the effective command-not-found hint: `NIU_COMMAND_NOT_FOUND_HINT`
/// env > built-in default (off). An invalid value warns once and falls back
/// to the default — a config issue must never break the command-not-found
/// path.
pub fn resolve_command_not_found_hint_with_source() -> (CommandNotFoundHint, SettingSource) {
    if let Some(value) = env_value(COMMAND_NOT_FOUND_HINT_ENV) {
        match CommandNotFoundHint::parse(&value) {
            Some(hint) => return (hint, SettingSource::Env),
            None => eprintln!("niubash: {COMMAND_NOT_FOUND_HINT_ENV} must be one of: off, wpm"),
        }
    }
    (CommandNotFoundHint::default(), SettingSource::Default)
}

pub fn resolve_command_not_found_hint() -> CommandNotFoundHint {
    resolve_command_not_found_hint_with_source().0
}

/// Resolve the effective TAB completion style:
/// `NIU_COMPLETION_STYLE` env > built-in default (column).
pub fn resolve_completion_style() -> (CompletionStyle, SettingSource) {
    if let Some(value) = env_value(COMPLETION_STYLE_ENV) {
        match CompletionStyle::parse(&value) {
            Some(style) => return (style, SettingSource::Env),
            None => eprintln!(
                "niubash: {COMPLETION_STYLE_ENV} must be one of: gnu, ide, column, list, inline"
            ),
        }
    }
    (CompletionStyle::default(), SettingSource::Default)
}

/// Resolve the effective TAB completion interaction:
/// `NIU_COMPLETION_INTERACTION` env > built-in default (cycle).
pub fn resolve_completion_interaction() -> (CompletionInteraction, SettingSource) {
    if let Some(value) = env_value(COMPLETION_INTERACTION_ENV) {
        match CompletionInteraction::parse(&value) {
            Some(interaction) => return (interaction, SettingSource::Env),
            None => {
                eprintln!("niubash: {COMPLETION_INTERACTION_ENV} must be one of: insert, cycle")
            }
        }
    }
    (CompletionInteraction::default(), SettingSource::Default)
}

/// Resolve the effective personal-rc policy from `NIU_RC_POLICY` only.
/// Anything unparseable fails closed to `primary-only`.
pub fn resolve_rc_policy_with_source() -> (RcPolicy, SettingSource) {
    if let Some(value) = env_value(RC_POLICY_ENV) {
        match RcPolicy::parse(&value) {
            Some(policy) => return (policy, SettingSource::Env),
            None => {
                eprintln!("niubash: {RC_POLICY_ENV} must be one of: primary-only, bashrc-fallback")
            }
        }
    }
    (RcPolicy::default(), SettingSource::Default)
}

pub fn resolve_rc_policy() -> RcPolicy {
    resolve_rc_policy_with_source().0
}

/// The readline inputrc path: `$INPUTRC` when set and non-empty, else
/// `~/.inputrc`. Mirrors the engine's `initialize_readline` resolution.
pub fn inputrc_path() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("INPUTRC") {
        if !value.is_empty() {
            return Some(PathBuf::from(value));
        }
    }
    shell_home_dir().map(|home| home.join(".inputrc"))
}

/// The validated `set NAME VALUE` variables an inputrc file contributes,
/// in file order. Missing/unreadable files are silent, exactly like
/// readline's own startup read.
pub fn inputrc_variables() -> Vec<(String, String)> {
    let Some(path) = inputrc_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let term = std::env::var("TERM").ok();
    let env = rubash::builtins::bind::InputrcEnv {
        term: term.as_deref(),
        editing_mode: None,
    };
    rubash::builtins::bind::parse_inputrc_with_env(
        &text,
        &path.display().to_string(),
        &env,
        &mut |name| std::fs::read_to_string(name).ok(),
    )
    .variables
}

fn inputrc_variable(name: &str) -> Option<String> {
    inputrc_variables()
        .into_iter()
        .rev()
        .find(|(variable, _)| variable == name)
        .map(|(_, value)| value)
}

/// The effective editing mode as a standalone `niu config` process sees it:
/// `$INPUTRC` / `~/.inputrc` `set editing-mode`, else the default (emacs).
/// A running shell uses the engine's own `bind_editing_mode()` after
/// `initialize_readline` instead, which also sees rc `bind 'set ...'` lines.
pub fn resolve_editor_mode_from_inputrc() -> (EditorMode, SettingSource) {
    if let Some(mode) = inputrc_variable(EDITOR_MODE_KEY) {
        if let Some(parsed) = EditorMode::parse(&mode) {
            return (parsed, SettingSource::Inputrc);
        }
    }
    (EditorMode::default(), SettingSource::Default)
}

/// The effective `keyseq-timeout` as a standalone `niu config` process sees
/// it: `$INPUTRC` / `~/.inputrc`, else readline's 500 ms default.
pub fn resolve_keyseq_timeout_from_inputrc() -> (u64, SettingSource) {
    if let Some(value) = inputrc_variable(KEYSEQ_TIMEOUT_KEY) {
        if let Ok(parsed) = value.trim().parse::<u64>() {
            return (parsed, SettingSource::Inputrc);
        }
    }
    (DEFAULT_KEYSEQ_TIMEOUT_MS, SettingSource::Default)
}

/// One row of `niu config` / `niu config get` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    pub key: &'static str,
    pub value: String,
    pub source: SettingSource,
}

impl SettingRow {
    /// Stable one-line format: `key = value (source)`.
    pub fn render(&self) -> String {
        format!("{} = {} ({})", self.key, self.value, self.source.as_str())
    }
}

fn row(key: &'static str, value: impl Into<String>, source: SettingSource) -> SettingRow {
    SettingRow {
        key,
        value: value.into(),
        source,
    }
}

/// Every effective setting and its source, in stable order.
pub fn setting_rows() -> Vec<SettingRow> {
    let (hint, hint_source) = resolve_command_not_found_hint_with_source();
    let (style, style_source) = resolve_completion_style();
    let (interaction, interaction_source) = resolve_completion_interaction();
    let (policy, policy_source) = resolve_rc_policy_with_source();
    let (editor_mode, editor_source) = resolve_editor_mode_from_inputrc();
    let (timeout, timeout_source) = resolve_keyseq_timeout_from_inputrc();
    vec![
        row(COMMAND_NOT_FOUND_HINT_KEY, hint.as_str(), hint_source),
        row(COMPLETION_STYLE_KEY, style.as_str(), style_source),
        row(
            COMPLETION_INTERACTION_KEY,
            interaction.as_str(),
            interaction_source,
        ),
        row(RC_POLICY_KEY, policy.as_str(), policy_source),
        row(EDITOR_MODE_KEY, editor_mode.as_str(), editor_source),
        row(KEYSEQ_TIMEOUT_KEY, timeout.to_string(), timeout_source),
    ]
}

/// Look up one setting by its display key or its environment variable name.
pub fn setting_row(key: &str) -> Option<SettingRow> {
    let canonical = match key {
        COMMAND_NOT_FOUND_HINT_KEY | COMMAND_NOT_FOUND_HINT_ENV => COMMAND_NOT_FOUND_HINT_KEY,
        COMPLETION_STYLE_KEY | COMPLETION_STYLE_ENV => COMPLETION_STYLE_KEY,
        COMPLETION_INTERACTION_KEY | COMPLETION_INTERACTION_ENV => COMPLETION_INTERACTION_KEY,
        RC_POLICY_KEY | RC_POLICY_ENV => RC_POLICY_KEY,
        EDITOR_MODE_KEY => EDITOR_MODE_KEY,
        KEYSEQ_TIMEOUT_KEY => KEYSEQ_TIMEOUT_KEY,
        _ => return None,
    };
    setting_rows().into_iter().find(|row| row.key == canonical)
}

/// The legacy `config.toml` path. Nothing on the runtime path reads it;
/// [`migrate_legacy_config`] and the one-line startup notice do.
/// `$NIU_CONFIG` overrides the path (tests, portable setups); the default
/// is `~/.niubash/config.toml`.
pub fn legacy_config_path() -> PathBuf {
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

pub fn legacy_config_exists() -> bool {
    legacy_config_path().exists()
}

/// The one-line interactive notice for a leftover legacy file. Returns
/// `None` when there is nothing to migrate.
pub fn legacy_config_notice() -> Option<String> {
    let path = legacy_config_path();
    if !path.exists() {
        return None;
    }
    Some(format!(
        "niubash: {} is no longer read — run `niu config migrate` to move its \
         settings into ~/.niubashrc (the old file is then renamed)",
        path.display()
    ))
}

/// The primary rc every rc-policy/migration write targets.
pub fn primary_rc_path() -> PathBuf {
    shell_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".niubashrc")
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "-_./:".contains(ch))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The rc line that exports one niu-proprietary setting.
pub fn export_line(name: &str, value: &str) -> String {
    format!("export {name}={}", shell_quote(value))
}

/// The rc line that persists the personal-rc policy.
pub fn rc_policy_export_line(policy: RcPolicy) -> String {
    export_line(RC_POLICY_ENV, policy.as_str())
}

fn block_bounds(text: &str, marker: &str) -> Option<(usize, usize)> {
    let start_tag = format!("# >>> {marker} >>>");
    let end_tag = format!("# <<< {marker} <<<");
    let start = text.find(&start_tag)?;
    let end_rel = text[start..].find(&end_tag)?;
    let end = start + end_rel + end_tag.len();
    // Include the trailing newline in the replaced range when present.
    let end = if text[end..].starts_with('\n') {
        end + 1
    } else {
        end
    };
    Some((start, end))
}

fn render_block(marker: &str, comment: &str, lines: &[String]) -> String {
    let mut block = format!("# >>> {marker} >>>\n");
    if !comment.is_empty() {
        block.push_str(&format!("# {comment}\n"));
    }
    for line in lines {
        block.push_str(line);
        block.push('\n');
    }
    block.push_str(&format!("# <<< {marker} <<<\n"));
    block
}

/// Insert or replace one commented marker block in the primary rc. The
/// block is idempotent: a second run with the same lines changes nothing,
/// and a changed line rewrites only the marked region. An absent rc is
/// created for migration; callers that must not create the primary (the
/// bashrc-fallback policy) check [`primary_rc_path`] first.
pub fn upsert_primary_rc_block(
    marker: &str,
    comment: &str,
    lines: &[String],
) -> anyhow::Result<(PathBuf, bool)> {
    let path = primary_rc_path();
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("could not read {}", path.display())),
    };
    let block = render_block(marker, comment, lines);
    let updated = match block_bounds(&existing, marker) {
        Some((start, end)) => {
            let mut next = String::with_capacity(existing.len() + block.len());
            next.push_str(&existing[..start]);
            next.push_str(&block);
            next.push_str(&existing[end..]);
            next
        }
        None => {
            let mut next = existing.clone();
            if !next.is_empty() && !next.ends_with('\n') {
                next.push('\n');
            }
            if !next.is_empty() {
                next.push('\n');
            }
            next.push_str(&block);
            next
        }
    };
    if updated == existing {
        return Ok((path, false));
    }
    crate::path_utils::write_file_atomic(&path, updated.as_bytes())
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok((path, true))
}

/// Remove one marker block from the primary rc (revoking a setup pick).
/// Returns `(path, changed)`; an absent rc or block is a no-op.
pub fn remove_primary_rc_block(marker: &str) -> anyhow::Result<(PathBuf, bool)> {
    let path = primary_rc_path();
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((path, false)),
        Err(err) => return Err(err).with_context(|| format!("could not read {}", path.display())),
    };
    let Some((start, end)) = block_bounds(&existing, marker) else {
        return Ok((path, false));
    };
    let mut next = String::with_capacity(existing.len());
    next.push_str(&existing[..start]);
    next.push_str(&existing[end..]);
    crate::path_utils::write_file_atomic(&path, next.as_bytes())
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok((path, true))
}

/// How `niu setup --rc-policy` persisted (or could not persist) the pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RcPolicyWrite {
    /// The primary rc exists and now carries the policy block.
    Written(PathBuf),
    /// No primary rc exists, so writing one would suppress the very
    /// fallback being selected. The caller prints this export line instead.
    Instruction(String),
    /// The default policy needs no persistence.
    Default,
}

/// Persist an explicit `niu setup --rc-policy` pick into the primary rc.
/// The policy itself is read from the environment at startup, so the line
/// is an `export NIU_RC_POLICY=...` (a child session inherits it; a fresh
/// terminal needs the variable exported by the environment).
pub fn persist_rc_policy(policy: RcPolicy) -> anyhow::Result<RcPolicyWrite> {
    const MARKER: &str = "niu setup rc-policy";
    if policy == RcPolicy::default() {
        remove_primary_rc_block(MARKER)?;
        return Ok(RcPolicyWrite::Default);
    }
    if !primary_rc_path().exists() {
        return Ok(RcPolicyWrite::Instruction(rc_policy_export_line(policy)));
    }
    let lines = vec![rc_policy_export_line(policy)];
    let (path, _) = upsert_primary_rc_block(
        MARKER,
        "Personal-rc policy selected by `niu setup --rc-policy`.",
        &lines,
    )?;
    Ok(RcPolicyWrite::Written(path))
}

/// Result of a one-time legacy migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// The old config.toml that was consumed.
    pub legacy_path: PathBuf,
    /// The rc file the translated lines were written to.
    pub rc_path: PathBuf,
    /// Where the old file was moved (`None` when nothing was renamed).
    pub backup_path: Option<PathBuf>,
    /// The rc lines the migration produced.
    pub lines: Vec<String>,
    /// Legacy keys/values that could not be translated.
    pub skipped: Vec<String>,
    /// Whether the marked rc block was created or changed.
    pub rc_changed: bool,
}

#[derive(Debug, Default, serde::Deserialize)]
struct LegacyConfigFile {
    #[serde(rename = "command-not-found-hint", default)]
    command_not_found_hint: Option<String>,
    #[serde(default)]
    completion: Option<LegacyCompletionSection>,
    #[serde(default)]
    startup: Option<LegacyStartupSection>,
    #[serde(default)]
    keymap: Option<LegacyKeymapSection>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct LegacyCompletionSection {
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    interaction: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct LegacyStartupSection {
    #[serde(rename = "rc-policy", default)]
    rc_policy: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct LegacyKeymapSection {
    #[serde(default)]
    bind: Option<Vec<String>>,
    #[serde(default)]
    unbind: Option<Vec<String>>,
}

fn legacy_lines(config: &LegacyConfigFile, skipped: &mut Vec<String>) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(value) = config.command_not_found_hint.as_deref() {
        match CommandNotFoundHint::parse(value) {
            Some(hint) => lines.push(export_line(COMMAND_NOT_FOUND_HINT_ENV, hint.as_str())),
            None => skipped.push(format!("{COMMAND_NOT_FOUND_HINT_KEY} = {value:?}")),
        }
    }
    if let Some(completion) = &config.completion {
        if let Some(value) = completion.style.as_deref() {
            match CompletionStyle::parse(value) {
                Some(style) => lines.push(export_line(COMPLETION_STYLE_ENV, style.as_str())),
                None => skipped.push(format!("{COMPLETION_STYLE_KEY} = {value:?}")),
            }
        }
        if let Some(value) = completion.interaction.as_deref() {
            match CompletionInteraction::parse(value) {
                Some(interaction) => lines.push(export_line(
                    COMPLETION_INTERACTION_ENV,
                    interaction.as_str(),
                )),
                None => skipped.push(format!("{COMPLETION_INTERACTION_KEY} = {value:?}")),
            }
        }
    }
    if let Some(startup) = &config.startup {
        if let Some(value) = startup.rc_policy.as_deref() {
            match RcPolicy::parse(value) {
                Some(policy) => lines.push(rc_policy_export_line(policy)),
                None => skipped.push(format!("{RC_POLICY_KEY} = {value:?}")),
            }
        }
    }
    if let Some(keymap) = &config.keymap {
        for spec in keymap.bind.iter().flatten() {
            match crate::repl::parse_keymap_bind_rule(spec) {
                Ok(_) => lines.push(format!("bind {}", shell_quote(spec))),
                Err(reason) => skipped.push(format!("keymap.bind {spec:?}: {reason}")),
            }
        }
        for key in keymap.unbind.iter().flatten() {
            match crate::repl::parse_keymap_unbind_key(key) {
                Ok(_) => lines.push(format!("bind -r {}", shell_quote(key))),
                Err(reason) => skipped.push(format!("keymap.unbind {key:?}: {reason}")),
            }
        }
    }
    lines
}

/// One-time migration from `~/.niubash/config.toml`:
///
/// 1. translate GNU-shaped keys to `bind`/`bind -r` rc lines,
/// 2. translate niu-proprietary keys to `export NIU_*` rc lines,
/// 3. append/update a commented marker block in `~/.niubashrc`,
/// 4. rename the old file to `config.toml.migrated-<UTC timestamp>`.
///
/// A missing file returns `Ok(None)`. A malformed file is refused and
/// left untouched. Re-running after a successful migration finds no
/// config.toml and appends nothing; re-running with the same marker
/// updates the block in place instead of duplicating it.
pub fn migrate_legacy_config() -> anyhow::Result<Option<MigrationReport>> {
    let legacy_path = legacy_config_path();
    let text = match std::fs::read_to_string(&legacy_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("could not read {}", legacy_path.display()))
        }
    };
    let parsed: LegacyConfigFile = toml::from_str(&text).with_context(|| {
        format!(
            "could not parse {} — fix or remove the file by hand; niu never \
             rewrites a config it cannot read",
            legacy_path.display()
        )
    })?;
    let mut skipped = Vec::new();
    let lines = legacy_lines(&parsed, &mut skipped);
    let rc_path = primary_rc_path();
    let rc_changed = if lines.is_empty() {
        false
    } else {
        let (_, changed) = upsert_primary_rc_block(
            "niu config migrate",
            "Migrated from the retired ~/.niubash/config.toml (1.5.0).",
            &lines,
        )?;
        changed
    };
    let backup_path = rename_legacy_config(&legacy_path)?;
    Ok(Some(MigrationReport {
        legacy_path,
        rc_path,
        backup_path: Some(backup_path),
        lines,
        skipped,
        rc_changed,
    }))
}

fn rename_legacy_config(path: &Path) -> anyhow::Result<PathBuf> {
    let stamp = utc_timestamp();
    let mut candidate = PathBuf::from(format!("{}.migrated-{stamp}", path.display()));
    let mut suffix = 1u32;
    while candidate.exists() {
        candidate = PathBuf::from(format!("{}.migrated-{stamp}-{suffix}", path.display()));
        suffix += 1;
    }
    std::fs::rename(path, &candidate).with_context(|| {
        format!(
            "could not rename {} to {}",
            path.display(),
            candidate.display()
        )
    })?;
    Ok(candidate)
}

/// UTC timestamp shaped for a filename: `YYYYMMDD-HHMMSSZ`.
fn utc_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's days-from-civil inverse (proleptic Gregorian).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(m <= 2);
    (year, m as u32, d as u32)
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

    struct EnvRestore {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvRestore {
        fn capture(names: &[&'static str]) -> Self {
            Self {
                saved: names
                    .iter()
                    .map(|name| (*name, std::env::var_os(name)))
                    .collect(),
            }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (name, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "niu-config-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

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
        assert_eq!(config.completion_interaction, CompletionInteraction::Cycle);
    }

    #[test]
    fn completion_style_env_resolves_and_invalid_falls_back() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _restore = EnvRestore::capture(&[COMPLETION_STYLE_ENV]);
        std::env::remove_var(COMPLETION_STYLE_ENV);
        assert_eq!(
            resolve_completion_style(),
            (CompletionStyle::Column, SettingSource::Default)
        );
        std::env::set_var(COMPLETION_STYLE_ENV, "list");
        assert_eq!(
            resolve_completion_style(),
            (CompletionStyle::List, SettingSource::Env)
        );
        std::env::set_var(COMPLETION_STYLE_ENV, "winget");
        assert_eq!(
            resolve_completion_style(),
            (CompletionStyle::Column, SettingSource::Default),
            "an invalid env value falls back to the built-in default"
        );
    }

    #[test]
    fn completion_interaction_env_resolves_and_invalid_falls_back() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _restore = EnvRestore::capture(&[COMPLETION_INTERACTION_ENV]);
        std::env::remove_var(COMPLETION_INTERACTION_ENV);
        assert_eq!(
            resolve_completion_interaction(),
            (CompletionInteraction::Cycle, SettingSource::Default)
        );
        std::env::set_var(COMPLETION_INTERACTION_ENV, "insert");
        assert_eq!(
            resolve_completion_interaction(),
            (CompletionInteraction::Insert, SettingSource::Env)
        );
        std::env::set_var(COMPLETION_INTERACTION_ENV, "spiral");
        assert_eq!(
            resolve_completion_interaction(),
            (CompletionInteraction::Cycle, SettingSource::Default)
        );
    }

    #[test]
    fn command_not_found_hint_env_resolves_and_invalid_falls_back() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _restore = EnvRestore::capture(&[COMMAND_NOT_FOUND_HINT_ENV]);
        std::env::remove_var(COMMAND_NOT_FOUND_HINT_ENV);
        assert_eq!(
            resolve_command_not_found_hint_with_source(),
            (CommandNotFoundHint::Off, SettingSource::Default)
        );
        std::env::set_var(COMMAND_NOT_FOUND_HINT_ENV, "wpm");
        assert_eq!(
            resolve_command_not_found_hint_with_source(),
            (CommandNotFoundHint::Wpm, SettingSource::Env)
        );
        std::env::set_var(COMMAND_NOT_FOUND_HINT_ENV, "winget");
        assert_eq!(
            resolve_command_not_found_hint_with_source(),
            (CommandNotFoundHint::Off, SettingSource::Default)
        );
    }

    #[test]
    fn rc_policy_env_resolves_and_invalid_fails_closed() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _restore = EnvRestore::capture(&[RC_POLICY_ENV]);
        std::env::remove_var(RC_POLICY_ENV);
        assert_eq!(
            resolve_rc_policy_with_source(),
            (RcPolicy::PrimaryOnly, SettingSource::Default)
        );
        std::env::set_var(RC_POLICY_ENV, "bashrc-fallback");
        assert_eq!(
            resolve_rc_policy_with_source(),
            (RcPolicy::BashrcFallback, SettingSource::Env)
        );
        for invalid in ["bashrc", "fallback", "", "primary-only; rm -rf"] {
            std::env::set_var(RC_POLICY_ENV, invalid);
            assert_eq!(
                resolve_rc_policy_with_source(),
                (RcPolicy::PrimaryOnly, SettingSource::Default),
                "{invalid:?} must fail closed"
            );
        }
    }

    #[test]
    fn inputrc_sets_editor_mode_and_keyseq_timeout() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = temp_dir("inputrc");
        std::fs::create_dir_all(&home).unwrap();
        let _restore = EnvRestore::capture(&["HOME", "INPUTRC", "TERM"]);
        std::env::set_var("HOME", &home);
        std::env::remove_var("INPUTRC");
        std::env::remove_var("TERM");
        std::fs::write(
            home.join(".inputrc"),
            "set editing-mode vi\nset keyseq-timeout 120\n\"\\C-g\": redisplay\n",
        )
        .unwrap();
        assert_eq!(
            resolve_editor_mode_from_inputrc(),
            (EditorMode::Vi, SettingSource::Inputrc)
        );
        assert_eq!(
            resolve_keyseq_timeout_from_inputrc(),
            (120, SettingSource::Inputrc)
        );
        // $INPUTRC wins over ~/.inputrc, exactly like readline.
        let other = home.join("custom.inputrc");
        std::fs::write(&other, "set editing-mode emacs\nset keyseq-timeout 250\n").unwrap();
        std::env::set_var("INPUTRC", &other);
        assert_eq!(
            resolve_editor_mode_from_inputrc(),
            (EditorMode::Emacs, SettingSource::Inputrc)
        );
        assert_eq!(
            resolve_keyseq_timeout_from_inputrc(),
            (250, SettingSource::Inputrc)
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn inputrc_absent_keeps_defaults() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = temp_dir("no-inputrc");
        std::fs::create_dir_all(&home).unwrap();
        let _restore = EnvRestore::capture(&["HOME", "INPUTRC"]);
        std::env::set_var("HOME", &home);
        std::env::remove_var("INPUTRC");
        assert_eq!(
            resolve_editor_mode_from_inputrc(),
            (EditorMode::Emacs, SettingSource::Default)
        );
        assert_eq!(
            resolve_keyseq_timeout_from_inputrc(),
            (DEFAULT_KEYSEQ_TIMEOUT_MS, SettingSource::Default)
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn setting_rows_render_value_and_source() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = temp_dir("rows");
        std::fs::create_dir_all(&home).unwrap();
        let _restore = EnvRestore::capture(&[
            "HOME",
            "INPUTRC",
            COMPLETION_STYLE_ENV,
            COMPLETION_INTERACTION_ENV,
            COMMAND_NOT_FOUND_HINT_ENV,
            RC_POLICY_ENV,
        ]);
        std::env::set_var("HOME", &home);
        std::env::remove_var("INPUTRC");
        std::env::remove_var(COMPLETION_STYLE_ENV);
        std::env::remove_var(COMPLETION_INTERACTION_ENV);
        std::env::remove_var(COMMAND_NOT_FOUND_HINT_ENV);
        std::env::remove_var(RC_POLICY_ENV);
        std::fs::write(home.join(".inputrc"), "set editing-mode vi\n").unwrap();
        std::env::set_var(COMPLETION_STYLE_ENV, "ide");

        let rows = setting_rows();
        let style = rows
            .iter()
            .find(|row| row.key == COMPLETION_STYLE_KEY)
            .unwrap();
        assert_eq!(style.render(), "completion.style = ide (env)");
        let editor = rows.iter().find(|row| row.key == EDITOR_MODE_KEY).unwrap();
        assert_eq!(editor.render(), "editing-mode = vi (inputrc)");
        let policy = rows.iter().find(|row| row.key == RC_POLICY_KEY).unwrap();
        assert_eq!(
            policy.render(),
            "startup.rc-policy = primary-only (default)"
        );
        assert_eq!(setting_row(COMPLETION_STYLE_ENV).unwrap().value, "ide");
        assert!(setting_row("no.such.key").is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    fn write_legacy_config(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn migrate_translates_keys_appends_rc_and_renames_once() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = temp_dir("migrate");
        std::fs::create_dir_all(&home).unwrap();
        let legacy = home.join("config.toml");
        let _restore = EnvRestore::capture(&["HOME", "NIU_CONFIG"]);
        std::env::set_var("HOME", &home);
        std::env::set_var("NIU_CONFIG", &legacy);
        write_legacy_config(
            &legacy,
            r#"
schema = "niubash:config@0.1.0"
command-not-found-hint = "wpm"

[completion]
style = "ide"
interaction = "insert"

[startup]
rc-policy = "bashrc-fallback"

[keymap]
bind = ['"\C-g": redisplay', '-x "\C-t": my-cmd']
unbind = ['"\C-a"']
"#,
        );
        let report = migrate_legacy_config()
            .unwrap()
            .expect("legacy file exists");
        assert_eq!(report.legacy_path, legacy);
        assert_eq!(report.rc_path, home.join(".niubashrc"));
        assert!(report.rc_changed);
        assert_eq!(
            report.lines,
            vec![
                "export NIU_COMMAND_NOT_FOUND_HINT=wpm".to_string(),
                "export NIU_COMPLETION_STYLE=ide".to_string(),
                "export NIU_COMPLETION_INTERACTION=insert".to_string(),
                "export NIU_RC_POLICY=bashrc-fallback".to_string(),
                r#"bind '"\C-g": redisplay'"#.to_string(),
                r#"bind '-x "\C-t": my-cmd'"#.to_string(),
                r#"bind -r '"\C-a"'"#.to_string(),
            ]
        );
        assert!(!legacy.exists(), "the old file must be renamed");
        let backup = report.backup_path.unwrap();
        assert!(backup.exists());
        let backup_name = backup.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            backup_name.starts_with("config.toml.migrated-"),
            "{backup_name}"
        );
        let rc = std::fs::read_to_string(home.join(".niubashrc")).unwrap();
        assert!(rc.contains("# >>> niu config migrate >>>"), "{rc}");
        assert!(rc.contains("export NIU_COMPLETION_STYLE=ide"), "{rc}");
        assert!(rc.contains(r#"bind '"\C-g": redisplay'"#), "{rc}");
        assert!(rc.contains("# <<< niu config migrate <<<"), "{rc}");

        // Re-running after the rename finds nothing and appends nothing.
        assert!(migrate_legacy_config().unwrap().is_none());
        assert_eq!(
            std::fs::read_to_string(home.join(".niubashrc")).unwrap(),
            rc
        );

        // Re-running with the same marker (a restored file) updates in
        // place instead of duplicating the block.
        write_legacy_config(&legacy, "[completion]\nstyle = \"gnu\"\n");
        let report = migrate_legacy_config().unwrap().unwrap();
        let rc = std::fs::read_to_string(home.join(".niubashrc")).unwrap();
        assert_eq!(
            rc.matches("# >>> niu config migrate >>>").count(),
            1,
            "{rc}"
        );
        assert!(rc.contains("export NIU_COMPLETION_STYLE=gnu"), "{rc}");
        assert!(report.rc_changed);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn migrate_refuses_malformed_file_and_leaves_it_in_place() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = temp_dir("migrate-bad");
        std::fs::create_dir_all(&home).unwrap();
        let legacy = home.join("config.toml");
        let _restore = EnvRestore::capture(&["HOME", "NIU_CONFIG"]);
        std::env::set_var("HOME", &home);
        std::env::set_var("NIU_CONFIG", &legacy);
        write_legacy_config(&legacy, "[completion\nstyle = ");
        let err = migrate_legacy_config().expect_err("malformed config must refuse");
        assert!(format!("{err:#}").contains("could not parse"), "{err:#}");
        assert!(legacy.exists(), "the old bytes stay untouched");
        assert!(!home.join(".niubashrc").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn migrate_reports_unknown_values_as_skipped() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = temp_dir("migrate-skip");
        std::fs::create_dir_all(&home).unwrap();
        let legacy = home.join("config.toml");
        let _restore = EnvRestore::capture(&["HOME", "NIU_CONFIG"]);
        std::env::set_var("HOME", &home);
        std::env::set_var("NIU_CONFIG", &legacy);
        write_legacy_config(
            &legacy,
            "command-not-found-hint = \"winget\"\n[completion]\nstyle = \"nope\"\n",
        );
        let report = migrate_legacy_config().unwrap().unwrap();
        assert!(report.lines.is_empty());
        assert_eq!(report.skipped.len(), 2);
        assert!(!home.join(".niubashrc").exists(), "nothing to write");
        assert!(!legacy.exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn persist_rc_policy_writes_marker_and_revokes_it() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = temp_dir("policy");
        std::fs::create_dir_all(&home).unwrap();
        let _restore = EnvRestore::capture(&["HOME"]);
        std::env::set_var("HOME", &home);
        // No primary rc: selecting fallback must not create one (that
        // would suppress the fallback it selects).
        assert_eq!(
            persist_rc_policy(RcPolicy::BashrcFallback).unwrap(),
            RcPolicyWrite::Instruction("export NIU_RC_POLICY=bashrc-fallback".to_string())
        );
        assert!(!home.join(".niubashrc").exists());

        std::fs::write(home.join(".niubashrc"), "# mine\n").unwrap();
        assert_eq!(
            persist_rc_policy(RcPolicy::BashrcFallback).unwrap(),
            RcPolicyWrite::Written(home.join(".niubashrc"))
        );
        let rc = std::fs::read_to_string(home.join(".niubashrc")).unwrap();
        assert!(rc.starts_with("# mine\n"), "{rc}");
        assert!(rc.contains("export NIU_RC_POLICY=bashrc-fallback"), "{rc}");
        assert_eq!(
            persist_rc_policy(RcPolicy::BashrcFallback).unwrap(),
            RcPolicyWrite::Written(home.join(".niubashrc"))
        );
        let rc_again = std::fs::read_to_string(home.join(".niubashrc")).unwrap();
        assert_eq!(rc, rc_again, "the same pick is idempotent");

        assert_eq!(
            persist_rc_policy(RcPolicy::PrimaryOnly).unwrap(),
            RcPolicyWrite::Default
        );
        let rc = std::fs::read_to_string(home.join(".niubashrc")).unwrap();
        assert!(!rc.contains("NIU_RC_POLICY"), "{rc}");
        assert!(rc.contains("# mine"), "{rc}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn utc_timestamp_is_filename_safe_and_ordered() {
        let stamp = utc_timestamp();
        assert_eq!(stamp.len(), 16, "{stamp}");
        assert!(stamp.ends_with('Z'));
        assert!(!stamp.contains(':'));
        // 2026-10-30 is a known civil date (day 0 = 1970-01-01).
        let (y, m, d) = civil_from_days(20_756);
        assert_eq!((y, m, d), (2026, 10, 30));
    }
}
