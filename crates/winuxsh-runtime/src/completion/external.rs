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
use std::sync::Mutex;
use std::time::SystemTime;

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
/// File path: `<cache_dir>/<sanitised_cache_key>.toml`
#[derive(Debug, Serialize, Deserialize)]
struct DiskCache {
    /// The sub-process command + args that produced these values (for reference).
    source_cmd: String,
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
                    return self
                        .tool_mtime_secs
                        .map_or(true, |cached| cached != mtime_secs);
                }
            }
        }
        false
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// In-memory cache (warm after first hit per session)
// ─────────────────────────────────────────────────────────────────────────────

struct MemEntry {
    values: Vec<String>,
    /// mtime secs of tool binary when loaded (mirrors DiskCache)
    tool_mtime_secs: Option<u64>,
}

/// Resolve the directory used to store dynamic completion caches.
///
/// Priority:
///   1. `WINSH_COMPLETION_CACHE_DIR` environment variable
///   2. `~/.winsh/completions/cache`
fn resolve_cache_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("WINSH_COMPLETION_CACHE_DIR") {
        let p = PathBuf::from(dir);
        if !p.exists() {
            let _ = std::fs::create_dir_all(&p);
        }
        return Some(p);
    }
    let base = shell_home_dir()?
        .join(".winsh")
        .join("completions")
        .join("cache");
    if !base.exists() {
        let _ = std::fs::create_dir_all(&base);
    }
    Some(base)
}

/// Turn a cache key like `fd::fd` into a safe filename `fd__fd.toml`.
fn cache_key_to_filename(key: &str) -> String {
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.toml", safe)
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

    /// Create a plugin with no compiled completion assets.
    ///
    /// First-party static command definitions live in the active
    /// `oh-my-winuxsh` bundle. The runtime only owns the completion engine and
    /// user/plugin loading paths, so bundle assets can update independently of
    /// the shell binary.
    pub fn new() -> Self {
        Self {
            definitions: HashMap::new(),
            mem_cache: Mutex::new(HashMap::new()),
            cache_dir: resolve_cache_dir(),
        }
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

        // Try the on-disk parsed cache
        if let Some(ref cache_dir) = self.cache_dir {
            let stem = bash_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .trim_start_matches('_');
            let cache_file = cache_dir.join(format!("{}.parsed.toml", stem));

            if cache_file.exists() {
                // Compare bash mtime against cache file mtime
                let cache_mtime = std::fs::metadata(&cache_file)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());

                let cache_fresh = match (bash_mtime, cache_mtime) {
                    (Some(bm), Some(cm)) => cm >= bm,
                    // If we can't read either mtime, assume stale
                    _ => false,
                };

                if cache_fresh {
                    if let Ok(contents) = std::fs::read_to_string(&cache_file) {
                        if let Ok(def) = toml::from_str::<CommandDef>(&contents) {
                            log::debug!(
                                "ExternalCompletionPlugin: bash cache hit {:?}",
                                cache_file
                            );
                            return Ok(def);
                        }
                    }
                } else {
                    log::debug!(
                        "ExternalCompletionPlugin: bash cache stale {:?}",
                        cache_file
                    );
                }
            }
        }

        // Parse the bash script
        let def = crate::completion::bash_import::parse_bash_completion(bash_path)?;

        // Write to cache
        if let Some(ref cache_dir) = self.cache_dir {
            let stem = bash_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .trim_start_matches('_');
            let cache_file = cache_dir.join(format!("{}.parsed.toml", stem));
            match toml::to_string_pretty(&def) {
                Ok(serialised) => {
                    if let Err(e) = std::fs::write(&cache_file, serialised) {
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

    /// Enrich flag descriptions by parsing `cmd -h` output for each loaded
    /// command.  Only fills in descriptions for flags that don't already have
    /// one.  Results are written back to the `.parsed.toml` cache so
    /// subsequent starts are instant.
    pub fn enrich_descriptions_from_help(&mut self) {
        let cmd_names: Vec<String> = self.definitions.keys().cloned().collect();

        for cmd_name in cmd_names {
            // Skip commands where every flag already has a description
            let needs_enrichment = self
                .definitions
                .get(&cmd_name)
                .map(|def| def.flags.iter().any(|f| f.description.is_none()))
                .unwrap_or(false);

            if !needs_enrichment {
                continue;
            }

            // Run `cmd -h` (some tools write help to stderr)
            let output = match std::process::Command::new(&cmd_name).arg("-h").output() {
                Ok(o) => o,
                Err(_) => continue,
            };

            let help_text = {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim().is_empty() {
                    String::from_utf8_lossy(&output.stderr).into_owned()
                } else {
                    stdout.into_owned()
                }
            };

            let desc_map = parse_help_descriptions(&help_text);
            if desc_map.is_empty() {
                continue;
            }

            let mut changed = false;
            if let Some(def) = self.definitions.get_mut(&cmd_name) {
                for flag in &mut def.flags {
                    if flag.description.is_some() {
                        continue;
                    }
                    // Try long flag first, then short
                    let desc = flag
                        .long
                        .as_ref()
                        .and_then(|l| desc_map.get(l.as_str()))
                        .or_else(|| flag.short.as_ref().and_then(|s| desc_map.get(s.as_str())));

                    if let Some(d) = desc {
                        flag.description = Some(d.clone());
                        changed = true;
                    }
                }

                // Persist enriched definitions to cache
                if changed {
                    if let Some(ref cache_dir) = self.cache_dir {
                        let cache_file = cache_dir.join(format!("{}.parsed.toml", cmd_name));
                        if let Ok(serialised) = toml::to_string_pretty(def) {
                            let _ = std::fs::write(&cache_file, serialised);
                        }
                    }
                }
            }
        }
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
        // Simple heuristic: second token in the line might be a sub-command name
        let tokens: Vec<&str> = context.input.split_whitespace().collect();
        if tokens.len() >= 2 {
            let sub_name = tokens[1];
            if let Some(sub) = def.subcommands.iter().find(|s| s.name == sub_name) {
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
            }) => {
                let cache_key = format!("{}::{}", def.command, values_from_command.cmd);
                self.resolve_dynamic(cache_key, values_from_command, context)
            }
            Some(ValuesSource::Path { .. }) => {
                // Delegate to PathCompleter by returning None here; the built-in
                // path completer further up the chain will handle it because
                // is_path_completion() will return true for a bare prefix.
                None
            }
            None => None,
        }
    }

    /// Run (or use cached) dynamic sub-process to obtain candidate values.
    ///
    /// Lookup order:
    ///   1. In-memory cache (same session, already verified)
    ///   2. Disk cache file (survives restarts; validated via mtime / TTL)
    ///   3. Sub-process execution → write disk cache → populate memory cache
    fn resolve_dynamic(
        &self,
        cache_key: String,
        spec: &DynamicCommand,
        context: &CompletionContext,
    ) -> Option<CompletionResult> {
        let tool_path = which_tool(&spec.cmd);
        let tool_mtime_secs = tool_path
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());

        // ── 1. In-memory cache ────────────────────────────────────────────────
        if let Ok(mem) = self.mem_cache.lock() {
            if let Some(entry) = mem.get(&cache_key) {
                // Stale only when tool mtime changed
                let stale = tool_mtime_secs
                    .zip(entry.tool_mtime_secs)
                    .map_or(false, |(current, cached)| current != cached);
                if !stale {
                    return self.filter_values(&entry.values, context);
                }
            }
        }

        // ── 2. Disk cache ─────────────────────────────────────────────────────
        if let Some(ref cache_dir) = self.cache_dir {
            let cache_file = cache_dir.join(cache_key_to_filename(&cache_key));
            if cache_file.exists() {
                if let Ok(contents) = std::fs::read_to_string(&cache_file) {
                    if let Ok(disk) = toml::from_str::<DiskCache>(&contents) {
                        if !disk.is_stale(tool_path.as_deref()) {
                            log::debug!(
                                "ExternalCompletionPlugin: disk cache hit {:?}",
                                cache_file
                            );
                            // Warm in-memory cache
                            if let Ok(mut mem) = self.mem_cache.lock() {
                                mem.insert(
                                    cache_key.clone(),
                                    MemEntry {
                                        values: disk.values.clone(),
                                        tool_mtime_secs: disk.tool_mtime_secs,
                                    },
                                );
                            }
                            return self.filter_values(&disk.values, context);
                        }
                        log::debug!(
                            "ExternalCompletionPlugin: disk cache stale {:?}",
                            cache_file
                        );
                    }
                }
            }
        }

        // ── 3. Sub-process ────────────────────────────────────────────────────
        log::debug!(
            "ExternalCompletionPlugin: running {:?} {:?}",
            spec.cmd,
            spec.args
        );
        let output = std::process::Command::new(&spec.cmd)
            .args(&spec.args)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let values: Vec<String> = stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        let written_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Write disk cache
        if let Some(ref cache_dir) = self.cache_dir {
            let disk = DiskCache {
                source_cmd: format!("{} {}", spec.cmd, spec.args.join(" ")),
                written_secs,
                ttl_secs: spec.cache_ttl_secs.unwrap_or(0),
                tool_mtime_secs,
                values: values.clone(),
            };
            match toml::to_string_pretty(&disk) {
                Ok(serialised) => {
                    let cache_file = cache_dir.join(cache_key_to_filename(&cache_key));
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

        // Update in-memory cache
        if let Ok(mut mem) = self.mem_cache.lock() {
            mem.insert(
                cache_key,
                MemEntry {
                    values: values.clone(),
                    tool_mtime_secs,
                },
            );
        }

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
}

// ─────────────────────────────────────────────────────────────────────────────
// Utilities
// ─────────────────────────────────────────────────────────────────────────────

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
fn which_tool(name: &str) -> Option<PathBuf> {
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_env) {
            for ext in &["", ".exe", ".bat", ".cmd"] {
                let candidate = dir.join(format!("{}{}", name, ext));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Help text parser – extract flag descriptions from `cmd -h` output
// ─────────────────────────────────────────────────────────────────────────────

/// Parse help text to extract flag→description mappings.
///
/// Recognises standard help formats produced by clap, argparse, etc:
/// ```text
///   -s, --case-sensitive             Description text
///       --long-only                  Description text
///   -e, --regexp <PATTERN>           Description text
/// ```
fn parse_help_descriptions(help_text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();

    for line in help_text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('-') {
            continue;
        }

        // Split at first double-space gap (separates flags from description)
        let (flag_part, desc) = match split_at_double_space(trimmed) {
            Some(pair) => pair,
            None => continue,
        };

        let desc = desc.trim();
        if desc.is_empty() {
            continue;
        }

        // Extract flag names from the flag portion
        for token in flag_part.split(|c: char| c == ',' || c == ' ') {
            let token = token.trim();
            if token.starts_with("--") {
                // Strip =VALUE suffix
                let flag = token.split('=').next().unwrap_or(token);
                map.insert(flag.to_string(), desc.to_string());
            } else if token.starts_with('-')
                && token.len() == 2
                && token.as_bytes()[1].is_ascii_alphanumeric()
            {
                map.insert(token.to_string(), desc.to_string());
            }
        }
    }

    map
}

/// Split a string at the first occurrence of 2+ consecutive spaces.
/// Returns `(before_gap, after_gap)` where `after_gap` starts at the first
/// non-space character after the gap.
fn split_at_double_space(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b' ' && bytes[i + 1] == b' ' {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j < bytes.len() {
                return Some((&s[..i], &s[j..]));
            }
        }
        i += 1;
    }
    None
}
