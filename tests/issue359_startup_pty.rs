#[path = "interactive/driver.rs"]
mod driver;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};

#[test]
fn fresh_interactive_startup_does_not_run_setup_or_create_rc() {
    if !require_pty_or_skip("issue359-fresh-startup") {
        return;
    }
    let mut session = NiuSession::spawn_cli(
        "issue359-fresh-startup",
        &[],
        &[("PS1".into(), "P1> ".into())],
        &[],
        (120, 30),
        DEFAULT_TIMEOUT,
    );
    session.wait_ready();
    assert!(!session.home().join(".niubashrc").exists());
    assert!(!session.home().join(".niubash/.setup-done").exists());
    session.send_line("exit");
    assert_eq!(session.wait_exit(), 0);
    assert!(!session.home().join(".niubashrc").exists());
    assert!(!session.home().join(".niubash/.setup-done").exists());
}
