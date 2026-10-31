//! `niu plugin tui` — the full-screen plugin manager (niubash#186).
//!
//! The lazy.nvim-shaped sibling of [`super::ui`]: same inventory, same
//! verbs, same discipline — the UI is a *view layer* and every mutation
//! goes through [`super::ui::apply_verb`], the one dispatch the CLI verbs
//! and the menu UI share. What this module adds is presentation only:
//!
//! - **M1** — crossterm alternate-buffer full screen with its own render
//!   loop (16ms tick), the [`super::ui::inventory`] sections as the list
//!   (category structure = section headers), single-key drive from one
//!   command table ([`TUI_KEYS`], the [`super::ui::UI_VERBS`] discipline:
//!   help is generated from the table so the two cannot drift), and a
//!   persistent help bar. Install/update/remove run on a worker thread;
//!   the loop never blocks on them, and the inventory is rebuilt when a
//!   job lands (the menu UI's rebuild-after-action rule).
//! - **M2** — a preview pane with a rounded border: beside the list on
//!   wide terminals, floating over it on narrow ones. Theme rows show the
//!   *live* prompt preview through the shared
//!   [`super::theme_preview::GalleryPreviews`] cache (isolated subprocess
//!   renders, sandboxed HOME, bounded timeout — the wt77 pipeline);
//!   source rows render the installed tree's README; recipe/collection
//!   rows render their index card (the static mode).
//!
//! Entering the TUI switches to the alternate screen and raw mode;
//! leaving (quit key, error, or panic unwind) restores both — the shell
//! session underneath is never touched. M3 (async task panel, fuzzy
//! search) and M4 (git status column, category tabs) are later phases of
//! #186; the `/` key and the task panel are deliberately absent here.
//!
//! The `t` key from the original #186 keymap is intentionally *unbound*:
//! it named the trust action, and the trust system was removed from the
//! product (owner red line 2026-10-28, removal PR #650) — there is no
//! trust state left to toggle. Pressing it explains exactly that.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, terminal,
};
use unicode_width::UnicodeWidthChar;

use super::theme_preview::GalleryPreviews;
use super::{catalog, distros, recipes, sources, ui};
use ui::{UiRow, UiRowKind, UiSection};

/// Render-loop tick (the #186 architecture fixes 16ms).
pub const TICK: Duration = Duration::from_millis(16);

/// At or above this terminal width the preview pane sits beside the list;
/// below it the preview floats over the list.
const SIDE_PANE_MIN_WIDTH: u16 = 90;

// ---------------------------------------------------------------------------
// Command table (the TUI_KEYS discipline: help is generated from this table)
// ---------------------------------------------------------------------------

/// One row of the TUI key table. `verb` is the [`ui::apply_verb`] id the
/// key dispatches for the row kinds it applies to, when it has one fixed
/// verb; context-dependent keys (Enter) describe themselves in `label`.
#[derive(Debug)]
pub struct TuiKey {
    pub key: &'static str,
    pub label: &'static str,
}

/// The single-key command table. The help bar is generated from this —
/// add a key here and the bar, the docs and the dispatch stay in step.
pub const TUI_KEYS: &[TuiKey] = &[
    TuiKey {
        key: "j/k",
        label: "move",
    },
    TuiKey {
        key: "i",
        label: "install",
    },
    TuiKey {
        key: "u",
        label: "update",
    },
    TuiKey {
        key: "x",
        label: "remove",
    },
    TuiKey {
        key: "Enter",
        label: "primary",
    },
    TuiKey {
        key: "p",
        label: "preview",
    },
    TuiKey {
        key: "q",
        label: "quit",
    },
];

/// The persistent help bar line, generated from [`TUI_KEYS`].
pub fn help_bar() -> String {
    TUI_KEYS
        .iter()
        .map(|entry| format!("{} {}", entry.key, entry.label))
        .collect::<Vec<_>>()
        .join(" · ")
}

// ---------------------------------------------------------------------------
// Model: pure state machine (no IO — the driver feeds it and executes effects)
// ---------------------------------------------------------------------------

/// What a key press asks the driver to do. The model never performs IO;
/// the driver runs verbs on a worker thread and rebuilds the inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiEffect {
    /// Nothing for the driver to do (pure model change, already applied).
    None,
    /// Leave the TUI.
    Quit,
    /// The selection moved: refresh the preview pane for the new row.
    SelectionChanged,
    /// Run one verb through [`ui::apply_verb`] (driver: worker thread).
    RunVerb { verb: &'static str, target: String },
}

/// Which static/live content the preview pane shows for the selected row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewKind {
    /// Index card / file content computed once per selection.
    Static,
    /// Live PS1 render of one theme, refreshed from the preview cache.
    LivePs1 { source_id: String, name: String },
}

/// The preview pane's content for the current selection.
#[derive(Debug, Clone)]
pub struct PreviewData {
    /// Pane title suffix: the row id (plus mode is rendered from `kind`).
    pub title: String,
    pub kind: PreviewKind,
    pub lines: Vec<String>,
}

/// One display line of the flattened list: a section header or a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlatRef {
    Header(usize),
    Row(usize, usize),
}

/// The TUI state. All transitions go through [`TuiModel::handle_key`];
/// rendering is a pure function of the model plus terminal geometry.
pub struct TuiModel {
    sections: Vec<UiSection>,
    flat: Vec<FlatRef>,
    /// Index into `flat` of the selected row (always a `FlatRef::Row`
    /// when any row exists).
    cursor: usize,
    /// First visible flat line (the driver/render keeps it cursor-true).
    scroll: usize,
    pub preview_visible: bool,
    pub preview: Option<PreviewData>,
    /// Last action outcome or hint (status line, below the list).
    pub status: Option<String>,
    /// Label of the verb currently running on the worker thread.
    pub busy: Option<String>,
    /// Armed destructive confirmation: the target id a second `x` removes.
    confirm_remove: Option<String>,
    /// Monotonic tick count (spinner animation; the driver advances it).
    pub tick: u64,
    pub quit: bool,
}

impl TuiModel {
    pub fn new(sections: Vec<UiSection>) -> Self {
        let mut model = TuiModel {
            sections,
            flat: Vec::new(),
            cursor: 0,
            scroll: 0,
            preview_visible: true,
            preview: None,
            status: None,
            busy: None,
            confirm_remove: None,
            tick: 0,
            quit: false,
        };
        model.rebuild_flat(None);
        model
    }

    /// Replace the inventory (after a job lands, the view shows live
    /// state). The selection follows the previously selected row id when
    /// it still exists, and clamps into range when it does not.
    pub fn set_sections(&mut self, sections: Vec<UiSection>) {
        let keep = self.selected_row().map(|row| row.id.clone());
        self.sections = sections;
        self.rebuild_flat(keep.as_deref());
    }

    fn rebuild_flat(&mut self, keep_id: Option<&str>) {
        self.flat = Vec::new();
        for (section_index, section) in self.sections.iter().enumerate() {
            self.flat.push(FlatRef::Header(section_index));
            for row_index in 0..section.rows.len() {
                self.flat.push(FlatRef::Row(section_index, row_index));
            }
        }
        self.cursor = self
            .flat
            .iter()
            .position(|entry| matches!(entry, FlatRef::Row(..)))
            .unwrap_or(0);
        if let Some(id) = keep_id {
            if let Some(position) = self.flat.iter().position(|entry| match entry {
                FlatRef::Row(section, row) => self.sections[*section].rows[*row].id == id,
                FlatRef::Header(_) => false,
            }) {
                self.cursor = position;
            }
        }
        self.scroll = 0;
    }

    /// The currently selected row, if the inventory has any.
    pub fn selected_row(&self) -> Option<&UiRow> {
        match self.flat.get(self.cursor) {
            Some(FlatRef::Row(section, row)) => self.sections.get(*section)?.rows.get(*row),
            _ => None,
        }
    }

    /// Whether the selected row lives in the global-verbs pseudo section.
    fn selected_is_global(&self) -> bool {
        match self.flat.get(self.cursor) {
            Some(FlatRef::Row(section, _)) => self.sections.get(*section).is_some_and(|s| s.global),
            _ => false,
        }
    }

    fn row_positions(&self) -> Vec<usize> {
        self.flat
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| matches!(entry, FlatRef::Row(..)).then_some(index))
            .collect()
    }

    /// Move the selection by `delta` selectable rows (headers skipped,
    /// clamped at both ends).
    fn move_by(&mut self, delta: isize) {
        let positions = self.row_positions();
        if positions.is_empty() {
            return;
        }
        let rank = positions
            .iter()
            .position(|position| *position == self.cursor)
            .unwrap_or(0);
        let next = (rank as isize + delta).clamp(0, positions.len() as isize - 1) as usize;
        self.cursor = positions[next];
    }

    fn move_to_rank(&mut self, rank: usize) {
        let positions = self.row_positions();
        if let Some(position) = positions.get(rank) {
            self.cursor = *position;
        }
    }

    /// Keep `scroll` such that the cursor line is inside the viewport of
    /// `viewport_rows` flat lines. When scrolling up lands on a section's
    /// first row, the section header scrolls in with it: the header is
    /// that row's context line, and the reset view (`rebuild_flat`,
    /// scroll 0 with the selection on the first row) shows it — the
    /// scrolled-up view of the same selection must match.
    pub fn ensure_visible(&mut self, viewport_rows: usize) {
        if viewport_rows == 0 {
            return;
        }
        if self.cursor < self.scroll {
            self.scroll = if matches!(self.flat.get(self.cursor), Some(FlatRef::Row(_, 0))) {
                self.cursor.saturating_sub(1)
            } else {
                self.cursor
            };
        } else if self.cursor >= self.scroll + viewport_rows {
            self.scroll = self.cursor + 1 - viewport_rows;
        }
        let max_scroll = self.flat.len().saturating_sub(viewport_rows);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    /// The verb the given key dispatches for the selected row, if any.
    /// `(verb, needs_confirm)` — removal confirms first, everything else
    /// runs immediately. Pure table work, shared by dispatch and tests.
    fn verb_for_key(&self, key: char) -> Option<(&'static str, bool)> {
        let row = self.selected_row()?;
        if self.selected_is_global() {
            return match key {
                'u' | '\n' => Some(("sync", false)),
                _ => None,
            };
        }
        match (&row.kind, key) {
            (
                UiRowKind::Recipe {
                    installable: true, ..
                },
                'i',
            ) => Some(("add", false)),
            (
                UiRowKind::Recipe {
                    installable: true, ..
                },
                '\n',
            ) => Some(("add", false)),
            (UiRowKind::Source { .. }, 'u') => Some(("update", false)),
            (UiRowKind::Source { .. }, '\n') => Some(("update", false)),
            (UiRowKind::Source { .. }, 'x') => Some(("remove-source", true)),
            (UiRowKind::Theme { .. }, '\n') => Some(("enable", false)),
            // Disabling a theme is its reversible "remove": no confirm.
            (UiRowKind::Theme { .. }, 'x') => Some(("disable", false)),
            (UiRowKind::Collection { .. }, 'i') => Some(("apply", false)),
            (UiRowKind::Collection { .. }, '\n') => Some(("apply", false)),
            _ => None,
        }
    }

    fn hint_for_key(&self, key: char) -> String {
        let Some(row) = self.selected_row() else {
            return "nothing to act on — the inventory is empty".into();
        };
        match (&row.kind, key) {
            (
                UiRowKind::Recipe {
                    installable: false, ..
                },
                'i' | '\n',
            ) => format!(
                "'{}' has no install driver — `niu plugin add {}` prints the package-manager route",
                row.id, row.id
            ),
            (UiRowKind::Recipe { .. }, 'u') => {
                format!("'{}' is not installed yet — press i to install", row.id)
            }
            (UiRowKind::Recipe { .. }, 'x') => {
                format!("'{}' is not installed — nothing to remove", row.id)
            }
            (UiRowKind::Source { .. }, 'i') => format!("'{}' is already installed", row.id),
            (UiRowKind::Theme { .. }, 'i') => {
                format!(
                    "themes activate, they do not install — Enter enables '{}'",
                    row.id
                )
            }
            (UiRowKind::Theme { .. }, 'u') => {
                "themes update with their source — select the source row and press u".into()
            }
            (UiRowKind::Collection { .. }, 'u' | 'x') => {
                "collections only apply — Enter installs every member".into()
            }
            _ => format!("that key does not apply to '{}'", row.id),
        }
    }

    /// One key press. `viewport_rows` is the list viewport height in flat
    /// lines (page moves and scroll maintenance need it).
    pub fn handle_key(&mut self, key: KeyEvent, viewport_rows: usize) -> TuiEffect {
        // Key-release events (Windows terminals report them) never act.
        if key.kind == KeyEventKind::Release {
            return TuiEffect::None;
        }
        if self.quit {
            return TuiEffect::None;
        }
        // Ctrl-C always leaves, busy or not (the worker is detached; its
        // verb keeps the same semantics as the CLI call it mirrors).
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return TuiEffect::Quit;
        }
        let before = self.cursor;
        let mut effect = TuiEffect::None;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.quit = true;
                return TuiEffect::Quit;
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(viewport_rows.max(1) as isize),
            KeyCode::PageUp => self.move_by(-(viewport_rows.max(1) as isize)),
            KeyCode::Char('g') | KeyCode::Home => self.move_to_rank(0),
            KeyCode::Char('G') | KeyCode::End => {
                let last = self.row_positions().len().saturating_sub(1);
                self.move_to_rank(last);
            }
            KeyCode::Char('p') => {
                self.preview_visible = !self.preview_visible;
                if self.preview_visible {
                    effect = TuiEffect::SelectionChanged;
                }
            }
            KeyCode::Char('t') => {
                self.status = Some(
                    "t is unbound: the trust system was removed from the product \
                     (2026-10-28) — there is no trust state to toggle"
                        .into(),
                );
            }
            KeyCode::Char('/') => {
                self.status = Some("fuzzy search arrives with M3 of #186".into());
            }
            KeyCode::Enter => return self.dispatch('\n', viewport_rows),
            KeyCode::Char(c @ ('i' | 'u' | 'x')) => return self.dispatch(c, viewport_rows),
            _ => {}
        }
        if self.cursor != before {
            self.confirm_remove = None;
            self.ensure_visible(viewport_rows);
            return TuiEffect::SelectionChanged;
        }
        effect
    }

    /// Action keys: resolve the verb for the selected row and either run
    /// it, arm its confirmation, or explain why it does not apply.
    fn dispatch(&mut self, key: char, viewport_rows: usize) -> TuiEffect {
        let Some(row) = self.selected_row().cloned() else {
            self.status = Some("nothing to act on — the inventory is empty".into());
            return TuiEffect::None;
        };
        if self.busy.is_some() {
            self.status = Some(format!(
                "busy: {} — wait for it to land before the next verb",
                self.busy.as_deref().unwrap_or_default()
            ));
            return TuiEffect::None;
        }
        let Some((verb, needs_confirm)) = self.verb_for_key(key) else {
            self.status = Some(self.hint_for_key(key));
            return TuiEffect::None;
        };
        if needs_confirm && self.confirm_remove.as_deref() != Some(row.id.as_str()) {
            self.confirm_remove = Some(row.id.clone());
            self.status = Some(format!(
                "remove '{}' — its tree and registry record are deleted. Press x again to confirm, any move to cancel",
                row.id
            ));
            return TuiEffect::None;
        }
        self.confirm_remove = None;
        let _ = viewport_rows;
        self.busy = Some(format!("{verb} {}", row.id));
        TuiEffect::RunVerb {
            verb,
            target: row.id.clone(),
        }
    }

    /// The worker thread landed: record the outcome; the driver rebuilds
    /// the inventory right after this (rebuild-after-action discipline).
    pub fn job_finished(&mut self, result: Result<String, String>) {
        self.busy = None;
        self.status = Some(match result {
            Ok(summary) => summary,
            Err(error) => format!("failed: {error}"),
        });
    }

    /// The text the status line shows: armed confirmation first, then the
    /// running job, then the last outcome/hint.
    pub fn status_line(&self) -> Option<String> {
        if self.confirm_remove.is_some() {
            return self.status.clone();
        }
        if let Some(busy) = &self.busy {
            return Some(format!("working: {busy} …"));
        }
        self.status.clone()
    }
}

// ---------------------------------------------------------------------------
// Rendering: pure functions of model + geometry. A frame is a grid of styled
// spans; the terminal writer translates spans to SGR, tests project them to
// plain text. Width math is unicode-width based and ANSI-aware (live preview
// lines carry the rendered prompt's color codes).
// ---------------------------------------------------------------------------

/// Span style vocabulary of the TUI frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Dim,
    Bold,
    Accent,
    Good,
    Bad,
    Warn,
    Border,
    /// The selected list row (reverse video in the terminal writer).
    CursorRow,
    /// Verbatim terminal bytes (live PS1 lines keep their own colors).
    RawAnsi,
}

/// One styled run of text inside a frame line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    fn new(text: impl Into<String>, style: Style) -> Self {
        Span {
            text: text.into(),
            style,
        }
    }
}

/// A rendered frame: exactly `height` lines, each a span list whose total
/// display width is at most `width`.
pub type Frame = Vec<Vec<Span>>;

/// Display width of a string, ignoring ANSI CSI sequences.
pub fn display_width(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC [ … final byte in @..~ — the sequence is invisible.
            if chars.clone().next() == Some('[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
                continue;
            }
            continue;
        }
        width += UnicodeWidthChar::width(c).unwrap_or(0);
    }
    width
}

/// Remove ANSI CSI sequences (plain-text projection for tests/logs).
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.clone().next() == Some('[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
                continue;
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Clip a string to at most `width` display columns. ANSI sequences are
/// never split; when an ANSI-bearing string is clipped, a reset is
/// appended so the color cannot bleed into the next span.
pub fn clip_line(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(text) <= width {
        return text.to_string();
    }
    let budget = width.saturating_sub(1); // room for the ellipsis
    let mut out = String::new();
    let mut used = 0;
    let mut has_ansi = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            has_ansi = true;
            out.push(c);
            out.push(chars.next().expect("peeked"));
            for next in chars.by_ref() {
                out.push(next);
                if ('\u{40}'..='\u{7e}').contains(&next) {
                    break;
                }
            }
            continue;
        }
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('\u{2026}');
    if has_ansi {
        out.push_str("\u{1b}[0m");
    }
    out
}

/// Pad a string with spaces to exactly `width` display columns (clipping
/// first when it is longer).
pub fn pad_line(text: &str, width: usize) -> String {
    let clipped = clip_line(text, width);
    let missing = width.saturating_sub(display_width(&clipped));
    format!("{clipped}{}", " ".repeat(missing))
}

/// Split one logical line into frame lines of at most `width` columns
/// (preview bodies wrap; list rows clip instead).
fn wrap_line(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for word in text.split_inclusive(' ') {
        let word_width = display_width(word);
        if used + word_width > width && used > 0 {
            lines.push(std::mem::take(&mut current));
            used = 0;
        }
        if word_width > width {
            // A single word longer than the pane: hard-clip it.
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            lines.push(clip_line(word.trim_end(), width));
            used = 0;
            continue;
        }
        current.push_str(word);
        used += word_width;
    }
    lines.push(current.trim_end().to_string());
    lines
}

const SPINNER: &[char] = &[
    '\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280f}',
];

/// Body height in lines for a terminal of `height` rows (title + status +
/// help take three).
fn body_height(height: u16) -> usize {
    (height as usize).saturating_sub(3).max(1)
}

/// Render the whole frame for a terminal of `width`x`height`.
pub fn render(model: &TuiModel, width: u16, height: u16) -> Frame {
    let width = width as usize;
    let height = height as usize;
    if width < 40 || height < 10 {
        let mut frame = vec![Vec::new(); height];
        if height > 0 && width > 0 {
            frame[0] = vec![Span::new(
                pad_line("terminal too small for the plugin TUI (need 40x10)", width),
                Style::Warn,
            )];
        }
        return frame;
    }
    let mut frame: Frame = Vec::with_capacity(height);
    frame.push(title_line(model, width));
    let body = body_height(height as u16);
    let list_lines = render_list(model, width, body);
    if model.preview_visible && model.preview.is_some() {
        if width as u16 >= SIDE_PANE_MIN_WIDTH {
            let list_width = (width * 55 / 100).max(40);
            let pane_width = width - list_width - 1;
            let pane = render_pane(model.preview.as_ref(), pane_width, body);
            for (index, list_line) in list_lines.into_iter().enumerate() {
                let mut composed = spans_to_width(list_line, list_width, Style::Plain);
                composed.push(Span::new(" ", Style::Plain));
                composed.extend(pane[index].clone());
                frame.push(composed);
            }
        } else {
            // Narrow terminal: the list renders full-width and the preview
            // floats over its center.
            let mut lines = list_lines;
            let pane_width = (width * 86 / 100).max(30);
            let pane_height = (body * 70 / 100).max(5).min(body);
            let pane = render_pane(model.preview.as_ref(), pane_width, pane_height);
            let top = (body - pane_height) / 2;
            let left = (width - pane_width) / 2;
            for (offset, pane_line) in pane.into_iter().enumerate() {
                let target = &mut lines[top + offset];
                let mut composed = spans_to_width(target.clone(), left, Style::Plain);
                composed.extend(pane_line);
                *target = composed;
            }
            frame.extend(lines);
        }
    } else {
        frame.extend(list_lines);
    }
    frame.push(status_line_render(model, width));
    frame.push(help_line(width));
    frame.truncate(height);
    while frame.len() < height {
        frame.push(Vec::new());
    }
    frame
}

/// Clip/pad a span list to exactly `width` display columns, preserving
/// styles span by span (the text of each span is clipped ANSI-aware).
/// Padding takes `fill`'s style — the cursor row pads in reverse video
/// so the selection band runs the full row width.
fn spans_to_width(spans: Vec<Span>, width: usize, fill: Style) -> Vec<Span> {
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        if used >= width {
            break;
        }
        let span_width = display_width(&span.text);
        if used + span_width <= width {
            used += span_width;
            out.push(span);
        } else {
            let clipped = clip_line(&span.text, width - used);
            used += display_width(&clipped);
            out.push(Span {
                text: clipped,
                style: span.style,
            });
        }
    }
    if used < width {
        out.push(Span::new(" ".repeat(width - used), fill));
    }
    out
}

fn title_line(model: &TuiModel, width: usize) -> Vec<Span> {
    let installed = model
        .sections
        .iter()
        .flat_map(|section| &section.rows)
        .filter(|row| matches!(row.kind, UiRowKind::Source { .. }))
        .count();
    let total: usize = model
        .sections
        .iter()
        .map(|section| section.rows.len())
        .sum();
    let mut text = format!(
        " niu plugin tui \u{2014} {} rows, {} sources installed ",
        total, installed
    );
    if let Some(busy) = &model.busy {
        let spinner = SPINNER[(model.tick as usize / 4) % SPINNER.len()];
        text.push_str(&format!(" \u{00b7} {spinner} {busy}"));
    }
    vec![Span::new(pad_line(&text, width), Style::Bold)]
}

fn render_list(model: &TuiModel, width: usize, body: usize) -> Vec<Vec<Span>> {
    let mut lines = Vec::with_capacity(body);
    let start = model.scroll.min(model.flat.len());
    for flat_index in start..(start + body).min(model.flat.len()) {
        match model.flat[flat_index] {
            FlatRef::Header(section) => {
                let section_ref = &model.sections[section];
                let text = format!(
                    "\u{25b8} {} ({})",
                    section_ref.title,
                    section_ref.rows.len()
                );
                lines.push(vec![Span::new(
                    pad_line(&text, width),
                    if section_ref.global {
                        Style::Dim
                    } else {
                        Style::Accent
                    },
                )]);
            }
            FlatRef::Row(section, row) => {
                let row_ref = &model.sections[section].rows[row];
                let selected = flat_index == model.cursor;
                let marker = if selected { "\u{276f}" } else { " " };
                let mark_style = match row_ref.state.as_str() {
                    "ready" | "active" => Style::Good,
                    "missing" => Style::Bad,
                    _ => Style::Dim,
                };
                let text_style = if selected {
                    Style::CursorRow
                } else {
                    Style::Plain
                };
                let mut spans = vec![
                    Span::new(format!("{marker} "), text_style),
                    Span::new(
                        format!("[{}] ", row_ref.state),
                        if selected {
                            Style::CursorRow
                        } else {
                            mark_style
                        },
                    ),
                    Span::new(format!("{} \u{2014} ", row_ref.id), text_style),
                    Span::new(
                        row_ref.hint.clone(),
                        if selected {
                            Style::CursorRow
                        } else {
                            Style::Dim
                        },
                    ),
                ];
                if selected {
                    // One style for the whole row keeps reverse video whole.
                    for span in &mut spans {
                        span.style = Style::CursorRow;
                    }
                }
                let fill = if selected {
                    Style::CursorRow
                } else {
                    Style::Plain
                };
                lines.push(spans_to_width(spans, width, fill));
            }
        }
    }
    while lines.len() < body {
        lines.push(Vec::new());
    }
    lines
}

/// The preview pane: rounded border, mode in the top border, body wrapped
/// to the inner width. Exactly `height` lines of `width` columns.
fn render_pane(preview: Option<&PreviewData>, width: usize, height: usize) -> Vec<Vec<Span>> {
    let mut lines = Vec::with_capacity(height);
    if width < 8 || height < 3 {
        return vec![Vec::new(); height];
    }
    let (title, body_lines): (String, Vec<String>) = match preview {
        Some(data) => {
            let mode = match &data.kind {
                PreviewKind::Static => "README",
                PreviewKind::LivePs1 { .. } => "LIVE PS1",
            };
            (
                format!(" Preview \u{2014} {} \u{00b7} {} ", mode, data.title),
                data.lines.clone(),
            )
        }
        None => (" Preview ".to_string(), vec!["(no selection)".into()]),
    };
    let inner = width - 2;
    let title_text = clip_line(&title, inner.saturating_sub(1));
    let dashes = inner.saturating_sub(display_width(&title_text) + 1);
    lines.push(vec![
        Span::new("\u{256d}\u{2500}", Style::Border),
        Span::new(title_text, Style::Border),
        Span::new("\u{2500}".repeat(dashes), Style::Border),
        Span::new("\u{256e}", Style::Border),
    ]);
    let inner_height = height - 2;
    let mut wrapped: Vec<String> = Vec::new();
    for line in &body_lines {
        if line.is_empty() {
            wrapped.push(String::new());
        } else {
            wrapped.extend(wrap_line(line, inner - 1));
        }
    }
    for index in 0..inner_height {
        let text = wrapped.get(index).cloned().unwrap_or_default();
        let is_live = matches!(
            preview.map(|data| &data.kind),
            Some(PreviewKind::LivePs1 { .. })
        );
        // Only lines actually carrying escape bytes go verbatim; plain
        // and blank lines of a live pane render as ordinary spans.
        let span_style = if is_live && text.contains('\u{1b}') {
            Style::RawAnsi
        } else {
            Style::Plain
        };
        lines.push(vec![
            Span::new("\u{2502} ", Style::Border),
            Span::new(pad_line(&text, inner - 1), span_style),
            Span::new("\u{2502}", Style::Border),
        ]);
    }
    lines.push(vec![Span::new(
        format!("\u{2570}{}\u{256f}", "\u{2500}".repeat(inner)),
        Style::Border,
    )]);
    lines
}

fn status_line_render(model: &TuiModel, width: usize) -> Vec<Span> {
    match model.status_line() {
        Some(text) => {
            let style = if model.confirm_remove.is_some() {
                Style::Warn
            } else if model.busy.is_some() {
                Style::Accent
            } else {
                Style::Dim
            };
            vec![Span::new(pad_line(&format!(" {text}"), width), style)]
        }
        None => vec![Span::new(" ".repeat(width), Style::Plain)],
    }
}

fn help_line(width: usize) -> Vec<Span> {
    vec![Span::new(
        pad_line(&format!(" {}", help_bar()), width),
        Style::Dim,
    )]
}

/// Plain-text projection of a frame (what snapshot tests compare and what
/// a log of the screen would show): styles dropped, ANSI stripped.
pub fn frame_plain_text(frame: &Frame) -> Vec<String> {
    frame
        .iter()
        .map(|line| {
            line.iter()
                .map(|span| strip_ansi(&span.text))
                .collect::<String>()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Preview content: static cards + README (pure builders over data the
// driver fetches), live theme lines from the shared GalleryPreviews cache.
// ---------------------------------------------------------------------------

/// README candidates at a source tree root, in preference order.
const README_NAMES: &[&str] = &[
    "README.md",
    "README",
    "readme.md",
    "Readme.md",
    "README.txt",
];

/// Read a tree's README (first candidate that exists), capped so a huge
/// file cannot flood the pane model; the pane wraps/clips at render.
pub fn readme_lines(tree: &Path) -> Option<Vec<String>> {
    for name in README_NAMES {
        let path = tree.join(name);
        if let Ok(text) = std::fs::read_to_string(&path) {
            return Some(
                text.lines()
                    .take(200)
                    .map(|line| line.trim_end().to_string())
                    .collect(),
            );
        }
    }
    None
}

/// Static preview for a source row: the registry facts, then the tree's
/// README when one is on disk (the M2 static mode). Falls back to the
/// facts alone for missing trees and README-less trees.
pub fn source_card_lines(
    record: &sources::SourceRecord,
    readme: Option<Vec<String>>,
) -> Vec<String> {
    let mut lines = vec![
        format!("source: {}", record.id),
        format!("origin: {}", record.url),
        format!(
            "version: {} \u{00b7} ref: {}",
            record.version, record.ref_name
        ),
        format!("license: {}", record.license),
        format!("installed: {}", record.installed_at),
        String::new(),
    ];
    match readme {
        Some(readme) if !readme.is_empty() => {
            lines.push("\u{2500}\u{2500} README \u{2500}\u{2500}".into());
            lines.extend(readme);
        }
        _ => lines.push("(no README in the installed tree)".into()),
    }
    lines
}

/// Static preview for a recipe row: its index card, with the curated
/// catalog origin when the id is one of the curated managers.
pub fn recipe_card_lines(
    recipe: &recipes::Recipe,
    curated: Option<&catalog::CatalogEntry>,
) -> Vec<String> {
    let mut lines = vec![
        format!("recipe: {}", recipe.id),
        format!("category: {}", recipe.category.as_str()),
        recipe.summary.clone(),
        String::new(),
        format!("license: {}", recipe.license),
        format!("info: {}", recipe.url),
    ];
    if let Some(entry) = curated {
        lines.push(format!("curated origin: {}", entry.origin));
    }
    match &recipe.driver {
        Some(_) => lines.push("driver: git clone on `i` / Enter".into()),
        None => lines
            .push("driver: none \u{2014} `niu plugin add` prints the package-manager route".into()),
    }
    if let Some(manager) = &recipe.manager {
        lines.push(format!("rides on source: {manager}"));
    }
    lines
}

/// Static preview for a collection row: description plus its members.
pub fn collection_card_lines(collection: &distros::Collection, origin_label: &str) -> Vec<String> {
    let mut lines = vec![
        format!("collection: {} ({origin_label})", collection.name),
        collection.description.clone(),
        String::new(),
        format!("{} recipes:", collection.entry.len()),
    ];
    for entry in collection.entry.iter().take(16) {
        lines.push(format!("  - {}", entry.recipe));
    }
    if collection.entry.len() > 16 {
        lines.push(format!(
            "  \u{2026} and {} more",
            collection.entry.len() - 16
        ));
    }
    lines
}

/// Compute the preview for the model's current selection. Called by the
/// driver when the selection changes (and after inventory rebuilds);
/// live rows start from whatever the shared cache already holds.
fn compute_preview(model: &TuiModel, previews: &GalleryPreviews) -> Option<PreviewData> {
    let row = model.selected_row()?;
    if model.selected_is_global() {
        return Some(PreviewData {
            title: row.id.clone(),
            kind: PreviewKind::Static,
            lines: vec![
                format!("global verb: {}", row.id),
                row.hint.clone(),
                String::new(),
                "u or Enter runs it across every installed source.".into(),
            ],
        });
    }
    match &row.kind {
        UiRowKind::Theme { source_id } => Some(PreviewData {
            title: row.id.clone(),
            kind: PreviewKind::LivePs1 {
                source_id: source_id.clone(),
                name: row.id.clone(),
            },
            lines: previews.lines_for(source_id, &row.id, Duration::ZERO),
        }),
        UiRowKind::Source { .. } => {
            let record = sources::read_source_registry()
                .into_iter()
                .find(|record| record.id == row.id);
            let lines = match &record {
                Some(record) => {
                    let readme = if record.path.is_dir() {
                        readme_lines(&record.path)
                    } else {
                        None
                    };
                    source_card_lines(record, readme)
                }
                None => vec![format!("source '{}' is not in the registry", row.id)],
            };
            Some(PreviewData {
                title: row.id.clone(),
                kind: PreviewKind::Static,
                lines,
            })
        }
        UiRowKind::Recipe { .. } => {
            let lines = match recipes::recipes().iter().find(|recipe| recipe.id == row.id) {
                Some(recipe) => {
                    let curated = catalog::catalog_entry(&row.id);
                    recipe_card_lines(recipe, curated.as_ref())
                }
                None => vec![format!("recipe '{}' is not in the index", row.id)],
            };
            Some(PreviewData {
                title: row.id.clone(),
                kind: PreviewKind::Static,
                lines,
            })
        }
        UiRowKind::Collection { .. } => {
            let listing = distros::collections()
                .into_iter()
                .find(|listing| listing.collection.name == row.id);
            let lines = match &listing {
                Some(listing) => collection_card_lines(
                    &listing.collection,
                    match listing.origin {
                        distros::CollectionOrigin::Builtin => "built-in",
                        distros::CollectionOrigin::Imported { .. } => "imported",
                    },
                ),
                None => vec![format!("collection '{}' is not listed", row.id)],
            };
            Some(PreviewData {
                title: row.id.clone(),
                kind: PreviewKind::Static,
                lines,
            })
        }
    }
}

/// Prefetch live previews for the selected theme and the next theme row
/// after it, so arrow-key browsing finds renders already warm (the
/// wizard's neighbor-prefetch discipline, niubash#170).
fn prefetch_neighbors(model: &TuiModel, previews: &GalleryPreviews) {
    let Some(row) = model.selected_row() else {
        return;
    };
    let UiRowKind::Theme { source_id } = &row.kind else {
        return;
    };
    previews.prefetch(source_id, &row.id);
    // The next theme row in inventory order: browsing down finds it warm.
    let themes: Vec<(&str, &str)> = model
        .sections
        .iter()
        .flat_map(|section| &section.rows)
        .filter_map(|row| match &row.kind {
            UiRowKind::Theme { source_id } => Some((source_id.as_str(), row.id.as_str())),
            _ => None,
        })
        .collect();
    if let Some(index) = themes.iter().position(|(_, name)| *name == row.id) {
        if let Some((next_source, next_name)) = themes.get(index + 1) {
            previews.prefetch(next_source, next_name);
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal guard + frame writer (the driver's terminal surface)
// ---------------------------------------------------------------------------

/// One terminal-mode step of entering or leaving the TUI. The sequences
/// are data so a test can prove every enter step has its inverse in the
/// exit sequence, in reverse order — the restoration contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermStep {
    EnableRawMode,
    DisableRawMode,
    EnterAlternateScreen,
    LeaveAlternateScreen,
    HideCursor,
    ShowCursor,
}

pub fn enter_steps() -> Vec<TermStep> {
    vec![
        TermStep::EnableRawMode,
        TermStep::EnterAlternateScreen,
        TermStep::HideCursor,
    ]
}

pub fn exit_steps() -> Vec<TermStep> {
    vec![
        TermStep::ShowCursor,
        TermStep::LeaveAlternateScreen,
        TermStep::DisableRawMode,
    ]
}

fn apply_step(stdout: &mut impl Write, step: TermStep) -> std::io::Result<()> {
    match step {
        TermStep::EnableRawMode => terminal::enable_raw_mode(),
        TermStep::DisableRawMode => terminal::disable_raw_mode(),
        TermStep::EnterAlternateScreen => execute!(stdout, terminal::EnterAlternateScreen),
        TermStep::LeaveAlternateScreen => execute!(stdout, terminal::LeaveAlternateScreen),
        TermStep::HideCursor => execute!(stdout, cursor::Hide),
        TermStep::ShowCursor => execute!(stdout, cursor::Show),
    }
}

/// RAII terminal restoration: entering the TUI applies [`enter_steps`];
/// dropping the guard (quit, error return, or panic unwind) applies
/// [`exit_steps`], so the alternate screen and raw mode can never leak
/// into the shell session that resumes underneath.
pub struct TerminalGuard {
    stdout: std::io::Stdout,
}

impl TerminalGuard {
    pub fn enter() -> std::io::Result<Self> {
        let mut guard = TerminalGuard {
            stdout: std::io::stdout(),
        };
        for step in enter_steps() {
            apply_step(&mut guard.stdout, step)?;
        }
        guard.stdout.flush()?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        for step in exit_steps() {
            let _ = apply_step(&mut self.stdout, step);
        }
        let _ = self.stdout.flush();
    }
}

fn sgr_open(style: Style) -> &'static str {
    match style {
        Style::Plain => "\u{1b}[0m",
        Style::Dim => "\u{1b}[2m",
        Style::Bold => "\u{1b}[1m",
        Style::Accent => "\u{1b}[1;36m",
        Style::Good => "\u{1b}[32m",
        Style::Bad => "\u{1b}[31m",
        Style::Warn => "\u{1b}[33m",
        Style::Border => "\u{1b}[36m",
        Style::CursorRow => "\u{1b}[7;1m",
        Style::RawAnsi => "",
    }
}

/// Paint one frame: cursor home, then each line's spans with their SGR,
/// clearing whatever a longer previous frame left behind.
fn paint(stdout: &mut impl Write, frame: &Frame) -> std::io::Result<()> {
    let mut out = String::new();
    out.push_str("\u{1b}[H");
    for (index, line) in frame.iter().enumerate() {
        if index > 0 {
            out.push_str("\r\n");
        }
        out.push_str("\u{1b}[2K");
        let mut current = Style::Plain;
        for span in line {
            if span.style != current {
                if span.style == Style::RawAnsi {
                    out.push_str("\u{1b}[0m");
                } else {
                    out.push_str(sgr_open(span.style));
                }
                current = span.style;
            }
            out.push_str(&span.text);
            if span.style == Style::RawAnsi {
                current = Style::Plain;
            }
        }
        out.push_str("\u{1b}[0m");
    }
    stdout.write_all(out.as_bytes())?;
    stdout.flush()
}

// ---------------------------------------------------------------------------
// Driver: `niu plugin tui`
// ---------------------------------------------------------------------------

/// Full-screen plugin manager entry. Refuses non-terminal stdio the same
/// way [`ui::run_ui`] does (health-style: name the object and the
/// alternative), then runs the render loop until the quit key.
pub fn run_tui() -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        let forms: Vec<&str> = ui::UI_VERBS.iter().map(|entry| entry.cli).collect();
        anyhow::bail!(
            "niu plugin tui needs an interactive terminal; \
             the menu form is `niu plugin ui`, and the same verbs exist on the CLI: {}",
            forms.join(" · ")
        );
    }
    let guard = TerminalGuard::enter()?;
    let result = event_loop();
    drop(guard);
    result
}

fn event_loop() -> anyhow::Result<()> {
    let mut model = TuiModel::new(ui::inventory());
    let previews = GalleryPreviews::new();
    model.preview = compute_preview(&model, &previews);
    prefetch_neighbors(&model, &previews);
    let (job_tx, job_rx) = mpsc::channel::<Result<String, String>>();
    let mut stdout = std::io::stdout();
    let (mut width, mut height) = terminal::size().unwrap_or((80, 24));
    let mut dirty = true;

    loop {
        // Jobs land between ticks: record the outcome, rebuild the
        // inventory (live state), refresh the preview.
        while let Ok(result) = job_rx.try_recv() {
            model.job_finished(result);
            model.set_sections(ui::inventory());
            model.preview = compute_preview(&model, &previews);
            prefetch_neighbors(&model, &previews);
            dirty = true;
        }
        // A live preview still rendering repaints when its render lands.
        if let Some(preview) = &model.preview {
            if let PreviewKind::LivePs1 { source_id, name } = &preview.kind {
                let lines = previews.lines_for(source_id, name, Duration::ZERO);
                if lines != preview.lines {
                    model.preview = Some(PreviewData {
                        title: preview.title.clone(),
                        kind: preview.kind.clone(),
                        lines,
                    });
                    dirty = true;
                }
            }
        }
        if dirty {
            model.ensure_visible(body_height(height));
            let frame = render(&model, width, height);
            paint(&mut stdout, &frame)?;
            dirty = false;
        }
        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) => {
                    let viewport = body_height(height);
                    match model.handle_key(key, viewport) {
                        TuiEffect::Quit => return Ok(()),
                        TuiEffect::SelectionChanged => {
                            model.preview = compute_preview(&model, &previews);
                            prefetch_neighbors(&model, &previews);
                            dirty = true;
                        }
                        TuiEffect::RunVerb { verb, target } => {
                            let tx = job_tx.clone();
                            std::thread::spawn(move || {
                                let result = ui::apply_verb(verb, &target)
                                    .map_err(|error| error.to_string());
                                let _ = tx.send(result);
                            });
                            dirty = true;
                        }
                        TuiEffect::None => {
                            dirty = true;
                        }
                    }
                }
                Event::Resize(new_width, new_height) => {
                    width = new_width;
                    height = new_height;
                    dirty = true;
                }
                _ => {}
            }
        }
        model.tick = model.tick.wrapping_add(1);
        if model.busy.is_some() {
            dirty = true; // spinner animation while a verb runs
        }
    }
}

// ---------------------------------------------------------------------------
// Pins (niubash#186): the model, key table, dispatch and renderer are pure,
// so M1/M2 behavior is pinned headlessly here. The ConPTY golden faces
// (real alternate-buffer entry/exit on Windows Terminal/conhost) are
// CI-verified — this module's terminal surface is the guard step pairs
// pinned below plus the driver's terminal check in run_tui.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn row(id: &str, kind: UiRowKind, state: &str, hint: &str) -> UiRow {
        UiRow {
            id: id.into(),
            kind,
            state: state.into(),
            hint: hint.into(),
        }
    }

    fn fake_sections() -> Vec<UiSection> {
        vec![
            UiSection {
                title: "Installed & ready \u{2014} sources".into(),
                rows: vec![
                    row(
                        "oh-my-bash",
                        UiRowKind::Source { missing: false },
                        "ready",
                        "hint-a",
                    ),
                    row(
                        "bash-it",
                        UiRowKind::Source { missing: false },
                        "ready",
                        "hint-b",
                    ),
                ],
                global: false,
            },
            UiSection {
                title: "Themes".into(),
                rows: vec![row(
                    "agnoster",
                    UiRowKind::Theme {
                        source_id: "oh-my-bash".into(),
                    },
                    "theme",
                    "oh-my-bash",
                )],
                global: false,
            },
            UiSection {
                title: "Recipes \u{2014} plugins (not installed)".into(),
                rows: vec![
                    row(
                        "z",
                        UiRowKind::Recipe {
                            installable: true,
                            installed: false,
                        },
                        "available",
                        "fast jumper",
                    ),
                    row(
                        "manual-thing",
                        UiRowKind::Recipe {
                            installable: false,
                            installed: false,
                        },
                        "available",
                        "no driver",
                    ),
                ],
                global: false,
            },
            UiSection {
                title: "Collections".into(),
                rows: vec![row(
                    "starter",
                    UiRowKind::Collection { builtin: true },
                    "built-in",
                    "the starter set",
                )],
                global: false,
            },
            UiSection {
                title: "Global verbs".into(),
                rows: vec![row(
                    "sync",
                    UiRowKind::Collection { builtin: false },
                    "global",
                    "update every source",
                )],
                global: true,
            },
        ]
    }

    fn selected_id(model: &TuiModel) -> String {
        model.selected_row().expect("a row is selected").id.clone()
    }

    #[test]
    fn issue186_m1_navigation_skips_section_headers_and_clamps() {
        let mut model = TuiModel::new(fake_sections());
        assert_eq!(selected_id(&model), "oh-my-bash");
        let mut walked = vec![selected_id(&model)];
        for _ in 0..6 {
            let effect = model.handle_key(ch('j'), 10);
            assert_eq!(effect, TuiEffect::SelectionChanged);
            walked.push(selected_id(&model));
        }
        assert_eq!(
            walked,
            [
                "oh-my-bash",
                "bash-it",
                "agnoster",
                "z",
                "manual-thing",
                "starter",
                "sync"
            ]
        );
        // Bottom end clamps: one more j changes nothing, reports no move.
        assert_eq!(model.handle_key(ch('j'), 10), TuiEffect::None);
        assert_eq!(selected_id(&model), "sync");
        // k walks back up; Down/Up arrows are the same moves.
        assert_eq!(model.handle_key(ch('k'), 10), TuiEffect::SelectionChanged);
        assert_eq!(selected_id(&model), "starter");
        assert_eq!(
            model.handle_key(key(KeyCode::Down), 10),
            TuiEffect::SelectionChanged
        );
        assert_eq!(selected_id(&model), "sync");
        assert_eq!(
            model.handle_key(key(KeyCode::Up), 10),
            TuiEffect::SelectionChanged
        );
        assert_eq!(selected_id(&model), "starter");
        // Key-release events never act (Windows terminals report them).
        let release = KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert_eq!(model.handle_key(release, 10), TuiEffect::None);
        assert_eq!(selected_id(&model), "starter");
    }

    #[test]
    fn issue186_m1_page_home_end_moves() {
        let rows: Vec<UiRow> = (0..30)
            .map(|index| {
                row(
                    &format!("src-{index:02}"),
                    UiRowKind::Source { missing: false },
                    "ready",
                    "hint",
                )
            })
            .collect();
        let mut model = TuiModel::new(vec![UiSection {
            title: "Sources".into(),
            rows,
            global: false,
        }]);
        assert_eq!(selected_id(&model), "src-00");
        model.handle_key(key(KeyCode::PageDown), 5);
        assert_eq!(selected_id(&model), "src-05");
        model.handle_key(key(KeyCode::PageDown), 5);
        assert_eq!(selected_id(&model), "src-10");
        model.handle_key(key(KeyCode::PageUp), 5);
        assert_eq!(selected_id(&model), "src-05");
        model.handle_key(ch('G'), 5);
        assert_eq!(selected_id(&model), "src-29");
        // The scroll window follows the cursor.
        assert!(model.cursor >= model.scroll && model.cursor < model.scroll + 5);
        model.handle_key(ch('g'), 5);
        assert_eq!(selected_id(&model), "src-00");
        assert_eq!(model.scroll, 0);
    }

    #[test]
    fn issue186_m1_quit_keys() {
        for quit_key in [
            ch('q'),
            key(KeyCode::Esc),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let mut model = TuiModel::new(fake_sections());
            assert_eq!(model.handle_key(quit_key, 10), TuiEffect::Quit);
            assert!(model.quit);
        }
    }

    #[test]
    fn issue186_m1_help_bar_is_generated_from_the_key_table() {
        let bar = help_bar();
        for entry in TUI_KEYS {
            assert!(
                bar.contains(&format!("{} {}", entry.key, entry.label)),
                "help bar is missing table entry {entry:?}: {bar}"
            );
        }
        // The #186 keymap, as shipped for M1+M2.
        assert!(bar.contains("i install"));
        assert!(bar.contains("u update"));
        assert!(bar.contains("x remove"));
        assert!(bar.contains("p preview"));
        assert!(bar.contains("q quit"));
    }

    #[test]
    fn issue186_m1_dispatch_maps_keys_to_verbs_per_row_kind() {
        // Recipe: i and Enter both dispatch add.
        let mut model = TuiModel::new(fake_sections());
        model.handle_key(ch('G'), 10); // sync row (global)
        model.handle_key(ch('k'), 10); // starter
        model.handle_key(ch('k'), 10); // manual-thing
        model.handle_key(ch('k'), 10); // z
        assert_eq!(selected_id(&model), "z");
        assert_eq!(
            model.handle_key(ch('i'), 10),
            TuiEffect::RunVerb {
                verb: "add",
                target: "z".into()
            }
        );
        assert_eq!(model.busy.as_deref(), Some("add z"));
        model.job_finished(Ok("installed".into()));
        assert_eq!(
            model.handle_key(key(KeyCode::Enter), 10),
            TuiEffect::RunVerb {
                verb: "add",
                target: "z".into()
            }
        );
        model.job_finished(Ok("installed".into()));

        // Source: u dispatches update; i explains instead of acting.
        model.handle_key(ch('g'), 10);
        assert_eq!(selected_id(&model), "oh-my-bash");
        assert_eq!(model.handle_key(ch('i'), 10), TuiEffect::None);
        assert!(model
            .status
            .as_deref()
            .unwrap()
            .contains("already installed"));
        assert_eq!(
            model.handle_key(ch('u'), 10),
            TuiEffect::RunVerb {
                verb: "update",
                target: "oh-my-bash".into()
            }
        );
        model.job_finished(Ok("updated".into()));

        // Theme: Enter enables, x disables (reversible — no confirm).
        model.handle_key(ch('j'), 10);
        model.handle_key(ch('j'), 10);
        assert_eq!(selected_id(&model), "agnoster");
        assert_eq!(
            model.handle_key(key(KeyCode::Enter), 10),
            TuiEffect::RunVerb {
                verb: "enable",
                target: "agnoster".into()
            }
        );
        model.job_finished(Ok("enabled".into()));
        assert_eq!(
            model.handle_key(ch('x'), 10),
            TuiEffect::RunVerb {
                verb: "disable",
                target: "agnoster".into()
            }
        );
        model.job_finished(Ok("disabled".into()));
        assert_eq!(model.handle_key(ch('u'), 10), TuiEffect::None);
        assert!(model
            .status
            .as_deref()
            .unwrap()
            .contains("with their source"));

        // Collection: Enter and i apply; the global row syncs on u/Enter.
        model.handle_key(ch('G'), 10);
        model.handle_key(ch('k'), 10);
        assert_eq!(selected_id(&model), "starter");
        assert_eq!(
            model.handle_key(ch('i'), 10),
            TuiEffect::RunVerb {
                verb: "apply",
                target: "starter".into()
            }
        );
        model.job_finished(Ok("applied".into()));
        model.handle_key(ch('j'), 10);
        assert_eq!(selected_id(&model), "sync");
        assert_eq!(
            model.handle_key(ch('u'), 10),
            TuiEffect::RunVerb {
                verb: "sync",
                target: "sync".into()
            }
        );
    }

    #[test]
    fn issue186_m1_remove_needs_a_second_x_and_moving_cancels() {
        let mut model = TuiModel::new(fake_sections());
        assert_eq!(selected_id(&model), "oh-my-bash");
        assert_eq!(model.handle_key(ch('x'), 10), TuiEffect::None);
        assert!(model
            .status
            .as_deref()
            .unwrap()
            .contains("Press x again to confirm"));
        // Moving away cancels the armed confirmation.
        model.handle_key(ch('j'), 10);
        model.handle_key(ch('k'), 10);
        assert_eq!(selected_id(&model), "oh-my-bash");
        assert_eq!(model.handle_key(ch('x'), 10), TuiEffect::None);
        // Second x on the same row dispatches the removal.
        assert_eq!(
            model.handle_key(ch('x'), 10),
            TuiEffect::RunVerb {
                verb: "remove-source",
                target: "oh-my-bash".into()
            }
        );
    }

    #[test]
    fn issue186_m1_busy_blocks_verbs_but_not_navigation() {
        let mut model = TuiModel::new(fake_sections());
        model.handle_key(ch('G'), 10);
        model.handle_key(ch('k'), 10);
        model.handle_key(ch('k'), 10);
        model.handle_key(ch('k'), 10);
        assert_eq!(selected_id(&model), "z");
        assert!(matches!(
            model.handle_key(ch('i'), 10),
            TuiEffect::RunVerb { .. }
        ));
        // While the add runs: verbs refuse with a busy note, j still moves.
        assert_eq!(model.handle_key(ch('u'), 10), TuiEffect::None);
        assert!(model.status.as_deref().unwrap().contains("busy: add z"));
        assert_eq!(model.handle_key(ch('j'), 10), TuiEffect::SelectionChanged);
        // A failed job surfaces the error and clears busy.
        model.job_finished(Err("network unreachable".into()));
        assert!(model.busy.is_none());
        assert_eq!(model.status.as_deref(), Some("failed: network unreachable"));
        // A landed job's summary becomes the status. (Selection sits on
        // manual-thing after the j above; step back to z first.)
        model.handle_key(ch('k'), 10);
        assert_eq!(selected_id(&model), "z");
        assert!(matches!(
            model.handle_key(ch('i'), 10),
            TuiEffect::RunVerb { .. }
        ));
        model.job_finished(Ok("installed 'z'".into()));
        assert_eq!(model.status.as_deref(), Some("installed 'z'"));
    }

    #[test]
    fn issue186_m1_t_is_unbound_and_slash_points_at_m3() {
        let mut model = TuiModel::new(fake_sections());
        assert_eq!(model.handle_key(ch('t'), 10), TuiEffect::None);
        let status = model.status.as_deref().unwrap();
        assert!(status.contains("unbound"), "{status}");
        assert!(status.contains("trust"), "{status}");
        assert_eq!(model.handle_key(ch('/'), 10), TuiEffect::None);
        assert!(model.status.as_deref().unwrap().contains("M3"));
    }

    #[test]
    fn issue186_m1_selection_survives_inventory_rebuild_by_id() {
        let mut model = TuiModel::new(fake_sections());
        model.handle_key(ch('G'), 10);
        model.handle_key(ch('k'), 10);
        model.handle_key(ch('k'), 10);
        model.handle_key(ch('k'), 10);
        assert_eq!(selected_id(&model), "z");
        // Reordered inventory: the selection follows the row id.
        let mut rebuilt = fake_sections();
        rebuilt.reverse();
        model.set_sections(rebuilt);
        assert_eq!(selected_id(&model), "z");
        // The selected row vanished: selection clamps to a live row.
        let mut shrunk = fake_sections();
        shrunk[2].rows.clear();
        model.set_sections(shrunk);
        assert!(model.selected_row().is_some());
    }

    #[test]
    fn issue186_m1_enter_exit_steps_are_inverse_pairs() {
        let inverse = |step: TermStep| match step {
            TermStep::EnableRawMode => TermStep::DisableRawMode,
            TermStep::DisableRawMode => TermStep::EnableRawMode,
            TermStep::EnterAlternateScreen => TermStep::LeaveAlternateScreen,
            TermStep::LeaveAlternateScreen => TermStep::EnterAlternateScreen,
            TermStep::HideCursor => TermStep::ShowCursor,
            TermStep::ShowCursor => TermStep::HideCursor,
        };
        let expected_exit: Vec<TermStep> = enter_steps()
            .iter()
            .rev()
            .map(|step| inverse(*step))
            .collect();
        assert_eq!(exit_steps(), expected_exit);
    }

    #[test]
    fn issue186_m1_render_snapshot_list_status_and_help() {
        let mut model = TuiModel::new(vec![
            UiSection {
                title: "Installed & ready \u{2014} sources".into(),
                rows: vec![row(
                    "oh-my-bash",
                    UiRowKind::Source { missing: false },
                    "ready",
                    "hint-a",
                )],
                global: false,
            },
            UiSection {
                title: "Recipes \u{2014} plugins (not installed)".into(),
                rows: vec![row(
                    "z",
                    UiRowKind::Recipe {
                        installable: true,
                        installed: false,
                    },
                    "available",
                    "fast jumper",
                )],
                global: false,
            },
        ]);
        model.preview_visible = false;
        let frame = render(&model, 80, 12);
        let plain = frame_plain_text(&frame);
        assert_eq!(plain.len(), 12);
        let content: Vec<&str> = plain.iter().map(|line| line.trim_end()).collect();
        assert_eq!(
            content,
            [
                " niu plugin tui \u{2014} 2 rows, 1 sources installed",
                "\u{25b8} Installed & ready \u{2014} sources (1)",
                "\u{276f} [ready] oh-my-bash \u{2014} hint-a",
                "\u{25b8} Recipes \u{2014} plugins (not installed) (1)",
                "  [available] z \u{2014} fast jumper",
                "",
                "",
                "",
                "",
                "",
                "",
                " j/k move \u{00b7} i install \u{00b7} u update \u{00b7} x remove \u{00b7} Enter primary \u{00b7} p preview \u{00b7} q quit",
            ]
        );
        // Padded lines really are full width (title, status, help).
        assert_eq!(display_width(&plain[0]), 80);
        assert_eq!(display_width(&plain[10]), 80);
        assert_eq!(display_width(&plain[11]), 80);
        // The selected row renders as one reverse-video span run.
        assert!(frame[2].iter().all(|span| span.style == Style::CursorRow));
    }

    #[test]
    fn issue186_m2_render_snapshot_side_pane_with_rounded_border() {
        let mut model = TuiModel::new(fake_sections());
        model.preview = Some(PreviewData {
            title: "oh-my-bash".into(),
            kind: PreviewKind::Static,
            lines: vec!["line one".into(), "line two".into()],
        });
        let frame = render(&model, 100, 14);
        let plain = frame_plain_text(&frame);
        assert_eq!(plain.len(), 14);
        // Body starts at frame line 1; the pane occupies columns 56..100.
        let pane_top: String = plain[1].chars().skip(56).collect();
        assert_eq!(
            pane_top,
            "\u{256d}\u{2500} Preview \u{2014} README \u{00b7} oh-my-bash \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}"
        );
        let pane_body: String = plain[2].chars().skip(56).collect();
        assert!(pane_body.starts_with("\u{2502} line one"), "{pane_body}");
        assert!(pane_body.ends_with('\u{2502}'));
        let pane_second: String = plain[3].chars().skip(56).collect();
        assert!(
            pane_second.starts_with("\u{2502} line two"),
            "{pane_second}"
        );
        // Body height is 11: the bottom border is the last body line.
        let pane_bottom: String = plain[11].chars().skip(56).collect();
        assert!(pane_bottom.starts_with('\u{2570}'), "{pane_bottom}");
        assert!(pane_bottom.ends_with('\u{256f}'));
        assert_eq!(display_width(&pane_bottom), 44);
        // The list still renders on the left of the same lines.
        assert!(plain[1].starts_with("\u{25b8} Installed & ready"));
    }

    #[test]
    fn issue186_m2_render_snapshot_floating_pane_on_narrow_terminal() {
        let mut model = TuiModel::new(fake_sections());
        model.preview = Some(PreviewData {
            title: "z".into(),
            kind: PreviewKind::Static,
            lines: vec!["recipe: z".into()],
        });
        let frame = render(&model, 70, 16);
        let plain = frame_plain_text(&frame);
        // Pane is 60 wide starting at column 5, 9 tall starting at body
        // line 2 (frame line 3), floating over the list.
        let top = &plain[3];
        assert_eq!(top.chars().nth(5), Some('\u{256d}'));
        assert!(top.contains("Preview \u{2014} README \u{00b7} z"), "{top}");
        let bottom = &plain[11];
        assert_eq!(bottom.chars().nth(5), Some('\u{2570}'));
        // The list shows above and below the float.
        assert!(plain[1].starts_with("\u{25b8} Installed & ready"));
        assert!(!plain[15].is_empty()); // help bar intact
    }

    #[test]
    fn issue186_m2_live_preview_lines_reach_the_pane_with_their_ansi() {
        let mut model = TuiModel::new(fake_sections());
        model.handle_key(ch('j'), 10);
        model.handle_key(ch('j'), 10);
        assert_eq!(selected_id(&model), "agnoster");
        let ps1 = "\u{1b}[32muser@host\u{1b}[0m ~/repo $".to_string();
        model.preview = Some(PreviewData {
            title: "agnoster".into(),
            kind: PreviewKind::LivePs1 {
                source_id: "oh-my-bash".into(),
                name: "agnoster".into(),
            },
            lines: vec![ps1.clone()],
        });
        let frame = render(&model, 100, 14);
        // The raw span keeps the prompt's own escape bytes verbatim...
        let raw: Vec<&Span> = frame
            .iter()
            .flatten()
            .filter(|span| span.style == Style::RawAnsi)
            .collect();
        assert_eq!(raw.len(), 1);
        assert!(raw[0].text.contains(&ps1), "{:?}", raw[0].text);
        // ...while the plain projection reads cleanly and the pane title
        // announces the live mode.
        let plain = frame_plain_text(&frame);
        assert!(plain[1].contains("LIVE PS1"));
        assert!(plain.iter().any(|line| line.contains("user@host")));
        // A still-rendering theme shows the shared placeholder instead.
        model.preview = Some(PreviewData {
            title: "agnoster".into(),
            kind: PreviewKind::LivePs1 {
                source_id: "oh-my-bash".into(),
                name: "agnoster".into(),
            },
            lines: vec![crate::plugins::theme_preview::RENDERING_PLACEHOLDER.to_string()],
        });
        let frame = render(&model, 100, 14);
        let plain = frame_plain_text(&frame);
        assert!(plain
            .iter()
            .any(|line| line.contains(crate::plugins::theme_preview::RENDERING_PLACEHOLDER)));
    }

    #[test]
    fn issue186_m2_readme_and_static_cards() {
        // README reading: preference order + missing-tree behavior.
        let root = std::env::temp_dir().join(format!("niu-tui-186-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), "# Title\n\nbody line\n").unwrap();
        let readme = readme_lines(&root).expect("README.md is found");
        assert_eq!(readme, ["# Title", "", "body line"]);
        assert!(readme_lines(&root.join("nope")).is_none());
        std::fs::remove_dir_all(&root).ok();

        // Source card: facts first, README after the rule; the fallback
        // note when the tree has none.
        let record = sources::SourceRecord {
            id: "oh-my-bash".into(),
            adapter: "oh-my-bash".into(),
            url: "https://github.com/ohmybash/oh-my-bash".into(),
            ref_name: "master".into(),
            version: "abc123".into(),
            path: PathBuf::from("/tmp/nowhere"),
            license: "MIT".into(),
            installed_at: "2026-10-01".into(),
            commit_sha: None,
            previous: None,
            spec_enabled: None,
            spec_theme: None,
        };
        let card = source_card_lines(&record, Some(vec!["# Hello".into()]));
        assert_eq!(card[0], "source: oh-my-bash");
        assert!(card.iter().any(|line| line.contains("README")));
        assert_eq!(card.last().map(String::as_str), Some("# Hello"));
        let bare = source_card_lines(&record, None);
        assert!(bare.last().unwrap().contains("no README"));

        // Recipe card: curated origin surfaces for catalog ids.
        let recipe = recipes::Recipe {
            id: "oh-my-bash".into(),
            category: recipes::RecipeCategory::Manager,
            summary: "a manager".into(),
            license: "MIT".into(),
            url: "https://example.invalid/oh-my-bash".into(),
            driver: Some(recipes::RecipeDriver::Git {
                kind: "oh-my-bash".into(),
                entry: None,
                origin: None,
            }),
            manager: None,
            asset_kind: None,
            asset: None,
        };
        let curated = catalog::catalog_entry("oh-my-bash");
        let card = recipe_card_lines(&recipe, curated.as_ref());
        assert!(card.iter().any(|line| line.starts_with("curated origin:")));
        assert!(card.iter().any(|line| line == "category: manager"));

        // Collection card: members listed, overflow summarized.
        let collection = distros::Collection {
            schema: None,
            name: "starter".into(),
            description: "the starter set".into(),
            entry: (0..20)
                .map(|index| distros::CollectionEntry {
                    recipe: format!("r{index}"),
                })
                .collect(),
        };
        let card = collection_card_lines(&collection, "built-in");
        assert!(card.iter().any(|line| line.contains("20 recipes")));
        assert!(card.last().unwrap().contains("and 4 more"));
    }

    #[test]
    fn issue186_m2_width_math_is_ansi_aware_and_never_splits_escapes() {
        assert_eq!(display_width("\u{1b}[32mabc\u{1b}[0m"), 3);
        assert_eq!(display_width("a\u{754c}b"), 4);
        assert_eq!(clip_line("abcdef", 4), "abc\u{2026}");
        let clipped = clip_line("\u{1b}[31mabcdef\u{1b}[0m", 4);
        assert!(clipped.starts_with("\u{1b}[31m"), "{clipped:?}");
        assert!(clipped.ends_with("\u{1b}[0m"), "{clipped:?}");
        assert_eq!(display_width(&clipped), 4);
        assert_eq!(pad_line("ab", 5), "ab   ");
        assert_eq!(strip_ansi("\u{1b}[1;36mhi\u{1b}[0m"), "hi");
    }
}
