// Custom completer for WinSH
// Integrates command, path, and variable completion

use crate::completion::command::CommandCompleter;
use crate::completion::external::{CommandCompletionPlugin, CommandDef, ExternalCompletionPlugin};
use crate::completion::path::{
    entry_is_executable, uncached_listing, DirCacheHandle, DirListingCache, ListedKind,
    PathCompleter, SessionKey,
};
use crate::completion::variables::VariableCompleter;
use crate::completion::{
    CompletionBehavior, CompletionContext, CompletionPlugin, CompletionResult,
};
use reedline::{Completer, CompletionResult as ReedlineCompletionResult, Span, Suggestion};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Live-PATH probe memo shared with the syntax highlighter (niubash#574).
///
/// Command names absent from the highlighter's startup snapshot are probed
/// against the session's current PATH and remembered here. Entries are keyed
/// by an environment fingerprint (PATH + PATHEXT + the serialized `hash`
/// table); a fingerprint change — a PATH assignment or any `hash` mutation —
/// retires the memo instead of serving stale answers. The shell also clears
/// the memo explicitly after `hash` / `wpm` command lines, since a package
/// install changes the filesystem without touching any fingerprinted env.
#[derive(Debug, Default)]
pub struct CommandProbe {
    fingerprint: Option<String>,
    results: HashMap<String, bool>,
    probes: usize,
}

impl CommandProbe {
    /// The memoized answer for `command`, or `None` when the name was never
    /// probed under the current fingerprint. A fingerprint mismatch retires
    /// every remembered answer.
    pub fn lookup(&mut self, fingerprint: &str, command: &str) -> Option<bool> {
        self.retire_stale(fingerprint);
        self.results.get(command).copied()
    }

    /// Record a probe outcome. The probe ran under this fingerprint, so a
    /// fingerprint that moved between `lookup` and `store` drops the result
    /// rather than memoizing an answer the current env never produced.
    pub fn store(&mut self, fingerprint: &str, command: &str, known: bool) {
        if self.fingerprint.as_deref() == Some(fingerprint) {
            self.results.insert(command.to_string(), known);
        }
    }

    /// Forget every remembered answer (explicit invalidation hook).
    pub fn clear(&mut self) {
        self.results.clear();
    }

    /// Number of live probes performed — memoization evidence for tests.
    pub fn probe_count(&self) -> usize {
        self.probes
    }

    /// Count one live probe (one filesystem walk for one command name).
    pub fn note_probe(&mut self) {
        self.probes += 1;
    }

    fn retire_stale(&mut self, fingerprint: &str) {
        if self.fingerprint.as_deref() == Some(fingerprint) {
            return;
        }
        self.fingerprint = Some(fingerprint.to_string());
        self.results.clear();
    }
}

/// State shared with completer: the host-owned faces only. The
/// engine-owned inputs (variable table, function names, open
/// fds) are NOT mirrored here — since GAP-4 (niubash#863) the
/// completer captures them live from `Session::completion_view`.
/// What remains is host-owned: the fallback directory and `/tmp`
/// base, the resolved behavior, the plugin table, the probe
/// memo, and the alias names (host alias map is their source).
pub struct CompletionState {
    pub current_dir: PathBuf,
    pub aliases: HashSet<String>,
    /// The backing directory for the session's `/tmp`, resolved by
    /// the engine from the session env (session TMPDIR wins) and
    /// refreshed by `Shell::update_completion_state`; the path
    /// completer and the highlighter root `/tmp` spellings here
    /// (niubash#627 G2). Starts at the process temp dir, the
    /// pre-sync fallback. The resolver face is GAP-5's object.
    pub tmp_base: PathBuf,
    pub behavior: CompletionBehavior,
    /// Registered completion plugins (e.g. command completion, external tool completion)
    pub plugins: Vec<Arc<dyn CompletionPlugin>>,
    /// Live-probe memo consulted by the syntax highlighter (niubash#574).
    pub command_probe: CommandProbe,
    /// User completion directories already loaded into the external plugin,
    /// kept so the external plugin can be rebuilt when bundle definitions
    /// change without losing user overrides.
    completion_dirs: Vec<PathBuf>,
    /// niubash#747: session-scoped directory-listing cache shared by
    /// every query this state serves. Each query borrows it through a
    /// `DirCacheHandle` stamped with the query's session key, so the
    /// path completer and the cwd command-slot overlay enumerate a
    /// directory at most once per completion session instead of
    /// re-walking (and re-statting) it per keystroke.
    pub dir_cache: Arc<Mutex<DirListingCache>>,
}

impl CompletionState {
    pub fn new(current_dir: PathBuf) -> Self {
        Self {
            current_dir,
            aliases: HashSet::new(),
            tmp_base: std::env::temp_dir(),
            behavior: CompletionBehavior::default(),
            plugins: Vec::new(),
            command_probe: CommandProbe::default(),
            completion_dirs: Vec::new(),
            dir_cache: Arc::new(Mutex::new(DirListingCache::default())),
        }
    }

    /// Register a completion plugin
    pub fn add_plugin(&mut self, plugin: Arc<dyn CompletionPlugin>) {
        plugin.on_init();
        self.plugins.push(plugin);
    }

    /// Load completion definitions from a list of directories.
    /// Each directory is scanned for `<cmd>.toml` and `<cmd>.bash` files.
    /// Also registers the `CommandCompletionPlugin` if not already present.
    pub fn load_completion_dirs(&mut self, dirs: &[PathBuf]) {
        self.load_completion_dirs_with_definitions(dirs, Vec::new());
    }

    /// Load translated definitions before user directories, so native TOML
    /// files remain the highest-priority override surface.
    pub fn load_completion_dirs_with_definitions(
        &mut self,
        dirs: &[PathBuf],
        definitions: Vec<CommandDef>,
    ) {
        self.load_completion_dirs_with_bundle_and_definitions(dirs, Vec::new(), definitions);
    }

    /// Load official bundle definitions over compiled fallback, then translated
    /// compatibility definitions, then user directories as the final override.
    pub fn load_completion_dirs_with_bundle_and_definitions(
        &mut self,
        dirs: &[PathBuf],
        bundle_definitions: Vec<CommandDef>,
        definitions: Vec<CommandDef>,
    ) {
        let has_command_plugin = self
            .plugins
            .iter()
            .any(|p| p.name() == "command-completion");
        if !has_command_plugin {
            self.add_plugin(Arc::new(CommandCompletionPlugin));
        }
        let mut external = ExternalCompletionPlugin::new();
        external.replace_definitions(bundle_definitions);
        external.load_definitions(definitions);
        for dir in dirs {
            external.load_dir(dir);
        }
        self.completion_dirs = dirs.to_vec();
        // Replace an existing external plugin in place so a second load (the
        // rc-declared NIU_COMPDEFS directories after Shell::new seeded the
        // config layer, issue #306) rebuilds the same slot instead of
        // stacking a duplicate plugin that would emit every candidate twice.
        if let Some(slot) = self.plugins.iter_mut().rev().find(|p| {
            p.as_any()
                .downcast_ref::<ExternalCompletionPlugin>()
                .is_some()
        }) {
            *slot = Arc::new(external);
        } else {
            self.add_plugin(Arc::new(external));
        }
    }

    /// Collect command names known to loaded completion plugins.
    ///
    /// External TOML definitions (from oh-my-niu, user overrides, etc.)
    /// register command names that the syntax highlighter should treat as
    /// valid even when the binary is extensionless and not in the
    /// compiled-in command set.
    pub fn plugin_command_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for plugin in &self.plugins {
            // ExternalCompletionPlugin exposes definition_names()
            // via downcast. CommandCompletionPlugin has no extra names.
            if let Some(ext) = plugin.as_any().downcast_ref::<ExternalCompletionPlugin>() {
                names.extend(ext.definition_names().into_iter().map(str::to_string));
            }
        }
        names
    }
}

/// One of GNU bash's context-specific completion entry points
/// (bashline.c `bash_complete_command` / `bash_complete_variable` /
/// `bash_complete_filename`, installed by `rl_add_defun` at
/// bashline.c:504-514). Each uses the same completion engine as TAB
/// but pins the candidate source instead of letting the cursor's
/// position choose it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecificCompletion {
    /// `complete-command` — command names (builtins, PATH, aliases,
    /// functions), `command_word_completion_function` (bashline.c:2005).
    Command,
    /// `complete-variable` — variable names, `variable_completion_function`
    /// (bashline.c:2568).
    Variable,
    /// `complete-filename` — filenames, `rl_filename_completion_function`
    /// (complete.c), with the filename word-break set bash installs for
    /// this entry point (bashline.c:3914).
    Filename,
    /// `complete-username` — user names, `rl_username_completion_function`
    /// (complete.c:2377, `getpwent`; see [`crate::completion::sources`]).
    Username,
    /// `complete-hostname` — host names, `hostname_completion_function`
    /// (bashline.c:2626, `/etc/hosts`).
    Hostname,
    /// `complete-into-braces` — filename matches compressed into one
    /// csh-style brace expression (bashline.c:3795 `bash_brace_completion`,
    /// bracecomp.c).
    Brace,
    /// `glob-complete-word` — bashline.c:4068 `bash_glob_complete_word`
    /// globs the word (with `*` appended, as `rl_explicit_arg` does in
    /// emacs mode) through `shell_glob_filename`, then completes against
    /// those matches through the ordinary completion machinery.
    Glob,
}

/// The host-spelled faces of one completion call, captured by
/// `Shell::completion_host_faces` before the attempt opens
/// (GAP-7, niubash#863): the current directory in host form, the
/// `/tmp` base, the resolved behavior, the alias names, and the
/// compdef registry. The engine-owned faces are NOT here — the
/// pipeline reads them live through the open attempt's
/// `session()` for the attempt's duration (the GAP-4 capture's
/// `view` component is exactly that read).
pub(crate) struct CompletionHostFaces {
    pub current_dir: PathBuf,
    pub tmp_base: PathBuf,
    pub behavior: CompletionBehavior,
    pub aliases: HashSet<String>,
    pub compdefs: Vec<(String, String)>,
}

/// One open completion attempt plus the host faces captured for
/// it — the pipeline's engine source while the attempt lives.
/// Drivers (GAP-7, niubash#863) open the guard through
/// `Session::begin_completion_attempt` and hand it down; every
/// engine read and execution of the call routes through it.
pub(crate) struct AttemptRun<'a, 's> {
    pub attempt: &'a mut rubash::api::CompletionAttempt<'s>,
    pub faces: &'a CompletionHostFaces,
}

/// The inputs of one completion call. Without an attempt the
/// engine-owned inputs are empty — what the attempt-dependent
/// faces already observed in that shape, now extended to the
/// variable / function / fd faces the retired snapshot used to
/// keep serving there (GAP-4, niubash#863).
struct CompletionInputs {
    current_dir: PathBuf,
    env_vars: HashMap<String, String>,
    aliases: HashSet<String>,
    functions: HashSet<String>,
    open_fds: Vec<u32>,
    tmp_base: PathBuf,
    behavior: CompletionBehavior,
    plugins: Vec<Arc<dyn CompletionPlugin>>,
    dir_cache: Arc<Mutex<DirListingCache>>,
}

/// Custom completer for WinSH
pub struct NiubashCompleter {
    state: Arc<Mutex<CompletionState>>,
    /// GAP-7 (niubash#863): the editor flavor's handle on the
    /// REPL's shell. A completer carrying it opens one
    /// `Session::begin_completion_attempt` guard per `complete` /
    /// `complete_specific` call (through `Shell::drive_completion`)
    /// and routes every engine read and execution of the call
    /// through that guard. `None` for standalone completers (unit
    /// probes, one-shot diagnostics): their engine-owned inputs
    /// stay empty — exactly what the retired thread-local bridge's
    /// absence produced on those surfaces.
    shell: Option<crate::shell::EditorShellHandle>,
}

impl NiubashCompleter {
    /// Create a new completer with shared state
    pub fn new(state: Arc<Mutex<CompletionState>>) -> Self {
        Self { state, shell: None }
    }

    /// Create a completer that drives completion attempts on the
    /// REPL's shell (GAP-7, niubash#863): the line editor's
    /// instances and the REPL's specific-completion driver. The
    /// handle's safety contract is documented on
    /// `EditorShellHandle`.
    pub fn with_shell(
        state: Arc<Mutex<CompletionState>>,
        shell: std::rc::Rc<std::cell::RefCell<crate::shell::Shell>>,
    ) -> Self {
        Self {
            state,
            shell: Some(crate::shell::EditorShellHandle::new(shell)),
        }
    }

    /// Assemble the inputs of one completion call: host-owned
    /// faces from the shared state (niubash#833 poison recovery);
    /// with an attempt open, the engine-owned faces from the
    /// attempt's live `Session::completion_view` and the host
    /// faces captured for the attempt (GAP-7, niubash#863 — the
    /// GAP-4 bridge capture's two halves, taken from their owners).
    fn capture_inputs(
        &self,
        engine: Option<(&rubash::api::CompletionAttempt<'_>, &CompletionHostFaces)>,
    ) -> CompletionInputs {
        let mut inputs = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            CompletionInputs {
                current_dir: state.current_dir.clone(),
                env_vars: HashMap::new(),
                aliases: state.aliases.clone(),
                functions: HashSet::new(),
                open_fds: Vec::new(),
                tmp_base: state.tmp_base.clone(),
                behavior: state.behavior,
                plugins: state.plugins.clone(),
                dir_cache: state.dir_cache.clone(),
            }
        };
        if let Some((attempt, faces)) = engine {
            let view = attempt.session().completion_view();
            inputs.current_dir = faces.current_dir.clone();
            inputs.env_vars = view.env.into_iter().collect();
            inputs.aliases = faces.aliases.clone();
            inputs.functions = view.functions.into_iter().collect();
            inputs.open_fds = view.open_fds;
            inputs.tmp_base = faces.tmp_base.clone();
            inputs.behavior = faces.behavior;
        }
        inputs
    }

    /// Return plain suggestions for internal probes and tests.
    ///
    /// A completer carrying the editor shell handle drives the
    /// call through one completion attempt on that shell; a
    /// standalone completer runs the same pipeline with no engine
    /// source.
    pub fn complete(&mut self, input: &str, cursor_pos: usize) -> Vec<Suggestion> {
        if let Some(handle) = self.shell.clone() {
            return handle.with_shell_mut(|shell| shell.drive_completion(self, input, cursor_pos));
        }
        self.complete_input(None, input, cursor_pos)
    }

    /// Complete through a caller-opened attempt (GAP-7,
    /// niubash#863): the driver owns the guard — the probe surface
    /// and nested-completion drivers — and every engine read and
    /// execution of this call routes through it.
    pub(crate) fn complete_with_attempt(
        &mut self,
        attempt: &mut rubash::api::CompletionAttempt<'_>,
        faces: &CompletionHostFaces,
        input: &str,
        cursor_pos: usize,
    ) -> Vec<Suggestion> {
        self.complete_input(Some(AttemptRun { attempt, faces }), input, cursor_pos)
    }

    /// Look up `name` in a session env snapshot, case-insensitively:
    /// Windows session envs carry `Path`/`PATHEXT` under varying
    /// casings, and the snapshot preserves the stored spelling.
    fn session_env_value(
        env_vars: &std::collections::HashMap<String, String>,
        name: &str,
    ) -> Option<String> {
        env_vars
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }

    /// Complete INPUT at CURSOR_POS with one pinned GNU completion
    /// source (see [`SpecificCompletion`]). This bypasses
    /// `complete_input`'s position-driven source choice only; every
    /// candidate comes from the same real completer/plugin table the
    /// TAB path uses, and the returned suggestions carry the same
    /// word span and whitespace policy.
    pub fn complete_specific(
        &mut self,
        mode: SpecificCompletion,
        input: &str,
        cursor_pos: usize,
    ) -> Vec<Suggestion> {
        if let Some(handle) = self.shell.clone() {
            return handle.with_shell_mut(|shell| {
                shell.drive_completion_specific(self, mode, input, cursor_pos)
            });
        }
        self.complete_specific_with_engine(None, mode, input, cursor_pos)
    }

    /// [`Self::complete_specific`] through a caller-opened attempt
    /// (GAP-7, niubash#863); see [`Self::complete_with_attempt`].
    pub(crate) fn complete_specific_with_attempt(
        &mut self,
        attempt: &mut rubash::api::CompletionAttempt<'_>,
        faces: &CompletionHostFaces,
        mode: SpecificCompletion,
        input: &str,
        cursor_pos: usize,
    ) -> Vec<Suggestion> {
        self.complete_specific_with_engine(
            Some(AttemptRun { attempt, faces }),
            mode,
            input,
            cursor_pos,
        )
    }

    fn complete_specific_with_engine(
        &mut self,
        mut engine: Option<AttemptRun<'_, '_>>,
        mode: SpecificCompletion,
        input: &str,
        cursor_pos: usize,
    ) -> Vec<Suggestion> {
        // GAP-7 (niubash#863): the engine-owned inputs are captured
        // live through the open attempt, not read from a per-line
        // snapshot; the host-owned faces come from the shared state
        // (with niubash#833 poison recovery, as before).
        let CompletionInputs {
            current_dir,
            env_vars,
            aliases,
            functions,
            open_fds,
            tmp_base,
            behavior,
            dir_cache,
            ..
        } = self.capture_inputs(engine.as_ref().map(|run| (&*run.attempt, run.faces)));

        let cursor_pos = cursor_pos.min(input.len());
        let mut context = CompletionContext::with_behavior(
            current_dir.clone(),
            input.to_string(),
            cursor_pos,
            behavior,
        );
        // GAP-9 (niubash#863): the context's lexical methods read
        // the token view of the attempt's session (derived in its
        // posix mode); a session-less call leaves the field unset
        // and the methods derive the view from the completion
        // module's stateless oracle instead.
        if let Some(run) = engine.as_ref() {
            context.token_view = Some(run.attempt.session().token_view(input));
        }
        context.open_fds = open_fds;
        context.tmp_base = tmp_base;
        context.file_colors = Some(crate::completion::FileColors::from_env_vars(&env_vars));
        // niubash#839/#845: producers resolving tools must see the
        // session's PATH/PATHEXT (the snapshot this context was built
        // from), not the process env.
        context.session_path = Self::session_env_value(&env_vars, "PATH");
        context.session_pathext = Self::session_env_value(&env_vars, "PATHEXT");
        context.dir_cache = Some(DirCacheHandle::with_shared(
            dir_cache,
            SessionKey::new(current_dir, String::new()),
        ));

        let result =
            match mode {
                SpecificCompletion::Command => {
                    let word = context.get_current_word().unwrap_or_default();
                    let mut matches = CommandCompleter::get_all_commands();
                    matches.extend(aliases);
                    matches.extend(functions);
                    matches.sort();
                    matches.dedup();
                    matches.retain(|command| context.behavior.matches(command, &word));
                    (!matches.is_empty()).then(|| CompletionResult::new(matches))
                }
                SpecificCompletion::Variable => {
                    let word = context.get_current_word().unwrap_or_default();
                    VariableCompleter::complete_names(&word, &env_vars)
                }
                SpecificCompletion::Filename => PathCompleter::complete(&context).ok().flatten(),
                SpecificCompletion::Username => {
                    let word = context.get_current_word().unwrap_or_default();
                    crate::completion::sources::complete_username(&word)
                }
                SpecificCompletion::Hostname => {
                    let word = context.get_current_word().unwrap_or_default();
                    crate::completion::sources::complete_hostname(&word)
                }
                // bracecomp.c `bash_brace_completion`: complete filenames
                // first, then collapse the raw match list into one brace
                // expression. GNU turns off filename quoting here and quotes
                // each inserted substring itself, so undo the path
                // completer's shell quoting before compressing.
                SpecificCompletion::Brace => PathCompleter::complete(&context)
                    .ok()
                    .flatten()
                    .and_then(|result| {
                        let mut names: Vec<String> = result
                            .completions
                            .iter()
                            .map(|value| crate::completion::path::unescape_shell_path(value))
                            .collect();
                        names.sort();
                        names.dedup();
                        crate::completion::brace::compress_braces(&names)
                            .map(|compressed| CompletionResult::new(vec![compressed]))
                    }),
                // bashline.c:4068 `bash_glob_complete_word`: in emacs mode
                // `rl_explicit_arg = 1` makes `glob_complete_word` append a
                // `*` to the word before `shell_glob_filename`, then the
                // ordinary `rl_complete_internal` machinery completes
                // against the resulting file names. The engine's pathname
                // expansion is the glob source; no second glob engine.
                SpecificCompletion::Glob => {
                    let word = context.get_current_word().unwrap_or_default();
                    let pattern = format!("{word}*");
                    let matches = engine
                        .as_mut()
                        .map(|run| run.attempt.session().glob_filename(&pattern))
                        .unwrap_or_default();
                    (!matches.is_empty()).then(|| CompletionResult::new(matches))
                }
            };

        match result {
            Some(result) => self.format_completions_for(result, &context),
            None => Vec::new(),
        }
    }

    /// Complete input
    fn complete_input(
        &mut self,
        mut engine: Option<AttemptRun<'_, '_>>,
        input: &str,
        cursor_pos: usize,
    ) -> Vec<Suggestion> {
        // GAP-7 (niubash#863): live capture through the open
        // attempt, as in `complete_specific` (niubash#833 poison
        // recovery for the host-owned faces lives in
        // `capture_inputs`).
        let CompletionInputs {
            current_dir,
            env_vars,
            aliases,
            functions,
            open_fds,
            tmp_base,
            behavior,
            plugins,
            dir_cache,
        } = self.capture_inputs(engine.as_ref().map(|run| (&*run.attempt, run.faces)));

        let mut context = CompletionContext::with_behavior(
            current_dir.clone(),
            input.to_string(),
            cursor_pos,
            behavior,
        );
        // GAP-9 (niubash#863): the context's lexical methods read
        // the token view of the attempt's session (derived in its
        // posix mode); a session-less call leaves the field unset
        // and the methods derive the view from the completion
        // module's stateless oracle instead.
        if let Some(run) = engine.as_ref() {
            context.token_view = Some(run.attempt.session().token_view(input));
        }
        context.open_fds = open_fds;
        context.tmp_base = tmp_base;
        // niubash#747: lend the query the session's listing cache.
        // The session key is this query's current directory plus the
        // line up to the word being completed — stable while the user
        // types/filters inside the word, retired when a new word or
        // command line starts.
        let word_start = context
            .current_word_span()
            .map(|(start, _)| start)
            .unwrap_or_else(|| {
                crate::completion::floor_char_boundary(input, cursor_pos.min(input.len()))
            });
        let line_stem = input[..word_start].to_string();
        context.dir_cache = Some(DirCacheHandle::with_shared(
            dir_cache,
            SessionKey::new(current_dir, line_stem),
        ));
        // niubash#743: the session's LS_COLORS table, resolved per
        // completion call so a fresh `export LS_COLORS=…` takes effect
        // on the next TAB without an editor rebuild.
        context.file_colors = Some(crate::completion::FileColors::from_env_vars(&env_vars));
        // niubash#839/#845: producers resolving tools must see the
        // session's PATH/PATHEXT (the snapshot this context was built
        // from), not the process env.
        context.session_path = Self::session_env_value(&env_vars, "PATH");
        context.session_pathext = Self::session_env_value(&env_vars, "PATHEXT");
        let mut all_suggestions = Vec::new();

        // GNU programmable completion owns a command's arguments whenever a
        // compspec is registered (`complete`, bash-completion, an inputrc
        // rule). Decide that once, up front: the static definition tables
        // and the path fallback are overlays for commands WITHOUT a
        // compspec, and must not be merged into its candidates.
        // niubash#830: probe the compspec for the command word of the
        // cursor's segment, not the first whitespace token of the
        // whole line — on 'true; git <TAB>' the old probe asked about
        // "true;", missed git's compspec, and let the static tables
        // answer with the wrong candidates. `get_command_name` is
        // quote/extglob-aware and skips grouping openers and
        // assignment prefixes ('( git ', 'VAR=1 git ').
        let command_word = context.get_command_name().unwrap_or_default();
        let has_compspec = !context.is_command_position()
            && !command_word.is_empty()
            && engine
                .as_ref()
                .is_some_and(|run| run.attempt.session().has_compspec(&command_word));

        // Shell-function completions (`NIU_COMPDEFS`): only available
        // while a completion attempt is open on the shell. Skipped
        // when a compspec owns the command (niubash#836): GNU precedence
        // gives the compspec the command's arguments outright, and
        // merging the compdef's candidates into its set produced
        // silently wrong candidate lists.
        if !has_compspec {
            if let Some(result) = compdef_suggestions(&context, engine.as_mut()) {
                all_suggestions.extend(self.format_completions_for(result, &context));
            }
        }

        // At command position, also surface matching entries from the current
        // working directory ahead of PATH command matches. This mirrors the
        // Windows-shell expectation that `win` in a folder containing
        // `./niubash/` should offer `./niubash/` first, instead of only showing
        // PATH executables like `winver` or `winrm`. Only entries that could
        // actually run from the command slot qualify — directories and
        // executable files (niubash#671); plain data files are not commands
        // and readline/GNU never offer them here.
        let cwd_path_suggestions = self.cwd_path_suggestions_at_command_position(&context);
        let alias_suggestions = self.alias_suggestions_at_command_position(&context, &aliases);
        let function_suggestions =
            self.function_suggestions_at_command_position(&context, &functions);

        // Try each plugin in order; only the first non-None result is used.
        // Skipped when a compspec owns the command (GNU precedence).
        for plugin in plugins.iter().filter(|_| !has_compspec) {
            if let Some(result) = plugin.complete(&context) {
                // Found a result, format it
                let formatted = self.format_completions_for(result, &context);
                all_suggestions.extend(formatted);
            }
        }

        // Current-directory paths take priority over PATH commands so users can
        // complete local files and directories without typing `./` first.
        if !cwd_path_suggestions.is_empty() {
            let mut combined = cwd_path_suggestions;
            combined.extend(alias_suggestions.clone());
            combined.extend(function_suggestions.clone());
            combined.extend(all_suggestions);
            all_suggestions = combined;
        } else if !alias_suggestions.is_empty() {
            let mut combined = alias_suggestions;
            combined.extend(function_suggestions.clone());
            combined.extend(all_suggestions);
            all_suggestions = combined;
        } else if !function_suggestions.is_empty() {
            let mut combined = function_suggestions;
            combined.extend(all_suggestions);
            all_suggestions = combined;
        }

        // Fallback to built-in path/variable/command completers
        if all_suggestions.is_empty() {
            if context.is_variable_completion() {
                if let Ok(Some(result)) = VariableCompleter::complete(&context, &env_vars) {
                    return self.format_completions_for(result, &context);
                }
            }
            if context.is_path_completion() && !has_compspec {
                if let Ok(Some(result)) = PathCompleter::complete(&context) {
                    all_suggestions.extend(self.format_completions_for(result, &context));
                }
            }
        }

        // GNU programmable completion (rubash engine): when the command has a
        // compspec registered via the `complete` builtin, argument candidates
        // come from the attempt's `complete_detailed` (GAP-7, niubash#863:
        // the guard form of `Session::complete_detailed`). The
        // completion *engine* lives in rubash while niubash keeps its
        // cwd-priority, common-command, and file overlays for everything else.
        // The `has_compspec` gate keeps this free for commands without a
        // compspec (the hook's fallback would rescan PATH uncached and emit
        // raw-unquoted file names that duplicate the path completer above).
        {
            if has_compspec {
                if let Some(run) = engine.as_mut() {
                    let outcome = run.attempt.complete_detailed(input, cursor_pos);
                    if !outcome.candidates.is_empty() {
                        // niubash#837: carry the engine's per-candidate
                        // descriptions and the attempt's insert policy
                        // through instead of dropping them at the
                        // bridge — Candidate.description used to be
                        // discarded here (`.map(|c| c.text)`), and the
                        // policy (rubash#2573's CompletionPolicy) never
                        // reached the renderer's append-whitespace
                        // decision at all.
                        let (texts, descriptions): (Vec<String>, Vec<Option<String>>) = outcome
                            .candidates
                            .into_iter()
                            .map(|candidate| (candidate.text, candidate.description))
                            .unzip();
                        let mut result = CompletionResult::with_descriptions(texts, descriptions);
                        result.suppress_append = outcome.policy.suppress_append;
                        all_suggestions.extend(self.format_completions_for(result, &context));
                    }
                }
            }
        }

        // Deduplicate by value, keeping the first occurrence so the
        // cwd-priority and compdef ordering above stays meaningful.
        let mut seen = HashSet::new();
        all_suggestions.retain(|s| seen.insert(s.value.clone()));

        all_suggestions
    }

    /// Build current-directory path suggestions when the cursor is at command
    /// position. Returns suggestions with the same `span` shape as the rest of
    /// the pipeline.
    fn cwd_path_suggestions_at_command_position(
        &self,
        context: &CompletionContext,
    ) -> Vec<Suggestion> {
        if !context.is_command_position() {
            return Vec::new();
        }
        let word = context.get_current_word().unwrap_or_default();
        // Skip flag-like input and explicit path indicators; those are handled
        // by the path completer with its own prefix preservation rules.
        if word.starts_with('-')
            || word.contains('/')
            || word.contains('\\')
            || word.starts_with('.')
        {
            return Vec::new();
        }

        // niubash#747: enumerate through the session's stat-free
        // listing cache — this overlay used to walk the cwd (and
        // stat every non-directory in it) on every query.
        let listing = match &context.dir_cache {
            Some(handle) => handle.listing(&context.current_dir),
            None => uncached_listing(&context.current_dir),
        };
        let Some(listing) = listing else {
            return Vec::new();
        };

        let mut candidates: Vec<CwdPathCandidate> = Vec::new();
        for entry in listing.entries() {
            let file_name = &entry.name;
            if !context.behavior.matches(file_name, &word) {
                continue;
            }
            let is_dir = entry.kind == ListedKind::Directory;
            // niubash#671: a non-directory qualifies for the command
            // slot only when it is executable — offering plain files
            // (`notes.txt`) as `./notes.txt` manufactures commands the
            // shell would never resolve at this position. The verdict
            // is the one per-entry check that needs metadata; the
            // session cache memoizes it (niubash#747).
            if !is_dir {
                let is_exec = match &context.dir_cache {
                    Some(handle) => handle.exec_verdict(&context.current_dir, file_name),
                    None => entry_is_executable(&context.current_dir, file_name),
                };
                if !is_exec {
                    continue;
                }
            }
            if file_name.starts_with('.') && !word.starts_with('.') {
                continue;
            }
            let escaped = shell_escape_path_segment(file_name);
            let value = if is_dir {
                format!("./{escaped}/")
            } else {
                format!("./{escaped}")
            };
            // niubash#743: these are real filesystem entries — paint
            // them through the session's LS_COLORS table too.
            let style = context.file_colors.as_ref().and_then(|colors| {
                colors.style_for(
                    if is_dir {
                        crate::completion::CandidateKind::Directory
                    } else {
                        crate::completion::CandidateKind::Executable
                    },
                    file_name,
                )
            });
            candidates.push(CwdPathCandidate {
                is_dir,
                value,
                style,
            });
        }

        if candidates.is_empty() {
            return Vec::new();
        }

        candidates.sort();
        candidates.dedup();

        let (span_start, span_end) = context
            .current_word_span()
            .unwrap_or((context.cursor_pos, context.cursor_pos));

        candidates
            .into_iter()
            .map(|candidate| Suggestion {
                append_whitespace: !candidate.is_dir,
                value: candidate.value,
                display_override: None,
                description: None,
                style: candidate.style,
                extra: None,
                span: Span {
                    start: span_start,
                    end: span_end,
                },
                match_indices: None,
            })
            .collect()
    }

    fn alias_suggestions_at_command_position(
        &self,
        context: &CompletionContext,
        aliases: &HashSet<String>,
    ) -> Vec<Suggestion> {
        if !context.is_command_position() {
            return Vec::new();
        }
        let Some(word) = context.get_current_word() else {
            return Vec::new();
        };
        if word.contains('/') || word.contains('\\') || word.starts_with('.') {
            return Vec::new();
        }

        let (span_start, span_end) = context
            .current_word_span()
            .unwrap_or((context.cursor_pos, context.cursor_pos));
        let mut candidates: Vec<_> = aliases
            .iter()
            .filter(|alias| context.behavior.matches(alias, &word))
            .cloned()
            .collect();
        candidates.sort();

        candidates
            .into_iter()
            .map(|candidate| Suggestion {
                value: candidate,
                display_override: None,
                description: Some("alias".to_string()),
                style: None,
                extra: None,
                span: Span {
                    start: span_start,
                    end: span_end,
                },
                append_whitespace: true,
                match_indices: None,
            })
            .collect()
    }

    fn function_suggestions_at_command_position(
        &self,
        context: &CompletionContext,
        functions: &HashSet<String>,
    ) -> Vec<Suggestion> {
        if !context.is_command_position() {
            return Vec::new();
        }
        let Some(word) = context.get_current_word() else {
            return Vec::new();
        };
        if word.contains('/') || word.contains('\\') || word.starts_with('.') {
            return Vec::new();
        }

        let (span_start, span_end) = context
            .current_word_span()
            .unwrap_or((context.cursor_pos, context.cursor_pos));
        let mut candidates: Vec<_> = functions
            .iter()
            .filter(|function| context.behavior.matches(function, &word))
            .cloned()
            .collect();
        candidates.sort();

        candidates
            .into_iter()
            .map(|candidate| Suggestion {
                value: candidate,
                display_override: None,
                description: Some("function".to_string()),
                style: None,
                extra: None,
                span: Span {
                    start: span_start,
                    end: span_end,
                },
                append_whitespace: true,
                match_indices: None,
            })
            .collect()
    }
    /// The only shaping entry (GAP-9, niubash#863): the span is a
    /// token-view derivation read from the pipeline's own context,
    /// which carries the attempt session's view — a fresh context
    /// here would re-derive it from the stateless oracle and could
    /// disagree with the context that produced the candidates
    /// (posix mode).
    fn format_completions_for(
        &self,
        result: CompletionResult,
        context: &CompletionContext,
    ) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();
        let (span_start, span_end) = context
            .current_word_span()
            .unwrap_or((context.cursor_pos, context.cursor_pos));

        for (i, completion) in result.completions.iter().enumerate() {
            let description = result.descriptions.get(i).and_then(|d| d.as_deref());
            let style = result.styles.get(i).copied().flatten();

            suggestions.push(Suggestion {
                value: completion.clone(),
                display_override: None,
                description: description.map(|s| s.to_string()),
                style,
                extra: None,
                span: Span {
                    start: span_start,
                    end: span_end,
                },
                // A producer carrying an engine insert policy
                // (niubash#837: -o nospace via rubash's
                // CompletionPolicy) overrides the text-shape rule;
                // every other producer keeps it.
                append_whitespace: !result.suppress_append
                    && should_append_completion_whitespace(completion),
                match_indices: None,
            });
        }

        suggestions
    }
}

fn should_append_completion_whitespace(completion: &str) -> bool {
    let value = completion
        .trim_end_matches('"')
        .trim_end_matches('\'')
        .trim_end();
    !(value.ends_with('/') || value.ends_with('\\'))
}

/// Run a registered shell-function completion through the open
/// completion attempt and shape its candidates. The function is
/// resolved by the command word against the compdef registry the
/// driver captured into the attempt's host faces; its candidates
/// are filtered by the current word prefix. Without an attempt
/// there is no compdef source (the retired bridge's absence
/// produced the same `None`).
fn compdef_suggestions(
    context: &CompletionContext,
    engine: Option<&mut AttemptRun<'_, '_>>,
) -> Option<CompletionResult> {
    if context.is_command_position() {
        return None;
    }
    let engine = engine?;
    let command_word = command_words_before_cursor(context)?.first()?.clone();
    let cword = comp_cword(context);
    let function = engine
        .faces
        .compdefs
        .iter()
        .find(|(cmd, _)| cmd == &command_word)
        .map(|(_, function)| function.clone())?;

    // COMP_WORDS includes the word being completed, matching bash.
    // `command_words_before_cursor` splits the segment up to the cursor,
    // so its last element already IS the in-progress word when the cursor
    // sits inside one — pushing `current_word` again handed compdef
    // functions a duplicated final word (B5: `mycmd c` produced
    // NIU_COMP_WORDS "mycmd c c"). When the cursor follows whitespace,
    // the word being completed is the empty word after the last complete
    // word; it still occupies the COMP_CWORD slot, so append it as an
    // empty element (bash COMP_WORDS=(mycmd "") with COMP_CWORD=1).
    let mut words = command_words_before_cursor(context)?;
    let current_word = context.get_current_word().unwrap_or_default();
    let appending = context
        .input
        .get(..context.cursor_pos.min(context.input.len()))
        .and_then(|input| input.chars().last())
        .is_some_and(char::is_whitespace);
    if appending {
        words.push(String::new());
    }

    let entries = run_compdef_through_attempt(engine.attempt, &function, &words, cword);
    if entries.is_empty() {
        return None;
    }

    let mut completions = Vec::new();
    let mut descriptions = Vec::new();
    for (value, description) in entries {
        // niubash#851: honor the configured matcher (case sensitivity
        // and Prefix/Substring mode) like every other candidate
        // source — the hardcoded case-sensitive `starts_with` here
        // silently under-delivered compdef candidates under an
        // insensitive/substring configuration.
        if !current_word.is_empty()
            && !appending
            && !context.behavior.matches(&value, current_word.as_str())
        {
            continue;
        }
        completions.push(value);
        descriptions.push(description);
    }
    if completions.is_empty() {
        return None;
    }
    Some(CompletionResult::with_descriptions(
        completions,
        descriptions,
    ))
}

/// Run one compdef function through the open attempt's function
/// face (GAP-7, niubash#863) — the guard form of the retired
/// `Shell::run_compdef_function`, with the same contract:
///
/// The function sees the command line through `NIU_COMP_WORDS` (space
/// joined) and `NIU_COMP_CWORD` (bash COMP_CWORD semantics), passed
/// as the call's temporary env — the guard restores both when the
/// call returns. It writes back `NIU_COMP_RESULT` with one candidate
/// per line, shaped `value` or `value<TAB>description`. Compdef
/// functions must not print to stdout: completion runs while the
/// line editor owns the terminal. `NIU_COMP_RESULT` is deliberately
/// NOT unset here — the guard exposes no post-call mutation face —
/// the driver unsets it once the attempt drops
/// (`Shell::drive_completion`), before any later command can
/// observe it.
pub(crate) fn run_compdef_through_attempt(
    attempt: &mut rubash::api::CompletionAttempt<'_>,
    function: &str,
    words: &[String],
    cword: usize,
) -> Vec<(String, Option<String>)> {
    let joined = words.join(" ");
    if let Err(err) = attempt.call_function_with_env(
        function,
        std::iter::empty::<String>(),
        [
            ("NIU_COMP_WORDS", joined.as_str()),
            ("NIU_COMP_CWORD", cword.to_string().as_str()),
        ],
    ) {
        log::warn!("completion function '{}' failed: {}", function, err);
    }

    // The stored read (GAP-6's `variable_value`): the result
    // channel is a plain assignment by contract, where the stored
    // read and the retired method's expansion read agree; the
    // guard exposes no `&mut` variables view for the expansion
    // form.
    let result = attempt.session().variable_value("NIU_COMP_RESULT");

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

/// Words of the current command segment before the cursor (the compdef
/// call's COMP_WORDS/COMP_CWORD inputs). Moved here from the retired
/// completion::runtime module — this file is their only consumer.
fn command_words_before_cursor(context: &CompletionContext) -> Option<Vec<String>> {
    // GAP-9 (niubash#863): the words are the token view's segment
    // derivation — the segment after the last top-level separator,
    // each word's dequoted value, from the one grammar. This
    // retires both the completer's private splitter and its
    // separator rescan: niubash#836's quoted-separator shapes
    // (`mycmd "a|b" <TAB>`) are the lexer's own verdicts now.
    let words: Vec<String> = context
        .view()
        .segment_words_before(context.cursor())
        .into_iter()
        .map(|span| span.value.clone())
        .collect();
    (!words.is_empty()).then_some(words)
}

fn comp_cword(context: &CompletionContext) -> usize {
    let Some(words) = command_words_before_cursor(context) else {
        return 0;
    };
    if context
        .input
        .get(..context.cursor_pos.min(context.input.len()))
        .and_then(|input| input.chars().last())
        .is_some_and(char::is_whitespace)
    {
        words.len()
    } else {
        words.len().saturating_sub(1)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct CwdPathCandidate {
    is_dir: bool,
    value: String,
    /// The LS_COLORS style (niubash#743); not part of ordering.
    style: Option<nu_ansi_term::Style>,
}

impl Ord for CwdPathCandidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.is_dir, other.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => self
                .value
                .to_ascii_lowercase()
                .cmp(&other.value.to_ascii_lowercase())
                .then_with(|| self.value.cmp(&other.value)),
        }
    }
}

impl PartialOrd for CwdPathCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Completer for NiubashCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> ReedlineCompletionResult {
        // Through the inherent `complete` so an editor-flavor
        // completer drives the call through a completion attempt
        // (GAP-7, niubash#863) — the trait surface is how reedline's
        // menus invoke this completer at a live Tab.
        ReedlineCompletionResult::fresh(self.complete(line, pos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_completer_creation() {
        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from(
            "/home/user",
        ))));
        let completer = NiubashCompleter::new(state);
        assert!(completer.state.lock().is_ok());
    }

    #[test]
    fn attempt_compdef_produces_candidates() {
        use crate::test_support::PROCESS_STATE_LOCK;
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Dropping this Shell must not take the test process's PATH
        // with it (see ProcessEnvRestore).
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);

        let mut shell = crate::shell::Shell::new().unwrap();
        assert!(shell
            .execute_script(
                r#"
niu_git_comp() {
    NIU_COMP_RESULT="alpha	First"$'\n'"beta"
}
"#
            )
            .is_ok());
        shell.compdefs = vec![("git".to_string(), "niu_git_comp".to_string())];
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let mut completer = NiubashCompleter::with_shell(state, shell.clone());
        let suggestions = completer.complete("git ", 4);

        let alpha = suggestions
            .iter()
            .find(|s| s.value == "alpha")
            .unwrap_or_else(|| panic!("missing compdef candidate, got {suggestions:?}"));
        assert_eq!(alpha.description.as_deref(), Some("First"));
        assert!(suggestions.iter().any(|s| s.value == "beta"));
        // Drop the Shell inside this test, while _env_restore is still
        // armed: the completer's handle holds an Rc clone, so drop it
        // first, then the test's own Rc.
        drop(completer);
        drop(shell);
    }

    /// B5 compdef word fidelity: NIU_COMP_WORDS must equal bash's
    /// COMP_WORDS for the segment — the in-progress word appears exactly
    /// once, and after trailing whitespace the current word is the empty
    /// word at COMP_CWORD (previously the in-progress word was appended
    /// a second time, and the trailing-space shape dropped the empty
    /// word while COMP_CWORD still pointed at its slot).
    #[test]
    fn b5_compdef_words_match_bash_comp_words() {
        use crate::test_support::PROCESS_STATE_LOCK;
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);

        let temp = unique_temp_dir("niubash-b5-compdef-words");
        std::fs::create_dir_all(&temp).unwrap();
        let dump = temp.join("dump.txt");
        let dump_native = dump.to_string_lossy().replace('\\', "/");
        let mut shell = crate::shell::Shell::new().unwrap();
        assert!(shell
            .execute_script(&format!(
                r#"
niu_git_comp() {{
    echo "W=[$NIU_COMP_WORDS] C=$NIU_COMP_CWORD" > "{dump_native}"
    NIU_COMP_RESULT="cand"
}}
"#
            ))
            .is_ok());
        shell.compdefs = vec![("git".to_string(), "niu_git_comp".to_string())];
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let mut completer = NiubashCompleter::with_shell(state, shell.clone());

        // Cursor inside the partial word: it is COMP_WORDS[COMP_CWORD],
        // present exactly once.
        let _ = completer.complete("git co", 6);
        assert_eq!(
            std::fs::read_to_string(&dump).unwrap().trim_end(),
            "W=[git co] C=1",
            "the in-progress word must not be duplicated in NIU_COMP_WORDS"
        );

        // Cursor after trailing whitespace: the current word is empty
        // but still occupies the COMP_CWORD slot.
        let _ = completer.complete("git ", 4);
        assert_eq!(
            std::fs::read_to_string(&dump).unwrap().trim_end(),
            "W=[git ] C=1",
            "the empty current word must occupy the COMP_CWORD slot"
        );

        let _ = std::fs::remove_dir_all(&temp);
    }

    /// niubash#836: a command with BOTH a GNU compspec and a
    /// `NIU_COMPDEFS` compdef is owned by the compspec — the compdef's
    /// candidates must not merge into its set. The control half (no
    /// compspec registered) proves the compdef still fires there.
    #[test]
    fn issue836_compdef_skipped_when_compspec_owns_the_command() {
        use crate::test_support::PROCESS_STATE_LOCK;
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);

        let mut shell = crate::shell::Shell::new().unwrap();
        assert!(shell
            .execute_script(
                r#"
niu_git_comp() {
    NIU_COMP_RESULT="compdefonly"
}
complete -W 'speconly' git
"#
            )
            .is_ok());
        shell.compdefs = vec![("git".to_string(), "niu_git_comp".to_string())];
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let mut completer = NiubashCompleter::with_shell(state, shell.clone());
        let suggestions = completer.complete("git ", 4);
        assert_eq!(
            suggestions
                .iter()
                .map(|suggestion| suggestion.value.as_str())
                .collect::<Vec<_>>(),
            vec!["speconly"],
            "the compspec owns git: the compdef candidate must not merge in"
        );

        // Control: the same compdef without a compspec still produces
        // its candidate.
        let mut shell = crate::shell::Shell::new().unwrap();
        assert!(shell
            .execute_script(
                r#"
niu_git_comp() {
    NIU_COMP_RESULT="compdefonly"
}
"#
            )
            .is_ok());
        shell.compdefs = vec![("git".to_string(), "niu_git_comp".to_string())];
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let mut completer = NiubashCompleter::with_shell(state, shell.clone());
        let suggestions = completer.complete("git ", 4);
        assert!(
            suggestions.iter().any(|s| s.value == "compdefonly"),
            "without a compspec the compdef must still fire, got {suggestions:?}"
        );
    }

    /// niubash#836: `command_words_before_cursor` must split on the
    /// cursor segment's real separators only — a `|`/`;` inside quotes
    /// is word text, not a command boundary.
    #[test]
    fn issue836_command_words_before_cursor_uses_the_quoted_segment() {
        let words = |input: &str| {
            let context =
                CompletionContext::new(PathBuf::from("."), input.to_string(), input.len());
            command_words_before_cursor(&context)
        };
        // The quoted `|` after the last real separator used to become
        // the boundary, leaving the fragment `b"` as the command word.
        assert_eq!(
            words("mycmd \"a|b\" ar"),
            Some(vec![
                "mycmd".to_string(),
                "a|b".to_string(),
                "ar".to_string()
            ])
        );
        assert_eq!(
            words("true; mycmd \"a|b\" ar"),
            Some(vec![
                "mycmd".to_string(),
                "a|b".to_string(),
                "ar".to_string()
            ])
        );
        assert_eq!(
            words("mycmd 'a;b' ar"),
            Some(vec![
                "mycmd".to_string(),
                "a;b".to_string(),
                "ar".to_string()
            ])
        );
        // Real separators still split.
        assert_eq!(
            words("echo \"a|b\"; mycmd ar"),
            Some(vec!["mycmd".to_string(), "ar".to_string()])
        );
        assert_eq!(
            words("ls | mycmd ar"),
            Some(vec!["mycmd".to_string(), "ar".to_string()])
        );
    }

    /// niubash#851: the compdef candidate filter must honor the
    /// configured matcher like every other source. The compdef below
    /// returns `Alpha`; completing `git AL` under the default
    /// (case-insensitive) behavior must offer it — the old hardcoded
    /// case-sensitive `starts_with` filter silently dropped it.
    #[test]
    fn issue851_compdef_filter_honors_the_configured_matcher() {
        use crate::test_support::PROCESS_STATE_LOCK;
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);

        let mut shell = crate::shell::Shell::new().unwrap();
        assert!(shell
            .execute_script(
                r#"
niu_git_comp() {
    NIU_COMP_RESULT="Alpha"$'\n'"beta"
}
"#
            )
            .is_ok());
        shell.compdefs = vec![("git".to_string(), "niu_git_comp".to_string())];
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));

        let context = CompletionContext::new(PathBuf::from("."), "git AL".to_string(), 6);
        // GAP-7 (niubash#863): the direct compdef call now takes the
        // open attempt explicitly.
        let mut shell = shell.borrow_mut();
        let faces = shell.completion_host_faces();
        let mut attempt = shell.session_mut().begin_completion_attempt();
        let mut run = AttemptRun {
            attempt: &mut attempt,
            faces: &faces,
        };
        let result = compdef_suggestions(&context, Some(&mut run))
            .unwrap_or_else(|| panic!("compdef must produce candidates for `git AL`"));
        assert!(
            result.completions.iter().any(|value| value == "Alpha"),
            "case-insensitive matching must keep `Alpha` for query `AL`, got {:?}",
            result.completions
        );
        assert!(
            !result.completions.iter().any(|value| value == "beta"),
            "`beta` does not match `AL` in any mode"
        );
    }
    #[test]
    fn compspec_candidates_flow_through_rubash_hook() {
        use crate::test_support::PROCESS_STATE_LOCK;
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Dropping this Shell must not take the test process's PATH
        // with it (see ProcessEnvRestore).
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);

        let mut shell = crate::shell::Shell::new().unwrap();
        // Register a GNU compspec on the rubash executor via the `complete`
        // builtin, exactly as a user script would.
        assert!(shell
            .execute_script("complete -W 'alpha beta gamma' mycmd")
            .is_ok());
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let mut completer = NiubashCompleter::with_shell(state, shell.clone());
        // Completing the second word of `mycmd a`: the compspec wordlist
        // filtered by the prefix `a` must yield `alpha` through the hook.
        let suggestions = completer.complete("mycmd a", 7);

        assert!(
            suggestions.iter().any(|s| s.value == "alpha"),
            "missing compspec candidate alpha, got {suggestions:?}"
        );
        // Drop the Shell inside this test, while _env_restore is still
        // armed: the completer's handle holds an Rc clone, so drop it
        // first, then the test's own Rc.
        drop(completer);
        drop(shell);
    }

    #[test]
    fn issue837_engine_nospace_policy_reaches_suggestions() {
        use crate::test_support::PROCESS_STATE_LOCK;
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);

        let mut shell = crate::shell::Shell::new().unwrap();
        // A compspec carrying -o nospace: the engine's insert policy
        // (rubash#2573 CompletionPolicy.suppress_append) must reach
        // the Suggestion's append_whitespace through the completer —
        // before niubash#837 the host used Session::complete and
        // the policy never crossed.
        assert!(shell
            .execute_script("complete -o nospace -W 'zzone zztwo' mycmd837")
            .is_ok());
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let mut completer = NiubashCompleter::with_shell(state, shell.clone());
        let suggestions = completer.complete("mycmd837 zz", 11);
        let zzone = suggestions
            .iter()
            .find(|s| s.value == "zzone")
            .unwrap_or_else(|| panic!("missing engine candidate, got {suggestions:?}"));
        assert!(
            !zzone.append_whitespace,
            "-o nospace must suppress the appended space: {zzone:?}"
        );
    }

    #[test]
    fn issue837_format_completions_carries_descriptions_and_suppress_append() {
        // The shaping half of the contract: a CompletionResult
        // built the way the engine leg now builds it (descriptions
        // unzipped from Candidate.description, suppress_append from
        // the attempt's policy) must surface both on the Suggestion.
        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let completer = NiubashCompleter::new(state);
        let mut result = CompletionResult::with_descriptions(
            vec!["alpha".to_string()],
            vec![Some("First".to_string())],
        );
        result.suppress_append = true;
        // GAP-9 (niubash#863): the retired `format_completions`
        // fresh-context wrapper is gone — the span now reads from
        // the pipeline's own context, so the test builds the same
        // context the wrapper used to build and calls
        // `format_completions_for` directly.
        let context = CompletionContext::new(PathBuf::new(), "cmd al".to_string(), 6);
        let suggestions = completer.format_completions_for(result, &context);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].description.as_deref(), Some("First"));
        assert!(!suggestions[0].append_whitespace);

        // Control: without the policy the text-shape rule appends.
        let result = CompletionResult::with_descriptions(
            vec!["alpha".to_string()],
            vec![Some("First".to_string())],
        );
        let suggestions = completer.format_completions_for(result, &context);
        assert!(suggestions[0].append_whitespace);
    }

    #[test]
    fn compspec_owns_the_command_over_static_definition_tables() {
        use crate::test_support::PROCESS_STATE_LOCK;
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let mut shell = crate::shell::Shell::new().unwrap();
        // `git` ships a static definition table (subcommands + flags) in the
        // embedded assets. A compspec registered for it must own the command:
        // the static table contributes nothing.
        assert!(shell.execute_script("complete -W 'zzz' git").is_ok());
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let mut completer = NiubashCompleter::with_shell(state, shell.clone());
        let suggestions = completer.complete("git ", 4);

        assert_eq!(
            suggestions
                .iter()
                .map(|suggestion| suggestion.value.as_str())
                .collect::<Vec<_>>(),
            vec!["zzz"],
            "a compspec must suppress the static table and the path fallback"
        );
    }

    #[test]
    fn issue830_compspec_probe_uses_cursor_segment_command_word() {
        use crate::test_support::PROCESS_STATE_LOCK;
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);

        let mut shell = crate::shell::Shell::new().unwrap();
        assert!(shell.execute_script("complete -W 'zzz' git").is_ok());
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let mut completer = NiubashCompleter::with_shell(state, shell.clone());

        // niubash#830: the probe used to take the first whitespace
        // token of the WHOLE line, so on each of these lines it asked
        // has_compspec about the wrong word ("true;", "VAR=1", "(",
        // "sudo"), missed git's compspec, and let the static table
        // answer. The compspec must own git's arguments on all of
        // them: its candidate arrives and no static-table candidate
        // (e.g. the `checkout` subcommand) leaks in.
        for line in ["true; git ", "VAR=1 git ", "sudo x; git "] {
            let suggestions = completer.complete(line, line.len());
            let values: Vec<&str> = suggestions
                .iter()
                .map(|suggestion| suggestion.value.as_str())
                .collect();
            assert!(
                values.contains(&"zzz"),
                "{line:?}: compspec candidate must flow, got {values:?}"
            );
            assert!(
                !values.contains(&"checkout"),
                "{line:?}: static git table must stay suppressed, got {values:?}"
            );
        }

        // '( git ': the host-side contract is that the probe names
        // git, the compspec gate engages, and the static table stays
        // suppressed. Whether the engine then yields candidates
        // inside an unclosed subshell is the engine's own segment
        // handling, so this shape pins only the no-leak half.
        let suggestions = completer.complete("( git ", "( git ".len());
        assert!(
            !suggestions
                .iter()
                .any(|suggestion| suggestion.value == "checkout"),
            "'( git ': static git table must stay suppressed, got {suggestions:?}"
        );

        // The issue's headline regression shape, exact: the compspec
        // is the ONLY source for git's arguments after a separator.
        let suggestions = completer.complete("true; git ", "true; git ".len());
        assert_eq!(
            suggestions
                .iter()
                .map(|suggestion| suggestion.value.as_str())
                .collect::<Vec<_>>(),
            vec!["zzz"]
        );
    }

    #[test]
    fn glob_completion_uses_the_engine_glob_filename() {
        use crate::test_support::PROCESS_STATE_LOCK;
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);

        let temp_dir = unique_temp_dir("niubash-glob-completion");
        std::fs::create_dir_all(&temp_dir).unwrap();
        for name in ["alpha.txt", "alpine.txt", "beta.txt"] {
            std::fs::write(temp_dir.join(name), name).unwrap();
        }

        let mut shell = crate::shell::Shell::new().unwrap();
        let script = format!("cd {}", temp_dir.display());
        assert!(shell.execute_script(&script).is_ok());
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));

        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let mut completer = NiubashCompleter::with_shell(state, shell.clone());
        let suggestions =
            completer.complete_specific(SpecificCompletion::Glob, "cat al*", "cat al*".len());

        assert_eq!(
            suggestions
                .iter()
                .map(|suggestion| suggestion.value.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha.txt", "alpine.txt"],
            "glob-complete-word globs the word plus '*' through Session::glob_filename"
        );
        for suggestion in &suggestions {
            assert_eq!(suggestion.span.start, 4);
            assert_eq!(suggestion.span.end, 7);
        }

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_load_completion_dirs() {
        let mut state = CompletionState::new(PathBuf::from("."));
        state.load_completion_dirs(&[]);
        // Should have registered at least the command plugin
        assert!(state
            .plugins
            .iter()
            .any(|p| p.name() == "command-completion"));
    }

    #[test]
    fn completer_span_covers_escaped_shell_word() {
        let temp_dir = unique_temp_dir("niubash-completer-span");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("two words.txt"), "two").unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let mut completer = NiubashCompleter::new(state);
        let input = "ls two\\ w";
        let suggestions = completer.complete(input, input.len());

        let suggestion = suggestions
            .iter()
            .find(|suggestion| suggestion.value == "two\\ words.txt")
            .unwrap_or_else(|| panic!("missing suggestion, got {suggestions:?}"));
        assert_eq!(suggestion.span.start, 3);
        assert_eq!(suggestion.span.end, input.len());

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    /// niubash#747 (a): candidate generation over a 6000-entry
    /// directory must not scale with per-entry stats. The old
    /// enumeration paid ~36–90µs/entry (≈220–550 ms for this
    /// directory). The hard guarantee here
    /// is structural — the session cache's metadata counter stays
    /// within the refinement window — with a loose clock bound as
    /// an end-to-end smoke shape (parallel test load makes tight
    /// budgets meaningless; the counter does not care).
    #[test]
    fn issue747_large_directory_candidate_generation_budget() {
        let temp_dir = unique_temp_dir("niubash-issue747-perf");
        std::fs::create_dir_all(&temp_dir).unwrap();
        for i in 0..6000 {
            std::fs::write(temp_dir.join(format!("entry{i:04}")), "x").unwrap();
        }

        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let probe = {
            let state = state.lock().unwrap();
            DirCacheHandle::with_shared(
                state.dir_cache.clone(),
                SessionKey::new(temp_dir.clone(), String::new()),
            )
        };
        let mut completer = NiubashCompleter::new(state);
        let start = std::time::Instant::now();
        let suggestions = completer.complete("ls ", 3);
        let elapsed = start.elapsed();
        assert_eq!(
            suggestions.len(),
            6000,
            "every entry must be offered, got {}",
            suggestions.len()
        );
        // The load-bearing assertion is structural, not the clock:
        // one enumeration, and per-entry metadata bounded by the
        // refinement window — a return of the per-entry stat shape
        // (2000 stats) fails here on any machine, loaded or not.
        let (enumerations, metadata_calls) = probe.stats();
        assert_eq!(enumerations, 1, "one session query enumerates once");
        assert!(
            metadata_calls <= crate::completion::path::EXEC_REFINE_LIMIT,
            "per-entry metadata must stay bounded by the refinement \
             window, got {metadata_calls} for 6000 entries"
        );
        // The clock is a smoke bound only, sized for a loaded CI
        // machine: the pre-fix shape measured ~180 ms idle for this
        // directory on the issue's machine class and grows with the
        // entry count, while the stat-free shape stays flat.
        assert!(
            elapsed < std::time::Duration::from_millis(600),
            "6000-entry candidate generation took {elapsed:?}; \
             the per-entry stat shape must not come back"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    /// niubash#747 (d): at command position the cwd overlay shares
    /// the session listing — two queries in one session enumerate
    /// the directory once, however many keystrokes re-ask.
    #[test]
    fn issue747_command_position_overlay_enumerates_once_per_session() {
        let temp_dir = unique_temp_dir("niubash-issue747-overlay");
        std::fs::create_dir_all(temp_dir.join("rundir")).unwrap();
        std::fs::write(temp_dir.join("run.sh"), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                temp_dir.join("run.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let probe = {
            let state = state.lock().unwrap();
            DirCacheHandle::with_shared(
                state.dir_cache.clone(),
                SessionKey::new(temp_dir.clone(), String::new()),
            )
        };
        let before = probe.stats().0;
        let mut completer = NiubashCompleter::new(state);
        let first = completer.complete("ru", 2);
        let second = completer.complete("run", 3);
        assert!(first
            .iter()
            .any(|suggestion| suggestion.value == "./rundir/"));
        assert!(second
            .iter()
            .any(|suggestion| suggestion.value == "./rundir/"));
        assert_eq!(
            probe.stats().0 - before,
            1,
            "the cwd overlay must enumerate once per completion session"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn burndown_complete_specific_pins_the_gnu_source() {
        let temp_dir = unique_temp_dir("niubash-specific-completion");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("spec-file-alpha.txt"), "x").unwrap();
        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        // GAP-4 (niubash#863): the alias, variable and directory
        // all reach the completer from one live shell (aliases from
        // the host map the attempt's faces capture, variables from
        // the engine view, the directory from the host-spelled
        // faces) — so this pin composes them on a real shell, the
        // same shape production completes in, instead of planting
        // them in state fields. GAP-7: the live shell arrives
        // through the completer's attempt-driving handle.
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);
        let mut shell = crate::shell::Shell::new().unwrap();
        let cd = format!("cd {}", temp_dir.to_string_lossy().replace('\\', "/"));
        assert!(shell.execute_interactive_line(&cd).is_ok());
        assert!(shell
            .execute_interactive_line("alias niu_specific_command='echo niu'")
            .is_ok());
        assert!(shell.execute_script("NIU_SPECIFIC_VAR=1").is_ok());
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));
        let mut completer = NiubashCompleter::with_shell(state, shell.clone());

        // complete-command ignores the cursor's argument position:
        // GNU's command_word_completion_function always completes a
        // command name (bashline.c:2005).
        let commands =
            completer.complete_specific(SpecificCompletion::Command, "echo niu_specific_co", 20);
        assert!(
            commands
                .iter()
                .any(|suggestion| suggestion.value == "niu_specific_command"),
            "command mode must complete the alias at argument position: {commands:?}"
        );

        // complete-variable completes a variable name, keeping the
        // user's `$` prefix (bashline.c:2568).
        let variables =
            completer.complete_specific(SpecificCompletion::Variable, "$NIU_SPECIFIC", 13);
        assert!(
            variables
                .iter()
                .any(|suggestion| suggestion.value == "$NIU_SPECIFIC_VAR"),
            "variable mode must complete the name: {variables:?}"
        );

        // complete-filename completes against the filesystem even at
        // an argument position (bashline.c:3896).
        let files = completer.complete_specific(SpecificCompletion::Filename, "echo spec-", 10);
        assert!(
            files
                .iter()
                .any(|suggestion| suggestion.value == "spec-file-alpha.txt"),
            "filename mode must complete the file: {files:?}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn burndown_brace_completion_compresses_filename_matches() {
        let temp_dir = unique_temp_dir("niubash-brace-completion");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("alpha-one.txt"), "x").unwrap();
        std::fs::write(temp_dir.join("alpha-two.txt"), "x").unwrap();
        std::fs::write(temp_dir.join("alpha three.txt"), "x").unwrap();
        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let mut completer = NiubashCompleter::new(state);

        // `complete-into-braces` completes filenames first, then returns
        // exactly one compressed expression (bashline.c
        // bash_brace_completion -> bracecomp.c hack_braces_completion).
        let suggestions = completer.complete_specific(SpecificCompletion::Brace, "echo alpha-", 11);
        assert_eq!(
            suggestions
                .iter()
                .map(|suggestion| suggestion.value.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha-{one.txt,two.txt}"],
            "brace completion must compress the filename matches: {suggestions:?}"
        );

        // The path completer's shell quoting is undone first: the raw
        // name is quoted once by bracecomp's sh_backslash_quote, not twice.
        let spaced = completer.complete_specific(SpecificCompletion::Brace, "echo alpha\\ t", 13);
        assert_eq!(
            spaced
                .iter()
                .map(|suggestion| suggestion.value.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha\\ three.txt"],
            "a raw filename space is backslash-quoted exactly once: {spaced:?}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
    }
}

#[cfg(test)]
mod cwd_priority_tests {
    use super::*;

    #[test]
    fn command_position_offers_matching_cwd_directory_ahead_of_path_commands() {
        // Build a temp cwd that contains a directory whose name shares a prefix
        // with a real PATH executable so we can prove directories win.
        let temp_dir = unique_temp_dir("niubash-cwd-priority");
        std::fs::create_dir_all(temp_dir.join("niubash")).unwrap();
        std::fs::create_dir_all(temp_dir.join("other")).unwrap();
        std::fs::write(temp_dir.join(" readme.txt"), "ignored").unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let mut completer = NiubashCompleter::new(state);
        let suggestions = completer.complete("niu", 3);

        let dir_suggestion = suggestions
            .iter()
            .find(|suggestion| suggestion.value == "./niubash/")
            .unwrap_or_else(|| panic!("expected ./niubash/ ahead of PATH, got {suggestions:?}"));
        assert_eq!(dir_suggestion.span.start, 0);
        assert_eq!(dir_suggestion.span.end, 3);
        assert!(!dir_suggestion.append_whitespace);

        // Directories whose names do not match the prefix must not sneak in.
        assert!(
            !suggestions
                .iter()
                .any(|suggestion| suggestion.value == "./other/"),
            "unexpected ./other/ in {suggestions:?}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn command_position_offers_matching_cwd_file_with_dot_slash() {
        // niubash#671 revised this pin: the fixture used to be a plain
        // data file (`winuxfile.txt`), which the command slot must NOT
        // offer — only an executable file qualifies. The `./` shape and
        // span contract for qualifying files are unchanged.
        let temp_dir = unique_temp_dir("niubash-cwd-priority-files");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("winuxfile.txt"), "x").unwrap();
        std::fs::write(temp_dir.join("winuxrun.exe"), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                temp_dir.join("winuxrun.exe"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let mut completer = NiubashCompleter::new(state);
        let suggestions = completer.complete("win", 3);

        let file_suggestion = suggestions
            .iter()
            .find(|suggestion| suggestion.value == "./winuxrun.exe")
            .unwrap_or_else(|| panic!("expected cwd file with ./, got {suggestions:?}"));
        assert_eq!(file_suggestion.span.start, 0);
        assert_eq!(file_suggestion.span.end, 3);
        assert!(file_suggestion.append_whitespace);
        assert!(
            !suggestions
                .iter()
                .any(|suggestion| suggestion.value == "./winuxfile.txt"),
            "plain data file leaked into command position: {suggestions:?}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn issue671_command_position_skips_plain_files_keeps_dirs_and_executables() {
        let temp_dir = unique_temp_dir("niubash-issue671");
        std::fs::create_dir_all(temp_dir.join("rundir")).unwrap();
        std::fs::write(temp_dir.join("runnotes.txt"), "x").unwrap();
        std::fs::write(temp_dir.join("runapp.exe"), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                temp_dir.join("runapp.exe"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let mut completer = NiubashCompleter::new(state);
        let suggestions = completer.complete("run", 3);

        assert!(
            suggestions
                .iter()
                .any(|suggestion| suggestion.value == "./rundir/"),
            "directory must stay offered: {suggestions:?}"
        );
        assert!(
            suggestions
                .iter()
                .any(|suggestion| suggestion.value == "./runapp.exe"),
            "executable file must stay offered: {suggestions:?}"
        );
        assert!(
            !suggestions
                .iter()
                .any(|suggestion| suggestion.value == "./runnotes.txt"),
            "plain data file must not be offered at command position: {suggestions:?}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn command_position_empty_input_offers_cwd_entries() {
        let temp_dir = unique_temp_dir("niubash-cwd-priority-empty");
        std::fs::create_dir_all(temp_dir.join("localdir")).unwrap();
        std::fs::write(temp_dir.join("localfile.txt"), "x").unwrap();
        std::fs::write(temp_dir.join(".hidden"), "x").unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let mut completer = NiubashCompleter::new(state);
        let suggestions = completer.complete("", 0);

        let dir_suggestion = suggestions
            .iter()
            .find(|suggestion| suggestion.value == "./localdir/")
            .unwrap_or_else(|| panic!("expected cwd dir on empty Tab, got {suggestions:?}"));
        assert!(!dir_suggestion.append_whitespace);

        // niubash#671 revised this pin: a plain data file is not a
        // command and must not be dumped into the empty-prompt list.
        assert!(
            !suggestions
                .iter()
                .any(|suggestion| suggestion.value == "./localfile.txt"),
            "plain data file leaked into empty-input command position: {suggestions:?}"
        );
        assert!(
            !suggestions
                .iter()
                .any(|suggestion| suggestion.value == ".hidden"),
            "hidden file leaked without dot prefix: {suggestions:?}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn command_position_cwd_suggestions_respect_hidden_dot_prefix() {
        let temp_dir = unique_temp_dir("niubash-cwd-priority-hidden");
        std::fs::create_dir_all(temp_dir.join(".niubash")).unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let mut completer = NiubashCompleter::new(state);

        // Without a dot prefix, hidden directories should not be offered.
        let visible = completer.complete("niu", 3);
        assert!(
            !visible
                .iter()
                .any(|suggestion| suggestion.value == ".niubash/"),
            "hidden dir leaked without dot prefix: {visible:?}"
        );

        // With a dot prefix, the hidden directory should appear.
        let dotted = completer.complete(".niu", 4);
        assert!(
            dotted
                .iter()
                .any(|suggestion| suggestion.value == ".niubash/"),
            "expected .niubash/ for dotted prefix, got {dotted:?}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn argument_position_uses_path_completer_for_cwd_directories() {
        // After `echo `, we are not at command position. The directory
        // shortcut for command position must not fire; argument position is
        // served by the path completer, which legitimately surfaces matching
        // directories from cwd. We assert the directory appears (via
        // PathCompleter), and that the command-position shortcut did not run
        // by checking the suggestion span ends at the cursor rather than at
        // the start of the typed word. Both paths produce a directory entry,
        // but only the path completer runs at argument position.
        let temp_dir = unique_temp_dir("niubash-cwd-priority-arg");
        std::fs::create_dir_all(temp_dir.join("niubash")).unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let mut completer = NiubashCompleter::new(state);
        let suggestions = completer.complete("echo niu", 8);

        let dir_suggestion = suggestions
            .iter()
            .find(|suggestion| suggestion.value == "niubash/")
            .unwrap_or_else(|| panic!("expected niubash/ via PathCompleter, got {suggestions:?}"));

        assert_eq!(dir_suggestion.span.start, 5);
        assert_eq!(dir_suggestion.span.end, 8);

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
    }
}

/// Escape a single path segment so it is safe to insert into a shell line.
/// Mirrors the escaping rules used by `path::shell_escape_path` but does not
/// include quoting, because command-position suggestions are full tokens and
/// the trailing `/` should remain visible to the user.
fn shell_escape_path_segment(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(
            ch,
            ' ' | '\t'
                | '\\'
                | '\''
                | '"'
                | '$'
                | '`'
                | '!'
                | '&'
                | ';'
                | '('
                | ')'
                | '<'
                | '>'
                | '|'
                | '*'
                | '?'
                | '['
                | ']'
                | '{'
                | '}'
        ) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}
