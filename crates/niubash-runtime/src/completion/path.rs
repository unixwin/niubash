// Path completion for WinSH
// Provides Tab completion for files and directories

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::completion::file_colors::{CandidateKind, FileColors};
use crate::completion::{CompletionBehavior, CompletionContext, CompletionResult};
use crate::path_utils::{resolve_logical_shell_path, shell_home_dir, shell_path_to_host_path};
use anyhow::Result;

// ─── Session-scoped directory listings (niubash#747) ─────────────────────
//
// Candidate enumeration used to pay a symlink-following stat(2) for
// EVERY directory entry — the LS_COLORS exec-bit refinement in the old
// `candidate_kind` — plus a fresh `read_dir` for every keystroke the
// editor re-queried with. On ordinary directories that is merely
// wasteful; on virtual trees (/proc, /sys, /dev) and very large
// directories it made Tab completion unusable.
//
// Enumeration now records only what `read_dir` already knows — the
// entry name and its dirent type — and never stats. A session-scoped
// cache keeps the enumerated listing for the (directory + line stem)
// being completed in, so navigation/filter re-queries filter the
// cached names instead of re-walking the directory. The one
// refinement that genuinely needs metadata (the Unix exec bit behind
// the LS_COLORS `ex` kind) is deferred until the candidate list is
// built and sorted, and runs for at most [`EXEC_REFINE_LIMIT`]
// regular-file candidates from the head of the list — the window a
// completion grid can show first. Verdicts are memoized on the cache,
// so one session pays an entry's stat at most once, and only for
// entries near the head of the list.

/// How many head-of-list regular-file candidates get their exec bit
/// refined (one stat each) per directory listing. A completion grid
/// shows at most a screenful (~100 cells); 128 covers the first
/// screen with margin.
pub(crate) const EXEC_REFINE_LIMIT: usize = 128;

/// How many directory listings one session cache retains. Drilling
/// into a subdirectory and back touches two; four leaves headroom
/// without letting a long session accumulate walks.
const MAX_CACHED_LISTINGS: usize = 4;

/// Identifies the completion session a directory listing belongs to:
/// the shell's current directory plus the input line up to the start
/// of the word being completed. Re-queries while the user types or
/// filters inside one word keep the key; starting a new word or a new
/// command line changes it and retires every cached listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKey {
    current_dir: PathBuf,
    line_stem: String,
}

impl SessionKey {
    pub fn new(current_dir: PathBuf, line_stem: String) -> Self {
        Self {
            current_dir,
            line_stem,
        }
    }
}

/// A directory entry's kind as the dirent reports it — the only
/// classification enumeration is allowed to make (niubash#747).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListedKind {
    Directory,
    Symlink,
    /// Any other typed entry (regular file, fifo, socket, …).
    Other,
    /// The dirent carried no usable type (`file_type()` errored).
    Unknown,
}

impl ListedKind {
    fn from_file_type(file_type: &fs::FileType) -> Self {
        if file_type.is_dir() {
            ListedKind::Directory
        } else if file_type.is_symlink() {
            ListedKind::Symlink
        } else {
            ListedKind::Other
        }
    }

    /// The LS_COLORS kind this dirent kind alone proves — everything
    /// except `Executable`, which needs the mode bits (a stat).
    fn candidate_kind(self) -> CandidateKind {
        match self {
            ListedKind::Directory => CandidateKind::Directory,
            ListedKind::Symlink => CandidateKind::Symlink,
            ListedKind::Other | ListedKind::Unknown => CandidateKind::File,
        }
    }
}

/// One enumerated directory entry: a name and its dirent kind.
#[derive(Debug, Clone)]
pub(crate) struct ListedEntry {
    pub name: String,
    pub kind: ListedKind,
}

/// An enumerated directory: the stat-free name list one session
/// reuses across re-queries.
#[derive(Debug)]
pub struct DirListing {
    entries: Vec<ListedEntry>,
    /// Directory mtime captured just after enumeration; a later
    /// mismatch retires the listing (adding/removing/renaming an
    /// entry bumps it). `None` when the platform reported no mtime —
    /// such a listing is trusted for its session. Filesystems with
    /// coarse mtime granularity can lag one tick behind a same-tick
    /// change; that is the snapshot semantics one completion session
    /// is allowed (procfs membership churn inside a session is the
    /// same accepted case).
    mtime: Option<SystemTime>,
}

impl DirListing {
    pub(crate) fn entries(&self) -> &[ListedEntry] {
        &self.entries
    }

    fn mtime_still_current(&self, dir: &Path) -> bool {
        match self.mtime {
            Some(recorded) => fs::metadata(dir)
                .and_then(|md| md.modified())
                .map(|now| now == recorded)
                .unwrap_or(false),
            None => true,
        }
    }
}

/// Enumerate one directory the stat-free way: names + dirent kinds.
/// `None` when the directory cannot be read at all (the old walk's
/// `read_dir` error case).
fn enumerate_dir(dir: &Path) -> Option<DirListing> {
    let read = fs::read_dir(dir).ok()?;
    let mut entries = Vec::new();
    for entry in read.flatten() {
        let kind = match entry.file_type() {
            Ok(ft) => ListedKind::from_file_type(&ft),
            Err(_) => ListedKind::Unknown,
        };
        entries.push(ListedEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
        });
    }
    let mtime = fs::metadata(dir).and_then(|md| md.modified()).ok();
    Some(DirListing { entries, mtime })
}

/// Enumerate without a session cache (contexts built outside the
/// completer's session plumbing, e.g. unit tests).
pub(crate) fn uncached_listing(dir: &Path) -> Option<Arc<DirListing>> {
    enumerate_dir(dir).map(Arc::new)
}

/// The shared directory-listing cache, owned by `CompletionState`
/// and lent to each query through a [`DirCacheHandle`].
#[derive(Debug, Default)]
pub struct DirListingCache {
    session: Option<SessionKey>,
    listings: Vec<(PathBuf, Arc<DirListing>)>,
    exec_verdicts: HashMap<(PathBuf, String), bool>,
    enumerations: usize,
    metadata_calls: usize,
}

/// One query's borrow of the shared listing cache, stamped with the
/// session the query belongs to. A session change clears the cache
/// on first touch.
#[derive(Debug, Clone)]
pub struct DirCacheHandle {
    shared: Arc<Mutex<DirListingCache>>,
    session: SessionKey,
}

impl DirCacheHandle {
    /// A standalone handle with its own cache (tests, probes).
    pub fn new(session: SessionKey) -> Self {
        Self {
            shared: Arc::new(Mutex::new(DirListingCache::default())),
            session,
        }
    }

    /// Wrap the completer state's shared cache for one session.
    pub(crate) fn with_shared(shared: Arc<Mutex<DirListingCache>>, session: SessionKey) -> Self {
        Self { shared, session }
    }

    /// The listing for `dir`, enumerating on first use in this
    /// session and reusing it while the directory's mtime holds.
    pub fn listing(&self, dir: &Path) -> Option<Arc<DirListing>> {
        let Ok(mut cache) = self.shared.lock() else {
            // A poisoned lock must not take path completion down:
            // answer uncached.
            return uncached_listing(dir);
        };
        if cache.session.as_ref() != Some(&self.session) {
            cache.session = Some(self.session.clone());
            cache.listings.clear();
            cache.exec_verdicts.clear();
        }
        if let Some(pos) = cache.listings.iter().position(|(d, _)| d == dir) {
            let (_, listing) = cache.listings.remove(pos);
            if listing.mtime_still_current(dir) {
                cache.listings.push((dir.to_path_buf(), listing.clone()));
                return Some(listing);
            }
            // Stale (the directory changed under the session):
            // fall through and re-enumerate.
        }
        let listing = Arc::new(enumerate_dir(dir)?);
        cache.enumerations += 1;
        cache.listings.push((dir.to_path_buf(), listing.clone()));
        if cache.listings.len() > MAX_CACHED_LISTINGS {
            cache.listings.remove(0);
        }
        Some(listing)
    }

    /// The exec verdict for one entry, memoized for the session.
    /// Computing a verdict is the ONLY per-entry metadata call left
    /// in completion, and it happens at most once per (directory,
    /// name) per session.
    pub fn exec_verdict(&self, dir: &Path, name: &str) -> bool {
        let Ok(mut cache) = self.shared.lock() else {
            return entry_is_executable(dir, name);
        };
        if cache.session.as_ref() != Some(&self.session) {
            // Asked outside the listing session: answer uncached
            // rather than polluting another session's memo.
            return entry_is_executable(dir, name);
        }
        let key = (dir.to_path_buf(), name.to_string());
        if let Some(verdict) = cache.exec_verdicts.get(&key) {
            return *verdict;
        }
        #[cfg(unix)]
        {
            cache.metadata_calls += 1;
        }
        let verdict = entry_is_executable(dir, name);
        cache.exec_verdicts.insert(key, verdict);
        verdict
    }

    /// (enumerations, per-entry metadata calls) so far — the
    /// structural evidence the niubash#747 pins assert on.
    pub fn stats(&self) -> (usize, usize) {
        match self.shared.lock() {
            Ok(cache) => (cache.enumerations, cache.metadata_calls),
            Err(_) => (0, 0),
        }
    }
}

/// Whether the entry at `dir`/`name` is executable, by the same
/// platform rule command resolution uses (niubash#671/#743): on Unix,
/// any execute bit (one stat, symlinks followed); on Windows, a
/// PATHEXT suffix — a pure name check.
pub(crate) fn entry_is_executable(dir: &Path, name: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(dir.join(name))
            .map(|md| md.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        let _ = dir;
        is_pathext_executable(name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (dir, name);
        false
    }
}

/// niubash#747: the virtual filesystem roots. Entries under them are
/// served name + dirent type only — no per-entry metadata, no
/// per-entry style resolution — because their metadata is synthetic
/// and expensive to materialize (on Windows /proc and /dev never
/// reach the walk at all: the simulated-namespace tier answers from
/// the engine registry; on Unix these are the real procfs/sysfs/dev
/// trees the walk would otherwise stat entry by entry).
fn is_virtual_tree(word: &str, base_dir: &Path) -> bool {
    if let Some(dir) = logical_dir_of(word) {
        if is_virtual_root_spelling(&dir) {
            return true;
        }
    }
    is_virtual_host_path(base_dir)
}

fn is_virtual_root_spelling(dir: &str) -> bool {
    matches!(dir, "/proc" | "/sys" | "/dev")
        || dir.starts_with("/proc/")
        || dir.starts_with("/sys/")
        || dir.starts_with("/dev/")
}

#[cfg(unix)]
fn is_virtual_host_path(path: &Path) -> bool {
    ["/proc", "/sys", "/dev"]
        .iter()
        .any(|root| path.starts_with(root))
}

#[cfg(not(unix))]
fn is_virtual_host_path(_path: &Path) -> bool {
    false
}

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
        let query = PathQuery::from_word(&context.current_dir, word, quote, &context.tmp_base);

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
            return Ok(Some(candidates_result(candidates)));
        }

        let base_dir = Self::normalize_path(&query.base_dir);

        // Check if base directory exists — one metadata call on the
        // directory itself replaces the old exists()+is_dir() pair.
        match fs::metadata(&base_dir) {
            Ok(md) if md.is_dir() => {}
            _ => return Ok(None),
        }

        let virtual_tree = is_virtual_tree(word, &base_dir);
        Self::complete_directory(&base_dir, &query, directories_only, context, virtual_tree)
    }

    /// Complete entries in a directory, from the session's cached
    /// stat-free listing (niubash#747).
    fn complete_directory(
        base_dir: &Path,
        query: &PathQuery,
        directories_only: bool,
        context: &CompletionContext,
        virtual_tree: bool,
    ) -> Result<Option<CompletionResult>> {
        let listing = match &context.dir_cache {
            Some(handle) => handle.listing(base_dir),
            None => uncached_listing(base_dir),
        };
        let Some(listing) = listing else {
            return Ok(None);
        };
        let behavior = context.behavior;
        let file_colors = context.file_colors.as_ref();

        let mut completions: Vec<PathCandidate> = Vec::new();

        for entry in listing.entries() {
            // The old walk skipped entries whose dirent type could
            // not be read; the listing records them as Unknown.
            if entry.kind == ListedKind::Unknown {
                continue;
            }
            if !name_matches(&query.prefix, &entry.name, behavior) {
                continue;
            }
            let is_dir = entry.kind == ListedKind::Directory;
            if directories_only && !is_dir {
                continue;
            }
            if entry.name.starts_with('.') && !query.prefix.starts_with('.') {
                continue;
            }

            // niubash#747: styling from the dirent kind alone —
            // directory, symlink and suffix/`fi` colors never needed
            // a stat. Virtual trees skip style resolution entirely.
            let style = if virtual_tree {
                None
            } else {
                file_colors
                    .and_then(|colors| colors.style_for(entry.kind.candidate_kind(), &entry.name))
            };
            completions.push(PathCandidate::new(
                entry.kind,
                &query.display_prefix,
                &entry.name,
                query.quote,
                style,
            ));
        }

        if completions.is_empty() {
            return Ok(None);
        }
        completions.sort();

        // niubash#747: the exec-bit refinement — the only per-entry
        // stat left — runs after the sort, over the head of the
        // list, and only when a color table is in play.
        if !virtual_tree && file_colors.is_some() {
            refine_exec_styles(&mut completions, base_dir, context);
        }
        Ok(Some(candidates_result(completions)))
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
    fn from_word(current_dir: &Path, word: &str, quote: Option<char>, tmp_base: &Path) -> Self {
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
                resolve_base_dir(current_dir, "/", tmp_base)
            } else {
                resolve_base_dir(current_dir, dir_part, tmp_base)
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

/// niubash#747: deferred exec-bit refinement. At most the first
/// [`EXEC_REFINE_LIMIT`] regular-file candidates (in display order)
/// get their kind upgraded from the dirent `File` to `Executable`
/// when the entry actually carries an exec bit — the only verdict
/// that needs metadata. Beyond the limit the candidate keeps its
/// suffix/`fi` style: in a directory that large the unrefined tail
/// is never on the grid's first screen, and the per-entry stat it
/// would cost is exactly what this issue removes. Verdicts flow
/// through the session cache (memoized, counted); contexts without
/// one compute them directly.
fn refine_exec_styles(
    completions: &mut [PathCandidate],
    base_dir: &Path,
    context: &CompletionContext,
) {
    let Some(colors) = context.file_colors.as_ref() else {
        return;
    };
    let mut refined = 0usize;
    for candidate in completions.iter_mut() {
        if refined >= EXEC_REFINE_LIMIT {
            break;
        }
        if candidate.kind != ListedKind::Other {
            continue;
        }
        refined += 1;
        let is_exec = match &context.dir_cache {
            Some(handle) => handle.exec_verdict(base_dir, &candidate.name),
            None => entry_is_executable(base_dir, &candidate.name),
        };
        if is_exec {
            candidate.style = colors.style_for(CandidateKind::Executable, &candidate.name);
        }
    }
}

#[derive(Debug)]
struct PathCandidate {
    is_dir: bool,
    /// The dirent kind the candidate was enumerated with
    /// (niubash#747); not part of ordering.
    kind: ListedKind,
    /// The raw entry name (before prefix/escaping); not part of
    /// ordering. The exec refinement stats by this name.
    name: String,
    sort_key: String,
    completion: String,
    /// The LS_COLORS style this candidate paints with (niubash#743);
    /// None when no table is in play or it has no rule for the kind.
    style: Option<nu_ansi_term::Style>,
}

impl PathCandidate {
    fn new(
        kind: ListedKind,
        display_prefix: &str,
        file_name: &str,
        quote: Option<char>,
        style: Option<nu_ansi_term::Style>,
    ) -> Self {
        let is_dir = kind == ListedKind::Directory;
        let mut path = format!("{}{}", display_prefix, file_name);
        if is_dir {
            path.push('/');
        }
        let completion = format_shell_path(&path, quote);
        Self {
            is_dir,
            kind,
            name: file_name.to_string(),
            sort_key: file_name.to_lowercase(),
            completion,
            style,
        }
    }
}

/// Fold a sorted candidate list into the completion result, carrying
/// each candidate's display style (niubash#743).
fn candidates_result(candidates: Vec<PathCandidate>) -> CompletionResult {
    CompletionResult::with_styles(
        candidates
            .iter()
            .map(|candidate| candidate.completion.clone())
            .collect(),
        candidates
            .into_iter()
            .map(|candidate| candidate.style)
            .collect(),
    )
}

/// Windows executable-name check: the extension appears in PATHEXT —
/// the same list the command completer's command-slot gate uses (both
/// go through `entry_is_executable` since niubash#747).
#[cfg(windows)]
fn is_pathext_executable(file_name: &str) -> bool {
    let Some(ext) = file_name
        .rsplit('.')
        .next()
        .filter(|_| file_name.contains('.'))
    else {
        return false;
    };
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    pathext
        .split(';')
        .filter(|item| !item.is_empty())
        .any(|item| item.trim_start_matches('.').eq_ignore_ascii_case(ext))
}

impl PartialEq for PathCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for PathCandidate {}

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

fn resolve_base_dir(current_dir: &Path, dir_part: &str, tmp_base: &Path) -> PathBuf {
    if let Some(path) = resolve_logical_shell_path(dir_part, tmp_base) {
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

/// Undo [`format_shell_path`], recovering the raw filename from a
/// suggestion. `complete-into-braces` needs the raw names: GNU's
/// `bracecomp.c` disables readline's filename quoting and applies its own
/// `sh_backslash_quote` to the unquoted match list.
pub(crate) fn unescape_shell_path(value: &str) -> String {
    if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return inner.replace("'\\''", "'");
    }
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return unescape_backslashes(inner);
    }
    unescape_backslashes(value)
}

fn unescape_backslashes(value: &str) -> String {
    let mut unescaped = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                unescaped.push(next);
            } else {
                unescaped.push(ch);
            }
        } else {
            unescaped.push(ch);
        }
    }
    unescaped
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
                if is_dir {
                    ListedKind::Directory
                } else {
                    ListedKind::Other
                },
                &query.display_prefix,
                name,
                query.quote,
                kind_style(context.file_colors.as_ref(), is_dir, name),
            ));
        }
        return Some(candidates);
    }
    None
}

/// The style for a candidate whose kind is known only as "directory
/// or not" — the simulated-namespace tiers never stat a real entry.
fn kind_style(
    file_colors: Option<&FileColors>,
    is_dir: bool,
    name: &str,
) -> Option<nu_ansi_term::Style> {
    file_colors.and_then(|colors| {
        colors.style_for(
            if is_dir {
                CandidateKind::Directory
            } else {
                CandidateKind::File
            },
            name,
        )
    })
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
                    if *is_dir {
                        ListedKind::Directory
                    } else {
                        ListedKind::Other
                    },
                    &query.display_prefix,
                    name,
                    query.quote,
                    kind_style(context.file_colors.as_ref(), *is_dir, name),
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
                            ListedKind::Other,
                            &query.display_prefix,
                            &name,
                            query.quote,
                            kind_style(context.file_colors.as_ref(), false, &name),
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

    struct EnvGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    fn query(word: &str) -> PathQuery {
        PathQuery::from_word(Path::new("/"), word, None, &std::env::temp_dir())
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

    /// niubash#627 G2: a `/tmp/` completion walks the directory the
    /// context's session tmp base names (the engine-resolved TMPDIR
    /// backing the shell synced in) — the walk steers by that base,
    /// not by the process temp dir.
    #[test]
    fn issue627_g2_completion_walks_the_session_tmp_base() {
        let _env_lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scratch = std::env::temp_dir().join(format!(
            "niubash-g2-completion-{}-{}",
            std::process::id(),
            nanos
        ));
        let root = scratch.join("root");
        let session_base = scratch.join("session-temp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&session_base).unwrap();
        std::fs::write(session_base.join("g2mark-session.txt"), "x").unwrap();
        std::fs::write(scratch.join("g2mark-decoy.txt"), "x").unwrap();
        let _root = EnvGuard::set("NIU_ROOT", &root);

        let mut context = CompletionContext::new(
            PathBuf::from("/"),
            "cat /tmp/g2mark".to_string(),
            "cat /tmp/g2mark".len(),
        );
        context.tmp_base = session_base.clone();
        let values = PathCompleter::complete(&context)
            .unwrap()
            .map(|result| result.completions)
            .unwrap_or_default();
        assert_eq!(values, vec!["/tmp/g2mark-session.txt".to_string()]);

        // Point the base at the scratch dir instead (the stateless
        // shape) and the same word finds the decoy there: the base
        // steers the walk, the spelling does not.
        let mut fallback = CompletionContext::new(
            PathBuf::from("/"),
            "cat /tmp/g2mark".to_string(),
            "cat /tmp/g2mark".len(),
        );
        fallback.tmp_base = scratch.clone();
        let values = PathCompleter::complete(&fallback)
            .unwrap()
            .map(|result| result.completions)
            .unwrap_or_default();
        assert_eq!(values, vec!["/tmp/g2mark-decoy.txt".to_string()]);

        let _ = std::fs::remove_dir_all(scratch);
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
        // The device spellings the registry promoted to first-class
        // classes (rubash#1898: ZeroDevice/FullDevice, listed by
        // special_dev_entries in the same change) are listed too —
        // niubash#688 re-derived this pin from the registry after
        // the promotion landed via rubash#2015.
        assert!(got.contains("/dev/zero"));
        assert!(got.contains("/dev/full"));
        // Spellings the registry does not simulate stay unlisted:
        // /dev/tty is deliberately unclassified (Filesystem) today.
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

    #[cfg(windows)]
    #[test]
    fn issue758_proc_pid_tier_lists_root_link() {
        // niubash#758: /proc/<pid>/root joined the registry's link
        // table (and with it the per-pid enumeration); the completion
        // tier reads that same enumeration face, so the entry is
        // offered beside its sibling links — a face that opens but
        // does not complete would be a half-face.
        let q = query("/proc/self/");
        let context = ctx(&[0, 1, 2]);
        let candidates = simulated_namespace_candidates(&q, "/proc/self/", &context, false)
            .expect("/proc/self is simulated on Windows");
        let values = values(candidates);
        assert!(values.contains(&"/proc/self/root".to_string()), "{values:?}");
        assert!(values.contains(&"/proc/self/cwd".to_string()), "{values:?}");
        assert!(values.contains(&"/proc/self/exe".to_string()), "{values:?}");
    }
}

#[cfg(test)]
mod issue747_tests {
    use super::*;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn set_exec(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let _ = path;
        }
    }

    /// A context completing `input` in `dir`, carrying the given
    /// session-cache handle and (optionally) the default color table.
    fn ctx(
        dir: &Path,
        input: &str,
        handle: Option<DirCacheHandle>,
        colors: bool,
    ) -> CompletionContext {
        let mut context = CompletionContext::new(dir.to_path_buf(), input.to_string(), input.len());
        context.dir_cache = handle;
        if colors {
            context.file_colors = Some(FileColors::defaults());
        }
        context
    }

    fn handle_for(dir: &Path, stem: &str) -> DirCacheHandle {
        DirCacheHandle::new(SessionKey::new(dir.to_path_buf(), stem.to_string()))
    }

    /// (d) Session reuse: two queries in one session (same directory,
    /// same line stem, different typed prefixes) enumerate once; a
    /// different session key re-enumerates; a subdirectory drilled
    /// into within the session gets its own listing without retiring
    /// the first.
    #[test]
    fn issue747_session_requeries_enumerate_once() {
        let dir = unique_temp_dir("niubash-issue747-session");
        std::fs::write(dir.join("alpha.txt"), "x").unwrap();
        std::fs::write(dir.join("alpine.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("nested.txt"), "x").unwrap();

        let shared = Arc::new(Mutex::new(DirListingCache::default()));
        let key = SessionKey::new(dir.clone(), "ls ".to_string());
        let handle = DirCacheHandle::with_shared(shared.clone(), key);

        let first = PathCompleter::complete(&ctx(&dir, "ls a", Some(handle.clone()), false))
            .unwrap()
            .expect("candidates");
        assert!(first.completions.iter().any(|c| c == "alpha.txt"));
        let second = PathCompleter::complete(&ctx(&dir, "ls al", Some(handle.clone()), false))
            .unwrap()
            .expect("candidates");
        assert!(second.completions.iter().any(|c| c == "alpine.txt"));
        assert_eq!(
            handle.stats().0,
            1,
            "two queries in one session must enumerate the directory once"
        );

        // Drilling into a subdirectory enumerates THAT directory but
        // keeps the first listing for the rest of the session.
        let drilled = PathCompleter::complete(&ctx(&dir, "ls sub/", Some(handle.clone()), false))
            .unwrap()
            .expect("candidates");
        assert!(drilled.completions.iter().any(|c| c == "sub/nested.txt"));
        assert_eq!(handle.stats().0, 2);
        let _ = PathCompleter::complete(&ctx(&dir, "ls a", Some(handle.clone()), false)).unwrap();
        assert_eq!(
            handle.stats().0,
            2,
            "the first listing survives a drill-down within the session"
        );

        // A new session key retires the cache.
        let other =
            DirCacheHandle::with_shared(shared, SessionKey::new(dir.clone(), "cat ".to_string()));
        let _ = PathCompleter::complete(&ctx(&dir, "cat a", Some(other.clone()), false)).unwrap();
        assert_eq!(other.stats().0, 3, "a new session re-enumerates");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// (b) Structural bound: refining exec styles over a 300-file
    /// directory costs at most EXEC_REFINE_LIMIT per-entry metadata
    /// calls — never one per entry — and the verdicts are memoized
    /// for the rest of the session.
    #[test]
    fn issue747_metadata_calls_bounded_by_refine_limit() {
        let dir = unique_temp_dir("niubash-issue747-bounded");
        for i in 0..300 {
            let path = dir.join(format!("f{i:03}"));
            std::fs::write(&path, "x").unwrap();
            if i % 2 == 0 {
                set_exec(&path);
            }
        }
        let handle = handle_for(&dir, "ls ");
        let result = PathCompleter::complete(&ctx(&dir, "ls ", Some(handle.clone()), true))
            .unwrap()
            .expect("candidates");
        assert_eq!(result.completions.len(), 300);
        let (_, metadata_calls) = handle.stats();
        assert!(
            metadata_calls <= EXEC_REFINE_LIMIT,
            "refinement must be capped at {EXEC_REFINE_LIMIT} stats, got {metadata_calls} for 300 entries"
        );
        #[cfg(unix)]
        assert!(
            metadata_calls > 0,
            "unix refinement must actually run inside the window"
        );

        // Same session, same query: verdicts are memoized, no new stats.
        let _ = PathCompleter::complete(&ctx(&dir, "ls ", Some(handle.clone()), true)).unwrap();
        assert_eq!(handle.stats().1, metadata_calls);

        let _ = std::fs::remove_dir_all(dir);
    }

    /// #743 behavior preserved inside the refinement window: an
    /// executable regular file paints with the `ex` style, a plain
    /// file with its suffix style, a directory with `di` — and the
    /// documented niubash#747 delta: past the refinement window an
    /// executable keeps its `fi`/suffix style instead of `ex`.
    #[test]
    fn issue747_exec_style_refined_at_head_unrefined_past_limit() {
        let colors = FileColors::defaults();
        let di = colors.style_for(CandidateKind::Directory, "x");

        let dir = unique_temp_dir("niubash-issue747-style");
        std::fs::create_dir_all(dir.join("adir")).unwrap();
        // Sorts first among the regular files: inside the window.
        let runme = dir.join("aarun");
        std::fs::write(&runme, "x").unwrap();
        set_exec(&runme);
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        // 200 plain files push the last-named executable past the
        // refinement window (candidates sort dirs first, then names).
        for i in 0..200 {
            std::fs::write(dir.join(format!("m{i:03}")), "x").unwrap();
        }
        let late = dir.join("zzexec");
        std::fs::write(&late, "x").unwrap();
        set_exec(&late);

        let handle = handle_for(&dir, "ls ");
        let result = PathCompleter::complete(&ctx(&dir, "ls ", Some(handle), true))
            .unwrap()
            .expect("candidates");
        let style_of = |name: &str| {
            let idx = result
                .completions
                .iter()
                .position(|c| c == name)
                .unwrap_or_else(|| panic!("missing {name} in {:?}", result.completions));
            result.styles[idx]
        };
        assert_eq!(style_of("adir/"), di);
        #[cfg(unix)]
        {
            let ex = colors.style_for(CandidateKind::Executable, "x");
            assert_eq!(style_of("aarun"), ex, "head executable keeps the ex style");
            assert_ne!(
                style_of("zzexec"),
                ex,
                "past the refinement window the ex upgrade is not paid for"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// (c) Virtual trees: spelling recognition, plus — on Unix, where
    /// /proc is a real tree the walk serves — candidates come back
    /// with dirent typing (dirs carry their slash), NO styles, and
    /// zero per-entry metadata calls.
    #[test]
    fn issue747_virtual_tree_spellings() {
        assert!(is_virtual_root_spelling("/proc"));
        assert!(is_virtual_root_spelling("/proc/1"));
        assert!(is_virtual_root_spelling("/sys"));
        assert!(is_virtual_root_spelling("/sys/kernel"));
        assert!(is_virtual_root_spelling("/dev"));
        assert!(is_virtual_root_spelling("/dev/fd"));
        assert!(!is_virtual_root_spelling("/"));
        assert!(!is_virtual_root_spelling("/procx"));
        assert!(!is_virtual_root_spelling("/home/proc"));
        assert!(is_virtual_tree("/proc/", Path::new("/elsewhere")));
        assert!(is_virtual_tree("/proc/1/ta", Path::new("/elsewhere")));
        assert!(!is_virtual_tree("/et", Path::new("/")));
    }

    #[cfg(unix)]
    #[test]
    fn issue747_virtual_proc_walk_is_stat_free_and_unstyled() {
        let proc_dir = Path::new("/proc");
        if !proc_dir.is_dir() {
            return;
        }
        // Word-anchored: completing /proc/ from elsewhere.
        let handle = handle_for(Path::new("/"), "ls ");
        let result = PathCompleter::complete(&ctx(
            Path::new("/"),
            "ls /proc/",
            Some(handle.clone()),
            true,
        ))
        .unwrap()
        .expect("/proc must complete");
        assert!(
            result.completions.len() > 10,
            "expected a real proc listing, got {:?}",
            result.completions
        );
        assert!(
            result.completions.iter().any(|c| c == "/proc/sys/"),
            "dirent typing survives: /proc/sys is a directory candidate"
        );
        assert!(
            result.completions.iter().any(|c| c == "/proc/self"),
            "dirent typing survives: /proc/self is a symlink candidate (no slash)"
        );
        assert!(
            result.styles.iter().all(|s| s.is_none()),
            "virtual trees resolve no per-entry styles"
        );
        assert_eq!(
            handle.stats().1,
            0,
            "virtual trees pay zero per-entry metadata calls"
        );

        // cwd-anchored: the shell is standing in /proc itself.
        let handle = handle_for(proc_dir, "ls ");
        let result = PathCompleter::complete(&ctx(proc_dir, "ls ", Some(handle.clone()), true))
            .unwrap()
            .expect("/proc must complete from inside");
        assert!(result.styles.iter().all(|s| s.is_none()));
        assert_eq!(handle.stats().1, 0);
    }

    /// Directory-mtime invalidation: a listing whose directory
    /// changed is re-enumerated even inside one session.
    #[test]
    fn issue747_listing_invalidated_when_directory_changes() {
        let dir = unique_temp_dir("niubash-issue747-invalidate");
        std::fs::write(dir.join("one.txt"), "x").unwrap();
        let handle = handle_for(&dir, "ls ");
        let _ = PathCompleter::complete(&ctx(&dir, "ls ", Some(handle.clone()), false)).unwrap();
        assert_eq!(handle.stats().0, 1);

        // Force a distinct directory mtime (some filesystems tick
        // coarsely) so the cached listing's validation token moves.
        force_new_dir_mtime(&dir);
        std::fs::write(dir.join("two.txt"), "x").unwrap();

        let result = PathCompleter::complete(&ctx(&dir, "ls ", Some(handle.clone()), false))
            .unwrap()
            .expect("candidates");
        assert_eq!(
            handle.stats().0,
            2,
            "a changed directory must be re-enumerated within the session"
        );
        assert!(result.completions.iter().any(|c| c == "two.txt"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Move a directory's mtime using only std: creating and
    /// removing an entry changes it on most filesystems, but coarse
    /// ticks may not move it — churn with short sleeps until the
    /// mtime actually differs (bounded).
    fn force_new_dir_mtime(dir: &Path) {
        let before = fs::metadata(dir).and_then(|md| md.modified()).ok();
        for i in 0..50 {
            let probe = dir.join(format!(".mtime-probe-{i}"));
            std::fs::write(&probe, "x").unwrap();
            let _ = std::fs::remove_file(&probe);
            std::thread::sleep(std::time::Duration::from_millis(20));
            let now = fs::metadata(dir).and_then(|md| md.modified()).ok();
            if now != before {
                return;
            }
        }
    }
}
