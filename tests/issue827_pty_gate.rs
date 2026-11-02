//! niubash#827 — the interactive PTY gate: real-terminal scenarios that
//! must RUN on every platform, not merely compile.
//!
//! Why this target exists: until now the full test suite ran only in the
//! Windows CI job; the ubuntu/macOS jobs hard-gated on `cargo check`
//! (plus an informational `--lib` run), and the Android targets were
//! check-only — so the entire interactive surface (tests/interactive.rs
//! is `#![cfg(windows)]`, and the driver-based completion targets were
//! never executed by a unix CI leg) had no unix execution gate at all.
//! The unix-only interactive regressions that followed (niubash#826 /
//! #828: TAB completion dead on Linux/Termux while Windows stayed
//! green) were invisible to CI by construction. This target is the
//! execution gate: `.github/workflows/ci.yml` runs it as a hard step in
//! the ubuntu/macOS `cross-check` job with `NIU_INTERACTIVE_TESTS=1`
//! (a missing pty is then a failure, not the driver's desktop skip),
//! and the Windows job's `cargo test --workspace` includes it like
//! every other target.
//!
//! Android: the cross-target jobs are check-only and there is no
//! Android device/emulator lane in CI, so no PTY scenario can execute
//! for Android in CI today. The interim Android gate is the Termux
//! real-machine acceptance run before release; these scenarios are
//! the checklist that acceptance replays by hand until an emulator
//! lane exists.
//!
//! Harness: [`mod driver`] — the repo's own portable-pty driver
//! (tests/interactive/driver.rs), the same harness behind
//! tests/interactive.rs and the #631/#769/#821 pins. Sessions spawn
//! over a caller-staged root via `spawn_shared_home` so the gate can
//! put real coreutils (`ls`, `sleep`) on the child's PATH — the
//! driver's default PATH is a System32 placeholder that leaves unix
//! sessions with builtins only.
//!
//! Scenario set (one test per scenario; expectations are GNU
//! bash/readline behavior, probed against GNU bash 5.3.20):
//!   S1 boot: prompt renders, a command executes, `exit` ends the
//!      session with code 0.
//!   S2 completion, unique FILE: `ls z` TAB commits `ls zebra.txt `
//!      (a file takes the trailing space).
//!   S3 completion, unique DIRECTORY (the 2026-11-02 user report,
//!      pin #1 of this gate): `ls t` TAB must commit `ls tools/`
//!      (a directory takes a slash, NO trailing space), drilling in
//!      and executing from there.
//!   S4 completion menu: two candidates open the menu; navigation +
//!      Enter accepts one WITH its directory slash.
//!   S5 Ctrl-R: reverse history search finds a prior command and the
//!      accept/submit pair executes it.
//!   S6 resize: PTY geometry change mid-session; the prompt survives
//!      and `$COLUMNS` follows the new width (the niubash#822 chain).
//!   S7 bracketed paste: a pasted line lands in the edit buffer
//!      unexecuted until Enter.
//!   S8 Ctrl-Z / fg (unix): a foreground job stops on Ctrl-Z, `jobs`
//!      shows it Stopped, `fg` resumes it to completion.
//!
//! Run: `NIU_INTERACTIVE_TESTS=1 cargo test --test issue827_pty_gate --locked -- --nocapture`

#[path = "interactive/driver.rs"]
mod driver;

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT, TAB};

/// Short window for negative ("must not appear") assertions.
const ABSENT_WINDOW: Duration = Duration::from_millis(800);
const CTRL_C: &str = "\u{3}";
const CTRL_R: &str = "\u{12}";
const DOWN: &str = "\u{1b}[B";

/// PATH the gated session's children see. The driver's default is a
/// System32-shaped placeholder (on unix it resolves to /tmp), which
/// leaves the session with builtins only; the gate's scenarios execute
/// real `ls`/`sleep`, so unix gets the system coreutils dirs. Windows
/// keeps the driver default (empty list): niu's own applet injection
/// provides the command set there, like every other driver suite.
fn gate_path_dirs() -> Vec<PathBuf> {
    if cfg!(windows) {
        Vec::new()
    } else {
        vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
    }
}

/// A fresh sandbox root for `spawn_shared_home` (the session removes
/// it on drop). Mirrors the driver's own unique-name shape.
fn staged_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("niu-827-{tag}-{}-{}", std::process::id(), nanos))
}

/// Spawn a gated session over a caller-staged root with the
/// deterministic sentinel rc (`P1> `/`P2> `, default plugins off).
fn spawn_gate(tag: &str, rc_extra: &str) -> NiuSession {
    let rc = format!("{}{}", driver::default_rc(), rc_extra);
    NiuSession::spawn_shared_home(
        &staged_root(tag),
        &rc,
        &[],
        &gate_path_dirs(),
        (100, 30),
        DEFAULT_TIMEOUT,
    )
}

/// The driver's `transcript()` marks ESC as the literal text `<ESC>`,
/// which splits rendered words wherever the editor emitted a CSI
/// sequence (cursor moves, show/hide cursor). For buffer-shape
/// assertions ("what does the edited line actually read"), strip the
/// CSI sequences themselves so the printed glyphs concatenate in
/// order; what remains is the text a user would read off the screen.
fn normalize_screen(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(pos) = rest.find("<ESC>") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 5..];
        if let Some(stripped) = after.strip_prefix('[') {
            // CSI: parameter/intermediate bytes until a final byte
            // in @ (0x40) ..= ~ (0x7E).
            let end = stripped
                .find(|c: char| ('@'..='~').contains(&c))
                .map(|i| i + c_len(stripped, i));
            match end {
                Some(e) => rest = &stripped[e..],
                None => rest = "",
            }
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Byte length of the char at byte index `i` in `s`.
fn c_len(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(0, |c| c.len_utf8())
}

/// Poll the normalized transcript until `needle` appears or the
/// budget runs out; returns the normalized screen either way so the
/// caller can assert (and report) on it.
fn await_screen(s: &NiuSession, needle: &str, budget: Duration) -> String {
    let deadline = Instant::now() + budget;
    loop {
        let screen = normalize_screen(&s.transcript());
        if screen.contains(needle) || Instant::now() >= deadline {
            return screen;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Let the editor finish its prompt reset before sending more
/// keystrokes. Observed on master (captain probes, 2026-11-02):
/// `expect_prompt()` returns the instant the prompt glyphs render,
/// but keystrokes sent in that same instant — right after a command
/// executed or a Ctrl-C reset — never reach the line (the transcript
/// shows total silence, as if the bytes were flushed during the
/// reset). The established suite absorbs the same window with an
/// `expect_absent` settle (e.g. `ctrl_c_midtyped_discards_line`);
/// a plain pause is the honest equivalent here. The identical
/// keystroke sequences, paced by an idle-drain PTY harness, drive
/// master correctly — this is harness pacing, not an expectation.
fn settle() {
    std::thread::sleep(Duration::from_millis(500));
}

// ---------------------------------------------------------------------------
// S1 — boot: prompt renders, command executes, clean exit.
// ---------------------------------------------------------------------------

#[test]
fn pty_gate_s1_boot_echo_and_clean_exit() {
    if !require_pty_or_skip("pty_gate_s1_boot_echo_and_clean_exit") {
        return;
    }
    let mut s = spawn_gate("s1", "H827=hello827-out\n");
    s.wait_ready();
    // Echo-proof (the #367 rule): the typed line shows `$H827`, so
    // only the executed output can satisfy the expectation.
    s.send_line("echo $H827");
    s.expect("hello827-out");
    s.expect_prompt();
    s.send_line("exit");
    assert_eq!(
        s.wait_exit(),
        0,
        "session must exit cleanly\n{}",
        s.transcript()
    );
}

// ---------------------------------------------------------------------------
// S2 — completion of a unique FILE commits the name plus a trailing
// space (GNU/readline: the append character for a regular file).
// ---------------------------------------------------------------------------

#[test]
fn pty_gate_s2_completion_unique_file_takes_trailing_space() {
    if !require_pty_or_skip("pty_gate_s2_completion_unique_file_takes_trailing_space") {
        return;
    }
    let mut s = spawn_gate("s2", "");
    std::fs::write(s.start().join("zebra.txt"), "ZBODY827\n").unwrap();
    s.wait_ready();
    // Commit leg: TAB + Enter must run the completed operand.
    s.send("ls z");
    s.send(TAB);
    s.send("\r");
    s.expect("zebra.txt");
    s.expect_prompt();
    settle();
    // Suffix leg: complete again, then type X. A file's trailing space
    // makes X a NEW argument (`ls zebra.txt X`); no space would glue
    // it onto the name (`ls zebra.txtX`). The rendered line decides.
    s.send("ls z");
    s.send(TAB);
    s.send("X");
    let screen = await_screen(&s, "zebra.txt", Duration::from_secs(3));
    assert!(
        screen.contains("zebra.txt X"),
        "file completion must commit `ls zebra.txt ` (trailing space); \
         rendered line does not show it\nscreen:\n{screen}"
    );
    s.send(CTRL_C);
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// S3 — completion of a unique DIRECTORY commits the name plus a slash
// and NO trailing space (GNU/readline marks directories with `/`).
// The 2026-11-02 user report (Termux): `ls t` TAB produced
// `ls tools ` — a trailing space where the slash belongs.
// ---------------------------------------------------------------------------

#[test]
fn pty_gate_s3_completion_unique_directory_takes_slash() {
    if !require_pty_or_skip("pty_gate_s3_completion_unique_directory_takes_slash") {
        return;
    }
    let mut s = spawn_gate("s3", "");
    std::fs::create_dir(s.start().join("tools")).unwrap();
    std::fs::write(s.start().join("tools").join("inner.txt"), "INBODY827\n").unwrap();
    s.wait_ready();
    // Buffer leg: complete, then type X. The slash makes X continue
    // INSIDE the directory (`ls tools/X`); a trailing space would
    // make it a new argument (`ls tools X`), no suffix would glue it
    // (`ls toolsX`). The rendered line decides.
    s.send("ls t");
    s.send(TAB);
    s.send("X");
    let screen = await_screen(&s, "tools", Duration::from_secs(3));
    assert!(
        screen.contains("tools/X"),
        "directory completion must commit `ls tools/` (slash, no trailing \
         space); rendered line does not show it\nscreen:\n{screen}"
    );
    s.send(CTRL_C);
    s.expect_prompt();
    settle();
    // Drill-in leg: from the committed `ls tools/`, a second word
    // continues inside the directory and completes the inner file;
    // Enter then executes the full path.
    s.send("ls t");
    s.send(TAB);
    s.send("i");
    s.send(TAB);
    s.send("\r");
    s.expect("tools/inner.txt");
    s.expect_prompt();
    s.expect_absent("cannot access", ABSENT_WINDOW);
}

// ---------------------------------------------------------------------------
// S4 — completion menu: two candidates sharing a prefix open the menu;
// navigation + Enter accepts the highlighted candidate WITH the
// suffix for its kind (a directory's slash).
// ---------------------------------------------------------------------------

#[test]
fn pty_gate_s4_completion_menu_navigates_and_accepts_with_slash() {
    if !require_pty_or_skip("pty_gate_s4_completion_menu_navigates_and_accepts_with_slash") {
        return;
    }
    let mut s = spawn_gate("s4", "");
    std::fs::create_dir(s.start().join("alpha")).unwrap();
    std::fs::write(s.start().join("alpha").join("aaa.txt"), "A827\n").unwrap();
    std::fs::create_dir(s.start().join("alpine")).unwrap();
    std::fs::write(s.start().join("alpine").join("bbb.txt"), "B827\n").unwrap();
    s.wait_ready();
    s.send("ls al");
    s.send(TAB);
    // The menu/list must render BOTH candidates.
    let screen = await_screen(&s, "alpine", Duration::from_secs(5));
    assert!(
        screen.contains("alpha") && screen.contains("alpine"),
        "completion menu must list both candidates\nscreen:\n{screen}"
    );
    // Navigate once, then Enter accepts the highlighted candidate
    // (niubash#821: with a menu up, Enter accepts, it does not submit).
    s.send(DOWN);
    s.send("\r");
    // Whichever directory was accepted, it must carry its slash:
    // typing X continues inside it.
    s.send("X");
    let screen = await_screen(&s, "/X", Duration::from_secs(3));
    assert!(
        screen.contains("alpha/X") || screen.contains("alpine/X"),
        "menu accept must commit the directory with its slash \
         (`ls alpha/` or `ls alpine/`)\nscreen:\n{screen}"
    );
    s.send(CTRL_C);
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// S5 — Ctrl-R: reverse history search finds a prior command; the
// accept Enter loads it and the submit Enter executes it.
// ---------------------------------------------------------------------------

#[test]
fn pty_gate_s5_ctrl_r_reverse_search_accepts_and_runs() {
    if !require_pty_or_skip("pty_gate_s5_ctrl_r_reverse_search_accepts_and_runs") {
        return;
    }
    let mut s = spawn_gate("s5", "S827=CR827-OUT\n");
    s.wait_ready();
    // Seed history with a command whose OUTPUT differs from its text
    // (echo-proof, the #420 rule): only a real re-execution prints
    // the value a second time.
    s.send_line("echo $S827");
    s.expect("CR827-OUT");
    s.expect_prompt();
    s.send(CTRL_R);
    std::thread::sleep(Duration::from_millis(120));
    s.send("S827");
    std::thread::sleep(Duration::from_millis(120));
    // First Enter accepts the match into the buffer, second submits.
    s.send("\r");
    s.send("\r");
    s.expect("CR827-OUT");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// S6 — resize: geometry change mid-session; the prompt keeps working
// and $COLUMNS follows the new width (the niubash#822 chain: the
// shell's own COLUMNS is the single source, refreshed on resize).
// ---------------------------------------------------------------------------

#[test]
fn pty_gate_s6_resize_prompt_survives_and_columns_follows() {
    if !require_pty_or_skip("pty_gate_s6_resize_prompt_survives_and_columns_follows") {
        return;
    }
    // spawn_gate geometry is (100, 30).
    let mut s = spawn_gate("s6", "");
    s.wait_ready();
    s.send_line("echo COL827=$COLUMNS");
    s.expect("COL827=100");
    s.expect_prompt();
    s.resize(80, 24);
    s.send_line("echo COL827=$COLUMNS");
    s.expect("COL827=80");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// S7 — bracketed paste: the pasted line lands in the edit buffer and
// nothing executes until Enter.
// ---------------------------------------------------------------------------

#[test]
fn pty_gate_s7_bracketed_paste_buffers_until_enter() {
    if cfg!(windows) {
        // Platform limitation, not a regression: ConPTY consumes the
        // ESC[200~/ESC[201~ markers before they reach the shell
        // (crossterm has no ANSI input parser on Windows), so a
        // terminal paste is carried by its CRLF signature there —
        // that shape is pinned in tests/interactive.rs. Same
        // treatment as the niubash#202 case.
        return;
    }
    if !require_pty_or_skip("pty_gate_s7_bracketed_paste_buffers_until_enter") {
        return;
    }
    let mut s = spawn_gate("s7", "P827=PASTE827-OUT\n");
    s.wait_ready();
    // One bracketed paste: a multi-word line, no Enter inside. The
    // buffer shows the literal `$P827`; only execution prints the
    // value, so the value is the execution witness.
    s.send("\u{1b}[200~echo $P827 alpha beta gamma\u{1b}[201~");
    s.expect_absent("PASTE827-OUT", ABSENT_WINDOW);
    s.send("\r");
    s.expect("PASTE827-OUT");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// S8 — Ctrl-Z / fg (unix): a foreground job stops on Ctrl-Z, `jobs`
// reports it Stopped, and `fg` resumes it to completion.
//
// Harness shape, stated plainly (the niubash#769 lesson): the DRIVEN
// shell is a session leader alone in its process group — an orphaned
// group — so the kernel discards SIGTSTP for the LEADER itself and
// no conclusion about the shell's own suspension may be drawn from
// this shape. The FOREGROUND JOB is different: the shell places it
// in its own process group inside the same session, with the shell
// in another group, so the job's group is NOT orphaned and SIGTSTP
// applies to it normally (the shape the captain verified live on
// 2026-11-02 with /proc state as ground truth). Every assertion
// below targets the job, never the leader.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn pty_gate_s8_ctrl_z_stops_job_and_fg_completes_it() {
    if !require_pty_or_skip("pty_gate_s8_ctrl_z_stops_job_and_fg_completes_it") {
        return;
    }
    let mut s = spawn_gate("s8", "");
    s.wait_ready();
    // ONE job whose own output is the completion witness: the list
    // lives inside the child `sh`, so the marker can only print when
    // the job truly runs to completion after `fg`. (A bare
    // `sleep 3; echo DONE` list at the prompt is NOT a valid witness:
    // GNU bash 5.3.20, replayed under this same pty shape, runs the
    // `echo` immediately after the stop — each `;` element is its own
    // job — so the marker proves nothing there. niu matches GNU on
    // that shape byte for byte; the single-job form is the one the
    // captain verified live on 2026-11-02.)
    s.send_line("sh -c 'sleep 3; echo DONE827'");
    // Let the job take the foreground before the stop key (a Ctrl-Z
    // that lands while the line is still being submitted belongs to
    // the editor, not the job — arrival timing, same as GNU).
    std::thread::sleep(Duration::from_millis(600));
    s.send("\u{1a}");
    // The shell regains its prompt with the job stopped, and the
    // witness must NOT have printed while it is stopped.
    s.expect_prompt();
    s.expect_absent("DONE827", ABSENT_WINDOW);
    s.send_line("jobs");
    s.expect("Stopped");
    s.expect_prompt();
    settle();
    // fg resumes the job; the remaining sleep runs out inside the
    // child and only then does the marker print.
    s.send_line("fg");
    s.expect("DONE827");
    s.expect_prompt();
}
