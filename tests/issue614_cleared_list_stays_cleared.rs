//! niubash#614 — the in-session `history` list must stay cleared after
//! `history -c`; only the file face was fixed by #569.
//!
//! Before the fix, -c emptied the reader, but the file deliberately
//! keeps the cleared lines until the session-end write (#569), and
//! reedline's refresh rebuilds the reader from that file on the next
//! query — so the first `history` after any later command listed the
//! whole pre-clear list again. GNU bash 5.3.20 (pty probe, same grid)
//! lists only the post-clear lines, numbered from 1.
//!
//! The fix records a view generation in the session ledger shared by
//! the provider and editor instances: -c hides the raw prefix that
//! predates it from list queries, and an explicit `history -r` ends
//! the hide (GNU loads the file back into the list on purpose).
//!
//! The list is asserted through `history > file` redirects, not the
//! pty stream: the stream carries editor repaints in which consuming
//! matches land unpredictably (see tests/interactive.rs), while the
//! redirected list is the builtin's exact output.

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
        "niu-hist614-{prefix}-{}-{nanos}",
        std::process::id()
    ))
}

fn spawn_seeded(prefix: &str) -> (NiuSession, PathBuf) {
    let root = unique_root(prefix);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".niubash_history"), "SEEDONE\nSEEDTWO\n").unwrap();
    let mut session =
        NiuSession::spawn_shared_home(&root, &default_rc(), &[], &[], (120, 30), DEFAULT_TIMEOUT);
    session.wait_ready();
    (session, home)
}

fn run(session: &mut NiuSession, line: &str) {
    session.send_line(line);
    session.expect_prompt();
    // The prompt is repainted several times per cycle and a consuming
    // match can land in an intermediate repaint; without settling, the
    // next line's bytes race the editor and can execute out of order
    // (the tests/interactive.rs settle_and_drain pattern).
    std::thread::sleep(std::time::Duration::from_millis(400));
    session.drain_screen();
}

#[test]
fn cleared_lines_stay_cleared_in_the_session_list() {
    if !require_pty_or_skip("cleared_lines_stay_cleared_in_the_session_list") {
        return;
    }
    let (mut session, home) = spawn_seeded("list");

    run(&mut session, "echo MARKER614");
    run(&mut session, "history -c");
    run(&mut session, "echo AFTER614");
    run(
        &mut session,
        &format!("history > {}", home.join("list-after-clear.txt").display()),
    );
    session.send_line("exit");
    session.wait_exit();

    let listing = std::fs::read_to_string(home.join("list-after-clear.txt")).unwrap();
    assert!(
        listing.contains("AFTER614"),
        "the post-clear line must be listed:\n{listing}"
    );
    assert!(
        !listing.contains("MARKER614"),
        "the cleared line must not resurrect in the list:\n{listing}"
    );
    assert!(
        !listing.contains("SEEDONE"),
        "the cleared seeds must not resurrect in the list:\n{listing}"
    );
    assert!(
        !listing.contains("history -c"),
        "the -c line itself was cleared too:\n{listing}"
    );

    // The file face (#569, extended by #642): the session-end write
    // removes every cleared line — the session's own lines AND the
    // seeds it loaded at startup — and keeps the post-clear lines.
    // The `history > ...` line replaces the bare `history` of the #569
    // grid as the recorded post-clear list command.
    let contents = std::fs::read_to_string(home.join(".niubash_history")).unwrap();
    assert!(
        !contents.contains("SEEDONE") && !contents.contains("SEEDTWO"),
        "cleared seeds must not survive the session-end write (niubash#642):\n{contents}"
    );
    assert!(
        !contents.contains("MARKER614") && !contents.contains("history -c"),
        "cleared lines must not survive the session-end write:\n{contents}"
    );
    assert!(
        contents.contains("AFTER614") && contents.trim_end().ends_with("exit"),
        "post-clear lines must reach the file:\n{contents}"
    );
}

#[test]
fn reread_after_clear_restores_the_list() {
    if !require_pty_or_skip("reread_after_clear_restores_the_list") {
        return;
    }
    let (mut session, home) = spawn_seeded("reread");

    run(&mut session, "echo MARKER614");
    run(&mut session, "history -c");
    run(
        &mut session,
        &format!("history > {}", home.join("list-cleared.txt").display()),
    );
    // GNU: `history -r` loads the file back into the list on purpose.
    run(&mut session, "history -r");
    run(
        &mut session,
        &format!("history > {}", home.join("list-restored.txt").display()),
    );
    session.send_line("exit");
    session.wait_exit();

    let cleared = std::fs::read_to_string(home.join("list-cleared.txt")).unwrap();
    assert!(
        !cleared.contains("SEEDONE") && !cleared.contains("MARKER614"),
        "the list must be cleared before the re-read:\n{cleared}"
    );
    let restored = std::fs::read_to_string(home.join("list-restored.txt")).unwrap();
    assert!(
        restored.contains("SEEDONE"),
        "the re-read must bring the file lines back:\n{restored}"
    );
    assert!(
        restored.contains("MARKER614"),
        "the re-read must bring the cleared lines back:\n{restored}"
    );
}
