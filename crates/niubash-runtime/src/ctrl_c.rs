//! Ctrl+C handling, split by who owns the OS-level signal surface.
//!
//! Windows: the HOST owns the console control surface. The engine's signal
//! backend there is a cross-process file mailbox (`register_signal_mailbox`,
//! rubash `src/builtins/kill.rs`), not kernel signals, so this module's
//! `SetConsoleCtrlHandler` intercept is the sole in-process receiver. The
//! handler only stores a flag and returns TRUE (don't terminate); the REPL
//! reacts at prompt-redraw time via reedline's `Signal::CtrlC` ->
//! `consume_ctrl_c()` -> the host-side trapint framework hooks.
//!
//! Unix: the ENGINE owns the kernel signal surface. `Executor` init calls
//! `register_signal_mailbox` (rubash `src/executor/init.rs`) ->
//! `kernel_signals::install()` (rubash `src/builtins/kill.rs`), which
//! registers signal_hook handlers for INT/TERM/HUP/QUIT/USR1/USR2 for the
//! whole process lifetime; deliveries are drained at command boundaries by
//! `run_pending_signal_traps`. The host must NOT install a second OS
//! handler for that set:
//! - a replace-style registration (`libc::signal`/`sigaction`) would
//!   overwrite signal-hook's action and swallow deliveries out of the
//!   engine's queue (traps would never fire, untrapped-INT exit lost);
//! - a signal-hook-style registration composes (signal-hook-registry runs
//!   every chained action), so each kernel SIGINT would be
//!   double-dispatched: once through the host flag here (`run_trap_hooks`
//!   from the REPL) and once through the engine's own command-boundary
//!   trap dispatch.
//!
//! The engine queue also has no non-destructive peek (`take_pending_signals`
//! drains it), so the host cannot poll it from the prompt loop either. The
//! host's only unix reaction point is reedline's `Signal::CtrlC` — while
//! the prompt owns the tty in raw mode no kernel SIGINT is generated at
//! all (the `0x03` byte is read as a keystroke).

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
        // Host swallows the event so the shell survives, but the engine's
        // in-process waits (sleep_interruptibly, external wait slices)
        // only poll the rubash signal mailbox — inject SIGINT there so
        // Ctrl+C aborts the foreground command (niubash D8) instead of
        // waiting for it to finish. At the reedline prompt the console is
        // in raw mode so this handler rarely fires; the `0x03` keystroke
        // path is Signal::CtrlC + consume_ctrl_c.
        rubash::builtins::kill::note_console_ctrl_c();
        return 1; // handled — don't terminate the shell
    }
    0 // pass through for other signals
}

/// Install the Ctrl+C handler. Call once at startup.
///
/// Windows-only effect; the unix body deliberately installs nothing (see
/// the module header for the engine-owns-signals contract).
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

/// Unix: always `false`. A `true` here would run the host-side trapint
/// framework hooks (see the REPL's `Signal::CtrlC` branch) *in addition*
/// to the engine's own dispatch of the same kernel SIGINT at the next
/// command boundary — and the engine's queue cannot be peeked without
/// draining it, so the host stays out of kernel-signal bookkeeping
/// entirely.
#[cfg(not(windows))]
pub fn consume_ctrl_c() -> bool {
    false
}

/// Unix: installs nothing on purpose — the rubash engine's
/// `kernel_signals` backend owns INT/TERM/HUP/QUIT/USR1/USR2 for the
/// process lifetime, and any second host registration would either
/// swallow engine deliveries (replace-style) or double-dispatch them
/// (chain-style). See the module header.
#[cfg(not(windows))]
pub fn install() {
    log::debug!(
        "ctrl_c: no host signal handler on unix; \
         rubash kernel_signals owns INT/TERM/HUP/QUIT/USR1/USR2"
    );
}

/// niubash#616: deliver exactly one SIGINT to the engine's pending-signal
/// queue for a prompt Ctrl+C, so the REPL can run the engine's boundary
/// dispatch (`Shell::dispatch_prompt_interrupt`) and a trapped INT fires
/// at the prompt like GNU. `handler_already_queued` is the Windows
/// `consume_ctrl_c()` result: when the console handler fired, it already
/// queued the delivery via `note_console_ctrl_c`, and a second one would
/// dispatch twice. On unix there is no host handler, so nothing is ever
/// pre-queued and the delivery is a real self-raise — it travels through
/// the engine's own kernel disposition (signal-hook), which is a
/// delivery, not a second registration (see the module header). When the
/// user ignores SIGINT (`trap '' INT`), the kernel discards the raise
/// before the engine ever sees it, matching GNU.
pub fn queue_prompt_sigint(handler_already_queued: bool) {
    if handler_already_queued {
        return;
    }
    #[cfg(windows)]
    {
        rubash::builtins::kill::note_console_ctrl_c();
    }
    #[cfg(not(windows))]
    {
        // SAFETY: `raise` is async-signal-safe and merely delivers the
        // signal to this process; the engine's installed handler records
        // it in its queue for the boundary poll the caller runs next.
        unsafe {
            libc::raise(libc::SIGINT);
        }
    }
}
