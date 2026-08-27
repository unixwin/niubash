//! Shell state and execution entry point
//!
//! Wraps a `rubash::Executor` and provides the interactive shell machinery
//! (prompt, history, completion). All shell language semantics are delegated
//! to rubash; this layer only adds the Windows-facing UX.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reedline::{Completer, Reedline};
use rubash::{
    executor::{Executor, HostExternalCommandOutput},
    lexer::tokenize,
    parser::parse,
    Ast, Token, TokenKind,
};

use crate::completion::{CompletionState, WinuxshCompleter};
use crate::config::{
    load as load_config, AutosuggestConfig, EditorMode, HookConfig, MenuConfig, NativePluginConfig,
    NativeWidgetBinding, NativeWidgetConfig, SyntaxHighlightConfig,
};
use crate::git_status::GitPromptSymbols;
use crate::path_utils::{shell_home_dir, shell_path_to_host_path};
use crate::plugins::{PluginKind, PluginProcessSpec, PluginRuntimeState, OFFICIAL_BUNDLE_NAME};
use crate::prompt::{BashPrompt, GitPromptDecor, PromptBackend, PromptIndicators, WinuxshPrompt};
use crate::prompt_segments::{
    SegmentId, SegmentPreset, SegmentPrompt, SegmentPromptAdapter, SegmentPromptConfig,
};

use crate::winuxcmd;

const DOTENV_MAX_SIZE: u64 = 10 * 1024 * 1024;
const COMPATIBLE_SHELL_PATH_ENV: &str = "WINUXSH_COMPATIBLE_SHELL_PATH";
const COMMAND_NOT_FOUND_PROVIDER_NAME: &str = "command-not-found";
#[allow(dead_code)]
const COMMAND_NOT_FOUND_PROVIDER_MAX_OUTPUT_BYTES: usize = 16 * 1024;
#[allow(dead_code)]
const COMMAND_NOT_FOUND_PROVIDER_MAX_LINES: usize = 32;
#[allow(dead_code)]
const COMMAND_NOT_FOUND_PROVIDER_MAX_LINE_BYTES: usize = 512;
const WINUXSH_RC_FILE: &str = ".winuxshrc";
const WINUXSH_LEGACY_RC_FILE: &str = ".winshrc";

/// Top-level shell state.
pub struct Shell {
    pub executor: Executor,
    pub completion_state: Arc<Mutex<CompletionState>>,
    pub prompt: PromptBackend,
    pub home_dir: PathBuf,
    pub shell_root: Option<PathBuf>,
    pub history_path: PathBuf,
    pub history_max_size: usize,
    pub history_ignore_space_prefixed: bool,
    pub history_mode: crate::config::HistoryMode,
    pub menu_config: MenuConfig,
    pub editor_mode: EditorMode,
    pub autosuggest: AutosuggestConfig,
    pub syntax_highlighting: SyntaxHighlightConfig,
    pub native_widgets: NativeWidgetConfig,
    pub native_widget_bindings: Vec<NativeWidgetBinding>,
    pub plugins: PluginRuntimeState,
    pub native_plugins: NativePluginConfig,
    pub hooks: HookConfig,
    pub aliases: HashMap<String, String>,
    pub zoxide_last_tracked_dir: Option<String>,
    pub last_working_dir_cache_path: PathBuf,
    pub last_working_dir_restored: bool,
    pub last_interactive_command: Option<String>,
    pub last_interactive_exit_code: Option<i32>,
    pub line_editor: Option<Reedline>,
    plugin_prompt_sync: PluginPromptSyncConfig,
    process_stdin_pipeline_bridge: bool,
    bash_prompt_command_running: bool,
}

#[derive(Debug, Clone)]
struct PluginPromptSyncConfig {
    enabled: bool,
    indicators: PromptIndicators,
    theme_name: String,
    prompt_symbol: String,
    git_prompt_symbols: GitPromptSymbols,
    git_prompt_format: Option<String>,
}

impl PluginPromptSyncConfig {
    #[cfg(test)]
    fn disabled() -> Self {
        Self {
            enabled: false,
            indicators: PromptIndicators::default(),
            theme_name: "default".to_string(),
            prompt_symbol: "%".to_string(),
            git_prompt_symbols: GitPromptSymbols::default(),
            git_prompt_format: None,
        }
    }
}

impl Shell {
    /// Construct a fresh shell: load config, install Ctrl+C handler, inject
    /// winuxcmd onto PATH, set up completion state and history.
    pub fn new() -> anyhow::Result<Self> {
        Self::new_with_script_name(Some("winuxsh"))
    }

    /// Construct a shell for scripts arriving on process stdin.
    pub fn new_for_stdin_script() -> anyhow::Result<Self> {
        Self::new_with_script_name(None)
    }

    fn new_with_script_name(script_name: Option<&str>) -> anyhow::Result<Self> {
        // 1. Load runtime defaults and environment-backed state.
        let mut config = load_config();
        config.history = config.history.with_env_overrides();

        // 2. Select the WinuxCmd installation and use its real directory tree
        // before constructing the executor.
        let home_dir = shell_home_dir().unwrap_or_else(|| PathBuf::from("."));
        let selected_winuxcmd_path = if config.winuxcmd_enabled {
            match winuxcmd::prepare_winuxcmd_with_override(None) {
                Ok(path) => Some(path),
                Err(e) => {
                    log::debug!("winuxcmd not on PATH: {}", e);
                    None
                }
            }
        } else {
            log::debug!("winuxcmd PATH injection disabled by config");
            None
        };
        let shell_root = prepare_shell_root(selected_winuxcmd_path.as_deref())?;

        // 3. Build rubash Executor after host path selection.
        let shell_was_missing = std::env::var_os("SHELL").is_none();
        if cfg!(windows) {
            // Keep the embedded Rubash path-display contract active for the
            // entire shell lifetime; `cd` consults the process environment.
            std::env::set_var("WINUXSH_SHELL_PATH_STYLE", "native");
        }
        let mut executor = Executor::new();
        // Reedline is the interactive history owner. Keep Rubash's Bash
        // history machinery disabled in the host shell so HISTFILE cannot
        // create a second, competing history stream.
        executor.set_shell_option("history", false);
        executor.unset_env("HISTFILE");
        // Winuxsh delegates Windows elevation to external providers such as the
        // WPM gsudo package. The experimental Rubash builtin is opt-in only.
        if std::env::var("WINUXSH_ENABLE_RUBASH_SUDO").as_deref() != Ok("1") {
            executor.set_builtin_disabled("sudo", true);
        }
        let shell_name = std::env::var("WINUXSH_INVOKED_AS")
            .ok()
            .or_else(|| std::env::args().next());
        if let Some(shell_name) = shell_name {
            let invoked_name = shell_name
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&shell_name)
                .trim_end_matches(".exe")
                .to_ascii_lowercase();
            executor.set_env("__RUBASH_SHELL_NAME", &invoked_name);
            if matches!(invoked_name.as_str(), "sh" | "ash") {
                executor.set_env("__RUBASH_POSIX_MODE", "1");
                executor.set_shell_option("posix", true);
            }
        }
        // Starship is initialized through its Bash integration even when
        // Winuxsh is invoked through the sh/bash command shims.
        executor.set_env("STARSHIP_SHELL", "bash");
        if cfg!(windows) && shell_was_missing {
            if let Ok(exe) = std::env::current_exe() {
                executor.set_env("SHELL", &host_path_to_shell_path(&exe.to_string_lossy()));
            }
        }
        // Winuxsh always presents an interactive shell, so aliases loaded
        // from ~/.winuxshrc must expand without requiring a user shopt line.
        executor.set_shopt_option("expand_aliases", true);
        executor.set_external_file_builtins_enabled(false);
        if let Some(root) = &shell_root {
            executor.set_shell_root(root);
        }
        if let Some(winuxcmd_path) = &selected_winuxcmd_path {
            executor.set_winuxcmd_path(winuxcmd_path);
        }
        if let Some(shell_path) = compatible_shell_path_from_env() {
            executor.set_compatible_shell_path(shell_path);
        }
        if let Some(script_name) = script_name {
            executor.set_env("__RUBASH_SCRIPT_NAME", script_name);
        }
        executor.set_env("WINUXSH_PROMPT_SYMBOL", &config.shell.prompt_symbol);
        sync_executor_path_from_process_path(&mut executor);
        let plugin_state = crate::plugins::effective_plugin_state(&config.plugins);
        let host_plugin_state = plugin_state.clone();
        executor.set_host_external_command_handler(move |words, env| {
            execute_winuxsh_host_external_command(words, env, &host_plugin_state)
        });

        // 5. Apply managed aliases so explicit machine state remains
        // authoritative when names collide.
        let mut aliases = HashMap::new();
        for (name, value) in &config.aliases {
            if apply_alias(&mut executor, name, value) {
                aliases.insert(name.clone(), value.clone());
            } else {
                log::warn!("Skipping invalid alias from config: {}", name);
            }
        }

        // Official Winuxsh builtin alias packs. Managed aliases take
        // precedence, and canonical plugin state gates each pack.
        for pack_name in ["git", "docker", "kubectl", "npm"] {
            if !plugin_state.is_enabled(pack_name) {
                continue;
            }
            let Some(pack_aliases) = crate::plugins::plugin_aliases(pack_name) else {
                continue;
            };
            for (name, value) in pack_aliases {
                if aliases.contains_key(&name) {
                    continue;
                }
                if apply_alias(&mut executor, &name, &value) {
                    aliases.insert(name, value);
                }
            }
        }

        // 6. Prompt + theme. Choose backend based on `prompt_style`:
        //    "segments"  -> new p10k-style segment engine
        //    "template"  -> legacy template engine (default, backward-compatible)
        let prompts_plugin_disabled =
            plugin_state.has_decision("prompts") && !plugin_state.is_enabled("prompts");
        let prompt_style = if prompts_plugin_disabled {
            "template"
        } else {
            config.shell.prompt_style.as_deref().unwrap_or("template")
        };
        let git_prompt_symbols = GitPromptSymbols::from(&config.git_prompt);
        let template_git_prompt_format = config.shell.git_prompt_format.clone();
        let native_prompt_configured = config.shell.prompt_format.is_some()
            || config.shell.right_prompt_format.is_some()
            || config.shell.git_prompt_format.is_some()
            || config.shell.prompt_style.is_some()
            || config.shell.segment_preset.is_some()
            || config.shell.left_prompt_elements.is_some()
            || config.shell.right_prompt_elements.is_some();
        let plugin_prompt_sync = PluginPromptSyncConfig {
            enabled: plugin_state.is_enabled("prompt-core") && !native_prompt_configured,
            indicators: config.shell.prompt_indicators.clone(),
            theme_name: config.theme_name.clone(),
            prompt_symbol: config.shell.prompt_symbol.clone(),
            git_prompt_symbols: git_prompt_symbols.clone(),
            git_prompt_format: template_git_prompt_format.clone(),
        };
        let prompt: PromptBackend = if prompt_style == "segments" {
            let preset_name = config.shell.segment_preset.as_deref().unwrap_or("classic");
            let preset = SegmentPreset::from_name(preset_name).unwrap_or(SegmentPreset::Classic);
            let mut seg_config = SegmentPromptConfig::from_preset(
                preset,
                &config.shell.prompt_symbol,
                git_prompt_symbols.clone(),
            );
            if let Some(bundle_preset) = crate::plugins::plugin_prompt_preset(preset_name) {
                let left_elements: Vec<SegmentId> = bundle_preset
                    .left_elements
                    .iter()
                    .filter_map(|segment| SegmentId::from_name(segment))
                    .collect();
                if !left_elements.is_empty() {
                    seg_config.left_elements = left_elements;
                }
                seg_config.right_elements = bundle_preset
                    .right_elements
                    .iter()
                    .filter_map(|segment| SegmentId::from_name(segment))
                    .collect();
                seg_config.separator = bundle_preset.separator;
                seg_config.git_prompt_format = bundle_preset.git_prompt_format;
            }
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
            let prompt_format = config.shell.prompt_format.clone();
            let right_prompt_format = config.shell.right_prompt_format.clone();
            let template_prompt = WinuxshPrompt::new_with_symbol(
                prompt_format,
                right_prompt_format,
                template_git_prompt_format.clone(),
                config.shell.prompt_indicators.clone(),
                &config.theme_name,
                git_prompt_symbols,
                config.shell.prompt_symbol.clone(),
            );
            PromptBackend::Template(template_prompt)
        };

        // 7. User-local state files.
        normalize_executor_home_env(&mut executor, &home_dir);
        ensure_windows_profile_env(&mut executor, &home_dir);
        ensure_prompt_terminal_env(&mut executor);
        set_default_winuxsh_framework_env(&mut executor, &home_dir);
        let history_path = config
            .history
            .path
            .clone()
            .unwrap_or_else(|| home_dir.join(".winuxsh_history"));
        let history_provider = crate::history::RubashHistoryProvider::with_file(
            config.history.max_size,
            history_path.clone(),
            config.history.mode,
        )
        .map_err(|error| {
            anyhow::anyhow!(
                "failed to open history provider {}: {}",
                history_path.display(),
                error
            )
        })?;
        executor.set_history_provider(Rc::new(RefCell::new(history_provider)));
        let last_working_dir_cache_path = default_last_working_dir_cache_path(&home_dir);

        // 8. Completion state.
        let mut initial_completion_state = CompletionState::new(
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        );
        initial_completion_state.behavior = config.completion_behavior;
        let completion_state = Arc::new(Mutex::new(initial_completion_state));
        let bundle_completion_defs = crate::plugins::plugin_completion_defs(&plugin_state);

        // 9. Load completion dirs from config (inline, not in thread).
        {
            let mut s = completion_state.lock().unwrap();
            s.load_completion_dirs_with_bundle_and_definitions(
                &config.completion_dirs,
                bundle_completion_defs,
                Vec::new(),
            );
        }

        let mut native_widgets = config.native_widgets.clone();
        if plugin_state.has_decision("keybindings") && !plugin_state.is_enabled("keybindings") {
            native_widgets.enabled = false;
            native_widgets.presets.clear();
        }
        let native_widget_bindings = Vec::new();

        let mut shell = Self {
            executor,
            completion_state,
            prompt,
            home_dir,
            shell_root,
            history_path,
            history_max_size: config.history.max_size,
            history_ignore_space_prefixed: config.history.ignore_space_prefixed,
            history_mode: config.history.mode,
            menu_config: config.menus,
            editor_mode: config.editor.edit_mode,
            autosuggest: config.autosuggest.with_env_overrides(),
            syntax_highlighting: config.syntax_highlighting.with_env_overrides(),
            native_widgets,
            native_widget_bindings,
            plugins: plugin_state,
            native_plugins: config.native_plugins,
            hooks: config.hooks,
            aliases,
            zoxide_last_tracked_dir: None,
            last_working_dir_cache_path,
            last_working_dir_restored: false,
            last_interactive_command: None,
            last_interactive_exit_code: None,
            line_editor: None,
            plugin_prompt_sync,
            process_stdin_pipeline_bridge: false,
            bash_prompt_command_running: false,
        };
        shell.sync_executor_pwd_from_process_cwd();
        shell.update_completion_state();
        Ok(shell)
    }

    pub fn enable_process_stdin_pipeline_bridge(&mut self) {
        self.process_stdin_pipeline_bridge = true;
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
        rewrite_winuxcmd_command_shims(&mut tokens, interactive_terminal_colors);

        let git_command = tokens
            .first()
            .is_some_and(|first| first.value.trim().eq_ignore_ascii_case("git"));

        // parse() returns Ast directly (not Result) in rubash.
        let mut ast = parse(&tokens);
        normalize_bare_windows_drive_commands(&mut ast);
        normalize_cd_windows_drive_args(&mut ast);
        normalize_winuxcmd_slash_drive_args(&mut ast);

        let mut printed_command_not_found_hints = false;
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
        } else if let Some(code) = self.execute_process_plugin_simple_ast(&ast)? {
            code
        } else if let Some(execution) = self.execute_host_synced_simple_ast(&ast) {
            match execution {
                Ok(code) => code,
                Err(rubash::executor::ExecuteError::ExitCode(code)) => code,
                Err(rubash::executor::ExecuteError::Return(code)) => code,
                Err(rubash::executor::ExecuteError::CommandNotFound(cmd)) => {
                    if self.command_not_found_plugin_enabled() {
                        let args = command_not_found_args(&ast, &cmd);
                        self.print_native_command_not_found(&cmd, &args);
                    } else {
                        eprintln!("winuxsh: {}: command not found", cmd);
                        self.print_command_not_found_hints(&cmd);
                    }
                    printed_command_not_found_hints = true;
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
                    if self.command_not_found_plugin_enabled() {
                        let args = command_not_found_args(&ast, &cmd);
                        self.print_native_command_not_found(&cmd, &args);
                    } else {
                        eprintln!("winuxsh: {}: command not found", cmd);
                        self.print_command_not_found_hints(&cmd);
                    }
                    printed_command_not_found_hints = true;
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

        if code == 127 && !printed_command_not_found_hints {
            self.print_command_not_found_hints_if_missing(&ast);
        }

        self.sync_process_cwd_from_executor_pwd();
        if git_command {
            self.mark_gitstatus_dirty_current_dir();
        }
        self.sync_process_path_from_executor_path();
        self.sync_alias_mirror_from_executor();
        Ok(code)
    }

    /// Execute a line as an interactive REPL command, including native hook
    /// points for prompt, command, and directory-change lifecycle behavior.
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
    /// This mirrors common last-working-dir guards: only jump when the
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

    /// Source the user's REPL startup file once before the first prompt.
    ///
    /// `~/.winuxshrc` is the primary interactive entry point. If it exists,
    /// source plugins are expected to be loaded from that file through the
    /// framework entry point. `~/.winshrc` remains a compatibility fallback.
    pub fn run_startup_rc(&mut self) {
        normalize_executor_home_env(&mut self.executor, &self.home_dir);
        ensure_windows_profile_env(&mut self.executor, &self.home_dir);
        ensure_prompt_terminal_env(&mut self.executor);
        self.sync_gitstatus_prompt_env();
        let rc_path = self.startup_rc_path();
        let primary_rc = rc_path.as_ref().is_some_and(|path| {
            path.file_name().and_then(|name| name.to_str()) == Some(WINUXSH_RC_FILE)
        });
        if !primary_rc {
            self.run_source_plugin_startup_scripts();
        }
        self.run_process_plugin_hooks("startup", &[]);
        let Some(path) = rc_path else {
            self.sync_prompt_from_plugin_env();
            return;
        };
        let Ok(script) = std::fs::read_to_string(&path) else {
            self.sync_prompt_from_plugin_env();
            return;
        };

        self.executor.set_env("WINUXSH_REPL_STARTUP", "1");
        match self.source_file_into_current_shell(&path) {
            Ok(code) => {
                if code != 0 {
                    log::warn!("{} exited with status {}", path.display(), code);
                }
                self.sync_alias_mirror_from_script(&script, code);
            }
            Err(err) => log::warn!("{} failed: {}", path.display(), err),
        }
        let _ = self.execute_script("unset WINUXSH_REPL_STARTUP");
        self.update_completion_state();
        self.sync_prompt_from_plugin_env();
    }

    fn startup_rc_path(&self) -> Option<PathBuf> {
        let primary = self.home_dir.join(WINUXSH_RC_FILE);
        if primary.is_file() {
            return Some(primary);
        }
        let legacy = self.home_dir.join(WINUXSH_LEGACY_RC_FILE);
        legacy.is_file().then_some(legacy)
    }

    fn uses_primary_startup_rc(&self) -> bool {
        self.startup_rc_path().as_ref().is_some_and(|path| {
            path.file_name().and_then(|name| name.to_str()) == Some(WINUXSH_RC_FILE)
        })
    }

    fn run_framework_hook_runner(&mut self, runner: &str, context: &[(&str, String)]) {
        for (name, value) in context {
            self.executor.set_env(name, value);
        }
        let _ = self.execute_script(&format!("{runner} 2>/dev/null || true"));
        if !context.is_empty() {
            let names = context
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(" ");
            let _ = self.execute_script(&format!("unset {names}"));
        }
    }

    fn sync_prompt_from_plugin_env(&mut self) {
        if !self.plugin_prompt_sync.enabled {
            return;
        }
        if self.bash_prompt_env_active() {
            return;
        }

        let left_template = self
            .executor
            .get_env("WINUXSH_PROMPT_LEFT")
            .map(str::to_owned);
        let right_template = self
            .executor
            .get_env("WINUXSH_PROMPT_RIGHT")
            .map(str::to_owned);
        if left_template.is_none() && right_template.is_none() {
            self.clear_gitstatus_prompt_env("none");
            return;
        }

        let theme_name = self
            .executor
            .get_env("WINUXSH_ACTIVE_THEME")
            .or_else(|| self.executor.get_env("WINUXSH_THEME"))
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&self.plugin_prompt_sync.theme_name);
        let prompt_symbol = self
            .executor
            .get_env("WINUXSH_PROMPT_SYMBOL")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&self.plugin_prompt_sync.prompt_symbol);
        let git_prompt_snapshot = self
            .executor
            .get_env("WINUXSH_PROMPT_GIT")
            .map(str::to_owned);
        let git_status_snapshot = self.git_status_from_prompt_env();
        let git_prompt_decor = self.git_prompt_decor_from_env();

        let mut prompt = WinuxshPrompt::new_with_symbol(
            left_template,
            right_template,
            self.plugin_prompt_sync.git_prompt_format.clone(),
            self.plugin_prompt_sync.indicators.clone(),
            theme_name,
            self.plugin_prompt_sync.git_prompt_symbols.clone(),
            prompt_symbol.to_string(),
        );
        prompt.set_git_prompt_snapshot(git_prompt_snapshot);
        prompt.set_git_status_snapshot(git_status_snapshot);
        prompt.set_git_prompt_decor(git_prompt_decor);
        self.prompt = PromptBackend::Template(prompt);
    }

    fn bash_prompt_env_active(&self) -> bool {
        self.executor
            .get_env("PROMPT_COMMAND")
            .is_some_and(|value| !value.trim().is_empty())
            || self
                .executor
                .get_env("PS1")
                .is_some_and(|value| !value.is_empty())
    }

    fn run_bash_prompt_command(&mut self, last_exit_code: i32) {
        if self.bash_prompt_command_running {
            return;
        }
        let Some(command) = self
            .executor
            .get_env("PROMPT_COMMAND")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
        else {
            return;
        };

        self.bash_prompt_command_running = true;
        ensure_prompt_terminal_env(&mut self.executor);
        self.executor.set_last_exit_code(last_exit_code);
        let _ = self.execute_script(&command);
        self.executor.set_last_exit_code(last_exit_code);
        self.bash_prompt_command_running = false;
    }

    fn sync_bash_prompt_from_env(&mut self) {
        if !self.bash_prompt_env_active() {
            return;
        }
        let ps1 = self.executor.get_env("PS1").unwrap_or("\\$ ").to_string();
        let ps2 = self.executor.get_env("PS2").unwrap_or("> ").to_string();
        let left = self.executor.expand_prompt_string_mut(&ps1);
        let multiline = self.executor.expand_prompt_string_mut(&ps2);
        self.prompt = PromptBackend::Bash(BashPrompt::new(left, multiline));
    }

    fn run_bash_ps0_preexec(&mut self) {
        let Some(ps0) = self
            .executor
            .get_env("PS0")
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
        else {
            return;
        };
        let last_exit_code = self.executor.last_exit_code();
        self.executor.set_env("PS0", &ps0);
        let rendered = self.executor.expand_prompt_string_mut(&ps0);
        if !rendered.is_empty() {
            let _ = std::io::stdout().write_all(rendered.as_bytes());
            let _ = std::io::stdout().flush();
        }
        self.executor.set_last_exit_code(last_exit_code);
    }

    fn git_status_from_prompt_env(&self) -> Option<crate::git_status::GitRepoStatus> {
        if self.executor.get_env("WINUXSH_PROMPT_GIT_SOURCE") != Some("native") {
            return None;
        }
        let branch = self
            .executor
            .get_env("WINUXSH_GITSTATUS_BRANCH")
            .filter(|value| !value.is_empty())
            .map(str::to_string)?;
        let status = crate::git_status::GitRepoStatus {
            branch: Some(branch),
            dirty: self.executor.get_env("WINUXSH_GITSTATUS_DIRTY") == Some("1"),
            staged: self.parse_gitstatus_count("WINUXSH_GITSTATUS_STAGED"),
            unstaged: self.parse_gitstatus_count("WINUXSH_GITSTATUS_UNSTAGED"),
            untracked: self.parse_gitstatus_count("WINUXSH_GITSTATUS_UNTRACKED"),
            deleted: self.parse_gitstatus_count("WINUXSH_GITSTATUS_DELETED"),
            ahead: self.parse_gitstatus_count("WINUXSH_GITSTATUS_AHEAD"),
            behind: self.parse_gitstatus_count("WINUXSH_GITSTATUS_BEHIND"),
            stashes: self.parse_gitstatus_count("WINUXSH_GITSTATUS_STASHES"),
            conflicts: self.parse_gitstatus_count("WINUXSH_GITSTATUS_CONFLICTS"),
        };
        if self.executor.get_env("WINUXSH_PROMPT_GIT")
            != Some(self.render_git_prompt_snapshot(&status).as_str())
        {
            return None;
        }
        Some(status)
    }

    fn parse_gitstatus_count(&self, name: &str) -> usize {
        self.executor
            .get_env(name)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0)
    }

    fn git_prompt_decor_from_env(&self) -> GitPromptDecor {
        GitPromptDecor {
            prefix: self
                .executor
                .get_env("WINUXSH_THEME_GIT_PROMPT_PREFIX")
                .unwrap_or("git:(")
                .to_string(),
            suffix: self
                .executor
                .get_env("WINUXSH_THEME_GIT_PROMPT_SUFFIX")
                .unwrap_or(")")
                .to_string(),
            dirty_suffix: self
                .executor
                .get_env("WINUXSH_THEME_GIT_PROMPT_DIRTY")
                .unwrap_or(" *")
                .to_string(),
            clean_suffix: self
                .executor
                .get_env("WINUXSH_THEME_GIT_PROMPT_CLEAN")
                .unwrap_or("")
                .to_string(),
        }
    }

    fn run_source_plugin_startup_scripts(&mut self) {
        let context = [("WINUXSH_REPL_PLUGIN_STARTUP", "1".to_string())];
        self.run_source_plugin_scripts_for_hook("startup", &context);
    }

    fn run_source_plugin_scripts_for_hook(&mut self, hook_name: &str, context: &[(&str, String)]) {
        for source in crate::plugins::source_plugin_scripts_for_hook(&self.plugins, hook_name) {
            let script = match std::fs::read_to_string(&source.path) {
                Ok(script) => script,
                Err(err) => {
                    log::warn!(
                        "source plugin '{}' hook '{}' failed to read {}: {}",
                        source.pack,
                        hook_name,
                        source.path.display(),
                        err
                    );
                    continue;
                }
            };
            let plugin_dir = source
                .path
                .parent()
                .map(|path| host_path_to_shell_path(&path.to_string_lossy()))
                .unwrap_or_default();
            let bundle_root = host_path_to_shell_path(&source.bundle_root.to_string_lossy());
            let plugin_source = host_path_to_shell_path(&source.path.to_string_lossy());
            for (name, value) in context {
                self.executor.set_env(name, value);
            }
            self.executor.set_env("WINUXSH", &bundle_root);
            self.executor
                .set_env("WINUXSH_PLUGIN_BUNDLE_DIR", &bundle_root);
            self.executor.set_env("WINUXSH_PLUGIN_NAME", &source.pack);
            self.executor.set_env("WINUXSH_PLUGIN_DIR", &plugin_dir);
            self.executor
                .set_env("WINUXSH_PLUGIN_SOURCE", &plugin_source);
            self.executor.set_env("WINUXSH_PLUGIN_HOOK", hook_name);
            match self.source_file_into_current_shell(&source.path) {
                Ok(code) => {
                    if code != 0 {
                        log::warn!(
                            "source plugin '{}' hook '{}' exited with status {}",
                            source.pack,
                            hook_name,
                            code
                        );
                    }
                    self.sync_alias_mirror_from_script(&script, code);
                }
                Err(err) => log::warn!(
                    "source plugin '{}' hook '{}' failed from {}: {}",
                    source.pack,
                    hook_name,
                    source.path.display(),
                    err
                ),
            }
            let mut unset_names = vec![
                "WINUXSH_PLUGIN_NAME".to_string(),
                "WINUXSH_PLUGIN_DIR".to_string(),
                "WINUXSH_PLUGIN_SOURCE".to_string(),
                "WINUXSH_PLUGIN_HOOK".to_string(),
            ];
            unset_names.extend(context.iter().map(|(name, _)| (*name).to_string()));
            let _ = self.execute_script(&format!("unset {}", unset_names.join(" ")));
        }
        self.update_completion_state();
    }

    fn source_file_into_current_shell(&mut self, path: &Path) -> anyhow::Result<i32> {
        let shell_path = host_path_to_shell_path(&path.to_string_lossy());
        self.execute_script(&format!(". {}", shell_quote(&shell_path)))
    }

    /// Run native hooks before rendering the next prompt.
    pub fn run_precmd_hooks(&mut self) {
        let last_exit_code = self.executor.last_exit_code();
        self.run_native_precmd_plugins();
        self.sync_gitstatus_prompt_env();
        let hooks = self.hooks.precmd.clone();
        let last_exit_code_string = last_exit_code.to_string();
        // Set in process env so segment prompt can read it via std::env::var.
        std::env::set_var("WINUXSH_LAST_EXIT_CODE", &last_exit_code_string);
        let context = [("WINUXSH_LAST_EXIT_CODE", last_exit_code_string)];
        if self.uses_primary_startup_rc() {
            self.run_framework_hook_runner("winuxsh_run_precmd_hooks", &context);
        } else {
            self.run_source_plugin_scripts_for_hook("precmd", &context);
        }
        self.run_process_plugin_hooks("precmd", &context);
        self.run_hook_scripts(&hooks, &context);
        self.run_bash_prompt_command(last_exit_code);
        self.sync_bash_prompt_from_env();
        self.sync_prompt_from_plugin_env();
    }

    /// Run native hooks immediately before the user's interactive command.
    pub fn run_preexec_hooks(&mut self, command: &str) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        self.run_native_preexec_plugins(command);
        let hooks = self.hooks.preexec.clone();
        let context = [("WINUXSH_PREEXEC_COMMAND", command.to_string())];
        if self.uses_primary_startup_rc() {
            self.run_framework_hook_runner("winuxsh_run_preexec_hooks", &context);
        } else {
            self.run_source_plugin_scripts_for_hook("preexec", &context);
        }
        self.run_process_plugin_hooks("preexec", &context);
        self.run_hook_scripts(&hooks, &context);
        self.run_bash_ps0_preexec();
    }

    /// Run native hooks when the interactive command changed directories.
    pub fn run_chpwd_hooks_if_changed(&mut self, old_pwd: &str, new_pwd: &str) {
        if same_shell_dir(old_pwd, new_pwd) {
            return;
        }
        let env = self.executor.env_vars_snapshot();
        if let Some(cwd) = shell_pwd_to_existing_host_dir(new_pwd, &env) {
            crate::git_status::request_refresh(&cwd);
        }
        self.run_native_chpwd_plugins();
        let hooks = self.hooks.chpwd.clone();
        let context = [
            ("WINUXSH_OLDPWD", old_pwd.to_string()),
            ("WINUXSH_PWD", new_pwd.to_string()),
        ];
        if self.uses_primary_startup_rc() {
            self.run_framework_hook_runner("winuxsh_run_chpwd_hooks", &context);
        } else {
            self.run_source_plugin_scripts_for_hook("chpwd", &context);
        }
        self.run_process_plugin_hooks("chpwd", &context);
        self.run_hook_scripts(&hooks, &context);
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

    fn sync_gitstatus_prompt_env(&mut self) {
        let Some(cwd) = self
            .executor_pwd_host_path()
            .or_else(|| std::env::current_dir().ok())
        else {
            self.clear_gitstatus_prompt_env("none");
            return;
        };
        let snapshot = crate::git_status::snapshot_for_prompt(&cwd);
        self.executor
            .set_env("WINUXSH_GITSTATUS_STATE", snapshot.state().as_str());
        let Some(status) = snapshot.status().cloned() else {
            self.clear_gitstatus_prompt_env(snapshot.state().as_str());
            return;
        };

        self.executor.set_env(
            "WINUXSH_GITSTATUS_BRANCH",
            status.branch.as_deref().unwrap_or_default(),
        );
        self.executor.set_env(
            "WINUXSH_GITSTATUS_DIRTY",
            if status.dirty { "1" } else { "0" },
        );
        self.executor
            .set_env("WINUXSH_GITSTATUS_STAGED", &status.staged.to_string());
        self.executor
            .set_env("WINUXSH_GITSTATUS_UNSTAGED", &status.unstaged.to_string());
        self.executor
            .set_env("WINUXSH_GITSTATUS_UNTRACKED", &status.untracked.to_string());
        self.executor
            .set_env("WINUXSH_GITSTATUS_DELETED", &status.deleted.to_string());
        self.executor
            .set_env("WINUXSH_GITSTATUS_AHEAD", &status.ahead.to_string());
        self.executor
            .set_env("WINUXSH_GITSTATUS_BEHIND", &status.behind.to_string());
        self.executor
            .set_env("WINUXSH_GITSTATUS_STASHES", &status.stashes.to_string());
        self.executor
            .set_env("WINUXSH_GITSTATUS_CONFLICTS", &status.conflicts.to_string());
        self.executor.set_env(
            "WINUXSH_PROMPT_GIT",
            &self.render_git_prompt_snapshot(&status),
        );
        self.executor.set_env("WINUXSH_PROMPT_GIT_SOURCE", "native");
        self.executor.set_env("WINUXSH_PROMPT_GIT_DIRTY", "0");
    }

    fn clear_gitstatus_prompt_env(&mut self, state: &str) {
        self.executor.set_env("WINUXSH_GITSTATUS_STATE", state);
        for name in [
            "WINUXSH_GITSTATUS_BRANCH",
            "WINUXSH_GITSTATUS_DIRTY",
            "WINUXSH_GITSTATUS_STAGED",
            "WINUXSH_GITSTATUS_UNSTAGED",
            "WINUXSH_GITSTATUS_UNTRACKED",
            "WINUXSH_GITSTATUS_DELETED",
            "WINUXSH_GITSTATUS_AHEAD",
            "WINUXSH_GITSTATUS_BEHIND",
            "WINUXSH_GITSTATUS_STASHES",
            "WINUXSH_GITSTATUS_CONFLICTS",
            "WINUXSH_PROMPT_GIT",
            "WINUXSH_PROMPT_GIT_SOURCE",
        ] {
            self.executor.unset_env(name);
        }
        self.executor.set_env("WINUXSH_PROMPT_GIT_DIRTY", "0");
    }

    fn render_git_prompt_snapshot(&self, status: &crate::git_status::GitRepoStatus) -> String {
        let Some(branch) = status.branch.as_deref().filter(|branch| !branch.is_empty()) else {
            return String::new();
        };
        let prefix = self
            .executor
            .get_env("WINUXSH_THEME_GIT_PROMPT_PREFIX")
            .unwrap_or("git:(");
        let suffix = self
            .executor
            .get_env("WINUXSH_THEME_GIT_PROMPT_SUFFIX")
            .unwrap_or(")");
        let dirty = self
            .executor
            .get_env("WINUXSH_THEME_GIT_PROMPT_DIRTY")
            .unwrap_or(" *");
        let clean = self
            .executor
            .get_env("WINUXSH_THEME_GIT_PROMPT_CLEAN")
            .unwrap_or("");
        let compact = status.compact_status_with(&self.plugin_prompt_sync.git_prompt_symbols);
        let mut body = branch.to_string();
        if !compact.is_empty() {
            body.push(' ');
            body.push_str(&compact);
        }
        body.push_str(if status.dirty { dirty } else { clean });
        format!("{prefix}{body}{suffix}")
    }

    fn mark_gitstatus_dirty_current_dir(&self) {
        if let Some(cwd) = self
            .executor_pwd_host_path()
            .or_else(|| std::env::current_dir().ok())
        {
            crate::git_status::mark_dirty(&cwd);
        }
    }

    fn native_plugin_enabled(&self, preset: &str) -> bool {
        if self.plugins.has_decision(preset) {
            return self.plugins.is_enabled(preset);
        }

        self.native_plugins.enabled
            && self
                .native_plugins
                .presets
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(preset))
    }

    fn execute_process_plugin_simple_ast(&mut self, ast: &Ast) -> anyhow::Result<Option<i32>> {
        if ast.commands.len() != 1 {
            return Ok(None);
        }
        let command = &ast.commands[0];
        if command.words.is_empty()
            || !command.assignments.is_empty()
            || !command.compound_assignments.is_empty()
            || !command.array_element_assignments.is_empty()
            || !command.process_substitutions.is_empty()
            || !command.command_substitutions.is_empty()
            || !command.arithmetic_expansions.is_empty()
            || !command.parameter_expansions.is_empty()
            || !command.brace_expansions.is_empty()
            || !command.extglob_patterns.is_empty()
            || !command.pathname_patterns.is_empty()
            || !command.redirects.is_empty()
            || command.redirect_in.is_some()
            || command.redirect_out.is_some()
            || command.append.is_some()
            || command.redirect_err.is_some()
            || command.redirect_err_append.is_some()
            || command.heredoc.is_some()
            || command.here_string.is_some()
            || command.pipe.is_some()
            || command.background
            || command.and_or.is_some()
            || command.inverted
            || command.pipeline_command.is_some()
            || command.and_or_list.is_some()
            || command.time_command.is_some()
            || command.background_command.is_some()
            || command.inverted_command.is_some()
            || command.subshell
            || command.subshell_end
            || command.for_command.is_some()
            || command.arithmetic_command.is_some()
            || command.if_command.is_some()
            || command.loop_command.is_some()
            || command.conditional_command.is_some()
            || command.subshell_command.is_some()
            || command.case_command.is_some()
            || command.select_command.is_some()
            || command.function_command.is_some()
            || command.brace_group.is_some()
            || command.coproc_command.is_some()
        {
            return Ok(None);
        }

        let command_name = &command.words[0];
        let Some((pack_name, process)) = self.process_plugin_for_command(command_name) else {
            return Ok(None);
        };
        let code = self.run_process_plugin_command(&pack_name, &process, &command.words[1..])?;
        Ok(Some(code))
    }

    fn process_plugin_for_command(
        &self,
        command_name: &str,
    ) -> Option<(String, PluginProcessSpec)> {
        crate::plugins::active_plugin_inventory()
            .packs
            .into_iter()
            .find_map(|pack| {
                if pack.kind != PluginKind::Process || !self.plugins.is_enabled(&pack.name) {
                    return None;
                }
                if !pack
                    .exports
                    .commands
                    .iter()
                    .any(|exported| exported == command_name)
                {
                    return None;
                }
                pack.process.map(|process| (pack.name, process))
            })
    }

    fn process_plugin_for_provider(
        &self,
        provider_name: &str,
    ) -> Option<(String, PluginProcessSpec, Vec<String>)> {
        process_plugin_for_provider_from_state(provider_name, &self.plugins)
    }

    fn process_plugins_for_hook(&self, hook_name: &str) -> Vec<(String, PluginProcessSpec)> {
        crate::plugins::active_plugin_inventory()
            .packs
            .into_iter()
            .filter_map(|pack| {
                if pack.kind != PluginKind::Process || !self.plugins.is_enabled(&pack.name) {
                    return None;
                }
                if !pack
                    .exports
                    .hooks
                    .iter()
                    .any(|exported| exported == hook_name)
                {
                    return None;
                }
                pack.process.map(|process| (pack.name, process))
            })
            .collect()
    }

    fn run_process_plugin_hooks(&mut self, hook_name: &str, context: &[(&str, String)]) {
        let hook_args = vec!["--hook".to_string(), hook_name.to_string()];
        for (pack_name, process) in self.process_plugins_for_hook(hook_name) {
            match self.run_process_plugin_invocation(
                &pack_name,
                &process,
                &hook_args,
                Some(hook_name),
                context,
            ) {
                Ok(0) => {}
                Ok(code) => log::warn!(
                    "process hook '{}' from plugin '{}' exited with status {}",
                    hook_name,
                    pack_name,
                    code
                ),
                Err(err) => log::warn!(
                    "process hook '{}' from plugin '{}' failed: {}",
                    hook_name,
                    pack_name,
                    err
                ),
            }
        }
    }

    fn run_process_plugin_command(
        &mut self,
        pack_name: &str,
        process: &PluginProcessSpec,
        user_args: &[String],
    ) -> anyhow::Result<i32> {
        self.run_process_plugin_invocation(pack_name, process, user_args, None, &[])
    }

    fn run_process_plugin_invocation(
        &mut self,
        pack_name: &str,
        process: &PluginProcessSpec,
        extra_args: &[String],
        hook_name: Option<&str>,
        context: &[(&str, String)],
    ) -> anyhow::Result<i32> {
        let output = self.run_process_plugin_invocation_capture(
            pack_name, process, extra_args, hook_name, context, true,
        )?;
        if !output.stdout.is_empty() {
            let _ = std::io::stdout().write_all(&output.stdout);
        }
        if !output.stderr.is_empty() {
            let _ = std::io::stderr().write_all(&output.stderr);
        }
        Ok(output.status)
    }

    fn run_process_plugin_invocation_capture(
        &mut self,
        pack_name: &str,
        process: &PluginProcessSpec,
        extra_args: &[String],
        hook_name: Option<&str>,
        context: &[(&str, String)],
        report_errors: bool,
    ) -> anyhow::Result<ProcessPluginInvocationOutput> {
        self.sync_process_cwd_from_executor_pwd();
        self.sync_process_path_from_executor_path();
        let env = std::env::vars().collect::<HashMap<_, _>>();
        run_process_plugin_invocation_capture_with_env(
            pack_name,
            process,
            extra_args,
            hook_name,
            context,
            report_errors,
            &env,
        )
    }
    fn native_selector_enabled(&self) -> bool {
        self.native_plugin_enabled("fzf")
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
        let dotenv_path = self.executor.resolve_shell_path(&pwd).join(".env");
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

        let host_pwd = self.executor.resolve_shell_path(&pwd);
        let host_pwd_display = host_pwd.to_string_lossy().replace('\\', "/");
        let command_path =
            resolve_native_command_path("zoxide").unwrap_or_else(|| PathBuf::from("zoxide"));
        let status = Command::new(command_path)
            .arg("add")
            .arg(&host_pwd_display)
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
        let Some(correction) = correction
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
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
        let host_base =
            resolve_shell_path_argument_with_env(&pwd, base, &self.executor.env_vars_snapshot());
        let candidates = directory_selector_candidates(&host_base);
        if candidates.is_empty() {
            return Ok(1);
        }

        let Some(selected) = run_native_fzf_selector(&candidates) else {
            return Ok(1);
        };
        let selected = host_path_to_shell_path_with_root(&selected, self.shell_root.as_deref());
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
            log::debug!(
                "native last-working-dir preset could not create cache dir: {}",
                err
            );
            return;
        }
        if let Err(err) = std::fs::write(&self.last_working_dir_cache_path, format!("{pwd}\n")) {
            log::debug!(
                "native last-working-dir preset could not write cache: {}",
                err
            );
        }
    }

    fn command_not_found_plugin_enabled(&self) -> bool {
        self.native_plugin_enabled(COMMAND_NOT_FOUND_PROVIDER_NAME)
            || self
                .process_plugin_for_provider(COMMAND_NOT_FOUND_PROVIDER_NAME)
                .is_some()
    }

    fn print_native_command_not_found(&mut self, command: &str, args: &[String]) {
        let provider_output = self.command_not_found_provider_output(command, args);
        for line in command_not_found_lines_with_provider(
            command,
            true,
            |candidate| resolve_native_command_path(candidate).is_some(),
            provider_output,
        ) {
            eprintln!("{}", line);
        }
    }

    fn command_not_found_provider_output(
        &mut self,
        command: &str,
        args: &[String],
    ) -> CommandNotFoundProviderOutput {
        let Some((pack_name, process, permissions)) =
            self.process_plugin_for_provider(COMMAND_NOT_FOUND_PROVIDER_NAME)
        else {
            return CommandNotFoundProviderOutput::Empty;
        };
        let cwd = if permissions
            .iter()
            .any(|permission| permission == "cwd:read")
        {
            self.executor.get_env("PWD").map(str::to_string)
        } else {
            None
        };
        let request =
            command_not_found_provider_request(command, args, cwd.as_deref(), |candidate| {
                resolve_native_command_path(candidate).is_some()
            });
        let mut provider_args = vec![
            "--provider".to_string(),
            COMMAND_NOT_FOUND_PROVIDER_NAME.to_string(),
            "--command".to_string(),
            request.command.clone(),
        ];
        for arg in &request.args {
            provider_args.push("--arg".to_string());
            provider_args.push(arg.clone());
        }
        if let Some(cwd) = &request.cwd {
            provider_args.push("--cwd".to_string());
            provider_args.push(cwd.clone());
        }
        for helper in &request.package_search_helpers {
            provider_args.push("--helper".to_string());
            provider_args.push(helper.clone());
        }
        let helper_list = request.package_search_helpers.join(";");
        let context = vec![
            (
                "WINUXSH_PROCESS_PLUGIN_PROVIDER",
                COMMAND_NOT_FOUND_PROVIDER_NAME.to_string(),
            ),
            ("WINUXSH_COMMAND_NOT_FOUND_COMMAND", request.command.clone()),
            ("WINUXSH_COMMAND_NOT_FOUND_HELPERS", helper_list),
        ];
        let output = match self.run_process_plugin_invocation_capture(
            &pack_name,
            &process,
            &provider_args,
            None,
            &context,
            false,
        ) {
            Ok(output) => output,
            Err(err) => {
                log::debug!(
                    "command-not-found provider '{}' invocation failed: {}",
                    pack_name,
                    err
                );
                return CommandNotFoundProviderOutput::Failed(err.to_string());
            }
        };
        if !output.stderr.is_empty() {
            log::debug!(
                "command-not-found provider '{}' wrote stderr: {}",
                pack_name,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if output.status != 0 {
            return CommandNotFoundProviderOutput::Failed(format!(
                "provider process exited with {}",
                output.status
            ));
        }
        parse_command_not_found_provider_output(&output.stdout)
    }

    fn print_command_not_found_hints_if_missing(&self, ast: &Ast) {
        let Some(command) = single_command_word(ast) else {
            return;
        };
        if resolve_native_command_path(command).is_some() {
            return;
        }

        self.print_command_not_found_hints(command);
    }

    fn print_command_not_found_hints(&self, command: &str) {
        for line in native_command_not_found_hint_lines(
            command,
            self.native_plugin_enabled("command-not-found"),
            |candidate| resolve_native_command_path(candidate).is_some(),
        ) {
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
                    Some(format!(
                        "winuxsh: alias available: {}={}",
                        name,
                        shell_quote(value)
                    ))
                } else {
                    None
                }
            })
            .collect();
        matches.sort();
        matches
    }

    fn sync_alias_mirror_from_executor(&mut self) {
        self.aliases = self.executor.aliases_snapshot();
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
        normalize_bare_windows_drive_commands(&mut ast);
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

    fn sync_alias_mirror_from_script(&mut self, script: &str, code: i32) {
        if code != 0 {
            return;
        }
        for line in script.lines() {
            self.sync_alias_mirror_from_line(line, code);
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
            self.aliases.insert(
                name.to_string(),
                strip_rubash_alias_quote_marker(value).to_string(),
            );
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
            state.env_vars = self.executor.env_vars_snapshot();
            state.aliases = self.aliases.keys().cloned().collect();
            state.functions = self.executor.functions_snapshot().into_iter().collect();
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
        let host_path = self.executor.resolve_shell_path(pwd);
        host_path.is_dir().then_some(host_path)
    }

    fn sync_executor_pwd_from_process_cwd(&mut self) {
        let Ok(cwd) = std::env::current_dir() else {
            return;
        };
        let normalized_pwd =
            host_path_to_shell_path_with_root(&cwd.to_string_lossy(), self.shell_root.as_deref());
        self.executor.set_env("PWD", &normalized_pwd);
    }

    fn sync_process_path_from_executor_path(&mut self) {
        let Some(path) = self.executor.get_env("PATH") else {
            return;
        };
        let env = self.executor.env_vars_snapshot();
        let process_path = process_path_from_shell_path_list(path, Some(&env));
        std::env::set_var("PATH", &process_path);
        if cfg!(windows) && process_path != path {
            self.executor.set_env("PATH", &process_path);
        }
    }

    fn sync_process_cwd_from_executor_pwd(&mut self) {
        let pwd = match self.executor.get_env("PWD") {
            Some(p) => p.to_string(),
            None => {
                let cwd = std::env::current_dir().unwrap_or_default();
                let pwd = host_path_to_shell_path_with_root(
                    &cwd.to_string_lossy(),
                    self.shell_root.as_deref(),
                );
                self.executor.set_env("PWD", &pwd);
                return;
            }
        };
        let host_pwd = self.executor.resolve_shell_path(&pwd);
        let target = host_pwd.clone();
        if !target.is_dir() {
            let cwd = std::env::current_dir().unwrap_or_default();
            let pwd = host_path_to_shell_path_with_root(
                &cwd.to_string_lossy(),
                self.shell_root.as_deref(),
            );
            self.executor.set_env("PWD", &pwd);
            return;
        }
        if std::env::set_current_dir(&target).is_err() {
            let cwd = std::env::current_dir().unwrap_or_default();
            let pwd = host_path_to_shell_path_with_root(
                &cwd.to_string_lossy(),
                self.shell_root.as_deref(),
            );
            self.executor.set_env("PWD", &pwd);
            return;
        }
        let normalized_pwd = host_path_to_shell_path_with_root(
            &host_pwd.to_string_lossy(),
            self.shell_root.as_deref(),
        );
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

    /// Run shell process teardown semantics that live in rubash's binary entry.
    pub fn finish_with_exit_trap(&mut self, status: i32) -> anyhow::Result<i32> {
        match self.executor.run_exit_trap_with_status(status) {
            Ok(code) => Ok(code),
            Err(rubash::executor::ExecuteError::ExitCode(code)) => Ok(code),
            Err(e) => {
                if !is_broken_pipe_execute_error(&e) {
                    eprintln!("winuxsh: {}", e);
                }
                Ok(1)
            }
        }
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
        rewrite_winuxcmd_command_shims(&mut tokens, interactive_terminal_colors);

        let mut ast = parse(&tokens);
        normalize_bare_windows_drive_commands(&mut ast);
        normalize_cd_windows_drive_args(&mut ast);
        normalize_winuxcmd_slash_drive_args(&mut ast);
        self.inject_process_stdin_for_rewritten_pipeline(&mut ast)?;

        let execution = if let Some(code) = self.execute_process_plugin_simple_ast(&ast)? {
            Ok(code)
        } else {
            self.execute_host_synced_simple_ast(&ast)
                .unwrap_or_else(|| match self.executor.execute_ast(&ast) {
                    Ok(()) => Ok(self.executor.last_exit_code()),
                    Err(err) => Err(err),
                })
        };

        let code = match execution {
            Ok(code) => code,
            Err(rubash::executor::ExecuteError::ExitCode(code)) => code,
            Err(rubash::executor::ExecuteError::Return(code)) => code,
            Err(rubash::executor::ExecuteError::CommandNotFound(cmd)) => {
                if self.command_not_found_plugin_enabled() {
                    let args = command_not_found_args(&ast, &cmd);
                    self.print_native_command_not_found(&cmd, &args);
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
        };

        self.sync_process_cwd_from_executor_pwd();
        self.sync_process_path_from_executor_path();
        self.sync_alias_mirror_from_executor();
        Ok(code)
    }

    fn inject_process_stdin_for_rewritten_pipeline(&mut self, ast: &mut Ast) -> anyhow::Result<()> {
        if !self.process_stdin_pipeline_bridge
            || self.executor.get_env("__RUBASH_INHERIT_PROCESS_STDIN") != Some("1")
        {
            return Ok(());
        }

        let Some(stage) = process_stdin_pipeline_bridge_stage(ast) else {
            return Ok(());
        };

        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        if input.is_empty() {
            return Ok(());
        }

        stage.heredoc = Some(format!("\x1e{input}"));
        stage.heredoc_delimiter = Some("WINUXSH_PROCESS_STDIN".to_string());
        Ok(())
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
                    self.sync_process_path_from_executor_path();
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
        normalize_cd_windows_drive_command(command);
    }
}

fn normalize_cd_windows_drive_command(command: &mut rubash::parser::CommandNode) {
    if let Some(and_or_list) = &mut command.and_or_list {
        for command in &mut and_or_list.commands {
            normalize_cd_windows_drive_command(command);
        }
    }

    if !command
        .words
        .first()
        .is_some_and(|word| word.eq_ignore_ascii_case("cd"))
    {
        return;
    }

    for word in command.words.iter_mut().skip(1) {
        if let Some(normalized) =
            cd_tilde_path_to_slash_drive(word).or_else(|| windows_drive_path_to_slash_drive(word))
        {
            *word = normalized;
        }
    }
}

fn normalize_bare_windows_drive_commands(ast: &mut Ast) {
    if !cfg!(windows) {
        return;
    }

    for command in &mut ast.commands {
        normalize_bare_windows_drive_command(command);
    }
}

fn normalize_bare_windows_drive_command(command: &mut rubash::parser::CommandNode) {
    if let Some(and_or_list) = &mut command.and_or_list {
        for command in &mut and_or_list.commands {
            normalize_bare_windows_drive_command(command);
        }
    }

    if !is_bare_windows_drive_command_shape(command)
        || command_has_redirects(command)
        || !command.assignments.is_empty()
        || !command.compound_assignments.is_empty()
        || !command.array_element_assignments.is_empty()
    {
        return;
    }

    let Some(drive_root) = bare_windows_drive_command_root(command) else {
        return;
    };

    command.words = vec!["cd".to_string(), drive_root];
    command.word_kinds = vec![TokenKind::Word, TokenKind::Word];
    command.word_metadata = command
        .words
        .iter()
        .enumerate()
        .map(|(index, word)| {
            rubash::parser::WordMetadata::literal(index, word.clone(), word.clone())
        })
        .collect();
}

fn is_bare_windows_drive_command_shape(command: &rubash::parser::CommandNode) -> bool {
    command.pipe.is_none()
        && !command.background
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
}

fn bare_windows_drive_command_root(command: &rubash::parser::CommandNode) -> Option<String> {
    let [word] = command.words.as_slice() else {
        return None;
    };
    if command
        .word_metadata
        .first()
        .is_some_and(|metadata| !metadata.word_quotes.is_empty() || metadata.raw.as_str() != word)
    {
        return None;
    }

    let bytes = word.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        Some(format!("{}:/", (bytes[0] as char).to_ascii_uppercase()))
    } else {
        None
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
    is_plain_simple_command(command)
        && !command
            .words
            .first()
            .is_some_and(|word| word == "set" || word == "trap")
}

fn is_plain_simple_command(command: &rubash::parser::CommandNode) -> bool {
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
}

fn command_has_redirects(command: &rubash::parser::CommandNode) -> bool {
    !command.redirects.is_empty()
        || command.redirect_in.is_some()
        || command.redirect_out.is_some()
        || command.append.is_some()
        || command.redirect_err.is_some()
        || command.redirect_err_append.is_some()
        || command.heredoc.is_some()
        || command.heredoc_delimiter.is_some()
        || !command.heredoc_redirects.is_empty()
        || command.here_string.is_some()
}

fn process_stdin_pipeline_bridge_stage(ast: &mut Ast) -> Option<&mut rubash::parser::CommandNode> {
    if ast.commands.len() != 1 {
        return None;
    }

    let pipeline = ast.commands[0].pipeline_command.as_mut()?;
    let first = pipeline.stages.first_mut()?;
    if command_has_redirects(first) {
        return None;
    }

    let command_name = first.words.first()?;
    let command_name = command_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command_name);
    matches!(
        command_name.to_ascii_lowercase().as_str(),
        "awk.exe"
            | "cat.exe"
            | "grep.exe"
            | "head.exe"
            | "sed.exe"
            | "sort.exe"
            | "tail.exe"
            | "tr.exe"
            | "uniq.exe"
            | "wc.exe"
    )
    .then_some(first)
}

fn execute_winuxsh_host_external_command(
    words: &[String],
    env: &HashMap<String, String>,
    plugins: &PluginRuntimeState,
) -> Option<HostExternalCommandOutput> {
    let [command, args @ ..] = words else {
        return None;
    };

    if resolve_native_command_path_with_env(command, env).is_some() {
        return None;
    }

    command_not_found_host_external_output(command, args, env, plugins)
}

fn command_not_found_host_external_output(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    plugins: &PluginRuntimeState,
) -> Option<HostExternalCommandOutput> {
    let provider_output =
        command_not_found_process_provider_output_for_env(command, args, env, plugins);
    if provider_output.is_none() && !plugins.is_enabled(COMMAND_NOT_FOUND_PROVIDER_NAME) {
        return None;
    }
    let provider_output = provider_output.unwrap_or(CommandNotFoundProviderOutput::Empty);
    let stderr = command_not_found_lines_with_provider(
        command,
        true,
        |candidate| resolve_native_command_path_with_env(candidate, env).is_some(),
        provider_output,
    )
    .join("\n")
        + "\n";
    Some(HostExternalCommandOutput {
        stdout: Vec::new(),
        stderr: stderr.into_bytes(),
        status: 127,
    })
}

fn command_not_found_process_provider_output_for_env(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    plugins: &PluginRuntimeState,
) -> Option<CommandNotFoundProviderOutput> {
    let (pack_name, process, permissions) =
        process_plugin_for_provider_from_state(COMMAND_NOT_FOUND_PROVIDER_NAME, plugins)?;
    let cwd = if permissions
        .iter()
        .any(|permission| permission == "cwd:read")
    {
        env.get("PWD").cloned()
    } else {
        None
    };
    let request = command_not_found_provider_request(command, args, cwd.as_deref(), |candidate| {
        resolve_native_command_path_with_env(candidate, env).is_some()
    });
    let mut provider_args = vec![
        "--provider".to_string(),
        COMMAND_NOT_FOUND_PROVIDER_NAME.to_string(),
        "--command".to_string(),
        request.command.clone(),
    ];
    for arg in &request.args {
        provider_args.push("--arg".to_string());
        provider_args.push(arg.clone());
    }
    if let Some(cwd) = &request.cwd {
        provider_args.push("--cwd".to_string());
        provider_args.push(cwd.clone());
    }
    for helper in &request.package_search_helpers {
        provider_args.push("--helper".to_string());
        provider_args.push(helper.clone());
    }
    let helper_list = request.package_search_helpers.join(";");
    let context = vec![
        (
            "WINUXSH_PROCESS_PLUGIN_PROVIDER",
            COMMAND_NOT_FOUND_PROVIDER_NAME.to_string(),
        ),
        ("WINUXSH_COMMAND_NOT_FOUND_COMMAND", request.command.clone()),
        ("WINUXSH_COMMAND_NOT_FOUND_HELPERS", helper_list),
    ];
    let output = match run_process_plugin_invocation_capture_with_env(
        &pack_name,
        &process,
        &provider_args,
        None,
        &context,
        false,
        env,
    ) {
        Ok(output) => output,
        Err(err) => {
            log::debug!(
                "command-not-found provider '{}' invocation failed: {}",
                pack_name,
                err
            );
            return Some(CommandNotFoundProviderOutput::Failed(err.to_string()));
        }
    };
    if !output.stderr.is_empty() {
        log::debug!(
            "command-not-found provider '{}' wrote stderr: {}",
            pack_name,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if output.status != 0 {
        return Some(CommandNotFoundProviderOutput::Failed(format!(
            "provider process exited with {}",
            output.status
        )));
    }
    Some(parse_command_not_found_provider_output(&output.stdout))
}

#[cfg(test)]
fn winuxsh_builtin_words(command: &rubash::parser::CommandNode) -> Option<(&str, &[String])> {
    let _ = command;
    None
}

fn is_cd_command(command: &rubash::parser::CommandNode) -> bool {
    command
        .words
        .first()
        .is_some_and(|word| word.eq_ignore_ascii_case("cd"))
}

fn rewrite_winuxcmd_command_shims(tokens: &mut Vec<Token>, interactive_terminal_colors: bool) {
    if !cfg!(windows) {
        return;
    }

    let mut command_start = 0;
    while command_start < tokens.len() {
        let command_end = find_command_separator(tokens, command_start).unwrap_or(tokens.len());
        let separator = tokens.get(command_end).map(|token| &token.kind);
        let terminal_output = !matches!(separator, Some(TokenKind::Background));
        rewrite_winuxcmd_command_shims_in_command(
            tokens,
            command_start,
            command_end,
            interactive_terminal_colors && terminal_output,
        );
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

fn rewrite_winuxcmd_command_shims_in_command(
    tokens: &mut Vec<Token>,
    start: usize,
    end: usize,
    interactive_terminal_colors: bool,
) {
    let mut stage_start = start;
    while stage_start < end {
        let stage_end = find_pipeline_separator(tokens, stage_start, end).unwrap_or(end);
        let add_terminal_grep_color = interactive_terminal_colors
            && stage_end == end
            && stage_outputs_to_terminal(tokens, stage_start, stage_end);
        rewrite_winuxcmd_command_shims_in_stage(
            tokens,
            stage_start,
            stage_end,
            add_terminal_grep_color,
        );
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

fn rewrite_winuxcmd_command_shims_in_stage(
    tokens: &mut Vec<Token>,
    start: usize,
    end: usize,
    add_terminal_grep_color: bool,
) {
    let Some(command_index) = simple_command_word_index(tokens, start, end) else {
        return;
    };

    // Rubash keeps quoted glob-like characters behind an internal marker until
    // execution. Winuxsh rewrites the AST before Rubash executes it, so restore
    // those literals at this external-command boundary.
    for token in &mut tokens[start..end] {
        token.value = token.value.replace('\x11', "");
        token.raw = token.raw.replace('\x11', "");
    }

    match winuxcmd_command_shim(&tokens[command_index]) {
        Some(WinuxCmdShim::Exe { target }) => {
            tokens[command_index].value = target.to_string();
            tokens[command_index].raw = target.to_string();
        }
        None => return,
    }

    if add_terminal_grep_color
        && grep_command_name(&tokens[command_index])
        && !grep_stage_has_color_option(tokens, command_index + 1, end)
    {
        tokens.insert(
            command_index + 1,
            Token::new(
                TokenKind::Word,
                "--color=always",
                tokens[command_index].position,
            ),
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

enum WinuxCmdShim {
    Exe { target: &'static str },
}

fn winuxcmd_command_shim(token: &Token) -> Option<WinuxCmdShim> {
    if !matches!(token.kind, TokenKind::Word) {
        return None;
    }

    for (name, target) in WINUXCMD_EXE_SHIMS {
        if token.value.eq_ignore_ascii_case(name) && token.raw.eq_ignore_ascii_case(name) {
            return Some(WinuxCmdShim::Exe { target });
        }
    }

    if token.value.eq_ignore_ascii_case("grep.exe") && token.raw.eq_ignore_ascii_case("grep.exe") {
        return Some(WinuxCmdShim::Exe { target: "grep.exe" });
    }
    None
}

const WINUXCMD_EXE_SHIMS: &[(&str, &str)] = &[("grep", "grep.exe")];

fn grep_command_name(token: &Token) -> bool {
    token.value.eq_ignore_ascii_case("grep") || token.value.eq_ignore_ascii_case("grep.exe")
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
        normalize_winuxcmd_slash_drive_command(command);
    }
}

fn normalize_winuxcmd_slash_drive_command(command: &mut rubash::parser::CommandNode) {
    if let Some(and_or_list) = &mut command.and_or_list {
        for command in &mut and_or_list.commands {
            normalize_winuxcmd_slash_drive_command(command);
        }
    }

    let Some(command_name) = command.words.first() else {
        return;
    };
    if !is_winuxcmd_path_command(command_name) {
        return;
    }

    for word in command.words.iter_mut().skip(1) {
        if let Some(normalized) = slash_drive_arg_to_windows_native(word) {
            *word = normalized;
        }
    }
}

fn is_winuxcmd_path_command(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    let command = command.strip_suffix(".exe").unwrap_or(&command);
    WINUXCMD_PATH_COMMANDS.contains(&command)
}

const WINUXCMD_PATH_COMMANDS: &[&str] = &[
    "awk",
    "b2sum",
    "base32",
    "base64",
    "basename",
    "basenc",
    "cat",
    "chcon",
    "chgrp",
    "chmod",
    "chown",
    "chroot",
    "cksum",
    "cmp",
    "col",
    "column",
    "comm",
    "cp",
    "cpio",
    "csplit",
    "cut",
    "cygpath",
    "d2u",
    "dd",
    "df",
    "diff",
    "diff3",
    "dir",
    "dirname",
    "dos2unix",
    "du",
    "expand",
    "fd",
    "file",
    "find",
    "fmt",
    "fold",
    "grep",
    "head",
    "hexdump",
    "hmac256",
    "install",
    "join",
    "jq",
    "less",
    "link",
    "ln",
    "lsof",
    "ls",
    "md5sum",
    "mkdir",
    "mkfifo",
    "mknod",
    "mktemp",
    "more",
    "mv",
    "nl",
    "od",
    "paste",
    "patch",
    "pathchk",
    "pr",
    "ptx",
    "readlink",
    "realpath",
    "rev",
    "rm",
    "rmdir",
    "sdiff",
    "sed",
    "sha1sum",
    "sha224sum",
    "sha256sum",
    "sha384sum",
    "sha512sum",
    "shred",
    "shuf",
    "sort",
    "split",
    "stat",
    "strings",
    "sum",
    "tac",
    "tail",
    "tar",
    "tee",
    "tic",
    "toe",
    "touch",
    "tree",
    "truncate",
    "tsort",
    "u2d",
    "unexpand",
    "uniq",
    "unix2dos",
    "unlink",
    "vdir",
    "wc",
    "xxd",
];

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
        return Some(format!("/{drive}/"));
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
                    // Double the separator so Rubash's lexer returns one literal
                    // backslash instead of treating it as a shell escape.
                    output.push('\\');
                    output.push('\\');
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
    ch.is_ascii_whitespace()
        || matches!(
            ch,
            '=' | '(' | '[' | '{' | ',' | ';' | '|' | '&' | '<' | '>'
        )
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

fn single_command_word(ast: &Ast) -> Option<&str> {
    if ast.commands.len() != 1 {
        return None;
    }
    ast.commands[0].words.first().map(String::as_str)
}

fn command_not_found_args(ast: &Ast, command: &str) -> Vec<String> {
    if ast.commands.len() != 1 {
        return Vec::new();
    }
    let words = ast.commands[0].words.as_slice();
    match words {
        [first, args @ ..] if first == command => args.to_vec(),
        _ => Vec::new(),
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandNotFoundProviderRequest {
    command: String,
    args: Vec<String>,
    cwd: Option<String>,
    package_search_helpers: Vec<String>,
}
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum CommandNotFoundProviderOutput {
    Suggestions(Vec<String>),
    Empty,
    Failed(String),
}
#[allow(dead_code)]
fn command_not_found_provider_request<F>(
    command: &str,
    args: &[String],
    cwd: Option<&str>,
    mut command_exists: F,
) -> CommandNotFoundProviderRequest
where
    F: FnMut(&str) -> bool,
{
    let package_search_helpers = ["winget", "scoop", "choco"]
        .into_iter()
        .filter(|candidate| command_exists(candidate))
        .map(str::to_string)
        .collect();
    CommandNotFoundProviderRequest {
        command: command.to_string(),
        args: args.to_vec(),
        cwd: cwd.map(str::to_string),
        package_search_helpers,
    }
}
#[allow(dead_code)]
fn parse_command_not_found_provider_output(bytes: &[u8]) -> CommandNotFoundProviderOutput {
    if bytes.len() > COMMAND_NOT_FOUND_PROVIDER_MAX_OUTPUT_BYTES {
        return CommandNotFoundProviderOutput::Failed(format!(
            "provider output exceeded {} bytes",
            COMMAND_NOT_FOUND_PROVIDER_MAX_OUTPUT_BYTES
        ));
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return CommandNotFoundProviderOutput::Failed("provider output was not UTF-8".to_string());
    };
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches(char::from(13));
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > COMMAND_NOT_FOUND_PROVIDER_MAX_LINE_BYTES {
            return CommandNotFoundProviderOutput::Failed(format!(
                "provider output line exceeded {} bytes",
                COMMAND_NOT_FOUND_PROVIDER_MAX_LINE_BYTES
            ));
        }
        lines.push(line.to_string());
        if lines.len() > COMMAND_NOT_FOUND_PROVIDER_MAX_LINES {
            return CommandNotFoundProviderOutput::Failed(format!(
                "provider output exceeded {} lines",
                COMMAND_NOT_FOUND_PROVIDER_MAX_LINES
            ));
        }
    }
    if lines.is_empty() {
        CommandNotFoundProviderOutput::Empty
    } else {
        CommandNotFoundProviderOutput::Suggestions(lines)
    }
}
#[allow(dead_code)]
fn native_command_not_found_lines<F>(
    command: &str,
    include_package_search: bool,
    mut command_exists: F,
) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    command_not_found_lines_with_provider(
        command,
        include_package_search,
        &mut command_exists,
        CommandNotFoundProviderOutput::Empty,
    )
}
fn command_not_found_lines_with_provider<F>(
    command: &str,
    include_package_search: bool,
    mut command_exists: F,
    provider_output: CommandNotFoundProviderOutput,
) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    let mut lines = vec![format!("winuxsh: {}: command not found", command)];
    if let CommandNotFoundProviderOutput::Suggestions(suggestions) = provider_output {
        if !suggestions.is_empty() {
            lines.extend(suggestions);
            return lines;
        }
    }
    lines.extend(native_command_not_found_hint_lines(
        command,
        include_package_search,
        &mut command_exists,
    ));
    lines
}
fn native_command_not_found_hint_lines<F>(
    command: &str,
    include_package_search: bool,
    mut command_exists: F,
) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    let mut lines = Vec::new();
    if !is_package_search_candidate(command) {
        return lines;
    }

    let search = shell_quote(command);
    if let Some(package) = wpm_package_for_command(command) {
        lines.push(format!(
            "winuxsh: try 'wpm install {}' to add {}",
            package, command
        ));
    }

    let mut hints = Vec::new();
    if include_package_search && command_exists("winget") {
        hints.push(format!("  winget search --name {}", search));
    }
    if include_package_search && command_exists("scoop") {
        hints.push(format!("  scoop search {}", search));
    }
    if include_package_search && command_exists("choco") {
        hints.push(format!("  choco search {}", search));
    }

    if !hints.is_empty() {
        lines.push("winuxsh: package search hints:".to_string());
        lines.extend(hints);
    }

    lines
}

fn wpm_package_for_command(command: &str) -> Option<&'static str> {
    match command {
        "awk" => Some("awk"),
        "gawk" => Some("gawk"),
        "jq" => Some("jq"),
        "yq" => Some("yq"),
        "ncat" => Some("ncat"),
        "7z" | "7zz" => Some("7zip"),
        "zstd" | "unzstd" | "zstdcat" => Some("zstd"),
        "rg" => Some("ripgrep"),
        "fd" => Some("fd"),
        "fzf" => Some("fzf"),
        "bat" => Some("bat"),
        "delta" => Some("delta"),
        "sd" => Some("sd"),
        "hyperfine" => Some("hyperfine"),
        "just" => Some("just"),
        "dust" => Some("dust"),
        "duf" => Some("duf"),
        "procs" => Some("procs"),
        "btm" => Some("bottom"),
        "wget" => Some("wget"),
        "aria2c" => Some("aria2"),
        "rclone" => Some("rclone"),
        "eza" => Some("eza"),
        "lsd" => Some("lsd"),
        "zoxide" => Some("zoxide"),
        "starship" => Some("starship"),
        "chezmoi" => Some("chezmoi"),
        "gh" => Some("gh"),
        "glab" => Some("glab"),
        "lazygit" => Some("lazygit"),
        "lazydocker" => Some("lazydocker"),
        "kubectl" => Some("kubectl"),
        "helm" => Some("helm"),
        "k9s" => Some("k9s"),
        "tofu" => Some("opentofu"),
        "sqlite3" => Some("sqlite"),
        "duckdb" => Some("duckdb"),
        "pandoc" => Some("pandoc"),
        "shellcheck" => Some("shellcheck"),
        "shfmt" => Some("shfmt"),
        "hadolint" => Some("hadolint"),
        "tokei" => Some("tokei"),
        "scc" => Some("scc"),
        "watchexec" => Some("watchexec"),
        "miniserve" => Some("miniserve"),
        "xh" => Some("xh"),
        "grpcurl" => Some("grpcurl"),
        "age" | "age-keygen" => Some("age"),
        "sops" => Some("sops"),
        "cosign" => Some("cosign"),
        "trivy" => Some("trivy"),
        "syft" => Some("syft"),
        "grype" => Some("grype"),
        "oras" => Some("oras"),
        "crane" => Some("crane"),
        "restic" => Some("restic"),
        "yazi" => Some("yazi"),
        "ouch" => Some("ouch"),
        "erd" => Some("erdtree"),
        "micro" => Some("micro"),
        "hx" => Some("helix"),
        "busybox" => Some("busybox"),
        "ffmpeg" | "ffprobe" => Some("ffmpeg"),
        _ => None,
    }
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
            | "ZSH"
    )
}

fn normalize_executor_home_env(executor: &mut Executor, home_dir: &Path) {
    let home = host_path_to_shell_path(&home_dir.to_string_lossy());
    let current = executor.get_env("HOME").unwrap_or_default();
    let should_update = cfg!(windows)
        || current.trim().is_empty()
        || current.contains('\\')
        || (cfg!(windows) && is_slash_drive_path(current));
    if should_update && !home.is_empty() {
        executor.set_env("HOME", &home);
    }
}

fn ensure_windows_profile_env(executor: &mut Executor, home_dir: &Path) {
    if !cfg!(windows) {
        return;
    }

    let home = shell_path_to_host_path(&home_dir.to_string_lossy()).replace('/', "\\");
    if home.trim().is_empty() {
        return;
    }

    executor.set_env("USERPROFILE", &home);
    if let Some((drive, path)) = windows_drive_and_home_path(&home) {
        set_executor_env_if_missing_or_empty(executor, "HOMEDRIVE", &drive);
        set_executor_env_if_missing_or_empty(executor, "HOMEPATH", &path);
    }
    set_executor_env_if_missing_or_empty(
        executor,
        "APPDATA",
        &format!("{}\\AppData\\Roaming", home.trim_end_matches('\\')),
    );
    set_executor_env_if_missing_or_empty(
        executor,
        "LOCALAPPDATA",
        &format!("{}\\AppData\\Local", home.trim_end_matches('\\')),
    );
}

fn ensure_prompt_terminal_env(executor: &mut Executor) {
    set_executor_env_if_missing_or_empty(executor, "COLUMNS", "80");
}

fn set_executor_env_if_missing_or_empty(executor: &mut Executor, name: &str, value: &str) {
    if executor
        .get_env(name)
        .map_or(true, |current| current.trim().is_empty())
    {
        executor.export_env(name, value);
    }
}

fn windows_drive_and_home_path(path: &str) -> Option<(String, String)> {
    let bytes = path.as_bytes();
    if bytes.len() < 3 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let drive = path[..2].to_string();
    let rest = path[2..].trim_start_matches(['\\', '/']);
    Some((drive, format!("\\{}", rest.replace('/', "\\"))))
}

fn set_default_winuxsh_framework_env(executor: &mut Executor, home_dir: &Path) {
    let configured_app_bundle = executor
        .get_env("WINUXSH_APP_BUNDLE_PATH")
        .map(str::to_owned)
        .map(|value| PathBuf::from(shell_path_to_host_path(&value)))
        .filter(|path| is_winuxsh_framework_dir(path));
    let discovered_app_bundle = app_bundled_winuxsh_framework_dir();
    let app_bundle = configured_app_bundle.or(discovered_app_bundle.clone());

    if executor.get_env("WINUXSH_APP_BUNDLE_PATH").is_none() {
        if let Some(path) = discovered_app_bundle {
            executor.set_env(
                "WINUXSH_APP_BUNDLE_PATH",
                &host_path_to_shell_path(&path.to_string_lossy()),
            );
        }
    }

    if executor.get_env("WINUXSH").is_none() {
        if let Some(path) = first_valid_winuxsh_framework_dir(home_dir, app_bundle.as_deref()) {
            executor.set_env("WINUXSH", &host_path_to_shell_path(&path.to_string_lossy()));
        }
    }
}

fn app_bundled_winuxsh_framework_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join("bundles").join(OFFICIAL_BUNDLE_NAME);
    is_winuxsh_framework_dir(&path).then_some(path)
}

fn first_valid_winuxsh_framework_dir(
    home_dir: &Path,
    app_bundle: Option<&Path>,
) -> Option<PathBuf> {
    let mut candidates = vec![
        home_dir.join(".oh-my-winuxsh"),
        home_dir.join(".winuxsh").join("oh-my-winuxsh"),
    ];
    let version_root = home_dir
        .join(".winuxsh")
        .join("bundles")
        .join(OFFICIAL_BUNDLE_NAME);
    if let Ok(entries) = std::fs::read_dir(version_root) {
        let mut versions = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        versions.sort();
        candidates.extend(versions);
    }
    if let Some(path) = app_bundle {
        candidates.push(path.to_path_buf());
    }
    candidates
        .into_iter()
        .find(|path| is_winuxsh_framework_dir(path))
}

fn is_winuxsh_framework_dir(path: &Path) -> bool {
    path.join("oh-my-winuxsh.winux").is_file()
}

fn shell_pwd_to_existing_host_dir(pwd: &str, env: &HashMap<String, String>) -> Option<PathBuf> {
    let path = Executor::resolve_shell_path_from_env(pwd, env);
    path.is_dir().then_some(path)
}

fn compatible_shell_path_from_env() -> Option<PathBuf> {
    let path = std::env::var_os(COMPATIBLE_SHELL_PATH_ENV)?;
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

fn prepare_shell_root(winuxcmd_path: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    if !cfg!(windows) {
        return Ok(None);
    }

    let root = std::env::var_os("WINUXSH_ROOT")
        .filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(shell_path_to_host_path(&value.to_string_lossy())))
        .or_else(|| winuxcmd_path.map(winuxcmd::installation_root));
    let Some(root) = root else {
        return Ok(None);
    };

    for relative in [
        "bin",
        "usr/bin",
        "usr/local/bin",
        "etc",
        "var",
        "tmp",
        "dev",
    ] {
        std::fs::create_dir_all(root.join(relative))?;
    }
    Ok(Some(root))
}

fn is_slash_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && (bytes.len() == 2 || bytes.get(2) == Some(&b'/'))
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

#[cfg(test)]
fn resolve_shell_path_argument(pwd: &str, arg: &str) -> PathBuf {
    resolve_shell_path_argument_with_env(pwd, arg, &HashMap::new())
}

fn resolve_shell_path_argument_with_env(
    pwd: &str,
    arg: &str,
    env: &HashMap<String, String>,
) -> PathBuf {
    if let Some(path) = resolve_current_user_tilde_path(arg) {
        return path;
    }

    let candidate = Executor::resolve_shell_path_from_env(arg, env);
    let normalized = arg.replace('\\', "/");
    if candidate.is_absolute() || is_windows_drive_path(&normalized) {
        return candidate;
    }

    Executor::resolve_shell_path_from_env(pwd, env).join(candidate)
}

fn resolve_current_user_tilde_path(arg: &str) -> Option<PathBuf> {
    let rest = if arg == "~" {
        ""
    } else {
        arg.strip_prefix("~/").or_else(|| arg.strip_prefix("~\\"))?
    };
    let home = shell_home_dir()?;
    let home = PathBuf::from(shell_path_to_host_path(home.to_string_lossy().as_ref()));
    if rest.is_empty() {
        Some(home)
    } else {
        Some(home.join(shell_path_to_host_path(rest)))
    }
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

fn apply_alias(executor: &mut Executor, name: &str, value: &str) -> bool {
    if !is_alias_name(name) {
        return false;
    }

    let source = format!("alias {}={}", name, shell_quote(value));
    let tokens = tokenize(&source);
    if tokens.is_empty() {
        return false;
    }
    let ast = parse(&tokens);
    executor.execute_ast(&ast).is_ok() && executor.last_exit_code() == 0
}

fn is_alias_name(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch == '-' || ch == '!' || ch.is_ascii_alphanumeric())
}

struct ProcessPluginInvocationOutput {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl ProcessPluginInvocationOutput {
    fn status(status: i32) -> Self {
        Self {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }
}

fn process_plugin_for_provider_from_state(
    provider_name: &str,
    plugins: &PluginRuntimeState,
) -> Option<(String, PluginProcessSpec, Vec<String>)> {
    crate::plugins::active_plugin_inventory()
        .packs
        .into_iter()
        .find_map(|pack| {
            if pack.kind != PluginKind::Process || !plugins.is_enabled(&pack.name) {
                return None;
            }
            if !pack
                .exports
                .providers
                .iter()
                .any(|exported| exported == provider_name)
            {
                return None;
            }
            pack.process
                .map(|process| (pack.name, process, pack.permissions))
        })
}

fn run_process_plugin_invocation_capture_with_env(
    pack_name: &str,
    process: &PluginProcessSpec,
    extra_args: &[String],
    hook_name: Option<&str>,
    context: &[(&str, String)],
    report_errors: bool,
    env: &HashMap<String, String>,
) -> anyhow::Result<ProcessPluginInvocationOutput> {
    let stdout_path = process_plugin_temp_path(&process.command, "stdout");
    let stderr_path = process_plugin_temp_path(&process.command, "stderr");
    let stdout = std::fs::File::create(&stdout_path)?;
    let stderr = std::fs::File::create(&stderr_path)?;
    let command_path = resolve_native_command_path_with_env(&process.command, env)
        .unwrap_or_else(|| PathBuf::from(&process.command));

    let mut command = Command::new(command_path);
    apply_shell_env_to_process_command(&mut command, env);
    if let Some(cwd) = process_working_dir_from_shell_env(env) {
        command.current_dir(cwd);
    }
    command
        .args(&process.args)
        .args(extra_args)
        .env("WINUXSH_PROCESS_PLUGIN_PACK", pack_name)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if let Some(hook_name) = hook_name {
        command.env("WINUXSH_PROCESS_PLUGIN_HOOK", hook_name);
    }
    for (name, value) in context {
        command.env(name, value);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = std::fs::remove_file(&stdout_path);
            let _ = std::fs::remove_file(&stderr_path);
            if report_errors {
                eprintln!(
                    "winuxsh: process plugin '{}' failed to run '{}': {}",
                    pack_name, process.command, err
                );
            } else {
                log::debug!(
                    "process plugin provider '{}' failed to run '{}': {}",
                    pack_name,
                    process.command,
                    err
                );
            }
            return Ok(ProcessPluginInvocationOutput::status(127));
        }
    };

    let timeout = Duration::from_millis(process.timeout_millis.max(1));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&stdout_path);
                    let _ = std::fs::remove_file(&stderr_path);
                    if report_errors {
                        eprintln!(
                            "winuxsh: process plugin '{}' command '{}' timed out after {}ms",
                            pack_name, process.command, process.timeout_millis
                        );
                    } else {
                        log::debug!(
                            "process plugin provider '{}' command '{}' timed out after {}ms",
                            pack_name,
                            process.command,
                            process.timeout_millis
                        );
                    }
                    return Ok(ProcessPluginInvocationOutput::status(124));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&stdout_path);
                let _ = std::fs::remove_file(&stderr_path);
                if report_errors {
                    eprintln!(
                        "winuxsh: process plugin '{}' failed while waiting for '{}': {}",
                        pack_name, process.command, err
                    );
                } else {
                    log::debug!(
                        "process plugin provider '{}' failed while waiting for '{}': {}",
                        pack_name,
                        process.command,
                        err
                    );
                }
                return Ok(ProcessPluginInvocationOutput::status(1));
            }
        }
    };

    let stdout = std::fs::read(&stdout_path).unwrap_or_default();
    let stderr = std::fs::read(&stderr_path).unwrap_or_default();
    let _ = std::fs::remove_file(&stdout_path);
    let _ = std::fs::remove_file(&stderr_path);
    Ok(ProcessPluginInvocationOutput {
        status: status.code().unwrap_or(1),
        stdout,
        stderr,
    })
}

fn apply_shell_env_to_process_command(command: &mut Command, env: &HashMap<String, String>) {
    for (name, value) in env {
        if name.eq_ignore_ascii_case("PATH") {
            command.env(name, process_path_from_shell_path_list(value, Some(env)));
        } else {
            command.env(name, value);
        }
    }
}

fn process_working_dir_from_shell_env(env: &HashMap<String, String>) -> Option<PathBuf> {
    let pwd = env.get("PWD")?;
    let cwd = Executor::resolve_shell_path_from_env(pwd, env);
    cwd.is_dir().then_some(cwd)
}

fn process_plugin_temp_path(command: &str, stream: &str) -> PathBuf {
    let safe_command: String = command
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "winuxsh-process-plugin-{}-{}-{}-{}",
        safe_command,
        stream,
        std::process::id(),
        nanos
    ))
}

fn resolve_native_command_path(command: &str) -> Option<PathBuf> {
    resolve_native_command_path_with_path(command, std::env::var_os("PATH")?)
}

fn resolve_native_command_path_with_env(
    command: &str,
    env: &HashMap<String, String>,
) -> Option<PathBuf> {
    let path = env
        .get("PATH")
        .map(|path| process_path_from_shell_path_list(path, Some(env)))
        .or_else(|| std::env::var("PATH").ok())?;
    resolve_native_command_path_with_path(command, path)
}

fn resolve_native_command_path_with_path(
    command: &str,
    path: impl AsRef<std::ffi::OsStr>,
) -> Option<PathBuf> {
    let command_path = PathBuf::from(command);
    if command_path.is_file() {
        return Some(command_path);
    }

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

    for dir in std::env::split_paths(path.as_ref()) {
        for ext in extensions {
            let candidate = dir.join(format!("{}{}", command, ext));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

fn sync_executor_path_from_process_path(executor: &mut Executor) {
    if let Ok(path) = std::env::var("PATH") {
        executor.set_env("PATH", &path);
    }
}

fn process_path_from_shell_path_list(value: &str, env: Option<&HashMap<String, String>>) -> String {
    if !cfg!(windows) {
        return value.to_string();
    }

    split_shell_path_list(value)
        .into_iter()
        .flat_map(|entry| shell_path_entry_to_process_paths(&entry, env))
        .collect::<Vec<_>>()
        .join(";")
}

fn split_shell_path_list(value: &str) -> Vec<String> {
    if value.contains(';') {
        return value
            .split(';')
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect();
    }

    if cfg!(windows) && is_windows_drive_path(value) {
        return vec![value.to_string()];
    }

    value
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

fn shell_path_entry_to_process_paths(
    entry: &str,
    env: Option<&HashMap<String, String>>,
) -> Vec<String> {
    if cfg!(windows) {
        let paths = env
            .map(|env| Executor::resolve_shell_path_process_entries_from_env(entry, env))
            .unwrap_or_else(|| vec![PathBuf::from(shell_path_to_host_path(entry))]);
        paths
            .into_iter()
            .map(|path| path.to_string_lossy().replace('/', "\\"))
            .collect()
    } else {
        vec![entry.to_string()]
    }
}

fn host_path_to_shell_path_with_root(value: &str, root: Option<&Path>) -> String {
    let normalized = value.replace('\\', "/");
    let Some(root) = root else {
        return host_path_to_shell_path(&normalized);
    };

    let root = root.to_string_lossy().replace('\\', "/");
    let root = root.trim_end_matches('/');
    if normalized.eq_ignore_ascii_case(root) {
        return "/".to_string();
    }
    if normalized.len() > root.len()
        && normalized[..root.len()].eq_ignore_ascii_case(root)
        && normalized.as_bytes().get(root.len()) == Some(&b'/')
    {
        return format!("/{}", &normalized[root.len() + 1..]);
    }
    host_path_to_shell_path(&normalized)
}

fn host_path_to_shell_path(value: &str) -> String {
    if cfg!(windows) {
        let normalized = value.replace('\\', "/");
        let bytes = normalized.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b'/'
        {
            let drive = (bytes[1] as char).to_ascii_uppercase();
            return format!("{drive}:{}", &normalized[2..]);
        }
        return normalized;
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
    use crate::test_support::PROCESS_STATE_LOCK;
    use reedline::Prompt;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn compatible_shell_path_env_is_explicit_and_non_empty() {
        let _lock = PROCESS_STATE_LOCK.lock().unwrap();

        {
            let _guard = EnvVarGuard::unset(COMPATIBLE_SHELL_PATH_ENV);
            assert_eq!(compatible_shell_path_from_env(), None);
        }

        {
            let _guard = EnvVarGuard::set_value(COMPATIBLE_SHELL_PATH_ENV, "");
            assert_eq!(compatible_shell_path_from_env(), None);
        }

        let shell_path = std::env::temp_dir().join("winuxsh-compatible-shell.exe");
        let _guard = EnvVarGuard::set(COMPATIBLE_SHELL_PATH_ENV, &shell_path);
        assert_eq!(compatible_shell_path_from_env(), Some(shell_path));
    }

    #[test]
    fn native_lifecycle_hooks_run_for_interactive_commands() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-native-hooks");
        let next_dir = temp.join("next");
        std::fs::create_dir_all(&next_dir).unwrap();
        let next_arg = shell_quote(&shell_display_path(&next_dir));

        let mut shell = test_shell(HookConfig {
            precmd: vec!["HOOK_PRECMD=\"precmd:$WINUXSH_LAST_EXIT_CODE\"".to_string()],
            preexec: vec!["HOOK_PREEXEC=\"preexec:$WINUXSH_PREEXEC_COMMAND\"".to_string()],
            chpwd: vec!["HOOK_CHPWD=\"chpwd:$WINUXSH_OLDPWD->$WINUXSH_PWD\"".to_string()],
        });

        shell.run_precmd_hooks();
        shell
            .execute_interactive_line(&format!("cd {}", next_arg))
            .unwrap();

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
    fn process_plugin_hooks_run_for_interactive_lifecycle_events() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-process-hooks");
        let bundle = temp.join("bundle");
        let bin = temp.join("bin");
        let home = temp.join("home");
        let next_dir = temp.join("next");
        let log_path = temp.join("process-hook.log");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&next_dir).unwrap();
        write_process_hook_test_bundle(
            &bundle,
            "9.9.8",
            &["startup", "precmd", "preexec", "chpwd"],
            1000,
        );
        write_fake_process_hook(&bin, 0, false);

        let _bundle_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_PATH", &bundle);
        let _root_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_ROOT", &temp.join("root"));
        let _lock_guard = EnvVarGuard::set("WINUXSH_PLUGIN_LOCK", &temp.join("plugin-lock.toml"));
        let _log_guard = EnvVarGuard::set("WINUXSH_PROCESS_HOOK_LOG", &log_path);
        let old_path = prepend_path_for_test(&bin);

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();
        shell.run_precmd_hooks();
        shell
            .execute_interactive_line(&format!(
                "cd {}",
                shell_quote(&shell_display_path(&next_dir))
            ))
            .unwrap();

        restore_path_for_test(old_path);
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("hook=startup"), "{log}");
        assert!(log.contains("hook=precmd"), "{log}");
        assert!(log.contains("hook=preexec"), "{log}");
        assert!(log.contains("hook=chpwd"), "{log}");
        assert!(log.contains("args=--format json --hook precmd"), "{log}");
        assert!(log.contains("last=0"), "{log}");
        assert!(log.contains("cmd=cd "), "{log}");

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn source_plugin_scripts_load_before_legacy_user_startup_rc() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-source-plugin-startup");
        let bundle = temp.join("bundle");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        write_source_plugin_test_bundle(&bundle, "9.9.8");
        std::fs::write(
            home.join(WINUXSH_LEGACY_RC_FILE),
            "export SOURCE_PLUGIN_USER_RC_SEES=\"$SOURCE_PLUGIN_VALUE\"\nalias source_alias='echo user-override'\n",
        )
        .unwrap();

        let _bundle_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_PATH", &bundle);
        let _root_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_ROOT", &temp.join("root"));
        let _lock_guard = EnvVarGuard::set("WINUXSH_PLUGIN_LOCK", &temp.join("plugin-lock.toml"));

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();

        assert_eq!(
            shell.executor.get_env("SOURCE_PLUGIN_VALUE"),
            Some("source-test:1")
        );
        assert_eq!(
            shell.executor.get_env("SOURCE_PLUGIN_USER_RC_SEES"),
            Some("source-test:1")
        );
        assert_eq!(
            shell.aliases.get("source_alias").map(String::as_str),
            Some("echo user-override")
        );
        assert!(shell
            .executor
            .get_env("WINUXSH_REPL_PLUGIN_STARTUP")
            .is_none());
        assert!(shell.executor.get_env("WINUXSH_PLUGIN_NAME").is_none());

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn framework_source_plugin_receives_bundle_root() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-framework-source-plugin-root");
        let bundle = temp.join("bundle");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        write_framework_source_plugin_test_bundle(&bundle, "9.9.12");

        let _bundle_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_PATH", &bundle);
        let _root_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_ROOT", &temp.join("root"));
        let _lock_guard = EnvVarGuard::set("WINUXSH_PLUGIN_LOCK", &temp.join("plugin-lock.toml"));

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();

        let expected_root = host_path_to_shell_path(&bundle.to_string_lossy());
        assert_eq!(
            shell.executor.get_env("FRAMEWORK_ROOT_VALUE"),
            Some("from-bundle-lib")
        );
        assert_eq!(
            shell.executor.get_env("FRAMEWORK_WINUXSH"),
            Some(expected_root.as_str())
        );
        assert_eq!(
            shell.executor.get_env("FRAMEWORK_BUNDLE_DIR"),
            Some(expected_root.as_str())
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn prompt_core_theme_templates_drive_default_host_prompt() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-framework-plugin-prompt-sync");
        let bundle = temp.join("bundle");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        write_framework_source_plugin_test_bundle(&bundle, "9.9.14");

        let _bundle_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_PATH", &bundle);
        let _root_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_ROOT", &temp.join("root"));
        let _lock_guard = EnvVarGuard::set("WINUXSH_PLUGIN_LOCK", &temp.join("plugin-lock.toml"));

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();

        let rendered = reedline::Prompt::render_prompt_left(&shell.prompt).into_owned();
        assert_eq!(
            shell.executor.get_env("WINUXSH_PROMPT_LEFT"),
            Some("PLUGIN:{git}{prompt_char} ")
        );
        assert!(rendered.contains("PLUGIN:"), "{rendered:?}");
        assert!(rendered.contains("SNAPSHOT:startup"), "{rendered:?}");
        assert!(rendered.contains('%'), "{rendered:?}");

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn prompt_core_precmd_git_snapshot_updates_next_host_prompt() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-framework-plugin-prompt-snapshot");
        let bundle = temp.join("bundle");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        write_framework_source_plugin_test_bundle(&bundle, "9.9.16");

        let _bundle_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_PATH", &bundle);
        let _root_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_ROOT", &temp.join("root"));
        let _lock_guard = EnvVarGuard::set("WINUXSH_PLUGIN_LOCK", &temp.join("plugin-lock.toml"));

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();
        shell.execute_script("false").unwrap();
        shell.run_precmd_hooks();

        let rendered = reedline::Prompt::render_prompt_left(&shell.prompt).into_owned();
        assert_eq!(
            shell.executor.get_env("WINUXSH_PROMPT_GIT"),
            Some("SNAPSHOT:precmd:1 ")
        );
        assert!(rendered.contains("SNAPSHOT:precmd:1"), "{rendered:?}");
        assert!(!rendered.contains("SNAPSHOT:startup"), "{rendered:?}");

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn source_plugin_scripts_run_for_interactive_lifecycle_hooks() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-source-plugin-lifecycle");
        let bundle = temp.join("bundle");
        let home = temp.join("home");
        let next_dir = temp.join("next");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&next_dir).unwrap();
        write_source_plugin_test_bundle_with_hooks(
            &bundle,
            "9.9.9",
            &["startup", "precmd", "preexec", "chpwd"],
        );

        let _bundle_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_PATH", &bundle);
        let _root_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_ROOT", &temp.join("root"));
        let _lock_guard = EnvVarGuard::set("WINUXSH_PLUGIN_LOCK", &temp.join("plugin-lock.toml"));

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();
        shell.run_precmd_hooks();
        shell
            .execute_interactive_line(&format!(
                "cd {}",
                shell_quote(&shell_display_path(&next_dir))
            ))
            .unwrap();

        assert_eq!(
            shell.executor.get_env("SOURCE_PLUGIN_VALUE"),
            Some("source-test:1")
        );
        assert_eq!(
            shell.executor.get_env("SOURCE_PLUGIN_PRECMD"),
            Some("source-test:0")
        );
        let preexec = shell
            .executor
            .get_env("SOURCE_PLUGIN_PREEXEC")
            .unwrap_or_default();
        assert!(preexec.starts_with("source-test:cd "), "{preexec}");
        let chpwd = shell
            .executor
            .get_env("SOURCE_PLUGIN_CHPWD")
            .unwrap_or_default();
        assert!(chpwd.starts_with("source-test:"), "{chpwd}");
        assert!(chpwd.contains("->"), "{chpwd}");
        assert!(shell.executor.get_env("WINUXSH_PLUGIN_HOOK").is_none());
        assert!(shell.executor.get_env("WINUXSH_PREEXEC_COMMAND").is_none());
        assert!(shell.executor.get_env("WINUXSH_OLDPWD").is_none());
        assert!(shell.executor.get_env("WINUXSH_PWD").is_none());

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn winuxshrc_runs_once_for_repl_startup_shell_customization() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-winshrc-startup");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(WINUXSH_RC_FILE),
            r#"
export WINUXSHRC_VALUE=from-rc
alias hello='echo from-alias'
"#,
        )
        .unwrap();

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.run_startup_rc();

        assert_eq!(shell.executor.get_env("WINUXSHRC_VALUE"), Some("from-rc"));
        assert_eq!(
            shell.aliases.get("hello").map(String::as_str),
            Some("echo from-alias")
        );
        assert_eq!(shell.execute_interactive_line("hello").unwrap(), 0);
        assert!(shell.executor.get_env("WINUXSH_REPL_STARTUP").is_none());

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn winuxshrc_takes_precedence_over_legacy_winshrc() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-primary-rc-startup");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(WINUXSH_LEGACY_RC_FILE),
            "export WINUXSH_RC_SOURCE=legacy\n",
        )
        .unwrap();
        std::fs::write(
            home.join(WINUXSH_RC_FILE),
            "export WINUXSH_RC_SOURCE=primary\n",
        )
        .unwrap();

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.run_startup_rc();

        assert_eq!(shell.executor.get_env("WINUXSH_RC_SOURCE"), Some("primary"));

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn winuxshrc_is_the_source_plugin_entrypoint_when_present() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-primary-rc-plugin-entrypoint");
        let bundle = temp.join("bundle");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        write_framework_source_plugin_test_bundle(&bundle, "9.9.17");
        std::fs::write(
            home.join(WINUXSH_RC_FILE),
            "export WINUXSH_RC_ENTRYPOINT=primary\n",
        )
        .unwrap();

        let _bundle_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_PATH", &bundle);
        let _root_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_ROOT", &temp.join("root"));
        let _lock_guard = EnvVarGuard::set("WINUXSH_PLUGIN_LOCK", &temp.join("plugin-lock.toml"));

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();

        assert_eq!(
            shell.executor.get_env("WINUXSH_RC_ENTRYPOINT"),
            Some("primary")
        );
        assert_eq!(shell.executor.get_env("FRAMEWORK_ROOT_VALUE"), None);

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn winuxshrc_framework_hooks_replace_host_source_lifecycle_hooks() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-primary-rc-framework-hooks");
        let bundle = temp.join("bundle");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        write_framework_source_plugin_test_bundle(&bundle, "9.9.18");
        std::fs::write(
            home.join(WINUXSH_RC_FILE),
            r#"
winuxsh_run_precmd_hooks() {
  export WINUXSH_FRAMEWORK_PRECMD="$WINUXSH_LAST_EXIT_CODE"
}
winuxsh_run_preexec_hooks() {
  export WINUXSH_FRAMEWORK_PREEXEC="$WINUXSH_PREEXEC_COMMAND"
}
winuxsh_run_chpwd_hooks() {
  export WINUXSH_FRAMEWORK_CHPWD="$WINUXSH_OLDPWD->$WINUXSH_PWD"
}
"#,
        )
        .unwrap();

        let _bundle_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_PATH", &bundle);
        let _root_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_ROOT", &temp.join("root"));
        let _lock_guard = EnvVarGuard::set("WINUXSH_PLUGIN_LOCK", &temp.join("plugin-lock.toml"));

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();
        shell.run_precmd_hooks();
        shell.run_preexec_hooks("echo hello");
        shell.run_chpwd_hooks_if_changed("C:/old", "C:/new");

        assert_eq!(
            shell.executor.get_env("WINUXSH_FRAMEWORK_PRECMD"),
            Some("0")
        );
        assert_eq!(
            shell.executor.get_env("WINUXSH_FRAMEWORK_PREEXEC"),
            Some("echo hello")
        );
        assert_eq!(
            shell.executor.get_env("WINUXSH_FRAMEWORK_CHPWD"),
            Some("C:/old->C:/new")
        );
        assert_eq!(shell.executor.get_env("FRAMEWORK_ROOT_VALUE"), None);
        assert_eq!(shell.executor.get_env("FRAMEWORK_ROOT_VALUE"), None);

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
        let lines = native_command_not_found_lines("rg", true, |command| {
            matches!(command, "winget" | "scoop")
        });

        assert_eq!(lines[0], "winuxsh: rg: command not found");
        assert!(lines.contains(&"winuxsh: try 'wpm install ripgrep' to add rg".to_string()));
        assert!(lines.contains(&"winuxsh: package search hints:".to_string()));
        assert!(lines.contains(&"  winget search --name 'rg'".to_string()));
        assert!(lines.contains(&"  scoop search 'rg'".to_string()));
        assert!(!lines.iter().any(|line| line.contains("choco search")));
    }

    #[test]
    fn native_command_not_found_hint_lines_include_wpm_without_search() {
        let lines = native_command_not_found_hint_lines("awk", false, |_| true);

        assert_eq!(lines, vec!["winuxsh: try 'wpm install awk' to add awk"]);
    }

    #[test]
    fn native_command_not_found_lines_skip_package_hints_for_paths() {
        let lines = native_command_not_found_lines("./missing", true, |_| true);

        assert_eq!(lines, vec!["winuxsh: ./missing: command not found"]);
    }
    #[test]
    fn command_not_found_provider_request_captures_context() {
        let request = command_not_found_provider_request(
            "rg",
            &["--files".to_string()],
            Some("C:/work/project"),
            |command| matches!(command, "winget" | "choco"),
        );
        assert_eq!(request.command, "rg");
        assert_eq!(request.args, vec!["--files".to_string()]);
        assert_eq!(request.cwd.as_deref(), Some("C:/work/project"));
        assert_eq!(
            request.package_search_helpers,
            vec!["winget".to_string(), "choco".to_string()]
        );
    }
    #[test]
    fn command_not_found_provider_suggestions_replace_native_hints() {
        let provider_output = parse_command_not_found_provider_output(
            b"winuxsh: provider suggests install ripgrep\n  custom search rg\n",
        );
        let lines = command_not_found_lines_with_provider("rg", true, |_| true, provider_output);
        assert_eq!(lines[0], "winuxsh: rg: command not found");
        assert_eq!(
            lines[1..],
            [
                "winuxsh: provider suggests install ripgrep".to_string(),
                "  custom search rg".to_string(),
            ]
        );
        assert!(!lines
            .iter()
            .any(|line| line.contains("wpm install ripgrep")));
    }
    #[test]
    fn command_not_found_provider_empty_output_falls_back_to_native_hints() {
        let provider_output = parse_command_not_found_provider_output(b"\n\r\n");
        assert_eq!(provider_output, CommandNotFoundProviderOutput::Empty);
        let lines = command_not_found_lines_with_provider("awk", false, |_| false, provider_output);
        assert_eq!(lines[0], "winuxsh: awk: command not found");
        assert!(lines.iter().any(|line| line.contains("wpm install awk")));
    }
    #[test]
    fn command_not_found_provider_failure_falls_back_to_native_hints() {
        let lines = command_not_found_lines_with_provider(
            "rg",
            true,
            |command| command == "winget",
            CommandNotFoundProviderOutput::Failed("timeout".to_string()),
        );
        assert_eq!(lines[0], "winuxsh: rg: command not found");
        assert!(lines
            .iter()
            .any(|line| line.contains("wpm install ripgrep")));
        assert!(lines.iter().any(|line| line.contains("winget search")));
    }
    #[test]
    fn command_not_found_provider_invalid_output_falls_back_to_native_hints() {
        let provider_output = parse_command_not_found_provider_output(&[0xff, 0xfe]);
        assert!(matches!(
            provider_output,
            CommandNotFoundProviderOutput::Failed(_)
        ));
        let lines = command_not_found_lines_with_provider("rg", false, |_| false, provider_output);
        assert_eq!(lines[0], "winuxsh: rg: command not found");
        assert!(lines
            .iter()
            .any(|line| line.contains("wpm install ripgrep")));
    }

    #[test]
    fn alias_mirror_tracks_successful_interactive_alias_commands() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        shell
            .execute_interactive_line("alias gst='git status'")
            .unwrap();
        assert_eq!(
            shell.aliases.get("gst").map(String::as_str),
            Some("git status")
        );
        assert_eq!(
            shell.native_alias_finder_matches("git status"),
            vec!["winuxsh: alias available: gst='git status'"]
        );

        shell.execute_interactive_line("unalias gst").unwrap();
        assert!(shell.native_alias_finder_matches("git status").is_empty());
    }

    #[test]
    fn completion_state_tracks_shell_local_variables_after_interactive_source() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        shell
            .execute_interactive_line("SOURCE_COMPLETION_VAR=from-source")
            .unwrap();

        assert_eq!(
            shell.executor.get_env("SOURCE_COMPLETION_VAR"),
            Some("from-source")
        );
        assert!(shell
            .completion_probe("$SOURCE_COMPLETION", "$SOURCE_COMPLETION".len())
            .contains(&"$SOURCE_COMPLETION_VAR".to_string()));
    }

    #[test]
    fn execute_interactive_script_runs_multiline_compound_blocks() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        shell.execute_interactive_line("HTTP_CODE=200").unwrap();
        let code = shell
            .execute_interactive_script("if [ $HTTP_CODE -eq 200 ]; then\n  RESULT=OK\nfi")
            .unwrap();

        assert_eq!(code, 0);
        assert_eq!(shell.executor.get_env("RESULT"), Some("OK"));
    }

    #[test]
    fn shell_path_to_host_path_converts_drive_style_paths() {
        if cfg!(windows) {
            assert_eq!(
                shell_path_to_host_path("/c/Users/me/project"),
                "C:/Users/me/project"
            );
            assert_eq!(shell_path_to_host_path("/d"), "D:/");
        } else {
            assert_eq!(
                shell_path_to_host_path("/c/Users/me/project"),
                "/c/Users/me/project"
            );
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
            assert_eq!(
                host_path_to_shell_path("/home/me/project"),
                "/home/me/project"
            );
        }
    }

    #[test]
    fn winuxsh_framework_discovery_prefers_user_dirs_before_app_bundle() {
        let temp = unique_temp_dir("winuxsh-framework-discovery");
        let home = temp.join("home");
        let home_dot = home.join(".oh-my-winuxsh");
        let home_config = home.join(".winuxsh").join("oh-my-winuxsh");
        let home_version = home
            .join(".winuxsh")
            .join("bundles")
            .join(OFFICIAL_BUNDLE_NAME)
            .join("1.0.0");
        let app_bundle = temp.join("app").join("bundles").join(OFFICIAL_BUNDLE_NAME);

        for path in [&home_dot, &home_config, &home_version, &app_bundle] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(path.join("oh-my-winuxsh.winux"), "").unwrap();
        }

        assert_eq!(
            first_valid_winuxsh_framework_dir(&home, Some(&app_bundle)).as_deref(),
            Some(home_dot.as_path())
        );
        std::fs::remove_file(home_dot.join("oh-my-winuxsh.winux")).unwrap();
        assert_eq!(
            first_valid_winuxsh_framework_dir(&home, Some(&app_bundle)).as_deref(),
            Some(home_config.as_path())
        );
        std::fs::remove_file(home_config.join("oh-my-winuxsh.winux")).unwrap();
        assert_eq!(
            first_valid_winuxsh_framework_dir(&home, Some(&app_bundle)).as_deref(),
            Some(home_version.as_path())
        );
        std::fs::remove_file(home_version.join("oh-my-winuxsh.winux")).unwrap();
        assert_eq!(
            first_valid_winuxsh_framework_dir(&home, Some(&app_bundle)).as_deref(),
            Some(app_bundle.as_path())
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn resolve_shell_path_argument_expands_current_user_tilde() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let temp = unique_temp_dir("winuxsh-tilde-path");
        let home = temp.join("home");
        let _home_guard = EnvVarGuard::set("HOME", &home);
        let _userprofile_guard = EnvVarGuard::set("USERPROFILE", &home);

        assert_eq!(
            host_display_path(&resolve_shell_path_argument("C:/work", "~")),
            host_display_path(&home)
        );
        assert_eq!(
            host_display_path(&resolve_shell_path_argument("C:/work", "~/dir/file.txt")),
            host_display_path(&home.join("dir").join("file.txt"))
        );
        assert_eq!(
            host_display_path(&resolve_shell_path_argument("C:/work", r"~\dir\file.txt")),
            host_display_path(&home.join("dir").join("file.txt"))
        );
        assert_eq!(
            host_display_path(&resolve_shell_path_argument("C:/work", "~other/file.txt")),
            host_display_path(&PathBuf::from("C:/work").join("~other").join("file.txt"))
        );
    }

    #[test]
    fn shell_home_dir_accepts_shell_style_userprofile() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _home_guard = EnvVarGuard::set_value("HOME", "");
        let _userprofile_guard = EnvVarGuard::set_value("USERPROFILE", "/c/Users/example");

        let home = shell_home_dir().unwrap();
        if cfg!(windows) {
            assert_eq!(host_display_path(&home), "C:/Users/example");
        } else {
            assert_eq!(host_display_path(&home), "/c/Users/example");
        }
    }

    #[cfg(windows)]
    #[test]
    fn shell_home_dir_prefers_userprofile_over_home() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let temp = unique_temp_dir("winuxsh-userprofile-home-precedence");
        let home = temp.join("home");
        let userprofile = temp.join("userprofile");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&userprofile).unwrap();
        let _home_guard = EnvVarGuard::set("HOME", &home);
        let _userprofile_guard = EnvVarGuard::set("USERPROFILE", &userprofile);

        assert_eq!(
            crate::path_utils::normalize_existing_host_path(shell_home_dir().unwrap()),
            crate::path_utils::normalize_existing_host_path(userprofile.clone())
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn startup_rc_uses_shell_style_userprofile_when_home_is_empty() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let temp = unique_temp_dir("winuxsh-shell-style-home-startup");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(WINUXSH_RC_FILE),
            "export WINUXSH_RC_SOURCE=primary\n",
        )
        .unwrap();
        let host_home = host_display_path(&home);
        let shell_style_home =
            if cfg!(windows) && host_home.len() >= 2 && host_home.as_bytes()[1] == b':' {
                let drive = (host_home.as_bytes()[0] as char).to_ascii_lowercase();
                format!("/{drive}{}", &host_home[2..])
            } else {
                host_home
            };
        let _home_guard = EnvVarGuard::set_value("HOME", "");
        let _userprofile_guard = EnvVarGuard::set_value("USERPROFILE", &shell_style_home);

        let mut shell = Shell::new().unwrap();
        shell.run_startup_rc();

        assert_eq!(shell.executor.get_env("WINUXSH_RC_SOURCE"), Some("primary"));
        if cfg!(windows) {
            assert!(
                shell
                    .executor
                    .get_env("HOME")
                    .is_some_and(|home| is_windows_drive_path(home)),
                "HOME should be Windows-native for external tools, got {:?}",
                shell.executor.get_env("HOME")
            );
        }
    }

    #[test]
    fn windows_drive_only_paths_normalize_to_drive_root() {
        if cfg!(windows) {
            assert_eq!(windows_drive_path_to_slash_drive("C:"), Some("/c/".into()));
            assert_eq!(
                windows_drive_path_to_slash_drive("C:/Users/me"),
                Some("/c/Users/me".into())
            );
        } else {
            assert_eq!(windows_drive_path_to_slash_drive("C:"), None);
        }
    }

    #[test]
    fn bare_windows_drive_commands_rewrite_to_cd_drive_root() {
        let tokens = tokenize("c:; echo keep");
        let mut ast = parse(&tokens);
        normalize_bare_windows_drive_commands(&mut ast);

        if cfg!(windows) {
            assert_eq!(ast.commands[0].words, vec!["cd", "C:/"]);
            assert_eq!(
                ast.commands[0].word_kinds,
                vec![TokenKind::Word, TokenKind::Word]
            );
            assert_eq!(ast.commands[0].word_metadata.len(), 2);
            assert_eq!(ast.commands[1].words, vec!["echo", "keep"]);
        } else {
            assert_eq!(ast.commands[0].words, vec!["c:"]);
        }
    }

    #[test]
    fn windows_drive_normalization_descends_into_and_or_lists() {
        let tokens = tokenize("cd c: && c:");
        let mut ast = parse(&tokens);
        normalize_bare_windows_drive_commands(&mut ast);
        normalize_cd_windows_drive_args(&mut ast);

        if cfg!(windows) {
            let and_or_list = ast.commands[0].and_or_list.as_ref().unwrap();
            assert_eq!(and_or_list.commands[0].words, vec!["cd", "/c/"]);
            assert_eq!(and_or_list.commands[1].words, vec!["cd", "/c/"]);
        } else {
            let and_or_list = ast.commands[0].and_or_list.as_ref().unwrap();
            assert_eq!(and_or_list.commands[0].words, vec!["cd", "c:"]);
            assert_eq!(and_or_list.commands[1].words, vec!["c:"]);
        }
    }

    #[test]
    fn process_path_from_shell_path_list_converts_msys_drive_entries() {
        if cfg!(windows) {
            assert_eq!(
                process_path_from_shell_path_list("/c/Users/me/bin;C:/Windows/System32", None),
                r"C:\Users\me\bin;C:\Windows\System32"
            );
        } else {
            assert_eq!(
                process_path_from_shell_path_list("/home/me/bin:/usr/bin", None),
                "/home/me/bin:/usr/bin"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn installed_winuxcmd_root_maps_host_path_helpers() {
        let _root_guard = EnvVarGuard::set_value("WINUXSH_ROOT", "");
        let root = unique_temp_dir("winuxsh-installed-root");
        let configured = root.join("root");
        let winuxcmd = configured.join("usr/bin/winuxcmd.exe");
        std::fs::create_dir_all(winuxcmd.parent().unwrap()).unwrap();
        std::fs::write(&winuxcmd, b"test").unwrap();
        let shell_root = prepare_shell_root(Some(&winuxcmd)).unwrap();
        assert_eq!(shell_root, Some(configured.clone()));

        let mut env = HashMap::new();
        env.insert(
            "__RUBASH_SHELL_ROOT".to_string(),
            configured.to_string_lossy().to_string(),
        );
        assert_eq!(
            process_path_from_shell_path_list("/usr/bin:/bin", Some(&env)),
            format!(
                r"{}\usr\bin;{}\bin",
                configured.display(),
                configured.display()
            )
        );
        assert_eq!(
            Executor::resolve_shell_path_from_env("/etc", &env),
            configured.join("etc")
        );
        assert_eq!(
            host_path_to_shell_path_with_root(
                &configured.join("etc").to_string_lossy(),
                Some(&configured),
            ),
            "/etc"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_windows_path_literals_are_normalized_before_tokenize() {
        if cfg!(windows) {
            assert_eq!(
                normalize_native_windows_path_literals(r"ls C:\Users\me"),
                r"ls C:\\Users\\me"
            );
            assert_eq!(
                normalize_native_windows_path_literals(r"ls --root=C:\Users\me"),
                r"ls --root=C:\\Users\\me"
            );
            assert_eq!(
                normalize_native_windows_path_literals(r"echo foo\ bar C:\Users\me"),
                r"echo foo\ bar C:\\Users\\me"
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

        assert_eq!(ast.commands[0].words[1], "C:\x14Users\x14me");
        assert_eq!(ast.commands[1].words[1], "C:\x14Users\x14me");
    }

    #[test]
    fn winuxcmd_slash_drive_args_are_translated_for_path_commands() {
        let tokens = tokenize(
            "ls /c/Users; mktemp /c/Users/test.XXXXXX.tmp; echo /c/Users; RealPath.Exe /c/Users; sha256sum /c/Users/file; ln /c/Users/a /c/Users/b; printf /c/Users; mkdir -p /c/Users/tmp && mktemp /c/Users/tmp/test.XXXXXX.tmp",
        );
        let mut ast = parse(&tokens);
        normalize_winuxcmd_slash_drive_args(&mut ast);

        if cfg!(windows) {
            assert_eq!(ast.commands[0].words[1], "C:/Users");
            assert_eq!(ast.commands[1].words[1], "C:/Users/test.XXXXXX.tmp");
            assert_eq!(ast.commands[2].words[1], "/c/Users");
            assert_eq!(ast.commands[3].words[1], "C:/Users");
            assert_eq!(ast.commands[4].words[1], "C:/Users/file");
            assert_eq!(ast.commands[5].words[1], "C:/Users/a");
            assert_eq!(ast.commands[5].words[2], "C:/Users/b");
            assert_eq!(ast.commands[6].words[1], "/c/Users");
            let and_or_list = ast.commands[7].and_or_list.as_ref().unwrap();
            assert_eq!(and_or_list.commands[0].words[2], "C:/Users/tmp");
            assert_eq!(
                and_or_list.commands[1].words[1],
                "C:/Users/tmp/test.XXXXXX.tmp"
            );
        } else {
            assert_eq!(ast.commands[0].words[1], "/c/Users");
            assert_eq!(ast.commands[1].words[1], "/c/Users/test.XXXXXX.tmp");
            assert_eq!(ast.commands[2].words[1], "/c/Users");
            assert_eq!(ast.commands[3].words[1], "/c/Users");
            assert_eq!(ast.commands[4].words[1], "/c/Users/file");
            assert_eq!(ast.commands[5].words[1], "/c/Users/a");
            assert_eq!(ast.commands[5].words[2], "/c/Users/b");
            assert_eq!(ast.commands[6].words[1], "/c/Users");
            let and_or_list = ast.commands[7].and_or_list.as_ref().unwrap();
            assert_eq!(and_or_list.commands[0].words[2], "/c/Users/tmp");
            assert_eq!(
                and_or_list.commands[1].words[1],
                "/c/Users/tmp/test.XXXXXX.tmp"
            );
        }
    }

    #[test]
    fn interactive_terminal_grep_colors_force_pipeline_final_stage() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("ls -la | grep map");
        rewrite_winuxcmd_command_shims(&mut tokens, true);
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
        rewrite_winuxcmd_command_shims(&mut tokens, true);
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
        rewrite_winuxcmd_command_shims(&mut tokens, true);
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
        rewrite_winuxcmd_command_shims(&mut tokens, true);
        let ast = parse(&tokens);
        let pipeline = ast.commands[0].pipeline_command.as_ref().unwrap();

        assert_eq!(pipeline.stages[1].words, vec!["grep.exe", "map"]);
    }

    #[test]
    fn interactive_terminal_grep_colors_force_simple_terminal_grep() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("grep map README.md");
        rewrite_winuxcmd_command_shims(&mut tokens, true);
        let ast = parse(&tokens);

        assert_eq!(
            ast.commands[0].words,
            vec!["grep.exe", "--color=always", "map", "README.md"]
        );
    }

    #[test]
    fn script_grep_rewrite_forces_external_grep_without_color() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("printf \"abc\\n\" | grep -E \"a.+c\"");
        rewrite_winuxcmd_command_shims(&mut tokens, false);
        let ast = parse(&tokens);
        let pipeline = ast.commands[0].pipeline_command.as_ref().unwrap();

        assert_eq!(pipeline.stages[1].words, vec!["grep.exe", "-E", "a.+c"]);
    }

    #[test]
    fn file_commands_are_not_rewritten_or_claimed_as_winuxsh_builtins() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("rm -rf -- '-p'; cat file; cp src dst");
        rewrite_winuxcmd_command_shims(&mut tokens, false);
        let ast = parse(&tokens);

        assert_eq!(ast.commands[0].words, vec!["rm", "-rf", "--", "-p"]);
        assert!(winuxsh_builtin_words(&ast.commands[0]).is_none());
        assert!(winuxsh_builtin_words(&ast.commands[1]).is_none());
        assert!(winuxsh_builtin_words(&ast.commands[2]).is_none());
    }

    #[test]
    fn source_and_file_commands_are_not_claimed_as_winuxsh_builtins() {
        let mut tokens = tokenize(
            "cat file; chmod +w file; cp src dst; kill -l; mkdir -p dir; mkfifo pipe; pwd; rm -rf dir; rmdir dir; self-update --check; source file; touch file",
        );
        rewrite_winuxcmd_command_shims(&mut tokens, false);
        let ast = parse(&tokens);

        let names: Vec<_> = ast
            .commands
            .iter()
            .map(|command| winuxsh_builtin_words(command).map(|(name, _)| name))
            .collect();
        assert_eq!(
            names,
            vec![None, None, None, None, None, None, None, None, None, None, None, None,]
        );
    }

    #[test]
    fn self_update_repl_commands_are_not_shell_builtins() {
        let ast = parse(&tokenize("self-update --check; update-winuxsh --dry-run"));

        let names: Vec<_> = ast
            .commands
            .iter()
            .map(|command| winuxsh_builtin_words(command).map(|(name, _)| name))
            .collect();
        assert_eq!(names, vec![None, None]);
    }

    #[test]
    fn host_external_cp_without_path_defers_to_command_not_found_surface() {
        let env = HashMap::from([
            ("PWD".to_string(), ".".to_string()),
            ("PATH".to_string(), "".to_string()),
        ]);
        let output = execute_winuxsh_host_external_command(
            &["cp".to_string(), "--version".to_string()],
            &env,
            &PluginRuntimeState::default(),
        );
        assert!(
            output.is_none(),
            "missing cp should be handled by normal command-not-found flow"
        );
    }

    #[test]
    fn host_external_cp_defers_to_path_command_when_available() {
        if !cfg!(windows) {
            return;
        }

        let temp = unique_temp_dir("winuxsh-cp-path-wins");
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(temp.join("cp.cmd"), "@echo off\r\necho external-cp\r\n").unwrap();
        let env = HashMap::from([
            ("PWD".to_string(), ".".to_string()),
            ("PATH".to_string(), host_display_path(&temp)),
        ]);

        let output = execute_winuxsh_host_external_command(
            &["cp".to_string(), "--version".to_string()],
            &env,
            &PluginRuntimeState::default(),
        );

        assert!(
            output.is_none(),
            "PATH cp should be allowed to execute normally"
        );
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn file_helpers_stay_on_path_resolution_surface() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("cat file; chmod +x script; cp -R src dst; mkdir -p dir; mkfifo pipe; rm -rf dir; rmdir dir; touch -t 202001010000 file");
        rewrite_winuxcmd_command_shims(&mut tokens, false);
        let ast = parse(&tokens);

        assert_eq!(ast.commands[0].words[0], "cat");
        assert_eq!(ast.commands[1].words[0], "chmod");
        assert_eq!(ast.commands[2].words[0], "cp");
        assert_eq!(ast.commands[3].words[0], "mkdir");
        assert_eq!(ast.commands[4].words[0], "mkfifo");
        assert_eq!(ast.commands[5].words[0], "rm");
        assert_eq!(ast.commands[6].words[0], "rmdir");
        assert_eq!(ast.commands[7].words[0], "touch");
        for command in &ast.commands {
            assert!(winuxsh_builtin_words(command).is_none());
        }
    }

    #[test]
    fn builtin_prefix_does_not_fabricate_file_builtins() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("builtin rm -- '-p'");
        rewrite_winuxcmd_command_shims(&mut tokens, false);
        let ast = parse(&tokens);

        assert_eq!(ast.commands[0].words, vec!["builtin", "rm", "--", "-p"]);
        assert!(winuxsh_builtin_words(&ast.commands[0]).is_none());
    }

    #[test]
    fn setopt_delegates_to_rubash_builtin_surface() {
        let ast = parse(&tokenize(
            "setopt hist_ignore_space; builtin setopt prompt_subst; command unsetopt prompt_subst",
        ));

        assert!(winuxsh_builtin_words(&ast.commands[0]).is_none());
        assert!(winuxsh_builtin_words(&ast.commands[1]).is_none());
        assert!(winuxsh_builtin_words(&ast.commands[2]).is_none());
    }
    #[test]
    fn pwd_delegates_to_rubash_without_winuxcmd_path_dependency() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("pwd; builtin pwd; command pwd");
        rewrite_winuxcmd_command_shims(&mut tokens, false);
        let ast = parse(&tokens);

        assert_eq!(ast.commands[0].words, vec!["pwd"]);
        assert_eq!(ast.commands[1].words, vec!["builtin", "pwd"]);
        assert_eq!(ast.commands[2].words, vec!["command", "pwd"]);
        assert!(winuxsh_builtin_words(&ast.commands[0]).is_none());
        assert!(winuxsh_builtin_words(&ast.commands[1]).is_none());
        assert!(winuxsh_builtin_words(&ast.commands[2]).is_none());
    }

    #[test]
    fn redirected_pwd_uses_rubash_redirection_path() {
        if !cfg!(windows) {
            return;
        }

        let ast = parse(&tokenize("pwd > out.txt"));

        assert!(winuxsh_builtin_words(&ast.commands[0]).is_none());
    }

    #[test]
    fn rewritten_grep_exe_first_pipeline_stage_gets_stdin_bridge() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("grep -E alpha | cat");
        rewrite_winuxcmd_command_shims(&mut tokens, false);
        let mut ast = parse(&tokens);

        {
            let stage = process_stdin_pipeline_bridge_stage(&mut ast).unwrap();
            assert_eq!(stage.words[0], "grep.exe");
        }
    }

    #[test]
    fn redirected_grep_exe_pipeline_stage_does_not_get_stdin_bridge() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("grep alpha < input.txt | cat");
        rewrite_winuxcmd_command_shims(&mut tokens, false);
        let mut ast = parse(&tokens);

        assert!(process_stdin_pipeline_bridge_stage(&mut ast).is_none());
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
        assert_ne!(
            shell.executor.get_env("NODE_OPTIONS"),
            Some("--require bad")
        );

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
        shell.native_plugins.presets = vec!["fzf".to_string()];

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

    #[test]
    fn bash_prompt_command_updates_ps1_before_prompt_render() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let _columns_guard = EnvVarGuard::set_value("COLUMNS", "80");
        let mut shell = test_shell(HookConfig::default());
        shell.executor.set_env(
            "PROMPT_COMMAND",
            "PS1=\"status:$? cols:${COLUMNS:-missing}> \"",
        );
        shell.executor.set_last_exit_code(7);

        shell.run_precmd_hooks();

        match &shell.prompt {
            PromptBackend::Bash(prompt) => {
                assert_eq!(prompt.render_prompt_left(), "status:7 cols:80> ");
            }
            _ => panic!("expected Bash prompt backend"),
        }
        assert_eq!(shell.executor.last_exit_code(), 7);
    }

    #[test]
    fn bash_ps1_prompt_escapes_render_from_executor_state() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell
            .executor
            .set_env("PS1", "user:\\u host:\\h dir:\\w \\\\$ ");
        shell.executor.set_env("PS2", "more> ");

        shell.run_precmd_hooks();

        match &shell.prompt {
            PromptBackend::Bash(prompt) => {
                let left = prompt.render_prompt_left();
                assert!(left.contains("user:"), "{left}");
                assert!(left.contains("host:"), "{left}");
                assert!(left.contains("dir:"), "{left}");
                assert!(!left.contains("\\u"), "{left}");
                assert_eq!(prompt.render_prompt_multiline_indicator(), "more> ");
            }
            _ => panic!("expected Bash prompt backend"),
        }
    }

    #[test]
    fn bash_ps0_runs_before_interactive_command() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.executor.set_env(
            "PS0",
            "${STARSHIP_START_TIME:$((STARSHIP_START_TIME=12345,0)):0}",
        );

        assert_eq!(shell.execute_interactive_line(":").unwrap(), 0);

        assert_eq!(shell.executor.get_env("STARSHIP_START_TIME"), Some("12345"));
    }

    #[test]
    fn default_official_git_plugin_installs_builtin_alias_pack() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-default-plugin-git");
        std::fs::create_dir_all(&temp).unwrap();

        let shell = Shell::new().unwrap();

        assert_eq!(
            shell.aliases.get("gst").map(String::as_str),
            Some("git status")
        );
        assert!(shell.plugins.is_enabled("git"));

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn external_bundle_alias_pack_does_not_use_official_fallback_aliases() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap();
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("winuxsh-external-git-alias-fallback");
        let bundle = temp.join("bundle");
        write_external_aliasless_git_bundle(&bundle, "9.9.13");
        let _bundle_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_PATH", &bundle);
        let _root_guard = EnvVarGuard::set("WINUXSH_PLUGIN_BUNDLE_ROOT", &temp.join("root"));
        let _lock_guard = EnvVarGuard::set("WINUXSH_PLUGIN_LOCK", &temp.join("plugin-lock.toml"));

        let shell = Shell::new().unwrap();

        assert!(shell.plugins.is_enabled("git"));
        assert!(!shell.aliases.contains_key("gst"));
        assert!(!shell.aliases.contains_key("gco"));

        let _ = std::fs::remove_dir_all(temp);
    }

    fn write_process_hook_test_bundle(
        path: &std::path::Path,
        version: &str,
        hooks: &[&str],
        timeout_millis: u64,
    ) {
        let hooks = hooks
            .iter()
            .map(|hook| format!("{hook:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::create_dir_all(path.join("packs").join("process-hook")).unwrap();
        std::fs::write(
            path.join("bundle.toml"),
            format!(
                r#"name = "oh-my-winuxsh"
version = {version:?}
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = ["process-hook"]
available = ["process-hook"]
[layout]
packs_dir = "packs"
"#
            ),
        )
        .unwrap();
        std::fs::write(
            path.join("packs").join("process-hook").join("plugin.toml"),
            format!(
                r#"name = "process-hook"
bundle = "oh-my-winuxsh"
version = {version:?}
kind = "process"
api = "winuxsh:plugin@0.1.0"
category = "workflow"
summary = "Process plugin lifecycle hook fixture."
default = true
permissions = ["cwd:read", "process:run:winuxsh-process-hook"]
required_binaries = ["winuxsh-process-hook"]
[exports]
aliases = false
completions = []
prompt_segments = []
hooks = [{hooks}]
commands = []
keybindings = []
[process]
protocol = "winuxsh:process-plugin@0.1.0"
command = "winuxsh-process-hook"
args = ["--format", "json"]
timeout_millis = {timeout_millis}
"#
            ),
        )
        .unwrap();
    }

    fn write_source_plugin_test_bundle(path: &std::path::Path, version: &str) {
        write_source_plugin_test_bundle_with_hooks(path, version, &["startup"]);
    }

    fn write_source_plugin_test_bundle_with_hooks(
        path: &std::path::Path,
        version: &str,
        hooks: &[&str],
    ) {
        let hooks = hooks
            .iter()
            .map(|hook| format!("{hook:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::create_dir_all(path.join("packs").join("source-test")).unwrap();
        std::fs::write(
            path.join("bundle.toml"),
            format!(
                r#"name = "oh-my-winuxsh"
version = {version:?}
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = ["source-test"]
available = ["source-test"]
[layout]
packs_dir = "packs"
"#
            ),
        )
        .unwrap();
        std::fs::write(
            path.join("packs").join("source-test").join("plugin.toml"),
            format!(
                r#"name = "source-test"
bundle = "oh-my-winuxsh"
version = {version:?}
kind = "source"
api = "winuxsh:plugin@0.1.0"
category = "workflow"
summary = "Source plugin startup fixture."
default = true
permissions = ["shell:source"]
required_binaries = []
[exports]
aliases = true
completions = []
prompt_segments = []
hooks = [{hooks}]
commands = []
keybindings = []
themes = []
[source]
entry = "packs/source-test/init.winux"
"#
            ),
        )
        .unwrap();
        std::fs::write(
            path.join("packs").join("source-test").join("init.winux"),
            r#"case "$WINUXSH_PLUGIN_HOOK" in
  startup)
    export SOURCE_PLUGIN_VALUE="$WINUXSH_PLUGIN_NAME:$WINUXSH_REPL_PLUGIN_STARTUP"
    alias source_alias='echo from-source-plugin'
    ;;
  precmd)
    export SOURCE_PLUGIN_PRECMD="$WINUXSH_PLUGIN_NAME:$WINUXSH_LAST_EXIT_CODE"
    ;;
  preexec)
    export SOURCE_PLUGIN_PREEXEC="$WINUXSH_PLUGIN_NAME:$WINUXSH_PREEXEC_COMMAND"
    ;;
  chpwd)
    export SOURCE_PLUGIN_CHPWD="$WINUXSH_PLUGIN_NAME:$WINUXSH_OLDPWD->$WINUXSH_PWD"
    ;;
esac
"#,
        )
        .unwrap();
    }

    fn write_external_aliasless_git_bundle(path: &std::path::Path, version: &str) {
        std::fs::create_dir_all(path.join("packs").join("git")).unwrap();
        std::fs::write(
            path.join("bundle.toml"),
            format!(
                r#"name = "community-tools"
version = {version:?}
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = ["git"]
available = ["git"]
[layout]
packs_dir = "packs"
aliases_dir = "aliases"
"#
            ),
        )
        .unwrap();
        std::fs::write(
            path.join("packs").join("git").join("plugin.toml"),
            format!(
                r#"name = "git"
bundle = "community-tools"
version = {version:?}
kind = "builtin"
api = "winuxsh:plugin@0.1.0"
category = "devtools"
summary = "External git alias marker without bundle alias asset."
default = true
permissions = ["cwd:read"]
required_binaries = []
[exports]
aliases = true
completions = []
prompt_segments = []
hooks = []
commands = []
keybindings = []
"#
            ),
        )
        .unwrap();
    }

    fn write_framework_source_plugin_test_bundle(path: &std::path::Path, version: &str) {
        std::fs::create_dir_all(path.join("plugins").join("prompt-core")).unwrap();
        std::fs::create_dir_all(path.join("plugins").join("theme-minimal")).unwrap();
        std::fs::create_dir_all(path.join("lib")).unwrap();
        std::fs::write(
            path.join("bundle.toml"),
            format!(
                r#"name = "oh-my-winuxsh"
version = {version:?}
api = "winuxsh:plugin-bundle@0.1.0"
min_winuxsh = "0.8.3"
[packs]
default = []
available = []
[layout]
packs_dir = "packs"
"#
            ),
        )
        .unwrap();
        std::fs::write(
            path.join("plugins").join("prompt-core").join("plugin.toml"),
            format!(
                r#"name = "prompt-core"
version = {version:?}
kind = "source"
entry = "prompt-core.plugin.winux"
summary = "Framework prompt core fixture."
default = true
permissions = ["shell:source"]
required_binaries = []
[exports]
hooks = ["startup", "precmd"]
prompt_segments = ["cwd", "prompt_char"]
"#
            ),
        )
        .unwrap();
        std::fs::write(
            path.join("lib").join("root.winux"),
            "export FRAMEWORK_ROOT_VALUE=from-bundle-lib\n",
        )
        .unwrap();
        std::fs::write(
            path.join("lib").join("prompt.winux"),
            r#"winuxsh_prompt_use_template() {
  WINUXSH_PROMPT_LEFT="$1"
  WINUXSH_PROMPT_RIGHT="${2:-}"
  export WINUXSH_PROMPT_LEFT WINUXSH_PROMPT_RIGHT
}
"#,
        )
        .unwrap();
        std::fs::write(
            path.join("plugins")
                .join("prompt-core")
                .join("prompt-core.plugin.winux"),
            r#". "$WINUXSH/lib/root.winux"
export FRAMEWORK_WINUXSH="$WINUXSH"
export FRAMEWORK_BUNDLE_DIR="$WINUXSH_PLUGIN_BUNDLE_DIR"
case "$WINUXSH_PLUGIN_HOOK" in
  precmd)
    WINUXSH_PROMPT_GIT="SNAPSHOT:precmd:$WINUXSH_LAST_EXIT_CODE "
    ;;
  *)
    WINUXSH_PROMPT_GIT="SNAPSHOT:startup "
    ;;
esac
export WINUXSH_PROMPT_GIT
"#,
        )
        .unwrap();
        std::fs::write(
            path.join("plugins")
                .join("theme-minimal")
                .join("plugin.toml"),
            format!(
                r#"name = "theme-minimal"
version = {version:?}
kind = "source"
entry = "theme-minimal.plugin.winux"
summary = "Framework minimal theme fixture."
default = true
permissions = ["shell:source"]
required_binaries = []
[exports]
prompt_segments = ["prompt_char"]
themes = ["minimal"]
"#
            ),
        )
        .unwrap();
        std::fs::write(
            path.join("plugins")
                .join("theme-minimal")
                .join("theme-minimal.plugin.winux"),
            r#"[ -f "$WINUXSH/lib/prompt.winux" ] && . "$WINUXSH/lib/prompt.winux"
WINUXSH_ACTIVE_THEME=minimal
WINUXSH_PROMPT_SYMBOL="${WINUXSH_PROMPT_SYMBOL:-%}"
export WINUXSH_ACTIVE_THEME WINUXSH_PROMPT_SYMBOL
winuxsh_prompt_use_template "PLUGIN:{git}{prompt_char} " ""
"#,
        )
        .unwrap();
    }

    fn write_fake_process_hook(bin: &std::path::Path, exit_code: i32, sleep_before_exit: bool) {
        std::fs::create_dir_all(bin).unwrap();
        if cfg!(windows) {
            let path = bin.join("winuxsh-process-hook.cmd");
            let sleep = if sleep_before_exit {
                "ping -n 3 127.0.0.1 >NUL\n"
            } else {
                ""
            };
            std::fs::write(
                path,
                format!(
                    "@if not \"%WINUXSH_PROCESS_HOOK_LOG%\"==\"\" echo hook=%WINUXSH_PROCESS_PLUGIN_HOOK%;pack=%WINUXSH_PROCESS_PLUGIN_PACK%;args=%*;last=%WINUXSH_LAST_EXIT_CODE%;cmd=%WINUXSH_PREEXEC_COMMAND%;old=%WINUXSH_OLDPWD%;pwd=%WINUXSH_PWD%>>\"%WINUXSH_PROCESS_HOOK_LOG%\"\n@{}@exit /b {}\n",
                    sleep, exit_code
                ),
            )
            .unwrap();
        } else {
            let path = bin.join("winuxsh-process-hook");
            let sleep = if sleep_before_exit { "sleep 2\n" } else { "" };
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\nif [ -n \"$WINUXSH_PROCESS_HOOK_LOG\" ]; then echo \"hook=$WINUXSH_PROCESS_PLUGIN_HOOK;pack=$WINUXSH_PROCESS_PLUGIN_PACK;args=$*;last=$WINUXSH_LAST_EXIT_CODE;cmd=$WINUXSH_PREEXEC_COMMAND;old=$WINUXSH_OLDPWD;pwd=$WINUXSH_PWD\" >> \"$WINUXSH_PROCESS_HOOK_LOG\"; fi\n{}exit {}\n",
                    sleep, exit_code
                ),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&path).unwrap().permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&path, permissions).unwrap();
            }
        }
    }

    fn test_shell(hooks: HookConfig) -> Shell {
        let mut executor = Executor::new();
        executor.set_shopt_option("expand_aliases", true);
        let mut shell = Shell {
            executor,
            completion_state: Arc::new(Mutex::new(CompletionState::new(PathBuf::from(".")))),
            prompt: PromptBackend::Template(WinuxshPrompt::new(None, None, None, "default")),
            home_dir: PathBuf::from("."),
            shell_root: None,
            history_path: PathBuf::from(".winuxsh_history"),
            history_max_size: 10000,
            history_ignore_space_prefixed: false,
            history_mode: crate::config::HistoryMode::default(),
            menu_config: MenuConfig::default(),
            editor_mode: EditorMode::Emacs,
            autosuggest: AutosuggestConfig::default(),
            syntax_highlighting: SyntaxHighlightConfig::default(),
            native_widgets: NativeWidgetConfig::default(),
            native_widget_bindings: Vec::new(),
            plugins: PluginRuntimeState::default(),
            native_plugins: NativePluginConfig::default(),
            hooks,
            aliases: HashMap::new(),
            zoxide_last_tracked_dir: None,
            last_working_dir_cache_path: PathBuf::from(".winuxsh/cache/last-working-dir"),
            last_working_dir_restored: false,
            last_interactive_command: None,
            last_interactive_exit_code: None,
            line_editor: None,
            plugin_prompt_sync: PluginPromptSyncConfig::disabled(),
            process_stdin_pipeline_bridge: false,
            bash_prompt_command_running: false,
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

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn set_value(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
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
        let exe = bin.join(if cfg!(windows) {
            "zoxide.cmd"
        } else {
            "zoxide"
        });
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
        let exe = bin.join(if cfg!(windows) {
            "thefuck.cmd"
        } else {
            "thefuck"
        });
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
            format!(
                "@echo off\r\n<nul set /p ={}\r\nexit /b 0\r\n",
                selected_path
            )
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
