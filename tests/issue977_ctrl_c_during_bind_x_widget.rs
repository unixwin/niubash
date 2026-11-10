//! niubash#977 — Ctrl-C during a `bind -x` widget must discard the line.
//!
//! GNU readline remembers a SIGINT caught while a widget runs and aborts
//! the edit line on resume (bashline.c: the caught signal aborts the line
//! when the widget returns). niu lost that half: while the widget runs,
//! the tty is in cooked mode (read_line returned, raw mode off), so a ^C
//! typed in that window is consumed by the line discipline — it aborts
//! the widget (the sleep dies) but the byte never reaches ruline, the
//! repl's Ctrl-C arm never runs, and the partially typed line is handed
//! back intact, so the next `exit` is submitted as `zzexit`.
//!
//! Chain (per test): bind a widget to a key → type a partial line → press
//! the chord → widget starts (RAN marker, intermediate assertion) → Ctrl-C
//! → the line must be discarded → `exit` must exit the session (never
//! `zzexit`). The slow widget (`sleep 2`) makes the cooked window
//! deterministic: the ^C always lands while the widget is still running,
//! so the pin does not depend on repaint/CPR timing (the 2 s-delay shape
//! in the issue's original probe was a cursor-position-reply artifact,
//! not the defect).
//!
//! Sleep-resolution precondition (fail-first adjudication, 2026-11-11):
//! bare `sleep` is NOT a builtin — it is rubash's fast-path emulated
//! external (`command_dispatch_primary.rs`, `PrimarySlot::Sleep`), which
//! stands down to a real spawn whenever the external is unresolvable
//! on PATH (`emulated_external_stands_down`, lookup_paths.rs:102). The
//! driver spawns with a System32-only PATH, so without a PATH repair
//! the slow widget errors `sleep: command not found` (127) at once and
//! the 2 s window never exists — the first fail-first run went 2/5
//! with cells 1/5/6 red for that reason, not for the defect. `rc_for`
//! therefore appends the system dirs to PATH (append, never replace,
//! so the Windows winuxcmd injection survives); with it the widget
//! sleeps a real 2 s (probe: RAN→DONE 2.02 s, no 127). The external
//! twin (`/bin/sleep 2`) needs no PATH.
//!
//! * `ctrl_c_during_slow_widget_discards_line` — the defect cell:
//!   ^C *during* the widget (bare `sleep`, rubash fast-path emulation
//!   = virtual child once it resolves on PATH, see the precondition
//!   above). FAILS on the pre-fix tree (the session submits `zzexit`
//!   and never exits), passes after the fix. Also asserts the GNU
//!   status: `$?` = 1 at the resumed prompt (eval.c:111 DISCARD arm),
//!   not 130.
//! * `ctrl_c_during_slow_external_sleep_widget_discards_line` — the
//!   same defect shape with an external `/bin/sleep` child (unix
//!   only): group delivery kills the real child, the widget unwinds,
//!   the line is discarded, `$?` = 1. Locks the boundary against the
//!   builtin-sleep fold (rubash#1640): at PTY level both spellings
//!   discard — the fold does not reproduce under group delivery.
//! * `ctrl_c_after_widget_returns_discards_line` — control cell: fast
//!   widget, ^C *after* it returns. Green on the base tree; guards the
//!   normal prompt-Ctrl-C path against the fix. The test waits for the
//!   post-return prompt before sending ^C: without that settle the ^C
//!   can land in the widget-return teardown window (tty still cooked,
//!   reedline not yet re-armed) and go red on base through the same
//!   defect — the first fail-first run proved exactly that.
//! * `ctrl_c_without_widget_discards_line` — control cell: no widget at
//!   all, ^C at the prompt discards. Green on the base tree.
//! * `ctrl_c_during_slow_widget_with_trap_keeps_line` — trapped INT:
//!   ^C *during* the widget runs the widget to completion, and the line
//!   is KEPT (the next command fuses with `zz`). Locks "trapped never
//!   discards" against the fix. It does NOT assert the trap runs once:
//!   GNU does, but the base tree never runs it for a SIGINT delivered
//!   during the virtual-child sleep (engine-side gap, prompt and widget
//!   alike — see the NOTE in the test body and the residual draft in
//!   REPORT §6). Green on the base tree for the narrowed contract.
//! * `ctrl_c_during_slow_widget_with_ignored_int_keeps_line` — ignored
//!   INT (`trap '' INT`): ^C is a no-op end to end, the widget runs
//!   to completion, and the line is KEPT. Green on the base tree.
//! * `ignored_int_leaves_status_zero` — the ignored shape's status:
//!   after the ignored ^C the live `$?` is 0 (GNU table), observed by
//!   clearing the kept line with Ctrl-U (no submission, so `$?` is
//!   untouched) and echoing it. Green on the base tree.

#[path = "interactive/driver.rs"]
mod driver;

use driver::{require_pty_or_skip, NiuSession, CTRL_C, DEFAULT_TIMEOUT};

/// rc with one widget bound to Ctrl-G f. `slow` sleeps 2 s so the ^C
/// lands deterministically inside the widget's cooked-mode window;
/// `fast` returns at once so the ^C lands after the widget returns.
/// `setup` runs before the binding (trap declarations); `sleep_cmd`
/// is the widget's sleep spelling — bare `sleep 2` (rubash fast-path
/// emulation, virtual child once it resolves on PATH) or an external
/// `/bin/sleep 2` (real child).
/// The rc appends the system dirs to PATH: the driver spawns with a
/// System32-only PATH, and bare `sleep` is unresolvable there — the
/// widget would error 127 at once and the slow window would never
/// exist (fail-first 2026-11-11). Append, never replace, so the
/// Windows winuxcmd injection survives.
fn rc_for(slow: bool, setup: &str, sleep_cmd: &str) -> String {
    let widget = if slow {
        format!("slow_widget() {{ echo WIDGET977-RAN; {sleep_cmd}; echo WIDGET977-DONE; }}")
    } else {
        "fast_widget() { echo WIDGET977-RAN; }".to_string()
    };
    let name = if slow { "slow_widget" } else { "fast_widget" };
    format!(
        "{}{}\nPATH=\"$PATH:/usr/bin:/bin\"\n{}\nbind -x '\"\\C-gf\": {}'\n",
        driver::default_rc(),
        setup,
        widget,
        name
    )
}

fn spawn(rc: &str) -> NiuSession {
    NiuSession::spawn_custom("issue977", rc, &[], (100, 30), DEFAULT_TIMEOUT)
}

/// The widget ran (RAN marker) but was interrupted before finishing
/// (no DONE marker) — the abort half of the chain.
fn assert_widget_interrupted(session: &NiuSession) {
    let transcript = session.transcript();
    assert!(
        transcript.contains("WIDGET977-RAN"),
        "widget must have started\n{transcript}"
    );
    assert!(
        !transcript.contains("WIDGET977-DONE"),
        "widget must have been interrupted by ^C, not run to completion\n{transcript}"
    );
}

/// The widget ran to completion (both markers) — the keep half of the
/// trapped/ignored chain.
fn assert_widget_completed(session: &NiuSession) {
    let transcript = session.transcript();
    assert!(
        transcript.contains("WIDGET977-RAN"),
        "widget must have started\n{transcript}"
    );
    assert!(
        transcript.contains("WIDGET977-DONE"),
        "widget must have run to completion\n{transcript}"
    );
}

/// `$?` at the current prompt is `expected`, observed through a fresh
/// `echo` line (the line is empty when this runs, so nothing fuses).
fn assert_status(session: &mut NiuSession, expected: u32) {
    session.send_line(&format!("echo \"STATUS=$?\""));
    session.expect(&format!("STATUS={expected}"));
}

/// The edit line is empty: `exit` exits the session (code 0) and the
/// transcript never shows the partial line fused with it (`zzexit`).
fn assert_line_discarded(session: &mut NiuSession) {
    session.send_line("exit");
    assert_eq!(
        session.wait_exit(),
        0,
        "session must exit on a bare `exit` (the line was discarded)\n{}",
        session.transcript()
    );
    assert!(
        !session.transcript().contains("zzexit"),
        "the partial line must not have survived to fuse with `exit`\n{}",
        session.transcript()
    );
}

/// The edit line survived: the next command fuses with the partial
/// line (`zzecho`), proving the line was kept. Waits for the fusion
/// evidence: asserting on the raw transcript right after `send_line`
/// races the shell's reply (round-2 fail-first proved it — the keep
/// half held, the immediate assert did not).
fn assert_line_kept(session: &mut NiuSession) {
    session.send_line("echo \"STATUS=$?\"");
    session.expect("zzecho: command not found");
}

#[test]
fn ctrl_c_during_slow_widget_discards_line() {
    if !require_pty_or_skip("ctrl_c_during_slow_widget_discards_line") {
        return;
    }
    let mut session = spawn(&rc_for(true, "", "sleep 2"));
    session.wait_ready();

    // Partial line, then the chord: the widget starts in the live editor.
    session.send("zz");
    session.send("\u{7}f");
    session.expect("WIDGET977-RAN");

    // ^C lands while the widget is still sleeping (cooked mode): the
    // widget aborts and the line must be discarded on resume.
    session.send(CTRL_C);
    session.expect_prompt();
    assert_widget_interrupted(&session);
    // GNU settles the discard to $? = 1 (eval.c:111 DISCARD arm), not 130.
    assert_status(&mut session, 1);
    assert_line_discarded(&mut session);
}

#[test]
#[cfg(unix)]
fn ctrl_c_during_slow_external_sleep_widget_discards_line() {
    if !require_pty_or_skip("ctrl_c_during_slow_external_sleep_widget_discards_line") {
        return;
    }
    let sleep = if std::path::Path::new("/bin/sleep").exists() {
        "/bin/sleep 2"
    } else {
        "/usr/bin/sleep 2"
    };
    let mut session = spawn(&rc_for(true, "", sleep));
    session.wait_ready();

    session.send("zz");
    session.send("\u{7}f");
    session.expect("WIDGET977-RAN");

    // Group delivery kills the real child: the widget unwinds, the
    // line is discarded, $? = 1 — the same verdict as the builtin
    // spelling, so the rubash#1640 fold does not reproduce here.
    session.send(CTRL_C);
    session.expect_prompt();
    assert_widget_interrupted(&session);
    assert_status(&mut session, 1);
    assert_line_discarded(&mut session);
}

#[test]
fn ctrl_c_after_widget_returns_discards_line() {
    if !require_pty_or_skip("ctrl_c_after_widget_returns_discards_line") {
        return;
    }
    let mut session = spawn(&rc_for(false, "", "sleep 2"));
    session.wait_ready();

    session.send("zz");
    session.send("\u{7}f");
    session.expect("WIDGET977-RAN");

    // The widget already returned; ^C at the resumed prompt discards
    // the line exactly as it does with no widget at all. Wait for the
    // post-return prompt first: sending ^C the instant RAN appears can
    // land it in the widget-return teardown window (tty still cooked,
    // reedline not yet re-armed), where the same defect eats it and
    // the line survives — the first fail-first run went red on base
    // exactly that way.
    session.expect_prompt();
    session.send(CTRL_C);
    session.expect_prompt();
    assert_line_discarded(&mut session);
}

#[test]
fn ctrl_c_without_widget_discards_line() {
    if !require_pty_or_skip("ctrl_c_without_widget_discards_line") {
        return;
    }
    let mut session = spawn(&rc_for(true, "", "sleep 2"));
    session.wait_ready();

    // No widget involved: ^C at the prompt discards the partial line.
    session.send("zz");
    session.send(CTRL_C);
    session.expect_prompt();
    assert_line_discarded(&mut session);
}

#[test]
fn ctrl_c_during_slow_widget_with_trap_keeps_line() {
    if !require_pty_or_skip("ctrl_c_during_slow_widget_with_trap_keeps_line") {
        return;
    }
    let mut session = spawn(&rc_for(true, "trap 'echo TRAPPED977' INT", "sleep 2"));
    session.wait_ready();

    session.send("zz");
    session.send("\u{7}f");
    session.expect("WIDGET977-RAN");

    // ^C during the widget: the widget runs to completion, and the
    // line is KEPT.
    //
    // NOTE (residual, not asserted here): GNU runs the INT trap exactly
    // once in this shape (REPORT §3 oracle), but on the base tree the
    // trap NEVER runs for a SIGINT delivered during the virtual-child
    // sleep — at the prompt as well as in a widget (direct PTY probes,
    // `probes/`): the trap is installed (`trap -p` shows it) and fires
    // exactly once for ^C at the bare prompt, yet stays at 0 through
    // the sleep, a +3 s settle, and a subsequent command. That is an
    // engine-side trap-delivery gap in `sleep_interruptibly`'s trapped
    // path, outside this host PR's scope (`interrupted` is structurally
    // false for trapped, so no host change can heal it) — it must NOT
    // be pinned here (pinning count 0 would fossilize a GNU
    // divergence), and it must NOT block this PR. The residual is
    // drafted for the captain in REPORT §6. This cell locks only the
    // host contract: trapped never discards.
    session.send(CTRL_C);
    session.expect_prompt();
    assert_widget_completed(&session);
    assert_line_kept(&mut session);
    assert_line_discarded(&mut session);
}

#[test]
fn ctrl_c_during_slow_widget_with_ignored_int_keeps_line() {
    if !require_pty_or_skip("ctrl_c_during_slow_widget_with_ignored_int_keeps_line") {
        return;
    }
    let mut session = spawn(&rc_for(true, "trap '' INT", "sleep 2"));
    session.wait_ready();

    session.send("zz");
    session.send("\u{7}f");
    session.expect("WIDGET977-RAN");

    // ^C during the widget is a no-op end to end: the widget runs to
    // completion and the line is KEPT.
    session.send(CTRL_C);
    session.expect_prompt();
    assert_widget_completed(&session);
    assert_line_kept(&mut session);
    assert_line_discarded(&mut session);
}

#[test]
fn ignored_int_leaves_status_zero() {
    if !require_pty_or_skip("ignored_int_leaves_status_zero") {
        return;
    }
    let mut session = spawn(&rc_for(true, "trap '' INT", "sleep 2"));
    session.wait_ready();

    session.send("zz");
    session.send("\u{7}f");
    session.expect("WIDGET977-RAN");

    // ^C during the widget is ignored; the widget completes and the
    // line is kept. Clear it with Ctrl-U (no submission, `$?`
    // untouched) and read the live status: GNU gives 0.
    session.send(CTRL_C);
    session.expect_prompt();
    assert_widget_completed(&session);
    session.send("\u{15}");
    assert_status(&mut session, 0);
    assert_line_discarded(&mut session);
}
