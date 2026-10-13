//! Runtime configuration defaults and environment overrides.

use std::collections::HashMap;
use std::path::PathBuf;

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookConfig {
    pub precmd: Vec<String>,
    pub preexec: Vec<String>,
    pub chpwd: Vec<String>,
    pub postcmd: Vec<String>,
    pub zshaddhistory: Vec<String>,
    pub zshexit: Vec<String>,
    pub greeting: Vec<String>,
    pub title: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMode {
    Emacs,
    Vi,
}

impl Default for EditorMode {
    fn default() -> Self {
        Self::Emacs
    }
}

impl EditorMode {}

#[derive(Debug, Clone, Default)]
pub struct EditorConfig {
    pub edit_mode: EditorMode,
}

/// History mode controlling how entries are shared across shell sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryMode {
    /// Live cross-session history, preserving the legacy behavior.
    Shared,
    /// Startup snapshot for navigation; builtins can see live file updates.
    Session,
    /// Startup snapshot for navigation; this session's later entries stay local.
    Private,
}

impl Default for HistoryMode {
    fn default() -> Self {
        Self::Shared
    }
}

impl HistoryMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "shared" => Some(Self::Shared),
            "session" => Some(Self::Session),
            "private" => Some(Self::Private),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryConfig {
    pub path: Option<PathBuf>,
    pub max_size: usize,
    pub mode: HistoryMode,
    pub ignore_space_prefixed: bool,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            path: None,
            max_size: 10000,
            mode: HistoryMode::default(),
            ignore_space_prefixed: false,
        }
    }
}

impl HistoryConfig {
    pub fn with_env_overrides(mut self) -> Self {
        if let Ok(value) = std::env::var("NIU_HISTORY_MODE") {
            if let Some(mode) = HistoryMode::parse(&value) {
                self.mode = mode;
            } else {
                eprintln!("niubash: NIU_HISTORY_MODE must be one of: shared, session, private");
            }
        }
        self
    }
}

/// Completion menu display style.
///
/// The default is [`CompletionStyle::Gnu`] (owner ruling 2026-10-12): TAB
/// completes the longest common prefix, and a second TAB on an unchanged
/// line prints the candidates as width-adaptive columns next to the prompt
/// and repaints it — a printed list, not an interactive menu. Arrow keys are
/// never captured and no popup is drawn, matching GNU readline's factory
/// behavior. The Ide popup remains available as
/// `NIU_COMPLETION_STYLE=ide` / `niu config set completion.style ide`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionStyle {
    /// GNU readline default: first TAB completes the common prefix (a lone
    /// candidate completes outright); an ambiguous second TAB lists the
    /// candidates columnar and repaints the prompt.
    Gnu,
    /// Multi-column popup with descriptions (VS Code style). The pre-1.4.1
    /// default, kept as an opt-in for existing users.
    Ide,
    /// Multi-column grid (like zsh `compinit`).
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

impl Default for CompletionStyle {
    fn default() -> Self {
        Self::Gnu
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
        // Resolution order (owner ruling 2026-10-12): NIU_COMPLETION_STYLE env
        // > persisted `completion.style` (config.toml) > the built-in default
        // (gnu). `resolve_completion_style` warns once on a bad env value.
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

/// The `[completion]` section of config.toml.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct CompletionConfigSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style: Option<String>,
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

/// Render the config file fresh from `config` (the mirrors.toml writer
/// convention): hand-added unknown keys or comments are not preserved, and
/// every known key the caller left unset is omitted.
fn write_user_config(config: &UserConfigFile) -> anyhow::Result<PathBuf> {
    let path = user_config_path();
    let mut text = String::new();
    text.push_str(&format!("schema = \"{USER_CONFIG_SCHEMA}\"\n"));
    if let Some(hint) = &config.command_not_found_hint {
        text.push_str(&format!("{COMMAND_NOT_FOUND_HINT_KEY} = \"{hint}\"\n"));
    }
    if let Some(completion) = &config.completion {
        if let Some(style) = &completion.style {
            // Rendered as the dotted key `completion.style`; the [completion]
            // table form the toml crate emits is the same namespace.
            text.push_str(&format!("\n[completion]\nstyle = \"{style}\"\n"));
        }
    }
    text.push_str(
        "\n\
        # ── notes ──────────────────────────────────────────────────────────\n\
        # `niu config set <key> <value>` rewrites this file; setting a key\n\
        # back to its default value removes the override again.\n\
        # command-not-found-hint: package-search suggestion printed after\n\
        #   \"command not found\". off (default) | wpm — wpm suggests one\n\
        #   `wpm search --name` line for high-confidence near-misses only;\n\
        #   third-party managers are never recommended (niubash#249).\n\
        # completion.style: TAB completion shape. gnu (default) | ide |\n\
        #   column | list | inline — `NIU_COMPLETION_STYLE` overrides this\n\
        #   file (env > config > default).\n",
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, text)?;
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
/// persisted `completion.style` (config.toml) > built-in default (gnu). An
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

/// Persist `completion.style` (the `niu config set completion.style` writer).
/// The file is rewritten with both known settings preserved; passing the
/// current built-in default removes the key again so the file never pins a
/// stale default.
pub fn set_completion_style(style: CompletionStyle) -> anyhow::Result<PathBuf> {
    let mut config = read_user_config();
    config.schema = Some(USER_CONFIG_SCHEMA.to_string());
    let is_default = style == CompletionStyle::default();
    if is_default {
        config.completion = None;
    } else {
        config.completion = Some(CompletionConfigSection {
            style: Some(style.as_str().to_string()),
        });
    }
    write_user_config(&config)
}

/// Persist `command-not-found-hint` (the `niu config set` writer). The
/// file renders fresh — hand-added unknown keys or comments are not
/// preserved, matching the mirrors.toml writer convention — and the other
/// known settings are carried over.
pub fn set_command_not_found_hint(hint: CommandNotFoundHint) -> anyhow::Result<PathBuf> {
    let mut config = read_user_config();
    config.schema = Some(USER_CONFIG_SCHEMA.to_string());
    let is_default = hint == CommandNotFoundHint::default();
    if is_default {
        config.command_not_found_hint = None;
    } else {
        config.command_not_found_hint = Some(hint.as_str().to_string());
    }
    write_user_config(&config)
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
    fn completion_style_default_is_gnu() {
        // Owner ruling 2026-10-12: the factory TAB completion shape is the GNU
        // readline default; the Ide popup moved to an opt-in.
        assert_eq!(CompletionStyle::default(), CompletionStyle::Gnu);
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
        assert_eq!(config.completion_style, CompletionStyle::Gnu);
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
            assert_eq!(style, CompletionStyle::Gnu);
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
            set_completion_style(CompletionStyle::Gnu)?;
            let text = std::fs::read_to_string(&temp)?;
            let parsed: UserConfigFile = toml::from_str(&text)?;
            assert!(parsed.completion.is_none(), "default must not be pinned");

            // An unparsable file value falls through to the default, and an
            // unparsable env value warns and falls through too (tested by the
            // warning landing on stderr — the value result is what's pinned).
            std::fs::write(&temp, "[completion]\nstyle = \"winget\"").unwrap();
            let (style, source) = resolve_completion_style();
            assert_eq!(style, CompletionStyle::Gnu);
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
        let result = (|| {
            // An unknown value degrades to the default, never to a hint.
            std::fs::write(&temp, "command-not-found-hint = \"winget\"").unwrap();
            assert_eq!(resolve_command_not_found_hint(), CommandNotFoundHint::Off);
            // Malformed TOML degrades too.
            std::fs::write(&temp, "[broken").unwrap();
            assert_eq!(resolve_command_not_found_hint(), CommandNotFoundHint::Off);
            Ok::<(), anyhow::Error>(())
        })();
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
        let result = (|| {
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
        })();
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
