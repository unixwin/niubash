//! Asset-level activation for trusted external sources — `niu plugin
//! list/enable/disable` operating on the *real* manager assets
//! (oh-my-bash themes/plugins/aliases/completions, bash-it components,
//! bash-completion as a whole, and per-file sources for wild plugins and
//! bpkg packages), each through the manager's own selection mechanism
//! (§11: preserve the native layout; owner ruling 2026-10-02: the external
//! ecosystem is the first-class content).
//!
//! Since §14.6.3 the CLI verbs are *sugar over the declarative spec*
//! (`~/.niubash/plugins.toml`): `enable`/`disable` edit the spec and run
//! the reconciler (`plugins::sync`), which materializes the managed blocks
//! idempotently. The spec is the source of truth; the rc block is derived.
//!
//! Activation state lives where the manager itself looks for it:
//!
//! * oh-my-bash — a managed block in `~/.niubashrc` defining
//!   `OSH_THEME`/`plugins=(…)`/`aliases=(…)`/`completions=(…)` right before
//!   the guarded loader snippet (exactly the arrays `oh-my-bash.sh`
//!   consumes; declarative, lazy.nvim-spec-style).
//! * bash-it — `enabled/<priority>---<file>` entries in the source tree
//!   (the mechanism `scripts/reloader.bash` reads); the theme variable and
//!   the guarded loader live in the managed rc block.
//! * bash-completion — one managed loader block; activation is
//!   whole-source.
//! * wild file sources / bpkg packages — one guarded `. <path>` line per
//!   enabled file inside the managed block (§14.4: byte-faithful to
//!   manually sourcing the file under GNU bash, missing functions and
//!   all).
//!
//! Every block keeps the adapter's existence guard, so a missing tree is a
//! silent no-op at startup (iron law 2: the fallback chain never breaks).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _};

use super::sources::{
    adapter_for, list_sources, read_source_registry, SelectionModel, SourceAsset, SourceAssetKind,
    SourceRecord, SourceStatus,
};
use super::spec::{self, PluginSpec, SpecSource};
use crate::path_utils::shell_home_dir;

/// Marker pair wrapping one managed block in `~/.niubashrc`. Managed lines
/// are rewritten by niu; everything outside the markers is the user's.
/// Source blocks and tool PATH blocks each carry their own marker kind —
/// `niu source` vs `niu tool` — so a tool id can never collide with a
/// source id's block and every managed block is surgically removable.
fn begin_marker(id: &str) -> String {
    format!("# >>> niu source {id} (managed by `niu plugin enable/disable`) >>>")
}

fn end_marker(id: &str) -> String {
    format!("# <<< niu source {id} <<<")
}

/// True when `id` has a managed source block in the rc (the enable state
/// as the loader sees it — one source of truth for listings and the
/// plugin UI). The former tool PATH-block channel retired with the
/// download driver (owner ruling 2026-10-04).
pub fn managed_block_present(id: &str) -> bool {
    let Ok(text) = fs::read_to_string(rc_file()) else {
        return false;
    };
    text.lines().any(|line| line.trim() == begin_marker(id))
}

/// The primary interactive rc file asset activation edits.
pub fn rc_file() -> PathBuf {
    shell_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".niubashrc")
}

/// Theme assignments (`OSH_THEME=`/`BASH_IT_THEME=`, with or without
/// `export`) hand-written OUTSIDE any managed block (niubash#196). The
/// supported channel is the `theme` field of a `[[sources]]` entry in
/// `~/.niubash/plugins.toml`; a hand-written rc line is rewritten away
/// when the managed block materializes, so callers can warn about it.
pub fn stray_theme_assignment_lines() -> Vec<String> {
    const THEME_VARS: [&str; 2] = ["OSH_THEME", "BASH_IT_THEME"];
    let Ok(text) = fs::read_to_string(rc_file()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut in_managed = false;
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("# >>> niu source ") {
            in_managed = true;
            continue;
        }
        if trimmed.starts_with("# <<< niu source ") {
            in_managed = false;
            continue;
        }
        if in_managed {
            continue;
        }
        let bare = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        if THEME_VARS
            .iter()
            .any(|var| bare.starts_with(var) && bare.as_bytes().get(var.len()) == Some(&b'='))
        {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// One asset row in the overview, with its activation state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AssetRow {
    pub asset: SourceAsset,
    pub enabled: bool,
}

/// Per-source section of `niu plugin list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceReport {
    pub status: SourceStatus,
    /// Assets with activation state (empty for degraded/untrusted rows).
    pub assets: Vec<AssetRow>,
    /// True when the managed rc block (or bash-it enabled/ entries) is in
    /// place, i.e. the source participates in startup.
    pub activated: bool,
}

/// Result of an enable/disable call: what happened and the exact undo.
#[derive(Debug, Clone)]
pub struct ActivationOutcome {
    pub summary: String,
    pub undo: String,
}

// ── Managed rc block ─────────────────────────────────────────────────────────

/// Parsed activation state inside one managed block.
#[derive(Debug, Clone, Default)]
struct BlockState {
    theme: Option<String>,
    arrays: BTreeSet<&'static str>,
    array_items: Vec<(String, Vec<String>)>,
    /// DirectFiles: tree-relative paths of enabled files.
    files: Vec<String>,
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'\''"#))
}

/// Parse a block body back into state. Only understands the lines this
/// module writes (single-quoted words); anything unrecognized is ignored
/// and will be re-rendered canonically on the next write.
fn parse_block(body: &str, model: &SelectionModel, record_id: &str) -> BlockState {
    const NO_ARRAYS: &[(&str, SourceAssetKind)] = &[];
    let (theme_var, arrays) = match model {
        SelectionModel::LoaderArrays { arrays, theme_var } => (Some(*theme_var), *arrays),
        SelectionModel::EnabledDir { theme_var } => (Some(*theme_var), NO_ARRAYS),
        SelectionModel::WholeSource | SelectionModel::DirectFiles => (None, NO_ARRAYS),
    };
    let mut state = BlockState::default();
    if let SelectionModel::DirectFiles = model {
        let prefix =
            format!(". \"${{NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}}/{record_id}/");
        for raw in body.lines() {
            let line = raw.trim();
            if let Some(rest) = line.strip_prefix(&prefix) {
                if let Some(relative) = rest.strip_suffix('"') {
                    if !relative.is_empty() && !state.files.iter().any(|f| f == relative) {
                        state.files.push(relative.to_string());
                    }
                }
            }
        }
        return state;
    }
    for raw in body.lines() {
        let line = raw.trim();
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if let Some(inner) = value
            .strip_prefix('(')
            .and_then(|v| v.strip_suffix(')'))
            .map(str::trim)
        {
            // Array assignment: plugins=('a' 'b')
            let items = split_quoted_words(inner);
            if let Some((var, _)) = arrays.iter().find(|(var, _)| *var == name) {
                match state
                    .array_items
                    .iter_mut()
                    .find(|(existing, _)| existing == var)
                {
                    Some((_, list)) => list.extend(items),
                    None => {
                        state.arrays.insert(var);
                        state.array_items.push((var.to_string(), items));
                    }
                }
            }
        } else if Some(name) == theme_var {
            // Theme assignment: OSH_THEME='name'
            let trimmed = value.trim_matches('\'').trim_matches('"');
            state.theme = (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
    }
    state
}

/// Split `'a' 'b' 'c'` into words (the exact form this module writes,
/// including `'\''`-escaped quotes inside a word).
fn split_quoted_words(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        // Skip separators between words.
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }
        if chars[index] != '\'' {
            // Not a form we write (hand-edited); skip the token entirely.
            while index < chars.len() && !chars[index].is_whitespace() {
                index += 1;
            }
            continue;
        }
        index += 1; // opening quote
        let mut word = String::new();
        let mut closed = false;
        while index < chars.len() {
            let ch = chars[index];
            if ch == '\'' {
                // `'\''` keeps a literal quote inside the word.
                if index + 2 < chars.len()
                    && chars[index + 1] == '\\'
                    && chars[index + 2] == '\''
                    && index + 3 < chars.len()
                    && chars[index + 3] == '\''
                {
                    word.push('\'');
                    index += 4;
                    continue;
                }
                index += 1; // closing quote
                closed = true;
                break;
            }
            word.push(ch);
            index += 1;
        }
        if closed {
            words.push(word);
        } else {
            break; // unterminated word (hand-edited); stop parsing
        }
    }
    words
}

/// Render a managed block: theme line, selection arrays (or per-file
/// guarded source lines for DirectFiles), then the adapter's guarded
/// loader snippet, wrapped in markers.
fn render_block(record: &SourceRecord, model: &SelectionModel, state: &BlockState) -> String {
    let adapter = adapter_for(&record.adapter).expect("adapter for a rendered block");
    let mut body = String::new();
    let theme_var = match model {
        SelectionModel::LoaderArrays { theme_var, .. } => Some(*theme_var),
        SelectionModel::EnabledDir { theme_var } => Some(*theme_var),
        SelectionModel::WholeSource | SelectionModel::DirectFiles => None,
    };
    if let (Some(var), Some(theme)) = (theme_var, &state.theme) {
        body.push_str(&format!("{}={}\n", var, shell_quote(theme)));
    }
    if let SelectionModel::LoaderArrays { arrays, .. } = model {
        for (var, _) in arrays.iter() {
            if let Some((_, items)) = state
                .array_items
                .iter()
                .find(|(existing, _)| existing == var)
            {
                if !items.is_empty() {
                    let quoted: Vec<String> = items.iter().map(|i| shell_quote(i)).collect();
                    body.push_str(&format!("{}=({})\n", var, quoted.join(" ")));
                }
            }
        }
    }
    if let SelectionModel::DirectFiles = model {
        let base = format!(
            "${{NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}}/{}",
            record.id
        );
        let mut files = state.files.clone();
        files.sort();
        files.dedup();
        for file in files {
            let path = format!("{base}/{file}");
            body.push_str(&format!("if [ -r \"{path}\" ]; then\n  . \"{path}\"\nfi\n"));
        }
    }
    let loader = adapter.loader_snippet(record);
    if !loader.is_empty() {
        body.push_str(&loader);
    }
    format!(
        "{}\n{}{}\n",
        begin_marker(&record.id),
        body,
        end_marker(&record.id)
    )
}

/// Every inclusive `(start, stop)` line span of a managed marker pair, in
/// file order (wt83 #175: every block operation scans ALL pairs — the
/// first-pair-only scan is what let a duplicate block keep loading after a
/// "successful" disable). A begin marker with no end marker is an error,
/// never a silent skip: the malformed block keeps executing at every
/// startup, so the caller must see the failure.
fn managed_block_spans(
    lines: &[String],
    begin: &str,
    end: &str,
) -> anyhow::Result<Vec<(usize, usize)>> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].trim() == begin {
            let stop = lines[index + 1..]
                .iter()
                .position(|line| line.trim() == end)
                .map(|offset| index + 1 + offset)
                .ok_or_else(|| {
                    anyhow!("rc block starting at '{begin}' has no end marker '{end}'")
                })?;
            spans.push((index, stop));
            index = stop + 1;
        } else {
            index += 1;
        }
    }
    Ok(spans)
}

fn rc_block_lines() -> anyhow::Result<Vec<String>> {
    let text = fs::read_to_string(rc_file())?;
    Ok(text.lines().map(str::to_string).collect())
}

/// Read one managed block *including* markers (for change detection).
fn managed_block_text(id: &str) -> Option<String> {
    let lines = rc_block_lines().ok()?;
    let begin = begin_marker(id);
    let end = end_marker(id);
    let (start, stop) = *managed_block_spans(&lines, &begin, &end).ok()?.first()?;
    let block: Vec<&str> = lines[start..=stop].iter().map(String::as_str).collect();
    Some(block.join("\n") + "\n")
}

/// All managed block bodies for an id (markers excluded), in file order —
/// the text bash actually executes when duplicate blocks exist (wt83 #175).
/// Read errors and a begin-without-end marker degrade to what was found,
/// the same tolerance the reads always had; writers never degrade.
fn read_managed_block_bodies(id: &str) -> Vec<String> {
    let Ok(lines) = rc_block_lines() else {
        return Vec::new();
    };
    let begin = begin_marker(id);
    let end = end_marker(id);
    managed_block_spans(&lines, &begin, &end)
        .map(|spans| {
            spans
                .iter()
                .map(|(start, stop)| lines[start + 1..*stop].join("\n"))
                .collect()
        })
        .unwrap_or_default()
}

/// Number of managed marker pairs for an id; errors on a begin marker with
/// no end marker (the malformed state must gate the write path).
fn managed_block_pair_count(id: &str) -> anyhow::Result<usize> {
    let lines = match rc_block_lines() {
        Ok(lines) => lines,
        Err(_) => return Ok(0),
    };
    Ok(managed_block_spans(&lines, &begin_marker(id), &end_marker(id))?.len())
}

/// The merged activation state across ALL of an id's managed blocks — what
/// a duplicate-block rc contributes beyond its first block. Arrays
/// accumulate and the last theme wins (the union is a superset, so a
/// duplicate repair never silently drops a hand line it could have kept).
fn merged_block_state(bodies: &[String], model: &SelectionModel, record_id: &str) -> BlockState {
    let mut merged = BlockState::default();
    for body in bodies {
        let state = parse_block(body, model, record_id);
        if state.theme.is_some() {
            merged.theme = state.theme;
        }
        for (var, items) in state.array_items {
            match merged
                .array_items
                .iter_mut()
                .find(|(existing, _)| *existing == var)
            {
                Some((_, list)) => {
                    for item in items {
                        if !list.contains(&item) {
                            list.push(item);
                        }
                    }
                }
                None => merged.array_items.push((var, items)),
            }
        }
        merged.files.extend(state.files);
    }
    merged.files.sort();
    merged.files.dedup();
    merged
}

/// The parsed state of the rc's managed blocks for an id (all of them).
fn managed_state(id: &str, model: &SelectionModel, record_id: &str) -> BlockState {
    let bodies = read_managed_block_bodies(id);
    merged_block_state(&bodies, model, record_id)
}

/// The rc's line ending: a CRLF rc stays CRLF (niubash#176 — the whole-file
/// rewrite used to flatten every line to LF, corrupting a mixed/rc-editors'
/// file). `str::lines()` strips the `\r`, so rejoining with the detected
/// terminator restores the file's own convention.
fn detect_eol(text: &str) -> &'static str {
    if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Backups live where the setup wizard already puts them, so one retention
/// policy covers both writers.
fn rc_backup_dir() -> Option<PathBuf> {
    Some(shell_home_dir()?.join(".niubash").join("backups"))
}

const RC_BACKUP_KEEP: usize = 10;

/// Back the rc up before a plugin verb rewrites it (niubash#176: every verb
/// that writes the rc backs up, not only the setup wizard) and prune the
/// backup set to the most recent [`RC_BACKUP_KEEP`] files — the wizard's
/// era never cleaned them, so `~/.niubash/backups` grew without bound.
fn backup_rc() {
    let Some(dir) = rc_backup_dir() else {
        return;
    };
    let Ok(raw) = fs::read(rc_file()) else {
        return; // nothing on disk yet; the first write creates the rc
    };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let _ = fs::write(dir.join(format!(".niubashrc.{nanos}.bak")), raw);
    prune_rc_backups(RC_BACKUP_KEEP);
}

/// Keep the newest `keep` `.niubashrc.*.bak` files (mtime, name as the
/// tiebreak); older ones are deleted.
fn prune_rc_backups(keep: usize) {
    let Some(dir) = rc_backup_dir() else {
        return;
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let mut backups: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".niubashrc.") && name.ends_with(".bak"))
        })
        .map(|path| {
            let modified = fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            (modified, path)
        })
        .collect();
    if backups.len() <= keep {
        return;
    }
    backups.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_, path) in backups.into_iter().skip(keep) {
        let _ = fs::remove_file(path);
    }
}

/// The id token of a marker-ish comment line (`# >>> niu source <id> …`):
/// the whitespace word right after `niu source`, whatever the bracket
/// decorations around it look like.
fn marker_id(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }
    let index = trimmed.find("niu source")? + "niu source".len();
    trimmed[index..]
        .split_whitespace()
        .next()
        .filter(|token| !token.is_empty())
}

/// A begin-marker-shaped comment whose text drifted from the exact managed
/// form (a hand edit, an older wording): `#` comment, `>>` decoration, the
/// right id — but not the exact marker (exact pairs are the normal path).
fn is_near_begin_marker(line: &str, id: &str, begin: &str) -> bool {
    let trimmed = line.trim();
    trimmed != begin
        && trimmed.starts_with('#')
        && trimmed
            .trim_start_matches('#')
            .trim_start()
            .starts_with(">>")
        && marker_id(trimmed) == Some(id)
}

/// Same for end markers (`<<<` decoration).
fn is_near_end_marker(line: &str, id: &str, end: &str) -> bool {
    let trimmed = line.trim();
    trimmed != end
        && trimmed.starts_with('#')
        && trimmed
            .trim_start_matches('#')
            .trim_start()
            .starts_with("<<<")
        && marker_id(trimmed) == Some(id)
}

/// Marker pairs whose text is *near* the managed form (niubash#176): a
/// begin marker missing its `>>>` tail or a stray indentation-aware edit.
/// Bash still sees the block's loader line (comments and all), so the stale
/// block EXECUTES — and a fresh exact block appended beside it made a
/// second, unmanaged, same-id activation. Near pairs are therefore spans
/// like any other (the write path repairs them); a near begin with no near
/// end is an error, the same policy as an exact begin with no end.
fn near_marker_spans(
    lines: &[String],
    id: &str,
    begin: &str,
    end: &str,
) -> anyhow::Result<Vec<(usize, usize)>> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if is_near_begin_marker(&lines[index], id, begin) {
            let stop = lines[index + 1..]
                .iter()
                .position(|line| line.trim() == end || is_near_end_marker(line, id, end))
                .map(|offset| index + 1 + offset)
                .ok_or_else(|| {
                    anyhow!(
                        "a hand-edited managed marker for '{id}' has no end marker — fix it to \
                         '{begin}' / '{end}' or remove the stale block"
                    )
                })?;
            spans.push((index, stop));
            index = stop + 1;
        } else {
            index += 1;
        }
    }
    Ok(spans)
}

/// The record (and its adapter's selection model) a marker pair belongs to,
/// for the managed-line classification below.
fn record_for_marker(
    marker: &str,
    registry: &[SourceRecord],
) -> Option<(SourceRecord, SelectionModel)> {
    let id = marker_id(marker)?;
    let record = registry.iter().find(|record| record.id == id)?;
    let model = adapter_for(&record.adapter)?.selection_model();
    Some((record.clone(), model))
}

/// Lines inside existing managed block(s) that the canonical rendering does
/// not carry AND that are not managed syntax: comments and blanks are
/// skipped, theme/array/loader lines are niu's to add or drop with the
/// spec (a dropped `plugins=('git')` line must NOT migrate out — the spec
/// dropped it), and anything else is a USER line that the naive replace
/// would silently drop (niubash#176).
fn foreign_body_lines(
    lines: &[String],
    spans: &[(usize, usize)],
    new_block: &str,
    managed: Option<&(SourceRecord, SelectionModel)>,
) -> Vec<String> {
    let canonical: std::collections::BTreeSet<String> = new_block
        .lines()
        .map(|line| line.trim().to_string())
        .collect();
    let (loader_lines, managed_prefixes): (std::collections::BTreeSet<String>, Vec<String>) =
        match managed {
            Some((record, model)) => {
                let adapter = adapter_for(&record.adapter);
                let loader = adapter
                    .map(|adapter| adapter.loader_snippet(record))
                    .unwrap_or_default();
                let loader_lines: std::collections::BTreeSet<String> =
                    loader.lines().map(|line| line.trim().to_string()).collect();
                let theme_var = match model {
                    SelectionModel::LoaderArrays { theme_var, .. }
                    | SelectionModel::EnabledDir { theme_var } => Some(*theme_var),
                    SelectionModel::WholeSource | SelectionModel::DirectFiles => None,
                };
                let mut prefixes = Vec::new();
                if let Some(var) = theme_var {
                    prefixes.push(format!("{var}="));
                }
                if let SelectionModel::LoaderArrays { arrays, .. } = model {
                    for (var, _) in arrays.iter() {
                        prefixes.push(format!("{var}="));
                    }
                }
                (loader_lines, prefixes)
            }
            None => (std::collections::BTreeSet::new(), Vec::new()),
        };
    let mut out = Vec::new();
    for (start, stop) in spans {
        for line in &lines[start + 1..*stop] {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if canonical.contains(trimmed) || loader_lines.contains(trimmed) {
                continue;
            }
            if managed_prefixes
                .iter()
                .any(|prefix| trimmed.starts_with(prefix.as_str()))
            {
                continue;
            }
            if let Some((_, SelectionModel::DirectFiles)) = managed {
                // The guarded per-file source lines are rendered structure:
                // `if [ -r "…" ]; then` / `  . "…"` / `fi`.
                if trimmed == "fi"
                    || trimmed.starts_with(". \"")
                    || trimmed.starts_with("if [ -r \"")
                {
                    continue;
                }
            }
            out.push(line.clone());
        }
    }
    out
}

const MIGRATED_USER_LINES_HEADER: &str =
    "# user lines moved out of a managed block by `niu plugin` (niu #176) — review and keep";

/// Insert or replace a managed block in the rc file, creating the rc when
/// absent. Returns the rc path (for the outcome message) plus how many
/// existing marker pairs (exact or near-marker, see
/// [`near_marker_spans`]) were consumed: 0 = fresh append, 1 = in-place
/// replace, >1 = duplicate blocks collapsed into the one canonical block
/// (wt83 #175 — duplicates are an error state, and write repairs them).
///
/// niubash#176 guards on every rewrite: the rc is backed up first
/// ([`backup_rc`]), the file's own line endings are preserved, user lines
/// found inside the replaced block(s) are migrated out (with a stderr
/// warning) instead of silently dropped, and hand-edited near-marker blocks
/// are repaired rather than duplicated.
fn write_managed_block(begin: &str, end: &str, block: &str) -> anyhow::Result<(PathBuf, usize)> {
    let path = rc_file();
    let raw = fs::read_to_string(&path).ok();
    let eol = raw.as_deref().map(detect_eol).unwrap_or("\n");
    let mut lines: Vec<String> = raw
        .as_deref()
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_else(|| {
            vec!["# Niubash interactive rc - edited by you and `niu plugin`.".to_string()]
        });
    let block_lines: Vec<String> = block.lines().map(str::to_string).collect();
    let id = marker_id(begin).unwrap_or_default().to_string();
    let mut spans = managed_block_spans(&lines, begin, end)?;
    let near = near_marker_spans(&lines, &id, begin, end)?;
    let mut repaired_near = false;
    for span in near {
        if !spans.contains(&span) {
            spans.push(span);
            repaired_near = true;
        }
    }
    spans.sort_unstable();
    if repaired_near {
        eprintln!(
            "warning: repaired a hand-edited managed marker block for '{id}' (its marker text \
             was not the exact managed form)"
        );
    }
    let managed = record_for_marker(begin, &read_source_registry());
    let foreign = foreign_body_lines(&lines, &spans, block, managed.as_ref());
    let consumed = spans.len();
    let mut insert_after_block = lines.len();
    match spans.as_slice() {
        [] => {
            if !lines.is_empty() && !lines.last().is_some_and(|line| line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.extend(block_lines);
        }
        [(start, stop)] => {
            insert_after_block = start + block_lines.len();
            lines.splice(*start..=*stop, block_lines);
        }
        many => {
            // Repair: drop every pair after the first (descending, so the
            // first span's indices hold), then splice the canonical block
            // into the first span.
            for (start, stop) in many.iter().skip(1).rev() {
                lines.drain(*start..=*stop);
            }
            let (first_start, first_stop) = many[0];
            insert_after_block = first_start + block_lines.len();
            lines.splice(first_start..=first_stop, block_lines);
        }
    }
    if !foreign.is_empty() {
        eprintln!(
            "warning: moved {} user line(s) out of the managed '{id}' block — they are now \
             right below it; review and keep what you need",
            foreign.len()
        );
        let mut migrated = vec![MIGRATED_USER_LINES_HEADER.to_string()];
        migrated.extend(foreign);
        let at = insert_after_block.min(lines.len());
        lines.splice(at..at, migrated);
    }
    let mut text = lines.join(eol);
    text.push_str(eol);
    if Some(text.as_str()) != raw.as_deref() {
        backup_rc();
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, text)?;
    Ok((path, consumed))
}

/// Remove EVERY managed marker pair (exact or near-marker, see
/// [`near_marker_spans`]) for (begin, end); returns how many pairs were
/// removed (wt83 #175 — disable must leave zero residue, not drop the
/// first block and strand the duplicates). User lines inside the removed
/// block(s) are migrated out with a warning, never silently dropped
/// (niubash#176); the rc is backed up before the rewrite.
fn remove_managed_block(begin: &str, end: &str) -> anyhow::Result<usize> {
    let path = rc_file();
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(0);
    };
    let eol = detect_eol(&text);
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let id = marker_id(begin).unwrap_or_default().to_string();
    let mut spans = managed_block_spans(&lines, begin, end)?;
    for span in near_marker_spans(&lines, &id, begin, end)? {
        if !spans.contains(&span) {
            spans.push(span);
        }
    }
    spans.sort_unstable();
    if spans.is_empty() {
        return Ok(0);
    }
    // Recognized managed lines for the removal path: the canonical
    // rendering of the block's own parsed state, plus managed-syntax
    // classification. Without it, every body line (loader lines included)
    // would look "foreign" — removal must migrate USER lines only, and an
    // unresolvable record conservatively migrates nothing.
    let registry = read_source_registry();
    let managed = record_for_marker(begin, &registry);
    let foreign = match &managed {
        Some((record, model)) => {
            let rendered = adapter_for(&record.adapter).map(|_adapter| {
                let state = managed_state(&record.id, model, &record.id);
                render_block(record, model, &state)
            });
            match &rendered {
                Some(rendered) => {
                    foreign_body_lines(&lines, &spans, rendered, Some(managed.as_ref().unwrap()))
                }
                None => Vec::new(),
            }
        }
        None => Vec::new(),
    };
    let first_start = spans[0].0;
    let mut rewritten: Vec<String> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if spans
            .iter()
            .any(|(start, stop)| index >= *start && index <= *stop)
        {
            continue;
        }
        if index == first_start && !foreign.is_empty() {
            eprintln!(
                "warning: moved {} user line(s) out of the managed '{id}' block being removed — \
                 they are kept above this spot; review and delete what you no longer need",
                foreign.len()
            );
            rewritten.push(MIGRATED_USER_LINES_HEADER.to_string());
            rewritten.extend(foreign.iter().cloned());
        }
        rewritten.push(line.clone());
    }
    let mut joined = rewritten.join(eol);
    joined.push_str(eol);
    backup_rc();
    fs::write(&path, joined)?;
    Ok(spans.len())
}

/// Build the managed activation block for a source with a theme selected
/// (used by the setup wizard and by `niu plugin enable <theme>`).
pub fn build_theme_block(source_id: &str, theme: &str) -> Option<String> {
    let record = read_source_registry()
        .into_iter()
        .find(|record| record.id == source_id)?;
    let adapter = adapter_for(&record.adapter)?;
    let model = adapter.selection_model();
    let state = BlockState {
        theme: Some(theme.to_string()),
        arrays: BTreeSet::new(),
        array_items: Vec::new(),
        files: Vec::new(),
    };
    Some(render_block(&record, &model, &state))
}

// ── bash-it enabled/ directory (tree-side state) ─────────────────────────────

const BASH_IT_LOAD_PRIORITY_SEPARATOR: &str = "---";
const DEFAULT_LOAD_PRIORITY: u32 = 500;

/// Load priority declared by a bash-it component header
/// (`# BASH_IT_LOAD_PRIORITY: 350`), defaulting to bash-it's own 500.
fn load_priority(file: &Path) -> u32 {
    fs::read_to_string(file)
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let rest = line.trim().strip_prefix("# BASH_IT_LOAD_PRIORITY:")?;
                rest.trim().parse::<u32>().ok()
            })
        })
        .unwrap_or(DEFAULT_LOAD_PRIORITY)
}

fn enabled_entries(record: &SourceRecord, asset: &SourceAsset) -> Vec<PathBuf> {
    let enabled_dir = record.path.join("enabled");
    let file_name = asset
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let suffix = format!("{BASH_IT_LOAD_PRIORITY_SEPARATOR}{file_name}");
    let Ok(entries) = fs::read_dir(&enabled_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&suffix))
        })
        .collect()
}

fn tree_enable(record: &SourceRecord, asset: &SourceAsset) -> anyhow::Result<()> {
    let enabled_dir = record.path.join("enabled");
    fs::create_dir_all(&enabled_dir)?;
    if !enabled_entries(record, asset).is_empty() {
        return Ok(()); // already enabled
    }
    let file_name = asset
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("asset path has no file name"))?;
    let target = enabled_dir.join(format!(
        "{}{}{}",
        load_priority(&asset.path),
        BASH_IT_LOAD_PRIORITY_SEPARATOR,
        file_name
    ));
    // Copy (not symlink): Windows symlinks need privileges, and the
    // reloader only globs `enabled/*.bash` either way.
    fs::copy(&asset.path, &target)
        .with_context(|| format!("failed to enable {}", asset.path.display()))?;
    Ok(())
}

fn tree_disable(record: &SourceRecord, asset: &SourceAsset) -> anyhow::Result<()> {
    for entry in enabled_entries(record, asset) {
        fs::remove_file(&entry)
            .with_context(|| format!("failed to disable {}", entry.display()))?;
    }
    Ok(())
}

/// Fully deactivate a source without touching the rest of its tree: the
/// managed rc block goes (ALL matching blocks — wt83 #175), and for
/// `enabled/` managers (bash-it) the tree entries go too. Used when the
/// spec stops declaring a source it owned (§14.6.3: spec is the truth; the
/// tree itself is only ever removed by `niu plugin source remove`).
/// Errors surface (a malformed block, an unremovable entry): a disable
/// that could not touch the rc must not print success (wt83 #175).
pub fn deactivate_block(record: &SourceRecord) -> anyhow::Result<()> {
    remove_managed_block(&begin_marker(&record.id), &end_marker(&record.id))?;
    if let Some(adapter) = adapter_for(&record.adapter) {
        if let SelectionModel::EnabledDir { .. } = adapter.selection_model() {
            for asset in adapter.list_assets(&record.path) {
                tree_disable(record, &asset)?;
            }
        }
    }
    Ok(())
}

// ── Overview ─────────────────────────────────────────────────────────────────

/// Compute `niu plugin list` rows: every registered source with its assets
/// and activation state (untrusted/degraded sources list with state only).
pub fn asset_overview() -> Vec<SourceReport> {
    let mut out = Vec::new();
    for status in list_sources() {
        let Some(adapter) = adapter_for(&status.record.adapter) else {
            out.push(SourceReport {
                status,
                assets: Vec::new(),
                activated: false,
            });
            continue;
        };
        if status.degraded || !status.record.trusted {
            out.push(SourceReport {
                status,
                assets: Vec::new(),
                activated: false,
            });
            continue;
        }
        let record = &status.record;
        let model = adapter.selection_model();
        let bodies = read_managed_block_bodies(&record.id);
        let state = merged_block_state(&bodies, &model, &record.id);
        let assets = adapter
            .list_assets(&record.path)
            .into_iter()
            .map(|asset| AssetRow {
                enabled: asset_enabled(&model, record, &state, &asset),
                asset,
            })
            .collect();
        out.push(SourceReport {
            status,
            assets,
            activated: !bodies.is_empty(),
        });
    }
    out
}

fn asset_enabled(
    model: &SelectionModel,
    record: &SourceRecord,
    state: &BlockState,
    asset: &SourceAsset,
) -> bool {
    match (model, asset.kind) {
        (SelectionModel::WholeSource, _) => false, // informational; whole-source activation
        (SelectionModel::DirectFiles, _) => state.files.iter().any(|f| f == &asset.name),
        (_, SourceAssetKind::Theme) => state.theme.as_deref() == Some(asset.name.as_str()),
        (SelectionModel::EnabledDir { .. }, _) => !enabled_entries(record, asset).is_empty(),
        (SelectionModel::LoaderArrays { arrays, .. }, kind) => {
            arrays.iter().any(|(var, array_kind)| {
                *array_kind == kind
                    && state
                        .array_items
                        .iter()
                        .find(|(existing, _)| existing == var)
                        .is_some_and(|(_, items)| items.iter().any(|item| item == &asset.name))
            })
        }
    }
}

// ── Enable / disable (spec sugar; §14.6.3) ───────────────────────────────────

/// Resolve an enable/disable target: a source id (`oh-my-bash`), a
/// qualified asset (`oh-my-bash/git`), or a bare asset name that must be
/// unambiguous across trusted sources.
enum Target {
    Source(SourceRecord),
    Asset {
        record: SourceRecord,
        asset: SourceAsset,
    },
}

fn resolve_target(name: &str) -> anyhow::Result<Target> {
    let records = read_source_registry();
    // Qualified form: <source-id>/<asset-name>. Asset names of file sources
    // are tree-relative paths, so only the FIRST '/' splits the pair.
    if let Some((source_id, asset_name)) = name.split_once('/') {
        if let Some(record) = records.iter().find(|record| record.id == source_id) {
            if let Some(asset) = find_asset(record, asset_name) {
                let target = Target::Asset {
                    record: record.clone(),
                    asset,
                };
                return check_gate(target, record);
            }
        }
    }
    // Source id.
    if let Some(record) = records.iter().find(|record| record.id == name) {
        return check_gate(Target::Source(record.clone()), record);
    }
    // Bare asset name across gated (trusted, non-degraded) sources.
    let mut matches = Vec::new();
    for record in &records {
        if let Some(asset) = find_asset(record, name) {
            matches.push((record.clone(), asset));
        }
    }
    match matches.len() {
        0 => {
            let installed: Vec<String> = records.iter().map(|r| r.id.clone()).collect();
            bail!(
                "no source or asset named '{name}'; installed sources: {}",
                if installed.is_empty() {
                    "(none — start with `niu plugin discover`)".to_string()
                } else {
                    installed.join(", ")
                }
            )
        }
        1 => check_gate(
            Target::Asset {
                record: matches[0].0.clone(),
                asset: matches[0].1.clone(),
            },
            &matches[0].0,
        ),
        _ => {
            let options: Vec<String> = matches
                .iter()
                .map(|(record, asset)| format!("{}/{}", record.id, asset.name))
                .collect();
            bail!(
                "'{name}' exists in multiple sources — pick one: {}",
                options.join(", ")
            )
        }
    }
}

fn find_asset(record: &SourceRecord, name: &str) -> Option<SourceAsset> {
    let adapter = adapter_for(&record.adapter)?;
    adapter
        .list_assets(&record.path)
        .into_iter()
        .find(|asset| asset.name == name)
}

/// The execution gate applies to activation too: untrusted or degraded
/// sources contribute nothing (§12.2/§11.4).
fn check_gate(target: Target, record: &SourceRecord) -> anyhow::Result<Target> {
    if !record.path.is_dir() {
        bail!(
            "source '{}' is degraded (tree missing); restore it first: `niu plugin restore {}`",
            record.id,
            record.id
        );
    }
    if !record.trusted {
        bail!(
            "source '{}' is installed but untrusted; review and trust it first: \
             `niu plugin trust {}`",
            record.id,
            record.id
        );
    }
    Ok(target)
}

/// The spec target a record is declared by (its recorded origin).
fn spec_target_for_record(record: &SourceRecord) -> String {
    record.url.clone()
}

/// Find or create the spec entry owning a record (matched by explicit id,
/// catalog id, or recorded origin).
fn upsert_spec_entry<'a>(spec: &'a mut PluginSpec, record: &SourceRecord) -> &'a mut SpecSource {
    let id = record.id.clone();
    if let Some(index) = spec.entry_index_for_id(&id) {
        return &mut spec.sources[index];
    }
    if let Some(index) = spec
        .sources
        .iter()
        .position(|source| source.target == record.url)
    {
        // Bind the derived id explicitly so future syncs match directly.
        spec.sources[index].id = Some(id);
        return &mut spec.sources[index];
    }
    spec.sources.push(SpecSource {
        target: spec_target_for_record(record),
        id: Some(id),
        kind: None,
        ref_name: None,
        theme: None,
        enable: Vec::new(),
    });
    let last = spec.sources.len() - 1;
    &mut spec.sources[last]
}

/// Names currently enabled for a record (its live selection: rc block
/// arrays / files, or bash-it enabled/ entries), used when adopting an
/// existing imperative install into the spec.
fn current_selection(record: &SourceRecord, model: &SelectionModel) -> Vec<String> {
    let Some(adapter) = adapter_for(&record.adapter) else {
        return Vec::new();
    };
    match model {
        SelectionModel::WholeSource => Vec::new(),
        SelectionModel::EnabledDir { .. } => adapter
            .list_assets(&record.path)
            .into_iter()
            .filter(|asset| !enabled_entries(record, asset).is_empty())
            .map(|asset| asset.name)
            .collect(),
        SelectionModel::LoaderArrays { .. } | SelectionModel::DirectFiles => {
            let state = managed_state(&record.id, model, &record.id);
            let mut names: Vec<String> = state
                .array_items
                .iter()
                .flat_map(|(_, items)| items.iter().cloned())
                .collect();
            names.extend(state.files);
            names
        }
    }
}

fn current_theme(record: &SourceRecord, model: &SelectionModel) -> Option<String> {
    managed_state(&record.id, model, &record.id)
        .theme
        .filter(|theme| !theme.is_empty())
}

/// The theme the source's managed rc block currently carries — the rc's
/// live state (None = no block, or no theme line). This is what the
/// theme-claim reconciliation (niubash#168) treats as the user's latest
/// expressed choice when the spec disagrees about theme ownership.
pub fn live_managed_theme(record: &SourceRecord) -> Option<String> {
    let adapter = adapter_for(&record.adapter)?;
    let model = adapter.selection_model();
    current_theme(record, &model).filter(|theme| !theme.is_empty())
}

/// The user's latest theme pick — a wizard/gallery pick or
/// `niu plugin enable <theme>` — recorded in the spec so the rc block and
/// the spec entry cannot disagree (niubash#168).
///
/// The pick claims `theme` for `source_id`'s entry (with its id pin, so an
/// ambiguous name is owned unambiguously) and MOVES the claim away from
/// every other entry: any other non-empty theme claim is cleared, and a
/// claimant left with no selection at all (it declared the theme and
/// nothing else) has its declaration removed — the next sync then drops
/// that source's activation block, so the losing framework never loads on
/// top of the picked theme. The prior shape (rc written, spec untouched)
/// let every sync re-materialize the stale claim and revert the pick.
pub fn claim_theme(source_id: &str, theme: &str) -> anyhow::Result<()> {
    if theme.is_empty() {
        bail!("an empty theme name cannot be claimed; use `niu plugin disable <theme>`");
    }
    let mut spec = spec::load_spec()?.unwrap_or_default();
    move_theme_claim(&mut spec, source_id, theme)?;
    spec::save_spec(&spec)?;
    Ok(())
}

/// The claim move itself, on an already-loaded spec: the shared half of
/// [`claim_theme`] and the theme branch of [`enable`].
fn move_theme_claim(spec: &mut PluginSpec, source_id: &str, theme: &str) -> anyhow::Result<()> {
    let record = read_source_registry()
        .into_iter()
        .find(|record| record.id == source_id)
        .ok_or_else(|| anyhow!("source '{source_id}' is not installed"))?;
    {
        let entry = upsert_spec_entry(spec, &record);
        entry.theme = Some(theme.to_string());
    }
    let picked_index = spec
        .entry_index_for_id(&record.id)
        .ok_or_else(|| anyhow!("spec entry for '{source_id}' vanished while claiming"))?;
    let mut dropped: Vec<usize> = Vec::new();
    for (index, other) in spec.sources.iter_mut().enumerate() {
        if index == picked_index {
            continue;
        }
        let was_claimed = other
            .theme
            .as_deref()
            .is_some_and(|existing| !existing.is_empty());
        if !was_claimed {
            continue;
        }
        other.theme = Some(String::new());
        // A claimant that declared the theme and nothing else existed only
        // to carry the claim that just moved: remove the declaration so its
        // activation follows (the sync's undeclared pass drops the block
        // and says so). Entries with their own selection stay declared —
        // only their theme line goes.
        if other.enable.is_empty() {
            dropped.push(index);
        }
    }
    for index in dropped.into_iter().rev() {
        spec.sources.remove(index);
    }
    Ok(())
}

/// The live activation snapshot of a record — (enabled asset names, active
/// theme) as the manager's own selection mechanism currently has it. This
/// is what spec adoption (`niu plugin sync --adopt`, the wizard's
/// collection apply, `niu plugin enable <source>`) snapshots into a spec
/// entry so the adopted spec round-trips: a plain sync afterwards
/// re-materializes exactly this state (byte-stable rc block, unchanged
/// registry `spec_enabled`/`spec_theme`).
pub fn live_selection(record: &SourceRecord) -> (Vec<String>, Option<String>) {
    let Some(adapter) = adapter_for(&record.adapter) else {
        return (Vec::new(), None);
    };
    let model = adapter.selection_model();
    (
        current_selection(record, &model),
        current_theme(record, &model).filter(|theme| !theme.is_empty()),
    )
}

/// `niu plugin enable <target>` — spec sugar (§14.6.3): declare the source
/// and/or asset in `~/.niubash/plugins.toml`, then sync, which materializes
/// the manager's own selection mechanism (rc arrays, enabled/ entries, or
/// guarded per-file source lines).
pub fn enable(name: &str) -> anyhow::Result<ActivationOutcome> {
    let target = resolve_target(name)?;
    let mut spec = spec::load_spec()?.unwrap_or_default();
    let (record, summary) = match target {
        Target::Source(record) => {
            let adapter = adapter_for(&record.adapter)
                .ok_or_else(|| anyhow!("unknown adapter '{}'", record.adapter))?;
            let model = adapter.selection_model();
            if let SelectionModel::DirectFiles = model {
                // Honest presentation (§14.6.1): no guessed "unique entry"
                // for wild file sources — list the candidates.
                let candidates: Vec<String> = adapter
                    .list_assets(&record.path)
                    .into_iter()
                    .map(|a| a.name)
                    .collect();
                bail!(
                    "source '{}' sources files individually — no guessed entry; pick one (`niu plugin list`): {}",
                    record.id,
                    candidates.join(", ")
                );
            }
            let entry = upsert_spec_entry(&mut spec, &record);
            if !matches!(model, SelectionModel::WholeSource) {
                // Adopt the live selection so the spec describes reality.
                let selection = current_selection(&record, &model);
                for item in selection {
                    if !entry.enable.contains(&item) {
                        entry.enable.push(item);
                    }
                }
                if entry.theme.is_none() {
                    entry.theme = current_theme(&record, &model);
                }
            }
            (
                record.clone(),
                format!(
                    "source '{}' declared in the spec and activated through its own loader",
                    record.id
                ),
            )
        }
        Target::Asset { record, asset } => {
            let adapter = adapter_for(&record.adapter)
                .ok_or_else(|| anyhow!("unknown adapter '{}'", record.adapter))?;
            let model = adapter.selection_model();
            if let SelectionModel::WholeSource = model {
                bail!(
                    "'{}' activates as a whole source; run `niu plugin enable {}` instead",
                    record.id,
                    record.id
                );
            }
            let entry = upsert_spec_entry(&mut spec, &record);
            let theme_pick = if asset.kind == SourceAssetKind::Theme {
                Some(asset.name.clone())
            } else {
                if !entry.enable.contains(&asset.name) {
                    entry.enable.push(asset.name.clone());
                }
                None
            };
            let summary = if let Some(theme) = &theme_pick {
                format!(
                    "theme '{}' ({}) declared in the spec; synced to the manager's theme variable",
                    theme, record.id
                )
            } else {
                format!(
                    "'{}' ({}) declared in the spec; synced through the manager's own selection",
                    asset.name, record.id
                )
            };
            // The pick MOVES the claim (niubash#168): the previous owner's
            // entry loses it, so no later sync re-materializes the old
            // framework's theme block over this choice.
            if let Some(theme) = &theme_pick {
                move_theme_claim(&mut spec, &record.id, theme)?;
            }
            (record.clone(), summary)
        }
    };
    spec::save_spec(&spec)?;
    let report = super::sync::sync_spec(super::sync::SyncOptions::default())?;
    // The rc materialization is the visible half of enable: a failed row
    // for this source (a malformed or unrepairable managed block) must
    // fail the verb — printing success while the rc was never written is
    // the fake success wt83 #175 records. A merged row (duplicate blocks
    // collapsed) is reported in the outcome so the repair is visible.
    let summary = finish_activation_summary(&record.id, summary, &report)?;
    Ok(ActivationOutcome {
        summary,
        undo: format!("niu plugin disable {name}"),
    })
}

/// Fold the reconciler's rows for `record_id` into an activation outcome:
/// a failed row turns into an error (the verb exits nonzero with the
/// reason); a merged row (duplicate managed blocks collapsed) is appended
/// to the summary text. Returns the (possibly annotated) summary.
fn finish_activation_summary(
    record_id: &str,
    summary: String,
    report: &super::sync::SyncReport,
) -> anyhow::Result<String> {
    let mut summary = summary;
    for row in &report.rows {
        if row.id != record_id {
            continue;
        }
        if row.action == "failed" {
            bail!("rc update for '{record_id}' failed: {}", row.detail);
        }
        if row.action == "merged" {
            summary.push_str(&format!("; {}", row.detail));
        }
    }
    Ok(summary)
}

/// `niu plugin disable <target>` — spec sugar: drop the asset (or the whole
/// declaration) from the spec and sync; the managed rc block follows.
pub fn disable(name: &str) -> anyhow::Result<ActivationOutcome> {
    let target = resolve_target(name)?;
    let mut spec = spec::load_spec()?.unwrap_or_default();
    let (record, removed_entry, summary) = match target {
        Target::Source(record) => {
            let index = spec.entry_index_for_id(&record.id);
            (
                record.clone(),
                index.is_some(),
                format!(
                    "source '{}' deactivated — spec entry removed, loader block dropped \
                     (tree kept; delete fully with `niu plugin source remove {}`)",
                    record.id, record.id
                ),
            )
        }
        Target::Asset { record, asset } => {
            // Edit (or create) the entry so the spec describes the new
            // state. Adopting the live selection first keeps everything the
            // user already had; a theme disable pins `theme = ''` — the
            // explicit "no theme" marker (absent would mean "unmanaged").
            let entry = upsert_spec_entry(&mut spec, &record);
            if asset.kind == SourceAssetKind::Theme {
                entry.theme = Some(String::new());
            } else {
                let model = adapter_for(&record.adapter)
                    .ok_or_else(|| anyhow!("unknown adapter '{}'", record.adapter))?
                    .selection_model();
                for item in current_selection(&record, &model) {
                    if !entry.enable.contains(&item) {
                        entry.enable.push(item);
                    }
                }
                entry.enable.retain(|item| item != &asset.name);
            }
            (
                record.clone(),
                false,
                format!(
                    "'{}' ({}) removed from the spec and the selection",
                    asset.name, record.id
                ),
            )
        }
    };
    if let Some(index) = spec.entry_index_for_id(&record.id) {
        if removed_entry {
            spec.sources.remove(index);
        }
    }
    spec::save_spec(&spec)?;
    let report = super::sync::sync_spec(super::sync::SyncOptions::default())?;
    // Same honesty contract as enable (wt83 #175): a disable whose rc
    // removal failed must fail loudly, not print "loader block dropped"
    // over a block that still loads at every startup.
    let summary = finish_activation_summary(&record.id, summary, &report)?;
    Ok(ActivationOutcome {
        summary,
        undo: format!("niu plugin enable {name}"),
    })
}

// ── Spec materialization (the sync engine's writer) ──────────────────────────

/// What one spec materialization pass did to a source.
#[derive(Debug, Clone)]
pub struct SpecMaterialization {
    /// `activated` | `unchanged` | `deactivated` | `unsupported`
    pub action: String,
    pub detail: String,
    /// New `spec_enabled` to persist (None = leave as-is).
    pub spec_enabled: Option<Vec<String>>,
    /// New `spec_theme` to persist.
    pub spec_theme: Option<String>,
}

/// Materialize one spec entry onto a trusted, non-degraded record
/// (§14.6.3 merge semantics): the spec's selection plus any hand-added
/// entries wins; entries the spec dropped (in `prev` but not `next`) are
/// removed; a hand-set theme survives a spec that does not declare one.
///
/// `theme_claimed_elsewhere` is the niubash#168 floor-yield input: true
/// when another spec entry claims a (non-empty) theme. A source with no
/// selection of its own then does NOT (re)create its loader-only floor
/// block — the framework would load on top of the claimed theme. A block
/// that already exists is left byte-stable (never silently flipped);
/// removal is an explicit verb or a reconciled claim move.
pub fn materialize_spec_selection(
    record: &SourceRecord,
    entry: &SpecSource,
    theme_claimed_elsewhere: bool,
) -> anyhow::Result<SpecMaterialization> {
    let Some(adapter) = adapter_for(&record.adapter) else {
        return Ok(SpecMaterialization {
            action: "unsupported".to_string(),
            detail: format!("unknown adapter '{}'", record.adapter),
            spec_enabled: None,
            spec_theme: None,
        });
    };
    let model = adapter.selection_model();
    let materialized = match model {
        SelectionModel::WholeSource => {
            // Declared = active; the guarded loader block is the unit.
            let rendered = render_block(record, &model, &BlockState::default());
            let (action, detail) =
                write_block_report(record, &rendered, "whole-source guarded loader")?;
            SpecMaterialization {
                action,
                detail,
                spec_enabled: Some(Vec::new()),
                spec_theme: None,
            }
        }
        SelectionModel::DirectFiles => {
            let assets = adapter.list_assets(&record.path);
            let known: Vec<&str> = assets.iter().map(|a| a.name.as_str()).collect();
            let (next, unknown) = filter_known(&entry.enable, &known);
            let current = current_selection(record, &model);
            let prev = record.spec_enabled.clone().unwrap_or_default();
            let hand_added = diff_keep_order(&current, &prev);
            let mut final_names = next.clone();
            for hand in hand_added {
                if !final_names.contains(&hand) {
                    final_names.push(hand);
                }
            }
            let state = BlockState {
                theme: None,
                arrays: BTreeSet::new(),
                array_items: Vec::new(),
                files: final_names.clone(),
            };
            let (action, detail) = apply_block(record, &model, &state, !final_names.is_empty())?;
            SpecMaterialization {
                action,
                detail: detail_with_unknown(detail, &unknown),
                // Persist the SPEC-OWNED selection only (`next`), not the
                // union with hand-added entries: `prev` must stay "what the
                // spec last materialized" so hand additions remain
                // hand-added on every later sync. Absorbing them into prev
                // would let the next spec-side removal kill them one sync
                // after the user typed them.
                spec_enabled: Some(next),
                spec_theme: None,
            }
        }
        SelectionModel::LoaderArrays { .. } | SelectionModel::EnabledDir { .. } => {
            let assets = adapter.list_assets(&record.path);
            let known: Vec<&str> = assets.iter().map(|a| a.name.as_str()).collect();
            let (next, unknown) = filter_known(&entry.enable, &known);
            let current = current_selection(record, &model);
            let prev = record.spec_enabled.clone().unwrap_or_default();
            let hand_added = diff_keep_order(&current, &prev);
            let mut final_names = next.clone();
            for hand in hand_added {
                if !final_names.contains(&hand) {
                    final_names.push(hand);
                }
            }
            // Theme: the spec's pick wins (`theme = ''` explicitly clears
            // it); a spec that declares none keeps the current one (manual
            // choice preserved).
            let theme = match entry.theme.as_deref() {
                Some("") => None,
                Some(theme) => Some(theme.to_string()),
                None => current_theme(record, &model),
            };
            let (action, detail) = match model {
                SelectionModel::EnabledDir { .. } => {
                    // Reconcile the tree-side enabled/ entries first.
                    let mut tree_changed = false;
                    for asset in &assets {
                        let wanted = final_names.contains(&asset.name);
                        let is_on = !enabled_entries(record, asset).is_empty();
                        if wanted && !is_on {
                            tree_enable(record, asset)?;
                            tree_changed = true;
                        } else if !wanted && is_on {
                            tree_disable(record, asset)?;
                            tree_changed = true;
                        }
                    }
                    let state = BlockState {
                        theme: theme.clone(),
                        arrays: BTreeSet::new(),
                        array_items: Vec::new(),
                        files: Vec::new(),
                    };
                    // Loader fidelity (defaults-as-floor §14.5/§14.6): a
                    // DECLARED loader-manager source is active — OMB with no
                    // OSH_THEME renders its own default theme, bash-it with an
                    // empty enabled/ set still sources its framework. Disable
                    // removes the declaration (and with it the block).
                    // EXCEPT (niubash#168): the floor yields to a claimed
                    // theme — a selection-less source never wins over a
                    // theme claimed by another declared source; a block
                    // already present stays (never silently flipped).
                    let has_selection = !final_names.is_empty() || theme.is_some();
                    let active = has_selection
                        || !theme_claimed_elsewhere
                        || managed_block_text(&record.id).is_some();
                    let (block_action, detail) = apply_block(record, &model, &state, active)?;
                    let action = if tree_changed
                        || block_action == "activated"
                        || block_action == "deactivated"
                    {
                        if final_names.is_empty() && theme.is_none() {
                            "deactivated".to_string()
                        } else {
                            "activated".to_string()
                        }
                    } else {
                        block_action
                    };
                    (action, detail)
                }
                _ => {
                    // Start from the live parsed block so hand-added
                    // entries (even ones niu cannot enumerate — the
                    // manager may still know them) keep their exact
                    // placement; then apply the spec's delta: drop what the
                    // spec dropped, add what it declares. ALL of the id's
                    // managed blocks seed the state (wt83 #175): with
                    // duplicates present, bash executes every block, so
                    // the union — not the first block — is what the user
                    // actually runs.
                    let mut state = managed_state(&record.id, &model, &record.id);
                    let dropped: Vec<String> = prev
                        .iter()
                        .filter(|name| !next.contains(name))
                        .cloned()
                        .collect();
                    for (_, items) in state.array_items.iter_mut() {
                        items.retain(|item| !dropped.contains(item));
                    }
                    if let SelectionModel::LoaderArrays { arrays, .. } = model {
                        for name in &next {
                            let already = state
                                .array_items
                                .iter()
                                .any(|(_, items)| items.contains(name));
                            if already {
                                continue;
                            }
                            let kind = assets
                                .iter()
                                .find(|asset| &asset.name == name)
                                .map(|asset| asset.kind);
                            let var = arrays.iter().find_map(|(var, array_kind)| {
                                (Some(*array_kind) == kind).then_some(*var)
                            });
                            if let Some(var) = var {
                                match state
                                    .array_items
                                    .iter_mut()
                                    .find(|(existing, _)| existing == var)
                                {
                                    Some((_, items)) => items.push(name.clone()),
                                    None => {
                                        state.arrays.insert(var);
                                        state
                                            .array_items
                                            .push((var.to_string(), vec![name.clone()]));
                                    }
                                }
                            }
                        }
                    }
                    state.theme = theme.clone();
                    // Same loader-fidelity invariant as the EnabledDir arm:
                    // declaration activates; empty arrays + no theme let the
                    // manager apply its own defaults (OMB default theme).
                    // And the same niubash#168 floor-yield: a selection-less
                    // source does not resurrect its framework over a theme
                    // claimed elsewhere; a present block stays.
                    let has_selection = theme.is_some()
                        || state.array_items.iter().any(|(_, items)| !items.is_empty());
                    let active = has_selection
                        || !theme_claimed_elsewhere
                        || managed_block_text(&record.id).is_some();
                    apply_block(record, &model, &state, active)?
                }
            };
            SpecMaterialization {
                action,
                detail: detail_with_unknown(detail, &unknown),
                // `prev` = the spec-owned selection only (see the
                // DirectFiles branch): hand-added entries must stay
                // hand-added across syncs, not become spec-removable one
                // sync after they were written.
                spec_enabled: Some(next),
                spec_theme: entry.theme.clone().filter(|theme| !theme.is_empty()),
            }
        }
    };
    Ok(materialized)
}

/// Filter a name list against the known asset names; returns (known, unknown).
fn filter_known(names: &[String], known: &[&str]) -> (Vec<String>, Vec<String>) {
    let mut kept = Vec::new();
    let mut unknown = Vec::new();
    for name in names {
        if known.contains(&name.as_str()) {
            kept.push(name.clone());
        } else {
            unknown.push(name.clone());
        }
    }
    (kept, unknown)
}

/// `source − remove`, keeping source order.
fn diff_keep_order(source: &[String], remove: &[String]) -> Vec<String> {
    source
        .iter()
        .filter(|item| !remove.contains(item))
        .cloned()
        .collect()
}

fn detail_with_unknown(detail: String, unknown: &[String]) -> String {
    if unknown.is_empty() {
        detail
    } else {
        format!(
            "{detail}; unknown spec asset(s) skipped: {}",
            unknown.join(", ")
        )
    }
}

/// Write (or drop) the managed block for a state; reports what happened.
/// `active` says whether anything is activated at all (selection non-empty
/// or a theme set) — a loader-only block still counts as active for
/// models whose selection lives outside the rc (bash-it enabled/ entries).
fn apply_block(
    record: &SourceRecord,
    model: &SelectionModel,
    state: &BlockState,
    active: bool,
) -> anyhow::Result<(String, String)> {
    if !active {
        let removed = remove_managed_block(&begin_marker(&record.id), &end_marker(&record.id))?;
        return Ok((
            if removed > 0 {
                "deactivated"
            } else {
                "unchanged"
            }
            .to_string(),
            "nothing enabled — managed block dropped".to_string(),
        ));
    }
    let rendered = render_block(record, model, state);
    write_block_report(
        record,
        &rendered,
        "selection materialized into the managed rc block",
    )
}

/// The shared write path behind materialization: replace the id's managed
/// block(s) with the canonical rendering unless the single existing block
/// is already byte-identical. Duplicate pairs are always collapsed and
/// reported as a `merged` row — byte-stability of the FIRST block must not
/// let sync call a duplicated-block rc "in sync" (wt83 #175).
fn write_block_report(
    record: &SourceRecord,
    rendered: &str,
    detail: &str,
) -> anyhow::Result<(String, String)> {
    let pair_count = managed_block_pair_count(&record.id)?;
    let unchanged = pair_count == 1
        && managed_block_text(&record.id).as_deref() == Some(rendered.to_string()).as_deref();
    if unchanged {
        return Ok(("unchanged".to_string(), detail.to_string()));
    }
    let (_, consumed) =
        write_managed_block(&begin_marker(&record.id), &end_marker(&record.id), rendered)?;
    if consumed > 1 {
        return Ok((
            "merged".to_string(),
            format!("collapsed {consumed} duplicate managed blocks into one canonical block"),
        ));
    }
    Ok(("activated".to_string(), detail.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::PROCESS_STATE_LOCK;
    use std::fs;

    struct EnvGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
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

    fn unique_temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "niu-assets-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_omb_fixture(root: &Path) {
        fs::create_dir_all(root.join("themes/agnoster")).unwrap();
        fs::create_dir_all(root.join("plugins/git")).unwrap();
        fs::create_dir_all(root.join("aliases")).unwrap();
        fs::write(
            root.join("oh-my-bash.sh"),
            "#!/usr/bin/env bash\ncase $- in *i*) ;; *) return;; esac\n",
        )
        .unwrap();
        fs::write(
            root.join("themes/agnoster/agnoster.theme.sh"),
            "PS1='agnoster> '\n",
        )
        .unwrap();
        fs::write(
            root.join("plugins/git/git.plugin.sh"),
            "alias gg='git status'\n",
        )
        .unwrap();
        fs::write(
            root.join("aliases/cargo.aliases.sh"),
            "alias cb='cargo build'\n",
        )
        .unwrap();
    }

    fn write_bash_it_fixture(root: &Path) {
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::create_dir_all(root.join("plugins/available")).unwrap();
        fs::create_dir_all(root.join("completion/available")).unwrap();
        fs::create_dir_all(root.join("themes/demox")).unwrap();
        fs::write(
            root.join("bash_it.sh"),
            "#!/usr/bin/env bash\nfor _f in \"$BASH_IT/enabled\"/*.bash; do [ -r \"$_f\" ] && . \"$_f\"; done\nunset _f\n",
        )
        .unwrap();
        fs::write(root.join("lib/composure.bash"), "# composure\n").unwrap();
        fs::write(
            root.join("plugins/available/base.plugin.bash"),
            "# BASH_IT_LOAD_PRIORITY: 350\n_base_fn() { :; }\n",
        )
        .unwrap();
        fs::write(
            root.join("completion/available/docker.completion.bash"),
            "# docker completion\n",
        )
        .unwrap();
        fs::write(
            root.join("themes/demox/demox.theme.bash"),
            "PS1='demox> '\n",
        )
        .unwrap();
    }

    fn write_bash_completion_fixture(root: &Path) {
        fs::create_dir_all(root.join("completions")).unwrap();
        fs::write(
            root.join("bash_completion"),
            "# stub\nBASH_COMPLETION_STUB=1\n",
        )
        .unwrap();
        fs::write(root.join("completions/git.bash"), "# git completion\n").unwrap();
    }

    /// A wild single-file plugin tree (§14.6.1 base layer): a repo with one
    /// sourceable file plus README/install noise.
    fn write_wild_fixture(root: &Path) {
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(
            root.join("spark.bash"),
            "#!/usr/bin/env bash\nspark() { echo 'spark!'; }\n",
        )
        .unwrap();
        fs::write(root.join("README.md"), "# spark\n").unwrap();
        fs::write(root.join("install.sh"), "echo installer\n").unwrap();
        fs::write(root.join("tests/spark.bats"), "@test 'x' { :; }\n").unwrap();
    }

    struct Sandbox {
        _home: EnvGuard,
        _userprofile: EnvGuard,
        _sources: EnvGuard,
        _spec: EnvGuard,
        temp: PathBuf,
    }

    /// Isolated HOME + sources root + spec path so enable/disable/sync
    /// never touch the real `~/.niubashrc`, sources, or spec.
    fn sandbox(label: &str) -> Sandbox {
        let temp = unique_temp_dir(label);
        let home = temp.join("home");
        fs::create_dir_all(&home).unwrap();
        Sandbox {
            _home: EnvGuard::set("HOME", &home.to_string_lossy()),
            _userprofile: EnvGuard::set("USERPROFILE", &home.to_string_lossy()),
            _sources: EnvGuard::set(
                "NIU_PLUGIN_SOURCES_ROOT",
                &temp.join("sources").to_string_lossy(),
            ),
            _spec: EnvGuard::set(
                "NIU_PLUGIN_SPEC",
                &home.join(".niubash/plugins.toml").to_string_lossy(),
            ),
            temp,
        }
    }

    fn install(origin: &Path) {
        super::super::sources::add_source(super::super::sources::SourceInstallRequest {
            adapter: None,
            origin: origin.to_string_lossy().into_owned(),
            ref_name: None,
            commit: None,
            expected_checksum: None,
            id: None,
            entry: None,
            fetch_budget: None,
        })
        .expect("fixture add");
    }

    fn trust(id: &str) {
        super::super::sources::trust_source(id).expect("fixture trust");
    }

    fn rc_text() -> String {
        fs::read_to_string(rc_file()).unwrap_or_default()
    }

    fn spec_text() -> String {
        fs::read_to_string(spec::spec_path()).unwrap_or_default()
    }

    #[test]
    fn stray_theme_scan_sees_only_lines_outside_managed_blocks() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sandbox = sandbox("stray-theme-scan");
        fs::write(
            rc_file(),
            format!(
                "export OSH_THEME=agnoster\n\
                 # >>> niu source oh-my-bash >>>\n\
                 OSH_THEME='robbyrussell'\n\
                 # <<< niu source oh-my-bash <<<\n\
                 BASH_IT_THEME='bobby'\n\
                 export NOSH_THEME='nope'\n"
            ),
        )
        .unwrap();
        let stray = super::stray_theme_assignment_lines();
        assert_eq!(stray.len(), 2, "{stray:?}");
        assert!(stray[0].contains("OSH_THEME=agnoster"), "{stray:?}");
        assert!(stray[1].contains("BASH_IT_THEME='bobby'"), "{stray:?}");
    }

    #[test]
    fn omb_enable_disable_round_trip_edits_rc_arrays() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("omb-origin");
        write_omb_fixture(&origin);
        let box_ = sandbox("omb-roundtrip");
        install(&origin);

        // Gate: untrusted sources cannot activate.
        let err = enable("oh-my-bash").expect_err("untrusted enable must fail");
        assert!(err.to_string().contains("untrusted"), "{err}");
        assert!(rc_text().is_empty(), "no rc written on refusal");
        trust("oh-my-bash");

        // Asset-level sugar: each enable writes the spec entry, sync
        // materializes the manager's own rc arrays.
        enable("git").expect("plugin enable");
        enable("cargo").expect("alias enable");
        enable("agnoster").expect("theme enable");
        let rc = rc_text();
        assert!(rc.contains("OSH_THEME='agnoster'"), "{rc}");
        assert!(rc.contains("plugins=('git')"), "{rc}");
        assert!(rc.contains("aliases=('cargo')"), "{rc}");
        let spec = spec_text();
        assert!(spec.contains("target = '"), "{spec}");
        assert!(spec.contains("'git'"), "{spec}");
        assert!(spec.contains("'agnoster'"), "{spec}");

        // Overview reflects the state.
        let overview = asset_overview();
        let report = overview
            .iter()
            .find(|r| r.status.record.id == "oh-my-bash")
            .unwrap();
        assert!(report.activated);
        let is_on = |name: &str| {
            report
                .assets
                .iter()
                .find(|row| row.asset.name == name)
                .unwrap()
                .enabled
        };
        assert!(is_on("git"));
        assert!(is_on("cargo"));
        assert!(is_on("agnoster"));

        // Disables reverse each piece through the spec.
        disable("git").expect("plugin disable");
        let rc = rc_text();
        assert!(!rc.contains("plugins=('git')"), "{rc}");
        disable("agnoster").expect("theme disable");
        assert!(!rc_text().contains("OSH_THEME="), "{}", rc_text());

        let err = enable("nope").expect_err("unknown target");
        assert!(
            err.to_string().contains("no source or asset named 'nope'"),
            "{err}"
        );

        // Source disable removes the declaration and the block, keeps the tree.
        enable("git").unwrap();
        disable("oh-my-bash").expect("source disable");
        let rc = rc_text();
        assert!(!rc.contains(">>> niu source oh-my-bash"), "{rc}");
        assert!(!spec_text().contains("target ="), "{}", spec_text());
        assert!(
            box_.temp.join("sources/oh-my-bash/oh-my-bash.sh").is_file(),
            "tree untouched"
        );
        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn rc_lines_outside_the_managed_block_survive_edits() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("omb-user-rc");
        write_omb_fixture(&origin);
        let box_ = sandbox("omb-user-rc");
        install(&origin);
        trust("oh-my-bash");

        // A user-written rc keeps its content through enable/disable.
        fs::write(
            rc_file(),
            "# my stuff\nexport MY_VAR=1\nalias ll='ls -la'\n",
        )
        .unwrap();
        enable("git").unwrap();
        let rc = rc_text();
        assert!(rc.contains("# my stuff"), "{rc}");
        assert!(rc.contains("export MY_VAR=1"), "{rc}");
        assert!(rc.contains("alias ll='ls -la'"), "{rc}");
        disable("git").unwrap();
        let rc = rc_text();
        assert!(rc.contains("# my stuff"), "{rc}");
        assert!(rc.contains("export MY_VAR=1"), "{rc}");
        assert!(!rc.contains("plugins="), "{rc}");
        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn bash_it_enable_uses_enabled_dir_and_theme_var() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("bit-origin");
        write_bash_it_fixture(&origin);
        let box_ = sandbox("bash-it");
        install(&origin);
        trust("bash-it");

        enable("base").expect("plugin enable");
        let entry = box_
            .temp
            .join("sources/bash-it/enabled/350---base.plugin.bash");
        assert!(entry.is_file(), "enabled/ entry with declared priority");
        let rc = rc_text();
        assert!(rc.contains("BASH_IT="), "{rc}");
        assert!(rc.contains(". \"$BASH_IT/bash_it.sh\""), "{rc}");

        enable("demox").expect("theme enable");
        assert!(rc_text().contains("BASH_IT_THEME='demox'"), "{}", rc_text());

        // Completion from the completion/available dir.
        enable("docker").expect("completion enable");
        assert!(
            box_.temp
                .join("sources/bash-it/enabled")
                .read_dir()
                .unwrap()
                .count()
                >= 2,
            "docker enabled entry exists"
        );

        disable("base").expect("plugin disable");
        assert!(!entry.exists(), "enabled/ entry removed");

        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn bash_completion_is_whole_source_activation() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("bc-origin");
        write_bash_completion_fixture(&origin);
        let box_ = sandbox("bash-completion");
        install(&origin);
        trust("bash-completion");

        enable("bash-completion").expect("whole-source enable");
        let rc = rc_text();
        assert!(rc.contains(". \"${NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}/bash-completion/bash_completion\""), "{rc}");
        assert!(spec_text().contains("bash-completion"), "{}", spec_text());
        // Individual completions are informational only.
        let err = enable("git").expect_err("asset enable must explain");
        assert!(
            err.to_string().contains("activates as a whole source"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn wild_file_source_enables_files_directly_and_honestly() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("wild-holder");
        let origin = holder.join("sparkline");
        fs::create_dir_all(&origin).unwrap();
        write_wild_fixture(&origin);
        let box_ = sandbox("wild");
        install(&origin);
        // The wild install derives its id from the origin tail.
        let record = read_source_registry().into_iter().next().unwrap();
        assert_eq!(record.id, "sparkline", "{}", record.id);
        assert_eq!(record.adapter, "file");
        trust(&record.id);

        // Honest enumeration: every *.sh/*.bash is a candidate, tagged, in
        // depth order; nothing is hidden and nothing is auto-picked.
        let adapter = adapter_for("file").unwrap();
        let assets = adapter.list_assets(&record.path);
        let names: Vec<(String, Option<String>)> = assets
            .iter()
            .map(|a| (a.name.clone(), a.tag.clone()))
            .collect();
        assert!(
            names.contains(&("spark.bash".to_string(), Some("script".to_string()))),
            "{names:?}"
        );
        assert!(
            names.contains(&(
                "install.sh".to_string(),
                Some("installer/test-like — review before sourcing".to_string())
            )),
            "install.sh must be listed with its tag, not hidden: {names:?}"
        );
        // Non-shell files under tests/ are not candidates at all.
        assert!(
            !names.iter().any(|(name, _)| name.starts_with("tests/")),
            "only *.sh/*.bash are candidates: {names:?}"
        );
        // README never appears.
        assert!(
            !names.iter().any(|(name, _)| name.contains("README")),
            "{names:?}"
        );

        // Source-level enable refuses to guess: it lists the candidates.
        let err = enable(&record.id).expect_err("no unique entry for wild sources");
        assert!(err.to_string().contains("pick one"), "{err}");
        assert!(err.to_string().contains("spark.bash"), "{err}");
        assert!(rc_text().is_empty(), "nothing sourced yet");

        // Per-file enable writes one guarded source line.
        enable("spark.bash").expect("file enable");
        let rc = rc_text();
        assert!(
            rc.contains(&format!(
                "if [ -r \"${{NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}}/{}/spark.bash\" ]; then",
                record.id
            )),
            "{rc}"
        );
        assert!(
            rc.contains(&format!(
                "  . \"${{NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}}/{}/spark.bash\"",
                record.id
            )),
            "{rc}"
        );

        // Disable drops the line (and the block with it).
        disable("spark.bash").expect("file disable");
        assert!(!rc_text().contains("spark.bash"), "{}", rc_text());
        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn ambiguous_bare_names_require_the_qualified_form() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let omb = unique_temp_dir("amb-omb");
        let bc = unique_temp_dir("amb-bc");
        write_omb_fixture(&omb);
        write_bash_completion_fixture(&bc);
        let box_ = sandbox("ambiguous");
        install(&omb);
        install(&bc);
        trust("oh-my-bash");
        trust("bash-completion");

        // "git" exists as an OMB plugin and a bash-completion completion.
        let err = enable("git").expect_err("ambiguity must be reported");
        assert!(err.to_string().contains("multiple sources"), "{err}");
        assert!(err.to_string().contains("oh-my-bash/git"), "{err}");

        enable("oh-my-bash/git").expect("qualified enable");
        assert!(rc_text().contains("plugins=('git')"), "{}", rc_text());
        let _ = fs::remove_dir_all(&omb);
        let _ = fs::remove_dir_all(&bc);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#168, `niu plugin enable <theme>` (the rc hint's own verb): a
    /// theme pick MOVES the claim — the previous owner's spec entry loses
    /// it (and its declaration entirely when it carried the theme only), so
    /// the previous framework's theme block goes and no later sync flips
    /// the pick back. Both directions must round-trip, byte-stably.
    #[test]
    fn theme_enable_moves_the_claim_across_sources() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let omb = unique_temp_dir("move-omb");
        let bit = unique_temp_dir("move-bit");
        write_omb_fixture(&omb);
        // The shared name (powerbash10k's shape): both fixtures ship `demox`.
        fs::create_dir_all(omb.join("themes/demox")).unwrap();
        fs::write(
            omb.join("themes/demox/demox.theme.sh"),
            "omb_theme_demox() { PS1='demox-omb> '; }\nomb_theme_demox\n",
        )
        .unwrap();
        write_bash_it_fixture(&bit);
        let box_ = sandbox("theme-claim-move");
        install(&omb);
        install(&bit);
        trust("oh-my-bash");
        trust("bash-it");

        // Pick demox from oh-my-bash: OSH block carries it, spec claims it.
        enable("oh-my-bash/demox").expect("omb pick");
        let rc = rc_text();
        assert!(rc.contains("OSH_THEME='demox'"), "{rc}");
        assert!(spec_text().contains("id = 'oh-my-bash'"), "{}", spec_text());
        assert!(spec_text().contains("theme = 'demox'"), "{}", spec_text());
        let picked_rc = rc_text();

        // A plain sync is a no-op (byte-stable).
        super::super::sync::sync_spec(super::super::sync::SyncOptions::default()).unwrap();
        assert_eq!(rc_text(), picked_rc, "sync must keep the picked theme");

        // Re-pick demox from bash-it: the claim MOVES — the OSH theme line
        // goes (the theme-only omb declaration follows it), the bash-it
        // block carries the theme, and the spec names bash-it as the owner.
        enable("bash-it/demox").expect("bash-it pick");
        let rc = rc_text();
        assert!(rc.contains("BASH_IT_THEME='demox'"), "{rc}");
        assert!(
            !rc.contains("OSH_THEME"),
            "the losing framework's theme block must go: {rc}"
        );
        let spec = spec_text();
        assert!(spec.contains("id = 'bash-it'"), "{spec}");
        assert!(
            !spec.contains("id = 'oh-my-bash'"),
            "the theme-only loser declaration is gone: {spec}"
        );
        let moved_rc = rc_text();
        super::super::sync::sync_spec(super::super::sync::SyncOptions::default()).unwrap();
        assert_eq!(
            rc_text(),
            moved_rc,
            "byte-stable across syncs after the move"
        );

        // And back again.
        enable("oh-my-bash/demox").expect("omb re-pick");
        let rc = rc_text();
        assert!(rc.contains("OSH_THEME='demox'"), "{rc}");
        assert!(!rc.contains("BASH_IT_THEME"), "{rc}");

        let _ = fs::remove_dir_all(&omb);
        let _ = fs::remove_dir_all(&bit);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn degraded_source_refuses_activation_with_restore_hint() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("deg-origin");
        write_omb_fixture(&origin);
        let box_ = sandbox("degraded");
        install(&origin);
        trust("oh-my-bash");
        fs::remove_dir_all(box_.temp.join("sources/oh-my-bash")).unwrap();

        let err = enable("oh-my-bash").expect_err("degraded enable must fail");
        assert!(err.to_string().contains("degraded"), "{err}");
        assert!(
            err.to_string().contains("niu plugin restore oh-my-bash"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn quoted_word_parser_handles_escaped_quotes() {
        assert_eq!(split_quoted_words("'a' 'b'"), vec!["a", "b"]);
        // An empty quoted word is one empty element (shell semantics).
        assert_eq!(split_quoted_words("''"), vec![""]);
        assert_eq!(split_quoted_words("'it'\\''s'"), vec!["it's"]);
        // Hand-edited junk tokens are skipped, not fatal.
        assert_eq!(split_quoted_words("'a' bare 'b'"), vec!["a", "b"]);
    }

    /// A managed block whose end marker was lost (hand edit, partial
    /// restore) must fail enable/disable HONESTLY (wt83 #175): the verbs
    /// exit with the writer's reason and the rc is left untouched — never
    /// a success print over an rc that was not written.
    #[test]
    fn malformed_block_fails_enable_and_disable_honestly() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("malformed-origin");
        write_omb_fixture(&origin);
        let box_ = sandbox("malformed");
        install(&origin);
        trust("oh-my-bash");

        enable("git").expect("clean enable first");
        // Corrupt: drop the end marker line.
        let rc = rc_text();
        let end = "# <<< niu source oh-my-bash <<<\n";
        assert!(rc.contains(end), "{rc}");
        let broken = rc.replace(end, "");
        fs::write(rc_file(), &broken).unwrap();
        let broken = rc_text();

        let err = disable("git").expect_err("disable must not print success over a failed write");
        assert!(err.to_string().contains("no end marker"), "{err}");
        assert!(
            err.to_string()
                .contains("rc update for 'oh-my-bash' failed"),
            "{err}"
        );
        assert_eq!(rc_text(), broken, "the rc is untouched by the failed verb");

        let err = enable("agnoster").expect_err("enable must fail the same way");
        assert!(err.to_string().contains("no end marker"), "{err}");
        assert_eq!(rc_text(), broken, "the rc is untouched by the failed verb");

        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// Duplicate managed blocks (whole-file backup-restore concatenation):
    /// sync must not call the file "in sync" — it reports the anomaly and
    /// collapses the duplicates into one canonical block — and a source
    /// disable removes EVERY matching pair, leaving zero residue (wt83 #175).
    #[test]
    fn duplicate_blocks_are_reported_by_sync_and_fully_removed_by_disable() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("dup-origin");
        write_omb_fixture(&origin);
        let box_ = sandbox("duplicate");
        install(&origin);
        trust("oh-my-bash");
        enable("git").expect("clean enable first");
        let one = rc_text();

        // Duplicate the whole rc onto itself.
        fs::write(rc_file(), format!("{one}{one}")).unwrap();
        let count = |needle: &str| rc_text().matches(needle).count();
        assert_eq!(count(">>> niu source oh-my-bash"), 2, "{:?}", rc_text());

        // Sync reports the anomaly (a `merged` row, so the report is not
        // "clean") and repairs to one canonical block.
        let report = super::super::sync::sync_spec(super::super::sync::SyncOptions::default())
            .expect("sync runs");
        assert!(
            report.rows.iter().any(|row| row.id == "oh-my-bash"
                && row.action == "merged"
                && row.detail.contains("duplicate managed blocks")),
            "{:?}",
            report.rows
        );
        assert_eq!(count(">>> niu source oh-my-bash"), 1, "{}", rc_text());
        assert!(rc_text().contains("plugins=('git')"), "{}", rc_text());

        // Duplicate again, then disable the source: zero residue.
        let one = rc_text();
        fs::write(rc_file(), format!("{one}{one}")).unwrap();
        assert_eq!(count(">>> niu source oh-my-bash"), 2);
        disable("oh-my-bash").expect("source disable removes every block");
        assert_eq!(count(">>> niu source oh-my-bash"), 0, "{}", rc_text());
        assert_eq!(count("<<< niu source oh-my-bash"), 0, "{}", rc_text());
        assert!(!rc_text().contains("oh-my-bash.sh"), "{}", rc_text());
        assert!(
            !spec_text().contains("target ="),
            "the declaration is gone: {}",
            spec_text()
        );

        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#176 (CRLF): a rc the user maintains with CRLF endings stays
    /// CRLF through a managed-block rewrite — no whole-file flattening to
    /// LF, and byte-identical rewrites do not churn the file at all.
    #[test]
    fn crlf_rc_keeps_its_line_endings_through_block_writes() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("crlf-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("crlf");
        fs::write(rc_file(), "alias ll='ls -l'\r\nset -o vi\r\n").unwrap();

        install(&origin);
        trust("oh-my-bash");
        enable("git").expect("enable writes the managed block");

        let text = rc_text();
        assert!(text.contains("alias ll='ls -l'"), "{text:?}");
        assert!(text.contains("plugins=('git')"), "{text:?}");
        let stray_lf = text.replace("\r\n", "").contains('\n');
        assert!(!stray_lf, "every line stays CRLF: {text:?}");

        // An unchanged second materialization is a byte-stable no-op.
        let before = fs::read(rc_file()).unwrap();
        enable("git").expect("second enable");
        assert_eq!(fs::read(rc_file()).unwrap(), before, "no churn");

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#176 (silent drop): a user line hand-written INSIDE a managed
    /// block is migrated out (right below it, under a marker comment) when
    /// the block is rewritten — never silently discarded.
    #[test]
    fn user_lines_inside_a_managed_block_migrate_out() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("migrate-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("migrate");
        install(&origin);
        trust("oh-my-bash");
        enable("git").expect("clean enable first");

        // The user hand-writes a line inside the managed block.
        let hijacked = rc_text().replacen(
            "# <<< niu source oh-my-bash <<<",
            "alias hh='history'\n# <<< niu source oh-my-bash <<<",
            1,
        );
        fs::write(rc_file(), &hijacked).unwrap();

        // A selection change rewrites the block; the alias survives outside
        // while the spec-dropped plugins line is dropped, not migrated.
        enable("cargo").expect("enable rewrites the block");
        let text = rc_text();
        assert!(text.contains("alias hh='history'"), "kept: {text}");
        assert!(text.contains(MIGRATED_USER_LINES_HEADER), "{text}");
        let begin = text.find(">>> niu source oh-my-bash").unwrap();
        let end = text.find("<<< niu source oh-my-bash").unwrap();
        let alias = text.find("alias hh='history'").unwrap();
        assert!(
            alias > end && alias > begin,
            "the alias moved OUT of the managed block: {text}"
        );
        assert!(text.contains("aliases=('cargo')"), "{text}");
        assert!(
            text.contains("plugins=('git')"),
            "the enable is additive; the managed line stays managed: {text}"
        );

        // The healed rc is stable: another rewrite does not re-migrate.
        let before = rc_text();
        enable("cargo").expect("second enable");
        assert_eq!(rc_text(), before, "byte-stable after migration");

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#176 (near markers): a hand-edited marker block (missing a
    /// `>` / `<`) is repaired into the exact managed form — a fresh exact
    /// block is appended BESIDE it, never duplicating the loader.
    #[test]
    fn near_marker_block_is_repaired_not_duplicated() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("near-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("near");
        install(&origin);
        trust("oh-my-bash");
        enable("git").expect("clean enable first");

        // Mangle the begin marker (drop one bracket); the end stays exact.
        let mangled =
            rc_text().replace("# >>> niu source oh-my-bash", "# >> niu source oh-my-bash");
        fs::write(rc_file(), &mangled).unwrap();
        let count = |needle: &str| rc_text().matches(needle).count();
        assert_eq!(
            count("# >>> niu source oh-my-bash"),
            0,
            "mangled: {}",
            rc_text()
        );
        assert_eq!(count("# >> niu source oh-my-bash"), 1);

        // A rewrite consumes the near-marker span; exactly one block remains.
        enable("cargo").expect("enable after the hand edit");
        let text = rc_text();
        assert_eq!(
            count("# >>> niu source oh-my-bash"),
            1,
            "one exact begin: {text}"
        );
        assert_eq!(
            count("# >> niu source oh-my-bash"),
            0,
            "the near-marker form is gone: {text}"
        );
        assert!(
            text.contains("# >>> niu source oh-my-bash (managed"),
            "exact markers restored: {text}"
        );
        assert!(text.contains("aliases=('cargo')"), "{text}");
        assert!(
            !text
                .lines()
                .any(|line| line.trim() == "# >> niu source oh-my-bash >>>"),
            "near marker gone: {text}"
        );

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#176 (backups): every plugin-verb rc rewrite backs the rc up
    /// into the wizard's backups dir, and the backup set is pruned to the
    /// most recent RC_BACKUP_KEEP files (the wizard era never cleaned up).
    #[test]
    fn plugin_verbs_backup_the_rc_and_prune_old_backups() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("backup-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("backup");
        fs::write(rc_file(), "alias ll='ls -l'\n").unwrap();
        let backup_dir = rc_backup_dir().unwrap();

        let backup_count = || {
            fs::read_dir(&backup_dir)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .filter(|entry| {
                            entry
                                .file_name()
                                .to_str()
                                .is_some_and(|name| name.ends_with(".bak"))
                        })
                        .count()
                })
                .unwrap_or(0)
        };
        assert_eq!(backup_count(), 0, "no backup before the first write");

        install(&origin);
        trust("oh-my-bash");
        enable("git").expect("first write");
        assert_eq!(backup_count(), 1, "the rewrite backed the rc up");
        let first_backup = fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(".bak"))
            })
            .unwrap();
        let backed_up = fs::read_to_string(first_backup.path()).unwrap();
        assert!(
            backed_up.contains("alias ll='ls -l'") && !backed_up.contains("niu source"),
            "the backup is the PRE-write rc: {backed_up}"
        );

        // Age 12 fake backups in, then rewrite again: the set is pruned to
        // RC_BACKUP_KEEP (the fresh backup displaces the stale ones).
        let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for index in 0..12 {
            let path = backup_dir.join(format!(".niubashrc.stale-{index}.bak"));
            fs::write(&path, "stale\n").unwrap();
            let file = fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(stale).unwrap();
        }
        enable("cargo").expect("second write");
        assert!(
            backup_count() <= RC_BACKUP_KEEP,
            "pruned to {RC_BACKUP_KEEP}: {}",
            backup_count()
        );

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }
}
