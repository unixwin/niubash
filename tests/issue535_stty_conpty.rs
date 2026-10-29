#![cfg(windows)]

//! niubash#535: `stty` tty queries under a real pseudo console.
//!
//! Root cause (adjudicated 2026-10-29): `stty` inside niu is answered by
//! the rubash engine's hidden console-mode builtin (the name stays out of
//! BUILTIN_NAMES, so `type stty` reports the winuxcmd applet's path while
//! dispatch claims the command in-process whenever `stty` resolves on
//! PATH — rubash#154/#282 design). The builtin's Windows `size` face
//! opened the console output device by a misspelled name ("CONOUT"
//! instead of the reserved "CONOUT$"), so `CreateFileW` failed on every
//! console and the size face reported the canned GNU diagnostic
//! (`stty: 'standard input': Inappropriate ioctl for device`, rc 1) even
//! in a real terminal; the `cols`/`rows` query operands did not exist at
//! all. The mode faces (raw/sane/echo) were never broken — they use the
//! stdin console handle directly.
//!
//! This target drives the built `niu.exe` through the shared ConPTY
//! driver (tests/interactive/driver.rs) at a distinctive geometry
//! (101 cols x 33 rows) and asserts the query faces answer with the
//! console's real geometry and the mode faces still apply cleanly.
//!
//! Gating follows the driver's own rule (`require_pty_or_skip`), like
//! tests/interactive.rs. A first version of this target ran ungated on
//! hosted CI (run 40793320883): the session spawned and the banner
//! expect passed, but the sentinel-prompt expect timed out
//! (driver.rs:767) — the hosted-runner ConPTY session class the gate
//! exists for — and, the readiness wait sitting outside the skip path,
//! the timeout failed the whole workspace job. The legs therefore run
//! on real desktops and wherever `NIU_INTERACTIVE_TESTS=1` forces them;
//! for hosted CI this face stays on the real-machine smoke list. The
//! engine fix itself is gated deterministically on the rubash side
//! (non-terminal diagnostic pins in tests/issue282_* plus the Windows
//! CI build of the changed builtin).

#[path = "interactive/driver.rs"]
mod driver;

use driver::{default_rc, niu_binary, require_pty_or_skip, NiuSession, HEAVY_TIMEOUT};

/// (cols, rows) the pseudo console is created with.
const SIZE: (u16, u16) = (101, 33);

/// The full #535 face set in one session: size/cols/rows report the
/// pseudo console's created geometry; raw/sane mode operands still
/// apply (they were never part of the breakage and must not regress);
/// a nested `niu -c` inheriting the session's console answers
/// identically (the `-c` face from the issue).
#[test]
fn stty_query_faces_answer_under_conpty() {
    if !require_pty_or_skip("stty_query_faces_answer_under_conpty") {
        return;
    }
    let mut s = NiuSession::spawn_custom("issue535", &default_rc(), &[], SIZE, HEAVY_TIMEOUT);
    s.wait_ready();
    s.send_line("echo \"SZ[$(stty size)]\"");
    s.expect("SZ[33 101]");
    s.send_line("echo \"CL[$(stty cols)]\"");
    s.expect("CL[101]");
    s.send_line("echo \"RW[$(stty rows)]\"");
    s.expect("RW[33]");
    s.send_line("stty raw; echo \"RAW#$?\"; stty sane; echo \"SANE#$?\"");
    s.expect("RAW#0");
    s.expect("SANE#0");
    // The `-c` face: a child niu inheriting this session's console
    // handles must answer identically (its builtin sees the same
    // console through the inherited stdin).
    let exe = niu_binary().display().to_string();
    s.send_line(&format!("X='{exe}'"));
    s.send_line("\"$X\" --norc -c 'echo IN[$(stty size)]'");
    s.expect("IN[33 101]");
}
