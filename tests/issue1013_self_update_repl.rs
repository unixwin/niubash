//! niubash#1013 — `self-update` in the REPL must NOT exit the REPL when no
//! installer was launched (already up to date, `--check`, `--dry-run`,
//! `--help`, Scoop/Store-managed, or error).

#[path = "interactive/driver.rs"]
mod driver;

use driver::{require_pty_or_skip, NiuSession};
use std::process::Command;

fn niu_bin() -> &'static str {
    env!("CARGO_BIN_EXE_niu")
}

#[test]
fn repl_stays_alive_after_non_installing_self_update_and_preserves_exit_status() {
    if !require_pty_or_skip(
        "repl_stays_alive_after_non_installing_self_update_and_preserves_exit_status",
    ) {
        return;
    }

    let mut session = NiuSession::spawn("issue1013");
    session.wait_ready();

    session.send_line("self-update --help");
    session.expect_prompt();
    std::thread::sleep(std::time::Duration::from_millis(300));
    session.drain_screen();

    session.send_line("echo AFTER_HELP:$?");
    session.expect("AFTER_HELP:0");
    session.expect_prompt();
    std::thread::sleep(std::time::Duration::from_millis(300));
    session.drain_screen();

    session.send_line("self-update --bogus-flag-1013");
    session.expect_prompt();
    std::thread::sleep(std::time::Duration::from_millis(300));
    session.drain_screen();

    session.send_line("echo AFTER_ERR:$?");
    session.expect("AFTER_ERR:1");
    session.expect_prompt();

    session.send_line("exit 0");
    assert_eq!(session.wait_exit(), 0);
}

#[test]
fn cli_and_repl_command_self_update_help_exit_zero() {
    let cli = Command::new(niu_bin())
        .args(["--self-update", "--help"])
        .output()
        .expect("run niu --self-update --help");
    assert!(cli.status.success(), "cli status: {:?}", cli.status);

    let repl_cmd = Command::new(niu_bin())
        .args(["--norc", "-C", "self-update --help"])
        .output()
        .expect("run niu --norc -C 'self-update --help'");
    assert!(
        repl_cmd.status.success(),
        "repl -C status: {:?}",
        repl_cmd.status
    );
}
