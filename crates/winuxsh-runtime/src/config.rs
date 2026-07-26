//! Configuration loading from `.winshrc.toml`

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::completion::{CompletionBehavior, CompletionMatchMode};
use crate::prompt::PromptIndicators;

/// Shell configuration, loaded from `~/.winshrc.toml`.
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

impl EditorMode {
    fn from_config_value(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "emacs" => Self::Emacs,
            "vi" => Self::Vi,
            other => {
                log::warn!("Unknown editor edit_mode '{}', falling back to emacs", other);
                Self::Emacs
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EditorConfig {
    pub edit_mode: EditorMode,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZshCompatLevel {
    Safe,
    Warn,
    Experimental,
}

impl Default for ZshCompatLevel {
    fn default() -> Self {
        Self::Safe
    }
}

impl ZshCompatLevel {
    fn from_config_value(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "safe" => Self::Safe,
            "warn" => Self::Warn,
            "experimental" => Self::Experimental,
            other => {
                log::warn!(
                    "Unknown zsh compat_level '{}', falling back to safe",
                    other
                );
                Self::Safe
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ZshConfig {
    pub enabled: bool,
    pub zdotdir: Option<PathBuf>,
    pub import_zshrc: bool,
    pub import_oh_my_zsh: bool,
    pub plugins: Vec<String>,
    pub compat_level: ZshCompatLevel,
    pub auto_apply: bool,
    pub autosuggestions: AutosuggestConfig,
    pub syntax_highlighting: SyntaxHighlightConfig,
    pub dynamic_completions: DynamicCompletionConfig,
    pub runtime_completions: RuntimeCompletionConfig,
    pub native_widgets: NativeWidgetConfig,
    pub native_plugins: NativePluginConfig,
}

impl Default for ZshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            zdotdir: None,
            import_zshrc: true,
            import_oh_my_zsh: true,
            plugins: Vec::new(),
            compat_level: ZshCompatLevel::Safe,
            auto_apply: false,
            autosuggestions: AutosuggestConfig::default(),
            syntax_highlighting: SyntaxHighlightConfig::default(),
            dynamic_completions: DynamicCompletionConfig::default(),
            runtime_completions: RuntimeCompletionConfig::default(),
            native_widgets: NativeWidgetConfig::default(),
            native_plugins: NativePluginConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicCompletionConfig {
    pub enabled: bool,
    pub commands: Vec<String>,
    pub timeout_millis: u64,
    pub cache_ttl_secs: Option<u64>,
    pub cache_dir: Option<PathBuf>,
}

impl Default for DynamicCompletionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            commands: Vec::new(),
            timeout_millis: 1500,
            cache_ttl_secs: Some(86400),
            cache_dir: None,
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
        if let Ok(value) = std::env::var("ZSH_AUTOSUGGEST_STRATEGY") {
            let strategies = parse_autosuggest_strategy_value(&value);
            if !strategies.is_empty() {
                self.strategies = strategies;
            }
        }
        if let Ok(value) = std::env::var("ZSH_AUTOSUGGEST_HIGHLIGHT_STYLE") {
            if !value.trim().is_empty() {
                self.highlight_style = value;
            }
        }
        if let Ok(value) = std::env::var("ZSH_AUTOSUGGEST_BUFFER_MAX_SIZE") {
            match value.trim().parse::<usize>() {
                Ok(max_size) => self.buffer_max_size = Some(max_size),
                Err(err) => log::warn!(
                    "Invalid ZSH_AUTOSUGGEST_BUFFER_MAX_SIZE '{}': {}",
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
        .map(|part| part.trim_matches('"').trim_matches('\'').to_ascii_lowercase())
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
        if let Ok(value) = std::env::var("ZSH_HIGHLIGHT_HIGHLIGHTERS") {
            let highlighters = parse_zsh_arrayish_value(&value);
            if !highlighters.is_empty() {
                self.highlighters = highlighters;
            }
        }
        if let Ok(value) = std::env::var("ZSH_HIGHLIGHT_MAXLENGTH") {
            match value.trim().parse::<usize>() {
                Ok(max_length) => self.max_length = Some(max_length),
                Err(err) => log::warn!("Invalid ZSH_HIGHLIGHT_MAXLENGTH '{}': {}", value, err),
            }
        }
        if let Ok(value) = std::env::var("ZSH_HIGHLIGHT_STYLES") {
            for (key, value) in parse_zsh_style_map_value(&value) {
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

fn parse_zsh_arrayish_value(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.trim_matches('"').trim_matches('\'').to_ascii_lowercase())
        .collect()
}

fn parse_zsh_style_map_value(value: &str) -> Vec<(String, String)> {
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

/// Top-level TOML structure.
#[derive(Debug, Deserialize)]
struct WinshrcToml {
    shell: Option<ShellToml>,
    theme: Option<ThemeToml>,
    editor: Option<EditorToml>,
    history: Option<HistoryToml>,
    menus: Option<MenusToml>,
    aliases: Option<HashMap<String, String>>,
    completions: Option<CompletionsToml>,
    winuxcmd: Option<WinuxCmdToml>,
    hooks: Option<HooksToml>,
    zsh: Option<ZshToml>,
    git_prompt: Option<GitPromptToml>,
}

#[derive(Debug, Deserialize)]
 struct ShellToml {
     prompt_format: Option<String>,
     prompt_symbol: Option<String>,
     right_prompt_format: Option<String>,
     git_prompt_format: Option<String>,
     prompt_indicator: Option<String>,
     emacs_indicator: Option<String>,
     vi_insert_indicator: Option<String>,
     vi_normal_indicator: Option<String>,
     multiline_indicator: Option<String>,
     history_search_indicator: Option<String>,
     history_search_fail_indicator: Option<String>,
    prompt_style: Option<String>,
    segment_preset: Option<String>,
    left_prompt_elements: Option<Vec<String>>,
    right_prompt_elements: Option<Vec<String>>,
 }

#[derive(Debug, Deserialize)]
struct ThemeToml {
    current_theme: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EditorToml {
    edit_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HistoryToml {
    path: Option<String>,
    max_size: Option<usize>,
    ignore_space_prefixed: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct MenusToml {
    completion_page_size: Option<usize>,
    history_page_size: Option<usize>,
    max_entry_lines: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CompletionsToml {
    completion_dirs: Option<Vec<String>>,
    case_sensitive: Option<bool>,
    matching: Option<String>,
    max_command_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct WinuxCmdToml {
    enabled: Option<bool>,
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HooksToml {
    precmd: Option<Vec<String>>,
    preexec: Option<Vec<String>>,
    chpwd: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ZshToml {
    enabled: Option<bool>,
    zdotdir: Option<String>,
    import_zshrc: Option<bool>,
    import_oh_my_zsh: Option<bool>,
    plugins: Option<Vec<String>>,
    compat_level: Option<String>,
    auto_apply: Option<bool>,
    autosuggestions: Option<AutosuggestToml>,
    syntax_highlighting: Option<SyntaxHighlightToml>,
    dynamic_completions: Option<DynamicCompletionToml>,
    runtime_completions: Option<RuntimeCompletionToml>,
    native_widgets: Option<NativeWidgetToml>,
    native_plugins: Option<NativePluginToml>,
}

#[derive(Debug, Deserialize)]
struct AutosuggestToml {
    enabled: Option<bool>,
    strategy: Option<Vec<String>>,
    highlight_style: Option<String>,
    buffer_max_size: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct SyntaxHighlightToml {
    enabled: Option<bool>,
    highlighters: Option<Vec<String>>,
    max_length: Option<usize>,
    styles: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct DynamicCompletionToml {
    enabled: Option<bool>,
    commands: Option<Vec<String>>,
    timeout_millis: Option<u64>,
    cache_ttl_secs: Option<u64>,
    cache_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RuntimeCompletionToml {
    enabled: Option<bool>,
    commands: Option<Vec<String>>,
    timeout_millis: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct NativeWidgetToml {
    enabled: Option<bool>,
    presets: Option<Vec<String>>,
    import_bindkeys: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct NativePluginToml {
    enabled: Option<bool>,
    presets: Option<Vec<String>>,
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
            // oh-my-zsh style: boolean symbols (no count by default).
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
    pub winuxcmd_path: Option<PathBuf>,
    pub hooks: HookConfig,
    pub zsh: ZshConfig,
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
            winuxcmd_path: None,
            hooks: HookConfig::default(), 
            zsh: ZshConfig::default(), 
            git_prompt: GitPromptConfig::default(),
        }
    }
}

/// Load config from `~/.winshrc.toml`. Returns defaults if the file
/// does not exist or cannot be parsed (logs warning).
#[derive(Debug, Deserialize)]
struct GitPromptToml {
    staged: Option<String>,
    unstaged: Option<String>,
    untracked: Option<String>,
    deleted: Option<String>,
    ahead: Option<String>,
    behind: Option<String>,
    stashes: Option<String>,
    conflicts: Option<String>,
    separator: Option<String>,
}

pub fn load() -> FullConfig {
    let config_path = default_config_path();

    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return FullConfig::default(),
    };

    let parsed: WinshrcToml = match toml::from_str(&content) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("Failed to parse {}: {}", config_path.display(), e);
            return FullConfig::default();
        }
    };

    build_config(parsed)
}

pub fn default_config_path() -> PathBuf {
    if let Ok(path) = std::env::var("WINUXSH_CONFIG") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".winshrc.toml")
}

fn build_config(parsed: WinshrcToml) -> FullConfig {
    let zsh = parsed.zsh.map(build_zsh_config).unwrap_or_default();
    let shell = parsed.shell;
    let completions = parsed.completions;
    let shell_config = ShellConfig {
        prompt_format: shell.as_ref().and_then(|s| s.prompt_format.clone()),
        right_prompt_format: shell.as_ref().and_then(|s| s.right_prompt_format.clone()),
        prompt_symbol: shell.as_ref().and_then(|s| s.prompt_symbol.clone()).unwrap_or_else(|| "%".to_string()),
        git_prompt_format: shell.as_ref().and_then(|s| s.git_prompt_format.clone()),
     prompt_indicators: shell
         .as_ref()
         .map(build_prompt_indicators)
         .unwrap_or_default(),
        prompt_style: shell.as_ref().and_then(|s| s.prompt_style.clone()),
        segment_preset: shell.as_ref().and_then(|s| s.segment_preset.clone()),
        left_prompt_elements: shell.as_ref().and_then(|s| s.left_prompt_elements.clone()),
        right_prompt_elements: shell.as_ref().and_then(|s| s.right_prompt_elements.clone()),
 };

    FullConfig {
        shell: shell_config,
        editor: EditorConfig {
            edit_mode: parsed
                .editor
                .and_then(|e| e.edit_mode)
                .map(|mode| EditorMode::from_config_value(&mode))
                .unwrap_or_default(),
        },
        history: parsed.history.map(build_history_config).unwrap_or_default(),
        menus: parsed.menus.map(build_menu_config).unwrap_or_default(),
        theme_name: parsed
            .theme
            .and_then(|t| t.current_theme)
            .unwrap_or_else(|| "default".to_string()),
        aliases: parsed.aliases.unwrap_or_default(),
        completion_dirs: completions
            .as_ref()
            .and_then(|c| c.completion_dirs.clone())
            .unwrap_or_default()
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        completion_behavior: completions
            .as_ref()
            .map(build_completion_behavior)
            .unwrap_or_default(),
        winuxcmd_enabled: parsed
            .winuxcmd
            .as_ref()
            .and_then(|w| w.enabled)
            .unwrap_or(true),
        winuxcmd_path: parsed.winuxcmd.and_then(|w| w.path).map(PathBuf::from),
        hooks: parsed.hooks.map(build_hook_config).unwrap_or_default(),
        zsh,
        git_prompt: build_git_prompt_config(parsed.git_prompt),
    }
}

fn build_git_prompt_config(parsed: Option<GitPromptToml>) -> GitPromptConfig {
    let defaults = GitPromptConfig::default();
    let Some(p) = parsed else { return defaults; };
    GitPromptConfig {
        staged: p.staged.unwrap_or(defaults.staged),
        unstaged: p.unstaged.unwrap_or(defaults.unstaged),
        untracked: p.untracked.unwrap_or(defaults.untracked),
        deleted: p.deleted.unwrap_or(defaults.deleted),
        ahead: p.ahead.unwrap_or(defaults.ahead),
        behind: p.behind.unwrap_or(defaults.behind),
        stashes: p.stashes.unwrap_or(defaults.stashes),
        conflicts: p.conflicts.unwrap_or(defaults.conflicts),
        separator: p.separator.unwrap_or(defaults.separator),
    }
}

fn build_completion_behavior(parsed: &CompletionsToml) -> CompletionBehavior {
    let defaults = CompletionBehavior::default();
    CompletionBehavior {
        case_sensitive: parsed.case_sensitive.unwrap_or(defaults.case_sensitive),
        match_mode: parsed
            .matching
            .as_deref()
            .map(completion_match_mode_from_config)
            .unwrap_or(defaults.match_mode),
        max_command_results: parsed
            .max_command_results
            .filter(|max_results| *max_results > 0),
    }
}

fn completion_match_mode_from_config(value: &str) -> CompletionMatchMode {
    match value.to_ascii_lowercase().as_str() {
        "prefix" => CompletionMatchMode::Prefix,
        "substring" => CompletionMatchMode::Substring,
        other => {
            log::warn!(
                "Unknown completions matching '{}', falling back to prefix",
                other
            );
            CompletionMatchMode::Prefix
        }
    }
}

fn build_history_config(parsed: HistoryToml) -> HistoryConfig {
    let defaults = HistoryConfig::default();
    HistoryConfig {
        path: parsed.path.as_deref().map(expand_tilde_path),
        max_size: parsed
            .max_size
            .filter(|max_size| *max_size > 0)
            .unwrap_or(defaults.max_size),
        ignore_space_prefixed: parsed
            .ignore_space_prefixed
            .unwrap_or(defaults.ignore_space_prefixed),
    }
}

fn build_menu_config(parsed: MenusToml) -> MenuConfig {
    let defaults = MenuConfig::default();
    MenuConfig {
        completion_page_size: parsed
            .completion_page_size
            .filter(|page_size| *page_size > 0)
            .unwrap_or(defaults.completion_page_size),
        history_page_size: parsed
            .history_page_size
            .filter(|page_size| *page_size > 0)
            .unwrap_or(defaults.history_page_size),
        max_entry_lines: parsed
            .max_entry_lines
            .filter(|max_lines| *max_lines > 0 && *max_lines <= u16::MAX as usize)
            .map(|max_lines| max_lines as u16)
            .unwrap_or(defaults.max_entry_lines),
    }
}

fn expand_tilde_path(value: &str) -> PathBuf {
    let home = || dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    if value == "~" {
        return home();
    }
    if let Some(rest) = value.strip_prefix("~/").or_else(|| value.strip_prefix("~\\")) {
        return home().join(rest);
    }
    PathBuf::from(value)
}

fn build_prompt_indicators(parsed: &ShellToml) -> PromptIndicators {
    let defaults = PromptIndicators::default();
    let prompt_indicator = parsed.prompt_indicator.clone().unwrap_or_default();
    PromptIndicators {
        default: prompt_indicator.clone(),
        emacs: parsed
            .emacs_indicator
            .clone()
            .unwrap_or_else(|| prompt_indicator.clone()),
        vi_insert: parsed
            .vi_insert_indicator
            .clone()
            .unwrap_or_else(|| prompt_indicator.clone()),
        vi_normal: parsed
            .vi_normal_indicator
            .clone()
            .unwrap_or_else(|| prompt_indicator.clone()),
        multiline: parsed
            .multiline_indicator
            .clone()
            .unwrap_or(defaults.multiline),
        history_search: parsed
            .history_search_indicator
            .clone()
            .unwrap_or(defaults.history_search),
        history_search_fail: parsed
            .history_search_fail_indicator
            .clone()
            .or_else(|| parsed.history_search_indicator.clone())
            .unwrap_or(defaults.history_search_fail),
    }
}

fn build_hook_config(parsed: HooksToml) -> HookConfig {
    HookConfig {
        precmd: parsed.precmd.unwrap_or_default(),
        preexec: parsed.preexec.unwrap_or_default(),
        chpwd: parsed.chpwd.unwrap_or_default(),
    }
}

fn build_zsh_config(parsed: ZshToml) -> ZshConfig {
    ZshConfig {
        enabled: parsed.enabled.unwrap_or(false),
        zdotdir: parsed.zdotdir.map(PathBuf::from),
        import_zshrc: parsed.import_zshrc.unwrap_or(true),
        import_oh_my_zsh: parsed.import_oh_my_zsh.unwrap_or(true),
        plugins: parsed.plugins.unwrap_or_default(),
        compat_level: parsed
            .compat_level
            .map(|level| ZshCompatLevel::from_config_value(&level))
            .unwrap_or_default(),
        auto_apply: parsed.auto_apply.unwrap_or(false),
        autosuggestions: parsed
            .autosuggestions
            .map(build_autosuggest_config)
            .unwrap_or_default(),
        syntax_highlighting: parsed
            .syntax_highlighting
            .map(build_syntax_highlight_config)
            .unwrap_or_default(),
        dynamic_completions: parsed
            .dynamic_completions
            .map(build_dynamic_completion_config)
            .unwrap_or_default(),
        runtime_completions: parsed
            .runtime_completions
            .map(build_runtime_completion_config)
            .unwrap_or_default(),
        native_widgets: parsed
            .native_widgets
            .map(build_native_widget_config)
            .unwrap_or_default(),
        native_plugins: parsed
            .native_plugins
            .map(build_native_plugin_config)
            .unwrap_or_default(),
    }
}

fn build_autosuggest_config(parsed: AutosuggestToml) -> AutosuggestConfig {
    let defaults = AutosuggestConfig::default();
    AutosuggestConfig {
        enabled: parsed.enabled.unwrap_or(defaults.enabled),
        strategies: parsed.strategy.unwrap_or(defaults.strategies),
        highlight_style: parsed.highlight_style.unwrap_or(defaults.highlight_style),
        buffer_max_size: parsed.buffer_max_size,
    }
}

fn build_syntax_highlight_config(parsed: SyntaxHighlightToml) -> SyntaxHighlightConfig {
    let defaults = SyntaxHighlightConfig::default();
    SyntaxHighlightConfig {
        enabled: parsed.enabled.unwrap_or(defaults.enabled),
        highlighters: parsed.highlighters.unwrap_or(defaults.highlighters),
        max_length: parsed.max_length,
        styles: parsed.styles.unwrap_or_default(),
    }
}

fn build_dynamic_completion_config(parsed: DynamicCompletionToml) -> DynamicCompletionConfig {
    let defaults = DynamicCompletionConfig::default();
    DynamicCompletionConfig {
        enabled: parsed.enabled.unwrap_or(defaults.enabled),
        commands: parsed.commands.unwrap_or(defaults.commands),
        timeout_millis: parsed.timeout_millis.unwrap_or(defaults.timeout_millis),
        cache_ttl_secs: parsed.cache_ttl_secs.or(defaults.cache_ttl_secs),
        cache_dir: parsed.cache_dir.map(PathBuf::from),
    }
}

fn build_runtime_completion_config(parsed: RuntimeCompletionToml) -> RuntimeCompletionConfig {
    let defaults = RuntimeCompletionConfig::default();
    RuntimeCompletionConfig {
        enabled: parsed.enabled.unwrap_or(defaults.enabled),
        commands: parsed.commands.unwrap_or(defaults.commands),
        timeout_millis: parsed.timeout_millis.unwrap_or(defaults.timeout_millis),
    }
}

fn build_native_widget_config(parsed: NativeWidgetToml) -> NativeWidgetConfig {
    let defaults = NativeWidgetConfig::default();
    NativeWidgetConfig {
        enabled: parsed.enabled.unwrap_or(defaults.enabled),
        presets: parsed.presets.unwrap_or(defaults.presets),
        import_bindkeys: parsed.import_bindkeys.unwrap_or(defaults.import_bindkeys),
    }
}

fn build_native_plugin_config(parsed: NativePluginToml) -> NativePluginConfig {
    let defaults = NativePluginConfig::default();
    NativePluginConfig {
        enabled: parsed.enabled.unwrap_or(defaults.enabled),
        presets: parsed.presets.unwrap_or(defaults.presets),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_config(input: &str) -> FullConfig {
        build_config(toml::from_str(input).unwrap())
    }

    #[test]
    fn defaults_to_emacs_edit_mode() {
        let config = parse_config("");
        assert_eq!(config.editor.edit_mode, EditorMode::Emacs);
    }

    #[test]
    fn parses_vi_edit_mode() {
        let config = parse_config(
            r#"
[editor]
edit_mode = "vi"
"#,
        );
        assert_eq!(config.editor.edit_mode, EditorMode::Vi);
    }

    #[test]
    fn defaults_history_config() {
        let config = parse_config("");
        assert_eq!(config.history, HistoryConfig::default());
    }

    #[test]
    fn parses_history_config_with_tilde_path() {
        let config = parse_config(
            r#"
[history]
path = "~/.custom_winuxsh_history"
max_size = 1234
ignore_space_prefixed = true
"#,
        );
        let expected_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".custom_winuxsh_history");
        assert_eq!(config.history.path, Some(expected_path));
        assert_eq!(config.history.max_size, 1234);
        assert!(config.history.ignore_space_prefixed);
    }

    #[test]
    fn zero_history_max_size_falls_back_to_default() {
        let config = parse_config(
            r#"
[history]
max_size = 0
"#,
        );
        assert_eq!(config.history.max_size, HistoryConfig::default().max_size);
    }

    #[test]
    fn defaults_menu_config() {
        let config = parse_config("");
        assert_eq!(config.menus, MenuConfig::default());
    }

    #[test]
    fn parses_menu_config() {
        let config = parse_config(
            r#"
[menus]
completion_page_size = 20
history_page_size = 30
max_entry_lines = 8
"#,
        );

        assert_eq!(
            config.menus,
            MenuConfig {
                completion_page_size: 20,
                history_page_size: 30,
                max_entry_lines: 8,
            }
        );
    }

    #[test]
    fn zero_menu_config_values_fall_back_to_defaults() {
        let config = parse_config(
            r#"
[menus]
completion_page_size = 0
history_page_size = 0
max_entry_lines = 0
"#,
        );

        assert_eq!(config.menus, MenuConfig::default());
    }

    #[test]
    fn defaults_completion_behavior() {
        let config = parse_config("");
        assert_eq!(config.completion_behavior, CompletionBehavior::default());
    }

    #[test]
    fn parses_completion_behavior_config() {
        let config = parse_config(
            r#"
[completions]
completion_dirs = ["C:/Users/me/completions"]
case_sensitive = true
matching = "substring"
max_command_results = 25
"#,
        );

        assert_eq!(
            config.completion_dirs,
            vec![PathBuf::from("C:/Users/me/completions")]
        );
        assert!(config.completion_behavior.case_sensitive);
        assert_eq!(
            config.completion_behavior.match_mode,
            CompletionMatchMode::Substring
        );
        assert_eq!(config.completion_behavior.max_command_results, Some(25));
    }

    #[test]
    fn unknown_completion_matching_falls_back_to_prefix() {
        let config = parse_config(
            r#"
[completions]
matching = "fuzzy"
max_command_results = 0
"#,
        );

        assert_eq!(
            config.completion_behavior.match_mode,
            CompletionMatchMode::Prefix
        );
        assert_eq!(config.completion_behavior.max_command_results, None);
    }

    #[test]
    fn parses_prompt_formats() {
        let config = parse_config(
            r#"
[shell]
prompt_format = "{cwd} %# "
right_prompt_format = "{user}@{host}"
"#,
        );

        assert_eq!(config.shell.prompt_format.as_deref(), Some("{cwd} %# "));
        assert_eq!(
            config.shell.right_prompt_format.as_deref(),
            Some("{user}@{host}")
        );
    }

    #[test]
    fn parses_prompt_indicators() {
        let config = parse_config(
            r#"
[shell]
prompt_indicator = "D "
emacs_indicator = "E "
vi_insert_indicator = "I "
vi_normal_indicator = "N "
multiline_indicator = "M "
history_search_indicator = "search:{term}:{status} "
history_search_fail_indicator = "fail:{term}:{status} "
"#,
        );

        assert_eq!(config.shell.prompt_indicators.default, "D ");
        assert_eq!(config.shell.prompt_indicators.emacs, "E ");
        assert_eq!(config.shell.prompt_indicators.vi_insert, "I ");
        assert_eq!(config.shell.prompt_indicators.vi_normal, "N ");
        assert_eq!(config.shell.prompt_indicators.multiline, "M ");
        assert_eq!(
            config.shell.prompt_indicators.history_search,
            "search:{term}:{status} "
        );
        assert_eq!(
            config.shell.prompt_indicators.history_search_fail,
            "fail:{term}:{status} "
        );
    }

    #[test]
    fn prompt_indicator_falls_back_to_editor_modes() {
        let config = parse_config(
            r#"
[shell]
prompt_indicator = "$ "
history_search_indicator = "history:{term} "
"#,
        );

        assert_eq!(config.shell.prompt_indicators.default, "$ ");
        assert_eq!(config.shell.prompt_indicators.emacs, "$ ");
        assert_eq!(config.shell.prompt_indicators.vi_insert, "$ ");
        assert_eq!(config.shell.prompt_indicators.vi_normal, "$ ");
        assert_eq!(config.shell.prompt_indicators.history_search, "history:{term} ");
        assert_eq!(
            config.shell.prompt_indicators.history_search_fail,
            "history:{term} "
        );
    }
    #[test]
    fn unknown_edit_mode_falls_back_to_emacs() {
        let config = parse_config(
            r#"
[editor]
edit_mode = "unknown"
"#,
        );
        assert_eq!(config.editor.edit_mode, EditorMode::Emacs);
    }

    #[test]
    fn parses_native_lifecycle_hooks() {
        let config = parse_config(
            r#"
[hooks]
precmd = ["echo before prompt"]
preexec = ["echo before command"]
chpwd = ["echo directory changed"]
"#,
        );

        assert_eq!(config.hooks.precmd, vec!["echo before prompt"]);
        assert_eq!(config.hooks.preexec, vec!["echo before command"]);
        assert_eq!(config.hooks.chpwd, vec!["echo directory changed"]);
    }

    #[test]
    fn parses_winuxcmd_enabled_and_path() {
        let config = parse_config(
            r#"
[winuxcmd]
enabled = false
path = "D:/tools/winuxcmd/winuxcmd.exe"
"#,
        );

        assert!(!config.winuxcmd_enabled);
        assert_eq!(
            config.winuxcmd_path,
            Some(PathBuf::from("D:/tools/winuxcmd/winuxcmd.exe"))
        );
    }

    #[test]
    fn parses_zsh_config() {
        let config = parse_config(
            r#"
[zsh]
enabled = true
zdotdir = "C:/Users/me"
import_zshrc = false
import_oh_my_zsh = true
plugins = ["git", "zsh-autosuggestions"]
compat_level = "warn"
auto_apply = true

[zsh.autosuggestions]
enabled = true
strategy = ["history", "completion"]
highlight_style = "fg=#ff00ff,bg=cyan,bold,underline"
buffer_max_size = 20

[zsh.syntax_highlighting]
enabled = true
highlighters = ["main"]
max_length = 512

[zsh.syntax_highlighting.styles]
command = "fg=green,bold"
unknown-token = "fg=red,bold"

[zsh.dynamic_completions]
enabled = true
commands = ["docker", "kubectl"]
timeout_millis = 2000
cache_ttl_secs = 120
cache_dir = "C:/Users/me/.winuxsh/cache/zsh-completions"

[zsh.runtime_completions]
enabled = true
commands = ["npm"]
timeout_millis = 750

[zsh.native_widgets]
enabled = true
presets = ["autosuggestions", "history_substring_search"]
import_bindkeys = true

[zsh.native_plugins]
enabled = true
presets = ["direnv"]
"#,
        );

        assert!(config.zsh.enabled);
        assert_eq!(config.zsh.zdotdir, Some(PathBuf::from("C:/Users/me")));
        assert!(!config.zsh.import_zshrc);
        assert!(config.zsh.import_oh_my_zsh);
        assert_eq!(config.zsh.plugins, vec!["git", "zsh-autosuggestions"]);
        assert_eq!(config.zsh.compat_level, ZshCompatLevel::Warn);
        assert!(config.zsh.auto_apply);
        assert!(config.zsh.autosuggestions.enabled);
        assert_eq!(
            config.zsh.autosuggestions.strategies,
            vec!["history", "completion"]
        );
        assert_eq!(
            config.zsh.autosuggestions.highlight_style,
            "fg=#ff00ff,bg=cyan,bold,underline"
        );
        assert_eq!(config.zsh.autosuggestions.buffer_max_size, Some(20));
        assert!(config.zsh.syntax_highlighting.enabled);
        assert_eq!(config.zsh.syntax_highlighting.highlighters, vec!["main"]);
        assert_eq!(config.zsh.syntax_highlighting.max_length, Some(512));
        assert_eq!(
            config.zsh.syntax_highlighting.styles.get("command").unwrap(),
            "fg=green,bold"
        );
        assert!(config.zsh.dynamic_completions.enabled);
        assert_eq!(
            config.zsh.dynamic_completions.commands,
            vec!["docker", "kubectl"]
        );
        assert_eq!(config.zsh.dynamic_completions.timeout_millis, 2000);
        assert_eq!(config.zsh.dynamic_completions.cache_ttl_secs, Some(120));
        assert_eq!(
            config.zsh.dynamic_completions.cache_dir,
            Some(PathBuf::from("C:/Users/me/.winuxsh/cache/zsh-completions"))
        );
        assert!(config.zsh.runtime_completions.enabled);
        assert_eq!(config.zsh.runtime_completions.commands, vec!["npm"]);
        assert_eq!(config.zsh.runtime_completions.timeout_millis, 750);
        assert!(config.zsh.native_widgets.enabled);
        assert_eq!(
            config.zsh.native_widgets.presets,
            vec!["autosuggestions", "history_substring_search"]
        );
        assert!(config.zsh.native_widgets.import_bindkeys);
        assert!(config.zsh.native_plugins.enabled);
        assert_eq!(config.zsh.native_plugins.presets, vec!["direnv"]);
    }

    #[test]
    fn parses_zsh_autosuggest_strategy_env_style() {
        assert_eq!(
            parse_autosuggest_strategy_value("(history completion)"),
            vec!["history", "completion"]
        );
        assert_eq!(
            parse_autosuggest_strategy_value("history,match_prev_cmd"),
            vec!["history", "match_prev_cmd"]
        );
    }

    #[test]
    fn history_strategy_requires_enabled_history() {
        let mut config = AutosuggestConfig::default();
        assert!(config.history_strategy_enabled());

        config.enabled = false;
        assert!(!config.history_strategy_enabled());

        config.enabled = true;
        config.strategies = vec!["completion".to_string()];
        assert!(!config.history_strategy_enabled());
    }

    #[test]
    fn parses_zsh_highlight_array_and_style_map_values() {
        assert_eq!(
            parse_zsh_arrayish_value("(main brackets)"),
            vec!["main", "brackets"]
        );
        assert_eq!(
            parse_zsh_style_map_value("path=fg=cyan;command=fg=green,bold"),
            vec![
                ("path".to_string(), "fg=cyan".to_string()),
                ("command".to_string(), "fg=green,bold".to_string())
            ]
        );
    }

    #[test]
    fn main_highlighter_requires_enabled_main() {
        let mut config = SyntaxHighlightConfig::default();
        assert!(config.main_highlighter_enabled());

        config.enabled = false;
        assert!(!config.main_highlighter_enabled());

        config.enabled = true;
        config.highlighters = vec!["brackets".to_string()];
        assert!(!config.main_highlighter_enabled());
    }
}
