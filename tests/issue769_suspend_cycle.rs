//! niubash#769: interactive C-z must stop the shell, and `fg` must
//! resume it — the GNU readline `rl_stop` contract, pinned end to end
//! on a real terminal.
//!
//! Harness shape (the reason this test nests): a shell spawned
//! directly by the test driver is a session leader alone in its
//! process group, and that group is ORPHANED — no member has a parent
//! in a different group of the same session — so the kernel discards
//! SIGTSTP/SIGTTIN/SIGTTOU for the whole group and NO shell can stop
//! there (control-verified against GNU bash 5.3.20 in the same
//! harness: it stays `S` too, even on an external SIGTSTP). The
//! observable contract needs a job-control parent, so the pin runs a
//! second niu as a foreground job of the driven session: the parent
//! places the job in its own process group and hands it the terminal
//! (the engine's job-control spawn treatment), the inner shell's
//! editor raises SIGTSTP on C-z, the kernel stops it, and the
//! parent's `fg` continues it.
//!
//! Assertions are process-state based (Linux `/proc`), not screen
//! scraping: the stop is a kernel fact, not a paint shape.
//!
//! Driver: [`mod driver`] (portable-pty; see tests/interactive.rs for
//! the harness contract). Run:
//! `NIU_INTERACTIVE_TESTS=1 cargo test --test issue769_suspend_cycle --locked -- --nocapture`

#![cfg(unix)]

#[path = "interactive/driver.rs"]
mod driver;

use std::time::{Duration, Instant};

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};

const CTRL_Z: &str = "\u{1a}";

/// Pids of live processes whose argv[0] is exactly `exe` AND whose
/// environment carries this session's sandbox `HOME` (other test
/// binaries drive niu sessions of their own on a shared machine —
/// the sandbox home is what makes a process ours), as (pid, ppid)
/// pairs.
fn niu_processes(exe: &std::path::Path, home: &std::path::Path) -> Vec<(u32, u32)> {
    let home_entry = format!("HOME={}", home.display()).into_bytes();
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or(&[]);
        if argv0 != exe.as_os_str().as_encoded_bytes() {
            continue;
        }
        let Ok(environ) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        if !environ
            .split(|b| *b == 0)
            .any(|kv| kv == home_entry.as_slice())
        {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // stat: pid (comm) state ppid ... — comm may contain spaces,
        // so the ppid is the second field after the closing paren.
        let Some((_, rest)) = stat.rsplit_once(") ") else {
            continue;
        };
        let mut fields = rest.split(' ');
        let _state = fields.next();
        if let Some(ppid) = fields.next().and_then(|f| f.parse::<u32>().ok()) {
            out.push((pid, ppid));
        }
    }
    out
}

/// The single-letter process state from `/proc/<pid>/stat`.
fn process_state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(") ")?;
    rest.chars().next()
}

/// Poll `cond` every 50 ms until it holds or `timeout` elapses.
fn wait_for(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            eprintln!("timed out waiting for {what}");
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn issue769_pty_ctrl_z_stops_and_fg_resumes_nested_shell() {
    if !require_pty_or_skip("issue769_pty_ctrl_z_stops_and_fg_resumes_nested_shell") {
        return;
    }
    let exe = driver::niu_binary();
    let mut s = NiuSession::spawn("suspend-769");
    s.wait_ready();

    // The driven session is the only niu so far: the job-control parent.
    let parents = niu_processes(&exe, s.home());
    assert_eq!(
        parents.len(),
        1,
        "expected exactly the driven session in /proc, got {parents:?}"
    );
    let outer_pid = parents[0].0;

    // A second niu as a foreground job of the first.
    s.send_line(&format!("{} --noprofile -i", exe.display()));
    s.expect("Niubash");
    s.expect_prompt();
    let inner_pid = {
        let mut found = None;
        let ok = wait_for("the nested shell to appear", DEFAULT_TIMEOUT, || {
            found = niu_processes(&exe, s.home())
                .into_iter()
                .find(|(_, ppid)| *ppid == outer_pid)
                .map(|(pid, _)| pid);
            found.is_some()
        });
        assert!(ok, "the nested niu never appeared as the session's child");
        found.expect("set by the successful wait")
    };

    // C-z in the nested shell's editor: the kernel must stop it.
    s.send(CTRL_Z);
    let stopped = wait_for("the nested shell to reach state T", DEFAULT_TIMEOUT, || {
        process_state(inner_pid) == Some('T')
    });
    assert!(
        stopped,
        "C-z must stop the nested shell (state T); last state: {:?}\n{}",
        process_state(inner_pid),
        s.transcript()
    );
    // The parent reports the stopped job before its next prompt.
    s.expect("Stopped");
    s.expect_prompt();

    // fg in the parent: SIGCONT + terminal hand-back; the nested
    // editor repaints and takes input again.
    s.send_line("fg");
    let resumed = wait_for("the nested shell to leave state T", DEFAULT_TIMEOUT, || {
        matches!(process_state(inner_pid), Some('S') | Some('R'))
    });
    assert!(
        resumed,
        "fg must resume the nested shell; last state: {:?}\n{}",
        process_state(inner_pid),
        s.transcript()
    );
    s.send_line("echo resumed-ok");
    s.expect("resumed-ok");
    s.expect_prompt();

    // Leave both shells cleanly.
    s.send_line("exit");
    s.expect_prompt();
    s.send_line("exit");
    assert_eq!(s.wait_exit(), 0);
}
