//! Host implementations of the GNU readline commands that have no
//! editor event in the pinned reedline fork (niubash#765/#766/#767).
//!
//! GNU readline is the contract source (vendored bash 5.3 tree):
//! `lib/readline/funmap.c` carries the canonical command names, the
//! implementations live in `lib/readline/bind.c` (the three dumps,
//! re-read-init-file), `lib/readline/util.c` (tilde-expand),
//! `lib/readline/text.c` (insert-comment, execute-named-command) and
//! `bashline.c` (shell-expand-line, dynamic-complete-history — the two
//! commands bash itself adds with `rl_add_defun`, bashline.c:468/519).
//!
//! The fork's event vocabulary has no variant for these commands, so
//! they ride the host-command channel instead: a key bound to one of
//! them produces `ReedlineEvent::ExecuteHostCommand` with the
//! [`crate::repl::READLINE_HOST_COMMAND_PREFIX`] payload, the editor
//! suspends, and the REPL dispatch in `repl::run_readline_command`
//! performs the command against the live editor and session. This
//! module holds the pure, testable halves: the name registry, the
//! GNU default binding table, the dump line formats, and the word /
//! completion / alias algorithms.

/// The readline commands this module delivers host-side. Every name
/// here maps in `repl::native_widget_event` to the readline host
/// sentinel instead of falling through to the shell-function widget
/// sentinel (a shell function by one of these names is NOT what GNU
/// runs for the key — the readline function is).
pub const HOST_COMMAND_NAMES: &[&str] = &[
    "dump-functions",
    "dump-macros",
    "dump-variables",
    "dynamic-complete-history",
    "execute-named-command",
    "re-read-init-file",
    "shell-expand-line",
    "tilde-expand",
];

/// GNU default bindings for the commands of this batch, in readline
/// keyseq spelling, single source of truth: the keymap builder turns
/// the one-key entries into keymap bindings and the multi-key entry
/// into a chord, and `dump-functions` reports these spellings.
///
/// Sources: `lib/readline/emacs_keymap.c` (M-~ and M-& are both
/// `rl_tilde_expand`, lines 359/459; M-# is `rl_insert_comment`;
/// M-x is `rl_execute_named_command`, line 451; C-x C-r is
/// `rl_re_read_init_file`), `bashline.c:527` (M-C-e is
/// shell-expand-line), `bashline.c:591` (M-TAB — spelled `\M-\C-i`,
/// TAB and C-i being the same key — is dynamic-complete-history).
/// `insert-comment` itself was delivered in the niubash#731 line;
/// its GNU default chord joins the table here so the dump and the
/// keymap agree.
pub const GNU_DEFAULT_COMMAND_BINDINGS: &[(&str, &str)] = &[
    ("\\M-&", "tilde-expand"),
    ("\\M-~", "tilde-expand"),
    ("\\M-#", "insert-comment"),
    ("\\M-x", "execute-named-command"),
    ("\\M-\\C-e", "shell-expand-line"),
    ("\\M-\\C-i", "dynamic-complete-history"),
    ("\\C-x\\C-r", "re-read-init-file"),
];

/// Names `repl::native_widget_event` delivers natively that the
/// engine's `READLINE_FUNCTIONS` table does not list (aliases and
/// the niubash#731 widening). Together with the engine table and
/// [`HOST_COMMAND_NAMES`] this is the candidate set
/// `execute-named-command` completes over and validates against —
/// the host's funmap, mirroring `rl_funmap_names` (funmap.c).
pub const EXTRA_NATIVE_NAMES: &[&str] = &[
    "beginning-of-buffer",
    "character-search",
    "character-search-backward",
    "clear-display",
    "complete",
    "complete-word",
    "digit-argument",
    "edit-and-execute-command",
    "end-of-buffer",
    "expand-or-complete",
    "history-search-backward",
    "history-search-forward",
    "insert-comment",
    "redraw-current-line",
    "start-kbd-macro",
    "end-kbd-macro",
    "suspend",
    "tab-insert",
    "universal-argument",
    "vi-filename-rubout",
    "yank-last-arg",
    "yank-nth-arg",
];

/// The full host funmap: engine `READLINE_FUNCTIONS` + the host
/// commands + the extra native names, sorted and deduplicated
/// (`rl_funmap_names` returns its names sorted, funmap.c).
pub fn funmap_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    names.extend_from_slice(rubash::builtins::bind::READLINE_FUNCTIONS);
    names.extend_from_slice(HOST_COMMAND_NAMES);
    names.extend_from_slice(EXTRA_NATIVE_NAMES);
    names.sort_unstable();
    names.dedup();
    names
}

/// One `dump-functions` line for NAME with invoking keyseqs SEQS,
/// mirroring `rl_print_keybinding` in its non-readable form
/// (lib/readline/bind.c:2747-2796): an unbound name reports
/// `NAME is not bound to any keys` (no trailing period); bound names
/// list at most five seqs, with `...` when more exist.
pub fn format_dump_function_line(name: &str, seqs: &[String]) -> String {
    if seqs.is_empty() {
        return format!("{name} is not bound to any keys");
    }
    let mut out = format!("{name} can be found on ");
    let shown = seqs.len().min(5);
    for (i, seq) in seqs.iter().take(shown).enumerate() {
        out.push_str(&format!("\"{seq}\""));
        if i + 1 < shown || seqs.len() > shown {
            out.push_str(", ");
        }
    }
    if seqs.len() > shown {
        out.push_str("...");
    } else {
        out.push('.');
    }
    out
}

/// The word span `rl_tilde_expand` (lib/readline/util.c:181-232)
/// operates on, as byte offsets `[start, end)`: the whitespace-
/// delimited word at POINT. GNU scans back from the point over
/// non-whitespace, then forward over non-whitespace; `whitespace`
/// in readline is space or tab (chardefs.h). Returns the span only
/// when the word starts with `~` — otherwise the command does
/// nothing.
pub fn tilde_word_span(buffer: &str, point: usize) -> Option<(usize, usize)> {
    let bytes = buffer.as_bytes();
    let point = point.min(bytes.len());
    let is_ws = |b: u8| b == b' ' || b == b'\t';
    // GNU: end = rl_point; start = end - 1, walked back while the
    // char is not whitespace, then start++ past the whitespace.
    let mut start = point;
    if start == bytes.len() && start > 0 && bytes[start - 1] == b'~' {
        // The util.c fast path: point at end on a `~`.
        start -= 1;
    } else {
        while start > 0 && !is_ws(bytes[start - 1]) {
            start -= 1;
        }
    }
    let mut end = start;
    while end < bytes.len() && !is_ws(bytes[end]) {
        end += 1;
    }
    if start < end && bytes[start] == b'~' {
        Some((start, end))
    } else {
        None
    }
}

/// The current completion word span for history completion: scan
/// back from POINT over characters outside GNU's default
/// `rl_basic_word_break_characters` set, and forward from POINT to
/// the next break character (complete.c's word-finding rule).
/// Returns byte offsets `[start, end)` plus the word text.
pub fn completion_word_span(buffer: &str, point: usize) -> (usize, usize) {
    const BREAKS: &[u8] = b" \t\n\"\\'`@$><=;|&{(";
    let bytes = buffer.as_bytes();
    let point = point.min(bytes.len());
    let mut start = point;
    while start > 0 && !BREAKS.contains(&bytes[start - 1]) {
        start -= 1;
    }
    let mut end = point;
    while end < bytes.len() && !BREAKS.contains(&bytes[end]) {
        end += 1;
    }
    (start, end)
}

/// Split one history entry into completion tokens the way
/// `history_tokenize` (lib/readline/histexpand.c) feeds
/// `build_history_completion_array` (bashline.c:3740-3779):
/// whitespace separates tokens outside quotes; quoted sections
/// stay glued to their token and the quote characters themselves
/// are dropped (the tokenizer's tokens are the words a command
/// line is made of).
pub fn history_tokens(entry: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in entry.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    escaped = true;
                } else {
                    current.push(c);
                }
            }
            None => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                } else if c == '\\' {
                    escaped = true;
                } else if c.is_whitespace() {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                } else {
                    current.push(c);
                }
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// All history-completion candidates for WORD, per
/// `history_completion_generator` (bashline.c:3782-3807): every
/// token of every history entry (walked newest first) is collected,
/// the list is sorted, and the generator yields the tokens that
/// start with WORD. The returned list is sorted and deduplicated —
/// the completion machinery presents each distinct match once.
pub fn history_completion_matches(entries: &[String], word: &str) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    for entry in entries.iter().rev() {
        all.extend(history_tokens(entry));
    }
    all.sort();
    all.into_iter()
        .filter(|token| token.starts_with(word))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Expand the first word of an alias VALUE while it names another
/// alias (the chain `alias_expand` follows until the first word is
/// not an alias or repeats a name already expanded in this chain).
/// Returns the expanded text and whether the last alias value used
/// ended in a blank — the flag that makes the word AFTER the whole
/// expansion an alias candidate as well.
fn expand_alias_chain(
    value: &str,
    lookup: &impl Fn(&str) -> Option<String>,
    seen: &mut Vec<String>,
) -> (String, bool) {
    let mut current = value.to_string();
    let mut ends_blank = value.ends_with(' ') || value.ends_with('\t');
    loop {
        let lead = current.len() - current.trim_start_matches([' ', '\t']).len();
        let rest = &current[lead..];
        let wend = rest.find([' ', '\t']).unwrap_or(rest.len());
        let first = &rest[..wend];
        if first.is_empty() || seen.iter().any(|s| s == first) {
            break;
        }
        let Some(next) = lookup(first) else { break };
        seen.push(first.to_string());
        ends_blank = next.ends_with(' ') || next.ends_with('\t');
        current = format!("{}{}{}", &current[..lead], next, &rest[wend..]);
    }
    (current, ends_blank)
}

/// The longest common prefix of a non-empty match list (the text
/// `rl_complete_internal` inserts when several matches agree).
pub fn common_prefix(matches: &[String]) -> String {
    let Some(first) = matches.first() else {
        return String::new();
    };
    let mut end = first.len();
    for other in &matches[1..] {
        let shared = first
            .chars()
            .zip(other.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum::<usize>()
            .min(end);
        end = shared;
        if end == 0 {
            break;
        }
    }
    first[..end].to_string()
}

/// One line of GNU history expansion (`histexpand.c`
/// `history_expand`), the first step of `shell_expand_line`
/// (bashline.c:2989-2991): ENTRIES are the session history, oldest
/// first, addressed from base 1 like the engine's own snapshot
/// (`HistSnapshot`). HISTCHARS is the session's `histchars` value
/// (`!^#` by default) and POSIX the session's posix mode — the
/// same context the engine builds for its interactive lines.
/// Returns `None` on an expansion error (GNU status -1: the
/// command fails and the line stays as it was).
pub fn history_expand_line(
    line: &str,
    entries: &[String],
    histchars: Option<&str>,
    posix: bool,
) -> Option<String> {
    struct Lookup<'a> {
        entries: &'a [String],
    }
    impl rubash::history_expand::HistLookup for Lookup<'_> {
        fn base(&self) -> usize {
            1
        }
        fn length(&self) -> usize {
            self.entries.len()
        }
        fn get(&self, offset: usize) -> Option<&str> {
            if offset >= 1 {
                self.entries.get(offset - 1).map(String::as_str)
            } else {
                None
            }
        }
    }
    let mut chars = rubash::history_expand::HistChars::default();
    if let Some(value) = histchars {
        let mut it = value.chars();
        if let Some(c) = it.next() {
            chars.expand = c;
        }
        if let Some(c) = it.next() {
            chars.subst = c;
        }
        if let Some(c) = it.next() {
            chars.comment = c;
        }
    }
    let ctx = rubash::history_expand::HistCtx { chars, posix };
    let mut state = rubash::history_expand::HistEngineState::default();
    let result = rubash::history_expand::history_expand(line, &Lookup { entries }, &mut state, ctx);
    if result.status < 0 {
        None
    } else {
        Some(result.text)
    }
}

/// Alias expansion of the command-position words of LINE, the
/// observable contract of bash's `alias_expand` (alias.c) as
/// `shell_expand_line` applies it (bashline.c:3003-3011): the
/// first word of the line and the first word after each unquoted
/// command separator (`;`, `|`, `&`, `(`, newline) are replaced
/// while they name an alias; an alias value ending in a blank
/// makes the following word eligible too; a word already expanded
/// in the current chain is not re-expanded (the loop guard).
/// Quoted words are never alias candidates. This is the host's
/// line-level pass — the engine keeps its own parse-time pass for
/// executed lines; the two agree on these shapes by construction
/// of the alias table they share.
pub fn expand_alias_positions(line: &str, aliases: &[(String, String)]) -> String {
    if aliases.is_empty() || line.is_empty() {
        return line.to_string();
    }
    let lookup = |word: &str| {
        aliases
            .iter()
            .find(|(name, _)| name == word)
            .map(|(_, value)| value.clone())
    };
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;
    // Whether the next word position is in command position.
    let mut command_position = true;
    // Whether the previous alias value ended in a blank, which
    // puts the next word in command position too.
    let mut expand_next = false;
    let mut seen: Vec<String> = Vec::new();
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '\'' || c == '"' || c == '\\' {
            // Quoted/escaped stretch: copy verbatim, never a
            // candidate span.
            let start = i;
            if c == '\\' {
                i = (i + 2).min(bytes.len());
            } else {
                i += 1;
                while i < bytes.len() && bytes[i] as char != c {
                    if c == '"' && bytes[i] == b'\\' {
                        i = (i + 2).min(bytes.len());
                    } else {
                        i += 1;
                    }
                }
                i = (i + 1).min(bytes.len());
            }
            out.push_str(&line[start..i]);
            command_position = false;
            expand_next = false;
            continue;
        }
        if c == ' ' || c == '\t' {
            out.push(c);
            i += 1;
            continue;
        }
        if matches!(c, ';' | '|' | '&' | '(' | '\n') {
            out.push(c);
            i += 1;
            command_position = true;
            expand_next = false;
            seen.clear();
            continue;
        }
        // A word: [i, end) over non-separator, non-blank bytes.
        let start = i;
        while i < bytes.len() {
            let b = bytes[i] as char;
            if matches!(
                b,
                ' ' | '\t' | ';' | '|' | '&' | '(' | ')' | '\n' | '\'' | '"' | '\\'
            ) {
                break;
            }
            i += 1;
        }
        let word = &line[start..i];
        if (command_position || expand_next) && !seen.iter().any(|s| s == word) {
            if let Some(value) = lookup(word) {
                seen.push(word.to_string());
                let (expanded, ends_blank) = expand_alias_chain(&value, &lookup, &mut seen);
                expand_next = ends_blank;
                out.push_str(&expanded);
                command_position = false;
                continue;
            }
        }
        out.push_str(word);
        command_position = false;
        expand_next = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue765_dump_function_line_formats_match_gnu() {
        // bind.c rl_print_keybinding, non-readable form.
        assert_eq!(
            format_dump_function_line("abort", &[]),
            "abort is not bound to any keys"
        );
        assert_eq!(
            format_dump_function_line("abort", &["\\C-g".to_string()]),
            "abort can be found on \"\\C-g\"."
        );
        assert_eq!(
            format_dump_function_line("self-insert", &["a".into(), "b".into(), "c".into()]),
            "self-insert can be found on \"a\", \"b\", \"c\"."
        );
        let seven: Vec<String> = (0..7).map(|n| format!("\\C-{n}")).collect();
        assert_eq!(
            format_dump_function_line("x", &seven),
            "x can be found on \"\\C-0\", \"\\C-1\", \"\\C-2\", \"\\C-3\", \"\\C-4\", ..."
        );
    }

    #[test]
    fn issue765_funmap_names_cover_the_batch_sorted() {
        let names = funmap_names();
        for name in HOST_COMMAND_NAMES {
            assert!(names.contains(name), "{name} must be in the funmap");
        }
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(
            names, sorted,
            "funmap names are sorted like rl_funmap_names"
        );
        assert!(names.contains(&"insert-comment"));
        assert!(names.contains(&"re-read-init-file"));
    }

    #[test]
    fn issue766_tilde_word_span_follows_util_c() {
        // Point at the end of a bare tilde word.
        assert_eq!(tilde_word_span("cd ~", 4), Some((3, 4)));
        // A ~user word mid-line, point inside it.
        assert_eq!(tilde_word_span("ls ~root/x ok", 6), Some((3, 10)));
        // Point at end-of-buffer on the tilde itself.
        assert_eq!(tilde_word_span("~", 1), Some((0, 1)));
        // Non-tilde words do nothing.
        assert_eq!(tilde_word_span("cd /tmp", 7), None);
        assert_eq!(tilde_word_span("echo a~b", 8), None);
        // Whitespace before the word is the boundary.
        assert_eq!(tilde_word_span("x  ~/a", 6), Some((3, 6)));
    }

    #[test]
    fn issue767_history_completion_matches_gnu_generator() {
        let entries = vec![
            "git status".to_string(),
            "git commit -m \"hello world\"".to_string(),
            "ls ~/docs".to_string(),
        ];
        // Tokens of all entries, sorted, prefix-filtered.
        assert_eq!(
            history_completion_matches(&entries, "git"),
            vec!["git".to_string()]
        );
        assert_eq!(
            history_completion_matches(&entries, "h"),
            vec!["hello".to_string()]
        );
        assert_eq!(
            history_completion_matches(&entries, "wo"),
            vec!["world".to_string()]
        );
        assert_eq!(
            history_completion_matches(&entries, "~"),
            vec!["~/docs".to_string()]
        );
        assert!(history_completion_matches(&entries, "zzz").is_empty());
        assert_eq!(common_prefix(&["foobar".into(), "foobaz".into()]), "fooba");
        assert_eq!(common_prefix(&["only".into()]), "only");
    }

    #[test]
    fn issue767_alias_positions_expand_command_words_only() {
        let aliases = vec![
            ("ll".to_string(), "ls -l".to_string()),
            ("g".to_string(), "git ".to_string()),
            ("st".to_string(), "status".to_string()),
        ];
        assert_eq!(expand_alias_positions("ll /tmp", &aliases), "ls -l /tmp");
        // Not in command position: untouched.
        assert_eq!(expand_alias_positions("echo ll", &aliases), "echo ll");
        // After a separator: command position again.
        assert_eq!(expand_alias_positions("cd /; ll", &aliases), "cd /; ls -l");
        // Trailing-blank alias makes the next word eligible.
        assert_eq!(expand_alias_positions("g st", &aliases), "git status");
        // Alias chains follow the replacement's first word, and a
        // self-referential alias stops at the loop guard.
        let chain = vec![
            ("a".to_string(), "ll -h".to_string()),
            ("ll".to_string(), "ls -l".to_string()),
            ("loop".to_string(), "loop -x".to_string()),
        ];
        assert_eq!(expand_alias_positions("a /tmp", &chain), "ls -l -h /tmp");
        assert_eq!(expand_alias_positions("loop", &chain), "loop -x");
        // Quoted word is never a candidate.
        assert_eq!(expand_alias_positions("'ll' x", &aliases), "'ll' x");
        // Unknown words pass through.
        assert_eq!(expand_alias_positions("nope", &aliases), "nope");
    }

    #[test]
    fn issue767_history_expand_line_uses_session_entries() {
        let entries = vec!["git status".to_string(), "ls /tmp".to_string()];
        assert_eq!(
            history_expand_line("!!", &entries, None, false).as_deref(),
            Some("ls /tmp")
        );
        assert_eq!(
            history_expand_line("!git", &entries, None, false).as_deref(),
            Some("git status")
        );
        assert_eq!(
            history_expand_line("echo plain", &entries, None, false).as_deref(),
            Some("echo plain")
        );
        // An unresolvable designator is an error: the line is kept.
        assert_eq!(history_expand_line("!zzz", &entries, None, false), None);
    }

    #[test]
    fn issue767_completion_word_span_uses_readline_breaks() {
        assert_eq!(completion_word_span("git ch", 6), (4, 6));
        assert_eq!(completion_word_span("echo $HO", 8), (6, 8));
        assert_eq!(completion_word_span("ls ~/do", 7), (3, 7));
        // Mid-word point: the span covers the whole word.
        assert_eq!(completion_word_span("checkout", 5), (0, 8));
    }
}
