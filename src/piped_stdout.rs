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
//! `finish` drains before process exit: it closes niu's pipe-write handle so
//! the pump reads EOF, then joins it, so normal runs never lose output. The
//! post-execution exit funnels route through `finish_and_exit`; a blocking
//! drain at that point matches GNU (`bash -c 'seq 1000000' | slow-reader`
//! waits for the pipe just like the pre-change niu blocked in write).
//!
//! The wrapper is never installed for the interactive REPL (console stdout —
//! a console cannot break), the launcher word paths (`--version`/`--help`
//! keep their own closed-pipe exit-0 policy, pinned by tests/closed_pipe.rs),
//! `--internal-*` pipeline components (their quiet exit-on-broken-write
//! policy IS the GNU child-SIGPIPE shape), `niu -i`, or when stdout is not a
//! pipe at all. Non-Windows targets keep the engine-side #455 semantics (no
//! pump): this guard is the Windows pipe-death analogue of SIGPIPE.

/// Exit status used when the process stdout dies: rubash#455 semantics — a
/// failed write is an ordinary command failure (status 1), reported once.
#[cfg(windows)]
const BROKEN_STDOUT_EXIT_CODE: i32 = 1;

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
        #[cfg(windows)]
        imp::install();
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
    use super::{BROKEN_STDOUT_DIAGNOSTIC, BROKEN_STDOUT_EXIT_CODE};
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::Mutex;

    use windows_sys::Win32::Foundation::{
        CloseHandle, SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE};
    use windows_sys::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::Pipes::CreatePipe;

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
    }

    static PUMP: Mutex<Option<PumpState>> = Mutex::new(None);

    /// Interpose the pump between the process's stdout writers and the real
    /// pipe — but only when stdout IS a pipe (the only stream whose reader
    /// can vanish and loop the engine). Must run before the first
    /// `std::io::stdout()` use so Rust's cached stdio handle resolves to the
    /// wrapper, keeping every writer on the same FIFO.
    pub(super) fn install() {
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
            if SetStdHandle(STD_OUTPUT_HANDLE, write_end) == 0 {
                CloseHandle(read_end);
                CloseHandle(write_end);
                return;
            }

            let (done_sender, done_receiver) = mpsc::channel::<()>();
            let read_file = std::mem::ManuallyDrop::new(File::from_raw_handle(read_end));
            let real_handle = real as isize;
            let spawned = std::thread::Builder::new()
                .name("niu-stdout-pump".to_string())
                .spawn(move || pump_loop(read_file, real_handle, done_sender));
            match spawned {
                Ok(_join) => {
                    *PUMP.lock().expect("pump state mutex") = Some(PumpState {
                        write_handle: write_end as isize,
                        done: done_receiver,
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
    /// that closed.
    fn pump_loop(mut read: std::mem::ManuallyDrop<File>, real_handle: isize, done: Sender<()>) {
        let mut real: std::mem::ManuallyDrop<File> =
            unsafe { std::mem::ManuallyDrop::new(File::from_raw_handle(real_handle as RawHandle)) };
        let mut buffer = vec![0_u8; PUMP_CHUNK_BYTES];
        loop {
            match read.read(&mut buffer) {
                Ok(0) => break, // EOF: every writer closed — fully drained
                Ok(size) => {
                    if write_all(&mut real, &buffer[..size]).is_err() {
                        report_and_exit();
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = done.send(());
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

    /// Drain: close niu's wrapper write end so the pump reads EOF once every
    /// other writer (background children) is gone, then wait for the pump to
    /// forward the remainder. Blocking here matches GNU — a live-but-slow
    /// reader holds the exit exactly as the pre-change niu blocked writing;
    /// a GONE reader can never block it because the pump fails its write and
    /// exits the process first.
    pub fn finish() {
        let Some(state) = PUMP.lock().ok().and_then(|mut slot| slot.take()) else {
            return;
        };
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
