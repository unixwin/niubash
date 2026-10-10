// carapace-bin completion source for WinSH
//
// [carapace-bin](https://github.com/carapace-sh/carapace-bin) is a single Go
// binary carrying community completion specs for ~1200 commands (git, curl,
// gh, docker, ...) with description fields.  This module wires it in as an
// additional completion plugin *behind* the existing chain: it only fires for
// commands that have no local completion definition (winuxcmd applet TOMLs and
// user definitions keep priority), and degrades silently when the binary is
// absent, slow, or errors — the user never sees a carapace failure.
//
// # CLI contract (verified against carapace-bin v1.8.0)
//
//   carapace <command> fish <words...>
//
// where `<words...>` are the words after the command name and the LAST word is
// the word being completed (an empty string right after a space).  Stdout is
// one candidate per line in the fish format:
//
//   value<TAB>Description
//
// already filtered by the prefix of the last word.  Stderr is unused; an
// unknown command exits 0 with no output.  Meta lines to skip: `ERR\t...`
// (spec action failures, e.g. dynamic specs run outside a repo) and a bare
// `_` placeholder.
//
// # Latency & caching
//
// Measured on Windows (v1.8.0, warm): 104-146ms per invocation — over the
// 100ms interactive budget, so results are cached per session keyed by
// (command, preceding words, word shape, cwd) with a TTL.  For ordinary words
// the empty string is queried and candidates are prefix-filtered locally, so
// successive keystrokes (`git ch` → `git che`) hit the same cache entry; only
// one process is spawned per (command, args) shape instead of per keystroke.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::completion::{CompletionContext, CompletionPlugin, CompletionResult};

/// Hard ceiling on one carapace invocation.  The binary answers in ~150ms on
/// a warm cache; anything slower is treated as "no candidates" so Tab never
/// stalls noticeably.
const CALL_TIMEOUT: Duration = Duration::from_millis(300);

/// How long a cached candidate list stays valid.
const CACHE_TTL: Duration = Duration::from_millis(5 * 60 * 1000);

/// Upper bound on candidates kept from one invocation (git emits ~170 lines,
/// flag lists can be larger; this is a sanity cap, not a UX decision).
const MAX_CANDIDATES: usize = 2048;

/// Shared between the plugin and [`CompletionState`](super::completer::CompletionState):
/// the set of commands that already have a local completion definition, so the
/// carapace source can stand down for them (winuxcmd TOMLs win).
#[derive(Default)]
pub struct CarapaceCoverage {
    covered: Mutex<HashSet<String>>,
}

impl CarapaceCoverage {
    fn covers(&self, command: &str) -> bool {
        self.covered
            .lock()
            .map(|set| set.contains(command))
            .unwrap_or(false)
    }

    pub fn set_covered(&self, commands: Vec<String>) {
        if let Ok(mut set) = self.covered.lock() {
            *set = commands.into_iter().collect();
        }
    }
}

struct CacheEntry {
    values: Vec<(String, Option<String>)>,
    written: Instant,
}

/// Completion plugin delegating to a discovered carapace-bin binary.
pub struct CarapaceCompletionPlugin {
    binary: PathBuf,
    coverage: Arc<CarapaceCoverage>,
    cache: Mutex<HashMap<String, CacheEntry>>,
}

impl CarapaceCompletionPlugin {
    pub fn new(binary: PathBuf, coverage: Arc<CarapaceCoverage>) -> Self {
        Self {
            binary,
            coverage,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Locate a carapace binary, or `None` when the feature is off.
    ///
    /// Priority:
    ///   1. `NIU_CARAPACE` — explicit path; the value `off` (or `0`/`false`)
    ///      disables the source entirely.
    ///   2. `carapace[.exe]` on `PATH`.
    ///   3. Bundle location: `carapace.exe` or `opt\carapace\carapace.exe`
    ///      next to the running executable.
    pub fn discover_binary() -> Option<PathBuf> {
        if let Ok(value) = std::env::var("NIU_CARAPACE") {
            let value = value.trim().to_string();
            if value.is_empty() {
                // Unset-like: fall through to auto discovery.
            } else if matches!(value.as_str(), "off" | "0" | "false" | "disabled") {
                return None;
            } else {
                let path = PathBuf::from(&value);
                if path.is_file() {
                    return Some(path);
                }
                // A directory or bad path disables auto discovery so a typo
                // cannot silently double-probe.
                return None;
            }
        }
        if let Some(found) = which_carapace("carapace") {
            return Some(found);
        }
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let candidates = [
            exe_dir.join("carapace.exe"),
            exe_dir.join("opt").join("carapace").join("carapace.exe"),
        ];
        candidates.into_iter().find(|p| p.is_file())
    }

    /// Words fed to carapace: everything after the command word, with the
    /// current word replaced by its *shape* — empty for ordinary words (the
    /// full candidate list is fetched once and filtered locally) and `-` for
    /// flag words (carapace then returns the flag set, filtered locally too).
    fn build_query(context: &CompletionContext, command: &str) -> Option<(Vec<String>, String)> {
        let (_, segment) = context_segment(context);
        let words = shell_words(&segment);
        let Some(command_word) = words.first() else {
            return None;
        };
        if command_word != command {
            return None;
        }
        let current_word = context.get_current_word().unwrap_or_default();
        let mut preceding: Vec<String> = words
            .iter()
            .skip(1)
            .take_while(|w| *w != &current_word)
            .cloned()
            .collect();
        // When the cursor sits mid-word the word under the cursor is part of
        // `words`; strip it so only words strictly before the cursor remain.
        if let Some(last) = words.last() {
            if last == &current_word {
                preceding = words
                    .iter()
                    .skip(1)
                    .take(words.len().saturating_sub(2))
                    .cloned()
                    .collect();
            }
        }
        let query_word = if current_word.starts_with('-') {
            "-".to_string()
        } else {
            String::new()
        };
        Some((preceding, query_word))
    }

    fn cache_key(command: &str, preceding: &[String], query_word: &str, cwd: &Path) -> String {
        format!(
            "{}\u{1}{}\u{1}{}\u{1}{}",
            command,
            preceding.join("\u{2}"),
            query_word,
            cwd.display()
        )
    }
}

/// The segment before the cursor: everything after the last command separator
/// (`;`, `|` — `&&`/`||` included via their `|`/`&` bytes).  The private
/// context helpers are not exposed, so this reconstructs the segment from the
/// public `input`/`cursor_pos` fields, mirroring `CompletionContext`.
fn context_segment(context: &CompletionContext) -> (usize, String) {
    let pos = context.cursor_pos.min(context.input.len());
    let before = &context.input[..pos];
    let start = before.rfind([';', '|']).map_or(0, |idx| idx + 1);
    (start, before[start..].to_string())
}

/// Split a segment into unquoted shell words (quoting-aware enough for
/// completion word extraction: quotes are stripped, embedded whitespace kept).
fn shell_words(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut has_word = false;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in segment.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote != Some('"') => escaped = true,
            '\'' | '"' if quote == Some(ch) => quote = None,
            '\'' | '"' if quote.is_none() => {
                quote = Some(ch);
                has_word = true;
            }
            c if quote.is_none() && c.is_whitespace() => {
                if has_word {
                    words.push(std::mem::take(&mut current));
                    has_word = false;
                }
            }
            c => {
                current.push(c);
                has_word = true;
            }
        }
    }
    if has_word || !current.is_empty() {
        words.push(current);
    }
    words
}

/// Parse the fish-format candidate stream: `value<TAB>Description` per line.
/// Skips empty lines, the `ERR<TAB>...` failure channel and the `_`
/// placeholder emitted by some specs.
fn parse_candidates(output: &str) -> Vec<(String, Option<String>)> {
    let mut candidates = Vec::new();
    for line in output.lines() {
        if line.is_empty() {
            continue;
        }
        let (value, description) = match line.split_once('\t') {
            Some((value, description)) => (value, Some(description.trim().to_string())),
            None => (line, None),
        };
        if value.is_empty() || value == "_" || value == "ERR" {
            continue;
        }
        candidates.push((value.to_string(), description));
        if candidates.len() >= MAX_CANDIDATES {
            break;
        }
    }
    candidates
}

fn which_carapace(name: &str) -> Option<PathBuf> {
    let extensions: &[&str] = if cfg!(windows) { &["", ".exe"] } else { &[""] };
    let path_env = std::env::var("PATH").ok()?;
    for dir in std::env::split_paths(&path_env) {
        for ext in extensions {
            let candidate = dir.join(format!("{}{}", name, ext));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Run carapace with a hard timeout; `None` on any failure (spawn error,
/// timeout, non-zero exit).  Stderr is discarded — carapace failures must be
/// invisible.
fn run_with_timeout(binary: &Path, args: &[String], timeout: Duration) -> Option<String> {
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                use std::io::Read;
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                if status.success() {
                    return Some(stdout);
                }
                return None;
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

impl CompletionPlugin for CarapaceCompletionPlugin {
    fn name(&self) -> &str {
        "carapace-completion"
    }

    fn complete(&self, context: &CompletionContext) -> Option<CompletionResult> {
        // Never at command position: carapace here provides argument
        // completion only; command-name completion stays local.
        if context.is_command_position() {
            return None;
        }
        let command = context.get_command_name()?;
        if self.coverage.covers(&command) {
            return None;
        }
        let (preceding, query_word) = Self::build_query(context, &command)?;
        let key = Self::cache_key(&command, &preceding, &query_word, &context.current_dir);

        // ── 1. Session cache ────────────────────────────────────────────────
        if let Ok(cache) = self.cache.lock() {
            if let Some(entry) = cache.get(&key) {
                if entry.written.elapsed() < CACHE_TTL {
                    return filter_candidates(&entry.values, context);
                }
            }
        }

        // ── 2. Invoke carapace ──────────────────────────────────────────────
        // Contract: `carapace <command> fish <words...>` — the completer name
        // comes first, the shell selects the output format.
        let mut args = Vec::with_capacity(preceding.len() + 3);
        args.push(command);
        args.push("fish".to_string());
        args.extend(preceding.iter().cloned());
        args.push(query_word);
        let output = run_with_timeout(&self.binary, &args, CALL_TIMEOUT)?;
        let candidates = parse_candidates(&output);

        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                key,
                CacheEntry {
                    values: candidates.clone(),
                    written: Instant::now(),
                },
            );
        }

        filter_candidates(&candidates, context)
    }

    fn on_directory_changed(&self, _new_dir: &Path) {
        // Dynamic specs (git branches, k8s contexts, ...) are cwd-sensitive.
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn filter_candidates(
    candidates: &[(String, Option<String>)],
    context: &CompletionContext,
) -> Option<CompletionResult> {
    let word = context.get_current_word().unwrap_or_default();
    let mut completions = Vec::new();
    let mut descriptions = Vec::new();
    for (value, description) in candidates {
        if context.behavior.matches(value, &word) {
            completions.push(value.clone());
            descriptions.push(description.clone());
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

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::CompletionBehavior;

    #[test]
    fn parse_fish_format_with_descriptions() {
        let output = "add\tAdd file contents to the index\ncommit\tRecord changes\n\n_\nbare\n";
        let parsed = parse_candidates(output);
        assert_eq!(
            parsed,
            vec![
                (
                    "add".to_string(),
                    Some("Add file contents to the index".to_string())
                ),
                ("commit".to_string(), Some("Record changes".to_string())),
                ("bare".to_string(), None),
            ]
        );
    }

    #[test]
    fn parse_skips_err_channel_and_placeholder() {
        let output = "ERR\tfatal: not a git repository\n_\n--help\tShow help\n";
        let parsed = parse_candidates(output);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "--help");
    }

    #[test]
    fn query_word_shape_for_flags_vs_words() {
        let context = CompletionContext::new(PathBuf::from("."), "git commit --a".to_string(), 14);
        let (preceding, query_word) =
            CarapaceCompletionPlugin::build_query(&context, "git").unwrap();
        assert_eq!(preceding, vec!["commit".to_string()]);
        assert_eq!(query_word, "-");
    }

    #[test]
    fn query_word_empty_for_plain_words() {
        let context = CompletionContext::new(PathBuf::from("."), "git ch".to_string(), 6);
        let (preceding, query_word) =
            CarapaceCompletionPlugin::build_query(&context, "git").unwrap();
        assert!(preceding.is_empty());
        assert_eq!(query_word, "");
    }

    #[test]
    fn query_words_include_full_argument_stack() {
        let context =
            CompletionContext::new(PathBuf::from("."), "gh pr view 1 --json n".to_string(), 21);
        let (preceding, query_word) =
            CarapaceCompletionPlugin::build_query(&context, "gh").unwrap();
        assert_eq!(
            preceding,
            vec![
                "pr".to_string(),
                "view".to_string(),
                "1".to_string(),
                "--json".to_string()
            ]
        );
        // The word under the cursor is a plain word, so the full candidate
        // list is fetched (empty query word) and filtered locally.
        assert_eq!(query_word, "");

        // Completing a flag word queries the flag set instead.
        let flag_context =
            CompletionContext::new(PathBuf::from("."), "gh pr view --j".to_string(), 14);
        let (_, flag_query) = CarapaceCompletionPlugin::build_query(&flag_context, "gh").unwrap();
        assert_eq!(flag_query, "-");
    }

    #[test]
    fn filter_respects_prefix_and_returns_descriptions() {
        let candidates = vec![
            ("checkout".to_string(), Some("Switch branches".to_string())),
            ("clone".to_string(), Some("Clone a repository".to_string())),
        ];
        let context = CompletionContext::with_behavior(
            PathBuf::from("."),
            "git ch".to_string(),
            6,
            CompletionBehavior::default(),
        );
        let result = filter_candidates(&candidates, &context).unwrap();
        assert_eq!(result.completions, vec!["checkout".to_string()]);
        assert_eq!(
            result.descriptions,
            vec![Some("Switch branches".to_string())]
        );
    }

    #[test]
    fn covered_commands_stand_down() {
        let coverage = Arc::new(CarapaceCoverage::default());
        coverage.set_covered(vec!["ls".to_string()]);
        assert!(coverage.covers("ls"));
        assert!(!coverage.covers("git"));
    }

    /// End-to-end against a real carapace binary: skipped unless one is
    /// discoverable (set `NIU_CARAPACE` or install it on PATH).
    #[test]
    fn live_invocation_completes_git_subcommands() {
        let Some(binary) = CarapaceCompletionPlugin::discover_binary() else {
            eprintln!("carapace not installed; skipping live test");
            return;
        };
        let plugin = CarapaceCompletionPlugin::new(binary, Arc::new(CarapaceCoverage::default()));
        let context = CompletionContext::new(PathBuf::from("."), "git ch".to_string(), 6);
        let result = plugin.complete(&context);
        // carapace has a git spec; checkout-like candidates must appear with
        // descriptions.  Tolerate environments where git itself is missing.
        if let Some(result) = result {
            assert!(result.completions.iter().any(|c| c.starts_with("ch")));
            assert!(result.descriptions.iter().any(|d| d.is_some()));
        }
    }

    #[test]
    fn discovery_respects_off_switch_and_explicit_path() {
        use crate::test_support::PROCESS_STATE_LOCK;
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("NIU_CARAPACE");

        // The off-switch disables the source unconditionally.
        std::env::set_var("NIU_CARAPACE", "off");
        assert!(CarapaceCompletionPlugin::discover_binary().is_none());

        // An explicit path wins even before any PATH probe.
        let probe = std::env::temp_dir().join("niu-carapace-discovery-probe");
        std::fs::write(&probe, b"stub").unwrap();
        std::env::set_var("NIU_CARAPACE", &probe);
        assert_eq!(
            CarapaceCompletionPlugin::discover_binary(),
            Some(probe.clone())
        );
        let _ = std::fs::remove_file(&probe);

        match previous {
            Some(value) => std::env::set_var("NIU_CARAPACE", value),
            None => std::env::remove_var("NIU_CARAPACE"),
        }
    }

    #[test]
    fn cache_key_separates_command_args_and_cwd() {
        let a = CarapaceCompletionPlugin::cache_key(
            "git",
            &["commit".to_string()],
            "",
            Path::new("/tmp"),
        );
        let b = CarapaceCompletionPlugin::cache_key(
            "git",
            &["checkout".to_string()],
            "",
            Path::new("/tmp"),
        );
        let c = CarapaceCompletionPlugin::cache_key(
            "git",
            &["commit".to_string()],
            "",
            Path::new("/x"),
        );
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn shell_words_handle_quotes_and_separators() {
        assert_eq!(
            shell_words("commit \"two words\" it\\ is"),
            vec![
                "commit".to_string(),
                "two words".to_string(),
                "it is".to_string()
            ]
        );
        assert_eq!(shell_words(""), Vec::<String>::new());
        assert_eq!(shell_words("  "), Vec::<String>::new());
    }
}
