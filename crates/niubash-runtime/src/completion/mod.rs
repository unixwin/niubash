// Completion module for WinSH
// Provides Tab completion for commands, paths, and variables

pub mod bash_import;
pub mod brace;
pub mod column_menu;
pub mod command;
pub mod completer;
pub mod cycle_menu;
pub mod external;
pub mod file_colors;
pub mod gnu_menu;
pub mod insert_menu;
pub mod menu_open;
pub mod path;
pub mod sources;
pub mod variables;
pub mod winuxcmd_assets;

pub(crate) use completer::CompletionHostFaces;
pub use completer::{CommandProbe, CompletionState, NiubashCompleter, SpecificCompletion};
pub use file_colors::FileColors;
/// The engine's candidate-kind vocabulary (niubash#863 GAP-11):\n/// the host's own `CandidateKind` is deleted — filesystem classes
/// are named by the engine type everywhere (`ListedKind` stays:
/// dirent truth is a different thing from candidate kind).
/// The ledger §6 spells this `rubash::api::CandidateKind`, and the
/// GAP-13 entity lane (rubash#2850) added the one-line re-export, so
/// the host consumes the re-export path — the checker matches the
/// consumed path against the recorded one exactly.
pub use rubash::api::CandidateKind;

use std::path::PathBuf;

use rubash::api::{TokenKindView, TokenSpan, TokenView};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionMatchMode {
    #[default]
    Prefix,
    Substring,
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
    /// The session's open fd numbers at capture time, from the
    /// engine's `Session::completion_view` (GAP-4, niubash#863;
    /// niubash#627). The path completer's dynamic tier lists these
    /// under `/dev/fd/`.
    pub open_fds: Vec<u32>,
    /// The backing directory for the session's `/tmp` at capture
    /// time, resolved by the engine from the session env (session
    /// TMPDIR wins) through the resolver face (GAP-5's object;
    /// niubash#627 G2). The path completer's logical resolver
    /// roots `/tmp` spellings here. Contexts built without session
    /// state keep the process temp dir — the fallback for the
    /// stateless case only.
    pub tmp_base: PathBuf,
    /// The session's parsed `LS_COLORS` table (niubash#743), resolved
    /// by the completer from the captured session env (process-env
    /// fallback, GNU defaults when unset). Filesystem producers
    /// (`path`, the cwd command-slot overlay) paint candidates through
    /// it; `None` leaves candidates unstyled.
    pub file_colors: Option<FileColors>,
    /// The session's directory-listing cache (niubash#747), lent by
    /// the completer so the path completer and the cwd command-slot
    /// overlay reuse one stat-free enumeration across a completion
    /// session's re-queries. Contexts built outside the completer
    /// keep `None` and enumerate uncached, exactly as before.
    pub dir_cache: Option<crate::completion::path::DirCacheHandle>,
    /// The session's `PATH` at capture time, from the captured
    /// session env (niubash#839). Producers that must
    /// resolve a tool the way the SESSION would — the dynamic
    /// completion fingerprint in `external.rs` — probe this value,
    /// not the process `PATH`: a tool that exists only on the session
    /// PATH (an interactive `export PATH=...` the process never saw)
    /// is otherwise invisible to the fingerprint while `Command::new`
    /// still resolves it at spawn. Contexts built without session
    /// state keep `None` and fall back to the process env.
    pub session_path: Option<String>,
    /// The session's `PATHEXT` at snapshot time (niubash#845), the
    /// companion of `session_path` for Windows tool resolution.
    /// `None` outside a session snapshot, with the same process-env
    /// fallback discipline.
    pub session_pathext: Option<String>,
    /// The engine token view of `input` (GAP-9, niubash#863),
    /// captured by the completer from the open attempt's session
    /// (`Session::token_view`, derived in the session's posix
    /// mode). Contexts built without a session leave it `None`;
    /// their lexical methods then derive the view from the
    /// module's stateless oracle session (default posix mode —
    /// the contract the retired host segment machinery served,
    /// since it had no posix notion either). Only
    /// session-carrying contexts observe a non-default mode.
    pub token_view: Option<TokenView>,
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
            open_fds: Vec::new(),
            tmp_base: std::env::temp_dir(),
            file_colors: None,
            dir_cache: None,
            session_path: None,
            session_pathext: None,
            token_view: None,
        }
    }

    /// The cursor as a byte offset into `input`, clamped to the
    /// input and rounded down to a char boundary.
    fn cursor(&self) -> usize {
        let pos = self.cursor_pos.min(self.input.len());
        floor_char_boundary(&self.input, pos)
    }

    /// The token view this context's lexical methods read
    /// (GAP-9, niubash#863): the view the completer captured
    /// from the open attempt's session when it built this
    /// context; for session-less contexts, the stateless
    /// oracle's view of `input`.
    fn view(&self) -> TokenView {
        self.token_view
            .clone()
            .unwrap_or_else(|| stateless_token_view(&self.input))
    }

    /// Get the word under cursor
    pub fn get_current_word(&self) -> Option<String> {
        self.get_current_shell_word().map(|word| word.value)
    }

    /// Get the shell word under cursor, including raw span and unescaped value.
    pub fn get_current_shell_word(&self) -> Option<ShellWord> {
        self.view().word_at(self.cursor()).map(shell_word_from_span)
    }

    /// Get the replacement span for the current shell word.
    pub fn current_word_span(&self) -> Option<(usize, usize)> {
        self.get_current_shell_word()
            .map(|word| (word.start, word.end))
    }

    /// Check if cursor is at command position (first word or after separator)
    pub fn is_command_position(&self) -> bool {
        let view = self.view();
        let cursor = self.cursor();
        let words = view.segment_words_before(cursor);
        if words.is_empty() {
            return true;
        }
        words.len() == 1 && !segment_ends_with_whitespace(&view, cursor)
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
        let view = self.view();
        let words = view.segment_words_before(self.cursor());
        // niubash#830: the command word is not always the segment's
        // first word — grouping openers (`( git …`, `{ git …`) and
        // environment assignment prefixes (`VAR=1 git …`) precede it.
        // Skip those so callers (the compspec probe, the external
        // plugin's definition lookup, the path completer's
        // directory-only check) name the command GNU would name. An
        // assignment with no following word still reports itself:
        // the line names no command yet. (GAP-9: the skip facts are
        // the token view's — the assignment shape is the span's
        // projected kind, folded by the engine's own lexer.)
        for (index, word) in words.iter().enumerate() {
            let value = word.value.trim_start_matches('(');
            if value.is_empty() || value == "{" {
                continue;
            }
            if word.kind == TokenKindView::Assignment && index + 1 < words.len() {
                continue;
            }
            return Some(value.to_string());
        }
        None
    }

    /// All words of the current command segment before the cursor, in
    /// order, with quoting/escaping already resolved (niubash#851):
    /// the segment starts after the last top-level separator, so
    /// words from an earlier command on the line never leak in, and
    /// a quoted word yields its dequoted value. (GAP-9: the words
    /// are the token view's segment derivation.)
    pub fn current_segment_words(&self) -> Vec<String> {
        self.view()
            .segment_words_before(self.cursor())
            .into_iter()
            .map(|span| span.value.clone())
            .collect()
    }

    /// Get the token immediately before the current word (the previous token)
    pub fn get_prev_token(&self) -> Option<String> {
        let view = self.view();
        let cursor = self.cursor();
        let words = view.segment_words_before(cursor);
        if words.is_empty() {
            return None;
        }
        if segment_ends_with_whitespace(&view, cursor) {
            return words.last().map(|word| word.value.clone());
        }
        if words.len() >= 2 {
            return words.get(words.len() - 2).map(|word| word.value.clone());
        }
        None
    }
}

/// The replacement span for the word at CURSOR in INPUT, using the
/// same shell-word parser the completer uses for its suggestions.
/// A cursor after whitespace has an empty span at the cursor.
pub fn completion_span(input: &str, cursor: usize) -> (usize, usize) {
    let cursor = cursor.min(input.len());
    CompletionContext::new(PathBuf::new(), input.to_string(), cursor)
        .current_word_span()
        .unwrap_or((cursor, cursor))
}

/// Fill a [`ShellWord`] from one engine token span (GAP-9,
/// niubash#863): raw = the span's source text, value = its
/// dequoted value, quote = the span's leading quote character.
fn shell_word_from_span(span: &TokenSpan) -> ShellWord {
    ShellWord {
        start: span.start,
        end: span.end,
        raw: span.text.clone(),
        value: span.value.clone(),
        quote: span
            .text
            .chars()
            .next()
            .filter(|ch| *ch == '\'' || *ch == '"'),
    }
}

/// Whether the cursor's segment ends in whitespace before the
/// cursor, read off the token view: the segment's last word
/// ends strictly before the cursor, so only gap text
/// (whitespace, a comment, a continuation) separates it from
/// the cursor — the verdict the retired segment scan's char
/// machine computed by hand.
fn segment_ends_with_whitespace(view: &TokenView, cursor: usize) -> bool {
    view.segment_words_before(cursor)
        .last()
        .is_some_and(|word| word.end < cursor)
}

thread_local! {
    /// Stateless tokenize oracle for session-less contexts
    /// (GAP-9, niubash#863). A context the completer built
    /// under an open attempt carries the attempt session's own
    /// view; every other context (producer probes, unit
    /// contexts) has no session to ask — the GAP-7 settlement
    /// leaves their engine-owned inputs empty. The token view
    /// is the one input that cannot stay empty: the context's
    /// lexical methods ARE the view's derivations. The oracle
    /// is a private session owned by this thread, used only
    /// as a tokenize source in the default posix mode — the
    /// contract the retired host segment machinery served,
    /// since it had no posix notion either. It holds no user
    /// state, never observes the REPL's session, and executes
    /// nothing; it is a tokenize oracle, not a session lend.
    static TOKEN_VIEW_ORACLE: std::cell::RefCell<Option<rubash::api::Session>> =
        const { std::cell::RefCell::new(None) };
}

/// The token view of `text` from the stateless oracle session.
/// Shared with the syntax highlighter (GAP-9, niubash#863),
/// whose shell-less instances have no session of their own.
pub(crate) fn stateless_token_view(text: &str) -> TokenView {
    TOKEN_VIEW_ORACLE.with(|oracle| {
        let mut slot = oracle.borrow_mut();
        let session = slot.get_or_insert_with(|| {
            rubash::api::Session::builder()
                .build()
                .expect("stateless token-view oracle session builds")
        });
        session.token_view(text)
    })
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
    /// Per-completion display style (parallel to `completions`;
    /// None = unstyled). Filesystem producers fill this from the
    /// session's `LS_COLORS` table (niubash#743).
    pub styles: Vec<Option<nu_ansi_term::Style>>,
    /// Common prefix (for partial completion)
    pub common_prefix: Option<String>,
    /// The governing compspec asked for no trailing space after an
    /// accepted match (`-o nospace` → rl_completion_suppress_append,
    /// carried on rubash's CompletionPolicy since rubash#2573). Only
    /// the rubash-engine producer sets this (niubash#837); every
    /// other producer leaves it false and keeps the text-shape rule.
    pub suppress_append: bool,
}

impl CompletionResult {
    pub fn new(completions: Vec<String>) -> Self {
        let common_prefix = Self::find_common_prefix(&completions);
        let len = completions.len();
        Self {
            completions,
            descriptions: vec![None; len],
            styles: vec![None; len],
            common_prefix,
            suppress_append: false,
        }
    }

    pub fn with_descriptions(completions: Vec<String>, descriptions: Vec<Option<String>>) -> Self {
        let common_prefix = Self::find_common_prefix(&completions);
        let len = completions.len();
        Self {
            completions,
            descriptions,
            styles: vec![None; len],
            common_prefix,
            suppress_append: false,
        }
    }

    /// Completions carrying per-candidate paint styles (the path
    /// completer's filesystem classification, niubash#743).
    pub fn with_styles(completions: Vec<String>, styles: Vec<Option<nu_ansi_term::Style>>) -> Self {
        let common_prefix = Self::find_common_prefix(&completions);
        let len = completions.len();
        Self {
            completions,
            descriptions: vec![None; len],
            styles,
            common_prefix,
            suppress_append: false,
        }
    }

    fn find_common_prefix(completions: &[String]) -> Option<String> {
        if completions.is_empty() {
            return None;
        }

        let first = &completions[0];
        let mut prefix_len = first.len();

        for completion in completions.iter().skip(1) {
            while prefix_len > 0 && !completion.starts_with(&first[..prefix_len]) {
                prefix_len -= 1;
                prefix_len = floor_char_boundary(first, prefix_len);
            }
            if prefix_len == 0 {
                return None;
            }
        }

        Some(first[..prefix_len].to_string())
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
    fn test_extglob_pipe_is_not_a_command_separator() {
        // niubash#530: in `ls @(a|b)` the `|` separates extglob
        // alternatives, not commands; the current word is the whole
        // pattern and the command name is still `ls`.
        //
        // GAP-9 (niubash#863): that verdict is only true with
        // `extglob` on — with the option off GNU parses `ls @(a|b)`
        // as a pipeline (`ls @(a | b)`), and the engine's token
        // view reports exactly that (probe: off-state spans `@`,
        // `(`, `a`, `|` Pipe, `b`, `)`; on-state span `@(a|b)`).
        // The old host scanner treated extglob groups as one word
        // unconditionally, ignoring shopt state; the view is the
        // authority now, so this pin carries its precondition
        // explicitly: the context is built from a live session
        // with `shopt -s extglob`. The gate is process-global, so
        // the test takes PROCESS_STATE_LOCK and restores the gate
        // before asserting.
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut shell = crate::shell::Shell::new().unwrap();
        shell.execute_script("shopt -s extglob").unwrap();
        let mut ctx =
            CompletionContext::new(PathBuf::from("/home/user"), "ls @(a|b)".to_string(), 9);
        ctx.token_view = Some(shell.session().token_view("ls @(a|b)"));
        shell.execute_script("shopt -u extglob").unwrap();
        assert_eq!(ctx.get_current_word(), Some("@(a|b)".to_string()));
        assert_eq!(ctx.get_command_name(), Some("ls".to_string()));

        // A real pipe still starts a new segment.
        let piped = CompletionContext::new(PathBuf::from("/home/user"), "ls | gre".to_string(), 8);
        assert!(piped.is_command_position());
        assert_eq!(piped.get_command_name(), Some("gre".to_string()));
    }

    #[test]
    fn test_extglob_off_pipe_is_a_command_separator() {
        // GAP-9 (niubash#863): the extglob-off counterpart of #530.
        // With `extglob` off (the GNU default) the `|` in
        // `ls @(a|b)` IS a pipe: the view reports `b` as the new
        // segment's command word and `)` as the word under the
        // cursor. This is the GNU-faithful verdict the old host
        // scanner's unconditional extglob treatment hid.
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut shell = crate::shell::Shell::new().unwrap();
        shell.execute_script("shopt -u extglob").unwrap();
        let ctx = CompletionContext::new(PathBuf::from("/home/user"), "ls @(a|b)".to_string(), 9);
        assert_eq!(ctx.get_current_word(), Some(")".to_string()));
        assert_eq!(ctx.get_command_name(), Some("b".to_string()));
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
