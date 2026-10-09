//! Shell state and execution entry point
//!
//! Wraps a `rubash::Executor` and provides the interactive shell machinery
//! (prompt, history, completion). All shell language semantics are delegated
//! to rubash; this layer only adds the Windows-facing UX.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use reedline::Reedline;
use rubash::{
    decode_to_visible_text, executor::Executor, lexer::tokenize, parser::parse, Ast, Token,
    TokenKind,
};

use crate::completion::{CompletionState, NiubashCompleter};
use crate::config::{
    load as load_config, AutosuggestConfig, EditorMode, HookConfig, MenuConfig,
    NativeWidgetBinding, NativeWidgetConfig, SyntaxHighlightConfig,
};
use crate::path_utils::{shell_home_dir, shell_path_to_host_path};
use crate::prompt::{BashPrompt, NiubashPrompt, PromptBackend};
use crate::prompt_segments::{
    SegmentId, SegmentPreset, SegmentPrompt, SegmentPromptAdapter, SegmentPromptConfig,
};

use crate::winuxcmd;

const COMPATIBLE_SHELL_PATH_ENV: &str = "NIU_COMPATIBLE_SHELL_PATH";
#[allow(dead_code)]
#[allow(dead_code)]
#[allow(dead_code)]
const NIU_RC_FILE: &str = ".niubashrc";
const NIU_COMPAT_RC_FILE: &str = ".winuxshrc";
/// Niubash-native non-interactive environment file variable. Takes precedence
/// over `BASH_ENV` so an agent shell can point at a dedicated init file.
const NIU_ENV_VAR: &str = "NIU_ENV";
/// GNU bash non-interactive environment file variable. Sourced by default for
/// non-interactive shells; kept for bash compatibility (OpenCode and other
/// agents already use BASH_ENV).
const BASH_ENV_VAR: &str = "BASH_ENV";

/// Editor outcome produced by a shell-function widget.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WidgetOutcome {
    /// Replacement buffer text; `None` keeps the buffer unchanged. An empty
    /// string is a valid replacement (it clears the line).
    pub buffer: Option<String>,
    /// Byte offset for the cursor inside the (possibly new) buffer; ignored
    /// when absent.
    pub cursor: Option<usize>,
    /// Submit the buffer after applying the outcome.
    pub accept: bool,
}

thread_local! {
    /// Interactive-REPL bridge letting the completer run shell functions in
    /// the engine during completion. reedline completers must be `Send`, so
    /// the shell is reached through this main-thread registry instead of a
    /// completer field. Installed by `build_line_editor`, main thread only.
    static COMPLETION_BRIDGE: RefCell<Option<Rc<RefCell<Shell>>>> =
        const { RefCell::new(None) };
}

/// Install the interactive shell bridge for shell-function completions
/// (`NIU_COMPDEFS`). Replaces any previous bridge for this thread.
pub fn install_completion_bridge(shell: &Rc<RefCell<Shell>>) {
    COMPLETION_BRIDGE.with(|cell| {
        *cell.borrow_mut() = Some(shell.clone());
    });
}

/// Run `f` with mutable access to the bridged shell, or `None` when no
/// bridge is installed on this thread.
pub(crate) fn with_completion_bridge<R>(f: impl FnOnce(&mut Shell) -> R) -> Option<R> {
    let rc = COMPLETION_BRIDGE.with(|cell| cell.borrow().as_ref().cloned())?;
    let mut shell = rc.borrow_mut();
    let result = f(&mut shell);
    drop(shell);
    Some(result)
}

/// Top-level shell state.
pub struct Shell {
    pub executor: Executor,
    pub completion_state: Arc<Mutex<CompletionState>>,
    pub prompt: PromptBackend,
    /// Product floor prompt (defaults-as-floor, design §14.5): the
    /// config-derived native prompt that renders only while the prompt slot
    /// is unclaimed. A claim is a non-empty PS1 in the executor environment
    /// — set by the user's own rc, an enabled external framework
    /// (oh-my-bash theme, `starship init bash`), or a runtime command.
    /// `sync_bash_prompt_from_env` swaps in the bash-compatible channel on
    /// claim and restores this floor on release; the floor itself never
    /// writes PS1, so it can never fight an active claim.
    floor_prompt: PromptBackend,
    pub home_dir: PathBuf,
    pub shell_root: Option<PathBuf>,
    pub history_path: PathBuf,
    pub history_max_size: usize,
    pub history_ignore_space_prefixed: bool,
    pub history_mode: crate::config::HistoryMode,
    pub menu_config: MenuConfig,
    pub editor_mode: EditorMode,
    /// Last observed engine `set -o emacs` / `set -o vi` flag pair, used by
    /// [`Shell::refresh_edit_mode`] to recover which of the two the user
    /// flipped last (GNU's editing-mode is one shared state; the engine
    /// stores the two options as independent flags). `None` until the first
    /// observation.
    edit_flags_seen: Option<(bool, bool)>,
    pub autosuggest: AutosuggestConfig,
    pub syntax_highlighting: SyntaxHighlightConfig,
    pub native_widgets: NativeWidgetConfig,
    pub native_widget_bindings: Vec<NativeWidgetBinding>,
    /// User-declared widget bindings parsed from `NIU_BINDKEYS` in the
    /// startup rc. Applied regardless of the native-widget pack gate.
    pub user_widget_bindings: Vec<NativeWidgetBinding>,
    /// Engine-registered `bind` bindings (registry snapshot taken after
    /// rc sourcing / at the last keymap rebuild). The reedline keymaps
    /// mirror these; `bind -x` entries trigger through the `__niu_bindx`
    /// sentinel and run via [`Shell::run_bind_x_command`].
    pub engine_bindings: Vec<rubash::shell::bind_registry::BindEntry>,
    /// Registry generation at snapshot time; compared against
    /// `executor.bind_registry_generation()` between prompts so a runtime
    /// `bind` call rebuilds the mirrored keymaps (GNU takes effect
    /// immediately; reedline keymaps are baked at editor build time).
    pub engine_bind_generation: u64,
    /// User-declared completion functions parsed from `NIU_COMPDEFS` in the
    /// startup rc: `(command, function)` pairs.
    pub compdefs: Vec<(String, String)>,
    pub hooks: HookConfig,
    pub aliases: HashMap<String, String>,
    pub last_interactive_command: Option<String>,
    pub last_interactive_exit_code: Option<i32>,
    pub line_editor: Option<Reedline>,
    process_stdin_pipeline_bridge: bool,
    bash_prompt_command_running: bool,
    // True once this shell enters the interactive REPL. Easter eggs are
    // routed only from here so `niu -c`, script files, and piped stdin stay
    // quiet and deterministic.
    pub interactive: bool,
    // --norc: do not source the interactive startup file.
    pub no_rc: bool,
    // --noprofile: do not run login-profile startup.
    pub no_profile: bool,
    // --rcfile / --init-file: alternate startup file.
    pub rc_file: Option<PathBuf>,
    // --noediting: disable readline-style line editing in the REPL.
    pub no_editing: bool,
}

pub struct StdinCurrentShellChild {
    script_name: String,
    script_path: PathBuf,
    positional_params: Vec<String>,
}

impl Shell {
    /// Construct a fresh shell: load config, install Ctrl+C handler, inject
    /// winuxcmd onto PATH, set up completion state and history.
    pub fn new() -> anyhow::Result<Self> {
        // `niu` is the sole public executable. Keep `$0` aligned with the
        // user-facing command; explicit script and `-c` names still override
        // this value at their call sites.
        Self::new_with_script_name(Some("niu"))
    }

    /// Construct a shell for scripts arriving on process stdin.
    pub fn new_for_stdin_script() -> anyhow::Result<Self> {
        Self::new_with_script_name(None)
    }

    fn new_with_script_name(script_name: Option<&str>) -> anyhow::Result<Self> {
        // 1. Load runtime defaults and environment-backed state.
        let mut config = load_config();
        config.history = config.history.with_env_overrides();
        crate::startup_trace::tick("config loaded");

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
        crate::startup_trace::tick("winuxcmd selected");
        let shell_root = prepare_shell_root(selected_winuxcmd_path.as_deref())?;
        crate::startup_trace::tick("shell root prepared");

        // 3. Build rubash Executor after host path selection.
        let shell_was_missing = std::env::var_os("SHELL").is_none();
        let default_shell_path = if cfg!(windows) && shell_was_missing {
            std::env::current_exe()
                .ok()
                .map(|exe| exe.to_string_lossy().replace('\\', "/"))
        } else {
            None
        };
        if cfg!(windows) {
            // Keep the embedded Rubash path-display contract active for the
            // entire shell lifetime; `cd` consults the process environment.
            std::env::set_var("NIU_SHELL_PATH_STYLE", "native");
            // Deprecated bridge: current rubash upstream still reads the
            // pre-rename variable; drop once rubash renames its readers.
            std::env::set_var("WINUXSH_SHELL_PATH_STYLE", "native");
        }
        if let Some(shell_path) = &default_shell_path {
            std::env::set_var("SHELL", shell_path);
        }
        let mut executor = Executor::new();
        crate::startup_trace::tick("executor created");
        // Script-mode history data plane stays with the engine: set -H,
        // fc, and the history builtin must behave in scripts exactly as
        // they do under rubash/GNU (errors10.sub: `history abcde` -> rc 2).
        // Reedline is the interactive history owner; enter_interactive()
        // turns the engine's machinery back off on the REPL path so
        // HISTFILE cannot create a second, competing history stream.
        // Niubash delegates Windows elevation to external providers such as the
        // WPM gsudo package. The experimental Rubash builtin is opt-in only.
        if std::env::var("NIU_ENABLE_RUBASH_SUDO").as_deref() != Ok("1") {
            executor.set_builtin_disabled("sudo", true);
        }
        let shell_name = std::env::var("NIU_INVOKED_AS")
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
        // Niubash is invoked through the sh/bash command shims.
        executor.set_env("STARSHIP_SHELL", "bash");
        if let Some(shell_path) = &default_shell_path {
            executor.set_env("SHELL", shell_path);
        }
        // Bash sets $BASH to the full pathname used to execute the current
        // instance (bash(1), BASH variable); scripts probe it to detect bash.
        // Forward-slash spelling matches rubash's own standalone main.rs.
        if let Ok(exe) = std::env::current_exe() {
            executor.export_env("BASH", &exe.to_string_lossy().replace('\\', "/"));
        }
        // GNU bash defaults expand_aliases on only for interactive shells
        // (shell.c init_interactive()/init_interactive_script()); -c,
        // script-file, and stdin runs keep the engine default (off).
        // enter_interactive() enables it for the REPL entry points.
        executor.set_shopt_option("expand_aliases", false);
        // P1 spawn takeover: leave the engine's external-file builtins (cat,
        // /bin/cat, mkdir, ...) at the rubash default (enabled) so script
        // suites under a POSIX PATH (=/usr/bin:/bin) resolve commands through
        // the engine's own external_command machinery exactly like rubash;
        // disabling it here used to force every such spawn through the
        // host command-not-found handler (rc=127).
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
            executor.set_env("BASH_ARGV0", script_name);
        }
        executor.set_env("NIU_PROMPT_SYMBOL", &config.shell.prompt_symbol);
        sync_executor_path_from_process_path(&mut executor);

        crate::startup_trace::tick("executor env ready");

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

        crate::startup_trace::tick("aliases");

        // 6. Prompt + theme. The template engine is the default backend;
        // the segment engine stays available for explicit configs.
        let prompt_style = config.shell.prompt_style.as_deref().unwrap_or("template");
        let prompt: PromptBackend = if prompt_style == "segments" {
            let preset_name = config.shell.segment_preset.as_deref().unwrap_or("classic");
            let preset = SegmentPreset::from_name(preset_name).unwrap_or(SegmentPreset::Classic);
            let mut seg_config =
                SegmentPromptConfig::from_preset(preset, &config.shell.prompt_symbol);
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
            let template_prompt = NiubashPrompt::new_with_symbol(
                prompt_format,
                right_prompt_format,
                config.shell.prompt_indicators.clone(),
                config.shell.prompt_symbol.clone(),
            );
            PromptBackend::Template(template_prompt)
        };

        crate::startup_trace::tick("prompt backend");

        // 7. User-local state files.
        normalize_executor_home_env(&mut executor, &home_dir);
        ensure_windows_profile_env(&mut executor, &home_dir);
        ensure_prompt_terminal_env(&mut executor);
        let history_path = config
            .history
            .path
            .clone()
            .unwrap_or_else(|| home_dir.join(".niubash_history"));
        // The history provider is infallible (niubash#134): an unopenable
        // history file — restricted-token sandbox (os error 5), path is a
        // directory, unwritable profile — degrades to an in-memory history
        // with a log::warn diagnostic. GNU bash likewise never aborts
        // startup over the history file (bashhist.c:320 load_history()
        // tolerates every read failure), so `niu -c` stays byte-stable for
        // agent one-shot use.
        let history_provider = crate::history::RubashHistoryProvider::with_file(
            config.history.max_size,
            history_path.clone(),
            config.history.mode,
        );
        executor.set_history_provider(Rc::new(RefCell::new(history_provider)));
        crate::startup_trace::tick("history provider");

        // 8. Completion state.
        let mut initial_completion_state = CompletionState::new(
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        );
        initial_completion_state.behavior = config.completion_behavior;
        let completion_state = Arc::new(Mutex::new(initial_completion_state));

        crate::startup_trace::tick("completion state");

        // 9. Load completion dirs from config (inline, not in thread).
        {
            let mut s = completion_state.lock().unwrap();
            s.load_completion_dirs(&config.completion_dirs);
        }

        let native_widgets = config.native_widgets.clone();

        let mut shell = Self {
            executor,
            completion_state,
            prompt: prompt.clone(),
            floor_prompt: prompt,
            home_dir,
            shell_root,
            history_path,
            history_max_size: config.history.max_size,
            history_ignore_space_prefixed: config.history.ignore_space_prefixed,
            history_mode: config.history.mode,
            menu_config: config.menus.with_env_overrides(),
            editor_mode: config.editor.edit_mode,
            edit_flags_seen: None,
            autosuggest: config.autosuggest.with_env_overrides(),
            syntax_highlighting: config.syntax_highlighting.with_env_overrides(),
            native_widgets,
            native_widget_bindings: Vec::new(),
            user_widget_bindings: Vec::new(),
            engine_bindings: Vec::new(),
            engine_bind_generation: 0,
            compdefs: Vec::new(),
            hooks: config.hooks,
            aliases,
            last_interactive_command: None,
            last_interactive_exit_code: None,
            line_editor: None,
            process_stdin_pipeline_bridge: false,
            bash_prompt_command_running: false,
            interactive: false,
            no_rc: false,
            no_profile: false,
            rc_file: None,
            no_editing: false,
        };
        crate::startup_trace::tick("completion + widgets");
        shell.sync_executor_pwd_from_process_cwd();
        shell.update_completion_state();
        crate::startup_trace::tick("Shell::new done");
        Ok(shell)
    }

    /// Whether the PS1 currently in the executor environment is a foreign
    /// Git Bash session value inherited from the process environment (see
    /// `enter_interactive`). Detection is a class check over the session
    /// machinery (`is_foreign_git_bash_ps1`), applied only before any user
    /// startup file has run, so the value here is provenance-guaranteed to
    /// be the inherited one.
    pub fn discard_foreign_inherited_prompt(&mut self) {
        if let Some(value) = self.executor.get_env("PS1") {
            if !value.is_empty() && crate::prompt::is_foreign_git_bash_ps1(value) {
                self.executor.unset_env("PS1");
            }
        }
    }

    pub fn enable_process_stdin_pipeline_bridge(&mut self) {
        self.process_stdin_pipeline_bridge = true;
    }

    pub fn set_script_name(&mut self, script_name: &str) {
        self.executor.set_env("__RUBASH_SCRIPT_NAME", script_name);
        self.executor.set_env("BASH_ARGV0", script_name);
    }

    pub fn stdin_current_shell_child(&self, command: &str) -> Option<StdinCurrentShellChild> {
        let tokens = tokenize(command);
        if tokens.is_empty() {
            return None;
        }
        let ast = parse(&tokens);
        let [cmd] = ast.commands.as_slice() else {
            return None;
        };
        if !is_plain_simple_command(cmd) || command_has_redirects(cmd) || cmd.words.len() < 2 {
            return None;
        }
        let Some(command_name) = cmd.words.first() else {
            return None;
        };
        let is_this_shell = matches!(command_name.as_str(), "$THIS_SH" | "${THIS_SH}")
            || self
                .executor
                .get_env("THIS_SH")
                .is_some_and(|this_sh| same_shell_dir(this_sh, command_name));
        if !is_this_shell {
            return None;
        }

        let script_name = decode_to_visible_text(&cmd.words[1]);
        let script_path = self.executor.resolve_shell_path(&script_name);
        if !script_path.is_file() {
            return None;
        }

        Some(StdinCurrentShellChild {
            script_name,
            script_path,
            positional_params: decoded_words(&cmd.words[1..]),
        })
    }

    pub fn execute_stdin_current_shell_child(
        &mut self,
        child: StdinCurrentShellChild,
        stdin_line: &str,
    ) -> anyhow::Result<i32> {
        let script = std::fs::read_to_string(&child.script_path)?;

        let saved_script_name = self
            .executor
            .get_env("__RUBASH_SCRIPT_NAME")
            .map(str::to_owned);
        let saved_bash_argv0 = self.executor.get_env("BASH_ARGV0").map(str::to_owned);
        let saved_function_stdin = self
            .executor
            .get_env("__RUBASH_FUNCTION_STDIN")
            .map(str::to_owned);
        let saved_function_stdin_offset = self
            .executor
            .get_env("__RUBASH_FUNCTION_STDIN_OFFSET")
            .map(str::to_owned);

        self.set_script_name(&child.script_name);
        self.executor.set_env("__RUBASH_FUNCTION_STDIN", stdin_line);
        self.executor.set_env("__RUBASH_FUNCTION_STDIN_OFFSET", "0");
        self.executor.set_positional_params(child.positional_params);

        let code = self.execute_script(&script)?;

        restore_executor_env(
            &mut self.executor,
            "__RUBASH_SCRIPT_NAME",
            saved_script_name,
        );
        restore_executor_env(&mut self.executor, "BASH_ARGV0", saved_bash_argv0);
        restore_executor_env(
            &mut self.executor,
            "__RUBASH_FUNCTION_STDIN",
            saved_function_stdin,
        );
        restore_executor_env(
            &mut self.executor,
            "__RUBASH_FUNCTION_STDIN_OFFSET",
            saved_function_stdin_offset,
        );

        Ok(code)
    }

    /// Mark this shell as interactive.
    ///
    /// Two consequences: easter eggs become routable, and rubash diagnostics
    /// are prefixed with the shell name (niu) without a line segment, which
    /// is what bash does for errors at an interactive prompt (GNU error.c
    /// report_prolog: interactive shells print only get_name_for_error()).
    /// The name goes to __RUBASH_SHELL_NAME — NOT __RUBASH_SCRIPT_NAME,
    /// which owns the script-path/$0 slot; polluting it here used to make
    /// interactive diagnostics read "niu: line 1: ..." like a -c run.
    pub fn enter_interactive(&mut self) {
        self.interactive = true;
        self.executor.set_env("__RUBASH_SHELL_NAME", "niu");
        // unixwin/niubash#117: a PS1 inherited from the process environment
        // (Git for Windows exports its session PS1 into every child) carries
        // machinery that only exists inside that Git Bash session — the
        // `__git_ps1` function, `$MSYSTEM`, the bracketed OSC title skeleton.
        // Discard it before the startup files run so (a) the shell's own
        // theme renders, and (b) the machinery is never expanded into a
        // per-prompt "command not found" error. Runs before any rc so a PS1
        // the user sets in their own startup file keeps full effect; `niu -c`
        // and script mode never pass through here, so env-var visibility for
        // non-interactive children stays GNU-identical.
        self.discard_foreign_inherited_prompt();
        // `$-` must contain `i` while startup files run: GNU sets
        // forced_interactive before run_startup_files (shell.c:672; flags.c:174
        // renders it in `$-`), so rc-level `case $- in *i*)` guards
        // (oh-my-bash, starship init, nvm) pass inside ~/.niubashrc. The
        // engine reads this marker in shell_option_flags (prompt_expansion.rs
        // `'i'` table arm); without it the marker only existed on the rubash
        // `-i` argv path, which the niubash REPL (no argv flags) never took.
        self.executor.set_env("__RUBASH_INTERACTIVE", "1");
        // GNU init_interactive (shell.c): interactive shells default
        // expand_aliases on so ~/.niubashrc aliases expand without a
        // user shopt line. Non-interactive entry points never call
        // this, keeping the GNU off default for scripts and -c.
        self.executor.set_shopt_option("expand_aliases", true);
        // Reedline owns interactive history (see the construction-site note):
        // disable the engine's Bash history machinery only on the REPL path
        // so HISTFILE cannot create a second, competing history stream.
        self.executor.set_shell_option("history", false);
        self.executor.unset_env("HISTFILE");
    }

    /// Route a one-command AST to an easter egg when this shell is
    /// interactive. Returns the egg's exit code, or `None` so the caller keeps
    /// ordinary command resolution.
    fn easter_egg_exit(&self, commands: &[rubash::parser::CommandNode]) -> Option<i32> {
        if !self.interactive || commands.len() != 1 {
            return None;
        }
        let words = decoded_words(&commands[0].words);
        let Some(head) = words.first() else {
            return None;
        };
        if !crate::easter_eggs::is_registered(head) {
            return None;
        }
        crate::easter_eggs::dispatch(true, &words).ok().flatten()
    }

    /// Execute a single input line via rubash. Returns the exit code.
    pub fn execute_line(&mut self, line: &str) -> anyhow::Result<i32> {
        self.execute_line_with_options(line, false)
    }

    /// unixwin/rubash#365: interactive batches that are not a single simple
    /// command run through the engine's grouped reader instead of the
    /// whole-buffer tokenize+parse fast path.
    ///
    /// GNU expands aliases while READING, one complete command at a time:
    /// the `read_token_word` tail checks `expand_aliases && quoted == 0`
    /// before `alias_expand_token` (parse.y:5756; the `quoted` flag is set
    /// by ANY backslash or quote in the word, parse.y:5321-5324 and the
    /// backslash arm 5366-5397), and the reader parses each command before
    /// executing it (shell.c reader_loop / evalstring.c parse_and_execute).
    /// A function body is therefore alias-checked once, against the table
    /// live at DEFINITION time, and never again when the body runs — so
    /// `f() { \cd "$@"; }` followed by `alias cd=f` keeps calling the
    /// builtin cd inside f, and `\`/`command` suppression is only the
    /// explicit spelling of that read-time rule.
    ///
    /// rubash's tokenize+parse fast path cannot consult the table at parse
    /// time, so the executor expands aliases at EXECUTION time against the
    /// live table (`expand_aliases_with_raw` over bodies flagged
    /// UNSTREAMED_FUNCTION_BODIES). For one simple command the table
    /// cannot change between parse and execution, so the fast path is
    /// equivalent. A batch with command separators (newline / `;` / `&&` /
    /// `||` / `&`), compound syntax, or a function definition (the grammar
    /// forces `;` or a newline before `}`) can diverge twice over: the
    /// body's words are re-checked against a LATER table (plain `cd`
    /// bodies recursed too, not just `\cd`), and the executor-level
    /// quote test (`raw_word_is_quoted`) does not count a bare backslash
    /// as quoting, so the `\cmd` suppression idiom (fnm/nvm/zoxide cd
    /// hooks) expanded and recursed f -> f -> ... until the stack
    /// overflowed (rubash#365, niu `-C` multi-line reproducer).
    ///
    /// Those batches take the same grouped driver rubash's own `-i -c`
    /// uses (`script_driver::run_script_with_history`, selected by
    /// `alias_live_at_start` in main.rs run_one_command): each command
    /// group's text is alias-expanded against the table live at read time
    /// (GNU parse-time semantics, including `\` suppression via
    /// lexer::alias_stream scan_word) and the executed result is marked
    /// `__RUBASH_ALIAS_STREAMED` so nothing expands a second time.
    /// Whitelist admission, not a symptom blacklist (AGENTS
    /// no-whack-a-mole): only input that is provably a single simple
    /// command keeps the host fast path — anything with a separator or
    /// compound token falls through to the real reader, and a false
    /// positive in the whitelist only costs the driver's speed. The
    /// winuxcmd grep shim and the Windows drive-arg AST normalizations
    /// are host fast-path extras that this engine route does not apply;
    /// single simple commands (the REPL's common case) keep them.
    fn execute_interactive_reader_batch(&mut self, script: &str, tokens: &[Token]) -> Option<i32> {
        if !self.interactive || !self.executor.alias_expansion_enabled() {
            return None;
        }
        let single_simple_command = !tokens.iter().any(|token| {
            matches!(
                token.kind,
                TokenKind::Semicolon
                    | TokenKind::And
                    | TokenKind::Or
                    | TokenKind::Background
                    | TokenKind::Keyword
            )
        });
        if single_simple_command {
            return None;
        }
        let code = rubash::script_driver::run_script_with_history(&mut self.executor, script, None);
        self.sync_process_cwd_from_executor_pwd();
        self.sync_process_path_from_executor_path();
        self.sync_alias_mirror_from_executor();
        Some(code)
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
        // rubash#365: alias-live interactive batches that are not a single
        // simple command need GNU's read-time alias semantics — see
        // execute_interactive_reader_batch.
        if let Some(code) = self.execute_interactive_reader_batch(&line, &tokens) {
            return Ok(code);
        }
        rewrite_winuxcmd_command_shims(&mut tokens, interactive_terminal_colors);

        // parse() returns Ast directly (not Result) in rubash.
        let mut ast = parse(&tokens);
        normalize_bare_windows_drive_commands(&mut ast);
        normalize_cd_windows_drive_args(&mut ast);
        normalize_winuxcmd_slash_drive_args(&mut ast);

        let mut printed_command_not_found_hints = false;
        let code = if let Some(exit) = self.easter_egg_exit(&ast.commands) {
            exit
        } else if let Some(execution) = self.execute_host_synced_simple_ast(&ast) {
            match execution {
                Ok(code) => code,
                Err(rubash::executor::ExecuteError::ExitCode(code)) => code,
                Err(rubash::executor::ExecuteError::Return(code)) => code,
                Err(rubash::executor::ExecuteError::ExpansionFailure(code)) => code,
                Err(rubash::executor::ExecuteError::FatalFunctionError(code)) => code,
                Err(rubash::executor::ExecuteError::CommandNotFound(cmd)) => {
                    eprintln!("niubash: {}: command not found", cmd);
                    self.print_command_not_found_hints(&cmd);
                    printed_command_not_found_hints = true;
                    127
                }
                Err(e) => {
                    if !is_broken_pipe_execute_error(&e) {
                        eprintln!("niubash: {}", e);
                    }
                    1
                }
            }
        } else {
            match self.executor.execute_ast(&ast) {
                Ok(()) => self.executor.last_exit_code(),
                Err(rubash::executor::ExecuteError::ExitCode(code)) => code,
                Err(rubash::executor::ExecuteError::Return(code)) => code,
                Err(rubash::executor::ExecuteError::ExpansionFailure(code)) => code,
                Err(rubash::executor::ExecuteError::FatalFunctionError(code)) => code,
                Err(rubash::executor::ExecuteError::CommandNotFound(cmd)) => {
                    eprintln!("niubash: {}: command not found", cmd);
                    self.print_command_not_found_hints(&cmd);
                    printed_command_not_found_hints = true;
                    127
                }
                Err(e) => {
                    if !is_broken_pipe_execute_error(&e) {
                        eprintln!("niubash: {}", e);
                    }
                    1
                }
            }
        };

        if code == 127 && !printed_command_not_found_hints {
            self.print_command_not_found_hints_if_missing(&ast);
        }

        self.sync_process_cwd_from_executor_pwd();
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
        self.run_postcmd_hooks(code);
        self.run_zshaddhistory_hooks(line);
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
        self.remember_interactive_command(script, code);
        let new_pwd = self.executor.get_env("PWD").map(str::to_owned);
        if let (Some(old_pwd), Some(new_pwd)) = (old_pwd, new_pwd) {
            self.run_chpwd_hooks_if_changed(&old_pwd, &new_pwd);
        }
        self.update_completion_state();
        Ok(code)
    }

    /// Source the user's REPL startup file once before the first prompt.
    ///
    /// `~/.niubashrc` is the primary interactive entry point. A pre-rename
    /// `~/.winuxshrc` is migrated once into `~/.niubashrc` (original file
    /// kept). The rc is ordinary shell code: the built-in plugin/theme
    /// stack is retired (niubash#145), so NIU_PLUGINS / NIU_THEME lines in
    /// an old rc are inert assignments the host no longer reads.
    pub fn run_startup_rc(&mut self) {
        self.run_startup_files(true);
    }

    /// niubash#180 session side: a finished `niu setup` run (a child
    /// process — the wizard cannot reach this session's executor state)
    /// leaves the one-shot marker `~/.niubash/setup-apply-pending`; when
    /// the live REPL draws its next prompt it consumes the marker and
    /// re-sources the startup rc in-process — the exact equivalent of the
    /// `source ~/.niubashrc` the finish screen names. No prompt machinery
    /// is duplicated here: the next `run_precmd_hooks` executes the fresh
    /// `PROMPT_COMMAND` and `sync_bash_prompt_from_env` re-reads `PS1`, so
    /// the new theme renders on that same prompt. The marker is consumed
    /// first (`setup_wizard::take_apply_pending_marker`), so a broken rc
    /// degrades to the honest warning instead of looping the REPL, and a
    /// hand-edited rc alone never triggers a re-source — only the wizard's
    /// own handoff does.
    pub fn apply_setup_config_if_pending(&mut self) {
        if !crate::setup_wizard::take_apply_pending_marker(&self.home_dir) {
            return;
        }
        let failed = || eprintln!("{}", crate::setup_wizard::session_apply_failed_notice());
        let Some(path) = self.startup_rc_path() else {
            failed();
            return;
        };
        if !path.is_file() {
            failed();
            return;
        }
        ensure_prompt_terminal_env(&mut self.executor);
        self.executor.set_env("NIU_REPL_STARTUP", "1");
        let outcome = self.source_file_into_current_shell(&path);
        let _ = self.execute_script("unset NIU_REPL_STARTUP");
        match outcome {
            // GNU maybe_execute_file discipline (evalfile.c:339, the same
            // contract source_startup_file implements): a failed or
            // nonzero-exiting re-source warns but never aborts the shell —
            // the session keeps running, with the old state where the new
            // rc did not take.
            Ok(0) => {
                self.sync_process_path_from_executor_path();
                self.update_completion_state();
                println!("{}", crate::setup_wizard::session_apply_notice());
            }
            Ok(code) => {
                log::warn!(
                    "{} exited with status {} during the setup re-apply",
                    path.display(),
                    code
                );
                eprintln!("{}", crate::setup_wizard::session_apply_failed_notice());
            }
            Err(err) => {
                log::warn!(
                    "{} failed during the setup re-apply: {}",
                    path.display(),
                    err
                );
                eprintln!("{}", crate::setup_wizard::session_apply_failed_notice());
            }
        }
    }

    /// Interactive startup files for a non-REPL interactive dispatch
    /// (`niu -i -c 'cmd'`, `niu -i script`). GNU shell.c: `-i` sets
    /// forced_interactive during option parsing, so run_startup_files
    /// (shell.c:1147) takes the interactive branch — rc file, not BASH_ENV —
    /// without any of the REPL-only markers.
    pub fn run_interactive_startup_rc(&mut self) {
        self.run_startup_files(false);
    }

    fn run_startup_files(&mut self, repl: bool) {
        // niubash#180 baseline: this session sources the current rc right
        // now, so a marker left by a setup that ran before this session
        // started is stale by definition — consume it here so only a setup
        // that runs while the session is live triggers the next-prompt
        // re-apply (apply_setup_config_if_pending).
        crate::setup_wizard::clear_stale_apply_pending_marker(&self.home_dir);
        normalize_executor_home_env(&mut self.executor, &self.home_dir);
        ensure_windows_profile_env(&mut self.executor, &self.home_dir);
        ensure_prompt_terminal_env(&mut self.executor);
        if self.no_profile {
            // GNU bash --noprofile skips login-profile startup; niubash has
            // no separate profile file, so skip the legacy migration pass.
        } else {
            migrate_legacy_winuxsh_rc(&self.home_dir);
        }
        // GNU shell.c:1241-1246 (bash --posix) and shell.c:1238-1239 (invoked
        // as sh, which Shell::new turns into posix mode): the interactive
        // startup file is `$ENV`, not the bash rc file, and an unset `$ENV`
        // sources nothing. --rcfile is a bashrc_file concept and is ignored
        // in this branch.
        if self.posix_mode_active() {
            self.execute_env_file("ENV");
            return;
        }
        if self.no_rc {
            return;
        }
        let Some(path) = self.startup_rc_path() else {
            return;
        };
        if !path.is_file() {
            return;
        }

        if repl {
            self.executor.set_env("NIU_REPL_STARTUP", "1");
        }
        self.source_startup_file(&path);
        if repl {
            let _ = self.execute_script("unset NIU_REPL_STARTUP");
        }

        self.update_completion_state();
        if repl {
            self.run_greeting_hooks();
        }
    }

    /// Source one startup file with GNU maybe_execute_file semantics:
    /// failures warn but never abort the shell (evalfile.c:339; the callers
    /// of run_startup_files ignore its return value).
    fn source_startup_file(&mut self, path: &Path) {
        match self.source_file_into_current_shell(path) {
            Ok(code) => {
                if code != 0 {
                    log::warn!("{} exited with status {}", path.display(), code);
                }
            }
            Err(err) => log::warn!("{} failed: {}", path.display(), err),
        }
        self.sync_process_path_from_executor_path();
    }

    /// True when this shell runs in POSIX mode. All three sources flip it
    /// before startup files run: invoked as sh/ash (Shell::new),
    /// `--posix`, and `-o posix` (ShellInvocation::apply_to_executor).
    fn posix_mode_active(&self) -> bool {
        self.executor.get_env("__RUBASH_POSIX_MODE").as_deref() == Some("1")
    }

    /// GNU shell.c:1103 execute_env_file: an unset or empty variable sources
    /// nothing; otherwise the value is expanded and the file executed with
    /// FEVAL_ENOENTOK (a missing file is silently skipped, evalfile.c:120).
    fn execute_env_file(&mut self, name: &str) {
        // GNU get_string_value reads the shell variable table, which at
        // startup-file time is the inherited environment; prefer the
        // executor table so an earlier startup file could export it.
        let raw = self
            .executor
            .get_env(name)
            .map(str::to_owned)
            .or_else(|| std::env::var(name).ok())
            .filter(|value| !value.trim().is_empty());
        let Some(raw) = raw else {
            return;
        };

        normalize_executor_home_env(&mut self.executor, &self.home_dir);
        let path = self.resolve_non_interactive_env_path(&raw);
        if !path.exists() {
            // GNU evalfile.c:120: ENOENT with FEVAL_ENOENTOK is silent.
            return;
        }
        if !path.is_file() {
            eprintln!("niu: {}: Is a directory", path.display());
            return;
        }
        self.source_startup_file(&path);
        self.update_completion_state();
    }

    /// Source the non-interactive environment file if one is configured.
    ///
    /// Mirrors GNU bash's BASH_ENV behavior (shell.c:1214-1220): a
    /// non-interactive, non-posix, bash-mode shell sources the file named by
    /// the environment variable before running its command or script. POSIX
    /// mode and sh-invoked shells source no environment file at all —
    /// `$ENV` is interactive-only under POSIX (shell.c:1244). Niubash adds a
    /// dedicated NIU_ENV variable that takes precedence over BASH_ENV in
    /// every mode, so an agent shell can point at a dedicated init file
    /// without touching the bash-compatible name.
    ///
    /// GNU's sshd/rshd special case (shell.c:1156-1180, `-c` over ssh
    /// sourcing the bashrc file) does not apply: its `SSH_CLIENT`/
    /// `SSH2_CLIENT` half is compiled out in the reference build
    /// (`config-top.h:108`, `SSH_SOURCE_BASHRC` commented out, so
    /// `run_by_ssh = 0`), and its other half needs isnetconn(stdin) — a
    /// socket on stdin, which a Windows spawn never hands the shell.
    /// Verified against the WSL GNU Bash 5.3.0 baseline: `SSH_CLIENT=...`
    /// `bash -c` sources `$BASH_ENV`, not `~/.bashrc`.
    ///
    /// Neither variable set is a no-op, preserving the zero-load fast path
    /// that keeps `niu -c` fast and deterministic. Only the user's single init
    /// file is sourced: no plugins, prompts, or completion machinery is loaded,
    /// so interactive-only content stays out of the one-shot execution path.
    pub fn source_non_interactive_env(&mut self) {
        let niu_raw = std::env::var(NIU_ENV_VAR)
            .ok()
            .filter(|value| !value.trim().is_empty());
        // GNU shell.c:1216 gates BASH_ENV on posixly_correct == 0 and
        // act_like_sh == 0; NIU_ENV stays available as the niubash extension
        // (agents invoke `niu -c`, never the sh shim).
        let bash_raw = if self.posix_mode_active() {
            None
        } else {
            std::env::var(BASH_ENV_VAR)
                .ok()
                .filter(|value| !value.trim().is_empty())
        };
        let Some(raw) = niu_raw.or(bash_raw) else {
            return;
        };

        normalize_executor_home_env(&mut self.executor, &self.home_dir);
        let path = self.resolve_non_interactive_env_path(&raw);
        if !path.exists() {
            // GNU maybe_execute_file passes FEVAL_ENOENTOK (evalfile.c:345):
            // a missing BASH_ENV file is skipped silently, keeping `-c`
            // stdout/stderr byte-stable for one-shot agents.
            return;
        }
        if !path.is_file() {
            eprintln!("niu: {}: Is a directory", path.display());
            return;
        }
        self.source_startup_file(&path);
        self.update_completion_state();
    }

    /// Resolve a non-interactive environment file path from the raw value of
    /// NIU_ENV or BASH_ENV. A leading ~ expands against the shell home
    /// directory; the remainder goes through the executor's path resolver so
    /// /c/..., C:/..., and native backslash spellings all work.
    fn resolve_non_interactive_env_path(&self, raw: &str) -> PathBuf {
        let expanded = if raw == "~" {
            self.home_dir.clone()
        } else if let Some(rest) = raw.strip_prefix("~/") {
            self.home_dir.join(rest)
        } else {
            PathBuf::from(shell_path_to_host_path(raw))
        };
        self.executor
            .resolve_shell_path(&expanded.to_string_lossy())
    }

    fn startup_rc_path(&self) -> Option<PathBuf> {
        if let Some(file) = &self.rc_file {
            return Some(file.clone());
        }
        let primary = self.home_dir.join(NIU_RC_FILE);
        if primary.is_file() {
            return Some(primary);
        }
        let compat = self.home_dir.join(NIU_COMPAT_RC_FILE);
        if compat.is_file() {
            return Some(compat);
        }
        None
    }

    /// Whether the prompt slot is currently claimed (defaults-as-floor,
    /// design §14.5). The claim token is a non-empty `PS1` — the same token
    /// GNU bash renders (`parse.y:6150 prompt_again` reads PS1 fresh every
    /// render), so whoever sets PS1 last owns the prompt: the user's rc, an
    /// enabled external framework, or a runtime command. `PROMPT_COMMAND`
    /// is deliberately NOT a claim: it is a pre-prompt hook (the engine
    /// executes it regardless of the prompt string), and hook-only users —
    /// oh-my-bash's `history` plugin, title setters — must not displace the
    /// product floor. A hook that intends to own the prompt sets PS1 itself
    /// (starship's `starship_precmd` does exactly that), and because
    /// `run_precmd_hooks` runs the hook before syncing, the claim is already
    /// visible when the backend is chosen.
    fn prompt_slot_claimed(&self) -> bool {
        self.executor
            .get_env("PS1")
            .is_some_and(|value| !value.is_empty())
    }

    /// Execute `PROMPT_COMMAND` through the engine and report whether the
    /// run ended in a TOP-LEVEL EXIT JUMP (niubash#191): the engine re-arms
    /// `exit_jump_pending` when the PC text raised one (rubash#433,
    /// evalstring.c:396-403 + :618-619 — parse_and_execute catches
    /// EXITPROG/ERREXIT at its own setjmp and RE-RAISES after the `out:`
    /// cleanup), exactly like GNU's jump unwinding execute_variable_command
    /// into reader_loop. The caller (the REPL's pre-prompt hook) must consume
    /// the jump and end the session instead of rendering another prompt. The
    /// returned status lives in `last_exit_code`: parse.y:7313/:7315's
    /// normal-return restore is skipped when the jump unwinds, so `exit 5`
    /// keeps rc 5. A plain failing PC or a parse error re-arms nothing: the
    /// caller restores the pre-PC `$?` and prompts again.
    fn run_bash_prompt_command(&mut self, last_exit_code: i32) -> bool {
        if self.bash_prompt_command_running {
            return false;
        }

        self.bash_prompt_command_running = true;
        ensure_prompt_terminal_env(&mut self.executor);
        self.executor.set_last_exit_code(last_exit_code);
        // The engine owns the GNU semantics (eval.c:305
        // execute_prompt_command): indexed-array elements in order,
        // associative arrays refused, scalar string once. The host only
        // triggers the pre-prompt hook; PROMPT_COMMAND itself may be an
        // array under bash >= 5.1 (oh-my-bash installs one), which the
        // engine dispatches without the host re-implementing it.
        self.executor.execute_prompt_command();
        let exit_jump = self.executor.take_exit_jump_pending();
        if !exit_jump {
            // Normal return: the pre-PC `$?` is restored (the engine already
            // rolled its own snapshot back; this keeps the host view in
            // sync). On the jump path the restore is skipped — the jump's
            // status IS the session status (parse.y:3021's restore line is
            // longjmped past in GNU).
            self.executor.set_last_exit_code(last_exit_code);
        }
        self.bash_prompt_command_running = false;
        exit_jump
    }

    fn sync_bash_prompt_from_env(&mut self) {
        if self.prompt_slot_claimed() {
            let ps1 = self.executor.get_env("PS1").unwrap_or("\\$ ").to_string();
            let ps2 = self.executor.get_env("PS2").unwrap_or("> ").to_string();
            let left = self.executor.expand_prompt_string_mut(&ps1);
            let multiline = self.executor.expand_prompt_string_mut(&ps2);
            self.prompt = PromptBackend::Bash(BashPrompt::new(left, multiline));
        } else if !matches!(
            self.prompt,
            PromptBackend::Template(_) | PromptBackend::Segments(_)
        ) {
            // Claim released (PS1 unset/emptied — framework disabled, theme
            // switched off, plain `unset PS1`): the product floor renders
            // again. GNU renders an empty prompt in this case (parse.y:6150
            // maps a missing PS1 to ""); niubash's contract is stronger —
            // the native floor is the floor (§14.5), so releasing the slot
            // restores it instead of freezing the last claimed face.
            self.prompt = self.floor_prompt.clone();
        }
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
        // GNU writes the decoded PS0 to stderr (eval.c:164-176 fprintf),
        // never stdout: `2>/dev/null` drops the bytes, `2>&1` captures them,
        // and redirected stdout stays clean (unixwin/niubash#190).
        if !rendered.is_empty() {
            shell_channel_write(ShellChannel::Stderr, rendered.as_bytes());
        }
        self.executor.set_last_exit_code(last_exit_code);
    }

    /// niubash#170 theme-preview channel: run the interactive pre-prompt
    /// pipeline once — the native hooks plus `PROMPT_COMMAND` through the
    /// engine's `execute_prompt_command` (exactly [`Shell::run_precmd_hooks`])
    /// — then print the rendered PS1 between the preview markers. The
    /// expansion is the same `expand_prompt_string_mut` call
    /// `sync_bash_prompt_from_env` makes for every interactive prompt, so a
    /// gallery preview shows the face the session would actually draw.
    ///
    /// Gated by `plugins::theme_preview::PRINT_RENDERED_PS1_ENV` on a
    /// `niu -c` child (`src/main.rs` -c route): the theme-gallery preview
    /// sources the theme's managed block in a throwaway child and reads the
    /// marked bytes. When no PS1 is claimed — the block failed, or the theme
    /// set no prompt — nothing is printed and the parent degrades the
    /// preview; a claimed-but-foreign PS1 cannot occur because the preview
    /// child strips PS1/PROMPT_COMMAND from its environment.
    pub fn print_rendered_prompt_for_preview(&mut self) {
        self.run_precmd_hooks();
        let Some(ps1) = self
            .executor
            .get_env("PS1")
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
        else {
            return;
        };
        let rendered = self.executor.expand_prompt_string_mut(&ps1);
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(crate::plugins::theme_preview::PS1_BEGIN_MARKER.as_bytes());
        let _ = out.write_all(b"\n");
        let _ = out.write_all(rendered.as_bytes());
        let _ = out.write_all(b"\n");
        let _ = out.write_all(crate::plugins::theme_preview::PS1_END_MARKER.as_bytes());
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }

    fn source_file_into_current_shell(&mut self, path: &Path) -> anyhow::Result<i32> {
        let shell_path = host_path_to_shell_path(&path.to_string_lossy());
        self.execute_script(&format!(". {}", shell_quote(&shell_path)))
    }

    /// Run native hooks before rendering the next prompt.
    ///
    /// Returns whether the run ended in a top-level EXIT JUMP (niubash#191):
    /// `PROMPT_COMMAND` raised `exit`/an errexit unwind, the engine re-armed
    /// the jump (rubash#433), and the driving REPL must end the session with
    /// `last_exit_code` instead of rendering another prompt — GNU's
    /// jump_to_top_level unwinds reader_loop directly (evalstring.c:618-619).
    /// `false` (the common case) means the session continues.
    pub fn run_precmd_hooks(&mut self) -> bool {
        let last_exit_code = self.executor.last_exit_code();
        let hooks = self.hooks.precmd.clone();
        let last_exit_code_string = last_exit_code.to_string();
        // Set in process env so segment prompt can read it via std::env::var.
        std::env::set_var("NIU_LAST_EXIT_CODE", &last_exit_code_string);
        let context = [("NIU_LAST_EXIT_CODE", last_exit_code_string)];
        self.run_hook_scripts(&hooks, &context);
        let pc_exit_jump = self.run_bash_prompt_command(last_exit_code);
        if pc_exit_jump {
            // The unwind skips everything a normal pre-prompt pass would
            // still do (prompt re-sync, title hooks): GNU longjmps out of
            // execute_variable_command into the reader's top level.
            return true;
        }
        self.sync_bash_prompt_from_env();
        let title = self.resolve_title_value();
        self.run_title_hooks(&title);
        false
    }

    /// Compute the current title for `run_title_hooks`.
    ///
    /// Resolution order: `NIU_TITLE` env var (set by plugins or users) wins,
    /// then the current `PWD` path, then a plain dot. Title hooks may use this
    /// value to write a terminal title via OSC escape sequences.
    fn resolve_title_value(&self) -> String {
        if let Some(title) = self.executor.get_env("NIU_TITLE") {
            if !title.is_empty() {
                return title.to_string();
            }
        }
        self.executor
            .get_env("PWD")
            .map(str::to_owned)
            .unwrap_or_else(|| ".".to_string())
    }

    /// Load user widget bindings from the `NIU_BINDKEYS` variable declared in
    /// the startup rc. Each line is `keyspec:widget`; unknown widget names
    /// become shell-function widgets at trigger time. Called after the rc has
    /// been sourced and before the line editor is built.
    pub fn load_user_widget_bindings(&mut self) {
        let value = self
            .executor
            .get_env("NIU_BINDKEYS")
            .map(str::to_owned)
            .unwrap_or_default();
        self.user_widget_bindings = crate::repl::parse_user_bindkeys(&value);
    }

    /// Re-resolve the live editing mode from the engine's `set -o emacs` /
    /// `set -o vi` flags. Returns `Some(new_mode)` when the effective mode
    /// changed (the REPL rebuilds the line editor for it), `None` otherwise.
    ///
    /// GNU semantics being modeled: `set -o emacs` and `set -o vi` are both
    /// routed to one editing-mode state — bash's o_options table
    /// (builtins/set.def:200, set.def:235) gives both entries
    /// `set_edit_mode` (builtins/set.def:424), which calls
    /// `rl_variable_bind("editing-mode", ...)` (set.def:427); readline binds
    /// that variable to `sv_editmode` (lib/readline/bind.c:2001), which
    /// swaps the active keymap immediately (bind.c:2092
    /// `_rl_keymap = vi_insertion_keymap` / bind.c:2104
    /// `emacs_standard_keymap`). The switch is therefore live and
    /// mid-session: it takes effect at the next prompt, and the last
    /// `set -o` wins. Neither option is listed in SHELLOPTS
    /// (rubash set/options.rs `shellopts_includes_option` mirrors GNU's
    /// exclusion), so the flags live in the engine env under
    /// `__RUBASH_SETOPT_<name>` (rubash set/options.rs `shell_option_key`).
    ///
    /// Because the engine keeps the two flags independent while GNU keeps
    /// one state, the last writer is recovered by diffing against the
    /// previously observed pair, and the resolved state is written back
    /// exclusively (the winner's flag on, the other off) — without that
    /// normalization the pair would freeze at both-on after
    /// `set -o emacs` (vi stays flagged on), and a later `set -o vi` could
    /// never be observed as a change. The write goes through the sanctioned
    /// `Executor::set_shell_option` (the same mutator the `set` builtin
    /// uses); emacs/vi are excluded from SHELLOPTS, so the write touches
    /// only the two option keys and makes `set -o` listings exclusive the
    /// way GNU reports them. A pair where both flags turned on in one
    /// inter-prompt gap (`set -o emacs; set -o vi` on a single command
    /// line) is unrecoverable and resolves to vi — the canonical bashrc
    /// idiom ends there, and re-asserting emacs is one explicit command
    /// away. GNU's `set +o <active mode>` disables line editing entirely
    /// (set.def:433-441 `no_line_editing`); that path is not wired here yet
    /// — a disable-only flip keeps the current mode instead (documented
    /// follow-up, niubash#184).
    ///
    /// inputrc dependency (niubash#185's lane): GNU also seeds the editing
    /// mode from readline's `set editing-mode` in INPUTRC (~/.inputrc,
    /// bound.c:2992), which this product does not read yet. Until that
    /// surface lands, the startup mode comes from the engine flags alone;
    /// when #185 adds an inputrc reader, its result must feed the same
    /// initial observation (the `None` branch below) rather than a second
    /// resolver.
    pub fn refresh_edit_mode(&mut self) -> Option<EditorMode> {
        // Value check mirrors rubash set/options.rs shell_option_enabled:
        // the key holds "1"/"0" and a missing key falls back to the option
        // default (both emacs and vi default to off).
        let emacs_on = self.executor.get_env("__RUBASH_SETOPT_emacs") == Some("1");
        let vi_on = self.executor.get_env("__RUBASH_SETOPT_vi") == Some("1");
        let now = (emacs_on, vi_on);
        let resolved = match self.edit_flags_seen.replace(now) {
            // First observation (startup, after the rc has run): the bashrc
            // idiom is `set -o vi`; an emacs-only flag or the all-off
            // default keeps the configured mode.
            None => {
                if vi_on {
                    EditorMode::Vi
                } else if emacs_on {
                    EditorMode::Emacs
                } else {
                    return None;
                }
            }
            Some(previous) if previous == now => return None,
            Some((emacs_was, vi_was)) => {
                match (emacs_on && !emacs_was, vi_on && !vi_was) {
                    // Only disables (or no-op `set +o` on the inactive
                    // option) — no new writer, keep the current mode.
                    (false, false) => return None,
                    (true, false) => EditorMode::Emacs,
                    (false, true) => EditorMode::Vi,
                    // Both flags flipped within one gap: order unknowable,
                    // resolved toward vi (see the doc comment above).
                    (true, true) => EditorMode::Vi,
                }
            }
        };
        self.normalize_edit_mode(resolved);
        if resolved == self.editor_mode {
            None
        } else {
            self.editor_mode = resolved;
            Some(resolved)
        }
    }

    /// Write `mode` back to the engine flags exclusively (GNU models emacs
    /// and vi as views of one editing-mode state, so exactly one is on) and
    /// sync the observed-pair cache to the written state.
    fn normalize_edit_mode(&mut self, mode: EditorMode) {
        let pair = match mode {
            EditorMode::Emacs => (true, false),
            EditorMode::Vi => (false, true),
        };
        self.executor.set_shell_option("emacs", pair.0);
        self.executor.set_shell_option("vi", pair.1);
        self.edit_flags_seen = Some(pair);
    }

    /// Snapshot the engine's `bind` registry for the line editor
    /// (niubash#185). Called after the rc has been sourced and before the
    /// line editor is built, and again whenever the registry generation
    /// moved so a runtime `bind` call mirrors into the next prompt.
    pub fn load_engine_bindings(&mut self) {
        self.engine_bindings = self.executor.bind_registry_snapshot();
        self.engine_bind_generation = self.executor.bind_registry_generation();
    }

    /// Run an engine `bind -x` command for a keypress and produce the
    /// editor outcome (niubash#185).
    ///
    /// The engine implements GNU's protocol (bashline.c:4593
    /// bash_execute_unix_command): the command runs with
    /// READLINE_LINE/READLINE_POINT/READLINE_MARK set (read back and
    /// unbound afterwards); a changed READLINE_LINE replaces the buffer
    /// (maybe_make_readline_line, bashline.c:2808) and READLINE_POINT —
    /// a CHARACTER offset, possibly the pre-change one when the command
    /// did not set it — lands the cursor, clamped to the buffer.
    ///
    /// `cursor_byte` is the reedline insertion point (byte offset) in
    /// `buffer`; read-back offsets convert back to byte offsets. A
    /// `bind -x` keypress never submits the line (GNU returns to
    /// editing), so `accept` is always false.
    pub fn run_bind_x_command(
        &mut self,
        index: usize,
        buffer: &str,
        cursor_byte: usize,
    ) -> WidgetOutcome {
        let Some(entry) = self.engine_bindings.get(index) else {
            return WidgetOutcome::default();
        };
        let rubash::shell::bind_registry::BindKind::Execute { command } = &entry.kind else {
            return WidgetOutcome::default();
        };
        let command = command.clone();
        let safe_cursor = buffer
            .char_indices()
            .map(|(offset, _)| offset)
            .filter(|offset| *offset <= cursor_byte)
            .max()
            .unwrap_or(0);
        let old_point = buffer[..safe_cursor].chars().count();
        let outcome = self
            .executor
            .run_bind_x_command(&command, buffer, old_point, old_point);

        // Read-back semantics (bashline.c:4692-4703).
        let Some(new_line) = outcome.line else {
            // Command unset READLINE_LINE: keep the editor as it stood.
            return WidgetOutcome::default();
        };
        let line_changed = new_line != buffer;
        let base = if line_changed { &new_line } else { buffer };
        // An unchanged READLINE_POINT holds the point we set before the
        // command ran, which is exactly GNU's final cursor after
        // maybe_make_readline_line + the point override.
        let point_chars = outcome.point.unwrap_or(old_point);
        let clamped = point_chars.min(base.chars().count());
        let cursor_byte = crate::repl::char_offset_to_byte(base, clamped);
        WidgetOutcome {
            buffer: line_changed.then_some(new_line),
            cursor: Some(cursor_byte),
            accept: false,
        }
    }

    /// True when the named widget function exists in the shell engine.
    pub fn widget_function_available(&self, name: &str) -> bool {
        self.executor.has_function(name)
    }

    /// Load user completion functions from the `NIU_COMPDEFS` variable
    /// declared in the startup rc. Each line is `command:function`.
    pub fn load_user_compdefs(&mut self) {
        let value = self
            .executor
            .get_env("NIU_COMPDEFS")
            .map(str::to_owned)
            .unwrap_or_default();
        self.compdefs = value
            .lines()
            .map(str::trim)
            .filter(|entry| !entry.is_empty() && !entry.starts_with('#'))
            .filter_map(|entry| {
                let (command, function) = entry.split_once(':')?;
                let command = command.trim();
                let function = function.trim();
                if command.is_empty() || function.is_empty() {
                    return None;
                }
                Some((command.to_string(), function.to_string()))
            })
            .collect();
    }

    /// Run a shell-function completion (`compdef`) and collect its
    /// candidates.
    ///
    /// The function sees the command line through `NIU_COMP_WORDS` (space
    /// joined) and `NIU_COMP_CWORD` (bash COMP_CWORD semantics). It writes
    /// back `NIU_COMP_RESULT` with one candidate per line, shaped
    /// `value` or `value<TAB>description`. Compdef functions must not print
    /// to stdout: completion runs while the line editor owns the terminal.
    /// Output variables are unset again before returning.
    pub fn run_compdef_function(
        &mut self,
        function: &str,
        words: &[String],
        cword: usize,
    ) -> Vec<(String, Option<String>)> {
        let joined = words.join(" ");
        if let Err(err) = self.executor.call_function_with_env(
            function,
            std::iter::empty::<String>(),
            [
                ("NIU_COMP_WORDS", joined.as_str()),
                ("NIU_COMP_CWORD", cword.to_string().as_str()),
            ],
        ) {
            log::warn!("completion function '{}' failed: {}", function, err);
        }

        let result = self.executor.get_env("NIU_COMP_RESULT").map(str::to_owned);
        let _ = self.execute_script("unset NIU_COMP_WORDS NIU_COMP_CWORD NIU_COMP_RESULT");

        result
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| match line.split_once('\t') {
                Some((value, description)) => (
                    value.trim().to_string(),
                    Some(description.trim().to_string()),
                ),
                None => (line.to_string(), None),
            })
            .collect()
    }

    /// Run a shell-function widget and collect its editor outcome.
    ///
    /// The function sees the current editor state through `NIU_WIDGET_BUFFER`
    /// and `NIU_WIDGET_CURSOR`. It may write back `NIU_WIDGET_RESULT` (a
    /// replacement buffer; setting it to the empty string clears the line),
    /// `NIU_WIDGET_CURSOR_RESULT` (byte offset in the new buffer), and
    /// `NIU_WIDGET_ACCEPT=1` (submit the buffer afterwards). All widget
    /// variables are unset again before returning.
    pub fn run_widget_function(
        &mut self,
        function: &str,
        buffer: &str,
        cursor: usize,
    ) -> WidgetOutcome {
        if let Err(err) = self.executor.call_function_with_env(
            function,
            std::iter::empty::<String>(),
            [
                ("NIU_WIDGET_BUFFER", buffer),
                ("NIU_WIDGET_CURSOR", cursor.to_string().as_str()),
            ],
        ) {
            log::warn!("widget function '{}' failed: {}", function, err);
        }

        let result = self
            .executor
            .get_env("NIU_WIDGET_RESULT")
            .map(str::to_owned);
        let accept = self
            .executor
            .get_env("NIU_WIDGET_ACCEPT")
            .map(str::to_owned);
        let cursor_result = self
            .executor
            .get_env("NIU_WIDGET_CURSOR_RESULT")
            .and_then(|value| value.trim().parse::<usize>().ok());

        let _ = self
            .execute_script("unset NIU_WIDGET_RESULT NIU_WIDGET_ACCEPT NIU_WIDGET_CURSOR_RESULT");

        WidgetOutcome {
            buffer: result,
            cursor: cursor_result,
            accept: accept.as_deref() == Some("1"),
        }
    }

    /// Run native hooks immediately before the user's interactive command.
    pub fn run_preexec_hooks(&mut self, command: &str) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        let hooks = self.hooks.preexec.clone();
        let context = [("NIU_PREEXEC_COMMAND", command.to_string())];
        self.run_hook_scripts(&hooks, &context);
        self.run_bash_ps0_preexec();
    }

    /// Run native hooks when the interactive command changed directories.
    pub fn run_chpwd_hooks_if_changed(&mut self, old_pwd: &str, new_pwd: &str) {
        if same_shell_dir(old_pwd, new_pwd) {
            return;
        }
        let hooks = self.hooks.chpwd.clone();
        let context = [
            ("NIU_OLDPWD", old_pwd.to_string()),
            ("NIU_PWD", new_pwd.to_string()),
        ];
        self.run_hook_scripts(&hooks, &context);
    }

    /// Run postcmd hooks after command execution.
    pub fn run_postcmd_hooks(&mut self, exit_code: i32) {
        let hooks = self.hooks.postcmd.clone();
        let exit_code_string = exit_code.to_string();
        let context = [("NIU_LAST_EXIT_CODE", exit_code_string)];
        self.run_hook_scripts(&hooks, &context);
    }

    /// Run zshaddhistory hooks after command is added to history.
    pub fn run_zshaddhistory_hooks(&mut self, command: &str) {
        let hooks = self.hooks.zshaddhistory.clone();
        let context = [("NIU_HISTORY_COMMAND", command.to_string())];
        self.run_hook_scripts(&hooks, &context);
    }

    /// Run zshexit hooks when shell exits.
    pub fn run_zshexit_hooks(&mut self) {
        let hooks = self.hooks.zshexit.clone();
        self.run_hook_scripts(&hooks, &[]);
    }

    /// Run greeting hooks at startup.
    pub fn run_greeting_hooks(&mut self) {
        let hooks = self.hooks.greeting.clone();
        self.run_hook_scripts(&hooks, &[]);
    }

    /// Run title hooks to set terminal title.
    pub fn run_title_hooks(&mut self, title: &str) {
        let hooks = self.hooks.title.clone();
        let context = [("NIU_TITLE", title.to_string())];
        self.run_hook_scripts(&hooks, &context);
    }

    fn print_command_not_found_hints_if_missing(&self, ast: &Ast) {
        let Some(command) = single_command_word(ast) else {
            return;
        };
        let command = decode_to_visible_text(command);
        if resolve_native_command_path(&command).is_some() {
            return;
        }

        self.print_command_not_found_hints(&command);
    }

    fn print_command_not_found_hints(&self, command: &str) {
        for line in native_command_not_found_hint_lines(command, |candidate| {
            resolve_native_command_path(candidate).is_some()
        }) {
            eprintln!("{}", line);
        }
    }

    fn sync_alias_mirror_from_executor(&mut self) {
        self.aliases = self.executor.aliases_snapshot();
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
        let mut completer = NiubashCompleter::new(self.completion_state.clone());
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
        self.run_zshexit_hooks();
        match self.executor.run_exit_trap_with_status(status) {
            Ok(code) => Ok(code),
            Err(rubash::executor::ExecuteError::ExitCode(code)) => Ok(code),
            Err(e) => {
                if !is_broken_pipe_execute_error(&e) {
                    eprintln!("niubash: {}", e);
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

        // P2: keep the engine's history data plane active for scripts so
        // `set -H`/`set -o history`, fc, and the history builtin behave as
        // under rubash/GNU. The interactive path still disables it in
        // enter_interactive() (reedline owns REPL history).
        //
        // Scripts that turn history expansion or aliases on must run through
        // the engine's GNU line-group reader (script_driver
        // run_script_with_history: per-group `!!`/`!str` expansion and
        // recording), the same routing as rubash's main.rs — tokenize+parse
        // +execute_ast would lose pre_process_line semantics entirely.
        let script = normalize_native_windows_path_literals(script);
        // unixwin/niubash#160: GNU parses every command string and script
        // through the read-parse-execute loop, and an unterminated quote /
        // backtick / `${` / `$(` / heredoc delimiter is a READ-TIME EOF
        // diagnostic (parse.y:5419-5437 read_token_word -> yyerror
        // "unexpected EOF while looking for matching X", exit status 2,
        // error.c:324) — `bash -c 'echo "x'` prints the diagnostic and
        // exits 2 without running anything. The tokenize+parse fast path
        // below folds an unclosed quote into the word and executed
        // `echo "x` as `echo x` with rc=0, silently. Fast-path admission
        // is inverted to a whitelist (AGENTS no-whack-a-mole move 2): only
        // provably-closed input keeps the local route; read-time EOF
        // shapes fall through to the engine's real driver. The gate below
        // is the engine's own run_source_impl gate, verbatim (script_
        // driver.rs:2017-2076): the heredoc-delimiter arm runs first and
        // unconditionally; the generic unclosed arm excludes `<<`-bearing
        // text (heredoc bodies are literal data, and a closed delimiter
        // after `<< "q` … is judged by arm 1 only). Both predicates are
        // owned by the engine (script_driver / lexer), so this adds no
        // second scanner — a false positive merely runs the engine's
        // normal driver. Interactive input keeps the REPL continuation
        // route — GNU interactive shells prompt PS2 instead of failing the
        // read (the engine gate mirrors this with `!interactive`).
        let parse_posix = self.executor.get_env("__RUBASH_POSIX_MODE").as_deref() == Some("1");
        if !self.interactive
            && (rubash::script_driver::heredoc_delimiter_unclosed_quote(&script).is_some()
                || (rubash::lexer::has_unclosed_input_syntax_posix(&script, parse_posix)
                    && !script.contains("<<")))
        {
            let code = rubash::script_driver::run_source_with_line_offset(
                &mut self.executor,
                &script,
                false,
                0,
                None,
                None,
            );
            self.sync_process_cwd_from_executor_pwd();
            self.sync_process_path_from_executor_path();
            self.sync_alias_mirror_from_executor();
            return Ok(code);
        }
        if !self.interactive
            && (rubash::script_driver::script_uses_history(&script)
                || rubash::script_driver::script_uses_aliases(
                    &script,
                    // rubash#414: parse-time alias state established before
                    // the reader starts (CLI -O expand_aliases / posix mode)
                    // must route through the line-group driver; the executor's
                    // own live flag is the truth source (same argument rubash
                    // main.rs passes at its script site).
                    self.executor.alias_expansion_enabled(),
                ))
        {
            let code =
                rubash::script_driver::run_script_with_history(&mut self.executor, &script, None);
            self.sync_process_cwd_from_executor_pwd();
            self.sync_process_path_from_executor_path();
            self.sync_alias_mirror_from_executor();
            return Ok(code);
        }
        let mut tokens = tokenize(&script);
        if tokens.is_empty() {
            return Ok(0);
        }
        // rubash#365: the interactive leg (REPL multi-line paste, and any
        // interactive multi-command batch) needs GNU's read-time alias
        // semantics for the same reasons as the line route above; the
        // non-interactive legs above already use this driver for
        // alias/history-bearing scripts.
        if let Some(code) = self.execute_interactive_reader_batch(&script, &tokens) {
            return Ok(code);
        }
        rewrite_winuxcmd_command_shims(&mut tokens, interactive_terminal_colors);

        let mut ast = parse(&tokens);
        normalize_bare_windows_drive_commands(&mut ast);
        normalize_cd_windows_drive_args(&mut ast);
        normalize_winuxcmd_slash_drive_args(&mut ast);
        self.inject_process_stdin_for_rewritten_pipeline(&mut ast)?;

        let execution = if let Some(exit) = self.easter_egg_exit(&ast.commands) {
            Ok(exit)
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
            Err(rubash::executor::ExecuteError::ExpansionFailure(code)) => code,
            Err(rubash::executor::ExecuteError::FatalFunctionError(code)) => code,
            Err(rubash::executor::ExecuteError::CommandNotFound(cmd)) => {
                eprintln!("niubash: {}: command not found", cmd);
                127
            }
            Err(e) => {
                if !is_broken_pipe_execute_error(&e) {
                    eprintln!("niubash: {}", e);
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

        stage.heredoc = Some(input);
        stage.heredoc_delimiter = Some("NIU_PROCESS_STDIN".to_string());
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

fn restore_executor_env(executor: &mut Executor, name: &str, value: Option<String>) {
    match value {
        Some(value) => executor.set_env(name, &value),
        None => executor.unset_env(name),
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
        if let Some(normalized) = cd_tilde_path_to_slash_drive(word)
            .or_else(|| windows_drive_path_to_slash_drive(word))
            .or_else(|| slash_drive_path_to_windows_native(word))
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

#[cfg(test)]
fn niubash_builtin_words(command: &rubash::parser::CommandNode) -> Option<(&str, &[String])> {
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

/// Decode rubash transport words to user-visible text for host-side
/// consumers that read `ast.words`/`token.value` directly (native plugins,
/// process plugins, script dispatch). The executor decodes carriers at its
/// own argv boundary; host code must use the public decoder instead of
/// touching carrier bytes itself.
fn decoded_words(words: &[String]) -> Vec<String> {
    words
        .iter()
        .map(|word| decode_to_visible_text(word))
        .collect()
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

    let Some(command_name) = command.words.first().cloned() else {
        return;
    };
    if !is_winuxcmd_path_command(&command_name) {
        return;
    }

    let translate = winuxcmd_path_translation_mask(&command_name, &command.words);
    for (index, word) in command.words.iter_mut().enumerate() {
        if index == 0 || !translate.get(index).copied().unwrap_or(true) {
            continue;
        }
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

/// Per-argument mask controlling which WinuxCmd command arguments may be
/// slash-drive translated. Pattern, regex, or script operands of `grep`,
/// `sed`, `awk`, and `find` are left untouched so a `/x/`-shaped pattern is
/// not rewritten into a Windows drive path (unixwin/niubash#61).
fn winuxcmd_path_translation_mask(command_name: &str, words: &[String]) -> Vec<bool> {
    let mut mask = vec![true; words.len()];
    if let Some(first) = mask.first_mut() {
        *first = false;
    }

    let name = command_name.trim_end_matches(".exe").to_ascii_lowercase();
    match name.as_str() {
        "grep" => mask_first_positional_pattern(
            words,
            &mut mask,
            &["-e", "--regexp"],
            &["-f", "--file"],
            &[
                "-m",
                "--max-count",
                "-A",
                "--after-context",
                "-B",
                "--before-context",
                "-C",
                "--context",
                "-d",
                "--devices",
                "-D",
                "--directories",
            ],
        ),
        "sed" => mask_first_positional_pattern(
            words,
            &mut mask,
            &["-e", "--expression"],
            &["-f", "--file"],
            &[],
        ),
        "awk" => mask_first_positional_pattern(words, &mut mask, &[], &["-f", "--file"], &[]),
        "find" => mask_find_expression(words, &mut mask),
        _ => {}
    }
    mask
}

fn short_option_char(opts: &[&str]) -> Option<char> {
    opts.iter().find_map(|opt| {
        let bytes = opt.as_bytes();
        if bytes.len() == 2 && bytes[0] == b'-' && bytes[1].is_ascii_alphabetic() {
            Some(bytes[1] as char)
        } else {
            None
        }
    })
}

/// Marks the first positional operand of a pattern/script command as
/// non-translatable. `pattern_opts` consume a separate pattern value,
/// `file_opts` consume a separate file operand, and `value_opts` consume a
/// numeric or action value; any of the first two suppresses the positional
/// pattern. Attached `--opt=value` and `-oVALUE` clusters carry the value in
/// the same token and only gate the positional operand.
fn mask_first_positional_pattern(
    words: &[String],
    mask: &mut [bool],
    pattern_opts: &[&str],
    file_opts: &[&str],
    value_opts: &[&str],
) {
    let pattern_short = short_option_char(pattern_opts);
    let file_short = short_option_char(file_opts);

    let mut pattern_via_option = false;
    let mut after_dashdash = false;
    let mut first_positional: Option<usize> = None;
    let mut i = 1;
    while i < words.len() {
        let arg = words[i].as_str();
        if !after_dashdash && arg == "--" {
            after_dashdash = true;
            i += 1;
            continue;
        }
        let is_option = !after_dashdash && arg.starts_with('-') && arg.len() > 1;

        if is_option {
            if pattern_opts.contains(&arg) {
                pattern_via_option = true;
                if let Some(slot) = mask.get_mut(i + 1) {
                    *slot = false;
                }
                i += 2;
                continue;
            }
            if file_opts.contains(&arg) {
                pattern_via_option = true;
                i += 2;
                continue;
            }
            if value_opts.contains(&arg) {
                i += 2;
                continue;
            }
            if let Some(name) = arg
                .strip_prefix("--")
                .and_then(|rest| rest.split('=').next())
            {
                let long = format!("--{name}");
                if pattern_opts.contains(&long.as_str()) || file_opts.contains(&long.as_str()) {
                    pattern_via_option = true;
                }
            } else if let Some(cluster) = arg.strip_prefix('-') {
                if let Some(first) = cluster.chars().next() {
                    if Some(first) == pattern_short || Some(first) == file_short {
                        pattern_via_option = true;
                    }
                }
            }
        } else if first_positional.is_none() {
            first_positional = Some(i);
        }
        i += 1;
    }

    if !pattern_via_option {
        if let Some(idx) = first_positional {
            if let Some(slot) = mask.get_mut(idx) {
                *slot = false;
            }
        }
    }
}

/// Marks `find` pattern-primary values (`-name`, `-path`, `-regex`, ...) as
/// non-translatable. Leading non-option operands are starting paths and stay
/// translatable.
fn mask_find_expression(words: &[String], mask: &mut [bool]) {
    let pattern_primaries = [
        "-name",
        "-path",
        "-regex",
        "-iregex",
        "-ipath",
        "-lname",
        "-wholename",
        "-ilname",
        "-iwholename",
    ];
    let mut in_expression = false;
    let mut i = 1;
    while i < words.len() {
        let arg = words[i].as_str();
        if !in_expression && (arg.starts_with('-') || matches!(arg, "!" | "(" | ")")) {
            in_expression = true;
        }
        if in_expression && pattern_primaries.contains(&arg) {
            if let Some(slot) = mask.get_mut(i + 1) {
                *slot = false;
            }
            i += 2;
            continue;
        }
        i += 1;
    }
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
    ast.commands[0]
        .words
        .first()
        .map(|word| decode_to_visible_text(word))
}

fn single_command_word(ast: &Ast) -> Option<&str> {
    if ast.commands.len() != 1 {
        return None;
    }
    ast.commands[0].words.first().map(String::as_str)
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum CommandNotFoundProviderOutput {
    Suggestions(Vec<String>),
    Empty,
    Failed(String),
}
fn native_command_not_found_hint_lines<F>(command: &str, mut command_exists: F) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    let mut lines = Vec::new();
    if !is_package_search_candidate(command) {
        return lines;
    }

    let search = shell_quote(command);

    // Application tools with a compiled-in plugin recipe: niu downloads
    // nothing (download retraction 2026-10-04), so the hint names the
    // platform's primary package-manager command directly — wpm first on
    // Windows (owner correction 2026-10-03), the native manager elsewhere.
    // `niu plugin add <recipe>` prints the same, fuller recommendation.
    if let Some(recipe) = plugin_recipe_for_command(command) {
        if let Some(install) = crate::plugins::recipes::first_recommendation(recipe) {
            lines.push(format!(
                "niubash: try '{install}' to add {command} (or: niu plugin add {recipe})"
            ));
        }
    }

    // The wpm channel survives only for the bundled Unix command layer
    // (awk/jq/7z/…), and only on Windows — wpm does not exist elsewhere and
    // the string must not surface in non-Windows builds.
    #[cfg(windows)]
    if let Some(package) = wpm_package_for_command(command) {
        lines.push(format!(
            "niubash: try 'wpm install {}' to add {}",
            package, command
        ));
    }

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
        lines.push("niubash: package search hints:".to_string());
        lines.extend(hints);
    }

    lines
}

/// Application tools that own a compiled-in plugin executable-tool recipe:
/// their install hint names the recipe's package-manager recommendation
/// (cross-platform — wpm first on Windows, native managers elsewhere).
/// Keep in sync with `assets/plugins/recipes.toml`: an entry here without a
/// recipe row would print a dead verb.
fn plugin_recipe_for_command(command: &str) -> Option<&'static str> {
    match command {
        "rg" => Some("ripgrep"),
        "fd" => Some("fd"),
        "fzf" => Some("fzf"),
        "bat" => Some("bat"),
        "eza" => Some("eza"),
        "zoxide" => Some("zoxide"),
        "dust" => Some("dust"),
        "duf" => Some("duf"),
        "erd" => Some("erdtree"),
        "direnv" => Some("direnv"),
        "starship" => Some("starship"),
        _ => None,
    }
}

/// Bundled Unix command-layer packages wpm still manages (owner ruling
/// 2026-10-03): the classic Unix toolbox that travels with the winuxcmd
/// tree — text/data filters, archive and transfer utilities. Application
/// tools are NOT here; they install through the plugin driver.
#[cfg(windows)]
fn wpm_package_for_command(command: &str) -> Option<&'static str> {
    match command {
        "awk" => Some("awk"),
        "gawk" => Some("gawk"),
        "jq" => Some("jq"),
        "yq" => Some("yq"),
        "ncat" => Some("ncat"),
        "7z" | "7zz" => Some("7zip"),
        "zstd" | "unzstd" | "zstdcat" => Some("zstd"),
        "wget" => Some("wget"),
        "aria2c" => Some("aria2"),
        "rclone" => Some("rclone"),
        "busybox" => Some("busybox"),
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

fn normalize_executor_home_env(executor: &mut Executor, home_dir: &Path) {
    let home = host_path_to_shell_path(&home_dir.to_string_lossy());
    let current = executor.get_env("HOME").unwrap_or_default();
    // GNU shell.c/variables.c: HOME comes from the caller's environment.
    // Normalize only when it is missing or Windows-shaped (backslashes,
    // slash-drive); an explicitly exported POSIX HOME is the caller's
    // choice and is honored verbatim (invocation.tests HOME=$TDIR).
    let should_update = current.trim().is_empty()
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

fn compatible_shell_path_from_env() -> Option<PathBuf> {
    let path = std::env::var_os(COMPATIBLE_SHELL_PATH_ENV)?;
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

/// One-time migration: rewrite a pre-rename `~/.winuxshrc` into
/// `~/.niubashrc` with the `NIU_*` environment prefix. The original file is
/// kept untouched; the rewrite is silent and idempotent.
fn migrate_legacy_winuxsh_rc(home_dir: &Path) {
    let source = home_dir.join(NIU_COMPAT_RC_FILE);
    let target = home_dir.join(NIU_RC_FILE);
    if target.is_file() || !source.is_file() {
        return;
    }
    let Ok(content) = std::fs::read_to_string(&source) else {
        return;
    };
    let migrated = rewrite_legacy_rc_content(&content);
    if std::fs::write(&target, migrated).is_err() {
        log::debug!(
            "failed to migrate {} to {}",
            source.display(),
            target.display()
        );
    }
}

/// Rename-brand textual rewrite for migrated rc files. `oh-my-winuxsh`
/// bundle references are preserved verbatim; `winuxcmd` names never match
/// the rewritten patterns.
fn rewrite_legacy_rc_content(content: &str) -> String {
    const BUNDLE_PROTECT: &str = "\u{1}OH_MY_BUNDLE_SLUG\u{1}";
    content
        .replace("oh-my-winuxsh", BUNDLE_PROTECT)
        .replace("update-winuxsh", "update-niubash")
        .replace(".winuxshrc", ".niubashrc")
        .replace("winuxshrc", "niubashrc")
        .replace("winuxsh.exe", "niu.exe")
        .replace("WINUXSH_", "NIU_")
        .replace("winuxsh", "niu")
        .replace("Winuxsh", "Niubash")
        .replace("WINUXSH", "NIUBASH")
        .replace(BUNDLE_PROTECT, "oh-my-winuxsh")
        // 1.0.0 ships the renamed entry point; migrated rc files keep their
        // legacy directory candidates but must probe the new entry name to
        // find the shipped bundle via NIU_APP_BUNDLE_PATH.
        .replace("oh-my-winuxsh.winux", "oh-my-niu.niu")
        .replace("oh-my-niu.winux", "oh-my-niu.niu")
}

fn prepare_shell_root(winuxcmd_path: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    if !cfg!(windows) {
        return Ok(None);
    }

    let root = std::env::var_os("NIU_ROOT")
        .or_else(|| std::env::var_os("WINUXSH_ROOT")) // deprecated rubash bridge
        .filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(shell_path_to_host_path(&value.to_string_lossy())))
        .or_else(|| winuxcmd_path.map(winuxcmd::installation_root));
    let Some(root) = root else {
        return Ok(None);
    };

    // /tmp is intentionally absent: it is backed by the real Windows temp
    // directory (unixwin/niubash#94), never by the install tree.
    for relative in ["bin", "usr/bin", "usr/local/bin", "etc", "var", "dev"] {
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

#[cfg(test)]
#[cfg(test)]
fn resolve_shell_path_argument(pwd: &str, arg: &str) -> PathBuf {
    resolve_shell_path_argument_with_env(pwd, arg, &HashMap::new())
}

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

fn resolve_native_command_path(command: &str) -> Option<PathBuf> {
    resolve_native_command_path_with_path(command, std::env::var_os("PATH")?)
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
    let bytes = value.as_bytes();
    let mut entries = Vec::new();
    let mut entry_start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        let is_separator = match byte {
            b';' => true,
            b':' => !is_windows_drive_colon(bytes, entry_start, index),
            _ => false,
        };
        if is_separator {
            if entry_start < index {
                entries.push(value[entry_start..index].to_string());
            }
            entry_start = index + 1;
        }
    }
    if entry_start < value.len() {
        entries.push(value[entry_start..].to_string());
    }
    entries
}

fn is_windows_drive_colon(bytes: &[u8], entry_start: usize, colon: usize) -> bool {
    cfg!(windows)
        && colon == entry_start + 1
        && bytes[entry_start].is_ascii_alphabetic()
        && bytes
            .get(colon + 1)
            .is_some_and(|next| matches!(next, b'\\' | b'/'))
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
        && normalized.is_char_boundary(root.len())
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

/// Process-standard stream targets for shell emission channels. PS0 is the
/// first user: it emits on [`ShellChannel::Stderr`] (GNU eval.c:164-176), so
/// redirected pipelines observe GNU's stream shape (unixwin/niubash#190).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellChannel {
    Stdout,
    Stderr,
}

#[cfg(test)]
thread_local! {
    /// Test seam for [`shell_channel_write`]: when armed with `Some`, channel
    /// writes are recorded here as (channel, payload) instead of reaching the
    /// real process handles, letting tests assert the stream separation.
    static CHANNEL_CAPTURE:
        std::cell::RefCell<Option<Vec<(ShellChannel, Vec<u8>)>>> =
        const { std::cell::RefCell::new(None) };
}

/// Write bytes to the named process-standard stream. Under `cfg(test)` an
/// armed [`CHANNEL_CAPTURE`] records the write instead of touching the real
/// handles (the built-in test harness may own/echo those, and PS0's stream
/// choice — not the handle identity — is the behavior under regression).
fn shell_channel_write(channel: ShellChannel, bytes: &[u8]) {
    #[cfg(test)]
    if CHANNEL_CAPTURE.with(|slot| slot.borrow().is_some()) {
        CHANNEL_CAPTURE.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .expect("capture armed")
                .push((channel, bytes.to_vec()));
        });
        return;
    }
    use std::io::Write as _;
    match channel {
        ShellChannel::Stdout => {
            let _ = std::io::stdout().write_all(bytes);
            let _ = std::io::stdout().flush();
        }
        ShellChannel::Stderr => {
            let _ = std::io::stderr().write_all(bytes);
            let _ = std::io::stderr().flush();
        }
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
    fn refresh_edit_mode_rc_vi_switches_at_startup() {
        // The standard bashrc line: `set -o vi` before the first prompt.
        let mut shell = test_shell(HookConfig::default());
        shell.executor.set_shell_option("vi", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Vi));
        assert_eq!(shell.editor_mode, EditorMode::Vi);
        // The normalization makes the flags exclusive, GNU-style: exactly
        // one of emacs/vi reads on.
        assert_eq!(shell.executor.get_env("__RUBASH_SETOPT_vi"), Some("1"));
        assert_ne!(shell.executor.get_env("__RUBASH_SETOPT_emacs"), Some("1"));
    }

    #[test]
    fn refresh_edit_mode_defaults_keep_configured_mode_quiet() {
        let mut shell = test_shell(HookConfig::default());
        // No engine flags at all: the configured default (emacs) stands and
        // no editor rebuild is signaled.
        assert_eq!(shell.refresh_edit_mode(), None);
        assert_eq!(shell.editor_mode, EditorMode::Emacs);
        // An explicit `set -o emacs` is also not a change.
        shell.executor.set_shell_option("emacs", true);
        assert_eq!(shell.refresh_edit_mode(), None);
    }

    #[test]
    fn refresh_edit_mode_live_flip_cycle_vi_emacs_vi() {
        // The mission journey: `set -o vi` mid-session, `set -o emacs`
        // returns, `set -o vi` again — the last writer must win every time
        // (GNU set.def:424 set_edit_mode rebinds editing-mode live through
        // readline bind.c:2092 sv_editmode).
        let mut shell = test_shell(HookConfig::default());
        shell.executor.set_shell_option("vi", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Vi));

        shell.executor.set_shell_option("emacs", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Emacs));

        // Without the exclusive write-back the vi flag would still read on
        // here and this flip would be invisible — the reason normalization
        // exists.
        shell.executor.set_shell_option("vi", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Vi));
    }

    #[test]
    fn refresh_edit_mode_noop_flips_and_disables_keep_mode() {
        let mut shell = test_shell(HookConfig::default());
        shell.executor.set_shell_option("vi", true);
        shell.refresh_edit_mode();
        // Re-asserting the active mode is not a change.
        shell.executor.set_shell_option("vi", true);
        assert_eq!(shell.refresh_edit_mode(), None);
        // `set +o emacs` while vi is active: GNU's set_edit_mode ignores a
        // disable of the inactive option (set.def:436-441) — no keymap
        // change.
        shell.executor.set_shell_option("emacs", false);
        assert_eq!(shell.refresh_edit_mode(), None);
        // An explicit `set -o emacs` is a new writer and must win.
        shell.executor.set_shell_option("emacs", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Emacs));
        shell.executor.set_shell_option("vi", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Vi));
        // `set +o <active>` is GNU's line-editing-off path (set.def:433-441,
        // not wired yet): flags drop to all-off, the mode stands.
        shell.executor.set_shell_option("vi", false);
        assert_eq!(shell.refresh_edit_mode(), None);
        assert_eq!(shell.editor_mode, EditorMode::Vi);
        // ... and the next explicit writer still lands.
        shell.executor.set_shell_option("emacs", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Emacs));
    }

    #[test]
    fn refresh_edit_mode_both_on_in_one_gap_resolves_vi() {
        // `set -o emacs; set -o vi` on one line between prompts: the order
        // is unrecoverable from the two flags; resolve toward the bashrc
        // idiom (documented tie-break).
        let mut shell = test_shell(HookConfig::default());
        shell.executor.set_shell_option("emacs", true);
        shell.executor.set_shell_option("vi", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Vi));
        // The exclusive write-back re-lands the pair, so the next refresh
        // is a no-op.
        assert_eq!(shell.refresh_edit_mode(), None);
    }

    #[test]
    fn refresh_edit_mode_engine_listing_stays_exclusive() {
        // GNU reports emacs/vi as exclusive views of one state
        // (get_edit_mode, set.def:446-451); after the product normalization
        // the engine's `set -o` listing must agree.
        let mut shell = test_shell(HookConfig::default());
        shell.executor.set_shell_option("vi", true);
        shell.refresh_edit_mode();
        shell.executor.set_shell_option("emacs", true);
        shell.refresh_edit_mode();
        assert_eq!(shell.executor.get_env("__RUBASH_SETOPT_emacs"), Some("1"));
        assert_eq!(shell.executor.get_env("__RUBASH_SETOPT_vi"), Some("0"));
        // SHELLOPTS never carries emacs/vi (rubash mirrors GNU's exclusion,
        // shellopts_includes_option), so the write-back must not add them.
        let shelopts = shell.executor.get_env("SHELLOPTS").unwrap_or_default();
        assert!(!shelopts
            .split(':')
            .any(|name| name == "emacs" || name == "vi"));
    }

    #[test]
    fn compatible_shell_path_env_is_explicit_and_non_empty() {
        let _lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        {
            let _guard = EnvVarGuard::unset(COMPATIBLE_SHELL_PATH_ENV);
            assert_eq!(compatible_shell_path_from_env(), None);
        }

        {
            let _guard = EnvVarGuard::set_value(COMPATIBLE_SHELL_PATH_ENV, "");
            assert_eq!(compatible_shell_path_from_env(), None);
        }

        let shell_path = std::env::temp_dir().join("niubash-compatible-shell.exe");
        let _guard = EnvVarGuard::set(COMPATIBLE_SHELL_PATH_ENV, &shell_path);
        assert_eq!(compatible_shell_path_from_env(), Some(shell_path));
    }

    #[test]
    fn precmd_invokes_title_hooks_with_env_title() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();

        let mut shell = test_shell(HookConfig {
            title: vec!["HOOK_TITLE=\"$NIU_TITLE\"".to_string()],
            ..Default::default()
        });
        shell.executor.set_env("NIU_TITLE", "custom-title");

        shell.run_precmd_hooks();

        assert_eq!(
            shell.executor.get_env("HOOK_TITLE"),
            Some("custom-title"),
            "title hook should observe NIU_TITLE from the executor env"
        );
        assert!(
            shell.executor.get_env("NIU_TITLE").is_none(),
            "hook context env should be cleaned up after the hook runs"
        );
    }

    #[test]
    fn precmd_invokes_title_hooks_with_pwd_fallback() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-title-pwd-fallback");
        std::fs::create_dir_all(&temp).unwrap();

        let mut shell = test_shell(HookConfig {
            title: vec!["HOOK_TITLE=\"$NIU_TITLE\"".to_string()],
            ..Default::default()
        });
        let target = shell_quote(&shell_display_path(&temp));
        shell
            .execute_interactive_line(&format!("cd {}", target))
            .unwrap();
        shell.execute_interactive_line("unset NIU_TITLE").unwrap();

        shell.run_precmd_hooks();

        let observed = shell.executor.get_env("HOOK_TITLE").unwrap_or_default();
        assert!(
            observed.contains("niubash-title-pwd-fallback"),
            "title hook should observe PWD as fallback, got: {observed:?}"
        );

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn compat_winuxshrc_is_migrated_and_sourced() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-compat-rc-migration");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(NIU_COMPAT_RC_FILE),
            "export NIU_COMPAT_RC_LOADED=1\nalias source_alias='echo user-override'\n",
        )
        .unwrap();

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home.clone();
        shell.run_startup_rc();

        // A pre-rename ~/.winuxshrc is migrated once into ~/.niubashrc (original
        // kept) and then sourced as the primary startup rc.
        assert_eq!(shell.executor.get_env("NIU_COMPAT_RC_LOADED"), Some("1"));
        assert_eq!(
            shell.aliases.get("source_alias").map(String::as_str),
            Some("echo user-override")
        );
        assert!(home.join(NIU_RC_FILE).is_file());
        assert!(home.join(NIU_COMPAT_RC_FILE).is_file());

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn standard_prompt_does_not_expose_private_use_markers() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = Shell::new().unwrap();
        shell.run_precmd_hooks();
        let rendered = reedline::Prompt::render_prompt_left(&shell.prompt).into_owned();

        assert!(
            !rendered
                .chars()
                .any(|ch| (0xE000..=0xE0FF).contains(&(ch as u32))),
            "prompt contains a raw-byte marker: {rendered:?}"
        );
    }

    #[test]
    fn niubashrc_runs_once_for_repl_startup_shell_customization() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-winshrc-startup");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(NIU_RC_FILE),
            r#"
export NIU_RC_VALUE=from-rc
alias hello='echo from-alias'
"#,
        )
        .unwrap();

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.run_startup_rc();

        assert_eq!(shell.executor.get_env("NIU_RC_VALUE"), Some("from-rc"));
        assert_eq!(
            shell.aliases.get("hello").map(String::as_str),
            Some("echo from-alias")
        );
        assert_eq!(shell.execute_interactive_line("hello").unwrap(), 0);
        assert!(shell.executor.get_env("NIU_REPL_STARTUP").is_none());

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn niubashrc_takes_precedence_over_compat_winuxshrc() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-primary-rc-startup");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(NIU_COMPAT_RC_FILE),
            "export NIU_RC_SOURCE=compat\n",
        )
        .unwrap();
        std::fs::write(home.join(NIU_RC_FILE), "export NIU_RC_SOURCE=primary\n").unwrap();

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.run_startup_rc();

        assert_eq!(shell.executor.get_env("NIU_RC_SOURCE"), Some("primary"));

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn compat_winuxshrc_used_when_primary_absent() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-compat-rc-startup");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(NIU_COMPAT_RC_FILE),
            "export NIU_RC_SOURCE=compat\n",
        )
        .unwrap();

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.run_startup_rc();

        assert_eq!(shell.executor.get_env("NIU_RC_SOURCE"), Some("compat"));

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn legacy_winuxshrc_is_migrated_to_niubashrc_once() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-rc-migration");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(NIU_COMPAT_RC_FILE),
            r#"
export NIU_THEME=sky
export WINUXSH_OLD_PREFIX=kept-as-niu
[ -f "$HOME/oh-my-winuxsh/theme" ] && . "$HOME/oh-my-winuxsh/theme"
[ -f "$NIU_APP_BUNDLE_PATH/oh-my-winuxsh.winux" ] && . "$NIU_APP_BUNDLE_PATH/oh-my-winuxsh.winux"
"#,
        )
        .unwrap();

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home.clone();
        shell.run_startup_rc();

        let migrated = std::fs::read_to_string(home.join(NIU_RC_FILE)).unwrap();
        assert!(migrated.contains("export NIU_THEME=sky"));
        assert!(migrated.contains("export NIU_OLD_PREFIX=kept-as-niu"));
        assert!(migrated.contains("oh-my-winuxsh/theme"));
        assert!(migrated.contains("oh-my-niu.niu"));
        assert!(!migrated.contains("oh-my-winuxsh.winux"));
        assert!(!migrated.contains("WINUXSH"));
        assert_eq!(shell.executor.get_env("NIU_THEME"), Some("sky"));
        // The original file is kept untouched as a backup.
        assert!(home.join(NIU_COMPAT_RC_FILE).is_file());
        // Re-running must not clobber an existing primary rc.
        std::fs::write(home.join(NIU_RC_FILE), "export NIU_THEME=custom\n").unwrap();
        migrate_legacy_winuxsh_rc(&home);
        assert_eq!(
            std::fs::read_to_string(home.join(NIU_RC_FILE)).unwrap(),
            "export NIU_THEME=custom\n"
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn non_interactive_env_is_noop_when_unset() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let _niu_guard = EnvVarGuard::unset("NIU_ENV");
        let _bash_guard = EnvVarGuard::unset("BASH_ENV");

        let temp = unique_temp_dir("niubash-env-noop");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("agent.env"), "export NIU_AGENT_ENV=loaded\n").unwrap();

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home.clone();
        shell.source_non_interactive_env();

        assert!(shell.executor.get_env("NIU_AGENT_ENV").is_none());

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn niu_env_sources_agent_init_file() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let _bash_guard = EnvVarGuard::unset("BASH_ENV");

        let temp = unique_temp_dir("niubash-env-niu");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let env_file = home.join("agent.env");
        std::fs::write(
            &env_file,
            "export NIU_AGENT_ENV=loaded\nalias ga='echo agent'\n",
        )
        .unwrap();

        let _niu_guard = EnvVarGuard::set("NIU_ENV", &env_file);

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.source_non_interactive_env();

        assert_eq!(shell.executor.get_env("NIU_AGENT_ENV"), Some("loaded"));
        // Aliases defined in the env file work in subsequent commands.
        assert_eq!(shell.execute_script("ga").unwrap(), 0);

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn bash_env_sources_when_niu_env_unset() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let _niu_guard = EnvVarGuard::unset("NIU_ENV");

        let temp = unique_temp_dir("niubash-env-bash");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let env_file = home.join("agent.env");
        std::fs::write(&env_file, "export NIU_AGENT_ENV=bash-loaded\n").unwrap();

        let _bash_guard = EnvVarGuard::set("BASH_ENV", &env_file);

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.source_non_interactive_env();

        assert_eq!(shell.executor.get_env("NIU_AGENT_ENV"), Some("bash-loaded"));

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn niu_env_takes_precedence_over_bash_env() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();

        let temp = unique_temp_dir("niubash-env-precedence");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let niu_file = home.join("niu.env");
        let bash_file = home.join("bash.env");
        std::fs::write(&niu_file, "export NIU_AGENT_ENV=from-niu\n").unwrap();
        std::fs::write(&bash_file, "export NIU_AGENT_ENV=from-bash\n").unwrap();

        let _niu_guard = EnvVarGuard::set("NIU_ENV", &niu_file);
        let _bash_guard = EnvVarGuard::set("BASH_ENV", &bash_file);

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.source_non_interactive_env();

        assert_eq!(shell.executor.get_env("NIU_AGENT_ENV"), Some("from-niu"));

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn non_interactive_env_expands_tilde_in_path() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let _bash_guard = EnvVarGuard::unset("BASH_ENV");

        let temp = unique_temp_dir("niubash-env-tilde");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("agent.env"),
            "export NIU_AGENT_ENV=tilde-loaded\n",
        )
        .unwrap();

        let _niu_guard = EnvVarGuard::set_value("NIU_ENV", "~/agent.env");

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.source_non_interactive_env();

        assert_eq!(
            shell.executor.get_env("NIU_AGENT_ENV"),
            Some("tilde-loaded")
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn non_interactive_env_ignores_empty_value() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let _bash_guard = EnvVarGuard::unset("BASH_ENV");

        let temp = unique_temp_dir("niubash-env-empty");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("agent.env"),
            "export NIU_AGENT_ENV=should-not-load\n",
        )
        .unwrap();

        let _niu_guard = EnvVarGuard::set_value("NIU_ENV", "");

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.source_non_interactive_env();

        assert!(shell.executor.get_env("NIU_AGENT_ENV").is_none());

        let _ = std::fs::remove_dir_all(temp);
    }
    #[test]
    fn user_bindkeys_load_from_rc_and_widgets_round_trip() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-user-widget-bindkeys");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(NIU_RC_FILE),
            r#"
NIU_BINDKEYS="Ctrl+X:niu_fzf_file
Alt+G:niu_git_status"

niu_fzf_file() {
    NIU_WIDGET_RESULT="picked:${#NIU_WIDGET_BUFFER}"
}
"#,
        )
        .unwrap();

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();
        shell.load_user_widget_bindings();

        assert_eq!(shell.user_widget_bindings.len(), 2);
        assert_eq!(shell.user_widget_bindings[0].key.as_deref(), Some("Ctrl+X"));
        assert_eq!(shell.user_widget_bindings[0].widget, "niu_fzf_file");
        assert_eq!(shell.user_widget_bindings[1].key.as_deref(), Some("Alt+G"));

        assert!(shell.widget_function_available("niu_fzf_file"));

        let outcome = shell.run_widget_function("niu_fzf_file", "hello", 5);
        assert_eq!(outcome.buffer.as_deref(), Some("picked:5"));
        assert!(!outcome.accept);
        assert_eq!(outcome.cursor, None);
        assert_eq!(shell.executor.get_env("NIU_WIDGET_RESULT"), None);
        assert_eq!(shell.executor.get_env("NIU_WIDGET_BUFFER"), None);

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn engine_bind_x_registered_via_rc_runs_with_readline_protocol() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-engine-bind-x");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(NIU_RC_FILE),
            r#"
bind -x '"\C-r": __fzf_history_stub'
__fzf_history_stub() {
    if [ -n "$READLINE_LINE" ]; then
        READLINE_LINE="picked:$READLINE_LINE"
    else
        READLINE_LINE="picked-empty"
    fi
    READLINE_POINT=${#READLINE_LINE}
}
"#,
        )
        .unwrap();

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();
        shell.load_engine_bindings();

        // The bind -x registration survived rc sourcing into the
        // registry snapshot the editor mirrors.
        assert_eq!(shell.engine_bindings.len(), 1);
        assert_eq!(shell.engine_bindings[0].keyseq, r#"\C-r"#);
        assert!(matches!(
            &shell.engine_bindings[0].kind,
            rubash::shell::bind_registry::BindKind::Execute { command }
                if command == "__fzf_history_stub"
        ));

        // fzf CTRL-R shape: the command reads READLINE_LINE and writes
        // the selection back; the outcome replaces the edit buffer.
        let outcome = shell.run_bind_x_command(0, "typed", 5);
        assert_eq!(outcome.buffer.as_deref(), Some("picked:typed"));
        assert_eq!(outcome.cursor, Some("picked:typed".len()));
        assert!(!outcome.accept, "bind -x never submits the line");

        // Empty-buffer press, cursor clamped to the new line's end.
        let outcome = shell.run_bind_x_command(0, "", 0);
        assert_eq!(outcome.buffer.as_deref(), Some("picked-empty"));
        assert_eq!(outcome.cursor, Some("picked-empty".len()));

        // GNU unbinds the READLINE_* variables after the command
        // (bashline.c:4573 unbind_readline_variables).
        assert_eq!(shell.executor.get_env("READLINE_LINE"), None);
        assert_eq!(shell.executor.get_env("READLINE_POINT"), None);

        // Registry generation moved by the rc `bind` call: the REPL uses
        // it to rebuild the mirrored keymaps before the next prompt.
        assert!(shell.engine_bind_generation > 0);

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn user_compdefs_load_from_rc_and_run_in_engine() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-user-compdefs");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(NIU_RC_FILE),
            r#"
NIU_COMPDEFS="git:niu_git_comp"

niu_git_comp() {
    NIU_COMP_RESULT="w:${NIU_COMP_WORDS}|c:${NIU_COMP_CWORD}"
}
"#,
        )
        .unwrap();

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();
        shell.load_user_compdefs();

        assert_eq!(shell.compdefs.len(), 1);
        assert_eq!(shell.compdefs[0].0, "git");
        assert_eq!(shell.compdefs[0].1, "niu_git_comp");

        let candidates =
            shell.run_compdef_function("niu_git_comp", &["git".to_string(), "co".to_string()], 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "w:git co|c:1");
        assert_eq!(candidates[0].1, None);
        assert_eq!(shell.executor.get_env("NIU_COMP_RESULT"), None);
        assert_eq!(shell.executor.get_env("NIU_COMP_WORDS"), None);

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn run_startup_rc_syncs_process_path_from_executor() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-rc-path-sync");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // Simulate a user rc that appends a new directory to PATH.
        let extra_dir = temp.join("extra-bin");
        std::fs::create_dir_all(&extra_dir).unwrap();
        let forward = extra_dir.to_string_lossy().replace('\\', "/");
        std::fs::write(
            home.join(NIU_RC_FILE),
            format!("export PATH=\"$PATH:{}\"", forward),
        )
        .unwrap();

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        shell.run_startup_rc();

        // After rc, std::env must reflect the executor PATH (including the new dir).
        let process_path = std::env::var("PATH").unwrap_or_default();
        let normalized_extra = forward.to_uppercase().replace('/', "\\");
        assert!(
            process_path.to_uppercase().contains(&normalized_extra),
            "std::env PATH should contain the new directory after run_startup_rc;              got: {}",
            process_path
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn native_command_not_found_lines_include_available_windows_package_managers() {
        // rg is an application tool with a compiled-in executable-tool
        // recipe: the install hint names the platform's package manager
        // directly (wpm first on Windows — owner correction 2026-10-03;
        // native managers elsewhere), plus the recipe verb for the fuller
        // recommendation.
        let lines = native_command_not_found_hint_lines("rg", |command| {
            matches!(command, "winget" | "scoop")
        });

        #[cfg(windows)]
        assert!(
            lines.contains(
                &"niubash: try 'wpm install ripgrep' to add rg \
                             (or: niu plugin add ripgrep)"
                    .to_string()
            ),
            "{lines:?}"
        );
        #[cfg(not(windows))]
        assert!(
            lines.iter().any(|line| line.contains("to add rg")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("wpm install awk")),
            "command-layer and application-tool channels must not cross: {lines:?}"
        );
        assert!(lines.contains(&"niubash: package search hints:".to_string()));
        assert!(lines.contains(&"  winget search --name 'rg'".to_string()));
        assert!(lines.contains(&"  scoop search 'rg'".to_string()));
        assert!(!lines.iter().any(|line| line.contains("choco search")));
    }

    #[test]
    fn native_command_not_found_hint_lines_include_wpm_without_search() {
        // awk is bundled Unix command layer: wpm keeps managing it, and the
        // hint stays wpm-only (Windows builds; the wpm channel compiles only
        // there).
        #[cfg(windows)]
        {
            let lines = native_command_not_found_hint_lines("awk", |_| false);

            assert_eq!(lines, vec!["niubash: try 'wpm install awk' to add awk"]);
        }
        // On other platforms the command layer has no wpm channel at all.
        #[cfg(not(windows))]
        {
            let lines = native_command_not_found_hint_lines("awk", |_| false);
            assert!(lines.is_empty(), "{lines:?}");
        }
    }

    #[test]
    fn plugin_recipe_hints_stay_cross_platform_and_recipe_backed() {
        // Every hint here must name a compiled-in recipe row (a dead verb
        // would be worse than no hint) — guarded by name against the seed
        // index so a renamed recipe fails loudly.
        for (command, recipe) in [
            ("rg", "ripgrep"),
            ("fd", "fd"),
            ("fzf", "fzf"),
            ("bat", "bat"),
            ("eza", "eza"),
            ("zoxide", "zoxide"),
            ("dust", "dust"),
            ("duf", "duf"),
            ("erd", "erdtree"),
            ("direnv", "direnv"),
            ("starship", "starship"),
        ] {
            assert_eq!(plugin_recipe_for_command(command), Some(recipe));
            assert!(
                crate::plugins::recipes::recipe(recipe).is_some_and(|row| {
                    matches!(
                        row.driver,
                        Some(crate::plugins::recipes::RecipeDriver::Download { .. })
                    )
                }),
                "hint recipe '{recipe}' for '{command}' has no executable-tool row in the seed index"
            );
            let lines = native_command_not_found_hint_lines(command, |_| false);
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains(&format!("niu plugin add {recipe}"))),
                "{command}: {lines:?}"
            );
            // The platform's primary install command is named directly
            // (wpm on Windows, a native manager elsewhere).
            let first = crate::plugins::recipes::first_recommendation(recipe)
                .unwrap_or_else(|| panic!("{recipe}: no recommendation"));
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains(&format!("try '{first}'"))),
                "{command}: expected '{first}' in {lines:?}"
            );
        }
    }

    #[test]
    fn native_command_not_found_lines_skip_package_hints_for_paths() {
        let lines = native_command_not_found_hint_lines("./missing", |_| true);

        assert!(lines.is_empty(), "{lines:?}");
    }
    #[test]
    fn alias_mirror_tracks_successful_interactive_alias_commands() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        shell
            .execute_interactive_line("alias gst='git status'")
            .unwrap();
        assert_eq!(
            shell.aliases.get("gst").map(String::as_str),
            Some("git status")
        );

        shell.execute_interactive_line("unalias gst").unwrap();
        assert!(shell.aliases.get("gst").is_none());
    }

    #[test]
    fn completion_state_tracks_shell_local_variables_after_interactive_source() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn resolve_shell_path_argument_expands_current_user_tilde() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("niubash-tilde-path");
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
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("niubash-userprofile-home-precedence");
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
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = unique_temp_dir("niubash-shell-style-home-startup");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(NIU_RC_FILE), "export NIU_RC_SOURCE=primary\n").unwrap();
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

        assert_eq!(shell.executor.get_env("NIU_RC_SOURCE"), Some("primary"));
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
    fn cd_slash_drive_args_normalize_to_windows_roots() {
        let tokens = tokenize("cd /d && cd /e/projects");
        let mut ast = parse(&tokens);
        normalize_cd_windows_drive_args(&mut ast);

        if cfg!(windows) {
            let and_or_list = ast.commands[0].and_or_list.as_ref().unwrap();
            assert_eq!(and_or_list.commands[0].words, vec!["cd", "D:/"]);
            assert_eq!(and_or_list.commands[1].words, vec!["cd", "E:/projects"]);
        } else {
            let and_or_list = ast.commands[0].and_or_list.as_ref().unwrap();
            assert_eq!(and_or_list.commands[0].words, vec!["cd", "/d"]);
            assert_eq!(and_or_list.commands[1].words, vec!["cd", "/e/projects"]);
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

    #[test]
    fn split_shell_path_list_preserves_windows_drive_colons() {
        assert_eq!(
            split_shell_path_list("D:/repo/bin:D:/sdk/tools:/usr/bin"),
            vec![
                "D:/repo/bin".to_string(),
                "D:/sdk/tools".to_string(),
                "/usr/bin".to_string(),
            ]
        );
        assert_eq!(
            split_shell_path_list("C:/Windows;D:\\Tools:/c/bin"),
            vec![
                "C:/Windows".to_string(),
                "D:\\Tools".to_string(),
                "/c/bin".to_string(),
            ]
        );
        if cfg!(windows) {
            assert_eq!(
                split_shell_path_list("D:/repo/bin"),
                vec!["D:/repo/bin".to_string()]
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn installed_winuxcmd_root_maps_host_path_helpers() {
        // Mutates the process env (NIU_ROOT): serialize with every other
        // env-touching test (Windows env races — wt61).
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _root_guard = EnvVarGuard::set_value("NIU_ROOT", "");
        let root = unique_temp_dir("niubash-installed-root");
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
    fn parameter_pattern_removal_wins_before_equals_in_pattern() {
        // The `=` and `\"` inside the pattern body are pattern text, not
        // assignment syntax — rubash parses them natively now.
        let script = r##"line='<rect x="0" fill="#fe0000"/>'; rest=${line#*fill=\"}; printf '%s\n' "${rest%%\"*}""##;
        let script = normalize_native_windows_path_literals(script);

        let mut shell = test_shell(HookConfig::default());
        assert_eq!(shell.execute_script(&script).unwrap(), 0);
        assert_eq!(shell.executor.get_env("rest"), Some("#fe0000\"/>"));
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
    fn winuxcmd_pattern_operands_are_not_slash_drive_translated() {
        let tokens = tokenize(
            "grep -F \"/h/\" /c/Users/file; grep /h/ /c/Users/file; grep -e /h/ /c/Users/file; grep -f /c/patfile /c/Users/file; grep -A 2 /h/ /c/Users/file; sed /h/d /c/Users/file; awk /h/ /c/Users/file; find /c/Users -name /h/ -print; find /e/ -maxdepth 1",
        );
        let mut ast = parse(&tokens);
        normalize_winuxcmd_slash_drive_args(&mut ast);

        if cfg!(windows) {
            assert_eq!(ast.commands[0].words[2], "/h/");
            assert_eq!(ast.commands[0].words[3], "C:/Users/file");
            assert_eq!(ast.commands[1].words[1], "/h/");
            assert_eq!(ast.commands[1].words[2], "C:/Users/file");
            assert_eq!(ast.commands[2].words[2], "/h/");
            assert_eq!(ast.commands[2].words[3], "C:/Users/file");
            assert_eq!(ast.commands[3].words[2], "C:/patfile");
            assert_eq!(ast.commands[3].words[3], "C:/Users/file");
            assert_eq!(ast.commands[4].words[1], "-A");
            assert_eq!(ast.commands[4].words[2], "2");
            assert_eq!(ast.commands[4].words[3], "/h/");
            assert_eq!(ast.commands[4].words[4], "C:/Users/file");
            assert_eq!(ast.commands[5].words[1], "/h/d");
            assert_eq!(ast.commands[5].words[2], "C:/Users/file");
            assert_eq!(ast.commands[6].words[1], "/h/");
            assert_eq!(ast.commands[6].words[2], "C:/Users/file");
            assert_eq!(ast.commands[7].words[1], "C:/Users");
            assert_eq!(ast.commands[7].words[3], "/h/");
            assert_eq!(ast.commands[8].words[1], "E:/");
        } else {
            assert_eq!(ast.commands[0].words[2], "/h/");
            assert_eq!(ast.commands[0].words[3], "/c/Users/file");
            assert_eq!(ast.commands[3].words[2], "/c/patfile");
            assert_eq!(ast.commands[4].words[3], "/h/");
            assert_eq!(ast.commands[5].words[1], "/h/d");
            assert_eq!(ast.commands[6].words[1], "/h/");
            assert_eq!(ast.commands[7].words[1], "/c/Users");
            assert_eq!(ast.commands[7].words[3], "/h/");
            assert_eq!(ast.commands[8].words[1], "/e/");
        }
    }

    #[test]
    fn shim_rewrite_preserves_quoted_glob_carriers() {
        // Phase 0 (host-semantic-layer-elimination): the host must not strip
        // rubash's \x11 quoted-glob carrier from token values — the executor
        // decodes it at the argv boundary, and stripping it here made
        // `echo a\*b` glob-expand. Host consumers decode via
        // rubash::decode_to_visible_text instead.
        let mut tokens = tokenize(r"grep a\*b file; echo c\?d");
        rewrite_winuxcmd_command_shims(&mut tokens, false);
        let ast = parse(&tokens);
        assert!(ast.commands[0].words[1].contains('\x11'));
        assert!(ast.commands[1].words[1].contains('\x11'));
        assert_eq!(decode_to_visible_text(&ast.commands[0].words[1]), "a*b");
        assert_eq!(decode_to_visible_text(&ast.commands[1].words[1]), "c?d");
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

        // The `+` stays behind rubash's \x11 data carrier until the executor
        // decodes it at the argv boundary; the host sees the transport form.
        assert_eq!(
            pipeline.stages[1].words,
            vec!["grep.exe", "-E", "a.\u{11}+c"]
        );
        assert_eq!(decode_to_visible_text(&pipeline.stages[1].words[2]), "a.+c");
    }

    #[test]
    fn file_commands_are_not_rewritten_or_claimed_as_niubash_builtins() {
        if !cfg!(windows) {
            return;
        }

        let mut tokens = tokenize("rm -rf -- '-p'; cat file; cp src dst");
        rewrite_winuxcmd_command_shims(&mut tokens, false);
        let ast = parse(&tokens);

        assert_eq!(ast.commands[0].words, vec!["rm", "-rf", "--", "-p"]);
        assert!(niubash_builtin_words(&ast.commands[0]).is_none());
        assert!(niubash_builtin_words(&ast.commands[1]).is_none());
        assert!(niubash_builtin_words(&ast.commands[2]).is_none());
    }

    #[test]
    fn source_and_file_commands_are_not_claimed_as_niubash_builtins() {
        let mut tokens = tokenize(
            "cat file; chmod +w file; cp src dst; kill -l; mkdir -p dir; mkfifo pipe; pwd; rm -rf dir; rmdir dir; self-update --check; source file; touch file",
        );
        rewrite_winuxcmd_command_shims(&mut tokens, false);
        let ast = parse(&tokens);

        let names: Vec<_> = ast
            .commands
            .iter()
            .map(|command| niubash_builtin_words(command).map(|(name, _)| name))
            .collect();
        assert_eq!(
            names,
            vec![None, None, None, None, None, None, None, None, None, None, None, None,]
        );
    }

    #[test]
    fn self_update_repl_commands_are_not_shell_builtins() {
        let ast = parse(&tokenize("self-update --check; update-niubash --dry-run"));

        let names: Vec<_> = ast
            .commands
            .iter()
            .map(|command| niubash_builtin_words(command).map(|(name, _)| name))
            .collect();
        assert_eq!(names, vec![None, None]);
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
            assert!(niubash_builtin_words(command).is_none());
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
        assert!(niubash_builtin_words(&ast.commands[0]).is_none());
    }

    #[test]
    fn setopt_delegates_to_rubash_builtin_surface() {
        let ast = parse(&tokenize(
            "setopt hist_ignore_space; builtin setopt prompt_subst; command unsetopt prompt_subst",
        ));

        assert!(niubash_builtin_words(&ast.commands[0]).is_none());
        assert!(niubash_builtin_words(&ast.commands[1]).is_none());
        assert!(niubash_builtin_words(&ast.commands[2]).is_none());
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
        assert!(niubash_builtin_words(&ast.commands[0]).is_none());
        assert!(niubash_builtin_words(&ast.commands[1]).is_none());
        assert!(niubash_builtin_words(&ast.commands[2]).is_none());
    }

    #[test]
    fn redirected_pwd_uses_rubash_redirection_path() {
        if !cfg!(windows) {
            return;
        }

        let ast = parse(&tokenize("pwd > out.txt"));

        assert!(niubash_builtin_words(&ast.commands[0]).is_none());
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
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-cwd-sync");
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

        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-cwd-sequence");
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
    fn bash_prompt_command_updates_ps1_before_prompt_render() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn enter_interactive_discards_foreign_inherited_ps1() {
        // unixwin/niubash#117 (reopened): the Git Bash session PS1 exported
        // into child processes must not be adopted by an interactive shell.
        // enter_interactive runs before the startup rc, so discarding there
        // leaves a PS1 set by the user's own rc fully in charge.
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell
            .executor
            .set_env("PS1", r"\[\033]0;x\007\]`__git_ps1` $ ");

        shell.enter_interactive();

        // The foreign value is gone from the environment, so no prompt path
        // (REPL sync or engine interactive stdin) can expand `__git_ps1`.
        assert_eq!(shell.executor.get_env("PS1"), None);
        // The shell's own theme backend is untouched by the discard.
        assert!(matches!(shell.prompt, PromptBackend::Template(_)));

        // A PS1 without Git Bash session machinery is user content and must
        // survive into prompt adoption exactly as before.
        let mut plain = test_shell(HookConfig::default());
        plain.executor.set_env("PS1", r"\u@\h:\w\$ ");
        plain.enter_interactive();
        assert_eq!(plain.executor.get_env("PS1"), Some(r"\u@\h:\w\$ "));
        plain.sync_bash_prompt_from_env();
        assert!(matches!(plain.prompt, PromptBackend::Bash(_)));
    }

    /// Defaults-as-floor (design §14.5): a PS1 written *after* interactive
    /// entry — the provenance an enabled framework has when its guarded
    /// loader block runs inside the startup rc — is never eligible for the
    /// #117 foreign-inheritance discard, even when its text happens to
    /// carry Git Bash session markers. The discard is provenance-gated
    /// (inherited env only, once, before any rc); framework recognition
    /// must never grow another content-detector.
    #[test]
    fn framework_ps1_set_after_interactive_entry_is_never_discarded() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.enter_interactive();
        // Simulate a framework theme block sourced from the rc. The value
        // deliberately reuses #117's marker shapes to prove ordering, not
        // content, is the guard.
        shell
            .executor
            .set_env("PS1", r"\u@\h $MSYSTEM \w `__git_ps1` $ ");
        shell.run_precmd_hooks();
        assert_eq!(
            shell.executor.get_env("PS1"),
            Some(r"\u@\h $MSYSTEM \w `__git_ps1` $ "),
            "a framework-set PS1 must keep full effect"
        );
        assert!(
            matches!(shell.prompt, PromptBackend::Bash(_)),
            "the claim renders through the bash-compatible channel"
        );
    }

    /// Defaults-as-floor (§14.5): releasing the claim (unset/empty PS1 —
    /// `niu plugin disable`, theme off, plain `unset PS1`) restores the
    /// product floor instead of freezing the last claimed face.
    #[test]
    fn prompt_claim_release_restores_the_floor() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.executor.set_env("PS1", "claimed-face> ");
        shell.run_precmd_hooks();
        match &shell.prompt {
            PromptBackend::Bash(prompt) => {
                assert_eq!(prompt.render_prompt_left(), "claimed-face> ");
            }
            _ => panic!("expected the claim to render"),
        }

        shell.executor.unset_env("PS1");
        shell.run_precmd_hooks();
        match &shell.prompt {
            PromptBackend::Template(floor) => {
                let left = reedline::Prompt::render_prompt_left(floor).into_owned();
                assert!(!left.contains("claimed-face"), "{left}");
            }
            _ => panic!("expected the product floor after release"),
        }

        // Re-claim after release still wins (claim/release/claim cycle).
        shell.executor.set_env("PS1", "reclaimed> ");
        shell.run_precmd_hooks();
        match &shell.prompt {
            PromptBackend::Bash(prompt) => {
                assert_eq!(prompt.render_prompt_left(), "reclaimed> ");
            }
            _ => panic!("expected re-claim to render"),
        }
    }

    /// Defaults-as-floor (§14.5): PROMPT_COMMAND is a hook, not a claim.
    /// Hook-only frameworks (oh-my-bash's `history` plugin registers
    /// `history -a` without any theme) must not displace the product floor
    /// with a placeholder PS1; the hook still runs every prompt.
    #[test]
    fn hook_only_prompt_command_keeps_the_floor() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.executor.set_env("PROMPT_COMMAND", "NIU_HOOK_RAN=1");

        shell.run_precmd_hooks();

        assert!(
            matches!(shell.prompt, PromptBackend::Template(_)),
            "a hook without PS1 must not claim the prompt slot"
        );
        assert_eq!(
            shell.executor.get_env("NIU_HOOK_RAN"),
            Some("1"),
            "the hook still ran before the prompt"
        );
    }

    /// starship's claim shape (src/init/starship.bash): PROMPT_COMMAND names
    /// a precmd function that assigns PS1 itself. Because the hook runs
    /// before the backend sync, the PS1 claim is visible in the same render
    /// cycle — the floor steps aside on the very first starship prompt.
    #[test]
    fn starship_style_precmd_hook_claims_via_ps1_same_cycle() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell
            .execute_line("starship_precmd() { PS1='starship-face> '; }")
            .unwrap();
        shell
            .execute_line("PROMPT_COMMAND=starship_precmd")
            .unwrap();

        shell.run_precmd_hooks();

        match &shell.prompt {
            PromptBackend::Bash(prompt) => {
                assert_eq!(prompt.render_prompt_left(), "starship-face> ");
            }
            _ => panic!("expected starship's PS1 claim to render"),
        }
    }

    #[test]
    fn bash_ps0_runs_before_interactive_command() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.executor.set_env(
            "PS0",
            "${STARSHIP_START_TIME:$((STARSHIP_START_TIME=12345,0)):0}",
        );

        assert_eq!(shell.execute_interactive_line(":").unwrap(), 0);

        assert_eq!(shell.executor.get_env("STARSHIP_START_TIME"), Some("12345"));
    }

    /// unixwin/niubash#190: PS0's decoded output must go to the stderr
    /// channel (GNU eval.c:176 writes stderr), with stdout left untouched —
    /// `cmd 2>/dev/null` drops the PS0 bytes while `cmd > file` stays clean.
    #[test]
    fn bash_ps0_expansion_targets_stderr_and_stdout_stays_clean() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.executor.set_env("PS0", "PS0-EXPANDED ");

        // Arm the capture, run one interactive line, then disarm — on panic
        // too, so the thread-local never leaks into a later test.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            CHANNEL_CAPTURE.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
            shell.execute_interactive_line(":").unwrap();
        }));
        let captured = CHANNEL_CAPTURE.with(|slot| slot.borrow_mut().take());
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
        let captured = captured.expect("capture disarmed");

        assert!(
            captured
                .iter()
                .all(|(channel, _)| *channel == ShellChannel::Stderr),
            "PS0 expansion must never write the stdout channel: {captured:?}"
        );
        assert_eq!(
            captured,
            vec![(ShellChannel::Stderr, b"PS0-EXPANDED ".to_vec())],
            "decoded PS0 bytes land on stderr exactly once"
        );
    }

    fn test_shell(hooks: HookConfig) -> Shell {
        let mut executor = Executor::new();
        executor.set_shopt_option("expand_aliases", true);
        let mut shell = Shell {
            executor,
            completion_state: Arc::new(Mutex::new(CompletionState::new(PathBuf::from(".")))),
            prompt: PromptBackend::Template(NiubashPrompt::new(None, None)),
            floor_prompt: PromptBackend::Template(NiubashPrompt::new(None, None)),
            home_dir: PathBuf::from("."),
            shell_root: None,
            history_path: PathBuf::from(".niubash_history"),
            history_max_size: 10000,
            history_ignore_space_prefixed: false,
            history_mode: crate::config::HistoryMode::default(),
            menu_config: MenuConfig::default(),
            editor_mode: EditorMode::Emacs,
            edit_flags_seen: None,
            autosuggest: AutosuggestConfig::default(),
            syntax_highlighting: SyntaxHighlightConfig::default(),
            native_widgets: NativeWidgetConfig::default(),
            native_widget_bindings: Vec::new(),
            user_widget_bindings: Vec::new(),
            engine_bindings: Vec::new(),
            engine_bind_generation: 0,
            compdefs: Vec::new(),
            hooks,
            aliases: HashMap::new(),
            last_interactive_command: None,
            last_interactive_exit_code: None,
            line_editor: None,
            process_stdin_pipeline_bridge: false,
            bash_prompt_command_running: false,
            interactive: false,
            no_rc: false,
            no_profile: false,
            rc_file: None,
            no_editing: false,
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

    fn write_fake_cwd_probe(bin: &std::path::Path, log_path: &str) {
        let script = format!("@echo off\r\n>\"{}\" echo %CD%\r\nexit /b 0\r\n", log_path);
        std::fs::write(bin.join("cwdprobe.cmd"), script).unwrap();
    }
}
