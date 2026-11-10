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
//! niubash#669: the handler above only ever fires for a CTRL_C_EVENT the
//! console server *generated*. A Ctrl+C that reaches the console as a raw
//! input record — harness-injected `WriteConsoleInputW` key records, or a
//! record queued while the input buffer sat in raw mode — is never
//! translated into an event, so during a foreground command nothing in
//! the process reacts: the virtual `sleep` and the sliced external wait
//! poll an empty mailbox until the command ends on its own (measured on
//! a real Windows PTY harness: `sleep 10` + ^C, no reaction at all, where
//! GNU in the same harness settles 130 at once — GNU/MSYS synthesizes the
//! signal inside its own runtime and never depends on the server event).
//! [`ForegroundWatch`] closes that leg on the host side: while the REPL
//! runs a foreground line it peeks the console input queue, and a queued
//! Ctrl+C key record gets the delivery the server skipped — the engine
//! mailbox note plus a `GenerateConsoleCtrlEvent` group broadcast for
//! real children — and is consumed so the next prompt does not read the
//! same ^C a second time.
//!
//! niubash#724: that synthesis is only owed while the console input
//! buffer has `ENABLE_PROCESSED_INPUT` set. The mode is a property of
//! the shared input buffer, so it is the foreground program's own
//! declaration: a program that disabled processed input (ssh, vim,
//! less — the raw-mode class) did so precisely to consume `0x03` as a
//! data byte itself, the way a POSIX program clearing `ISIG` owns the
//! byte. Its queued Ctrl+C records are its input, not a skipped
//! delivery, and synthesizing anyway double-delivers: the byte reaches
//! the program *and* a `CTRL_C_EVENT` reaches every process sharing
//! the console (a group-0 broadcast cannot be scoped to the child —
//! for `CTRL_C_EVENT` the system delivers console-wide regardless of
//! the group argument), driving raw-mode programs down their event
//! exit path: an interactive ssh session dies on a later Ctrl+C even
//! though the remote host already echoed `^C` from the forwarded byte.
//! The watch therefore samples the input mode at fire time and stays
//! silent under raw mode; see
//! [`queued_ctrl_c_earns_synthesized_delivery`].
//!
//! niubash#971: the same handler must also answer console close /
//! logoff / shutdown events (`CTRL_CLOSE_EVENT` and kin), which the
//! old code passed through (`return 0`) so the default handler called
//! `ExitProcess` and the session-end funnel — EXIT trap plus the
//! history flush — never ran: closing a Windows Terminal tab lost the
//! whole session's history. GNU saves history on SIGHUP for exactly
//! this shape (bash 5.3 sig.c `termsig_handler`: "the history will
//! not be saved when an interactive shell is running in a terminal
//! window that gets closed with the `close' button") and then runs
//! the EXIT trap before dying by the signal. The handler therefore
//! requests a main-thread session end ([`ConsoleCloseState`]) and
//! waits, bounded, for the flush to land; a prompt-blocked REPL is
//! woken by injecting one Ctrl+C key record into the console input
//! queue (reedline reads it as `Signal::CtrlC`, and the post-read
//! close check diverts to the funnel), a foreground execution is
//! aborted through the engine mailbox note instead, and a main thread
//! that never gets there is covered by a handler-thread fallback
//! flush of the registered [`crate::history::SessionEndFlushHandle`]
//! (the flush only touches `Arc<Mutex<...>>` state). The handler never
//! runs shell code itself, and the process still dies by the system
//! termination (`0xC000013A`), matching GNU's death-by-SIGHUP.
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[cfg(windows)]
static CTRL_C_RECEIVED: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
use windows_sys::Win32::System::Console::{
    SetConsoleCtrlHandler, WriteConsoleInputW, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
    CTRL_SHUTDOWN_EVENT, INPUT_RECORD, KEY_EVENT,
};

/// Win32 console control event codes, for the pure classifier below.
/// On Windows these resolve to the `windows_sys` names imported above.
/// Off Windows the shadows carry the same real Win32 values —
/// verified against the vendored `windows_sys` 0.59.0 and 0.61.2
/// sources (`Windows::Win32::System::Console`): CTRL_C_EVENT = 0,
/// CTRL_BREAK_EVENT = 1, CTRL_CLOSE_EVENT = 2, CTRL_LOGOFF_EVENT = 5,
/// CTRL_SHUTDOWN_EVENT = 6 — so the classification table the unit
/// tests pin off Windows is the Windows table itself.
#[cfg(not(windows))]
#[cfg_attr(not(windows), allow(dead_code))]
const CTRL_C_EVENT: u32 = 0;
#[cfg(not(windows))]
const CTRL_CLOSE_EVENT: u32 = 2;
#[cfg(not(windows))]
const CTRL_LOGOFF_EVENT: u32 = 5;
#[cfg(not(windows))]
const CTRL_SHUTDOWN_EVENT: u32 = 6;

#[cfg(windows)]
unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> i32 {
    if ctrl_type == CTRL_C_EVENT {
        // niubash#669: a CTRL_C_EVENT this process generated itself
        // (ForegroundWatch's group broadcast) reaches this handler too;
        // the watch already queued the engine note for that press, so
        // skip the flag and the note to avoid double dispatch.
        if take_self_forwarded_ctrl_c() {
            return 1; // handled — don't terminate the shell
        }
        CTRL_C_RECEIVED.store(true, Ordering::SeqCst);
        // Host swallows the event so the shell survives, but the engine's
        // in-process waits (sleep_interruptibly, external wait slices)
        // only poll the rubash signal mailbox — inject SIGINT there so
        // Ctrl+C aborts the foreground command (niubash D8) instead of
        // waiting for it to finish. At the reedline prompt the console is
        // in raw mode so this handler rarely fires; the `0x03` keystroke
        // path is Signal::CtrlC + consume_ctrl_c.
        rubash::script_driver::inject_console_ctrl_c();
        return 1; // handled — don't terminate the shell
    }
    if classify_console_control_event(ctrl_type) == ConsoleControlAction::RequestSessionEnd {
        // niubash#971: close/logoff/shutdown — request the main-thread
        // session end (history flush + EXIT trap) and wait, bounded,
        // for the flush to land; the system terminates the process
        // after this handler returns, so the exit code stays
        // 0xC000013A.
        handle_console_close();
        return 1; // handled — the funnel (or the fallback flush) ran
    }
    0 // pass through for other signals
}

/// niubash#971: what a console control event asks the host to do.
/// Pure over the event code so the table is testable off Windows; the
/// Windows handler passes the raw `ctrl_type` through, and the codes
/// are the Win32 values (windows_sys names them the same).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConsoleControlAction {
    /// Keep the current behavior: Ctrl+C keeps its handler path, and
    /// Ctrl+Break / unknown events pass through to the default handler.
    KeepGoing,
    /// A console close/logoff/shutdown event: end the session through
    /// the session-end funnel (GNU's SIGHUP shape).
    RequestSessionEnd,
}

// On non-Windows the only consumer is this module's unit tests (the
// handler itself is Windows-only); keep the cross-check
// `-D warnings` line green there.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn classify_console_control_event(ctrl_type: u32) -> ConsoleControlAction {
    match ctrl_type {
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            ConsoleControlAction::RequestSessionEnd
        }
        _ => ConsoleControlAction::KeepGoing,
    }
}

/// niubash#971: the console-close request + completion state, shared
/// between the console handler thread and the REPL main thread. The
/// handler only ever sets the request and waits for completion; the
/// main thread only ever reads the request and signals completion
/// after the session-end flush — no lock is taken on the hot path,
/// and the completion wait is bounded (see [`CLOSE_FLUSH_WAIT_MS`]).
pub(crate) struct ConsoleCloseState {
    requested: AtomicBool,
    done: Mutex<bool>,
    done_cv: Condvar,
}

impl ConsoleCloseState {
    pub(crate) const fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            done: Mutex::new(false),
            done_cv: Condvar::new(),
        }
    }

    /// Mark a close requested; returns true only on the first call,
    /// so a second event (close followed by logoff) never re-injects
    /// a wake record or a second mailbox note.
    #[cfg(any(windows, test))]
    pub(crate) fn request(&self) -> bool {
        !self.requested.swap(true, Ordering::SeqCst)
    }

    pub(crate) fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// Signal that the session-end flush landed.
    pub(crate) fn notify_done(&self) {
        let mut done = self.done.lock().unwrap_or_else(|e| e.into_inner());
        *done = true;
        self.done_cv.notify_all();
    }

    /// Wait up to `timeout` for the flush-done signal.
    #[cfg(any(windows, test))]
    pub(crate) fn wait_done(&self, timeout: std::time::Duration) -> bool {
        let done = self.done.lock().unwrap_or_else(|e| e.into_inner());
        if *done {
            return true;
        }
        let (done, _) = self
            .done_cv
            .wait_timeout(done, timeout)
            .unwrap_or_else(|e| e.into_inner());
        *done
    }
}

/// The process-wide close state (niubash#971).
static CONSOLE_CLOSE: ConsoleCloseState = ConsoleCloseState::new();

/// Whether the REPL main thread is blocked in `read_line` (niubash#971).
/// The handler injects its wake record only while this is set: during
/// foreground execution the console input queue belongs to the command,
/// and an injected record would either be consumed as the command's
/// input or mistaken by the ForegroundWatch for a skipped delivery
/// (#669/#724).
static AT_PROMPT: AtomicBool = AtomicBool::new(false);

/// niubash#971: has a console close/logoff/shutdown event requested the
/// session end? Read by the REPL loop (top + after every read) on all
/// platforms; only ever set on Windows.
pub(crate) fn console_close_requested() -> bool {
    CONSOLE_CLOSE.is_requested()
}

/// niubash#971: the session-end flush landed (called by the REPL close
/// path after the flush, before parking for the system termination).
pub(crate) fn notify_console_close_flush_done() {
    CONSOLE_CLOSE.notify_done();
}

/// niubash#971: record whether the REPL is blocked in `read_line`.
/// Set immediately before the read, cleared immediately after it
/// returns — the handler's wake-record gate.
pub(crate) fn set_prompt_read_active(active: bool) {
    AT_PROMPT.store(active, Ordering::SeqCst);
}

/// How long the close handler waits for the main thread's session-end
/// flush before falling back to a handler-thread flush (niubash#971).
/// The system gives a close handler on the order of 5s before
/// terminating the process, so the bound stays inside that window.
#[cfg(windows)]
const CLOSE_FLUSH_WAIT_MS: u64 = 4_000;

/// niubash#971: the session-end flush handle the console-close path
/// falls back to when the main thread never reaches the session-end
/// funnel. Registered once at interactive entry (run_repl), after the
/// history provider is installed.
#[cfg(windows)]
static CLOSE_FLUSH_HANDLE: Mutex<Option<Arc<crate::history::SessionEndFlushHandle>>> =
    Mutex::new(None);

/// niubash#971: register the session-end flush handle (see
/// [`CLOSE_FLUSH_HANDLE`]).
#[cfg(windows)]
pub(crate) fn register_console_close_flush(handle: Arc<crate::history::SessionEndFlushHandle>) {
    if let Ok(mut slot) = CLOSE_FLUSH_HANDLE.lock() {
        *slot = Some(handle);
    }
}

/// Unix: no console control handler exists, so there is nothing to
/// register; the call site stays uniform.
#[cfg(not(windows))]
pub(crate) fn register_console_close_flush(_handle: Arc<crate::history::SessionEndFlushHandle>) {}

/// niubash#971: a console close/logoff/shutdown event arrived. Wake or
/// abort the main thread, then wait (bounded) for its session-end
/// flush; if it never lands, flush the history from this thread.
///
/// Runs on the console handler thread: it never touches the `Shell`
/// (an `Rc<RefCell<...>>` the main thread owns), never runs shell code,
/// and never takes a lock the main thread could be holding across a
/// wait — the only locks are the close state's own completion cell
/// and the flush handle slot.
#[cfg(windows)]
fn handle_console_close() {
    // First arrival only: wake the prompt or abort the foreground wait.
    if CONSOLE_CLOSE.request() {
        if AT_PROMPT.load(Ordering::SeqCst) {
            inject_prompt_wake_record();
        } else {
            // The main thread is running a foreground command: abort
            // its engine waits through the mailbox note (the same note
            // the Ctrl+C handler injects — the wait slices consume it,
            // so nothing stays queued for the EXIT trap's first
            // boundary). The foreground child itself receives the
            // close event directly and dies by default.
            rubash::script_driver::inject_console_ctrl_c();
        }
    }
    if CONSOLE_CLOSE.wait_done(std::time::Duration::from_millis(CLOSE_FLUSH_WAIT_MS)) {
        return;
    }
    // The main thread never got there (stuck foreground command, slow
    // EXIT trap): land the history from this thread, then return and
    // let the system terminate the process.
    flush_session_history_from_handler_thread();
}

/// niubash#971: wake a prompt-blocked `read_line` by injecting one
/// Ctrl+C key record (ETX unit, LEFT_CTRL_PRESSED) into the console
/// input queue. Reedline reads it as `Signal::CtrlC`, and the REPL's
/// post-read close check diverts to the session-end funnel instead of
/// the interrupt arm. Best-effort: without a console stdin there is
/// nothing to wake, and the fallback flush covers the history.
#[cfg(windows)]
fn inject_prompt_wake_record() {
    let Some(handle) = crate::typeahead_guard::stdin_console_handle() else {
        return;
    };
    let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
    record.EventType = KEY_EVENT as u16;
    unsafe {
        let key = &mut record.Event.KeyEvent;
        key.bKeyDown = 1;
        key.wRepeatCount = 1;
        key.wVirtualKeyCode = 0x43; // 'C'
        key.wVirtualScanCode = 0x2E;
        key.uChar.UnicodeChar = 0x03;
        key.dwControlKeyState = 0x0008; // LEFT_CTRL_PRESSED
    }
    unsafe {
        let mut written = 0u32;
        WriteConsoleInputW(handle.0, &record, 1, &mut written);
    }
}

/// niubash#971: land the session history from the handler thread
/// (fallback when the main thread's session-end flush never lands
/// inside the bounded wait). The handle's flush only touches
/// `Arc<Mutex<...>>` state: it serializes against a main-thread flush
/// on the state lock, and a second flush after the first is a no-op,
/// so racing the funnel is harmless. Best-effort: no handle (or a
/// poisoned slot) simply means nothing to flush.
#[cfg(windows)]
fn flush_session_history_from_handler_thread() {
    let handle = CLOSE_FLUSH_HANDLE.lock().ok().and_then(|slot| slot.clone());
    if let Some(handle) = handle {
        handle.flush();
    }
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
/// queued the delivery via the engine note (script_driver::inject_console_ctrl_c), and a second one would
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
        rubash::script_driver::inject_console_ctrl_c();
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

/// niubash#669: does a console input record describe a Ctrl+C key press?
/// Field-level predicate over the record's primitives so it is testable
/// off Windows. `unicode_char` is the record's UTF-16 code unit,
/// `virtual_key` its virtual-key code, `control_state` the
/// `dwControlKeyState` bits (LEFT_CTRL_PRESSED 0x0008, RIGHT_CTRL_PRESSED
/// 0x0004, SHIFT_PRESSED 0x0010).
///
/// Two shapes count, matching what the line editor itself treats as
/// Ctrl+C when it reads the queue:
/// - the ETX unit (0x03) — the byte-oriented form a terminal delivers;
/// - the 'C' key with a Ctrl modifier and no Shift — the form
///   `WriteConsoleInputW` injectors write when they synthesize the chord
///   as a key record (char unit NUL or 'C'/'c' depending on the writer).
// On non-Windows the only consumers are this module's unit tests
// (the watch itself is Windows-only); keep the cross-check
// `-D warnings` line green there.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn is_ctrl_c_key_record(
    is_key_event: bool,
    key_down: bool,
    unicode_char: u16,
    virtual_key: u16,
    control_state: u32,
) -> bool {
    if !is_key_event || !key_down {
        return false;
    }
    if unicode_char == 0x03 {
        return true;
    }
    const VK_C: u16 = 0x43;
    const CTRL_PRESSED: u32 = 0x0008 | 0x0004;
    const SHIFT_PRESSED: u32 = 0x0010;
    virtual_key == VK_C
        && control_state & CTRL_PRESSED != 0
        && control_state & SHIFT_PRESSED == 0
        && matches!(unicode_char, 0 | 0x43 | 0x63)
}

/// niubash#724: the foreground watch's fire decision, as a pure
/// function of the two facts sampled per wake: whether the peeked
/// queue holds a Ctrl+C record, and whether the console input buffer
/// currently has `ENABLE_PROCESSED_INPUT` set.
///
/// - Processed input on + a record queued: in this mode the console
///   server turns a real Ctrl+C press into a `CTRL_C_EVENT` and never
///   queues it, so a queued record means the synthesis was bypassed
///   (records injected straight into the input buffer — the #669
///   shape) and the delivery is owed.
/// - Processed input off: the foreground program disabled it to
///   consume `0x03` as data (ssh, vim, less — the POSIX `ISIG`-off
///   class), so the record is that program's input. Synthesizing
///   would double-deliver — the byte, plus a console-wide event the
///   program never asked for — and force it down its event exit
///   path (#724). The watch stays silent.
///
/// Field-level like [`is_ctrl_c_key_record`] so the decision table is
/// testable off Windows; the Windows watch reads the live mode with
/// `GetConsoleMode` at fire time.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn queued_ctrl_c_earns_synthesized_delivery(
    record_is_ctrl_c: bool,
    processed_input_enabled: bool,
) -> bool {
    record_is_ctrl_c && processed_input_enabled
}

/// niubash#669: number of `GenerateConsoleCtrlEvent` broadcasts this
/// process generated itself (see `ForegroundWatch`). The console handler
/// fires for those too — the broadcast reaches every process in the
/// group, including this one — and must skip them: the watch already
/// queued the engine note for the delivery it synthesized, so a second
/// note (and a prompt-side flag) would double-dispatch one Ctrl+C.
#[cfg(windows)]
static SELF_FORWARDED_CTRL_C: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(windows)]
fn mark_self_forwarded_ctrl_c() {
    SELF_FORWARDED_CTRL_C.fetch_add(1, Ordering::SeqCst);
}

/// Consume one self-forwarded mark, returning whether one was pending.
/// A real Ctrl+C racing a synthesized one can consume the mark instead;
/// the outcome is still exactly one queued note for the press the watch
/// saw, so the race costs nothing observable.
#[cfg(windows)]
fn take_self_forwarded_ctrl_c() -> bool {
    SELF_FORWARDED_CTRL_C
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1))
        .is_ok()
}

/// niubash#669: host-side Ctrl+C watch for the foreground-execution
/// window. Armed by the REPL around the execution of one submitted line
/// (RAII: dropping ends the watch); a no-op off Windows and whenever
/// stdin is not an interactive console.
///
/// Why this exists: the console handler in this module only fires for a
/// server-generated CTRL_C_EVENT. A Ctrl+C that sits in the console
/// input queue as a key record — injected by PTY harnesses through
/// `WriteConsoleInputW`, or queued during a raw-mode stretch — never
/// becomes an event, and while a foreground command runs nobody reads
/// the queue, so the press was simply invisible until the command ended
/// on its own (the #669 measurement: `sleep 10` + ^C, no reaction; GNU
/// in the same harness synthesizes the signal in-runtime and settles
/// 130 immediately). The watch peeks the queue without consuming it; a
/// queued Ctrl+C record gets the delivery the server skipped:
///
/// 1. the engine mailbox note (the same `inject_console_ctrl_c` the
///    console handler performs), so the virtual `sleep` and the sliced
///    external wait abort with 130;
/// 2. a `GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)` group broadcast, so
///    a real child sharing the console process group receives the
///    standard event;
/// 3. consumption of exactly that record, so the next prompt does not
///    read the same press again as a prompt interrupt.
///
/// One gate on all three legs (niubash#724): the delivery is owed
/// only while the console input buffer has `ENABLE_PROCESSED_INPUT`
/// set, sampled at fire time. A foreground program that cleared it
/// (ssh, vim, less) owns the queued record as data; for its presses
/// the watch neither synthesizes nor consumes — it keeps watching,
/// because the same line may later run a command in the #669 shape.
///
/// Peeking never takes input away from the foreground command (the
/// command owns the terminal in this window — `cat`/`read`/`ssh` must
/// see console stdin); the only consumption is the single Ctrl+C record
/// at the moment the command is being aborted anyway. One-shot per arm:
/// after a delivery the watch thread exits.
pub struct ForegroundWatch {
    // Held only for its Drop (disarming stops the watcher thread);
    // the value itself is never read.
    #[cfg(windows)]
    #[allow(dead_code)]
    inner: Option<watch_imp::Watch>,
}

impl ForegroundWatch {
    /// Arm the watch for one foreground execution. Cheap when it cannot
    /// apply (non-console stdin, non-Windows): returns a disarmed guard.
    pub fn arm() -> Self {
        #[cfg(windows)]
        {
            Self {
                inner: watch_imp::Watch::arm(),
            }
        }
        #[cfg(not(windows))]
        {
            Self {}
        }
    }
}

#[cfg(windows)]
mod watch_imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use windows_sys::Win32::Foundation::{HANDLE, WAIT_FAILED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Console::{
        GenerateConsoleCtrlEvent, GetConsoleMode, GetNumberOfConsoleInputEvents, PeekConsoleInputW,
        ReadConsoleInputW, WriteConsoleInputW, CTRL_C_EVENT, ENABLE_PROCESSED_INPUT, INPUT_RECORD,
        KEY_EVENT,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    use super::{is_ctrl_c_key_record, mark_self_forwarded_ctrl_c};
    use crate::typeahead_guard::stdin_console_handle;

    /// Wake latency when the queue is empty. The wait below returns the
    /// instant a record lands, so this only bounds the stop check.
    const POLL_MS: u32 = 20;

    /// Records inspected per peek. A Ctrl+C pressed during a foreground
    /// command sits within a handful of records of the head; a deeper
    /// queue means the press is stale typeahead for the next prompt,
    /// which the editor will read as its own Ctrl+C — not this watch's
    /// delivery to make.
    const PEEK_DEPTH: usize = 128;

    pub(super) struct Watch {
        stop: Arc<AtomicBool>,
    }

    impl Watch {
        pub(super) fn arm() -> Option<Watch> {
            let handle = stdin_console_handle()?;
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let spawned = std::thread::Builder::new()
                .name("niu-ctrlc-watch".to_string())
                .spawn(move || {
                    // Move the OwnedHandle itself across: it carries
                    // the unsafe Send impl (the typeahead sweeper moves
                    // its handle the same way); the raw HANDLE field is
                    // not Send, so destructure a pair to force
                    // whole-variable capture instead of disjoint field
                    // capture of `handle.0` alone. The handle drops
                    // (closes) when the thread ends.
                    let (handle, stop) = (handle, thread_stop);
                    run(handle.0, stop);
                })
                .is_ok();
            if !spawned {
                return None;
            }
            Some(Watch { stop })
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            // Detached by design (the typeahead sweeper parks the same
            // way): the thread polls on a POLL_MS cadence and exits on
            // its own; joining here would tax every REPL line by up to
            // one cadence.
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    fn run(conin: HANDLE, stop: Arc<AtomicBool>) {
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let wait = unsafe { WaitForSingleObject(conin, POLL_MS) };
            if wait == WAIT_FAILED {
                return;
            }
            if wait != WAIT_OBJECT_0 {
                continue; // timeout: queue still empty
            }
            if stop.load(Ordering::SeqCst) {
                return;
            }
            if super::queued_ctrl_c_earns_synthesized_delivery(
                peek_finds_ctrl_c(conin),
                processed_input_enabled(conin),
            ) {
                deliver(conin);
                return; // one-shot
            }
        }
    }

    fn record_is_ctrl_c(record: &INPUT_RECORD) -> bool {
        if record.EventType != KEY_EVENT as u16 {
            return false;
        }
        // SAFETY: EventType is KEY_EVENT, so the KeyEvent arm is active.
        let key = unsafe { &record.Event.KeyEvent };
        is_ctrl_c_key_record(
            true,
            key.bKeyDown != 0,
            // SAFETY: union read of the active KeyEvent arm.
            unsafe { key.uChar.UnicodeChar },
            key.wVirtualKeyCode,
            key.dwControlKeyState,
        )
    }

    fn peek_finds_ctrl_c(conin: HANDLE) -> bool {
        let mut buffer: Vec<INPUT_RECORD> = vec![unsafe { std::mem::zeroed() }; PEEK_DEPTH];
        let mut read = 0u32;
        let ok =
            unsafe { PeekConsoleInputW(conin, buffer.as_mut_ptr(), PEEK_DEPTH as u32, &mut read) };
        if ok == 0 || read == 0 {
            return false;
        }
        buffer.truncate(read as usize);
        buffer.iter().any(record_is_ctrl_c)
    }

    /// niubash#724: the console input buffer's current
    /// `ENABLE_PROCESSED_INPUT` state — the term that decides whether a
    /// queued Ctrl+C record is a delivery the server skipped (#669) or
    /// the foreground program's own data (#724). The mode belongs to
    /// the shared input buffer, so this reads the mode the foreground
    /// program itself established for its run. An unreadable mode is
    /// treated as processed: synthesis is suppressed only on positively
    /// observed raw mode, never on a failed probe.
    fn processed_input_enabled(conin: HANDLE) -> bool {
        let mut mode = 0u32;
        if unsafe { GetConsoleMode(conin, &mut mode) } == 0 {
            return true;
        }
        mode & ENABLE_PROCESSED_INPUT != 0
    }

    /// Perform the delivery the console server skipped for a queued
    /// Ctrl+C record. See the `ForegroundWatch` doc for the three legs.
    fn deliver(conin: HANDLE) {
        // Mark BEFORE generating: the broadcast also reaches this
        // process, and its handler must not queue a second note or set
        // the prompt flag for an event the watch already accounted.
        mark_self_forwarded_ctrl_c();
        rubash::script_driver::inject_console_ctrl_c();
        unsafe {
            GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0);
        }
        consume_one_ctrl_c_record(conin);
    }

    /// Remove the first queued Ctrl+C record, requeueing everything
    /// else in order (the drain-and-rewrite the typeahead writeback
    /// uses: `WriteConsoleInputW` only appends, so order is preserved
    /// by rewriting the survivors behind nothing else pending). If the
    /// foreground command already consumed the record itself, nothing
    /// matches and the queue is rewritten unchanged.
    fn consume_one_ctrl_c_record(conin: HANDLE) {
        let mut pending: Vec<INPUT_RECORD> = Vec::new();
        loop {
            let mut count = 0u32;
            if unsafe { GetNumberOfConsoleInputEvents(conin, &mut count) } == 0 || count == 0 {
                break;
            }
            let mut batch: Vec<INPUT_RECORD> = vec![unsafe { std::mem::zeroed() }; count as usize];
            let mut read = 0u32;
            let ok = unsafe { ReadConsoleInputW(conin, batch.as_mut_ptr(), count, &mut read) };
            if ok == 0 || read == 0 {
                break;
            }
            batch.truncate(read as usize);
            pending.extend(batch);
        }
        if let Some(pos) = pending.iter().position(record_is_ctrl_c) {
            pending.remove(pos);
        }
        if !pending.is_empty() {
            unsafe {
                let mut written = 0u32;
                WriteConsoleInputW(conin, pending.as_ptr(), pending.len() as u32, &mut written);
            }
        }
    }
}

#[cfg(test)]
mod ctrl_c_record_tests {
    use super::is_ctrl_c_key_record;

    const LEFT_CTRL: u32 = 0x0008;
    const RIGHT_CTRL: u32 = 0x0004;
    const SHIFT: u32 = 0x0010;

    #[test]
    fn etx_unit_is_ctrl_c() {
        assert!(is_ctrl_c_key_record(true, true, 0x03, 0x43, LEFT_CTRL));
        // Even with no modifier bits recorded, the ETX unit alone is
        // the byte a terminal sends for the chord.
        assert!(is_ctrl_c_key_record(true, true, 0x03, 0, 0));
    }

    #[test]
    fn injected_c_key_with_ctrl_is_ctrl_c() {
        // WriteConsoleInputW harness shape: 'C' VK + Ctrl, NUL char.
        assert!(is_ctrl_c_key_record(true, true, 0, 0x43, LEFT_CTRL));
        assert!(is_ctrl_c_key_record(true, true, 0, 0x43, RIGHT_CTRL));
        // Writers that fill the char unit: 'C' (shiftless press still
        // reports the uppercase unit when CapsLock/Shift is involved in
        // the writer's table) and 'c'.
        assert!(is_ctrl_c_key_record(true, true, 0x43, 0x43, LEFT_CTRL));
        assert!(is_ctrl_c_key_record(true, true, 0x63, 0x43, LEFT_CTRL));
    }

    #[test]
    fn plain_and_shifted_c_are_not_ctrl_c() {
        assert!(!is_ctrl_c_key_record(true, true, 0x63, 0x43, 0));
        assert!(!is_ctrl_c_key_record(true, true, 0x43, 0x43, SHIFT));
        // Ctrl+Shift+C is the terminal's copy chord, not an interrupt.
        assert!(!is_ctrl_c_key_record(
            true,
            true,
            0x43,
            0x43,
            LEFT_CTRL | SHIFT
        ));
    }

    #[test]
    fn release_and_non_key_records_are_not_ctrl_c() {
        assert!(!is_ctrl_c_key_record(true, false, 0x03, 0x43, LEFT_CTRL));
        assert!(!is_ctrl_c_key_record(false, true, 0x03, 0x43, LEFT_CTRL));
    }

    #[test]
    fn foreground_watch_arm_is_inert_without_a_console() {
        // Test processes run with redirected stdin: arming must be a
        // silent no-op and dropping must not hang.
        let watch = super::ForegroundWatch::arm();
        drop(watch);
        let watch = super::ForegroundWatch::arm();
        drop(watch);
    }

    #[test]
    fn issue724_raw_mode_record_is_the_foreground_programs_data() {
        // A raw-mode foreground program (ssh/vim/less class) disabled
        // processed input precisely to consume 0x03 itself; a queued
        // Ctrl+C record is its input and must not earn a synthesized
        // delivery.
        assert!(!super::queued_ctrl_c_earns_synthesized_delivery(
            true, false
        ));
    }

    #[test]
    fn issue669_processed_mode_record_still_earns_delivery() {
        // The #669 shape is unchanged: processed input on, yet a
        // record sits queued — the server synthesis was bypassed and
        // the delivery is owed. A non-Ctrl+C record never earns one.
        assert!(super::queued_ctrl_c_earns_synthesized_delivery(true, true));
        assert!(!super::queued_ctrl_c_earns_synthesized_delivery(
            false, true
        ));
        assert!(!super::queued_ctrl_c_earns_synthesized_delivery(
            false, false
        ));
    }

    #[test]
    fn issue971_close_logoff_shutdown_request_session_end() {
        use super::{
            classify_console_control_event, ConsoleControlAction::*, CTRL_CLOSE_EVENT,
            CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
        };
        // The named codes carry the real Win32 values on both
        // platforms (the windows_sys names on Windows, the shadow
        // constants above off it): CLOSE = 2, LOGOFF = 5,
        // SHUTDOWN = 6.
        assert_eq!(CTRL_CLOSE_EVENT, 2);
        assert_eq!(CTRL_LOGOFF_EVENT, 5);
        assert_eq!(CTRL_SHUTDOWN_EVENT, 6);
        // Literal event codes as well, so the classification table
        // is pinned even if a name could drift: 2/5/6 request the
        // session end. (2 is CTRL_CLOSE_EVENT — an earlier draft
        // of this test asserted it kept going under a wrong
        // BREAK = 2 label; on Windows that assertion is false.)
        assert_eq!(classify_console_control_event(2), RequestSessionEnd);
        assert_eq!(classify_console_control_event(5), RequestSessionEnd);
        assert_eq!(classify_console_control_event(6), RequestSessionEnd);
        assert_eq!(
            classify_console_control_event(CTRL_CLOSE_EVENT),
            RequestSessionEnd
        );
        assert_eq!(
            classify_console_control_event(CTRL_LOGOFF_EVENT),
            RequestSessionEnd
        );
        assert_eq!(
            classify_console_control_event(CTRL_SHUTDOWN_EVENT),
            RequestSessionEnd
        );
    }

    #[test]
    fn issue971_ctrl_c_break_and_unknown_keep_going() {
        use super::{classify_console_control_event, ConsoleControlAction::*, CTRL_C_EVENT};
        // CTRL_C_EVENT is really 0, on both platforms (shadow
        // constants above; windows_sys names on Windows). The
        // handler intercepts C before the classifier runs, and the
        // classifier itself never requests a session end for it.
        assert_eq!(CTRL_C_EVENT, 0);
        assert_eq!(classify_console_control_event(CTRL_C_EVENT), KeepGoing);
        assert_eq!(classify_console_control_event(0), KeepGoing);
        // CTRL_BREAK_EVENT = 1: out of scope for #971, unchanged.
        assert_eq!(classify_console_control_event(1), KeepGoing);
        // 3 names no Win32 console event; unknown codes pass
        // through to the default handler, unchanged.
        assert_eq!(classify_console_control_event(3), KeepGoing);
        assert_eq!(classify_console_control_event(7), KeepGoing);
        assert_eq!(classify_console_control_event(u32::MAX), KeepGoing);
    }

    #[test]
    fn issue971_close_state_request_is_one_shot_and_waitable() {
        use super::ConsoleCloseState;
        use std::time::Duration;
        let state = ConsoleCloseState::new();
        assert!(!state.is_requested());
        // A wait with nothing requested times out instead of hanging.
        assert!(!state.wait_done(Duration::from_millis(1)));
        // The first request wins; a second event does not re-arm.
        assert!(state.request());
        assert!(!state.request());
        assert!(state.is_requested());
        // Completion unblocks a waiter immediately.
        let state = std::sync::Arc::new(state);
        let waiter = std::sync::Arc::clone(&state);
        let done = std::thread::spawn(move || waiter.wait_done(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(10));
        state.notify_done();
        assert!(done.join().unwrap());
        assert!(state.wait_done(Duration::from_millis(1)));
    }

    #[test]
    fn issue971_session_end_flush_handle_is_send_and_sync() {
        // The handler thread runs the fallback flush off the REPL
        // thread, so the handle must cross threads.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<crate::history::SessionEndFlushHandle>();
    }
}
