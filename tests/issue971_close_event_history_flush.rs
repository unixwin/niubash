//! niubash#971 — closing the Windows Terminal tab must flush the
//! session history. On Windows the tab close is a CTRL_CLOSE_EVENT;
//! the console handler used to return 0 for it, so the default
//! handler killed the process before `end_repl_session` ran and the
//! whole session's history was lost. The fix routes the event to the
//! session-end funnel (EXIT trap + history flush) on the main thread.
//!
//! The Windows event cannot be driven from this Linux host, so the
//! chain cell below drives the unix analogue — SIGHUP to the session
//! leader — which exercises the same session-end funnel (history
//! flush + EXIT trap) through a real signal-driven exit. GNU saves
//! history and runs the EXIT trap on SIGHUP, then dies by the signal
//! (bash 5.3 sig.c `termsig_handler`: `maybe_save_shell_history`,
//! then `run_exit_trap`, then `kill_shell` re-raises the signal).
//! The Windows end-to-end (ConPTY tab close) is a MANUAL item in the
//! lane REPORT. The classification + handler-state legs are unit
//! pins in ctrl_c.rs; the fallback-flush leg is a unit pin in
//! history.rs.
//!
//! Unix-only: the chain cell drives the session leader by SIGHUP,
//! which has no Windows `libc` spelling (`kill`/`SIGHUP` do not
//! exist in the Windows `libc` crate). Windows coverage is the
//! ConPTY MANUAL item plus the ctrl_c/history unit pins.
#![cfg(unix)]

#[path = "interactive/driver.rs"]
mod driver;

use driver::{default_rc, require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_root(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "niu-hist971-{prefix}-{}-{nanos}",
        std::process::id()
    ))
}

fn spawn_seeded(prefix: &str) -> (NiuSession, PathBuf) {
    let root = unique_root(prefix);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".niubash_history"), "seedA\nseedB\n").unwrap();
    let rc = format!(
        "{}\ntrap 'echo EXITTRAP >> \"$HOME/trapmark.txt\"' EXIT\n",
        default_rc()
    );
    let mut session =
        NiuSession::spawn_shared_home(&root, &rc, &[], &[], (120, 30), DEFAULT_TIMEOUT);
    session.wait_ready();
    (session, home)
}

#[test]
fn issue971_sighup_flushes_session_history_and_runs_exit_trap() {
    if !require_pty_or_skip("issue971_sighup") {
        return;
    }
    let (mut session, home) = spawn_seeded("sighup");
    session.send_line("echo SIGHUP_MARKER_971");
    session.expect_prompt();
    // See tests/issue614_cleared_list_stays_cleared.rs for why the
    // settle + drain is needed between lines.
    std::thread::sleep(std::time::Duration::from_millis(400));
    session.drain_screen();
    // The unix analogue of the Windows tab close: hang up the
    // session leader. If the signal kills the process without the
    // session-end funnel, the marker line and the trap mark are both
    // lost — the #971 shape. If the funnel runs, both land.
    let pid = session.child_pid().expect("spawned shell has a pid");
    let rc = unsafe { libc::kill(pid as i32, libc::SIGHUP) };
    assert_eq!(rc, 0, "kill(SIGHUP) to pid {pid}");
    // The session must end (a hang here is itself the failure: the
    // signal was ignored or swallowed instead of ending the session).
    let _code = session.wait_exit();
    let history = std::fs::read_to_string(home.join(".niubash_history"))
        .expect("history file exists after the session ends");
    assert!(
        history.contains("echo SIGHUP_MARKER_971"),
        "the signal-driven exit flushed the session line: {history:?}"
    );
    assert!(
        history.contains("seedA") && history.contains("seedB"),
        "the flush kept the seeded base lines: {history:?}"
    );
    let trapmark = std::fs::read_to_string(home.join("trapmark.txt"))
        .expect("trap mark file exists after the session ends");
    assert!(
        trapmark.contains("EXITTRAP"),
        "the signal-driven exit ran the EXIT trap: {trapmark:?}"
    );
}
