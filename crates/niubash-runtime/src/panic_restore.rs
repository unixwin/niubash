//! Best-effort console restoration on the panic path.
//!
//! Release binaries build with `panic = "abort"`, so no `Drop` guards run
//! when a panic unwinds — a panic while reedline holds the console in raw
//! mode would otherwise leave the terminal broken (no echo, hidden cursor).
//! The panic hook still executes before abort, so it is the last chance to
//! restore the console. This mirrors the restoration reedline performs in
//! `Drop for Reedline`.

use std::io::IsTerminal;

/// Restore the console state that reedline's `Drop` guards would normally
/// restore. Best-effort by design: every result is ignored because this runs
/// on the panic path right before `panic = "abort"` terminates the process.
pub fn restore_terminal_state() {
    // Raw mode lives on the console input handle; disabling it is safe even
    // when raw mode was never enabled.
    let _ = crossterm::terminal::disable_raw_mode();

    // Cursor commands are ANSI output; only emit them into a real terminal so
    // redirected stdout never receives escape bytes.
    if std::io::stdout().is_terminal() {
        let _ = crossterm::terminal::enable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::cursor::SetCursorStyle::DefaultUserShape,
            crossterm::cursor::Show
        );
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Install the process-wide panic hook. Chains the previous hook so panic
/// messaging stays identical; terminal restoration runs first because with
/// `panic = "abort"` this is the last code that executes.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal_state();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_is_safe_to_call_repeatedly() {
        restore_terminal_state();
        restore_terminal_state();
    }

    #[test]
    fn install_is_safe_to_call_repeatedly() {
        install_panic_hook();
        install_panic_hook();
    }
}
