//! Native Niubash line highlighter.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};
use rubash::executor::Executor;

use crate::autosuggest::parse_style;
use crate::completion::{command::CommandCompleter, CommandProbe, CompletionState};
use crate::config::SyntaxHighlightConfig;
use crate::path_utils::{
    resolve_logical_shell_path, shell_home_dir, shell_path_to_host_path, split_shell_path_list,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SyntaxKind {
    Default,
    UnknownToken,
    ReservedWord,
    Builtin,
    Command,
    CommandSeparator,
    Path,
    PathPrefix,
    SingleQuotedArgument,
    DoubleQuotedArgument,
    Variable,
    CommandSubstitution,
    SingleHyphenOption,
    DoubleHyphenOption,
    Assign,
    Redirection,
    Comment,
}

impl SyntaxKind {
    fn style_key(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::UnknownToken => "unknown-token",
            Self::ReservedWord => "reserved-word",
            Self::Builtin => "builtin",
            Self::Command => "command",
            Self::CommandSeparator => "commandseparator",
            Self::Path => "path",
            Self::PathPrefix => "path_prefix",
            Self::SingleQuotedArgument => "single-quoted-argument",
            Self::DoubleQuotedArgument => "double-quoted-argument",
            Self::Variable => "dollar-double-quoted-argument",
            Self::CommandSubstitution => "command-substitution",
            Self::SingleHyphenOption => "single-hyphen-option",
            Self::DoubleHyphenOption => "double-hyphen-option",
            Self::Assign => "assign",
            Self::Redirection => "redirection",
            Self::Comment => "comment",
        }
    }
}

/// Reedline highlighter for common shell tokens.
pub struct NiubashSyntaxHighlighter {
    styles: HashMap<SyntaxKind, Style>,
    commands: HashSet<String>,
    completion_state: Option<Arc<Mutex<CompletionState>>>,
    /// Live-probe memo for highlighters built without engine state; the
    /// state-carrying path shares [`CompletionState::command_probe`] instead.
    local_probe: Mutex<CommandProbe>,
    max_length: Option<usize>,
}

impl NiubashSyntaxHighlighter {
    pub fn new(config: &SyntaxHighlightConfig) -> Self {
        Self::new_with_commands(config, std::iter::empty::<String>())
    }

    pub fn new_with_commands<I, S>(config: &SyntaxHighlightConfig, commands: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::new_with_commands_internal(config, commands, None)
    }

    pub fn new_with_commands_and_state<I, S>(
        config: &SyntaxHighlightConfig,
        commands: I,
        completion_state: Arc<Mutex<CompletionState>>,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::new_with_commands_internal(config, commands, Some(completion_state))
    }

    fn new_with_commands_internal<I, S>(
        config: &SyntaxHighlightConfig,
        commands: I,
        completion_state: Option<Arc<Mutex<CompletionState>>>,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut styles = default_styles();
        for (key, value) in &config.styles {
            if let Some(kind) = kind_from_style_key(key) {
                let default = styles.get(&kind).copied().unwrap_or_default();
                styles.insert(kind, parse_style(value, default));
            }
        }

        let mut command_set: HashSet<String> = CommandCompleter::get_all_commands()
            .into_iter()
            .map(|command| command.to_ascii_lowercase())
            .collect();
        command_set.extend(
            CommandCompleter::get_common_commands()
                .into_iter()
                .map(|command| command.to_ascii_lowercase()),
        );
        command_set.extend(
            commands
                .into_iter()
                .map(|command| command.as_ref().to_ascii_lowercase()),
        );

        Self {
            styles,
            commands: command_set,
            completion_state,
            local_probe: Mutex::new(CommandProbe::default()),
            max_length: config.max_length,
        }
    }

    fn style(&self, kind: SyntaxKind) -> Style {
        self.styles.get(&kind).copied().unwrap_or_default()
    }

    /// The backing directory for the session's `/tmp`: the value the
    /// shell syncs into the shared completion state (engine-resolved
    /// from the session env, so a session TMPDIR assignment moves it
    /// immediately — niubash#627 G2). Highlighters built without
    /// engine state fall back to the process temp dir.
    fn session_tmp_base(&self) -> PathBuf {
        self.completion_state
            .as_ref()
            .and_then(|state| state.lock().ok())
            .map(|state| state.tmp_base.clone())
            .unwrap_or_else(std::env::temp_dir)
    }
}

impl Highlighter for NiubashSyntaxHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        if self
            .max_length
            .map(|max| line.chars().count() > max)
            .unwrap_or(false)
        {
            return plain_text(line);
        }

        let tokens = lex(line);
        if tokens.is_empty() {
            return plain_text(line);
        }

        let mut styled = StyledText::new();
        let mut pos = 0;
        let mut expect_command = true;
        let mut after_redirection = false;

        for token in tokens {
            if pos < token.start {
                styled.push((
                    self.style(SyntaxKind::Default),
                    line[pos..token.start].to_string(),
                ));
            }

            let kind = match token.kind {
                TokenKind::Comment => SyntaxKind::Comment,
                TokenKind::CommandSeparator => {
                    expect_command = true;
                    after_redirection = false;
                    SyntaxKind::CommandSeparator
                }
                TokenKind::Redirection => {
                    after_redirection = true;
                    SyntaxKind::Redirection
                }
                TokenKind::Word => {
                    let classified =
                        classify_word(&token.text, expect_command, after_redirection, self);
                    if expect_command && classified != SyntaxKind::Assign {
                        expect_command = false;
                    }
                    after_redirection = false;
                    classified
                }
            };

            styled.push((self.style(kind), token.text));
            pos = token.end;
        }

        if pos < line.len() {
            styled.push((self.style(SyntaxKind::Default), line[pos..].to_string()));
        }

        styled
    }
}

fn plain_text(line: &str) -> StyledText {
    let mut styled = StyledText::new();
    styled.push((Style::new(), line.to_string()));
    styled
}

fn classify_word(
    word: &str,
    expect_command: bool,
    after_redirection: bool,
    highlighter: &NiubashSyntaxHighlighter,
) -> SyntaxKind {
    let unquoted = unquote_word(word);

    if expect_command && is_assignment(&unquoted) {
        return SyntaxKind::Assign;
    }

    if expect_command {
        if is_reserved_word(&unquoted) {
            return SyntaxKind::ReservedWord;
        }
        if is_shell_builtin(&unquoted) {
            return SyntaxKind::Builtin;
        }
        // REPL-only commands (`self-update`, `update-niubash`, `logout`) never
        // resolve on PATH, but the interactive shell accepts them, so color
        // them like builtins instead of unknown tokens (niu#260).
        if crate::repl::is_repl_reserved_command(&unquoted) {
            return SyntaxKind::Builtin;
        }
        if highlighter.is_known_command(&unquoted) {
            return SyntaxKind::Command;
        }
        if let Some(path_kind) = path_kind(&unquoted, &highlighter.session_tmp_base()) {
            return path_kind;
        }
        return SyntaxKind::UnknownToken;
    }

    if after_redirection {
        return path_kind(&unquoted, &highlighter.session_tmp_base())
            .unwrap_or(SyntaxKind::Default);
    }
    if word.starts_with('\'') {
        return SyntaxKind::SingleQuotedArgument;
    }
    if word.starts_with('"') {
        return SyntaxKind::DoubleQuotedArgument;
    }
    if word.contains("$(") || word.contains('`') {
        return SyntaxKind::CommandSubstitution;
    }
    if contains_variable(word) {
        return SyntaxKind::Variable;
    }
    if word.starts_with("--") {
        return SyntaxKind::DoubleHyphenOption;
    }
    if word.starts_with('-') && word.len() > 1 {
        return SyntaxKind::SingleHyphenOption;
    }
    if is_assignment(&unquoted) {
        return SyntaxKind::Assign;
    }
    path_kind(&unquoted, &highlighter.session_tmp_base()).unwrap_or(SyntaxKind::Default)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    start: usize,
    end: usize,
    text: String,
    kind: TokenKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Word,
    CommandSeparator,
    Redirection,
    Comment,
}

fn lex(line: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < line.len() {
        let ch = next_char(line, index);
        if ch.is_whitespace() {
            index += ch.len_utf8();
            continue;
        }

        if ch == '#' && comment_can_start(line, index) {
            tokens.push(Token {
                start: index,
                end: line.len(),
                text: line[index..].to_string(),
                kind: TokenKind::Comment,
            });
            break;
        }

        if let Some(end) = command_separator_end(line, index) {
            tokens.push(Token {
                start: index,
                end,
                text: line[index..end].to_string(),
                kind: TokenKind::CommandSeparator,
            });
            index = end;
            continue;
        }

        if let Some(end) = redirection_end(line, index) {
            tokens.push(Token {
                start: index,
                end,
                text: line[index..end].to_string(),
                kind: TokenKind::Redirection,
            });
            index = end;
            continue;
        }

        let end = word_end(line, index);
        tokens.push(Token {
            start: index,
            end,
            text: line[index..end].to_string(),
            kind: TokenKind::Word,
        });
        index = end;
    }

    tokens
}

fn next_char(line: &str, index: usize) -> char {
    line[index..].chars().next().unwrap()
}

fn comment_can_start(line: &str, index: usize) -> bool {
    index == 0
        || line[..index]
            .chars()
            .last()
            .is_some_and(char::is_whitespace)
}

fn command_separator_end(line: &str, index: usize) -> Option<usize> {
    for separator in ["&&", "||", "|", ";"] {
        if line[index..].starts_with(separator) {
            return Some(index + separator.len());
        }
    }
    None
}

fn redirection_end(line: &str, index: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut probe = index;

    while probe < line.len() && bytes[probe].is_ascii_digit() {
        probe += 1;
    }

    if probe >= line.len() {
        return None;
    }

    let ch = next_char(line, probe);
    if ch != '<' && ch != '>' {
        return None;
    }

    probe += ch.len_utf8();
    while probe < line.len() {
        let ch = next_char(line, probe);
        if ch.is_whitespace() || matches!(ch, ';' | '|') {
            break;
        }
        if ch == '<' || ch == '>' || ch == '&' || ch.is_ascii_digit() || ch == '-' {
            probe += ch.len_utf8();
        } else {
            break;
        }
    }

    Some(probe)
}

fn word_end(line: &str, start: usize) -> usize {
    let mut index = start;
    let mut quote = None;
    let mut escaped = false;

    while index < line.len() {
        let ch = next_char(line, index);
        if escaped {
            escaped = false;
            index += ch.len_utf8();
            continue;
        }

        if ch == '\\' {
            escaped = true;
            index += ch.len_utf8();
            continue;
        }

        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            index += ch.len_utf8();
            continue;
        }

        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            index += ch.len_utf8();
            continue;
        }

        if ch.is_whitespace()
            || ch == '#'
            || command_separator_end(line, index).is_some()
            || redirection_end(line, index).is_some()
        {
            break;
        }

        index += ch.len_utf8();
    }

    index
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn is_reserved_word(word: &str) -> bool {
    matches!(
        word,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "for"
            | "in"
            | "do"
            | "done"
            | "while"
            | "until"
            | "case"
            | "esac"
            | "function"
            | "{"
            | "}"
    )
}

fn is_shell_builtin(word: &str) -> bool {
    matches!(
        word,
        "alias"
            | "bg"
            | "break"
            | "cd"
            | "continue"
            | "echo"
            | "eval"
            | "exec"
            | "exit"
            | "export"
            | "false"
            | "fg"
            | "help"
            | "history"
            | "jobs"
            | "pwd"
            | "read"
            | "return"
            | "set"
            | "setopt"
            | "shift"
            | "source"
            | "test"
            | "true"
            | "type"
            | "unalias"
            | "unset"
            | "unsetopt"
    )
}

impl NiubashSyntaxHighlighter {
    fn is_known_command(&self, command: &str) -> bool {
        let lower = command.to_ascii_lowercase();
        if self.is_known_command_name(&lower) {
            return true;
        }
        if lower.ends_with(".exe") || lower.ends_with(".cmd") || lower.ends_with(".bat") {
            let stem = Path::new(&lower)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(&lower);
            return self.is_known_command_name(stem);
        }
        false
    }

    fn is_known_command_name(&self, command: &str) -> bool {
        if self.commands.contains(command) {
            return true;
        }

        let Some(state) = &self.completion_state else {
            return self.probe_process_env(command);
        };

        // Dynamic names and the memoized probe answers share the completion
        // lock; a memo miss snapshots the env and re-locks only to record
        // the outcome, so the filesystem walk never runs under the lock
        // (niubash#574).
        let (fingerprint, env_vars) = {
            let Ok(mut state) = state.lock() else {
                return false;
            };
            let dynamic = state
                .aliases
                .iter()
                .chain(state.functions.iter())
                .chain(state.plugin_command_names().iter())
                .any(|name| name.eq_ignore_ascii_case(command));
            if dynamic {
                return true;
            }
            if !probe_eligible(command) {
                return false;
            }
            let fingerprint = command_probe_fingerprint(&state.env_vars);
            match state.command_probe.lookup(&fingerprint, command) {
                Some(known) => return known,
                None => (fingerprint, state.env_vars.clone()),
            }
        };

        let known = command_on_path(
            command,
            &env_path_dirs(&env_vars),
            &probe_extensions(env_vars.get("PATHEXT").map(String::as_str)),
        );
        if let Ok(mut state) = state.lock() {
            state.command_probe.note_probe();
            state.command_probe.store(&fingerprint, command, known);
        }
        known
    }

    /// Highlighters built without engine state probe the process
    /// environment instead; their memo sits on the highlighter itself.
    fn probe_process_env(&self, command: &str) -> bool {
        if !probe_eligible(command) {
            return false;
        }
        let fingerprint = process_env_fingerprint();
        let Ok(mut probe) = self.local_probe.lock() else {
            return false;
        };
        if let Some(known) = probe.lookup(&fingerprint, command) {
            return known;
        }
        let dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        let known = command_on_path(
            command,
            &dirs,
            &probe_extensions(std::env::var("PATHEXT").ok().as_deref()),
        );
        probe.note_probe();
        probe.store(&fingerprint, command, known);
        known
    }
}

/// A name that could resolve through PATH: bare words only — anything with
/// a path separator or drive colon is a path the `path_kind` arm handles.
fn probe_eligible(command: &str) -> bool {
    !command.is_empty() && !command.chars().any(|ch| matches!(ch, '/' | '\\' | ':'))
}

/// Fingerprint of the env a live probe ran under. PATH and PATHEXT are the
/// probe inputs; the serialized `hash` table rides along so `hash -r/-d/-p`
/// and per-command hashing retire remembered answers (niubash#574).
fn command_probe_fingerprint(env_vars: &HashMap<String, String>) -> String {
    let mut fingerprint = String::new();
    for key in ["PATH", "PATHEXT", "__RUBASH_HASH_TABLE"] {
        if let Some(value) = env_vars.get(key) {
            fingerprint.push_str(value);
        }
        fingerprint.push('\u{1e}');
    }
    fingerprint
}

fn process_env_fingerprint() -> String {
    let mut fingerprint = String::new();
    for key in ["PATH", "PATHEXT"] {
        if let Ok(value) = std::env::var(key) {
            fingerprint.push_str(&value);
        }
        fingerprint.push('\u{1e}');
    }
    fingerprint
}

/// Native directories a shell PATH resolves to. On Windows each entry goes
/// through the engine's translator so root-mapped logical directories
/// (`/usr/bin` under the shell root) reach their real backing directory.
fn env_path_dirs(env_vars: &HashMap<String, String>) -> Vec<PathBuf> {
    let Some(path) = env_vars.get("PATH") else {
        return Vec::new();
    };
    split_shell_path_list(path)
        .iter()
        .flat_map(|entry| {
            if cfg!(windows) {
                Executor::resolve_shell_path_process_entries_from_env(entry, env_vars)
            } else {
                vec![PathBuf::from(entry)]
            }
        })
        .collect()
}

/// Candidate suffixes appended to a bare command name: every PATHEXT entry
/// plus the extensionless name itself. Unix resolves only the bare name.
fn probe_extensions(pathext: Option<&str>) -> Vec<String> {
    if !cfg!(windows) {
        return vec![String::new()];
    }
    let mut extensions: Vec<String> = pathext
        .unwrap_or(".COM;.EXE;.BAT;.CMD")
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(str::to_string)
        .collect();
    extensions.push(String::new());
    extensions
}

fn command_on_path(command: &str, dirs: &[PathBuf], extensions: &[String]) -> bool {
    dirs.iter().any(|dir| {
        extensions
            .iter()
            .any(|ext| dir.join(format!("{command}{ext}")).is_file())
    })
}

fn contains_variable(word: &str) -> bool {
    let mut chars = word.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' {
            match chars.peek() {
                Some('{') | Some('_') => return true,
                Some(next) if next.is_ascii_alphabetic() => return true,
                _ => {}
            }
        }
    }
    false
}

fn path_kind(word: &str, tmp_base: &Path) -> Option<SyntaxKind> {
    // niubash#628: ask the engine's special-path registry before the
    // existence check. Simulated paths have no filesystem presence
    // (the enclave's physical /dev is an empty dir, /proc has no
    // physical counterpart at all), so the existence check can only
    // ever miss them — at command position they fell through to
    // UnknownToken and rendered red. A registry hit settles the word
    // as a path directly; a miss falls through to the existence
    // check unchanged, so genuinely nonexistent ordinary paths are
    // still marked (the registry only prevents false reds, it does
    // not disarm the check).
    if is_simulated_path(word) {
        return Some(SyntaxKind::Path);
    }
    let path = resolve_path(word, tmp_base);
    if path.exists() {
        return Some(SyntaxKind::Path);
    }
    if path_prefix_exists(&path) {
        return Some(SyntaxKind::PathPrefix);
    }
    None
}

/// Whether `word` spells a path the engine serves from simulation
/// rather than the filesystem, per the special-path registry
/// (rubash::special_paths) — the same classification the open points
/// use. Covers the classified spellings themselves (`/dev/null`,
/// `/dev/fd/3`, `/dev/tcp/host/port`, `/proc/self/status`, ...) plus
/// the simulated namespaces' directory spellings (`/proc`,
/// `/proc/self`, `/dev/fd`, ...), which the registry's enumeration
/// faces answer for.
fn is_simulated_path(word: &str) -> bool {
    let word = shell_unescape_word(word.trim_matches(|ch| matches!(ch, '\'' | '"' | ')' | '(')));
    let normalized = word.replace('\\', "/");
    if rubash::special_paths::classify_special_path(&normalized)
        != rubash::special_paths::SpecialPathClass::Filesystem
    {
        return true;
    }
    let trimmed = normalized.trim_end_matches('/');
    if rubash::special_paths::synthetic_proc_dir_entries(trimmed).is_some() {
        return true;
    }
    if trimmed == "/dev" {
        return true;
    }
    if let Some(name) = trimmed.strip_prefix("/dev/") {
        return rubash::special_paths::special_dev_entries()
            .iter()
            .any(|(entry, is_dir)| *is_dir && *entry == name);
    }
    false
}

fn resolve_path(word: &str, tmp_base: &Path) -> PathBuf {
    let word = shell_unescape_word(word.trim_matches(|ch| matches!(ch, '\'' | '"' | ')' | '(')));
    let expanded = if word == "~" {
        shell_home_dir()
    } else if let Some(rest) = word.strip_prefix("~/").or_else(|| word.strip_prefix("~\\")) {
        shell_home_dir().map(|home| home.join(shell_path_to_host_path(rest)))
    } else {
        None
    };

    let path = expanded
        .or_else(|| resolve_logical_shell_path(&word, tmp_base))
        .unwrap_or_else(|| PathBuf::from(word));
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn path_prefix_exists(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if file_name.is_empty() {
        return false;
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let Ok(entries) = std::fs::read_dir(parent) else {
        return false;
    };

    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .map(|candidate| candidate.starts_with(file_name))
            .unwrap_or(false)
    })
}

fn unquote_word(word: &str) -> String {
    if word.len() >= 2 {
        let bytes = word.as_bytes();
        if (bytes[0] == b'\'' && bytes[word.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[word.len() - 1] == b'"')
        {
            return shell_unescape_word(&word[1..word.len() - 1]);
        }
    }
    shell_unescape_word(word)
}

fn shell_unescape_word(word: &str) -> String {
    let mut value = String::new();
    let mut escaped = false;
    let mut chars = word.chars().peekable();
    while let Some(ch) = chars.next() {
        if escaped {
            value.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            if cfg!(windows) {
                match chars.peek().copied() {
                    Some(next) if !is_shell_escape_target(next) => {
                        value.push(ch);
                        continue;
                    }
                    Some(_) => {}
                    None => {
                        value.push(ch);
                        continue;
                    }
                }
            }
            escaped = true;
            continue;
        }
        value.push(ch);
    }
    if escaped {
        value.push('\\');
    }
    value
}

fn is_shell_escape_target(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '\\' | '\''
                | '"'
                | '$'
                | '`'
                | '!'
                | '&'
                | '|'
                | ';'
                | '<'
                | '>'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '*'
                | '?'
                | '#'
        )
}

fn kind_from_style_key(key: &str) -> Option<SyntaxKind> {
    let key = key.to_ascii_lowercase();
    [
        SyntaxKind::Default,
        SyntaxKind::UnknownToken,
        SyntaxKind::ReservedWord,
        SyntaxKind::Builtin,
        SyntaxKind::Command,
        SyntaxKind::CommandSeparator,
        SyntaxKind::Path,
        SyntaxKind::PathPrefix,
        SyntaxKind::SingleQuotedArgument,
        SyntaxKind::DoubleQuotedArgument,
        SyntaxKind::Variable,
        SyntaxKind::CommandSubstitution,
        SyntaxKind::SingleHyphenOption,
        SyntaxKind::DoubleHyphenOption,
        SyntaxKind::Assign,
        SyntaxKind::Redirection,
        SyntaxKind::Comment,
    ]
    .into_iter()
    .find(|kind| kind.style_key() == key)
}

fn default_styles() -> HashMap<SyntaxKind, Style> {
    HashMap::from([
        (SyntaxKind::Default, Style::new()),
        (SyntaxKind::UnknownToken, Style::new().fg(Color::Red)),
        (
            SyntaxKind::ReservedWord,
            Style::new().bold().fg(Color::Purple),
        ),
        (SyntaxKind::Builtin, Style::new().fg(Color::Cyan)),
        (SyntaxKind::Command, Style::new().fg(Color::Green)),
        (SyntaxKind::CommandSeparator, Style::new().bold()),
        (SyntaxKind::Path, Style::new().underline().fg(Color::Cyan)),
        (SyntaxKind::PathPrefix, Style::new().fg(Color::Cyan)),
        (
            SyntaxKind::SingleQuotedArgument,
            Style::new().fg(Color::Yellow),
        ),
        (
            SyntaxKind::DoubleQuotedArgument,
            Style::new().fg(Color::Yellow),
        ),
        (SyntaxKind::Variable, Style::new().fg(Color::LightPurple)),
        (
            SyntaxKind::CommandSubstitution,
            Style::new().fg(Color::LightPurple),
        ),
        (SyntaxKind::SingleHyphenOption, Style::new().fg(Color::Blue)),
        (SyntaxKind::DoubleHyphenOption, Style::new().fg(Color::Blue)),
        (SyntaxKind::Assign, Style::new().fg(Color::LightCyan)),
        (SyntaxKind::Redirection, Style::new().fg(Color::Yellow)),
        (SyntaxKind::Comment, Style::new().fg(Color::DarkGray)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn highlights_known_and_unknown_commands() {
        let highlighter = NiubashSyntaxHighlighter::new(&SyntaxHighlightConfig::default());
        let styled = highlighter.highlight("ls; definitely-not-a-niubash-command", 0);

        assert_eq!(
            style_for(&styled, "ls"),
            highlighter.style(SyntaxKind::Command)
        );
        assert_eq!(
            style_for(&styled, "definitely-not-a-niubash-command"),
            highlighter.style(SyntaxKind::UnknownToken)
        );
    }

    #[test]
    fn highlights_main_syntax_tokens() {
        let highlighter = NiubashSyntaxHighlighter::new(&SyntaxHighlightConfig::default());
        let styled = highlighter.highlight("echo \"$HOME\" | grep --ignore-case foo # note", 0);

        assert_eq!(
            style_for(&styled, "echo"),
            highlighter.style(SyntaxKind::Builtin)
        );
        assert_eq!(
            style_for(&styled, "\"$HOME\""),
            highlighter.style(SyntaxKind::DoubleQuotedArgument)
        );
        assert_eq!(
            style_for(&styled, "|"),
            highlighter.style(SyntaxKind::CommandSeparator)
        );
        assert_eq!(
            style_for(&styled, "--ignore-case"),
            highlighter.style(SyntaxKind::DoubleHyphenOption)
        );
        assert_eq!(
            style_for(&styled, "# note"),
            highlighter.style(SyntaxKind::Comment)
        );
    }

    #[test]
    fn highlights_existing_paths_and_prefixes() {
        let temp = unique_temp_dir("niubash-highlight-path");
        std::fs::create_dir_all(&temp).unwrap();
        let file = temp.join("sample.txt");
        std::fs::write(&file, "ok").unwrap();
        let prefix = temp.join("sam");
        let line = format!("cat {} {}", file.display(), prefix.display());

        let highlighter = NiubashSyntaxHighlighter::new(&SyntaxHighlightConfig::default());
        let styled = highlighter.highlight(&line, 0);

        assert_eq!(
            style_for(&styled, &file.display().to_string()),
            highlighter.style(SyntaxKind::Path)
        );
        assert_eq!(
            style_for(&styled, &prefix.display().to_string()),
            highlighter.style(SyntaxKind::PathPrefix)
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn highlights_aliases_and_exe_commands_as_commands() {
        let highlighter = NiubashSyntaxHighlighter::new_with_commands(
            &SyntaxHighlightConfig::default(),
            ["fetch"],
        );
        let styled = highlighter.highlight("fetch; which.exe winuxcmd", 0);

        assert_eq!(
            style_for(&styled, "fetch"),
            highlighter.style(SyntaxKind::Command)
        );
        assert_eq!(
            style_for(&styled, "which.exe"),
            highlighter.style(SyntaxKind::Command)
        );
    }

    #[test]
    fn highlights_dynamic_functions_as_commands() {
        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        state
            .lock()
            .unwrap()
            .functions
            .insert("deploy_site".to_string());
        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state,
        );
        let styled = highlighter.highlight("deploy_site --production", 0);

        assert_eq!(
            style_for(&styled, "deploy_site"),
            highlighter.style(SyntaxKind::Command)
        );
    }

    #[test]
    fn highlights_existing_paths_at_command_position() {
        let temp = unique_temp_dir("niubash-highlight-command-path");
        std::fs::create_dir_all(&temp).unwrap();
        let file = temp.join("script.sh");
        std::fs::write(&file, "echo ok").unwrap();
        let line = file.display().to_string();

        let highlighter = NiubashSyntaxHighlighter::new(&SyntaxHighlightConfig::default());
        let styled = highlighter.highlight(&line, 0);

        assert_eq!(
            style_for(&styled, &line),
            highlighter.style(SyntaxKind::Path)
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn highlights_escaped_paths_as_paths() {
        let temp = unique_temp_dir("niubash-highlight-escaped-path");
        std::fs::create_dir_all(&temp).unwrap();
        let file = temp.join("two words.txt");
        std::fs::write(&file, "ok").unwrap();
        let line = format!("cat {}", file.display().to_string().replace(' ', "\\ "));

        let highlighter = NiubashSyntaxHighlighter::new(&SyntaxHighlightConfig::default());
        let styled = highlighter.highlight(&line, 0);

        assert_eq!(
            style_for(&styled, &file.display().to_string().replace(' ', "\\ ")),
            highlighter.style(SyntaxKind::Path)
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn honors_style_overrides_and_max_length() {
        let mut config = SyntaxHighlightConfig {
            max_length: Some(2),
            ..Default::default()
        };
        config
            .styles
            .insert("command".to_string(), "fg=magenta,bold".to_string());

        let highlighter = NiubashSyntaxHighlighter::new(&config);
        assert_eq!(
            highlighter.style(SyntaxKind::Command).foreground,
            Some(Color::Magenta)
        );
        assert!(highlighter.style(SyntaxKind::Command).is_bold);
        assert_eq!(highlighter.highlight("echo", 0).buffer[0].0, Style::new());
    }

    #[test]
    fn highlights_plugin_provided_commands() {
        use crate::completion::external::{CommandDef, ExternalCompletionPlugin};

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        {
            let mut s = state.lock().unwrap();
            let mut ext = ExternalCompletionPlugin::new();
            ext.load_definitions(vec![CommandDef {
                command: "dsh".to_string(),
                description: Some("DeepSeek Harness CLI".to_string()),
                flags: vec![],
                subcommands: vec![],
            }]);
            s.add_plugin(Arc::new(ext));
        }

        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state,
        );
        let styled = highlighter.highlight("dsh web", 0);

        assert_eq!(
            style_for(&styled, "dsh"),
            highlighter.style(SyntaxKind::Command)
        );
    }

    #[test]
    fn highlights_repl_only_commands_as_builtins() {
        // niu#260: `self-update` (and friends) are intercepted by the REPL
        // before engine command resolution, so they never appear on PATH and
        // must not be painted as unknown tokens.
        let highlighter = NiubashSyntaxHighlighter::new(&SyntaxHighlightConfig::default());

        for word in ["self-update", "update-niubash", "logout"] {
            let styled = highlighter.highlight(word, 0);
            assert_eq!(
                style_for(&styled, word),
                highlighter.style(SyntaxKind::Builtin),
                "expected {word} to highlight as builtin"
            );
        }
    }

    #[test]
    fn repl_only_words_in_argument_position_stay_default() {
        let highlighter = NiubashSyntaxHighlighter::new(&SyntaxHighlightConfig::default());
        let styled = highlighter.highlight("echo self-update", 0);

        assert_eq!(
            style_for(&styled, "self-update"),
            highlighter.style(SyntaxKind::Default)
        );
    }

    /// Write a stand-in installed-command file for the live-probe tests.
    /// On unix a PATH entry only counts as a command when it carries an
    /// execute bit (GNU PATH lookup semantics), and `fs::write` creates
    /// 0644 — which the probe correctly ignores there. Windows has no
    /// execute bit; the `.exe` suffix matched via PATHEXT is what counts,
    /// so the chmod is unix-only. (niubash#574)
    fn write_probe_command(path: &std::path::Path) {
        std::fs::write(path, "binary").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// On-disk file name for a live-probe fixture command. On Windows
    /// the engine resolves a bare word through PATHEXT, so the fixture
    /// file carries the `.exe` suffix there. On unix the engine
    /// resolves only the exact file name — verified against the engine
    /// (rubash@368353e via the niu binary): a PATH holding only
    /// `probe574fresh.exe` (0755) leaves `command -v probe574fresh`
    /// at rc=1 and execution at rc=127, even with PATHEXT set — so
    /// the unix fixture file uses the bare name. (niubash#574)
    #[cfg(unix)]
    fn probe_command_file_name(base: &str) -> String {
        base.to_string()
    }

    #[cfg(not(unix))]
    fn probe_command_file_name(base: &str) -> String {
        format!("{base}.exe")
    }

    /// niubash#574: a command installed mid-session is absent from the
    /// startup command set, but the live probe finds it on PATH once the
    /// memo is invalidated (the `wpm install` hook clears it).
    #[test]
    fn live_probe_renders_mid_session_install_known_after_invalidation() {
        let temp = unique_temp_dir("niubash-highlight-probe");
        std::fs::create_dir_all(&temp).unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        state.lock().unwrap().env_vars =
            HashMap::from([("PATH".to_string(), temp.display().to_string())]);

        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state.clone(),
        );

        let styled = highlighter.highlight("probe574fresh --help", 0);
        assert_eq!(
            style_for(&styled, "probe574fresh"),
            highlighter.style(SyntaxKind::UnknownToken)
        );

        // Install mid-session: the memoized miss still answers, so the name
        // keeps rendering unknown until the shell clears the memo.
        write_probe_command(&temp.join(probe_command_file_name("probe574fresh")));
        let styled = highlighter.highlight("probe574fresh --help", 0);
        assert_eq!(
            style_for(&styled, "probe574fresh"),
            highlighter.style(SyntaxKind::UnknownToken)
        );

        state.lock().unwrap().command_probe.clear();
        let styled = highlighter.highlight("probe574fresh --help", 0);
        assert_eq!(
            style_for(&styled, "probe574fresh"),
            highlighter.style(SyntaxKind::Command)
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    /// The memo serves repeat paints: highlighting the same unknown name
    /// must not re-walk PATH every keystroke (niubash#574 acceptance).
    #[test]
    fn live_probe_memo_prevents_repeated_path_walks() {
        let temp = unique_temp_dir("niubash-highlight-probe-memo");
        std::fs::create_dir_all(&temp).unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        state.lock().unwrap().env_vars =
            HashMap::from([("PATH".to_string(), temp.display().to_string())]);

        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state.clone(),
        );

        for flag in ["--help", "--version", "--flag"] {
            let styled = highlighter.highlight(&format!("probe574memo {flag}"), 0);
            assert_eq!(
                style_for(&styled, "probe574memo"),
                highlighter.style(SyntaxKind::UnknownToken)
            );
        }
        assert_eq!(state.lock().unwrap().command_probe.probe_count(), 1);

        let _ = std::fs::remove_dir_all(temp);
    }

    /// Gibberish still renders unknown through the probe path even after
    /// invalidation — the probe answers what the filesystem says.
    #[test]
    fn live_probe_keeps_gibberish_unknown() {
        let temp = unique_temp_dir("niubash-highlight-probe-gibberish");
        std::fs::create_dir_all(&temp).unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        state.lock().unwrap().env_vars =
            HashMap::from([("PATH".to_string(), temp.display().to_string())]);

        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state.clone(),
        );

        state.lock().unwrap().command_probe.clear();
        let styled = highlighter.highlight("probe574notreal --help", 0);
        assert_eq!(
            style_for(&styled, "probe574notreal"),
            highlighter.style(SyntaxKind::UnknownToken)
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    /// A PATH assignment or a `hash` table mutation retires the memo
    /// through the fingerprint — no explicit clear needed (niubash#574).
    #[test]
    fn live_probe_retires_memo_when_path_or_hash_table_moves() {
        let temp_a = unique_temp_dir("niubash-highlight-probe-fp-a");
        let temp_b = unique_temp_dir("niubash-highlight-probe-fp-b");
        std::fs::create_dir_all(&temp_a).unwrap();
        std::fs::create_dir_all(&temp_b).unwrap();
        write_probe_command(&temp_b.join(probe_command_file_name("probe574fp")));

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        state.lock().unwrap().env_vars =
            HashMap::from([("PATH".to_string(), temp_a.display().to_string())]);

        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state.clone(),
        );

        let styled = highlighter.highlight("probe574fp --help", 0);
        assert_eq!(
            style_for(&styled, "probe574fp"),
            highlighter.style(SyntaxKind::UnknownToken)
        );

        // PATH gained the install dir: the fingerprint moved, so the next
        // paint re-probes and finds the command with no explicit clear.
        let grown = format!("{};{}", temp_a.display(), temp_b.display());
        state
            .lock()
            .unwrap()
            .env_vars
            .insert("PATH".to_string(), grown);
        let styled = highlighter.highlight("probe574fp --help", 0);
        assert_eq!(
            style_for(&styled, "probe574fp"),
            highlighter.style(SyntaxKind::Command)
        );

        // A `hash` table mutation also retires the memo: shrink PATH again
        // so the miss is memoized under a new fingerprint, add the command
        // where the new PATH can see it, then flip the serialized table
        // the engine publishes on `hash -r`/`-d`/`-p`.
        state
            .lock()
            .unwrap()
            .env_vars
            .insert("PATH".to_string(), temp_a.display().to_string());
        let styled = highlighter.highlight("probe574fp --help", 0);
        assert_eq!(
            style_for(&styled, "probe574fp"),
            highlighter.style(SyntaxKind::UnknownToken)
        );
        write_probe_command(&temp_a.join(probe_command_file_name("probe574fp")));
        state.lock().unwrap().env_vars.insert(
            "__RUBASH_HASH_TABLE".to_string(),
            "probe574fp=D:/x$1$1".to_string(),
        );
        let styled = highlighter.highlight("probe574fp --help", 0);
        assert_eq!(
            style_for(&styled, "probe574fp"),
            highlighter.style(SyntaxKind::Command)
        );

        let _ = std::fs::remove_dir_all(temp_a);
        let _ = std::fs::remove_dir_all(temp_b);
    }

    /// Bare words only reach the PATH probe — names carrying a path
    /// separator or drive colon stay path-kind candidates, never probes.
    #[test]
    fn live_probe_skips_path_shaped_words() {
        let temp = unique_temp_dir("niubash-highlight-probe-skip");
        std::fs::create_dir_all(&temp).unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        state.lock().unwrap().env_vars =
            HashMap::from([("PATH".to_string(), temp.display().to_string())]);

        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state.clone(),
        );

        let styled = highlighter.highlight("dir/probe574missing", 0);
        assert_eq!(
            style_for(&styled, "dir/probe574missing"),
            highlighter.style(SyntaxKind::UnknownToken)
        );
        assert_eq!(state.lock().unwrap().command_probe.probe_count(), 0);

        let _ = std::fs::remove_dir_all(temp);
    }

    fn style_for(styled: &StyledText, fragment: &str) -> Style {
        styled
            .buffer
            .iter()
            .find(|(_, text)| text == fragment)
            .or_else(|| {
                styled
                    .buffer
                    .iter()
                    .find(|(_, text)| text.contains(fragment))
            })
            .map(|(style, _)| *style)
            .unwrap_or_else(|| panic!("missing fragment {fragment:?} in {:?}", raw_parts(styled)))
    }

    fn raw_parts(styled: &StyledText) -> Vec<String> {
        styled.buffer.iter().map(|(_, text)| text.clone()).collect()
    }

    /// niubash#627 G2: highlighting roots `/tmp` spellings in the
    /// shared state's session tmp base (the engine-resolved TMPDIR
    /// backing) — a file that exists only under that base highlights
    /// as a path. A stateless highlighter falls back to the process
    /// temp dir and misses the same spelling.
    #[test]
    fn issue627_g2_highlight_follows_the_states_tmp_base() {
        use std::sync::{Arc, Mutex};

        struct EnvGuard {
            name: &'static str,
            previous: Option<std::ffi::OsString>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.previous {
                    Some(value) => std::env::set_var(self.name, value),
                    None => std::env::remove_var(self.name),
                }
            }
        }

        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scratch = std::env::temp_dir().join(format!(
            "niubash-g2-highlight-{}-{}",
            std::process::id(),
            nanos
        ));
        let root = scratch.join("root");
        let session_base = scratch.join("session-temp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&session_base).unwrap();
        let file_name = format!("g2mark-{}.txt", std::process::id());
        std::fs::write(session_base.join(&file_name), "x").unwrap();
        let _root = EnvGuard {
            name: "NIU_ROOT",
            previous: std::env::var_os("NIU_ROOT"),
        };
        std::env::set_var("NIU_ROOT", &root);

        let mut state = CompletionState::new(PathBuf::from("/"));
        state.tmp_base = session_base.clone();
        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            Arc::new(Mutex::new(state)),
        );
        let line = format!("cat /tmp/{file_name}");
        let styled = highlighter.highlight(&line, 0);
        assert_eq!(
            style_for(&styled, &format!("/tmp/{file_name}")),
            highlighter.style(SyntaxKind::Path)
        );

        // Stateless: the process temp holds no such file, so the
        // spelling is not confirmed as a path.
        let plain = NiubashSyntaxHighlighter::new(&SyntaxHighlightConfig::default());
        let styled = plain.highlight(&line, 0);
        assert_ne!(
            style_for(&styled, &format!("/tmp/{file_name}")),
            plain.style(SyntaxKind::Path)
        );

        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn issue628_simulated_spellings_are_paths() {
        // Classified spellings: the registry arm answers on every
        // platform for the shell-invented families (device, fd
        // alias, net); the /proc content spellings reach Path on
        // Windows through the registry and elsewhere through the
        // real procfs existence check — the styled outcome is the
        // same.
        for spelling in [
            "/dev/null",
            "/dev/zero",
            "/dev/full",
            "/dev/stdin",
            "/dev/stdout",
            "/dev/stderr",
            "/dev/fd/3",
            "/dev/tcp/127.0.0.1/80",
            "/dev/udp/example.invalid/53",
            "/proc/self/status",
            "/proc/self/exe",
            "/proc/self/root",
            "/dev/fd",
            "/dev/tcp",
            "/proc/self",
            "/proc",
        ] {
            assert_eq!(
                path_kind(spelling, &std::env::temp_dir()),
                Some(SyntaxKind::Path),
                "{spelling} must highlight as a path"
            );
        }
        // The registry pre-check itself: /proc directory spellings
        // are simulation only where the simulation lives.
        assert!(is_simulated_path("/dev/null"));
        assert!(is_simulated_path("/dev/fd/3"));
        assert!(is_simulated_path("/dev/tcp/127.0.0.1/80"));
        assert_eq!(is_simulated_path("/proc/self"), cfg!(windows));
        // niubash#758: the root link classifies through the same
        // registry arm as its sibling links.
        assert_eq!(is_simulated_path("/proc/self/root"), cfg!(windows));
        // The zero/full devices joined the registry as first-class
        // classes with the audit-D5 simulation (rubash#1898, landed
        // via rubash#2015), so the registry arm serves them now —
        // niubash#688 re-derived this pin from the registry.
        assert!(is_simulated_path("/dev/zero"));
        assert!(is_simulated_path("/dev/full"));
        // Spellings the registry does not serve are not simulated,
        // and ordinary words fall through to the existence check
        // untouched.
        assert!(!is_simulated_path("/etc/passwd"));
        assert!(!is_simulated_path("plain-word"));
    }

    #[test]
    fn issue628_simulated_paths_do_not_render_as_unknown() {
        let highlighter = NiubashSyntaxHighlighter::new(&SyntaxHighlightConfig::default());

        // Command position: a simulated path used as the command
        // word must not fall to UnknownToken (the red foreground).
        let styled = highlighter.highlight("/dev/null", 0);
        assert_eq!(
            style_for(&styled, "/dev/null"),
            highlighter.style(SyntaxKind::Path)
        );

        // Argument position: recognized as paths, not left Default.
        let styled = highlighter.highlight("cat /dev/null /dev/fd/3", 0);
        assert_eq!(
            style_for(&styled, "/dev/null"),
            highlighter.style(SyntaxKind::Path)
        );
        assert_eq!(
            style_for(&styled, "/dev/fd/3"),
            highlighter.style(SyntaxKind::Path)
        );

        // The registry pre-check must not disarm the existence
        // check: a genuinely nonexistent ordinary path stays
        // unstyled as an argument and UnknownToken at command
        // position.
        let styled = highlighter.highlight("cat /no/such/file-xyz-628", 0);
        assert_eq!(
            style_for(&styled, "/no/such/file-xyz-628"),
            highlighter.style(SyntaxKind::Default)
        );
        let styled = highlighter.highlight("/no/such/file-xyz-628", 0);
        assert_eq!(
            style_for(&styled, "/no/such/file-xyz-628"),
            highlighter.style(SyntaxKind::UnknownToken)
        );
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
    }
}
