//! Minimal ANSI text styling for human-facing CLI output.
//!
//! Ground rules:
//! - Styling is **terminal-only**: when stdout is not a TTY (pipes, scripts,
//!   AI tools) every helper returns the input unchanged, so piped output
//!   never contains escape bytes.
//! - `NO_COLOR` disables styling unconditionally.
//! - On Windows the helper force-enables the VT bits on the stdout console
//!   handle (best effort) so legacy conhost hosts can still render colors.

use std::io::IsTerminal;
use std::sync::OnceLock;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return false;
        }
        if !std::io::stdout().is_terminal() {
            return false;
        }
        enable_windows_vt();
        true
    })
}

#[cfg(windows)]
fn enable_windows_vt() {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_PROCESSED_OUTPUT,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE,
    };
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() {
            return;
        }
        let mut mode = 0;
        if GetConsoleMode(handle, &mut mode) != 0 {
            let _ = SetConsoleMode(
                handle,
                mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            );
        }
    }
}

#[cfg(not(windows))]
fn enable_windows_vt() {}

fn wrap(code: &str, text: &str) -> String {
    if enabled() {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

pub fn bold(text: &str) -> String {
    wrap(BOLD, text)
}

pub fn dim(text: &str) -> String {
    wrap(DIM, text)
}

pub fn green(text: &str) -> String {
    wrap(GREEN, text)
}

pub fn yellow(text: &str) -> String {
    wrap(YELLOW, text)
}

pub fn red(text: &str) -> String {
    wrap(RED, text)
}

pub fn cyan(text: &str) -> String {
    wrap(CYAN, text)
}

/// Status symbol for an enabled pack: a green check on terminals, a plain
/// check mark otherwise.
pub fn on_symbol() -> String {
    green("✓")
}

/// Status symbol for an available-but-off pack: a dim dot.
pub fn off_symbol() -> String {
    dim("·")
}

/// Status symbol for a warning pack (missing binaries and friends).
pub fn warn_symbol() -> String {
    yellow("!")
}

/// Dim "needs: a, b" note for packs whose required programs are absent.
/// Empty when nothing is missing.
pub fn warn_missing_note(required: &[String]) -> String {
    let missing = crate::plugins::missing_required_binaries(required);
    if missing.is_empty() {
        String::new()
    } else {
        yellow(format!("(needs: {})", missing.join(", ")).as_str())
    }
}
