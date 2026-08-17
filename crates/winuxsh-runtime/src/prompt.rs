//! Prompt rendering for winuxsh
//!
//! Implements the `reedline::Prompt` trait using a template string
//! with substitutions: {user}, {host}, {cwd}, {symbol}.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::git_status::{GitPromptSymbols, GitRepoStatus};
use crate::prompt_segments::SegmentPromptAdapter;
use crate::theme::{by_name, Theme};
use reedline::{
    Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, PromptViMode,
};

/// Prompt indicators rendered by reedline after the left prompt.
///
/// Defaults preserve the historical winuxsh behavior: the main prompt template
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
            vi_insert: String::new(),
            vi_normal: String::new(),
            multiline: "> ".to_string(),
            history_search: "(history search) ".to_string(),
            history_search_fail: "(history search) ".to_string(),
        }
    }
}

/// A prompt that renders the configured template with theme-aware ANSI colours.
pub struct WinuxshPrompt {
    template: String,
    right_template: Option<String>,
    git_prompt_format: Option<String>,
    git_prompt_snapshot: Option<String>,
    git_status_snapshot: Option<GitRepoStatus>,
    git_prompt_decor: GitPromptDecor,
    git_prompt_symbols: GitPromptSymbols,
    indicators: PromptIndicators,
    prompt_symbol: String,
    theme: Theme,
}

/// Theme/plugin-owned text around a host-provided Git status snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPromptDecor {
    pub prefix: String,
    pub suffix: String,
    pub dirty_suffix: String,
    pub clean_suffix: String,
}

impl Default for GitPromptDecor {
    fn default() -> Self {
        Self {
            prefix: "git:(".to_string(),
            suffix: ")".to_string(),
            dirty_suffix: " *".to_string(),
            clean_suffix: String::new(),
        }
    }
}

impl WinuxshPrompt {
    pub fn new(
        template: Option<String>,
        right_template: Option<String>,
        git_prompt_format: Option<String>,
        theme_name: &str,
    ) -> Self {
        Self::new_with_symbol(
            template,
            right_template,
            git_prompt_format,
            PromptIndicators::default(),
            theme_name,
            GitPromptSymbols::default(),
            "%".to_string(),
        )
    }

    pub fn new_with_indicators(
        template: Option<String>,
        right_template: Option<String>,
        git_prompt_format: Option<String>,
        indicators: PromptIndicators,
        theme_name: &str,
        symbols: GitPromptSymbols,
    ) -> Self {
        Self::new_with_symbol(
            template,
            right_template,
            git_prompt_format,
            indicators,
            theme_name,
            symbols,
            "%".to_string(),
        )
    }

    pub fn new_with_symbol(
        template: Option<String>,
        right_template: Option<String>,
        git_prompt_format: Option<String>,
        indicators: PromptIndicators,
        theme_name: &str,
        symbols: GitPromptSymbols,
        prompt_symbol: String,
    ) -> Self {
        let t = template.unwrap_or_else(|| "{user}@{host} {cwd} {git_prompt}%# ".to_string());
        Self {
            template: t,
            right_template,
            git_prompt_format,
            git_prompt_snapshot: None,
            git_status_snapshot: None,
            git_prompt_decor: GitPromptDecor::default(),
            indicators,
            git_prompt_symbols: symbols,
            prompt_symbol,
            theme: by_name(theme_name),
        }
    }

    pub fn set_git_prompt_snapshot(&mut self, snapshot: Option<String>) {
        self.git_prompt_snapshot = snapshot;
    }

    pub fn set_git_status_snapshot(&mut self, snapshot: Option<GitRepoStatus>) {
        self.git_status_snapshot = snapshot;
    }

    pub fn set_git_prompt_decor(&mut self, decor: GitPromptDecor) {
        self.git_prompt_decor = decor;
    }

    pub fn set_theme(&mut self, theme_name: &str) {
        self.theme = by_name(theme_name);
    }

    fn paint_git_status_detail(&self, compact: &str, dirty: bool) -> String {
        if compact.is_empty() {
            String::new()
        } else if dirty {
            self.theme.git_dirty.paint(compact).to_string()
        } else {
            self.theme.git_status_detail.paint(compact).to_string()
        }
    }

    fn render_git_prompt_from_status(&self, status: &GitRepoStatus) -> String {
        let Some(branch) = status.branch.as_deref().filter(|branch| !branch.is_empty()) else {
            return String::new();
        };

        let compact = status.compact_status_with(&self.git_prompt_symbols);
        let branch_colored = if status.dirty {
            self.theme.git_dirty.paint(branch).to_string()
        } else {
            self.theme.git_clean.paint(branch).to_string()
        };
        let status_colored = self.paint_git_status_detail(&compact, status.dirty);
        let mut body = match self.git_prompt_format.as_deref() {
            Some("{git_branch}") | None => branch_colored,
            Some(other) => other
                .replace("{git_branch}", &branch_colored)
                .replace("{git_status}", &status_colored),
        };
        if self.git_prompt_format.is_none() && !status_colored.is_empty() {
            body.push(' ');
            body.push_str(&status_colored);
        }

        let state_suffix = if status.dirty {
            &self.git_prompt_decor.dirty_suffix
        } else {
            &self.git_prompt_decor.clean_suffix
        };
        if !state_suffix.is_empty() {
            let suffix = if status.dirty {
                self.theme.git_dirty.paint(state_suffix).to_string()
            } else {
                self.theme.git_clean.paint(state_suffix).to_string()
            };
            body.push_str(&suffix);
        }

        format!(
            "{}{}{}",
            self.theme
                .git_status_detail
                .paint(&self.git_prompt_decor.prefix),
            body,
            self.theme
                .git_status_detail
                .paint(&self.git_prompt_decor.suffix)
        )
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

        let user_s = self.theme.prompt_user.paint(&user).to_string();
        let host_s = self.theme.prompt_host.paint(&host).to_string();
        let dir_s = self.theme.prompt_dir.paint(&cwd).to_string();
        let dir_base_s = self.theme.prompt_dir.paint(&cwd_base).to_string();
        let sym_s = self
            .theme
            .prompt_symbol
            .paint(&self.prompt_symbol)
            .to_string();
        let user_host_s = format!("{user_s}@{host_s}");
        let needs_git = template.contains("{git");
        let git_status: Option<GitRepoStatus> = if needs_git {
            self.git_status_snapshot.clone().or_else(|| {
                if self.git_prompt_snapshot.is_none() {
                    cwd_path
                        .as_deref()
                        .and_then(crate::git_status::collect_for_prompt)
                } else {
                    None
                }
            })
        } else {
            None
        };
        let git_branch = git_status
            .as_ref()
            .and_then(|s| s.branch.clone())
            .unwrap_or_default();
        let compact = git_status
            .as_ref()
            .map(|s| s.compact_status_with(&self.git_prompt_symbols))
            .unwrap_or_default();
        let git_dirty = git_status.as_ref().map(|s| s.dirty).unwrap_or(false);
        let git_branch_s = if git_branch.is_empty() {
            String::new()
        } else {
            self.theme.prompt_symbol.paint(&git_branch).to_string()
        };
        let git_status_s = self.paint_git_status_detail(&compact, git_dirty);
        let git_prompt_s = if let Some(status) = git_status.as_ref() {
            self.render_git_prompt_from_status(status)
        } else if let Some(snapshot) = &self.git_prompt_snapshot {
            snapshot.clone()
        } else if git_branch.is_empty() {
            String::new()
        } else {
            let branch_colored = if git_dirty {
                self.theme.git_dirty.paint(&git_branch).to_string()
            } else {
                self.theme.git_clean.paint(&git_branch).to_string()
            };
            let body = match self.git_prompt_format.as_deref() {
                Some("{git_branch}") | None => branch_colored,
                Some(other) => other
                    .replace("{git_branch}", &branch_colored)
                    .replace("{git_status}", &git_status_s),
            };
            if git_status_s.is_empty() {
                body
            } else {
                format!("{body} {git_status_s}")
            }
        };

        let time_str = format_local_time();
        let time_str_24 = time_str.clone();
        let command_execution_time = std::env::var("WINUXSH_LAST_COMMAND_DURATION")
            .or_else(|_| std::env::var("WINUXSH_CMD_EXEC_TIME_MS"))
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
            .replace("{git_prompt}", &git_prompt_s)
            .replace("{git}", &git_prompt_s)
            .replace("{git_branch}", &git_branch_s)
            .replace("{git_status}", &git_status_s)
            .replace("{git_dirty}", if git_dirty { "✚" } else { "" })
            .replace("{command_execution_time}", &command_execution_time)
            .replace(
                "{git_staged}",
                &git_status
                    .as_ref()
                    .map(|s| s.staged.to_string())
                    .unwrap_or_default(),
            )
            .replace(
                "{git_unstaged}",
                &git_status
                    .as_ref()
                    .map(|s| s.unstaged.to_string())
                    .unwrap_or_default(),
            )
            .replace(
                "{git_untracked}",
                &git_status
                    .as_ref()
                    .map(|s| s.untracked.to_string())
                    .unwrap_or_default(),
            )
            .replace(
                "{git_deleted}",
                &git_status
                    .as_ref()
                    .map(|s| s.deleted.to_string())
                    .unwrap_or_default(),
            )
            .replace(
                "{git_ahead}",
                &git_status
                    .as_ref()
                    .map(|s| s.ahead.to_string())
                    .unwrap_or_default(),
            )
            .replace(
                "{git_behind}",
                &git_status
                    .as_ref()
                    .map(|s| s.behind.to_string())
                    .unwrap_or_default(),
            )
            .replace(
                "{git_stashes}",
                &git_status
                    .as_ref()
                    .map(|s| s.stashes.to_string())
                    .unwrap_or_default(),
            )
            .replace(
                "{git_conflicts}",
                &git_status
                    .as_ref()
                    .map(|s| s.conflicts.to_string())
                    .unwrap_or_default(),
            )
            .replace(
                "%#",
                &self
                    .theme
                    .prompt_symbol
                    .paint(&self.prompt_symbol)
                    .to_string(),
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
        let last_status = std::env::var("WINUXSH_LAST_STATUS")
            .or_else(|_| std::env::var("WINUXSH_LAST_EXIT_CODE"))
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
    match std::env::var("WINUXSH_PROMPT_CWD_STYLE")
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

fn home_dir_for_prompt() -> Option<PathBuf> {
    crate::path_utils::shell_home_dir()
}

fn normalize_display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    normalize_display_path(left).eq_ignore_ascii_case(&normalize_display_path(right))
}

#[allow(dead_code)]
fn git_branch_from_dir(start: &Path) -> Option<String> {
    for dir in start.ancestors() {
        let git_path = dir.join(".git");
        if git_path.is_dir() {
            return git_branch_from_git_dir(&git_path);
        }
        if git_path.is_file() {
            let git_dir = read_gitdir_file(&git_path)?;
            let git_dir = if git_dir.is_absolute() {
                git_dir
            } else {
                dir.join(git_dir)
            };
            return git_branch_from_git_dir(&git_dir);
        }
    }
    None
}

fn read_gitdir_file(path: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(path).ok()?;
    let gitdir = content.trim().strip_prefix("gitdir:")?.trim();
    if gitdir.is_empty() {
        None
    } else {
        Some(PathBuf::from(gitdir))
    }
}

fn git_branch_from_git_dir(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        return (!branch.is_empty()).then(|| branch.to_string());
    }
    if head.len() >= 7 && head.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Some(head.chars().take(7).collect());
    }
    None
}

impl Prompt for WinuxshPrompt {
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
}

/// Bash-compatible prompt values rendered from PS1/PS2 after the shell has run
/// public Bash prompt hooks such as PROMPT_COMMAND.
pub struct BashPrompt {
    left: String,
    multiline: String,
}

impl BashPrompt {
    pub fn new(left: String, multiline: String) -> Self {
        Self { left, multiline }
    }
}

impl Prompt for BashPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
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
pub enum PromptBackend {
    Template(WinuxshPrompt),
    Segments(SegmentPromptAdapter),
    Bash(BashPrompt),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::PROCESS_STATE_LOCK;
    use std::process::Stdio;

    #[test]
    fn renders_optional_right_prompt() {
        let prompt = WinuxshPrompt::new(
            Some("left> ".to_string()),
            Some("right".to_string()),
            None,
            "default",
        );

        assert_eq!(prompt.render_prompt_right(), "right");
    }

    #[test]
    fn omits_right_prompt_when_unset() {
        let prompt = WinuxshPrompt::new(Some("left> ".to_string()), None, None, "default");

        assert_eq!(prompt.render_prompt_right(), "");
    }

    #[test]
    fn time_tokens_render_system_local_clock() {
        let prompt =
            WinuxshPrompt::new(Some("{time} {time_24}".to_string()), None, None, "default");

        let rendered = prompt.render_prompt_left();
        let expected = format_local_time();
        if rendered != format!("{expected} {expected}") {
            let expected = format_local_time();
            assert_eq!(rendered, format!("{expected} {expected}"));
        }
    }

    #[test]
    fn reads_branch_from_git_head() {
        let dir = unique_temp_dir("winuxsh-prompt-git-head");
        let git_dir = dir.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        assert_eq!(git_branch_from_dir(&dir).as_deref(), Some("main"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn renders_git_prompt_only_inside_git_repo() {
        let dir = unique_temp_dir("winuxsh-prompt-git-render");
        std::fs::create_dir_all(&dir).unwrap();
        init_real_repo(&dir);
        // Cache is cleared and warmed inside the process lock below to
        // avoid parallel-test cache eviction.
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap();
        // Clear + warm cache while holding the process lock, so a parallel
        // test cannot evict our cache entry before we render.
        crate::git_status::clear_cache();
        crate::git_status::collect(&dir);
        let _cwd = CwdGuard::enter(&dir);

        let prompt = WinuxshPrompt::new(
            Some("{git_prompt}".to_string()),
            None,
            Some("git:({git_branch})".to_string()),
            "default",
        );

        let rendered = prompt.render_prompt_left();
        assert!(rendered.contains("git:("));
        assert!(rendered.contains(")"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cwd_token_defaults_to_home_relative_display() {
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let home = unique_temp_dir("winuxsh-prompt-home");
        let project = home.join("repo").join("project");
        std::fs::create_dir_all(&project).unwrap();
        let _home = EnvGuard::set("HOME", &home.to_string_lossy());
        let _userprofile = EnvGuard::unset("USERPROFILE");
        let _style = EnvGuard::unset("WINUXSH_PROMPT_CWD_STYLE");
        let _cwd = CwdGuard::enter(&project);

        let prompt = WinuxshPrompt::new(
            Some("{cwd} {cwd_base} %~".to_string()),
            None,
            None,
            "default",
        );
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
        let _process_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let home = unique_temp_dir("winuxsh-prompt-shell-home");
        let project = home.join("repo").join("project");
        std::fs::create_dir_all(&project).unwrap();
        let _home = EnvGuard::set("HOME", &host_to_shell_style_path(&home));
        let _userprofile = EnvGuard::unset("USERPROFILE");
        let _style = EnvGuard::unset("WINUXSH_PROMPT_CWD_STYLE");
        let _cwd = CwdGuard::enter(&project);

        let prompt = WinuxshPrompt::new(Some("{cwd}".to_string()), None, None, "default");
        let rendered = prompt.render_prompt_left();

        assert!(rendered.contains("~/repo/project"), "{rendered:?}");
        assert!(!rendered.contains("/c/"), "{rendered:?}");
        assert!(!rendered.contains(&display_path(&home)), "{rendered:?}");

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn structured_git_snapshot_is_repainted_with_theme_colours() {
        let mut prompt = WinuxshPrompt::new(
            Some("{git}{prompt_char}".to_string()),
            None,
            None,
            "default",
        );
        prompt.set_git_status_snapshot(Some(GitRepoStatus {
            branch: Some("feature".to_string()),
            dirty: true,
            staged: 1,
            unstaged: 1,
            untracked: 1,
            deleted: 0,
            ahead: 0,
            behind: 0,
            stashes: 0,
            conflicts: 0,
        }));
        prompt.set_git_prompt_decor(GitPromptDecor {
            prefix: String::new(),
            suffix: String::new(),
            dirty_suffix: " *".to_string(),
            clean_suffix: String::new(),
        });

        let rendered = prompt.render_prompt_left();

        assert!(rendered.contains("feature"), "{rendered:?}");
        assert!(rendered.contains("\u{1b}["), "{rendered:?}");
        assert_eq!(
            ansi_style_before(&rendered, "feature"),
            ansi_style_before(&rendered, "● ✚ ?"),
            "{rendered:?}"
        );
        assert!(rendered.contains('%'), "{rendered:?}");
    }

    #[test]
    fn default_indicators_preserve_existing_behavior() {
        let prompt = WinuxshPrompt::new(Some("left> ".to_string()), None, None, "default");

        assert_eq!(prompt.render_prompt_indicator(PromptEditMode::Default), "");
        assert_eq!(prompt.render_prompt_indicator(PromptEditMode::Emacs), "");
        assert_eq!(
            prompt.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Insert)),
            ""
        );
        assert_eq!(
            prompt.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Normal)),
            ""
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
    fn renders_configured_prompt_indicators() {
        let prompt = WinuxshPrompt::new_with_indicators(
            Some("left> ".to_string()),
            None,
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
            "default",
            GitPromptSymbols::default(),
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

    fn ansi_style_before<'a>(rendered: &'a str, needle: &str) -> &'a str {
        let idx = rendered.find(needle).expect("needle should be rendered");
        let start = rendered[..idx]
            .rfind("\u{1b}[")
            .expect("style should exist");
        &rendered[start..idx]
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
    fn init_real_repo(dir: &Path) {
        let o = std::process::Command::new("git")
            .arg("init")
            .current_dir(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .expect("git init should succeed");
        assert!(o.status.success(), "git init failed");
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
