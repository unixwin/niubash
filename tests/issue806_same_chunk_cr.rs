//! niubash#806 R2 regression: the line editor dropped the SAME-CHUNK
//! trailing CR — one pty write of `cmd\r\n` left the command parked in
//! the edit buffer (it executed only when some LATER Enter flushed it),
//! while the split shape (text, pause, lone `\r`) always submitted.
//!
//! t4 evidence (`D:\repo\nbbuild\evidence\pre-1.5.0\t4\`, repro R2):
//! pywinpty `write("echo ok\r\n")` → 0/5 executed in a 0.5 s/2 s window;
//! `write("echo ok")` + 0.7 s + `write("\r")` → 5/5. Real-world impact:
//! a pasted command followed immediately by Enter (and every
//! send_line-style automation driver that writes text + CRLF as ONE
//! write) could silently not run.
//!
//! GNU oracle (WSL GNU Bash 5.3.20, `wsl python3` PTY probe, script
//! file): the same single write EXECUTES — the CR accepts the line and
//! the LF is a second, empty accept (`$ echo ok\r\nok\r\n$ \r\n$ `).
//! The niubash fix drops the consumed LF (no second prompt cycle); the
//! EXECUTION is the pinned behavior here.
//!
//! Root cause: `PasteChunkEditMode::enter_opens_chunk`
//! (crates/niubash-runtime/src/repl.rs) classified any chunk-opening CR
//! with its LF partner queued behind it as paste data — but a real
//! paste's first break is followed by MORE BURST BODY, while a single
//! machine-fed line is the pair and silence. The fix takes that second
//! look (`paste_body_behind`, niubash#806): body behind → chunk (paste
//! behavior unchanged); silence → the CR submits (GNU typeahead, the
//! niubash#420 precedent).
//!
//! Pins (Windows ConPTY path; the Unix bracketed-paste path is pinned by
//! `tests/interactive.rs::multiline_bracketed_paste_buffers_whole_chunk`):
//!
//! * single write `echo $R2A\r\n` — executes with NO follow-up key;
//! * split write (text, pause, `\r`) — still executes (control);
//! * multi-line single write (`\r\n` per break) — still buffers as one
//!   quiet chunk, nothing executes until the user's Enter (the
//!   niubash#202 paste contract, re-pinned here for the same fix's
//!   blast radius).

#[path = "interactive/driver.rs"]
mod driver;

use std::time::Duration;

use driver::{require_pty_or_skip, NiuSession, HEAVY_TIMEOUT};

/// Window in which a parked (unexecuted) line must NOT have produced its
/// marker — mirrors the ABSENT_WINDOW convention of tests/interactive.rs.
const ABSENT_WINDOW: Duration = Duration::from_millis(800);

/// Settle pause between the text write and the lone CR of the split
/// control — past the paste burst gap, like the t4 driver's 0.7 s.
const SPLIT_PAUSE: Duration = Duration::from_millis(700);

/// niubash#806 R2 itself: `write("cmd\r\n")` as ONE write executes
/// without any follow-up keystroke. Markers are rc variables — only
/// EXECUTED output can produce them, never the echoed edit buffer.
#[test]
fn single_write_crlf_executes_806() {
    if !cfg!(windows) {
        // The chunk-open body probe is the Windows paste-chunk path; on
        // Unix the tty translates CR and the write executes per line
        // through the ordinary path (see the module doc).
        return;
    }
    if !require_pty_or_skip("single_write_crlf_executes_806") {
        return;
    }
    let rc = format!("{}\nR2A=R2A-DONE-806\n", driver::default_rc());
    let mut s = NiuSession::spawn_custom("r2-806-single", &rc, &[], (120, 30), HEAVY_TIMEOUT);
    s.wait_ready();
    // The failing shape, verbatim: text + CRLF in one write, then nothing.
    s.send("echo $R2A\r\n");
    s.expect("R2A-DONE-806");
    s.expect_prompt();
}

/// Control leg (worked before the fix, must keep working): the split
/// write — text, a pause past the burst gap, then a lone CR — submits.
#[test]
fn split_write_cr_executes_806() {
    if !cfg!(windows) {
        return;
    }
    if !require_pty_or_skip("split_write_cr_executes_806") {
        return;
    }
    let rc = format!("{}\nR2B=R2B-DONE-806\n", driver::default_rc());
    let mut s = NiuSession::spawn_custom("r2-806-split", &rc, &[], (120, 30), HEAVY_TIMEOUT);
    s.wait_ready();
    s.send("echo $R2B");
    std::thread::sleep(SPLIT_PAUSE);
    s.send("\r");
    s.expect("R2B-DONE-806");
    s.expect_prompt();
}

/// Paste guard (the niubash#202 contract this fix must not break): a
/// MULTI-line CRLF write buffers as one quiet chunk — body is queued
/// behind every pair, so each chunk-opening CR sees paste evidence — and
/// nothing executes until the user's own Enter.
#[test]
fn multiline_crlf_write_still_buffers_806() {
    if !cfg!(windows) {
        return;
    }
    if !require_pty_or_skip("multiline_crlf_write_still_buffers_806") {
        return;
    }
    let rc = format!(
        "{}\nR2C=R2C-DONE-806\nR2D=R2D-DONE-806\n",
        driver::default_rc()
    );
    let mut s = NiuSession::spawn_custom("r2-806-paste", &rc, &[], (120, 30), HEAVY_TIMEOUT);
    s.wait_ready();
    s.send("echo $R2C\r\necho $R2D\r\n");
    s.expect_absent("R2C-DONE-806", ABSENT_WINDOW);
    s.expect_absent("R2D-DONE-806", ABSENT_WINDOW);
    s.send("\r");
    s.expect("R2C-DONE-806");
    s.expect("R2D-DONE-806");
    s.expect_prompt();
}
