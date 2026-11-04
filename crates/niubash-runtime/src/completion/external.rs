// External completion plugin for WinSH
//
// This module provides two completion plugins:
//
//  1. CommandCompletionPlugin  – replaces the old hard-wired CommandCompleter,
//     completing built-in and PATH command names at the command position.
//
//  2. ExternalCompletionPlugin – loads per-command completion definitions from
//     TOML files in a user-configured directory and provides flag / argument
//     completion for those commands.
//
// # Cache strategy (lazy-init + mtime invalidation)
//
//   When a flag's values come from a sub-process (`values_from_command`), the
//   result is written to a cache file under the cache directory.  On the next
//   Tab press the disk cache is read first.  Whenever the tool binary's mtime
//   changes (e.g. after an upgrade) the cache is automatically regenerated.
//
// # When to use `values_from_command` vs `values`
//
//   Use `values` (static) for flags whose value domain is fixed and small:
//
//     [[flags]]
//     long = "--type"
//     short = "-t"
//     takes_value = true
//     values = ["f", "d", "l", "e", "x", "b", "c", "s", "p"]
//
//   Use `values_from_command` only when the tool itself can emit candidates,
//   e.g. tools built with clap that support a `--generate-completions` or a
//   dedicated listing sub-command:
//
//     [[flags]]
//     long = "--profile"
//     takes_value = true
//     [flags.values_from_command]
//     cmd = "mytool"
//     args = ["list-profiles", "--quiet"]
//     cache_ttl_secs = 300    # optional TTL; omit to rely solely on mtime

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::completion::command::CommandCompleter;
use crate::completion::{CompletionContext, CompletionPlugin, CompletionResult};
use crate::path_utils::shell_home_dir;

// ─────────────────────────────────────────────────────────────────────────────
// CommandCompletionPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Completion plugin that completes built-in and PATH command names.
/// Replaces the old hard-wired `CommandCompleter` call in `completer.rs`.
pub struct CommandCompletionPlugin;

impl CompletionPlugin for CommandCompletionPlugin {
    fn name(&self) -> &str {
        "command-completion"
    }

    fn complete(&self, context: &CompletionContext) -> Option<CompletionResult> {
        CommandCompleter::complete(context).ok().flatten()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TOML definition structures
// ─────────────────────────────────────────────────────────────────────────────

/// How the candidate values for a flag are obtained.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ValuesSource {
    /// Static list baked into the definition file.
    Static { values: Vec<String> },
    /// Run a sub-process; each stdout line becomes a candidate.
    Dynamic { values_from_command: DynamicCommand },
    /// Delegate to the built-in path completer.
    Path { values_from: PathLiteral },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicCommand {
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// How many seconds the cached result stays valid (default: use mtime check)
    #[serde(default)]
    pub cache_ttl_secs: Option<u64>,
}

/// Marker type that only deserialises from the string `"path"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PathLiteral;

impl From<PathLiteral> for String {
    fn from(_: PathLiteral) -> String {
        "path".to_string()
    }
}

impl TryFrom<String> for PathLiteral {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s == "path" {
            Ok(PathLiteral)
        } else {
            Err(format!("expected \"path\", got {:?}", s))
        }
    }
}

/// A single flag / option definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagDef {
    #[serde(default)]
    pub short: Option<String>,
    #[serde(default)]
    pub long: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Does this flag consume the next token as its value?
    #[serde(default)]
    pub takes_value: bool,
    /// Source for the flag's values (if `takes_value = true`)
    #[serde(flatten)]
    pub values_source: Option<ValuesSource>,
}

/// A sub-command definition (e.g. `git commit`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubcommandDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub flags: Vec<FlagDef>,
}

/// Top-level definition for one external command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDef {
    pub command: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub flags: Vec<FlagDef>,
    #[serde(default)]
    pub subcommands: Vec<SubcommandDef>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Disk cache file format
// ─────────────────────────────────────────────────────────────────────────────

/// On-disk representation of a single cached dynamic query.
/// File path: `<cache_dir>/<hashed_cache_key>.toml` (see [`CacheKey`]).
#[derive(Debug, Serialize, Deserialize)]
struct DiskCache {
    /// The sub-process command + args that produced these values (for reference).
    source_cmd: String,
    /// niubash#845: the full [`CacheKey`] identity that produced this
    /// entry. A read whose identity differs is a miss, never a serve:
    /// before this field existed the file name was the only key, and
    /// it omitted the args, so colliding specs shared one entry.
    /// Entries written before #845 carry the empty string and are
    /// regenerated once.
    #[serde(default)]
    cache_identity: String,
    /// niubash#845: the resolved tool path that produced this entry.
    /// A different resolved path (the session PATH moved, a shadowing
    /// install appeared) is a different tool identity — stale, even
    /// when the mtimes happen to agree.
    #[serde(default)]
    tool_path: Option<String>,
    /// Seconds since Unix epoch when this file was written.
    written_secs: u64,
    /// Optional TTL in seconds; 0 means "use mtime only".
    #[serde(default)]
    ttl_secs: u64,
    /// Unix epoch seconds of the tool binary's mtime at write time.
    /// `None` when the binary could not be located.
    #[serde(default)]
    tool_mtime_secs: Option<u64>,
    /// The candidate values.
    values: Vec<String>,
}

impl DiskCache {
    /// Returns true when the on-disk entry is stale and should be regenerated.
    fn is_stale(&self, tool_path: Option<&Path>) -> bool {
        // niubash#845: tool identity first — the entry answers for
        // exactly the resolved path recorded at write time.
        if self.tool_path.as_deref()
            != tool_path
                .map(|path| path.to_string_lossy().into_owned())
                .as_deref()
        {
            return true;
        }
        // TTL check (only when ttl_secs > 0)
        if self.ttl_secs > 0 {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if now.saturating_sub(self.written_secs) > self.ttl_secs {
                return true;
            }
        }
        // mtime check
        if let Some(tool_path) = tool_path {
            if let Ok(meta) = std::fs::metadata(tool_path) {
                if let Ok(mtime) = meta.modified() {
                    let mtime_secs = mtime
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    return self.tool_mtime_secs != Some(mtime_secs);
                }
            }
        }
        false
    }
}

/// niubash#845: the ONE key every dynamic-completion cache layer
/// (memory, disk file name, disk entry validation) keys on. Before
/// this type existed the key was `format!("{command}::{cmd}")` — it
/// omitted the args (two `takes_value` flags sharing a tool with
/// different args collided for the whole session), the TTL, and the
/// tool identity (a ttl-less entry survived binary upgrades forever
/// whenever `which_tool` missed). Every input that can change the
/// answer is in this struct; nothing else is consulted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    /// The command whose definition declared the dynamic source.
    owner_command: String,
    /// The tool to run.
    cmd: String,
    /// The tool's args, verbatim and in order.
    args: Vec<String>,
    /// The declared TTL (`None` = mtime-only freshness).
    ttl_secs: Option<u64>,
    /// The resolved tool path under the session PATH (identity, not
    /// just fingerprint: the same name resolved elsewhere is a
    /// different tool).
    tool_path: Option<PathBuf>,
}

impl CacheKey {
    fn new(owner_command: &str, spec: &DynamicCommand, tool_path: Option<PathBuf>) -> Self {
        Self {
            owner_command: owner_command.to_string(),
            cmd: spec.cmd.clone(),
            args: spec.args.clone(),
            ttl_secs: spec.cache_ttl_secs,
            tool_path,
        }
    }

    /// Canonical identity string: the mem-cache key and the value
    /// stored in (and required of) every disk entry.
    fn identity(&self) -> String {
        format!(
            "{}::{}::{}::{}::{}",
            self.owner_command,
            self.cmd,
            self.args.join("\u{1f}"),
            self.ttl_secs
                .map(|ttl| ttl.to_string())
                .unwrap_or_else(|| "-".to_string()),
            self.tool_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
    }

    /// Disk file name for this key: a hash of the identity, so two
    /// identities can never collide through filename sanitisation.
    fn filename(&self) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.identity().hash(&mut hasher);
        format!("dyn-{:016x}.toml", hasher.finish())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// In-memory cache (warm after first hit per session)
// ─────────────────────────────────────────────────────────────────────────────

struct MemEntry {
    values: Vec<String>,
    /// mtime secs of tool binary when loaded (mirrors DiskCache)
    tool_mtime_secs: Option<u64>,
    /// niubash#845: absolute expiry (epoch secs) from the declared
    /// TTL, `None` for mtime-only entries. Before this field existed
    /// the mem cache ignored `cache_ttl_secs` entirely — the disk
    /// layer honored it, the warm layer served the values for the
    /// whole session.
    expires_at_secs: Option<u64>,
}

impl MemEntry {
    /// Fresh at `now_secs` against the tool's current mtime: the TTL
    /// must not have run out AND the tool must not have changed.
    fn is_fresh(&self, now_secs: u64, current_tool_mtime_secs: Option<u64>) -> bool {
        if let Some(expires_at) = self.expires_at_secs {
            if now_secs >= expires_at {
                return false;
            }
        }
        match (current_tool_mtime_secs, self.tool_mtime_secs) {
            (Some(current), Some(cached)) => current == cached,
            _ => true,
        }
    }
}

/// Resolve the directory used to store dynamic completion caches.
///
/// Priority:
///   1. `NIU_COMPLETION_CACHE_DIR` environment variable
///   2. `~/.niubash/completions/cache`
///
/// niubash#841: the pre-rename location is never written anymore;
/// it stays readable through [`legacy_cache_dir`] so caches written
/// by older builds keep working.
fn resolve_cache_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("NIU_COMPLETION_CACHE_DIR") {
        let p = PathBuf::from(dir);
        if !p.exists() {
            let _ = std::fs::create_dir_all(&p);
        }
        return Some(p);
    }
    let base = shell_home_dir()?
        .join(".niubash")
        .join("completions")
        .join("cache");
    if !base.exists() {
        let _ = std::fs::create_dir_all(&base);
    }
    Some(base)
}

/// The pre-rename cache directory (niubash#841): read-only fallback
/// for entries the primary directory does not have. Consulted on
/// reads, never written to, and never created.
///
/// Priority:
///   1. `WINSH_COMPLETION_CACHE_DIR` environment variable
///   2. `~/.winsh/completions/cache`
fn legacy_cache_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("WINSH_COMPLETION_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    shell_home_dir().map(|home| home.join(".winsh").join("completions").join("cache"))
}

/// niubash#845: the `.parsed.toml` cache file for a bash completion
/// source, keyed on the FULL source path (hashed), not the stem:
/// `_foo.bash` in directory A and directory B used to share one
/// `<stem>.parsed.toml`, so whichever directory parsed first served
/// its definition for both.
fn parsed_cache_file(cache_dir: &Path, bash_path: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let stem = bash_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .trim_start_matches('_');
    let canonical = std::fs::canonicalize(bash_path).unwrap_or_else(|_| bash_path.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.to_string_lossy().hash(&mut hasher);
    cache_dir.join(format!("{}-{:016x}.parsed.toml", stem, hasher.finish()))
}

/// The source-mtime header a parsed cache file carries (a TOML
/// comment, so the file still parses as a bare `CommandDef`).
fn parsed_cache_mtime_header(bash_mtime_secs: u64) -> String {
    format!("# niu-bash-source-mtime: {}", bash_mtime_secs)
}

/// The stored source mtime a parsed cache file records, if any.
fn parsed_cache_stored_mtime(contents: &str) -> Option<u64> {
    contents
        .lines()
        .next()?
        .strip_prefix("# niu-bash-source-mtime: ")?
        .trim()
        .parse()
        .ok()
}

/// niubash#845: a parsed cache is fresh only when the stored source
/// mtime EQUALS the source's current mtime. The pre-fix comparison
/// was `cache_file_mtime >= bash_mtime` — a source re-materialized
/// with an OLDER mtime (git checkout of an older revision, `cp -p`)
/// kept the stale parse trusted indefinitely, because the cache file
/// (written later) always compared newer.
fn parsed_cache_fresh(stored_mtime: Option<u64>, bash_mtime: Option<u64>) -> bool {
    matches!((stored_mtime, bash_mtime), (Some(stored), Some(current)) if stored == current)
}

// ─────────────────────────────────────────────────────────────────────────────
// ExternalCompletionPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Completion plugin that reads per-command completion definitions from a
/// directory of TOML files and provides flag/argument completion for them.
pub struct ExternalCompletionPlugin {
    /// command name → definition
    definitions: HashMap<String, CommandDef>,
    /// In-memory warm cache (cache_key → MemEntry)
    mem_cache: Mutex<HashMap<String, MemEntry>>,
    /// Directory where disk cache files are stored.
    /// Resolved once at construction time.
    cache_dir: Option<PathBuf>,
    /// Pre-rename cache directory (niubash#841): read-only fallback,
    /// resolved once at construction time. Never written to.
    legacy_cache_dir: Option<PathBuf>,
}

/// The embedded WinuxCmd completion definitions, parsed once per
/// process. The parse is the one expensive step in constructing an
/// [`ExternalCompletionPlugin`] (niubash#755); [`warm_embedded_definitions`]
/// exists to move it off the interactive startup path.
static EMBEDDED_DEFS: LazyLock<Vec<CommandDef>> =
    LazyLock::new(|| crate::completion::winuxcmd_assets::definitions().collect());

/// Start the one-time embedded-definition parse on a background
/// thread so it overlaps the rest of startup instead of landing
/// inside the first completion-state seeding (niubash#755). The
/// LazyLock remains the single source: a consumer that arrives
/// before the warm-up finishes waits only for the remainder of the
/// same parse. Identical data, identical order, identical merge
/// behavior — only the wall-clock placement changes. A spawn failure
/// is not an error; the lazy path still parses on first use.
pub fn warm_embedded_definitions() {
    let _ = std::thread::Builder::new()
        .name("niu-embedded-defs-warmup".to_string())
        .spawn(|| {
            LazyLock::force(&EMBEDDED_DEFS);
        });
}

impl ExternalCompletionPlugin {
    /// Number of loaded definitions (for diagnostics)
    pub fn definition_count(&self) -> usize {
        self.definitions.len()
    }

    /// Names of all loaded command definitions (for diagnostics)
    pub fn definition_names(&self) -> Vec<&str> {
        self.definitions.keys().map(|s| s.as_str()).collect()
    }

    /// Add already-translated command definitions.
    ///
    /// This is used by injected completion definitions. Callers
    /// control priority by choosing when to invoke it relative to `load_dir()`.
    pub fn load_definitions<I>(&mut self, definitions: I)
    where
        I: IntoIterator<Item = CommandDef>,
    {
        for def in definitions {
            if let Some(existing) = self.definitions.get_mut(&def.command) {
                merge_command_def(existing, def);
            } else {
                self.definitions.insert(def.command.clone(), def);
            }
        }
    }

    /// Replace command definitions wholesale.
    ///
    /// This is used for official bundle assets: a newer bundle definition should
    /// be able to update or remove data from the compiled fallback definition.
    pub fn replace_definitions<I>(&mut self, definitions: I)
    where
        I: IntoIterator<Item = CommandDef>,
    {
        for def in definitions {
            self.definitions.insert(def.command.clone(), def);
        }
    }

    /// Create a plugin seeded with the embedded WinuxCmd applet definitions.
    ///
    /// These defaults are the compiled fallback layer (layer 0): they are
    /// loaded at construction time, before bundle definitions
    /// (`replace_definitions` overwrites per command), translated definitions
    /// (`load_definitions` merges) and user completion dirs (`load_dir`
    /// overwrites per command) — so a user TOML always wins, matching the
    /// documented load order. The WinuxCmd definitions are generated from
    /// `winuxcmd --help` by `scripts/generate-winuxcmd-completions.py` and
    /// embedded from `assets/completions/winuxcmd/` via
    /// [`crate::completion::winuxcmd_assets`]. Non-applet commands (git,
    /// docker, kubectl, npm, …) are served by the bundle layer — the
    /// official oh-my-niu distribution ships their tables — or by user
    /// completion dirs; the hand-curated embedded tables they used to ride
    /// on were removed with the product-TOML deletion (1.5.0).
    pub fn new() -> Self {
        let mut plugin = Self {
            definitions: HashMap::new(),
            mem_cache: Mutex::new(HashMap::new()),
            cache_dir: resolve_cache_dir(),
            legacy_cache_dir: legacy_cache_dir(),
        };
        plugin.load_definitions(EMBEDDED_DEFS.iter().cloned());
        plugin
    }

    /// Load all `*.toml` and `*.bash` definition files from `dir`.
    ///
    /// Load order / priority:
    ///   0. Built-in winuxcmd defaults are loaded at construction time.
    ///   1. User `*.toml` files are loaded first and take **highest priority**.
    ///      A user-written `fd.toml` will suppress auto-import of `fd.bash`.
    ///   2. `*.bash` files whose stem has no matching `.toml` are auto-imported.
    ///      The parsed result is written to the cache dir as
    ///      `<cmd>.parsed.toml` and reused on subsequent starts while the
    ///      source `.bash` file's mtime is unchanged.
    ///
    /// Files that fail to parse are skipped with a warning.
    pub fn load_from_dir(dir: &Path) -> Self {
        let mut plugin = Self::new();
        plugin.load_dir(dir);
        plugin
    }

    /// Load definitions from a single directory into this plugin.
    /// Can be called multiple times for different directories.
    pub fn load_dir(&mut self, dir: &Path) {
        if !dir.is_dir() {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                log::warn!("ExternalCompletionPlugin: cannot read dir {:?}: {}", dir, e);
                return;
            }
        };

        // Collect all entries first so we can check both extensions
        let mut toml_stems: HashSet<String> = HashSet::new();
        let mut bash_paths: Vec<PathBuf> = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            match path.extension().and_then(|e| e.to_str()) {
                Some("toml") => {
                    match self.load_file(&path) {
                        Ok(def) => {
                            log::debug!("ExternalCompletionPlugin: loaded toml {:?}", path);
                            // Track the stem (file name without extension) so we
                            // can suppress bash auto-import for the same command.
                            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                                toml_stems.insert(stem.to_string());
                            }
                            self.definitions.insert(def.command.clone(), def);
                        }
                        Err(e) => {
                            log::warn!(
                                "ExternalCompletionPlugin: failed to load {:?}: {}",
                                path,
                                e
                            );
                        }
                    }
                }
                Some("bash") => {
                    bash_paths.push(path);
                }
                _ => {}
            }
        }

        // Auto-import bash files that have no matching .toml override
        for bash_path in &bash_paths {
            let stem = match bash_path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.trim_start_matches('_').to_string(),
                None => continue,
            };
            // Skip if a .toml with the same stem (or command name) already loaded
            if toml_stems.contains(&stem) || self.definitions.contains_key(&stem) {
                log::debug!(
                    "ExternalCompletionPlugin: skipping {:?} (overridden by .toml)",
                    bash_path
                );
                continue;
            }
            match self.load_bash_with_cache(bash_path) {
                Ok(def) => {
                    log::debug!(
                        "ExternalCompletionPlugin: auto-imported bash {:?} as {:?}",
                        bash_path,
                        def.command
                    );
                    self.definitions.insert(def.command.clone(), def);
                }
                Err(e) => {
                    log::warn!(
                        "ExternalCompletionPlugin: failed to import bash {:?}: {}",
                        bash_path,
                        e
                    );
                }
            }
        }
    }

    /// Parse a bash completion script, using a cached `.parsed.toml` if the
    /// source file has not changed (mtime check).
    fn load_bash_with_cache(&self, bash_path: &Path) -> Result<CommandDef, String> {
        // Obtain source file mtime
        let bash_mtime = std::fs::metadata(bash_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());

        // Try the on-disk parsed cache: keyed on the full source
        // path, fresh only when the source mtime recorded inside the
        // cache EQUALS the source's current mtime (niubash#845).
        if let Some(ref cache_dir) = self.cache_dir {
            let cache_file = parsed_cache_file(cache_dir, bash_path);
            if cache_file.exists() {
                if let Ok(contents) = std::fs::read_to_string(&cache_file) {
                    if parsed_cache_fresh(parsed_cache_stored_mtime(&contents), bash_mtime) {
                        if let Ok(def) = toml::from_str::<CommandDef>(&contents) {
                            log::debug!(
                                "ExternalCompletionPlugin: bash cache hit {:?}",
                                cache_file
                            );
                            return Ok(def);
                        }
                    } else {
                        log::debug!(
                            "ExternalCompletionPlugin: bash cache stale {:?}",
                            cache_file
                        );
                    }
                }
            }
        }

        // Parse the bash script
        let def = crate::completion::bash_import::parse_bash_completion(bash_path)?;

        // Write to cache, recording the source mtime it answers for.
        if let Some(ref cache_dir) = self.cache_dir {
            let cache_file = parsed_cache_file(cache_dir, bash_path);
            match toml::to_string_pretty(&def) {
                Ok(serialised) => {
                    let body = match bash_mtime {
                        Some(secs) => {
                            format!("{}\n{}", parsed_cache_mtime_header(secs), serialised)
                        }
                        None => serialised,
                    };
                    if let Err(e) = std::fs::write(&cache_file, body) {
                        log::warn!(
                            "ExternalCompletionPlugin: failed to write bash cache {:?}: {}",
                            cache_file,
                            e
                        );
                    } else {
                        log::debug!(
                            "ExternalCompletionPlugin: wrote bash cache {:?}",
                            cache_file
                        );
                    }
                }
                Err(e) => {
                    log::warn!("ExternalCompletionPlugin: serialise error: {}", e);
                }
            }
        }

        Ok(def)
    }

    fn load_file(&self, path: &Path) -> Result<CommandDef, String> {
        let content = std::fs::read_to_string(path).map_err(|e| format!("read error: {}", e))?;
        toml::from_str(&content).map_err(|e| format!("parse error: {}", e))
    }

    // ── Completion logic ──────────────────────────────────────────────────────

    fn complete_for_command(
        &self,
        def: &CommandDef,
        context: &CompletionContext,
    ) -> Option<CompletionResult> {
        let word = context.get_current_word().unwrap_or_default();
        let prev = context.get_prev_token();

        // Check if prev token is a known takes_value flag → complete the value
        if let Some(prev_token) = &prev {
            let flags = Self::effective_flags(def, context);
            if let Some(flag) = flags.iter().find(|f| {
                f.takes_value
                    && (f.short.as_deref() == Some(prev_token.as_str())
                        || f.long.as_deref() == Some(prev_token.as_str()))
            }) {
                return self.complete_flag_value(flag, def, context);
            }
        }

        // Otherwise complete flag names when word starts with '-'
        if word.starts_with('-') || context.is_command_position() {
            // Only offer flag completion when we are NOT at command position
            if !context.is_command_position() {
                return self.complete_flag_names(def, context, &word);
            }
        }

        if Self::is_subcommand_position(def, context, prev.as_deref(), &word) {
            return Self::complete_subcommands(def, context, &word);
        }

        None
    }

    fn is_subcommand_position(
        def: &CommandDef,
        context: &CompletionContext,
        prev: Option<&str>,
        word: &str,
    ) -> bool {
        if def.subcommands.is_empty() || context.is_command_position() || word.starts_with('-') {
            return false;
        }
        prev == Some(def.command.as_str())
    }

    fn complete_subcommands(
        def: &CommandDef,
        context: &CompletionContext,
        word: &str,
    ) -> Option<CompletionResult> {
        let mut completions = Vec::new();
        let mut descriptions = Vec::new();
        for subcommand in &def.subcommands {
            if context.behavior.matches(&subcommand.name, word) {
                completions.push(subcommand.name.clone());
                descriptions.push(subcommand.description.clone());
            }
        }
        if completions.is_empty() {
            None
        } else {
            Some(CompletionResult::with_descriptions(
                completions,
                descriptions,
            ))
        }
    }

    /// Return the flags applicable to the current sub-command (if any),
    /// falling back to top-level flags.
    fn effective_flags<'a>(def: &'a CommandDef, context: &CompletionContext) -> Vec<&'a FlagDef> {
        // niubash#851: the sub-command is the second word of the
        // CURRENT segment, resolved by the segment/quote-aware word
        // machinery — the old `split_whitespace` over the whole line
        // served the top-level command's flags for
        // `true; git checkout -<TAB>` (tokens[1] was `git`) and for
        // quoted command words.
        let words = context.current_segment_words();
        if let Some(sub_name) = words.get(1) {
            if let Some(sub) = def.subcommands.iter().find(|s| &s.name == sub_name) {
                return sub.flags.iter().collect();
            }
        }
        def.flags.iter().collect()
    }

    fn complete_flag_names(
        &self,
        def: &CommandDef,
        context: &CompletionContext,
        word: &str,
    ) -> Option<CompletionResult> {
        let flags = Self::effective_flags(def, context);
        let mut completions: Vec<String> = Vec::new();
        let mut descriptions: Vec<Option<String>> = Vec::new();

        for flag in flags {
            if let Some(short) = &flag.short {
                if context.behavior.matches(short, word) {
                    completions.push(short.clone());
                    descriptions.push(flag.description.clone());
                }
            }
            if let Some(long) = &flag.long {
                if context.behavior.matches(long, word) {
                    completions.push(long.clone());
                    descriptions.push(flag.description.clone());
                }
            }
        }

        if completions.is_empty() {
            None
        } else {
            Some(CompletionResult::with_descriptions(
                completions,
                descriptions,
            ))
        }
    }

    fn complete_flag_value(
        &self,
        flag: &FlagDef,
        def: &CommandDef,
        context: &CompletionContext,
    ) -> Option<CompletionResult> {
        match &flag.values_source {
            Some(ValuesSource::Static { values }) => {
                let word = context.get_current_word().unwrap_or_default();
                let matches: Vec<String> = values
                    .iter()
                    .filter(|v| context.behavior.matches(v, &word))
                    .cloned()
                    .collect();
                if matches.is_empty() {
                    None
                } else {
                    Some(CompletionResult::new(matches))
                }
            }
            Some(ValuesSource::Dynamic {
                values_from_command,
            }) => self.resolve_dynamic(&def.command, values_from_command, context),
            Some(ValuesSource::Path { .. }) => {
                // Delegate to PathCompleter by returning None here; the built-in
                // path completer further up the chain will handle it because
                // is_path_completion() will return true for a bare prefix.
                None
            }
            None => None,
        }
    }

    /// Read a fresh disk-cache entry for `cache_key` from `dir`,
    /// warming the in-memory cache on a hit. Returns the cached
    /// values, or None on any miss (no directory, absent file,
    /// unparsable, identity mismatch, stale). The identity check is
    /// niubash#845's: an entry written for a different key (owner,
    /// args, TTL, or tool) is not this call's entry, whichever
    /// directory it is read from — the primary directory and the
    /// niubash#841 legacy fallback are held to the same rule.
    fn read_disk_cache(
        &self,
        dir: Option<&Path>,
        cache_key: &CacheKey,
        mem_key: &str,
        tool_path: Option<&Path>,
    ) -> Option<Vec<String>> {
        let cache_file = dir?.join(cache_key.filename());
        if !cache_file.exists() {
            return None;
        }
        let contents = std::fs::read_to_string(&cache_file).ok()?;
        let disk: DiskCache = toml::from_str(&contents).ok()?;
        if disk.cache_identity != mem_key || disk.is_stale(tool_path) {
            log::debug!(
                "ExternalCompletionPlugin: disk cache stale {:?}",
                cache_file
            );
            return None;
        }
        log::debug!("ExternalCompletionPlugin: disk cache hit {:?}", cache_file);
        // Warm in-memory cache (niubash#833: poison recovery)
        self.mem_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                mem_key.to_string(),
                MemEntry {
                    values: disk.values.clone(),
                    tool_mtime_secs: disk.tool_mtime_secs,
                    expires_at_secs: (disk.ttl_secs > 0).then(|| disk.written_secs + disk.ttl_secs),
                },
            );
        Some(disk.values)
    }

    /// Run (or use cached) dynamic sub-process to obtain candidate values.
    ///
    /// Lookup order:
    ///   1. In-memory cache (same session, already verified)
    ///   2. Disk cache file (survives restarts; validated via mtime / TTL)
    ///      2b. Pre-rename legacy disk cache, read-only (niubash#841)
    ///   3. Sub-process execution → write disk cache → populate memory cache
    fn resolve_dynamic(
        &self,
        owner_command: &str,
        spec: &DynamicCommand,
        context: &CompletionContext,
    ) -> Option<CompletionResult> {
        let tool_path = which_tool(
            &spec.cmd,
            context.session_path.as_deref(),
            context.session_pathext.as_deref(),
        );
        let tool_mtime_secs = tool_path
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        // niubash#845: ONE key over every input (owner, cmd, args,
        // TTL, tool identity) drives the mem cache, the disk file
        // name, and the disk entry validation below.
        let cache_key = CacheKey::new(owner_command, spec, tool_path.clone());
        let mem_key = cache_key.identity();
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // ── 1. In-memory cache ────────────────────────────────────────────────
        // niubash#833: recover a poisoned mem-cache lock instead of
        // treating every lookup as a miss (which respawned the
        // values_from_command subprocess on every TAB).
        {
            let mem = self.mem_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = mem.get(&mem_key) {
                if entry.is_fresh(now_secs, tool_mtime_secs) {
                    return self.filter_values(&entry.values, context);
                }
            }
        }

        // ── 2. Disk cache ─────────────────────────────────────────────────────
        if let Some(values) = self.read_disk_cache(
            self.cache_dir.as_deref(),
            &cache_key,
            &mem_key,
            tool_path.as_deref(),
        ) {
            return self.filter_values(&values, context);
        }
        // niubash#841: read-fallback to the pre-rename cache
        // directory. Legacy entries are served (and warmed into
        // memory) but the legacy directory is never written again —
        // the next subprocess refresh lands in the primary directory.
        if let Some(values) = self.read_disk_cache(
            self.legacy_cache_dir.as_deref(),
            &cache_key,
            &mem_key,
            tool_path.as_deref(),
        ) {
            return self.filter_values(&values, context);
        }

        // ── 3. Sub-process ────────────────────────────────────────────────────
        log::debug!(
            "ExternalCompletionPlugin: running {:?} {:?}",
            spec.cmd,
            spec.args
        );
        let values = run_dynamic_command(
            spec,
            context.session_path.as_deref(),
            self.cache_dir.as_deref(),
            DYNAMIC_COMMAND_TIMEOUT,
        )?;

        let written_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Write disk cache
        if let Some(ref cache_dir) = self.cache_dir {
            let disk = DiskCache {
                source_cmd: format!("{} {}", spec.cmd, spec.args.join(" ")),
                cache_identity: mem_key.clone(),
                tool_path: tool_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                written_secs,
                ttl_secs: spec.cache_ttl_secs.unwrap_or(0),
                tool_mtime_secs,
                values: values.clone(),
            };
            match toml::to_string_pretty(&disk) {
                Ok(serialised) => {
                    let cache_file = cache_dir.join(cache_key.filename());
                    if let Err(e) = std::fs::write(&cache_file, serialised) {
                        log::warn!(
                            "ExternalCompletionPlugin: failed to write cache {:?}: {}",
                            cache_file,
                            e
                        );
                    } else {
                        log::debug!(
                            "ExternalCompletionPlugin: wrote disk cache {:?}",
                            cache_file
                        );
                    }
                }
                Err(e) => {
                    log::warn!("ExternalCompletionPlugin: serialise error: {}", e);
                }
            }
        }

        // Update in-memory cache (niubash#833: poison recovery)
        self.mem_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                mem_key,
                MemEntry {
                    values: values.clone(),
                    tool_mtime_secs,
                    expires_at_secs: spec
                        .cache_ttl_secs
                        .map(|ttl| written_secs.saturating_add(ttl)),
                },
            );

        self.filter_values(&values, context)
    }

    /// Filter a value list by the current word and configured match behavior.
    fn filter_values(
        &self,
        values: &[String],
        context: &CompletionContext,
    ) -> Option<CompletionResult> {
        let word = context.get_current_word().unwrap_or_default();
        let matches: Vec<String> = values
            .iter()
            .filter(|v| context.behavior.matches(v, &word))
            .cloned()
            .collect();
        if matches.is_empty() {
            None
        } else {
            Some(CompletionResult::new(matches))
        }
    }
}

impl Default for ExternalCompletionPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl CompletionPlugin for ExternalCompletionPlugin {
    fn name(&self) -> &str {
        "external-completion"
    }

    fn complete(&self, context: &CompletionContext) -> Option<CompletionResult> {
        // Identify the command being typed
        let cmd_name = context.get_command_name()?;
        let def = self.definitions.get(&cmd_name)?;
        self.complete_for_command(def, context)
    }

    fn on_directory_changed(&self, _new_dir: &Path) {
        // Path-based caches could be invalidated here in the future
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Utilities
// ─────────────────────────────────────────────────────────────────────────────

/// niubash#839: hard bound for one dynamic completion tool run. The
/// TAB path is interactive: a tool that hangs must not freeze it, so
/// past this bound the child is killed and the completion degrades to
/// no candidates (with the once-per-session warning below).
const DYNAMIC_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

/// niubash#839: failure warnings are once-per-session per shape. A
/// broken tool would otherwise warn on every Tab press; silence after
/// the first warning keeps the failure discoverable (it is in the
/// log) without spamming it, and — the point of the change — a
/// failure is never again indistinguishable from a legitimately
/// empty result in the log.
static DYNAMIC_WARNED_SPAWN: AtomicBool = AtomicBool::new(false);
static DYNAMIC_WARNED_NONZERO: AtomicBool = AtomicBool::new(false);
static DYNAMIC_WARNED_TIMEOUT: AtomicBool = AtomicBool::new(false);

fn warn_dynamic_once(flag: &AtomicBool, message: String) {
    if !flag.swap(true, Ordering::SeqCst) {
        log::warn!("ExternalCompletionPlugin: {}", message);
    }
}

/// Run one dynamic completion tool with a bounded wait, returning its
/// stdout lines on success (niubash#839).
///
/// Failure shapes are DISTINCT in the log and all return `None`
/// (no candidates — the same user-visible degradation as a
/// legitimately empty result, which logs nothing): spawn failure,
/// non-zero exit, and timeout each warn once per session. Stdout
/// goes to a scratch file, not a pipe, so a tool that out-writes the
/// pipe buffer cannot wedge the bounded wait (the same isolation
/// shape as the git prompt's spawn). The session PATH, when the
/// completer snapshot carries one, is handed to the child so the
/// spawn resolves the same tool `which_tool` fingerprinted.
fn run_dynamic_command(
    spec: &DynamicCommand,
    session_path: Option<&str>,
    scratch_dir: Option<&Path>,
    timeout: Duration,
) -> Option<Vec<String>> {
    static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let scratch = scratch_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "niu-dynamic-completion-{}-{}.out",
            std::process::id(),
            seq
        ));
    let sink = match std::fs::File::create(&scratch) {
        Ok(file) => file,
        Err(err) => {
            warn_dynamic_once(
                &DYNAMIC_WARNED_SPAWN,
                format!(
                    "dynamic completion tool '{}' could not start (scratch file): {}",
                    spec.cmd, err
                ),
            );
            return None;
        }
    };
    let mut command = crate::console_guard::suppressed_command(&spec.cmd);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::from(sink));
    if let Some(path) = session_path {
        command.env("PATH", path);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            warn_dynamic_once(
                &DYNAMIC_WARNED_SPAWN,
                format!(
                    "dynamic completion tool '{}' failed to spawn: {}",
                    spec.cmd, err
                ),
            );
            let _ = std::fs::remove_file(&scratch);
            return None;
        }
    };
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break None,
        }
    };
    let Some(status) = status else {
        warn_dynamic_once(
            &DYNAMIC_WARNED_TIMEOUT,
            format!(
                "dynamic completion tool '{}' exceeded the {}s completion bound and was killed",
                spec.cmd,
                timeout.as_secs()
            ),
        );
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&scratch);
        return None;
    };
    if !status.success() {
        warn_dynamic_once(
            &DYNAMIC_WARNED_NONZERO,
            format!(
                "dynamic completion tool '{}' exited with {} (no candidates served)",
                spec.cmd, status
            ),
        );
        let _ = std::fs::remove_file(&scratch);
        return None;
    }
    let stdout = std::fs::read_to_string(&scratch).unwrap_or_default();
    let _ = std::fs::remove_file(&scratch);
    Some(
        stdout
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
    )
}

fn merge_command_def(existing: &mut CommandDef, incoming: CommandDef) {
    if existing.description.is_none() {
        existing.description = incoming.description;
    }

    for flag in incoming.flags {
        if let Some(existing_flag) = existing
            .flags
            .iter_mut()
            .find(|existing_flag| same_flag(existing_flag, &flag))
        {
            merge_flag_def(existing_flag, flag);
        } else {
            existing.flags.push(flag);
        }
    }

    for subcommand in incoming.subcommands {
        if !existing
            .subcommands
            .iter()
            .any(|existing_subcommand| existing_subcommand.name == subcommand.name)
        {
            existing.subcommands.push(subcommand);
        }
    }
}

fn merge_flag_def(existing: &mut FlagDef, incoming: FlagDef) {
    if existing.short.is_none() {
        existing.short = incoming.short;
    }
    if existing.long.is_none() {
        existing.long = incoming.long;
    }
    if existing.description.is_none() {
        existing.description = incoming.description;
    }
    if !existing.takes_value {
        existing.takes_value = incoming.takes_value;
    }
    if existing.values_source.is_none() {
        existing.values_source = incoming.values_source;
    }
}

fn same_flag(left: &FlagDef, right: &FlagDef) -> bool {
    left.long.is_some() && left.long == right.long
        || left.short.is_some() && left.short == right.short
}

/// Locate a tool binary in PATH, returning its full path (for mtime checks).
///
/// niubash#839: the probe runs against the SESSION env PATH the
/// completer snapshots (`session_path`), not the process PATH. A tool
/// that exists only on the session PATH — an interactive
/// `export PATH=...` the process env never saw — was invisible to the
/// mtime fingerprint even though the spawn below resolves it, so its
/// cache entry could never go stale. `None` keeps the stateless
/// fallback: the process PATH, for contexts built outside a session.
/// The candidate suffixes for a Windows tool probe: the bare name
/// first, then the PATHEXT entries (niubash#845). PATHEXT comes from
/// the session env when the completer snapshot carries it, else the
/// process env; the historical `.exe .bat .cmd` list is only the
/// fallback when neither env sets PATHEXT at all. Before #845 the
/// list was hardcoded, so a tool whose extension lived only in a
/// session PATHEXT (`.py`, a custom `.ps1` shim, …) was missed →
/// `tool_path = None` → the mtime leg of staleness was skipped and a
/// ttl-less disk entry survived binary upgrades forever.
#[cfg_attr(not(windows), allow(dead_code))] // pinned cross-platform; consumed on Windows
fn pathext_extensions(session_pathext: Option<&str>) -> Vec<String> {
    let raw = session_pathext
        .map(str::to_string)
        .or_else(|| std::env::var("PATHEXT").ok());
    let mut extensions = vec![String::new()];
    match raw {
        Some(raw) => {
            for entry in raw.split(';') {
                let entry = entry.trim();
                if entry.starts_with('.') && !extensions.iter().any(|e| e == entry) {
                    extensions.push(entry.to_string());
                }
            }
            if extensions.len() == 1 {
                extensions.extend([".exe", ".bat", ".cmd"].iter().map(|s| s.to_string()));
            }
        }
        None => extensions.extend([".exe", ".bat", ".cmd"].iter().map(|s| s.to_string())),
    }
    extensions
}

fn which_tool(
    name: &str,
    session_path: Option<&str>,
    session_pathext: Option<&str>,
) -> Option<PathBuf> {
    // Probe PATHEXT-style suffixes on Windows only, mirroring
    // `resolve_command_path` in runtime.rs. On Unix the bare name is the
    // only candidate — a literal `name.exe` in an earlier PATH directory
    // must not shadow the real extensionless `name` — and it must carry an
    // executable bit.
    let extensions: Vec<String> = if cfg!(windows) {
        pathext_extensions(session_pathext)
    } else {
        vec![String::new()]
    };

    let path_env = match session_path {
        Some(path) => path.to_string(),
        None => std::env::var("PATH").ok()?,
    };
    for dir in std::env::split_paths(&path_env) {
        for ext in &extensions {
            let candidate = dir.join(format!("{}{}", name, ext));
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// True when `path` names a file we may execute: a regular file, plus — on
/// Unix — any executable bit set.
#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn spec(cmd: &str, args: &[&str]) -> DynamicCommand {
        DynamicCommand {
            cmd: cmd.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            cache_ttl_secs: None,
        }
    }

    /// niubash#839: the fingerprint probe reads the SESSION PATH the
    /// completer snapshots — a tool living only in a session-only
    /// directory is found through it and invisible without it.
    #[test]
    fn issue839_which_tool_probes_the_session_path() {
        let dir = unique_temp_dir("niubash-issue839-which");
        let file_name = if cfg!(windows) {
            "t839tool.exe"
        } else {
            "t839tool"
        };
        let tool = dir.join(file_name);
        std::fs::write(&tool, b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let session_path = dir.to_string_lossy().into_owned();
        // On Windows the resolved path carries the PATHEXT entry's own
        // case (the ambient PATHEXT spells `.EXE`), which names the
        // same file as the on-disk `.exe`; compare case-insensitively
        // there. Elsewhere the comparison stays byte-exact.
        let found = which_tool("t839tool", Some(&session_path), None);
        if cfg!(windows) {
            assert_eq!(
                found.as_ref().map(|p| p.to_string_lossy().to_lowercase()),
                Some(tool.to_string_lossy().to_lowercase()),
                "a session-PATH-only tool must be fingerprint-visible"
            );
        } else {
            assert_eq!(
                found,
                Some(tool.clone()),
                "a session-PATH-only tool must be fingerprint-visible"
            );
        }
        let elsewhere = dir.join("elsewhere");
        assert_eq!(
            which_tool("t839tool", Some(&elsewhere.to_string_lossy()), None),
            None,
            "a PATH that lacks the tool must not find it"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// niubash#839: a missing tool is a spawn failure → `None`, the
    /// same degradation as legit-empty but (via the once-per-session
    /// warning in the implementation) no longer silent.
    #[test]
    fn issue839_missing_tool_yields_no_candidates() {
        let dir = unique_temp_dir("niubash-issue839-missing");
        assert_eq!(
            run_dynamic_command(
                &spec("niu-no-such-tool-839", &[]),
                None,
                Some(&dir),
                Duration::from_secs(2),
            ),
            None
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// niubash#845: the CacheKey covers every input — two specs that
    /// differ only in args, TTL, or resolved tool path are different
    /// keys (different identity AND different disk file); identical
    /// inputs are the same key.
    #[test]
    fn issue845_cache_key_covers_args_ttl_and_tool_identity() {
        let base = spec("tool", &["list", "--type", "a"]);
        let other_args = spec("tool", &["list", "--type", "b"]);
        let mut other_ttl = spec("tool", &["list", "--type", "a"]);
        other_ttl.cache_ttl_secs = Some(300);
        let tool_a = PathBuf::from("/bin/tool");
        let tool_b = PathBuf::from("/opt/tool");

        let key = CacheKey::new("owner", &base, Some(tool_a.clone()));
        assert_eq!(key, CacheKey::new("owner", &base, Some(tool_a.clone())));
        for (label, other) in [
            (
                "args",
                CacheKey::new("owner", &other_args, Some(tool_a.clone())),
            ),
            (
                "ttl",
                CacheKey::new("owner", &other_ttl, Some(tool_a.clone())),
            ),
            ("tool path", CacheKey::new("owner", &base, Some(tool_b))),
            ("owner", CacheKey::new("other", &base, Some(tool_a.clone()))),
            ("missing tool", CacheKey::new("owner", &base, None)),
        ] {
            assert_ne!(
                key.identity(),
                other.identity(),
                "{label} must change the identity"
            );
            assert_ne!(
                key.filename(),
                other.filename(),
                "{label} must change the disk file"
            );
        }
    }

    /// niubash#845: the mem cache honors the TTL it was written with.
    #[test]
    fn issue845_mem_entry_honors_ttl_and_tool_mtime() {
        let entry = MemEntry {
            values: vec!["v".to_string()],
            tool_mtime_secs: Some(100),
            expires_at_secs: Some(1_000),
        };
        assert!(entry.is_fresh(999, Some(100)), "inside the TTL, same tool");
        assert!(!entry.is_fresh(1_000, Some(100)), "the TTL ran out");
        assert!(!entry.is_fresh(999, Some(101)), "the tool binary changed");
        let no_ttl = MemEntry {
            values: vec!["v".to_string()],
            tool_mtime_secs: Some(100),
            expires_at_secs: None,
        };
        assert!(
            no_ttl.is_fresh(u64::MAX - 1, Some(100)),
            "mtime-only entries do not expire"
        );
    }

    /// niubash#845: `.parsed.toml` files are keyed on the full source
    /// path — same stem, different directories, different cache files.
    #[test]
    fn issue845_parsed_cache_keyed_on_full_source_path() {
        let dir = unique_temp_dir("niubash-issue845-parsed");
        let a = dir.join("a").join("_foo.bash");
        let b = dir.join("b").join("_foo.bash");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();
        std::fs::write(&a, "# a").unwrap();
        std::fs::write(&b, "# b").unwrap();
        assert_ne!(parsed_cache_file(&dir, &a), parsed_cache_file(&dir, &b));
        assert_eq!(parsed_cache_file(&dir, &a), parsed_cache_file(&dir, &a));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// niubash#845: parsed-cache freshness is mtime EQUALITY — a
    /// source re-materialized with an older mtime (the `>=` bug shape)
    /// is stale, and so are both directions of disagreement.
    #[test]
    fn issue845_parsed_freshness_requires_mtime_equality() {
        assert!(parsed_cache_fresh(Some(500), Some(500)));
        assert!(
            !parsed_cache_fresh(Some(900), Some(500)),
            "cache newer than an older-mtime source must NOT be trusted"
        );
        assert!(
            !parsed_cache_fresh(Some(500), Some(900)),
            "source rewritten newer"
        );
        assert!(!parsed_cache_fresh(None, Some(500)), "no recorded mtime");
        assert!(
            !parsed_cache_fresh(Some(500), None),
            "no readable source mtime"
        );
        // The stored-mtime header round-trips through a real TOML doc.
        let body = format!("{}\ncommand = \"x\"\n", parsed_cache_mtime_header(500));
        assert_eq!(parsed_cache_stored_mtime(&body), Some(500));
        assert!(
            toml::from_str::<CommandDef>(&body).is_ok(),
            "the header is a TOML comment"
        );
    }

    /// niubash#845: a disk entry answers only for the tool identity
    /// recorded in it — a different resolved path is stale even when
    /// the TTL has not run out.
    #[test]
    fn issue845_disk_cache_stale_on_tool_identity_change() {
        let dir = unique_temp_dir("niubash-issue845-disk");
        let tool = dir.join("tool");
        std::fs::write(&tool, b"x").unwrap();
        let mtime_secs = std::fs::metadata(&tool)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let entry = DiskCache {
            source_cmd: "tool".to_string(),
            cache_identity: "id".to_string(),
            tool_path: Some(tool.to_string_lossy().into_owned()),
            written_secs: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            ttl_secs: 0,
            tool_mtime_secs: Some(mtime_secs),
            values: vec!["v".to_string()],
        };
        assert!(!entry.is_stale(Some(&tool)), "same tool, same mtime: fresh");
        let other = dir.join("other-tool");
        assert!(
            entry.is_stale(Some(&other)),
            "a different resolved path is a different tool"
        );
        assert!(entry.is_stale(None), "the recorded tool vanished");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// niubash#845: PATHEXT comes from the session value, not a
    /// hardcoded list (the defaults only fill an unset/empty one).
    #[test]
    fn issue845_pathext_extensions_come_from_the_session_value() {
        assert_eq!(
            pathext_extensions(Some(".COM;.EXE;.PY")),
            vec![
                "".to_string(),
                ".COM".to_string(),
                ".EXE".to_string(),
                ".PY".to_string()
            ]
        );
        let defaults = pathext_extensions(None);
        // With no session value the function falls back to the ambient
        // PATHEXT environment before its built-in defaults, so the
        // case of the entries is the environment's own (`.EXE` on a
        // Windows runner, `.exe` in the built-in list). The claim this
        // pins is membership, not spelling: compare case-insensitively.
        let has_ext = |want: &str| defaults.iter().any(|e| e.eq_ignore_ascii_case(want));
        assert!(has_ext(".exe") && has_ext(".bat"));
    }

    /// niubash#845 end-to-end (unix): two `takes_value` flags sharing
    /// one tool with different args must NOT share a cache entry —
    /// the second flag's values are its own, not the first flag's.
    #[cfg(unix)]
    #[test]
    fn issue845_flags_sharing_a_tool_with_different_args_do_not_collide() {
        let dir = unique_temp_dir("niubash-issue845-e2e");
        let mut flag_a = FlagDef {
            short: None,
            long: Some("--aa".to_string()),
            description: None,
            takes_value: true,
            values_source: None,
        };
        flag_a.values_source = Some(ValuesSource::Dynamic {
            values_from_command: spec("sh", &["-c", "printf 'from-a\\n'"]),
        });
        let mut flag_b = FlagDef {
            long: Some("--bb".to_string()),
            ..flag_a.clone()
        };
        flag_b.values_source = Some(ValuesSource::Dynamic {
            values_from_command: spec("sh", &["-c", "printf 'from-b\\n'"]),
        });
        let def = CommandDef {
            command: "t845".to_string(),
            description: None,
            flags: vec![flag_a, flag_b],
            subcommands: Vec::new(),
        };
        let mut definitions = HashMap::new();
        definitions.insert("t845".to_string(), def);
        let plugin = ExternalCompletionPlugin {
            definitions,
            mem_cache: Mutex::new(HashMap::new()),
            cache_dir: Some(dir.clone()),
            legacy_cache_dir: None,
        };
        let values_for = |input: &str| -> Vec<String> {
            let context = CompletionContext::new(dir.clone(), input.to_string(), input.len());
            plugin
                .complete(&context)
                .map(|result| result.completions)
                .unwrap_or_default()
        };
        assert_eq!(values_for("t845 --aa "), vec!["from-a".to_string()]);
        assert_eq!(
            values_for("t845 --bb "),
            vec!["from-b".to_string()],
            "the second flag must not be served the first flag's cached values"
        );
        // And the first flag still serves its own afterwards.
        assert_eq!(values_for("t845 --aa "), vec!["from-a".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn issue839_dynamic_run_success_failure_and_timeout_shapes() {
        let dir = unique_temp_dir("niubash-issue839-run");
        // Success: stdout lines are the candidates.
        assert_eq!(
            run_dynamic_command(
                &spec("sh", &["-c", "printf 'alpha\\nbeta\\n'"]),
                None,
                Some(&dir),
                Duration::from_secs(5),
            ),
            Some(vec!["alpha".to_string(), "beta".to_string()])
        );
        // Non-zero exit: no candidates (and distinct in the log).
        assert_eq!(
            run_dynamic_command(
                &spec("sh", &["-c", "exit 3"]),
                None,
                Some(&dir),
                Duration::from_secs(5),
            ),
            None
        );
        // A hanging tool is bounded: killed at the timeout instead of
        // freezing the TAB path.
        let started = Instant::now();
        assert_eq!(
            run_dynamic_command(
                &spec("sh", &["-c", "sleep 5"]),
                None,
                Some(&dir),
                Duration::from_millis(300),
            ),
            None
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the bounded wait must return promptly, took {:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// niubash#833: a poisoned mem-cache lock used to read as a
    /// permanent cache miss, so `values_from_command` respawned its
    /// subprocess on every TAB. The recovered guard must serve the
    /// cached values instead.
    #[cfg(unix)]
    #[test]
    fn issue833_poisoned_mem_cache_does_not_respawn_dynamic_command() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "niu-issue833-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let counter = dir.join("counter");
        let script = dir.join("tool.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho run >> \"{}\"\necho alpha\necho beta\n",
                counter.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut plugin = ExternalCompletionPlugin::new();
        // No disk cache: the in-memory cache is the only warm tier,
        // so a mem miss is directly observable as a subprocess respawn.
        plugin.cache_dir = None;
        plugin.definitions.insert(
            "issue833tool".to_string(),
            CommandDef {
                command: "issue833tool".to_string(),
                description: None,
                flags: vec![FlagDef {
                    short: None,
                    long: Some("--opt".to_string()),
                    description: None,
                    takes_value: true,
                    values_source: Some(ValuesSource::Dynamic {
                        values_from_command: DynamicCommand {
                            cmd: script.to_string_lossy().into_owned(),
                            args: Vec::new(),
                            cache_ttl_secs: None,
                        },
                    }),
                }],
                subcommands: Vec::new(),
            },
        );

        let runs = || {
            std::fs::read_to_string(&counter)
                .map(|s| s.lines().count())
                .unwrap_or(0)
        };
        let input = "issue833tool --opt ";
        let context = CompletionContext::new(PathBuf::from("."), input.to_string(), input.len());

        let first = plugin
            .complete(&context)
            .expect("dynamic values on first completion");
        assert!(first.completions.iter().any(|v| v == "alpha"));
        assert!(first.completions.iter().any(|v| v == "beta"));
        assert_eq!(runs(), 1, "first completion runs the subprocess once");

        let _ = plugin.complete(&context);
        assert_eq!(runs(), 1, "second completion is a warm mem-cache hit");

        // Poison the mem-cache lock the way a panic in a writer would.
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let _guard = plugin.mem_cache.lock().unwrap();
                panic!("issue833: deliberate mem_cache poison");
            });
            assert!(handle.join().is_err());
        });
        assert!(
            plugin.mem_cache.lock().is_err(),
            "the lock must actually be poisoned for this pin to mean anything"
        );

        let third = plugin
            .complete(&context)
            .expect("dynamic values must survive mem-cache poison");
        assert_eq!(third.completions, first.completions);
        assert_eq!(
            runs(),
            1,
            "poisoned mem cache must not respawn the subprocess per TAB"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// niubash#841: the primary cache dir uses the NIU_ name and
    /// ~/.niubash; the pre-rename WINSH_ name and ~/.winsh feed only
    /// the read-only legacy fallback.
    #[test]
    fn issue841_cache_dir_resolution_uses_niu_names() {
        let _lock = crate::test_support::PROCESS_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _restore = crate::test_support::ProcessEnvRestore::new(&[
            "NIU_COMPLETION_CACHE_DIR",
            "WINSH_COMPLETION_CACHE_DIR",
            "HOME",
            "USERPROFILE",
        ]);
        let home = unique_temp_dir("issue841-home");
        std::env::set_var("HOME", &home);
        std::env::remove_var("USERPROFILE");
        std::env::remove_var("NIU_COMPLETION_CACHE_DIR");
        std::env::remove_var("WINSH_COMPLETION_CACHE_DIR");

        // On Windows the resolver's home spelling is the long form
        // (runneradmin) while temp_dir hands out the 8.3 short form
        // (RUNNER~1) for the same directory; compare the home-based
        // expectations through the canonical long spelling there.
        // Elsewhere the comparison stays byte-exact.
        if cfg!(windows) {
            let spelled = |suffix: &str| -> Option<PathBuf> {
                std::fs::canonicalize(&home).ok().map(|base| {
                    let base = base
                        .to_string_lossy()
                        .trim_start_matches(r"\\?\")
                        .to_string();
                    suffix
                        .split('/')
                        .fold(PathBuf::from(base), |acc, part| acc.join(part))
                })
            };
            assert_eq!(resolve_cache_dir(), spelled(".niubash/completions/cache"));
            assert_eq!(legacy_cache_dir(), spelled(".winsh/completions/cache"));
        } else {
            assert_eq!(
                resolve_cache_dir(),
                Some(home.join(".niubash").join("completions").join("cache"))
            );
            assert_eq!(
                legacy_cache_dir(),
                Some(home.join(".winsh").join("completions").join("cache"))
            );
        }

        // The NIU_ variable names the primary directory.
        let custom = home.join("custom-cache");
        std::env::set_var("NIU_COMPLETION_CACHE_DIR", &custom);
        assert_eq!(resolve_cache_dir(), Some(custom.clone()));

        // The WINSH_ variable feeds only the legacy fallback — it
        // must not move the primary directory back.
        let legacy_custom = home.join("legacy-cache");
        std::env::set_var("WINSH_COMPLETION_CACHE_DIR", &legacy_custom);
        assert_eq!(legacy_cache_dir(), Some(legacy_custom));
        assert_eq!(resolve_cache_dir(), Some(custom));

        let _ = std::fs::remove_dir_all(&home);
    }

    /// niubash#841: a cache entry written by an older build into the
    /// legacy directory is still served (no subprocess respawn), and
    /// the legacy directory is not written in the process.
    #[cfg(unix)]
    #[test]
    fn issue841_legacy_disk_cache_is_served_read_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = unique_temp_dir("issue841-fallback");
        let primary = root.join("primary");
        let legacy = root.join("legacy");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&legacy).unwrap();
        let counter = root.join("counter");
        let script = root.join("tool.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho run >> \"{}\"\necho freshvalue\n",
                counter.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cmd = script.to_string_lossy().into_owned();

        // Plant a legacy disk entry for this dynamic command. The
        // recorded tool mtime must match the script's, or the entry
        // is (correctly) judged stale.
        let tool_mtime_secs = std::fs::metadata(&script)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Planted under the CURRENT key discipline (niubash#845):
        // the entry must carry the identity the resolver computes
        // for this exact dynamic command and resolved tool, and
        // live under that key's hashed filename, or the read is
        // (correctly) a miss.
        let spec = DynamicCommand {
            cmd: cmd.clone(),
            args: Vec::new(),
            cache_ttl_secs: None,
        };
        let cache_key = CacheKey::new("issue841tool", &spec, Some(script.clone()));
        let disk = DiskCache {
            source_cmd: cmd.clone(),
            cache_identity: cache_key.identity(),
            tool_path: Some(cmd.clone()),
            written_secs: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            ttl_secs: 0,
            tool_mtime_secs: Some(tool_mtime_secs),
            values: vec!["legacyvalue".to_string()],
        };
        std::fs::write(
            legacy.join(cache_key.filename()),
            toml::to_string_pretty(&disk).unwrap(),
        )
        .unwrap();

        let mut plugin = ExternalCompletionPlugin::new();
        plugin.cache_dir = Some(primary.clone());
        plugin.legacy_cache_dir = Some(legacy.clone());
        plugin.definitions.insert(
            "issue841tool".to_string(),
            CommandDef {
                command: "issue841tool".to_string(),
                description: None,
                flags: vec![FlagDef {
                    short: None,
                    long: Some("--opt".to_string()),
                    description: None,
                    takes_value: true,
                    values_source: Some(ValuesSource::Dynamic {
                        values_from_command: spec,
                    }),
                }],
                subcommands: Vec::new(),
            },
        );

        let input = "issue841tool --opt ";
        let context = CompletionContext::new(PathBuf::from("."), input.to_string(), input.len());
        let result = plugin
            .complete(&context)
            .expect("the legacy disk entry must be served");
        assert_eq!(result.completions, vec!["legacyvalue".to_string()]);
        assert!(
            !counter.exists(),
            "a legacy hit must not respawn the subprocess"
        );
        assert!(
            std::fs::read_dir(&primary).unwrap().next().is_none(),
            "nothing is written anywhere on a legacy read hit"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    fn flag(long: &str) -> FlagDef {
        FlagDef {
            short: None,
            long: Some(long.to_string()),
            description: None,
            takes_value: false,
            values_source: None,
        }
    }

    fn git_def() -> CommandDef {
        CommandDef {
            command: "git".to_string(),
            description: None,
            flags: vec![flag("--version")],
            subcommands: vec![SubcommandDef {
                name: "checkout".to_string(),
                description: None,
                flags: vec![flag("--detach")],
            }],
        }
    }

    fn context(input: &str) -> CompletionContext {
        CompletionContext::new(PathBuf::new(), input.to_string(), input.len())
    }

    /// niubash#851: `effective_flags` must resolve the sub-command
    /// from the current segment, not from whole-line token [1] —
    /// `true; git checkout -<TAB>` serves checkout's flags, and a
    /// quoted command word still resolves.
    #[test]
    fn issue851_effective_flags_uses_the_current_segment() {
        let def = git_def();

        let flags = ExternalCompletionPlugin::effective_flags(&def, &context("git checkout -"));
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].long.as_deref(), Some("--detach"));

        let flags =
            ExternalCompletionPlugin::effective_flags(&def, &context("true; git checkout -"));
        assert_eq!(
            flags.len(),
            1,
            "the segment after `;` owns the completion, not the line's first command"
        );
        assert_eq!(flags[0].long.as_deref(), Some("--detach"));

        let flags = ExternalCompletionPlugin::effective_flags(&def, &context("'git' checkout -"));
        assert_eq!(flags[0].long.as_deref(), Some("--detach"));

        // No sub-command in the segment: top-level flags.
        let flags = ExternalCompletionPlugin::effective_flags(&def, &context("git -"));
        assert_eq!(flags[0].long.as_deref(), Some("--version"));
        let flags = ExternalCompletionPlugin::effective_flags(&def, &context("true; git -"));
        assert_eq!(flags[0].long.as_deref(), Some("--version"));
    }
}
