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
    // util.c:99 `_rl_abort_internal` / util.c:122 `rl_abort`:
    // clear the pending command (numeric argument, mark, pending
    // input, keyboard macro) and ding — the line itself stays.
    "abort",
    // bashline.c:3966/3954/3896 `bash_complete_command` /
    // `bash_complete_variable` / `bash_complete_filename`: the same
    // completion machinery as TAB, pinned to one candidate source.
    "complete-command",
    "complete-filename",
    "complete-variable",
    "dump-functions",
    "dump-macros",
    "dump-variables",
    "dynamic-complete-history",
    "execute-named-command",
    // bashline.c:2902 `history_expand_line`: history expansion of the
    // whole line, without shell-expand-line's alias/word stages.
    "history-expand-line",
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
/// M-x is `rl_execute_named_command`, line 451; C-g and M-C-g are
/// `rl_abort`, lines 42/325), `text.c:1179` (`do-lowercase-version`
/// is a dispatch rule, so it has no entry here), `bashline.c:527`
/// (M-C-e is shell-expand-line), `bashline.c:530` (M-^ is
/// history-expand-line), `bashline.c:555-581` (M-{ / M-! / M-$ /
/// M-/ / M-@ / M-~ / M-g are the completion entry points), and
/// `bashline.c:591` (M-TAB — spelled `\M-\C-i`, TAB and C-i being
/// the same key — is dynamic-complete-history). The three completion
/// modes backed by host sources (`complete-command`,
/// `complete-variable`, `complete-filename`) are listed; the four
/// whose sources the host does not own stay out of the table so they
/// keep their documented gap.
pub const GNU_DEFAULT_COMMAND_BINDINGS: &[(&str, &str)] = &[
    ("\\C-g", "abort"),
    ("\\M-&", "tilde-expand"),
    ("\\M-~", "tilde-expand"),
    ("\\M-#", "insert-comment"),
    ("\\M-x", "execute-named-command"),
    ("\\M-\\C-e", "shell-expand-line"),
    ("\\M-^", "history-expand-line"),
    ("\\M-!", "complete-command"),
    ("\\M-$", "complete-variable"),
    ("\\M-/", "complete-filename"),
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
/// The tokenizer behind GNU's history completion:
/// `history_tokenize` (lib/readline/histexpand.c:1692, via
/// `history_tokenize_internal` :1638 and `history_tokenize_word`
/// :1483) feeds `build_history_completion_array`
/// (bashline.c:3735). Tokens are exact substrings of the entry —
/// quote characters are RETAINED (`"hello world"` is one token,
/// quotes and all) — split the way the shell would split them:
/// whitespace separates, the delimiter characters
/// (`history_word_delimiters`, histexpand.c:52) form their own
/// tokens, quoted spans stay glued, and a `#` at a token start
/// ends the entry (bash sets `history_comment_char` to '#',
/// variables.c:6175).
pub fn history_tokens(entry: &str) -> Vec<String> {
    let bytes = entry.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        // fielddelim (histexpand.c:58): whitespace or newline.
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'#' {
            break;
        }
        let start = i;
        i = history_tokenize_word(bytes, i);
        if i == start {
            // A delimiter the word scanner would not consume:
            // adjacent delimiters make a separate field
            // (histexpand.c:1672-1677).
            i += 1;
            while i < bytes.len() && HISTORY_WORD_DELIMITERS.contains(&bytes[i]) {
                i += 1;
            }
        }
        tokens.push(entry[start..i].to_string());
    }
    tokens
}

/// `history_word_delimiters` (histexpand.c:52).
const HISTORY_WORD_DELIMITERS: &[u8] = b" \t\n;&()|<>";

/// `HISTORY_QUOTE_CHARACTERS` (histexpand.c:53).
const HISTORY_QUOTE_CHARACTERS: &[u8] = b"\"'`";

/// `slashify_in_quotes` (histexpand.c:56).
const SLASHIFY_IN_QUOTES: &[u8] = b"\\`\"$";

/// One word of `history_tokenize_word` (histexpand.c:1483):
/// returns the index one past the word starting at IND.
fn history_tokenize_word(bytes: &[u8], ind: usize) -> usize {
    let at = |p: usize| -> u8 { bytes.get(p).copied().unwrap_or(0) };
    let mut i = ind;
    let mut delimiter = 0u8;
    let mut delimopen = 0u8;
    let mut nestdelim = 0u32;

    if matches!(at(i), b'(' | b')' | b'\n') {
        return i + 1;
    }

    // The C tests the operator set again after the digit scan (a
    // digit run before `<`/`>` is a file descriptor and the
    // operator joins its token), so the two checks are sequential,
    // not alternatives.
    let mut check_operator = true;
    if at(i).is_ascii_digit() {
        let mut j = i;
        while at(j).is_ascii_digit() {
            j += 1;
        }
        if j >= bytes.len() {
            return j;
        }
        i = j;
        check_operator = matches!(at(i), b'<' | b'>');
    }
    if check_operator && matches!(at(i), b'<' | b'>' | b';' | b'&' | b'|') {
        let c = at(i);
        let peek = at(i + 1);
        if peek == c {
            if peek == b'<' && matches!(at(i + 2), b'-' | b'<') {
                i += 1;
            }
            return i + 2;
        } else if peek == b'&' && matches!(c, b'>' | b'<') {
            let mut j = i + 2;
            while at(j).is_ascii_digit() {
                j += 1;
            }
            if at(j) == b'-' {
                j += 1;
            }
            return j;
        } else if (peek == b'>' && c == b'&') || (peek == b'|' && c == b'>') {
            return i + 2;
        } else if peek == b'(' && matches!(c, b'>' | b'<') {
            // Process substitution: a nested word follows.
            i += 2;
            delimopen = b'(';
            delimiter = b')';
            nestdelim = 1;
        } else {
            return i + 1;
        }
    }

    // get_word (histexpand.c:1555): scan the word body.
    if delimiter == 0 && HISTORY_QUOTE_CHARACTERS.contains(&at(i)) {
        delimiter = at(i);
        i += 1;
    }
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' && at(i + 1) == b'\n' {
            i += 2;
            continue;
        }
        // As written in the C, the slashify test examines the
        // backslash itself — a member of slashify_in_quotes — so a
        // backslash outside single quotes always skips the next
        // character (histexpand.c:1568-1575).
        if c == b'\\'
            && delimiter != b'\''
            && (delimiter != b'"' || SLASHIFY_IN_QUOTES.contains(&c))
        {
            i += 1;
            if i >= bytes.len() {
                break;
            }
            i += 1;
            continue;
        }
        if nestdelim > 0 && c == delimopen {
            nestdelim += 1;
            i += 1;
            continue;
        }
        if nestdelim > 0 && c == delimiter {
            nestdelim -= 1;
            if nestdelim == 0 {
                delimiter = 0;
            }
            i += 1;
            continue;
        }
        if delimiter != 0 && c == delimiter {
            delimiter = 0;
            i += 1;
            continue;
        }
        // Command/process substitution and extended globs nest.
        if nestdelim == 0
            && delimiter == 0
            && matches!(c, b'<' | b'>' | b'$' | b'!' | b'@' | b'?' | b'+' | b'*')
            && at(i + 1) == b'('
        {
            i += 1;
            if i + 1 >= bytes.len() {
                break;
            }
            delimopen = b'(';
            delimiter = b')';
            nestdelim = 1;
            i += 1;
            continue;
        }
        if delimiter == 0 && HISTORY_WORD_DELIMITERS.contains(&c) {
            break;
        }
        if delimiter == 0 && HISTORY_QUOTE_CHARACTERS.contains(&c) {
            delimiter = c;
        }
        i += 1;
    }
    i
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

/// The text one completion press inserts for MATCHES, given the word
/// being completed — `rl_complete_internal`'s default (non-listing)
/// outcome: a lone match is inserted whole, with the word-breaking
/// space GNU appends after a complete filename; several matches
/// insert their longest common prefix. `None` means the press made
/// no progress (it dings in GNU).
pub fn completion_replacement(matches: &[String], word: &str) -> Option<String> {
    match matches {
        [] => None,
        [only] => {
            let mut replacement = only.clone();
            let bare = replacement
                .trim_end_matches('"')
                .trim_end_matches('\'')
                .trim_end();
            if !(bare.ends_with('/') || bare.ends_with('\\')) {
                replacement.push(' ');
            }
            Some(replacement)
        }
        many => {
            let prefix = common_prefix(many);
            (prefix != word).then_some(prefix)
        }
    }
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

/// Alias expansion of LINE, a direct port of GNU's
/// `alias_expand` (alias.c:465) with its `skipws` (:353),
/// `rd_token` (:438), and `skipquotes` (:308) — the function
/// `shell_expand_line` applies (bashline.c:3003-3011). The scan is
/// purely textual (byte-wise, like the C), which fixes the
/// observable details:
///
/// - whitespace and quoted/backslash-quoted stretches are copied
///   verbatim by the skip pass and are never alias candidates;
/// - the first token, a token after a command separator
///   (`\r\n;|&(` — including separators the skip pass crossed),
///   and the token after a value that ends in a blank are the
///   only candidates; a token containing a backslash never
///   expands;
/// - the value replaces the token VERBATIM: a trailing-blank
///   value plus the original separating blank yields two blanks
///   (`g st` with `g='git '` gives `git  status`), and the
///   substituted text is NOT rescanned, so a value whose first
///   word names another alias stays as written (`a='ll -h'`
///   gives `ll -h`, not the chain) — both confirmed against the
///   oracle binary's M-C-e. Chain expansion is the parser's
///   business when the line executes, not this function's.
pub fn expand_alias_positions(line: &str, aliases: &[(String, String)]) -> String {
    let lookup = |word: &str| -> Option<&str> {
        aliases
            .iter()
            .find(|(name, _)| name == word)
            .map(|(_, value)| value.as_str())
    };
    let bytes = line.as_bytes();
    let at = |p: usize| -> u8 { bytes.get(p).copied().unwrap_or(0) };
    let command_separator = |c: u8| matches!(c, b'\r' | b'\n' | b';' | b'|' | b'&' | b'(');

    let mut out: Vec<u8> = Vec::with_capacity(line.len());
    let mut i = 0usize;
    let mut command_word = true;
    let mut expand_next = false;
    loop {
        let start = i;
        i = alias_skipws(bytes, i, &mut command_word);
        if start == i && i >= bytes.len() {
            break;
        }
        out.extend_from_slice(&bytes[start..i]);

        let real_start = i;
        if command_separator(at(i)) {
            command_word = true;
        }
        let mut expand_this_token = command_word || expand_next;
        expand_next = false;

        let tok_start = i;
        i = alias_rd_token(bytes, i);
        let mut tl = i - tok_start;
        if tl == 0 && i < bytes.len() {
            // A single-character token, probably a delimiter.
            tl = 1;
            i += 1;
        }
        // Token boundaries fall on ASCII delimiters, so the token
        // is always whole UTF-8; the fallback is defensive only.
        let token = std::str::from_utf8(&bytes[tok_start..tok_start + tl]).unwrap_or("");
        if token.contains('\\') {
            expand_this_token = false;
        }
        let mut expanded = false;
        if !token.is_empty() && expand_this_token {
            if let Some(value) = lookup(token) {
                out.extend_from_slice(value.as_bytes());
                if matches!(value.bytes().last(), Some(b' ') | Some(b'\t')) {
                    expand_next = true;
                }
                expanded = true;
            }
        }
        if !expanded {
            out.extend_from_slice(&bytes[real_start..real_start + tl]);
        }
        command_word = false;
    }
    String::from_utf8(out).unwrap_or_else(|_| line.to_string())
}

/// `skipquotes` (alias.c:308): the index of the quote closing
/// the quoted string that opens at START (or the end of input).
fn alias_skipquotes(bytes: &[u8], start: usize) -> usize {
    let delimiter = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i >= bytes.len() {
                break;
            }
            i += 1;
            continue;
        }
        if bytes[i] == delimiter {
            return i;
        }
        i += 1;
    }
    i
}

/// `skipws` (alias.c:353): skip whitespace, quoted strings,
/// backslash-quoted words, and command separators (crossing a
/// separator marks the next token a command word). The caller
/// copies the skipped span verbatim.
fn alias_skipws(bytes: &[u8], start: usize, command_word: &mut bool) -> usize {
    let at = |p: usize| -> u8 { bytes.get(p).copied().unwrap_or(0) };
    let mut i = start;
    let mut pass_next = false;
    let mut backslash_quoted_word = false;
    while i < bytes.len() {
        let c = bytes[i];
        if pass_next {
            pass_next = false;
            i += 1;
            continue;
        }
        if c == b' ' || c == b'\t' {
            backslash_quoted_word = false;
            i += 1;
            continue;
        }
        if c == b'\\' {
            if i + 1 >= bytes.len() {
                break;
            }
            if at(i + 1).is_ascii_alphabetic() {
                backslash_quoted_word = true;
            } else {
                pass_next = true;
            }
            i += 1;
            continue;
        }
        if c == b'\'' || c == b'"' {
            i = alias_skipquotes(bytes, i);
            if i >= bytes.len() {
                break;
            }
            if at(i + 1).is_ascii_alphabetic() {
                backslash_quoted_word = true;
            }
            i += 1;
            continue;
        }
        if backslash_quoted_word {
            i += 1;
            continue;
        }
        if matches!(c, b'\r' | b'\n' | b';' | b'|' | b'&' | b'(') {
            *command_word = true;
            i += 1;
            continue;
        }
        break;
    }
    i
}

/// `rd_token` (alias.c:438): the index one past the token that
/// starts at START — a run of token characters (anything but
/// whitespace and the self-delimiting set ` \t\n\r;|&()`), with
/// backslash escapes and quoted stretches consumed whole.
fn alias_rd_token(bytes: &[u8], start: usize) -> usize {
    let self_delimiting = |c: u8| {
        matches!(
            c,
            b' ' | b'\t' | b'\n' | b'\r' | b';' | b'|' | b'&' | b'(' | b')'
        )
    };
    let mut i = start;
    while i < bytes.len() && !self_delimiting(bytes[i]) {
        if bytes[i] == b'\\' {
            i += 1;
            if i >= bytes.len() {
                break;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            i = alias_skipquotes(bytes, i);
            if i >= bytes.len() {
                break;
            }
            i += 1;
            continue;
        }
        i += 1;
    }
    i
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
        // Tokens are history_tokenize's exact substrings, sorted
        // and prefix-filtered: the quoted span is ONE token with
        // its quotes retained, so it answers to `"h`, not to `h`.
        assert_eq!(
            history_tokens("git commit -m \"hello world\""),
            vec!["git", "commit", "-m", "\"hello world\""]
        );
        assert_eq!(history_tokens("a;b"), vec!["a", ";", "b"]);
        assert_eq!(history_tokens("echo hi # there"), vec!["echo", "hi"]);
        assert_eq!(
            history_completion_matches(&entries, "git"),
            vec!["git".to_string()]
        );
        assert!(history_completion_matches(&entries, "h").is_empty());
        assert_eq!(
            history_completion_matches(&entries, "\"h"),
            vec!["\"hello world\"".to_string()]
        );
        assert!(history_completion_matches(&entries, "wo").is_empty());
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
        // Trailing-blank alias makes the next word eligible. The
        // splice is verbatim (alias.c:465), so the value's trailing
        // blank AND the original separating blank both survive:
        // "git  status" with two blanks is GNU's own M-C-e output,
        // confirmed against the oracle binary.
        assert_eq!(expand_alias_positions("g st", &aliases), "git  status");
        // The substituted text is NOT rescanned: a value whose
        // first word names another alias stays as written (the
        // oracle's M-C-e turns `a /tmp` into `ll -h /tmp`), and a
        // self-referential value therefore terminates trivially.
        let chain = vec![
            ("a".to_string(), "ll -h".to_string()),
            ("ll".to_string(), "ls -l".to_string()),
            ("loop".to_string(), "loop -x".to_string()),
        ];
        assert_eq!(expand_alias_positions("a /tmp", &chain), "ll -h /tmp");
        assert_eq!(expand_alias_positions("loop", &chain), "loop -x");
        // Quoted and backslash-quoted words are never candidates.
        assert_eq!(expand_alias_positions("'ll' x", &aliases), "'ll' x");
        assert_eq!(expand_alias_positions("\\ll x", &aliases), "\\ll x");
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

    #[test]
    fn burndown_completion_replacement_follows_rl_complete_internal() {
        // A lone filename gains the word-breaking space ...
        assert_eq!(
            completion_replacement(&["README".into()], "RE").as_deref(),
            Some("README ")
        );
        // ... except a directory, which keeps its slash as the hint.
        assert_eq!(
            completion_replacement(&["src/".into()], "s").as_deref(),
            Some("src/")
        );
        // Several matches insert the longest common prefix.
        assert_eq!(
            completion_replacement(&["alpha".into(), "alpine".into()], "a").as_deref(),
            Some("alp")
        );
        // No progress (or no matches) is a ding, not an edit.
        assert_eq!(completion_replacement(&[], "a"), None);
        assert_eq!(
            completion_replacement(&["alpha".into(), "alpine".into()], "alp"),
            None
        );
    }
}
