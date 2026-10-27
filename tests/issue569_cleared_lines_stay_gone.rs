//! niubash#569 — lines cleared with `history -c` must not survive in the
//! shared history file after the session exits (GNU: a cleared line never
//! reaches the file at exit).
//!
//! Before the fix the live sync had already written the cleared lines to
//! the file when they were typed, and the session-end write re-appended
//! everything it found missing, so the next session read the cleared
//! lines back. The fix tracks this session's pending/cleared lines in a
//! ledger shared with the editor instance and removes exactly the cleared
//! lines from the file at session end — without truncating the file
//! (seeds and other sessions' lines survive).
//!
//! The probe mirrors the audit's h5-on grid: fake HOME + seeds, then
//! `shopt -s histappend`, `echo MARKER_H5`, `history -c`, `history`,
//! `exit`. The final file must hold the seeds plus the post-clear lines
//! only — neither MARKER_H5 nor the `history -c` line itself.

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
        "niu-hist569-{prefix}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn cleared_lines_do_not_survive_session_exit() {
    if !require_pty_or_skip("cleared_lines_do_not_survive_session_exit") {
        return;
    }
    let root = unique_root("h5-on");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".niubash_history"), "SEEDONE\nSEEDTWO\n").unwrap();

    let mut session =
        NiuSession::spawn_shared_home(&root, &default_rc(), &[], &[], (120, 30), DEFAULT_TIMEOUT);
    session.wait_ready();
    for line in [
        "shopt -s histappend",
        "echo MARKER_H5",
        "history -c",
        "history",
        "exit",
    ] {
        session.send_line(line);
        if line != "exit" {
            session.expect_prompt();
        }
    }
    session.wait_exit();

    let contents = std::fs::read_to_string(home.join(".niubash_history")).unwrap();
    assert_eq!(
        contents, "SEEDONE\nSEEDTWO\nhistory\nexit\n",
        "cleared lines (MARKER_H5, the history -c line, the shopt line) \
         must be gone from the file, and only post-clear lines remain"
    );
    assert!(
        !contents.contains("MARKER_H5"),
        "MARKER_H5 must not resurrect:\n{contents}"
    );
    assert!(
        !contents.contains("history -c"),
        "the history -c line itself must not survive:\n{contents}"
    );
}
