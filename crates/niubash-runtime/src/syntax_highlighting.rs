//! Native Niubash line highlighter.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};
use rubash::api::Session;

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
    /// GAP-7 (niubash#863): the editor flavor's handle on the
    /// REPL's shell — the known-command probe reads the live
    /// completion view through one short completion attempt opened
    /// on it. `None` for highlighters built without a shell (their
    /// probe falls back to the process environment, as before).
    shell: Option<crate::shell::EditorShellHandle>,
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
            shell: None,
            local_probe: Mutex::new(CommandProbe::default()),
            max_length: config.max_length,
        }
    }

    /// GAP-7 (niubash#863): attach the editor shell handle (the
    /// REPL's editor instance). The handle's safety contract is
    /// documented on `EditorShellHandle`.
    pub fn with_shell(
        mut self,
        shell: std::rc::Rc<std::cell::RefCell<crate::shell::Shell>>,
    ) -> Self {
        self.shell = Some(crate::shell::EditorShellHandle::new(shell));
        self
    }

    fn style(&self, kind: SyntaxKind) -> Style {
        self.styles.get(&kind).copied().unwrap_or_default()
    }

    /// The backing directory for the session's `/tmp`: the value the
    /// shell syncs into the shared completion state (engine-resolved
    /// from the session env, so a session TMPDIR assignment moves it
    /// immediately — niubash#627 G2). Highlighters built without
    /// engine state fall back to the process temp dir.
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

    /// The engine token view of `line` (GAP-9, niubash#863): the
    /// live session's view when this highlighter carries the
    /// editor shell — a plain `&self` read through the handle
    /// (the Q-H shape: never a completion attempt) — the
    /// stateless oracle's view otherwise. `Session` is
    /// deliberately `!Send` (rubash pins it with
    /// `PhantomData<Rc<()>>`), so a shell-less highlighter
    /// cannot own a fallback session and stay `Send` for
    /// ruline's `Highlighter`; the oracle is the fallback, the
    /// same one the completion contexts use.
    fn token_view(&self, line: &str) -> rubash::api::TokenView {
        if let Some(handle) = &self.shell {
            handle.with_shell_mut(|shell| shell.session().token_view(line))
        } else {
            crate::completion::stateless_token_view(line)
        }
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

        let view = self.token_view(line);
        if view.tokens.is_empty() {
            return plain_text(line);
        }

        let mut styled = StyledText::new();
        let mut pos = 0;

        for span in &view.tokens {
            if pos < span.start {
                styled.push((
                    self.style(SyntaxKind::Default),
                    line[pos..span.start].to_string(),
                ));
            }

            let kind = self.classify_span(span);
            styled.push((self.style(kind), span.text.clone()));
            pos = span.end;
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

impl NiubashSyntaxHighlighter {
    /// The style of one engine token span (GAP-9, niubash#863).
    /// Separators, redirections, comments and here-doc bodies
    /// map straight off the span kind; word-shaped spans map off
    /// the engine-folded role — Reserved / Assignment / Builtin
    /// are the engine's verdicts, not the retired host tables.
    fn classify_span(&self, span: &rubash::api::TokenSpan) -> SyntaxKind {
        use rubash::api::{TokenKindView, TokenRole};
        match span.kind {
            TokenKindView::Comment => SyntaxKind::Comment,
            TokenKindView::HereDocBody => SyntaxKind::Default,
            TokenKindView::RedirectOut
            | TokenKindView::RedirectIn
            | TokenKindView::Append
            | TokenKindView::RedirectErr
            | TokenKindView::RedirectErrAppend
            | TokenKindView::HereDoc
            | TokenKindView::HereString => SyntaxKind::Redirection,
            kind if kind.is_separator() => SyntaxKind::CommandSeparator,
            _ => match span.role {
                TokenRole::Reserved => SyntaxKind::ReservedWord,
                TokenRole::Assignment => SyntaxKind::Assign,
                TokenRole::Builtin => SyntaxKind::Builtin,
                TokenRole::RedirectTarget => {
                    path_kind(&unquote_word(&span.text), &self.session_tmp_base())
                        .unwrap_or(SyntaxKind::Default)
                }
                TokenRole::Plain => self.classify_plain_word(span),
            },
        }
    }

    /// A word-shaped span the engine roles `Plain`: the
    /// command-position chain when the span stands where a
    /// command word may stand, the argument chain otherwise.
    /// The span's `command_position` replaces the retired
    /// scanner's `expect_command` state; the `role` already
    /// settled the reserved-word / builtin / prefix-assignment
    /// questions, so this chain only keeps the host-owned
    /// decisions — REPL-only commands, the known-command probe,
    /// and the path fallback.
    fn classify_plain_word(&self, span: &rubash::api::TokenSpan) -> SyntaxKind {
        if span.command_position {
            let unquoted = unquote_word(&span.text);
            // REPL-only commands (`self-update`, `update-niubash`,
            // `logout`) never resolve on PATH, but the interactive
            // shell accepts them, so color them like builtins
            // instead of unknown tokens (niu#260).
            if crate::repl::is_repl_reserved_command(&unquoted) {
                return SyntaxKind::Builtin;
            }
            if self.is_known_command(&unquoted) {
                return SyntaxKind::Command;
            }
            if let Some(path_kind) = path_kind(&unquoted, &self.session_tmp_base()) {
                return path_kind;
            }
            return SyntaxKind::UnknownToken;
        }
        classify_argument_word(&span.text, &self.session_tmp_base())
    }
}

/// The argument-position chain, unchanged from the retired
/// scanner's: quotes, command substitution, variables, options,
/// then a trailing assignment, then the path fallback.
fn classify_argument_word(word: &str, tmp_base: &Path) -> SyntaxKind {
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
    if is_assignment(&unquote_word(word)) {
        return SyntaxKind::Assign;
    }
    path_kind(&unquote_word(word), tmp_base).unwrap_or(SyntaxKind::Default)
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

        // Host-owned dynamic names first — aliases and plugin
        // names live on the shared state and never needed the
        // engine (a state-carrying highlighter away from a live
        // shell still resolves them).
        match state.lock() {
            Ok(state) => {
                if state
                    .aliases
                    .iter()
                    .chain(state.plugin_command_names().iter())
                    .any(|name| name.eq_ignore_ascii_case(command))
                {
                    return true;
                }
            }
            Err(_) => return false,
        }

        // GAP-7 (niubash#863): the dynamic function names and the
        // probe's env come from one live capture of the engine's
        // completion view, read through a short completion attempt
        // on the editor shell (GAP-4's bridge capture, guard form).
        // Without a shell handle the session faces have no source:
        // process-env probe.
        let Some(view) = self.live_completion_view() else {
            return self.probe_process_env(command);
        };
        self.known_command_name_with_view(command, &view, state)
    }

    /// The live completion view, read through one short completion
    /// attempt opened on the editor shell handle (GAP-7,
    /// niubash#863). `None` when this highlighter carries no shell.
    fn live_completion_view(&self) -> Option<rubash::api::CompletionView> {
        let handle = self.shell.as_ref()?;
        let mut shell = handle.borrow_mut();
        let attempt = shell.session_mut().begin_completion_attempt();
        Some(attempt.session().completion_view())
    }

    /// The view-consuming half of the known-command probe: resolve
    /// `command` against a live completion view (function names,
    /// then the memoized PATH probe keyed by the view's env
    /// fingerprint). Split out so a driver holding an open attempt
    /// can resolve against the attempt's view directly — the
    /// nested-completion shape GAP-7 makes live (the GAP-4 pin's
    /// dark shape: a probe consulted under nesting observed no
    /// engine source at all).
    pub(crate) fn known_command_name_with_view(
        &self,
        command: &str,
        view: &rubash::api::CompletionView,
        state: &Arc<Mutex<CompletionState>>,
    ) -> bool {
        // Function names and the memoized probe answers share the
        // completion lock; a memo miss reads the env from the
        // capture and re-locks only to record the outcome, so the
        // filesystem walk never runs under the lock (niubash#574).
        let (fingerprint, env_vars) = {
            let Ok(mut state) = state.lock() else {
                return false;
            };
            if view
                .functions
                .iter()
                .any(|name| name.eq_ignore_ascii_case(command))
            {
                return true;
            }
            if !probe_eligible(command) {
                return false;
            }
            // The memo key is the engine's own env fingerprint
            // (GAP-4): any variable change retires the memo,
            // covering the PATH / PATHEXT / hash-table moves the
            // retired host fingerprint keyed on (niubash#574).
            let fingerprint = view.env_fingerprint.to_string();
            match state.command_probe.lookup(&fingerprint, command) {
                Some(known) => return known,
                None => {
                    let env_vars: HashMap<String, String> = view.env.iter().cloned().collect();
                    (fingerprint, env_vars)
                }
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
                Session::resolve_shell_path_process_entries_from_env(entry, env_vars)
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
    fn highlights_engine_builtins_missing_from_the_old_static_table() {
        // GAP-9 (niubash#863): the builtin verdict comes from the
        // engine-folded role now, not the retired host table —
        // `printf` is an engine builtin the old `is_shell_builtin`
        // table never listed, so it used to fall through to the
        // known-command probe (Command) instead of painting
        // Builtin.
        let highlighter = NiubashSyntaxHighlighter::new(&SyntaxHighlightConfig::default());
        let styled = highlighter.highlight("printf '%s\\n' hello", 0);
        assert_eq!(
            style_for(&styled, "printf"),
            highlighter.style(SyntaxKind::Builtin)
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
        // GAP-4 (niubash#863): the function name reaches the
        // highlighter from the engine's live completion view, not
        // from a state field — define it in a real shell and hand
        // the shell to the highlighter, the same shape production
        // highlights in. GAP-7: the view is read through a short
        // completion attempt on that handle.
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);
        let mut shell = crate::shell::Shell::new().unwrap();
        assert!(shell.execute_script("deploy_site() { true; }").is_ok());
        let shell = std::rc::Rc::new(std::cell::RefCell::new(shell));

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state,
        )
        .with_shell(shell.clone());
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

    /// GAP-4 (niubash#863): a real shell whose session PATH is
    /// `path` — the #574 live-probe pins read the probe env from
    /// the engine's live completion view, so their env lives in
    /// the session itself (the state fields they used to poke
    /// retired with the snapshot). Returns the Rc; callers hand it
    /// to the highlighter via `with_shell` (GAP-7: the attempt
    /// handle replaces the installed bridge).
    fn probe_shell(path: &str) -> std::rc::Rc<std::cell::RefCell<crate::shell::Shell>> {
        let mut shell = crate::shell::Shell::new().unwrap();
        shell.session_mut().variables().set("PATH", path);
        std::rc::Rc::new(std::cell::RefCell::new(shell))
    }

    fn probe_shell_set(
        shell: &std::rc::Rc<std::cell::RefCell<crate::shell::Shell>>,
        name: &str,
        value: &str,
    ) {
        shell
            .borrow_mut()
            .session_mut()
            .variables()
            .set(name, value);
    }

    /// niubash#574: a command installed mid-session is absent from the
    /// startup command set, but the live probe finds it on PATH once the
    /// memo is invalidated (the `wpm install` hook clears it).
    #[test]
    fn live_probe_renders_mid_session_install_known_after_invalidation() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);
        let temp = unique_temp_dir("niubash-highlight-probe");
        std::fs::create_dir_all(&temp).unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let shell = probe_shell(&temp.display().to_string());

        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state.clone(),
        )
        .with_shell(shell.clone());

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
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);
        let temp = unique_temp_dir("niubash-highlight-probe-memo");
        std::fs::create_dir_all(&temp).unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let shell = probe_shell(&temp.display().to_string());

        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state.clone(),
        )
        .with_shell(shell.clone());

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
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);
        let temp = unique_temp_dir("niubash-highlight-probe-gibberish");
        std::fs::create_dir_all(&temp).unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let shell = probe_shell(&temp.display().to_string());

        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state.clone(),
        )
        .with_shell(shell.clone());

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
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);
        let temp_a = unique_temp_dir("niubash-highlight-probe-fp-a");
        let temp_b = unique_temp_dir("niubash-highlight-probe-fp-b");
        std::fs::create_dir_all(&temp_a).unwrap();
        std::fs::create_dir_all(&temp_b).unwrap();
        write_probe_command(&temp_b.join(probe_command_file_name("probe574fp")));

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let shell = probe_shell(&temp_a.display().to_string());

        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state.clone(),
        )
        .with_shell(shell.clone());

        let styled = highlighter.highlight("probe574fp --help", 0);
        assert_eq!(
            style_for(&styled, "probe574fp"),
            highlighter.style(SyntaxKind::UnknownToken)
        );

        // PATH gained the install dir: the fingerprint moved, so the next
        // paint re-probes and finds the command with no explicit clear.
        // (GAP-4: the fingerprint is the engine's whole-env fingerprint,
        // so a PATH move — like the hash-table move below — retires the
        // memo through the same single key.)
        let grown = format!("{};{}", temp_a.display(), temp_b.display());
        probe_shell_set(&shell, "PATH", &grown);
        let styled = highlighter.highlight("probe574fp --help", 0);
        assert_eq!(
            style_for(&styled, "probe574fp"),
            highlighter.style(SyntaxKind::Command)
        );

        // A `hash` table mutation also retires the memo: shrink PATH again
        // so the miss is memoized under a new fingerprint, add the command
        // where the new PATH can see it, then flip the serialized table
        // the engine publishes on `hash -r`/`-d`/`-p`.
        probe_shell_set(&shell, "PATH", &temp_a.display().to_string());
        let styled = highlighter.highlight("probe574fp --help", 0);
        assert_eq!(
            style_for(&styled, "probe574fp"),
            highlighter.style(SyntaxKind::UnknownToken)
        );
        write_probe_command(&temp_a.join(probe_command_file_name("probe574fp")));
        probe_shell_set(&shell, "__RUBASH_HASH_TABLE", "probe574fp=D:/x$1$1");
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
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env_restore = crate::test_support::ProcessEnvRestore::new(&["PATH", "PATHEXT"]);
        let temp = unique_temp_dir("niubash-highlight-probe-skip");
        std::fs::create_dir_all(&temp).unwrap();

        let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
        let shell = probe_shell(&temp.display().to_string());

        let highlighter = NiubashSyntaxHighlighter::new_with_commands_and_state(
            &SyntaxHighlightConfig::default(),
            std::iter::empty::<String>(),
            state.clone(),
        )
        .with_shell(shell.clone());

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
