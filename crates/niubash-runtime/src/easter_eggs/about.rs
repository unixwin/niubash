use std::io::{self, Write};

use crossterm::{cursor, event, execute, terminal};

/// `about`: version banner, features, and the hidden-command index. Refuses to
/// draw when stdout is not a terminal so piped output stays clean. Waits for
/// one keypress before returning so the REPL prompt cannot push the screen
/// away before it is read.
pub(crate) fn run() -> anyhow::Result<i32> {
    if !crate::terminal::stdout_is_terminal() {
        return Ok(0);
    }
    let mut stdout = io::stdout();
    stdout.write_all(b"\x1b[2J\x1b[H")?;
    stdout.flush()?;
    print_logo()?;
    print_info()?;
    wait_for_key()?;
    Ok(0)
}

/// Block until any key arrives; raw mode is best-effort because the caller
/// already verified stdout is a terminal.
fn wait_for_key() -> anyhow::Result<()> {
    let mut stdout = io::stdout();
    write!(stdout, "\n  \x1b[90mpress any key to continue\x1b[0m")?;
    stdout.flush()?;
    if terminal::enable_raw_mode().is_ok() {
        let _ = execute!(stdout, cursor::Hide);
        let _ = event::read();
        let _ = execute!(stdout, cursor::Show);
        let _ = terminal::disable_raw_mode();
        // Clear the line so the pressed key cannot echo into the prompt.
        write!(stdout, "\r\x1b[K")?;
        stdout.flush()?;
    }
    Ok(())
}

fn print_logo() -> anyhow::Result<i32> {
    let mut stdout = io::stdout();
    // pyfiglet -f standard "niubash"; one accent color per band of lines.
    // Cyan
    stdout.write_all(b"\x1b[1;96m")?;
    stdout.write_all(b"       _       _               _     \n")?;
    stdout.write_all(b" _ __ (_)_   _| |__   __ _ ___| |__  \n")?;
    // Yellow
    stdout.write_all(b"\x1b[1;93m")?;
    stdout.write_all(b"| '_ \\| | | | | '_ \\ / _` / __| '_ \\ \n")?;
    // Green
    stdout.write_all(b"\x1b[1;92m")?;
    stdout.write_all(b"| | | | | |_| | |_) | (_| \\__ \\ | | |\n")?;
    // Magenta
    stdout.write_all(b"\x1b[1;95m")?;
    stdout.write_all(b"|_| |_|_|\\__,_|_.__/ \\__,_|___/_| |_|\n")?;
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
    stdout.write_all(b"    \x1b[92m*\x1b[0m External plugin sources (oh-my-bash et al.)\n")?;
    stdout.write_all(b"    \x1b[92m*\x1b[0m Reedline-based interactive input\n")?;

    // Runtime facts — the self-verifiable "no emulation" claim
    stdout.write_all(b"\n  \x1b[1;93mRuntime:\x1b[0m\n")?;
    stdout.write_all(b"    \x1b[92m*\x1b[0m native Win32 process - no POSIX emulation layer\n")?;
    stdout
        .write_all(b"    \x1b[92m*\x1b[0m no cygwin1.dll / msys-2.0.dll anywhere in the stack\n")?;
    stdout.write_all(
        b"    \x1b[92m*\x1b[0m no path conversion layer: native paths are first-class\n",
    )?;

    // Where to go next
    stdout.write_all(b"\n  \x1b[1;93mNext steps:\x1b[0m\n")?;
    stdout
        .write_all(b"    \x1b[96mniu setup\x1b[0m    - re-run the setup wizard (theme, tools)\n")?;
    stdout.write_all(
        b"    \x1b[96m~/.niubashrc\x1b[0m  - your startup config, plain bash syntax\n",
    )?;

    // Games
    stdout.write_all(b"\n  \x1b[1;93mGames:\x1b[0m\n")?;
    stdout.write_all(b"    \x1b[96msnake\x1b[0m   - Eat the apples, do not eat yourself\n")?;

    // Hidden commands
    stdout.write_all(b"\n  \x1b[1;93mHidden commands:\x1b[0m\n")?;
    stdout.write_all(b"    \x1b[96mabout\x1b[0m   - This screen\n")?;

    // Footer
    stdout.write_all(b"\n  \x1b[90mgithub.com/unixwin/niubash\x1b[0m\n")?;

    stdout.flush()?;
    Ok(0)
}
