//! Shell state and execution entry point
//!
//! Wraps a `rubash::api::Session` (the 1.5.0 Session contract) and
//! provides the interactive shell machinery (prompt, history,
//! completion). All shell language semantics are delegated to
//! rubash; this layer only adds the Windows-facing UX. Faces the
//! contract does not cover reach the executor through the
//! recorded tier-C seam (`Session::executor` / `executor_mut`);
//! `Shell::executor` / `Shell::executor_mut` below are the only
//! doors to it, so the retained direct set stays enumerable.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use reedline::Reedline;
use rubash::api::{Flow, PreparedLine, RunOptions, Session};
use rubash::{
    decode_to_visible_text, executor::Executor, lexer::tokenize, parser::parse, Ast, Token,
    TokenKind,
};

use crate::completion::{CompletionState, NiubashCompleter};
use crate::config::{
    load as load_config, AutosuggestConfig, CommandNotFoundHint, EditorMode, HookConfig,
    MenuConfig, NativeWidgetBinding, NativeWidgetConfig, SyntaxHighlightConfig,
};
use crate::path_utils::{shell_home_dir, shell_path_to_host_path, split_shell_path_list};
use crate::prompt::{BashPrompt, NiubashPrompt, PromptBackend};
use crate::prompt_segments::{
    SegmentId, SegmentPreset, SegmentPrompt, SegmentPromptAdapter, SegmentPromptConfig,
};

use crate::winuxcmd;

const COMPATIBLE_SHELL_PATH_ENV: &str = "NIU_COMPATIBLE_SHELL_PATH";
#[allow(dead_code)]
const NIU_RC_FILE: &str = ".niubashrc";
/// Niubash-native non-interactive environment file variable. Takes precedence
/// over `BASH_ENV` so an agent shell can point at a dedicated init file.
const NIU_ENV_VAR: &str = "NIU_ENV";
/// GNU bash non-interactive environment file variable. Sourced by default for
/// non-interactive shells; kept for bash compatibility (OpenCode and other
/// agents already use BASH_ENV).
const BASH_ENV_VAR: &str = "BASH_ENV";

/// GNU bash's compiled-in default primary prompt, used when the
/// environment carries no PS1 at all (niubash#672). Verified against
/// the GNU 5.3.20 oracle: an interactive shell started with PS1 unset
/// reports `PS1=[\s-\v\$ ]` (and renders `bash-5.3$ ` / `bash-5.3# `).
const GNU_DEFAULT_PS1: &str = "\\s-\\v\\$ ";

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

/// Remove the completion bridge installed on this thread, dropping the
/// bridged Shell if the caller holds no other `Rc` clone.
///
/// Test-only: the bridge is thread-local, so without an explicit
/// uninstall the Shell outlives the test function and is dropped at
/// thread teardown — after any test-local state restore has already
/// run (see `test_support::ProcessEnvRestore`).
#[cfg(test)]
pub(crate) fn uninstall_completion_bridge() {
    COMPLETION_BRIDGE.with(|cell| {
        *cell.borrow_mut() = None;
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
    /// The engine session this shell drives (the 1.5.0 Session
    /// contract). `None` only after
    /// [`Shell::finish_with_engine_finish`] consumes it through
    /// `Session::close` on a route tail; the accessors below panic
    /// on use after that point, which is a host bug by construction.
    /// The one sanctioned post-close read is
    /// [`Shell::current_host_cwd`], which the finish funnel feeds
    /// through `finish_cwd` below before consuming the session.
    pub session: Option<Session>,
    /// The shell's final directory, captured by
    /// [`Shell::finish_with_engine_finish`] after the EXIT trap has
    /// run and before the session is consumed by `Session::close`.
    /// The `--cwd-state` recorder in the binary reads the directory
    /// AFTER the finish funnel returns (docs/cwd-state.md §3: the
    /// record happens after the trap, from `PWD`, because a trap's
    /// `cd` updates `PWD` but not the process cwd), so the value
    /// must be taken while the session is still alive and handed
    /// over here. `None` until that funnel runs.
    finish_cwd: Option<PathBuf>,
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
    /// Windows factory-default "Git Bash compatible" prompt is active:
    /// every prompt rebuild re-renders it from the current directory until
    /// a PS1/theme claim displaces it (the claim check lives in
    /// `prompt_for_display`). Never set for one-shot shells.
    /// off-Windows the field is write-only (the refresh site is cfg'd out)
    /// — the allow keeps the -D warnings unix lanes clean.
    #[cfg_attr(not(windows), allow(dead_code))]
    gitbash_prompt_active: bool,
    /// niubash#592 (S2): sequence number of the most recent prompt pass
    /// whose clone `prompt_for_display` stamped with a computed Vcs
    /// value. Incremented only alongside a stamp; the number is
    /// probe/debugging identity for "which pass computed this value",
    /// never a freshness input — every pass recomputes unconditionally.
    /// Written only on the Windows surface (the segments Vcs computation
    /// is cfg'd there), like `gitbash_prompt_active` above.
    #[cfg_attr(not(windows), allow(dead_code))]
    vcs_pass_seq: u64,
    /// niubash#672: this shell seeded PS1 with the GNU compiled-in
    /// default at interactive entry because the environment carried no
    /// PS1 at all. While the variable still holds exactly that seeded
    /// value it is NOT a prompt claim: the product floor keeps
    /// rendering (design §14.5), exactly as if PS1 were unset. Any
    /// write by the user changes the value and claims the slot through
    /// the ordinary path; a user who deliberately assigns the default
    /// string verbatim keeps the floor (accepted, documented edge —
    /// the value alone cannot distinguish the two authors).
    ps1_seeded_default: bool,
    pub home_dir: PathBuf,
    pub shell_root: Option<PathBuf>,
    pub history_path: PathBuf,
    pub history_max_size: usize,
    pub history_ignore_space_prefixed: bool,
    /// Handle to the hosted history provider, retained so a host can run
    /// the session-end write explicitly (see
    /// [`Shell::flush_session_history`]). The standard one-shot funnel does
    /// NOT call this: since #281 its rc-0 exits return from main normally,
    /// so the provider's own Drop flush runs — this exists for embedding
    /// hosts that terminate through `std::process::exit` on paths the
    /// funnel does not own. `None` in test constructions that never touch
    /// history.
    history_provider: Option<Rc<RefCell<crate::history::RubashHistoryProvider>>>,
    /// niubash#661: armed at the end of `enter_interactive`, once the
    /// host has pointed `HISTFILE` at the configured history path.
    /// While armed, `refresh_history_settings` treats a live `HISTFILE`
    /// naming a different file as a redirect of the session stream.
    /// Before arming, an inherited `HISTFILE` must NOT redirect
    /// anything: the configured path is the file of record at startup
    /// (niubash#571), and only an assignment made inside the session
    /// counts as a redirect.
    history_redirect_armed: bool,
    /// niubash#661: a redirect was applied to the provider and
    /// `history_path` by the last refresh; the REPL loop consumes this
    /// to rebuild the line editor, whose history instance still backs
    /// the old file. Taken by `take_history_redirect`.
    history_redirect_pending_editor_rebuild: bool,
    pub menu_config: MenuConfig,
    pub editor_mode: EditorMode,
    /// Command-not-found package-search hint policy (niubash#249): off by
    /// default — a typo prints only the GNU one-liner;
    /// `niu config set command-not-found-hint wpm` opts in to a wpm-only
    /// near-miss suggestion.
    pub command_not_found_hint: CommandNotFoundHint,
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
    pub engine_bindings: Vec<crate::bind_surface::EngineBindEntry>,
    /// Registry generation at snapshot time; compared against
    /// the registry's live generation (through the bind surface)
    /// between prompts so a runtime `bind` call rebuilds the
    /// mirrored keymaps (GNU takes effect immediately; reedline
    /// keymaps are baked at editor build time).
    pub engine_bind_generation: u64,
    /// The user's persistent `[keymap].bind` rules from config.toml
    /// (niubash#630, K4), parsed into bind-surface mirror entries by
    /// [`Shell::load_user_keymap_rules`]. The editor build applies
    /// them inside the user layer, after `NIU_BINDKEYS`; their
    /// `bind -x` triggers resolve through [`Shell::run_bind_x_command`]
    /// at indices offset past `engine_bindings`, so this list and the
    /// snapshot together form the one resolution list.
    pub user_keymap_rules: Vec<crate::bind_surface::EngineBindEntry>,
    /// The user's persistent `[keymap].unbind` keys from config.toml
    /// (niubash#630, K4): slots the editor build removes outright at
    /// the end of the user layer.
    pub user_keymap_unbinds: Vec<(reedline::KeyModifiers, reedline::KeyCode)>,
    /// User-declared completion functions parsed from `NIU_COMPDEFS` in the
    /// startup rc: `(command, function)` pairs.
    pub compdefs: Vec<(String, String)>,
    /// Config-layer completion directories (`FullConfig::completion_dirs`),
    /// recorded at construction so the post-rc `load_user_compdefs` reload
    /// (NIU_COMPDEFS directory entries, issue #306) rebuilds the union
    /// instead of dropping the config surface.
    config_completion_dirs: Vec<PathBuf>,
    pub hooks: HookConfig,
    pub aliases: HashMap<String, String>,
    pub last_interactive_command: Option<String>,
    pub last_interactive_exit_code: Option<i32>,
    pub line_editor: Option<Reedline>,
    process_stdin_pipeline_bridge: bool,
    bash_prompt_command_running: bool,
    interactive_lines_read: usize,
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
    startup_invocation: Option<rubash::invocation::ShellInvocation>,
    // rubash#2129 (niu companion): this session's login classification,
    // owned by the host and derived ONLY from this process's own
    // invocation — the argv[0] `-` seed recorded at construction, the
    // `ShellInvocation` parse recorded by
    // `apply_invocation_before_startup`, or the legacy `-c -l` mark
    // (`mark_own_login_shell`). `login_shell_active` reads this field;
    // the executor's `__RUBASH_LOGIN_SHELL` carrier is a write-only
    // projection of it for engine faces (the `logout` builtin, the
    // `login_shell` shopt) and is never read back: the carrier is
    // exported, so a child that merely inherits it (a procsub respawn
    // out of an rc file — the fzf completion.bash shape) would
    // otherwise adopt the parent's login shape and re-run the profile
    // chain without bound.
    own_login_shell: bool,
    // rubash#2129 (niu companion): this process is a respawned internal
    // child of another session (the engine's procsub / `&` / coproc
    // transports), snapshotted once at construction from the same
    // marker pair the engine tests at `Executor::new`. A respawned
    // child is GNU's forked subshell: the parent session already ran
    // startup, so the startup funnel sources nothing for it.
    respawned_child: bool,
    // niubash#400: `Shell::new` found the rbash invocation face (argv[0]
    // basename or the shim-forwarded NIU_INVOKED_AS). Only the mark lives
    // here; `finish_startup` turns it into `set -o restricted` at the same
    // point the `-r`/`--restricted` flag form activates — after the startup
    // files (rubash invocation.rs ShellInvocation::finish_startup) — so a
    // BASH_ENV / profile / ENV chain sources unrestricted exactly like the
    // flag form and GNU.
    invoked_as_rbash: bool,
    // niubash#402: set by `execute_script_inner` when a script the host
    // ran ended in a top-level `exit` (the engine's `ExitCode` unwind, GNU
    // jump_to_top_level EXITPROG). The startup-file sourcer consumes it so an
    // `exit` in the rc terminates the invocation with that status instead of
    // being flattened to a plain status that lets the command line run.
    script_exit_jump: bool,
    // niubash R1: set by the interactive submission paths when the engine
    // unwound the submitted line/script with a top-level jump — the
    // `ExecuteError::ExitCode` / `FatalFunctionError` unwind (a real
    // `exit` inside a list, function, or alias expansion; the errexit
    // break), or the grouped reader route reporting one. The REPL consumes
    // it right after the submission finishes and ends the session with this
    // status, the same shape as GNU's jump_to_top_level unwinding
    // reader_loop (evalstring.c:618-619). A parse-abort `ExitCode(2)` is
    // excluded at the set sites (`take_parse_error`), so an interactive
    // syntax error still only discards its own group.
    session_exit_jump: Option<i32>,
    // Non-interactive fast path (`Shell::new_one_shot`,
    // `Shell::new_for_stdin_script_fast`): the shell serves a non-interactive
    // run that can never reach the REPL, so REPL-only state is never
    // initialized and the prompt backend stays a bare placeholder. Bash
    // renders no prompt and reads no completion state when non-interactive;
    // the history provider and managed aliases still stay (both pinned by
    // contract tests for -c).
    pub one_shot: bool,
}

pub struct StdinCurrentShellChild {
    script_name: String,
    script_path: PathBuf,
    positional_params: Vec<String>,
}

/// The `$0` a fresh shell starts with. GNU shell.c set_shell_name uses the
/// invocation name: when the process was reached through a shell shim the
/// shim forwards its argv[0] stem in NIU_INVOKED_AS, so `sh -c 'echo $0'`
/// prints `sh` (or `-sh` for a login-marked name), not niu — and since
/// rubash#598 the same forward carries the bash/rbash faces, so
/// `bash -c 'echo $0'` prints `bash`. Without a shim, argv[0] supplies
/// the face. Other invocations keep the historical `niu` default.
fn default_script_name() -> String {
    let forwarded = std::env::var("NIU_INVOKED_AS").ok().or_else(|| {
        std::env::args_os()
            .next()
            .map(|arg| arg.to_string_lossy().into_owned())
    });
    default_script_name_from(forwarded)
}

/// Consume the shim-forwarded invocation name out of the process
/// environment, removing it in the same breath: it describes how THIS
/// process was invoked, not a setting children should inherit. Without the
/// removal a nested `sh -c 'niu ...'` would hand the plain-niu child the
/// `sh` contract (POSIX mode, `$0` = sh) that GNU keys off argv[0] alone.
fn invoked_as_from_env() -> Option<String> {
    let invoked_as = std::env::var("NIU_INVOKED_AS").ok()?;
    std::env::remove_var("NIU_INVOKED_AS");
    Some(invoked_as)
}

fn default_script_name_from(raw: Option<String>) -> String {
    let Some(raw) = raw else {
        return "niu".to_string();
    };
    let basename = raw.rsplit(['/', '\\']).next().unwrap_or(&raw);
    let lowered = basename.to_ascii_lowercase();
    let stem = lowered.strip_suffix(".exe").unwrap_or(&lowered);
    let unmarked = stem.strip_prefix('-').unwrap_or(stem);
    // The four faces the shim forwards (rubash#598): sh/ash key the POSIX
    // contract, bash/rbash only need `$0` to read the invocation name
    // (GNU shell.c set_shell_name binds dollar_vars[0] to argv[0] for
    // every face; shell.c:1258-1299 keys rbash off it). Everything else
    // stays the plain-niu default.
    if matches!(unmarked, "sh" | "ash" | "bash" | "rbash") {
        stem.to_string()
    } else {
        "niu".to_string()
    }
}

/// Report a `[keymap]` rule that cannot be used (niubash#630, K4):
/// once per process per distinct message. The loader reruns whenever
/// the engine bind generation forces an editor rebuild, and a config
/// error must be *reported* (DESIGN §5.2) without becoming
/// per-rebuild noise.
fn report_keymap_rule_error(message: &str) {
    static REPORTED: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);
    let mut guard = REPORTED.lock().unwrap_or_else(|err| err.into_inner());
    let seen = guard.get_or_insert_with(Default::default);
    if seen.insert(message.to_string()) {
        eprintln!("{message}");
    }
}

impl Shell {
    /// The live engine session (the 1.5.0 Session contract).
    pub fn session(&self) -> &Session {
        self.session.as_ref().expect("shell session already closed")
    }

    /// The live engine session, mutable.
    pub fn session_mut(&mut self) -> &mut Session {
        self.session.as_mut().expect("shell session already closed")
    }

    /// The session's executor through the recorded tier-C seam
    /// (`Session::executor`): the direct faces the contract does
    /// not cover, enumerated in the API ledger. Prefer the
    /// contract faces (run methods, state views, capabilities)
    /// wherever one exists.
    pub fn executor(&self) -> &Executor {
        self.session().executor()
    }

    /// The session's executor, mutable, through the recorded
    /// tier-C seam (`Session::executor_mut`).
    pub fn executor_mut(&mut self) -> &mut Executor {
        self.session_mut().executor_mut()
    }

    /// Construct a fresh shell: load config, install Ctrl+C handler, inject
    /// winuxcmd onto PATH, set up completion state and history.
    pub fn new() -> anyhow::Result<Self> {
        // `niu` is the sole public executable. Keep `$0` aligned with the
        // user-facing command; explicit script and `-c` names still override
        // `niu` is the sole public executable. Keep `$0` aligned with the
        // user-facing command; explicit script and `-c` names still override
        // this value at their call sites. When the process was reached
        // through the sh.exe shim (the shim forwards its argv[0] stem via
        // NIU_INVOKED_AS), GNU shell.c set_shell_name shows the invocation
        // name instead: `sh -c 'echo $0'` prints `sh`, never niu. The
        // one-shot `-c` path (new_one_shot) carries the same name: the shim
        // contract keys off the invocation, not off interactivity.
        Self::new_with_script_name(Some(default_script_name().as_str()), false)
    }

    /// Construct a shell for scripts arriving on process stdin.
    ///
    /// This is the full constructor: a stdin shell built here can still be
    /// promoted to the REPL by its caller (`-i`, or a terminal stdio set),
    /// so the prompt backend and completion state are initialized eagerly.
    pub fn new_for_stdin_script() -> anyhow::Result<Self> {
        Self::new_with_script_name(None, false)
    }

    /// Construct a shell for scripts arriving on process stdin when the
    /// invocation is provably non-interactive (niubash#562): no `-i`, and
    /// the stdio set is not a terminal, so the caller will never promote
    /// this shell to the REPL or the interactive stdin driver.
    ///
    /// Same stdin naming contract as [`Shell::new_for_stdin_script`] (no
    /// script name is pre-bound; the caller binds argv\[0\] itself), but
    /// the REPL-only initialization is skipped exactly as in
    /// [`Shell::new_one_shot`] — the full prompt backend, the completion
    /// directory scan, and the completion snapshot. The startup-file
    /// behavior is untouched: the login profile chain and BASH_ENV are
    /// sourced by the caller after construction, driven by executor marks
    /// and shell fields both constructors produce identically.
    pub fn new_for_stdin_script_fast() -> anyhow::Result<Self> {
        Self::new_with_script_name(None, true)
    }

    /// Construct a minimal shell for non-interactive one-shot execution
    /// (`niu -c`, agent and CI harnesses).
    ///
    /// Skips everything only the interactive REPL reads — the prompt/theme
    /// backend, the completion directory scan, and the final completion
    /// snapshot. Bash itself renders no prompt and expands no completions
    /// when non-interactive, so the one-shot surface matches those
    /// semantics; managed aliases stay, while the history provider is
    /// installed only on interactive entry. The shim-forwarded invocation
    /// name applies here too: `sh -c ...` is a one-shot `sh`, and GNU
    /// shell.c set_shell_name keys the `$0`/POSIX contract off the
    /// invocation name, not off interactivity.
    pub fn new_one_shot() -> anyhow::Result<Self> {
        Self::new_with_script_name(Some(default_script_name().as_str()), true)
    }

    fn new_with_script_name(script_name: Option<&str>, one_shot: bool) -> anyhow::Result<Self> {
        // Consume the shim-forwarded invocation name before anything else
        // reads it: it must not leak into child processes. The `$0` default
        // above peeked at the same value (removal happens after that peek),
        // and the shell-name block below reuses `invoked_as` rather than
        // re-reading the now-cleared environment.
        let invoked_as = invoked_as_from_env();
        // rubash#2129 (niu companion): snapshot the engine's internal
        // respawn markers BEFORE the Session builder's `Executor::new`
        // consumes them — the same pair the engine itself tests there
        // (rubash executor/init.rs: `__RUBASH_COPROC_CHILD == "1"` or a
        // present `__RUBASH_SHELL_PID`). Read once, here, into host
        // state; the startup funnel never re-reads the raw markers.
        let respawned_child = std::env::var("__RUBASH_COPROC_CHILD").as_deref() == Ok("1")
            || std::env::var_os("__RUBASH_SHELL_PID").is_some();
        // niubash#495: an MSIX package root is read-only, so the default
        // shell-root resolution fails every session with os error 5. Point
        // NIU_ROOT at the package's private LocalCache before the root is
        // prepared — and before winuxcmd activation, which roots the link
        // family under the same writable tree.
        crate::msix::apply_writable_shell_root();
        // 1. Load runtime defaults and environment-backed state.
        // (niubash#572: HistoryConfig has no env overrides anymore — the
        // retired NIU_HISTORY_MODE switch is gone, so there is nothing to
        // apply here.)
        let config = load_config();
        // niubash#249: resolve the opt-in hint policy once at startup
        // (config.toml, overridden by NIU_COMMAND_NOT_FOUND_HINT).
        let command_not_found_hint = crate::config::resolve_command_not_found_hint();
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
        // NIU_SHELL_PATH_STYLE / WINUXSH_SHELL_PATH_STYLE are no longer set
        // here: rubash#556 (P0 #537) retired the engine-side readers, so the
        // vars are inert and only misinformed child processes. The engine now
        // renders $PWD/`pwd` in the POSIX persona form unconditionally; native
        // spelling is a spawn-boundary concern the host handles below.
        if let Some(shell_path) = &default_shell_path {
            std::env::set_var("SHELL", shell_path);
        }
        // The engine session is assembled through the 1.5.0
        // Session contract's builder (rubash::api::SessionBuilder):
        // the configuration the host used to write onto a bare
        // Executor after construction is builder input here, in
        // the same relative order. The process-exit role stays at
        // its default (library role): niubash's exit semantics are
        // the host funnels', exactly as when the executor was
        // constructed directly (the consumption audit's
        // adapted-equivalent row).
        let mut builder = Session::builder();
        crate::startup_trace::tick("executor created");
        // Script-mode history data plane stays with the engine: set -H,
        // fc, and the history builtin must behave in scripts exactly as
        // they do under rubash/GNU (errors10.sub: `history abcde` -> rc 2).
        // Reedline is the interactive history owner; enter_interactive()
        // turns the engine's machinery back off on the REPL path so
        // HISTFILE cannot create a second, competing history stream.
        // (Consequence for the builder: no history provider is
        // injected at construction — the provider is installed at
        // interactive entry, through the tier-C seam, so the
        // single-stream discipline above is preserved.)
        // Niubash delegates Windows elevation to external providers such as the
        // WPM gsudo package. The experimental Rubash builtin is opt-in only.
        if std::env::var("NIU_ENABLE_RUBASH_SUDO").as_deref() != Ok("1") {
            builder = builder.disable_builtin("sudo");
        }
        // niubash#578: argv[0] via args_os + lossy, never env::args() —
        // a non-Unicode face name (byte-named link, unpaired surrogate)
        // must not panic Shell construction; the lossy form is what the
        // face/$0 contract can show for such a name.
        let shell_name = invoked_as.or_else(|| {
            std::env::args_os()
                .next()
                .map(|arg| arg.to_string_lossy().into_owned())
        });
        let mut invoked_as_rbash = false;
        // rubash#2129 (niu companion): the host-owned login
        // classification (see the `own_login_shell` field). The only
        // construction-time source is the argv[0] `-` mark in the
        // shell-name block below — the same seed that arms the
        // builder's login funnel; the parse-based source joins in
        // `apply_invocation_before_startup`.
        let mut own_login_shell = false;
        // The posix *option* half of the sh/ash seed pair is applied
        // after build (the env half is a builder seed below): the
        // engine option store does not exist before the session
        // does, and nothing between here and build reads it.
        let mut posix_option_seed = false;
        if let Some(shell_name) = shell_name {
            let invoked_name = shell_name
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&shell_name)
                .trim_end_matches(".exe")
                .to_ascii_lowercase();
            builder = builder.env("__RUBASH_SHELL_NAME", invoked_name.as_str());
            // GNU shell.c set_shell_name: a leading `-' on argv[0] marks a
            // login shell independent of --login/-l; the stripped name drives
            // the POSIX-mode contract below.
            let unmarked_name = invoked_name.strip_prefix('-').unwrap_or(&invoked_name);
            if unmarked_name.len() < invoked_name.len() {
                // The builder's login seed is the engine's own
                // login funnel (the env + shopt pair
                // `Executor::mark_login_shell` writes), replacing
                // the host's hand copy of that pair. The pin this
                // builds against carries the Session contract, so
                // the funnel is available on every supported rev.
                builder = builder.login_shell(true);
                own_login_shell = true;
            }
            if matches!(unmarked_name, "sh" | "ash") {
                builder = builder.env("__RUBASH_POSIX_MODE", "1");
                posix_option_seed = true;
            }
            // niubash#400: a shell invoked under the name `rbash' starts
            // restricted (GNU shell.c:1258-1299 shell_is_restricted /
            // maybe_make_restricted key the face off the argv[0] basename,
            // after the `-' login marker is stripped). Only the MARK is taken
            // here: the option itself is written in `finish_startup`, the
            // same point the `-r`/`--restricted` flag form activates (rubash
            // invocation.rs `ShellInvocation::finish_startup`, "after the
            // host finishes startup files") — the startup files must see the
            // unrestricted shell so a BASH_ENV/profile/ENV chain sources
            // exactly like the flag form and GNU.
            invoked_as_rbash = unmarked_name == "rbash";
        }
        // Starship is initialized through its Bash integration even when
        // Niubash is invoked through the sh/bash command shims.
        builder = builder.env("STARSHIP_SHELL", "bash");
        if let Some(shell_path) = &default_shell_path {
            builder = builder.env("SHELL", shell_path.as_str());
        }
        // Bash sets $BASH to the full pathname used to execute the current
        // instance (bash(1), BASH variable); scripts probe it to detect bash.
        // Forward-slash spelling matches rubash's own standalone main.rs.
        // niubash#655: seed WITHOUT the export attribute, like GNU
        // (variables.c: BASH is a plain shell variable) and rubash's own
        // main (rubash#1325) — exporting it leaked BASH into every child
        // process's environment. (Builder env seeds carry no export
        // attribute, the same shape.)
        if let Ok(exe) = std::env::current_exe() {
            builder = builder.env("BASH", exe.to_string_lossy().replace('\\', "/"));
        }
        // P1 spawn takeover: leave the engine's external-file builtins (cat,
        // /bin/cat, mkdir, ...) at the rubash default (enabled) so script
        // suites under a POSIX PATH (=/usr/bin:/bin) resolve commands through
        // the engine's own external_command machinery exactly like rubash;
        // disabling it here used to force every such spawn through the
        // host command-not-found handler (rc=127).
        if let Some(root) = &shell_root {
            builder = builder.shell_root(root.clone());
        }
        if let Some(winuxcmd_path) = &selected_winuxcmd_path {
            builder = builder.winuxcmd_path(winuxcmd_path.clone());
        }
        if let Some(shell_path) = compatible_shell_path_from_env() {
            builder = builder.compatible_shell_path(shell_path);
        }
        if let Some(script_name) = script_name {
            // The builder's argv0 seed writes `__RUBASH_SCRIPT_NAME`;
            // the host's second name for it rides as a plain env
            // seed (the builder has no BASH_ARGV0 knob, and needs
            // none — it is an ordinary seed).
            builder = builder.argv0(script_name);
            builder = builder.env("BASH_ARGV0", script_name);
        }
        builder = builder.env("NIU_PROMPT_SYMBOL", config.shell.prompt_symbol.as_str());
        let mut session = builder
            .build()
            .map_err(|error| anyhow::anyhow!("session build failed: {error:?}"))?;
        // niubash#567 (FFI P4): resolve the FFI route provider's
        // discovery + tier table now so the first external command
        // does not pay for it. A no-op unless the master switch is
        // on — session startup (including the non-interactive fast
        // path) pays nothing for a disabled accelerator.
        crate::ffi::warm_up();
        if posix_option_seed {
            session.options().set("posix", true);
        }
        // rubash#2129 (niu companion): carrier normalization. The
        // executor the builder just assembled imported the process
        // environment wholesale, so it can carry a parent session's
        // exported startup-shape carriers — and shape is decided by
        // THIS process's own invocation, never by inheritance (GNU
        // decides from the invocation itself: shell.c:497-503 login,
        // shell.c:540-549 interactive). Unless the argv[0] `-` seed
        // above earned login, an inherited `__RUBASH_LOGIN_SHELL` is
        // removed before any startup funnel can see it; this session's
        // own login, when its parse earns it, is (re)written later by
        // `apply_invocation_before_startup` / `mark_own_login_shell`.
        // `__RUBASH_INTERACTIVE` is scrubbed unconditionally: no
        // construction path seeds it, so a copy present here is
        // inherited by construction — and every host face that earns
        // the interactive shape (enter_interactive, the piped `-i`
        // route, the theme-preview channel) writes it after
        // classification, from the session's own parse.
        if !own_login_shell {
            session.executor_mut().unset_env("__RUBASH_LOGIN_SHELL");
        }
        session.executor_mut().unset_env("__RUBASH_INTERACTIVE");
        // GNU bash defaults expand_aliases on only for interactive shells
        // (shell.c init_interactive()/init_interactive_script()); -c,
        // script-file, and stdin runs keep the engine default (off).
        // enter_interactive() enables it for the REPL entry points.
        session.options().set("expand_aliases", false);
        sync_executor_path_from_process_path(session.executor_mut());

        crate::startup_trace::tick("executor env ready");

        // 5. Apply managed aliases so explicit machine state remains
        // authoritative when names collide.
        let mut aliases = HashMap::new();
        for (name, value) in &config.aliases {
            if apply_alias(session.executor_mut(), name, value) {
                aliases.insert(name.clone(), value.clone());
            } else {
                log::warn!("Skipping invalid alias from config: {}", name);
            }
        }

        crate::startup_trace::tick("aliases");

        // 6. Prompt + theme. The template engine is the default backend;
        // the segment engine stays available for explicit configs. One-shot
        // shells never render a prompt: use the cheapest backend instead of
        // compiling the template or the segment engine.
        // Factory default (owner adjudication 2026-10-12): on Windows an
        // unstyled shell renders the built-in "Git Bash compatible" prompt
        // (src/git_prompt.rs — green user@host, yellow cwd, cyan branch,
        // no git child process). Explicit styles win; PS1/theme claims
        // still displace it per prompt (see prompt_for_display).
        #[cfg(windows)]
        let factory_style = "gitbash";
        #[cfg(not(windows))]
        let factory_style = "template";
        let prompt_style = config
            .shell
            .prompt_style
            .as_deref()
            .unwrap_or(factory_style);
        let prompt: PromptBackend = if one_shot {
            // One-shot shells never render a prompt. Keep the backend
            // terminal-free: BashPrompt::new probes terminal width during
            // construction and can spawn `tput` twice on non-tty Unix
            // invocations (niubash#351).
            PromptBackend::Template(NiubashPrompt::new(Some(String::new()), None))
        } else if prompt_style == "gitbash" {
            #[cfg(windows)]
            {
                PromptBackend::Bash(BashPrompt::new(
                    crate::git_prompt::render(
                        &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
                    ),
                    String::new(),
                ))
            }
            #[cfg(not(windows))]
            {
                // A config that names "gitbash" off-Windows falls back to the
                // template floor: the theme is a Windows-only surface (owner
                // adjudication) and src/git_prompt.rs does not compile there.
                PromptBackend::Template(NiubashPrompt::new(None, None))
            }
        } else if prompt_style == "segments" {
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
        normalize_executor_home_env(session.executor_mut(), &home_dir);
        ensure_windows_profile_env(session.executor_mut(), &home_dir);
        ensure_prompt_terminal_env(session.executor_mut());
        let history_path = config
            .history
            .path
            .clone()
            .unwrap_or_else(|| home_dir.join(".niubash_history"));
        // 8. Completion state.
        let mut initial_completion_state = CompletionState::new(
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        );
        initial_completion_state.behavior = config.completion_behavior;
        let completion_state = Arc::new(Mutex::new(initial_completion_state));

        crate::startup_trace::tick("completion state");

        // 9. Load completion dirs from config (inline, not in thread).
        // One-shot shells skip the scan: nothing reads it without a REPL.
        if !one_shot {
            let mut s = completion_state.lock().unwrap();
            s.load_completion_dirs(&config.completion_dirs);
        }

        let native_widgets = config.native_widgets.clone();

        let gitbash_prompt_active = cfg!(windows) && !one_shot && prompt_style == "gitbash";
        let mut shell = Self {
            session: Some(session),
            finish_cwd: None,
            completion_state,
            prompt: prompt.clone(),
            gitbash_prompt_active,
            vcs_pass_seq: 0,
            ps1_seeded_default: false,
            floor_prompt: prompt,
            home_dir,
            shell_root,
            history_path,
            history_max_size: config.history.max_size,
            history_ignore_space_prefixed: config.history.ignore_space_prefixed,
            history_provider: None,
            history_redirect_armed: false,
            history_redirect_pending_editor_rebuild: false,
            menu_config: config.menus.with_env_overrides(),
            editor_mode: config.editor.edit_mode,
            command_not_found_hint,
            edit_flags_seen: None,
            autosuggest: config.autosuggest.with_env_overrides(),
            syntax_highlighting: config.syntax_highlighting.with_env_overrides(),
            native_widgets,
            native_widget_bindings: Vec::new(),
            user_widget_bindings: Vec::new(),
            engine_bindings: Vec::new(),
            user_keymap_rules: Vec::new(),
            user_keymap_unbinds: Vec::new(),
            engine_bind_generation: 0,
            compdefs: Vec::new(),
            config_completion_dirs: config.completion_dirs.clone(),
            hooks: config.hooks,
            aliases,
            last_interactive_command: None,
            last_interactive_exit_code: None,
            line_editor: None,
            process_stdin_pipeline_bridge: false,
            bash_prompt_command_running: false,
            interactive_lines_read: 0,
            interactive: false,
            no_rc: false,
            no_profile: false,
            rc_file: None,
            no_editing: false,
            startup_invocation: None,
            own_login_shell,
            respawned_child,
            invoked_as_rbash,
            script_exit_jump: false,
            session_exit_jump: None,
            one_shot,
        };
        crate::startup_trace::tick("completion + widgets");
        shell.sync_executor_pwd_from_process_cwd();
        if !one_shot {
            shell.update_completion_state();
        }
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
        if let Some(value) = self.session_mut().variables().resolved_value("PS1") {
            if !value.is_empty() && crate::prompt::is_foreign_git_bash_ps1(&value) {
                self.session_mut().variables().unset("PS1");
            }
        }
    }

    pub fn enable_process_stdin_pipeline_bridge(&mut self) {
        self.process_stdin_pipeline_bridge = true;
    }

    pub fn set_script_name(&mut self, script_name: &str) {
        self.session_mut()
            .variables()
            .set("__RUBASH_SCRIPT_NAME", script_name);
        self.session_mut()
            .variables()
            .set("BASH_ARGV0", script_name);
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
        let command_name = cmd.words.first()?;
        let is_this_shell = matches!(command_name.as_str(), "$THIS_SH" | "${THIS_SH}")
            || self
                .executor()
                .get_env("THIS_SH")
                .is_some_and(|this_sh| same_shell_dir(this_sh, command_name));
        if !is_this_shell {
            return None;
        }

        let script_name = decode_to_visible_text(&cmd.words[1]);
        let script_path = self.executor().resolve_shell_path(&script_name);
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
            .session_mut()
            .variables()
            .resolved_value("__RUBASH_SCRIPT_NAME");
        let saved_bash_argv0 = self.session_mut().variables().resolved_value("BASH_ARGV0");
        let saved_function_stdin = self
            .session_mut()
            .variables()
            .resolved_value("__RUBASH_FUNCTION_STDIN");
        let saved_function_stdin_offset = self
            .session_mut()
            .variables()
            .resolved_value("__RUBASH_FUNCTION_STDIN_OFFSET");

        self.set_script_name(&child.script_name);
        self.session_mut()
            .variables()
            .set("__RUBASH_FUNCTION_STDIN", stdin_line);
        self.session_mut()
            .variables()
            .set("__RUBASH_FUNCTION_STDIN_OFFSET", "0");
        self.session_mut().set_positional(child.positional_params);

        let code = self.execute_script(&script)?;

        restore_executor_env(
            self.executor_mut(),
            "__RUBASH_SCRIPT_NAME",
            saved_script_name,
        );
        restore_executor_env(self.executor_mut(), "BASH_ARGV0", saved_bash_argv0);
        restore_executor_env(
            self.executor_mut(),
            "__RUBASH_FUNCTION_STDIN",
            saved_function_stdin,
        );
        restore_executor_env(
            self.executor_mut(),
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
        if self.history_provider.is_none() {
            let provider = Rc::new(RefCell::new(
                crate::history::RubashHistoryProvider::with_file(
                    self.history_max_size,
                    self.history_path.clone(),
                ),
            ));
            self.executor_mut().set_history_provider(provider.clone());
            self.history_provider = Some(provider);
        }
        // niubash#660: GNU interactive shells default the `history`
        // option ON (bashhist.c: remember_on_history starts enabled;
        // rubash's own main.rs asserts the same initial value at its
        // interactive entry). The engine table defaults it off because
        // scripts must not record, so assert the interactive default
        // here — but only when nothing has set the option yet: an
        // explicit startup setting (`niu +o history`, an embedding
        // host) is the user's word and stands. From here on the option
        // is live user state: `set +o history` / `set -o history` at
        // the prompt flip it, and `refresh_history_settings` snapshots
        // it into the settings cell the editor's record funnel
        // consults. (Entry used to force the option OFF so the
        // engine's machinery stayed down; that also froze the toggle
        // in the host's chosen position, which is the #660 bug. The
        // machinery stays down without it: the executor never creates
        // a session list while a provider is installed, so the option
        // being on cannot start a second stream.)
        if self
            .session_mut()
            .variables()
            .resolved_value("__RUBASH_SETOPT_history")
            .is_none()
        {
            self.session_mut().options().set("history", true);
        }
        // niubash#646: snapshot the inherited HIST* values into the
        // settings cell; the REPL loops refresh it at every prompt
        // boundary, so values the startup files set land there before
        // the first line is recorded.
        self.refresh_history_settings();
        self.session_mut()
            .variables()
            .set("__RUBASH_SHELL_NAME", "niu");
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
        // niubash#672: GNU seeds PS1 with its compiled-in default when
        // the environment carries none, so an interactive shell always
        // HAS a PS1 value (probes like ${PS1:+...} and `declare -p PS1`
        // must see it). Seed after the foreign-PS1 discard above: a
        // surviving inherited PS1 is a genuine claim and is never
        // overwritten, and rc files have not run yet, so a user PS1
        // still lands on top. The seed is a plain variable (never
        // exported, matching GNU) and `ps1_seeded_default` keeps it
        // from claiming the prompt slot away from the product floor.
        if self.executor().get_env("PS1").is_none() {
            self.executor_mut().set_env("PS1", GNU_DEFAULT_PS1);
            self.ps1_seeded_default = true;
        }
        // `$-` must contain `i` while startup files run: GNU sets
        // forced_interactive before run_startup_files (shell.c:672; flags.c:174
        // renders it in `$-`), so rc-level `case $- in *i*)` guards
        // (oh-my-bash, starship init, nvm) pass inside ~/.niubashrc. The
        // engine reads this marker in shell_option_flags (prompt_expansion.rs
        // `'i'` table arm); without it the marker only existed on the rubash
        // `-i` argv path, which the niubash REPL (no argv flags) never took.
        self.session_mut()
            .variables()
            .set("__RUBASH_INTERACTIVE", "1");
        // Job control must exist for every interactive session, whoever
        // drives it: GNU initialize_job_control ends with jobs.c:4869
        // `change_flag ('m', job_control ? '-' : '+')`, which keeps `m` for
        // an interactive shell (WSL GNU bash 5.3.x: `bash -i -c 'echo $-'`
        // -> `himBHc`). The engine applies that default at its own
        // interactive stdin entry (rubash script_driver.rs:4203-4209,
        // rubash#1991) — which only the piped `niu -i` face reaches. The
        // REPL faces drive lines through execute_interactive_line instead,
        // so without this mirror their `monitor` stayed off and `fg`/`bg`
        // with no jobs refused with the NON-interactive wording `fg: no
        // job control` (rubash job_builtins.rs:2332-2337, the fg_bg.def:
        // 108-113 `job_control == 0` branch) where GNU prints `fg: current:
        // no such job` (job_builtins.rs:2659-2671 write_job_not_found).
        // niubash#683. Same gate as the engine's: an explicit startup
        // transport value (`-m`/`+m`, `-o`/`+o monitor` ->
        // `__RUBASH_SETOPT_monitor`) is the user's word and stands; a
        // later `set +o monitor` in the rc or at the prompt wins over this
        // default because the key then exists.
        if self
            .session_mut()
            .variables()
            .resolved_value("__RUBASH_SETOPT_monitor")
            .is_none()
        {
            self.session_mut().options().set("monitor", true);
        }
        // GNU init_interactive (shell.c): interactive shells default
        // expand_aliases on so ~/.niubashrc aliases expand without a
        // user shopt line. Non-interactive entry points never call
        // this, keeping the GNU off default for scripts and -c.
        let expand_aliases = self
            .startup_invocation
            .as_ref()
            .and_then(|invocation| {
                invocation
                    .shopt_flags
                    .iter()
                    .rev()
                    .find_map(|(name, enabled)| (name == "expand_aliases").then_some(*enabled))
            })
            .unwrap_or(true);
        self.session_mut()
            .options()
            .set("expand_aliases", expand_aliases);
        // Reedline owns interactive history (see the construction-site note):
        // the engine's own list machinery stays off on the REPL path
        // because the executor never creates a session list while a
        // history provider is installed, so HISTFILE cannot create a
        // second, competing history stream. HISTFILE itself now
        // defaults to the host history's backing file (niubash#571)
        // instead of being unset. GNU interactive shells always have a
        // HISTFILE default (~/.bash_history), and the bare `history -a/-w/
        // -r/-n` forms resolve their filename from it — with HISTFILE unset
        // the provider dispatch reported `filename not specified` and every
        // bash-it-style per-prompt hook (`history -a; history -c; history
        // -r`) broke at the first leg. Pointing HISTFILE at the provider's
        // own backing file keeps exactly one stream: the builtins resolve
        // to the file the provider already owns, and the provider's
        // backing-file fast paths serve them. A HISTFILE the user exports
        // before launch is still not the file of record — the configured
        // history path (not an inherited HISTFILE) remains it; an
        // assignment made inside the session is a different matter and
        // does redirect the stream (niubash#661, armed below).
        let history_file = self.history_path.to_string_lossy().into_owned();
        self.session_mut()
            .variables()
            .set("HISTFILE", &history_file);
        // rubash#1248: assert the other half of the interactive history
        // contract. GNU init_interactive turns histexpand ON by default
        // (bashhist.c:288 HISTEXPAND_DEFAULT), and the engine's per-line
        // expansion entry (script_driver::pre_process_interactive_line,
        // which execute_interactive_line drives) gates on it alone - the
        // `history` flag above is deliberately not consulted there, because
        // the host owns the list. Without this assertion the flag could be
        // inherited off (a prior `set +H` in a shared executor, an
        // invocation that pre-set it), and `!!` would stay dead even with
        // the host driving expansion. A user's own `set +H` in rc or at
        // the prompt still wins: it lands after this, in line order.
        self.session_mut().options().set("histexpand", true);
        // niubash#661: HISTFILE now names the file of record, so a
        // later in-session assignment is unambiguously a redirect of
        // the history stream: arm the live-HISTFILE watch that
        // `refresh_history_settings` runs at every prompt boundary.
        self.history_redirect_armed = true;
    }

    /// Route a one-command AST to an easter egg when this shell is
    /// interactive. Returns the egg's exit code, or `None` so the caller keeps
    /// ordinary command resolution.
    fn easter_egg_exit(&self, commands: &[rubash::parser::CommandNode]) -> Option<i32> {
        if !self.interactive || commands.len() != 1 {
            return None;
        }
        let words = decoded_words(&commands[0].words);
        let head = words.first()?;
        if !crate::easter_eggs::is_registered(head) {
            return None;
        }
        crate::easter_eggs::dispatch(true, &words).ok().flatten()
    }

    /// Execute a single input line via rubash. Returns the exit code.
    pub fn execute_line(&mut self, line: &str) -> anyhow::Result<i32> {
        self.execute_line_with_options(line, false, 0)
    }

    /// Read alias-bearing interactive input through the engine's grouped reader.
    /// Each submission is a fresh read boundary, even when its line number is one.
    fn execute_interactive_reader_batch(
        &mut self,
        script: &str,
        tokens: &[Token],
        line_offset: usize,
    ) -> Option<i32> {
        if !self.interactive || !self.executor_mut().alias_expansion_enabled() {
            return None;
        }
        let needs_reader = tokens.iter().any(|token| {
            matches!(
                token.kind,
                TokenKind::Semicolon
                    | TokenKind::And
                    | TokenKind::Or
                    | TokenKind::Background
                    | TokenKind::Keyword
            ) || self.aliases.contains_key(&token.value)
        });
        if !needs_reader {
            return None;
        }
        let (code, exit_shell) =
            rubash::script_driver::run_script_with_history_at_line_offset_tracked(
                self.executor_mut(),
                script,
                line_offset,
                None,
            );
        if exit_shell {
            // The grouped reader already excluded the parse-abort discard
            // (niubash R1): the engine unwound this submission for real —
            // `true; exit 3`, an alias expanding to `exit`, errexit — so the
            // REPL must end the session instead of prompting again.
            self.session_exit_jump = Some(code);
        }
        self.sync_process_cwd_from_executor_pwd();
        self.sync_process_path_from_executor_path();
        self.sync_alias_mirror_from_executor();
        Some(code)
    }

    /// Record that an execution surfaced a top-level exit jump, unless the
    /// `ExitCode`/`FatalFunctionError` was raised by a parse abort
    /// (niubash R1): interactive syntax errors print the diagnostic and
    /// discard their own group only — GNU keeps prompting (eval.c:202-205),
    /// and script_driver's grouped reader makes the same split via
    /// `take_parse_error`.
    fn note_session_exit_jump(&mut self, code: i32) {
        if !self.executor_mut().take_parse_error() {
            self.session_exit_jump = Some(code);
        }
    }

    /// Classify an `ExecuteError::ExitCode` unwind surfacing from an
    /// interactive submission (niubash#610). The engine's grouped driver
    /// makes this exact split (script_driver.rs, rubash#1662): a signal
    /// DISCARD — an untrapped SIGINT delivered to an interactive shell,
    /// marked by the boundary dispatch via `signal_discard_pending` —
    /// is not a jump_to_top_level exit; the submission simply ends with
    /// `$? = code` and the reader prompts again. Only a non-discard
    /// unwind (a real top-level `exit`, errexit, ...) arms the session
    /// jump, via [`Shell::note_session_exit_jump`] (which itself
    /// excludes parse aborts, niubash R1).
    fn note_exit_code_unwind(&mut self, code: i32) {
        if self.executor_mut().take_signal_discard_pending() {
            // Consume the parse flag exactly as note_session_exit_jump
            // would, so no stale flag leaks into the next submission.
            let _ = self.executor_mut().take_parse_error();
            return;
        }
        self.note_session_exit_jump(code);
    }

    /// Take the pending interactive exit jump (niubash R1): `Some(status)`
    /// means the last submission ended in a top-level `exit`/errexit unwind
    /// and the REPL must end the session with that status.
    pub fn take_session_exit_jump(&mut self) -> Option<i32> {
        self.session_exit_jump.take()
    }

    /// niubash#616: run the engine's pending-signal dispatch for a prompt
    /// Ctrl+C. GNU runs a trapped INT at the prompt immediately (the trap
    /// observes `$?` = 130 on entry and `$?` is 130 again afterwards);
    /// reedline's raw mode produces no kernel SIGINT there, so the caller
    /// first delivers one through `ctrl_c::queue_prompt_sigint` and this
    /// method then gives the executor a single boundary poll — an empty
    /// program, so nothing executes, nothing is recorded, and no DEBUG
    /// trap or hook observes a synthetic command; only the poll at the
    /// tail of `execute_ast` runs. The unwind classification mirrors a
    /// real submission's (`note_exit_code_unwind`): an untrapped delivery
    /// is a DISCARD (no jump, no diagnostic), while a trap action that
    /// itself exits arms the session jump for the REPL to consume.
    pub fn dispatch_prompt_interrupt(&mut self) {
        self.session_exit_jump = None;
        // The trap (and the post-interrupt state) observes 130, as for a
        // command killed by SIGINT; the engine's dispatch preserves this
        // value into the action (`saved_exit` in run_pending_signal_traps).
        self.session_mut().set_last_status(130);
        let ast = rubash::parser::Ast {
            commands: Vec::new(),
        };
        match self.executor_mut().execute_ast(&ast) {
            Ok(()) => {}
            Err(rubash::executor::ExecuteError::ExitCode(code)) => {
                self.session_mut().set_last_status(code);
                self.note_exit_code_unwind(code);
            }
            Err(rubash::executor::ExecuteError::SigintUnwind) => {
                self.session_mut().set_last_status(130);
            }
            Err(_) => {}
        }
        // GNU leaves `$?` = 128+SIGINT after a prompt interrupt whether or
        // not a trap ran and whatever the trap's own last status was.
        if self.session_exit_jump.is_none() {
            self.session_mut().set_last_status(130);
        }
        self.sync_process_cwd_from_executor_pwd();
        self.sync_process_path_from_executor_path();
        self.sync_alias_mirror_from_executor();
    }

    fn execute_line_with_options(
        &mut self,
        line: &str,
        interactive_terminal_colors: bool,
        line_offset: usize,
    ) -> anyhow::Result<i32> {
        let line = if interactive_terminal_colors {
            line
        } else {
            line.trim()
        };
        if line.is_empty() {
            return Ok(0);
        }

        let line = normalize_native_windows_path_literals(line);
        let mut tokens = tokenize(&line);
        // niubash#513: tokenize's shopt simulation leaves the parse-time
        // extglob gate at the line's final state; restore the entry state
        // before parsing this line (see resync_parse_extglob_gate).
        self.resync_parse_extglob_gate();
        if tokens.is_empty() {
            return Ok(0);
        }
        if let Some(code) = self.execute_interactive_reader_batch(&line, &tokens, line_offset) {
            return Ok(code);
        }
        for token in &mut tokens {
            token.position += line_offset;
            token.column += line_offset;
            token.logical_line += line_offset;
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
                Err(rubash::executor::ExecuteError::ExitCode(code)) => {
                    // Keep `$?` in sync (#542) and record the jump so the
                    // REPL ends the session (#544 R1) — unless the unwind
                    // is a signal discard (#610).
                    self.session_mut().set_last_status(code);
                    self.note_exit_code_unwind(code);
                    code
                }
                Err(rubash::executor::ExecuteError::Return(code)) => code,
                Err(rubash::executor::ExecuteError::ExpansionFailure(code)) => code,
                Err(rubash::executor::ExecuteError::FatalFunctionError(code)) => {
                    self.note_session_exit_jump(code);
                    code
                }
                Err(rubash::executor::ExecuteError::SigintUnwind) => {
                    // rubash#1639 DISCARD verdict (the engine's grouped
                    // driver maps it identically): the foreground job
                    // died of SIGINT, the line is discarded, the reader
                    // prompts again with $? = 130 — no diagnostic, no
                    // session jump (niubash#610).
                    self.session_mut().set_last_status(130);
                    130
                }
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
            match self.executor_mut().execute_ast(&ast) {
                Ok(()) => self.session().last_status(),
                Err(rubash::executor::ExecuteError::ExitCode(code)) => {
                    self.session_mut().set_last_status(code);
                    self.note_exit_code_unwind(code);
                    code
                }
                Err(rubash::executor::ExecuteError::Return(code)) => code,
                Err(rubash::executor::ExecuteError::ExpansionFailure(code)) => code,
                Err(rubash::executor::ExecuteError::FatalFunctionError(code)) => {
                    self.note_session_exit_jump(code);
                    code
                }
                Err(rubash::executor::ExecuteError::SigintUnwind) => {
                    // rubash#1639 DISCARD verdict (the engine's grouped
                    // driver maps it identically): the foreground job
                    // died of SIGINT, the line is discarded, the reader
                    // prompts again with $? = 130 — no diagnostic, no
                    // session jump (niubash#610).
                    self.session_mut().set_last_status(130);
                    130
                }
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
        // Drop any jump recorded outside this submission (a mid-session rc
        // re-source, niubash#180) so only a jump raised below is consumed
        // when the REPL checks right after this call (niubash R1).
        self.session_exit_jump = None;
        let old_pwd = self.session_mut().variables().resolved_value("PWD");
        self.run_preexec_hooks(line);
        let line_offset = self.interactive_lines_read;
        self.interactive_lines_read += line.split_inclusive('\n').count().max(1);
        // rubash#1248: run the accepted line through the engine's
        // interactive history expansion (bashhist.c pre_process_line)
        // before anything tokenizes it. Until now the host handed the
        // engine the RAW line, so `!!` / `!n` / `!$` / `^a^b` reached
        // the tokenizer literally and died as "command not found" -
        // the engine-side state was always self-consistent; the host
        // simply never drove the entry point built for reedline-style
        // hosts. The expansion resolves against the history provider
        // enter_interactive installed (the live list reedline records
        // into), echoes the expanded line to stderr like GNU, and
        // rewrites the recorded entry to the expansion. Hooks and
        // bookkeeping keep receiving the RAW line, exactly as they
        // did before; only execution sees the expansion. A Discard
        // (failed expansion, or the `:p` print-only modifier)
        // executes nothing and leaves `$?` untouched, which is GNU's
        // behavior for both.
        //
        // 1.5.0 migration: the entry point is the Session contract's
        // prepare_line (the tier-A form of the script_driver entry
        // this used to call directly; same funnel, same outcomes).
        let code = match self.session_mut().prepare_line(line) {
            PreparedLine::Execute(expanded) => {
                let code = self.execute_line_with_options(&expanded, true, line_offset)?;
                self.invalidate_command_probe_after_line(&expanded);
                self.invalidate_git_prompt_after_line();
                code
            }
            PreparedLine::Discard => self.session().last_status(),
            // PreparedLine is non_exhaustive; an unknown future
            // outcome is treated as Discard (execute nothing,
            // leave `$?` untouched) until the host adopts it.
            _ => self.session().last_status(),
        };
        self.run_postcmd_hooks(code);
        self.run_zshaddhistory_hooks(line);
        self.remember_interactive_command(line, code);
        let new_pwd = self.session_mut().variables().resolved_value("PWD");
        if let (Some(old_pwd), Some(new_pwd)) = (old_pwd, new_pwd) {
            self.run_chpwd_hooks_if_changed(&old_pwd, &new_pwd);
        }
        self.update_completion_state();
        Ok(code)
    }

    /// Execute a complete multi-line interactive input block via rubash script
    /// execution while preserving REPL lifecycle hooks.
    ///
    /// rubash#1248: history expansion is deliberately NOT applied to the
    /// block as a whole. GNU expands per accepted LINE as it is read, and
    /// never inside heredoc bodies or compound-command continuations the
    /// reader has already grouped; a block-level pass here would rewrite
    /// text GNU leaves literal. Single-line submissions (the overwhelming
    /// interactive case) go through execute_interactive_line and expand.
    pub fn execute_interactive_script(&mut self, script: &str) -> anyhow::Result<i32> {
        // Same stale-jump drop as execute_interactive_line (niubash R1).
        self.session_exit_jump = None;
        let old_pwd = self.session_mut().variables().resolved_value("PWD");
        self.run_preexec_hooks(script);
        let line_offset = self.interactive_lines_read;
        self.interactive_lines_read += script.split_inclusive('\n').count().max(1);
        let code = self.execute_script_with_options(script, true, line_offset)?;
        self.invalidate_command_probe_after_line(script);
        self.invalidate_git_prompt_after_line();
        self.remember_interactive_command(script, code);
        let new_pwd = self.session_mut().variables().resolved_value("PWD");
        if let (Some(old_pwd), Some(new_pwd)) = (old_pwd, new_pwd) {
            self.run_chpwd_hooks_if_changed(&old_pwd, &new_pwd);
        }
        self.update_completion_state();
        Ok(code)
    }

    pub fn apply_invocation_before_startup(
        &mut self,
        invocation: &rubash::invocation::ShellInvocation,
    ) -> Result<(), String> {
        invocation.apply_to_executor_before_startup(self.executor_mut())?;
        // rubash#2129 (niu companion): the parse is this process's own
        // word on its shape. An earned `--login`/`-l` joins the
        // construction-time argv[0] seed in the host classification
        // the startup funnel reads (`login_shell_active`); the engine
        // apply above has already written the carrier projection for
        // it (`mark_login_shell`). An invocation that did not earn
        // login leaves the classification — and the executor
        // construction scrubbed of any inherited carrier — exactly as
        // they are.
        if invocation.login {
            self.own_login_shell = true;
        }
        self.startup_invocation = Some(invocation.clone());
        Ok(())
    }

    /// Record that this process's own invocation earned the login
    /// shape on a route whose parse never reaches `ShellInvocation`
    /// (the launcher's legacy `niu -c -l` route, niubash#587): the
    /// host classification (`own_login_shell`) and the engine's
    /// carrier + `login_shell` shopt projection
    /// (`Executor::mark_login_shell`, so `$-` gains `l`) move
    /// together, here only — each reader takes its own side.
    pub fn mark_own_login_shell(&mut self) {
        self.own_login_shell = true;
        self.executor_mut().mark_login_shell();
    }

    fn finish_startup(&mut self) {
        if let Some(invocation) = self.startup_invocation.take() {
            invocation.finish_startup(self.executor_mut());
        }
        // niubash#400: the rbash face activates HERE — the same point the
        // `-r`/`--restricted` flag write lands (the engine's
        // ShellInvocation::finish_startup, "after the host finishes startup
        // files") and after every startup file has been sourced. Activating
        // in `Shell::new` instead would arm the restricted `.`/slash gates
        // before the chain runs: a BASH_ENV file or the whole login profile
        // chain would be refused (`.: <path>: restricted`) where the flag
        // form and GNU load them fine. `set_shell_option` is idempotent, so
        // the extra `finish_startup` passes on the REPL paths are harmless.
        if self.invoked_as_rbash {
            // NOT through the session options view: `restricted` is
            // a flag-only option in the engine (GNU: `set -r`, never
            // a `set -o` enumeration entry), so the view's family
            // check does not know the name and its set() reports
            // false with no state change — the migration's rewrite
            // of this line to the view silently disarmed the rbash
            // persona (tests/rc_readonly_loader.rs
            // rbash_named_session_loads_the_generated_loader_then_restricts).
            // The executor's set_shell_option resolves the name in
            // the set machinery itself, including the
            // maybe_make_restricted readonly marks.
            self.executor_mut().set_shell_option("restricted", true);
        }
    }

    /// Source the user's REPL startup file once before the first prompt.
    ///
    /// `~/.niubashrc` is the automatic interactive rc entry point. Under
    /// the default `startup.rc-policy = "primary-only"` a missing primary
    /// leaves native defaults and no other rc is searched; an explicit
    /// `bashrc-fallback` setup pick makes `~/.bashrc` eligible only when
    /// the primary is genuinely absent (niubash#359; the two never stack).
    /// The rc is ordinary shell code: the built-in plugin/theme
    /// stack is retired (niubash#145), so NIU_PLUGINS / NIU_THEME lines in
    /// an old rc are inert assignments the host no longer reads. Returns
    /// `Some(status)` when the rc ended with a top-level `exit`
    /// (niubash#402); the caller must then end the invocation with that
    /// status instead of drawing a prompt or running the command line.
    pub fn run_startup_rc(&mut self) -> Option<i32> {
        let exit = self.run_startup_files(true);
        self.finish_startup();
        exit
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
        ensure_prompt_terminal_env(self.executor_mut());
        // niubash#359: a finished `niu setup` may have published a new
        // generated-loader generation; re-source it with the same decision
        // logic before the rc re-apply (the wizard's rc carries no managed
        // markers, so a clean machine resolves to Load — the same re-source
        // semantics the rc itself has always had here). Same ordering as
        // run_startup_files: loader first, then the NIU_REPL_STARTUP
        // marker, then the rc.
        let _ = self.load_generated_plugin_loader(Some(&path));
        self.session_mut().variables().set("NIU_REPL_STARTUP", "1");
        let outcome = self.source_file_into_current_shell(&path);
        self.session_mut().variables().unset("NIU_REPL_STARTUP");
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
    /// without any of the REPL-only markers. Returns `Some(status)` when the
    /// rc ended with a top-level `exit` (niubash#402); the caller must then
    /// terminate instead of running the command line.
    pub fn run_interactive_startup_rc(&mut self) -> Option<i32> {
        let exit = self.run_startup_files(false);
        self.finish_startup();
        exit
    }

    /// True when this session is a login shell BY ITS OWN INVOCATION:
    /// `--login`/`-l` in the host's own parse
    /// (`apply_invocation_before_startup` records it), the legacy
    /// `-c -l` mark (`mark_own_login_shell`), or an argv[0] whose
    /// basename starts with `-` (shell.c set_shell_name; Shell::new
    /// records the same seed that arms the builder's login funnel).
    /// The login profile chain then replaces the interactive-RC
    /// branch of the startup path (niubash#399); the env-file gates are
    /// independent of it (see `source_non_interactive_env`).
    ///
    /// rubash#2129 (niu companion): this used to read the executor's
    /// `__RUBASH_LOGIN_SHELL` carrier back — but the carrier is an
    /// exported projection, so a child that merely inherited it (the
    /// procsub respawn out of an rc file, fzf's completion.bash shape)
    /// adopted the parent's login shape and re-ran the profile chain,
    /// recursing without bound. The classification is host state now
    /// (`own_login_shell`); the carrier is written for engine faces
    /// and never read back here.
    pub fn login_shell_active(&self) -> bool {
        self.own_login_shell
    }

    /// The login profile chain (GNU shell.c:1836-1880
    /// run_login_profile_files, the same order the engine's
    /// `invocation::login_profile_files` implements): /etc/profile —
    /// resolved through the shell root on Windows, so the winuxcmd tree's
    /// `etc` directory answers when one ships — then the FIRST EXISTING of
    /// ~/.bash_profile, ~/.bash_login, ~/.profile. A missing candidate is
    /// skipped silently (open_startup_file's failed fopen is not an error).
    fn login_profile_paths(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let system_profile = self.executor().resolve_shell_path("/etc/profile");
        if system_profile.exists() {
            files.push(system_profile);
        }
        for name in [".bash_profile", ".bash_login", ".profile"] {
            let candidate = self.home_dir.join(name);
            // GNU execute_profile_file (shell.c:1124-1126) chains on
            // maybe_execute_file's result: only an ENOENT (missing) file
            // falls through to the next candidate — a candidate that
            // EXISTS but cannot be sourced (a directory, unreadable)
            // ends the chain with a warning (rubash#1160). Existence,
            // not is_file, is therefore the selection test.
            if candidate.exists() {
                files.push(candidate);
                break;
            }
        }
        files
    }

    /// Source the login profile chain (see [`Self::login_profile_paths`]),
    /// honouring `--noprofile`. Returns `Some(status)` when a profile ended
    /// with a top-level `exit` (the niubash#402 unwind, niubash#399). The
    /// chain replaces only the rc branch of the startup path; the env-file
    /// gates run independently of login (see the callers).
    fn source_login_profile_files(&mut self) -> Option<i32> {
        if self.no_profile {
            return None;
        }
        for path in self.login_profile_paths() {
            if !path.is_file() {
                // GNU evalfile.c:126-140: an existing-but-unsourceable
                // startup file warns (`is a directory`) instead of
                // failing silently (rubash#1160) — the same shape the
                // env-file paths below already use.
                eprintln!("niu: {}: Is a directory", path.display());
                continue;
            }
            if let Some(status) = self.source_startup_file(&path) {
                return Some(status);
            }
        }
        None
    }

    fn run_startup_files(&mut self, repl: bool) -> Option<i32> {
        // 1.5.0 migration adjudication: the Session contract's
        // source_startup_files / RcPolicy is deliberately NOT adopted
        // for this chain. The contract sources engine-selected files
        // per class and propagates a read failure as an error; this
        // chain's file selection is host policy (primary-only rc
        // resolution, the login chain, host-expanded $ENV), the
        // generated plugin loader interleaves at three
        // branch-specific slots under a guard keyed on the rc text,
        // failures warn and continue per file (GNU maybe_execute_file
        // semantics), and the branch tails (marker pairing,
        // completion snapshot, greeting) interleave between files.
        // No class decomposition expresses that without behavior
        // change, so the per-file funnel below stays the host's,
        // over the tier-C seam (see the migration report).
        // rubash#2129 (niu companion): the respawn door. A respawned
        // internal child is GNU's forked subshell (execute_cmd.c:1576
        // execute_in_subshell): the parent session already ran startup
        // and this process inherits its post-startup state, so the
        // funnel sources NOTHING for it — no profile chain, no rc, no
        // $ENV. The engine applies the same door in its own
        // run_startup_files; without it, a startup file that respawns
        // the shell (the fzf procsub shape) re-enters this funnel in
        // every generation.
        if self.respawned_child {
            return None;
        }
        // niubash#180 baseline: this session sources the current rc right
        // now, so a marker left by a setup that ran before this session
        // started is stale by definition — consume it here so only a setup
        // that runs while the session is live triggers the next-prompt
        // re-apply (apply_setup_config_if_pending).
        crate::setup_wizard::clear_stale_apply_pending_marker(&self.home_dir);
        let home_dir = self.home_dir.clone();
        normalize_executor_home_env(self.executor_mut(), &home_dir);
        ensure_windows_profile_env(self.executor_mut(), &home_dir);
        ensure_prompt_terminal_env(self.executor_mut());
        // GNU shell.c:1833 run_startup_files: the login profile chain is
        // checked first and replaces the interactive-RC alternative — a
        // login shell sources /etc/profile and the first user profile
        // whether or not it is interactive, and --rcfile (a bashrc_file
        // concept) is never consulted on it. niubash#399: an `exit` in a
        // profile ends the invocation with the profile's status before any
        // prompt or command line, like every other startup file
        // (niubash#402).
        let login = self.login_shell_active();
        if login {
            if repl {
                self.session_mut().variables().set("NIU_REPL_STARTUP", "1");
            }
            let exit = self.source_login_profile_files();
            if let Some(status) = exit {
                // niubash#402 discipline: the jump's status must survive the
                // marker cleanup, and none of the remaining startup steps run.
                if repl {
                    self.session_mut().variables().unset("NIU_REPL_STARTUP");
                }
                return Some(status);
            }
            if repl {
                self.session_mut().variables().unset("NIU_REPL_STARTUP");
            }
        }
        // GNU shell.c:1241-1246 (bash --posix) and shell.c:1238-1239 (invoked
        // as sh, which Shell::new turns into posix mode): the interactive
        // startup file is `$ENV`, not the bash rc file, and an unset `$ENV`
        // sources nothing. The $ENV gate is INDEPENDENT of login state —
        // `sh -l -i -c` sources the profile chain and $ENV alike
        // (niubash#412 review of the #399 PR). --rcfile is a bashrc_file
        // concept and is ignored in this branch. An `exit` in the file ends
        // the invocation with its status (niubash#399).
        if self.posix_mode_active() {
            // niubash#603: the generated plugin loader is niu's own
            // activation plane, not a user file — posix mode governs the
            // choice and order of USER files (the profile chain above,
            // then $ENV), not whether the product integration loads; the
            // mode flag must not silently strip it (the #590 defect
            // class, which the login branch below already fixed for
            // ordinary shells). The slot mirrors the ordinary branch's
            // relative order: a login shell has just sourced the profile
            // chain, so the loader lands after it — the #590 slot — and
            // before $ENV, the posix rc face, which is where the loader
            // sits relative to the rc on the non-login branch (loader
            // first, so the user file's lines win). An `exit` inside
            // unwinds like any startup file, before $ENV and the login
            // tail run. No rc is sourced in this mode at all, so the
            // double-execution guard has no rc text to scan (None), as
            // on the login branch.
            if let Some(status) = self.load_generated_plugin_loader(None) {
                return Some(status);
            }
            let exit = self.execute_env_file("ENV");
            if let Some(status) = exit {
                // niubash#422: an ENV `exit` is a startup-file jump like any
                // other (niubash#402) — return the status before the login
                // tail runs. Letting the tail through made `sh -l -i` emit
                // greeting output AFTER the ENV file had already ended the
                // invocation; GNU (WSL bash 5.3.0 oracle: `sh -l -i`, $ENV =
                // "echo ENV-RAN; exit 7") terminates silently with rc 7 and
                // never renders the first prompt (the control run without
                // the exit does). The same early return the profile and rc
                // exit paths above take.
                return Some(status);
            }
            if login {
                self.update_completion_state();
                if repl {
                    self.finish_startup();
                    self.run_greeting_hooks();
                }
            }
            return None;
        }
        if login {
            // The profile chain replaces the rc branch only (shell.c:1833);
            // the env files above are not part of that exchange.
            // niubash#590: the generated plugin loader is niu's own
            // activation plane, not the rc — the docs promise the host
            // sources it with no login exception, and returning here
            // silently disabled every plugin and theme for login shells.
            // Load it after the profile, before the completion snapshot
            // (so loader-provided definitions and NIU_* registrations
            // reach the REPL like they do on the rc branch); an `exit`
            // inside unwinds like any startup file. Nothing else is
            // sourced on this branch, so the double-execution guard has
            // no rc text to scan (None). The rc itself stays skipped —
            // that half is GNU login parity.
            if let Some(status) = self.load_generated_plugin_loader(None) {
                return Some(status);
            }
            self.update_completion_state();
            if repl {
                self.finish_startup();
                self.run_greeting_hooks();
            }
            return None;
        }
        if self.no_rc {
            return None;
        }
        let rc_path = self.startup_rc_path();
        // niubash#359: the generated plugin loader is the host-owned,
        // read-only activation channel. It sources BEFORE the user rc so
        // user overrides (PS1/alias/function) win, and only when the rc
        // text carries no superseded managed-block marker (the
        // double-execution guard — an old block in the rc is the live path,
        // and loading both would execute the plugin set twice) and the
        // rollback switch (NIU_PLUGIN_LOADER=off) allows loading. The step
        // is zero-write by construction: it reads bytes and sources a file
        // niu generated under an explicit verb.
        if let Some(status) = self.load_generated_plugin_loader(rc_path.as_deref()) {
            return Some(status);
        }
        let path = rc_path?;
        if !path.exists() {
            // GNU maybe_execute_file passes FEVAL_ENOENTOK (evalfile.c:345):
            // a missing --rcfile is silent, exactly like a missing default.
            return None;
        }
        if !path.is_file() {
            // GNU evalfile.c:126-140: an existing-but-unsourceable rc
            // (a directory) warns and the shell runs without it — the
            // old is_file filter dropped it silently (rubash#1160).
            eprintln!("niu: {}: Is a directory", path.display());
            self.update_completion_state();
            if repl {
                self.finish_startup();
                self.run_greeting_hooks();
            }
            return None;
        }

        if repl {
            self.session_mut().variables().set("NIU_REPL_STARTUP", "1");
        }
        let exit = self.source_startup_file(&path);
        if let Some(status) = exit {
            // niubash#402: the rc ended with `exit` — the invocation
            // terminates with that status and none of the remaining startup
            // steps (completion sync, greeting) or the caller's command line
            // run (GNU jump_to_top_level unwinds reader_loop). Unset the
            // marker directly so the cleanup cannot overwrite the jump's
            // status.
            if repl {
                self.session_mut().variables().unset("NIU_REPL_STARTUP");
            }
            return Some(status);
        }
        if repl {
            self.session_mut().variables().unset("NIU_REPL_STARTUP");
        }

        self.update_completion_state();
        if repl {
            self.finish_startup();
            self.run_greeting_hooks();
        }
        None
    }

    /// The generated plugin loader step of the rc startup branch
    /// (niubash#359): decide read-only whether the host loads
    /// `~/.niubash/generated/plugins.niu` this startup, source it before
    /// the user rc when allowed, and serve the one-time migration hint
    /// when the rc still carries a superseded managed block. Returns
    /// `Some(status)` only when the loader ended with a top-level `exit`
    /// (the same niubash#402 unwind as any startup file).
    fn load_generated_plugin_loader(&mut self, rc_path: Option<&Path>) -> Option<i32> {
        let rc_text = rc_path.and_then(|path| std::fs::read_to_string(path).ok());
        let loader_text = crate::plugins::loader::read_loader_text();
        if loader_text.is_none() {
            // niubash#586 (secondary face): read_loader_text folds an
            // unreadable or non-UTF-8 loader into the same None as a
            // missing one, so a corrupt loader failed silently. A file
            // that exists but cannot be read as text is diagnosed, not
            // loaded and not mistaken for "no plugins yet".
            let path = crate::plugins::loader::loader_path();
            if path.is_file() {
                eprintln!(
                    "niu: warning: the generated plugin loader exists but could not be read \
                     as text, so it was not loaded: {}; regenerate it with `niu plugin sync`",
                    path.display()
                );
            }
        }
        match crate::plugins::loader::startup_decision(rc_text.as_deref(), loader_text.as_deref()) {
            crate::plugins::loader::StartupDecision::Missing
            | crate::plugins::loader::StartupDecision::OptedOut => None,
            crate::plugins::loader::StartupDecision::SupersededBlockPresent => {
                // The old managed block in the user rc is the live
                // activation path; the loader stays off (double-execution
                // guard). One-time read-only hint (design §4 mitigation):
                // the marker write is config-dir internal and best-effort,
                // never blocking, and only ever happens in this
                // legacy-block situation — the clean-home startup matrix
                // stays byte-for-byte untouched.
                if crate::plugins::loader::should_show_legacy_hint() {
                    eprintln!(
                        "niubash: your rc still carries an old managed plugin block, so the \
                         generated plugin loader stays off; review the migration with:  \
                         niu plugin migrate-rc"
                    );
                }
                None
            }
            crate::plugins::loader::StartupDecision::Invalid(reason) => {
                // niubash#586: a truncated or foreign loader is never
                // sourced (the validate() gate in startup_decision).
                // Warn with the path and the regeneration verb, and
                // keep the shell fully usable without the loader.
                eprintln!(
                    "niu: warning: the generated plugin loader failed validation and was \
                     not loaded ({reason}): {}; regenerate it with `niu plugin sync`",
                    crate::plugins::loader::loader_path().display()
                );
                None
            }
            crate::plugins::loader::StartupDecision::Load => {
                let path = crate::plugins::loader::loader_path();
                self.source_startup_file(&path)
            }
        }
    }

    /// Source one startup file with GNU maybe_execute_file semantics:
    /// failures warn but never abort the shell (evalfile.c:339). Returns
    /// `Some(status)` only when the file ended with a top-level `exit`
    /// (niubash#402) — an ordinary nonzero status is logged and ignored,
    /// exactly as GNU keeps reading after a failing command.
    fn source_startup_file(&mut self, path: &Path) -> Option<i32> {
        let code = match self.source_file_into_current_shell(path) {
            Ok(code) => code,
            Err(err) => {
                log::warn!("{} failed: {}", path.display(), err);
                1
            }
        };
        self.sync_process_path_from_executor_path();
        // niubash#402: an `exit` executed while the startup file is sourced
        // terminates the invoking shell with that status (GNU
        // jump_to_top_level unwinds reader_loop), so hand it to the caller
        // to end the invocation; a plain failing command is not a jump and
        // only warns.
        if self.script_exit_jump {
            self.script_exit_jump = false;
            return Some(code);
        }
        if code != 0 {
            log::warn!("{} exited with status {}", path.display(), code);
        }
        None
    }

    /// Re-sync the engine's parse-time extglob gate with the executor's
    /// live shopt state after a whole-text `tokenize` (niubash#513).
    ///
    /// rubash's tokenizer simulates GNU's per-line `shopt -s/-u extglob`
    /// flips as it lexes (lexer `line_extglob_mode_change`), so when
    /// `tokenize` returns, the process-global parse gate holds the
    /// script's FINAL simulated state. rubash's own driver parses each
    /// complete command before later text is tokenized, so it never
    /// observes that end state; this host tokenizes the whole input first
    /// and parses afterwards, so a flip anywhere in the text would leak
    /// BACKWARD into earlier commands: `case` patterns the lexer split at
    /// `(` under a closed gate were reassembled by the parser's live-gate
    /// check, and the parse-error fallback driver re-parsed its first
    /// group under the leaked gate (`niu -c` ran `+(...)` patterns before
    /// the `shopt -s extglob` that lexically follows them). GNU decides at
    /// read time (parse.y:5466 `read_token_word`; `reset_parser`
    /// parse.y:3502 syncs the gate from the CURRENT shopt state), so
    /// restore the gate to the state the executor carries on entry before
    /// parsing: a flip the input itself performs takes effect when its
    /// `shopt` command executes — the builtin re-syncs the gate — which
    /// is GNU's cadence for the NEXT read. The executor state is the
    /// engine's `__RUBASH_SHOPT_STATE` carrier (the same read
    /// script_driver's `shopt_state_enabled` performs); an absent carrier
    /// means the default state, where extglob is off.
    fn resync_parse_extglob_gate(&mut self) {
        let enabled = self
            .session_mut()
            .variables()
            .resolved_value("__RUBASH_SHOPT_STATE")
            .is_some_and(|state| state.split('\u{1f}').any(|entry| entry == "extglob"));
        self.session_mut().options().set("extglob", enabled);
    }

    /// True when this shell runs in POSIX mode. All three sources flip it
    /// before startup files run: invoked as sh/ash (Shell::new),
    /// `--posix`, and `-o posix` (ShellInvocation::apply_to_executor).
    fn posix_mode_active(&self) -> bool {
        self.executor().get_env("__RUBASH_POSIX_MODE") == Some("1")
    }

    /// GNU shell.c:1103 execute_env_file: an unset or empty variable sources
    /// nothing; otherwise the value is expanded and the file executed with
    /// FEVAL_ENOENTOK (a missing file is silently skipped, evalfile.c:120).
    /// Returns `Some(status)` when the file ended with a top-level `exit`
    /// (niubash#399: GNU ends the shell with that status — an `exit` in a
    /// posix `$ENV` file under `-i -c` terminates the invocation).
    fn execute_env_file(&mut self, name: &str) -> Option<i32> {
        // GNU get_string_value reads the shell variable table, which at
        // startup-file time is the inherited environment; prefer the
        // executor table so an earlier startup file could export it.
        let raw = self
            .session_mut()
            .variables()
            .resolved_value(name)
            .or_else(|| std::env::var(name).ok())
            .filter(|value| !value.trim().is_empty());
        let raw = raw?;

        let home_dir = self.home_dir.clone();
        normalize_executor_home_env(self.executor_mut(), &home_dir);
        let path = self.resolve_non_interactive_env_path(&raw);
        if !path.exists() {
            // GNU evalfile.c:120: ENOENT with FEVAL_ENOENTOK is silent.
            return None;
        }
        // GNU maybe_execute_file (evalfile.c) only refuses directories;
        // every other existing path is opened and read, so a character
        // device like /dev/null sources as an empty file (niubash#512:
        // `!path.is_file()` misreported /dev/null as a directory).
        if path.is_dir() {
            eprintln!("niu: {}: Is a directory", path.display());
            return None;
        }
        let exit = self.source_startup_file(&path);
        self.update_completion_state();
        exit
    }

    /// Source the non-interactive startup files. GNU shell.c:722
    /// run_startup_files: a login shell sources the login profile chain
    /// first (niubash#399), whether or not a command string follows. The
    /// BASH_ENV / NIU_ENV gate below is INDEPENDENT of login —
    /// shell.c:1214-1220 keys on interactivity and posix mode, never on
    /// login state, so `bash -l -c` sources the profile chain AND the env
    /// file (niubash#412 review of the #399 PR). Returns `Some(status)`
    /// when a startup file ended with a top-level `exit` (the niubash#402
    /// unwind): the caller must end the invocation with that status instead
    /// of running its command line or script — GNU ends the shell for a
    /// profile `exit` and for a BASH_ENV `exit` alike.
    ///
    /// The env-file branch mirrors GNU bash's BASH_ENV behavior
    /// (shell.c:1214-1220): a non-interactive, non-posix, bash-mode shell
    /// sources the file named by the environment variable before running
    /// its command or script. POSIX mode and sh-invoked shells source no
    /// environment file at all — `$ENV` is interactive-only under POSIX
    /// (shell.c:1244). Niubash adds a dedicated NIU_ENV variable that takes
    /// precedence over BASH_ENV in every mode, so an agent shell can point
    /// at a dedicated init file without touching the bash-compatible name.
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
    pub fn source_non_interactive_env(&mut self) -> Option<i32> {
        // rubash#2129 (niu companion): the respawn door, as in
        // run_startup_files — a respawned internal child (the procsub
        // `niu -c 'complete -p ...'` generation is exactly this route)
        // sources no profile chain and no BASH_ENV/NIU_ENV file; the
        // parent session's startup is the only one. finish_startup
        // still runs so the route tails keep their shape.
        if self.respawned_child {
            self.finish_startup();
            return None;
        }
        let mut exit = if self.login_shell_active() {
            self.source_login_profile_files()
        } else {
            None
        };
        if exit.is_none() {
            exit = self.source_non_interactive_startup_file();
        }
        self.finish_startup();
        exit
    }

    fn source_non_interactive_startup_file(&mut self) -> Option<i32> {
        // niubash#588: GNU reads the shell variable table, which by the
        // time the env file is considered already carries whatever the
        // login profile exported (source_non_interactive_env runs the
        // profile chain first, this gate second) — a profile that
        // exports BASH_ENV points the shell at the file. Mirror the
        // $ENV read in execute_env_file: executor table first, process
        // environment as fallback. NIU_ENV keeps precedence over
        // BASH_ENV in both sources.
        let niu_raw = self
            .session_mut()
            .variables()
            .resolved_value(NIU_ENV_VAR)
            .or_else(|| std::env::var(NIU_ENV_VAR).ok())
            .filter(|value| !value.trim().is_empty());
        // GNU shell.c:1216 gates BASH_ENV on posixly_correct == 0 and
        // act_like_sh == 0; NIU_ENV stays available as the niubash extension
        // (agents invoke `niu -c`, never the sh shim).
        let bash_raw = if self.posix_mode_active() {
            None
        } else {
            self.session_mut()
                .variables()
                .resolved_value(BASH_ENV_VAR)
                .or_else(|| std::env::var(BASH_ENV_VAR).ok())
                .filter(|value| !value.trim().is_empty())
        };
        let raw = niu_raw.or(bash_raw)?;

        let home_dir = self.home_dir.clone();
        normalize_executor_home_env(self.executor_mut(), &home_dir);
        let path = self.resolve_non_interactive_env_path(&raw);
        if !path.exists() {
            // GNU maybe_execute_file passes FEVAL_ENOENTOK (evalfile.c:345):
            // a missing BASH_ENV file is skipped silently, keeping `-c`
            // stdout/stderr byte-stable for one-shot agents.
            return None;
        }
        // Only a directory is refused (see execute_env_file, niubash#512):
        // /dev/null and other non-regular files are read like GNU does.
        if path.is_dir() {
            eprintln!("niu: {}: Is a directory", path.display());
            return None;
        }
        // niubash#399: an `exit` in the env file is a jump like any other
        // startup file's (GNU ends the shell with its status); propagate it
        // so the invoker terminates before the command line runs.
        let exit = self.source_startup_file(&path);
        self.update_completion_state();
        exit
    }

    /// Resolve a non-interactive environment file path from the raw value of
    /// NIU_ENV or BASH_ENV. A leading ~ expands against the shell home
    /// directory; the remainder goes through the executor's path resolver so
    /// /c/..., C:/..., and native backslash spellings all work.
    fn resolve_non_interactive_env_path(&self, raw: &str) -> PathBuf {
        // niubash#589: GNU expands the env-file value before opening it
        // (tilde + parameter expansion against the shell variable
        // table); the old resolver expanded only "~" and "~/" and let
        // `$HOME/...`, `${VAR}`, `~user` and `~\` fall through to the
        // path resolver literally, where the missing file was then
        // skipped silently — the startup file simply never ran.
        let expanded = self.expand_env_file_value(raw);
        let path = if expanded == "~" {
            self.home_dir.clone()
        } else if let Some(rest) = expanded.strip_prefix("~/") {
            self.home_dir.join(rest)
        } else if let Some(rest) = expanded.strip_prefix("~\\") {
            self.home_dir.join(rest)
        } else {
            PathBuf::from(shell_path_to_host_path(&expanded))
        };
        self.executor().resolve_shell_path(&path.to_string_lossy())
    }

    /// Expand an env-file value (NIU_ENV / BASH_ENV / $ENV) the way GNU
    /// expands it before maybe_execute_file: a leading `~user` resolves
    /// against that user's home, then `$VAR` / `${VAR}` substitute from
    /// the executor variable table (process environment as fallback —
    /// the same read order as the variable lookup itself, niubash#588).
    /// Only the plain forms are expanded; a braced form that is not a
    /// plain name is left literal rather than half-expanded, and an
    /// unresolvable `~user` stays literal too (GNU leaves the tilde in
    /// place when no home can be found). Expansion is a single pass:
    /// substituted values are data, never re-expanded.
    fn expand_env_file_value(&self, raw: &str) -> String {
        let after_tilde = self.expand_env_file_tilde_user(raw);
        expand_env_file_params(&after_tilde, |name| {
            self.executor()
                .get_env(name)
                .map(str::to_owned)
                .or_else(|| std::env::var(name).ok())
                .unwrap_or_default()
        })
    }

    /// Resolve a leading `~user` prefix (niubash#589). The bare forms
    /// ("~", "~/", "~\") name the shell's own home, which the caller
    /// resolves against home_dir — leave those untouched here.
    fn expand_env_file_tilde_user(&self, raw: &str) -> String {
        let Some(rest) = raw.strip_prefix('~') else {
            return raw.to_string();
        };
        // The tilde prefix runs to the first separator, '/' or '\'
        // alike (the Windows-first spelling `~\` is a filed shape).
        let sep = rest.find(['/', '\\']).unwrap_or(rest.len());
        let user = &rest[..sep];
        if user.is_empty() {
            return raw.to_string();
        }
        let home = if current_login_user().as_deref() == Some(user) {
            Some(self.home_dir.clone())
        } else {
            passwd_home_dir(user)
        };
        match home {
            Some(home) => format!("{}{}", home.to_string_lossy(), &rest[sep..]),
            None => raw.to_string(),
        }
    }

    fn startup_rc_path(&self) -> Option<PathBuf> {
        if let Some(file) = &self.rc_file {
            return Some(file.clone());
        }
        let primary = self.home_dir.join(NIU_RC_FILE);
        if primary.exists() {
            return Some(primary);
        }
        // niubash#359 `startup.rc-policy`: only an explicit setup-persisted
        // `bashrc-fallback` pick lets ~/.bashrc fill the personal-rc slot,
        // and only when the primary is GENUINELY absent — symlink_metadata
        // does not follow the final entry away, so a dangling/cyclic
        // .niubashrc symlink still owns the slot, and any non-NotFound
        // error (permission, traversal, IO) is ambiguous presence rather
        // than absence. `primary.exists()` was already false for both
        // cases; keep it as the first gate and resolve the ambiguity only
        // when the opt-in is on. A missing/broken .bashrc leaves defaults;
        // its later open/source failure never selects another file.
        if crate::config::resolve_rc_policy() == crate::config::RcPolicy::BashrcFallback {
            match std::fs::symlink_metadata(&primary) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    let bashrc = self.home_dir.join(".bashrc");
                    if bashrc.exists() {
                        return Some(bashrc);
                    }
                }
                // Any other outcome (Ok metadata or an ambiguous error)
                // means the primary slot is occupied — no fallback.
                _ => return Some(primary),
            }
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
        match self.executor().get_env("PS1") {
            Some(value) if !value.is_empty() => {
                // niubash#672: the host-seeded GNU default is not a
                // claim while the variable still holds the seeded
                // value verbatim.
                !(self.ps1_seeded_default && value == GNU_DEFAULT_PS1)
            }
            _ => false,
        }
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
        ensure_prompt_terminal_env(self.executor_mut());
        self.session_mut().set_last_status(last_exit_code);
        // The engine owns the GNU semantics (eval.c:305
        // execute_prompt_command): indexed-array elements in order,
        // associative arrays refused, scalar string once. The host only
        // triggers the pre-prompt hook; PROMPT_COMMAND itself may be an
        // array under bash >= 5.1 (oh-my-bash installs one), which the
        // engine dispatches without the host re-implementing it.
        self.session_mut().run_prompt_command();
        let exit_jump = self.executor_mut().take_exit_jump_pending();
        if !exit_jump {
            // Normal return: the pre-PC `$?` is restored (the engine already
            // rolled its own snapshot back; this keeps the host view in
            // sync). On the jump path the restore is skipped — the jump's
            // status IS the session status (parse.y:3021's restore line is
            // longjmped past in GNU).
            self.session_mut().set_last_status(last_exit_code);
        }
        self.bash_prompt_command_running = false;
        exit_jump
    }

    /// The prompt the next REPL read should render. On the Windows factory
    /// default ("gitbash") this re-renders from the current directory every
    /// call — the branch/dirty segment follows `cd` without any PS1 claim.
    /// A PS1/theme claim (prompt_slot_claimed) always wins: the claim path
    /// rewrites `self.prompt`, so the flag check alone is not enough — an
    /// active flag with a live claim renders the claimed prompt untouched.
    ///
    /// On the segments face, the clone handed out here is stamped with
    /// this pass's Vcs value (niubash#592, S2): the value is read from
    /// the cross-pass segment cache in this call — which the REPL
    /// makes after `run_precmd_hooks` — and the pass's repaints
    /// re-read the stamp instead of touching the cache again.
    /// Since niubash#685 the read never computes and never waits: on a
    /// cold or stale entry this call launches a background refresh and
    /// stamps the cached value as-is, so a hook that changes the
    /// repository is reflected from the NEXT pass (the fingerprint
    /// detects the change here; the fresh value lands asynchronously).
    /// `self.prompt` itself is never stamped, so no stamped value can
    /// outlive its pass.
    pub(crate) fn prompt_for_display(&mut self) -> PromptBackend {
        #[cfg(windows)]
        if self.gitbash_prompt_active && !self.prompt_slot_claimed() {
            let left = crate::git_prompt::render(
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            );
            self.prompt = PromptBackend::Bash(BashPrompt::new(left, String::new()));
        }
        let prompt = self.prompt.clone();
        #[cfg(windows)]
        let mut prompt = prompt;
        #[cfg(windows)]
        if let PromptBackend::Segments(adapter) = &mut prompt {
            if adapter.inner.config_has_vcs() {
                self.vcs_pass_seq += 1;
                let seq = self.vcs_pass_seq;
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let value = crate::git_prompt::branch_segment(&cwd);
                adapter.inner.stamp_vcs(seq, value);
            }
        }
        prompt
    }

    fn sync_bash_prompt_from_env(&mut self) {
        if self.prompt_slot_claimed() {
            let ps1 = self
                .session_mut()
                .variables()
                .resolved_value("PS1")
                .unwrap_or_else(|| "\\$ ".to_string())
                .to_string();
            let ps2 = self
                .session_mut()
                .variables()
                .resolved_value("PS2")
                .unwrap_or_else(|| "> ".to_string())
                .to_string();
            let left = self.session_mut().expand_prompt(&ps1);
            let multiline = self.session_mut().expand_prompt(&ps2);
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
            .session_mut()
            .variables()
            .resolved_value("PS0")
            .filter(|value| !value.is_empty())
        else {
            return;
        };
        let last_exit_code = self.session().last_status();
        self.session_mut().variables().set("PS0", &ps0);
        let rendered = self.session_mut().expand_prompt(&ps0);
        // GNU writes the decoded PS0 to stderr (eval.c:164-176 fprintf),
        // never stdout: `2>/dev/null` drops the bytes, `2>&1` captures them,
        // and redirected stdout stays clean (unixwin/niubash#190).
        if !rendered.is_empty() {
            shell_channel_write(ShellChannel::Stderr, rendered.as_bytes());
        }
        self.session_mut().set_last_status(last_exit_code);
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
            .session_mut()
            .variables()
            .resolved_value("PS1")
            .filter(|value| !value.is_empty())
        else {
            return;
        };
        let rendered = self.session_mut().expand_prompt(&ps1);
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
        // niubash#402: clear before sourcing so only this file's own
        // top-level `exit` (recorded by `execute_script_inner`) is observed
        // by the caller below. The `. <path>` command is one simple command
        // with no history/alias/heredoc content of its own, so the grouped
        // readers are bypassed: they would flatten the `exit` into a plain
        // status before it can be seen.
        self.script_exit_jump = false;
        self.execute_script_inner(&format!(". {}", shell_quote(&shell_path)), false, 0, false)
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
        let last_exit_code = self.session().last_status();
        let hooks = std::rc::Rc::clone(&self.hooks.precmd);
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
        // niubash#685: hooks are commands too — a precmd script or
        // PROMPT_COMMAND can rewrite worktree files the git segment's
        // fingerprint cannot see, so a cycle that actually ran one
        // marks the cached segment dirty. The refresh stays in the
        // background; the pass below still never waits on git. A cycle
        // with no user hook content marks nothing, so idle passes in a
        // hook-less shell stay spawn-free.
        if !hooks.is_empty()
            || self
                .executor()
                .get_env("PROMPT_COMMAND")
                .is_some_and(|value| !value.is_empty())
        {
            self.invalidate_git_prompt_after_line();
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
        if let Some(title) = self.executor().get_env("NIU_TITLE") {
            if !title.is_empty() {
                return title.to_string();
            }
        }
        self.executor()
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
            .session_mut()
            .variables()
            .resolved_value("NIU_BINDKEYS")
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
        let emacs_on = self
            .session_mut()
            .variables()
            .resolved_value("__RUBASH_SETOPT_emacs")
            .as_deref()
            == Some("1");
        let vi_on = self
            .session_mut()
            .variables()
            .resolved_value("__RUBASH_SETOPT_vi")
            .as_deref()
            == Some("1");
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
        self.session_mut().options().set("emacs", pair.0);
        self.session_mut().options().set("vi", pair.1);
        self.edit_flags_seen = Some(pair);
    }

    /// Snapshot the engine's `bind` registry for the line editor
    /// (niubash#185). Called after the rc has been sourced and before the
    /// line editor is built, and again whenever the registry generation
    /// moved so a runtime `bind` call mirrors into the next prompt.
    pub fn load_engine_bindings(&mut self) {
        // The registry itself is Tier C: the snapshot arrives
        // through the gated bind surface, already converted to
        // niu-owned mirror types (PLAN row 11, M-3).
        let (entries, generation) = crate::bind_surface::load_engine_bindings(self.executor());
        self.engine_bindings = entries;
        self.engine_bind_generation = generation;
    }

    /// Whether the engine's bind registry has moved since the
    /// last snapshot (the REPL's between-prompts staleness
    /// probe, routed through the same gated surface).
    pub fn engine_bindings_stale(&self) -> bool {
        crate::bind_surface::current_generation(self.executor()) != self.engine_bind_generation
    }

    /// Load the user's persistent keymap rules from config.toml's
    /// `[keymap]` section (niubash#630, K4). Each `bind` element is a
    /// GNU bind argument string, parsed by the engine's own bind
    /// parsers (see `crate::repl::parse_keymap_bind_rule`); each
    /// `unbind` element names one key. A rule that does not parse is
    /// reported once per process — its exact text and the reason —
    /// and skipped: it never breaks startup and never applies
    /// silently. Called wherever the engine snapshot is (re)taken, so
    /// a rebuild always adjudicates script registrations against the
    /// current user rules.
    pub fn load_user_keymap_rules(&mut self) {
        let (bind_specs, unbind_specs) = crate::config::resolve_keymap_rule_specs();
        let mut rules = Vec::new();
        for spec in &bind_specs {
            match crate::repl::parse_keymap_bind_rule(spec) {
                Ok(entry) => rules.push(entry),
                Err(reason) => report_keymap_rule_error(&format!(
                    "niubash: [keymap].bind rule '{spec}' ignored: {reason}"
                )),
            }
        }
        let mut unbinds = Vec::new();
        for spec in &unbind_specs {
            match crate::repl::parse_keymap_unbind_key(spec) {
                Ok(key) => unbinds.push(key),
                Err(reason) => report_keymap_rule_error(&format!(
                    "niubash: [keymap].unbind entry '{spec}' ignored: {reason}"
                )),
            }
        }
        self.user_keymap_rules = rules;
        self.user_keymap_unbinds = unbinds;
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
        // One resolution list (niubash#630, K4): the engine snapshot
        // at its own indices, the `[keymap]` user rules right behind
        // it — the indices the editor build baked into the triggers.
        let entry = self.engine_bindings.get(index).or_else(|| {
            index
                .checked_sub(self.engine_bindings.len())
                .and_then(|offset| self.user_keymap_rules.get(offset))
        });
        let Some(entry) = entry else {
            return WidgetOutcome::default();
        };
        let crate::bind_surface::EngineBindKind::Execute { command } = &entry.kind else {
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
            .executor_mut()
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
        self.executor().has_function(name)
    }

    /// Load user completion declarations from the `NIU_COMPDEFS` variable
    /// declared in the startup rc. Each line is `command:function`, or —
    /// since issue #306 — a path to an existing directory holding
    /// completion-definition files: `<cmd>.toml` (the same schema as the
    /// embedded applet definitions) and `<cmd>.bash` (statically parsed, the
    /// file body is never executed). Directory lines are resolved first so a
    /// Windows drive-letter path (`D:/defs`) is never misread as a
    /// `command:function` pair; declared directories are loaded into the
    /// completion state together with the config-layer directories.
    pub fn load_user_compdefs(&mut self) {
        let value = self
            .session_mut()
            .variables()
            .resolved_value("NIU_COMPDEFS")
            .unwrap_or_default();
        let mut compdefs = Vec::new();
        let mut compdef_dirs: Vec<PathBuf> = Vec::new();
        for entry in value.lines().map(str::trim) {
            if entry.is_empty() || entry.starts_with('#') {
                continue;
            }
            let candidate = self.resolve_non_interactive_env_path(entry);
            if candidate.is_dir() {
                compdef_dirs.push(candidate);
                continue;
            }
            if let Some((command, function)) = entry.split_once(':') {
                let command = command.trim();
                let function = function.trim();
                if command.is_empty() || function.is_empty() {
                    continue;
                }
                compdefs.push((command.to_string(), function.to_string()));
            }
        }
        self.compdefs = compdefs;
        if !compdef_dirs.is_empty() {
            let mut dirs = self.config_completion_dirs.clone();
            dirs.extend(compdef_dirs);
            if let Ok(mut state) = self.completion_state.lock() {
                state.load_completion_dirs(&dirs);
            }
        }
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
        if let Err(err) = self.executor_mut().call_function_with_env(
            function,
            std::iter::empty::<String>(),
            [
                ("NIU_COMP_WORDS", joined.as_str()),
                ("NIU_COMP_CWORD", cword.to_string().as_str()),
            ],
        ) {
            log::warn!("completion function '{}' failed: {}", function, err);
        }

        let result = self
            .session_mut()
            .variables()
            .resolved_value("NIU_COMP_RESULT");
        self.session_mut().variables().unset("NIU_COMP_WORDS");
        self.session_mut().variables().unset("NIU_COMP_CWORD");
        self.session_mut().variables().unset("NIU_COMP_RESULT");

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
        if let Err(err) = self.executor_mut().call_function_with_env(
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
            .session_mut()
            .variables()
            .resolved_value("NIU_WIDGET_RESULT");
        let accept = self
            .session_mut()
            .variables()
            .resolved_value("NIU_WIDGET_ACCEPT");
        let cursor_result = self
            .session_mut()
            .variables()
            .resolved_value("NIU_WIDGET_CURSOR_RESULT")
            .and_then(|value| value.trim().parse::<usize>().ok());

        self.session_mut().variables().unset("NIU_WIDGET_RESULT");
        self.session_mut().variables().unset("NIU_WIDGET_ACCEPT");
        self.session_mut()
            .variables()
            .unset("NIU_WIDGET_CURSOR_RESULT");

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
        let hooks = std::rc::Rc::clone(&self.hooks.preexec);
        let context = [("NIU_PREEXEC_COMMAND", command.to_string())];
        self.run_hook_scripts(&hooks, &context);
        self.run_bash_ps0_preexec();
    }

    /// Run native hooks when the interactive command changed directories.
    pub fn run_chpwd_hooks_if_changed(&mut self, old_pwd: &str, new_pwd: &str) {
        if same_shell_dir(old_pwd, new_pwd) {
            return;
        }
        let hooks = std::rc::Rc::clone(&self.hooks.chpwd);
        let context = [
            ("NIU_OLDPWD", old_pwd.to_string()),
            ("NIU_PWD", new_pwd.to_string()),
        ];
        self.run_hook_scripts(&hooks, &context);
    }

    /// Run postcmd hooks after command execution.
    pub fn run_postcmd_hooks(&mut self, exit_code: i32) {
        let hooks = std::rc::Rc::clone(&self.hooks.postcmd);
        let exit_code_string = exit_code.to_string();
        let context = [("NIU_LAST_EXIT_CODE", exit_code_string)];
        self.run_hook_scripts(&hooks, &context);
    }

    /// Run zshaddhistory hooks after command is added to history.
    pub fn run_zshaddhistory_hooks(&mut self, command: &str) {
        let hooks = std::rc::Rc::clone(&self.hooks.zshaddhistory);
        let context = [("NIU_HISTORY_COMMAND", command.to_string())];
        self.run_hook_scripts(&hooks, &context);
    }

    /// Run zshexit hooks when shell exits.
    pub fn run_zshexit_hooks(&mut self) {
        let hooks = std::rc::Rc::clone(&self.hooks.zshexit);
        self.run_hook_scripts(&hooks, &[]);
    }

    /// Run greeting hooks at startup.
    pub fn run_greeting_hooks(&mut self) {
        let hooks = std::rc::Rc::clone(&self.hooks.greeting);
        self.run_hook_scripts(&hooks, &[]);
    }

    /// Run title hooks to set terminal title.
    pub fn run_title_hooks(&mut self, title: &str) {
        let hooks = std::rc::Rc::clone(&self.hooks.title);
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
        for line in native_command_not_found_hint_lines_with_policy(
            command,
            |candidate| resolve_native_command_path(candidate).is_some(),
            self.command_not_found_hint,
        ) {
            // Routed through the shell channel (not a bare eprintln!) so
            // the cfg(test) capture seam observes hint emission.
            shell_channel_write(ShellChannel::Stderr, format!("{line}\n").as_bytes());
        }
    }

    fn sync_alias_mirror_from_executor(&mut self) {
        self.aliases = self.session().aliases().into_iter().collect();
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
            self.session_mut().variables().set(name, value);
        }

        for hook in hooks {
            match self.execute_script(hook) {
                Ok(0) => {}
                Ok(code) => log::warn!("native hook exited with status {}", code),
                Err(err) => log::warn!("native hook failed: {}", err),
            }
        }

        if !context.is_empty() {
            for (name, _) in context {
                self.session_mut().variables().unset(name);
            }
        }
    }

    /// Order-independent fingerprint of the engine's live env map
    /// (niubash#594): the sum of per-entry hashes, so equal maps always
    /// yield equal fingerprints regardless of iteration order, and any
    /// name or value change moves the sum (short of a hash collision).
    /// Never returns 0, which `CompletionState` reserves for "no
    /// snapshot yet".
    fn completion_env_fingerprint(env: &std::collections::HashMap<String, String>) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut acc = (env.len() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for (name, value) in env {
            let mut hasher = DefaultHasher::new();
            name.hash(&mut hasher);
            value.hash(&mut hasher);
            acc = acc.wrapping_add(hasher.finish());
        }
        if acc == 0 {
            1
        } else {
            acc
        }
    }

    /// niubash#574: forget the highlighter's memoized live-probe answers
    /// after a submitted line that can move command resolution without
    /// moving the probe fingerprint — `hash` (every form mutates or clears
    /// the table) and `wpm` (install/remove/update change the filesystem
    /// while PATH stays byte-identical). Per-command exit status is not
    /// visible once a compound line runs, so the memo — only a cache — is
    /// cleared unconditionally on detection; a spurious clear just means
    /// the next paint re-probes.
    fn invalidate_command_probe_after_line(&self, text: &str) {
        if self.one_shot || !line_touches_command_resolution(&tokenize(text)) {
            return;
        }
        if let Ok(mut state) = self.completion_state.lock() {
            state.command_probe.clear();
        }
        // Audit item C1: the completion PATH caches share the probe's
        // blind spot — the filesystem changed while the env fingerprint
        // (PATH + PATHEXT) stayed byte-identical — so they retire on
        // the same lines.
        crate::completion::command::CommandCompleter::invalidate_command_caches();
    }

    /// niubash#685: a completed interactive command cycle can have
    /// rewritten worktree files without touching HEAD or the index —
    /// the two things the git prompt segment's fingerprint can see —
    /// so the cached segment for the (post-command) current repository
    /// is marked dirty. The next prompt pass serves the cached value
    /// and refreshes it in the background; no pass ever waits on git.
    /// Called from the same two post-line sites as the command-probe
    /// invalidation above (the interactive line and script funnels);
    /// an empty submission never reaches either site, so it costs
    /// nothing.
    fn invalidate_git_prompt_after_line(&self) {
        #[cfg(windows)]
        {
            if self.one_shot {
                return;
            }
            if let Ok(cwd) = std::env::current_dir() {
                crate::git_prompt::invalidate_segment_for_cwd(&cwd);
            }
        }
    }

    /// Update the shared completion state from the current env + cwd.
    pub fn update_completion_state(&self) {
        // One-shot shells have no REPL that reads completion state; the full
        // env clone + function snapshot would be pure startup overhead.
        if self.one_shot {
            return;
        }
        // niubash#594: this runs after every interactive line, and the
        // snapshot used to re-clone the whole env map, the alias name set
        // and the function name set unconditionally — a per-line tax that
        // grows with session state although the state is only read by the
        // completer (Tab) and the syntax highlighter. Gate each component
        // on exactly what it stores: env name/value pairs via a
        // fingerprint of the engine's live env map, function and alias
        // name sets via exact comparison of the sorted names. A component
        // whose stored content is unchanged keeps its existing
        // allocation; readers observe the same state as a full rebuild
        // would have produced.
        //
        // Bounded exception: `env_vars_snapshot` also renders the
        // engine's attribute lists (EXPORTED_VARS, READONLY_VARS, ...)
        // into the snapshot. A pure attribute toggle on an existing
        // variable (e.g. `readonly x`) does not touch the live env map,
        // so those rendered pseudo-entries refresh on the next real env
        // change instead of immediately. They are engine storage keys,
        // not user variables; the variable names and values completion
        // actually consumes are always current.
        let env_fingerprint = Self::completion_env_fingerprint(self.executor().env_vars());
        let function_names = self.session().functions();
        // niubash#627: the /dev/fd completion tier lists the session's
        // live fd numbers; sync them from the engine's fd table with
        // the same compare-and-store discipline as the other
        // components (the vec is tiny — fd counts, not env-sized).
        let open_fds = self.executor().open_fds();
        // niubash#627 G2: the /tmp backing directory, resolved by the
        // engine from the SESSION env (its TMPDIR wins, the process
        // temp dir is the engine's own fallback) through the Session
        // seam. Completion and highlighting root /tmp spellings in
        // this value, so a session `TMPDIR=/x` moves them on the very
        // next line — the same immediacy the engine's opens have —
        // instead of pinning both readers to the process env forever.
        let tmp_base = self.executor().resolve_shell_path("/tmp");
        let mut alias_names: Vec<String> = self.aliases.keys().cloned().collect();
        alias_names.sort_unstable();
        if let Ok(mut state) = self.completion_state.lock() {
            state.current_dir = self
                .executor_pwd_host_path()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| state.current_dir.clone());
            if state.snapshot_env_fingerprint != env_fingerprint {
                state.env_vars = self.executor().env_vars_snapshot();
                state.snapshot_env_fingerprint = env_fingerprint;
            }
            if state.snapshot_function_names != function_names {
                state.functions = function_names.iter().cloned().collect();
                state.snapshot_function_names = function_names;
            }
            if state.snapshot_alias_names != alias_names {
                state.aliases = alias_names.iter().cloned().collect();
                state.snapshot_alias_names = alias_names;
            }
            if state.open_fds != open_fds {
                state.open_fds = open_fds;
            }
            if state.tmp_base != tmp_base {
                state.tmp_base = tmp_base;
            }
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

    /// The directory this shell considers current: the engine `PWD` when it
    /// resolves to an existing host directory, else the process cwd.
    ///
    /// `execute_script` syncs the process cwd to the shell's final `PWD`
    /// before returning, and an EXIT trap's `cd` only updates `PWD`, so
    /// `PWD` is the truth for "where did this invocation end" — the value
    /// the `--cwd-state` host contract records.
    pub fn current_host_cwd(&self) -> PathBuf {
        // After `finish_with_engine_finish` consumed the session the
        // executor is gone; the funnel captured the final directory
        // (post-EXIT-trap) into `finish_cwd` for exactly this read.
        if self.session.is_none() {
            return self
                .finish_cwd
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        }
        self.executor_pwd_host_path()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    fn executor_pwd_host_path(&self) -> Option<PathBuf> {
        let pwd = self.executor().get_env("PWD")?;
        let host_path = self.executor().resolve_shell_path(pwd);
        host_path.is_dir().then_some(host_path)
    }

    /// The engine-canonical persona rendering of a host path, used everywhere
    /// the host (re)stamps the executor's `PWD`: rubash#556 (P0 #537) made the
    /// engine render `$PWD`/`pwd` in the POSIX persona form (`/c/...`)
    /// unconditionally, so a host-side stamp in native drive form would bend
    /// shell-visible PWD back into the retired style and make the form depend
    /// on which route (simple sequence vs compound line) executed last. The
    /// root mapping still applies first so isolated-root layouts keep their
    /// `/...` spelling; native drive spelling stays a spawn-boundary concern
    /// (process cwd, child argv, PATH translation).
    fn persona_pwd_from_host_path(&self, host_path: &str) -> String {
        rubash::executor::shell_pwd_display_path(&host_path_to_shell_path_with_root(
            host_path,
            self.shell_root.as_deref(),
        ))
    }

    fn sync_executor_pwd_from_process_cwd(&mut self) {
        let Ok(cwd) = std::env::current_dir() else {
            return;
        };
        let normalized_pwd = self.persona_pwd_from_host_path(&cwd.to_string_lossy());
        self.session_mut().variables().set("PWD", &normalized_pwd);
    }

    fn sync_process_path_from_executor_path(&mut self) {
        let Some(path) = self.session_mut().variables().resolved_value("PATH") else {
            return;
        };
        let env = self.executor_mut().env_vars_snapshot();
        let process_path = process_path_from_shell_path_list(&path, Some(&env));
        std::env::set_var("PATH", &process_path);
        if cfg!(windows) && process_path != path {
            self.session_mut().variables().set("PATH", &process_path);
        }
    }

    fn sync_process_cwd_from_executor_pwd(&mut self) {
        let pwd = match self.session_mut().variables().resolved_value("PWD") {
            Some(p) => p.to_string(),
            None => {
                let cwd = std::env::current_dir().unwrap_or_default();
                let pwd = self.persona_pwd_from_host_path(&cwd.to_string_lossy());
                self.session_mut().variables().set("PWD", &pwd);
                return;
            }
        };
        let host_pwd = self.executor_mut().resolve_shell_path(&pwd);
        let target = host_pwd.clone();
        if !target.is_dir() {
            let cwd = std::env::current_dir().unwrap_or_default();
            let pwd = self.persona_pwd_from_host_path(&cwd.to_string_lossy());
            self.session_mut().variables().set("PWD", &pwd);
            return;
        }
        if std::env::set_current_dir(&target).is_err() {
            let cwd = std::env::current_dir().unwrap_or_default();
            let pwd = self.persona_pwd_from_host_path(&cwd.to_string_lossy());
            self.session_mut().variables().set("PWD", &pwd);
            return;
        }
        let normalized_pwd = self.persona_pwd_from_host_path(&host_pwd.to_string_lossy());
        self.session_mut().variables().set("PWD", &normalized_pwd);
        if let Some(old_pwd) = self.session_mut().variables().resolved_value("OLDPWD") {
            let normalized_old_pwd = normalize_shell_visible_path(&old_pwd);
            if normalized_old_pwd != old_pwd {
                self.session_mut()
                    .variables()
                    .set("OLDPWD", &normalized_old_pwd);
            }
        }
    }

    /// Last exit code from rubash executor.
    pub fn last_exit_code(&self) -> i32 {
        self.session().last_status()
    }

    /// Run shell process teardown semantics that live in rubash's binary entry.
    pub fn finish_with_exit_trap(&mut self, status: i32) -> anyhow::Result<i32> {
        self.run_zshexit_hooks();
        let result = match self.executor_mut().run_exit_trap_with_status(status) {
            Ok(code) => Ok(code),
            Err(rubash::executor::ExecuteError::ExitCode(code)) => Ok(code),
            Err(e) => {
                if !is_broken_pipe_execute_error(&e) {
                    eprintln!("niubash: {}", e);
                }
                Ok(1)
            }
        };
        // niubash#652: after the EXIT trap (whose writes still belong to
        // the streams), close every still-open persistent output process
        // substitution so its deferred body runs with the accumulated
        // input — the `finish_shell` step rubash's own main always runs
        // (rubash#1336: `exec > >(tee log)` lost the whole log when the
        // shell simply ended). This funnel is shared by the REPL exit
        // (#649's end_repl_session), startup-file exits, and the engine
        // stdin route, so the close lives here; it is idempotent (closed
        // entries are skipped), so routes whose engine driver already
        // finished internally are unaffected.
        self.executor_mut()
            .close_all_persistent_output_process_substitutions();
        result
    }

    /// niubash#652: teardown for the one-shot and host-driven stream
    /// routes (script file, -c, the host stdin driver) via the engine's
    /// shared finish duties — the same set `script_driver::finish_shell`
    /// performs at rubash's own route tails: the session-history write
    /// (inert on these routes unless the script itself armed history,
    /// rubash#1049 parity), the end-of-stream job-notification flush
    /// (rubash#1853), the EXIT trap, the debugger exit note, and the
    /// persistent output procsub close (rubash#1336). These routes
    /// previously ran only the trap (finish_with_exit_trap), so the
    /// flush and the close never happened: a signal death reaped during
    /// the last command stayed silent, and `exec > >(cat > LOG)` lost
    /// the whole log. The composition below runs the trap through
    /// finish_with_exit_trap and the remaining duties through
    /// `Session::close`; the in-body comment explains why (the
    /// --cwd-state post-finish read) and records the one ordering
    /// delta. REPL exits do NOT come here: the REPL's history write
    /// is the ledger-reconciled provider flush in end_repl_session
    /// (#642/#649), which must stay after the trap.
    pub fn finish_with_engine_finish(&mut self, status: i32) -> i32 {
        // Composition (repaired after PR #668's cwd-state
        // regression): the EXIT trap runs FIRST, through the host's
        // retained trap funnel, and the shell's final directory is
        // captured immediately afterwards; only then is the session
        // consumed by `Session::close` for the remaining finish
        // duties (session-history write, stream-end job flush,
        // debugger exit note — the trap inside close's finish_shell
        // is already taken and the procsub close already done, so
        // both are no-ops there).
        //
        // Why the trap cannot stay inside close(): the binary's
        // `--cwd-state` recorder reads the final directory through
        // `current_host_cwd` AFTER this funnel returns, and the
        // documented contract (docs/cwd-state.md §3) is that the
        // record reflects the EXIT trap's `cd`, which updates
        // `PWD` but not the process cwd. A consuming close() leaves
        // no live session to read that `PWD` from: the first
        // version of this migration step closed first and read
        // afterwards, the accessor panicked ("shell session
        // already closed"), the process exited 1, and the state
        // file was never written (tests/cwd_state.rs 9/11 on
        // PR #668's CI). Capturing between the trap and the close
        // is the only point where the post-trap directory exists.
        //
        // Known ordering delta vs a single finish_shell pass: the
        // history write and the job-notification flush now run
        // (inside close) after the trap instead of before it. No
        // pin covers trap-vs-flush ordering (#652's pins assert
        // the notice is flushed and the procsub bodies are
        // delivered, both unchanged); the delta is confined to a
        // trap that itself mutates history state or races a job
        // notice. The status close() finishes with is the trap
        // funnel's resulting code, made the session's last status
        // first, exactly as the direct close did.
        let code = self.finish_with_exit_trap(status).unwrap_or(status);
        self.finish_cwd = Some(self.current_host_cwd());
        self.session_mut().set_last_status(code);
        let session = self.session.take().expect("shell session already closed");
        session.close()
    }

    /// niubash#576: whether the most recent script run through this
    /// shell unwound via a top-level jump — GNU jump_to_top_level
    /// (EXITPROG for `exit`, ERREXIT for an errexit abort) — rather
    /// than ending on an ordinary command status.
    /// `execute_script_inner` records the jump in `script_exit_jump`
    /// when it flattens the engine's `ExitCode` unwind into a plain
    /// status (niubash#402); batch-feeding drivers (the host's stdin
    /// reader) must observe it to stop feeding, exactly as GNU's
    /// reader_loop is unwound by the jump. Take-style: each check
    /// clears the flag, like `Executor::take_exit_jump_pending`.
    pub fn take_script_exit_jump(&mut self) -> bool {
        std::mem::take(&mut self.script_exit_jump)
    }

    /// Run the session-end history write on demand (GNU exit_shell ->
    /// write_history, bashhist.c).
    ///
    /// The standard exit paths do NOT need this: the one-shot funnel's rc-0
    /// exits return from main normally (#281), so the provider's Drop flush
    /// runs, and interactive sessions unwind the same way. This exists for
    /// embedding hosts that terminate through `std::process::exit` on a
    /// path the funnel does not own — a hard exit skips Drop, so the write
    /// must be made explicitly. Best-effort exactly like every
    /// history-file write: never fails, never reaches stderr.
    pub fn flush_session_history(&self) {
        let Some(handle) = self.history_provider.as_ref() else {
            return;
        };
        if let Ok(mut provider) = handle.try_borrow_mut() {
            provider.flush_at_session_end();
        }
    }

    /// The history ledger shared with the provider instance, if
    /// interactive entry installed one (niubash#569). The editor history
    /// instance adopts it so provider-side -c/-a/-w and the session-end
    /// write account for lines the editor records. `None` when no
    /// provider is installed (the editor then keeps its own ledger).
    pub(crate) fn history_ledger(&self) -> Option<Arc<Mutex<crate::history::SessionLedger>>> {
        self.history_provider
            .as_ref()
            .and_then(|provider| provider.try_borrow().ok())
            .map(|provider| provider.ledger())
    }

    /// The HIST* settings cell shared with the provider instance, if
    /// interactive entry installed one (niubash#646). The editor
    /// history instance adopts it, mirroring `history_ledger`.
    pub(crate) fn history_settings(&self) -> Option<Arc<Mutex<crate::history::HistorySettings>>> {
        self.history_provider
            .as_ref()
            .and_then(|provider| provider.try_borrow().ok())
            .map(|provider| provider.settings())
    }

    /// Refresh the shared HIST* settings cell from the executor's
    /// current state (niubash#646).
    ///
    /// GNU reads each history variable at the moment of the operation
    /// it governs — HISTCONTROL/HISTIGNORE when a line is recorded
    /// (bashhist.c check_history_control/history_should_ignore),
    /// HISTSIZE when the list is stifled, HISTFILESIZE and histappend
    /// at the session-end write (maybe_save_shell_history) — so the
    /// host re-snapshots the cell at every prompt boundary (both REPL
    /// loops), at interactive entry, and once more after the EXIT trap
    /// inside `end_repl_session`, and the history objects consult the
    /// snapshot. The size variables go through the engine's own
    /// `SessionHistory::size_limit` mapping (variables.c sv_histsize:
    /// unset/empty/negative/non-numeric means no limit); histappend is
    /// read from the engine's `__RUBASH_SHOPT_STATE` carrier, the same
    /// read `resync_parse_extglob_gate` performs for extglob.
    /// HISTTIMEFORMAT is not snapshotted: it has no consumer on this
    /// side of the provider contract (see `HistorySettings`).
    ///
    /// Two live-state faces ride along (niubash#660/#661): the
    /// `history` shell option is snapshotted into the same cell (the
    /// editor's record funnel drops lines while it is off), and a
    /// `HISTFILE` assignment made inside the session redirects the
    /// history stream to the new file (see `apply_history_redirect`).
    pub fn refresh_history_settings(&mut self) {
        let Some(settings) = self.history_settings() else {
            return;
        };
        let histcontrol = self
            .session_mut()
            .variables()
            .resolved_value("HISTCONTROL")
            .unwrap_or_default();
        let histignore = self
            .session_mut()
            .variables()
            .resolved_value("HISTIGNORE")
            .unwrap_or_default();
        let histsize = rubash::history::SessionHistory::size_limit(
            self.session_mut()
                .variables()
                .resolved_value("HISTSIZE")
                .as_deref(),
        );
        let histfilesize = rubash::history::SessionHistory::size_limit(
            self.session_mut()
                .variables()
                .resolved_value("HISTFILESIZE")
                .as_deref(),
        );
        let histappend = self
            .session_mut()
            .variables()
            .resolved_value("__RUBASH_SHOPT_STATE")
            .is_some_and(|state| state.split('\u{1f}').any(|entry| entry == "histappend"));
        // niubash#660: the `history` shell option, read from the
        // engine's own option store (the `__RUBASH_SETOPT_` carrier
        // `refresh_edit_mode` reads for emacs/vi). An absent key counts
        // as enabled: this cell exists only on the interactive path,
        // where GNU's default is on and `enter_interactive` writes the
        // key explicitly, so absence means a construction that never
        // passed through interactive entry — fail open to recording
        // rather than silently swallow a session's lines.
        let history_enabled = self
            .session_mut()
            .variables()
            .resolved_value("__RUBASH_SETOPT_history")
            .as_deref()
            != Some("0");
        if let Ok(mut settings) = settings.lock() {
            settings.histcontrol = histcontrol;
            settings.histignore = histignore;
            settings.histsize = histsize;
            settings.histfilesize = histfilesize;
            settings.histappend = histappend;
            settings.history_enabled = history_enabled;
        }
        self.apply_history_redirect();
    }

    /// niubash#661: follow a live `HISTFILE` redirect. GNU reads
    /// HISTFILE at the moment of each operation it governs, so an
    /// in-session `export HISTFILE=<new>` moves the session's history
    /// stream: the provider retargets its backing file (settling the
    /// old file first), `history_path` follows, and the REPL loop is
    /// signalled to rebuild the line editor, whose history instance
    /// still backs the old file.
    ///
    /// Deliberate non-redirects: `HISTFILE` unset or empty keeps the
    /// file of record (GNU's unset-erases-history-file face is not
    /// part of this change; the host's single-stream doctrine needs a
    /// backing file), and while the watch is unarmed (before
    /// `enter_interactive` completes) an inherited value never
    /// redirects — the configured path is the startup file of record
    /// (niubash#571).
    fn apply_history_redirect(&mut self) {
        if !self.history_redirect_armed {
            return;
        }
        let Some(live) = self.session_mut().variables().resolved_value("HISTFILE") else {
            return;
        };
        if live.is_empty() {
            return;
        }
        let live_path = PathBuf::from(&live);
        if live_path == self.history_path {
            return;
        }
        let Some(provider) = self.history_provider.as_ref() else {
            return;
        };
        if let Ok(mut provider) = provider.try_borrow_mut() {
            provider.retarget(live_path.clone());
        }
        // Adopt the redirect only when the provider actually moved:
        // its retarget is atomic on failure, and pointing
        // `history_path` at a file the provider does not back would
        // split the two instances onto different files.
        let adopted = provider
            .try_borrow()
            .map(|provider| provider.backing_file() == Some(live_path.clone()))
            .unwrap_or(false);
        if adopted {
            self.history_path = live_path;
            self.history_redirect_pending_editor_rebuild = true;
        }
    }

    /// niubash#661: consume the editor-rebuild signal a `HISTFILE`
    /// redirect left for the REPL loop (see `apply_history_redirect`).
    pub(crate) fn take_history_redirect(&mut self) -> bool {
        std::mem::take(&mut self.history_redirect_pending_editor_rebuild)
    }

    /// Execute an entire script (multi-line) via rubash full AST execution.
    ///
    /// Unlike `execute_line` which tokenizes/parses/executes each line
    /// independently, this method tokenizes the whole script at once.
    /// This enables heredocs, line continuations (backslash-newline),
    /// and multi-line compound commands (if/for/while across lines).
    pub fn execute_script(&mut self, script: &str) -> anyhow::Result<i32> {
        self.execute_script_with_options(script, false, 0)
    }

    /// Execute a script whose physical lines continue an input stream that
    /// started `line_offset` lines earlier. The host stdin-script reader
    /// (main.rs run_stdin_script) gathers complete batches and executes them
    /// one at a time; GNU's reader_loop keeps ONE physical line counter
    /// across the whole stream, so a diagnostic in the third gathered batch
    /// still reports `line 3`, never a per-batch `line 1`
    /// (unixwin/niubash#431).
    pub fn execute_script_at_line_offset(
        &mut self,
        script: &str,
        line_offset: usize,
    ) -> anyhow::Result<i32> {
        self.execute_script_with_options(script, false, line_offset)
    }

    fn execute_script_with_options(
        &mut self,
        script: &str,
        interactive_terminal_colors: bool,
        line_offset: usize,
    ) -> anyhow::Result<i32> {
        self.execute_script_inner(script, interactive_terminal_colors, line_offset, true)
    }

    /// The body of [`execute_script_with_options`]. `allow_grouped_readers`
    /// is false only for the startup-file `. <path>` source (niubash#402):
    /// the engine's grouped readers flatten a top-level `exit` into a plain
    /// status before the caller can observe it, so the source path forces the
    /// tokenize+parse+execute_ast route, whose `ExitCode` unwind is recorded
    /// in `script_exit_jump`.
    fn execute_script_inner(
        &mut self,
        script: &str,
        interactive_terminal_colors: bool,
        line_offset: usize,
        allow_grouped_readers: bool,
    ) -> anyhow::Result<i32> {
        let script = if interactive_terminal_colors {
            script
        } else {
            script.trim()
        };
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
        let parse_posix = self
            .session_mut()
            .variables()
            .resolved_value("__RUBASH_POSIX_MODE")
            .as_deref()
            == Some("1");
        if allow_grouped_readers
            && !self.interactive
            && (rubash::script_driver::heredoc_delimiter_unclosed_quote(&script).is_some()
                || (rubash::lexer::has_unclosed_input_syntax_posix(&script, parse_posix)
                    && !script.contains("<<")))
        {
            // niubash#653: pass the batch's real line_offset through —
            // this arm used to hardcode 0, so a host stdin batch whose
            // unclosed quote sat on physical line 3 reported line 1
            // (the tracked history arm below passes it correctly).
            //
            // 1.5.0 migration note: this arm stays on the direct
            // driver call instead of following the tracked arm into
            // Session::run. The contract's line-offset route consumes
            // the executor's parse-error pending into its Flow, and
            // Flow has no parse-error variant in this publication
            // step (run() diagnostics are the contract's recorded
            // out-of-scope remainder) — the host batch feeder's stop
            // rule reads that pending itself, so routing this arm
            // through run() would silently keep feeding a stream
            // after a mid-stream syntax error. Revisit when the
            // contract surfaces run diagnostics.
            let code = rubash::script_driver::run_source_with_line_offset(
                self.executor_mut(),
                &script,
                false,
                line_offset,
                None,
                None,
            );
            self.sync_process_cwd_from_executor_pwd();
            self.sync_process_path_from_executor_path();
            self.sync_alias_mirror_from_executor();
            return Ok(code);
        }
        if allow_grouped_readers
            && !self.interactive
            && (rubash::script_driver::script_uses_history(&script)
                || rubash::script_driver::script_uses_aliases(
                    &script,
                    // rubash#414: parse-time alias state established before
                    // the reader starts (CLI -O expand_aliases / posix mode)
                    // must route through the line-group driver; the executor's
                    // own live flag is the truth source (same argument rubash
                    // main.rs passes at its script site).
                    self.executor_mut().alias_expansion_enabled(),
                ))
        {
            // niubash#576: take the TRACKED grouped run and record an
            // unwind in `script_exit_jump`, like the ExitCode arm below
            // does for the tokenize+execute route. The untracked run
            // drops the classification (the grouped driver takes the
            // executor's exit-jump flag itself), so a batch feeder saw
            // only a plain nonzero status for e.g. an alias that expands
            // to `exit 3` and kept feeding the stream — GNU's
            // jump_to_top_level unwinds reader_loop for good. The
            // tracked entry is the engine's host-facing one (the REPL
            // reader route uses it for the same reason); it also keeps
            // a host-owned history provider authoritative instead of
            // installing a scratch session over it (niubash#508).
            //
            // 1.5.0 migration: this arm runs through the Session
            // contract's run(), whose tracked route is this same
            // driver. The contract's Flow separates a real exit
            // (ExitRequested) from the errexit break the driver's
            // stop bool conflates; the batch feeder's stop rule
            // already handles the errexit break through its own
            // predicate, so recording the jump on ExitRequested
            // alone preserves the stop behavior. Admission above
            // stays the host's (#414 live-flag form); the
            // contract's internal alias liveness additionally
            // requires a non-empty alias table, so an alias-shaped
            // script with an empty table takes the contract's
            // line-offset route inside run() — with no aliases to
            // expand and no history use admitted, the routes agree
            // on the outcome.
            let mut options = RunOptions::default();
            options.line_offset = line_offset;
            let outcome = self
                .session_mut()
                .run(&script, &options)
                .map_err(|error| anyhow::anyhow!("session run failed: {error:?}"))?;
            if let Flow::ExitRequested(status) = outcome.flow {
                self.script_exit_jump = true;
                self.note_session_exit_jump(status);
            }
            self.sync_process_cwd_from_executor_pwd();
            self.sync_process_path_from_executor_path();
            self.sync_alias_mirror_from_executor();
            return Ok(outcome.status);
        }
        let mut tokens = tokenize(&script);
        // niubash#513: tokenize's shopt simulation leaves the parse-time
        // extglob gate at the script's final state; restore the entry
        // state before parsing (and before the parse-error fallback
        // driver re-parses group by group). See resync_parse_extglob_gate.
        self.resync_parse_extglob_gate();
        if tokens.is_empty() {
            return Ok(0);
        }
        // rubash#365: the interactive leg (REPL multi-line paste, and any
        // interactive multi-command batch) needs GNU's read-time alias
        // semantics for the same reasons as the line route above; the
        // non-interactive legs above already use this driver for
        // alias/history-bearing scripts.
        if allow_grouped_readers {
            if let Some(code) = self.execute_interactive_reader_batch(&script, &tokens, line_offset)
            {
                return Ok(code);
            }
        }
        for token in &mut tokens {
            token.position += line_offset;
            token.column += line_offset;
            token.logical_line += line_offset;
        }
        rewrite_winuxcmd_command_shims(&mut tokens, interactive_terminal_colors);

        // unixwin/niubash#431: the engine's readers parse with the stray-close
        // guard armed (script_driver run_source_impl arms
        // ParseLoopOptions::stray_close_is_error — GNU parse.y rejects a `)'
        // or case terminator reaching command position with `syntax error near
        // unexpected token'); the default parse() keeps the pre-guard
        // leniency, silently dropping the stray token and running the rest of
        // the input. That leniency is how every host route that lands here
        // (`-c`, `--encoded-command`, script file, piped/redirected stdin —
        // anything but the engine's own stdin driver behind `-s`) executed
        // `niu -c '(printf a; printf b);)'` with rc 0 where GNU bash 5.3.0
        // exits 2 without running anything (WSL oracle, all four forms). Arm
        // the same guard for the non-interactive fast path, threading the
        // source text so the offending line echoes verbatim (GNU parse.y
        // y.error; token reconstruction cannot recover the original spacing —
        // the default path printed `` `echo a ;;' `` for input `echo a;;`).
        // Interactive keeps the default parse: its reader-batch route above
        // already hands semicolon/paren-bearing input to the engine, and its
        // diagnostics take the interactive shape. When the armed parse trips,
        // skip the host execution entirely and let the engine's group-wise
        // driver run the input — that reproduces GNU's read-parse-execute
        // split byte-for-byte (a `;`+newline-terminated list before the stray
        // token still runs, then the diagnostic aborts with status 2), which
        // a whole-string parse-then-execute cannot. Same admission shape as
        // the unclosed-quote fall-through above: a false positive merely runs
        // the engine's normal driver.
        let mut ast = if self.interactive {
            parse(&tokens)
        } else {
            let armed = rubash::parser::parse_with_options(
                &tokens,
                rubash::parser::ParseLoopOptions {
                    stray_close_is_error: true,
                    source_text: Some(script.clone().into()),
                    diagnostic_text: None,
                    source_line_offset: line_offset,
                },
            );
            if allow_grouped_readers
                && armed
                    .commands
                    .iter()
                    .any(Self::command_has_parse_error_marker)
            {
                let code = rubash::script_driver::run_source_with_line_offset(
                    self.executor_mut(),
                    &script,
                    false,
                    line_offset,
                    None,
                    None,
                );
                self.sync_process_cwd_from_executor_pwd();
                self.sync_process_path_from_executor_path();
                self.sync_alias_mirror_from_executor();
                return Ok(code);
            }
            armed
        };
        normalize_bare_windows_drive_commands(&mut ast);
        normalize_cd_windows_drive_args(&mut ast);
        normalize_winuxcmd_slash_drive_args(&mut ast);
        self.inject_process_stdin_for_rewritten_pipeline(&mut ast)?;

        let execution = if let Some(exit) = self.easter_egg_exit(&ast.commands) {
            Ok(exit)
        } else {
            self.execute_host_synced_simple_ast(&ast)
                .unwrap_or_else(|| match self.executor_mut().execute_ast(&ast) {
                    Ok(()) => Ok(self.session().last_status()),
                    Err(err) => Err(err),
                })
        };

        let code = match execution {
            Ok(code) => code,
            Err(rubash::executor::ExecuteError::ExitCode(code)) => {
                if self.executor_mut().take_signal_discard_pending() {
                    // niubash#610: a signal DISCARD (rubash#1662 — an
                    // untrapped SIGINT delivered to an interactive
                    // shell, e.g. Ctrl-C interrupting a pipeline inside
                    // a multi-line block) is not an exit for either jump
                    // ledger: the block ends with `$? = code` and the
                    // caller carries on. Consume the parse flag exactly
                    // as note_session_exit_jump would, so no stale flag
                    // leaks into the next submission.
                    let _ = self.executor_mut().take_parse_error();
                } else {
                    // niubash#402: the script unwound with a top-level `exit`
                    // (GNU jump_to_top_level EXITPROG; the engine uses the same
                    // `ExitCode` classification for the errexit ERREXIT unwind —
                    // GNU terminates for both), so record it. The startup-file
                    // sourcer ends the invocation with this status instead of
                    // reading it as an ordinary command result.
                    self.script_exit_jump = true;
                    // niubash R1: an interactive multi-line submission that
                    // unwound the same way must end the REPL session too —
                    // `execute_interactive_script` feeds straight through here.
                    self.note_session_exit_jump(code);
                }
                code
            }
            Err(rubash::executor::ExecuteError::Return(code)) => code,
            Err(rubash::executor::ExecuteError::ExpansionFailure(code)) => code,
            Err(rubash::executor::ExecuteError::FatalFunctionError(code)) => {
                self.note_session_exit_jump(code);
                code
            }
            Err(rubash::executor::ExecuteError::SigintUnwind) => {
                // rubash#1639 DISCARD verdict, same split as the line
                // route (niubash#610): no diagnostic, no jump ledgers.
                self.session_mut().set_last_status(130);
                130
            }
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

    /// Whether a parse-loop command node carries one of the parser's error
    /// markers (`__RUBASH_PARSE_ERROR*`). Top-level scan only: the stray-close
    /// guard and every hoisting path in rubash's parse loop push their error
    /// node at the list's top level, and that top-level delta is exactly what
    /// arming the guard adds over the default parse (compound bodies keep
    /// their own leniency — rubash's run_inline_section saves/restores the
    /// flag). Mirrors the marker set script_driver's
    /// command_tree_has_parse_error reads (rubash-internal there).
    fn command_has_parse_error_marker(command: &rubash::parser::CommandNode) -> bool {
        command
            .assignments
            .iter()
            .any(|(name, _)| name.starts_with("__RUBASH_PARSE_ERROR"))
    }

    fn inject_process_stdin_for_rewritten_pipeline(&mut self, ast: &mut Ast) -> anyhow::Result<()> {
        if !self.process_stdin_pipeline_bridge
            || self
                .session_mut()
                .variables()
                .resolved_value("__RUBASH_INHERIT_PROCESS_STDIN")
                .as_deref()
                != Some("1")
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
            match self.executor_mut().execute_command(command) {
                Ok(()) => {
                    self.sync_process_cwd_from_executor_pwd();
                    self.sync_process_path_from_executor_path();
                }
                // Propagate the unwind instead of flattening it to a status:
                // the callers' `ExitCode` arms record the jump (niubash#402
                // for the script path, niubash R1 for the interactive line),
                // which `cd /tmp; exit 3`-shaped host-synced sequences need
                // exactly like every other list.
                Err(rubash::executor::ExecuteError::ExitCode(code)) => {
                    return Some(Err(rubash::executor::ExecuteError::ExitCode(code)))
                }
                Err(rubash::executor::ExecuteError::Return(code)) => return Some(Ok(code)),
                Err(err) => return Some(Err(err)),
            }
        }

        Some(Ok(self.session().last_status()))
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
    // The `word_quotes` half of this admission is gone: the
    // engine field was always empty (any word that would record
    // a quote is stored in its quoted form instead — the field's
    // own doc named this reader as its last consumer), so the
    // disjunct never fired. Dropping the read retires the field
    // engine-side (companion deletion in rubash).
    if command
        .word_metadata
        .first()
        .is_some_and(|metadata| metadata.raw.as_str() != word)
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

/// True when a tokenized line runs `hash` or `wpm` at any command position
/// (niubash#574's probe-memo invalidation trigger).
fn line_touches_command_resolution(tokens: &[Token]) -> bool {
    let mut start = 0;
    while start < tokens.len() {
        let end = find_command_separator(tokens, start).unwrap_or(tokens.len());
        let mut stage_start = start;
        while stage_start < end {
            let stage_end = find_pipeline_separator(tokens, stage_start, end).unwrap_or(end);
            if let Some(index) = simple_command_word_index(tokens, stage_start, stage_end) {
                let name = decode_to_visible_text(&tokens[index].value);
                if command_resolution_mutator(&name) {
                    return true;
                }
            }
            stage_start = stage_end + 1;
        }
        if end == tokens.len() {
            break;
        }
        start = end + 1;
    }
    false
}

fn command_resolution_mutator(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let stem = name
        .strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".cmd"))
        .or_else(|| name.strip_suffix(".bat"))
        .unwrap_or(&name);
    matches!(stem, "hash" | "wpm")
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
        // basename/dirname are filesystem-blind string transforms: GNU
        // dirname.c/basename.c never stat their operand, so a POSIX-shaped
        // NAME is display text, not a slash-drive path (niubash#346:
        // `dirname /a/b` must print `/a`, not the drive-form effect `A:`).
        // Mask every positional, mirroring rubash's argv_data_primary_positions.
        "basename" | "dirname" => mask_all_positionals(words, &mut mask),
        _ => {}
    }
    mask
}

/// Masks every non-switch word: for filesystem-blind string tools
/// (basename/dirname) each positional — NAME and SUFFIX alike — is data.
fn mask_all_positionals(words: &[String], mask: &mut [bool]) {
    for (index, arg) in words.iter().enumerate() {
        if index == 0 {
            continue;
        }
        if !(arg.len() > 1 && arg.starts_with('-')) {
            if let Some(slot) = mask.get_mut(index) {
                *slot = false;
            }
        }
    }
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
/// Test-facing default-policy shape of the hint builder: production always
/// resolves the policy from `Shell::command_not_found_hint`.
#[cfg(test)]
fn native_command_not_found_hint_lines<F>(command: &str, command_exists: F) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    native_command_not_found_hint_lines_with_policy(
        command,
        command_exists,
        crate::config::CommandNotFoundHint::default(),
    )
}

/// niubash#249: the unsolicited "package search hints" block is gone. The
/// recipe/wpm install hints stay (they fire only for exact, known tool
/// names — targeted advice, not a typo-triggered advertisement). The
/// search suggestion itself is opt-in (`niu config set
/// command-not-found-hint wpm`) and names only niu's own wpm — never
/// winget/scoop/choco.
#[cfg_attr(not(windows), allow(unused_mut))]
fn native_command_not_found_hint_lines_with_policy<F>(
    command: &str,
    mut command_exists: F,
    hint: crate::config::CommandNotFoundHint,
) -> Vec<String>
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

    // niubash#249: the package search suggestion is opt-in, wpm-only, and
    // fires only for high-confidence near-misses (see
    // `is_high_confidence_package_near_miss`). Off by default — GNU bash
    // parity for the common typo.
    #[cfg(windows)]
    if hint == crate::config::CommandNotFoundHint::Wpm
        && is_high_confidence_package_near_miss(command, &mut command_exists)
    {
        lines.push("niubash: package search hints:".to_string());
        lines.push(format!("  wpm search --name {}", search));
    }
    #[cfg(not(windows))]
    // wpm does not exist on this platform, so the opt-in channel has
    // nothing to name: the policy parameter and the existence probe stay
    // intentionally unused here.
    let _ = (command_exists, hint, search);

    lines
}

/// niubash#249 trigger tightening (opt-in channel only): the typed word is
/// a high-confidence package near-miss only when dropping its final
/// character — edit distance ≤ 1 by truncation, the typo shape the issue
/// names — hits an installed command or a known package command name.
/// Arbitrary one-off typos ("win") stay hint-less even with the channel
/// enabled.
#[cfg(windows)]
fn is_high_confidence_package_near_miss<F>(command: &str, command_exists: &mut F) -> bool
where
    F: FnMut(&str) -> bool,
{
    let Some((last_char_index, _)) = command.char_indices().last() else {
        return false;
    };
    let prefix = &command[..last_char_index];
    if prefix.is_empty() {
        return false;
    }
    command_exists(prefix)
        || wpm_package_for_command(prefix).is_some()
        || plugin_recipe_for_command(prefix).is_some()
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

/// The login name of the user running this process, for `~user`
/// env-file expansion (niubash#589): USER on Unix-likes, USERNAME on
/// Windows. `None` when neither is set — a named tilde then falls back
/// to the passwd lookup and, failing that, stays literal.
fn current_login_user() -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("USERNAME")
                .ok()
                .filter(|value| !value.is_empty())
        })
}

/// Home directory of a named user from the passwd database, for
/// `~user` env-file expansion (niubash#589). Platforms without a
/// passwd database (Windows) resolve only the current user, which the
/// caller handles before consulting this.
#[cfg(unix)]
fn passwd_home_dir(user: &str) -> Option<PathBuf> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let mut fields = line.split(':');
        if fields.next() == Some(user) {
            let home = fields.nth(4)?;
            if !home.is_empty() {
                return Some(PathBuf::from(home));
            }
        }
    }
    None
}

#[cfg(not(unix))]
fn passwd_home_dir(_user: &str) -> Option<PathBuf> {
    None
}

/// A plain shell variable name (`[A-Za-z_][A-Za-z0-9_]*`) — the only
/// braced form env-file expansion substitutes (niubash#589).
fn is_plain_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Single-pass `$VAR` / `${VAR}` substitution for env-file values
/// (niubash#589): `$` followed by anything that is not a plain name or
/// a plain braced name is copied through literally, so a value can
/// never be half-expanded into a different path than the user wrote.
fn expand_env_file_params(value: &str, lookup: impl Fn(&str) -> String) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' || i + 1 >= chars.len() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        if chars[i + 1] == '{' {
            if let Some(close) = (i + 2..chars.len()).find(|&j| chars[j] == '}') {
                let name: String = chars[i + 2..close].iter().collect();
                if is_plain_env_var_name(&name) {
                    out.push_str(&lookup(&name));
                    i = close + 1;
                    continue;
                }
            }
            out.push('$');
            i += 1;
            continue;
        }
        let first = chars[i + 1];
        if first.is_ascii_alphabetic() || first == '_' {
            let mut end = i + 1;
            while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            let name: String = chars[i + 1..end].iter().collect();
            out.push_str(&lookup(&name));
            i = end;
            continue;
        }
        out.push('$');
        i += 1;
    }
    out
}

fn normalize_executor_home_env(executor: &mut Executor, home_dir: &Path) {
    let home = host_path_to_shell_path(&home_dir.to_string_lossy());
    let current = executor.get_env("HOME").unwrap_or_default();
    // GNU shell.c/variables.c: HOME comes from the caller's environment.
    // Normalize only when it is missing or Windows-shaped (backslashes,
    // slash-drive); an explicitly exported POSIX HOME is the caller's
    // choice and is honored verbatim (invocation.tests HOME=$TDIR).
    // niubash#546: a missing/empty HOME must not be fabricated from the
    // resolved home directory on a POSIX host — GNU leaves HOME unset
    // when the caller never provided one (`env -i niu` showed
    // HOME=/root; `cd` then silently landed there instead of failing
    // with "HOME not set"). Filling stays for Windows hosts (their
    // tools key off HOME) and when the caller did provide a home via
    // HOME/USERPROFILE in some form this normalization only reshapes.
    let caller_provided_home = ["HOME", "USERPROFILE"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    let should_update = (current.trim().is_empty() && (cfg!(windows) || caller_provided_home))
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
    // niubash#667: COLUMNS is a plain shell variable in GNU bash — the
    // shell maintains it for its own line discipline and prompt math,
    // and it reaches a child's environment only when the user exports
    // it. The width seed must therefore go through the non-exporting
    // write. (The shared helper below exports on purpose: its other
    // callers seed the Windows profile variables, which ARE environment
    // citizens.) Same-family check: LINES is never seeded by the host —
    // the engine's own winsize face owns it — so COLUMNS is the only
    // terminal-size variable on this path.
    if executor
        .get_env("COLUMNS")
        .is_none_or(|current| current.trim().is_empty())
    {
        executor.set_env("COLUMNS", "80");
    }
}

fn set_executor_env_if_missing_or_empty(executor: &mut Executor, name: &str, value: &str) {
    if executor
        .get_env(name)
        .is_none_or(|current| current.trim().is_empty())
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
    /// Reserved stdout-side half of the GNU stream-shape contract; every
    /// live shell emitter is stderr-only today (PS0, eval.c:164-176), so
    /// no constructor exists yet. The channel-capture tests assert that
    /// stderr-only shape against this two-variant enum.
    #[allow(dead_code)]
    Stdout,
    Stderr,
}

/// Payload recorded by the [`CHANNEL_CAPTURE`] test seam: `Some` while armed,
/// each entry a `(channel, bytes)` pair.
#[cfg(test)]
type ChannelCaptureBuffer = Option<Vec<(ShellChannel, Vec<u8>)>>;

#[cfg(test)]
thread_local! {
    /// Test seam for [`shell_channel_write`]: when armed with `Some`, channel
    /// writes are recorded here as (channel, payload) instead of reaching the
    /// real process handles, letting tests assert the stream separation.
    static CHANNEL_CAPTURE: std::cell::RefCell<ChannelCaptureBuffer> =
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
    fn host_history_provider_is_installed_only_on_interactive_entry() {
        use rubash::history::HistoryProvider;

        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "interactive-entry\n").unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = path.clone();
        assert!(shell.history_provider.is_none());

        shell.enter_interactive();
        let provider = shell.history_provider.as_ref().unwrap().clone();
        assert_eq!(
            provider.borrow_mut().entries().unwrap(),
            ["interactive-entry"]
        );
        provider.borrow_mut().clear().unwrap();
        shell.enter_interactive();
        assert!(Rc::ptr_eq(
            &provider,
            shell.history_provider.as_ref().unwrap()
        ));
        assert!(provider.borrow_mut().entries().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "interactive-entry\n"
        );
    }

    #[test]
    fn interactive_histfile_defaults_to_the_host_history_backing_file() {
        // niubash#571: a GNU interactive shell always has a HISTFILE
        // default, and the bare `history -a/-w/-r/-n` forms resolve their
        // filename from it. The REPL used to unset HISTFILE, so every bare
        // form failed with `filename not specified` and bash-it-style
        // per-prompt hooks broke at the first leg.
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = path.clone();

        shell.enter_interactive();
        assert_eq!(
            shell
                .executor_mut()
                .get_env("HISTFILE")
                .map(|v| v.to_string()),
            Some(path.to_string_lossy().into_owned()),
            "HISTFILE must name the provider's backing file, not be unset"
        );
    }

    #[test]
    fn issue660_history_option_gates_editor_recording_live() {
        // niubash#660: GNU `set +o history` stops recording until
        // `set -o history` (bashhist.c remember_on_history; verified
        // against bash 5.3.20 — including that the `set +o history`
        // line itself is still recorded and the `set -o history` line
        // is not, because the line is saved before it executes and
        // the snapshot turns at the following prompt boundary; the
        // sequence below mirrors that order).
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = path.clone();
        shell.enter_interactive();

        // The editor twin over the shell's shared cells, gated the
        // way build_line_editor gates the instance it hands reedline.
        use reedline::History as _;
        let mut editor = crate::history::LiveFileBackedHistory::with_shared_ledger_and_settings(
            shell.history_max_size,
            shell.history_path.clone(),
            shell.history_ledger().unwrap(),
            shell.history_settings().unwrap(),
        );
        editor.set_history_option_gate();

        editor
            .save(reedline::HistoryItem::from_command_line("before-off"))
            .unwrap();
        shell.execute_line("set +o history").unwrap();
        shell.refresh_history_settings();
        editor
            .save(reedline::HistoryItem::from_command_line("secret-while-off"))
            .unwrap();
        shell.execute_line("set -o history").unwrap();
        shell.refresh_history_settings();
        editor
            .save(reedline::HistoryItem::from_command_line("after-on"))
            .unwrap();

        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("before-off"), "{persisted:?}");
        assert!(
            !persisted.contains("secret-while-off"),
            "line recorded despite `set +o history`: {persisted:?}"
        );
        assert!(persisted.contains("after-on"), "{persisted:?}");
    }

    #[test]
    fn issue661_in_session_histfile_assignment_redirects_the_stream() {
        // niubash#661: GNU reads HISTFILE at the moment of each
        // operation it governs; an in-session `export HISTFILE=<new>`
        // moves later saves and the session-end write to the new
        // file, and the old file gains nothing after the switch.
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("histA");
        let b = temp.path().join("histB");
        std::fs::write(&a, "seedA\n").unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = a.clone();
        shell.enter_interactive();
        assert!(!shell.take_history_redirect());

        let b_shell = b.to_string_lossy().replace('\\', "/");
        shell
            .execute_line(&format!("export HISTFILE='{b_shell}'"))
            .unwrap();
        shell.refresh_history_settings();
        assert!(
            shell.take_history_redirect(),
            "the redirect must signal the editor rebuild"
        );
        assert!(
            !shell.take_history_redirect(),
            "the signal is consumed once"
        );
        assert_eq!(shell.history_path, b);
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "seedA\n",
            "the new file holds the session list at switch time"
        );
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "seedA\n");

        record_accepted_line(&shell, "after-redirect-marker");
        assert!(
            std::fs::read_to_string(&b)
                .unwrap()
                .contains("after-redirect-marker"),
            "later saves must land in the new file"
        );
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "seedA\n");

        shell.flush_session_history();
        assert!(std::fs::read_to_string(&b)
            .unwrap()
            .contains("after-redirect-marker"));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "seedA\n");
    }

    /// Simulate what reedline does with an accepted line in production:
    /// record it into the shared history (the provider's list) BEFORE the
    /// line executes. rubash#1248's expansion resolves against this list.
    fn record_accepted_line(shell: &Shell, line: &str) {
        use rubash::history::HistoryProvider;
        let provider = shell.history_provider.as_ref().unwrap().clone();
        provider.borrow_mut().append(line.to_string()).unwrap();
    }

    #[test]
    fn interactive_bang_bang_reexecutes_the_previous_line() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = temp.path().join("history");
        shell.enter_interactive();

        // A non-idempotent first line makes re-execution observable.
        assert_eq!(
            shell
                .execute_interactive_line("T1248=${T1248}alpha")
                .unwrap(),
            0
        );
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("T1248")
                .as_deref(),
            Some("alpha")
        );
        record_accepted_line(&shell, "T1248=${T1248}alpha");

        record_accepted_line(&shell, "!!");
        assert_eq!(shell.execute_interactive_line("!!").unwrap(), 0);
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("T1248")
                .as_deref(),
            Some("alphaalpha")
        );

        // The recorded raw `!!` was rewritten to the expansion, GNU-style.
        use rubash::history::HistoryProvider;
        let provider = shell.history_provider.as_ref().unwrap().clone();
        assert_eq!(
            provider.borrow_mut().entries().unwrap(),
            ["T1248=${T1248}alpha", "T1248=${T1248}alpha"]
        );

        // Quick substitution (^a^b) re-runs the previous line edited.
        record_accepted_line(&shell, "^alpha^beta");
        assert_eq!(shell.execute_interactive_line("^alpha^beta").unwrap(), 0);
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("T1248")
                .as_deref(),
            Some("alphaalphabeta")
        );
    }

    #[test]
    fn interactive_failed_expansion_keeps_the_previous_exit_code() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = temp.path().join("history");
        shell.enter_interactive();

        // GNU: a failed history expansion executes nothing and leaves $?
        // exactly as it was (verified against bash 5.3.20).
        assert_eq!(shell.execute_interactive_line("true").unwrap(), 0);
        assert_eq!(
            shell
                .execute_interactive_line("!no_such_event_1248")
                .unwrap(),
            0
        );
        assert_eq!(shell.execute_interactive_line("false").unwrap(), 1);
        assert_eq!(
            shell
                .execute_interactive_line("!no_such_event_1248")
                .unwrap(),
            1
        );
    }

    #[test]
    fn interactive_set_plus_h_turns_expansion_off() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = temp.path().join("history");
        shell.enter_interactive();

        assert_eq!(shell.execute_interactive_line("T1248=alpha").unwrap(), 0);
        record_accepted_line(&shell, "T1248=alpha");
        assert_eq!(shell.execute_interactive_line("set +H").unwrap(), 0);
        record_accepted_line(&shell, "!!");
        // With histexpand off, `!!` reaches the tokenizer literally and is
        // just an unknown command - the user's later `set +H` must win over
        // the default enter_interactive asserted.
        assert_eq!(shell.execute_interactive_line("!!").unwrap(), 127);
    }

    #[test]
    fn noninteractive_lines_never_expand_history() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut shell = test_shell(HookConfig::default());
        // No enter_interactive: script / -c lines keep the literal text.
        assert_eq!(shell.execute_line("!!").unwrap(), 127);
    }

    #[test]
    fn refresh_edit_mode_rc_vi_switches_at_startup() {
        // The standard bashrc line: `set -o vi` before the first prompt.
        let mut shell = test_shell(HookConfig::default());
        shell.session_mut().options().set("vi", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Vi));
        assert_eq!(shell.editor_mode, EditorMode::Vi);
        // The normalization makes the flags exclusive, GNU-style: exactly
        // one of emacs/vi reads on.
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("__RUBASH_SETOPT_vi")
                .as_deref(),
            Some("1")
        );
        assert_ne!(
            shell
                .session_mut()
                .variables()
                .resolved_value("__RUBASH_SETOPT_emacs")
                .as_deref(),
            Some("1")
        );
    }

    #[test]
    fn refresh_edit_mode_defaults_keep_configured_mode_quiet() {
        let mut shell = test_shell(HookConfig::default());
        // No engine flags at all: the configured default (emacs) stands and
        // no editor rebuild is signaled.
        assert_eq!(shell.refresh_edit_mode(), None);
        assert_eq!(shell.editor_mode, EditorMode::Emacs);
        // An explicit `set -o emacs` is also not a change.
        shell.session_mut().options().set("emacs", true);
        assert_eq!(shell.refresh_edit_mode(), None);
    }

    #[test]
    fn refresh_edit_mode_live_flip_cycle_vi_emacs_vi() {
        // The mission journey: `set -o vi` mid-session, `set -o emacs`
        // returns, `set -o vi` again — the last writer must win every time
        // (GNU set.def:424 set_edit_mode rebinds editing-mode live through
        // readline bind.c:2092 sv_editmode).
        let mut shell = test_shell(HookConfig::default());
        shell.session_mut().options().set("vi", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Vi));

        shell.session_mut().options().set("emacs", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Emacs));

        // Without the exclusive write-back the vi flag would still read on
        // here and this flip would be invisible — the reason normalization
        // exists.
        shell.session_mut().options().set("vi", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Vi));
    }

    #[test]
    fn refresh_edit_mode_noop_flips_and_disables_keep_mode() {
        let mut shell = test_shell(HookConfig::default());
        shell.session_mut().options().set("vi", true);
        shell.refresh_edit_mode();
        // Re-asserting the active mode is not a change.
        shell.session_mut().options().set("vi", true);
        assert_eq!(shell.refresh_edit_mode(), None);
        // `set +o emacs` while vi is active: GNU's set_edit_mode ignores a
        // disable of the inactive option (set.def:436-441) — no keymap
        // change.
        shell.session_mut().options().set("emacs", false);
        assert_eq!(shell.refresh_edit_mode(), None);
        // An explicit `set -o emacs` is a new writer and must win.
        shell.session_mut().options().set("emacs", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Emacs));
        shell.session_mut().options().set("vi", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Vi));
        // `set +o <active>` is GNU's line-editing-off path (set.def:433-441,
        // not wired yet): flags drop to all-off, the mode stands.
        shell.session_mut().options().set("vi", false);
        assert_eq!(shell.refresh_edit_mode(), None);
        assert_eq!(shell.editor_mode, EditorMode::Vi);
        // ... and the next explicit writer still lands.
        shell.session_mut().options().set("emacs", true);
        assert_eq!(shell.refresh_edit_mode(), Some(EditorMode::Emacs));
    }

    #[test]
    fn refresh_edit_mode_both_on_in_one_gap_resolves_vi() {
        // `set -o emacs; set -o vi` on one line between prompts: the order
        // is unrecoverable from the two flags; resolve toward the bashrc
        // idiom (documented tie-break).
        let mut shell = test_shell(HookConfig::default());
        shell.session_mut().options().set("emacs", true);
        shell.session_mut().options().set("vi", true);
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
        shell.session_mut().options().set("vi", true);
        shell.refresh_edit_mode();
        shell.session_mut().options().set("emacs", true);
        shell.refresh_edit_mode();
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("__RUBASH_SETOPT_emacs")
                .as_deref(),
            Some("1")
        );
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("__RUBASH_SETOPT_vi")
                .as_deref(),
            Some("0")
        );
        // SHELLOPTS never carries emacs/vi (rubash mirrors GNU's exclusion,
        // shellopts_includes_option), so the write-back must not add them.
        let shelopts = shell
            .executor_mut()
            .get_env("SHELLOPTS")
            .unwrap_or_default();
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
            title: std::rc::Rc::from(vec!["HOOK_TITLE=\"$NIU_TITLE\"".to_string()]),
            ..Default::default()
        });
        shell
            .session_mut()
            .variables()
            .set("NIU_TITLE", "custom-title");

        shell.run_precmd_hooks();

        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("HOOK_TITLE")
                .as_deref(),
            Some("custom-title"),
            "title hook should observe NIU_TITLE from the executor env"
        );
        assert!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_TITLE")
                .as_deref()
                .is_none(),
            "hook context env should be cleaned up after the hook runs"
        );
    }

    #[test]
    fn issue596_hook_context_cleanup_uses_direct_unset() {
        // niubash#596: hook context teardown used to run a full
        // `execute_script("unset ...")` lex/parse/execute roundtrip on
        // every prompt. The direct `unset_env` path must be observably
        // identical: the hook sees the context vars set, and all of them
        // are gone afterwards (no script roundtrip involved).
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();

        let mut shell = test_shell(HookConfig::default());
        let hooks = ["HOOK_SAW_A=\"$CTX_A\"; HOOK_SAW_B=\"$CTX_B\"".to_string()];
        let context = [
            ("CTX_A", "alpha".to_string()),
            ("CTX_B", "beta".to_string()),
        ];
        shell.run_hook_scripts(&hooks, &context);

        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("HOOK_SAW_A")
                .as_deref(),
            Some("alpha")
        );
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("HOOK_SAW_B")
                .as_deref(),
            Some("beta")
        );
        assert!(
            shell
                .session_mut()
                .variables()
                .resolved_value("CTX_A")
                .as_deref()
                .is_none(),
            "context var CTX_A must be unset after the hook run"
        );
        assert!(
            shell
                .session_mut()
                .variables()
                .resolved_value("CTX_B")
                .as_deref()
                .is_none(),
            "context var CTX_B must be unset after the hook run"
        );
    }

    #[test]
    fn precmd_invokes_title_hooks_with_pwd_fallback() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-title-pwd-fallback");
        std::fs::create_dir_all(&temp).unwrap();

        let mut shell = test_shell(HookConfig {
            title: std::rc::Rc::from(vec!["HOOK_TITLE=\"$NIU_TITLE\"".to_string()]),
            ..Default::default()
        });
        let target = shell_quote(&shell_display_path(&temp));
        shell
            .execute_interactive_line(&format!("cd {}", target))
            .unwrap();
        shell.execute_interactive_line("unset NIU_TITLE").unwrap();

        shell.run_precmd_hooks();

        let observed = shell
            .executor_mut()
            .get_env("HOOK_TITLE")
            .unwrap_or_default();
        assert!(
            observed.contains("niubash-title-pwd-fallback"),
            "title hook should observe PWD as fallback, got: {observed:?}"
        );

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn missing_primary_rc_ignores_other_rc_files_without_modifying_them() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-primary-only-defaults");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let original = "export NIU_IGNORED_RC_TEST=loaded\nalias ignored_rc_alias='echo loaded'\n";
        for name in [".winuxshrc", ".winshrc", ".bashrc"] {
            std::fs::write(home.join(name), original).unwrap();
        }
        for repl in [true, false] {
            let mut shell = test_shell(HookConfig::default());
            shell.home_dir = home.clone();
            let default_prompt = shell.session_mut().variables().resolved_value("PS1");
            assert_eq!(shell.startup_rc_path(), None);
            for _ in 0..2 {
                shell.run_startup_files(repl);
                assert_eq!(
                    shell
                        .session_mut()
                        .variables()
                        .resolved_value("NIU_IGNORED_RC_TEST")
                        .as_deref(),
                    None
                );
                assert!(!shell.aliases.contains_key("ignored_rc_alias"));
                assert_eq!(
                    shell
                        .session_mut()
                        .variables()
                        .resolved_value("PS1")
                        .as_deref(),
                    default_prompt.as_deref()
                );
                assert!(!home.join(NIU_RC_FILE).exists());
                for name in [".winuxshrc", ".winshrc", ".bashrc"] {
                    assert_eq!(std::fs::read_to_string(home.join(name)).unwrap(), original);
                }
            }
        }
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

        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_RC_VALUE")
                .as_deref(),
            Some("from-rc")
        );
        assert_eq!(
            shell.aliases.get("hello").map(String::as_str),
            Some("echo from-alias")
        );
        assert_eq!(shell.execute_interactive_line("hello").unwrap(), 0);
        assert!(shell
            .session_mut()
            .variables()
            .resolved_value("NIU_REPL_STARTUP")
            .as_deref()
            .is_none());

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn primary_rc_is_the_only_automatic_startup_file() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-primary-only-startup");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let ignored = "export NIU_IGNORED_RC_TEST=loaded\n";
        for name in [".winuxshrc", ".winshrc", ".bashrc"] {
            std::fs::write(home.join(name), ignored).unwrap();
        }
        for (primary, expected) in [
            ("export NIU_PRIMARY_RC_TEST=primary\n", Some("primary")),
            ("", None),
            ("if then\n", None),
        ] {
            std::fs::write(home.join(NIU_RC_FILE), primary).unwrap();
            let mut shell = test_shell(HookConfig::default());
            shell.home_dir = home.clone();
            assert_eq!(shell.startup_rc_path(), Some(home.join(NIU_RC_FILE)));
            shell.run_startup_rc();
            assert_eq!(
                shell
                    .session_mut()
                    .variables()
                    .resolved_value("NIU_PRIMARY_RC_TEST")
                    .as_deref(),
                expected
            );
            assert_eq!(
                shell
                    .session_mut()
                    .variables()
                    .resolved_value("NIU_IGNORED_RC_TEST")
                    .as_deref(),
                None
            );
            assert_eq!(
                std::fs::read_to_string(home.join(NIU_RC_FILE)).unwrap(),
                primary
            );
            for name in [".winuxshrc", ".winshrc", ".bashrc"] {
                assert_eq!(std::fs::read_to_string(home.join(name)).unwrap(), ignored);
            }
        }
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn startup_rc_override_and_norc_leave_ignored_rc_files_untouched() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-rc-selection-read-only");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let original = "export NIU_LEGACY_SELECTION_TEST=legacy\n";
        for name in [".winuxshrc", ".winshrc", ".bashrc"] {
            std::fs::write(home.join(name), original).unwrap();
        }
        let custom = home.join("custom.rc");
        std::fs::write(&custom, "export NIU_CUSTOM_SELECTION_TEST=custom\n").unwrap();

        for no_rc in [true, false] {
            let mut shell = test_shell(HookConfig::default());
            shell.home_dir = home.clone();
            shell.no_rc = no_rc;
            shell.rc_file = Some(custom.clone());
            shell.run_interactive_startup_rc();
            assert_eq!(
                shell
                    .session_mut()
                    .variables()
                    .resolved_value("NIU_LEGACY_SELECTION_TEST")
                    .as_deref(),
                None
            );
            assert_eq!(
                shell
                    .session_mut()
                    .variables()
                    .resolved_value("NIU_CUSTOM_SELECTION_TEST")
                    .as_deref(),
                if no_rc { None } else { Some("custom") }
            );
            assert!(!home.join(NIU_RC_FILE).exists());
            for name in [".winuxshrc", ".winshrc", ".bashrc"] {
                assert_eq!(std::fs::read_to_string(home.join(name)).unwrap(), original);
            }
        }

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
        let _ = shell.source_non_interactive_env();

        assert!(shell
            .session_mut()
            .variables()
            .resolved_value("NIU_AGENT_ENV")
            .as_deref()
            .is_none());

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
        let _ = shell.source_non_interactive_env();

        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_AGENT_ENV")
                .as_deref(),
            Some("loaded")
        );
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
        let _ = shell.source_non_interactive_env();

        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_AGENT_ENV")
                .as_deref(),
            Some("bash-loaded")
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    /// niubash#512: `BASH_ENV=/dev/null` is the standard idiom for
    /// neutralizing startup files. GNU bash sources it silently (a
    /// readable character device is an empty file to maybe_execute_file);
    /// the loader must not refuse it as "Is a directory" — only a real
    /// directory gets that diagnostic. The refusal printed to stderr and
    /// returned before sourcing, so the observable contract here is that
    /// the env step completes and the shell runs its command normally.
    #[cfg(unix)]
    #[test]
    fn bash_env_dev_null_sources_silently() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let _niu_guard = EnvVarGuard::unset("NIU_ENV");

        let temp = unique_temp_dir("niubash-env-devnull");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let _bash_guard = EnvVarGuard::set_value("BASH_ENV", "/dev/null");

        let mut shell = test_shell(HookConfig::default());
        shell.home_dir = home;
        let exit = shell.source_non_interactive_env();

        assert!(exit.is_none());
        assert_eq!(shell.execute_script("echo ok").unwrap(), 0);

        let _ = std::fs::remove_dir_all(temp);
    }

    /// niubash#513: a `shopt -s extglob` that lexically FOLLOWS a `case`
    /// must not open the parse gate for that case. GNU reads one line at
    /// a time (parse.y:5466 gates pattern chars on the gate in effect at
    /// read time), so the first case is a syntax error (rc 2) and nothing
    /// in it runs, even though the same input enables extglob later.
    #[test]
    fn extglob_flip_later_in_script_does_not_open_parse_gate_for_earlier_case() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();

        let mut shell = test_shell(HookConfig::default());
        // The non-interactive entry points (#513's `-c`/script faces)
        // construct with expand_aliases off (Shell::new_with_script_name);
        // test_shell defaults it on for REPL-shaped tests, which would
        // route this script to the engine's grouped reader instead of the
        // host tokenize+parse route under test.
        shell
            .executor_mut()
            .set_shopt_option("expand_aliases", false);
        let code = shell
            .execute_script(
                "R=none; case abc in +([a-z])) R=m1;; *) R=no;; esac; \
                 shopt -s extglob; case abc in +([a-z])) R=m2;; *) R=no2;; esac",
            )
            .unwrap();

        assert_eq!(code, 2, "the gated-off case is a syntax error");
        // GNU parses a whole line before executing any of it, so the
        // assignment on the same line as the rejected case is discarded
        // with the line and neither case body runs.
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("R")
                .as_deref(),
            None,
            "no case body may run"
        );
    }

    /// Control for the #513 gate restore: a `shopt -s extglob` on its OWN
    /// line still opens the gate for the NEXT line's case (GNU's
    /// line-at-a-time cadence), and a later `shopt -u extglob` line
    /// closes it again for the line after that.
    #[test]
    fn extglob_flip_on_earlier_line_opens_gate_for_later_lines() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();

        let mut shell = test_shell(HookConfig::default());
        shell
            .executor_mut()
            .set_shopt_option("expand_aliases", false);
        let code = shell
            .execute_script(
                "shopt -s extglob\n\
                 case abc in +([a-z])) R=m1;; *) R=no;; esac\n\
                 shopt -u extglob\n\
                 case abc in +([a-z])) R=m2;; *) R=no2;; esac",
            )
            .unwrap();

        assert_eq!(code, 2, "the case after `shopt -u extglob` errors");
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("R")
                .as_deref(),
            Some("m1"),
            "the case under the opened gate matched; the gated-off one did not run"
        );
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
        let _ = shell.source_non_interactive_env();

        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_AGENT_ENV")
                .as_deref(),
            Some("from-niu")
        );

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
        let _ = shell.source_non_interactive_env();

        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_AGENT_ENV")
                .as_deref(),
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
        let _ = shell.source_non_interactive_env();

        assert!(shell
            .session_mut()
            .variables()
            .resolved_value("NIU_AGENT_ENV")
            .as_deref()
            .is_none());

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
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_WIDGET_RESULT")
                .as_deref(),
            None
        );
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_WIDGET_BUFFER")
                .as_deref(),
            None
        );

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
            crate::bind_surface::EngineBindKind::Execute { command }
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
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("READLINE_LINE")
                .as_deref(),
            None
        );
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("READLINE_POINT")
                .as_deref(),
            None
        );

        // Registry generation moved by the rc `bind` call: the REPL uses
        // it to rebuild the mirrored keymaps before the next prompt.
        assert!(shell.engine_bind_generation > 0);

        let _ = std::fs::remove_dir_all(temp);
    }

    // niubash#630, K4: the [keymap] loader parses config.toml's rules
    // into the user layer (bad rules reported + skipped, never fatal),
    // and a user rule's `bind -x` resolves behind the engine snapshot:
    // index 0 is the engine's command, index 1 the user's.
    #[test]
    fn issue630_user_keymap_rules_load_and_bindx_resolves_behind_engine() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-keymap-rules");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(NIU_RC_FILE),
            r#"
bind -x '"\C-r": __engine_stub'
__engine_stub() { READLINE_LINE="engine:$READLINE_LINE"; READLINE_POINT=${#READLINE_LINE}; }
__user_stub() { READLINE_LINE="user:$READLINE_LINE"; READLINE_POINT=${#READLINE_LINE}; }
"#,
        )
        .unwrap();
        let config_path = temp.join("config.toml");
        std::fs::write(
            &config_path,
            "[keymap]\n\
             bind = ['-x \"\\C-g\": __user_stub', '\"\\C-x\\C-f\": redisplay', 'bogus rule']\n\
             unbind = ['\"\\C-a\"']\n",
        )
        .unwrap();
        let _config_guard = EnvVarGuard::set_value(
            "NIU_CONFIG",
            config_path.to_str().expect("temp path is UTF-8"),
        );

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();
        shell.load_engine_bindings();
        shell.load_user_keymap_rules();

        assert_eq!(shell.engine_bindings.len(), 1);
        // The chained rule parses (mirroring drops it later, by
        // shape); the bogus rule was reported and skipped.
        assert_eq!(shell.user_keymap_rules.len(), 2);
        assert_eq!(shell.user_keymap_unbinds.len(), 1);

        // Index 0 runs the engine's command…
        let outcome = shell.run_bind_x_command(0, "buf", 3);
        assert_eq!(outcome.buffer.as_deref(), Some("engine:buf"));
        // …and index 1 — the offset the editor build bakes into a
        // user rule's trigger — runs the USER command, never the
        // engine entry's.
        let outcome = shell.run_bind_x_command(1, "buf", 3);
        assert_eq!(outcome.buffer.as_deref(), Some("user:buf"));
        // Past the end of the combined list: a no-op.
        let outcome = shell.run_bind_x_command(2, "buf", 3);
        assert_eq!(outcome.buffer, None);

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
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_COMP_RESULT")
                .as_deref(),
            None
        );
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_COMP_WORDS")
                .as_deref(),
            None
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn niu_compdefs_directory_entries_load_completion_files() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-compdefs-dir");
        let home = temp.join("home");
        let defs = temp.join("compdefs");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&defs).unwrap();
        // Native TOML: same schema as the embedded applet definitions.
        std::fs::write(
            defs.join("mytool.toml"),
            r#"command = "mytool"
description = "fixture command"

[[flags]]
long = "--fixture-flag"
description = "declared in the completion dir"
"#,
        )
        .unwrap();
        // Standard bash completion script: statically parsed (opts= line plus
        // a case block), the file body is never executed.
        std::fs::write(
            defs.join("bashtool.bash"),
            r#"_bashtool() {
    local opts
    opts="-h -V --verbose --output"
    case "${prev}" in
        --output|-o)
            COMPREPLY=($(compgen -f "${cur}"))
            return 0
            ;;
        *)
            COMPREPLY=()
            ;;
    esac
}
complete -F _bashtool bashtool
"#,
        )
        .unwrap();

        let defs_forward = defs.to_string_lossy().replace('\\', "/");
        std::fs::write(
            home.join(NIU_RC_FILE),
            format!(
                r#"
NIU_COMPDEFS="{defs_forward}"

niu_fixture_comp() {{
    NIU_COMP_RESULT="alpha"
}}
NIU_COMPDEFS="$NIU_COMPDEFS
mytool2:niu_fixture_comp"
"#
            ),
        )
        .unwrap();

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        shell.run_startup_rc();
        shell.load_user_compdefs();

        // The `command:function` pair in the same variable still reaches the
        // function layer.
        assert_eq!(
            shell.compdefs,
            vec![("mytool2".to_string(), "niu_fixture_comp".to_string())]
        );
        // Both definition files from the declared directory serve flag
        // candidates through the same completion state the probe reads.
        assert!(
            shell
                .completion_probe("mytool -", 10)
                .contains(&"--fixture-flag".to_string()),
            "TOML definition from the NIU_COMPDEFS directory must serve flags"
        );
        assert!(
            shell
                .completion_probe("bashtool -", 10)
                .contains(&"--verbose".to_string()),
            "bash completion import from the NIU_COMPDEFS directory must serve flags"
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn niu_env_compspecs_reach_the_completion_surface() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let _bash_guard = EnvVarGuard::unset("BASH_ENV");

        let temp = unique_temp_dir("niubash-env-compspec");
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let env_file = home.join("agent.env");
        std::fs::write(
            &env_file,
            r#"
complete -W 'status switch merge rebase' envtool
_envtooldyn_hook() {
    COMPREPLY=()
}
complete -F _envtooldyn_hook envtooldyn
"#,
        )
        .unwrap();

        let _niu_guard = EnvVarGuard::set("NIU_ENV", &env_file);

        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        let _ = shell.source_non_interactive_env();

        // Issue #307: `complete` registrations from NIU_ENV land in the
        // engine compspec registry — the state `--completion-probe` serves
        // candidates from. The `-F` registration itself is visible here;
        // turning the -F hook into Tab candidates is the rubash engine
        // follow-up, so candidate flow is covered by the -W compspec and the
        // binary probe tests.
        assert!(shell.executor_mut().has_compspec("envtool"));
        assert!(shell.executor_mut().has_compspec("envtooldyn"));

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
    fn native_command_not_found_lines_drop_package_search_hints_by_default() {
        // niubash#249: rg owns a compiled-in executable-tool recipe, so the
        // targeted install hint stays; the winget/scoop/choco search block
        // is gone under the default (off) policy — even when every
        // third-party manager is installed.
        let lines = native_command_not_found_hint_lines("rg", |command| {
            matches!(command, "winget" | "scoop" | "choco")
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
        assert!(
            !lines.iter().any(|line| line.contains("search hints")),
            "package search hints are opt-in (niubash#249): {lines:?}"
        );
        for manager in ["winget", "scoop", "choco"] {
            assert!(
                !lines.iter().any(|line| line.contains(manager)),
                "third-party managers are never recommended: {lines:?}"
            );
        }
    }

    #[test]
    fn typo_gets_no_package_hint_by_default_issue_249_repro() {
        // owner repro (niubash#249): `% win` printed the command-not-found
        // line and then a winget search advertisement. Under the default
        // policy the hint channel contributes zero lines — the executor's
        // GNU one-liner is the whole output.
        let lines = native_command_not_found_hint_lines("win", |command| {
            matches!(command, "winget" | "scoop" | "choco")
        });
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn command_not_found_hint_wpm_opt_in_suggests_only_wpm_for_near_misses() {
        // Opt-in channel (`niu config set command-not-found-hint wpm`):
        // only wpm is ever named, and only when the word is a
        // high-confidence near-miss — dropping the last character hits a
        // known package command name (jq) or an installed command (rg).
        #[cfg(windows)]
        {
            let lines = native_command_not_found_hint_lines_with_policy(
                "jqq",
                |_| false,
                CommandNotFoundHint::Wpm,
            );
            assert_eq!(
                lines,
                vec![
                    "niubash: package search hints:".to_string(),
                    "  wpm search --name 'jqq'".to_string(),
                ]
            );

            let lines = native_command_not_found_hint_lines_with_policy(
                "rgt",
                |command| command == "rg",
                CommandNotFoundHint::Wpm,
            );
            assert_eq!(
                lines,
                vec![
                    "niubash: package search hints:".to_string(),
                    "  wpm search --name 'rgt'".to_string(),
                ]
            );
        }
        // wpm does not exist off Windows: the string must never surface
        // there, even with the switch set.
        #[cfg(not(windows))]
        {
            let lines = native_command_not_found_hint_lines_with_policy(
                "jqq",
                |_| true,
                CommandNotFoundHint::Wpm,
            );
            assert!(lines.is_empty(), "{lines:?}");
        }
    }

    #[test]
    fn command_not_found_hint_wpm_opt_in_stays_silent_for_arbitrary_typos() {
        // Trigger tightening (niubash#249): "win" minus its last character
        // ("wi") hits neither an installed command nor a known package
        // command name — a pure typo gets no hint even with the channel on.
        let lines = native_command_not_found_hint_lines_with_policy(
            "win",
            |command| matches!(command, "winget" | "scoop" | "choco"),
            CommandNotFoundHint::Wpm,
        );
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn shell_field_gates_command_not_found_hint_emission() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Default (off): a miss contributes no hint lines at all.
        {
            let shell = test_shell(HookConfig::default());
            assert_eq!(shell.command_not_found_hint, CommandNotFoundHint::Off);
            CHANNEL_CAPTURE.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
            shell.print_command_not_found_hints("win");
            let captured = CHANNEL_CAPTURE.with(|slot| slot.borrow_mut().take());
            assert!(
                captured.unwrap_or_default().is_empty(),
                "default policy must stay silent (niubash#249)"
            );
        }
        // Opt-in (wpm): the near-miss suggestion lands on stderr, wpm only.
        #[cfg(windows)]
        {
            let mut shell = test_shell(HookConfig::default());
            shell.command_not_found_hint = CommandNotFoundHint::Wpm;
            CHANNEL_CAPTURE.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
            shell.print_command_not_found_hints("jqq");
            let captured = CHANNEL_CAPTURE.with(|slot| slot.borrow_mut().take());
            let captured = captured.expect("capture disarmed");
            assert!(
                captured
                    .iter()
                    .all(|(channel, _)| *channel == ShellChannel::Stderr),
                "hints are stderr-only: {captured:?}"
            );
            let text: String = captured
                .into_iter()
                .map(|(_, bytes)| String::from_utf8(bytes).unwrap())
                .collect();
            assert_eq!(
                text,
                "niubash: package search hints:\n  wpm search --name 'jqq'\n"
            );
            assert!(!text.contains("winget"));
        }
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
    fn interactive_lineno_advances_across_fast_reader_and_multiline_inputs() {
        let _env_lock = PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-interactive-line-base");
        std::fs::create_dir_all(&temp).unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = temp.join("history");
        shell.enter_interactive();
        shell.execute_interactive_line("FIRST=$LINENO").unwrap();
        shell
            .execute_interactive_line("alias mark='SECOND=$LINENO'")
            .unwrap();
        shell.execute_interactive_line("mark").unwrap();
        shell
            .execute_interactive_script("\nFOURTH=$LINENO\nFIFTH=$LINENO\n")
            .unwrap();
        shell.execute_interactive_line("SIXTH=$LINENO").unwrap();
        for (name, expected) in [
            ("FIRST", "1"),
            ("SECOND", "3"),
            ("FOURTH", "5"),
            ("FIFTH", "6"),
            ("SIXTH", "7"),
        ] {
            assert_eq!(
                shell
                    .session_mut()
                    .variables()
                    .resolved_value(name)
                    .as_deref(),
                Some(expected),
                "{name}"
            );
        }
        shell
            .execute_interactive_line("shopt -u expand_aliases")
            .unwrap();
        shell.execute_interactive_line("OFF_LINE=$LINENO").unwrap();
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("OFF_LINE")
                .as_deref(),
            Some("9")
        );
        drop(shell);
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn interactive_alias_cli_disable_survives_repl_entry() {
        let _env_lock = PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-alias-cli-disable");
        std::fs::create_dir_all(&temp).unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = temp.join("history");
        let invocation = rubash::invocation::ShellInvocation::parse(&[
            "-i".into(),
            "+O".into(),
            "expand_aliases".into(),
        ])
        .unwrap();
        shell.apply_invocation_before_startup(&invocation).unwrap();
        shell.enter_interactive();
        assert!(!shell.executor_mut().alias_expansion_enabled());
        drop(shell);
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn interactive_same_line_alias_waits_for_next_submission() {
        let _env_lock = PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-alias-read-boundary");
        std::fs::create_dir_all(&temp).unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = temp.join("history");
        shell.enter_interactive();
        assert_eq!(
            shell
                .execute_interactive_line(
                    "alias nextprompt='NIU_ALIAS_BOUNDARY_RESULT=next'; nextprompt"
                )
                .unwrap(),
            127
        );
        assert!(shell
            .executor_mut()
            .get_env("NIU_ALIAS_BOUNDARY_RESULT")
            .is_none());
        assert_eq!(shell.execute_interactive_line("nextprompt").unwrap(), 0);
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_ALIAS_BOUNDARY_RESULT")
                .as_deref(),
            Some("next")
        );
        drop(shell);
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn live_alias_executes_after_separate_interactive_submissions() {
        let _env_lock = PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-live-alias");
        std::fs::create_dir_all(&temp).unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = temp.join("history");
        shell.enter_interactive();
        assert_eq!(
            shell
                .execute_interactive_line("alias ll='NIU_LIVE_ALIAS_RESULT=expanded'")
                .unwrap(),
            0
        );
        assert!(shell.executor_mut().alias_expansion_enabled());
        assert!(shell
            .executor_mut()
            .get_env("__RUBASH_ALIAS_STREAMED")
            .is_none());
        assert_eq!(
            shell.aliases.get("ll").map(String::as_str),
            Some("NIU_LIVE_ALIAS_RESULT=expanded")
        );
        assert_eq!(shell.execute_interactive_line("alias ll").unwrap(), 0);
        assert_eq!(shell.execute_interactive_line("ll").unwrap(), 0);
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_LIVE_ALIAS_RESULT")
                .as_deref(),
            Some("expanded")
        );
        drop(shell);
        let _ = std::fs::remove_dir_all(temp);
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
        assert!(!shell.aliases.contains_key("gst"));
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
            shell
                .session_mut()
                .variables()
                .resolved_value("SOURCE_COMPLETION_VAR")
                .as_deref(),
            Some("from-source")
        );
        assert!(shell
            .completion_probe("$SOURCE_COMPLETION", "$SOURCE_COMPLETION".len())
            .contains(&"$SOURCE_COMPLETION_VAR".to_string()));
    }

    #[test]
    fn issue594_completion_state_skips_rebuild_when_nothing_changed() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        shell.execute_interactive_line("ISSUE594_VAR=one").unwrap();
        // Plant sentinels in the shared state: a rebuild would replace
        // all three components wholesale and wipe them, so their
        // survival proves the niubash#594 gate skipped the re-clone.
        {
            let mut state = shell.completion_state.lock().unwrap();
            state
                .env_vars
                .insert("ISSUE594_SENTINEL".to_string(), "keep".to_string());
            state.functions.insert("issue594_sentinel_fn".to_string());
            state.aliases.insert("issue594_sentinel_alias".to_string());
        }
        shell.update_completion_state();
        let state = shell.completion_state.lock().unwrap();
        assert!(state.env_vars.contains_key("ISSUE594_SENTINEL"));
        assert!(state.functions.contains("issue594_sentinel_fn"));
        assert!(state.aliases.contains("issue594_sentinel_alias"));
        assert_eq!(
            state.env_vars.get("ISSUE594_VAR").map(String::as_str),
            Some("one")
        );
    }

    #[test]
    fn issue594_completion_state_tracks_function_define_and_unset() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        shell
            .execute_interactive_line("issue594fn() { true; }")
            .unwrap();
        assert!(shell
            .completion_probe("issue594f", "issue594f".len())
            .contains(&"issue594fn".to_string()));

        shell
            .execute_interactive_line("unset -f issue594fn")
            .unwrap();
        assert!(!shell
            .completion_probe("issue594f", "issue594f".len())
            .contains(&"issue594fn".to_string()));
    }

    #[test]
    fn issue594_completion_state_tracks_alias_and_env_value_changes() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        shell
            .execute_interactive_line("alias i594ll='ls -la'")
            .unwrap();
        assert!(shell
            .completion_probe("i594l", "i594l".len())
            .contains(&"i594ll".to_string()));

        shell.execute_interactive_line("unalias i594ll").unwrap();
        assert!(!shell
            .completion_probe("i594l", "i594l".len())
            .contains(&"i594ll".to_string()));

        shell.execute_interactive_line("ISSUE594_V=alpha").unwrap();
        assert_eq!(
            shell
                .completion_state
                .lock()
                .unwrap()
                .env_vars
                .get("ISSUE594_V")
                .map(String::as_str),
            Some("alpha")
        );
        shell.execute_interactive_line("ISSUE594_V=beta").unwrap();
        assert_eq!(
            shell
                .completion_state
                .lock()
                .unwrap()
                .env_vars
                .get("ISSUE594_V")
                .map(String::as_str),
            Some("beta")
        );
    }

    #[test]
    fn issue594_completion_env_fingerprint_is_stable_and_change_sensitive() {
        let mut a = std::collections::HashMap::new();
        a.insert("A".to_string(), "1".to_string());
        a.insert("B".to_string(), "2".to_string());
        let mut b = std::collections::HashMap::new();
        b.insert("B".to_string(), "2".to_string());
        b.insert("A".to_string(), "1".to_string());
        // Insertion order must not matter, and 0 is the reserved
        // "no snapshot yet" sentinel the helper must never emit.
        assert_eq!(
            Shell::completion_env_fingerprint(&a),
            Shell::completion_env_fingerprint(&b)
        );
        assert_ne!(Shell::completion_env_fingerprint(&a), 0);
        b.insert("B".to_string(), "changed".to_string());
        assert_ne!(
            Shell::completion_env_fingerprint(&a),
            Shell::completion_env_fingerprint(&b)
        );
        b.remove("B");
        assert_ne!(
            Shell::completion_env_fingerprint(&a),
            Shell::completion_env_fingerprint(&b)
        );
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
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("RESULT")
                .as_deref(),
            Some("OK")
        );
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

    /// niubash#302: the shared resolver honors `HOME` isolation on every
    /// platform — an explicit `HOME` override wins over `USERPROFILE`
    /// (the pre-#302 Windows order preferred the system profile, which
    /// leaked HOME-only sandboxes back into the real home).
    #[test]
    fn shell_home_dir_prefers_home_env_over_userprofile() {
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
            crate::path_utils::normalize_existing_host_path(home.clone())
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

        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_RC_SOURCE")
                .as_deref(),
            Some("primary")
        );
        if cfg!(windows) {
            assert!(
                shell
                    .executor_mut()
                    .get_env("HOME")
                    .is_some_and(is_windows_drive_path),
                "HOME should be Windows-native for external tools, got {:?}",
                shell
                    .session_mut()
                    .variables()
                    .resolved_value("HOME")
                    .as_deref()
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
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("rest")
                .as_deref(),
            Some("#fe0000\"/>")
        );
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
    fn filesystem_blind_string_tools_keep_slash_drive_operands_verbatim() {
        // niubash#346(c): dirname/basename never stat their operand, so a
        // POSIX-shaped NAME such as /a/b is display text, not a drive form.
        // The host must not fold it to A:/b before rubash's own mask runs
        // (rubash argv_data_primary_positions exempts the same two tools).
        let tokens = tokenize(
            "dirname /a/b; basename /a/b/c.txt; dirname -z /a/b /usr/bin/seq; dirname a/b",
        );
        let mut ast = parse(&tokens);
        normalize_winuxcmd_slash_drive_args(&mut ast);

        assert_eq!(ast.commands[0].words[1], "/a/b");
        assert_eq!(ast.commands[1].words[1], "/a/b/c.txt");
        assert_eq!(ast.commands[2].words[1], "-z");
        assert_eq!(ast.commands[2].words[2], "/a/b");
        assert_eq!(ast.commands[2].words[3], "/usr/bin/seq");
        assert_eq!(ast.commands[3].words[1], "a/b");
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

    /// niubash#574: the probe-memo invalidation trigger sees `hash`/`wpm`
    /// at any command position and ignores them elsewhere.
    #[test]
    fn probe_memo_invalidation_tracks_hash_and_wpm_words() {
        for line in [
            "hash -r",
            "hash",
            "hash -d ls",
            "hash -p /x y",
            "wpm install eza",
            "wpm.exe remove eza",
            "WPM update",
            "ls; hash -d foo | cat",
            "ls && wpm list",
            "PATH=/tmp wpm index update",
        ] {
            assert!(
                line_touches_command_resolution(&tokenize(line)),
                "expected {line:?} to invalidate the probe memo"
            );
        }
        for line in [
            "echo hash",
            "echo wpm install eza",
            "hashish -r",
            "pwpm install eza",
            "ls -la",
            "type hash",
        ] {
            assert!(
                !line_touches_command_resolution(&tokenize(line)),
                "expected {line:?} not to invalidate the probe memo"
            );
        }
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
            shell
                .session_mut()
                .variables()
                .resolved_value("PWD")
                .as_deref()
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
            shell
                .session_mut()
                .variables()
                .resolved_value("PWD")
                .as_deref()
        );
        if cfg!(windows) {
            // rubash#556 (P0 #537): $PWD/`pwd` render the POSIX persona form
            // unconditionally — the native-style hatch is retired, and the
            // form must not depend on which route executed the cd.
            let pwd = shell
                .session_mut()
                .variables()
                .resolved_value("PWD")
                .unwrap_or_default();
            let expected_persona = format!(
                "/{}{}",
                (target_shell_path.as_bytes()[0] as char).to_ascii_lowercase(),
                &target_shell_path[2..]
            );
            assert_eq!(
                pwd, expected_persona,
                "PWD must render the POSIX persona form (rubash#556/#537), got {pwd:?}"
            );
        } else {
            assert_eq!(
                shell
                    .session_mut()
                    .variables()
                    .resolved_value("PWD")
                    .as_deref(),
                Some(target_shell_path.as_str())
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

        // RAII env guards: a panicking assert must not leak this
        // test's PATH/PATHEXT into the shared test process.
        let mut paths = vec![bin.clone()];
        if let Some(old_path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&old_path));
        }
        let new_path = std::env::join_paths(paths).unwrap();
        let _path_guard = EnvVarGuard::set("PATH", std::path::Path::new(&new_path));
        let _pathext_guard = EnvVarGuard::set_value("PATHEXT", ".COM;.EXE;.BAT;.CMD");
        std::env::set_current_dir(&start).unwrap();

        let mut shell = test_shell(HookConfig::default());
        let code = shell.execute_line("cd target; cwdprobe").unwrap();

        assert_eq!(code, 0);
        let observed = std::fs::read_to_string(&log).unwrap();
        let observed = host_path_to_shell_path(observed.trim());
        let expected = shell_display_path(&target);
        // temp_dir() may spell the fixture root with its 8.3 alias
        // (RUNNER~1) while the child's %CD% reports the long form, so
        // compare the canonicalized directories (the completion-cwd
        // assert above does the same) instead of the spellings.
        let observed_dir = std::path::PathBuf::from(shell_path_to_host_path(&observed));
        assert_eq!(
            observed_dir.canonicalize().unwrap(),
            target.canonicalize().unwrap(),
            "native child cwd mismatch: observed={observed:?}, expected={expected:?}"
        );
        assert!(
            same_shell_dir(
                shell
                    .session_mut()
                    .variables()
                    .resolved_value("PWD")
                    .as_deref()
                    .unwrap_or_default(),
                &expected
            ),
            "executor PWD mismatch: {:?}, expected={expected:?}",
            shell
                .session_mut()
                .variables()
                .resolved_value("PWD")
                .as_deref()
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn bash_prompt_command_updates_ps1_before_prompt_render() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let _columns_guard = EnvVarGuard::set_value("COLUMNS", "80");
        let mut shell = test_shell(HookConfig::default());
        shell.session_mut().variables().set(
            "PROMPT_COMMAND",
            "PS1=\"status:$? cols:${COLUMNS:-missing}> \"",
        );
        shell.session_mut().set_last_status(7);

        shell.run_precmd_hooks();

        match &shell.prompt {
            PromptBackend::Bash(prompt) => {
                assert_eq!(prompt.render_prompt_left(), "status:7 cols:80> ");
            }
            _ => panic!("expected Bash prompt backend"),
        }
        assert_eq!(shell.session().last_status(), 7);
    }

    #[test]
    fn one_shot_shell_uses_a_terminal_free_prompt_backend() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let shell = Shell::new_one_shot().expect("one-shot shell should initialize");

        assert!(shell.one_shot);
        assert!(matches!(shell.prompt, PromptBackend::Template(_)));
    }

    #[test]
    fn issue562_stdin_fast_shell_skips_repl_only_init() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let shell = Shell::new_for_stdin_script_fast().expect("stdin fast shell should initialize");

        // The #562 fast path carries the same REPL-free construction as a
        // one-shot shell: placeholder prompt backend, no completion scan.
        assert!(shell.one_shot);
        assert!(matches!(shell.prompt, PromptBackend::Template(_)));
        // …but the stdin naming contract is the stdin one, not the -c one:
        // no script name is pre-bound, so the caller binds argv[0] itself
        // (see run_stdin_script's __RUBASH_SCRIPT_NAME binding).
        assert!(shell.executor().get_env("__RUBASH_SCRIPT_NAME").is_none());
    }

    #[test]
    fn issue562_stdin_full_shell_keeps_repl_capable_init() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let shell = Shell::new_for_stdin_script().expect("stdin shell should initialize");

        // The full stdin constructor stays promotable to the REPL (-i or a
        // terminal stdio set), so it must NOT take the fast path.
        assert!(!shell.one_shot);
        assert!(shell.executor().get_env("__RUBASH_SCRIPT_NAME").is_none());
    }

    #[test]
    fn bash_ps1_prompt_escapes_render_from_executor_state() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell
            .executor_mut()
            .set_env("PS1", "user:\\u host:\\h dir:\\w \\\\$ ");
        shell.session_mut().variables().set("PS2", "more> ");

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
        let history_dir = tempfile::tempdir().unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = history_dir.path().join("first-history");
        shell
            .executor_mut()
            .set_env("PS1", r"\[\033]0;x\007\]`__git_ps1` $ ");

        shell.enter_interactive();

        // The foreign value is gone from the environment, so no prompt path
        // (REPL sync or engine interactive stdin) can expand `__git_ps1`.
        // Entry then seeds the GNU compiled-in default (niubash#672),
        // read back through the same executor face the seed wrote.
        assert_eq!(shell.executor().get_env("PS1"), Some(GNU_DEFAULT_PS1));
        // The shell's own theme backend is untouched by the discard.
        assert!(matches!(shell.prompt, PromptBackend::Template(_)));

        // A PS1 without Git Bash session machinery is user content and must
        // survive into prompt adoption exactly as before.
        let mut plain = test_shell(HookConfig::default());
        plain.history_path = history_dir.path().join("plain-history");
        plain.session_mut().variables().set("PS1", r"\u@\h:\w\$ ");
        plain.enter_interactive();
        assert_eq!(
            plain
                .session_mut()
                .variables()
                .resolved_value("PS1")
                .as_deref(),
            Some(r"\u@\h:\w\$ ")
        );
        plain.sync_bash_prompt_from_env();
        assert!(matches!(plain.prompt, PromptBackend::Bash(_)));
    }

    /// niubash#672: with no PS1 anywhere, interactive entry seeds the
    /// GNU compiled-in default as a plain variable — visible to probes
    /// (`${PS1:+...}`, `declare -p PS1`) — while the product floor
    /// keeps rendering: the seed is not a §14.5 prompt claim. A user
    /// write afterwards claims the slot through the ordinary path.
    #[test]
    fn enter_interactive_seeds_gnu_default_ps1_without_claiming_the_floor() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let history_dir = tempfile::tempdir().unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = history_dir.path().join("history");
        shell.executor_mut().unset_env("PS1");

        shell.enter_interactive();

        assert_eq!(shell.executor().get_env("PS1"), Some(GNU_DEFAULT_PS1));
        assert!(!shell.prompt_slot_claimed());
        shell.run_precmd_hooks();
        assert!(
            matches!(shell.prompt, PromptBackend::Template(_)),
            "the floor keeps rendering under the seeded default"
        );

        shell.executor_mut().set_env("PS1", "mine> ");
        assert!(shell.prompt_slot_claimed());
        shell.run_precmd_hooks();
        assert!(matches!(shell.prompt, PromptBackend::Bash(_)));
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
        let history_dir = tempfile::tempdir().unwrap();
        let mut shell = test_shell(HookConfig::default());
        shell.history_path = history_dir.path().join("history");
        shell.enter_interactive();
        // Simulate a framework theme block sourced from the rc. The value
        // deliberately reuses #117's marker shapes to prove ordering, not
        // content, is the guard.
        shell
            .executor_mut()
            .set_env("PS1", r"\u@\h $MSYSTEM \w `__git_ps1` $ ");
        shell.run_precmd_hooks();
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("PS1")
                .as_deref(),
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
        shell.session_mut().variables().set("PS1", "claimed-face> ");
        shell.run_precmd_hooks();
        match &shell.prompt {
            PromptBackend::Bash(prompt) => {
                assert_eq!(prompt.render_prompt_left(), "claimed-face> ");
            }
            _ => panic!("expected the claim to render"),
        }

        shell.session_mut().variables().unset("PS1");
        shell.run_precmd_hooks();
        match &shell.prompt {
            PromptBackend::Template(floor) => {
                let left = reedline::Prompt::render_prompt_left(floor).into_owned();
                assert!(!left.contains("claimed-face"), "{left}");
            }
            _ => panic!("expected the product floor after release"),
        }

        // Re-claim after release still wins (claim/release/claim cycle).
        shell.session_mut().variables().set("PS1", "reclaimed> ");
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
        shell
            .executor_mut()
            .set_env("PROMPT_COMMAND", "NIU_HOOK_RAN=1");

        shell.run_precmd_hooks();

        assert!(
            matches!(shell.prompt, PromptBackend::Template(_)),
            "a hook without PS1 must not claim the prompt slot"
        );
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_HOOK_RAN")
                .as_deref(),
            Some("1"),
            "the hook still ran before the prompt"
        );
    }

    // ---- niubash#592 (S2): per-pass Vcs computation gates ----
    //
    // The acceptance probe of the S2 design (its §6): integer spawn
    // counts only, read off the `read_status` call counter — the
    // prompt's single git spawn point, one call per spawn. Every probe
    // eprintln!s its integers so the numbers land in the test log
    // verbatim. The fixtures drive real git; under the cluster's Wine
    // shim (a `git.exe` answering --version with a marker and serving
    // `status` from a state file) the same tests run with the state
    // file standing in for repository state.

    /// Run `git` in `dir` (fixture plumbing, mirroring git_prompt's
    /// test helper); `false` means git is unusable here and the
    /// fixture must abort quietly.
    #[cfg(windows)]
    fn git_ok(dir: &std::path::Path, args: &[&str]) -> bool {
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
    }

    /// A one-commit repository whose branch is `branch`.
    #[cfg(windows)]
    fn repo_on_branch(dir: &std::path::Path, branch: &str) -> bool {
        let init = format!("--initial-branch={branch}");
        git_ok(dir, &["init", "--quiet", &init, "."])
            && git_ok(dir, &["commit", "--quiet", "--allow-empty", "-m", "init"])
    }

    /// True when PATH's git is the niubash#592 Wine shim: it answers
    /// --version with a marker and serves `status` from the state file
    /// named by `NIU592_GIT_STATE`.
    #[cfg(windows)]
    fn shim_git_in_use() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains("niu592-shim"))
            .unwrap_or(false)
    }

    /// Point the Wine shim's canned `status` at `branch` (clean tree).
    /// A no-op outside shim runs (real git needs no state file).
    #[cfg(windows)]
    fn write_shim_state(branch: &str) {
        if let Ok(state) = std::env::var("NIU592_GIT_STATE") {
            let _ = std::fs::write(
                state,
                format!(
                    "# branch.oid 1111111111111111111111111111111111111111\n\
                     # branch.head {branch}\n"
                ),
            );
        }
    }

    #[cfg(windows)]
    fn segments_shell_with_vcs() -> Shell {
        let mut shell = test_shell(HookConfig::default());
        shell.prompt = PromptBackend::Segments(SegmentPromptAdapter::new(SegmentPrompt::new(
            SegmentPromptConfig {
                left_elements: vec![SegmentId::Vcs],
                right_elements: Vec::new(),
                separator: " ".to_string(),
                prompt_symbol: "$".to_string(),
                preset: None,
            },
        )));
        shell
    }

    #[cfg(windows)]
    fn render_left_of(prompt: &PromptBackend) -> String {
        reedline::Prompt::render_prompt_left(prompt).into_owned()
    }

    /// Probe cells 1+2 (§6) and the §5 rows they cover (first frame,
    /// ordinary pass, repaint storm, completion/resize repaints — all
    /// repaints of the handed-out prompt), under the niubash#685
    /// revision of the S2 contract: a pass never computes and never
    /// waits — the first pass launches ONE background refresh and
    /// renders without the segment, repaints re-read the stamp, and
    /// later passes are cache hits that add zero spawns while the
    /// fingerprint is unchanged and no command cycle intervenes.
    #[cfg(windows)]
    #[test]
    fn issue592_probe_repaint_storm_and_pass_budget() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_on_branch(tmp.path(), "main") {
            return;
        }
        write_shim_state("main");
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        std::env::set_current_dir(tmp.path()).unwrap();
        let mut shell = segments_shell_with_vcs();
        // Keyed by the worktree as resolved from the process cwd —
        // the same derivation `prompt_for_display` feeds the cache.
        let key = crate::git_prompt::worktree_key_for(&std::env::current_dir().unwrap()).unwrap();

        // Cell 1 / first frame: the cold pass renders without the
        // segment (it does not wait for git), a repaint storm re-reads
        // the stamp, and exactly one background spawn lands the value.
        let prompt = shell.prompt_for_display();
        assert!(
            !render_left_of(&prompt).contains("main"),
            "the cold pass must not wait for the segment"
        );
        for _ in 0..10 {
            let _ = render_left_of(&prompt);
            let _ = reedline::Prompt::render_prompt_right(&prompt);
        }
        assert!(crate::git_prompt::wait_for_segment_cache_idle(
            std::time::Duration::from_secs(30)
        ));
        let after_storm = crate::git_prompt::read_status_call_count(&key);
        eprintln!("issue592 probe cell1 (#685): after_10_repaints_spawns={after_storm}");
        assert_eq!(after_storm, 1, "one background spawn; repaints never spawn");

        // Cell 2: K further passes, each with its own repaint storm,
        // are cache hits — the total stays at one spawn.
        for k in 2..=4usize {
            let p = shell.prompt_for_display();
            for _ in 0..5 {
                let _ = render_left_of(&p);
            }
            assert!(render_left_of(&p).contains("main"));
            let total = crate::git_prompt::read_status_call_count(&key);
            eprintln!("issue592 probe cell2 (#685): passes={k} total_spawns={total}");
            assert_eq!(total, 1, "pass {k} must be a cache hit");
        }
    }

    /// Probe cell 5 (§6, the §3.4 ordering invariant), as revised by
    /// niubash#685: the S2 shape reflected a hook's repository change
    /// in the SAME pass by computing synchronously after the hooks —
    /// that computation is exactly the per-pass wait #685 removes.
    /// The revised invariant: hooks still run before the pass, the
    /// pass detects the change (the fingerprint under real git, the
    /// command-cycle mark for the hook itself under any git) and
    /// launches ONE background refresh while rendering the cached
    /// value, and the hook's branch is visible from the NEXT pass.
    #[cfg(windows)]
    #[test]
    fn issue592_probe_hook_change_visible_next_pass() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_on_branch(tmp.path(), "main") {
            return;
        }
        write_shim_state("main");
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        std::env::set_current_dir(tmp.path()).unwrap();
        let mut shell = segments_shell_with_vcs();
        shell.sync_executor_pwd_from_process_cwd();
        let key = crate::git_prompt::worktree_key_for(&std::env::current_dir().unwrap()).unwrap();

        // Settle the cache on the pre-hook value.
        let _ = shell.prompt_for_display();
        assert!(crate::git_prompt::wait_for_segment_cache_idle(
            std::time::Duration::from_secs(30)
        ));
        let before = shell.prompt_for_display();
        assert!(render_left_of(&before).contains("main"));
        assert_eq!(crate::git_prompt::read_status_call_count(&key), 1);

        // REPL order: hooks first, prompt second. Under the Wine shim
        // the "repository change" is the canned status flipping to the
        // new branch (rewritten by the hook itself, via a builtin so
        // the hook needs no external command); with real git the hook
        // switches branch for real.
        let hook = if shim_git_in_use() {
            let state = std::env::var("NIU592_GIT_STATE").unwrap();
            format!(
                "printf '# branch.oid 1111111111111111111111111111111111111111\\n\
                 # branch.head hooked\\n' > \"{state}\""
            )
        } else {
            "git checkout --quiet -b hooked".to_string()
        };
        shell.session_mut().variables().set("PROMPT_COMMAND", &hook);
        shell.run_precmd_hooks();

        // The pass after the hook renders the cached (pre-hook) value
        // — it never waits — and exactly one background spawn lands
        // the hook's branch for the next pass.
        let prompt = shell.prompt_for_display();
        let left = render_left_of(&prompt);
        assert!(crate::git_prompt::wait_for_segment_cache_idle(
            std::time::Duration::from_secs(30)
        ));
        let spawns = crate::git_prompt::read_status_call_count(&key);
        eprintln!("issue592 probe cell5 (#685): total_spawns={spawns} pass_after_hook={left:?}");
        assert_eq!(
            spawns, 2,
            "the hook cycle costs exactly one background spawn"
        );
        assert!(
            left.contains("main") && !left.contains("hooked"),
            "the pass after the hook serves the cached value: {left:?}"
        );
        let next = shell.prompt_for_display();
        assert!(
            render_left_of(&next).contains("hooked"),
            "the hook's branch is visible from the next pass"
        );
        assert_eq!(crate::git_prompt::read_status_call_count(&key), 2);
    }

    /// §5 rows "empty Enter" / "prompt Ctrl+C": both return to the
    /// loop top and take a fresh prompt with no command cycle in
    /// between (repl.rs: the empty-line `continue`, the CtrlC branch's
    /// `continue` — neither reaches `execute_interactive_line`), so
    /// consecutive passes with nothing between them are their exact
    /// shape. Under niubash#685 they are all cache hits: the first
    /// pass's one background spawn is the only one. (The S2-era
    /// expectation — each such pass computes once — is superseded; a
    /// command that DID run, even one interrupted mid-flight, goes
    /// through the command-cycle mark instead and costs one
    /// background refresh, pinned in git_prompt's own tests.)
    #[cfg(windows)]
    #[test]
    fn issue592_consecutive_passes_hit_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_on_branch(tmp.path(), "main") {
            return;
        }
        write_shim_state("main");
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        std::env::set_current_dir(tmp.path()).unwrap();
        let mut shell = segments_shell_with_vcs();
        let key = crate::git_prompt::worktree_key_for(&std::env::current_dir().unwrap()).unwrap();
        for m in 1..=3usize {
            let p = shell.prompt_for_display();
            let _ = render_left_of(&p);
            assert!(crate::git_prompt::wait_for_segment_cache_idle(
                std::time::Duration::from_secs(30)
            ));
            let total = crate::git_prompt::read_status_call_count(&key);
            eprintln!("issue592 probe empty-enter shape (#685): passes={m} total_spawns={total}");
            assert_eq!(total, 1, "pass {m} must be served by the one landing");
        }
        let p = shell.prompt_for_display();
        assert!(render_left_of(&p).contains("main"));
    }

    /// Probe cell 3 (§6) / §5 "outside a repository" row: no pass and
    /// no repaint ever spawns, and the segment stays empty. (Under
    /// #685 the count is keyed by resolved worktree; outside a
    /// repository there is none — `find_worktree` short-circuits
    /// before the cache or the spawn point can be reached, which the
    /// `None` key pins structurally.)
    #[cfg(windows)]
    #[test]
    fn issue592_outside_repository_never_spawns() {
        let tmp = tempfile::tempdir().unwrap();
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        std::env::set_current_dir(tmp.path()).unwrap();
        let mut shell = segments_shell_with_vcs();
        assert!(crate::git_prompt::worktree_key_for(tmp.path()).is_none());
        for _ in 0..3 {
            let p = shell.prompt_for_display();
            for _ in 0..4 {
                let _ = render_left_of(&p);
            }
            assert_eq!(render_left_of(&p), "");
        }
        let total = crate::git_prompt::read_status_call_count(tmp.path());
        eprintln!("issue592 probe cell3: outside_repo_total_spawns={total}");
        assert_eq!(total, 0, "outside a repository nothing may spawn");
    }

    /// Probe cell 4 (§6) and the §5 "cd" row, under niubash#685: the
    /// root and a subdirectory of one repository share the root's
    /// cache entry (the key is the resolved worktree), so the
    /// subdirectory pass is a hit at no extra spawn; moving to
    /// another repository is cold there — the first pass renders
    /// without the segment, one background spawn lands it, and the
    /// next pass shows it.
    #[cfg(windows)]
    #[test]
    fn issue592_subdir_matches_root_and_cd_recomputes_next_pass() {
        let repo_a = tempfile::tempdir().unwrap();
        if !repo_on_branch(repo_a.path(), "main") {
            return;
        }
        let repo_b = tempfile::tempdir().unwrap();
        if !repo_on_branch(repo_b.path(), "other") {
            return;
        }
        let sub = repo_a.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        write_shim_state("main");
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = segments_shell_with_vcs();

        std::env::set_current_dir(repo_a.path()).unwrap();
        let key_a = crate::git_prompt::worktree_key_for(&std::env::current_dir().unwrap()).unwrap();
        let _ = shell.prompt_for_display();
        assert!(crate::git_prompt::wait_for_segment_cache_idle(
            std::time::Duration::from_secs(30)
        ));
        let at_root = render_left_of(&shell.prompt_for_display());
        let after_root = crate::git_prompt::read_status_call_count(&key_a);
        std::env::set_current_dir(&sub).unwrap();
        let at_sub = render_left_of(&shell.prompt_for_display());
        let after_sub = crate::git_prompt::read_status_call_count(&key_a);
        eprintln!(
            "issue592 probe cell4 (#685): root_spawns={after_root} subdir_spawns={after_sub} \
             identical={}",
            at_root == at_sub
        );
        assert_eq!(after_root, 1);
        assert_eq!(after_sub, 1, "the subdirectory pass is a cache hit");
        assert_eq!(at_root, at_sub, "same repository, same segment");
        assert!(at_root.contains("main"));

        write_shim_state("other");
        std::env::set_current_dir(repo_b.path()).unwrap();
        let key_b = crate::git_prompt::worktree_key_for(&std::env::current_dir().unwrap()).unwrap();
        let cold_b = render_left_of(&shell.prompt_for_display());
        assert!(
            !cold_b.contains("other"),
            "the first pass in a new repository does not wait: {cold_b:?}"
        );
        assert!(crate::git_prompt::wait_for_segment_cache_idle(
            std::time::Duration::from_secs(30)
        ));
        let at_b = render_left_of(&shell.prompt_for_display());
        let total_b = crate::git_prompt::read_status_call_count(&key_b);
        eprintln!("issue592 probe cd row (#685): repo_b_spawns={total_b} segment_b={at_b:?}");
        assert_eq!(total_b, 1, "the cd costs exactly one background spawn");
        assert!(at_b.contains("other"), "{at_b:?}");
        assert_eq!(crate::git_prompt::read_status_call_count(&key_a), 1);
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
        shell.session_mut().variables().set(
            "PS0",
            "${STARSHIP_START_TIME:$((STARSHIP_START_TIME=12345,0)):0}",
        );

        assert_eq!(shell.execute_interactive_line(":").unwrap(), 0);

        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("STARSHIP_START_TIME")
                .as_deref(),
            Some("12345")
        );
    }

    /// unixwin/niubash#190: PS0's decoded output must go to the stderr
    /// channel (GNU eval.c:176 writes stderr), with stdout left untouched —
    /// `cmd 2>/dev/null` drops the PS0 bytes while `cmd > file` stays clean.
    #[test]
    fn bash_ps0_expansion_targets_stderr_and_stdout_stays_clean() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.session_mut().variables().set("PS0", "PS0-EXPANDED ");

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

    #[test]
    fn restricted_startup_finishes_before_greeting_and_precmd_hooks() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let temp = unique_temp_dir("niubash-restricted-hooks");
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(temp.join(NIU_RC_FILE), "cd .\nSTARTUP_CD=$?\n").unwrap();
        let mut shell = test_shell(HookConfig {
            greeting: std::rc::Rc::from(vec!["cd .; GREETING_CD=$?".into()]),
            precmd: std::rc::Rc::from(vec!["cd .; PRECMD_CD=$?".into()]),
            ..HookConfig::default()
        });
        shell.home_dir = temp.clone();
        let invocation = rubash::invocation::ShellInvocation::parse(&["-r".into()]).unwrap();
        shell.apply_invocation_before_startup(&invocation).unwrap();
        shell.run_startup_rc();
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("STARTUP_CD")
                .as_deref(),
            Some("0")
        );
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("GREETING_CD")
                .as_deref(),
            Some("1")
        );
        shell.run_precmd_hooks();
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("PRECMD_CD")
                .as_deref(),
            Some("1")
        );
        assert!(shell.startup_invocation.is_none());
        std::fs::remove_dir_all(temp).unwrap();
    }

    /// niubash#422: on the login posix face (`sh -l -i`), an `exit` in the
    /// `$ENV` file ends the startup BEFORE the login tail — greeting hooks
    /// (and the completion sync) must not run after the file already ended
    /// the invocation. GNU 5.3.0 oracle (WSL `sh -l -i`, $ENV = "echo
    /// ENV-RAN; exit 7"): the marker prints, the shell exits 7, the first
    /// prompt never renders (the no-exit control does render it). The same
    /// early return the profile and rc exit paths take.
    #[test]
    fn login_posix_env_exit_skips_the_greeting_startup_tail() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let env_file = temp.path().join("env422.src");
        std::fs::write(&env_file, "export ENV_RAN=$((2+3))\nexit 7\n").unwrap();
        let mut shell = test_shell(HookConfig {
            greeting: std::rc::Rc::from(vec!["export GREETING_RAN=$((6*7))".into()]),
            ..HookConfig::default()
        });
        shell.home_dir = temp.path().to_path_buf();
        // The `sh -l -i` shape: posix mode (invoked as sh) plus the
        // host login classification, REPL dispatch (the greeting tail
        // only runs with repl). rubash#2129: the classification is
        // host state (`own_login_shell`), not the env carrier this
        // test used to plant.
        shell
            .session_mut()
            .variables()
            .set("__RUBASH_POSIX_MODE", "1");
        shell.own_login_shell = true;
        shell
            .executor_mut()
            .set_env("ENV", &env_file.to_string_lossy());
        let exit = shell.run_startup_files(true);
        assert_eq!(
            exit,
            Some(7),
            "the ENV file's exit status is the startup status"
        );
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("ENV_RAN")
                .as_deref(),
            Some("5"),
            "the ENV file itself ran"
        );
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("GREETING_RAN")
                .as_deref(),
            None,
            "no greeting hook may run after the ENV exit jump"
        );
        assert_ne!(
            shell
                .session_mut()
                .variables()
                .resolved_value("NIU_REPL_STARTUP")
                .as_deref(),
            Some("1"),
            "the REPL startup marker must not leak past the jump"
        );
    }

    /// Control for the jump above: the same face WITHOUT the ENV exit still
    /// runs the greeting tail (the guard is the exit jump, not the face).
    #[test]
    fn login_posix_env_without_exit_still_runs_the_greeting_tail() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let env_file = temp.path().join("env422ctl.src");
        std::fs::write(&env_file, "export ENV_RAN=$((2+3))\n").unwrap();
        let mut shell = test_shell(HookConfig {
            greeting: std::rc::Rc::from(vec!["export GREETING_RAN=$((6*7))".into()]),
            ..HookConfig::default()
        });
        shell.home_dir = temp.path().to_path_buf();
        shell
            .session_mut()
            .variables()
            .set("__RUBASH_POSIX_MODE", "1");
        // rubash#2129: host login classification, not the env carrier.
        shell.own_login_shell = true;
        shell
            .executor_mut()
            .set_env("ENV", &env_file.to_string_lossy());
        let exit = shell.run_startup_files(true);
        assert_eq!(exit, None);
        assert_eq!(
            shell
                .session_mut()
                .variables()
                .resolved_value("GREETING_RAN")
                .as_deref(),
            Some("42")
        );
    }

    #[test]
    fn interactive_list_and_alias_exits_record_session_jump() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.enter_interactive();

        // `;`-bearing submissions take the engine's grouped reader route:
        // the grouped driver reports the top-level unwind (niubash R1).
        assert_eq!(shell.execute_interactive_line("true; exit 3").unwrap(), 3);
        assert_eq!(shell.take_session_exit_jump(), Some(3));

        // An alias expanding to `exit` reaches the same reader route.
        shell.execute_interactive_line("alias bye=exit").unwrap();
        assert_eq!(shell.take_session_exit_jump(), None);
        assert_eq!(shell.execute_interactive_line("bye 3").unwrap(), 3);
        assert_eq!(shell.take_session_exit_jump(), Some(3));

        // A plain successful line leaves no jump behind.
        assert_eq!(shell.execute_interactive_line("echo alive").unwrap(), 0);
        assert_eq!(shell.take_session_exit_jump(), None);
    }

    #[test]
    fn interactive_function_exit_jumps_but_subshell_and_echo_do_not() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.enter_interactive();

        // Two-line form: the definition carries no jump, the bare call runs
        // through execute_ast and the body's `exit` unwinds to the top.
        shell.execute_interactive_line("f(){ exit 4; }").unwrap();
        assert_eq!(shell.take_session_exit_jump(), None);
        assert_eq!(shell.execute_interactive_line("f").unwrap(), 4);
        assert_eq!(shell.take_session_exit_jump(), Some(4));

        // GNU's subshell boundary contains the jump (verified bash:
        // `(exit 3)` leaves the interactive session running).
        assert_eq!(shell.execute_interactive_line("(exit 5)").unwrap(), 5);
        assert_eq!(shell.take_session_exit_jump(), None);

        // `echo exit` is just a command whose argument happens to be the
        // word `exit`.
        assert_eq!(shell.execute_interactive_line("echo exit").unwrap(), 0);
        assert_eq!(shell.take_session_exit_jump(), None);
    }

    #[test]
    fn interactive_diagnosed_and_parse_error_shapes_keep_the_session() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.enter_interactive();

        // bash 5.3: `exit abc` reports `numeric argument required` and the
        // command fails with 2 while the shell continues (rubash pins the
        // 5.3 probe D semantics) — no unwind, no session end.
        assert_eq!(shell.execute_interactive_line("exit abc").unwrap(), 2);
        assert_eq!(shell.take_session_exit_jump(), None);

        // GNU CHECK_HELPOPT: `exit --help` prints the help and reports the
        // usage status from the builtin itself, without jumping.
        let code = shell.execute_interactive_line("exit --help").unwrap();
        assert_eq!(code, 2);
        assert_eq!(shell.take_session_exit_jump(), None);

        // An interactive syntax error discards its group only and the
        // session keeps running (eval.c:202-205) — the parse-abort
        // ExitCode(2) must not read as an exit jump.
        let code = shell.execute_interactive_line("echo )bad").unwrap();
        assert_eq!(code, 2);
        assert_eq!(shell.take_session_exit_jump(), None);
    }

    #[test]
    fn interactive_submission_drops_a_stale_session_jump() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());
        shell.enter_interactive();

        // A non-REPL script run (startup re-source shape, niubash#180) may
        // record a jump; the next interactive submission must not inherit it.
        assert_eq!(shell.execute_script("exit 9").unwrap(), 9);
        assert_eq!(shell.session_exit_jump, Some(9));
        assert_eq!(shell.execute_interactive_line("echo hi").unwrap(), 0);
        assert_eq!(shell.take_session_exit_jump(), None);
    }

    pub(super) fn test_shell(hooks: HookConfig) -> Shell {
        let mut session = Session::builder().build().expect("test session builds");
        session.options().set("expand_aliases", true);
        let mut shell = Shell {
            session: Some(session),
            finish_cwd: None,
            completion_state: Arc::new(Mutex::new(CompletionState::new(PathBuf::from(".")))),
            prompt: PromptBackend::Template(NiubashPrompt::new(None, None)),
            gitbash_prompt_active: false,
            vcs_pass_seq: 0,
            ps1_seeded_default: false,
            floor_prompt: PromptBackend::Template(NiubashPrompt::new(None, None)),
            home_dir: PathBuf::from("."),
            shell_root: None,
            history_path: PathBuf::from(".niubash_history"),
            history_max_size: 10000,
            history_ignore_space_prefixed: false,
            history_provider: None,
            history_redirect_armed: false,
            history_redirect_pending_editor_rebuild: false,
            menu_config: MenuConfig::default(),
            editor_mode: EditorMode::Emacs,
            command_not_found_hint: CommandNotFoundHint::Off,
            edit_flags_seen: None,
            autosuggest: AutosuggestConfig::default(),
            syntax_highlighting: SyntaxHighlightConfig::default(),
            native_widgets: NativeWidgetConfig::default(),
            native_widget_bindings: Vec::new(),
            user_widget_bindings: Vec::new(),
            engine_bindings: Vec::new(),
            user_keymap_rules: Vec::new(),
            user_keymap_unbinds: Vec::new(),
            engine_bind_generation: 0,
            compdefs: Vec::new(),
            config_completion_dirs: Vec::new(),
            hooks,
            aliases: HashMap::new(),
            last_interactive_command: None,
            last_interactive_exit_code: None,
            line_editor: None,
            process_stdin_pipeline_bridge: false,
            bash_prompt_command_running: false,
            interactive_lines_read: 0,
            interactive: false,
            no_rc: false,
            no_profile: false,
            rc_file: None,
            no_editing: false,
            startup_invocation: None,
            own_login_shell: false,
            respawned_child: false,
            invoked_as_rbash: false,
            script_exit_jump: false,
            session_exit_jump: None,
            one_shot: false,
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

    pub(super) struct CwdGuard {
        previous: PathBuf,
    }

    impl CwdGuard {
        pub(super) fn capture() -> Self {
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

#[cfg(test)]
mod invocation_name_tests {
    use super::tests::{test_shell, CwdGuard};
    use super::{default_script_name_from, HookConfig};
    use crate::test_support::PROCESS_STATE_LOCK;

    #[test]
    fn sh_shim_forward_yields_sh_as_dollar_zero() {
        // The sh.exe shim forwards its argv[0] stem verbatim.
        assert_eq!(default_script_name_from(Some("sh".to_string())), "sh");
        assert_eq!(
            default_script_name_from(Some("C:/x/winuxcmd/bin/sh.exe".to_string())),
            "sh"
        );
        assert_eq!(
            default_script_name_from(Some("\\usr\\bin\\ASH.EXE".to_string())),
            "ash"
        );
    }

    #[test]
    fn login_marker_survives_into_dollar_zero() {
        // GNU shell.c set_shell_name keeps the leading `-': a login `sh'
        // shows $0 = -sh.
        assert_eq!(default_script_name_from(Some("-sh".to_string())), "-sh");
        assert_eq!(default_script_name_from(Some("-SH.exe".to_string())), "-sh");
    }

    #[test]
    fn bash_and_rbash_faces_report_the_invocation_name() {
        // rubash#598 companion (niubash#321): the shim forwards the
        // bash/rbash stems too, and GNU set_shell_name binds $0 to argv[0]
        // for every face — `bash -c 'echo $0'` must read `bash`, not niu.
        assert_eq!(default_script_name_from(Some("bash".to_string())), "bash");
        assert_eq!(
            default_script_name_from(Some("C:/x/winuxcmd/bin/bash.exe".to_string())),
            "bash"
        );
        assert_eq!(default_script_name_from(Some("rbash".to_string())), "rbash");
        assert_eq!(
            default_script_name_from(Some("-BASH.EXE".to_string())),
            "-bash"
        );
    }

    #[test]
    fn plain_niu_and_unknown_stems_keep_the_niu_default() {
        // A direct niu.exe run (or any stem the shim never forwards) keeps
        // the historical default byte-for-byte.
        assert_eq!(default_script_name_from(None), "niu");
        assert_eq!(default_script_name_from(Some("niu".to_string())), "niu");
        assert_eq!(default_script_name_from(Some("mysh".to_string())), "niu");
        assert_eq!(
            default_script_name_from(Some("C:/x/bin/niubash.exe".to_string())),
            "niu"
        );
    }

    #[test]
    fn invoked_as_is_consumed_from_the_process_environment() {
        // The variable describes how THIS process was invoked, nothing a
        // child should inherit: reading it removes it, so a `niu` or `bash`
        // spawned later under `sh -c` starts clean and keeps its own
        // argv[0]-keyed GNU contract.
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("NIU_INVOKED_AS");
        std::env::set_var("NIU_INVOKED_AS", "sh");
        assert_eq!(super::invoked_as_from_env(), Some("sh".to_string()));
        assert_eq!(std::env::var_os("NIU_INVOKED_AS"), None);
        // A consumed variable stays consumed; reading again yields None
        // without touching the environment a second time.
        assert_eq!(super::invoked_as_from_env(), None);
        match previous {
            Some(value) => std::env::set_var("NIU_INVOKED_AS", value),
            None => std::env::remove_var("NIU_INVOKED_AS"),
        }
    }

    /// niubash#627 end-to-end (Windows, where the namespaces are
    /// simulated): the completion probe — the same state the REPL's
    /// Tab reads — serves the registry's /dev names, the registry's
    /// /proc enumeration, and the live fd table under /dev/fd/.
    #[cfg(windows)]
    #[test]
    fn issue627_completion_probe_serves_simulated_namespaces() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        // Static tier: /dev/ lists the registry's simulated names.
        let dev = shell.completion_probe("cat /dev/", "cat /dev/".len());
        for expected in ["/dev/null", "/dev/stdin", "/dev/fd/", "/dev/tcp/"] {
            assert!(
                dev.contains(&expected.to_string()),
                "{dev:?} missing {expected}"
            );
        }

        // /proc/ enumerates through the registry's proc face.
        let proc_list = shell.completion_probe("cat /proc/", "cat /proc/".len());
        assert!(
            proc_list.contains(&"/proc/meminfo".to_string()),
            "{proc_list:?}"
        );
        assert!(
            proc_list.contains(&"/proc/self/".to_string()),
            "{proc_list:?}"
        );

        // Dynamic tier: the fd table decides — fd 3 appears once
        // opened and disappears once closed.
        let before = shell.completion_probe("cat /dev/fd/", "cat /dev/fd/".len());
        assert!(!before.contains(&"/dev/fd/3".to_string()), "{before:?}");
        shell.execute_interactive_line("exec 3>/dev/null").unwrap();
        let opened = shell.completion_probe("cat /dev/fd/", "cat /dev/fd/".len());
        assert!(opened.contains(&"/dev/fd/3".to_string()), "{opened:?}");
        shell.execute_interactive_line("exec 3>&-").unwrap();
        let closed = shell.completion_probe("cat /dev/fd/", "cat /dev/fd/".len());
        assert!(!closed.contains(&"/dev/fd/3".to_string()), "{closed:?}");

        // Net dirs complete nothing past the guide form.
        let net = shell.completion_probe("cat /dev/tcp/", "cat /dev/tcp/".len());
        assert!(net.is_empty(), "{net:?}");
    }

    /// niubash#627 G2 end-to-end (Windows, where /tmp is the enclave
    /// namespace): a session `TMPDIR` assignment moves /tmp
    /// completion to the new backing directory on the very next
    /// probe — the readers follow the session value the engine
    /// resolves, not the process environment they used to read. A
    /// subshell assignment does not leak back, and unsetting TMPDIR
    /// returns completion to the process temp fallback.
    #[cfg(windows)]
    #[test]
    fn issue627_g2_completion_follows_session_tmpdir() {
        struct EnvGuard {
            name: &'static str,
            previous: Option<std::ffi::OsString>,
        }
        impl EnvGuard {
            fn set(name: &'static str, value: &std::path::Path) -> Self {
                let previous = std::env::var_os(name);
                std::env::set_var(name, value);
                Self { name, previous }
            }
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.previous {
                    Some(value) => std::env::set_var(self.name, value),
                    None => std::env::remove_var(self.name),
                }
            }
        }

        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cwd_guard = CwdGuard::capture();
        let mut shell = test_shell(HookConfig::default());

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scratch =
            std::env::temp_dir().join(format!("niubash-g2-shell-{}-{}", std::process::id(), nanos));
        let dir_a = scratch.join("a");
        let dir_b = scratch.join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        // The enclave resolver only answers with a shell root
        // configured (a real niu process always exports its own);
        // point it at a stub root for the test.
        let stub_root = scratch.join("root");
        std::fs::create_dir_all(&stub_root).unwrap();
        let _root_guard = EnvGuard::set("NIU_ROOT", &stub_root);
        std::fs::write(dir_a.join("g2alpha.txt"), "x").unwrap();
        std::fs::write(dir_b.join("g2beta.txt"), "x").unwrap();
        let slash = |dir: &std::path::Path| dir.to_string_lossy().replace('\\', "/");

        shell
            .execute_interactive_line(&format!("TMPDIR={}", slash(&dir_a)))
            .unwrap();
        let got = shell.completion_probe("cat /tmp/g2", "cat /tmp/g2".len());
        assert!(got.contains(&"/tmp/g2alpha.txt".to_string()), "{got:?}");
        assert!(!got.contains(&"/tmp/g2beta.txt".to_string()), "{got:?}");

        // Repointing the session value moves completion immediately.
        shell
            .execute_interactive_line(&format!("TMPDIR={}", slash(&dir_b)))
            .unwrap();
        let got = shell.completion_probe("cat /tmp/g2", "cat /tmp/g2".len());
        assert!(got.contains(&"/tmp/g2beta.txt".to_string()), "{got:?}");
        assert!(!got.contains(&"/tmp/g2alpha.txt".to_string()), "{got:?}");

        // A subshell assignment is scoped to the subshell (GNU
        // inheritance shape): the session value — and completion —
        // stay where the parent session put them.
        shell
            .execute_interactive_line(&format!("( TMPDIR={} )", slash(&dir_a)))
            .unwrap();
        let got = shell.completion_probe("cat /tmp/g2", "cat /tmp/g2".len());
        assert!(got.contains(&"/tmp/g2beta.txt".to_string()), "{got:?}");
        assert!(!got.contains(&"/tmp/g2alpha.txt".to_string()), "{got:?}");

        // Unsetting TMPDIR falls back to the process temp dir.
        shell.execute_interactive_line("unset TMPDIR").unwrap();
        let fallback_name = format!("g2fallback-{}.txt", std::process::id());
        let fallback_file = std::env::temp_dir().join(&fallback_name);
        std::fs::write(&fallback_file, "x").unwrap();
        let input = format!("cat /tmp/{}", &fallback_name[..fallback_name.len() - 4]);
        let got = shell.completion_probe(&input, input.len());
        assert!(got.contains(&format!("/tmp/{fallback_name}")), "{got:?}");
        let _ = std::fs::remove_file(fallback_file);
        let _ = std::fs::remove_dir_all(scratch);
    }

    /// niubash#627 G2 (non-Windows): the synced tmp base is whatever
    /// the ENGINE resolves — off Windows there is no enclave, so a
    /// session TMPDIR does not move it. The host takes the engine's
    /// answer through the Session seam instead of inventing its own
    /// TMPDIR read (the pre-G2 copies diverged exactly that way).
    #[cfg(not(windows))]
    #[test]
    fn issue627_g2_tmp_base_is_the_engine_answer_off_windows() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut shell = test_shell(HookConfig::default());
        shell
            .execute_interactive_line("TMPDIR=/definitely/not/the/base")
            .unwrap();
        shell.update_completion_state();
        let state = shell.completion_state.lock().unwrap();
        assert_eq!(state.tmp_base, std::path::PathBuf::from("/tmp"));
    }
}
