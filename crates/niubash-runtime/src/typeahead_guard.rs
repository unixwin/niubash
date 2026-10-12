//! Typeahead guard for the prompt-rebuild window (niubash#167).
//!
//! Between two `read_line` calls the console sits in the cooked baseline
//! mode while the shell runs the prompt machinery: startup rc files
//! (plugin bootstrap children) and `PROMPT_COMMAND` (theme helpers —
//! `date`, `git`, `awk`, `wmic`, ...). Those children inherit the
//! interactive console, and some runtimes probe the shared console input
//! buffer at startup (observed with Git-for-Windows MSYS2 binaries —
//! `git`/`awk`/`grep`/`date` under a themed prompt): the probe consumes
//! one pending `KEY_EVENT` record, the PRESS of the first key typed
//! while the machinery runs. Its RELEASE survives, so the edit line ends
//! up missing exactly its first byte: `echo` arrives as `cho`, `niu` as
//! `iu`, `source` as `ource` (wt67 drv-run1 evidence, 11/11 resends).
//!
//! GNU readline never reads from the input stream during redisplay
//! (lib/readline display.c — signal-safe redisplay), and on GNU/Linux
//! children cannot reach another process's tty queue, so bash has no
//! equivalent hazard. On Windows the console input buffer is readable by
//! every attached process, so the shell itself must keep the user's
//! typeahead out of reach while machinery with inherited console handles
//! runs.
//!
//! [`TypeaheadGuard`] does exactly that, without changing what the
//! editor sees once it comes back:
//!
//! - [`TypeaheadGuard::arm`] starts a sweeper thread that moves *all*
//!   pending console input records into an in-process buffer every few
//!   milliseconds. The typeahead spends the risky window inside the
//!   shell, where probing children cannot reach it.
//! - [`TypeaheadGuard::disarm_and_reinject`] stops the sweeper and
//!   writes the buffered records back into the console input queue with
//!   `WriteConsoleInputW`, so the line editor reads them exactly as if
//!   they had waited in the queue the whole time. Records are copied
//!   verbatim (press/release pairs, mouse and focus events, UTF-16
//!   surrogate halves), so nothing is transformed or lost.
//!
//! Scope, deliberately narrow: the guard is armed only over the
//! prompt-rebuild window (rc bootstrap at startup plus the pre-prompt
//! hook stretch). It is NOT armed while a foreground command executes —
//! the command owns the terminal then (GNU semantics: `cat`, `read`,
//! `ssh` must see console stdin) — nor while `read_line` owns the
//! console, where the line editor is the only reader. A sweeper that
//! wakes from a read after disarm reinjects whatever record it holds
//! before exiting (generation check), so a disarmed guard can never keep
//! user input.
//!
//! Off Windows the hazard does not exist (a child cannot read another
//! process's tty input queue), so the guard arms nothing.

/// Guard protecting console typeahead while prompt machinery runs.
///
/// Cloning/disarming is intentionally explicit: exactly one guard lives
/// in the REPL loop, armed between `read_line` calls and disarmed with
/// reinjection right before the next `read_line`.
pub struct TypeaheadGuard {
    inner: Option<imp::Guard>,
}

impl TypeaheadGuard {
    /// A disarmed guard.
    pub fn disarmed() -> Self {
        Self { inner: None }
    }

    /// Begin sweeping the console input queue into the guard's buffer.
    /// No-op when the shell's stdin is not an interactive console (no
    /// shared input buffer to protect) or when already armed.
    pub fn arm(&mut self) {
        if self.inner.is_none() {
            self.inner = imp::Guard::arm();
        }
    }

    /// Stop sweeping and write every saved record back into the console
    /// input queue, in order, for the line editor to read next. Safe to
    /// call repeatedly; also runs on drop.
    pub fn disarm_and_reinject(&mut self) {
        if let Some(guard) = self.inner.take() {
            guard.disarm_and_reinject();
        }
    }
}

impl Drop for TypeaheadGuard {
    fn drop(&mut self) {
        self.disarm_and_reinject();
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{
        CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
        WAIT_FAILED, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Console::{
        GetNumberOfConsoleInputEvents, GetStdHandle, ReadConsoleInputW, WriteConsoleInputW,
        INPUT_RECORD, STD_INPUT_HANDLE,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, WaitForSingleObject};

    /// Poll cadence of the sweeper thread. Short enough that typeahead
    /// is moved out of the shared queue long before the next probing
    /// child spawns (observed journey cadence: one child every ~30-90
    /// ms), long enough that the thread's footprint stays invisible.
    const SWEEP_INTERVAL_MS: u32 = 2;

    /// How long disarm waits for the sweeper to park before detaching
    /// it. The sweeper's waits are bounded by [`SWEEP_INTERVAL_MS`], so
    /// it parks almost immediately; only a count-then-read race against
    /// a probing child can hold it longer, and that thread self-heals
    /// (reinjects its record and parks on its own).
    const PARK_TIMEOUT_MS: u64 = 50;

    /// A console-input handle closed on drop, so a detached sweeper can
    /// never leak one. Visible to the tests, which stage their own records
    /// through a duplicate of the same console handle (niubash#287).
    pub(super) struct OwnedHandle(pub(super) HANDLE);
    // Safety: the handle is the process-wide console input buffer, which
    // is valid on any thread; each OwnedHandle is exclusively owned by
    // exactly one thread (the guard's reinject copy never crosses a
    // spawn boundary after construction, the sweeper's copy moves into
    // its thread exactly once and stays there).
    unsafe impl Send for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    pub(super) struct Guard {
        stop: Arc<AtomicBool>,
        stopped: Arc<AtomicBool>,
        generation: Arc<AtomicU64>,
        saved: Arc<Mutex<Vec<INPUT_RECORD>>>,
        handle: OwnedHandle,
    }

    impl Guard {
        pub(super) fn arm() -> Option<Guard> {
            if !super::stdin_is_console() {
                return None;
            }
            // Two duplicates: the reinject path owns one, the sweeper
            // thread owns the other, so a detached sweeper keeps a valid
            // handle for its self-heal reinjection.
            let reinject_handle = OwnedHandle(duplicate_stdin_console_handle()?);
            let sweeper_handle = OwnedHandle(duplicate_stdin_console_handle()?);

            let stop = Arc::new(AtomicBool::new(false));
            let stopped = Arc::new(AtomicBool::new(false));
            let generation = Arc::new(AtomicU64::new(0));
            let saved = Arc::new(Mutex::new(Vec::new()));

            let spawned = std::thread::Builder::new()
                .name("niu-typeahead".to_string())
                .spawn({
                    let stop = Arc::clone(&stop);
                    let stopped = Arc::clone(&stopped);
                    let generation = Arc::clone(&generation);
                    let saved = Arc::clone(&saved);
                    move || sweeper(sweeper_handle, stop, stopped, generation, saved)
                })
                .is_ok();
            if !spawned {
                return None;
            }
            Some(Guard {
                stop,
                stopped,
                generation,
                saved,
                handle: reinject_handle,
            })
        }

        pub(super) fn disarm_and_reinject(self) {
            // Ask the sweeper to park and mark the generation dead; a
            // sweeper that wakes later reinjects what it holds and exits.
            self.stop.store(true, Ordering::SeqCst);
            self.generation.fetch_add(1, Ordering::SeqCst);

            let deadline = Instant::now() + Duration::from_millis(PARK_TIMEOUT_MS);
            while !self.stopped.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            // Parked or detached: the saved records are ours now. A
            // detached sweeper never appends after the generation check,
            // so this take cannot race a late writer.
            let records = self
                .saved
                .lock()
                .map(|mut buffer| std::mem::take(&mut *buffer))
                .unwrap_or_default();
            reinject(self.handle.0, &records);
        }
    }

    /// Sweep pending console input records into `saved` until told to
    /// park. Every read is preceded by an availability wait plus a count
    /// so the thread never blocks on an empty queue. If a probing child
    /// steals the counted records between count and read, the
    /// `ReadConsoleInputW` can block until the next record arrives; when
    /// that happens after disarm, the generation check catches it, the
    /// record is reinjected, and the thread parks — a disarmed guard
    /// never keeps user input.
    fn sweeper(
        handle: OwnedHandle,
        stop: Arc<AtomicBool>,
        stopped: Arc<AtomicBool>,
        generation: Arc<AtomicU64>,
        saved: Arc<Mutex<Vec<INPUT_RECORD>>>,
    ) {
        let conin = handle.0;
        let my_generation = generation.load(Ordering::SeqCst);
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let wait = unsafe { WaitForSingleObject(conin, SWEEP_INTERVAL_MS) };
            if wait == WAIT_FAILED {
                break;
            }
            if wait != WAIT_OBJECT_0 {
                continue; // timeout: the queue is empty
            }
            let mut available = 0u32;
            if unsafe { GetNumberOfConsoleInputEvents(conin, &mut available) } == 0
                || available == 0
            {
                continue;
            }
            let mut records: Vec<INPUT_RECORD> =
                vec![unsafe { std::mem::zeroed() }; available as usize];
            let mut read = 0u32;
            let ok =
                unsafe { ReadConsoleInputW(conin, records.as_mut_ptr(), available, &mut read) };
            let swept: Vec<INPUT_RECORD> = if ok == 0 || read == 0 {
                Vec::new()
            } else {
                records.truncate(read as usize);
                records
            };
            if !swept.is_empty() {
                if let Ok(mut buffer) = saved.lock() {
                    buffer.extend(swept);
                }
            }
            if generation.load(Ordering::SeqCst) != my_generation {
                // Disarmed while we were reading (possibly blocked and
                // woken by a fresh record): hand everything we hold
                // straight back to the queue and park. The buffer is
                // DRAINED under the lock: the disarm side takes whatever
                // remains after we park, so reinjecting a borrowed view
                // would let both paths write the same records back — every
                // swept key arriving twice at the editor (niubash#287).
                let records = saved
                    .lock()
                    .map(|mut buffer| std::mem::take(&mut *buffer))
                    .unwrap_or_default();
                reinject(conin, &records);
                break;
            }
        }
        stopped.store(true, Ordering::SeqCst);
    }

    fn reinject(conin: HANDLE, records: &[INPUT_RECORD]) {
        if records.is_empty() {
            return;
        }
        unsafe {
            let mut written = 0u32;
            WriteConsoleInputW(conin, records.as_ptr(), records.len() as u32, &mut written);
        }
    }

    pub(super) fn duplicate_stdin_console_handle() -> Option<HANDLE> {
        unsafe {
            let source = GetStdHandle(STD_INPUT_HANDLE);
            if source.is_null() || source == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut duplicate: HANDLE = std::ptr::null_mut();
            if DuplicateHandle(
                GetCurrentProcess(),
                source,
                GetCurrentProcess(),
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            ) == 0
            {
                return None;
            }
            Some(duplicate)
        }
    }
}

/// Whether the shell's stdin is an interactive console (the only shape
/// with a shared input buffer worth guarding).
#[cfg(windows)]
fn stdin_is_console() -> bool {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE};
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut mode = 0u32;
        GetConsoleMode(handle, &mut mode) != 0
    }
}

#[cfg(not(windows))]
mod imp {
    pub(super) struct Guard;

    impl Guard {
        pub(super) fn arm() -> Option<Guard> {
            None
        }

        pub(super) fn disarm_and_reinject(self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_is_disarmed_by_default_and_rearms_cleanly() {
        let mut guard = TypeaheadGuard::disarmed();
        guard.disarm_and_reinject(); // no-op, must not panic
        guard.arm();
        guard.disarm_and_reinject();
        guard.arm();
        guard.disarm_and_reinject();
    }

    #[test]
    fn drop_reinjects_without_panicking() {
        let mut guard = TypeaheadGuard::disarmed();
        guard.arm();
        drop(guard);
    }

    /// niubash#287: the sweeper's self-heal path and `disarm_and_reinject`
    /// must never write the same swept record back twice. Feed the live
    /// console queue so the self-heal interleaving actually runs: r1 is
    /// swept while armed, r2 lands as disarm fires, the sweeper wakes on it
    /// and takes the generation-miss branch. Whatever the interleaving, the
    /// invariant after the fix is "every swept record reinjected exactly
    /// once" — the records must come back exactly once each (the buggy
    /// borrowed-view reinject doubles them to four).
    #[cfg(windows)]
    #[test]
    fn sweeper_selfheal_and_disarm_reinject_each_record_exactly_once() {
        use std::time::Duration;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::Console::{
            GetNumberOfConsoleInputEvents, ReadConsoleInputW, WriteConsoleInputW, INPUT_RECORD,
            INPUT_RECORD_0, KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0,
        };

        if !stdin_is_console() {
            // Redirected stdin (pipelines, some CI): the guard arms nothing
            // and the shared console queue does not exist.
            eprintln!("skip: stdin is not a console");
            return;
        }
        let handle = imp::OwnedHandle(
            imp::duplicate_stdin_console_handle().expect("console stdin duplicates"),
        );

        fn drain_console(handle: HANDLE) -> Vec<INPUT_RECORD> {
            let mut all = Vec::new();
            loop {
                let mut pending = 0u32;
                if unsafe { GetNumberOfConsoleInputEvents(handle, &mut pending) } == 0
                    || pending == 0
                {
                    break;
                }
                let mut buffer: Vec<INPUT_RECORD> =
                    vec![unsafe { std::mem::zeroed() }; pending as usize];
                let mut read = 0u32;
                let ok =
                    unsafe { ReadConsoleInputW(handle, buffer.as_mut_ptr(), pending, &mut read) };
                if ok == 0 || read == 0 {
                    break;
                }
                buffer.truncate(read as usize);
                all.extend(buffer);
                if all.len() > 64 {
                    break; // safety valve; never expected
                }
            }
            all
        }

        fn write_record(handle: HANDLE, record: INPUT_RECORD) {
            let mut written = 0u32;
            let ok = unsafe { WriteConsoleInputW(handle, &record, 1, &mut written) };
            assert_eq!(ok, 1, "test record must reach the console queue");
        }

        fn key_record(ch: u16) -> INPUT_RECORD {
            INPUT_RECORD {
                EventType: KEY_EVENT as u16,
                Event: INPUT_RECORD_0 {
                    KeyEvent: KEY_EVENT_RECORD {
                        bKeyDown: 1,
                        wRepeatCount: 1,
                        wVirtualKeyCode: 0,
                        wVirtualScanCode: 0,
                        uChar: KEY_EVENT_RECORD_0 { UnicodeChar: ch },
                        dwControlKeyState: 0,
                    },
                },
            }
        }

        fn is_probe_press(record: &INPUT_RECORD) -> bool {
            if record.EventType != KEY_EVENT as u16 {
                return false;
            }
            // SAFETY: EventType is KEY_EVENT, so the KeyEvent arm is active.
            let key = unsafe { &record.Event.KeyEvent };
            key.bKeyDown != 0
                && matches!(
                    // SAFETY: union field read of the active KeyEvent arm.
                    unsafe { key.uChar.UnicodeChar },
                    97 | 98 // 'a' | 'b'
                )
        }

        // Clean slate: drop anything a runner or earlier test left queued.
        drain_console(handle.0);

        let mut guard = TypeaheadGuard::disarmed();
        guard.arm();
        // Park the sweeper on its cadence, then land r1 where it will be
        // swept into the guard's buffer.
        std::thread::sleep(Duration::from_millis(30));
        write_record(handle.0, key_record('a' as u16));
        std::thread::sleep(Duration::from_millis(30));
        // r2 as disarm fires: the sweeper is inside its read window, so the
        // generation-miss self-heal branch runs while disarm waits for it
        // to park.
        write_record(handle.0, key_record('b' as u16));
        guard.disarm_and_reinject();

        // A detached sweeper still has PARK_TIMEOUT to self-heal.
        std::thread::sleep(Duration::from_millis(50));
        let drained = drain_console(handle.0);
        let probes = drained
            .iter()
            .filter(|record| is_probe_press(record))
            .count();
        assert_eq!(
            probes, 2,
            "each swept record must be reinjected exactly once (niubash#287), got {probes}"
        );
        // Leave the shared queue clean for whatever runs next.
        drain_console(handle.0);
    }
}
