//! Shell state and execution entry point
//!
//! Wraps a `rubash::Executor` and provides the interactive shell machinery
//! (prompt, history, completion). All shell language semantics are delegated
//! to rubash; this layer only adds the Windows-facing UX.


use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reedline::{Completer, Reedline};
use rubash::{executor::Executor, lexer::tokenize, parser::parse, Ast, Token, TokenKind};

use crate::completion::runtime::RuntimeCompletionPlugin;
use crate::completion::{CompletionState, WinuxshCompleter};
use crate::config::{
    load as load_config, AutosuggestConfig, EditorMode, HookConfig, MenuConfig, NativePluginConfig,
    NativeWidgetConfig, SyntaxHighlightConfig,
};
use crate::prompt::{PromptBackend, WinuxshPrompt};
use crate::prompt_segments::{SegmentPrompt, SegmentPromptAdapter, SegmentPromptConfig, SegmentId, SegmentPreset};
use crate::zsh_compat::{
    apply_alias, apply_safe_env, completion_defs_from_report,
    dynamic_completion_defs_from_report_with_options, git_prompt_format_from_report, scan,
    runtime_completion_commands_from_report, DynamicCompletionRunOptions, NativeWidgetSuggestion,
    ZshImportOptions,
};

use crate::winuxcmd;

const DOTENV_MAX_SIZE: u64 = 10 * 1024 * 1024;

/// Top-level shell state.
pub struct Shell {
    pub executor: Executor,
    pub completion_state: Arc<Mutex<CompletionState>>,
    pub prompt: PromptBackend,
    pub home_dir: PathBuf,
    pub history_path: PathBuf,
    pub history_max_size: usize,
    pub history_ignore_space_prefixed: bool,
    pub menu_config: MenuConfig,
    pub editor_mode: EditorMode,
    pub autosuggest: AutosuggestConfig,
    pub syntax_highlighting: SyntaxHighlightConfig,
    pub native_widgets: NativeWidgetConfig,
    pub native_widget_bindings: Vec<NativeWidgetSuggestion>,
    pub native_plugins: NativePluginConfig,
    pub hooks: HookConfig,
    pub aliases: HashMap<String, String>,
    pub zoxide_last_tracked_dir: Option<String>,
    pub last_working_dir_cache_path: PathBuf,
    pub last_working_dir_restored: bool,
    pub last_interactive_command: Option<String>,
    pub last_interactive_exit_code: Option<i32>,
    pub line_editor: Option<Reedline>,
}

impl Shell {
    /// Construct a fresh shell: load config, install Ctrl+C handler, inject
    /// winuxcmd onto PATH, set up completion state and history.
    pub fn new() -> anyhow::Result<Self> {
        // 1. Load config from ~/.winshrc.toml.
        let config = load_config();

        // 2. Apply opt-in, known-safe zsh profile env/PATH records before
        // winuxcmd injection and before rubash snapshots the process env.
        let zsh_report = if config.zsh.enabled && config.zsh.auto_apply {
            let report = scan(&ZshImportOptions::from_config(&config.zsh));
            let summary = apply_safe_env(&report);
            log::debug!(
                "zsh safe env import: env={} path_entries={}",
                summary.env_applied,
                summary.path_entries_applied
            );
            Some(report)
        } else {
            None
        };

        // 3. WinuxCmd PATH injection (best-effort), honoring config override.
        if config.winuxcmd_enabled {
            if let Err(e) = winuxcmd::ensure_on_path_with_override(config.winuxcmd_path.as_deref()) {
                log::debug!("winuxcmd not on PATH: {}", e);
            }
        } else {
            log::debug!("winuxcmd PATH injection disabled by config");
        }

        // 4. Build rubash Executor after PATH injection.
        let mut executor = Executor::new();

        // 5. Apply imported aliases first, then native config aliases so
        // ~/.winshrc.toml remains authoritative when names collide.
        let mut aliases = HashMap::new();
        if let Some(report) = &zsh_report {
            let mut aliases_applied = 0usize;
            for alias in &report.aliases {
                if apply_alias(&mut executor, &alias.name, &alias.value) {
                    aliases.insert(alias.name.clone(), alias.value.clone());
                    aliases_applied += 1;
                }
            }
            log::debug!("zsh safe alias import: aliases={}", aliases_applied);
        }
        for (name, value) in &config.aliases {
            if apply_alias(&mut executor, name, value) {
                aliases.insert(name.clone(), value.clone());
            } else {
                log::warn!("Skipping invalid alias from config: {}", name);
            }
        }

        // Built-in git aliases (oh-my-zsh style). Always available but
        // user ~/.winshrc.toml aliases take precedence.
        for (name, value) in Self::default_git_aliases() {
            if !aliases.contains_key(name) {
                if apply_alias(&mut executor, name, value) {
                    aliases.insert(name.to_string(), value.to_string());
                }
            }
        }

        // 6. Prompt + theme. Choose backend based on `prompt_style`:
        //    "segments"  -> new p10k-style segment engine
        //    "template"  -> legacy template engine (default, backward-compatible)
        let prompt_style = config.shell.prompt_style.as_deref().unwrap_or("template");
        let git_prompt_symbols = crate::git_status::GitPromptSymbols::from(&config.git_prompt);
        let prompt: PromptBackend = if prompt_style == "segments" {
            let preset = config
                .shell
                .segment_preset
                .as_deref()
                .and_then(SegmentPreset::from_name)
                .unwrap_or(SegmentPreset::Classic);
            let mut seg_config = SegmentPromptConfig::from_preset(
                preset,
                &config.shell.prompt_symbol,
                git_prompt_symbols,
            );
            seg_config.theme_name = config.theme_name.clone();
            if let Some(ref left) = config.shell.left_prompt_elements {
                seg_config.left_elements = left
                    .iter()
                    .filter_map(|s| SegmentId::from_name(s))
                    .collect();
            }
            if let Some(ref right) = config.shell.right_prompt_elements {
                seg_config.right_elements = right
                    .iter()
                    .filter_map(|s| SegmentId::from_name(s))
                    .collect();
            }
            PromptBackend::Segments(SegmentPromptAdapter::new(SegmentPrompt::new(seg_config)))
        } else {
            // Native TOML stays authoritative; zsh prompt imports only fill
            // empty native prompt fields.
            let prompt_format = config.shell.prompt_format.clone().or_else(|| {
                zsh_report.as_ref().and_then(|report| {
                    report
                        .prompt
                        .as_ref()
                        .and_then(|prompt| prompt.translated_format.clone())
                })
            });
            let right_prompt_format = config.shell.right_prompt_format.clone().or_else(|| {
                zsh_report.as_ref().and_then(|report| {
                    report
                        .right_prompt
                        .as_ref()
                        .and_then(|prompt| prompt.translated_format.clone())
                })
            });
            let git_prompt_format = config
                .shell
                .git_prompt_format
                .clone()
                .or_else(|| zsh_report.as_ref().and_then(git_prompt_format_from_report));
            let template_prompt = WinuxshPrompt::new_with_symbol(
                prompt_format,
                right_prompt_format,
                git_prompt_format,
                config.shell.prompt_indicators.clone(),
                &config.theme_name,
                git_prompt_symbols,
                config.shell.prompt_symbol.clone(),
            );
            PromptBackend::Template(template_prompt)
        };

        // 7. User-local state files.
        let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let history_path = config
            .history
            .path
            .clone()
            .unwrap_or_else(|| home_dir.join(".winuxsh_history"));
        let last_working_dir_cache_path = default_last_working_dir_cache_path(&home_dir);

        // 8. Completion state.
        let mut initial_completion_state = CompletionState::new(
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        );
        initial_completion_state.behavior = config.completion_behavior;
        let completion_state = Arc::new(Mutex::new(initial_completion_state));
        let mut zsh_completion_defs = zsh_report
            .as_ref()
            .map(completion_defs_from_report)
            .unwrap_or_default();
        if let (Some(report), Some(options)) = (
            zsh_report.as_ref(),
            DynamicCompletionRunOptions::from_zsh_config(&config.zsh),
        ) {
            zsh_completion_defs.extend(dynamic_completion_defs_from_report_with_options(
                report,
                &options,
            ));
        }

        // 9. Load completion dirs from config (inline, not in thread).
        {
            let mut s = completion_state.lock().unwrap();
            s.load_completion_dirs_with_definitions(&config.completion_dirs, zsh_completion_defs);
            if config.zsh.runtime_completions.enabled {
                if let Some(report) = zsh_report.as_ref() {
                    let runtime_commands = runtime_completion_commands_from_report(
                        report,
                        &config.zsh.runtime_completions.commands,
                    );
                    if !runtime_commands.is_empty() {
                        s.add_plugin(Arc::new(RuntimeCompletionPlugin::new(
                            runtime_commands,
                            Duration::from_millis(
                                config.zsh.runtime_completions.timeout_millis.max(1),
                            ),
                        )));
                    }
                }
            }
        }

        let mut syntax_highlighting = config.zsh.syntax_highlighting.clone();
        if let Some(report) = &zsh_report {
            for style in &report.highlight_styles {
                syntax_highlighting
                    .styles
                    .entry(style.key.clone())
                    .or_insert_with(|| style.value.clone());
            }
        }
        let native_widget_bindings = if config.zsh.native_widgets.enabled
            && config.zsh.native_widgets.import_bindkeys
        {
            zsh_report
                .as_ref()
                .map(|report| report.native_widgets.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let mut shell = Self {
            executor,
            completion_state,
            prompt,
            home_dir,
            history_path,
            history_max_size: config.history.max_size,
            history_ignore_space_prefixed: config.history.ignore_space_prefixed,
            menu_config: config.menus,
            editor_mode: config.editor.edit_mode,
            autosuggest: config.zsh.autosuggestions.with_env_overrides(),
            syntax_highlighting: syntax_highlighting.with_env_overrides(),
            native_widgets: config.zsh.native_widgets,
            native_widget_bindings,
            native_plugins: config.zsh.native_plugins,
            hooks: config.hooks,
            aliases,
            zoxide_last_tracked_dir: None,
            last_working_dir_cache_path,
            last_working_dir_restored: false,
            last_interactive_command: None,
            last_interactive_exit_code: None,
            line_editor: None,
        };
        shell.sync_executor_pwd_from_process_cwd();
        shell.update_completion_state();
        Ok(shell)
    }

    /// Execute a single input line via rubash. Returns the exit code.
    pub fn execute_line(&mut self, line: &str) -> anyhow::Result<i32> {
        self.execute_line_with_options(line, false)
    }

    fn execute_line_with_options(
        &mut self,
        line: &str,
        interactive_terminal_colors: bool,
    ) -> anyhow::Result<i32> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(0);
        }

        let line = normalize_native_windows_path_literals(line);
        let mut tokens = tokenize(&line);
        if tokens.is_empty() {
            return Ok(0);
        }
        if interactive_terminal_colors {
            enable_interactive_terminal_grep_colors(&mut tokens);
        }

        // If the user just ran a `git ...` command, invalidate the prompt's
        // cached git status so the next prompt re-reads the live state.
        if let Some(first) = tokens.first() {
            if first.value.trim().eq_ignore_ascii_case("git") {
                crate::git_status::clear_cache();
            }
        }

        // parse() returns Ast directly (not Result) in rubash.
        let mut ast = parse(&tokens);
        normalize_cd_windows_drive_args(&mut ast);
        normalize_winuxcmd_slash_drive_args(&mut ast);

        let code = if self.native_plugin_enabled("zoxide")
            && ast.commands.len() == 1
            && ast.commands[0]
                .words
                .first()
                .is_some_and(|command| command == "z")
        {
            self.execute_native_zoxide(&ast.commands[0].words[1..])?
        } else if self.native_plugin_enabled("thefuck")
            && ast.commands.len() == 1
            && ast.commands[0]
                .words
                .first()
                .is_some_and(|command| command == "fuck")
        {
            self.execute_native_thefuck(&ast.commands[0].words[1..])?
        } else if self.native_selector_enabled()
            && ast.commands.len() == 1
            && ast.commands[0]
                .words
                .first()
                .is_some_and(|command| command == "cdf" || command == "fzf-cd")
        {
            self.execute_native_fzf_cd(&ast.commands[0].words[1..])?
        } else if self.native_plugin_enabled("last-working-dir")
            && ast.commands.len() == 1
            && ast.commands[0]
                .words
                .first()
                .is_some_and(|command| command == "lwd")
        {
            self.execute_native_last_working_dir()?
        } else if let Some(execution) = self.execute_host_synced_simple_ast(&ast) {
            match execution {
                Ok(code) => code,
                Err(rubash::executor::ExecuteError::ExitCode(code)) => code,
                Err(rubash::executor::ExecuteError::Return(code)) => code,
                Err(rubash::executor::ExecuteError::CommandNotFound(cmd)) => {
                    if self.native_plugin_enabled("command-not-found") {
                        self.print_native_command_not_found(&cmd);
                    } else {
                        eprintln!("winuxsh: {}: command not found", cmd);
                    }
                    127
                }
                Err(e) => {
                    if !is_broken_pipe_execute_error(&e) {
                        eprintln!("winuxsh: {}", e);
                    }
                    1
                }
            }
        } else {
            match self.executor.execute_ast(&ast) {
                Ok(()) => self.executor.last_exit_code(),
                Err(rubash::executor::ExecuteError::ExitCode(code)) => code,
                Err(rubash::executor::ExecuteError::Return(code)) => code,
                Err(rubash::executor::ExecuteError::CommandNotFound(cmd)) => {
                    if self.native_plugin_enabled("command-not-found") {
                        self.print_native_command_not_found(&cmd);
                    } else {
                        eprintln!("winuxsh: {}: command not found", cmd);
                    }
                    127
                }
                Err(e) => {
                    if !is_broken_pipe_execute_error(&e) {
                        eprintln!("winuxsh: {}", e);
                    }
                    1
                }
            }
        };

        self.sync_process_cwd_from_executor_pwd();
        Ok(code)
    }

    /// Execute a line as an interactive REPL command, including native hook
    /// points that mirror common zsh lifecycle concepts.
    pub fn execute_interactive_line(&mut self, line: &str) -> anyhow::Result<i32> {
        let old_pwd = self.executor.get_env("PWD").map(str::to_owned);
        self.run_preexec_hooks(line);
        let code = self.execute_line_with_options(line, true)?;
        self.sync_alias_mirror_from_line(line, code);
        self.remember_interactive_command(line, code);
        let new_pwd = self.executor.get_env("PWD").map(str::to_owned);
        if let (Some(old_pwd), Some(new_pwd)) = (old_pwd, new_pwd) {
            self.run_chpwd_hooks_if_changed(&old_pwd, &new_pwd);
        }
        self.update_completion_state();
        Ok(code)
    }

    /// Execute a complete multi-line interactive input block via rubash script
    /// execution while preserving REPL lifecycle hooks.
    pub fn execute_interactive_script(&mut self, script: &str) -> anyhow::Result<i32> {
        let old_pwd = self.executor.get_env("PWD").map(str::to_owned);
        self.run_preexec_hooks(script);
        let code = self.execute_script_with_options(script, true)?;
        self.sync_alias_mirror_from_line(script, code);
        self.remember_interactive_command(script, code);
        let new_pwd = self.executor.get_env("PWD").map(str::to_owned);
        if let (Some(old_pwd), Some(new_pwd)) = (old_pwd, new_pwd) {
            self.run_chpwd_hooks_if_changed(&old_pwd, &new_pwd);
        }
        self.update_completion_state();
        Ok(code)
    }

    /// Restore the last working directory once for interactive REPL startup.
    ///
    /// This mirrors Oh My Zsh's last-working-dir guard: only jump when the
    /// shell starts in the normal home directory, so terminals opened directly
    /// inside a project are left alone.
    pub fn restore_last_working_dir_for_repl(&mut self) {
        if self.last_working_dir_restored || !self.native_plugin_enabled("last-working-dir") {
            return;
        }
        self.last_working_dir_restored = true;

        let Some(old_pwd) = self.executor.get_env("PWD").map(str::to_owned) else {
            return;
        };
        let home_pwd = host_path_to_shell_path(&self.home_dir.to_string_lossy());
        if !same_shell_dir(&old_pwd, &home_pwd) {
            return;
        }

        if self.execute_native_last_working_dir().ok() != Some(0) {
            return;
        }

        let Some(new_pwd) = self.executor.get_env("PWD").map(str::to_owned) else {
            return;
        };
        self.run_chpwd_hooks_if_changed(&old_pwd, &new_pwd);
        self.update_completion_state();
    }

    /// Run native hooks once after REPL startup, before the first prompt.
    pub fn run_startup_hooks(&mut self) {
        let hooks = self.hooks.startup.clone();
        self.run_hook_scripts(&hooks, &[("WINUXSH_REPL_STARTUP", "1".to_string())]);
        self.update_completion_state();
    }

    /// Run native hooks before rendering the next prompt.
    pub fn run_precmd_hooks(&mut self) {
        self.run_native_precmd_plugins();
        let hooks = self.hooks.precmd.clone();
        let last_exit_code = self.executor.last_exit_code().to_string();
        // Set in process env so segment prompt can read it via std::env::var.
        std::env::set_var("WINUXSH_LAST_EXIT_CODE", &last_exit_code);
        self.run_hook_scripts(&hooks, &[("WINUXSH_LAST_EXIT_CODE", last_exit_code)]);
    }

    /// Run native hooks immediately before the user's interactive command.
    pub fn run_preexec_hooks(&mut self, command: &str) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        self.run_native_preexec_plugins(command);
        let hooks = self.hooks.preexec.clone();
        self.run_hook_scripts(&hooks, &[("WINUXSH_PREEXEC_COMMAND", command.to_string())]);
    }

    /// Run native hooks when the interactive command changed directories.
    pub fn run_chpwd_hooks_if_changed(&mut self, old_pwd: &str, new_pwd: &str) {
        if same_shell_dir(old_pwd, new_pwd) {
            return;
        }
        self.run_native_chpwd_plugins();
        let hooks = self.hooks.chpwd.clone();
        self.run_hook_scripts(
            &hooks,
            &[
                ("WINUXSH_OLDPWD", old_pwd.to_string()),
                ("WINUXSH_PWD", new_pwd.to_string()),
            ],
        );
    }

    fn run_native_precmd_plugins(&mut self) {
        if self.native_plugin_enabled("direnv") {
            self.apply_direnv_export();
        }
        if self.native_plugin_enabled("dotenv") {
            self.apply_dotenv_current_dir();
        }
        if self.native_plugin_enabled("zoxide") {
            self.track_zoxide_current_dir();
        }
    }

    fn run_native_preexec_plugins(&mut self, command: &str) {
        if self.native_plugin_enabled("alias-finder") {
            for suggestion in self.native_alias_finder_matches(command) {
                println!("{}", suggestion);
            }
        }
    }

    fn run_native_chpwd_plugins(&mut self) {
        if self.native_plugin_enabled("direnv") {
            self.apply_direnv_export();
        }
        if self.native_plugin_enabled("dotenv") {
            self.apply_dotenv_current_dir();
        }
        if self.native_plugin_enabled("zoxide") {
            self.track_zoxide_current_dir();
        }
        if self.native_plugin_enabled("last-working-dir") {
            self.save_last_working_dir_current_dir();
        }
    }

    fn native_plugin_enabled(&self, preset: &str) -> bool {
        self.native_plugins.enabled
            && self
                .native_plugins
                .presets
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(preset))
    }

    fn native_selector_enabled(&self) -> bool {
        self.native_plugin_enabled("fzf") || self.native_plugin_enabled("zsh-interactive-cd")
    }

    fn apply_direnv_export(&mut self) {
        let command_path =
            resolve_native_command_path("direnv").unwrap_or_else(|| PathBuf::from("direnv"));
        let output = match Command::new(command_path)
            .args(["export", "bash"])
            .stderr(Stdio::null())
            .output()
        {
            Ok(output) => output,
            Err(err) => {
                log::debug!("native direnv preset skipped: {}", err);
                return;
            }
        };

        if !output.status.success() {
            log::debug!("native direnv preset returned {}", output.status);
            return;
        }

        let script = String::from_utf8_lossy(&output.stdout);
        self.apply_direnv_export_script(&script);
    }

    fn apply_direnv_export_script(&mut self, script: &str) {
        if script.trim().is_empty() {
            return;
        }
        if let Err(err) = self.execute_script(script) {
            log::warn!("native direnv preset failed to apply export: {}", err);
        }
    }

    fn apply_dotenv_current_dir(&mut self) {
        let Some(pwd) = self.executor.get_env("PWD").map(str::to_owned) else {
            return;
        };
        let dotenv_path = PathBuf::from(shell_path_to_host_path(&pwd)).join(".env");
        let Ok(metadata) = std::fs::metadata(&dotenv_path) else {
            return;
        };
        if !metadata.is_file() {
            return;
        }
        if metadata.len() > DOTENV_MAX_SIZE {
            log::debug!(
                "native dotenv preset skipped oversized file {}",
                dotenv_path.display()
            );
            return;
        }
        let Ok(content) = std::fs::read_to_string(&dotenv_path) else {
            log::debug!(
                "native dotenv preset could not read {}",
                dotenv_path.display()
            );
            return;
        };

        for (key, value) in parse_dotenv_assignments(&content) {
            self.executor.set_env(&key, &value);
        }
    }

    fn track_zoxide_current_dir(&mut self) {
        let Some(pwd) = self.executor.get_env("PWD").map(str::to_owned) else {
            return;
        };
        if self
            .zoxide_last_tracked_dir
            .as_deref()
            .is_some_and(|last| same_shell_dir(last, &pwd))
        {
            return;
        }

        let host_pwd = shell_path_to_host_path(&pwd);
        let command_path =
            resolve_native_command_path("zoxide").unwrap_or_else(|| PathBuf::from("zoxide"));
        let status = Command::new(command_path)
            .arg("add")
            .arg(&host_pwd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        match status {
            Ok(status) if status.success() => {
                self.zoxide_last_tracked_dir = Some(pwd);
            }
            Ok(status) => {
                log::debug!("native zoxide preset returned {}", status);
            }
            Err(err) => {
                log::debug!("native zoxide preset skipped: {}", err);
            }
        }
    }

    fn execute_native_zoxide(&mut self, args: &[String]) -> anyhow::Result<i32> {
        let command_path =
            resolve_native_command_path("zoxide").unwrap_or_else(|| PathBuf::from("zoxide"));
        let output = match Command::new(command_path)
            .arg("query")
            .args(args)
            .stderr(Stdio::null())
            .output()
        {
            Ok(output) => output,
            Err(err) => {
                log::debug!("native zoxide query skipped: {}", err);
                return Ok(127);
            }
        };

        if !output.status.success() {
            return Ok(output.status.code().unwrap_or(1));
        }

        let target = String::from_utf8_lossy(&output.stdout);
        let target = target.trim_matches(['\r', '\n']);
        if target.is_empty() {
            return Ok(1);
        }

        let target = host_path_to_shell_path(target);
        self.execute_line(&format!("cd {}", shell_quote(&target)))
    }

    fn execute_native_thefuck(&mut self, args: &[String]) -> anyhow::Result<i32> {
        let correction_args = if args.is_empty() {
            let Some(command) = self.last_interactive_command.as_ref() else {
                return Ok(1);
            };
            vec![command.clone()]
        } else {
            args.to_vec()
        };

        let command_path =
            resolve_native_command_path("thefuck").unwrap_or_else(|| PathBuf::from("thefuck"));
        let output = match Command::new(command_path)
            .args(&correction_args)
            .env("THEFUCK_REQUIRE_CONFIRMATION", "0")
            .stderr(Stdio::null())
            .output()
        {
            Ok(output) => output,
            Err(err) => {
                log::debug!("native thefuck preset skipped: {}", err);
                return Ok(127);
            }
        };

        if !output.status.success() {
            return Ok(output.status.code().unwrap_or(1));
        }

        let correction = String::from_utf8_lossy(&output.stdout);
        let Some(correction) = correction.lines().map(str::trim).find(|line| !line.is_empty())
        else {
            return Ok(1);
        };

        self.execute_line(correction)
    }

    fn execute_native_fzf_cd(&mut self, args: &[String]) -> anyhow::Result<i32> {
        let Some(pwd) = self.executor.get_env("PWD").map(str::to_owned) else {
            return Ok(1);
        };
        let base = args.first().map(String::as_str).unwrap_or(".");
        let host_base = resolve_shell_path_argument(&pwd, base);
        let candidates = directory_selector_candidates(&host_base);
        if candidates.is_empty() {
            return Ok(1);
        }

        let Some(selected) = run_native_fzf_selector(&candidates) else {
            return Ok(1);
        };
        let selected = host_path_to_shell_path(&selected);
        self.execute_line(&format!("cd {}", shell_quote(&selected)))
    }

    fn execute_native_last_working_dir(&mut self) -> anyhow::Result<i32> {
        let Some(target) = self.read_last_working_dir_target() else {
            return Ok(1);
        };
        self.execute_line(&format!("cd {}", shell_quote(&target)))
    }

    fn read_last_working_dir_target(&self) -> Option<String> {
        let content = std::fs::read_to_string(&self.last_working_dir_cache_path).ok()?;
        let target = content.trim_matches(['\r', '\n']).trim();
        if target.is_empty() {
            return None;
        }
        Some(host_path_to_shell_path(target))
    }

    fn save_last_working_dir_current_dir(&self) {
        let Some(pwd) = self.executor.get_env("PWD") else {
            return;
        };
        let Some(parent) = self.last_working_dir_cache_path.parent() else {
            return;
        };
        if let Err(err) = std::fs::create_dir_all(parent) {
            log::debug!("native last-working-dir preset could not create cache dir: {}", err);
            return;
        }
        if let Err(err) = std::fs::write(&self.last_working_dir_cache_path, format!("{pwd}\n")) {
            log::debug!("native last-working-dir preset could not write cache: {}", err);
        }
    }

    fn print_native_command_not_found(&self, command: &str) {
        for line in native_command_not_found_lines(command, |candidate| {
            resolve_native_command_path(candidate).is_some()
        }) {
            eprintln!("{}", line);
        }
    }

    fn native_alias_finder_matches(&self, command: &str) -> Vec<String> {
        let command = normalize_alias_finder_command(command);
        if command.is_empty() {
            return Vec::new();
        }

        let mut matches: Vec<_> = self
            .aliases
            .iter()
            .filter_map(|(name, value)| {
                if normalize_alias_finder_command(value) == command && name != &command {
                    Some(format!("winuxsh: alias available: {}={}", name, shell_quote(value)))
                } else {
                    None
                }
            })
            .collect();
        matches.sort();
        matches
    }

    fn sync_alias_mirror_from_line(&mut self, line: &str, code: i32) {
        if code != 0 {
            return;
        }

        let line = normalize_native_windows_path_literals(line);
        let tokens = tokenize(&line);
        if tokens.is_empty() {
            return;
        }

        let mut ast = parse(&tokens);
        normalize_cd_windows_drive_args(&mut ast);
        normalize_winuxcmd_slash_drive_args(&mut ast);
        if ast.commands.len() != 1 {
            return;
        }

        let mut words = ast.commands[0].words.as_slice();
        if words.first().is_some_and(|word| word == "builtin") {
            words = &words[1..];
        }

        match words.first().map(String::as_str) {
            Some("alias") => self.sync_alias_assignments(&words[1..]),
            Some("unalias") => self.sync_unalias_arguments(&words[1..]),
            _ => {}
        }
    }

    fn sync_alias_assignments(&mut self, args: &[String]) {
        for arg in args {
            if arg == "-p" || arg == "--" {
                continue;
            }
            let Some((name, value)) = arg.split_once('=') else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            self.aliases
                .insert(name.to_string(), strip_rubash_alias_quote_marker(value).to_string());
        }
    }

    fn sync_unalias_arguments(&mut self, args: &[String]) {
        let mut allow_options = true;
        for arg in args {
            if allow_options && arg == "--" {
                allow_options = false;
                continue;
            }
            if allow_options && arg == "-a" {
                self.aliases.clear();
                continue;
            }
            if allow_options && arg.starts_with('-') {
                continue;
            }
            self.aliases.remove(arg);
        }
    }

    fn remember_interactive_command(&mut self, line: &str, code: i32) {
        let line = line.trim();
        if line.is_empty() || first_command_word(line).is_some_and(|word| word == "fuck") {
            return;
        }
        self.last_interactive_command = Some(line.to_string());
        self.last_interactive_exit_code = Some(code);
    }

    fn run_hook_scripts(&mut self, hooks: &[String], context: &[(&str, String)]) {
        if hooks.is_empty() {
            return;
        }

        for (name, value) in context {
            self.executor.set_env(name, value);
        }

        for hook in hooks {
            match self.execute_script(hook) {
                Ok(0) => {}
                Ok(code) => log::warn!("native hook exited with status {}", code),
                Err(err) => log::warn!("native hook failed: {}", err),
            }
        }

        if !context.is_empty() {
            let unset = format!(
                "unset {}",
                context
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let _ = self.execute_script(&unset);
        }
    }

    /// Update the shared completion state from the current env + cwd.
    pub fn update_completion_state(&self) {
        if let Ok(mut state) = self.completion_state.lock() {
            state.current_dir = self
                .executor_pwd_host_path()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| state.current_dir.clone());
            state.env_vars = std::env::vars()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
        }
    }

    /// Return completion candidates using the same completer state as the REPL.
    ///
    /// This is primarily a deterministic probe surface for binary tests and
    /// agent diagnostics; it avoids trying to drive reedline through a TTY.
    pub fn completion_probe(&self, input: &str, cursor_pos: usize) -> Vec<String> {
        self.update_completion_state();
        let mut completer = WinuxshCompleter::new(self.completion_state.clone());
        let cursor_pos = cursor_pos.min(input.len());
        completer
            .complete(input, cursor_pos)
            .into_iter()
            .map(|suggestion| suggestion.value)
            .collect()
    }

    fn executor_pwd_host_path(&self) -> Option<PathBuf> {
        let pwd = self.executor.get_env("PWD")?;
        let host_path = PathBuf::from(shell_path_to_host_path(pwd));
        host_path.is_dir().then_some(host_path)
    }

    fn sync_executor_pwd_from_process_cwd(&mut self) {
        let Ok(cwd) = std::env::current_dir() else {
            return;
        };
        let normalized_pwd = host_path_to_shell_path(&cwd.to_string_lossy());
        self.executor.set_env("PWD", &normalized_pwd);
    }
    fn sync_process_cwd_from_executor_pwd(&mut self) {
        let pwd = match self.executor.get_env("PWD") {
            Some(p) => p.to_string(),
            None => {
                let cwd = std::env::current_dir().unwrap_or_default();
                let pwd = host_path_to_shell_path(&cwd.to_string_lossy());
                self.executor.set_env("PWD", &pwd);
                return;
            }
        };
        let host_pwd = shell_path_to_host_path(&pwd);
        let target = if cfg!(windows) {
            PathBuf::from(host_pwd.replace("/", "\\"))
        } else {
            PathBuf::from(&host_pwd)
        };
        if !target.is_dir() {
            let cwd = std::env::current_dir().unwrap_or_default();
            let pwd = host_path_to_shell_path(&cwd.to_string_lossy());
            self.executor.set_env("PWD", &pwd);
            return;
        }
        if std::env::set_current_dir(&target).is_err() {
            let cwd = std::env::current_dir().unwrap_or_default();
            let pwd = host_path_to_shell_path(&cwd.to_string_lossy());
            self.executor.set_env("PWD", &pwd);
            return;
        }
        let normalized_pwd = host_path_to_shell_path(&host_pwd);
        self.executor.set_env("PWD", &normalized_pwd);
        if let Some(old_pwd) = self.executor.get_env("OLDPWD").map(str::to_owned) {
            let normalized_old_pwd = normalize_shell_visible_path(&old_pwd);
            if normalized_old_pwd != old_pwd {
                self.executor.set_env("OLDPWD", &normalized_old_pwd);
            }
        }
    }

    /// Last exit code from rubash executor.
    pub fn last_exit_code(&self) -> i32 {
        self.executor.last_exit_code()
    }

    /// Execute an entire script (multi-line) via rubash full AST execution.
    ///
    /// Unlike `execute_line` which tokenizes/parses/executes each line
    /// independently, this method tokenizes the whole script at once.
    /// This enables heredocs, line continuations (backslash-newline),
    /// and multi-line compound commands (if/for/while across lines).
    pub fn execute_script(&mut self, script: &str) -> anyhow::Result<i32> {
        self.execute_script_with_options(script, false)
    }

    fn execute_script_with_options(
        &mut self,
        script: &str,
        interactive_terminal_colors: bool,
    ) -> anyhow::Result<i32> {
        let script = script.trim();
        if script.is_empty() {
            return Ok(0);
        }

        let script = normalize_native_windows_path_literals(script);
        let mut tokens = tokenize(&script);
        if tokens.is_empty() {
            return Ok(0);
        }
        if interactive_terminal_colors {
            enable_interactive_terminal_grep_colors(&mut tokens);
        }

        let mut ast = parse(&tokens);
        normalize_cd_windows_drive_args(&mut ast);
        normalize_winuxcmd_slash_drive_args(&mut ast);

        let execution = self
            .execute_host_synced_simple_ast(&ast)
            .unwrap_or_else(|| match self.executor.execute_ast(&ast) {
                Ok(()) => Ok(self.executor.last_exit_code()),
                Err(err) => Err(err),
            });

        let code = match execution {
            Ok(code) => code,
            Err(rubash::executor::ExecuteError::ExitCode(code)) => code,
            Err(rubash::executor::ExecuteError::Return(code)) => code,
            Err(rubash::executor::ExecuteError::CommandNotFound(cmd)) => {
                eprintln!("winuxsh: {}: command not found", cmd);
                127
            }
            Err(e) => {
                if !is_broken_pipe_execute_error(&e) {
                    eprintln!("winuxsh: {}", e);
                }
                1
            }
        };

        self.sync_process_cwd_from_executor_pwd();
        Ok(code)
    }
    /// Built-in git aliases (oh-my-zsh style).
    fn default_git_aliases() -> Vec<(&'static str, &'static str)> {
        vec![
            ("g", "git"),
            ("ga", "git add"),
            ("gaa", "git add --all"),
            ("gb", "git branch"),
            ("gba", "git branch -a"),
            ("gbl", "git blame -b -w"),
            ("gc", "git commit -v"),
            ("gca", "git commit -v -a"),
            ("gcam", "git commit -a -m"),
            ("gcb", "git checkout -b"),
            ("gcl", "git clone --recurse-submodules"),
            ("gcm", "git checkout master"),
            ("gcmsg", "git commit -m"),
            ("gco", "git checkout"),
            ("gcp", "git cherry-pick"),
            ("gd", "git diff"),
            ("gdca", "git diff --cached"),
            ("gf", "git fetch"),
            ("gfa", "git fetch --all --prune"),
            ("gl", "git pull"),
            ("glo", "git log --oneline --decorate"),
            ("gm", "git merge"),
            ("gp", "git push"),
            ("gpf", "git push --force-with-lease"),
            ("gr", "git remote"),
            ("gra", "git remote add"),
            ("grb", "git rebase"),
            ("grh", "git reset HEAD"),
            ("grhh", "git reset HEAD --hard"),
            ("grm", "git rm"),
            ("grs", "git reset"),
            ("grv", "git remote -v"),
            ("gsh", "git show"),
            ("gss", "git status -s"),
            ("gst", "git status"),
            ("gsta", "git stash save"),
            ("gstp", "git stash pop"),
            ("gstl", "git stash list"),
            ("gup", "git pull --rebase"),
        ]
    }

    fn execute_host_synced_simple_ast(
        &mut self,
        ast: &Ast,
    ) -> Option<Result<i32, rubash::executor::ExecuteError>> {
        if !is_host_synced_simple_sequence(ast) {
            return None;
        }

        for command in &ast.commands {
            match self.executor.execute_command(command) {
                Ok(()) => {
                    self.sync_process_cwd_from_executor_pwd();
                }
                Err(rubash::executor::ExecuteError::ExitCode(code)) => return Some(Ok(code)),
                Err(rubash::executor::ExecuteError::Return(code)) => return Some(Ok(code)),
                Err(err) => return Some(Err(err)),
            }
        }

        Some(Ok(self.executor.last_exit_code()))
    }
}

fn same_shell_dir(left: &str, right: &str) -> bool {
    let left = normalize_shell_dir_for_compare(left);
    let right = normalize_shell_dir_for_compare(right);
    if cfg!(windows) {
        left.eq_ignore_ascii_case(&right)
    } else {
        left == right
    }
}

fn normalize_cd_windows_drive_args(ast: &mut Ast) {
    if !cfg!(windows) {
        return;
    }

    for command in &mut ast.commands {
        if !command
            .words
            .first()
            .is_some_and(|word| word.eq_ignore_ascii_case("cd"))
        {
            continue;
        }

        for word in command.words.iter_mut().skip(1) {
            if let Some(normalized) =
                cd_tilde_path_to_slash_drive(word).or_else(|| windows_drive_path_to_slash_drive(word))
            {
                *word = normalized;
            }
        }
    }
}

fn cd_tilde_path_to_slash_drive(value: &str) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }

    let rest = if value == "~" {
        ""
    } else {
        value.strip_prefix("~/")?
    };

    let home = std::env::var("HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|value| !value.is_empty())
        })?;
    let home = windows_drive_path_to_slash_drive(&home).unwrap_or_else(|| home.replace('\\', "/"));
    if rest.is_empty() {
        Some(home)
    } else {
        Some(format!("{}/{}", home.trim_end_matches('/'), rest))
    }
}

fn is_host_synced_simple_sequence(ast: &Ast) -> bool {
    if !cfg!(windows) || !ast.commands.iter().any(is_cd_command) {
        return false;
    }

    ast.commands.iter().all(is_host_synced_simple_command)
}

fn is_host_synced_simple_command(command: &rubash::parser::CommandNode) -> bool {
    command.pipe.is_none()
        && !command.background
        && command.and_or.is_none()
        && !command.inverted
        && command.pipeline_command.is_none()
        && command.and_or_list.is_none()
        && command.time_command.is_none()
        && command.background_command.is_none()
        && command.inverted_command.is_none()
        && !command.subshell
        && !command.subshell_end
        && command.for_command.is_none()
        && command.arithmetic_command.is_none()
        && command.if_command.is_none()
        && command.loop_command.is_none()
        && command.conditional_command.is_none()
        && command.subshell_command.is_none()
        && command.case_command.is_none()
        && command.select_command.is_none()
        && command.function_command.is_none()
        && command.brace_group.is_none()
        && command.coproc_command.is_none()
        && !command
            .words
            .first()
            .is_some_and(|word| word == "set" || word == "trap")
}

fn is_cd_command(command: &rubash::parser::CommandNode) -> bool {
    command
        .words
        .first()
        .is_some_and(|word| word.eq_ignore_ascii_case("cd"))
}

fn enable_interactive_terminal_grep_colors(tokens: &mut Vec<Token>) {
    if !cfg!(windows) {
        return;
    }

    let mut command_start = 0;
    while command_start < tokens.len() {
        let command_end = find_command_separator(tokens, command_start).unwrap_or(tokens.len());
        let separator = tokens.get(command_end).map(|token| &token.kind);
        let terminal_output = !matches!(separator, Some(TokenKind::Background));
        if terminal_output {
            enable_terminal_grep_colors_in_command(tokens, command_start, command_end);
        }
        if command_end == tokens.len() {
            break;
        }
        command_start = command_end + 1;
    }
}

fn find_command_separator(tokens: &[Token], start: usize) -> Option<usize> {
    tokens
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(index, token)| {
            matches!(
                token.kind,
                TokenKind::Semicolon
                    | TokenKind::And
                    | TokenKind::Or
                    | TokenKind::Background
                    | TokenKind::Eof
            )
            .then_some(index)
        })
}

fn enable_terminal_grep_colors_in_command(tokens: &mut Vec<Token>, start: usize, end: usize) {
    let mut stage_start = start;
    while stage_start < end {
        let stage_end = find_pipeline_separator(tokens, stage_start, end).unwrap_or(end);
        if stage_end == end && stage_outputs_to_terminal(tokens, stage_start, stage_end) {
            enable_terminal_grep_colors_in_stage(tokens, stage_start, stage_end);
            return;
        }
        stage_start = stage_end + 1;
    }
}

fn find_pipeline_separator(tokens: &[Token], start: usize, end: usize) -> Option<usize> {
    tokens[start..end]
        .iter()
        .position(|token| matches!(token.kind, TokenKind::Pipe | TokenKind::PipeErr))
        .map(|offset| start + offset)
}

fn stage_outputs_to_terminal(tokens: &[Token], start: usize, end: usize) -> bool {
    !tokens[start..end]
        .iter()
        .any(|token| matches!(token.kind, TokenKind::RedirectOut | TokenKind::Append))
}

fn enable_terminal_grep_colors_in_stage(tokens: &mut Vec<Token>, start: usize, end: usize) {
    let Some(command_index) = simple_command_word_index(tokens, start, end) else {
        return;
    };
    let Some(needs_exe_shim) = grep_command_needs_exe_shim(&tokens[command_index]) else {
        return;
    };
    let has_color_option = grep_stage_has_color_option(tokens, command_index + 1, end);

    // rubash has a no-color optimized pipeline implementation for bare `grep`.
    // Use the winuxcmd shim so terminal-facing interactive grep reaches grep.exe.
    if needs_exe_shim {
        tokens[command_index].value = "grep.exe".to_string();
        tokens[command_index].raw = "grep.exe".to_string();
    }
    if !has_color_option {
        tokens.insert(
            command_index + 1,
            Token::new(TokenKind::Word, "--color=always", tokens[command_index].position),
        );
    }
}

fn simple_command_word_index(tokens: &[Token], start: usize, end: usize) -> Option<usize> {
    let mut saw_command_prefix = false;
    for (offset, token) in tokens[start..end].iter().enumerate() {
        match token.kind {
            TokenKind::Assignment => continue,
            TokenKind::Word if token.value == "command" && !saw_command_prefix => {
                saw_command_prefix = true;
            }
            TokenKind::Word if token.value == "builtin" && !saw_command_prefix => return None,
            TokenKind::Word => return Some(start + offset),
            _ => {}
        }
    }
    None
}

fn grep_command_needs_exe_shim(token: &Token) -> Option<bool> {
    if !matches!(token.kind, TokenKind::Word) {
        return None;
    }
    if token.value.eq_ignore_ascii_case("grep") && token.raw.eq_ignore_ascii_case("grep") {
        return Some(true);
    }
    if token.value.eq_ignore_ascii_case("grep.exe") && token.raw.eq_ignore_ascii_case("grep.exe") {
        return Some(false);
    }
    None
}

fn grep_stage_has_color_option(tokens: &[Token], start: usize, end: usize) -> bool {
    for token in &tokens[start..end] {
        if !matches!(
            token.kind,
            TokenKind::Word | TokenKind::Variable | TokenKind::Assignment | TokenKind::CommandSubst
        ) {
            continue;
        }
        let value = token.value.as_str();
        if value == "--" {
            break;
        }
        if value == "--color"
            || value == "--colour"
            || value.starts_with("--color=")
            || value.starts_with("--colour=")
        {
            return true;
        }
    }
    false
}

fn normalize_winuxcmd_slash_drive_args(ast: &mut Ast) {
    if !cfg!(windows) {
        return;
    }

    for command in &mut ast.commands {
        let Some(command_name) = command.words.first() else {
            continue;
        };
        if !is_winuxcmd_path_command(command_name) {
            continue;
        }

        for word in command.words.iter_mut().skip(1) {
            if let Some(normalized) = slash_drive_arg_to_windows_native(word) {
                *word = normalized;
            }
        }
    }
}

fn is_winuxcmd_path_command(command: &str) -> bool {
    let command = command.strip_suffix(".exe").unwrap_or(command);
    matches!(
        command,
        "ls" | "cat"
            | "grep"
            | "find"
            | "cp"
            | "mv"
            | "rm"
            | "mkdir"
            | "touch"
            | "chmod"
            | "tar"
    )
}

fn slash_drive_arg_to_windows_native(value: &str) -> Option<String> {
    if let Some(path) = slash_drive_path_to_windows_native(value) {
        return Some(path);
    }

    let (prefix, path) = value.split_once('=')?;
    slash_drive_path_to_windows_native(path).map(|path| format!("{prefix}={path}"))
}

fn slash_drive_path_to_windows_native(value: &str) -> Option<String> {
    let normalized = value.replace('\\', "/");
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && (bytes.len() == 2 || bytes.get(2) == Some(&b'/'))
    {
        Some(shell_path_to_host_path(&normalized).replace('\\', "/"))
    } else {
        None
    }
}

fn windows_drive_path_to_slash_drive(value: &str) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }

    let normalized = value.replace('\\', "/");
    let bytes = normalized.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }

    let drive = (bytes[0] as char).to_ascii_lowercase();
    if bytes.len() == 2 {
        return Some(format!("/{drive}"));
    }
    if bytes.get(2) == Some(&b'/') {
        return Some(format!("/{drive}{}", &normalized[2..]));
    }

    None
}

fn normalize_shell_dir_for_compare(value: &str) -> String {
    let normalized = normalize_shell_visible_path(value)
        .trim_end_matches(['/', '\\'])
        .replace('/', "\\");
    if normalized.is_empty() {
        value.to_string()
    } else {
        normalized
    }
}

fn normalize_alias_finder_command(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_rubash_alias_quote_marker(value: &str) -> &str {
    value.strip_prefix('\x1c').unwrap_or(value)
}

fn normalize_native_windows_path_literals(input: &str) -> String {
    if !cfg!(windows) {
        return input.to_string();
    }

    let chars: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len());
    let mut changed = false;
    let mut quote: Option<char> = None;
    let mut index = 0;

    while index < chars.len() {
        let ch = chars[index];

        if let Some(quote_char) = quote {
            output.push(ch);
            if ch == quote_char {
                quote = None;
            }
            index += 1;
            continue;
        }

        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            output.push(ch);
            index += 1;
            continue;
        }

        if is_native_windows_path_literal_start(&chars, index) {
            while index < chars.len() && !is_shell_word_boundary(chars[index]) {
                let path_ch = chars[index];
                if path_ch == '\\' {
                    output.push('/');
                    changed = true;
                } else {
                    output.push(path_ch);
                }
                index += 1;
            }
            continue;
        }

        output.push(ch);
        index += 1;
    }

    if changed {
        output
    } else {
        input.to_string()
    }
}

fn is_native_windows_path_literal_start(chars: &[char], index: usize) -> bool {
    index + 2 < chars.len()
        && chars[index].is_ascii_alphabetic()
        && chars[index + 1] == ':'
        && chars[index + 2] == '\\'
        && (index == 0 || is_windows_path_literal_boundary(chars[index - 1]))
}

fn is_windows_path_literal_boundary(ch: char) -> bool {
    ch.is_ascii_whitespace() || matches!(ch, '=' | '(' | '[' | '{' | ',' | ';' | '|' | '&' | '<' | '>')
}

fn is_shell_word_boundary(ch: char) -> bool {
    ch.is_ascii_whitespace() || matches!(ch, ';' | '|' | '&' | '<' | '>' | '(' | ')' | '\'' | '"')
}

fn first_command_word(line: &str) -> Option<String> {
    let line = normalize_native_windows_path_literals(line);
    let tokens = tokenize(&line);
    if tokens.is_empty() {
        return None;
    }
    let ast = parse(&tokens);
    if ast.commands.len() != 1 {
        return None;
    }
    ast.commands[0].words.first().cloned()
}

fn native_command_not_found_lines<F>(command: &str, mut command_exists: F) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    let mut lines = vec![format!("winuxsh: {}: command not found", command)];
    if !is_package_search_candidate(command) {
        return lines;
    }

    let search = shell_quote(command);
    let mut hints = Vec::new();
    if command_exists("winget") {
        hints.push(format!("  winget search --name {}", search));
    }
    if command_exists("scoop") {
        hints.push(format!("  scoop search {}", search));
    }
    if command_exists("choco") {
        hints.push(format!("  choco search {}", search));
    }

    if !hints.is_empty() {
        lines.push("winuxsh: package search hints:".to_string());
        lines.extend(hints);
    }

    lines
}

fn is_package_search_candidate(command: &str) -> bool {
    !command.is_empty()
        && !command.contains('/')
        && !command.contains('\\')
        && !command.contains(':')
}

fn is_broken_pipe_execute_error(error: &rubash::executor::ExecuteError) -> bool {
    match error {
        rubash::executor::ExecuteError::IoError(error) => is_broken_pipe_io_error(error),
        _ => {
            let message = error.to_string();
            message.contains("os error 232") || message.contains("管道正在被关闭")
        }
    }
}

fn is_broken_pipe_io_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::BrokenPipe || error.raw_os_error() == Some(232)
}

fn parse_dotenv_assignments(content: &str) -> Vec<(String, String)> {
    let mut assignments = Vec::new();
    for raw_line in content.lines() {
        let mut line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export") {
            if rest.chars().next().is_some_and(char::is_whitespace) {
                line = rest.trim_start();
            }
        }

        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !is_safe_dotenv_key(key) || is_forbidden_dotenv_key(key) {
            continue;
        }

        let Some(value) = parse_dotenv_value(raw_value.trim()) else {
            continue;
        };
        assignments.push((key.to_string(), value));
    }
    assignments
}

fn parse_dotenv_value(value: &str) -> Option<String> {
    if value.contains("$(") || value.contains('`') {
        return None;
    }
    if value.starts_with('"') || value.starts_with('\'') {
        return parse_quoted_dotenv_value(value);
    }
    let value = strip_unquoted_dotenv_comment(value).trim();
    if value.contains(';') {
        return None;
    }
    Some(value.to_string())
}

fn parse_quoted_dotenv_value(value: &str) -> Option<String> {
    let quote = value.chars().next()?;
    let mut escaped = false;
    let mut out = String::new();
    for ch in value[quote.len_utf8()..].chars() {
        if escaped {
            out.push(match ch {
                'n' if quote == '"' => '\n',
                'r' if quote == '"' => '\r',
                't' if quote == '"' => '\t',
                other => other,
            });
            escaped = false;
            continue;
        }
        if quote == '"' && ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            return Some(out);
        }
        out.push(ch);
    }
    None
}

fn strip_unquoted_dotenv_comment(value: &str) -> &str {
    let bytes = value.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] == b'#' && (index == 0 || bytes[index - 1].is_ascii_whitespace()) {
            return &value[..index];
        }
    }
    value
}

fn is_safe_dotenv_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn is_forbidden_dotenv_key(key: &str) -> bool {
    matches!(
        key.to_ascii_uppercase().as_str(),
        "BASH_ENV"
            | "DYLD_INSERT_LIBRARIES"
            | "EDITOR"
            | "ENV"
            | "GIT_CONFIG_GLOBAL"
            | "GIT_DIR"
            | "GIT_EDITOR"
            | "GIT_EXEC_PATH"
            | "GIT_EXTERNAL_DIFF"
            | "GIT_PAGER"
            | "GIT_SSH"
            | "GIT_SSH_COMMAND"
            | "GIT_SSL_NO_VERIFY"
            | "GIT_TEMPLATE_DIR"
            | "LD_LIBRARY_PATH"
            | "LD_PRELOAD"
            | "NODE_OPTIONS"
            | "PAGER"
            | "PATH"
            | "VISUAL"
            | "ZDOTDIR"
            | "ZSH"
    )
}

fn default_last_working_dir_cache_path(home_dir: &Path) -> PathBuf {
    let mut file_name = "last-working-dir".to_string();
    if let Ok(ssh_user) = std::env::var("SSH_USER") {
        let suffix = sanitize_cache_file_suffix(ssh_user.trim());
        if !suffix.is_empty() {
            file_name.push('.');
            file_name.push_str(&suffix);
        }
    }
    home_dir.join(".winuxsh").join("cache").join(file_name)
}

fn sanitize_cache_file_suffix(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect()
}

fn resolve_shell_path_argument(pwd: &str, arg: &str) -> PathBuf {
    let normalized = shell_path_to_host_path(arg);
    let candidate = PathBuf::from(&normalized);
    if candidate.is_absolute() || is_windows_drive_path(&normalized) {
        return candidate;
    }

    PathBuf::from(shell_path_to_host_path(pwd)).join(candidate)
}

fn directory_selector_candidates(host_base: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(host_base) else {
        return Vec::new();
    };

    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
        let path = entry.path();
        candidates.push(host_path_to_shell_path(&path.to_string_lossy()));
    }
    candidates.sort();
    candidates
}

fn run_native_fzf_selector(candidates: &[String]) -> Option<String> {
    let command_path = resolve_native_command_path("fzf").unwrap_or_else(|| PathBuf::from("fzf"));
    let mut child = Command::new(command_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;

    if let Some(mut stdin) = child.stdin.take() {
        for candidate in candidates {
            if writeln!(stdin, "{}", candidate).is_err() {
                break;
            }
        }
    }

    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }

    let selected = String::from_utf8_lossy(&output.stdout);
    selected
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn resolve_native_command_path(command: &str) -> Option<PathBuf> {
    let command_path = PathBuf::from(command);
    if command_path.is_file() {
        return Some(command_path);
    }

    let path = std::env::var_os("PATH")?;
    let has_extension = PathBuf::from(command)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some();
    let extensions: &[&str] = if has_extension {
        &[""]
    } else if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };

    for dir in std::env::split_paths(&path) {
        for ext in extensions {
            let candidate = dir.join(format!("{}{}", command, ext));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

fn shell_path_to_host_path(value: &str) -> String {
    let normalized = value.replace('\\', "/");
    if cfg!(windows) {
        let bytes = normalized.as_bytes();
        if bytes.len() == 2
            && bytes[0] == b'/'
            && (bytes[1] as char).is_ascii_alphabetic()
        {
            let drive = (bytes[1] as char).to_ascii_uppercase();
            return format!("{}:/", drive);
        }
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && (bytes[1] as char).is_ascii_alphabetic()
            && bytes[2] == b'/'
        {
            let drive = (bytes[1] as char).to_ascii_uppercase();
            return format!("{}:{}", drive, &normalized[2..]);
        }
    }
    value.to_string()
}

fn host_path_to_shell_path(value: &str) -> String {
    if cfg!(windows) {
        return value.replace('\\', "/");
    }
    value.to_string()
}

fn normalize_shell_visible_path(value: &str) -> String {
    if cfg!(windows) {
        shell_path_to_host_path(value).replace('\\', "/")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use crate::test_support::PROCESS_STATE_LOCK;
    use std::time::{SystemTime, UNIX_EPOCH};


    #[test]
    fn native_lifecycle_hooks_run_for_interactive_commands() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-native-hooks");
        let next_dir = temp.join("next");
        std::fs::create_dir_all(&next_dir).unwrap();
        let next_arg = shell_quote(&shell_display_path(&next_dir));

        let mut shell = test_shell(HookConfig {
            startup: vec!["HOOK_STARTUP=\"startup:$WINUXSH_REPL_STARTUP\"".to_string()],
            precmd: vec!["HOOK_PRECMD=\"precmd:$WINUXSH_LAST_EXIT_CODE\"".to_string()],
            preexec: vec!["HOOK_PREEXEC=\"preexec:$WINUXSH_PREEXEC_COMMAND\"".to_string()],
            chpwd: vec!["HOOK_CHPWD=\"chpwd:$WINUXSH_OLDPWD->$WINUXSH_PWD\"".to_string()],
        });

        shell.run_startup_hooks();
        shell.run_precmd_hooks();
        shell
            .execute_interactive_line(&format!("cd {}", next_arg))
            .unwrap();

        assert_eq!(shell.executor.get_env("HOOK_STARTUP"), Some("startup:1"));
        assert_eq!(shell.executor.get_env("HOOK_PRECMD"), Some("precmd:0"));
        let preexec = shell.executor.get_env("HOOK_PREEXEC").unwrap_or_default();
        assert!(preexec.starts_with("preexec:cd "), "{preexec}");
        let chpwd = shell.executor.get_env("HOOK_CHPWD").unwrap_or_default();
        assert!(chpwd.starts_with("chpwd:"), "{chpwd}");
        assert!(chpwd.contains("->"), "{chpwd}");
        assert!(shell.executor.get_env("WINUXSH_LAST_EXIT_CODE").is_none());
        assert!(shell.executor.get_env("WINUXSH_REPL_STARTUP").is_none());
        assert!(shell.executor.get_env("WINUXSH_PREEXEC_COMMAND").is_none());
        assert!(shell.executor.get_env("WINUXSH_OLDPWD").is_none());
        assert!(shell.executor.get_env("WINUXSH_PWD").is_none());

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn native_direnv_export_script_applies_to_executor_env() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        shell.apply_direnv_export_script("export DIRENV_TEST_VALUE=active\n");

        assert_eq!(shell.executor.get_env("DIRENV_TEST_VALUE"), Some("active"));
    }

    #[test]
    fn native_alias_finder_matches_known_alias_values() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.native_plugins.enabled = true;
        shell.native_plugins.presets = vec!["alias-finder".to_string()];
        shell
            .aliases
            .insert("gst".to_string(), "git status".to_string());

        assert_eq!(
            shell.native_alias_finder_matches(" git   status "),
            vec!["winuxsh: alias available: gst='git status'"]
        );
        assert!(shell.native_alias_finder_matches("git diff").is_empty());
    }

    #[test]
    fn native_command_not_found_lines_include_available_windows_package_managers() {
        let lines = native_command_not_found_lines("rg", |command| {
            matches!(command, "winget" | "scoop")
        });

        assert_eq!(lines[0], "winuxsh: rg: command not found");
        assert!(lines.contains(&"winuxsh: package search hints:".to_string()));
        assert!(lines.contains(&"  winget search --name 'rg'".to_string()));
        assert!(lines.contains(&"  scoop search 'rg'".to_string()));
        assert!(!lines.iter().any(|line| line.contains("choco search")));
    }

    #[test]
    fn native_command_not_found_lines_skip_package_hints_for_paths() {
        let lines = native_command_not_found_lines("./missing", |_| true);

        assert_eq!(lines, vec!["winuxsh: ./missing: command not found"]);
    }

    #[test]
    fn alias_mirror_tracks_successful_interactive_alias_commands() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        shell.execute_interactive_line("alias gst='git status'").unwrap();
        assert_eq!(shell.aliases.get("gst").map(String::as_str), Some("git status"));
        assert_eq!(
            shell.native_alias_finder_matches("git status"),
            vec!["winuxsh: alias available: gst='git status'"]
        );

        shell.execute_interactive_line("unalias gst").unwrap();
        assert!(shell.native_alias_finder_matches("git status").is_empty());
    }

    #[test]
    fn execute_interactive_script_runs_multiline_compound_blocks() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        shell.execute_interactive_line("HTTP_CODE=200").unwrap();
        let code = shell
            .execute_interactive_script(
                "if [ $HTTP_CODE -eq 200 ]; then\n  RESULT=OK\nfi",
            )
            .unwrap();

        assert_eq!(code, 0);
        assert_eq!(shell.executor.get_env("RESULT"), Some("OK"));
    }

    #[test]
    fn shell_path_to_host_path_converts_drive_style_paths() {
        if cfg!(windows) {
            assert_eq!(shell_path_to_host_path("/c/Users/me/project"), "C:/Users/me/project");
            assert_eq!(shell_path_to_host_path("/d"), "D:/");
        } else {
            assert_eq!(shell_path_to_host_path("/c/Users/me/project"), "/c/Users/me/project");
        }
    }

    #[test]
    fn host_path_to_shell_path_uses_windows_native_drive_paths() {
        if cfg!(windows) {
            assert_eq!(
                host_path_to_shell_path(r"C:\Users\me\project"),
                "C:/Users/me/project"
            );
            assert_eq!(
                host_path_to_shell_path("C:/Users/me/project"),
                "C:/Users/me/project"
            );
        } else {
            assert_eq!(host_path_to_shell_path("/home/me/project"), "/home/me/project");
        }
    }

    #[test]
    fn native_windows_path_literals_are_normalized_before_tokenize() {
        if cfg!(windows) {
            assert_eq!(
                normalize_native_windows_path_literals(r"ls C:\Users\me"),
                "ls C:/Users/me"
            );
            assert_eq!(
                normalize_native_windows_path_literals(r"ls --root=C:\Users\me"),
                "ls --root=C:/Users/me"
            );
            assert_eq!(
                normalize_native_windows_path_literals(r"echo foo\ bar C:\Users\me"),
                r"echo foo\ bar C:/Users/me"
            );
            assert_eq!(
                normalize_native_windows_path_literals(r"echo 'C:\Users\me'"),
                r"echo 'C:\Users\me'"
            );
            assert_eq!(
                normalize_native_windows_path_literals(r"echo http:\example"),
                r"echo http:\example"
            );
        } else {
            assert_eq!(
                normalize_native_windows_path_literals(r"ls C:\Users\me"),
                r"ls C:\Users\me"
            );
        }
    }

    #[test]
    fn native_windows_path_literals_survive_rubash_tokenize() {
        if !cfg!(windows) {
            return;
        }

        let line = normalize_native_windows_path_literals(r"ls C:\Users\me; echo C:\Users\me");
        let tokens = tokenize(&line);
        let mut ast = parse(&tokens);
        normalize_cd_windows_drive_args(&mut ast);
        normalize_winuxcmd_slash_drive_args(&mut ast);

        assert_eq!(ast.commands[0].words[1], "C:/Users/me");
        assert_eq!(ast.commands[1].words[1], "C:/Users/me");
    }

    #[test]
    fn winuxcmd_slash_drive_args_are_translated_for_path_commands() {
        let tokens = tokenize("ls /c/Users; echo /c/Users");
        let mut ast = parse(&tokens);
        normalize_winuxcmd_slash_drive_args(&mut ast);

        if cfg!(windows) {
            assert_eq!(ast.commands[0].words[1], "C:/Users");
            assert_eq!(ast.commands[1].words[1], "/c/Users");
        } else {
            assert_eq!(ast.commands[0].words[1], "/c/Users");
            assert_eq!(ast.commands[1].words[1], "/c/Users");
        }
    }

    #[test]
    fn interactive_terminal_grep_colors_force_pipeline_final_stage() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("ls -la | grep map");
        enable_interactive_terminal_grep_colors(&mut tokens);
        let ast = parse(&tokens);
        let pipeline = ast.commands[0].pipeline_command.as_ref().unwrap();

        assert_eq!(
            pipeline.stages[1].words,
            vec!["grep.exe", "--color=always", "map"]
        );
    }

    #[test]
    fn interactive_terminal_grep_colors_preserve_explicit_color_choice() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("ls -la | grep --color=never map");
        enable_interactive_terminal_grep_colors(&mut tokens);
        let ast = parse(&tokens);
        let pipeline = ast.commands[0].pipeline_command.as_ref().unwrap();

        assert_eq!(
            pipeline.stages[1].words,
            vec!["grep.exe", "--color=never", "map"]
        );
    }

    #[test]
    fn interactive_terminal_grep_colors_force_grep_exe_pipeline_stage() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("ls -la | grep.exe map");
        enable_interactive_terminal_grep_colors(&mut tokens);
        let ast = parse(&tokens);
        let pipeline = ast.commands[0].pipeline_command.as_ref().unwrap();

        assert_eq!(
            pipeline.stages[1].words,
            vec!["grep.exe", "--color=always", "map"]
        );
    }

    #[test]
    fn interactive_terminal_grep_colors_skip_redirected_stdout() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("ls -la | grep map > out.txt");
        enable_interactive_terminal_grep_colors(&mut tokens);
        let ast = parse(&tokens);
        let pipeline = ast.commands[0].pipeline_command.as_ref().unwrap();

        assert_eq!(pipeline.stages[1].words, vec!["grep", "map"]);
    }

    #[test]
    fn interactive_terminal_grep_colors_force_simple_terminal_grep() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("grep map README.md");
        enable_interactive_terminal_grep_colors(&mut tokens);
        let ast = parse(&tokens);

        assert_eq!(
            ast.commands[0].words,
            vec!["grep.exe", "--color=always", "map", "README.md"]
        );
    }

    #[test]
    fn interactive_cd_syncs_process_cwd_and_normalizes_pwd() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-cwd-sync");
        let target = temp.join("target");
        std::fs::create_dir_all(&target).unwrap();

        let mut shell = test_shell(HookConfig::default());
        let target_shell_path = shell_display_path(&target);
        let code = shell
            .execute_interactive_line(&format!("cd {}", shell_quote(&target_shell_path)))
            .unwrap();
        assert_eq!(
            code,
            0,
            "cd failed, PWD={:?}, target={target_shell_path}",
            shell.executor.get_env("PWD")
        );

        let completion_cwd = shell
            .completion_state
            .lock()
            .unwrap()
            .current_dir
            .canonicalize()
            .unwrap();
        assert_eq!(
            completion_cwd,
            target.canonicalize().unwrap(),
            "completion cwd did not sync, PWD={:?}",
            shell.executor.get_env("PWD")
        );
        assert_eq!(
            shell.executor.get_env("PWD").as_deref(),
            Some(target_shell_path.as_str())
        );
        if cfg!(windows) {
            assert!(
                !shell
                    .executor
                    .get_env("PWD")
                    .unwrap_or_default()
                    .starts_with("/c/"),
                "PWD should be Windows-native, got {:?}",
                shell.executor.get_env("PWD")
            );
        }

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn execute_line_syncs_cd_before_following_windows_child_command() {
        if !cfg!(windows) {
            return;
        }

        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-cwd-sequence");
        let start = temp.join("start");
        let target = start.join("target");
        let bin = temp.join("bin");
        let log = temp.join("cwdprobe.txt");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        write_fake_cwd_probe(&bin, &host_display_path(&log));

        let old_path = prepend_path_for_test(&bin);
        let old_pathext = std::env::var_os("PATHEXT");
        std::env::set_var("PATHEXT", ".COM;.EXE;.BAT;.CMD");
        std::env::set_current_dir(&start).unwrap();

        let mut shell = test_shell(HookConfig::default());
        let code = shell.execute_line("cd target; cwdprobe").unwrap();

        assert_eq!(code, 0);
        let observed = std::fs::read_to_string(&log).unwrap();
        let observed = host_path_to_shell_path(observed.trim());
        let expected = shell_display_path(&target);
        assert!(
            same_shell_dir(&observed, &expected),
            "native child cwd mismatch: observed={observed:?}, expected={expected:?}"
        );
        assert!(
            same_shell_dir(shell.executor.get_env("PWD").unwrap_or_default(), &expected),
            "executor PWD mismatch: {:?}, expected={expected:?}",
            shell.executor.get_env("PWD")
        );

        restore_path_for_test(old_path);
        match old_pathext {
            Some(value) => std::env::set_var("PATHEXT", value),
            None => std::env::remove_var("PATHEXT"),
        }
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn native_dotenv_precmd_applies_safe_assignments() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-native-dotenv-precmd");
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(
            temp.join(".env"),
            r#"
SAFE_VALUE=alpha
export QUOTED_VALUE="hello world"
SINGLE_VALUE='single value'
COMMENTED_VALUE=ok # comment
PATH=bad
NODE_OPTIONS=--require bad
BAD-KEY=bad
EXPAND_VALUE=$(whoami)
BACKTICK_VALUE=`whoami`
"#,
        )
        .unwrap();

        let mut shell = test_shell(HookConfig::default());
        shell.native_plugins.enabled = true;
        shell.native_plugins.presets = vec!["dotenv".to_string()];
        shell.executor.set_env("PWD", &shell_display_path(&temp));
        shell.run_precmd_hooks();

        assert_eq!(shell.executor.get_env("SAFE_VALUE"), Some("alpha"));
        assert_eq!(shell.executor.get_env("QUOTED_VALUE"), Some("hello world"));
        assert_eq!(shell.executor.get_env("SINGLE_VALUE"), Some("single value"));
        assert_eq!(shell.executor.get_env("COMMENTED_VALUE"), Some("ok"));
        assert!(shell.executor.get_env("BAD-KEY").is_none());
        assert!(shell.executor.get_env("EXPAND_VALUE").is_none());
        assert!(shell.executor.get_env("BACKTICK_VALUE").is_none());
        assert_ne!(shell.executor.get_env("PATH"), Some("bad"));
        assert_ne!(shell.executor.get_env("NODE_OPTIONS"), Some("--require bad"));

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn native_dotenv_chpwd_applies_project_env() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-native-dotenv-chpwd");
        let project = temp.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(".env"), "PROJECT_ENV=loaded\n").unwrap();

        let mut shell = test_shell(HookConfig::default());
        shell.native_plugins.enabled = true;
        shell.native_plugins.presets = vec!["dotenv".to_string()];
        let project_shell_path = shell_display_path(&project);
        shell
            .execute_interactive_line(&format!("cd {}", shell_quote(&project_shell_path)))
            .unwrap();

        assert_eq!(shell.executor.get_env("PROJECT_ENV"), Some("loaded"));

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn native_zoxide_command_changes_directory_and_tracks_pwd() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-native-zoxide");
        let bin = temp.join("bin");
        let target = temp.join("target");
        let log = temp.join("zoxide-add.txt");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        let target_path = host_display_path(&target);
        let target_shell_path = shell_display_path(&target);
        let log_path = host_display_path(&log);
        write_fake_zoxide(&bin, &target_path, &log_path);
        let old_path = prepend_path_for_test(&bin);

        let mut shell = test_shell(HookConfig::default());
        shell.native_plugins.enabled = true;
        shell.native_plugins.presets = vec!["zoxide".to_string()];

        shell.execute_line("z project").unwrap();
        let pwd = shell.executor.get_env("PWD").unwrap_or_default();
        assert!(
            same_shell_dir(&pwd, &target_shell_path),
            "{pwd} != {target_shell_path}"
        );

        shell.run_precmd_hooks();
        let tracked = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            tracked.trim(),
            shell_path_to_host_path(shell.executor.get_env("PWD").unwrap_or_default())
        );

        restore_path_for_test(old_path);
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn native_thefuck_command_corrects_previous_interactive_command() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-native-thefuck");
        let bin = temp.join("bin");
        let target = temp.join("target");
        let log = temp.join("thefuck-args.txt");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        let target_shell_path = shell_display_path(&target);
        let correction = format!("cd {}", shell_quote(&target_shell_path));
        let log_path = host_display_path(&log);
        write_fake_thefuck(&bin, &correction, &log_path);
        let old_path = prepend_path_for_test(&bin);

        let mut shell = test_shell(HookConfig::default());
        shell.native_plugins.enabled = true;
        shell.native_plugins.presets = vec!["thefuck".to_string()];

        assert_eq!(shell.execute_interactive_line("badcmd").unwrap(), 127);
        assert_eq!(shell.last_interactive_command.as_deref(), Some("badcmd"));
        assert_eq!(shell.last_interactive_exit_code, Some(127));

        assert_eq!(shell.execute_interactive_line("fuck").unwrap(), 0);
        let pwd = shell.executor.get_env("PWD").unwrap_or_default();
        assert!(
            same_shell_dir(&pwd, &target_shell_path),
            "{pwd} != {target_shell_path}"
        );
        let invoked_with = std::fs::read_to_string(&log).unwrap();
        assert!(invoked_with.contains("badcmd"), "{invoked_with}");
        assert_eq!(shell.last_interactive_command.as_deref(), Some("badcmd"));

        restore_path_for_test(old_path);
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn native_fzf_cd_command_changes_directory_to_selected_path() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-native-fzf-cd");
        let bin = temp.join("bin");
        let parent = temp.join("parent");
        let target = parent.join("target");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(parent.join("sibling")).unwrap();

        let target_shell_path = shell_display_path(&target);
        write_fake_fzf(&bin, &target_shell_path);
        let old_path = prepend_path_for_test(&bin);

        let mut shell = test_shell(HookConfig::default());
        shell.native_plugins.enabled = true;
        shell.native_plugins.presets = vec!["zsh-interactive-cd".to_string()];

        let parent_shell_path = shell_display_path(&parent);
        assert_eq!(
            shell
                .execute_line(&format!("cdf {}", shell_quote(&parent_shell_path)))
                .unwrap(),
            0
        );
        let pwd = shell.executor.get_env("PWD").unwrap_or_default();
        assert!(
            same_shell_dir(&pwd, &target_shell_path),
            "{pwd} != {target_shell_path}"
        );

        restore_path_for_test(old_path);
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn native_last_working_dir_command_and_repl_restore_use_cache() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-native-last-working-dir");
        let home = temp.join("home");
        let target = temp.join("target");
        let other = temp.join("other");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        let cache_path = temp.join("cache").join("last-working-dir");
        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        let target_shell_path = shell_display_path(&target);
        std::fs::write(&cache_path, format!("{target_shell_path}\n")).unwrap();

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home.clone();
        shell.last_working_dir_cache_path = cache_path.clone();
        shell.native_plugins.enabled = true;
        shell.native_plugins.presets = vec!["last-working-dir".to_string()];
        shell
            .execute_line(&format!("cd {}", shell_quote(&shell_display_path(&other))))
            .unwrap();

        assert_eq!(shell.execute_line("lwd").unwrap(), 0);
        let pwd = shell.executor.get_env("PWD").unwrap_or_default();
        assert!(
            same_shell_dir(&pwd, &target_shell_path),
            "{pwd} != {target_shell_path}"
        );

        let home_shell_path = shell_display_path(&home);
        let mut restore_shell = test_shell(HookConfig::default());
        restore_shell.home_dir = home.clone();
        restore_shell.last_working_dir_cache_path = cache_path.clone();
        restore_shell.native_plugins.enabled = true;
        restore_shell.native_plugins.presets = vec!["last-working-dir".to_string()];
        restore_shell
            .execute_line(&format!("cd {}", shell_quote(&home_shell_path)))
            .unwrap();
        restore_shell.restore_last_working_dir_for_repl();
        let restored_pwd = restore_shell.executor.get_env("PWD").unwrap_or_default();
        assert!(
            same_shell_dir(&restored_pwd, &target_shell_path),
            "{restored_pwd} != {target_shell_path}"
        );

        let other_shell_path = shell_display_path(&other);
        let mut no_restore_shell = test_shell(HookConfig::default());
        no_restore_shell.home_dir = home;
        no_restore_shell.last_working_dir_cache_path = cache_path;
        no_restore_shell.native_plugins.enabled = true;
        no_restore_shell.native_plugins.presets = vec!["last-working-dir".to_string()];
        no_restore_shell
            .execute_line(&format!("cd {}", shell_quote(&other_shell_path)))
            .unwrap();
        no_restore_shell.restore_last_working_dir_for_repl();
        let unchanged_pwd = no_restore_shell.executor.get_env("PWD").unwrap_or_default();
        assert!(
            same_shell_dir(&unchanged_pwd, &other_shell_path),
            "{unchanged_pwd} != {other_shell_path}"
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn native_last_working_dir_chpwd_writes_cache() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-native-last-working-dir-chpwd");
        let home = temp.join("home");
        let target = temp.join("target");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        let cache_path = temp.join("cache").join("last-working-dir");
        let target_shell_path = shell_display_path(&target);

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.last_working_dir_cache_path = cache_path.clone();
        shell.native_plugins.enabled = true;
        shell.native_plugins.presets = vec!["last-working-dir".to_string()];
        shell
            .execute_interactive_line(&format!("cd {}", shell_quote(&target_shell_path)))
            .unwrap();

        let cached = std::fs::read_to_string(&cache_path).unwrap();
        assert_eq!(cached.trim(), target_shell_path);

        let _ = std::fs::remove_dir_all(temp);
    }

    fn test_shell(hooks: HookConfig) -> Shell {
        let mut shell = Shell {
            executor: Executor::new(),
            completion_state: Arc::new(Mutex::new(CompletionState::new(PathBuf::from(".")))),
            prompt: PromptBackend::Template(WinuxshPrompt::new(None, None, None, "default")),
            home_dir: PathBuf::from("."),
            history_path: PathBuf::from(".winuxsh_history"),
            history_max_size: 10000,
            history_ignore_space_prefixed: false,
            menu_config: MenuConfig::default(),
            editor_mode: EditorMode::Emacs,
            autosuggest: AutosuggestConfig::default(),
            syntax_highlighting: SyntaxHighlightConfig::default(),
            native_widgets: NativeWidgetConfig::default(),
            native_widget_bindings: Vec::new(),
            native_plugins: NativePluginConfig::default(),
            hooks,
            aliases: HashMap::new(),
            zoxide_last_tracked_dir: None,
            last_working_dir_cache_path: PathBuf::from(".winuxsh/cache/last-working-dir"),
            last_working_dir_restored: false,
            last_interactive_command: None,
            last_interactive_exit_code: None,
            line_editor: None,
        };
        shell.sync_executor_pwd_from_process_cwd();
        shell
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
    }

    fn shell_display_path(path: &std::path::Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    fn host_display_path(path: &std::path::Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    fn prepend_path_for_test(dir: &std::path::Path) -> Option<std::ffi::OsString> {
        let old_path = std::env::var_os("PATH");
        let mut paths = vec![dir.to_path_buf()];
        if let Some(old_path) = &old_path {
            paths.extend(std::env::split_paths(old_path));
        }
        let new_path = std::env::join_paths(paths).unwrap();
        std::env::set_var("PATH", new_path);
        old_path
    }

    fn restore_path_for_test(old_path: Option<std::ffi::OsString>) {
        match old_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }

    struct CwdGuard {
        previous: PathBuf,
    }

    impl CwdGuard {
        fn capture() -> Self {
            Self {
                previous: std::env::current_dir().unwrap(),
            }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    fn write_fake_zoxide(bin: &std::path::Path, target_path: &str, log_path: &str) {
        let script = if cfg!(windows) {
            format!(
                "@echo off\r\nif \"%1\"==\"query\" (\r\n  <nul set /p ={}\r\n  exit /b 0\r\n)\r\nif \"%1\"==\"add\" (\r\n  >\"{}\" echo %~2\r\n  exit /b 0\r\n)\r\nexit /b 1\r\n",
                target_path, log_path
            )
        } else {
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"query\" ]; then\n  printf '%s\\n' '{}'\n  exit 0\nfi\nif [ \"$1\" = \"add\" ]; then\n  printf '%s\\n' \"$2\" > '{}'\n  exit 0\nfi\nexit 1\n",
                target_path, log_path
            )
        };
        let exe = bin.join(if cfg!(windows) { "zoxide.cmd" } else { "zoxide" });
        std::fs::write(&exe, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&exe).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&exe, permissions).unwrap();
        }
    }

    fn write_fake_thefuck(bin: &std::path::Path, correction: &str, log_path: &str) {
        let script = if cfg!(windows) {
            format!(
                "@echo off\r\n>\"{}\" echo %*\r\n<nul set /p ={}\r\nexit /b 0\r\n",
                log_path, correction
            )
        } else {
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nprintf '%s\\n' {}\n",
                log_path,
                shell_quote(correction)
            )
        };
        let exe = bin.join(if cfg!(windows) { "thefuck.cmd" } else { "thefuck" });
        std::fs::write(&exe, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&exe).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&exe, permissions).unwrap();
        }
    }

    fn write_fake_fzf(bin: &std::path::Path, selected_path: &str) {
        let script = if cfg!(windows) {
            format!("@echo off\r\n<nul set /p ={}\r\nexit /b 0\r\n", selected_path)
        } else {
            format!("#!/bin/sh\nprintf '%s\\n' {}\n", shell_quote(selected_path))
        };
        let exe = bin.join(if cfg!(windows) { "fzf.cmd" } else { "fzf" });
        std::fs::write(&exe, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&exe).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&exe, permissions).unwrap();
        }
    }

    fn write_fake_cwd_probe(bin: &std::path::Path, log_path: &str) {
        let script = format!("@echo off\r\n>\"{}\" echo %CD%\r\nexit /b 0\r\n", log_path);
        std::fs::write(bin.join("cwdprobe.cmd"), script).unwrap();
    }
}
