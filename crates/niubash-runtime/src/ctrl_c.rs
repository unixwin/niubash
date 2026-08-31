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
pub fn consume_ctrl_c() -> bool {
    false
}

#[cfg(not(windows))]
pub fn install() {}

/// Run trap hooks for a given signal name.
/// This is called by the shell when a signal is received.
pub fn run_trap_hooks(shell: &mut crate::shell::Shell, signal: &str) {
    match signal {
        "trapint" => {
            shell.run_trapint_hooks();
        }
        "trapwinch" => {
            shell.run_trapwinch_hooks();
        }
        "trapusr1" => {
            shell.run_trapusr1_hooks();
        }
        "trapusr2" => {
            shell.run_trapusr2_hooks();
        }
        "trappipe" => {
            shell.run_trappipe_hooks();
        }
        "trapterm" => {
            shell.run_trapterm_hooks();
        }
        "trapchld" => {
            shell.run_trapchld_hooks();
        }
        "traperr" | "trappzerr" => {
            shell.run_traperr_hooks();
        }
        "trapdebug" => {
            shell.run_trapdebug_hooks();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Shell;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Every dispatched signal, the framework runner it must reach, and the
    /// marker variable that runner exports. `traperr` and `trappzerr`
    /// intentionally share one runner, matching bash.
    const TRAP_DISPATCH: [(&str, &str, &str); 10] = [
        ("trapint", "niubash_run_trapint_hooks", "NIU_TRAP_INT"),
        ("trapwinch", "niubash_run_trapwinch_hooks", "NIU_TRAP_WINCH"),
        ("trapusr1", "niubash_run_trapusr1_hooks", "NIU_TRAP_USR1"),
        ("trapusr2", "niubash_run_trapusr2_hooks", "NIU_TRAP_USR2"),
        ("trappipe", "niubash_run_trappipe_hooks", "NIU_TRAP_PIPE"),
        ("trapterm", "niubash_run_trapterm_hooks", "NIU_TRAP_TERM"),
        ("trapchld", "niubash_run_trapchld_hooks", "NIU_TRAP_CHLD"),
        ("traperr", "niubash_run_traperr_hooks", "NIU_TRAP_ERR"),
        ("trappzerr", "niubash_run_traperr_hooks", "NIU_TRAP_ERR"),
        ("trapdebug", "niubash_run_trapdebug_hooks", "NIU_TRAP_DEBUG"),
    ];

    /// Temp home directory, removed on drop.
    struct TempHome(PathBuf);

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Shell whose primary rc exists but is empty, so the framework-runner
    /// dispatch path is active. The runner functions themselves are defined
    /// directly by the caller rather than through `run_startup_rc`.
    fn shell_with_empty_primary_rc() -> (Shell, TempHome) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!(
            "niubash-ctrlc-trap-{}-{}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".niubashrc"), "").unwrap();

        let home_guard = TempHome(home.clone());
        let mut shell = Shell::new().unwrap();
        shell.home_dir = home;
        (shell, home_guard)
    }

    #[test]
    fn run_trap_hooks_dispatches_all_ten_signals() {
        let _state_lock = crate::test_support::PROCESS_STATE_LOCK.lock().unwrap();
        let (mut shell, _home) = shell_with_empty_primary_rc();

        for (_, runner, marker) in TRAP_DISPATCH {
            shell
                .execute_script(&format!("{runner}() {{\n  export {marker}=1\n}}\n"))
                .unwrap();
        }

        for (signal, _, marker) in TRAP_DISPATCH {
            shell.execute_script(&format!("unset {marker}")).unwrap();
            run_trap_hooks(&mut shell, signal);
            assert_eq!(
                shell.executor.get_env(marker),
                Some("1"),
                "signal {signal} did not reach its framework hook runner"
            );
        }
    }

    #[test]
    fn run_trap_hooks_without_primary_rc_is_a_noop() {
        let _state_lock = crate::test_support::PROCESS_STATE_LOCK.lock().unwrap();
        let (mut shell, _home) = shell_with_empty_primary_rc();
        std::fs::remove_file(shell.home_dir.join(".niubashrc")).unwrap();

        shell
            .execute_script("niubash_run_trapint_hooks() {\n  export NIU_TRAP_INT=1\n}\n")
            .unwrap();
        run_trap_hooks(&mut shell, "trapint");

        assert_eq!(shell.executor.get_env("NIU_TRAP_INT"), None);
    }

    #[test]
    fn run_trap_hooks_ignores_unknown_signal() {
        let _state_lock = crate::test_support::PROCESS_STATE_LOCK.lock().unwrap();
        let (mut shell, _home) = shell_with_empty_primary_rc();

        shell
            .execute_script("niubash_run_trapint_hooks() {\n  export NIU_TRAP_INT=1\n}\n")
            .unwrap();
        run_trap_hooks(&mut shell, "trapsomethingelse");

        assert_eq!(shell.executor.get_env("NIU_TRAP_INT"), None);
    }
}
