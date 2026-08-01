//! p10k-style segment-based prompt engine
//!
//! Replaces the template-based `WinuxshPrompt` with a powerlevel10k-inspired
//! segment system: ordered lists of prompt elements with foreground/background
//! colours, separated by powerline triangles (`\u{E0B0}` / `\u{E0B2}`).
//!
//! Five built-in presets (lean, classic, rainbow, pure, robbyrussell) provide
//! ready-to-use configurations. Users can also define custom element orders
//! via TOML.

use std::borrow::Cow;
use std::path::Path;

use nu_ansi_term::{Color, Style};
use reedline::{Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus};

use crate::git_status::{collect_for_prompt, GitPromptSymbols, GitRepoStatus};
use crate::prompt::{format_local_time, PromptIndicators};

// ---- Segment identifiers ----

/// Identifies a single prompt segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SegmentId {
    Dir,
    Vcs,
    Status,
    Time,
    PromptChar,
    OsIcon,
    Context,
    BackgroundJobs,
    CommandExecutionTime,
    Newline,
    Custom(String),
}

impl SegmentId {
    /// Parse from a config string (case-insensitive, accepts `-` and `_`).
    pub fn from_name(s: &str) -> Option<Self> {
        let normalised: String = s
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect();
        match normalised.as_str() {
            "dir" => Some(Self::Dir),
            "vcs" => Some(Self::Vcs),
            "status" => Some(Self::Status),
            "time" => Some(Self::Time),
            "promptchar" => Some(Self::PromptChar),
            "osicon" => Some(Self::OsIcon),
            "context" => Some(Self::Context),
            "backgroundjobs" => Some(Self::BackgroundJobs),
            "commandexecutiontime" | "cmdexectime" => Some(Self::CommandExecutionTime),
            "newline" => Some(Self::Newline),
            _ => None,
        }
    }
}

// ---- Rendered output types ----

/// A single rendered segment with colour metadata.
#[derive(Debug, Clone)]
pub struct RenderedSegment {
    pub content: String,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
}

/// One logical line of the prompt (split by `Newline` segments).
#[derive(Debug, Clone)]
pub struct PromptLine {
    pub segments: Vec<RenderedSegment>,
}

// ---- Preset definition ----

/// Built-in segment prompt presets mirroring p10k themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentPreset {
    Lean,
    Classic,
    Rainbow,
    Pure,
    Robbyrussell,
}

impl SegmentPreset {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lean" => Some(Self::Lean),
            "classic" => Some(Self::Classic),
            "rainbow" => Some(Self::Rainbow),
            "pure" => Some(Self::Pure),
            "robbyrussell" => Some(Self::Robbyrussell),
            _ => None,
        }
    }

    pub fn names() -> &'static [&'static str] {
        &["lean", "classic", "rainbow", "pure", "robbyrussell"]
    }
}

// ---- Colour palette per preset ----

type SegmentColour = (Option<Color>, Option<Color>); // (fg, bg)

fn preset_colour(preset: SegmentPreset, segment: &SegmentId) -> SegmentColour {
    use Color::*;
    match preset {
        SegmentPreset::Lean => match segment {
            SegmentId::Dir => (Some(White), Some(Blue)),
            SegmentId::Vcs => (Some(Black), Some(Green)),
            SegmentId::Status => (Some(White), None),
            SegmentId::Time => (Some(White), None),
            SegmentId::PromptChar => (Some(White), None),
            SegmentId::OsIcon => (Some(White), Some(Blue)),
            SegmentId::Context => (Some(Cyan), None),
            SegmentId::BackgroundJobs => (Some(Yellow), None),
            SegmentId::CommandExecutionTime => (Some(Yellow), None),
            _ => (None, None),
        },
        SegmentPreset::Classic => match segment {
            SegmentId::Dir => (Some(White), Some(Blue)),
            SegmentId::Vcs => (Some(Black), Some(Green)),
            SegmentId::Status => (Some(Black), Some(Yellow)),
            SegmentId::Time => (Some(Black), Some(Yellow)),
            SegmentId::PromptChar => (Some(White), None),
            SegmentId::OsIcon => (Some(White), Some(Blue)),
            SegmentId::Context => (Some(Black), Some(Cyan)),
            SegmentId::BackgroundJobs => (Some(Yellow), None),
            SegmentId::CommandExecutionTime => (Some(Yellow), None),
            _ => (None, None),
        },
        SegmentPreset::Rainbow => match segment {
            SegmentId::Dir => (Some(Blue), None),
            SegmentId::Vcs => (Some(Green), None),
            SegmentId::Status => (Some(Red), None),
            SegmentId::Time => (Some(Yellow), None),
            SegmentId::PromptChar => (Some(White), None),
            SegmentId::OsIcon => (Some(Purple), None),
            SegmentId::Context => (Some(Cyan), None),
            SegmentId::BackgroundJobs => (Some(Yellow), None),
            SegmentId::CommandExecutionTime => (Some(Yellow), None),
            _ => (None, None),
        },
        SegmentPreset::Pure => match segment {
            SegmentId::Dir => (Some(Cyan), None),
            SegmentId::Vcs => (Some(Black), Some(Green)),
            SegmentId::Status => (Some(Red), None),
            SegmentId::PromptChar => (Some(Magenta), None),
            SegmentId::Context => (Some(Black), Some(Cyan)),
            SegmentId::CommandExecutionTime => (Some(Yellow), None),
            _ => (None, None),
        },
        SegmentPreset::Robbyrussell => match segment {
            SegmentId::Dir => (Some(Green), None),
            SegmentId::Vcs => (Some(Cyan), None),
            SegmentId::Status => (Some(Red), None),
            SegmentId::PromptChar => (Some(Green), None),
            SegmentId::Context => (Some(Green), None),
            _ => (None, None),
        },
    }
}

// ---- Segment content rendering ----

/// Produce the raw text content (no ANSI styling) for a segment.
fn render_content(
    segment: &SegmentId,
    git: Option<&GitRepoStatus>,
    config: &SegmentPromptConfig,
) -> Option<String> {
    match segment {
        SegmentId::Dir => {
            let cwd = std::env::current_dir().ok()?;
            Some(short_dir(&cwd))
        }
        SegmentId::Vcs => {
            let status = git?;
            let branch = status.branch.as_deref().unwrap_or("");
            if branch.is_empty() {
                return None;
            }
            let compact = status.compact_status_with(&config.git_prompt_symbols);
            let body = match &config.git_prompt_format {
                Some(fmt) if !fmt.is_empty() => fmt
                    .replace("{git_branch}", branch)
                    .replace("{git_status}", &compact),
                _ => {
                    if compact.is_empty() {
                        branch.to_string()
                    } else {
                        format!("{} {}", branch, compact)
                    }
                }
            };
            Some(body)
        }
        SegmentId::Status => {
            let code = std::env::var("WINUXSH_LAST_EXIT_CODE")
                .ok()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            if code == 0 {
                None
            } else {
                Some(format!("\u{2718} {}", code))
            }
        }
        SegmentId::Time => Some(format_local_time()),
        SegmentId::PromptChar => Some(config.prompt_symbol.clone()),
        SegmentId::OsIcon => Some("\u{1f4bb}".to_string()),
        SegmentId::Context => {
            let user = std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_else(|_| "?".to_string());
            let host = std::env::var("HOSTNAME")
                .or_else(|_| std::env::var("COMPUTERNAME"))
                .unwrap_or_else(|_| "winhost".to_string());
            Some(format!("{}@{}", user, host))
        }
        SegmentId::BackgroundJobs => {
            let jobs = std::env::var("WINUXSH_BACKGROUND_JOBS").ok();
            jobs.filter(|j| !j.is_empty() && j != "0")
                .map(|j| format!("{}&", j))
        }
        SegmentId::CommandExecutionTime => {
            let ms = std::env::var("WINUXSH_CMD_EXEC_TIME_MS").ok();
            ms.filter(|m| !m.is_empty()).map(|m| format!("{}ms", m))
        }
        SegmentId::Newline => Some(String::new()),
        SegmentId::Custom(name) => {
            let env_name = format!("WINUXSH_SEGMENT_{}", name.to_ascii_uppercase());
            std::env::var(&env_name).ok()
        }
    }
}

/// Shorten a directory path by replacing `$HOME` with `~`.
fn short_dir(path: &Path) -> String {
    let home = dirs::home_dir();
    let path_str = path.to_string_lossy();
    if let Some(home) = home {
        let home_str = home.to_string_lossy();
        if let Some(rest) = path_str.strip_prefix(&*home_str) {
            if rest.is_empty() {
                return "~".to_string();
            }
            let rest = rest.trim_start_matches('\\').trim_start_matches('/');
            return format!("~{}{}", std::path::MAIN_SEPARATOR, rest);
        }
    }
    path_str.to_string()
}

// ---- Prompt configuration ----

/// Configuration for the segment-based prompt engine.
#[derive(Debug, Clone)]
pub struct SegmentPromptConfig {
    pub left_elements: Vec<SegmentId>,
    pub right_elements: Vec<SegmentId>,
    pub separator: String,
    pub theme_name: String,
    pub prompt_symbol: String,
    pub git_prompt_symbols: GitPromptSymbols,
    pub git_prompt_format: Option<String>,
    pub preset: Option<SegmentPreset>,
}

impl SegmentPromptConfig {
    /// Build from a preset with default element ordering and colour.
    pub fn from_preset(
        preset: SegmentPreset,
        prompt_symbol: &str,
        git_symbols: GitPromptSymbols,
    ) -> Self {
        let (left, right, separator) = match preset {
            SegmentPreset::Lean => (
                vec![
                    SegmentId::Dir,
                    SegmentId::Vcs,
                    SegmentId::Newline,
                    SegmentId::PromptChar,
                ],
                vec![],
                " ".to_string(),
            ),
            SegmentPreset::Classic => (
                vec![SegmentId::Dir, SegmentId::Vcs, SegmentId::Newline],
                vec![SegmentId::Status, SegmentId::Time],
                "\u{e0b0}".to_string(),
            ),
            SegmentPreset::Rainbow => (
                vec![SegmentId::Dir, SegmentId::Vcs, SegmentId::Newline],
                vec![SegmentId::Status, SegmentId::Time],
                "\u{e0b0}".to_string(),
            ),
            SegmentPreset::Pure => (
                vec![
                    SegmentId::Context,
                    SegmentId::Dir,
                    SegmentId::Vcs,
                    SegmentId::CommandExecutionTime,
                    SegmentId::Newline,
                    SegmentId::PromptChar,
                ],
                vec![],
                " ".to_string(),
            ),
            SegmentPreset::Robbyrussell => (
                vec![
                    SegmentId::Context,
                    SegmentId::Dir,
                    SegmentId::Vcs,
                    SegmentId::Newline,
                    SegmentId::PromptChar,
                ],
                vec![],
                " ".to_string(),
            ),
        };
        let git_format = match preset {
            SegmentPreset::Classic | SegmentPreset::Rainbow => {
                Some("git:({git_branch})".to_string())
            }
            _ => None,
        };
        Self {
            left_elements: left,
            right_elements: right,
            separator,
            theme_name: "default".to_string(),
            prompt_symbol: prompt_symbol.to_string(),
            git_prompt_symbols: git_symbols,
            git_prompt_format: git_format,
            preset: Some(preset),
        }
    }
}

// ---- Prompt engine ----

/// The segment-based prompt engine.
#[derive(Debug, Clone)]
pub struct SegmentPrompt {
    config: SegmentPromptConfig,
}

impl SegmentPrompt {
    pub fn new(config: SegmentPromptConfig) -> Self {
        Self { config }
    }

    /// Render the full left prompt as an ANSI-coloured string.
    pub fn render_left(&self) -> String {
        let lines = self.render_lines();
        if lines.is_empty() {
            return String::new();
        }
        if lines.len() == 1 {
            render_line(&lines[0], &self.config.separator)
        } else {
            let mut out = String::new();
            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                if i == 0 {
                    out.push_str(&render_multiline_first(line, &self.config.separator));
                } else if i == lines.len() - 1 {
                    out.push_str(&render_multiline_last(line, &self.config.separator));
                } else {
                    out.push_str(&render_multiline_mid(line, &self.config.separator));
                }
            }
            out
        }
    }

    /// Render the right prompt as an ANSI-coloured string.
    pub fn render_right(&self) -> String {
        if self.config.right_elements.is_empty() {
            return String::new();
        }
        let git = current_git_status();
        let mut segments: Vec<RenderedSegment> = Vec::new();
        for element in &self.config.right_elements {
            if *element == SegmentId::Newline {
                break;
            }
            if let Some(content) = render_content(element, git.as_ref(), &self.config) {
                let (fg, bg) = self.colour_for(element);
                segments.push(RenderedSegment { content, fg, bg });
            }
        }
        if segments.is_empty() {
            return String::new();
        }
        segments
            .iter()
            .map(|s| style_segment(s))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn colour_for(&self, segment: &SegmentId) -> SegmentColour {
        if let Some(preset) = self.config.preset {
            preset_colour(preset, segment)
        } else {
            let theme = crate::theme::by_name(&self.config.theme_name);
            match segment {
                SegmentId::Dir => (theme.prompt_dir.foreground, None),
                SegmentId::Vcs => (theme.git_clean.foreground, None),
                SegmentId::Status => (theme.error.foreground, None),
                SegmentId::Time => (theme.prompt_dir.foreground, None),
                SegmentId::PromptChar => (theme.prompt_symbol.foreground, None),
                SegmentId::Context => (theme.prompt_user.foreground, None),
                _ => (None, None),
            }
        }
    }

    /// Split left_elements into `PromptLine`s, breaking at `Newline` segments.
    fn render_lines(&self) -> Vec<PromptLine> {
        let git = current_git_status();
        let mut lines: Vec<PromptLine> = Vec::new();
        let mut current: Vec<RenderedSegment> = Vec::new();

        for element in &self.config.left_elements {
            if *element == SegmentId::Newline {
                lines.push(PromptLine { segments: current });
                current = Vec::new();
                continue;
            }
            if let Some(content) = render_content(element, git.as_ref(), &self.config) {
                let (fg, bg) = self.colour_for(element);
                current.push(RenderedSegment { content, fg, bg });
            }
        }
        if !current.is_empty() || lines.is_empty() {
            lines.push(PromptLine { segments: current });
        }
        lines
    }
}

// ---- Line rendering helpers ----

/// ANSI-style a single segment.
fn style_segment(seg: &RenderedSegment) -> String {
    let mut style = Style::new();
    if let Some(fg) = seg.fg {
        style = style.fg(fg);
    }
    if let Some(bg) = seg.bg {
        style = style.on(bg);
    }
    style.paint(&seg.content).to_string()
}

/// Render a line of segments without any multiline prefix.
fn render_line(line: &PromptLine, separator: &str) -> String {
    let mut out = String::new();
    let mut prev_bg: Option<Color> = None;
    for seg in &line.segments {
        if !out.is_empty() && prev_bg.is_some() {
            out.push_str(&render_separator(seg, prev_bg, separator));
        }
        out.push_str(&style_segment(seg));
        prev_bg = seg.bg;
    }
    out
}

/// Render the first line with `╭─` prefix.
fn render_multiline_first(line: &PromptLine, separator: &str) -> String {
    let mut out = "\u{256d}\u{2500} ".to_string();
    let mut prev_bg: Option<Color> = None;
    for seg in &line.segments {
        if prev_bg.is_some() {
            out.push_str(&render_separator(seg, prev_bg, separator));
        }
        out.push_str(&style_segment(seg));
        prev_bg = seg.bg;
    }
    out
}

/// Render a middle line with `├─` prefix.
fn render_multiline_mid(line: &PromptLine, separator: &str) -> String {
    let mut out = "\u{251c}\u{2500} ".to_string();
    let mut prev_bg: Option<Color> = None;
    for seg in &line.segments {
        if prev_bg.is_some() {
            out.push_str(&render_separator(seg, prev_bg, separator));
        }
        out.push_str(&style_segment(seg));
        prev_bg = seg.bg;
    }
    out
}

/// Render the last line with `╰─` prefix.
fn render_multiline_last(line: &PromptLine, separator: &str) -> String {
    let mut out = "\u{2570}\u{2500} ".to_string();
    let mut prev_bg: Option<Color> = None;
    for seg in &line.segments {
        if prev_bg.is_some() {
            out.push_str(&render_separator(seg, prev_bg, separator));
        }
        out.push_str(&style_segment(seg));
        prev_bg = seg.bg;
    }
    out
}

/// Render a powerline (or plain) separator between two adjacent segments.
fn render_separator(next: &RenderedSegment, prev_bg: Option<Color>, separator: &str) -> String {
    let sep_fg = next.bg.or(next.fg);
    if let (Some(pbg), Some(sfg)) = (prev_bg, sep_fg) {
        let style = Style::new().on(pbg).fg(sfg);
        style.paint(separator).to_string()
    } else {
        separator.to_string()
    }
}

/// Read the current git status (non-blocking) for prompt rendering.
fn current_git_status() -> Option<GitRepoStatus> {
    let cwd = std::env::current_dir().ok()?;
    collect_for_prompt(&cwd)
}

// ---- reedline adapter ----

/// Wraps `SegmentPrompt` into the `reedline::Prompt` trait.
pub struct SegmentPromptAdapter {
    pub inner: SegmentPrompt,
    pub indicators: PromptIndicators,
}

impl SegmentPromptAdapter {
    pub fn new(inner: SegmentPrompt) -> Self {
        Self {
            inner,
            indicators: PromptIndicators::default(),
        }
    }
}

impl Prompt for SegmentPromptAdapter {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Owned(self.inner.render_left())
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Owned(self.inner.render_right())
    }

    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("\u{2570}\u{2500} ")
    }

    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        let status = match search.status {
            PromptHistorySearchStatus::Passing => "passing",
            PromptHistorySearchStatus::Failing => "failing",
        };
        Cow::Owned(format!("(h-search: {} {}) ", search.term, status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_status::GitPromptSymbols;
    use std::sync::Mutex;

    static STATUS_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn segment_id_from_name_case_insensitive() {
        assert_eq!(SegmentId::from_name("dir"), Some(SegmentId::Dir));
        assert_eq!(SegmentId::from_name("DIR"), Some(SegmentId::Dir));
        assert_eq!(SegmentId::from_name("vcs"), Some(SegmentId::Vcs));
        assert_eq!(
            SegmentId::from_name("prompt_char"),
            Some(SegmentId::PromptChar)
        );
        assert_eq!(
            SegmentId::from_name("prompt-char"),
            Some(SegmentId::PromptChar)
        );
        assert_eq!(SegmentId::from_name("newline"), Some(SegmentId::Newline));
        assert_eq!(SegmentId::from_name(""), None);
    }

    #[test]
    fn preset_from_name_works() {
        assert_eq!(
            SegmentPreset::from_name("classic"),
            Some(SegmentPreset::Classic)
        );
        assert_eq!(SegmentPreset::from_name("LEAN"), Some(SegmentPreset::Lean));
        assert_eq!(
            SegmentPreset::from_name("robbyrussell"),
            Some(SegmentPreset::Robbyrussell)
        );
        assert_eq!(SegmentPreset::from_name("unknown"), None);
    }

    #[test]
    fn short_dir_replaces_home_with_tilde() {
        let home = dirs::home_dir().unwrap();
        let s = short_dir(&home);
        assert_eq!(s, "~");
    }

    #[test]
    fn short_dir_preserves_non_home_paths() {
        let p = Path::new("C:\\");
        let s = short_dir(p);
        assert_eq!(s, "C:\\");
    }

    #[test]
    fn lean_preset_has_correct_elements() {
        let cfg = SegmentPromptConfig::from_preset(
            SegmentPreset::Lean,
            "\u{276f}",
            GitPromptSymbols::default(),
        );
        assert!(cfg.left_elements.contains(&SegmentId::Dir));
        assert!(cfg.left_elements.contains(&SegmentId::Vcs));
        assert!(cfg.left_elements.contains(&SegmentId::Newline));
        assert!(cfg.left_elements.contains(&SegmentId::PromptChar));
        assert!(cfg.right_elements.is_empty());
    }

    #[test]
    fn classic_preset_has_right_elements() {
        let cfg = SegmentPromptConfig::from_preset(
            SegmentPreset::Classic,
            "\u{276f}",
            GitPromptSymbols::default(),
        );
        assert!(cfg.left_elements.contains(&SegmentId::Dir));
        assert!(cfg.right_elements.contains(&SegmentId::Time));
    }

    #[test]
    fn time_content_renders_hhmm() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::Time, None, &cfg);
        assert!(content.is_some());
        let s = content.unwrap();
        assert_eq!(s.len(), 5);
        assert_eq!(&s[2..3], ":");
    }

    #[test]
    fn time_segment_renders_system_local_clock() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::Time, None, &cfg).unwrap();
        let expected = format_local_time();
        if content != expected {
            let expected = format_local_time();
            assert_eq!(content, expected);
        }
    }

    #[test]
    fn prompt_char_is_symbol() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::PromptChar, None, &cfg);
        assert_eq!(content.as_deref(), Some("\u{276f}"));
    }

    #[test]
    fn vcs_segment_returns_none_without_git() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::Vcs, None, &cfg);
        assert!(content.is_none());
    }

    #[test]
    fn status_segment_shows_nothing_on_success() {
        let _guard = STATUS_ENV_LOCK.lock().unwrap();
        let cfg = default_cfg();
        std::env::remove_var("WINUXSH_LAST_EXIT_CODE");
        let content = render_content(&SegmentId::Status, None, &cfg);
        assert!(content.is_none());
    }

    #[test]
    fn status_segment_shows_non_zero_code() {
        let _guard = STATUS_ENV_LOCK.lock().unwrap();
        let cfg = default_cfg();
        std::env::set_var("WINUXSH_LAST_EXIT_CODE", "1");
        let content = render_content(&SegmentId::Status, None, &cfg);
        assert!(content.is_some());
        assert!(content.unwrap().contains("1"));
        std::env::remove_var("WINUXSH_LAST_EXIT_CODE");
    }

    #[test]
    fn newline_renders_empty() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::Newline, None, &cfg);
        assert_eq!(content, Some(String::new()));
    }

    #[test]
    fn group_by_newline_splits_elements() {
        let cfg = SegmentPromptConfig {
            left_elements: vec![SegmentId::Dir, SegmentId::Newline, SegmentId::PromptChar],
            ..default_cfg()
        };
        let prompt = SegmentPrompt::new(cfg);
        let lines = prompt.render_lines();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn render_lean_does_not_panic() {
        let cfg = SegmentPromptConfig::from_preset(
            SegmentPreset::Lean,
            "\u{276f}",
            GitPromptSymbols::default(),
        );
        let prompt = SegmentPrompt::new(cfg);
        let _left = prompt.render_left();
        let _right = prompt.render_right();
    }

    #[test]
    fn render_classic_does_not_panic() {
        let cfg = SegmentPromptConfig::from_preset(
            SegmentPreset::Classic,
            "\u{276f}",
            GitPromptSymbols::default(),
        );
        let prompt = SegmentPrompt::new(cfg);
        let _left = prompt.render_left();
        let _right = prompt.render_right();
    }

    fn default_cfg() -> SegmentPromptConfig {
        SegmentPromptConfig {
            left_elements: vec![SegmentId::Dir, SegmentId::PromptChar],
            right_elements: vec![SegmentId::Time],
            separator: " ".to_string(),
            theme_name: "default".to_string(),
            prompt_symbol: "\u{276f}".to_string(),
            git_prompt_symbols: GitPromptSymbols::default(),
            git_prompt_format: None,
            preset: Some(SegmentPreset::Lean),
        }
    }
}
