//! Console baseline guard for the interactive REPL.
//!
//! Misbehaving child processes can leave the Windows console in a broken
//! state when they exit: Windows OpenSSH (`ssh.exe`) in particular saves the
//! console modes at startup and restores stale values on its failed-auth
//! path, which disables `ENABLE_VIRTUAL_TERMINAL_PROCESSING` /
//! `ENABLE_PROCESSED_OUTPUT` on the shared screen buffer handle. After that,
//! every escape sequence the REPL emits prints literally (`←[?25l`,
//! `♪◙`), the cursor stays hidden, and the prompt looks frozen.
//!
//! This mirrors what GNU bash does on Unix: snapshot the terminal discipline
//! at shell startup and restore it before every prompt. Here the discipline
//! is the console mode of the three standard handles:
//!
//! - stdin keeps whatever mode the shell started with (line input, echo,
//!   processed input); reedline re-enables raw mode itself on top of it for
//!   each `read_line`.
//! - stdout/stderr additionally get the VT-processing bits forced on, so a
//!   baseline captured *after* pollution is still repaired.
//!
//! Cursor visibility cannot be queried from the console API, so `restore`
//! always re-shows the cursor before the next prompt is drawn.

/// Snapshot of the console modes niubash owns between interactive commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConsoleBaseline {
    stdin_mode: Option<u32>,
    stdout_mode: Option<u32>,
    stderr_mode: Option<u32>,
}

/// Capture the console baseline for the current process. Captured once at
/// REPL startup; call [`restore`] before every prompt redraw.
pub fn capture() -> ConsoleBaseline {
    platform::capture()
}

/// Restore the baseline console modes and re-show the cursor. Safe to call
/// when handles are redirected (non-console handles are skipped).
pub fn restore(baseline: &ConsoleBaseline) {
    platform::restore(baseline);
}

#[cfg(windows)]
mod platform {
    use std::io::IsTerminal;

    use super::ConsoleBaseline;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_PROCESSED_OUTPUT,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WRAP_AT_EOL_OUTPUT, STD_ERROR_HANDLE,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    pub fn capture() -> ConsoleBaseline {
        ConsoleBaseline {
            stdin_mode: console_mode(STD_INPUT_HANDLE),
            stdout_mode: console_mode(STD_OUTPUT_HANDLE).map(ensure_vt_bits),
            stderr_mode: console_mode(STD_ERROR_HANDLE).map(ensure_vt_bits),
        }
    }

    pub fn restore(baseline: &ConsoleBaseline) {
        if let Some(mode) = baseline.stdin_mode {
            set_console_mode(STD_INPUT_HANDLE, mode);
        }
        if let Some(mode) = baseline.stdout_mode {
            set_console_mode(STD_OUTPUT_HANDLE, mode);
        }
        if let Some(mode) = baseline.stderr_mode {
            set_console_mode(STD_ERROR_HANDLE, mode);
        }

        // Cursor commands are ANSI output; only emit them into a real
        // terminal so redirected stdout never receives escape bytes.
        if std::io::stdout().is_terminal() {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
        }
    }

    /// Output modes niubash relies on. Forced in addition to whatever the
    /// snapshot saw so a baseline taken after pollution still heals.
    fn ensure_vt_bits(mode: u32) -> u32 {
        mode | ENABLE_PROCESSED_OUTPUT
            | ENABLE_WRAP_AT_EOL_OUTPUT
            | ENABLE_VIRTUAL_TERMINAL_PROCESSING
    }

    fn console_mode(handle_id: u32) -> Option<u32> {
        unsafe {
            let handle = GetStdHandle(handle_id);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) != 0 {
                Some(mode)
            } else {
                None
            }
        }
    }

    fn set_console_mode(handle_id: u32, mode: u32) {
        unsafe {
            let handle = GetStdHandle(handle_id);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return;
            }
            SetConsoleMode(handle, mode);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn ensure_vt_bits_forces_output_bits() {
            let restored = ensure_vt_bits(0);
            assert_ne!(restored & ENABLE_PROCESSED_OUTPUT, 0);
            assert_ne!(restored & ENABLE_WRAP_AT_EOL_OUTPUT, 0);
            assert_ne!(restored & ENABLE_VIRTUAL_TERMINAL_PROCESSING, 0);

            // Existing bits are preserved.
            let with_extra = ensure_vt_bits(0x8000_0000);
            assert_ne!(with_extra & 0x8000_0000, 0);
        }

        #[test]
        fn capture_and_restore_is_safe_and_idempotent() {
            let baseline = capture();
            restore(&baseline);
            restore(&baseline);
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::ConsoleBaseline;

    // On Unix the terminal discipline lives in termios; reedline/crossterm
    // already bracket each read with raw-mode save/restore, and child
    // processes are expected to leave the tty alone. Keep this a no-op until
    // there is evidence of the same pollution class on Unix.
    pub fn capture() -> ConsoleBaseline {
        ConsoleBaseline
    }

    pub fn restore(_baseline: &ConsoleBaseline) {}
}

#[cfg(not(windows))]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_and_restore_is_safe_and_idempotent() {
        let baseline = capture();
        restore(&baseline);
        restore(&baseline);
    }
}
