use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use crossterm::{cursor, execute, terminal};

const MATRIX_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz@#$%^&*()_+-=[]{}|;':\",./<>?\\~`";
const FRAME_MS: u64 = 50;
const DURATION_SECS: u64 = 3;

struct Column {
    y: usize,
    speed: usize,
    char_idx: usize,
}

// Lock-free seed: the old `static mut` was an unsafe data race waiting to
// happen if an egg was ever driven from two threads.
static SEED: AtomicU64 = AtomicU64::new(12345);

fn pseudo_random() -> u64 {
    let mut seed = SEED.load(Ordering::Relaxed);
    loop {
        let next = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        match SEED.compare_exchange_weak(seed, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next >> 33,
            Err(actual) => seed = actual,
        }
    }
}

/// `matrix`: three seconds of green rain. Refuses to draw when stdout is not
/// a terminal so piped output stays clean.
pub(crate) fn run() -> anyhow::Result<i32> {
    if !crate::terminal::stdout_is_terminal() {
        return Ok(0);
    }
    let (cols, rows) = terminal::size()
        .map(|(c, r)| (c as usize, r as usize))
        .unwrap_or((80, 24));
    if cols == 0 || rows == 0 {
        return Ok(0);
    }

    let mut stdout = io::stdout();
    // Alternate screen so the rain does not destroy the REPL scrollback.
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
    let code = rain(&mut stdout, cols, rows);
    execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen)?;
    code
}

fn rain(stdout: &mut io::Stdout, cols: usize, rows: usize) -> anyhow::Result<i32> {
    let mut columns: Vec<Column> = (0..cols)
        .step_by(2)
        .map(|_| Column {
            y: 0,
            speed: 1 + (pseudo_random() as usize % 3),
            char_idx: pseudo_random() as usize % MATRIX_CHARS.len(),
        })
        .collect();

    let start = std::time::Instant::now();
    let mut tick: u64 = 0;

    while start.elapsed() < Duration::from_secs(DURATION_SECS) {
        tick += 1;
        for (i, col) in columns.iter_mut().enumerate() {
            if tick % col.speed as u64 == 0 {
                let x = i * 2;
                if col.y > 0 && col.y <= rows {
                    let y = col.y - 1;
                    write!(stdout, "\x1b[{};{}H\x1b[32;2m{}", y + 1, x + 1, MATRIX_CHARS[col.char_idx] as char)?;
                }
                if col.y < rows {
                    let y = col.y;
                    write!(stdout, "\x1b[{};{}H\x1b[1;97m{}", y + 1, x + 1, MATRIX_CHARS[col.char_idx] as char)?;
                }
                col.char_idx = (col.char_idx + 1) % MATRIX_CHARS.len();
                if col.y >= rows {
                    col.y = 0;
                    col.speed = 1 + (pseudo_random() as usize % 3);
                } else {
                    col.y += 1;
                }
            }
        }
        stdout.flush()?;
        thread::sleep(Duration::from_millis(FRAME_MS));
    }
    Ok(0)
}
