//! Interactive terminal menu with arrow-key navigation.
//!
//! Provides `interactive_choice` for arrow/jk/Enter/ESC selection and
//! `SideBySideLayout` for logo-left / wizard-right rendering.

use std::io::{self, Write};

use crossterm::{
    cursor::{self, MoveToColumn, MoveToRow},
    event::{self, Event, KeyCode, KeyModifiers},
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
}

/// Run an interactive arrow-key menu. Returns `Selection::Confirmed(index)` or
/// `Selection::UseDefault` on ESC / Ctrl-C.
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
    let _raw = RawMode::enter();

    let mut selected = default_idx.min(options.len().saturating_sub(1));

    // Initial render
    println!();
    println!("  {}", label);
    if !help.is_empty() {
        for line in help.lines() {
            println!("  {}", line);
        }
    }
    println!();
    render_options(options, selected);
    if !help.is_empty() {
        println!();
        println!("  \x1b[2m↑↓ navigate  Enter confirm  ESC default\x1b[0m");
    }
    println!();
    io::stdout().flush().ok();

    let start_row = cursor::position().map(|p| p.1).unwrap_or(0);
    let menu_top = start_row - options.len() as u16 - if help.is_empty() { 0 } else { 2 };

    loop {
        let key = match event::read() {
            Ok(Event::Key(k)) => k,
            _ => continue,
        };
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if !modifiers.contains(KeyModifiers::CONTROL) => {
                let old = selected;
                selected = selected.saturating_sub(1);
                if old != selected {
                    redraw_option(options, old, selected, menu_top);
                }
            }
            KeyCode::Down | KeyCode::Char('j') if !modifiers.contains(KeyModifiers::CONTROL) => {
                let old = selected;
                selected = (selected + 1).min(options.len() - 1);
                if old != selected {
                    redraw_option(options, old, selected, menu_top);
                }
            }
            KeyCode::Enter => {
                return Selection::Confirmed(selected);
            }
            KeyCode::Esc | KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                return Selection::UseDefault;
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(idx) = c.to_digit(10) {
                    let idx = idx as usize;
                    if idx >= 1 && idx <= options.len() {
                        let old = selected;
                        selected = idx - 1;
                        if old != selected {
                            redraw_option(options, old, selected, menu_top);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Render all options, highlighting `selected`.
fn render_options(options: &[&str], selected: usize) {
    for (i, opt) in options.iter().enumerate() {
        print_option(i, *opt, i == selected);
    }
}

/// Print a single option line. `highlight` applies inverse colors.
fn print_option(idx: usize, opt: &str, highlight: bool) {
    if highlight {
        print!("  \x1b[7m ◆ {}) {:<20} \x1b[0m", idx + 1, opt);
    } else {
        print!("    {}) {:<20} ", idx + 1, opt);
    }
    println!();
}

/// Redraw only two option lines (old and new selection) to minimize flicker.
fn redraw_option(options: &[&str], old: usize, new: usize, menu_top: u16) {
    let mut stdout = io::stdout();

    // Redraw old selection (unhighlighted)
    let old_row = menu_top + old as u16;
    let _ = execute!(
        stdout,
        MoveToRow(old_row),
        MoveToColumn(0),
        Clear(ClearType::CurrentLine)
    );
    print_option(old, options[old], false);

    // Redraw new selection (highlighted)
    let new_row = menu_top + new as u16;
    let _ = execute!(
        stdout,
        MoveToRow(new_row),
        MoveToColumn(0),
        Clear(ClearType::CurrentLine)
    );
    print_option(new, options[new], true);

    // Move cursor below menu to avoid visual artifacts
    let below = menu_top + options.len() as u16;
    let _ = execute!(stdout, MoveToRow(below), MoveToColumn(0));
    stdout.flush().ok();
}

/// Get terminal width. Falls back to 80 if detection fails.
pub fn term_width() -> u16 {
    terminal::size().map(|(w, _)| w).unwrap_or(80)
}

/// Get terminal height. Falls back to 24 if detection fails.
pub fn term_height() -> u16 {
    terminal::size().map(|(_, h)| h).unwrap_or(24)
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

/// Estimate visible width of a string, ignoring ANSI escape sequences.
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
        width += 1;
    }
    width
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
