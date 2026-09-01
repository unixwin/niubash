use std::io::{self, Write};
use std::thread;
use std::time::Duration;

const FRAMES: &[&str] = &[
    // Frame 0: arms up
    "    \\O/    \n     |     \n    / \\    ",
    // Frame 1: right arm up
    "     O\\    \n     |     \n    / \\    ",
    // Frame 2: neutral
    "     O     \n     |     \n    / \\    ",
    // Frame 3: left arm up
    "    /O     \n     |     \n    / \\    ",
    // Frame 4: arms up again (bounce)
    "    \\O/    \n     |     \n    / \\    ",
    // Frame 5: lean right
    "      O\\   \n      |    \n     / \\   ",
    // Frame 6: neutral
    "     O     \n     |     \n    / \\    ",
    // Frame 7: lean left
    "   /O      \n    |      \n   / \\     ",
];

const COLORS: &[&str] = &[
    "\x1b[1;91m", // red
    "\x1b[1;93m", // yellow
    "\x1b[1;92m", // green
    "\x1b[1;96m", // cyan
    "\x1b[1;95m", // magenta
    "\x1b[1;94m", // blue
];

const FRAME_MS: u64 = 200;
const TOTAL_FRAMES: usize = 24;

pub(crate) fn run() -> anyhow::Result<i32> {
    let mut stdout = io::stdout();

    stdout.write_all(b"\x1b[?25l")?;
    stdout.write_all(b"\x1b[2J")?;
    stdout.write_all(b"\x1b[H")?;

    // Title
    write!(stdout, "\x1b[1;95m")?;
    stdout.write_all(b"~ niubash party mode ~\n\n")?;
    stdout.write_all(b"\x1b[0m")?;

    let _base_y = 3; // start below title

    for i in 0..TOTAL_FRAMES {
        stdout.write_all(b"\x1b[H")?;
        // Title
        write!(stdout, "\x1b[1;95m")?;
        stdout.write_all(b"~ niubash party mode ~\n\n")?;
        stdout.write_all(b"\x1b[0m")?;

        let frame_idx = i % FRAMES.len();
        let color_idx = i % COLORS.len();
        let frame = FRAMES[frame_idx];
        let color = COLORS[color_idx];

        // Print decorative border
        write!(stdout, "{}", color)?;
        stdout.write_all(b"+")?;
        for _ in 0..12 {
            stdout.write_all(b"-")?;
        }
        stdout.write_all(b"+\n")?;

        for line in frame.lines() {
            stdout.write_all(b"|")?;
            write!(stdout, "{}", color)?;
            stdout.write_all(line.as_bytes())?;
            stdout.write_all(b"\x1b[0m")?;
            // pad to 12 chars
            let pad = 12usize.saturating_sub(line.len());
            for _ in 0..pad {
                stdout.write_all(b" ")?;
            }
            stdout.write_all(b"|\n")?;
        }

        stdout.write_all(b"+")?;
        for _ in 0..12 {
            stdout.write_all(b"-")?;
        }
        stdout.write_all(b"+\n")?;

        // Music notes
        write!(stdout, "{}", color)?;
        let notes = ["♪", "♫", "♬", "♩"];
        let note = notes[i % notes.len()];
        write!(stdout, "\n   {}  {}  {}\n", note, note, note)?;
        stdout.write_all(b"\x1b[0m")?;

        stdout.flush()?;
        thread::sleep(Duration::from_millis(FRAME_MS));
    }

    stdout.write_all(b"\x1b[2J\x1b[H")?;
    stdout.write_all(b"\x1b[?25h")?;
    stdout.flush()?;
    Ok(0)
}
