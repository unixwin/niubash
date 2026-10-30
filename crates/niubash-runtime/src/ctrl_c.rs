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
/// queued the delivery via the engine note (script_driver::inject_console_ctrl_c), and a second one would
/// dispatch twice. On unix there is no host handler, so nothing is ever
/// pre-queued and the delivery is a real self-raise — it travels through
/// the engine's own kernel disposition (signal-hook), which is a
/// delivery, not a second registration (see the module header). When the
/// user ignores SIGINT (`trap '' INT`), the kernel discards the raise
/// before the engine ever sees it, matching GNU.
pub fn queue_prompt_sigint(handler_already_queued: bool) {
    // niubash#567 (FFI P4): even when the console handler already
    // queued the engine delivery, the interrupt happened - an FFI
    // call in flight must observe it through its cancel_check
    // callback, and the next FFI call must decline as cancelled
    // rather than run. This funnel is the one place every prompt
    // Ctrl+C passes through, so the note goes before the
    // already-queued early return.
    crate::ffi::note_prompt_interrupt();
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
}
