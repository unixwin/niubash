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
