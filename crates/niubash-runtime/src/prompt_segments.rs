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
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nu_ansi_term::{Color, Style};
use reedline::{Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus};

use crate::idle_repaint::LiveVcsCell;
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
    /// `[segments.git] id = "vcs"` declaration, powered by #292's renderer
    /// (`branch` / `branch*` / short SHA). The read is one bounded, isolated
    /// `git status` inside a repository and nothing at all outside one.
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
///
/// `vcs_stamp` is the calling prompt's per-pass Vcs stamp (niubash#592,
/// S2): a stamped prompt's Vcs segment re-reads the stamp, an unstamped
/// prompt falls back to computing at render time. `dir_memo` is the
/// calling prompt's Dir memo (audit item A2), the same per-pass shape
/// for the one other segment that touches the file system; `None`
/// (direct callers in tests) computes uncached.
fn render_content(
    segment: &SegmentId,
    config: &SegmentPromptConfig,
    vcs_stamp: Option<&VcsPassStamp>,
    dir_memo: Option<&DirMemoCell>,
) -> Option<String> {
    match segment {
        SegmentId::Dir => {
            let cwd = std::env::current_dir().ok()?;
            Some(match dir_memo {
                Some(memo) => short_dir_memoized(&cwd, memo),
                None => short_dir(&cwd),
            })
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
        // niubash#592 (S2): a stamped prompt re-reads its pass's stamped
        // value — repaints never spawn. `None` in the stamp is the
        // pass's read result ("no segment this pass"), not a missing
        // value, so it is re-read too. The read-at-render fallback
        // remains only for unstamped prompts; in production the one
        // unstamped renderer is the --noediting loop's resident prompt,
        // which renders once per pass exactly as it did before S2.
        // Since niubash#685 the fallback reads the cross-pass segment
        // cache: it can launch a background refresh but never waits.
        SegmentId::Vcs => match vcs_stamp {
            Some(stamp) => stamp.value.clone(),
            None => vcs_segment_content(),
        },
        SegmentId::Newline => Some(String::new()),
        SegmentId::Custom(name) => {
            let env_name = format!("NIU_SEGMENT_{}", name.to_ascii_uppercase());
            std::env::var(&env_name).ok()
        }
    }
}

/// The Dir segment's memo (audit item A2): the shortened cwd plus the
/// inputs it was computed from — the cwd itself and the raw `HOME` /
/// `USERPROFILE` values `shell_home_dir` resolves from (its
/// `dirs::home_dir()` fallback when both are unset is process-stable
/// and needs no key component). Keyed on all of them, a hit can never
/// serve a value computed under different inputs.
#[derive(Debug)]
struct DirMemo {
    cwd: PathBuf,
    home: Option<OsString>,
    userprofile: Option<OsString>,
    text: String,
}

/// Shared Dir memo cell, cloned with the prompt (like the template
/// prompt's cwd memo): per-pass clones share it, and the key check is
/// what keeps a shared cell honest across passes.
type DirMemoCell = Arc<Mutex<Option<DirMemo>>>;

/// `short_dir` through the prompt's Dir memo. Within a pass the cwd
/// cannot move — it changes only when a command runs, which ends the
/// pass — so repaints reuse the one `canonicalize(home)` result
/// instead of paying it per repaint.
fn short_dir_memoized(cwd: &Path, memo: &DirMemoCell) -> String {
    let home = std::env::var_os("HOME");
    let userprofile = std::env::var_os("USERPROFILE");
    if let Ok(guard) = memo.lock() {
        if let Some(memo) = guard.as_ref() {
            if memo.cwd == cwd && memo.home == home && memo.userprofile == userprofile {
                return memo.text.clone();
            }
        }
    }
    let text = short_dir(cwd);
    if let Ok(mut guard) = memo.lock() {
        *guard = Some(DirMemo {
            cwd: cwd.to_path_buf(),
            home,
            userprofile,
            text: text.clone(),
        });
    }
    text
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
/// `None` (empty segment) outside a repository — the #292 renderer, backed
/// by the user's own git through git_prompt's cross-pass cache (see
/// git_prompt.rs for the isolation rules and the #145 relationship: the
/// one scoped child runs on a refresh thread no draw waits on, killed
/// past its timeout, and no spawn at all outside a repository).
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

/// The Vcs value one prompt pass stamped onto its prompt clone
/// (niubash#592, S2). `seq` is the pass's monotonic sequence number —
/// probe/debugging identity for "which pass stamped this value", never
/// a freshness input. `value` is the segment body exactly as
/// `git_prompt::branch_segment` returned it at stamp time; since
/// niubash#685 that call is a cross-pass cache read, so `None` can
/// mean a cold or stale entry whose background refresh has not landed
/// yet, as well as a computed "no segment" (outside a repository, git
/// unavailable, or the last read failed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsPassStamp {
    pub seq: u64,
    pub value: Option<String>,
}

/// The segment-based prompt engine.
#[derive(Debug, Clone)]
pub struct SegmentPrompt {
    config: SegmentPromptConfig,
    /// The per-pass Vcs stamp (niubash#592, S2), tri-state: `None` =
    /// unstamped (a prompt no pass has handed out — the shell's resident
    /// prompt, whose renders fall back to reading the segment cache at
    /// render time); `Some(stamp)` = stamped by
    /// `Shell::prompt_for_display` for one pass, and this clone's
    /// renders re-read `stamp.value` instead of touching the cache.
    /// The resident prompt in `Shell` is never stamped, so a stamp
    /// physically cannot outlive the pass that stamped it: the clone
    /// carrying it is dropped when the pass's read_line returns.
    vcs_stamp: Option<VcsPassStamp>,
    /// The stamp's live cell (niubash#787): created alongside the
    /// stamp when the pass's cwd is inside a worktree, and shared
    /// with the idle-repaint registry for exactly this pass. When a
    /// segment refresh lands mid-pass, the landing re-stamps the
    /// cell and the editor repaint that follows renders the landed
    /// value — renders still never touch the cache; the cell is a
    /// host-written stamp, not a render read. `None` whenever the
    /// prompt is unstamped or the pass has no worktree.
    vcs_live: Option<Arc<LiveVcsCell>>,
    /// The Dir segment's memo (audit item A2), the Vcs stamp's sibling
    /// for the one other file-system-touching segment: shared across
    /// clones and keyed by (cwd, HOME, USERPROFILE), so repaints in a
    /// pass reuse the shortened cwd instead of re-canonicalizing home
    /// per repaint.
    dir_memo: DirMemoCell,
}

impl SegmentPrompt {
    pub fn new(config: SegmentPromptConfig) -> Self {
        Self {
            config,
            vcs_stamp: None,
            vcs_live: None,
            dir_memo: Arc::new(Mutex::new(None)),
        }
    }

    /// Whether this prompt's configuration draws the Vcs segment on
    /// either side (niubash#592: the per-pass cache read only runs for
    /// prompts that can show it). The production caller is the
    /// Windows-gated stamp in `Shell::prompt_for_display`.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn config_has_vcs(&self) -> bool {
        self.config
            .left_elements
            .iter()
            .chain(self.config.right_elements.iter())
            .any(|element| *element == SegmentId::Vcs)
    }

    /// Stamp this prompt (a per-pass clone) with the Vcs value its pass
    /// read from the segment cache (niubash#592, S2; niubash#685).
    /// Called exactly once per pass by `Shell::prompt_for_display`,
    /// after the precmd hooks have run. `worktree` is the repository
    /// the value belongs to: when the pass runs inside one, the stamp
    /// also gets its live cell (niubash#787), so a refresh landing
    /// mid-pass can re-stamp this clone in place.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn stamp_vcs(&mut self, seq: u64, value: Option<String>, worktree: Option<PathBuf>) {
        self.vcs_live = worktree.map(|_| Arc::new(LiveVcsCell::new(value.clone())));
        self.vcs_stamp = Some(VcsPassStamp { seq, value });
    }

    /// The live Vcs cell this pass renders through, if it carries
    /// one (niubash#787): the REPL registers it with the
    /// idle-repaint registry for the pass's `read_line`.
    pub(crate) fn live_vcs_cell(&self) -> Option<Arc<LiveVcsCell>> {
        self.vcs_live.clone()
    }

    /// The stamp this prompt's renders read: the live cell's current
    /// value when the pass carries one (a mid-pass landing may have
    /// re-stamped it, niubash#787), else the value stamped at pass
    /// start. `None` stays `None` — an unstamped prompt keeps its
    /// render-time cache fallback.
    fn vcs_render_stamp(&self) -> Option<VcsPassStamp> {
        match (&self.vcs_live, &self.vcs_stamp) {
            (Some(cell), stamp) => Some(VcsPassStamp {
                seq: stamp.as_ref().map_or(0, |stamp| stamp.seq),
                value: cell.get(),
            }),
            (None, stamp) => stamp.clone(),
        }
    }

    /// The per-pass Vcs stamp, if this prompt was stamped (niubash#592).
    /// Test builds only: production renders read the stamp internally.
    #[cfg(test)]
    pub(crate) fn vcs_stamp(&self) -> Option<&VcsPassStamp> {
        self.vcs_stamp.as_ref()
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
        let stamp = self.vcs_render_stamp();
        for element in &self.config.right_elements {
            if *element == SegmentId::Newline {
                break;
            }
            if let Some(content) =
                render_content(element, &self.config, stamp.as_ref(), Some(&self.dir_memo))
            {
                let (fg, bg) = self.colour_for(element, &content);
                segments.push(RenderedSegment { content, fg, bg });
            }
        }
        if segments.is_empty() {
            return String::new();
        }
        segments
            .iter()
            .map(style_segment)
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
        let stamp = self.vcs_render_stamp();

        for element in &self.config.left_elements {
            if *element == SegmentId::Newline {
                lines.push(PromptLine { segments: current });
                current = Vec::new();
                continue;
            }
            if let Some(content) =
                render_content(element, &self.config, stamp.as_ref(), Some(&self.dir_memo))
            {
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
    fn dir_segment_memo_misses_when_cwd_or_home_moves() {
        // Audit item A2: the Dir memo must never serve a value computed
        // under different inputs — moving cwd (same home) and moving
        // HOME (same cwd) both have to miss and recompute.
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = unique_temp_dir("niubash-segment-memo-home");
        let other_home = unique_temp_dir("niubash-segment-memo-other");
        std::fs::create_dir_all(home.join("proj-a")).unwrap();
        std::fs::create_dir_all(home.join("proj-b")).unwrap();
        std::fs::create_dir_all(&other_home).unwrap();
        let cfg = default_cfg();
        let cell: DirMemoCell = Arc::new(Mutex::new(None));
        let previous = std::env::current_dir().unwrap();
        let sep = std::path::MAIN_SEPARATOR;
        let _home = EnvGuard::set("HOME", &home.to_string_lossy());
        let _userprofile = EnvGuard::set("USERPROFILE", &home.to_string_lossy());
        // short_dir compares against the canonicalized home.
        let canonical_home = shell_home_dir().unwrap();
        let canon_a = canonical_home.join("proj-a");
        let canon_b = canonical_home.join("proj-b");

        std::env::set_current_dir(&canon_a).unwrap();
        let first = render_content(&SegmentId::Dir, &cfg, None, Some(&cell)).unwrap();
        let again = render_content(&SegmentId::Dir, &cfg, None, Some(&cell)).unwrap();
        std::env::set_current_dir(&canon_b).unwrap();
        let moved = render_content(&SegmentId::Dir, &cfg, None, Some(&cell)).unwrap();
        let foreign = {
            let _home2 = EnvGuard::set("HOME", &other_home.to_string_lossy());
            render_content(&SegmentId::Dir, &cfg, None, Some(&cell)).unwrap()
        };
        std::env::set_current_dir(previous).unwrap();

        assert_eq!(first, format!("~{sep}proj-a"));
        assert_eq!(again, first);
        assert_eq!(moved, format!("~{sep}proj-b"));
        assert!(!foreign.starts_with('~'), "{foreign}");
        assert!(foreign.ends_with("proj-b"), "{foreign}");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&other_home);
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
        let content = render_content(&SegmentId::Time, &cfg, None, None);
        assert!(content.is_some());
        let s = content.unwrap();
        assert_eq!(s.len(), 5);
        assert_eq!(&s[2..3], ":");
    }

    #[test]
    fn time_segment_renders_system_local_clock() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::Time, &cfg, None, None).unwrap();
        let expected = format_local_time();
        if content != expected {
            let expected = format_local_time();
            assert_eq!(content, expected);
        }
    }

    #[test]
    fn prompt_char_is_symbol() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::PromptChar, &cfg, None, None);
        assert_eq!(content.as_deref(), Some("\u{276f}"));
    }

    #[test]
    fn status_segment_shows_nothing_on_success() {
        // Mutates the process env (NIU_LAST_EXIT_CODE): must serialize with
        // every other env-touching test (Windows env races — wt61).
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = default_cfg();
        std::env::remove_var("NIU_LAST_EXIT_CODE");
        let content = render_content(&SegmentId::Status, &cfg, None, None);
        assert!(content.is_none());
    }

    #[test]
    fn status_segment_shows_non_zero_code() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = default_cfg();
        std::env::set_var("NIU_LAST_EXIT_CODE", "1");
        let content = render_content(&SegmentId::Status, &cfg, None, None);
        assert!(content.is_some());
        assert!(content.unwrap().contains("1"));
        std::env::remove_var("NIU_LAST_EXIT_CODE");
    }

    #[test]
    fn newline_renders_empty() {
        let cfg = default_cfg();
        let content = render_content(&SegmentId::Newline, &cfg, None, None);
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

    // ---- niubash#592 (S2): per-pass Vcs stamp ----
    //
    // The stamp machinery is platform-independent (only the computation
    // behind it is Windows-only), so the tri-state contract is pinned
    // here on every platform; the spawn-count gates live with the
    // Windows prompt tests in shell.rs.

    fn vcs_only_prompt() -> SegmentPrompt {
        SegmentPrompt::new(SegmentPromptConfig {
            left_elements: vec![SegmentId::Vcs],
            right_elements: Vec::new(),
            separator: " ".to_string(),
            prompt_symbol: "$".to_string(),
            preset: None,
        })
    }

    #[test]
    fn issue592_config_has_vcs_checks_both_sides() {
        assert!(vcs_only_prompt().config_has_vcs());
        let mut right_only = vcs_only_prompt();
        right_only.config.left_elements.clear();
        right_only.config.right_elements.push(SegmentId::Vcs);
        assert!(right_only.config_has_vcs());
        let no_vcs = SegmentPrompt::new(SegmentPromptConfig {
            left_elements: vec![SegmentId::Dir],
            right_elements: Vec::new(),
            separator: " ".to_string(),
            prompt_symbol: "$".to_string(),
            preset: None,
        });
        assert!(!no_vcs.config_has_vcs());
    }

    #[test]
    fn issue592_stamped_prompt_renders_its_pass_value() {
        let mut prompt = vcs_only_prompt();
        assert!(prompt.vcs_stamp().is_none(), "fresh prompt is unstamped");
        prompt.stamp_vcs(7, Some("hooked*".to_string()), None);
        let stamp = prompt.vcs_stamp().expect("stamped");
        assert_eq!(stamp.seq, 7);
        assert_eq!(stamp.value.as_deref(), Some("hooked*"));
        let left = prompt.render_left();
        assert!(left.contains("hooked"), "{left:?}");
    }

    #[test]
    fn issue592_stamped_none_renders_no_segment() {
        // A stamped `None` is a computed result ("no segment this
        // pass"), not a missing value: the render must re-read it and
        // stay empty rather than falling back to a fresh computation.
        let mut prompt = vcs_only_prompt();
        prompt.stamp_vcs(1, None, None);
        assert_eq!(prompt.render_left(), "");
        assert_eq!(prompt.render_right(), "");
    }

    #[test]
    fn issue787_landing_restamps_the_pass_clone_in_place() {
        // niubash#787: a pass stamped cold (no segment yet) renders
        // the landed value after the landing re-stamps its live
        // cell — the same write the idle-repaint registry performs —
        // without a new stamp and without touching the cache.
        let mut prompt = vcs_only_prompt();
        let worktree = PathBuf::from("/tmp/issue787-segments-repo");
        prompt.stamp_vcs(3, None, Some(worktree));
        assert_eq!(prompt.render_left(), "");
        let cell = prompt
            .live_vcs_cell()
            .expect("a stamped pass inside a worktree carries a live cell");
        assert!(cell.set_if_changed(&Some("main".to_string())));
        let left = prompt.render_left();
        assert!(left.contains("main"), "{left:?}");
        // The pass snapshot keeps its stamp-time identity: the live
        // cell is a re-stamp channel, not a second stamp.
        let stamp = prompt.vcs_stamp().expect("stamped");
        assert_eq!(stamp.seq, 3);
        assert_eq!(stamp.value, None);
    }

    /// Off-Windows the unstamped fallback is the empty segment (the Vcs
    /// surface is Windows-only), so an unstamped prompt renders nothing.
    #[cfg(not(windows))]
    #[test]
    fn issue592_unstamped_prompt_renders_empty_off_windows() {
        assert_eq!(vcs_only_prompt().render_left(), "");
    }

    // ---- SegmentId::Vcs (niubash#331) ----
    //
    // The Vcs segment reads the user's git and the prompt surface it belongs
    // to is Windows-only, so these tests are windows-gated like git_prompt.

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

    /// Minimal one-commit repository on branch `main` (clean index) — the
    /// shared fixture for the Vcs render matrix. Real git: the segment reads
    /// the user's git now, so the fixture drives the same path.
    #[cfg(windows)]
    fn init_clean_repo(dir: &Path) -> bool {
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        git(&["init", "--quiet", "--initial-branch=main", "."])
            && git(&["commit", "--quiet", "--allow-empty", "-m", "init"])
    }

    #[cfg(windows)]
    fn git_stdout(dir: &Path, args: &[&str]) -> String {
        match std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => String::new(),
        }
    }

    #[cfg(windows)]
    fn vcs_content_in(dir: &Path) -> Option<String> {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd = CwdGuard::enter(dir);
        // The segment is served from the #685 cross-pass cache: a cold
        // read returns None and lands its value from a background
        // refresh. Drive the read to its settled value the way
        // git_prompt's own probes do — trigger, wait for the landing
        // (event-driven, never a poll), read again.
        let _ = vcs_segment_content();
        let _ = crate::git_prompt::wait_for_segment_cache_idle(std::time::Duration::from_secs(30));
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
        if !init_clean_repo(tmp.path()) {
            return;
        }
        let content = vcs_content_in(tmp.path()).unwrap();
        assert_eq!(content, "main", "clean repo: bare branch, no star");
        assert!(!content.ends_with('*'), "{content:?}");
    }

    #[cfg(windows)]
    #[test]
    fn vcs_segment_marks_dirty_with_the_star() {
        let tmp = tempfile::tempdir().unwrap();
        if !init_clean_repo(tmp.path()) {
            return;
        }
        std::fs::write(tmp.path().join("tracked.txt"), b"dirty").unwrap();
        let content = vcs_content_in(tmp.path()).unwrap();
        assert_eq!(content, "main*", "dirty repo: branch + star");
    }

    #[cfg(windows)]
    #[test]
    fn vcs_segment_shows_short_sha_when_detached() {
        let tmp = tempfile::tempdir().unwrap();
        if !init_clean_repo(tmp.path()) {
            return;
        }
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["switch", "--detach", "HEAD"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if !status.map(|status| status.success()).unwrap_or(false) {
            return;
        }
        let head = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
        if head.len() < 7 {
            return;
        }
        let content = vcs_content_in(tmp.path()).unwrap();
        assert_eq!(content, &head[..7], "detached: short SHA");
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
            if !init_clean_repo(tmp.path()) {
                return;
            }
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
                // Warm the #685 segment cache to its settled value
                // first: render_left reads the cache and never waits,
                // so a cold render would show no Vcs segment at all.
                let _ = crate::git_prompt::branch_segment(tmp.path());
                let _ = crate::git_prompt::wait_for_segment_cache_idle(
                    std::time::Duration::from_secs(30),
                );
                prompt.render_left()
            };
            let ansi = if expected == Color::Cyan {
                "\u{1b}[36m"
            } else {
                "\u{1b}[33m"
            };
            assert!(
                left.contains(ansi),
                "dirty={dirty} must paint {expected:?}: {left:?}"
            );
            assert!(left.contains("main"), "dirty={dirty}: {left:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn probe_spawn_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        if !init_clean_repo(tmp.path()) {
            return;
        }
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd = CwdGuard::enter(tmp.path());
        let mut samples = Vec::new();
        for _ in 0..10 {
            let start = std::time::Instant::now();
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args([
                    "-c",
                    "core.fsmonitor=false",
                    "--no-optional-locks",
                    "status",
                    "--porcelain=v2",
                    "--branch",
                ])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            samples.push(start.elapsed());
        }
        samples.sort();
        eprintln!(
            "BASELINE direct spawn p50={:?} max={:?}",
            samples[5], samples[9]
        );
    }

    /// The relative budget: the segment may cost at most this multiple of the
    /// bare `git status` spawn it wraps, both measured in this process. The
    /// segment is the spawn plus a worktree walk and a capped scratch read,
    /// so a healthy ratio sits near 1.0-1.2; 1.5x still catches a second
    /// spawn sneaking in (~2x), a drain thread (a scheduler quantum, ~15 ms —
    /// the reason read_status uses a scratch file), or a hidden-console flag
    /// (measured +28 ms) while leaving the ratio room to breathe on a loaded
    /// runner.
    #[cfg(windows)]
    const VCS_BUDGET_RELATIVE_MULT: f64 = 1.5;
    /// The old flat budget, demoted to a soft warn (#337's spirit): printed
    /// for the forensics, never fatal — on the 2-core shared CI runner it
    /// false-armed twice (#423: runs ce5f225e p50 47.7 ms and 76f148b1
    /// p50 49.4 ms against max 73.2 ms, with the bare spawn alone costing
    /// that much there).
    #[cfg(windows)]
    const VCS_BUDGET_SOFT_WARN_MS: u128 = 45;
    /// The absolute hard cap (#337's hard gate): 3x the old flat budget. A
    /// hang, an unbounded status sweep, or a wedged child must fail even on a
    /// runner slow enough to drag the baseline up with it. The worst sample
    /// ever observed on the shared runner was 73.2 ms — under half of this.
    #[cfg(windows)]
    const VCS_BUDGET_HARD_CAP_MS: u128 = 135;

    /// One bare `git status`, shaped exactly like `git_prompt::read_status`'s
    /// spawn (same flags, same cwd, null stdin, scratch-file stdout, discarded
    /// stderr, same env): the in-process baseline the relative budget is keyed
    /// to. Anything the segment does beyond this spawn is what the ratio gate
    /// measures.
    #[cfg(windows)]
    fn bare_status_spawn(worktree: &Path) -> std::time::Duration {
        let scratch =
            std::env::temp_dir().join(format!("niu-budget-baseline-{}.out", std::process::id()));
        let start = std::time::Instant::now();
        if let Ok(sink) = std::fs::File::create(&scratch) {
            if let Ok(mut child) = std::process::Command::new("git")
                .arg("-c")
                .arg("core.fsmonitor=false")
                .arg("--no-optional-locks")
                .args(["status", "--porcelain=v2", "--branch"])
                .current_dir(worktree)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::from(sink))
                .stderr(std::process::Stdio::null())
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_TERMINAL_PROMPT", "0")
                .spawn()
            {
                let _ = child.wait();
            }
        }
        let elapsed = start.elapsed();
        let _ = std::fs::remove_file(&scratch);
        elapsed
    }

    #[cfg(windows)]
    #[test]
    fn vcs_segment_per_pass_computation_p50_within_the_budget() {
        // Issue #331 scope 4 originally held p50 <= 10ms around the in-process
        // git2 read. The segment runs the user's own git by design (see
        // git_prompt.rs: fidelity is the point): one bounded
        // `status --porcelain=v2 --branch` per command cycle per
        // repository, in the background since #685 — the same call as
        // the bare git baseline, so the refresh adds no overhead of its
        // own (the earlier CREATE_NO_WINDOW flag alone cost 28 ms by
        // forcing a fresh hidden console for the child). Git Bash's
        // `__git_ps1` forks three to five times for the same answer
        // (100-300 ms).
        //
        // The flat 45 ms budget was calibrated on a fast desktop (p50 26 ms)
        // and false-armed twice on the 2-core shared CI runner (#423): a
        // unit-level absolute p50 has no verdict on shared hardware. The gate
        // is therefore a two-door composite:
        //
        //   1. Primary, #604's ratio shape: segment p50 <= 1.5x the p50 of the
        //      bare spawn it wraps (VCS_BUDGET_RELATIVE_MULT), interleaved
        //      sample-for-sample so runner-wide load lands on both
        //      distributions alike. A uniform slowdown moves both numbers and
        //      the ratio holds; a real regression does not — a second spawn
        //      doubles the ratio, a fixed quantum shows up as ratio on any
        //      machine.
        //   2. Backstop: p50 over VCS_BUDGET_HARD_CAP_MS fails regardless of
        //      the ratio, so a hang or an unbounded sweep cannot hide behind
        //      an inflated baseline. VCS_BUDGET_SOFT_WARN_MS keeps the old
        //      budget visible as a warn only.
        //
        // niubash#592 (S2) premise change, stated openly: this gate used to
        // sample `vcs_segment_content()` — the per-RENDER computation, back
        // when every repaint re-ran it. Under S2 a render no longer
        // computes at all: `Shell::prompt_for_display` computes the value
        // once per prompt pass via `git_prompt::branch_segment` and the
        // pass's repaints re-read the stamp. The gate's subject therefore
        // moves to that per-pass computation, sampled here by calling
        // `branch_segment` directly (the same call the stamp performs).
        // The assertions are unchanged in form and value — segment p50
        // <= 1.5x the bare-spawn baseline p50 (VCS_BUDGET_RELATIVE_MULT),
        // the 45 ms soft warn, the 135 ms hard cap, and the
        // `bare_status_spawn` baseline are all exactly as before; only
        // the sampled entry point changed, from the render shim to the
        // per-pass computation. (The gate's previous name,
        // `vcs_segment_render_p50_within_the_render_budget`, changed with
        // its subject.)
        //
        // niubash#685 premise change, stated as openly: the sampled call
        // no longer computes on the pass at all — it reads the cross-pass
        // segment cache, so after warm-up the samples are cache hits and
        // the timing doors guard the READ (a regression that re-blocks
        // the pass on git would still trip the hard cap on a loaded
        // runner, but the sharp door is the new count assertion below:
        // the 21 sampled reads must add ZERO `read_status` calls for
        // this worktree).
        let tmp = tempfile::tempdir().unwrap();
        if !init_clean_repo(tmp.path()) {
            return;
        }

        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd = CwdGuard::enter(tmp.path());
        for _ in 0..3 {
            let _ = crate::git_prompt::branch_segment(tmp.path()); // warm-up
        }
        // Settle the warm-up's background refresh so the samples below
        // are pure cache reads (#685), then snapshot the spawn count.
        assert!(crate::git_prompt::wait_for_segment_cache_idle(
            std::time::Duration::from_secs(30)
        ));
        let key = crate::git_prompt::worktree_key_for(tmp.path()).unwrap();
        let spawns_before = crate::git_prompt::read_status_call_count(&key);
        const SAMPLES: usize = 21;
        let mut baseline: Vec<std::time::Duration> = Vec::with_capacity(SAMPLES);
        let mut samples: Vec<std::time::Duration> = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            baseline.push(bare_status_spawn(tmp.path()));
            let start = std::time::Instant::now();
            let _ = crate::git_prompt::branch_segment(tmp.path());
            samples.push(start.elapsed());
        }
        assert_eq!(
            crate::git_prompt::read_status_call_count(&key),
            spawns_before,
            "sampled segment reads must be cache hits — a pass never spawns"
        );
        baseline.sort();
        samples.sort();
        let baseline_p50 = baseline[SAMPLES / 2];
        let p50 = samples[SAMPLES / 2];
        // Floor guards the degenerate baseline (a spawn rounding to 0 ms)
        // so the ratio can never collapse the budget to nothing.
        let relative_budget = baseline_p50
            .mul_f64(VCS_BUDGET_RELATIVE_MULT)
            .max(std::time::Duration::from_millis(20));
        eprintln!(
            "VCS budget: baseline p50={baseline_p50:?} segment p50={p50:?} \
             ratio budget={relative_budget:?} ({VCS_BUDGET_RELATIVE_MULT}x) \
             max={max:?}",
            max = samples[SAMPLES - 1],
        );
        if p50.as_millis() > VCS_BUDGET_SOFT_WARN_MS {
            eprintln!(
                "WARN: Vcs segment p50 {p50:?} is over the \
                 {VCS_BUDGET_SOFT_WARN_MS}ms soft budget (baseline \
                 {baseline_p50:?}) — slow runner, reported, not gated"
            );
        }
        assert!(
            p50 <= relative_budget,
            "Vcs segment p50 {p50:?} exceeds {VCS_BUDGET_RELATIVE_MULT}x the \
             in-process bare-spawn baseline {baseline_p50:?} (budget \
             {relative_budget:?}) — the segment is adding cost of its own"
        );
        assert!(
            p50.as_millis() <= VCS_BUDGET_HARD_CAP_MS,
            "Vcs segment p50 {p50:?} exceeds the {VCS_BUDGET_HARD_CAP_MS}ms \
             hard cap (baseline {baseline_p50:?}, max {max:?}) — hang or \
             unbounded sweep",
            max = samples[SAMPLES - 1],
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
