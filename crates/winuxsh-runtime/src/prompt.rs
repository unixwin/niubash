//! Prompt rendering for winuxsh
//!
//! Implements the `reedline::Prompt` trait using a template string
//! with substitutions: {user}, {host}, {cwd}, {symbol}.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::git_status::GitPromptSymbols;
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
    git_prompt_symbols: GitPromptSymbols,
    indicators: PromptIndicators,
    prompt_symbol: String,
    theme: Theme,
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
            indicators,
            git_prompt_symbols: symbols,
            prompt_symbol,
            theme: by_name(theme_name),
        }
    }

    pub fn set_theme(&mut self, theme_name: &str) {
        self.theme = by_name(theme_name);
    }

    fn render_template(&self, template: &str) -> String {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "?".to_string());
        let host = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "winhost".to_string());
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "?".to_string());

        let user_s = self.theme.prompt_user.paint(&user).to_string();
        let host_s = self.theme.prompt_host.paint(&host).to_string();
        let dir_s = self.theme.prompt_dir.paint(&cwd).to_string();
        let sym_s = self
            .theme
            .prompt_symbol
            .paint(&self.prompt_symbol)
            .to_string();
        let git_status: Option<crate::git_status::GitRepoStatus> = std::env::current_dir()
            .ok()
            .and_then(|cwd| crate::git_status::collect_for_prompt(&cwd));
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
        let git_status_s = if compact.is_empty() {
            String::new()
        } else {
            self.theme.git_status_detail.paint(&compact).to_string()
        };
        let git_prompt_s = if git_branch.is_empty() {
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

        template
            .replace("{time}", &time_str)
            .replace("{time_24}", &time_str_24)
            .replace("{user}", &user_s)
            .replace("{host}", &host_s)
            .replace("{cwd}", &dir_s)
            .replace("{symbol}", &sym_s)
            .replace("{git_prompt}", &git_prompt_s)
            .replace("{git_branch}", &git_branch_s)
            .replace("{git_status}", &git_status_s)
            .replace("{git_dirty}", if git_dirty { "✚" } else { "" })
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
            .replace("%~", &cwd)
    }

    fn render_indicator_template(&self, template: &str, mode: &str) -> String {
        self.render_template(template).replace("{mode}", mode)
    }

    fn render_history_search_template(&self, template: &str, status: &str, term: &str) -> String {
        self.render_template(template)
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
        Cow::Owned(self.render_template(&self.template))
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        match &self.right_template {
            Some(template) => Cow::Owned(self.render_template(template)),
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
        Cow::Owned(self.render_template(&self.indicators.multiline))
    }

    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        let (template, status) = match search.status {
            PromptHistorySearchStatus::Passing => (&self.indicators.history_search, "passing"),
            PromptHistorySearchStatus::Failing => (&self.indicators.history_search_fail, "failing"),
        };
        Cow::Owned(self.render_history_search_template(template, status, &search.term))
    }
}

/// Backend selector for the prompt: legacy template engine or new segment engine.
pub enum PromptBackend {
    Template(WinuxshPrompt),
    Segments(SegmentPromptAdapter),
}

impl Prompt for PromptBackend {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        match self {
            PromptBackend::Template(p) => p.render_prompt_left(),
            PromptBackend::Segments(p) => p.render_prompt_left(),
        }
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        match self {
            PromptBackend::Template(p) => p.render_prompt_right(),
            PromptBackend::Segments(p) => p.render_prompt_right(),
        }
    }

    fn render_prompt_indicator(&self, mode: PromptEditMode) -> Cow<'_, str> {
        match self {
            PromptBackend::Template(p) => p.render_prompt_indicator(mode),
            PromptBackend::Segments(p) => p.render_prompt_indicator(mode),
        }
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        match self {
            PromptBackend::Template(p) => p.render_prompt_multiline_indicator(),
            PromptBackend::Segments(p) => p.render_prompt_multiline_indicator(),
        }
    }

    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        match self {
            PromptBackend::Template(p) => p.render_prompt_history_search_indicator(search),
            PromptBackend::Segments(p) => p.render_prompt_history_search_indicator(search),
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
