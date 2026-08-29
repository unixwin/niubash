//! Win32 Ctrl+C handler
//!
//! Installs a console control handler that intercepts Ctrl+C so it doesn't
//! terminate the shell. On Ctrl+C we simply return TRUE (signal handled),
//! allowing the REPL loop to react via reedline's CtrlC signal.

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
static CTRL_C_RECEIVED: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
use windows_sys::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_C_EVENT};

#[cfg(windows)]
unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> i32 {
    if ctrl_type == CTRL_C_EVENT {
        CTRL_C_RECEIVED.store(true, Ordering::SeqCst);
        return 1; // handled — don't terminate the shell
    }
    0 // pass through for other signals
}

/// Install the Ctrl+C handler. Call once at startup.
#[cfg(windows)]
pub fn install() {
    unsafe {
        if SetConsoleCtrlHandler(Some(ctrl_handler), 1) == 0 {
            eprintln!("Warning: failed to set Ctrl+C handler");
        } else {
            log::debug!("Ctrl+C handler installed");
        }
    }
}

#[cfg(windows)]
pub fn consume_ctrl_c() -> bool {
    CTRL_C_RECEIVED.swap(false, Ordering::SeqCst)
}

#[cfg(not(windows))]
pub fn install() {}
