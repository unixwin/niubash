// Completion module for WinSH
// Provides Tab completion for commands, paths, and variables

pub mod bash_import;
pub mod command;
pub mod completer;
pub mod external;
pub mod path;
pub mod runtime;
pub mod variables;

pub use completer::{CompletionState, WinuxshCompleter};

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionMatchMode {
    Prefix,
    Substring,
}

impl Default for CompletionMatchMode {
    fn default() -> Self {
        Self::Prefix
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompletionBehavior {
    pub case_sensitive: bool,
    pub match_mode: CompletionMatchMode,
    pub max_command_results: Option<usize>,
}

impl CompletionBehavior {
    pub fn matches(&self, candidate: &str, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }

        if self.case_sensitive {
            return match self.match_mode {
                CompletionMatchMode::Prefix => candidate.starts_with(query),
                CompletionMatchMode::Substring => candidate.contains(query),
            };
        }

        let candidate = candidate.to_ascii_lowercase();
        let query = query.to_ascii_lowercase();
        match self.match_mode {
            CompletionMatchMode::Prefix => candidate.starts_with(&query),
            CompletionMatchMode::Substring => candidate.contains(&query),
        }
    }
}

/// Completion context
pub struct CompletionContext {
    /// Current working directory
    pub current_dir: PathBuf,
    /// Current input line
    pub input: String,
    /// Cursor position in input
    pub cursor_pos: usize,
    /// User-configurable matching behavior.
    pub behavior: CompletionBehavior,
}

/// Shell word under the cursor, parsed only for completion purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellWord {
    pub start: usize,
    pub end: usize,
    pub raw: String,
    pub value: String,
    pub quote: Option<char>,
}

impl CompletionContext {
    pub fn new(current_dir: PathBuf, input: String, cursor_pos: usize) -> Self {
        Self::with_behavior(
            current_dir,
            input,
            cursor_pos,
            CompletionBehavior::default(),
        )
    }

    pub fn with_behavior(
        current_dir: PathBuf,
        input: String,
        cursor_pos: usize,
        behavior: CompletionBehavior,
    ) -> Self {
        Self {
            current_dir,
            input,
            cursor_pos,
            behavior,
        }
    }

    /// Get the word under cursor
    pub fn get_current_word(&self) -> Option<String> {
        self.get_current_shell_word().map(|word| word.value)
    }

    /// Get the shell word under cursor, including raw span and unescaped value.
    pub fn get_current_shell_word(&self) -> Option<ShellWord> {
        let (segment_start, segment) = self.current_segment_before_cursor();
        if segment_ends_with_unescaped_whitespace(segment) {
            return None;
        }
        shell_words_in_segment(segment, segment_start).pop()
    }

    /// Get the replacement span for the current shell word.
    pub fn current_word_span(&self) -> Option<(usize, usize)> {
        self.get_current_shell_word()
            .map(|word| (word.start, word.end))
    }

    /// Check if cursor is at command position (first word or after separator)
    pub fn is_command_position(&self) -> bool {
        let (_, segment) = self.current_segment_before_cursor();
        if segment.trim().is_empty() {
            return true;
        }

        let words = shell_words_in_segment(segment, 0);
        words.is_empty() || (words.len() == 1 && !segment_ends_with_unescaped_whitespace(segment))
    }

    /// Check if current word is a path (contains / or \ or starts with .)
    pub fn is_path_completion(&self) -> bool {
        let Some(word) = self.get_current_word() else {
            return !self.is_command_position();
        };

        // Flag prefixes are never path completions.
        if word.starts_with('-') {
            return false;
        }
        // Explicit path indicators.
        if word.contains('/') || word.contains('\\') || word.starts_with('.') {
            return true;
        }
        // If not at command position, treat as path.
        if !self.is_command_position() {
            return true;
        }
        false
    }

    /// Check if current word is a variable (starts with $)
    pub fn is_variable_completion(&self) -> bool {
        if let Some(word) = self.get_current_word() {
            word.starts_with('$')
        } else {
            false
        }
    }

    /// Get the command name at the start of the current input line segment
    pub fn get_command_name(&self) -> Option<String> {
        let (segment_start, segment) = self.current_segment_before_cursor();
        shell_words_in_segment(segment, segment_start)
            .first()
            .map(|word| word.value.clone())
    }

    /// Get the token immediately before the current word (the previous token)
    pub fn get_prev_token(&self) -> Option<String> {
        let (segment_start, segment) = self.current_segment_before_cursor();
        let words = shell_words_in_segment(segment, segment_start);
        if words.is_empty() {
            return None;
        }
        if segment_ends_with_unescaped_whitespace(segment) {
            return words.last().map(|word| word.value.clone());
        }
        if words.len() >= 2 {
            return words.get(words.len() - 2).map(|word| word.value.clone());
        }
        None
    }

    fn current_segment_before_cursor(&self) -> (usize, &str) {
        let pos = self.cursor_pos.min(self.input.len());
        let pos = floor_char_boundary(&self.input, pos);
        let before_cursor = &self.input[..pos];
        let segment_start = last_command_separator(before_cursor)
            .map(|idx| ceil_char_boundary(before_cursor, idx + 1))
            .unwrap_or(0);
        (segment_start, &before_cursor[segment_start..])
    }
}

/// Trait for pluggable completion providers.
/// Implementations must be Send + Sync so they can be stored behind Arc<Mutex<>>.
pub trait CompletionPlugin: Send + Sync {
    /// Identifier for this plugin (used in logs and config)
    fn name(&self) -> &str;

    /// Attempt to complete the current context.
    /// Return None to pass through to the next plugin.
    fn complete(&self, context: &CompletionContext) -> Option<CompletionResult>;

    /// Called once when the shell starts up (e.g. warm caches)
    fn on_init(&self) {}

    /// Called after every command execution (e.g. invalidate caches on cd)
    fn on_command_executed(&self, _cmd: &str) {}

    /// Called when the working directory changes
    fn on_directory_changed(&self, _new_dir: &std::path::Path) {}

    /// Upcast to Any for downcasting in CompletionState.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Completion result
pub struct CompletionResult {
    /// Completions
    pub completions: Vec<String>,
    /// Per-completion description (parallel to `completions`; None = no description)
    pub descriptions: Vec<Option<String>>,
    /// Common prefix (for partial completion)
    pub common_prefix: Option<String>,
}

impl CompletionResult {
    pub fn new(completions: Vec<String>) -> Self {
        let common_prefix = Self::find_common_prefix(&completions);
        let len = completions.len();
        Self {
            completions,
            descriptions: vec![None; len],
            common_prefix,
        }
    }

    pub fn with_descriptions(completions: Vec<String>, descriptions: Vec<Option<String>>) -> Self {
        let common_prefix = Self::find_common_prefix(&completions);
        Self {
            completions,
            descriptions,
            common_prefix,
        }
    }

    fn find_common_prefix(completions: &[String]) -> Option<String> {
        if completions.is_empty() {
            return None;
        }

        let first = &completions[0];
        let mut prefix_len = first.len();

        for completion in completions.iter().skip(1) {
            while !completion.starts_with(&first[..prefix_len]) && prefix_len > 0 {
                prefix_len -= 1;
            }
            if prefix_len == 0 {
                return None;
            }
        }

        Some(first[..prefix_len].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_current_word() {
        let ctx = CompletionContext::new(
            PathBuf::from("/home/user"),
            "echo hello world".to_string(),
            10,
        );
        assert_eq!(ctx.get_current_word(), Some("hello".to_string()));
    }

    #[test]
    fn completion_behavior_matches_prefix_and_substring_modes() {
        let default_behavior = CompletionBehavior::default();
        assert!(default_behavior.matches("grep", "gre"));
        assert!(default_behavior.matches("grep", "GRE"));
        assert!(!default_behavior.matches("grep", "ep"));

        let substring = CompletionBehavior {
            match_mode: CompletionMatchMode::Substring,
            ..CompletionBehavior::default()
        };
        assert!(substring.matches("grep", "ep"));

        let case_sensitive = CompletionBehavior {
            case_sensitive: true,
            ..CompletionBehavior::default()
        };
        assert!(case_sensitive.matches("grep", "gre"));
        assert!(!case_sensitive.matches("grep", "GRE"));
    }

    #[test]
    fn test_get_current_word_handles_escapes_and_quotes() {
        let escaped =
            CompletionContext::new(PathBuf::from("/home/user"), "ls two\\ w".to_string(), 9);
        let escaped_word = escaped.get_current_shell_word().unwrap();
        assert_eq!(escaped_word.value, "two w");
        assert_eq!(escaped_word.start, 3);
        assert_eq!(escaped.current_word_span(), Some((3, 9)));

        let quoted =
            CompletionContext::new(PathBuf::from("/home/user"), "ls \"two w".to_string(), 9);
        let quoted_word = quoted.get_current_shell_word().unwrap();
        assert_eq!(quoted_word.value, "two w");
        assert_eq!(quoted_word.quote, Some('"'));
        assert_eq!(quoted_word.start, 3);
    }

    #[test]
    fn test_is_command_position_for_partial_and_empty_commands() {
        let empty = CompletionContext::new(PathBuf::from("/home/user"), "".to_string(), 0);
        assert!(empty.is_command_position());

        let partial = CompletionContext::new(PathBuf::from("/home/user"), "gre".to_string(), 3);
        assert!(partial.is_command_position());

        let after_pipe =
            CompletionContext::new(PathBuf::from("/home/user"), "ls | gre".to_string(), 8);
        assert!(after_pipe.is_command_position());

        let arg = CompletionContext::new(PathBuf::from("/home/user"), "echo gre".to_string(), 8);
        assert!(!arg.is_command_position());

        let escaped_arg =
            CompletionContext::new(PathBuf::from("/home/user"), "echo two\\ w".to_string(), 11);
        assert!(!escaped_arg.is_command_position());

        let quoted_pipe = CompletionContext::new(
            PathBuf::from("/home/user"),
            "echo \"|\" | gre".to_string(),
            14,
        );
        assert!(quoted_pipe.is_command_position());
    }

    #[test]
    fn test_is_path_completion() {
        let ctx =
            CompletionContext::new(PathBuf::from("/home/user"), "cat /tmp/fil".to_string(), 12);
        assert!(ctx.is_path_completion());
    }

    #[test]
    fn test_blank_argument_position_is_path_completion() {
        let arg = CompletionContext::new(PathBuf::from("/home/user"), "ls ".to_string(), 3);
        assert!(arg.is_path_completion());

        let after_pipe =
            CompletionContext::new(PathBuf::from("/home/user"), "ls | ".to_string(), 5);
        assert!(!after_pipe.is_path_completion());
        assert!(after_pipe.is_command_position());
    }

    #[test]
    fn test_is_variable_completion() {
        let ctx = CompletionContext::new(PathBuf::from("/home/user"), "echo $HOM".to_string(), 9);
        assert!(ctx.is_variable_completion());
    }

    #[test]
    fn test_prev_token_handles_escaped_values() {
        let value = CompletionContext::new(
            PathBuf::from("/home/user"),
            "cmd --flag two\\ w".to_string(),
            17,
        );
        assert_eq!(value.get_prev_token(), Some("--flag".to_string()));

        let blank =
            CompletionContext::new(PathBuf::from("/home/user"), "cmd --flag ".to_string(), 11);
        assert_eq!(blank.get_prev_token(), Some("--flag".to_string()));
    }
}

// ─── Char-boundary helpers ────────────────────────────────────────────────────

/// Round `pos` down to the nearest UTF-8 char boundary in `s`.
/// If `pos >= s.len()` returns `s.len()`.
fn floor_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    // Walk backwards until we land on a char boundary
    let mut p = pos;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Round `pos` up to the nearest UTF-8 char boundary in `s`.
/// If `pos >= s.len()` returns `s.len()`.
fn ceil_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut p = pos;
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p
}

fn last_command_separator(input: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    let mut last = None;

    for (idx, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        if matches!(ch, ';' | '|' | '&' | '\n') {
            last = Some(idx);
        }
    }

    last
}

fn shell_words_in_segment(segment: &str, offset: usize) -> Vec<ShellWord> {
    let mut words = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut escaped = false;

    for (idx, ch) in segment.char_indices() {
        if start.is_none() {
            if ch.is_whitespace() {
                continue;
            }
            start = Some(idx);
        }

        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() {
            if let Some(start_idx) = start.take() {
                words.push(make_shell_word(segment, offset, start_idx, idx));
            }
        }
    }

    if let Some(start_idx) = start {
        words.push(make_shell_word(segment, offset, start_idx, segment.len()));
    }

    words
}

fn make_shell_word(segment: &str, offset: usize, start: usize, end: usize) -> ShellWord {
    let raw = segment[start..end].to_string();
    let quote = raw.chars().next().filter(|ch| *ch == '\'' || *ch == '"');
    ShellWord {
        start: offset + start,
        end: offset + end,
        value: unescape_shell_word(&raw),
        raw,
        quote,
    }
}

fn unescape_shell_word(raw: &str) -> String {
    let mut value = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in raw.chars() {
        if escaped {
            value.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                value.push(ch);
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        value.push(ch);
    }

    if escaped {
        value.push('\\');
    }
    value
}

fn segment_ends_with_unescaped_whitespace(segment: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    let mut ends_with_whitespace = false;

    for ch in segment.chars() {
        if escaped {
            escaped = false;
            ends_with_whitespace = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            ends_with_whitespace = false;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            ends_with_whitespace = false;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            ends_with_whitespace = false;
            continue;
        }
        ends_with_whitespace = ch.is_whitespace();
    }

    ends_with_whitespace
}
