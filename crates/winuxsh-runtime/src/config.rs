//! Runtime configuration defaults and environment overrides.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::completion::CompletionBehavior;

use crate::plugins::OFFICIAL_BUNDLE_NAME;
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
    /// Optional format for the git prompt segment. Supports `{git_branch}` and
    /// `{git_status}` placeholders, e.g. `git:({git_branch})`. When unset, the
    /// branch name is rendered on its own.
    pub git_prompt_format: Option<String>,
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
        if let Ok(value) = std::env::var("WINUXSH_HISTORY_MODE") {
            if let Some(mode) = HistoryMode::parse(&value) {
                self.mode = mode;
            } else {
                eprintln!("winuxsh: WINUXSH_HISTORY_MODE must be one of: shared, session, private");
            }
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuConfig {
    pub completion_page_size: usize,
    pub history_page_size: usize,
    pub max_entry_lines: u16,
}

impl Default for MenuConfig {
    fn default() -> Self {
        Self {
            completion_page_size: 10,
            history_page_size: 10,
            max_entry_lines: 5,
        }
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
pub struct NativePluginConfig {
    pub enabled: bool,
    pub presets: Vec<String>,
}

impl Default for NativePluginConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            presets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginConfig {
    pub enabled: bool,
    pub bundles: Vec<String>,
    pub load: Vec<String>,
    pub packs: HashMap<String, PluginPackConfig>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bundles: vec![OFFICIAL_BUNDLE_NAME.to_string()],
            load: Vec::new(),
            packs: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginPackConfig {
    pub enabled: Option<bool>,
    pub permissions: Vec<String>,
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
        if let Ok(value) = std::env::var("WINUXSH_AUTOSUGGEST_STRATEGY") {
            let strategies = parse_autosuggest_strategy_value(&value);
            if !strategies.is_empty() {
                self.strategies = strategies;
            }
        }
        if let Ok(value) = std::env::var("WINUXSH_AUTOSUGGEST_HIGHLIGHT_STYLE") {
            if !value.trim().is_empty() {
                self.highlight_style = value;
            }
        }
        if let Ok(value) = std::env::var("WINUXSH_AUTOSUGGEST_BUFFER_MAX_SIZE") {
            match value.trim().parse::<usize>() {
                Ok(max_size) => self.buffer_max_size = Some(max_size),
                Err(err) => log::warn!(
                    "Invalid WINUXSH_AUTOSUGGEST_BUFFER_MAX_SIZE '{}': {}",
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
        if let Ok(value) = std::env::var("WINUXSH_HIGHLIGHT_HIGHLIGHTERS") {
            let highlighters = parse_arrayish_value(&value);
            if !highlighters.is_empty() {
                self.highlighters = highlighters;
            }
        }
        if let Ok(value) = std::env::var("WINUXSH_HIGHLIGHT_MAXLENGTH") {
            match value.trim().parse::<usize>() {
                Ok(max_length) => self.max_length = Some(max_length),
                Err(err) => log::warn!("Invalid WINUXSH_HIGHLIGHT_MAXLENGTH '{}': {}", value, err),
            }
        }
        if let Ok(value) = std::env::var("WINUXSH_HIGHLIGHT_STYLES") {
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

/// User-configurable git prompt symbols.
///
/// Each field is a format string where `{n}` is replaced by the count.
/// Empty or missing fields inherit defaults; an empty string set
/// explicitly suppresses that segment (oh-my-posh / starship style).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPromptConfig {
    pub staged: String,
    pub unstaged: String,
    pub untracked: String,
    pub deleted: String,
    pub ahead: String,
    pub behind: String,
    pub stashes: String,
    pub conflicts: String,
    pub separator: String,
}

impl Default for GitPromptConfig {
    fn default() -> Self {
        Self {
            // Boolean symbols by default: no count unless the format uses {n}.
            // Users who want counts can set e.g. staged = "●{n}" explicitly.
            staged: "●".to_string(),
            unstaged: "✚".to_string(),
            untracked: "?".to_string(),
            deleted: "✖".to_string(),
            ahead: "↑".to_string(),
            behind: "↓".to_string(),
            stashes: "⚑".to_string(),
            conflicts: "✖".to_string(),
            separator: " ".to_string(),
        }
    }
}

use crate::git_status::GitPromptSymbols;

impl From<&GitPromptConfig> for GitPromptSymbols {
    fn from(cfg: &GitPromptConfig) -> Self {
        Self {
            staged: cfg.staged.clone(),
            unstaged: cfg.unstaged.clone(),
            untracked: cfg.untracked.clone(),
            deleted: cfg.deleted.clone(),
            ahead: cfg.ahead.clone(),
            behind: cfg.behind.clone(),
            stashes: cfg.stashes.clone(),
            conflicts: cfg.conflicts.clone(),
            separator: cfg.separator.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FullConfig {
    pub shell: ShellConfig,
    pub editor: EditorConfig,
    pub history: HistoryConfig,
    pub menus: MenuConfig,
    pub theme_name: String,
    pub aliases: HashMap<String, String>,
    pub completion_dirs: Vec<PathBuf>,
    pub completion_behavior: CompletionBehavior,
    pub winuxcmd_enabled: bool,
    pub hooks: HookConfig,
    pub plugins: PluginConfig,
    pub autosuggest: AutosuggestConfig,
    pub syntax_highlighting: SyntaxHighlightConfig,
    pub runtime_completions: RuntimeCompletionConfig,
    pub native_widgets: NativeWidgetConfig,
    pub native_plugins: NativePluginConfig,
    pub git_prompt: GitPromptConfig,
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
            theme_name: "default".to_string(),
            aliases: HashMap::new(),
            completion_dirs: Vec::new(),
            completion_behavior: CompletionBehavior::default(),
            winuxcmd_enabled: true,
            hooks: HookConfig::default(),
            plugins: PluginConfig::default(),
            autosuggest: AutosuggestConfig::default(),
            syntax_highlighting: SyntaxHighlightConfig::default(),
            runtime_completions: RuntimeCompletionConfig::default(),
            native_widgets: NativeWidgetConfig::default(),
            native_plugins: NativePluginConfig::default(),
            git_prompt: GitPromptConfig::default(),
        }
    }
}

/// Return built-in defaults. User startup configuration belongs in `~/.winuxshrc`.
pub fn load() -> FullConfig {
    FullConfig::default()
}
