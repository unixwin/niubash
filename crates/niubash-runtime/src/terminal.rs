//! Terminal detection helpers for deciding when the reedline UI is safe to run.

/// Return true only when stdin, stdout, and stderr are attached to an interactive terminal.
///
/// If any stream is redirected or piped, callers should avoid full-screen/line-editor UI and
/// execute deterministic script-style surfaces instead.
pub fn stdio_is_interactive() -> bool {
    platform::stdin_is_terminal()
        && platform::stdout_is_terminal()
        && platform::stderr_is_terminal()
}

/// Return true only when stdout is attached to a real terminal.
///
/// Full-screen easter eggs must refuse to draw when stdout is a pipe or a
/// file: escape sequences in redirected output corrupt CI logs and break
/// the deterministic non-interactive contract.
pub fn stdout_is_terminal() -> bool {
    platform::stdout_is_terminal()
}

/// Current terminal width in columns, falling back to the 80-column
/// historical default when the size cannot be queried (piped output, CI
/// consoles mid-reconfigure).
///
/// Used by the prompt channel's right-align split (niubash#169): the jump
/// detection needs the real margin to recognize a clamping cursor-forward.
pub fn terminal_columns() -> u16 {
    crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80)
}

/// Live terminal size as `(columns, rows)`, or `None` when the console
/// query fails (no console attached, redirected output). The same
/// CONOUT$ query `terminal_columns` folds to a width — kept as a pair
/// so the winsize bind (niubash#799) can write both `$COLUMNS` and
/// `$LINES` from one read.
pub fn terminal_size() -> Option<(u16, u16)> {
    crossterm::terminal::size().ok()
}

/// The fallback dimensions layout code uses when a terminal size is
/// unavailable or reports a zero dimension.
pub const FALLBACK_COLUMNS: u16 = 80;
pub const FALLBACK_ROWS: u16 = 24;

/// Resolve a raw terminal-size query into dimensions layout code can
/// trust (niubash#852). A successful query can still report a real
/// `0` (a console mid-reconfigure, a pty with no winsize set): the
/// old `unwrap_or((80, 24))` at the TUI/menu call sites only replaced
/// a FAILED query, so a zero dimension flowed into layout math that
/// then painted past the edge. Like `refresh_winsize_variables`
/// (which refuses non-positive dimensions outright), a zero here is
/// not a size — each zero dimension falls back independently, and a
/// failed query falls back as a pair.
pub fn sane_terminal_size(size: Option<(u16, u16)>) -> (u16, u16) {
    match size {
        Some((columns, rows)) => (
            if columns > 0 {
                columns
            } else {
                FALLBACK_COLUMNS
            },
            if rows > 0 { rows } else { FALLBACK_ROWS },
        ),
        None => (FALLBACK_COLUMNS, FALLBACK_ROWS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// niubash#852: zero dimensions must not reach layout math, and
    /// a real dimension must survive untouched.
    #[test]
    fn issue852_sane_terminal_size_guards_zero_dimensions() {
        assert_eq!(sane_terminal_size(Some((120, 40))), (120, 40));
        assert_eq!(sane_terminal_size(Some((0, 40))), (80, 40));
        assert_eq!(sane_terminal_size(Some((120, 0))), (120, 24));
        assert_eq!(sane_terminal_size(Some((0, 0))), (80, 24));
        assert_eq!(sane_terminal_size(None), (80, 24));
    }
}
#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    pub fn stdin_is_terminal() -> bool {
        std_handle_is_console(STD_INPUT_HANDLE)
    }

    pub fn stdout_is_terminal() -> bool {
        std_handle_is_console(STD_OUTPUT_HANDLE)
    }

    pub fn stderr_is_terminal() -> bool {
        std_handle_is_console(STD_ERROR_HANDLE)
    }

    fn std_handle_is_console(handle_id: u32) -> bool {
        unsafe {
            let handle = GetStdHandle(handle_id);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return false;
            }

            let mut mode = 0;
            GetConsoleMode(handle, &mut mode) != 0
        }
    }
}
#[cfg(unix)]
mod platform {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }

    pub fn stdin_is_terminal() -> bool {
        fd_is_terminal(0)
    }

    pub fn stdout_is_terminal() -> bool {
        fd_is_terminal(1)
    }

    pub fn stderr_is_terminal() -> bool {
        fd_is_terminal(2)
    }

    fn fd_is_terminal(fd: i32) -> bool {
        unsafe { isatty(fd) == 1 }
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    pub fn stdin_is_terminal() -> bool {
        false
    }

    pub fn stdout_is_terminal() -> bool {
        false
    }

    pub fn stderr_is_terminal() -> bool {
        false
    }
}
