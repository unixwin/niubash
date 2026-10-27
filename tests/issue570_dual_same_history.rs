//! niubash#570 — two interleaved sessions typing the identical line must
//! leave two copies in the shared history file (GNU positional semantics).
//!
//! Before the fix the second session's save saw the first session's line
//! as its last entry and reedline's consecutive-duplicate filter dropped
//! the second copy, so the file held one `echo SAMECMD` where GNU holds
//! two. The fix appends a line equal to the last entry to the file
//! directly and rebuilds the reader instead of routing it through
//! reedline's filter.
//!
//! Both sessions run over one staged home (the audit's dual-probe
//! morphology): the file is read only after both sessions have exited,
//! so neither session's drop can remove it first.

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
        "niu-hist570-{prefix}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn dual_sessions_keep_both_copies_of_the_same_line() {
    if !require_pty_or_skip("dual_sessions_keep_both_copies_of_the_same_line") {
        return;
    }
    let root = unique_root("dual-same");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".niubash_history"), "SEEDONE\nSEEDTWO\n").unwrap();

    // Both sessions share this root (and therefore this history file).
    // The second spawn re-stages the same rc and re-creates the same dirs;
    // neither drop runs before the file is read below.
    let mut a =
        NiuSession::spawn_shared_home(&root, &default_rc(), &[], &[], (120, 30), DEFAULT_TIMEOUT);
    let mut b =
        NiuSession::spawn_shared_home(&root, &default_rc(), &[], &[], (120, 30), DEFAULT_TIMEOUT);
    a.wait_ready();
    b.wait_ready();

    // A first, then B: B's save refreshes from the file, sees A's line as
    // its last entry, and must still append its own copy.
    a.send_line("echo SAMECMD");
    a.expect_prompt();
    b.send_line("echo SAMECMD");
    b.expect_prompt();
    a.send_line("exit");
    a.wait_exit();
    b.send_line("exit");
    b.wait_exit();

    let contents = std::fs::read_to_string(home.join(".niubash_history")).unwrap();
    let count = contents.lines().filter(|l| *l == "echo SAMECMD").count();
    assert_eq!(
        count, 2,
        "the shared file must hold both sessions' copies of the line:\n{contents}"
    );
    assert_eq!(
        contents.lines().filter(|l| *l == "SEEDONE").count(),
        1,
        "seed lines must not duplicate:\n{contents}"
    );
    assert_eq!(
        contents.lines().filter(|l| *l == "exit").count(),
        2,
        "both sessions' exit lines must be recorded:\n{contents}"
    );
}
