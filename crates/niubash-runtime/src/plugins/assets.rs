//! Asset-level activation for external sources — `niu plugin
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

use std::collections::{BTreeMap, BTreeSet};
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
///
/// niubash#359: no verb WRITES these markers anymore (the generated
/// loader replaced the managed block), but the recognition stays: the
/// startup double-execution guard scans the user rc for them read-only,
/// `niu plugin migrate-rc` reports them, and the doctor names them.
fn begin_marker(id: &str) -> String {
    format!("# >>> niu source {id} (managed by `niu plugin enable/disable`) >>>")
}

fn end_marker(id: &str) -> String {
    format!("# <<< niu source {id} <<<")
}

/// Read-only recognition primitives for the migrate-rc/doctor surfaces
/// (`plugins::loader` scans the user rc with these). They expose what the
/// private spellings above render, with no write path anywhere.
pub fn begin_marker_for(id: &str) -> String {
    begin_marker(id)
}

pub fn end_marker_for(id: &str) -> String {
    end_marker(id)
}

/// True when the line is marker-shaped for `id`: an exact managed marker
/// or the near (hand-edited) decoration — anything bash would recognize
/// as this product's block vocabulary. Read-only classification.
pub fn is_legacy_marker_line(line: &str, id: &str) -> bool {
    let begin = begin_marker(id);
    let end = end_marker(id);
    line.trim() == begin || line.trim() == end
}

/// The distinct source ids whose marker-shaped lines the rc carries, in
/// first-appearance order.
pub fn legacy_marker_ids(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        if let Some(id) = marker_id(line) {
            if !out.iter().any(|existing| existing == id) {
                out.push(id.to_string());
            }
        }
    }
    out
}

/// Inclusive (start, stop) spans of exact marker pairs — the read-only
/// recognition primitive [`managed_block_spans`] exposes for the
/// migrate-rc/doctor surfaces.
pub fn managed_block_spans_public(
    lines: &[String],
    begin: &str,
    end: &str,
) -> anyhow::Result<Vec<(usize, usize)>> {
    managed_block_spans(lines, begin, end)
}

/// The same for near-marker (hand-edited) pairs.
pub fn near_marker_spans_public(
    lines: &[String],
    id: &str,
    begin: &str,
    end: &str,
) -> anyhow::Result<Vec<(usize, usize)>> {
    near_marker_spans(lines, id, begin, end)
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
    /// Assets with activation state (empty for rows whose tree is missing).
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
///
/// niubash#359: this writer shape is RETIRED from every verb path (the
/// generated loader replaces the managed block; see
/// [`render_block_body`] + `plugins::loader`). The function stays as the
/// code-level rollback shape (design §4.3) and for the read-side helpers
/// that recognize what old rcs still carry. The 359.12 structure ratchet
/// pins that no verb can reach it.
fn render_block(record: &SourceRecord, model: &SelectionModel, state: &BlockState) -> String {
    format!(
        "{}\n{}{}\n",
        begin_marker(&record.id),
        render_block_body(record, model, state),
        end_marker(&record.id)
    )
}

/// The body of one source's activation block — theme line, selection
/// arrays (or per-file guarded source lines for DirectFiles), then the
/// adapter's guarded loader snippet. This exact
/// byte stream used to sit between a managed block's markers in the user
/// rc; since niubash#359 it is the body of a section in the generated
/// loader (`plugins::loader`), unchanged in content, changed in position.
fn render_block_body(record: &SourceRecord, model: &SelectionModel, state: &BlockState) -> String {
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
            body.push_str(&format!("if [ -f \"{path}\" ]; then\n  . \"{path}\"\nfi\n"));
        }
    }
    let loader = adapter.loader_snippet(record);
    if !loader.is_empty() {
        body.push_str(&loader);
    }
    body
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
///
/// niubash#359: no writer remains; the migrate-rc verb reports what this
/// finds instead of rewriting it.
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

/// The live activation state of an id, wherever the loader can see it
/// (niubash#359): the generated loader's section first — it is what the
/// host will actually source once the migration completes — then the
/// legacy rc managed blocks (pre-migration rcs keep working read-only;
/// verbs read their selection to merge it into the loader generation).
fn live_block_bodies(id: &str) -> Vec<String> {
    if let Some(text) = super::loader::read_loader_text() {
        if let Some(section) = super::loader::parse_sections(&text)
            .into_iter()
            .find(|section| section.id == id)
        {
            // An empty body still counts as the live materialization (a
            // declared whole-source framework floors on its own loader).
            return vec![section.body];
        }
    }
    read_managed_block_bodies(id)
}

/// True when `id` has live activation state (loader section or legacy rc
/// block) — the floor-yield check and the listing surfaces.
fn live_block_present(id: &str) -> bool {
    !live_block_bodies(id).is_empty()
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
    let bodies = live_block_bodies(id);
    merged_block_state(&bodies, model, record_id)
}

/// The rc's line ending: a CRLF rc stays CRLF (niubash#176 — the whole-file
/// rewrite used to flatten every line to LF, corrupting a mixed/rc-editors'
/// file). `str::lines()` strips the `\r`, so rejoining with the detected
/// terminator restores the file's own convention.
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
                // guarded source lines rendered by this manager.
                if trimmed == "fi"
                    || trimmed.starts_with(". \"")
                    || trimmed.starts_with("if [ -f \"")
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

/// Any wording of the migration header niu ever wrote (niubash#324): the
/// stable prefix outlived the suffix edits, and section detection must
/// recognize the older spellings a corrupt rc still carries.
fn is_migrated_section_header(line: &str) -> bool {
    line.trim()
        .starts_with("# user lines moved out of a managed block by")
}

/// A line that always terminates a migrated-user-lines section: any managed
/// marker shape (exact or near, any id — niu's own block vocabulary never
/// appears inside a section body).
fn is_managed_marker_boundary(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('#')
        && (trimmed
            .trim_start_matches('#')
            .trim_start()
            .starts_with(">>>")
            || trimmed
                .trim_start_matches('#')
                .trim_start()
                .starts_with("<<<"))
}

/// One drift-guard line (niubash#297's retired static snippet, id embedded
/// in the echo): the exact shapes older niu builds wrote inside managed
/// blocks. The guard itself is gone (niubash#647) and current sections
/// never carry it; a moved-out copy of these lines in the user area is
/// niu-authored debris that only this detector still recognizes — kept so
/// the migration below and the doctor can clean up after the retired
/// feature.
fn is_drift_guard_debris(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "fi"
        || trimmed
            == r#"_niu_drift="${NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}/drifted-sources.txt""#
        || trimmed.starts_with("if [ -s \"$_niu_drift\" ]")
        || trimmed.starts_with("echo \"niubash plugin: source ")
        || trimmed == "unset _niu_drift"
        || trimmed.starts_with("# drift guard (static): `niu plugin sync` lists drifted ids")
}

/// Inclusive `(start, stop)` spans of the rc's migrated-user-lines sections:
/// a [`MIGRATED_USER_LINES_HEADER`] line plus the body up to (not
/// including) the next section header, any managed marker line, or EOF.
fn migrated_section_spans(lines: &[String]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if is_migrated_section_header(&lines[index]) {
            let mut stop = index;
            while stop + 1 < lines.len()
                && !is_migrated_section_header(&lines[stop + 1])
                && !is_managed_marker_boundary(&lines[stop + 1])
            {
                stop += 1;
            }
            // Trailing blank lines are separators, not section content.
            while stop > index && lines[stop].trim().is_empty() {
                stop -= 1;
            }
            spans.push((index, stop));
            index = stop + 1;
        } else {
            index += 1;
        }
    }
    spans
}

/// Strip niu-authored drift-guard debris out of the rc's migrated sections
/// (niubash#324): older builds' managed-block rewrites copied previously
/// moved-out content as fresh user lines, and the copy boundary recognized
/// the guard's closing `fi` as managed structure while migrating its
/// openers — leaving N dangling `if` fragments in the user area, a syntax
/// error on every startup. The debris is dead text by construction (the
/// guard is retired, niubash#647, and nothing renders it any more),
/// so a section whose every non-comment line is guard-shaped goes
/// entirely. A section mixing genuine user lines with debris is kept
/// VERBATIM — the user area is only ever edited whole-section here, never
/// line-wise (niubash#176: user lines are sacrosanct, and surgical edits
/// could orphan a user's own if/fi pair).
///
/// Returns the rewritten lines and how many sections disappeared.
fn normalize_migrated_sections(lines: Vec<String>) -> (Vec<String>, usize) {
    let spans = migrated_section_spans(&lines);
    if spans.is_empty() {
        return (lines, 0);
    }
    let mut dropped = Vec::new();
    for (start, stop) in spans {
        let mut has_debris = false;
        let mut all_debris = true;
        let mut index = start + 1;
        while index <= stop {
            let trimmed = lines[index].trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                index += 1;
                continue;
            }
            if is_drift_guard_debris(trimmed) {
                has_debris = true;
            } else {
                all_debris = false;
            }
            index += 1;
        }
        if has_debris && all_debris {
            dropped.push((start, stop));
        }
        // A section mixing user lines with debris is left verbatim: the
        // user area is only ever edited whole-section here, never line-wise
        // (niubash#176 — user lines are sacrosanct; the debris alongside
        // them is inert once the section keeps its user lines).
    }
    if dropped.is_empty() {
        return (lines, 0);
    }
    let mut out = Vec::with_capacity(lines.len());
    let mut cursor = 0;
    let mut removed = 0;
    for (start, stop) in dropped {
        out.extend(lines[cursor..start].iter().cloned());
        cursor = stop + 1;
        removed += 1;
        // Consume the section's own trailing separator blanks so a dropped
        // run of sections leaves no stacked blank lines behind (and no
        // user blank elsewhere is ever touched).
        while cursor < lines.len() && lines[cursor].trim().is_empty() {
            cursor += 1;
        }
    }
    out.extend(lines[cursor..].iter().cloned());
    (out, removed)
}

/// True when the rc on disk still carries moved-out drift-guard debris
/// (niubash#324) — reported read-only by the doctor (the write-era repair
/// paths are retired; the evidence is the product's job now, the fix is
/// the user's).
pub fn rc_has_migrated_debris() -> bool {
    let Ok(text) = fs::read_to_string(rc_file()) else {
        return false;
    };
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let (_, removed) = normalize_migrated_sections(lines);
    removed > 0
}

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
/// are repaired rather than duplicated. niubash#324 adds: previously
/// migrated sections are normalized first (stale guard debris removed, the
/// owner-corrupt rc converges) and a newly migrated line that an existing
/// section already carries is merged into it, never copied again.
///
/// niubash#359: RETIRED from every verb path (rollback zone — see the
/// module-level note on [`apply_block`]).
#[allow(dead_code)]
fn write_managed_block(begin: &str, end: &str, block: &str) -> anyhow::Result<(PathBuf, usize)> {
    let path = rc_file();
    let raw = fs::read_to_string(&path).ok();
    let eol = raw.as_deref().map(detect_eol).unwrap_or("\n");
    let lines: Vec<String> = raw
        .as_deref()
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_else(|| {
            vec!["# Niubash interactive rc - edited by you and `niu plugin`.".to_string()]
        });
    // niubash#324: normalize BEFORE the span scan (dropped sections shift
    // indices), and remember the surviving sections' lines — a migrated
    // line they already carry must not be appended a second time.
    let (mut lines, debris_removed) = normalize_migrated_sections(lines);
    if debris_removed > 0 {
        eprintln!(
            "warning: removed {debris_removed} stale moved-out drift-guard remnant(s) left by \
             an older niu (niu #324) — the drift guard itself is retired (niubash#647); nothing reads these lines"
        );
    }
    let pile_lines: std::collections::BTreeSet<String> = migrated_section_spans(&lines)
        .iter()
        .flat_map(|(start, stop)| lines[*start + 1..=*stop].iter().cloned())
        .map(|line| line.trim().to_string())
        .collect();
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
    // niubash#324 (move, not copy): a line an existing migrated section
    // already carries is NOT migrated again — the pile keeps one copy under
    // its header and the in-block copy leaves with this rewrite, so the
    // user area never grows a duplicate section per verb.
    let mut foreign = foreign_body_lines(&lines, &spans, block, managed.as_ref());
    foreign.retain(|line| !pile_lines.contains(line.trim()));
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
    // niubash#304: the rewrite swaps in through a same-dir temp + replace
    // rename, so a concurrent reader (a `sed`/pipe run against the rc)
    // never observes the truncate-in-place window a bare fs::write opens.
    crate::path_utils::write_file_atomic(&path, text.as_bytes())?;
    Ok((path, consumed))
}

/// Remove EVERY managed marker pair (exact or near-marker, see
/// [`near_marker_spans`]) for (begin, end); returns how many pairs were
/// removed (wt83 #175 — disable must leave zero residue, not drop the
/// first block and strand the duplicates). User lines inside the removed
/// block(s) are migrated out with a warning, never silently dropped
/// (niubash#176); the rc is backed up before the rewrite.
///
/// niubash#359: RETIRED from every verb path (rollback zone — see the
/// module-level note on [`apply_block`]).
#[allow(dead_code)]
fn remove_managed_block(begin: &str, end: &str) -> anyhow::Result<usize> {
    let path = rc_file();
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(0);
    };
    let eol = detect_eol(&text);
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    // niubash#324: normalize the migrated sections BEFORE the span scan
    // (dropped debris shifts indices) — an already-corrupt rc converges on
    // the removal path too, even when this verb has no block to remove.
    let (lines, debris_removed) = normalize_migrated_sections(lines);
    if debris_removed > 0 {
        eprintln!(
            "warning: removed {debris_removed} stale moved-out drift-guard remnant(s) left by \
             an older niu (niu #324) — the drift guard itself is retired (niubash#647); nothing reads these lines"
        );
    }
    // Same move-not-copy dedupe as the write path (niubash#324): lines an
    // existing migrated section carries are never re-inserted.
    let pile_lines: std::collections::BTreeSet<String> = migrated_section_spans(&lines)
        .iter()
        .flat_map(|(start, stop)| lines[*start + 1..=*stop].iter().cloned())
        .map(|line| line.trim().to_string())
        .collect();
    let id = marker_id(begin).unwrap_or_default().to_string();
    let mut spans = managed_block_spans(&lines, begin, end)?;
    for span in near_marker_spans(&lines, &id, begin, end)? {
        if !spans.contains(&span) {
            spans.push(span);
        }
    }
    spans.sort_unstable();
    if spans.is_empty() {
        if debris_removed > 0 {
            // No block to remove, but the normalization repaired the rc:
            // persist it through the same backup + atomic swap.
            backup_rc();
            let mut joined = lines.join(eol);
            joined.push_str(eol);
            crate::path_utils::write_file_atomic(&path, joined.as_bytes())?;
        }
        return Ok(0);
    }
    // Recognized managed lines for the removal path: the canonical
    // rendering of the block's own parsed state, plus managed-syntax
    // classification. Without it, every body line (loader lines included)
    // would look "foreign" — removal must migrate USER lines only, and an
    // unresolvable record conservatively migrates nothing.
    let registry = read_source_registry();
    let managed = record_for_marker(begin, &registry);
    let mut foreign = match &managed {
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
    foreign.retain(|line| !pile_lines.contains(line.trim()));
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
    // niubash#304: same atomic swap as the insert path — no truncate window.
    crate::path_utils::write_file_atomic(&path, joined.as_bytes())?;
    Ok(spans.len())
}

/// Render a source's managed-theme BLOCK as a string — a pure renderer with
/// no write path. niubash#359: the setup wizard no longer embeds this in
/// the rc and verbs publish loader sections instead; the live consumers are
/// the theme previewer (it sources the rendering in a child process) and
/// the tests that seed write-era rc fixtures. Kept pub: the preview must
/// render byte-identically to what a write-era rc carried.
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

/// Deactivate a source's TREE-side activation (bash-it `enabled/` entries)
/// without touching the rest of the tree — and without touching the user
/// rc (niubash#359: the loader generation drops the section instead; the
/// sync caller omits the id from the computed set). Used when the spec
/// stops declaring a source it owned (§14.6.3: spec is the truth; the tree
/// itself is only ever removed by `niu plugin source remove`). Errors
/// surface: a disable that could not do its tree half must not print
/// success (wt83 #175).
pub fn deactivate_tree(record: &SourceRecord) -> anyhow::Result<()> {
    if let Some(adapter) = adapter_for(&record.adapter) {
        if let SelectionModel::EnabledDir { .. } = adapter.selection_model() {
            for asset in adapter.list_assets(&record.path) {
                tree_disable(record, &asset)?;
            }
        }
    }
    Ok(())
}

/// Fully deactivate a source: the managed rc block goes (ALL matching
/// blocks — wt83 #175) and the `enabled/` tree entries go too.
///
/// niubash#359: RETIRED from every verb path — the rc half violated the
/// zero-write contract. Kept unwired in the rollback zone (design §4.3);
/// the live path is [`deactivate_tree`] + loader generation omission.
/// Private on purpose: a pub re-export was a compile-passing path from any
/// module straight into `remove_managed_block` (CR F1 mutation). Restoring
/// `pub` is part of the rollback itself.
#[allow(dead_code)]
fn deactivate_block(record: &SourceRecord) -> anyhow::Result<()> {
    remove_managed_block(&begin_marker(&record.id), &end_marker(&record.id))?;
    deactivate_tree(record)
}

// ── Overview ─────────────────────────────────────────────────────────────────

/// Compute `niu plugin list` rows: every registered source with its assets
/// and activation state (sources whose tree is missing list with state
/// only).
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
        if status.missing {
            out.push(SourceReport {
                status,
                assets: Vec::new(),
                activated: false,
            });
            continue;
        }
        let record = &status.record;
        let model = adapter.selection_model();
        let bodies = live_block_bodies(&record.id);
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
/// unambiguous across installed sources.
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
    // Bare asset name across installed sources whose tree is present.
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
            // A bare name shipping from several installed sources is
            // ambiguous — UNLESS the spec's own theme claim names exactly
            // one of the matches. The wizard's finish screen and the setup
            // journal print the bare name in the undo receipt
            // (`niu plugin disable powerline-multiline`), so the undo
            // contract needs the ACTIVE theme's owning source to win the
            // resolution instead of a refusal
            // (wt80-undo-receipt-ambiguous-theme, release run 38404767765
            // P4-S3): the ambiguity exists precisely because that claim's
            // pick installed the same name from two frameworks (niu#168's
            // same-name family). The claim is recorded intent, not a
            // guess; with no claim, or a claim matching several
            // candidates, the honest ambiguity error stands.
            if let Ok(Some(spec)) = spec::load_spec() {
                let claims: Vec<(&str, &str)> = spec
                    .sources
                    .iter()
                    .filter_map(|entry| {
                        let id = entry.id.as_deref()?;
                        let theme = entry.theme.as_deref()?;
                        if theme.is_empty() {
                            None
                        } else {
                            Some((id, theme))
                        }
                    })
                    .collect();
                let owned: Vec<&(SourceRecord, SourceAsset)> = matches
                    .iter()
                    .filter(|(record, asset)| {
                        asset.kind == SourceAssetKind::Theme
                            && claims
                                .iter()
                                .any(|(id, theme)| *id == record.id && *theme == asset.name)
                    })
                    .collect();
                if owned.len() == 1 {
                    let (record, asset) = owned[0];
                    return check_gate(
                        Target::Asset {
                            record: record.clone(),
                            asset: asset.clone(),
                        },
                        record,
                    );
                }
            }
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

/// Activation requires the tree to be there: a source whose directory is
/// gone contributes nothing until restored (§11.4).
fn check_gate(target: Target, record: &SourceRecord) -> anyhow::Result<Target> {
    if !record.path.is_dir() {
        bail!(
            "source '{}' cannot activate (tree missing); restore it first: `niu plugin restore {}`",
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
    // The loader generation is the visible half of enable: a failed row
    // for this source must fail the verb — printing success while the
    // generation was never published is the fake success wt83 #175
    // records. (The user rc is never written, niubash#359: when the rc
    // still carries the legacy managed block, the CLI prints the
    // migrate-rc note so the transition state is visible.)
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
                    "source '{}' deactivated — spec entry removed, loader section dropped \
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
    // Same honesty contract as enable (wt83 #175): a disable whose
    // generation publish failed must fail loudly, not print "loader
    // section dropped" over a section that still loads at every startup.
    let summary = finish_activation_summary(&record.id, summary, &report)?;
    Ok(ActivationOutcome {
        summary,
        undo: format!("niu plugin enable {name}"),
    })
}

// ── Spec materialization (compute + loader generation publish) ───────────────

/// What one spec computation pass decided for a source — WITHOUT touching
/// the user rc (niubash#359: the rc is never written; the decision lands
/// in the generated loader through [`publish_loader_generation`]).
#[derive(Debug, Clone)]
pub struct ComputedSelection {
    /// The source's rendered section body (theme/arrays/files + loader
    /// snippet) when it participates in the generation;
    /// `None` = the generation drops the section (nothing enabled).
    pub section: Option<String>,
    pub detail: String,
    /// New `spec_enabled` to persist (None = leave as-is).
    pub spec_enabled: Option<Vec<String>>,
    /// New `spec_theme` to persist.
    pub spec_theme: Option<String>,
    /// The record's adapter is unknown — the row reports `unsupported`
    /// (nothing can be computed, the section is dropped, never silently).
    pub unsupported: bool,
}

/// One row of a loader generation publish: the same action vocabulary the
/// rc materialization used (activated | unchanged | deactivated), derived
/// by diffing the desired sections against the current generation.
#[derive(Debug, Clone)]
pub struct GenerationRow {
    pub id: String,
    pub action: String,
    pub detail: String,
}

/// Compute one spec entry's selection against an installed record whose
/// tree is present (§14.6.3 merge semantics): the spec's selection plus any
/// hand-added entries wins; entries the spec dropped (in `prev` but not
/// `next`) are removed; a hand-set theme survives a spec that does not
/// declare one.
///
/// `theme_claimed_elsewhere` is the niubash#168 floor-yield input: true
/// when another spec entry claims a (non-empty) theme. A source with no
/// selection of its own then does NOT (re)create its loader-only floor
/// section — the framework would load on top of the claimed theme. A
/// section that already exists is left byte-stable (never silently
/// flipped); removal is an explicit verb or a reconciled claim move.
///
/// Side effects are confined to the control plane and the source's own
/// tree (bash-it `enabled/` entries): the user rc is never touched
/// (niubash#359).
pub fn compute_spec_selection(
    record: &SourceRecord,
    entry: &SpecSource,
    theme_claimed_elsewhere: bool,
) -> anyhow::Result<ComputedSelection> {
    let Some(adapter) = adapter_for(&record.adapter) else {
        return Ok(ComputedSelection {
            section: None,
            detail: format!("unknown adapter '{}'", record.adapter),
            spec_enabled: None,
            spec_theme: None,
            unsupported: true,
        });
    };
    let model = adapter.selection_model();
    let computed = match model {
        SelectionModel::WholeSource => {
            // Declared = active; the guarded loader section is the unit.
            ComputedSelection {
                section: Some(render_block_body(record, &model, &BlockState::default())),
                detail: "whole-source guarded loader".to_string(),
                spec_enabled: Some(Vec::new()),
                spec_theme: None,
                unsupported: false,
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
            let active = !final_names.is_empty();
            ComputedSelection {
                section: active.then(|| render_block_body(record, &model, &state)),
                detail: detail_with_unknown("per-file guarded source lines".to_string(), &unknown),
                // Persist the SPEC-OWNED selection only (`next`), not the
                // union with hand-added entries: `prev` must stay "what the
                // spec last materialized" so hand additions remain
                // hand-added on every later sync. Absorbing them into prev
                // would let the next spec-side removal kill them one sync
                // after the user typed them.
                spec_enabled: Some(next),
                spec_theme: None,
                unsupported: false,
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
            let (tree_changed, state) = match model {
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
                    (
                        tree_changed,
                        BlockState {
                            theme: theme.clone(),
                            arrays: BTreeSet::new(),
                            array_items: Vec::new(),
                            files: Vec::new(),
                        },
                    )
                }
                _ => {
                    // Start from the live parsed section so hand-added
                    // entries (even ones niu cannot enumerate — the
                    // manager may still know them) keep their exact
                    // placement; then apply the spec's delta: drop what the
                    // spec dropped, add what it declares. The live state is
                    // the generated loader's section (falling back to the
                    // legacy rc blocks on pre-migration machines).
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
                    (false, state)
                }
            };
            // Same loader-fidelity invariant as before (defaults-as-floor
            // §14.5/§14.6): a DECLARED loader-manager source is active —
            // OMB with no OSH_THEME renders its own default theme, bash-it
            // with an empty enabled/ set still sources its framework.
            // Disable removes the declaration (and with it the section).
            // EXCEPT (niubash#168): the floor yields to a claimed theme —
            // a selection-less source never wins over a theme claimed by
            // another declared source; a present section stays (never
            // silently flipped).
            let has_selection = theme.is_some()
                || !final_names.is_empty()
                || state.array_items.iter().any(|(_, items)| !items.is_empty());
            let active =
                has_selection || !theme_claimed_elsewhere || live_block_present(&record.id);
            let detail = if tree_changed {
                "enabled/ entries reconciled; loader section from the manager's own selection"
                    .to_string()
            } else {
                "selection materialized into the generated loader".to_string()
            };
            ComputedSelection {
                section: active.then(|| render_block_body(record, &model, &state)),
                detail: detail_with_unknown(detail, &unknown),
                // `prev` = the spec-owned selection only (see the
                // DirectFiles branch): hand-added entries must stay
                // hand-added across syncs, not become spec-removable one
                // sync after they were written.
                spec_enabled: Some(next),
                spec_theme: entry.theme.clone().filter(|theme| !theme.is_empty()),
                unsupported: false,
            }
        }
    };
    Ok(computed)
}

/// Render + publish the loader generation from the computed selections
/// (niubash#359; gate case 359.7): sections are keyed and sorted by id so
/// identical manifest state renders byte-identical output; the previous
/// generation survives a failed validation/publish unchanged
/// (validate-then-atomic), and a byte-identical regeneration is a no-op.
///
/// `keep_current` names ids this pass SKIPPED (missing tree,
/// install failure): their current sections carry over verbatim,
/// exactly as the rc materialization era left their blocks untouched on a
/// skip. `drop_silent` names ids another pass already reported
/// (sync's undeclared-source deactivation): their sections go without a
/// duplicate row. Returns the per-id diff rows and whether a new
/// generation was published.
pub fn publish_loader_generation(
    computed: &[(String, ComputedSelection)],
    keep_current: &BTreeSet<String>,
    drop_silent: &BTreeSet<String>,
) -> anyhow::Result<(Vec<GenerationRow>, bool)> {
    let current_text = super::loader::read_loader_text().unwrap_or_default();
    let current: BTreeMap<String, String> = super::loader::parse_sections(&current_text)
        .into_iter()
        .map(|section| (section.id, section.body))
        .collect();
    let mut desired: BTreeMap<String, String> = BTreeMap::new();
    for (id, selection) in computed {
        if let Some(body) = &selection.section {
            // Normalize the trailing newline: the renderer's body ends with
            // one (the wrapper supplies the line break), the parser's
            // section body does not — identical content must compare equal
            // so an unchanged manifest stays byte-stable.
            desired.insert(id.clone(), body.trim_end_matches('\n').to_string());
        }
    }
    for id in keep_current {
        if !desired.contains_key(id) {
            if let Some(body) = current.get(id) {
                desired.insert(id.clone(), body.clone());
            }
        }
    }
    let mut rows = Vec::new();
    for (id, selection) in computed {
        if selection.unsupported {
            rows.push(GenerationRow {
                id: id.clone(),
                action: "unsupported".to_string(),
                detail: selection.detail.clone(),
            });
            continue;
        }
        let action = match (desired.get(id), current.get(id)) {
            (Some(body), Some(current_body)) if body == current_body => "unchanged",
            (Some(_), _) => "activated",
            (None, Some(_)) => "deactivated",
            (None, None) => "unchanged",
        };
        rows.push(GenerationRow {
            id: id.clone(),
            action: action.to_string(),
            detail: selection.detail.clone(),
        });
    }
    // Sections the current generation carries but neither this pass
    // computed nor kept are stale (the source left the manifest entirely):
    // the generation drops them, with a visible row — unless another pass
    // already reported the drop (`drop_silent`).
    for id in current.keys() {
        if !desired.contains_key(id)
            && !computed.iter().any(|(existing, _)| existing == id)
            && !keep_current.contains(id)
            && !drop_silent.contains(id)
        {
            rows.push(GenerationRow {
                id: id.clone(),
                action: "deactivated".to_string(),
                detail: "source left the manifest — loader section dropped".to_string(),
            });
        }
    }
    let sections: Vec<super::loader::LoaderSection> = desired
        .into_iter()
        .map(|(id, body)| super::loader::LoaderSection { id, body })
        .collect();
    // The double-execution record (design §2.3.3): this generation
    // supersedes the managed marker pairs of every id it manages, plus
    // everything a previous generation recorded (migrate-rc additions —
    // the record must survive regeneration).
    let mut superseded: BTreeSet<String> = BTreeSet::new();
    for section in &sections {
        superseded.insert(begin_marker(&section.id));
        superseded.insert(end_marker(&section.id));
    }
    for marker in super::loader::recorded_superseded_markers(&current_text) {
        superseded.insert(marker);
    }
    let rendered = super::loader::render_document(&sections, &superseded);
    let published = super::loader::publish(&rendered)?;
    Ok((rows, published))
}

/// Regenerate the loader generation from the manifest alone — no installs,
/// no registry or spec writes (the migrate-rc half of the generator;
/// design §4.1 step 2). Sources the spec declares but that cannot safely
/// materialize keep their current section exactly as a sync skip would:
/// missing trees, compute failures — AND entries that still
/// resolve to an id but have no registry record yet (their activation,
/// if any was ever published, carries over instead of being silently
/// dropped: a regeneration on the migrate-rc verb must never de-activate).
/// Only entries that resolve to NO id at all (never installed, never
/// published — no section can exist) are omitted, which is a no-op.
pub fn regenerate_loader_generation() -> anyhow::Result<bool> {
    let Some(spec) = spec::load_spec()? else {
        // No spec: nothing is declared, the generation is empty (the rc's
        // legacy blocks keep executing until the user migrates).
        return publish_loader_generation(&[], &BTreeSet::new(), &BTreeSet::new())
            .map(|(_, published)| published);
    };
    let registry = read_source_registry();
    let theme_claimed_elsewhere: Vec<bool> = spec
        .sources
        .iter()
        .enumerate()
        .map(|(index, _)| {
            spec.sources.iter().enumerate().any(|(other, other_entry)| {
                other != index
                    && other_entry
                        .theme
                        .as_deref()
                        .is_some_and(|theme| !theme.is_empty())
            })
        })
        .collect();
    let mut computed: Vec<(String, ComputedSelection)> = Vec::new();
    let mut keep_current: BTreeSet<String> = BTreeSet::new();
    for (index, entry) in spec.sources.iter().enumerate() {
        let Some(record) = super::sync::record_for_entry(entry, &registry) else {
            // No install record — but if the entry resolves to an id, a
            // stale section from an earlier machine state must carry over
            // (a skip never de-activates; CR F3).
            if let Some(id) = spec::resolve_entry_id(entry) {
                keep_current.insert(id);
            }
            continue;
        };
        let claimed = theme_claimed_elsewhere.get(index).copied().unwrap_or(false);
        let eligible = record.path.is_dir();
        if !eligible {
            keep_current.insert(record.id.clone());
            continue;
        }
        match compute_spec_selection(record, entry, claimed) {
            Ok(selection) => computed.push((record.id.clone(), selection)),
            Err(_) => {
                keep_current.insert(record.id.clone());
            }
        }
    }
    publish_loader_generation(&computed, &keep_current, &BTreeSet::new())
        .map(|(_, published)| published)
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

// ── Retired rc writers (code-level rollback zone, design §4.3) ───────────────
//
// niubash#359 case A retired every verb's right to rewrite the user rc.
// The functions below are the retired managed-block write machinery:
// UNWIRED (no verb path reaches them — gate case 359.12's structure
// ratchet pins that) but kept so a code-level rollback is a small diff:
// restore the call sites and the managed-block behavior returns. They
// must never be called from a verb; new code touches the generated
// loader (`plugins::loader`) instead.

/// Write (or drop) the managed block for a state; reports what happened.
/// `active` says whether anything is activated at all (selection non-empty
/// or a theme set) — a loader-only block still counts as active for
/// models whose selection lives outside the rc (bash-it enabled/ entries).
#[allow(dead_code)]
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
/// let sync call a duplicated-block rc "in sync" (wt83 #175). The
/// byte-identical fast path also falls through when the rc still carries
/// moved-out guard debris (niubash#324): the block is in sync but the file
/// around it is not, and the rewrite normalizes it.
#[allow(dead_code)]
fn write_block_report(
    record: &SourceRecord,
    rendered: &str,
    detail: &str,
) -> anyhow::Result<(String, String)> {
    let pair_count = managed_block_pair_count(&record.id)?;
    let unchanged = pair_count == 1
        && managed_block_text(&record.id).as_deref() == Some(rendered.to_string()).as_deref()
        && !rc_has_migrated_debris();
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
            credential_guard: false,
        })
        .expect("fixture add");
    }

    fn rc_text() -> String {
        fs::read_to_string(rc_file()).unwrap_or_default()
    }

    fn spec_text() -> String {
        fs::read_to_string(spec::spec_path()).unwrap_or_default()
    }

    /// The generated loader's text in the sandbox (the activation plane the
    /// verbs publish since niubash#359).
    fn loader_text() -> String {
        super::super::loader::read_loader_text().unwrap_or_default()
    }

    /// The loader section body for one id (wrapper lines excluded).
    fn loader_section(id: &str) -> String {
        super::super::loader::parse_sections(&loader_text())
            .into_iter()
            .find(|section| section.id == id)
            .map(|section| section.body)
            .unwrap_or_default()
    }

    fn rc_bytes_snapshot() -> Option<Vec<u8>> {
        fs::read(rc_file()).ok()
    }

    fn assert_rc_bytes_unchanged(before: &Option<Vec<u8>>) {
        assert_eq!(
            before.as_deref(),
            fs::read(rc_file()).ok().as_deref(),
            "the user rc must be byte-identical (niubash#359: no verb writes the rc)"
        );
    }

    /// Seed a legacy managed block (the write-era shape) into the sandbox
    /// rc — the only way a test creates one now, exactly as an old niu
    /// version would have left it.
    fn seed_legacy_block(id: &str, body: &str) {
        let mut text = rc_text();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&super::begin_marker_for(id));
        text.push('\n');
        text.push_str(body.trim_end_matches('\n'));
        text.push('\n');
        text.push_str(&super::end_marker_for(id));
        text.push('\n');
        fs::write(rc_file(), text).unwrap();
    }

    /// The write-era canonical body of an oh-my-bash managed block (what an
    /// old niu rendered between the markers) for legacy-rc seeding.
    const OMB_BLOCK: &str = "OSH_THEME='robbyrussell'\nplugins=('git')\n. \"$OSH/oh-my-bash.sh\"";

    #[test]
    fn stray_theme_scan_sees_only_lines_outside_managed_blocks() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sandbox = sandbox("stray-theme-scan");
        fs::write(
            rc_file(),
            "export OSH_THEME=agnoster\n\
                 # >>> niu source oh-my-bash >>>\n\
                 OSH_THEME='robbyrussell'\n\
                 # <<< niu source oh-my-bash <<<\n\
                 BASH_IT_THEME='bobby'\n\
                 export NOSH_THEME='nope'\n",
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

        // niubash#647: no trust gate — enable works right after install.
        assert!(rc_text().is_empty(), "no rc written before any verb");

        // Asset-level sugar: each enable edits the spec entry, sync
        // publishes the generated loader (niubash#359: the rc is never
        // written).
        enable("git").expect("plugin enable");
        enable("cargo").expect("alias enable");
        enable("agnoster").expect("theme enable");
        let loader = loader_text();
        assert!(loader.contains("OSH_THEME='agnoster'"), "{loader}");
        assert!(loader.contains("plugins=('git')"), "{loader}");
        assert!(loader.contains("aliases=('cargo')"), "{loader}");
        assert!(
            !rc_text().contains("OSH_THEME"),
            "no rc was ever written: {}",
            rc_text()
        );
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
        let loader = loader_text();
        assert!(!loader.contains("plugins=('git')"), "{loader}");
        disable("agnoster").expect("theme disable");
        assert!(!loader_text().contains("OSH_THEME="), "{}", loader_text());

        let err = enable("nope").expect_err("unknown target");
        assert!(
            err.to_string().contains("no source or asset named 'nope'"),
            "{err}"
        );

        // Source disable removes the declaration and the loader section,
        // keeps the tree (and never touches the rc).
        enable("git").unwrap();
        disable("oh-my-bash").expect("source disable");
        let loader = loader_text();
        assert!(
            !loader.contains("niu plugin source: oh-my-bash"),
            "{loader}"
        );
        assert!(!spec_text().contains("target ="), "{}", spec_text());
        assert!(
            box_.temp.join("sources/oh-my-bash/oh-my-bash.sh").is_file(),
            "tree untouched"
        );
        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn ambiguous_theme_disable_resolves_to_the_claim_owner() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Two frameworks ship the same theme name — niu#168's same-name
        // family (oh-my-bash AND bash-it carry e.g. powerline-multiline).
        // The wizard's undo receipt prints the bare name, so the bare-name
        // disable must reach the theme's OWNING source instead of refusing
        // (wt80-undo-receipt-ambiguous-theme, release run 38404767765
        // P4-S3).
        let omb_origin = unique_temp_dir("same-name-omb");
        write_omb_fixture(&omb_origin);
        let bashit_origin = unique_temp_dir("same-name-bashit");
        write_bash_it_fixture(&bashit_origin);
        fs::create_dir_all(bashit_origin.join("themes/agnoster")).unwrap();
        fs::write(
            bashit_origin.join("themes/agnoster/agnoster.theme.bash"),
            "PS1='agnoster-bashit> '\n",
        )
        .unwrap();
        let box_ = sandbox("same-name-undo");
        install(&omb_origin);
        install(&bashit_origin);

        // No claim yet: the bare name stays honestly ambiguous.
        let err = disable("agnoster").expect_err("ambiguous disable must fail");
        assert!(
            err.to_string().contains("exists in multiple sources"),
            "{err}"
        );

        // The wizard's path: the pick claims the theme for one framework
        // (the qualified enable), then the undo receipt runs the bare-name
        // disable from the finish screen.
        enable("oh-my-bash/agnoster").expect("qualified theme enable");
        assert!(
            loader_text().contains("OSH_THEME='agnoster'"),
            "{}",
            loader_text()
        );
        disable("agnoster").expect("bare-name disable resolves to the claim owner");
        assert!(!loader_text().contains("OSH_THEME="), "{}", loader_text());
        let spec = spec_text();
        assert!(
            spec.contains("theme = ''"),
            "theme pick explicitly cleared: {spec}"
        );

        // The claim is evidence, not a blanket pass: once cleared (theme
        // = ''), the bare name is ambiguous again.
        let err = disable("agnoster").expect_err("cleared claim restores ambiguity");
        assert!(
            err.to_string().contains("exists in multiple sources"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&omb_origin);
        let _ = fs::remove_dir_all(&bashit_origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn rc_lines_survive_verbs_because_nothing_writes_the_rc() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("omb-user-rc");
        write_omb_fixture(&origin);
        let box_ = sandbox("omb-user-rc");
        install(&origin);

        // A user-written rc keeps its content through enable/disable --
        // byte-identically (niubash#359: no verb writes the rc; the
        // selection lands in the generated loader instead).
        fs::write(
            rc_file(),
            "# my stuff\nexport MY_VAR=1\nalias ll='ls -la'\n",
        )
        .unwrap();
        let before = rc_bytes_snapshot();
        enable("git").unwrap();
        assert_rc_bytes_unchanged(&before);
        assert!(
            loader_text().contains("plugins=('git')"),
            "{}",
            loader_text()
        );
        disable("git").unwrap();
        assert_rc_bytes_unchanged(&before);
        assert!(!loader_text().contains("plugins="), "{}", loader_text());
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

        enable("base").expect("plugin enable");
        let entry = box_
            .temp
            .join("sources/bash-it/enabled/350---base.plugin.bash");
        assert!(entry.is_file(), "enabled/ entry with declared priority");
        let loader = loader_section("bash-it");
        assert!(loader.contains("BASH_IT="), "{loader}");
        assert!(loader.contains(". \"$BASH_IT/bash_it.sh\""), "{loader}");

        enable("demox").expect("theme enable");
        assert!(
            loader_section("bash-it").contains("BASH_IT_THEME='demox'"),
            "{}",
            loader_text()
        );

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

        enable("bash-completion").expect("whole-source enable");
        let loader = loader_section("bash-completion");
        assert!(loader.contains(". \"${NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}/bash-completion/bash_completion\""), "{loader}");
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
        assert!(loader_text().is_empty(), "nothing sourced yet");

        // Per-file enable publishes one guarded source line in the loader.
        enable("spark.bash").expect("file enable");
        let loader = loader_section(&record.id);
        assert!(
            loader.contains(&format!(
                "if [ -f \"${{NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}}/{}/spark.bash\" ]; then",
                record.id
            )),
            "{loader}"
        );
        assert!(
            loader.contains(&format!(
                "  . \"${{NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}}/{}/spark.bash\"",
                record.id
            )),
            "{loader}"
        );

        // Disable drops the line (and the section with it).
        disable("spark.bash").expect("file disable");
        assert!(!loader_text().contains("spark.bash"), "{}", loader_text());
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

        // "git" exists as an OMB plugin and a bash-completion completion.
        let err = enable("git").expect_err("ambiguity must be reported");
        assert!(err.to_string().contains("multiple sources"), "{err}");
        assert!(err.to_string().contains("oh-my-bash/git"), "{err}");

        enable("oh-my-bash/git").expect("qualified enable");
        assert!(
            loader_text().contains("plugins=('git')"),
            "{}",
            loader_text()
        );
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

        // Pick demox from oh-my-bash: the loader carries it, the spec
        // claims it, and the rc is untouched (niubash#359).
        enable("oh-my-bash/demox").expect("omb pick");
        let loader = loader_text();
        assert!(loader.contains("OSH_THEME='demox'"), "{loader}");
        assert!(spec_text().contains("id = 'oh-my-bash'"), "{}", spec_text());
        assert!(spec_text().contains("theme = 'demox'"), "{}", spec_text());
        let picked_loader = loader_text();

        // A plain sync is a no-op (byte-stable generation).
        super::super::sync::sync_spec(super::super::sync::SyncOptions::default()).unwrap();
        assert_eq!(
            loader_text(),
            picked_loader,
            "sync must keep the picked theme"
        );

        // Re-pick demox from bash-it: the claim MOVES -- the OSH theme line
        // goes (the theme-only omb declaration follows it), the bash-it
        // section carries the theme, and the spec names bash-it as owner.
        enable("bash-it/demox").expect("bash-it pick");
        let loader = loader_text();
        assert!(loader.contains("BASH_IT_THEME='demox'"), "{loader}");
        assert!(
            !loader.contains("OSH_THEME"),
            "the losing framework's theme section must go: {loader}"
        );
        let spec = spec_text();
        assert!(spec.contains("id = 'bash-it'"), "{spec}");
        assert!(
            !spec.contains("id = 'oh-my-bash'"),
            "the theme-only loser declaration is gone: {spec}"
        );
        let moved_loader = loader_text();
        super::super::sync::sync_spec(super::super::sync::SyncOptions::default()).unwrap();
        assert_eq!(
            loader_text(),
            moved_loader,
            "byte-stable across syncs after the move"
        );

        // And back again.
        enable("oh-my-bash/demox").expect("omb re-pick");
        let loader = loader_text();
        assert!(loader.contains("OSH_THEME='demox'"), "{loader}");
        assert!(!loader.contains("BASH_IT_THEME"), "{loader}");

        let _ = fs::remove_dir_all(&omb);
        let _ = fs::remove_dir_all(&bit);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    #[test]
    fn missing_source_refuses_activation_with_restore_hint() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("deg-origin");
        write_omb_fixture(&origin);
        let box_ = sandbox("missing");
        install(&origin);
        fs::remove_dir_all(box_.temp.join("sources/oh-my-bash")).unwrap();

        let err = enable("oh-my-bash").expect_err("missing-tree enable must fail");
        assert!(err.to_string().contains("tree missing"), "{err}");
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
    /// A legacy rc whose managed block lost its end marker (hand edit,
    /// partial restore) no longer BLOCKS anything (niubash#359: the verbs
    /// never parse or rewrite the rc), and the scan reports the malformed
    /// span so the doctor/migrate-rc can name it. The rc stays untouched.
    #[test]
    fn malformed_legacy_block_does_not_block_verbs_and_is_reported() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("malformed-origin");
        write_omb_fixture(&origin);
        let box_ = sandbox("malformed");
        install(&origin);

        enable("git").expect("clean enable first");
        // Seed the write-era block, then corrupt it: drop the end marker
        // line (a partial hand edit an old rc could carry).
        seed_legacy_block(
            "oh-my-bash",
            "OSH_THEME='robbyrussell'\n. \"$OSH/oh-my-bash.sh\"\n",
        );
        let rc = rc_text();
        let end = "# <<< niu source oh-my-bash <<<\n";
        assert!(rc.contains(end), "{rc}");
        let broken = rc.replace(end, "");
        fs::write(rc_file(), &broken).unwrap();
        let broken_bytes = fs::read(rc_file()).unwrap();

        // The verbs do not parse the rc: they publish the loader and leave
        // the malformed block exactly where the user left it.
        disable("git").expect("disable succeeds (the rc is not the write target)");
        enable("agnoster").expect("enable succeeds the same way");
        assert_eq!(
            fs::read(rc_file()).unwrap(),
            broken_bytes,
            "the rc is untouched by the verbs"
        );
        assert!(
            !loader_text().contains("plugins=('git')"),
            "the disable landed in the loader: {}",
            loader_text()
        );
        assert!(
            loader_text().contains("OSH_THEME='agnoster'"),
            "{}",
            loader_text()
        );

        // The scan reports the malformed span honestly.
        let blocks = super::super::loader::scan_legacy_blocks(&rc_text()).unwrap();
        assert!(
            blocks.iter().any(|b| b.id == "oh-my-bash" && b.end_missing),
            "{blocks:?}"
        );

        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// Duplicate managed blocks (whole-file backup-restore concatenation):
    /// sync must not call the file "in sync" — it reports the anomaly and
    /// collapses the duplicates into one canonical block — and a source
    /// disable removes EVERY matching pair, leaving zero residue (wt83 #175).
    /// Duplicate legacy blocks (whole-file backup-restore concatenation)
    /// are the USER's file state: no verb collapses or removes them anymore
    /// (niubash#359), the loader generation ignores them, and the scan
    /// reports every span so migrate-rc can name them for hand removal.
    #[test]
    fn duplicate_legacy_blocks_are_reported_and_left_to_the_user() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("dup-origin");
        write_omb_fixture(&origin);
        let box_ = sandbox("duplicate");
        install(&origin);
        enable("git").expect("loader published first");
        let _ = loader_text();

        // Seed the write-era block, then duplicate the whole rc onto itself.
        seed_legacy_block("oh-my-bash", OMB_BLOCK);
        let one = rc_text();
        fs::write(rc_file(), format!("{one}{one}")).unwrap();
        let count = |needle: &str| rc_text().matches(needle).count();
        assert_eq!(count(">>> niu source oh-my-bash"), 2, "{}", rc_text());
        let duplicated = fs::read(rc_file()).unwrap();

        // Sync does not repair the file: the rc is untouched, the loader
        // generation is the (unchanged) activation plane.
        let report = super::super::sync::sync_spec(super::super::sync::SyncOptions::default())
            .expect("sync runs");
        assert!(
            !report.rows.iter().any(|row| row.action == "failed"),
            "{:?}",
            report.rows
        );
        assert_eq!(fs::read(rc_file()).unwrap(), duplicated, "no rc rewrite");
        assert!(
            loader_text().contains("plugins=('git')"),
            "{}",
            loader_text()
        );

        // The scan reports BOTH spans with their line ranges.
        let blocks = super::super::loader::scan_legacy_blocks(&rc_text()).unwrap();
        let spans: Vec<(usize, usize)> = blocks
            .iter()
            .filter(|b| b.id == "oh-my-bash")
            .map(|b| (b.start_line, b.end_line))
            .collect();
        assert_eq!(spans.len(), 2, "{blocks:?}");
        assert!(spans[0].1 < spans[1].0, "disjoint spans: {blocks:?}");

        // Disabling the source drops the LOADER section; the rc keeps its
        // duplicated blocks (removal stays the user's edit).
        disable("oh-my-bash").expect("source disable");
        assert!(
            !loader_text().contains("niu plugin source: oh-my-bash"),
            "{}",
            loader_text()
        );
        assert_eq!(fs::read(rc_file()).unwrap(), duplicated, "still no rewrite");

        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#176 (CRLF): a rc the user maintains with CRLF endings stays
    /// CRLF through a managed-block rewrite — no whole-file flattening to
    /// LF, and byte-identical rewrites do not churn the file at all.
    /// niubash#176 (CRLF) under the read-only regime: a rc the user
    /// maintains with CRLF endings is never rewritten at all, so its line
    /// endings survive trivially -- and the loader carries the selection.
    #[test]
    fn crlf_rc_is_never_touched_by_verbs() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("crlf-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("crlf");
        fs::write(rc_file(), "alias ll='ls -l'\r\nset -o vi\r\n").unwrap();

        install(&origin);
        enable("git").expect("enable publishes the loader");

        let text = rc_text();
        assert!(text.contains("alias ll='ls -l'"), "{text:?}");
        assert!(!text.contains("plugins="), "no rc write happened: {text:?}");
        let stray_lf = text.replace("\r\n", "").contains('\n');
        assert!(!stray_lf, "every line stays CRLF: {text:?}");
        assert!(
            loader_section("oh-my-bash").contains("plugins=('git')"),
            "{}",
            loader_text()
        );

        // A repeated enable is a no-op for the rc and byte-stable for the
        // loader.
        let before = fs::read(rc_file()).unwrap();
        let loader_before = fs::read(super::super::loader::loader_path()).unwrap();
        enable("git").expect("second enable");
        assert_eq!(fs::read(rc_file()).unwrap(), before, "no churn");
        assert_eq!(
            fs::read(super::super::loader::loader_path()).unwrap(),
            loader_before,
            "byte-stable generation"
        );

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#176 (silent drop): a user line hand-written INSIDE a managed
    /// block is migrated out (right below it, under a marker comment) when
    /// the block is rewritten — never silently discarded.
    /// niubash#176 (silent drop), read-only edition: a user line
    /// hand-written INSIDE a legacy managed block stays exactly where the
    /// user put it -- no verb rewrites the rc, so nothing can drop or move
    /// it. The scan still sees the block; migrate-rc's guidance tells the
    /// user what to keep when THEY remove it.
    #[test]
    fn user_lines_inside_a_legacy_block_stay_untouched() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("migrate-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("migrate");
        install(&origin);
        enable("git").expect("loader published first");

        // The user hand-writes a line inside the legacy block.
        seed_legacy_block(
            "oh-my-bash",
            "plugins=('git')\nalias hh='history'\n. \"$OSH/oh-my-bash.sh\"",
        );
        let seeded = fs::read(rc_file()).unwrap();

        // A selection change touches the loader only; the alias survives
        // inside the block, byte for byte.
        enable("cargo").expect("enable publishes the loader");
        let text = rc_text();
        assert_eq!(fs::read(rc_file()).unwrap(), seeded, "rc untouched");
        assert!(text.contains("alias hh='history'"), "kept: {text}");
        assert!(
            loader_section("oh-my-bash").contains("aliases=('cargo')"),
            "{}",
            loader_text()
        );
        assert!(
            loader_section("oh-my-bash").contains("plugins=('git')"),
            "the enable is additive; the managed line stays managed: {}",
            loader_text()
        );

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#176 (near markers): a hand-edited marker block (missing a
    /// `>` / `<`) is repaired into the exact managed form — a fresh exact
    /// block is appended BESIDE it, never duplicating the loader.
    /// niubash#176 (near markers), read-only edition: a hand-edited marker
    /// block (missing a `>`) is never repaired in place anymore -- the rc
    /// keeps the user's bytes -- and the scan still recognizes the near
    /// shape so the double-execution guard and migrate-rc see it.
    #[test]
    fn near_marker_legacy_block_is_reported_never_repaired() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("near-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("near");
        install(&origin);
        enable("git").expect("loader published first");

        // Seed a block whose begin marker lost one bracket; the end stays
        // exact (the write-era near-marker shape).
        let mut text = rc_text();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("# >> niu source oh-my-bash >>>\n");
        text.push_str(OMB_BLOCK);
        text.push('\n');
        text.push_str("# <<< niu source oh-my-bash <<<\n");
        fs::write(rc_file(), &text).unwrap();
        let seeded = fs::read(rc_file()).unwrap();
        let count = |needle: &str| rc_text().matches(needle).count();
        assert_eq!(count("# >> niu source oh-my-bash"), 1);

        // A rewrite consumes nothing: the near-marker span stays as-is.
        enable("cargo").expect("enable after the hand edit");
        assert_eq!(fs::read(rc_file()).unwrap(), seeded, "no rc rewrite");
        assert_eq!(count("# >> niu source oh-my-bash"), 1);
        assert_eq!(count("# >>> niu source oh-my-bash"), 0);
        assert!(
            loader_section("oh-my-bash").contains("aliases=('cargo')"),
            "{}",
            loader_text()
        );

        // The scan reports the near span (exact = false).
        let blocks = super::super::loader::scan_legacy_blocks(&rc_text()).unwrap();
        assert!(
            blocks
                .iter()
                .any(|b| b.id == "oh-my-bash" && !b.exact && !b.end_missing),
            "{blocks:?}"
        );

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#176 (backups): every plugin-verb rc rewrite backs the rc up
    /// into the wizard's backups dir, and the backup set is pruned to the
    /// most recent RC_BACKUP_KEEP files (the wizard era never cleaned up).
    /// niubash#176 (backups), read-only edition: plugin verbs no longer
    /// rewrite the rc, so they no longer create backups either -- and they
    /// leave an existing backup set (including stale ones) exactly as the
    /// user kept it.
    #[test]
    fn plugin_verbs_never_touch_the_rc_or_its_backups() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder = unique_temp_dir("backup-holder");
        let origin = holder.join("oh-my-fixture");
        fs::create_dir_all(&origin).unwrap();
        write_omb_fixture(&origin);
        let box_ = sandbox("backup");
        fs::write(rc_file(), "alias ll='ls -l'\n").unwrap();
        let backup_dir = rc_backup_dir().unwrap();
        // A stale backup set from the write era.
        fs::create_dir_all(&backup_dir).unwrap();
        let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for index in 0..12 {
            let path = backup_dir.join(format!(".niubashrc.stale-{index}.bak"));
            fs::write(&path, "stale\n").unwrap();
            let file = fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(stale).unwrap();
        }
        let backup_names = || {
            let mut names: Vec<String> = fs::read_dir(&backup_dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.ends_with(".bak"))
                })
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        let before = backup_names();

        install(&origin);
        enable("git").expect("loader publish");
        enable("cargo").expect("second loader publish");
        disable("git").expect("disable");

        assert_eq!(
            backup_names(),
            before,
            "the backup set is untouched: no new backup, no pruning"
        );
        assert!(
            loader_text().contains("aliases=('cargo')"),
            "the verbs still worked: {}",
            loader_text()
        );

        let _ = fs::remove_dir_all(&holder);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// niubash#304: while the managed-block writer rewrites the rc, a
    /// concurrent reader must never observe an empty/missing window. The
    /// rewrite swaps the file in whole (same-dir temp + replace rename;
    /// Windows MoveFileExW REPLACE_EXISTING), so every read either fails
    /// to race at all or lands on a complete old/new generation carrying
    /// the sentinel body. A truncate-in-place writer would strand reads
    /// with an empty/partial body; the wizard's former
    /// remove-then-rename would strand them with NotFound.
    #[test]
    fn concurrent_reader_never_sees_an_empty_rc_window() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sandbox = sandbox("atomic-rc-reader");
        let rc = rc_file();
        let filler = "# user filler line padding the rc body\n".repeat(4000);
        fs::write(&rc, format!("{filler}SENTINEL-RC-BODY-END\n")).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let violations = Arc::new(Mutex::new(Vec::new()));
        let reader = {
            let stop = Arc::clone(&stop);
            let violations = Arc::clone(&violations);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match fs::read_to_string(&rc) {
                        Ok(text) => {
                            if text.is_empty() || !text.contains("SENTINEL-RC-BODY-END") {
                                violations.lock().unwrap().push(format!(
                                    "window: len={} sentinel={} head={:?}",
                                    text.len(),
                                    text.contains("SENTINEL-RC-BODY-END"),
                                    text.chars().take(40).collect::<String>(),
                                ));
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            violations
                                .lock()
                                .unwrap()
                                .push("window: rc momentarily absent".to_string());
                        }
                        Err(_) => {}
                    }
                }
            })
        };

        for generation in 0..120 {
            let id = format!("gen{generation}");
            let block = format!(
                "{begin}\n. \"$HOME/.niubash/sources/{id}.sh\"\n{end}\n",
                begin = begin_marker(&id),
                end = end_marker(&id),
            );
            let (path, _) = write_managed_block(&begin_marker(&id), &end_marker(&id), &block)
                .expect("managed rewrite");
            let text = fs::read_to_string(&path).unwrap();
            assert!(
                text.contains("SENTINEL-RC-BODY-END"),
                "writer's own generation {generation} lost the user body"
            );
        }

        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        let violations = violations.lock().unwrap();
        assert!(
            violations.is_empty(),
            "concurrent readers observed an empty/missing rc window: {:#?}",
            *violations
        );
        let _ = fs::remove_dir_all(&_sandbox.temp);
    }

    // ── niubash#324: moved-out user lines are moved, never copied ───────────

    /// The moved-out drift-guard fragment exactly as the corrupt rc of
    /// niubash#324 carries it: the niubash#297 static guard whose closing
    /// `fi` the old copy boundary recognized as managed structure and left
    /// behind. `with_fi` restores the complete shape.
    fn moved_out_guard(id: &str, with_fi: bool) -> String {
        format!(
            "# drift guard (static): `niu plugin sync` lists drifted ids in drifted-sources.txt\n\
             _niu_drift=\"${{NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}}/drifted-sources.txt\"\n\
             if [ -s \"$_niu_drift\" ] && command -v grep >/dev/null 2>&1 && grep -qx '{id}' \
             \"$_niu_drift\" 2>/dev/null; then\n\
             \x20 echo \"niubash plugin: source {id} no longer matches its lockfile pin — run: \
             niu plugin source verify {id}; repair with: niu plugin restore {id}\" >&2\n\
             {}unset _niu_drift\n",
            if with_fi { "fi\n" } else { "" }
        )
    }

    fn if_fi_balance(text: &str) -> (usize, usize) {
        let opens = text
            .lines()
            .filter(|line| line.trim_start().starts_with("if "))
            .count();
        let closes = text.lines().filter(|line| line.trim() == "fi").count();
        (opens, closes)
    }

    /// THE niubash#324 repro (the owner's real-machine rc): after the legit
    /// managed oh-my-bash block, SIX consecutive copies of the moved-out
    /// header + guard fragment, only the last carrying its `fi` — five
    /// dangling `if`s, a syntax error on every startup. Every plugin verb
    /// (the startup `sync --bootstrap` among them) must CONVERGE the file:
    /// exactly one complete guard, inside the managed block, zero
    /// moved-out sections, if/fi balanced file-wide — and then hold that
    /// state byte-stably across further verb rounds.
    /// THE niubash#324 repro, read-only edition (gate case 359.6): after
    /// the canonical oh-my-bash block, SIX consecutive copies of the
    /// moved-out header + guard fragment, only the last carrying its `fi`
    /// -- five dangling `if`s, a syntax error at every startup. The write
    /// era CONVERGED this file on every verb; the read-only era PRESERVES
    /// it: every plugin verb round leaves the corrupt bytes identical,
    /// fixes nothing, worsens nothing -- the repair is the user's
    /// (migrate-rc reports; the doctor names the debris).
    #[test]
    fn corrupt_owner_rc_is_preserved_byte_identically_through_plugin_ops() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("i324-origin");
        write_omb_fixture(&origin);
        let box_ = sandbox("i324-converge");
        install(&origin);
        enable("git").expect("loader generation first");

        // The corrupt fixture: write-era block + 5 fi-less fragments + the
        // final complete copy (issue #324's exact shape).
        seed_legacy_block("oh-my-bash", OMB_BLOCK);
        let base = rc_text().trim_end().to_string();
        let mut corrupt = base.clone();
        for with_fi in [false, false, false, false, false, true] {
            corrupt.push('\n');
            corrupt.push_str(MIGRATED_USER_LINES_HEADER);
            corrupt.push('\n');
            corrupt.push_str(&moved_out_guard("oh-my-bash", with_fi));
        }
        fs::write(rc_file(), &corrupt).unwrap();
        let corrupt_bytes = fs::read(rc_file()).unwrap();
        let count = |needle: &str| rc_text().matches(needle).count();
        assert_eq!(count("moved out of a managed block"), 6, "{}", rc_text());
        let (opens, closes) = if_fi_balance(&rc_text());
        assert_eq!(opens - closes, 5, "five dangling ifs seeded: {}", rc_text());

        // Six rounds of the plugin verbs, sync/enable/disable alternating:
        // the rc stays byte-identical through every round.
        for _round in 0..6 {
            super::super::sync::sync_spec(super::super::sync::SyncOptions::default())
                .expect("sync");
            disable("git").expect("asset disable");
            enable("git").expect("asset enable");
            disable("oh-my-bash").expect("source disable");
            enable("oh-my-bash").expect("source enable");
            assert_eq!(
                fs::read(rc_file()).unwrap(),
                corrupt_bytes,
                "the corrupt rc is preserved through the verb round"
            );
        }
        super::super::sync::sync_spec(super::super::sync::SyncOptions::default())
            .expect("final sync");
        assert_eq!(
            fs::read(rc_file()).unwrap(),
            corrupt_bytes,
            "the corrupt rc is preserved by sync"
        );

        // The loader is still the working activation plane (the
        // double-execution guard keeps it off while the legacy markers
        // remain -- that is the correct transitional state).
        assert!(
            loader_section("oh-my-bash").contains("plugins=('git')"),
            "the loader generation carries the selection: {}",
            loader_text()
        );
        assert_eq!(
            count("moved out of a managed block"),
            6,
            "nothing was cleaned: the file is the user's"
        );

        // Fixed point: a further identical verb round changes nothing.
        super::super::sync::sync_spec(super::super::sync::SyncOptions::default()).unwrap();
        disable("git").unwrap();
        enable("git").unwrap();
        disable("oh-my-bash").unwrap();
        enable("oh-my-bash").unwrap();
        super::super::sync::sync_spec(super::super::sync::SyncOptions::default()).unwrap();
        assert_eq!(fs::read(rc_file()).unwrap(), corrupt_bytes, "fixed point");

        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// The healthy #176 shape (managed block + ONE moved-out section) must
    /// never GROW across a plugin-verb loop: the section's guard copy is
    /// niu debris, so the first verb converges it away (the complete guard
    /// is re-rendered inside the block), and from then on the whole file
    /// is byte-stable with its `fi` intact — the loop regression the issue
    /// asks for.
    /// The healthy #176 shape (legacy block + ONE moved-out section) must
    /// never GROW across a plugin-verb loop. In the write era the verb
    /// converged the debris; in the read-only era the whole file is simply
    /// byte-stable from the first round -- no verb touches it.
    #[test]
    fn moved_out_guard_region_stays_byte_stable_across_verb_loops() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("i324-healthy");
        write_omb_fixture(&origin);
        let box_ = sandbox("i324-stable");
        install(&origin);
        enable("git").expect("loader generation first");
        seed_legacy_block("oh-my-bash", OMB_BLOCK);
        let base = rc_text().trim_end().to_string();
        let healthy = format!(
            "{base}\n{}\n{}",
            MIGRATED_USER_LINES_HEADER,
            moved_out_guard("oh-my-bash", true)
        );
        fs::write(rc_file(), &healthy).unwrap();
        let seeded = fs::read(rc_file()).unwrap();

        for _round in 0..6 {
            super::super::sync::sync_spec(super::super::sync::SyncOptions::default()).unwrap();
            disable("git").unwrap();
            enable("git").unwrap();
            disable("oh-my-bash").unwrap();
            enable("oh-my-bash").unwrap();
            assert_eq!(
                fs::read(rc_file()).unwrap(),
                seeded,
                "user-lines region byte-stable across the loop"
            );
        }
        let (opens, closes) = if_fi_balance(&rc_text());
        assert_eq!(opens, closes, "fi intact: {}", rc_text());

        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// Move, not copy (niubash#324): a user line that comes back INSIDE the
    /// block (restore-from-backup, an old installer, a hand edit) and is
    /// migrated a second time merges into the existing migrated section —
    /// the pile never grows a duplicate section per verb.
    /// Move, not copy (niubash#324), read-only edition: a user line that
    /// sits inside the block AND in an existing migrated section is never
    /// duplicated by a verb -- because no verb rewrites the file at all.
    /// The pile stays exactly as the user left it.
    #[test]
    fn re_migrated_user_line_region_is_never_touched_again() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("i324-dedupe");
        write_omb_fixture(&origin);
        let box_ = sandbox("i324-dedupe");
        install(&origin);
        enable("git").expect("loader generation first");

        // The alias was already migrated out once; the SAME line is back
        // inside the legacy block.
        seed_legacy_block(
            "oh-my-bash",
            &format!(
                "plugins=('git')\nalias hh='history'\n. \"$OSH/oh-my-bash.sh\"\n{}\nalias hh='history'",
                MIGRATED_USER_LINES_HEADER
            ),
        );
        let seeded = fs::read(rc_file()).unwrap();

        enable("cargo").expect("the enable that would once have re-migrated");
        assert_eq!(fs::read(rc_file()).unwrap(), seeded, "rc untouched");
        let text = rc_text();
        assert_eq!(
            text.matches("moved out of a managed block").count(),
            1,
            "one migrated section, not one per verb: {text}"
        );
        assert_eq!(
            text.matches("alias hh='history'").count(),
            2,
            "the user's own copies, untouched: {text}"
        );
        assert!(
            loader_section("oh-my-bash").contains("aliases=('cargo')"),
            "{}",
            loader_text()
        );

        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }

    /// The conservative edge (niubash#324): a migrated section mixing
    /// genuine user lines with guard debris is kept VERBATIM — whole
    /// sections are dropped, never line-wise edits that could orphan a
    /// user's own if/fi pair. Pure debris sections (the owner's file) go
    /// entirely; user lines are never touched by the convergence.
    #[test]
    fn mixed_user_and_debris_pile_section_is_kept_verbatim() {
        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_temp_dir("i324-mixed");
        write_omb_fixture(&origin);
        let box_ = sandbox("i324-mixed");
        install(&origin);
        enable("git").expect("canonical block first");

        let mixed_section = format!(
            "{}\nalias uu='uptime'\n{}",
            MIGRATED_USER_LINES_HEADER,
            moved_out_guard("oh-my-bash", false)
        );
        let seeded = format!("{}\n{}\n", rc_text().trim_end(), mixed_section);
        fs::write(rc_file(), &seeded).unwrap();

        enable("cargo").expect("a rewrite runs the normalizer");
        let text = rc_text();
        assert!(
            text.contains("alias uu='uptime'"),
            "the user line survives verbatim: {text}"
        );
        assert_eq!(
            text.matches("moved out of a managed block").count(),
            1,
            "the mixed section is kept, not dropped: {text}"
        );
        assert_eq!(text.matches("alias uu='uptime'").count(), 1, "{text}");

        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&box_.temp);
    }
}
