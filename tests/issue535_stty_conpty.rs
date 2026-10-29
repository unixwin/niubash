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
//! Unlike tests/interactive.rs this target does not skip merely because
//! the CI marker is set: the legs are single batch commands with marker
//! assertions, not timing-sensitive live-session dances. If no pseudo
//! terminal can be constructed at all, the target skips with a notice
//! (that shape then belongs to the real-machine smoke list).

#[path = "interactive/driver.rs"]
mod driver;

use driver::{default_rc, niu_binary, NiuSession, HEAVY_TIMEOUT};

/// (cols, rows) the pseudo console is created with.
const SIZE: (u16, u16) = (101, 33);

fn spawn_or_skip(context: &str) -> Option<NiuSession> {
    // The driver's spawn panics when the session cannot be established;
    // catch it so a host with no usable pseudo terminal skips honestly
    // instead of failing the suite.
    for _ in 0..2 {
        let attempt = std::panic::catch_unwind(|| {
            NiuSession::spawn_custom("issue535", &default_rc(), &[], SIZE, HEAVY_TIMEOUT)
        });
        match attempt {
            Ok(mut session) => {
                session.wait_ready();
                return Some(session);
            }
            Err(_) => continue,
        }
    }
    eprintln!("SKIP {context}: could not establish a ConPTY session on this host");
    None
}

/// The full #535 face set in one session: size/cols/rows report the
/// pseudo console's created geometry; raw/sane mode operands still
/// apply (they were never part of the breakage and must not regress).
#[test]
fn stty_query_faces_answer_under_conpty() {
    let Some(mut s) = spawn_or_skip("stty_query_faces_answer_under_conpty") else {
        return;
    };
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
