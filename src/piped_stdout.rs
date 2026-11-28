//! niu product-layer guard for a closed process stdout (niubash#245).
//!
//! rubash#455 made the engine treat a closed-output write as the hitting
//! command's own failure (diagnostic, status 1, list continues) — GNU-correct
//! for pipeline members and finite scripts. But when the SHELL'S OWN stdout
//! is the pipe whose reader went away, "keeps running" degenerates: every
//! subsequent write fails again, so an unbounded producer (`while true; do
//! echo …; done`, `yes`) earns one diagnostic per iteration forever and niu
//! never exits (host_contract::closed_stdout_pipe). On Unix SIGPIPE would
//! kill such a producer mid-write; Windows has no signal analogue — the same
//! condition surfaces as ERROR_BROKEN_PIPE (109) / ERROR_NO_DATA (232) from
//! the write, which is exactly what the engine's write path reports.
//!
//! Fix shape: for non-interactive executions with a PIPE on stdout, niu
//! interposes a kernel pipe plus a pump thread between the process's
//! stdout writers (engine builtins, external children — everything that
//! targets STD_OUTPUT_HANDLE) and the real downstream:
//!
//! ```text
//! writers -> wrapper pipe (SetStdHandle) -> pump thread -> real stdout
//! ```
//!
//! The pump is the only writer to the real handle, so byte order across all
//! writers is unchanged (single FIFO). When the downstream reader goes away
//! the pump's write fails with the broken-pipe error and niu enforces the
//! rubash#455 contract at the process level: ONE diagnostic on stderr, then
//! exit status 1 — no per-iteration spam, no unbounded loop. While the
//! reader lives, the pump is transparent: same bytes, same order, and
//! backpressure flows through the pipe buffer exactly as before.
//!
//! `finish` drains before process exit: it flags the pump as draining and
//! closes niu's pipe-write handle; the pump forwards everything written up
//! to that point, then stops. It deliberately does NOT wait for EOF:
//! background descendants inherit the write end and can postpone EOF
//! forever (niubash#1122 — a long-lived daemon made `niu -c` never exit).
//! The post-execution exit funnels route through `finish_and_exit`; a
//! live-but-slow reader still holds the drain exactly as GNU
//! (`bash -c 'seq 1000000' | slow-reader` waits for the pipe just like the
//! pre-change niu blocked in write), but a lingering background writer no
//! longer does. The price is that bytes a background descendant writes
//! only after `finish` has run may be lost; GNU delivers them because its
//! children write to the real pipe directly.
//!
//! The wrapper is never installed for the interactive REPL (console stdout —
//! a console cannot break), the launcher word paths (`--version`/`--help`
//! keep their own closed-pipe exit-0 policy, pinned by tests/closed_pipe.rs),
//! `--internal-*` pipeline components (their quiet exit-on-broken-write
//! policy IS the GNU child-SIGPIPE shape), `niu -i`, or when stdout is not a
//! pipe at all. Non-Windows targets keep the engine-side #455 semantics (no
//! pump): this guard is the Windows pipe-death analogue of SIGPIPE.
//!
//! One install case carries a different broken-write POLICY (niubash#1079):
//! a provenance-validated respawned child — a process-substitution or coproc
//! body the engine respawned as a plain `niu -c '<body>'` with its stdout on
//! the substitution pipe. In GNU that body is a forked subshell, so a reader
//! that stops early kills it with SIGPIPE: no diagnostic, and the parent
//! shell's `wait` reports 141. The pump still installs (the child's writers
//! need the same single FIFO), but when the downstream write fails the pump
//! exits quietly with 141 instead of the #245 diagnostic + status 1. The
//! claim is read in `install_for_argv`, before the engine imports and scrubs
//! the provenance keys, via rubash's own parentage validation — a user-run
//! `niu -c '…' | head -1` carries no validated claim and keeps the #245
//! behavior exactly.

/// Exit status used when the process stdout dies: rubash#455 semantics — a
/// failed write is an ordinary command failure (status 1), reported once.
#[cfg(windows)]
const BROKEN_STDOUT_EXIT_CODE: i32 = 1;

/// Exit status for a provenance-validated respawned child (niubash#1079)
/// whose downstream reader went away: the GNU SIGPIPE death status,
/// 128 + 13. rubash already reports 141 for a producer cut off by its
/// reader (`yes | head -1` gives PIPESTATUS[0] 141), so the parent shell's
/// `wait` on a procsub/coproc child must see the same value.
#[cfg(windows)]
const RESPAWNED_BROKEN_PIPE_EXIT_CODE: i32 = 141;

/// Diagnosed once, by the pump, when the downstream reader is gone. The
/// wording deliberately avoids the raw OS surfaces ("Broken pipe",
/// "os error 232", "管道正在被关闭") that host_contract treats as scary
/// spam markers — the condition is described, not the errno.
#[cfg(windows)]
const BROKEN_STDOUT_DIAGNOSTIC: &str =
    "niu: stdout: write error: pipe closed by downstream reader\n";

/// Whether argv selects one of the non-interactive execution flows this guard
/// covers. Called before anything in the process has written to stdout.
pub fn install_for_argv(args: &[String]) {
    if execution_flow_argv(args) {
        // niubash#1079: read the respawn claim NOW, before the engine
        // imports the environment and scrubs the provenance keys — this is
        // the same pre-`Executor::new` read rubash's own main performs for
        // its async-child entry face. The verdict rides parentage
        // validation, not the raw env strings, so a user shell that merely
        // inherits lookalike keys does not qualify.
        #[cfg(windows)]
        imp::install(rubash::executor::respawn_provenance_validated_from_process_env());
    }
}

/// argv shapes that execute a script/command non-interactively (`niu -c`,
/// `-s`, a script path, a stdin-script via bare `niu` with redirected stdin,
/// and engine-option words that may prefix those). Launcher words, the
/// interactive `-i` route, REPL subcommands, and `--internal-*` pipeline
/// components are left exactly as they were.
fn execution_flow_argv(args: &[String]) -> bool {
    const LAUNCHER_WORDS: &[&str] = &[
        "-h",
        "--help",
        "-V",
        "--version",
        "-C",
        "--repl-command",
        "--completion-probe",
        "--install-wt-profile",
        "--self-update",
    ];
    const NON_SHELL_SUBCOMMANDS: &[&str] = &[
        "setup",
        "configure",
        "font",
        "doctor",
        "config",
        "plugin",
        "skill",
    ];
    match args.get(1) {
        // Bare `niu`: the REPL when interactive, otherwise a stdin script —
        // only the stdin-script shape executes through the engine here.
        None => !niubash_runtime::terminal::stdio_is_interactive(),
        Some(first) => {
            if LAUNCHER_WORDS.contains(&first.as_str())
                || NON_SHELL_SUBCOMMANDS.contains(&first.as_str())
                || first.starts_with("--internal-")
                || first == "-i"
            {
                return false;
            }
            // Engine-option words (possibly prefixing -c) and script paths.
            args.len() >= 2
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::{
        BROKEN_STDOUT_DIAGNOSTIC, BROKEN_STDOUT_EXIT_CODE, RESPAWNED_BROKEN_PIPE_EXIT_CODE,
    };
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        CloseHandle, SetHandleInformation, ERROR_NO_DATA, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE};
    use windows_sys::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::Pipes::{CreatePipe, SetNamedPipeHandleState, PIPE_NOWAIT};

    type Handle = windows_sys::Win32::Foundation::HANDLE;

    /// Wrapper pipe capacity (bytes). Only absorbs burst while the pump
    /// forwards; sustained flow is governed by downstream backpressure.
    const PIPE_BUFFER_BYTES: u32 = 1024 * 1024;
    /// Pump read chunk.
    const PUMP_CHUNK_BYTES: usize = 64 * 1024;

    struct PumpState {
        /// niu's retained copy of the wrapper pipe write end — the handle
        /// that is STD_OUTPUT_HANDLE for the rest of the process. Closed in
        /// `finish` so the pump observes EOF and can drain.
        write_handle: isize,
        /// Signaled by the pump after it forwarded everything (EOF).
        done: Receiver<()>,
        /// Set by `finish` when the exit drain starts. Once set, the pump
        /// stops at the first empty read instead of waiting for EOF:
        /// background descendants may hold the inheritable write end open
        /// forever (niubash#1122), so EOF is not a drain condition niu
        /// controls.
        draining: Arc<AtomicBool>,
    }

    static PUMP: Mutex<Option<PumpState>> = Mutex::new(None);

    /// Interpose the pump between the process's stdout writers and the real
    /// pipe — but only when stdout IS a pipe (the only stream whose reader
    /// can vanish and loop the engine). Must run before the first
    /// `std::io::stdout()` use so Rust's cached stdio handle resolves to the
    /// wrapper, keeping every writer on the same FIFO.
    pub(super) fn install(respawned_child: bool) {
        unsafe {
            let real: Handle = GetStdHandle(STD_OUTPUT_HANDLE);
            if real.is_null() || real == INVALID_HANDLE_VALUE {
                return;
            }
            if GetFileType(real) != FILE_TYPE_PIPE {
                return;
            }

            let mut read_end: Handle = std::ptr::null_mut();
            let mut write_end: Handle = std::ptr::null_mut();
            // The write end must be inheritable: external children spawned by
            // scripts inherit STD_OUTPUT_HANDLE and their output flows
            // through the same pump (same bytes, same order as before).
            let security = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: std::ptr::null_mut(),
                bInheritHandle: 1,
            };
            if CreatePipe(&mut read_end, &mut write_end, &security, PIPE_BUFFER_BYTES) == 0 {
                return;
            }
            // The read end is the pump's private sink; keep it out of
            // children so a lingering child cannot hold the drain open in
            // reverse (only write ends matter for EOF).
            SetHandleInformation(read_end, HANDLE_FLAG_INHERIT, 0);
            // The pump must be able to poll the wrapper pipe. With a
            // blocking read it could only stop at EOF, and EOF requires
            // EVERY inheritable write end to close — including the ones
            // background descendants carry away (niubash#1122). NOWAIT
            // makes an empty pipe report an empty read instead, which is
            // what lets `finish` drain only what is already written. If
            // the call fails the pump keeps the pre-#1122 blocking drain.
            let mode = PIPE_NOWAIT;
            let _ = SetNamedPipeHandleState(read_end, &mode, std::ptr::null(), std::ptr::null());
            if SetStdHandle(STD_OUTPUT_HANDLE, write_end) == 0 {
                CloseHandle(read_end);
                CloseHandle(write_end);
                return;
            }

            let (done_sender, done_receiver) = mpsc::channel::<()>();
            let read_file = std::mem::ManuallyDrop::new(File::from_raw_handle(read_end));
            let real_handle = real as isize;
            let draining = Arc::new(AtomicBool::new(false));
            let pump_draining = Arc::clone(&draining);
            let spawned = std::thread::Builder::new()
                .name("niu-stdout-pump".to_string())
                .spawn(move || {
                    pump_loop(
                        read_file,
                        real_handle,
                        done_sender,
                        respawned_child,
                        pump_draining,
                    )
                });
            match spawned {
                Ok(_join) => {
                    *PUMP.lock().expect("pump state mutex") = Some(PumpState {
                        write_handle: write_end as isize,
                        done: done_receiver,
                        draining,
                    });
                }
                Err(_) => {
                    // Fail open: put the real stdout back and run unwrapped
                    // (the pre-#245 behavior) rather than lose the stream.
                    SetStdHandle(STD_OUTPUT_HANDLE, real);
                    CloseHandle(read_end);
                    CloseHandle(write_end);
                }
            }
        }
    }

    /// Forward wrapper-pipe bytes to the real downstream until EOF. Any
    /// downstream write failure is the reader-gone condition: report ONCE
    /// and kill the process (status 1) — the anti-loop guarantee that the
    /// engine alone cannot provide when the shell's own stdout is the pipe
    /// that closed. A provenance-validated respawned child (niubash#1079)
    /// takes the GNU child-SIGPIPE shape instead: no diagnostic, exit 141 —
    /// in GNU that child is a forked subshell and dies by the signal.
    fn pump_loop(
        mut read: std::mem::ManuallyDrop<File>,
        real_handle: isize,
        done: Sender<()>,
        respawned_child: bool,
        draining: Arc<AtomicBool>,
    ) {
        let mut real: std::mem::ManuallyDrop<File> =
            unsafe { std::mem::ManuallyDrop::new(File::from_raw_handle(real_handle as RawHandle)) };
        let mut buffer = vec![0_u8; PUMP_CHUNK_BYTES];
        loop {
            match read.read(&mut buffer) {
                // A NOWAIT read on a momentarily empty pipe surfaces as
                // Ok(0) — std maps ERROR_NO_DATA to a zero-length read —
                // NOT as an EOF signal. Only `draining` distinguishes
                // "empty for now" from "empty for good": before
                // `finish`, poll again (a live writer — at minimum the
                // shell itself — always exists then); once draining,
                // empty means every byte written before `finish` has
                // been forwarded, so stop (the error-form arm below is
                // the same condition kept defensively).
                Ok(0) => {
                    if draining.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(size) => {
                    if write_all(&mut real, &buffer[..size]).is_err() {
                        if respawned_child {
                            std::process::exit(RESPAWNED_BROKEN_PIPE_EXIT_CODE);
                        }
                        report_and_exit();
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if is_would_block(&error) => {
                    // NOWAIT read on a momentarily empty pipe (error
                    // form of the Ok(0) case above). Before
                    // `finish`, just poll again — more output may come.
                    // Once draining, empty means every byte written
                    // before `finish` has been forwarded (foreground
                    // writers are all awaited by then, so their bytes
                    // are already in the pipe): stop instead of waiting
                    // for an EOF that background descendants may
                    // postpone forever (niubash#1122).
                    if draining.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => break,
            }
        }
        let _ = done.send(());
    }

    /// A NOWAIT pipe read with no data available reports ERROR_NO_DATA.
    /// In practice std surfaces that condition as a zero-length Ok(0)
    /// read (handled by the Ok(0) arm in `pump_loop`); this error-form
    /// check is kept defensively for paths that report it as an error.
    fn is_would_block(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::WouldBlock
            || error.raw_os_error() == Some(ERROR_NO_DATA as i32)
    }

    fn write_all(real: &mut File, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            match real.write(data) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "stdout write made no progress",
                    ))
                }
                Ok(written) => data = &data[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn report_and_exit() -> ! {
        {
            let mut stderr = io::stderr().lock();
            let _ = stderr.write_all(BROKEN_STDOUT_DIAGNOSTIC.as_bytes());
            let _ = stderr.flush();
        }
        std::process::exit(BROKEN_STDOUT_EXIT_CODE)
    }

    /// Drain (niubash#1122): flag the pump as draining, close niu's
    /// wrapper write end, then wait for the pump to forward everything
    /// written BEFORE this call. The drain deliberately does not wait
    /// for EOF: background descendants inherit the wrapper write end, so
    /// EOF can be postponed indefinitely by a daemon that outlives the
    /// script — the pre-#1122 behavior, which made `niu -c 'gradle …'`
    /// never exit. Bytes a background descendant writes only after this
    /// point may therefore be lost; GNU delivers them because its
    /// children write to the real pipe directly. A live-but-slow reader
    /// still holds the exit (the pump blocks forwarding, exactly as GNU
    /// blocks in write), and a GONE reader can never block it because
    /// the pump fails its write and exits the process first.
    pub fn finish() {
        let Some(state) = PUMP.lock().ok().and_then(|mut slot| slot.take()) else {
            return;
        };
        state.draining.store(true, Ordering::SeqCst);
        unsafe {
            CloseHandle(state.write_handle as Handle);
        }
        // Err means the pump thread is already gone (it exited the process
        // via the broken-write path); either way there is nothing to await.
        let _ = state.done.recv();
    }
}

#[cfg(windows)]
pub(crate) use imp::finish;

#[cfg(not(windows))]
pub fn finish() {}

/// Drain the pump (when installed), then exit with `code`. Used by every
/// post-execution exit funnel so `niu -c '…; false' | cat` still delivers
/// the script's full output before the nonzero status lands.
pub fn finish_and_exit(code: i32) -> ! {
    finish();
    std::process::exit(code)
}
