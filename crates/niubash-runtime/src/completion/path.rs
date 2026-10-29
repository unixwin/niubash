// Path completion for WinSH
// Provides Tab completion for files and directories

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use crate::completion::{CompletionBehavior, CompletionContext, CompletionResult};
use crate::path_utils::{shell_home_dir, shell_path_to_host_path};
use anyhow::Result;

/// Path completer
pub struct PathCompleter;

impl PathCompleter {
    /// Complete a path
    pub fn complete(context: &CompletionContext) -> Result<Option<CompletionResult>> {
        let shell_word = context.get_current_shell_word();
        let word = shell_word
            .as_ref()
            .map(|word| word.value.as_str())
            .unwrap_or_default();
        let quote = shell_word.as_ref().and_then(|word| word.quote);
        let directories_only = context
            .get_command_name()
            .as_deref()
            .is_some_and(is_directory_only_command);
        let query = PathQuery::from_word(&context.current_dir, word, quote);

        // niubash#627: the /dev and /proc namespaces are simulated by
        // the engine — there is no real directory to walk. Their
        // candidates come from the engine's special-path registry,
        // the same table the open points classify through.
        if let Some(mut candidates) =
            simulated_namespace_candidates(&query, word, context, directories_only)
        {
            if candidates.is_empty() {
                return Ok(None);
            }
            candidates.sort();
            return Ok(Some(CompletionResult::new(
                candidates
                    .into_iter()
                    .map(|candidate| candidate.completion)
                    .collect(),
            )));
        }

        let base_dir = Self::normalize_path(&query.base_dir);

        // Check if base directory exists
        if !base_dir.exists() || !base_dir.is_dir() {
            return Ok(None);
        }

        Self::complete_directory(&base_dir, &query, directories_only, context.behavior)
    }

    /// Complete entries in a directory
    fn complete_directory(
        base_dir: &Path,
        query: &PathQuery,
        directories_only: bool,
        behavior: CompletionBehavior,
    ) -> Result<Option<CompletionResult>> {
        let entries = match fs::read_dir(base_dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(None),
        };

        let mut completions: Vec<PathCandidate> = Vec::new();

        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();

            let matched = name_matches(&query.prefix, &file_name, behavior);
            if matched {
                let file_type = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if directories_only && !file_type.is_dir() {
                    continue;
                }
                if file_name.starts_with('.') && !query.prefix.starts_with('.') {
                    continue;
                }

                completions.push(PathCandidate::new(
                    file_type.is_dir(),
                    &query.display_prefix,
                    &file_name,
                    query.quote,
                ));
            }
        }

        if completions.is_empty() {
            Ok(None)
        } else {
            completions.sort();
            Ok(Some(CompletionResult::new(
                completions
                    .into_iter()
                    .map(|candidate| candidate.completion)
                    .collect(),
            )))
        }
    }

    /// Normalize path for Windows/Unix compatibility
    fn normalize_path(path: &Path) -> PathBuf {
        let path_str = path.to_string_lossy().to_string();

        // Convert Unix paths to Windows paths
        #[cfg(windows)]
        let normalized = path_str.replace('/', "\\");
        #[cfg(not(windows))]
        let normalized = path_str;

        PathBuf::from(normalized)
    }
}

#[derive(Debug)]
struct PathQuery {
    base_dir: PathBuf,
    prefix: String,
    display_prefix: String,
    quote: Option<char>,
}

impl PathQuery {
    fn from_word(current_dir: &Path, word: &str, quote: Option<char>) -> Self {
        if word.is_empty() {
            return Self {
                base_dir: current_dir.to_path_buf(),
                prefix: String::new(),
                display_prefix: String::new(),
                quote,
            };
        }

        if word == "." {
            return Self {
                base_dir: current_dir.to_path_buf(),
                prefix: ".".to_string(),
                display_prefix: String::new(),
                quote,
            };
        }

        if word == "./" || word == ".\\" {
            return Self {
                base_dir: current_dir.to_path_buf(),
                prefix: String::new(),
                display_prefix: word.to_string(),
                quote,
            };
        }

        if let Some(last_sep) = word.rfind(['/', '\\']) {
            let display_prefix = word[..=last_sep].to_string();
            let dir_part = &word[..last_sep];
            let prefix = word[last_sep + 1..].to_string();
            let base_dir = if dir_part.is_empty() {
                resolve_base_dir(current_dir, "/")
            } else {
                resolve_base_dir(current_dir, dir_part)
            };
            return Self {
                base_dir,
                prefix,
                display_prefix,
                quote,
            };
        }

        Self {
            base_dir: current_dir.to_path_buf(),
            prefix: word.to_string(),
            display_prefix: String::new(),
            quote,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct PathCandidate {
    is_dir: bool,
    sort_key: String,
    completion: String,
}

impl PathCandidate {
    fn new(is_dir: bool, display_prefix: &str, file_name: &str, quote: Option<char>) -> Self {
        let mut path = format!("{}{}", display_prefix, file_name);
        if is_dir {
            path.push('/');
        }
        let completion = format_shell_path(&path, quote);
        Self {
            is_dir,
            sort_key: file_name.to_lowercase(),
            completion,
        }
    }
}

impl Ord for PathCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_dir, other.is_dir) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => self
                .sort_key
                .cmp(&other.sort_key)
                .then_with(|| self.completion.cmp(&other.completion)),
        }
    }
}

impl PartialOrd for PathCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn resolve_base_dir(current_dir: &Path, dir_part: &str) -> PathBuf {
    if let Some(path) = resolve_logical_shell_dir(dir_part) {
        return path;
    }
    if let Some(path) = expand_tilde_path(dir_part) {
        return path;
    }

    if is_windows_drive_only(dir_part) {
        return PathBuf::from(format!("{}/", dir_part));
    }

    let path = PathBuf::from(dir_part);
    if path.is_absolute() || has_windows_drive_prefix(dir_part) {
        path
    } else {
        current_dir.join(path)
    }
}

fn resolve_logical_shell_dir(value: &str) -> Option<PathBuf> {
    let normalized = value.replace('\\', "/");
    let root = std::env::var_os("NIU_ROOT")
        .or_else(|| std::env::var_os("WINUXSH_ROOT")) // deprecated rubash bridge
        .map(PathBuf::from)?;
    if normalized == "/" {
        return Some(root);
    }
    if normalized == "/home" || normalized.starts_with("/home/") {
        let users = shell_home_dir()?.parent()?.to_path_buf();
        let rest = normalized
            .trim_start_matches("/home")
            .trim_start_matches('/');
        return Some(if rest.is_empty() {
            users
        } else {
            users.join(rest)
        });
    }
    // /tmp and /var/tmp are per-user temporary namespaces backed by the real
    // Windows temp dir, never the install tree (unixwin/niubash#94).
    if normalized == "/tmp" || normalized.starts_with("/tmp/") {
        let rest = normalized
            .trim_start_matches("/tmp")
            .trim_start_matches('/');
        return Some(if rest.is_empty() {
            std::env::temp_dir()
        } else {
            std::env::temp_dir().join(rest)
        });
    }
    if normalized == "/var/tmp" || normalized.starts_with("/var/tmp/") {
        let rest = normalized
            .trim_start_matches("/var/tmp")
            .trim_start_matches('/');
        let base = std::env::temp_dir().join("var").join("tmp");
        return Some(if rest.is_empty() {
            base
        } else {
            base.join(rest)
        });
    }
    let rest = normalized.strip_prefix('/')?;
    match rest.split('/').next() {
        Some("bin" | "dev" | "etc" | "opt" | "usr" | "var") => Some(root.join(rest)),
        _ => None,
    }
}

fn expand_tilde_path(value: &str) -> Option<PathBuf> {
    if value == "~" {
        return shell_home_dir();
    }
    value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
        .and_then(|rest| shell_home_dir().map(|home| home.join(shell_path_to_host_path(rest))))
}

fn shell_escape_path(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if should_escape_path_char(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn format_shell_path(value: &str, quote: Option<char>) -> String {
    match quote {
        Some('\'') => format!("'{}'", value.replace('\'', "'\\''")),
        Some('"') => format!("\"{}\"", escape_double_quoted_path(value)),
        _ => shell_escape_path(value),
    }
}

fn escape_double_quoted_path(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '"' | '$' | '`') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn should_escape_path_char(ch: char) -> bool {
    matches!(
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
    )
}

fn has_windows_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_windows_drive_only(value: &str) -> bool {
    value.len() == 2 && has_windows_drive_prefix(value)
}

fn is_directory_only_command(command: &str) -> bool {
    matches!(command, "cd" | "pushd")
}

/// Whether a candidate `name` matches the typed `prefix`, under the
/// same rules the filesystem walk uses.
fn name_matches(prefix: &str, name: &str, behavior: CompletionBehavior) -> bool {
    // niubash#530: a word containing glob metacharacters (`*`, `@(a|b)`)
    // is a pattern, not a literal prefix — GNU completes it against the
    // directory entries the pattern matches. The plain prefix matcher
    // could never match such a word, so Tab dead-ended at
    // NO RECORDS FOUND even though plain filename completion worked.
    if prefix_is_glob_pattern(prefix) {
        glob_completion_matches(prefix, name, behavior)
    } else {
        behavior.matches(name, prefix)
    }
}

// ─── Simulated-namespace completion (niubash#627) ───────────────────────
//
// On Windows the /dev and /proc namespaces are simulated by the
// engine: there is no real directory behind them (the enclave's
// physical dev dir exists but is empty, and /proc has no physical
// counterpart at all), so the filesystem walk can only ever answer
// "no candidates". The candidates below come from the engine's
// special-path registry (rubash::special_paths) — the same table the
// open points classify through — so completion can never promise a
// spelling the opens do not serve, nor hide one they do. The host
// keeps no special-path knowledge of its own here.

/// Candidates for a word whose directory part names a simulated
/// namespace, or None when it names an ordinary directory (the
/// caller then does the filesystem walk). A simulated answer is
/// authoritative even when empty (no name matches the prefix, or the
/// net dirs whose children are free text): it must not fall through
/// to a walk of the enclave's physical directories.
fn simulated_namespace_candidates(
    query: &PathQuery,
    word: &str,
    context: &CompletionContext,
    directories_only: bool,
) -> Option<Vec<PathCandidate>> {
    let dir = logical_dir_of(word)?;
    #[cfg(windows)]
    {
        if let Some(candidates) = dev_namespace_candidates(&dir, query, context, directories_only) {
            return Some(candidates);
        }
    }
    // The registry's proc enumeration face is the /proc directory
    // source; it answers None off Windows, where the real procfs
    // lists itself through the ordinary walk.
    if let Some(names) = rubash::special_paths::synthetic_proc_dir_entries(&dir) {
        let mut candidates = Vec::new();
        for name in &names {
            if !name_matches(&query.prefix, name, context.behavior) {
                continue;
            }
            let is_dir =
                rubash::special_paths::synthetic_proc_dir_entries(&format!("{dir}/{name}"))
                    .is_some();
            if directories_only && !is_dir {
                continue;
            }
            candidates.push(PathCandidate::new(
                is_dir,
                &query.display_prefix,
                name,
                query.quote,
            ));
        }
        return Some(candidates);
    }
    None
}

/// The /dev family's candidates, by directory spelling. Compiled for
/// tests on every platform; the dispatch only calls it on Windows,
/// where the namespace is simulated — elsewhere /dev is a real
/// directory the ordinary walk lists.
#[cfg(any(windows, test))]
fn dev_namespace_candidates(
    dir: &str,
    query: &PathQuery,
    context: &CompletionContext,
    directories_only: bool,
) -> Option<Vec<PathCandidate>> {
    match dir {
        // Static tier: the registry's listable /dev names.
        "/dev" => {
            let mut candidates = Vec::new();
            for (name, is_dir) in rubash::special_paths::special_dev_entries() {
                if directories_only && !is_dir {
                    continue;
                }
                if !name_matches(&query.prefix, name, context.behavior) {
                    continue;
                }
                candidates.push(PathCandidate::new(
                    *is_dir,
                    &query.display_prefix,
                    name,
                    query.quote,
                ));
            }
            Some(candidates)
        }
        // Dynamic tier: the session's live fd numbers, from the fd
        // table snapshot in the context — never a name list. The
        // /proc/self/fd spelling is the same table by alias.
        "/dev/fd" | "/proc/self/fd" => {
            let mut candidates = Vec::new();
            if !directories_only {
                for fd in &context.open_fds {
                    let name = fd.to_string();
                    if name_matches(&query.prefix, &name, context.behavior) {
                        candidates.push(PathCandidate::new(
                            false,
                            &query.display_prefix,
                            &name,
                            query.quote,
                        ));
                    }
                }
            }
            Some(candidates)
        }
        // Net dirs: children are free-text host/port pairs. GNU has
        // no completion there, and neither do we — an authoritative
        // empty answer, so the enclave walk cannot invent any.
        "/dev/tcp" | "/dev/udp" => Some(Vec::new()),
        _ => None,
    }
}

/// The directory portion of the word as typed, in logical
/// (forward-slash) form: "/dev/fd/3" → "/dev/fd", "/dev/" → "/dev".
/// None when the word has no separator (a bare name completes in the
/// current directory).
fn logical_dir_of(word: &str) -> Option<String> {
    let sep = word.rfind(['/', '\\'])?;
    if sep == 0 {
        return Some("/".to_string());
    }
    Some(word[..sep].replace('\\', "/"))
}

// ─── Glob-pattern filename completion (niubash#530) ────────────────────────
//
// When the word being completed contains glob metacharacters, GNU bash
// treats it as a pattern and offers the directory entries it matches
// (glob completion). The functions below implement just the pattern
// language completion needs: `*`, `?`, `[...]` classes, backslash
// escapes, and the extglob groups `@()`, `?()`, `*()`, `+()`, `!()`.

/// True when `prefix` contains an unescaped glob metacharacter that turns
/// filename completion into pattern completion.
fn prefix_is_glob_pattern(prefix: &str) -> bool {
    let bytes = prefix.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'*' | b'?' | b'[' => return true,
            b'@' | b'+' | b'!' if bytes.get(i + 1) == Some(&b'(') => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// Match `name` against the typed glob `pattern`. Completion patterns are
/// incremental — the user may still be typing — so a name also matches
/// when the pattern matches a prefix of it (as if a `*` were appended).
fn glob_completion_matches(pattern: &str, name: &str, behavior: CompletionBehavior) -> bool {
    if behavior.case_sensitive {
        let pat: Vec<char> = pattern.chars().collect();
        let text: Vec<char> = name.chars().collect();
        glob_match_prefix(&pat, &text)
    } else {
        let pat: Vec<char> = pattern.to_lowercase().chars().collect();
        let text: Vec<char> = name.to_lowercase().chars().collect();
        glob_match_prefix(&pat, &text)
    }
}

fn glob_match_prefix(pat: &[char], text: &[char]) -> bool {
    if pat.is_empty() {
        return true; // pattern exhausted: it matched a prefix of the name
    }
    if pat.len() >= 2 && pat[1] == '(' && matches!(pat[0], '@' | '?' | '*' | '+' | '!') {
        if let Some((alts, rest)) = parse_extglob_group(&pat[2..]) {
            return match pat[0] {
                '@' => alts
                    .iter()
                    .any(|alt| match_alt_then(alt, text, rest, false)),
                '?' => {
                    glob_match_prefix(rest, text)
                        || alts
                            .iter()
                            .any(|alt| match_alt_then(alt, text, rest, false))
                }
                '*' => match_star_group(&alts, text, rest, true, false),
                '+' => match_star_group(&alts, text, rest, false, false),
                '!' => {
                    (0..=text.len()).any(|take| {
                        // When nothing follows the group, the group itself
                        // must account for the whole remaining name —
                        // otherwise `!(a|b)` would "match" `a` by consuming
                        // zero characters and ignoring the rest.
                        !(rest.is_empty() && take < text.len())
                            && alts.iter().all(|alt| !glob_match_exact(alt, &text[..take]))
                            && glob_match_prefix(rest, &text[take..])
                    })
                }
                _ => unreachable!(),
            };
        }
    }
    match pat[0] {
        '*' => (0..=text.len()).any(|take| glob_match_prefix(&pat[1..], &text[take..])),
        '?' => !text.is_empty() && glob_match_prefix(&pat[1..], &text[1..]),
        '[' => {
            if text.is_empty() {
                return false;
            }
            match parse_char_class(pat) {
                Some((negated, ranges, consumed)) => {
                    let inside = ranges
                        .iter()
                        .any(|(lo, hi)| text[0] >= *lo && text[0] <= *hi);
                    inside != negated && glob_match_prefix(&pat[consumed..], &text[1..])
                }
                None => text[0] == '[' && glob_match_prefix(&pat[1..], &text[1..]),
            }
        }
        '\\' if pat.len() >= 2 => {
            !text.is_empty() && text[0] == pat[1] && glob_match_prefix(&pat[2..], &text[1..])
        }
        c => !text.is_empty() && text[0] == c && glob_match_prefix(&pat[1..], &text[1..]),
    }
}

/// Full-string glob match (used for extglob alternatives, which must match
/// the whole segment they consume).
fn glob_match_exact(pat: &[char], text: &[char]) -> bool {
    if pat.is_empty() {
        return text.is_empty();
    }
    if pat.len() >= 2 && pat[1] == '(' && matches!(pat[0], '@' | '?' | '*' | '+' | '!') {
        if let Some((alts, rest)) = parse_extglob_group(&pat[2..]) {
            return match pat[0] {
                '@' => alts.iter().any(|alt| match_alt_then(alt, text, rest, true)),
                '?' => {
                    glob_match_exact(rest, text)
                        || alts.iter().any(|alt| match_alt_then(alt, text, rest, true))
                }
                '*' => match_star_group(&alts, text, rest, true, true),
                '+' => match_star_group(&alts, text, rest, false, true),
                '!' => (0..=text.len()).any(|take| {
                    alts.iter().all(|alt| !glob_match_exact(alt, &text[..take]))
                        && glob_match_exact(rest, &text[take..])
                }),
                _ => unreachable!(),
            };
        }
    }
    match pat[0] {
        '*' => (0..=text.len()).any(|take| glob_match_exact(&pat[1..], &text[take..])),
        '?' => !text.is_empty() && glob_match_exact(&pat[1..], &text[1..]),
        '[' => {
            if text.is_empty() {
                return false;
            }
            match parse_char_class(pat) {
                Some((negated, ranges, consumed)) => {
                    let inside = ranges
                        .iter()
                        .any(|(lo, hi)| text[0] >= *lo && text[0] <= *hi);
                    inside != negated && glob_match_exact(&pat[consumed..], &text[1..])
                }
                None => text[0] == '[' && glob_match_exact(&pat[1..], &text[1..]),
            }
        }
        '\\' if pat.len() >= 2 => {
            !text.is_empty() && text[0] == pat[1] && glob_match_exact(&pat[2..], &text[1..])
        }
        c => !text.is_empty() && text[0] == c && glob_match_exact(&pat[1..], &text[1..]),
    }
}

/// Match one extglob alternative against a prefix of `text`, then continue
/// matching `rest` against the remainder (exactly when `exact`, as a
/// completion prefix otherwise).
fn match_alt_then(alt: &[char], text: &[char], rest: &[char], exact: bool) -> bool {
    (0..=text.len()).any(|take| {
        glob_match_exact(alt, &text[..take])
            && if exact {
                glob_match_exact(rest, &text[take..])
            } else {
                glob_match_prefix(rest, &text[take..])
            }
    })
}

fn match_star_group(
    alts: &[Vec<char>],
    text: &[char],
    rest: &[char],
    allow_zero: bool,
    exact: bool,
) -> bool {
    let rest_matches = if exact {
        glob_match_exact(rest, text)
    } else {
        glob_match_prefix(rest, text)
    };
    if allow_zero && rest_matches {
        return true;
    }
    alts.iter().any(|alt| {
        (1..=text.len()).any(|take| {
            glob_match_exact(alt, &text[..take])
                && match_star_group(alts, &text[take..], rest, true, exact)
        })
    })
}

/// Parse the inside of an extglob group (the pattern slice just after
/// `(`): returns the alternatives (split on unescaped `|` at nesting
/// depth 0) and the pattern remainder after the closing `)`.
fn parse_extglob_group(pat: &[char]) -> Option<(Vec<Vec<char>>, &[char])> {
    let mut alts = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < pat.len() {
        match pat[i] {
            '\\' if i + 1 < pat.len() => {
                current.push(pat[i]);
                current.push(pat[i + 1]);
                i += 2;
            }
            '(' => {
                depth += 1;
                current.push(pat[i]);
                i += 1;
            }
            ')' => {
                if depth == 0 {
                    alts.push(current);
                    return Some((alts, &pat[i + 1..]));
                }
                depth -= 1;
                current.push(pat[i]);
                i += 1;
            }
            '|' if depth == 0 => {
                alts.push(std::mem::take(&mut current));
                i += 1;
            }
            c => {
                current.push(c);
                i += 1;
            }
        }
    }
    None
}

/// Parsed `[...]` class: (negated, ranges, chars consumed including the
/// brackets).
type ParsedCharClass = (bool, Vec<(char, char)>, usize);

/// Parse a `[...]` class at the start of `pat`. An unterminated class is
/// None (the `[` then matches literally).
fn parse_char_class(pat: &[char]) -> Option<ParsedCharClass> {
    let mut i = 1;
    let mut negated = false;
    if i < pat.len() && (pat[i] == '!' || pat[i] == '^') {
        negated = true;
        i += 1;
    }
    let mut ranges = Vec::new();
    if i < pat.len() && pat[i] == ']' {
        ranges.push((']', ']'));
        i += 1;
    }
    while i < pat.len() && pat[i] != ']' {
        if pat[i] == '\\' && i + 1 < pat.len() {
            let lo = pat[i + 1];
            if i + 3 < pat.len() && pat[i + 2] == '-' && pat[i + 3] != ']' {
                ranges.push((lo, pat[i + 3]));
                i += 4;
            } else {
                ranges.push((lo, lo));
                i += 2;
            }
        } else if i + 2 < pat.len() && pat[i + 1] == '-' && pat[i + 2] != ']' {
            ranges.push((pat[i], pat[i + 2]));
            i += 3;
        } else {
            ranges.push((pat[i], pat[i]));
            i += 1;
        }
    }
    if i >= pat.len() {
        return None;
    }
    Some((negated, ranges, i + 1))
}

#[cfg(test)]
mod glob_completion_tests {
    use super::*;

    fn ctx(dir: &Path, input: &str) -> CompletionContext {
        CompletionContext::new(dir.to_path_buf(), input.to_string(), input.len())
    }

    fn complete_values(dir: &Path, input: &str) -> Vec<String> {
        let context = ctx(dir, input);
        PathCompleter::complete(&context)
            .unwrap()
            .map(|result| result.completions)
            .unwrap_or_default()
    }

    fn seed_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "niubash-glob-completion-{}-{}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["alpha.txt", "alpha2.txt", "beta.txt", "gamma.log"] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        dir
    }

    #[test]
    fn star_word_completes_all_files() {
        let dir = seed_dir();
        let values = complete_values(&dir, "ls *");
        assert!(values.contains(&"alpha.txt".to_string()), "{values:?}");
        assert!(values.contains(&"beta.txt".to_string()), "{values:?}");
        assert!(values.contains(&"gamma.log".to_string()), "{values:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn extglob_word_completes_matching_files() {
        let dir = seed_dir();
        let values = complete_values(&dir, "ls @(a|b)");
        assert!(values.contains(&"alpha.txt".to_string()), "{values:?}");
        assert!(values.contains(&"alpha2.txt".to_string()), "{values:?}");
        assert!(values.contains(&"beta.txt".to_string()), "{values:?}");
        assert!(!values.contains(&"gamma.log".to_string()), "{values:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn glob_suffix_and_class_forms() {
        let dir = seed_dir();
        let txt = complete_values(&dir, "ls *.txt");
        assert_eq!(txt.len(), 3, "{txt:?}");
        let class = complete_values(&dir, "ls [ab]*");
        assert!(class.contains(&"alpha.txt".to_string()), "{class:?}");
        assert!(class.contains(&"beta.txt".to_string()), "{class:?}");
        assert!(!class.contains(&"gamma.log".to_string()), "{class:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn glob_matcher_units() {
        let ci = CompletionBehavior::default();
        assert!(glob_completion_matches("*", "anything", ci));
        assert!(glob_completion_matches("a*", "alpha", ci));
        assert!(!glob_completion_matches("b*", "alpha", ci));
        assert!(glob_completion_matches("@(a|b)", "beta", ci));
        assert!(!glob_completion_matches("@(a|b)", "gamma", ci));
        assert!(glob_completion_matches("+(a|b)", "abba", ci));
        assert!(glob_completion_matches("!(a|b)", "gamma", ci));
        assert!(!glob_completion_matches("!(a|b)", "a", ci));
        assert!(glob_completion_matches("[a-c]?", "bx", ci));
        assert!(!glob_completion_matches("[!a-c]?", "bx", ci));
        assert!(glob_completion_matches("literal", "literal.txt", ci));
    }
}

#[cfg(test)]
mod simulated_namespace_tests {
    use super::*;
    use std::collections::BTreeSet;

    fn query(word: &str) -> PathQuery {
        PathQuery::from_word(Path::new("/"), word, None)
    }

    fn ctx(fds: &[u32]) -> CompletionContext {
        let mut context = CompletionContext::new(PathBuf::from("/"), String::new(), 0);
        context.open_fds = fds.to_vec();
        context
    }

    fn values(mut candidates: Vec<PathCandidate>) -> Vec<String> {
        candidates.sort();
        candidates
            .into_iter()
            .map(|candidate| candidate.completion)
            .collect()
    }

    #[test]
    fn issue627_logical_dir_splitting() {
        assert_eq!(logical_dir_of("/dev/fd/3").as_deref(), Some("/dev/fd"));
        assert_eq!(logical_dir_of("/dev/").as_deref(), Some("/dev"));
        assert_eq!(logical_dir_of("/dev").as_deref(), Some("/"));
        assert_eq!(logical_dir_of("plain"), None);
        assert_eq!(logical_dir_of("C:\\dev\\").as_deref(), Some("C:/dev"));
    }

    #[test]
    fn issue627_dev_static_tier_lists_registry_names() {
        let q = query("/dev/");
        let candidates = dev_namespace_candidates("/dev", &q, &ctx(&[]), false)
            .expect("/dev is a simulated namespace");
        let got: BTreeSet<String> = values(candidates).into_iter().collect();
        // The expectation is rebuilt from the registry table itself:
        // this pin follows the registry, it does not restate it.
        let expected: BTreeSet<String> = rubash::special_paths::special_dev_entries()
            .iter()
            .map(|(name, is_dir)| {
                if *is_dir {
                    format!("/dev/{name}/")
                } else {
                    format!("/dev/{name}")
                }
            })
            .collect();
        assert_eq!(got, expected);
        // Every listed leaf is a spelling the registry classifies as
        // special — completion never promises what the opens refuse.
        for (name, is_dir) in rubash::special_paths::special_dev_entries() {
            if !is_dir {
                assert_ne!(
                    rubash::special_paths::classify_special_path(&format!("/dev/{name}")),
                    rubash::special_paths::SpecialPathClass::Filesystem,
                    "listed leaf {name} must classify as special"
                );
            }
        }
        // Spellings the registry does not simulate stay unlisted.
        assert!(!got.iter().any(|value| value.contains("zero")));
        assert!(!got.iter().any(|value| value.contains("tty")));
    }

    #[test]
    fn issue627_dev_static_tier_prefix_and_directories_only() {
        let q = query("/dev/n");
        let candidates = dev_namespace_candidates("/dev", &q, &ctx(&[]), false).unwrap();
        assert_eq!(values(candidates), vec!["/dev/null".to_string()]);

        let q = query("/dev/");
        let dirs = dev_namespace_candidates("/dev", &q, &ctx(&[]), true).unwrap();
        assert_eq!(
            values(dirs),
            vec![
                "/dev/fd/".to_string(),
                "/dev/tcp/".to_string(),
                "/dev/udp/".to_string()
            ]
        );
    }

    #[test]
    fn issue627_dev_fd_dynamic_tier_tracks_the_fd_table() {
        let q = query("/dev/fd/");
        let all = dev_namespace_candidates("/dev/fd", &q, &ctx(&[0, 1, 2, 3, 10]), false).unwrap();
        assert_eq!(
            values(all),
            vec![
                "/dev/fd/0".to_string(),
                "/dev/fd/1".to_string(),
                "/dev/fd/10".to_string(),
                "/dev/fd/2".to_string(),
                "/dev/fd/3".to_string()
            ]
        );

        // The prefix narrows by the number's spelling.
        let q = query("/dev/fd/1");
        let narrowed =
            dev_namespace_candidates("/dev/fd", &q, &ctx(&[0, 1, 2, 3, 10]), false).unwrap();
        assert_eq!(
            values(narrowed),
            vec!["/dev/fd/1".to_string(), "/dev/fd/10".to_string()]
        );

        // Closing an fd removes it: the tier tracks the table.
        let q = query("/dev/fd/");
        let after_close = dev_namespace_candidates("/dev/fd", &q, &ctx(&[0, 1, 2]), false).unwrap();
        assert!(!values(after_close).iter().any(|value| value == "/dev/fd/3"));

        // The /proc/self/fd spelling serves the same table.
        let q = query("/proc/self/fd/");
        let alias =
            dev_namespace_candidates("/proc/self/fd", &q, &ctx(&[0, 1, 2, 3]), false).unwrap();
        assert!(values(alias).contains(&"/proc/self/fd/3".to_string()));

        // Fds are not directories.
        let q = query("/dev/fd/");
        assert!(
            dev_namespace_candidates("/dev/fd", &q, &ctx(&[0, 1, 2, 3]), true)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn issue627_net_dirs_complete_nothing() {
        for dir in ["/dev/tcp", "/dev/udp"] {
            let q = query(&format!("{dir}/"));
            let candidates = dev_namespace_candidates(dir, &q, &ctx(&[0, 1, 2]), false);
            assert_eq!(
                candidates.map(|candidates| candidates.len()),
                Some(0),
                "{dir} must answer authoritatively empty"
            );
        }
        // An ordinary directory is not this dispatch's business.
        let q = query("/etc/");
        assert!(dev_namespace_candidates("/etc", &q, &ctx(&[]), false).is_none());
    }

    #[test]
    fn issue627_proc_falls_through_where_no_simulation() {
        // Off Windows the registry's proc face answers None and the
        // real procfs is the ordinary walk's business.
        if !cfg!(windows) {
            let q = query("/proc/");
            let context = ctx(&[0, 1, 2]);
            assert!(simulated_namespace_candidates(&q, "/proc/", &context, false).is_none());
        }
    }

    #[cfg(windows)]
    #[test]
    fn issue627_proc_tier_lists_registry_entries() {
        let q = query("/proc/");
        let context = ctx(&[0, 1, 2]);
        let candidates = simulated_namespace_candidates(&q, "/proc/", &context, false)
            .expect("/proc is simulated on Windows");
        let values = values(candidates);
        assert!(values.contains(&"/proc/meminfo".to_string()), "{values:?}");
        assert!(values.contains(&"/proc/self/".to_string()), "{values:?}");
    }
}
