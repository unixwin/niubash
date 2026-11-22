#[path = "interactive/driver.rs"]
mod driver;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};

#[test]
fn repro_bind_x_chord_and_alt_and_readline_vars() {
    if !require_pty_or_skip("repro_bind_x_chord_and_alt_and_readline_vars") {
        return;
    }
    let rc = format!(
        "{}\n\
         _zinsert() {{\n\
             echo \"GOT_LINE=[$READLINE_LINE] GOT_POINT=[$READLINE_POINT]\"\n\
             READLINE_LINE=\"echo $READLINE_LINE\"\n\
             READLINE_POINT=5\n\
         }}\n\
         bind -x '\"\\es\": _zinsert'\n",
        driver::default_rc()
    );
    let mut session = NiuSession::spawn_custom("issue1063", &rc, &[], (100, 30), DEFAULT_TIMEOUT);
    session.wait_ready();

    // 1. Register bind -x interactively at the prompt, then trigger \C-x\C-t
    session.send_line(r#"bind -x '"\C-x\C-t":echo OK'"#);
    session.expect_prompt();
    session.send("\u{18}\u{14}");
    session.expect("OK");
    session.expect_prompt();

    // 2. Register inline bind -x reading READLINE_LINE and READLINE_POINT
    session.send_line(r#"bind -x '"\C-x\C-l":echo "L=[$READLINE_LINE] P=[$READLINE_POINT]"'"#);
    session.expect_prompt();
    session.send("hello");
    session.send("\u{18}\u{0c}");
    session.expect("L=[hello] P=[5]");
    session.expect_prompt();
    session.send("\u{15}"); // Ctrl-U clear line

    // 3. Test \es with pre-typed text "world" and cursor movement
    session.send("world");
    session.send("\u{1b}s");
    session.expect("GOT_LINE=[world] GOT_POINT=[5]");
    session.expect_prompt();

    // Verify READLINE_LINE was restored into edit buffer ("echo world")
    session.send_line("");
    session.expect("world");
    session.expect_prompt();

    session.send_line("exit");
    assert_eq!(session.wait_exit(), 0, "{}", session.transcript());
}
