#![cfg(windows)]

//! niubash#537 regression: the completion menu must never be live when the
//! user did not leave an activate pending — the "phantom menu" case.
//!
//! Root cause: `GnuMenu::menu_event` queued engine events in a single slot.
//! reedline repaints once per input batch, so a TAB and a typed char
//! landing in the same batch queued `Activate` and `Edit`; the `Edit`
//! overwrote the unprocessed `Activate` *after* `handle_menu_event` had
//! already flipped `is_active` true at dispatch. The phase machine never
//! ran, `is_active` stayed true, and the engine routed the next Enter to
//! `MenuAccept` — swallowed whole under the GNU style (its
//! `replace_in_buffer` is a deliberate no-op) or a stale-candidate commit
//! under the interactive styles. Fix: the queue is FIFO-drained in
//! `update_working_details`, and `apply_edit` releases `active` whenever
//! the phase machine is not `Listed`.
//!
//! Driver: [`mod driver`] (portable-pty ConPTY; see tests/interactive.rs
//! for the harness contract).

#[path = "interactive/driver.rs"]
mod driver;

use std::time::Duration;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT, TAB};

const ABSENT_WINDOW: Duration = Duration::from_millis(800);

fn spawn_style(prefix: &str, style: &str) -> NiuSession {
    NiuSession::spawn_custom(
        prefix,
        &driver::default_rc(),
        &[("NIU_COMPLETION_STYLE".to_string(), style.to_string())],
        (120, 30),
        DEFAULT_TIMEOUT,
    )
}

/// `cat m4<TAB>` splices `m4-` and leaves the ambiguous pair as the menu's
/// values; `x` changes the word; a `\ty` burst then races a second Activate
/// against an Edit in one batch. Before the fix the Edit overwrote the
/// pending Activate — `is_active` stayed true with the stale `[one,two]`
/// values — so the Enter died inside a no-op `MenuAccept` instead of
/// submitting `cat m4-xy` (which must fail as a missing file, proving the
/// line ran verbatim). After the fix the batch drains Activate then Edit:
/// no candidates for `m4-xy`, menu fully reset, Enter submits.
#[test]
fn raced_tab_edit_batch_leaves_no_phantom_menu() {
    if !require_pty_or_skip("raced_tab_edit_batch_leaves_no_phantom_menu") {
        return;
    }
    // The phantom-menu defect lives in GnuMenu's event queue (the menu
    // niubash owns). The reedline-native styles keep their own shape.
    let style = "gnu";
    {
        let mut s = spawn_style(&format!("phantom-{style}"), style);
        std::fs::write(s.start().join("m4-one.txt"), "m4-body\n").unwrap();
        std::fs::write(s.start().join("m4-two.txt"), "m4-body\n").unwrap();
        s.wait_ready();
        s.send("cat m4");
        s.send(TAB);
        // Let the first attempt paint: `cat m4-` spliced, attempt anchored.
        // (No expect needle: the highlighted buffer wraps `m4` in ANSI so
        // the spliced dash is not a contiguous "m4-".)
        std::thread::sleep(Duration::from_millis(700));
        s.send("x");
        // The raced batch: a second TAB's Activate and the typed `y`'s
        // Edit in one write — the phantom-menu trigger.
        s.send("\ty");
        // Execution-level assertion (echo-proof): `cat m4-xy` running
        // verbatim prints the engine's No-such-file line — a swallowed
        // Enter (MenuAccept over the stale values) produces nothing, and a
        // committed candidate would print `m4-body` instead.
        s.send("\r");
        s.expect_absent("m4-body", ABSENT_WINDOW);
        // `cat m4-xy` executes verbatim → the engine's No-such-file line.
        s.expect("No such file");
        s.expect_prompt();
    }
}

/// The trailing-slash repro from niubash#537 verbatim: a fresh line,
/// `cd projects/` typed with NO TAB at all, Enter. The menu must not be
/// open (nothing summoned it), Enter executes `cd projects/` verbatim —
/// the marker is the changed sentinel cwd visible in `pwd` output.
#[test]
fn trailing_slash_without_tab_opens_no_menu() {
    if !require_pty_or_skip("trailing_slash_without_tab_opens_no_menu") {
        return;
    }
    for style in ["gnu", "column", "list"] {
        let mut s = spawn_style(&format!("slash-{style}"), style);
        std::fs::create_dir_all(s.start().join("projects/repro1023")).unwrap();
        std::fs::create_dir_all(s.start().join("projects/repro2048")).unwrap();
        s.wait_ready();
        for ch in "cd projects/".chars() {
            s.send(&ch.to_string());
            std::thread::sleep(Duration::from_millis(60));
        }
        s.send("\r");
        s.send_line("pwd");
        // The executed cwd is `…/start/projects` — Enter ran the line, not
        // a committed candidate (`…/projects/repro1023` would mean the
        // first candidate was spliced and executed).
        s.expect("start/projects");
        s.expect_absent("repro1023", ABSENT_WINDOW);
        s.expect_prompt();
    }
}
