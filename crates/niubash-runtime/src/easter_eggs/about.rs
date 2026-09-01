use std::io::{self, Write};

pub(crate) fn run() -> anyhow::Result<i32> {
    let mut stdout = io::stdout();
    stdout.write_all(b"\x1b[2J\x1b[H")?;
    stdout.flush()?;
    print_logo()?;
    print_info()?;
    Ok(0)
}

fn print_logo() -> anyhow::Result<i32> {
    let mut stdout = io::stdout();
    // Cyan
    stdout.write_all(b"\x1b[1;96m")?;
    stdout.write_all(b"                _               _\n")?;
    stdout.write_all(b"               (_)             | |\n")?;
    // Yellow
    stdout.write_all(b"\x1b[1;93m")?;
    stdout.write_all(b" _ __   __ _ __ _ _ __   __ _| |\n")?;
    stdout.write_all(b"| '_ \\ / _` / _` | '_ \\ / _` | |\n")?;
    // Green
    stdout.write_all(b"\x1b[1;92m")?;
    stdout.write_all(b"| | | | (_| | (_| | | | | (_| | |\n")?;
    stdout.write_all(b"|_| |_|\\__, |\\__,_|_| |_|\\__,_|_|\n")?;
    // Magenta
    stdout.write_all(b"\x1b[1;95m")?;
    stdout.write_all(b"        __/ |\n")?;
    stdout.write_all(b"       |___/\n")?;
    // Reset
    stdout.write_all(b"\x1b[0m")?;
    stdout.flush()?;
    Ok(0)
}

fn print_info() -> anyhow::Result<i32> {
    let mut stdout = io::stdout();

    // Title
    stdout.write_all(b"\n  \x1b[1;97mniubash\x1b[0m")?;
    stdout.write_all(b" - A bash-compatible shell for Windows\n")?;
    stdout.write_all(b"  \x1b[90m--------------------------------------------\x1b[0m\n")?;

    // Features
    stdout.write_all(b"\n  \x1b[1;93mFeatures:\x1b[0m\n")?;
    stdout.write_all(b"    \x1b[92m*\x1b[0m Bash-compatible scripting via rubash\n")?;
    stdout.write_all(b"    \x1b[92m*\x1b[0m Native Windows integration (winuxcmd)\n")?;
    stdout.write_all(b"    \x1b[92m*\x1b[0m Plugin system (oh-my-niu)\n")?;
    stdout.write_all(b"    \x1b[92m*\x1b[0m Reedline-based interactive input\n")?;

    // Hidden commands
    stdout.write_all(b"\n  \x1b[1;93mHidden commands:\x1b[0m\n")?;
    stdout.write_all(b"    \x1b[96mmatrix\x1b[0m  - Take the red pill\n")?;
    stdout.write_all(b"    \x1b[96mparty\x1b[0m   - Dance time\n")?;
    stdout.write_all(b"    \x1b[96mgame\x1b[0m    - Snake game\n")?;
    stdout.write_all(b"    \x1b[96mabout\x1b[0m   - This screen\n")?;

    // Footer
    stdout.write_all(b"\n  \x1b[90mgithub.com/unixwin/niubash\x1b[0m\n")?;

    stdout.flush()?;
    Ok(0)
}
