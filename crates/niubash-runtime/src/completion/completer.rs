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

/// State shared with completer
pub struct CompletionState {
    pub current_dir: PathBuf,
    pub env_vars: HashMap<String, String>,
    pub aliases: HashSet<String>,
    pub functions: HashSet<String>,
    /// The session's open fd numbers, synced from the engine's fd
    /// table by `Shell::update_completion_state`. The path
    /// completer's dynamic tier lists these under `/dev/fd/`
    /// (niubash#627).
    pub open_fds: Vec<u32>,
    /// The backing directory for the session's `/tmp`, resolved by
    /// the engine from the session env (session TMPDIR wins) and
    /// synced by `Shell::update_completion_state`. The path
    /// completer and the syntax highlighter root `/tmp` spellings
    /// here so both follow a session `TMPDIR` assignment immediately
    /// (niubash#627 G2). Starts at the process temp dir, the pre-sync
    /// fallback.
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
    /// niubash#594: fingerprint of the engine's live env map that
    /// `env_vars` was snapshotted from. `Shell::update_completion_state`
    /// compares it against the current live map and skips the re-clone
    /// when unchanged. 0 = no snapshot yet / invalidated by another
    /// writer, which forces a rebuild on the next update.
    pub snapshot_env_fingerprint: u64,
    /// niubash#594: the sorted engine function names `functions` was
    /// built from, kept so the next update can skip the rebuild when
    /// the name set is unchanged.
    pub snapshot_function_names: Vec<String>,
    /// niubash#594: the sorted alias names `aliases` was built from.
    pub snapshot_alias_names: Vec<String>,
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
            env_vars: HashMap::new(),
            aliases: HashSet::new(),
            functions: HashSet::new(),
            open_fds: Vec::new(),
            tmp_base: std::env::temp_dir(),
            behavior: CompletionBehavior::default(),
            plugins: Vec::new(),
            command_probe: CommandProbe::default(),
            completion_dirs: Vec::new(),
            snapshot_env_fingerprint: 0,
            snapshot_function_names: Vec::new(),
            snapshot_alias_names: Vec::new(),
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
}

/// Custom completer for WinSH
pub struct NiubashCompleter {
    state: Arc<Mutex<CompletionState>>,
}

impl NiubashCompleter {
    /// Create a new completer with shared state
    pub fn new(state: Arc<Mutex<CompletionState>>) -> Self {
        Self { state }
    }

    /// Update state
    pub fn update_state(&self, current_dir: PathBuf, env_vars: HashMap<String, String>) {
        if let Ok(mut state) = self.state.lock() {
            state.current_dir = current_dir;
            state.env_vars = env_vars;
            // niubash#594: this write bypasses the fingerprint gate, so
            // invalidate it — the next `update_completion_state` rebuilds
            // the env snapshot from the engine instead of trusting a
            // fingerprint that no longer describes `env_vars`.
            state.snapshot_env_fingerprint = 0;
        }
    }

    /// Return plain suggestions for internal probes and tests.
    pub fn complete(&mut self, input: &str, cursor_pos: usize) -> Vec<Suggestion> {
        self.complete_input(input, cursor_pos)
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
        let (current_dir, env_vars, aliases, functions, open_fds, tmp_base, behavior, dir_cache) =
            if let Ok(state) = self.state.lock() {
                (
                    state.current_dir.clone(),
                    state.env_vars.clone(),
                    state.aliases.clone(),
                    state.functions.clone(),
                    state.open_fds.clone(),
                    state.tmp_base.clone(),
                    state.behavior,
                    state.dir_cache.clone(),
                )
            } else {
                return Vec::new();
            };

        let cursor_pos = cursor_pos.min(input.len());
        let mut context = CompletionContext::with_behavior(
            current_dir.clone(),
            input.to_string(),
            cursor_pos,
            behavior,
        );
        context.open_fds = open_fds;
        context.tmp_base = tmp_base;
        context.file_colors = Some(crate::completion::FileColors::from_env_vars(&env_vars));
        context.dir_cache = Some(DirCacheHandle::with_shared(
            dir_cache,
            SessionKey::new(current_dir, String::new()),
        ));

        let result = match mode {
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
        };

        match result {
            Some(result) => self.format_completions(result, input, cursor_pos),
            None => Vec::new(),
        }
    }

    /// Complete input
    fn complete_input(&mut self, input: &str, cursor_pos: usize) -> Vec<Suggestion> {
        let (
            current_dir,
            env_vars,
            aliases,
            functions,
            open_fds,
            tmp_base,
            behavior,
            plugins,
            dir_cache,
        ) = if let Ok(state) = self.state.lock() {
            (
                state.current_dir.clone(),
                state.env_vars.clone(),
                state.aliases.clone(),
                state.functions.clone(),
                state.open_fds.clone(),
                state.tmp_base.clone(),
                state.behavior,
                state.plugins.clone(),
                state.dir_cache.clone(),
            )
        } else {
            return Vec::new();
        };

        let mut context = CompletionContext::with_behavior(
            current_dir.clone(),
            input.to_string(),
            cursor_pos,
            behavior,
        );
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
            .unwrap_or_else(|| floor_char_boundary(input, cursor_pos.min(input.len())));
        let line_stem = input[..word_start].to_string();
        context.dir_cache = Some(DirCacheHandle::with_shared(
            dir_cache,
            SessionKey::new(current_dir, line_stem),
        ));
        // niubash#743: the session's LS_COLORS table, resolved per
        // completion call so a fresh `export LS_COLORS=…` takes effect
        // on the next TAB without an editor rebuild.
        context.file_colors = Some(crate::completion::FileColors::from_env_vars(&env_vars));
        let mut all_suggestions = Vec::new();

        // GNU programmable completion owns a command's arguments whenever a
        // compspec is registered (`complete`, bash-completion, an inputrc
        // rule). Decide that once, up front: the static definition tables
        // and the path fallback are overlays for commands WITHOUT a
        // compspec, and must not be merged into its candidates.
        let first_word = input.split_whitespace().next().unwrap_or_default();
        let has_compspec = !context.is_command_position()
            && !first_word.is_empty()
            && crate::shell::with_completion_bridge(|shell| {
                shell.session().has_compspec(first_word)
            })
            .unwrap_or(false);

        // Shell-function completions (`NIU_COMPDEFS`): only available while
        // the interactive REPL has installed the shell bridge.
        if let Some(result) = compdef_suggestions(&context) {
            all_suggestions.extend(self.format_completions(result, input, cursor_pos));
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
                let formatted = self.format_completions(result, input, cursor_pos);
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
                    return self.format_completions(result, input, cursor_pos);
                }
            }
            if context.is_path_completion() && !has_compspec {
                if let Ok(Some(result)) = PathCompleter::complete(&context) {
                    all_suggestions.extend(self.format_completions(result, input, cursor_pos));
                }
            }
        }

        // GNU programmable completion (rubash engine): when the command has a
        // compspec registered via the `complete` builtin, argument candidates
        // come from `Session::complete` through the shell bridge. The
        // completion *engine* lives in rubash while niubash keeps its
        // cwd-priority, common-command, and file overlays for everything else.
        // The `has_compspec` gate keeps this free for commands without a
        // compspec (the hook's fallback would rescan PATH uncached and emit
        // raw-unquoted file names that duplicate the path completer above).
        {
            if has_compspec {
                if let Some(cands) = crate::shell::with_completion_bridge(|shell| {
                    shell
                        .session_mut()
                        .complete(input, cursor_pos)
                        .into_iter()
                        .map(|candidate| candidate.text)
                        .collect::<Vec<String>>()
                }) {
                    if !cands.is_empty() {
                        let result = CompletionResult::new(cands);
                        all_suggestions.extend(self.format_completions(result, input, cursor_pos));
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
    fn format_completions(
        &self,
        result: CompletionResult,
        input: &str,
        cursor_pos: usize,
    ) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();
        let span_context = CompletionContext::new(PathBuf::new(), input.to_string(), cursor_pos);
        let (span_start, span_end) = span_context
            .current_word_span()
            .unwrap_or((cursor_pos, cursor_pos));

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
                append_whitespace: should_append_completion_whitespace(completion),
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

/// Run a registered shell-function completion through the installed shell
/// bridge and shape its candidates. The function is resolved by the command
/// word; its candidates are filtered by the current word prefix.
fn compdef_suggestions(context: &CompletionContext) -> Option<CompletionResult> {
    if context.is_command_position() {
        return None;
    }
    let command_word = command_words_before_cursor(context)?.first()?.clone();
    let cword = comp_cword(context);
    let function = crate::shell::with_completion_bridge(|shell| {
        let (_, function) = shell
            .compdefs
            .iter()
            .find(|(cmd, _)| cmd == &command_word)?;
        Some(function.clone())
    })??;

    // COMP_WORDS includes the word being completed, matching bash.
    let mut words = command_words_before_cursor(context)?;
    let current_word = context.get_current_word().unwrap_or_default();
    let appending = context
        .input
        .get(..context.cursor_pos.min(context.input.len()))
        .and_then(|input| input.chars().last())
        .is_some_and(char::is_whitespace);
    if !appending {
        words.push(current_word.clone());
    }

    let entries = crate::shell::with_completion_bridge(|shell| {
        shell.run_compdef_function(&function, &words, cword)
    })?;
    if entries.is_empty() {
        return None;
    }

    let mut completions = Vec::new();
    let mut descriptions = Vec::new();
    for (value, description) in entries {
        if !current_word.is_empty() && !appending && !value.starts_with(current_word.as_str()) {
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

/// Words of the current command segment before the cursor (the compdef
/// bridge's COMP_WORDS/COMP_CWORD inputs). Moved here from the retired
/// completion::runtime module — this file is their only consumer.
fn command_words_before_cursor(context: &CompletionContext) -> Option<Vec<String>> {
    let pos = context.cursor_pos.min(context.input.len());
    let pos = floor_char_boundary(&context.input, pos);
    let before_cursor = &context.input[..pos];
    let start = before_cursor
        .rfind([';', '|', '&', '\n'])
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let segment = before_cursor[start..].trim_start();
    let words = split_shell_words(segment);
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

fn split_shell_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut single = false;
    let mut double = false;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            ch if ch.is_whitespace() && !single && !double => {
                if !current.is_empty() {
                    words.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn floor_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut p = pos;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
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
        ReedlineCompletionResult::fresh(self.complete_input(line, pos))
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
    fn shell_bridge_compdef_produces_candidates() {
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
        crate::shell::install_completion_bridge(&shell);

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let mut completer = NiubashCompleter::new(state);
        let suggestions = completer.complete("git ", 4);

        let alpha = suggestions
            .iter()
            .find(|s| s.value == "alpha")
            .unwrap_or_else(|| panic!("missing compdef candidate, got {suggestions:?}"));
        assert_eq!(alpha.description.as_deref(), Some("First"));
        assert!(suggestions.iter().any(|s| s.value == "beta"));
        // Drop the Shell inside this test, while _env_restore is still
        // armed (the thread-local bridge would otherwise defer the drop
        // to thread teardown).
        crate::shell::uninstall_completion_bridge();
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
        crate::shell::install_completion_bridge(&shell);

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let mut completer = NiubashCompleter::new(state);
        // Completing the second word of `mycmd a`: the compspec wordlist
        // filtered by the prefix `a` must yield `alpha` through the hook.
        let suggestions = completer.complete("mycmd a", 7);

        assert!(
            suggestions.iter().any(|s| s.value == "alpha"),
            "missing compspec candidate alpha, got {suggestions:?}"
        );
        // Drop the Shell inside this test, while _env_restore is still
        // armed (the thread-local bridge would otherwise defer the drop
        // to thread teardown).
        crate::shell::uninstall_completion_bridge();
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
        crate::shell::install_completion_bridge(&shell);

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let mut completer = NiubashCompleter::new(state);
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
        {
            let mut state = state.lock().unwrap();
            state.aliases.insert("niu_specific_command".to_string());
            state
                .env_vars
                .insert("NIU_SPECIFIC_VAR".to_string(), "1".to_string());
        }
        let mut completer = NiubashCompleter::new(state);

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
