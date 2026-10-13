//! p10k-style segment-based prompt engine
//!
//! Replaces the template-based `NiubashPrompt` with a powerlevel10k-inspired
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

use crate::path_utils::shell_home_dir;
use crate::prompt::{format_local_time, PromptIndicators};

// ---- Segment identifiers ----

/// Identifies a single prompt segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SegmentId {
    Dir,
    Status,
    Time,
    PromptChar,
    OsIcon,
    Context,
    BackgroundJobs,
    CommandExecutionTime,
    Newline,
    /// Version-control segment (git). niubash#331: the oh-my-niu bundle's
    /// `[segments.git] id = "vcs"` declaration, powered by #292's in-process
    /// git2 renderer (`branch` / `branch*` / short SHA) — zero child
    /// processes on the prompt path.
    Vcs,
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
            "status" => Some(Self::Status),
            "time" => Some(Self::Time),
            "promptchar" => Some(Self::PromptChar),
            "osicon" => Some(Self::OsIcon),
            "context" => Some(Self::Context),
            "backgroundjobs" => Some(Self::BackgroundJobs),
            "commandexecutiontime" | "cmdexectime" => Some(Self::CommandExecutionTime),
            "newline" => Some(Self::Newline),
            // The bundle segment is named `git`, the native id is `vcs`
            // (prompts/segments.toml: `[segments.git] id = "vcs"`) — accept
            // either spelling so the declaration and the presets resolve.
            "vcs" | "git" => Some(Self::Vcs),
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
            SegmentId::Status => (Some(Red), None),
            SegmentId::PromptChar => (Some(Magenta), None),
            SegmentId::Context => (Some(Black), Some(Cyan)),
            SegmentId::CommandExecutionTime => (Some(Yellow), None),
            _ => (None, None),
        },
        SegmentPreset::Robbyrussell => match segment {
            SegmentId::Dir => (Some(Green), None),
            SegmentId::Status => (Some(Red), None),
            SegmentId::PromptChar => (Some(Green), None),
            SegmentId::Context => (Some(Green), None),
            _ => (None, None),
        },
    }
}

// ---- Segment content rendering ----

/// Produce the raw text content (no ANSI styling) for a segment.
fn render_content(segment: &SegmentId, config: &SegmentPromptConfig) -> Option<String> {
    match segment {
        SegmentId::Dir => {
            let cwd = std::env::current_dir().ok()?;
            Some(short_dir(&cwd))
        }
        SegmentId::Status => {
            let code = std::env::var("NIU_LAST_EXIT_CODE")
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
            let jobs = std::env::var("NIU_BACKGROUND_JOBS").ok();
            jobs.filter(|j| !j.is_empty() && j != "0")
                .map(|j| format!("{}&", j))
        }
        SegmentId::CommandExecutionTime => {
            let ms = std::env::var("NIU_CMD_EXEC_TIME_MS").ok();
            ms.filter(|m| !m.is_empty()).map(|m| format!("{}ms", m))
        }
        SegmentId::Vcs => vcs_segment_content(),
        SegmentId::Newline => Some(String::new()),
        SegmentId::Custom(name) => {
            let env_name = format!("NIU_SEGMENT_{}", name.to_ascii_uppercase());
            std::env::var(&env_name).ok()
        }
    }
}

/// Shorten a directory path by replacing `$HOME` with `~`.
fn short_dir(path: &Path) -> String {
    let home = shell_home_dir();
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

/// The Vcs segment body: `branch` / `branch*` (dirty) / short SHA (detached),
/// `None` (empty segment) outside a repository — the #292 git2 renderer, so
/// the whole path stays in-process. No `Command`/spawn anywhere on this
/// route: niubash#604 measured PS1-embedded `$(...)` at ~100x the native
/// segment cost, and the niubash#145 red line forbids prompt-path forks.
#[cfg(windows)]
fn vcs_segment_content() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    crate::git_prompt::branch_segment(&cwd)
}

/// Off-Windows the git segment is not a surface (`git_prompt` is a
/// windows-only module, see lib.rs): the segment renders empty the same way
/// the gitbash prompt style falls back to the template floor there.
#[cfg(not(windows))]
fn vcs_segment_content() -> Option<String> {
    None
}

/// Vcs colouring, the oh-my-niu theme contract: `[git_clean]` cyan,
/// `[git_dirty]` yellow. The renderer marks dirtiness with the trailing `*`
/// (`branch*`), so the star alone decides the colour — a detached short SHA
/// renders clean-cyan exactly the way #292's gitbash segment does. Applied
/// regardless of preset (the compiled palettes carry no per-preset git
/// colours; the bundle themes speak with one voice here).
fn vcs_colour(content: &str) -> SegmentColour {
    if content.ends_with('*') {
        (Some(Color::Yellow), None)
    } else {
        (Some(Color::Cyan), None)
    }
}

// ---- Prompt configuration ----

/// Configuration for the segment-based prompt engine.
#[derive(Debug, Clone)]
pub struct SegmentPromptConfig {
    pub left_elements: Vec<SegmentId>,
    pub right_elements: Vec<SegmentId>,
    pub separator: String,
    pub prompt_symbol: String,
    pub preset: Option<SegmentPreset>,
}

impl SegmentPromptConfig {
    /// Build from a preset with default element ordering and colour.
    pub fn from_preset(preset: SegmentPreset, prompt_symbol: &str) -> Self {
        let (left, right, separator) = match preset {
            SegmentPreset::Lean => (
                vec![SegmentId::Dir, SegmentId::Newline, SegmentId::PromptChar],
                vec![],
                " ".to_string(),
            ),
            SegmentPreset::Classic => (
                vec![SegmentId::Dir, SegmentId::Newline],
                vec![SegmentId::Status, SegmentId::Time],
                "\u{e0b0}".to_string(),
            ),
            SegmentPreset::Rainbow => (
                vec![SegmentId::Dir, SegmentId::Newline],
                vec![SegmentId::Status, SegmentId::Time],
                "\u{e0b0}".to_string(),
            ),
            SegmentPreset::Pure => (
                vec![
                    SegmentId::Context,
                    SegmentId::Dir,
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
                    SegmentId::Newline,
                    SegmentId::PromptChar,
                ],
                vec![],
                " ".to_string(),
            ),
        };
        Self {
            left_elements: left,
            right_elements: right,
            separator,
            prompt_symbol: prompt_symbol.to_string(),
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
        let mut segments: Vec<RenderedSegment> = Vec::new();
        for element in &self.config.right_elements {
            if *element == SegmentId::Newline {
                break;
            }
            if let Some(content) = render_content(element, &self.config) {
                let (fg, bg) = self.colour_for(element, &content);
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

    fn colour_for(&self, segment: &SegmentId, content: &str) -> SegmentColour {
        // Vcs carries its own colour contract (oh-my-niu themes'
        // `[git_clean]` cyan / `[git_dirty]` yellow), keyed off the dirty
        // star the renderer appends — see `vcs_colour`.
        if *segment == SegmentId::Vcs {
            return vcs_colour(content);
        }
        match self.config.preset {
            Some(preset) => preset_colour(preset, segment),
            // Custom element orders without a preset render unstyled; the
            // built-in theme lookup retired with niubash#145.
            None => (None, None),
        }
    }

    /// Split left_elements into `PromptLine`s, breaking at `Newline` segments.
    fn render_lines(&self) -> Vec<PromptLine> {
        let mut lines: Vec<PromptLine> = Vec::new();
        let mut current: Vec<RenderedSegment> = Vec::new();

        for element in &self.config.left_elements {
            if *element == SegmentId::Newline {
                lines.push(PromptLine { segments: current });
                current = Vec::new();
                continue;
            }
            if let Some(content) = render_content(element, &self.config) {
                let (fg, bg) = self.colour_for(element, &content);
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

// ---- reedline adapter ----

/// Wraps `SegmentPrompt` into the `reedline::Prompt` trait.
#[derive(Clone)]
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
    use crate::test_support::PROCESS_STATE_LOCK;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn segment_id_from_name_case_insensitive() {
        assert_eq!(SegmentId::from_name("dir"), Some(SegmentId::Dir));
        assert_eq!(SegmentId::from_name("DIR"), Some(SegmentId::Dir));
        // niubash#331 revives the segment: "vcs" (the native id the oh-my-niu
        // bundle declares) and "git" (the bundle segment name) both resolve.
        assert_eq!(SegmentId::from_name("vcs"), Some(SegmentId::Vcs));
        assert_eq!(SegmentId::from_name("git"), Some(SegmentId::Vcs));
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
    fn vcs_from_name_matrix() {
        // Native id, bundle segment name, case, separator spellings — the
        // parser strips non-alphanumerics, so every shape lands on Vcs.
        for name in ["vcs", "git", "VCS", "Git", "GIT", "v-c-s", "v_c_s"] {
            assert_eq!(
                SegmentId::from_name(name),
                Some(SegmentId::Vcs),
                "{name:?} must resolve to SegmentId::Vcs"
            );
        }
        // Near-misses stay unknown.
        for name in ["gitstatus", "vc", "gitbash"] {
            assert_eq!(SegmentId::from_name(name), None, "{name:?} is not Vcs");
        }
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
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = unique_temp_dir("niubash-segment-prompt-home");
        std::fs::create_dir_all(&home).unwrap();
        let _home = EnvGuard::set("HOME", &home.to_string_lossy());
        let home = shell_home_dir().unwrap();
        let s = short_dir(&home);
        assert_eq!(s, "~");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn short_dir_preserves_non_home_paths() {
        let p = Path::new("C:\\");
        let s = short_dir(p);
        assert_eq!(s, "C:\\");
    }

    #[test]
    fn lean_preset_has_correct_elements() {
        let cfg = SegmentPromptConfig::from_preset(SegmentPreset::Lean, "\u{276f}");
        assert!(cfg.left_elements.contains(&SegmentId::Dir));
        assert!(cfg.left_elements.contains(&SegmentId::Newline));
        assert!(cfg.left_elements.contains(&SegmentId::PromptChar));
        assert!(cfg.right_elements.is_empty());
    }

    #[test]
    fn classic_preset_has_right_elements() {
        let cfg = SegmentPromptConfig::from_preset(SegmentPreset::Classic, "\u{276f}");
        assert!(cfg.left_elements.contains(&SegmentId::Dir));
        assert!(cfg.right_elements.contains(&SegmentId::Time));
    }

    #[test]
    fn time_content_renders_hhmm() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::Time, &cfg);
        assert!(content.is_some());
        let s = content.unwrap();
        assert_eq!(s.len(), 5);
        assert_eq!(&s[2..3], ":");
    }

    #[test]
    fn time_segment_renders_system_local_clock() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::Time, &cfg).unwrap();
        let expected = format_local_time();
        if content != expected {
            let expected = format_local_time();
            assert_eq!(content, expected);
        }
    }

    #[test]
    fn prompt_char_is_symbol() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::PromptChar, &cfg);
        assert_eq!(content.as_deref(), Some("\u{276f}"));
    }

    #[test]
    fn status_segment_shows_nothing_on_success() {
        // Mutates the process env (NIU_LAST_EXIT_CODE): must serialize with
        // every other env-touching test (Windows env races — wt61).
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = default_cfg();
        std::env::remove_var("NIU_LAST_EXIT_CODE");
        let content = render_content(&SegmentId::Status, &cfg);
        assert!(content.is_none());
    }

    #[test]
    fn status_segment_shows_non_zero_code() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = default_cfg();
        std::env::set_var("NIU_LAST_EXIT_CODE", "1");
        let content = render_content(&SegmentId::Status, &cfg);
        assert!(content.is_some());
        assert!(content.unwrap().contains("1"));
        std::env::remove_var("NIU_LAST_EXIT_CODE");
    }

    #[test]
    fn newline_renders_empty() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::Newline, &cfg);
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
        let cfg = SegmentPromptConfig::from_preset(SegmentPreset::Lean, "\u{276f}");
        let prompt = SegmentPrompt::new(cfg);
        let _left = prompt.render_left();
        let _right = prompt.render_right();
    }

    #[test]
    fn render_classic_does_not_panic() {
        let cfg = SegmentPromptConfig::from_preset(SegmentPreset::Classic, "\u{276f}");
        let prompt = SegmentPrompt::new(cfg);
        let _left = prompt.render_left();
        let _right = prompt.render_right();
    }

    fn default_cfg() -> SegmentPromptConfig {
        SegmentPromptConfig {
            left_elements: vec![SegmentId::Dir, SegmentId::PromptChar],
            right_elements: vec![SegmentId::Time],
            separator: " ".to_string(),
            prompt_symbol: "\u{276f}".to_string(),
            preset: Some(SegmentPreset::Lean),
        }
    }

    struct EnvGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.name, previous);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
    }

    // ---- SegmentId::Vcs (niubash#331) ----
    //
    // git2 is a windows-only dependency (Cargo.toml target gate), so the
    // fixture-building tests are windows-gated like the git_prompt module.

    /// A CwdGuard for the `current_dir`-driven Vcs segment; serialised with
    /// the PROCESS_STATE_LOCK like every other process-state mutation
    /// (Windows env/cwd races — wt61).
    #[cfg(windows)]
    struct CwdGuard {
        previous: std::path::PathBuf,
    }

    #[cfg(windows)]
    impl CwdGuard {
        fn enter(path: &Path) -> Self {
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            Self { previous }
        }
    }

    #[cfg(windows)]
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    /// Minimal one-commit repository (clean index) — the shared fixture for
    /// the Vcs render matrix.
    fn init_clean_repo(dir: &Path) -> git2::Repository {
        let repo = git2::Repository::init(dir).unwrap();
        let mut index = repo.index().unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        // `tree` borrows `repo`; release it before the repo is handed back.
        drop(tree);
        repo
    }

    #[cfg(windows)]
    fn vcs_content_in(dir: &Path) -> Option<String> {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd = CwdGuard::enter(dir);
        vcs_segment_content()
    }

    #[cfg(windows)]
    #[test]
    fn vcs_segment_is_empty_outside_a_repository() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(vcs_content_in(tmp.path()), None);
    }

    #[cfg(windows)]
    #[test]
    fn vcs_segment_shows_branch_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_clean_repo(tmp.path());
        let branch = repo.head().unwrap().shorthand().unwrap().to_string();
        let content = vcs_content_in(tmp.path()).unwrap();
        assert_eq!(content, branch, "clean repo: bare branch, no star");
        assert!(!content.ends_with('*'), "{content:?}");
    }

    #[cfg(windows)]
    #[test]
    fn vcs_segment_marks_dirty_with_the_star() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_clean_repo(tmp.path());
        let file = tmp.path().join("tracked.txt");
        std::fs::write(&file, b"dirty").unwrap();
        let branch = repo.head().unwrap().shorthand().unwrap().to_string();
        let content = vcs_content_in(tmp.path()).unwrap();
        assert_eq!(content, format!("{branch}*"), "dirty repo: branch + star");
    }

    #[cfg(windows)]
    #[test]
    fn vcs_segment_shows_short_sha_when_detached() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_clean_repo(tmp.path());
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        repo.set_head_detached(oid).unwrap();
        let content = vcs_content_in(tmp.path()).unwrap();
        assert_eq!(content, &oid.to_string()[..7], "detached: short SHA");
    }

    #[test]
    fn vcs_colour_follows_the_oh_my_niu_theme_contract() {
        // [git_clean] cyan / [git_dirty] yellow — the dirty star decides.
        let (clean_fg, clean_bg) = vcs_colour("main");
        let (dirty_fg, dirty_bg) = vcs_colour("main*");
        let (detached_fg, _) = vcs_colour("a1b2c3d");
        assert_eq!(clean_fg, Some(Color::Cyan));
        assert_eq!(clean_bg, None);
        assert_eq!(dirty_fg, Some(Color::Yellow));
        assert_eq!(dirty_bg, None);
        assert_eq!(detached_fg, Some(Color::Cyan), "detached renders clean");
    }

    #[cfg(windows)]
    #[test]
    fn vcs_segment_paints_cyan_clean_and_yellow_dirty() {
        let cases: [(bool, Color); 2] = [(false, Color::Cyan), (true, Color::Yellow)];
        for (dirty, expected) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let repo = init_clean_repo(tmp.path());
            if dirty {
                std::fs::write(tmp.path().join("tracked.txt"), b"dirty").unwrap();
            }
            let cfg = SegmentPromptConfig {
                left_elements: vec![SegmentId::Vcs],
                right_elements: vec![],
                separator: " ".to_string(),
                prompt_symbol: "\u{276f}".to_string(),
                preset: Some(SegmentPreset::Lean),
            };
            let prompt = SegmentPrompt::new(cfg);
            let left = {
                let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                let _cwd = CwdGuard::enter(tmp.path());
                prompt.render_left()
            };
            let branch = repo.head().unwrap().shorthand().unwrap().to_string();
            let ansi = if expected == Color::Cyan {
                "\u{1b}[36m"
            } else {
                "\u{1b}[33m"
            };
            assert!(
                left.contains(ansi),
                "dirty={dirty} must paint {expected:?}: {left:?}"
            );
            assert!(left.contains(&branch), "dirty={dirty}: {left:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn vcs_segment_render_p50_within_10ms_budget() {
        // Issue #331 scope 4: p50 <= 10ms (10x headroom over the 2.5ms
        // warm git2 measurement). The median of 21 renders absorbs the
        // first-open cold cost; a per-prompt git child process (#604's
        // anti-pattern) would blow the budget by an order of magnitude.
        let tmp = tempfile::tempdir().unwrap();
        init_clean_repo(tmp.path());

        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd = CwdGuard::enter(tmp.path());
        for _ in 0..3 {
            let _ = vcs_segment_content(); // warm-up
        }
        let mut samples: Vec<std::time::Duration> = Vec::with_capacity(21);
        for _ in 0..21 {
            let start = std::time::Instant::now();
            let _ = vcs_segment_content();
            samples.push(start.elapsed());
        }
        samples.sort();
        let p50 = samples[samples.len() / 2];
        assert!(
            p50.as_millis() <= 10,
            "Vcs segment p50 {p50:?} exceeds the 10ms budget (max {max:?})",
            max = samples[samples.len() - 1]
        );
    }

    /// Verbatim mirror of oh-my-niu `prompts/segments.toml` (bundle clone,
    /// unixwin/oh-my-niu). Pinned here so contract drift fails this test on
    /// either side: a renamed id or a preset referencing an unknown segment
    /// breaks resolution the day the bundle ships.
    const OH_MY_NIU_SEGMENTS_TOML: &str = r#"
[segments.cwd]
id = "dir"
description = "Current working directory, shortened for prompt display."

[segments.git]
id = "vcs"
description = "Git branch and compact repository status."

[segments.status]
id = "status"
description = "Last command exit status when non-zero."

[segments.time]
id = "time"
description = "Current local time."

[segments.user_host]
id = "context"
description = "User and host context."

[segments.prompt_char]
id = "prompt_char"
description = "Mode-neutral prompt symbol."

[segments.newline]
id = "newline"
description = "Prompt line break."

[segments.command_execution_time]
id = "command_execution_time"
description = "Previous command duration."

[presets.lean]
left = ["cwd", "git", "newline", "prompt_char"]
right = []
separator = " "

[presets.classic]
left = ["cwd", "git", "newline"]
right = ["status", "time"]
separator = "\uE0B0"
git_prompt_format = "git:({git_branch})"

[presets.rainbow]
left = ["cwd", "git", "newline"]
right = ["status", "time"]
separator = "\uE0B0"
git_prompt_format = "git:({git_branch})"

[presets.pure]
left = ["user_host", "cwd", "git", "command_execution_time", "newline", "prompt_char"]
right = []
separator = " "

[presets.robbyrussell]
left = ["user_host", "cwd", "git", "newline", "prompt_char"]
right = []
separator = " "
"#;

    #[test]
    fn oh_my_niu_segments_toml_ids_resolve() {
        let value: toml::Value = toml::from_str(OH_MY_NIU_SEGMENTS_TOML).unwrap();
        let segments = value["segments"].as_table().unwrap();
        let mut name_to_id = std::collections::BTreeMap::new();
        for (name, spec) in segments {
            let id = spec["id"].as_str().unwrap();
            let resolved = SegmentId::from_name(id);
            assert!(
                resolved.is_some(),
                "bundle id {id:?} (segment {name:?}) has no native SegmentId"
            );
            name_to_id.insert(name.as_str(), id);
        }
        // Every shipped preset element must resolve — directly by native
        // name, or through the bundle's name -> id mapping.
        for (preset, spec) in value["presets"].as_table().unwrap() {
            for side in ["left", "right"] {
                let Some(list) = spec.get(side).and_then(|l| l.as_array()) else {
                    continue;
                };
                for element in list {
                    let name = element.as_str().unwrap();
                    let resolves = SegmentId::from_name(name).is_some()
                        || name_to_id
                            .get(name)
                            .is_some_and(|id| SegmentId::from_name(id).is_some());
                    assert!(
                        resolves,
                        "preset {preset:?} references unresolvable segment {name:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn oh_my_niu_git_segment_resolves_under_name_and_id() {
        // The issue's headline socket: `[segments.git] id = "vcs"`. Both the
        // bundle name ("git") and the native id ("vcs") land on SegmentId::Vcs.
        assert_eq!(SegmentId::from_name("git"), Some(SegmentId::Vcs));
        assert_eq!(SegmentId::from_name("vcs"), Some(SegmentId::Vcs));
        // Preset integration: the shipped `lean` preset lists `git`; through
        // the bundle mapping it must resolve to the native Vcs segment.
        let value: toml::Value = toml::from_str(OH_MY_NIU_SEGMENTS_TOML).unwrap();
        let lean_left: Vec<String> = value["presets"]["lean"]["left"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap().to_string())
            .collect();
        let git_name = "git";
        assert!(lean_left.contains(&git_name.to_string()));
        let bundle_id = value["segments"][git_name]["id"].as_str().unwrap();
        assert_eq!(SegmentId::from_name(bundle_id), Some(SegmentId::Vcs));
    }
}
