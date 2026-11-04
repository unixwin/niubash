//! Interactive terminal menu with arrow-key navigation.
//!
//! Provides `interactive_choice` for arrow/jk/Enter/ESC selection and
//! `SideBySideLayout` for logo-left / wizard-right rendering.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::{
    cursor::{self, MoveDown, MoveToColumn, MoveUp},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, Clear, ClearType},
};

/// Result of an interactive menu selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// User confirmed a specific option (0-indexed).
    Confirmed(usize),
    /// User pressed ESC — caller should use the default.
    UseDefault,
    /// User pressed Ctrl-C — caller should abort the whole flow.
    Abort,
}

/// What a key press should do to the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    /// Highlight a different option.
    MoveTo(usize),
    /// Confirm the highlighted option.
    Confirm,
    /// Take the caller-provided default (ESC).
    UseDefault,
    /// Abort the whole flow (Ctrl-C).
    Abort,
    /// Do nothing.
    Ignore,
}

/// Map a key event to a menu action.
///
/// Windows emits `Press`, `Repeat`, and `Release` key events; Unix terminals
/// only report presses. Release events must be ignored or every key is handled
/// twice and a key-up left in the input queue can confirm the *next* menu.
fn key_action(key: &KeyEvent, selected: usize, len: usize) -> MenuAction {
    if key.kind == KeyEventKind::Release {
        return MenuAction::Ignore;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Up | KeyCode::Char('k') if !ctrl => MenuAction::MoveTo(selected.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') if !ctrl => {
            MenuAction::MoveTo((selected + 1).min(len.saturating_sub(1)))
        }
        KeyCode::Enter => MenuAction::Confirm,
        KeyCode::Esc => MenuAction::UseDefault,
        KeyCode::Char('c') if ctrl => MenuAction::Abort,
        KeyCode::Char(c) if c.is_ascii_digit() => match c.to_digit(10) {
            Some(idx) if idx >= 1 && idx as usize <= len => MenuAction::MoveTo(idx as usize - 1),
            _ => MenuAction::Ignore,
        },
        _ => MenuAction::Ignore,
    }
}

const MENU_HINT_EN: &str =
    "\x1b[2m↑↓ navigate  1-9 jump  Enter confirm  Esc defaults  Ctrl+C quit\x1b[0m";
const MENU_HINT_ZH: &str =
    "\x1b[2m↑↓ 移动  1-9 跳转  Enter 确认  Esc 全部用默认  Ctrl+C 退出\x1b[0m";

/// The menu's own hint line follows the wizard language so a Chinese wizard
/// never shows an English footer.
fn menu_hint() -> &'static str {
    if crate::setup_wizard::wizard_lang_is_chinese() {
        MENU_HINT_ZH
    } else {
        MENU_HINT_EN
    }
}

/// Run an interactive arrow-key menu. Returns `Selection::Confirmed(index)` on
/// Enter, `Selection::UseDefault` on ESC, `Selection::Abort` on Ctrl-C.
///
/// `label` is printed once above the options. `help` is shown below the options
/// when non-empty. `default_idx` highlights the pre-selected option and is
/// returned on ESC.
pub fn interactive_choice(
    label: &str,
    options: &[&str],
    default_idx: usize,
    help: &str,
) -> Selection {
    interactive_choice_ex(label, options, default_idx, help, None)
}

/// Which pass is asking the preview callback. `Measure` probes the fixed
/// preview-pane height once per option while the menu is being laid out —
/// the callback must answer from constants only, with no rendering work
/// (niubash#170: a live theme preview starts child renders on `Draw` only,
/// so the layout sweep can never spawn them). `Draw` is a real paint of the
/// highlighted option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewPhase {
    Measure,
    Draw,
}

/// Like [`interactive_choice`], plus an optional live preview rendered below
/// the menu and refreshed whenever the highlight moves. `preview` maps an
/// option index (plus the [`PreviewPhase`] asking) to the lines to display.
pub fn interactive_choice_ex(
    label: &str,
    options: &[&str],
    default_idx: usize,
    help: &str,
    preview: Option<&dyn Fn(PreviewPhase, usize) -> Vec<String>>,
) -> Selection {
    let _raw = RawMode::enter();

    let mut selected = default_idx.min(options.len().saturating_sub(1));

    // Fixed preview area height so in-place redraws never shift rows.
    let preview_height = preview
        .map(|pv| {
            options
                .iter()
                .enumerate()
                .map(|(i, _)| pv(PreviewPhase::Measure, i).len())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);

    // A menu taller than the viewport cannot be repainted in place — the
    // initial MoveUp clamps at the top row and every row after that lands
    // on the wrong line. Cap the option rows to a sliding window around
    // the selection instead, keeping the block height constant.
    let chrome = help.lines().count() + preview_height + 9;
    let max_opts = (term_height() as usize)
        .saturating_sub(chrome)
        .max(5)
        .min(options.len());

    // Initial render
    println!();
    println!("  {}", label);
    if !help.is_empty() {
        for line in help.lines() {
            println!("  {}", clip_line(line, term_width() as usize - 4));
        }
    }
    println!();
    io::stdout().flush().ok();
    render_options(options, selected, max_opts);

    println!();
    println!("  {}", menu_hint());
    println!();
    io::stdout().flush().ok();

    if let Some(pv) = preview {
        let lines = pv(PreviewPhase::Draw, selected);
        for line in &lines {
            println!("  {}", clip_line(line, term_width() as usize - 4));
        }
        for _ in lines.len()..preview_height {
            println!();
        }
    }
    println!();
    io::stdout().flush().ok();

    // Row where the cursor rests: one blank line below the preview area.
    // Used only to clamp MoveUp so tiny terminals cannot overshoot.
    let rest_row = cursor::position().map(|p| p.1).unwrap_or(u16::MAX);

    // Drop input queued before this menu was drawn so a stale key event can't
    // instantly confirm it (e.g. an Enter pressed at the previous prompt).
    while event::poll(Duration::ZERO).unwrap_or(false) {
        if event::read().is_err() {
            break;
        }
    }

    // The pane may fill asynchronously (niubash#170: the preview callback
    // must answer instantly, the render lands in the background). Track the
    // painted pane so the idle poll below can pick the result up without a
    // keypress and repaint only on change.
    let mut painted_pane: Option<Vec<String>> = preview.map(|pv| pv(PreviewPhase::Draw, selected));

    loop {
        // 50ms idle poll: keys are read the moment they arrive; between
        // keys the loop wakes to pick up an async pane render (the callback
        // is a cheap cache read when idle). Menus are short-lived, so the
        // wakeup cost is noise.
        if !event::poll(Duration::from_millis(50)).unwrap_or(false) {
            if let (Some(pv), Some(painted)) = (preview, painted_pane.as_ref()) {
                let lines = pv(PreviewPhase::Draw, selected);
                if &lines != painted {
                    painted_pane = Some(lines.clone());
                    redraw_menu(
                        options,
                        selected,
                        Some(&lines),
                        preview_height,
                        rest_row,
                        max_opts,
                    );
                }
            }
            continue;
        }
        let key = match event::read() {
            Ok(Event::Key(k)) => k,
            Ok(_) => continue,
            // Persistent read failure (e.g. input went away): take the default
            // rather than spinning forever.
            Err(_) => return Selection::UseDefault,
        };
        match key_action(&key, selected, options.len()) {
            MenuAction::Confirm => return Selection::Confirmed(selected),
            MenuAction::UseDefault => return Selection::UseDefault,
            MenuAction::Abort => return Selection::Abort,
            MenuAction::MoveTo(new) if new != selected => {
                selected = new;
                // Coalesce a navigation burst (niubash#170): a fast arrow
                // walk can queue keys while the menu paints, and a live pane
                // whose render is still in flight must never multiply that
                // cost. Drain every already-queued key and repaint ONCE for
                // the final position; a Confirm/Esc inside the burst acts on
                // the coalesced position (last key wins, same as an
                // un-coalesced queue). The Draw below must be instant — the
                // async pane pickup above brings the render in when it
                // lands.
                while event::poll(Duration::ZERO).unwrap_or(false) {
                    match event::read() {
                        Ok(Event::Key(k)) => match key_action(&k, selected, options.len()) {
                            MenuAction::MoveTo(next) if next != selected => selected = next,
                            MenuAction::Confirm => return Selection::Confirmed(selected),
                            MenuAction::UseDefault => return Selection::UseDefault,
                            MenuAction::Abort => return Selection::Abort,
                            _ => {}
                        },
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
                let lines = preview.map(|pv| pv(PreviewPhase::Draw, selected));
                painted_pane = lines.clone();
                redraw_menu(
                    options,
                    selected,
                    lines.as_deref(),
                    preview_height,
                    rest_row,
                    max_opts,
                );
            }
            _ => {}
        }
    }
}

/// Visible option window for long menus: `[start, end)`, `end - start ==
/// max_visible` (or `len` when the whole list fits).
fn window_bounds(len: usize, selected: usize, max_visible: usize) -> (usize, usize) {
    if len <= max_visible {
        return (0, len);
    }
    let start = selected
        .saturating_sub(max_visible / 2)
        .min(len - max_visible);
    (start, start + max_visible)
}

/// Render the option window, highlighting `selected`. Scrolled-off ends are
/// marked with dim `↑ N more` / `↓ N more` rows so the block height stays
/// `max_visible` regardless of scroll position.
fn render_options(options: &[&str], selected: usize, max_visible: usize) {
    let mut stdout = io::stdout();
    let (start, end) = window_bounds(options.len(), selected, max_visible);
    for row in start..end {
        write_option_row(&mut stdout, options, row, selected, start, end);
        println!();
    }
}

/// Write one row of the option window: either an option line or a scroll
/// indicator. Never emits a newline.
fn write_option_row(
    stdout: &mut impl io::Write,
    options: &[&str],
    row: usize,
    selected: usize,
    start: usize,
    end: usize,
) {
    if row == start && start > 0 {
        let _ = write!(stdout, "  \x1b[2m  ↑ {} more\x1b[0m", start);
    } else if row == end - 1 && end < options.len() {
        let _ = write!(
            stdout,
            "  \x1b[2m  ↓ {} more\x1b[0m",
            options.len() - (end - 1)
        );
    } else {
        write_option(stdout, row, options[row], row == selected);
    }
}

/// Write one option line clipped to the terminal width — never emits a
/// newline, so it is safe mid-redraw at the bottom of the screen.
fn write_option(stdout: &mut impl io::Write, idx: usize, opt: &str, highlight: bool) {
    let text = clip_line(opt, (term_width() as usize).saturating_sub(10));
    let _ = if highlight {
        write!(stdout, "  \x1b[7m ◆ {}) {:<20} \x1b[0m", idx + 1, text)
    } else {
        write!(stdout, "    {}) {:<20} ", idx + 1, text)
    };
}

/// Repaint the option rows plus the preview area using only relative cursor
/// moves (`MoveUp`/`MoveDown`) and newline-free writes. Absolute row
/// addressing breaks when the menu is drawn at the bottom of the screen —
/// each `println!` scrolls the buffer and every recorded row goes stale —
/// and long lines that wrap do the same. `rest_row` clamps the initial
/// MoveUp so a menu taller than the viewport cannot overshoot the top.
fn redraw_menu(
    options: &[&str],
    selected: usize,
    preview_lines: Option<&[String]>,
    preview_height: usize,
    rest_row: u16,
    max_opts: usize,
) {
    let mut stdout = io::stdout();
    // Rows from the first visible option row to the rest position: window +
    // blank + hint + blank + preview rows + trailing blank.
    let top_to_rest = (max_opts + preview_height + 4) as u16;
    let _ = execute!(stdout, MoveUp(top_to_rest.min(rest_row)), MoveToColumn(0));
    let (start, end) = window_bounds(options.len(), selected, max_opts);
    for row in start..end {
        let _ = execute!(stdout, Clear(ClearType::CurrentLine));
        write_option_row(&mut stdout, options, row, selected, start, end);
        let _ = execute!(stdout, MoveDown(1), MoveToColumn(0));
    }
    // Skip the blank line, hint row, and blank line between options and
    // the preview area.
    let _ = execute!(stdout, MoveDown(3), MoveToColumn(0));
    for j in 0..preview_height {
        let _ = execute!(stdout, Clear(ClearType::CurrentLine));
        if let Some(line) = preview_lines.and_then(|l| l.get(j)) {
            let _ = write!(
                stdout,
                "  {}",
                clip_line(line, (term_width() as usize).saturating_sub(4))
            );
        }
        let _ = execute!(stdout, MoveDown(1), MoveToColumn(0));
    }
    // Land back on the rest row (trailing blank below the preview).
    let _ = execute!(stdout, MoveDown(1), MoveToColumn(0));
    stdout.flush().ok();
}

/// Truncate `s` to at most `max` visible display columns, passing ANSI escape
/// sequences through untouched and appending a reset when anything was cut.
/// Wide glyphs (CJK, emoji) count as two columns so Chinese text clips at the
/// same physical edge as ASCII.
fn clip_line(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len());
    let mut visible = 0usize;
    let mut cut = false;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            out.push(ch);
            for c in chars.by_ref() {
                out.push(c);
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        let width = char_width(ch);
        if visible + width > max {
            cut = true;
            break;
        }
        visible += width;
        out.push(ch);
    }
    if cut {
        out.push_str("\x1b[0m");
    }
    out
}

/// Get terminal width. Falls back to 80 if detection fails or the
/// terminal reports a real 0 width (niubash#852: a zero is not a
/// size — layout math would paint past the edge).
pub fn term_width() -> u16 {
    crate::terminal::sane_terminal_size(terminal::size().ok()).0
}

/// Get terminal height. Falls back to 24 if detection fails or the
/// terminal reports a real 0 height (niubash#852).
pub fn term_height() -> u16 {
    crate::terminal::sane_terminal_size(terminal::size().ok()).1
}

/// Print content in a side-by-side layout: logo on the left, wizard text on the right.
///
/// `logo_lines` are pre-rendered ANSI logo lines. `content_lines` are plain text.
/// If the terminal is too narrow (< `min_width`), falls back to stacked mode.
pub fn print_side_by_side(logo_lines: &[String], content_lines: &[String], min_width: u16) {
    let width = term_width();
    if width < min_width || logo_lines.is_empty() {
        // Stacked fallback
        for line in logo_lines {
            println!("{}", line);
        }
        for line in content_lines {
            println!("{}", line);
        }
        return;
    }

    let logo_w = (width / 3).min(50) as usize;
    let gap = 2;

    let max_rows = logo_lines.len().max(content_lines.len());
    for row in 0..max_rows {
        let logo_part = logo_lines.get(row).map(|s| s.as_str()).unwrap_or("");
        let content_part = content_lines.get(row).map(|s| s.as_str()).unwrap_or("");

        // Strip ANSI from logo for width measurement
        let visible_len = strip_ansi_width(logo_part);
        let padding = if visible_len < logo_w {
            " ".repeat(logo_w - visible_len)
        } else {
            String::new()
        };

        print!("{}{}", logo_part, padding);
        // Move to logo column boundary
        print!("\x1b[{}C{}", gap, content_part);
        println!();
    }
}

/// Estimate visible width of a string in terminal columns, ignoring ANSI
/// escape sequences. Wide glyphs (CJK, emoji) count as two columns.
fn strip_ansi_width(s: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
            continue;
        }
        if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        width += char_width(ch);
    }
    width
}

/// Display width of one character in terminal columns: 2 for East Asian wide
/// and fullwidth glyphs plus emoji, 1 otherwise. Covers the ranges that
/// actually appear in wizard text; unlisted code points fall back to 1.
pub(crate) fn char_width(ch: char) -> usize {
    let c = ch as u32;
    let wide = matches!(c,
        0x1100..=0x115F // Hangul Jamo
            | 0x2E80..=0x303E // CJK radicals, Kangxi, CJK punctuation
            | 0x3041..=0x33FF // Hiragana, Katakana, CJK compat, wide punct
            | 0x3400..=0x4DBF // CJK Extension A
            | 0x4E00..=0x9FFF // CJK Unified Ideographs
            | 0xA000..=0xA4CF // Yi
            | 0xAC00..=0xD7A3 // Hangul syllables
            | 0xF900..=0xFAFF // CJK Compatibility Ideographs
            | 0xFE30..=0xFE4F // CJK compatibility forms
            | 0xFF00..=0xFF60 // Fullwidth forms
            | 0xFFE0..=0xFFE6
            | 0x1F004 | 0x1F0CF
            | 0x1F18E | 0x1F191..=0x1F19A
            | 0x1F200..=0x1F320 // Enclosed ideographs, emoji
            | 0x1F32D..=0x1F335
            | 0x1F337..=0x1F37C
            | 0x1F37E..=0x1F393
            | 0x1F3A0..=0x1F3CA
            | 0x1F3CF..=0x1F3D3
            | 0x1F3E0..=0x1F3F0
            | 0x1F3F4
            | 0x1F3F8..=0x1F43E
            | 0x1F440
            | 0x1F442..=0x1F4FC
            | 0x1F4FF..=0x1F53D
            | 0x1F54B..=0x1F54E
            | 0x1F550..=0x1F567
            | 0x1F57A
            | 0x1F595..=0x1F596
            | 0x1F5A4
            | 0x1F5FB..=0x1F64F
            | 0x1F680..=0x1F6C5
            | 0x1F6CC
            | 0x1F6D0..=0x1F6D2
            | 0x1F6EB..=0x1F6EC
            | 0x1F910..=0x1F93E
            | 0x1F940..=0x1F970
            | 0x1F973..=0x1F976
            | 0x1F97A
            | 0x1F97C..=0x1F9A2
            | 0x1F9B0..=0x1F9B9
            | 0x1F9C0..=0x1F9C2
            | 0x1F9D0..=0x1F9FF
            | 0x20024..=0x20042 // Heavy width variants used by logos
    );
    if wide {
        2
    } else {
        1
    }
}

/// Terminal display width of a string, ANSI escapes excluded.
pub(crate) fn display_width(s: &str) -> usize {
    strip_ansi_width(s)
}

/// Pad `s` with spaces to `width` display columns (no-op when already wider).
/// Format specs like `{:<14}` pad by character count and misalign columns
/// once CJK text is involved; wizard label columns use this instead.
pub(crate) fn pad_display(s: &str, width: usize) -> String {
    let len = display_width(s);
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

/// RAII guard for crossterm raw mode.
struct RawMode;

impl RawMode {
    fn enter() -> Self {
        let _ = terminal::enable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, cursor::Hide);
        RawMode
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, cursor::Show);
        let _ = terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn release_events_are_ignored() {
        // Windows reports Press + Release for every keystroke; handling both
        // would move the cursor twice and let a key-up confirm the next menu.
        {
            let kind = KeyEventKind::Release;
            let ev = KeyEvent::new_with_kind(KeyCode::Down, KeyModifiers::NONE, kind);
            assert_eq!(key_action(&ev, 0, 3), MenuAction::Ignore);
            let ev = KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, kind);
            assert_eq!(key_action(&ev, 0, 3), MenuAction::Ignore);
        }
    }

    #[test]
    fn arrow_press_moves_once() {
        assert_eq!(key_action(&key(KeyCode::Down), 0, 3), MenuAction::MoveTo(1));
        assert_eq!(key_action(&key(KeyCode::Up), 2, 3), MenuAction::MoveTo(1));
        // Holding a key repeats it; repeats should still navigate.
        let ev = KeyEvent::new_with_kind(KeyCode::Down, KeyModifiers::NONE, KeyEventKind::Repeat);
        assert_eq!(key_action(&ev, 0, 3), MenuAction::MoveTo(1));
    }

    #[test]
    fn arrows_clamp_at_edges() {
        assert_eq!(key_action(&key(KeyCode::Up), 0, 3), MenuAction::MoveTo(0));
        assert_eq!(key_action(&key(KeyCode::Down), 2, 3), MenuAction::MoveTo(2));
    }

    #[test]
    fn enter_confirms_and_esc_uses_default() {
        assert_eq!(key_action(&key(KeyCode::Enter), 1, 3), MenuAction::Confirm);
        assert_eq!(key_action(&key(KeyCode::Esc), 1, 3), MenuAction::UseDefault);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(key_action(&ctrl_c, 1, 3), MenuAction::Abort);
    }

    #[test]
    fn digits_jump_to_option() {
        assert_eq!(
            key_action(&key(KeyCode::Char('2')), 0, 3),
            MenuAction::MoveTo(1)
        );
        assert_eq!(
            key_action(&key(KeyCode::Char('9')), 0, 3),
            MenuAction::Ignore
        );
        assert_eq!(
            key_action(&key(KeyCode::Char('0')), 0, 3),
            MenuAction::Ignore
        );
    }

    #[test]
    fn wide_glyphs_count_two_columns() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('│'), 1);
        assert_eq!(char_width('中'), 2);
        assert_eq!(char_width('，'), 2);
        assert_eq!(char_width('Ａ'), 2);
        assert_eq!(display_width("中文ab"), 6);
        assert_eq!(display_width("\x1b[7m中文\x1b[0m"), 4);
    }

    #[test]
    fn clip_line_uses_display_width() {
        // Five ASCII chars fit in five columns; three CJK chars do not.
        assert_eq!(clip_line("abcde", 5), "abcde");
        // "中文a" fills all five columns exactly; the trailing "bc" is cut,
        // so a reset is appended to close any open styling.
        assert_eq!(clip_line("中文abc", 5), "中文a\x1b[0m");
        // A wide glyph that would straddle the limit is cut whole.
        assert_eq!(clip_line("ab中c", 3), "ab\x1b[0m");
        // ANSI sequences never consume budget.
        assert_eq!(clip_line("\x1b[7m中文\x1b[0m", 4), "\x1b[7m中文\x1b[0m");
    }

    #[test]
    fn pad_display_aligns_mixed_scripts() {
        assert_eq!(pad_display("中文", 6), "中文  ");
        assert_eq!(pad_display("abc", 2), "abc");
        assert_eq!(display_width(&pad_display("中文ab", 10)), 10);
    }
}
