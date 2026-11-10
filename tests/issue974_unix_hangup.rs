//! niubash#974 — unix: a hangup at the interactive prompt must end the
//! session through the session-end funnel, dying by the signal.
//!
//! The defect: closing a terminal tab (kernel SIGHUP + a dead pty) or a
//! bare `kill -HUP` at the prompt left the session hung forever. The
//! engine queued the delivery (its kernel-signal backend owns SIGHUP),
//! but the prompt sits inside the line editor's `read_line`, which
//! never runs a command boundary and never returns for a dead tty — so
//! the queued hangup was never consumed, and the whole session's
//! history and its EXIT trap were lost. A hangup consumed late, by
//! submitting another line, ended the session with exit code 0. GNU
//! bash 5.3 in the same harness dies by SIGHUP in ~0.1 s with the
//! session history saved and the EXIT trap run (sig.c
//! `termsig_handler`: `maybe_save_shell_history`, `hangup_all_jobs`,
//! `run_exit_trap`, then `kill_shell` re-raises the signal).
//!
//! These cells drive a real session on a pty in chain form: spawn ->
//! type a marker command -> settle at the prompt -> deliver the hangup
//! -> assert the process died BY SIGHUP (the wait status carries the
//! signal, not an exit code), promptly, without any further input —
//! and that the session history (seed lines + the typed line) and the
//! EXIT trap marker both landed on disk. Two control cells pin the
//! adjudication edges: a TRAPPED hangup runs its action at the prompt
//! and the session (and the half-typed line) survives; an IGNORED
//! hangup leaves the session fully alive. The last cell is the chain
//! shape absorbed from PR #975's pin set by captain directive (see its
//! own comment for the provenance and its base fail-first record).
//!
//! The window cell also closes the session's input side after the
//! kill. The driver keeps a master reader clone alive in its output
//! pump, so a full kernel master-side close is not reproducible
//! through it; the explicit SIGHUP is the same signal the kernel
//! delivers to the foreground process group on a real master close,
//! and the full-fidelity close form is covered by the lane's probe
//! evidence (see the PR description).
//!
//! Unix-only: the cells drive the session leader by `kill(2)`/SIGHUP,
//! which have no Windows spelling. Run with
//! `cargo test --test issue974_unix_hangup --locked` (the pty probe
//! skips on hosts without a pseudo terminal unless
//! NIU_INTERACTIVE_TESTS=1 forces it).
#![cfg(unix)]

#[path = "interactive/driver.rs"]
mod driver;

use driver::{default_rc, require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn unique_root(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "niu-issue974-{prefix}-{}-{nanos}",
        std::process::id()
    ))
}

/// Spawn a session over a fresh home whose `.niubash_history` is
/// seeded and whose rc arms an EXIT trap writing `$HOME/trapmark.txt`,
/// plus any cell-specific rc lines. Returns session + home path.
fn spawn_seeded(prefix: &str, extra_rc: &str) -> (NiuSession, PathBuf) {
    let root = unique_root(prefix);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".niubash_history"), "seedA\nseedB\n").unwrap();
    let rc = format!(
        "{}\ntrap 'echo EXITTRAP >> \"$HOME/trapmark.txt\"' EXIT\n{extra_rc}\n",
        default_rc()
    );
    let mut session =
        NiuSession::spawn_shared_home(&root, &rc, &[], &[], (120, 30), DEFAULT_TIMEOUT);
    session.wait_ready();
    (session, home)
}

/// The session shell's pid, reported by the shell itself (`$$`) into
/// `$HOME/.niu-pid`.
fn shell_pid(session: &mut NiuSession, home: &Path) -> i32 {
    session.send_line("echo $$ > \"$HOME/.niu-pid\"");
    session.expect_prompt();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = std::fs::read_to_string(home.join(".niu-pid")) {
            if let Ok(pid) = text.trim().parse::<i32>() {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "session did not report its pid\n{}",
            session.transcript()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn send_hangup(pid: i32) {
    let rc = unsafe { libc::kill(pid, libc::SIGHUP) };
    assert_eq!(rc, 0, "kill(SIGHUP) to pid {pid}");
}

/// Type the marker command and settle at the prompt (the settle +
/// drain between lines mirrors the recorded need from the issue614
/// cells: the hangup must land on a parked prompt, not mid-teardown
/// of the marker command).
fn type_marker_and_settle(session: &mut NiuSession, marker: &str) {
    session.send_line(&format!("echo {marker}"));
    session.expect_prompt();
    std::thread::sleep(Duration::from_millis(400));
    session.drain_screen();
}

/// The shared hangup-death assertions: the wait status is a SIGHUP
/// death (not an exit code), the flush kept the seeds and gained the
/// typed marker line, and the EXIT trap ran.
fn assert_hangup_death(session: &mut NiuSession, home: &Path, marker: &str) {
    let status = session.wait_status();
    assert_eq!(
        status.signal(),
        Some("Hangup"),
        "the session must die BY SIGHUP (wait status signal), got {status:?}"
    );
    let history = std::fs::read_to_string(home.join(".niubash_history"))
        .expect("history file exists after the session ends");
    assert!(
        history.contains("seedA") && history.contains("seedB"),
        "the flush kept the seeded base lines: {history:?}"
    );
    assert!(
        history.contains(&format!("echo {marker}")),
        "the hangup exit flushed the session's typed line: {history:?}"
    );
    let trapmark = std::fs::read_to_string(home.join("trapmark.txt"))
        .expect("trap mark file exists after the session ends");
    assert!(
        trapmark.contains("EXITTRAP"),
        "the hangup exit ran the EXIT trap: {trapmark:?}"
    );
}

#[test]
fn issue974_signal_form_dies_by_signal_with_history_and_trap() {
    if !require_pty_or_skip("issue974_signal_form") {
        return;
    }
    let (mut session, home) = spawn_seeded("signal", "");
    let pid = shell_pid(&mut session, &home);
    type_marker_and_settle(&mut session, "HUP_MARKER_SIGNAL");
    // No further input after this point: on the defect base the
    // session stays parked in the read and wait_status times out.
    let sent_at = Instant::now();
    send_hangup(pid);
    assert_hangup_death(&mut session, &home, "HUP_MARKER_SIGNAL");
    assert!(
        sent_at.elapsed() < Duration::from_secs(8),
        "the hangup death must be prompt, took {:?}",
        sent_at.elapsed()
    );
}

#[test]
fn issue974_window_form_dies_by_signal_with_history_and_trap() {
    if !require_pty_or_skip("issue974_window_form") {
        return;
    }
    let (mut session, home) = spawn_seeded("window", "");
    let pid = shell_pid(&mut session, &home);
    type_marker_and_settle(&mut session, "HUP_MARKER_WINDOW");
    send_hangup(pid);
    // The window half: the terminal's input side goes away with the
    // tab. The session must still land the funnel and die by SIGHUP.
    session.close_input();
    assert_hangup_death(&mut session, &home, "HUP_MARKER_WINDOW");
}

#[test]
fn issue974_trapped_hangup_runs_trap_and_the_edit_resumes() {
    if !require_pty_or_skip("issue974_trapped_hangup") {
        return;
    }
    let (mut session, home) = spawn_seeded(
        "trapped",
        "trap 'echo HUPTRAP >> \"$HOME/huptrap.txt\"' HUP",
    );
    let pid = shell_pid(&mut session, &home);
    // Half-type a line, then hang up: the trap must run at the prompt
    // and the suspended edit must resume intact afterwards.
    session.send("echo RESUMED_$((6*7))");
    std::thread::sleep(Duration::from_millis(300));
    send_hangup(pid);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(home.join("huptrap.txt"))
            .is_ok_and(|text| text.contains("HUPTRAP"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the trapped HUP did not run at the prompt\n{}",
            session.transcript()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // Still alive, and the half-typed line still executes.
    session.press_enter();
    session.expect("RESUMED_42");
    session.send_line("exit");
    let status = session.wait_status();
    assert!(
        status.signal().is_none() && status.exit_code() == 0,
        "a trapped hangup leaves a clean session to exit 0, got {status:?}"
    );
}

#[test]
fn issue974_ignored_hangup_leaves_the_session_alive() {
    if !require_pty_or_skip("issue974_ignored_hangup") {
        return;
    }
    let (mut session, home) = spawn_seeded("ignored", "trap '' HUP");
    let pid = shell_pid(&mut session, &home);
    type_marker_and_settle(&mut session, "HUP_MARKER_IGNORED");
    send_hangup(pid);
    std::thread::sleep(Duration::from_millis(1000));
    session.send_line("echo STILL_ALIVE_$((3*3))");
    session.expect("STILL_ALIVE_9");
    session.send_line("exit");
    let status = session.wait_status();
    assert!(
        status.signal().is_none() && status.exit_code() == 0,
        "an ignored hangup leaves a clean session to exit 0, got {status:?}"
    );
}

/// Absorbed cell (captain directive, 2026-11-10): this chain shape was
/// first pinned by PR #975 for niubash#971, where it drove the unix
/// analogue of the Windows tab close. The #975 captain review
/// (`~/workspace/review-975/CAPTAIN-VERIFY.md`, finding F2) ruled the
/// shape belongs to THIS issue's face — a SIGHUP at a parked prompt is
/// the unix hangup mechanism, not the Windows close event — and
/// re-scoped the cell into the #974 pin set (review §6.2 option (a));
/// #975's rework removes it from that PR so the two lanes do not
/// overlap. Its base fail-first record stands in the same dossier
/// (§2, leg L1): on base `e6fc93f8` the cell ran 0 pass / 1 fail,
/// panicking in the driver's `wait_exit` ("session did not exit after
/// the exit request") with the marker line echoed in the transcript —
/// the parked-prompt hang reproduced. The cell keeps that shape here:
/// seeded history, one typed marker line, settle + drain, SIGHUP,
/// then NO further input; the session must end and the funnel must
/// have landed (history flush with the marker line and the seeds,
/// EXIT trap mark). The wait-status death-form assertion rides on the
/// sibling cells above; this cell mirrors the absorbed shape's own
/// assertions. (The pid here comes from the shell's own `$$` report
/// before the marker line — same target process, one less driver
/// dependency than the original cell's accessor.)
#[test]
fn issue974_absorbed_971_cell_sighup_flushes_history_and_runs_trap() {
    if !require_pty_or_skip("issue974_absorbed_971_cell") {
        return;
    }
    let (mut session, home) = spawn_seeded("absorbed971", "");
    let pid = shell_pid(&mut session, &home);
    session.send_line("echo SIGHUP_MARKER_971");
    session.expect_prompt();
    // Settle + drain between the line and the signal, as in the
    // absorbed shape: the hangup must meet a parked prompt.
    std::thread::sleep(Duration::from_millis(400));
    session.drain_screen();
    send_hangup(pid);
    // The session must end on the signal alone; if it hangs,
    // wait_status panics at the session timeout — the base shape.
    let _status = session.wait_status();
    let history = std::fs::read_to_string(home.join(".niubash_history"))
        .expect("history file exists after the session ends");
    assert!(
        history.contains("echo SIGHUP_MARKER_971"),
        "the signal-driven exit flushed the session line: {history:?}"
    );
    assert!(
        history.contains("seedA") && history.contains("seedB"),
        "the flush kept the seeded base lines: {history:?}"
    );
    let trapmark = std::fs::read_to_string(home.join("trapmark.txt"))
        .expect("trap mark file exists after the session ends");
    assert!(
        trapmark.contains("EXITTRAP"),
        "the signal-driven exit ran the EXIT trap: {trapmark:?}"
    );
}
