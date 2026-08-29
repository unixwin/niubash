// Custom completer for WinSH
// Integrates command, path, and variable completion

use crate::completion::external::{CommandCompletionPlugin, CommandDef, ExternalCompletionPlugin};
use crate::completion::path::PathCompleter;
use crate::completion::variables::VariableCompleter;
use crate::completion::{
    CompletionBehavior, CompletionContext, CompletionPlugin, CompletionResult,
};
use reedline::{Completer, Span, Suggestion};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// State shared with completer
pub struct CompletionState {
    pub current_dir: PathBuf,
    pub env_vars: HashMap<String, String>,
    pub aliases: HashSet<String>,
    pub functions: HashSet<String>,
    pub behavior: CompletionBehavior,
    /// Registered completion plugins (e.g. command completion, external tool completion)
    pub plugins: Vec<Arc<dyn CompletionPlugin>>,
}

impl CompletionState {
    pub fn new(current_dir: PathBuf) -> Self {
        Self {
            current_dir,
            env_vars: HashMap::new(),
            aliases: HashSet::new(),
            functions: HashSet::new(),
            behavior: CompletionBehavior::default(),
            plugins: Vec::new(),
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
        self.add_plugin(Arc::new(external));
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
        }
    }

    /// Complete input
    fn complete_input(&mut self, input: &str, cursor_pos: usize) -> Vec<Suggestion> {
        let (current_dir, env_vars, aliases, functions, behavior, plugins) =
            if let Ok(state) = self.state.lock() {
                (
                    state.current_dir.clone(),
                    state.env_vars.clone(),
                    state.aliases.clone(),
                    state.functions.clone(),
                    state.behavior,
                    state.plugins.clone(),
                )
            } else {
                return Vec::new();
            };

        let context =
            CompletionContext::with_behavior(current_dir, input.to_string(), cursor_pos, behavior);
        let mut all_suggestions = Vec::new();

        // At command position, also surface matching entries from the current
        // working directory ahead of PATH command matches. This mirrors the
        // Windows-shell expectation that `win` in a folder containing
        // `./niubash/` should offer `./niubash/` first, instead of only showing
        // PATH executables like `winver` or `winrm`.
        let cwd_path_suggestions = self.cwd_path_suggestions_at_command_position(&context);
        let alias_suggestions = self.alias_suggestions_at_command_position(&context, &aliases);
        let function_suggestions =
            self.function_suggestions_at_command_position(&context, &functions);

        // Try each plugin in order; only the first non-None result is used
        for plugin in &plugins {
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
            if context.is_path_completion() {
                if let Ok(Some(result)) = PathCompleter::complete(&context) {
                    all_suggestions.extend(self.format_completions(result, input, cursor_pos));
                }
            }
        }

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

        let entries = match std::fs::read_dir(&context.current_dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };

        let mut candidates: Vec<CwdPathCandidate> = Vec::new();
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if !context.behavior.matches(&file_name, &word) {
                continue;
            }
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if file_name.starts_with('.') && !word.starts_with('.') {
                continue;
            }
            let escaped = shell_escape_path_segment(&file_name);
            let value = if is_dir {
                format!("./{escaped}/")
            } else {
                format!("./{escaped}")
            };
            candidates.push(CwdPathCandidate { is_dir, value });
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
                description: None,
                style: None,
                extra: None,
                span: Span {
                    start: span_start,
                    end: span_end,
                },
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
                description: Some("alias".to_string()),
                style: None,
                extra: None,
                span: Span {
                    start: span_start,
                    end: span_end,
                },
                append_whitespace: true,
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
                description: Some("function".to_string()),
                style: None,
                extra: None,
                span: Span {
                    start: span_start,
                    end: span_end,
                },
                append_whitespace: true,
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

            suggestions.push(Suggestion {
                value: completion.clone(),
                description: description.map(|s| s.to_string()),
                style: None,
                extra: None,
                span: Span {
                    start: span_start,
                    end: span_end,
                },
                append_whitespace: should_append_completion_whitespace(completion),
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

#[derive(Debug, Eq, PartialEq)]
struct CwdPathCandidate {
    is_dir: bool,
    value: String,
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
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        self.complete_input(line, pos)
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
        let temp_dir = unique_temp_dir("niubash-cwd-priority-files");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("winuxfile.txt"), "x").unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(temp_dir.clone())));
        let mut completer = NiubashCompleter::new(state);
        let suggestions = completer.complete("win", 3);

        let file_suggestion = suggestions
            .iter()
            .find(|suggestion| suggestion.value == "./winuxfile.txt")
            .unwrap_or_else(|| panic!("expected cwd file with ./, got {suggestions:?}"));
        assert_eq!(file_suggestion.span.start, 0);
        assert_eq!(file_suggestion.span.end, 3);
        assert!(file_suggestion.append_whitespace);

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

        let file_suggestion = suggestions
            .iter()
            .find(|suggestion| suggestion.value == "./localfile.txt")
            .unwrap_or_else(|| panic!("expected cwd file on empty Tab, got {suggestions:?}"));
        assert!(file_suggestion.append_whitespace);
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
