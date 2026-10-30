//! niubash#642 — EVERY REPL exit path must run the session-end
//! history reconciliation: the EXIT trap plus the flush that removes
//! the lines this session cleared — the session's own lines AND the
//! base lines it loaded at startup (niubash#569 covered only the own
//! lines, and only on the paths that reached the flush at all).
//!
//! Before the fix (RC audit D-02, PTY probes): the bare-`exit` path
//! flushed but left cleared base lines in the file, so the next
//! session loaded lines the user had cleared; the PROMPT_COMMAND
//! exit-jump path and the startup-rc/self-update paths skipped the
//! flush entirely (self-update skipped the EXIT trap too).
//!
//! Two cells here: the bare `exit` path and the precmd
//! (PROMPT_COMMAND) exit-jump path, both over a seeded history file,
//! both with an EXIT trap marker. The self-update path shares
//! `end_repl_session` with the bare-exit cell but is not driven here:
//! it spawns a real self-update child, which is neither hermetic nor
//! safe in CI. The startup-rc path shares the same funnel and is
//! covered by the unit-level flush pins.

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
        "niu-hist642-{prefix}-{}-{nanos}",
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

fn run(session: &mut NiuSession, line: &str) {
    session.send_line(line);
    session.expect_prompt();
    // See tests/issue614_cleared_list_stays_cleared.rs for why the
    // settle + drain is needed between lines.
    std::thread::sleep(std::time::Duration::from_millis(400));
    session.drain_screen();
}

#[test]
fn bare_exit_reconciles_cleared_base_lines() {
    if !require_pty_or_skip("bare_exit_reconciles_cleared_base_lines") {
        return;
    }
    let (mut session, home) = spawn_seeded("bare");

    run(&mut session, "history -c");
    run(&mut session, "echo probeline");
    session.send_line("exit");
    let code = session.wait_exit();
    assert_eq!(code, 0, "bare exit keeps status 0");

    assert!(
        home.join("trapmark.txt").exists(),
        "the EXIT trap must run on the bare-exit path"
    );
    let contents = std::fs::read_to_string(home.join(".niubash_history")).unwrap();
    assert_eq!(
        contents, "echo probeline\nexit\n",
        "cleared seeds and the cleared -c line are gone; post-clear lines stay"
    );
}

#[test]
fn precmd_exit_reconciles_cleared_base_lines() {
    if !require_pty_or_skip("precmd_exit_reconciles_cleared_base_lines") {
        return;
    }
    let (mut session, home) = spawn_seeded("precmd");

    run(&mut session, "echo pre1");
    run(&mut session, "history -c");
    run(&mut session, "echo post1");
    // Arming the PC exit ends the session at the NEXT prompt cycle's
    // precmd pass — no further prompt is rendered, so there is no
    // prompt to expect after this line; wait_exit is the sync point
    // (the exit can only happen if the assignment executed and the
    // precmd consumed its jump).
    session.send_line("PROMPT_COMMAND='exit 3'");
    std::thread::sleep(std::time::Duration::from_millis(400));
    session.drain_screen();
    let code = session.wait_exit();
    assert_eq!(code, 3, "the precmd jump's status is the session status");

    assert!(
        home.join("trapmark.txt").exists(),
        "the EXIT trap must run on the precmd-exit path"
    );
    let contents = std::fs::read_to_string(home.join(".niubash_history")).unwrap();
    assert_eq!(
        contents, "echo post1\nPROMPT_COMMAND='exit 3'\n",
        "everything up to the clear (seeds included) is removed; \
         post-clear lines stay"
    );
}
