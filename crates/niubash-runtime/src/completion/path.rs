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

            if behavior.matches(&file_name, &query.prefix) {
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

        if let Some(last_sep) = word.rfind(|c| c == '/' || c == '\\') {
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
    let rest = normalized.strip_prefix('/')?;
    match rest.split('/').next() {
        Some("bin" | "dev" | "etc" | "opt" | "tmp" | "usr" | "var") => Some(root.join(rest)),
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
