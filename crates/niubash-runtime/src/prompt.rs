//! Prompt rendering for niubash
//!
//! Implements the `reedline::Prompt` trait using a template string
//! with substitutions: {user}, {host}, {cwd}, {symbol}.

use std::borrow::Cow;
use std::path::Path;

use crate::prompt_segments::SegmentPromptAdapter;
use nu_ansi_term::{Color, Style};
use reedline::{
    Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, PromptViMode,
};

/// Prompt indicators rendered by reedline after the left prompt.
///
/// Defaults preserve the historical niubash behavior: the main prompt template
/// carries the visible symbol, while multiline and history search keep their
/// original built-in text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptIndicators {
    pub default: String,
    pub emacs: String,
    pub vi_insert: String,
    pub vi_normal: String,
    pub multiline: String,
    pub history_search: String,
    pub history_search_fail: String,
}

impl Default for PromptIndicators {
    fn default() -> Self {
        Self {
            default: String::new(),
            emacs: String::new(),
            // niubash#184: a minimal, theme-neutral vi-mode indicator pair
            // (insert / normal, ASCII so the editor's width math is exact
            // on every terminal). GNU bash itself renders no vi-mode
            // indicator (readline has no mode display); the pair follows
            // the `i` / `-` convention of bash vi-mode plugins, and stays
            // quiet in emacs mode like bash. Configurable through
            // PromptIndicators (config.shell.prompt_indicators).
            vi_insert: "i ".to_string(),
            vi_normal: "- ".to_string(),
            multiline: "> ".to_string(),
            history_search: "(history search) ".to_string(),
            history_search_fail: "(history search) ".to_string(),
        }
    }
}

/// The mode indicator for a live reedline edit mode, given the vi pair.
///
/// Emacs/default render nothing (bash has no emacs-mode indicator); vi
/// visual reuses the normal indicator, like reedline's own DefaultPrompt
/// (prompt/default.rs:64 reuses DEFAULT_VI_NORMAL_PROMPT_INDICATOR for
/// Visual). Used by the continuation prompt; the template prompt keeps its
/// configurable `{mode}`-template mapping.
pub(crate) fn mode_indicator_for(mode: PromptEditMode, vi_insert: &str, vi_normal: &str) -> String {
    match mode {
        PromptEditMode::Vi(PromptViMode::Insert) => vi_insert.to_string(),
        PromptEditMode::Vi(_) => vi_normal.to_string(),
        _ => String::new(),
    }
}

/// Fixed prompt colour schema. The built-in theme stack (NIU_THEME lookup,
/// user TOML themes, bundle themes) is retired with niubash#145; external
/// themes style themselves through the bash PS1/PROMPT_COMMAND channel.
#[derive(Clone)]
struct PromptStyles {
    user: Style,
    host: Style,
    dir: Style,
    symbol: Style,
}

impl Default for PromptStyles {
    fn default() -> Self {
        Self {
            user: Style::new().bold().fg(Color::Default),
            host: Style::new().bold().fg(Color::Default),
            dir: Style::new().bold().fg(Color::Default),
            symbol: Style::new().fg(Color::Default),
        }
    }
}

/// A prompt that renders the configured template with ANSI colours.
#[derive(Clone)]
pub struct NiubashPrompt {
    template: String,
    right_template: Option<String>,
    indicators: PromptIndicators,
    prompt_symbol: String,
    styles: PromptStyles,
}

impl NiubashPrompt {
    pub fn new(template: Option<String>, right_template: Option<String>) -> Self {
        Self::new_with_symbol(
            template,
            right_template,
            PromptIndicators::default(),
            "%".to_string(),
        )
    }

    pub fn new_with_indicators(
        template: Option<String>,
        right_template: Option<String>,
        indicators: PromptIndicators,
    ) -> Self {
        Self::new_with_symbol(template, right_template, indicators, "%".to_string())
    }

    pub fn new_with_symbol(
        template: Option<String>,
        right_template: Option<String>,
        indicators: PromptIndicators,
        prompt_symbol: String,
    ) -> Self {
        let t = template.unwrap_or_else(|| "{user}@{host} {cwd} %# ".to_string());
        Self {
            template: t,
            right_template,
            indicators,
            prompt_symbol,
            styles: PromptStyles::default(),
        }
    }

    fn render_template(&self, template: &str, status_token: Option<&str>) -> String {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "?".to_string());
        let host = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "winhost".to_string());
        let cwd_path = std::env::current_dir().ok();
        let cwd = cwd_path
            .as_deref()
            .map(display_cwd)
            .unwrap_or_else(|| "?".to_string());
        let cwd_base = cwd_path
            .as_deref()
            .and_then(display_cwd_base)
            .unwrap_or_else(|| cwd.clone());

        let user_s = self.styles.user.paint(&user).to_string();
        let host_s = self.styles.host.paint(&host).to_string();
        let dir_s = self.styles.dir.paint(&cwd).to_string();
        let dir_base_s = self.styles.dir.paint(&cwd_base).to_string();
        let sym_s = self.styles.symbol.paint(&self.prompt_symbol).to_string();
        let user_host_s = format!("{user_s}@{host_s}");

        let time_str = format_local_time();
        let time_str_24 = time_str.clone();
        let command_execution_time = std::env::var("NIU_LAST_COMMAND_DURATION")
            .or_else(|_| std::env::var("NIU_CMD_EXEC_TIME_MS"))
            .unwrap_or_default();

        let mut rendered = template
            .replace("{time}", &time_str)
            .replace("{time_24}", &time_str_24)
            .replace("{user}", &user_s)
            .replace("{host}", &host_s)
            .replace("{user_host}", &user_host_s)
            .replace("{cwd}", &dir_s)
            .replace("{cwd_base}", &dir_base_s)
            .replace("{symbol}", &sym_s)
            .replace("{prompt_char}", &sym_s)
            .replace("{newline}", "\n")
            .replace("{command_execution_time}", &command_execution_time)
            .replace(
                "%#",
                &self.styles.symbol.paint(&self.prompt_symbol).to_string(),
            )
            .replace("%n", &user)
            .replace("%m", &host)
            .replace("%~", &cwd);
        if let Some(status_token) = status_token {
            rendered = rendered.replace("{status}", status_token);
        }
        rendered
    }

    fn render_prompt_template(&self, template: &str) -> String {
        let last_status = std::env::var("NIU_LAST_STATUS")
            .or_else(|_| std::env::var("NIU_LAST_EXIT_CODE"))
            .unwrap_or_default();
        let status_token = if last_status.is_empty() || last_status == "0" {
            String::new()
        } else {
            format!("status:{last_status} ")
        };
        self.render_template(template, Some(&status_token))
    }

    fn render_indicator_template(&self, template: &str, mode: &str) -> String {
        self.render_template(template, None).replace("{mode}", mode)
    }

    fn render_history_search_template(&self, template: &str, status: &str, term: &str) -> String {
        self.render_template(template, None)
            .replace("{status}", status)
            .replace("{term}", term)
    }
}

pub(crate) fn format_local_time() -> String {
    let (hours, mins) = local_hour_minute();
    format!("{:02}:{:02}", hours, mins)
}

#[cfg(windows)]
fn local_hour_minute() -> (u32, u32) {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;

    let mut local_time = std::mem::MaybeUninit::<SYSTEMTIME>::zeroed();
    unsafe {
        GetLocalTime(local_time.as_mut_ptr());
        let local_time = local_time.assume_init();
        (local_time.wHour as u32, local_time.wMinute as u32)
    }
}

#[cfg(not(windows))]
fn local_hour_minute() -> (u32, u32) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs = now % 86400;
    ((secs / 3600) as u32, ((secs % 3600) / 60) as u32)
}

fn display_cwd(path: &Path) -> String {
    let path = crate::path_utils::normalize_existing_host_path(path.to_path_buf());
    match std::env::var("NIU_PROMPT_CWD_STYLE")
        .unwrap_or_else(|_| "home".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "full" | "absolute" => normalize_display_path(&path),
        "basename" | "short" => {
            display_cwd_base(&path).unwrap_or_else(|| normalize_display_path(&path))
        }
        _ => home_relative_display_path(&path),
    }
}

fn display_cwd_base(path: &Path) -> Option<String> {
    let path = crate::path_utils::normalize_existing_host_path(path.to_path_buf());
    if paths_equal(&path, &home_dir_for_prompt()?) {
        return Some("~".to_string());
    }
    path.file_name().and_then(|name| {
        let value = name.to_string_lossy();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn home_relative_display_path(path: &Path) -> String {
    let path = crate::path_utils::normalize_existing_host_path(path.to_path_buf());
    let Some(home) = home_dir_for_prompt() else {
        return normalize_display_path(&path);
    };
    if paths_equal(&path, &home) {
        return "~".to_string();
    }
    if let Ok(relative) = path.strip_prefix(&home) {
        let relative = normalize_display_path(relative);
        if relative.is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", relative.trim_start_matches('/'))
        }
    } else {
        normalize_display_path(&path)
    }
}

fn home_dir_for_prompt() -> Option<std::path::PathBuf> {
    crate::path_utils::shell_home_dir()
}

fn normalize_display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    normalize_display_path(left).eq_ignore_ascii_case(&normalize_display_path(right))
}

impl Prompt for NiubashPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Owned(self.render_prompt_template(&self.template))
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        match &self.right_template {
            Some(template) => Cow::Owned(self.render_prompt_template(template)),
            None => Cow::Borrowed(""),
        }
    }

    fn render_prompt_indicator(&self, mode: PromptEditMode) -> Cow<'_, str> {
        let (template, mode_name): (&String, Cow<'_, str>) = match mode {
            PromptEditMode::Default => (&self.indicators.default, Cow::Borrowed("default")),
            PromptEditMode::Emacs => (&self.indicators.emacs, Cow::Borrowed("emacs")),
            PromptEditMode::Vi(PromptViMode::Insert) => {
                (&self.indicators.vi_insert, Cow::Borrowed("vi_insert"))
            }
            PromptEditMode::Vi(PromptViMode::Normal) => {
                (&self.indicators.vi_normal, Cow::Borrowed("vi_normal"))
            }
            PromptEditMode::Vi(_) => (&self.indicators.vi_normal, Cow::Borrowed("vi_visual")),
            PromptEditMode::Helix(_) => (&self.indicators.default, Cow::Borrowed("helix")),
            PromptEditMode::Custom(mode) => (&self.indicators.default, Cow::Owned(mode)),
        };
        Cow::Owned(self.render_indicator_template(template, &mode_name))
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Owned(self.render_template(&self.indicators.multiline, None))
    }

    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        let (template, status) = match search.status {
            PromptHistorySearchStatus::Passing => (&self.indicators.history_search, "passing"),
            PromptHistorySearchStatus::Failing => (&self.indicators.history_search_fail, "failing"),
        };
        Cow::Owned(self.render_history_search_template(template, status, &search.term))
    }

    fn get_indicator_color(&self) -> Color {
        // Theme-neutral vi-mode indicator: reedline's default paints the
        // indicator cyan; niubash's floor prompt stays uncolored (the
        // built-in theme stack is retired, niubash#145), so the "i "/"- "
        // pair renders in the terminal's default color.
        Color::Default
    }
}

/// Bash-compatible prompt values rendered from PS1/PS2 after the shell has run
/// public Bash prompt hooks such as PROMPT_COMMAND.
///
/// The rendered PS1 may carry the theme's own right-align cursor surgery
/// (`CSI 500 C` + `CSI n D`, oh-my-bash/bash-it powerline-multiline). The
/// line editor walks the prompt as a linear text run and strips ANSI, so the
/// surgery bytes detach its last-line visible width and row count from the
/// real render and the editing cursor lands away from the input line
/// (niubash#169). `BashPrompt::new` therefore splits the surgery at the
/// channel ([`crate::prompt_right_align::split_right_align`]): the aligned
/// tail is served through `render_prompt_right`, which the editor positions
/// from the tail's own escape-excluded visible width.
#[derive(Clone)]
pub struct BashPrompt {
    left: String,
    right: Option<String>,
    right_on_last_line: bool,
    multiline: String,
}

impl BashPrompt {
    pub fn new(left: String, multiline: String) -> Self {
        // GNU readline display.c:437-463 (expand_prompt): the \x01/\x02
        // prompt-ignore markers (RL_PROMPT_START/END_IGNORE, emitted by
        // decode_prompt_string parse.y:6609-6622 for `\[`/`\]` when line
        // editing is active) are width-accounting delimiters only — the
        // DISPLAYED prompt is assembled without them, so the terminal never
        // receives those bytes (byte-verified WSL GNU bash 5.3.0 piped-`-i`).
        // Reedline has no marker concept: it prints render_prompt_* output
        // verbatim, so a `set -o emacs`/`set -o vi` session (themes set
        // both) leaked the raw bytes to ConPTY and broke strict VT parsers
        // (niubash#431, the wt91-allthemes F9 family). Strip them once here
        // — the channel boundary every editor-rendered surface (left,
        // right-align tail, multiline/PS2 indicator) is built from — instead
        // of per render site. Width accounting is unchanged: the markers are
        // zero-width (unicode-width control chars) and the SGR content they
        // wrap was already escape-excluded by the editor's own strip_ansi
        // math. PS0 is a DIFFERENT channel and keeps its markers: GNU writes
        // the decoded string straight to stderr without a readline pass
        // (eval.c:164-176 fprintf; byte-verified: PS0='\[\e[31mPRE\]' puts
        // literal \x01 \x1b [31m PRE \x01 \x1b [0m \x02 on stderr).
        let strip_markers = |value: String| -> String {
            value
                .chars()
                .filter(|ch| *ch != '\x01' && *ch != '\x02')
                .collect()
        };
        let left = strip_markers(left);
        let multiline = strip_markers(multiline);
        let columns = crate::terminal::terminal_columns();
        let split = crate::prompt_right_align::split_right_align(&left, columns);
        Self {
            left: split.left,
            right: split.right,
            right_on_last_line: split.right_on_last_line,
            // PS2 (the continuation indicator) passes through untouched:
            // real themes do not right-align it, and a dropped tail would
            // have no editor surface to render on.
            multiline,
        }
    }
}

impl Prompt for BashPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        match &self.right {
            Some(right) => Cow::Borrowed(right),
            None => Cow::Borrowed(""),
        }
    }

    fn right_prompt_on_last_line(&self) -> bool {
        self.right_on_last_line
    }

    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.multiline)
    }

    fn render_prompt_history_search_indicator(&self, _search: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Borrowed("(history search) ")
    }
}

/// Backend selector for the prompt: legacy template engine or new segment engine.
#[derive(Clone)]
pub enum PromptBackend {
    Template(NiubashPrompt),
    Segments(SegmentPromptAdapter),
    Bash(BashPrompt),
}

impl PromptBackend {
    /// The vi-mode indicator strings a continuation read should carry.
    ///
    /// Only the template backend carries configured indicators; the segment
    /// and claimed-PS1 (Bash) backends render no indicator of their own
    /// (segments presets own their look; a claimed PS1 means the user's
    /// theme owns the prompt slot — GNU bash likewise ships no built-in
    /// indicator, vi-mode plugins add their own into PS1), so those use the
    /// default pair.
    pub(crate) fn vi_indicators(&self) -> (String, String) {
        match self {
            PromptBackend::Template(prompt) => (
                prompt.indicators.vi_insert.clone(),
                prompt.indicators.vi_normal.clone(),
            ),
            PromptBackend::Segments(_) | PromptBackend::Bash(_) => {
                let defaults = PromptIndicators::default();
                (defaults.vi_insert, defaults.vi_normal)
            }
        }
    }
}

/// Whether a PS1 value carries Git Bash (MSYS2) session machinery that has no
/// meaning inside this shell (unixwin/niubash#117).
///
/// Git for Windows exports its interactive-session PS1 into every child
/// process. That value references machinery which only exists inside the Git
/// Bash session that rendered it: the `__git_ps1` function from git-prompt.sh
/// (loaded only into interactive Git Bash), the `$MSYSTEM`/`$TITLEPREFIX`
/// session variables, and the OSC-title prefix bracket pair in Git Bash's
/// default skeleton. Adopting it here means expanding `__git_ps1` on every
/// prompt render (a "command not found" error line per prompt) and replacing
/// the niubash theme with the foreign session's shape.
///
/// This is a class check over that machinery, deliberately NOT applied to
/// later PS1 values: the caller gates it by provenance (only the value
/// inherited from the process environment is eligible for discarding), so a
/// PS1 the user sets in their own startup rc keeps full effect.
pub(crate) fn is_foreign_git_bash_ps1(value: &str) -> bool {
    const GIT_BASH_SESSION_MARKERS: &[&str] = &[
        // git-prompt.sh function, only defined inside interactive Git Bash
        "__git_ps1",
        // Git Bash default PS1 skeleton: bash-bracketed OSC title prefix
        r"\[\e]0;",
        r"\[\033]0;",
        // Git Bash session variables ($MSYSTEM renders "MINGW64"/"MSYS")
        "$MSYSTEM",
        "${MSYSTEM}",
        "$TITLEPREFIX",
        "${TITLEPREFIX}",
    ];
    GIT_BASH_SESSION_MARKERS
        .iter()
        .any(|marker| value.contains(marker))
}

impl Prompt for PromptBackend {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        match self {
            PromptBackend::Template(p) => p.render_prompt_left(),
            PromptBackend::Segments(p) => p.render_prompt_left(),
            PromptBackend::Bash(p) => p.render_prompt_left(),
        }
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        match self {
            PromptBackend::Template(p) => p.render_prompt_right(),
            PromptBackend::Segments(p) => p.render_prompt_right(),
            PromptBackend::Bash(p) => p.render_prompt_right(),
        }
    }

    fn render_prompt_indicator(&self, mode: PromptEditMode) -> Cow<'_, str> {
        match self {
            PromptBackend::Template(p) => p.render_prompt_indicator(mode),
            PromptBackend::Segments(p) => p.render_prompt_indicator(mode),
            PromptBackend::Bash(p) => p.render_prompt_indicator(mode),
        }
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        match self {
            PromptBackend::Template(p) => p.render_prompt_multiline_indicator(),
            PromptBackend::Segments(p) => p.render_prompt_multiline_indicator(),
            PromptBackend::Bash(p) => p.render_prompt_multiline_indicator(),
        }
    }

    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        match self {
            PromptBackend::Template(p) => p.render_prompt_history_search_indicator(search),
            PromptBackend::Segments(p) => p.render_prompt_history_search_indicator(search),
            PromptBackend::Bash(p) => p.render_prompt_history_search_indicator(search),
        }
    }

    fn get_indicator_color(&self) -> Color {
        // Must reach the backend: the editor renders through this enum, and
        // the default would paint the vi indicator in reedline's cyan even
        // though every backend wants the theme-neutral terminal default
        // (niubash#184).
        match self {
            PromptBackend::Template(p) => p.get_indicator_color(),
            PromptBackend::Segments(_) | PromptBackend::Bash(_) => Color::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::PROCESS_STATE_LOCK;
    use std::path::PathBuf;

    #[test]
    fn renders_optional_right_prompt() {
        let prompt = NiubashPrompt::new(Some("left> ".to_string()), Some("right".to_string()));

        assert_eq!(prompt.render_prompt_right(), "right");
    }

    // niubash#431 (wt91-allthemes F9 family): the engine's decode_prompt_string
    // emits the readline prompt-ignore markers \x01/\x02 (RL_PROMPT_START/
    // END_IGNORE) for `\[`/`\]` whenever line editing is active — a `set -o
    // emacs`/`set -o vi` theme or user rc flips it on. GNU's readline strips
    // them before display (display.c:437-463 expand_prompt, width accounting
    // kept); reedline has no marker concept and prints render_prompt_* output
    // verbatim, so BashPrompt — the channel every editor-rendered surface is
    // built from — must strip them itself. Byte-verified WSL GNU 5.3.0: the
    // terminal-bound prompt carries no \x01/\x02; PS0 is a DIFFERENT channel
    // and keeps them (eval.c:164-176 writes the decoded string straight to
    // stderr — verified `\x1b[31m` arrives wrapped in the raw marker bytes).

    #[test]
    fn bash_prompt_strips_ignore_markers_from_left_right_and_multiline() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // bash-it colors.bash shape: purple="\[\e[0;35m\]" -> marker-wrapped
        // SGR in the rendered PS1, plus a marker-wrapped right-align tail and
        // a marker-wrapped PS2 continuation indicator.
        let left =
            "\x01\x1b[0;35m\x02user@h \x01\x1b[31m\x02\x1b[500C\x1b[6D\x01\x1b[0m\x02RIGHT\n$ ";
        let multiline = "> \x01\x1b[32m\x02cont\x01\x1b[0m\x02 ";
        let prompt = BashPrompt::new(left.to_string(), multiline.to_string());
        let rendered_left = prompt.render_prompt_left();
        let rendered_right = prompt.render_prompt_right();
        let rendered_multiline = prompt.render_prompt_multiline_indicator();
        for (channel, text) in [
            ("left", &rendered_left),
            ("right", &rendered_right),
            ("multiline", &rendered_multiline),
        ] {
            assert!(
                !text.contains('\x01') && !text.contains('\x02'),
                "{channel} channel leaks ignore markers: {text:?}"
            );
        }
        // The printable text and SGR colouring survive the strip.
        assert!(rendered_left.contains("user@h"), "{rendered_left:?}");
        assert!(
            rendered_left.contains('\x1b'),
            "SGR lost: {rendered_left:?}"
        );
        assert!(
            rendered_multiline.contains("cont"),
            "{rendered_multiline:?}"
        );
    }

    #[test]
    fn bash_prompt_marker_strip_keeps_right_align_split_working() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // The oh-my-bash powerline-multiline shape (niubash#169): the surgery
        // jump must still split off a right prompt after the markers are
        // dropped — the strip may not reintroduce the linear-text-run
        // accounting disconnect the split exists to fix.
        let left = "left\x1b[500C\x1b[6DRIGHT $ ";
        let prompt = BashPrompt::new(left.to_string(), "> ".to_string());
        assert_eq!(prompt.render_prompt_right(), "RIGHT $ ");
        assert_eq!(prompt.render_prompt_left(), "left");
    }

    #[test]
    fn foreign_git_bash_ps1_detection_covers_session_machinery() {
        // unixwin/niubash#117: the exact PS1 Git for Windows 2.54 exports
        // into child processes (from the issue report).
        let git_bash_default = concat!(
            r"\[\033]0;$TITLEPREFIX:$PWD\007\]",
            r"\n\[\033[32m\]\u@\h ",
            r"\[\033[35m\]$MSYSTEM ",
            r"\[\033[33m\]\w\[\033[36m\]`__git_ps1`\[\033[0m\]",
            r"\n$ "
        );
        assert!(is_foreign_git_bash_ps1(git_bash_default));
        // Minimal reproducers from the reopened issue (v1.2.4 retest).
        assert!(is_foreign_git_bash_ps1(r"\[\033]0;x\007\]`__git_ps1` $ "));
        assert!(is_foreign_git_bash_ps1(r"\[\e]0;$PWD\007\]\$ "));
        assert!(is_foreign_git_bash_ps1(r"\u@\h $MSYSTEM \w $ "));
        // A user's own (or plain GNU) PS1 carries none of the session
        // machinery and must never be discarded.
        assert!(!is_foreign_git_bash_ps1("\\s-\\v\\$ "));
        assert!(!is_foreign_git_bash_ps1(r"\u@\h:\w\$ "));
        assert!(!is_foreign_git_bash_ps1("niu> "));
        assert!(!is_foreign_git_bash_ps1(""));
        // A portable OSC title written directly (not in the Git Bash
        // \[...] bracket skeleton) is user content, not session machinery.
        assert!(!is_foreign_git_bash_ps1("\x1b]0;my title\x07$ "));
    }

    #[test]
    fn omits_right_prompt_when_unset() {
        let prompt = NiubashPrompt::new(Some("left> ".to_string()), None);

        assert_eq!(prompt.render_prompt_right(), "");
    }

    #[test]
    fn time_tokens_render_system_local_clock() {
        let prompt = NiubashPrompt::new(Some("{time} {time_24}".to_string()), None);

        let rendered = prompt.render_prompt_left();
        let expected = format_local_time();
        if rendered != format!("{expected} {expected}") {
            let expected = format_local_time();
            assert_eq!(rendered, format!("{expected} {expected}"));
        }
    }

    #[test]
    fn cwd_token_defaults_to_home_relative_display() {
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = unique_temp_dir("niubash-prompt-home");
        let project = home.join("repo").join("project");
        std::fs::create_dir_all(&project).unwrap();
        let _home = EnvGuard::set("HOME", &home.to_string_lossy());
        let _userprofile = EnvGuard::unset("USERPROFILE");
        let _style = EnvGuard::unset("NIU_PROMPT_CWD_STYLE");
        let _cwd = CwdGuard::enter(&project);

        let prompt = NiubashPrompt::new(Some("{cwd} {cwd_base} %~".to_string()), None);
        let rendered = prompt.render_prompt_left();

        assert!(rendered.contains("~/repo/project"), "{rendered:?}");
        assert!(rendered.contains("project"), "{rendered:?}");
        assert!(
            !rendered.contains(&home.to_string_lossy().to_string()),
            "{rendered:?}"
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn cwd_token_accepts_shell_style_home_env() {
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = unique_temp_dir("niubash-prompt-shell-home");
        let project = home.join("repo").join("project");
        std::fs::create_dir_all(&project).unwrap();
        let _home = EnvGuard::set("HOME", &host_to_shell_style_path(&home));
        let _userprofile = EnvGuard::unset("USERPROFILE");
        let _style = EnvGuard::unset("NIU_PROMPT_CWD_STYLE");
        let _cwd = CwdGuard::enter(&project);

        let prompt = NiubashPrompt::new(Some("{cwd}".to_string()), None);
        let rendered = prompt.render_prompt_left();

        assert!(rendered.contains("~/repo/project"), "{rendered:?}");
        assert!(!rendered.contains("/c/"), "{rendered:?}");
        assert!(!rendered.contains(&display_path(&home)), "{rendered:?}");

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn default_indicators_render_vi_pair_and_stay_quiet_in_emacs() {
        // niubash#184: emacs mode stays quiet (bash ships no emacs-mode
        // indicator); vi insert/normal render the minimal ASCII pair so a
        // vi user can tell the modes apart (dd before you know the mode is
        // how edit sessions get destroyed).
        let prompt = NiubashPrompt::new(Some("left> ".to_string()), None);

        assert_eq!(prompt.render_prompt_indicator(PromptEditMode::Default), "");
        assert_eq!(prompt.render_prompt_indicator(PromptEditMode::Emacs), "");
        assert_eq!(
            prompt.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Insert)),
            "i "
        );
        assert_eq!(
            prompt.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Normal)),
            "- "
        );
        assert_eq!(prompt.render_prompt_multiline_indicator(), "> ");
        assert_eq!(
            prompt.render_prompt_history_search_indicator(PromptHistorySearch::new(
                PromptHistorySearchStatus::Passing,
                "git".to_string(),
            )),
            "(history search) "
        );
    }

    #[test]
    fn mode_indicator_for_maps_live_reedline_mode() {
        // The continuation prompt maps the LIVE PromptEditMode reedline
        // passes per repaint: ESC inside a multi-line edit must flip the
        // indicator without extra plumbing.
        assert_eq!(mode_indicator_for(PromptEditMode::Emacs, "i ", "- "), "");
        assert_eq!(mode_indicator_for(PromptEditMode::Default, "i ", "- "), "");
        assert_eq!(
            mode_indicator_for(PromptEditMode::Vi(PromptViMode::Insert), "i ", "- "),
            "i "
        );
        assert_eq!(
            mode_indicator_for(PromptEditMode::Vi(PromptViMode::Normal), "i ", "- "),
            "- "
        );
        // Visual reuses the normal indicator (reedline DefaultPrompt does
        // the same for its defaults).
        assert_eq!(
            mode_indicator_for(PromptEditMode::Vi(PromptViMode::Visual), "i ", "- "),
            "- "
        );
    }

    #[test]
    fn continuation_prompt_carries_vi_indicators() {
        use crate::repl::ContinuationPrompt;

        let backend = PromptBackend::Template(NiubashPrompt::new(Some("P1> ".into()), None));
        let continuation = ContinuationPrompt::new(&backend, backend.vi_indicators());

        assert_eq!(
            continuation.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Insert)),
            "i "
        );
        assert_eq!(
            continuation.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Normal)),
            "- "
        );
        assert_eq!(
            continuation.render_prompt_indicator(PromptEditMode::Emacs),
            ""
        );
    }

    #[test]
    fn renders_configured_prompt_indicators() {
        let prompt = NiubashPrompt::new_with_indicators(
            Some("left> ".to_string()),
            None,
            PromptIndicators {
                default: "[{mode}] ".to_string(),
                emacs: "E ".to_string(),
                vi_insert: "I ".to_string(),
                vi_normal: "N ".to_string(),
                multiline: "M ".to_string(),
                history_search: "search:{term}:{status} ".to_string(),
                history_search_fail: "fail:{term}:{status} ".to_string(),
            },
        );

        assert_eq!(
            prompt.render_prompt_indicator(PromptEditMode::Default),
            "[default] "
        );
        assert_eq!(prompt.render_prompt_indicator(PromptEditMode::Emacs), "E ");
        assert_eq!(
            prompt.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Insert)),
            "I "
        );
        assert_eq!(
            prompt.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Normal)),
            "N "
        );
        assert_eq!(prompt.render_prompt_multiline_indicator(), "M ");
        assert_eq!(
            prompt.render_prompt_history_search_indicator(PromptHistorySearch::new(
                PromptHistorySearchStatus::Passing,
                "git".to_string(),
            )),
            "search:git:passing "
        );
        assert_eq!(
            prompt.render_prompt_history_search_indicator(PromptHistorySearch::new(
                PromptHistorySearchStatus::Failing,
                "oops".to_string(),
            )),
            "fail:oops:failing "
        );
    }

    #[test]
    fn git_prompt_tokens_are_gone_after_retirement() {
        // The host no longer renders git status (issue #145): template text
        // mentioning the retired tokens must render as plain braces, and the
        // default template must not contain {git_prompt}.
        let prompt = NiubashPrompt::new(Some("{git} {git_branch}".to_string()), None);
        let rendered = prompt.render_prompt_left();
        assert!(rendered.contains("{git}"), "{rendered:?}");
        assert!(rendered.contains("{git_branch}"), "{rendered:?}");

        let default = NiubashPrompt::new(None, None);
        let rendered = default.render_prompt_left();
        assert!(!rendered.contains("{git"), "{rendered:?}");
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
    }

    fn host_to_shell_style_path(path: &Path) -> String {
        let display = display_path(path);
        if cfg!(windows) && display.len() >= 3 && display.as_bytes()[1] == b':' {
            let drive = (display.as_bytes()[0] as char).to_ascii_lowercase();
            format!("/{drive}/{}", &display[3..])
        } else {
            display
        }
    }

    fn display_path(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
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

        fn unset(name: &'static str) -> Self {
            let previous = std::env::var_os(name);
            std::env::remove_var(name);
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

    struct CwdGuard {
        previous: PathBuf,
    }

    impl CwdGuard {
        fn enter(path: &Path) -> Self {
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            Self { previous }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }
}
