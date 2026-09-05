use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute,
    terminal::{self, ClearType},
};

const FRAME_MS: u64 = 250;
const TOTAL_FRAMES: usize = 13;

const BRAND: &str = "\x1b[38;5;214m";
const DIM: &str = "\x1b[38;5;245m";
const RESET: &str = "\x1b[0m";

// The niubash bull in three poses: alert, blinking, and exhaling. Each
// frame is padded to twelve columns so a clear-screen redraw never
// leaves a ragged edge.
const FRAMES: &[&[&str]] = &[
    &[
        r"      ,-.  ,-. ",
        r"     /   \/   \ ",
        r"    |  o    o  | ",
        r"    |    --    | ",
        r"     \   __   / ",
        r"      '------' ",
    ],
    &[
        r"      ,-.  ,-. ",
        r"     /   \/   \ ",
        r"    |  -    -  | ",
        r"    |    --    | ",
        r"     \   __   / ",
        r"      '------' ",
    ],
    &[
        r"      ,-.  ,-. ",
        r"     /   \/   \ ",
        r"    |  o    o  | ",
        r"    |    --    | ",
        r"     \   __   / ",
        r"      '------' ",
        r"      ~   ~   ",
    ],
];

/// `cow`: the niubash bull, animated. Runs for a few seconds and exits, or
/// exits early on `q`, enter, or space. Refuses to draw when stdout is not
/// a terminal so piped output stays clean.
pub(crate) fn run() -> anyhow::Result<i32> {
    if !crate::terminal::stdout_is_terminal() {
        return Ok(0);
    }
    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
    let code = play(&mut stdout);
    execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    code
}

fn play(stdout: &mut io::Stdout) -> anyhow::Result<i32> {
    let start = std::time::Instant::now();
    let duration = Duration::from_millis(FRAME_MS * TOTAL_FRAMES as u64);
    for frame in 0..TOTAL_FRAMES {
        draw(stdout, frame, false)?;
        if event::poll(Duration::from_millis(FRAME_MS / 4))? {
            if let Event::Key(key) = event::read()? {
                if matches!(
                    key.code,
                    KeyCode::Char('q') | KeyCode::Char('Q')
                        | KeyCode::Enter
                        | KeyCode::Char(' ')
                        | KeyCode::Esc
                ) {
                    break;
                }
            }
        }
    }

    // One last breath, then hand the screen back to the REPL.
    draw(stdout, 2, true)?;
    let elapsed = start.elapsed();
    if elapsed < duration {
        thread::sleep(duration.saturating_sub(elapsed));
    }
    Ok(0)
}

fn draw(stdout: &mut io::Stdout, frame: usize, done: bool) -> anyhow::Result<()> {
    let lines = FRAMES[frame % FRAMES.len()];
    execute!(stdout, terminal::Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    write!(stdout, "  {BRAND}~ niubash cow ~{RESET}\n\n")?;
    for (index, line) in lines.iter().enumerate() {
        let color = if index == 0 || index == 1 || index + 1 == lines.len() {
            BRAND
        } else {
            DIM
        };
        write!(stdout, "  {color}{line}{RESET}\n")?;
    }
    write!(stdout, "\n")?;
    if done {
        write!(stdout, "  \x1b[1;96mmuuu~{RESET}\n")?;
    } else {
        write!(stdout, "  \x1b[90mq, enter or space to leave{RESET}\n")?;
    }
    stdout.flush()?;
    Ok(())
}
