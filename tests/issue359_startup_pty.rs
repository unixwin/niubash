#[path = "interactive/driver.rs"]
mod driver;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};

#[test]
fn restricted_repl_defers_until_rc_and_restricts_prompt_command_and_body() {
    if !require_pty_or_skip("restricted-repl-startup") {
        return;
    }
    for args in [&["-r"][..], &["-r", "--noediting"][..]] {
        let mut session = NiuSession::spawn_custom_with_args(
            "restricted-repl-startup",
            "PS1='P1> '\nPS2='P2> '\ncd \"$HOME\"\nPATH=restricted-startup-path\nSTARTUP_COUNT=$(( ${STARTUP_COUNT:-0} + 1 ))\nPROMPT_COMMAND='cd .; printf \"precmd-status:%s\\n\" \"$?\"'\n",
            args,
            &[],
            (120, 30),
            DEFAULT_TIMEOUT,
        );
        session.expect("precmd-status:1");
        session.expect("P1>");
        session.send_line("printf 'body:%s:%s\\n' \"$STARTUP_COUNT\" \"$PATH\"");
        session.expect("body:1:restricted-startup-path");
        session.expect("P1>");
        session.send_line("cd .");
        session.expect("restricted");
        session.expect("P1>");
        session.send_line("PATH=body-path");
        session.expect("PATH: readonly variable");
        session.expect("P1>");
        session.send_line("exit");
        assert_eq!(session.wait_exit(), 0);
    }
}

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
