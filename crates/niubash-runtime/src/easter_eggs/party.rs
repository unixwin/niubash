use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use crossterm::{cursor, execute, terminal::{self, ClearType}};

const RESET: &str = "\x1b[0m";

// Each frame is three lines: arms, spine, legs. Raw strings keep the ASCII
// art readable without backslash escaping.
const FRAMES: &[&[&str]] = &[
    // Arms up
    &[r"    \O/    ", r"     |     ", r"    / \    "],
    // Right arm up
    &[r"     O\    ", r"     |     ", r"    / \    "],
    // Neutral
    &[r"     O     ", r"     |     ", r"    / \    "],
    // Left arm up
    &[r"    /O     ", r"     |     ", r"    / \    "],
    // Arms up again (bounce)
    &[r"    \O/    ", r"     |     ", r"    / \    "],
    // Lean right
    &[r"      O\   ", r"      |    ", r"     / \   "],
    // Neutral
    &[r"     O     ", r"     |     ", r"    / \    "],
    // Lean left
    &[r"   /O      ", r"    |      ", r"   / \     "],
];

const COLORS: &[&str] = &[
    "\x1b[1;91m", // red
    "\x1b[1;93m", // yellow
    "\x1b[1;92m", // green
    "\x1b[1;96m", // cyan
    "\x1b[1;95m", // magenta
    "\x1b[1;94m", // blue
];

const NOTES: &[&str] = &["\u{266a}", "\u{266b}", "\u{266c}", "\u{2669}"];

const FRAME_MS: u64 = 200;
const TOTAL_FRAMES: usize = 24;

/// `party`: 24 frames of a tiny break-dance. Refuses to draw when stdout is
/// not a terminal so piped output stays clean.
pub(crate) fn run() -> anyhow::Result<i32> {
    if !crate::terminal::stdout_is_terminal() {
        return Ok(0);
    }
    let mut stdout = io::stdout();
    // Alternate screen so the dance does not destroy the REPL scrollback.
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
    let code = dance(&mut stdout);
    execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen)?;
    code
}

fn dance(stdout: &mut io::Stdout) -> anyhow::Result<i32> {
    for frame in 0..TOTAL_FRAMES {
        draw_frame(stdout, frame)?;
        thread::sleep(Duration::from_millis(FRAME_MS));
    }
    Ok(0)
}

fn draw_frame(stdout: &mut io::Stdout, frame: usize) -> anyhow::Result<()> {
    // A full clear (not just a home cursor) so a short frame cannot leave
    // the tail of a longer one on screen.
    execute!(stdout, terminal::Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    write!(stdout, "\x1b[1;95m  ~ niubash party mode ~{RESET}\n\n")?;

    let color = COLORS[frame % COLORS.len()];
    let lines = FRAMES[frame % FRAMES.len()];

    write!(stdout, "{color}+{RESET}")?;
    for _ in 0..12 {
        stdout.write_all(b"-")?;
    }
    write!(stdout, "+{RESET}\n")?;

    for line in lines {
        write!(stdout, "{color}|")?;
        stdout.write_all(line.as_bytes())?;
        let pad = 12usize.saturating_sub(line.len());
        for _ in 0..pad {
            stdout.write_all(b" ")?;
        }
        write!(stdout, "|{RESET}\n")?;
    }

    write!(stdout, "{color}+{RESET}")?;
    for _ in 0..12 {
        stdout.write_all(b"-")?;
    }
    write!(stdout, "+{RESET}\n")?;

    let note = NOTES[frame % NOTES.len()];
    write!(stdout, "\n   {color}{note}  {note}  {note}{RESET}\n")?;
    stdout.flush()?;
    Ok(())
}
