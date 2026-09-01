use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use crossterm::terminal;

const MATRIX_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz@#$%^&*()_+-=[]{}|;':\",./<>?\\~`";
const FRAME_MS: u64 = 50;
const DURATION_SECS: u64 = 3;

struct Column {
    y: usize,
    speed: usize,
    char_idx: usize,
}

pub(crate) fn run() -> anyhow::Result<i32> {
    let mut stdout = io::stdout();
    let (cols, rows) = terminal::size()
        .map(|(c, r)| (c as usize, r as usize))
        .unwrap_or((80, 24));
    if cols == 0 || rows == 0 {
        return Ok(0);
    }

    let mut columns: Vec<Column> = (0..cols)
        .step_by(2)
        .map(|_| Column {
            y: 0,
            speed: 1 + (pseudo_random() as usize % 3),
            char_idx: pseudo_random() as usize % MATRIX_CHARS.len(),
        })
        .collect();

    stdout.write_all(b"\x1b[?25l")?; // hide cursor
    stdout.write_all(b"\x1b[2J")?; // clear screen
    stdout.write_all(b"\x1b[H")?; // home
    stdout.flush()?;

    let start = std::time::Instant::now();
    let mut tick: u64 = 0;

    while start.elapsed() < Duration::from_secs(DURATION_SECS) {
        tick += 1;
        for (i, col) in columns.iter_mut().enumerate() {
            if tick % col.speed as u64 == 0 {
                let x = i * 2;
                if col.y > 0 && col.y <= rows {
                    let y = col.y - 1;
                    write!(
                        stdout,
                        "\x1b[{};{}H\x1b[32;2m{}",
                        y + 1,
                        x + 1,
                        MATRIX_CHARS[col.char_idx] as char
                    )?;
                }
                if col.y < rows {
                    let y = col.y;
                    write!(
                        stdout,
                        "\x1b[{};{}H\x1b[1;97m{}",
                        y + 1,
                        x + 1,
                        MATRIX_CHARS[col.char_idx] as char
                    )?;
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

    stdout.write_all(b"\x1b[2J\x1b[H")?; // clear and home
    stdout.write_all(b"\x1b[?25h")?; // show cursor
    stdout.flush()?;
    Ok(0)
}

static mut SEED: u64 = 12345;
fn pseudo_random() -> u64 {
    unsafe {
        SEED = SEED
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        SEED >> 33
    }
}
