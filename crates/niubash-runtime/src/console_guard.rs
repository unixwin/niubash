//! Console baseline guard for the interactive REPL.
//!
//! Misbehaving child processes can leave the Windows console in a broken
//! state when they exit: Windows OpenSSH (`ssh.exe`) in particular saves the
//! console modes at startup and restores stale values on its failed-auth
//! path, which disables `ENABLE_VIRTUAL_TERMINAL_PROCESSING` /
//! `ENABLE_PROCESSED_OUTPUT` on the shared screen buffer handle. After that,
//! every escape sequence the REPL emits prints literally (`←[?25l`,
//! `♪◙`), the cursor stays hidden, and the prompt looks frozen.
//!
//! This mirrors what GNU bash does on Unix: snapshot the terminal discipline
//! at shell startup and restore it before every prompt. Here the discipline
//! is the console mode of the three standard handles:
//!
//! - stdin keeps whatever mode the shell started with (line input, echo,
//!   processed input); reedline re-enables raw mode itself on top of it for
//!   each `read_line`.
//! - stdout/stderr additionally get the VT-processing bits forced on, so a
//!   baseline captured *after* pollution is still repaired.
//!
//! Cursor visibility cannot be queried from the console API, so `restore`
//! always re-shows the cursor before the next prompt is drawn.

/// Snapshot of the console modes niubash owns between interactive commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConsoleBaseline {
    stdin_mode: Option<u32>,
    stdout_mode: Option<u32>,
    stderr_mode: Option<u32>,
}

/// Capture the console baseline for the current process. Captured once at
/// REPL startup; call [`restore`] before every prompt redraw.
pub fn capture() -> ConsoleBaseline {
    platform::capture()
}

/// Prefer UTF-8 for Windows console I/O so native child tools render Unicode
/// filenames consistently. This is best-effort and a no-op off Windows.
pub fn prefer_utf8_code_page() {
    platform::prefer_utf8_code_page();
}

/// Force-enable ANSI VT processing on the stdout/stderr console handles.
/// Legacy conhost — the default terminal on Windows 10 — starts new
/// processes without `ENABLE_VIRTUAL_TERMINAL_PROCESSING`, so every escape
/// sequence prints literally until something opts in. Call once at startup
/// so early output like the setup wizard logo renders correctly; handles
/// that are not consoles are skipped.
pub fn enable_vt_output() {
    platform::enable_vt_output();
}

/// Restore the baseline console modes and re-show the cursor. Safe to call
/// when handles are redirected (non-console handles are skipped).
pub fn restore(baseline: &ConsoleBaseline) {
    platform::restore(baseline);
}

/// niubash#379, host level: a console-less invoker (agent/CI/git hook that
/// spawns `niu -c` plainly, without the `bash.exe` forwarder's
/// CREATE_NO_WINDOW/DETACHED_PROCESS) makes Windows auto-allocate a
/// *visible* console window for this console-subsystem binary before main()
/// runs. Releasing that console kills the window within milliseconds
/// instead of letting it outlive the whole command.
///
/// Safety rails, all required: `GetConsoleProcessList` must report this
/// process as the console's sole attachment (an inherited console — a real
/// terminal — has other processes on it and is never ours to free), and
/// `GetConsoleWindow()` must report a VISIBLE window — a *hidden* console
/// (CREATE_NO_WINDOW from the `bash.exe` forwarder, or ConPTY) is kept
/// alive so grandchildren inherit it for free instead of paying ~22 ms
/// per child spawn. ConPTY is the subtle rail: on current Windows a
/// pseudoconsole DOES own a window handle — it belongs to the headless
/// console host and is never shown — so a non-NULL window alone does not
/// discriminate; a lone ConPTY client (the journey/asset-timing gates'
/// `niu setup`, `niu --rcfile <rc> -i` PtyProcess spawns) is process-list
/// 1 with a non-NULL *invisible* window, and freeing it kills the pty's
/// only client and ends the session outputless (niubash#455, release run
/// 39363549422 J7 + every per-asset fp/rr phase). `IsWindowVisible` is the
/// discriminator that matches the harm: only a console that can paint a
/// window on the user's desktop is ours to release.
/// The CALLER decides whether the invocation might be
/// interactive before releasing: Windows binds the auto-allocated
/// console's handles onto the standard streams, so an is_terminal() check
/// cannot tell an agent's auto console from a user's freshly opened one.
///
/// Trade-off noted: `niu script.sh`-style non-interactive launches that
/// arrive with their own fresh console (file-association/double-click
/// spawns) lose the window — the script's output has nowhere to display.
/// Shipped shortcuts launch `wt -p` without args (interactive), so the
/// packaged faces never hit this; a user wanting a scripted window uses
/// `start cmd /k niu script.sh`.
pub fn release_orphan_console() {
    platform::release_orphan_console();
}

/// niubash#451, child level: when niu itself is attached to no console
/// (agent/CI spawn — e.g. a forwarder that launched us DETACHED, or after
/// `release_orphan_console` fired), a plainly spawned console-subsystem
/// child makes Windows allocate a *visible* console window for it.
/// CREATE_NO_WINDOW gives the child a hidden console with full console-API
/// access; under a real console or ConPTY (`GetConsoleCP()!=0`) this is a
/// no-op and children inherit the real console as before.
pub fn suppress_orphan_console(command: &mut std::process::Command) {
    platform::suppress_orphan_console(command);
}

/// `Command::new` + `suppress_orphan_console` — the form spawn sites use
/// when the Command is built inline for a fluent `.output()`/`.status()`.
pub fn suppressed_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    platform::suppress_orphan_console(&mut command);
    command
}

/// niubash#462: claim the first child's hidden `CREATE_NO_WINDOW` console for
/// the host session so subsequent external spawns inherit it for free instead
/// of paying ~22 ms per child spawn.
pub fn adopt_orphan_console(child_pid: u32) {
    platform::adopt_orphan_console(child_pid);
}

#[cfg(windows)]
mod platform {
    use std::io::IsTerminal;

    use super::ConsoleBaseline;
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::GetFileType;
    use windows_sys::Win32::System::Console::{
        AttachConsole, FreeConsole, GetConsoleCP, GetConsoleMode, GetConsoleProcessList,
        GetConsoleWindow, GetStdHandle, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP,
        SetStdHandle, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        ENABLE_WRAP_AT_EOL_OUTPUT, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Threading::SwitchToThread;
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;

    const UTF8_CODE_PAGE: u32 = 65001;

    pub fn capture() -> ConsoleBaseline {
        ConsoleBaseline {
            stdin_mode: console_mode(STD_INPUT_HANDLE),
            stdout_mode: console_mode(STD_OUTPUT_HANDLE).map(ensure_vt_bits),
            stderr_mode: console_mode(STD_ERROR_HANDLE).map(ensure_vt_bits),
        }
    }

    pub fn prefer_utf8_code_page() {
        unsafe {
            let _ = SetConsoleCP(UTF8_CODE_PAGE);
            let _ = SetConsoleOutputCP(UTF8_CODE_PAGE);
        }
    }

    pub fn enable_vt_output() {
        for handle_id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if let Some(mode) = console_mode(handle_id) {
                set_console_mode(handle_id, ensure_vt_bits(mode));
            }
        }
    }

    pub fn release_orphan_console() {
        // GetConsoleCP()!=0 says a console is attached. The window rail is
        // VISIBILITY, not presence: a ConPTY pseudoconsole owns a NON-NULL
        // but invisible window (its headless host's), so non-NULL alone
        // would free the pty's only client and kill the session
        // (niubash#455). CREATE_NO_WINDOW consoles report no/hidden window
        // and stay kept either way.
        if unsafe { GetConsoleCP() } == 0 {
            return;
        }
        let window = unsafe { GetConsoleWindow() };
        if window.is_null() || unsafe { IsWindowVisible(window) } == 0 {
            return;
        }
        let mut pids = [0u32; 64];
        // The call reports the full attachment count even when the buffer
        // is short, so ==1 means "allocated for this process alone".
        if unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) } != 1 {
            return;
        }
        unsafe {
            FreeConsole();
            repair_dead_std_handles();
        }
    }

    /// niubash#455: a plainly spawned `niu -c`/`niu script` gets both its
    /// orphan console AND its std handles from Windows — `FreeConsole`
    /// destroys the console but leaves the three std slots pointing at it,
    /// so every later external spawn inherits invalid handles: the engine's
    /// stdio plan fails its handle dup ("Bad file descriptor", rc 126) and
    /// a child that does run sees dead console I/O (blank render, dead
    /// input — the exact #455 handoff failure). Re-point each dead slot at
    /// an inheritable NUL handle: children spawn with valid stdio, reads
    /// get EOF, writes are discarded — the same headless posture a
    /// DETACHED invoker hands us via pipes.
    unsafe fn repair_dead_std_handles() {
        for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let h = unsafe { GetStdHandle(id) };
            let dead = if h.is_null() || h == INVALID_HANDLE_VALUE {
                true
            } else {
                // A freed console's handles report either a lookup failure
                // or FILE_TYPE_CHAR; only real file/pipe handles survive.
                unsafe { SetLastError(0) };
                let t = unsafe { GetFileType(h) };
                t == 0 || t == windows_sys::Win32::Storage::FileSystem::FILE_TYPE_CHAR
            };
            if dead {
                if let Some(nul) = open_nul_handle() {
                    unsafe { SetStdHandle(id, nul) };
                }
            }
        }
    }

    /// An inheritable `NUL` device handle. Leaked into the process's std
    /// slot — like every std handle it lives for the process lifetime.
    fn open_nul_handle() -> Option<windows_sys::Win32::Foundation::HANDLE> {
        use windows_sys::Win32::Foundation::GENERIC_READ;
        use windows_sys::Win32::Foundation::GENERIC_WRITE;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        let sa = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()
                as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let name: Vec<u16> = "NUL".encode_utf16().chain(std::iter::once(0)).collect();
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &sa,
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            None
        } else {
            Some(h)
        }
    }

    pub fn suppress_orphan_console(command: &mut std::process::Command) {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW = 0x08000000 (WinBase.h). GetConsoleWindow() is
        // not the gate: under ConPTY it reports a NON-NULL handle (the
        // headless host's invisible PseudoConsoleWindow) while a console
        // is very much attached — GetConsoleCP()==0 means truly detached.
        if unsafe { GetConsoleCP() } == 0 && !try_alloc_hidden_console() {
            command.creation_flags(0x0800_0000);
        }
    }

    unsafe fn preserve_std_handles<F: FnOnce() -> bool>(f: F) -> bool {
        unsafe fn live_std_handle(id: u32) -> Option<*mut core::ffi::c_void> {
            let h = unsafe { GetStdHandle(id) };
            if h.is_null() || h == INVALID_HANDLE_VALUE {
                return None;
            }
            unsafe {
                SetLastError(0);
                if GetFileType(h) == 0 && GetLastError() != 0 {
                    return None;
                }
            }
            Some(h)
        }

        let stdin = live_std_handle(STD_INPUT_HANDLE);
        let stdout = live_std_handle(STD_OUTPUT_HANDLE);
        let stderr = live_std_handle(STD_ERROR_HANDLE);
        let ok = f();
        // niubash#852: restore on partial failure too, not only on
        // success — a half-failed AllocConsoleWithOptions may already
        // have swapped some std handles, and leaving the swap in
        // place hands broken handles to every later adopt/spawn path.
        // Restoring unconditionally is safe: on full success the
        // handles are put back exactly as before, and when `f` never
        // touched them the restore is a no-op rewrite of the same
        // values.
        if let Some(h) = stdin {
            let _ = SetStdHandle(STD_INPUT_HANDLE, h);
        }
        if let Some(h) = stdout {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
        }
        if let Some(h) = stderr {
            let _ = SetStdHandle(STD_ERROR_HANDLE, h);
        }
        ok
    }

    fn try_alloc_hidden_console() -> bool {
        #[repr(C)]
        struct AllocConsoleOptions {
            mode: i32,
            use_show_window: i32,
            show_window: u16,
        }
        type AllocConsoleWithOptionsFn =
            unsafe extern "system" fn(*const AllocConsoleOptions, *mut i32) -> i32;
        const ALLOC_CONSOLE_MODE_NO_WINDOW: i32 = 2;

        extern "system" {
            fn GetModuleHandleA(module_name: *const u8) -> *mut core::ffi::c_void;
            fn GetProcAddress(
                module: *mut core::ffi::c_void,
                proc_name: *const u8,
            ) -> *mut core::ffi::c_void;
        }

        unsafe {
            let module = GetModuleHandleA(c"kernelbase.dll".as_ptr().cast());
            if module.is_null() {
                return false;
            }
            let proc = GetProcAddress(module, c"AllocConsoleWithOptions".as_ptr().cast());
            if proc.is_null() {
                return false;
            }
            let func: AllocConsoleWithOptionsFn = core::mem::transmute(proc);
            preserve_std_handles(|| {
                let options = AllocConsoleOptions {
                    mode: ALLOC_CONSOLE_MODE_NO_WINDOW,
                    use_show_window: 0,
                    show_window: 0,
                };
                let mut result = 0i32;
                func(&options, &mut result) >= 0 && GetConsoleCP() != 0
            })
        }
    }

    pub fn adopt_orphan_console(child_pid: u32) {
        unsafe {
            if GetConsoleCP() != 0 {
                return;
            }
            const ERROR_INVALID_HANDLE: u32 = 6;
            const ERROR_GEN_FAILURE: u32 = 31;
            preserve_std_handles(|| {
                let start = std::time::Instant::now();
                let spin_limit = std::time::Duration::from_millis(30);
                let gen_failure_limit = std::time::Duration::from_millis(250);
                let invalid_handle_limit = std::time::Duration::from_millis(500);
                loop {
                    if AttachConsole(child_pid) != 0 {
                        return true;
                    }
                    let err = GetLastError();
                    let elapsed = start.elapsed();
                    let keep_waiting = (err == ERROR_INVALID_HANDLE
                        && elapsed < invalid_handle_limit)
                        || (err == ERROR_GEN_FAILURE && elapsed < gen_failure_limit);
                    if !keep_waiting {
                        return false;
                    }
                    if elapsed < spin_limit {
                        if SwitchToThread() == 0 {
                            std::thread::yield_now();
                        }
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            });
        }
    }

    pub fn restore(baseline: &ConsoleBaseline) {
        if let Some(mode) = baseline.stdin_mode {
            set_console_mode(STD_INPUT_HANDLE, mode);
        }
        if let Some(mode) = baseline.stdout_mode {
            set_console_mode(STD_OUTPUT_HANDLE, mode);
        }
        if let Some(mode) = baseline.stderr_mode {
            set_console_mode(STD_ERROR_HANDLE, mode);
        }

        // Cursor commands are ANSI output; only emit them into a real
        // terminal so redirected stdout never receives escape bytes.
        if std::io::stdout().is_terminal() {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
        }
    }

    /// Output modes niubash relies on. Forced in addition to whatever the
    /// snapshot saw so a baseline taken after pollution still heals.
    fn ensure_vt_bits(mode: u32) -> u32 {
        mode | ENABLE_PROCESSED_OUTPUT
            | ENABLE_WRAP_AT_EOL_OUTPUT
            | ENABLE_VIRTUAL_TERMINAL_PROCESSING
    }

    fn console_mode(handle_id: u32) -> Option<u32> {
        unsafe {
            let handle = GetStdHandle(handle_id);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) != 0 {
                Some(mode)
            } else {
                None
            }
        }
    }

    fn set_console_mode(handle_id: u32, mode: u32) {
        unsafe {
            let handle = GetStdHandle(handle_id);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return;
            }
            SetConsoleMode(handle, mode);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn ensure_vt_bits_forces_output_bits() {
            let restored = ensure_vt_bits(0);
            assert_ne!(restored & ENABLE_PROCESSED_OUTPUT, 0);
            assert_ne!(restored & ENABLE_WRAP_AT_EOL_OUTPUT, 0);
            assert_ne!(restored & ENABLE_VIRTUAL_TERMINAL_PROCESSING, 0);

            // Existing bits are preserved.
            let with_extra = ensure_vt_bits(0x8000_0000);
            assert_ne!(with_extra & 0x8000_0000, 0);
        }

        #[test]
        fn capture_and_restore_is_safe_and_idempotent() {
            let baseline = capture();
            prefer_utf8_code_page();
            enable_vt_output();
            restore(&baseline);
            restore(&baseline);
        }

        /// niubash#852 (CI-verifies, windows-only code): the std
        /// handles must be restored even when the guarded operation
        /// reports failure — a partially-failed console allocation
        /// used to leave them swapped.
        #[test]
        fn issue852_preserve_std_handles_restores_on_failure() {
            use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
            let before = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
            let ok = unsafe { preserve_std_handles(|| false) };
            assert!(!ok, "the guarded failure must still report false");
            assert_eq!(
                unsafe { GetStdHandle(STD_OUTPUT_HANDLE) },
                before,
                "a failed guarded call must not leave std handles swapped"
            );
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::ConsoleBaseline;

    // On Unix the terminal discipline lives in termios; reedline/crossterm
    // already bracket each read with raw-mode save/restore, and child
    // processes are expected to leave the tty alone. Keep this a no-op until
    // there is evidence of the same pollution class on Unix.
    pub fn capture() -> ConsoleBaseline {
        ConsoleBaseline::default()
    }

    pub fn prefer_utf8_code_page() {}

    pub fn enable_vt_output() {}

    pub fn release_orphan_console() {}

    pub fn suppress_orphan_console(_command: &mut std::process::Command) {}

    pub fn adopt_orphan_console(_child_pid: u32) {}

    pub fn restore(_baseline: &ConsoleBaseline) {}
}

#[cfg(not(windows))]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_and_restore_is_safe_and_idempotent() {
        let baseline = capture();
        enable_vt_output();
        restore(&baseline);
        restore(&baseline);
    }
}
