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
use crate::config::HistoryMode;
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
    pub history_mode: HistoryMode,
    pub history_max_size: usize,
    pub history_ignore_space_prefixed: bool,
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
        let config = load_config();

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