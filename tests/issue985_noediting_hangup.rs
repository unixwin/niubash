//! niubash#985 — unix: the `--noediting` fallback REPL must answer a
//! hangup like the line-editor loop does (#974): die by the signal,
//! not hang, and not exit plainly.
//!
//! The defect has two faces, both rooted in
//! `run_repl_without_line_editor` never meeting the hangup doorbell
//! (#974) that the editor loop consults:
//!
//! 1. The blocking shape: the fallback loop parks in
//!    `StdinLock::read_line`, which silently retries on EINTR, and
//!    the doorbell itself is only installed on the editor path (the
//!    `no_editing` early return in `run_repl` precedes the install).
//!    A SIGHUP queued by the engine's signal backend is therefore
//!    never consumed at a parked prompt — the session hangs forever,
//!    or ends through the plain EOF finish (exit code of the last
//!    command) when the hangup arrives together with the pty input
//!    going away, losing the signal-death form.
//! 2. The delayed shape: a SIGHUP that lands while a command runs is
//!    drained at the next engine boundary; the untrapped termsig
//!    adjudication arms the session exit jump with 129, and the
//!    fallback loop consumed that jump through the PLAIN session
//!    finish, whose two-guard composition (EXIT trap, then stream-end
//!    duties) drops the engine's termsig mark with its first guard
//!    and resolves the finish to the interrupted line's own status —
//!    so the process exits plainly with that status instead of dying
//!    by the signal (measured on the base: exit 0 after `sleep`,
//!    exit 3 after a command ending 3; the 129 never reaches the
//!    process exit). The loop also never recorded the executed line
//!    in the first place — the host's save-at-accept (which reedline
//!    performs on the editor path) was missing here — so no finish
//!    on this path could flush it. GNU bash 5.3 dies by SIGHUP in both
//!    shapes (sig.c `termsig_handler`: `maybe_save_shell_history`,
//!    `hangup_all_jobs`, `run_exit_trap`, then `kill_shell` re-raises
//!    the signal).
//!
//! These cells drive a `--noediting` session on a pty in chain form:
//! spawn -> type a marker command -> settle -> deliver the hangup ->
//! assert the process died BY SIGHUP (the wait status carries the
//! signal, not an exit code), promptly, without any further input —
//! and that the session history (seed lines + the typed line) and the
//! EXIT trap marker both landed on disk. Control cells pin the
//! adjudication edges on this loop: a TRAPPED hangup runs its action
//! at the prompt and the session (and the not-yet-submitted line)
//! survives; an IGNORED hangup leaves the session fully alive; plain
//! EOF still ends the session through the plain finish with the last
//! status.
//!
//! Unix-only: the cells drive the session leader by `kill(2)`/SIGHUP,
//! which have no Windows spelling. Run with
//! `cargo test --test issue985_noediting_hangup --locked` (the pty
//! probe skips on hosts without a pseudo terminal unless
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
        "niu-issue985-{prefix}-{}-{nanos}",
        std::process::id()
    ))
}

/// Spawn a `--noediting` session over a fresh home whose
/// `.niubash_history` is seeded and whose rc arms an EXIT trap writing
/// `$HOME/trapmark.txt`, plus any cell-specific rc lines. Returns
/// session + home path.
fn spawn_seeded(prefix: &str, extra_rc: &str) -> (NiuSession, PathBuf) {
    let root = unique_root(prefix);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".niubash_history"), "seedA\nseedB\n").unwrap();
    let rc = format!(
        "{}\ntrap 'echo EXITTRAP >> \"$HOME/trapmark.txt\"' EXIT\n{extra_rc}\n",
        default_rc()
    );
    let mut session = NiuSession::spawn_shared_home_with_args(
        &root,
        &rc,
        &["--noediting"],
        &[],
        (120, 30),
        DEFAULT_TIMEOUT,
    );
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
fn issue985_signal_form_dies_by_signal_with_history_and_trap() {
    if !require_pty_or_skip("issue985_signal_form") {
        return;
    }
    let (mut session, home) = spawn_seeded("signal", "");
    let pid = shell_pid(&mut session, &home);
    type_marker_and_settle(&mut session, "HUP_MARKER_SIGNAL");
    // No further input after this point: on the defect base the
    // fallback read never returns and wait_status times out.
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
fn issue985_window_form_dies_by_signal_with_history_and_trap() {
    if !require_pty_or_skip("issue985_window_form") {
        return;
    }
    let (mut session, home) = spawn_seeded("window", "");
    let pid = shell_pid(&mut session, &home);
    type_marker_and_settle(&mut session, "HUP_MARKER_WINDOW");
    send_hangup(pid);
    // The window half: the terminal's input side goes away with the
    // tab. On the defect base the read ends by EOF and the session
    // exits with the last command's code through the plain finish —
    // the signal-death assertion is what catches it.
    session.close_input();
    assert_hangup_death(&mut session, &home, "HUP_MARKER_WINDOW");
}

#[test]
fn issue985_execution_period_hangup_dies_by_signal_not_plain_exit() {
    if !require_pty_or_skip("issue985_execution_period_hangup") {
        return;
    }
    let (mut session, home) = spawn_seeded("execperiod", "");
    let pid = shell_pid(&mut session, &home);
    // The hangup lands while a command owns the terminal; it is
    // drained at the next boundary. The long command is a pure
    // builtin arithmetic loop: the pin harness's cleared PATH has
    // no external `sleep` on this host, and the loop's dense
    // per-iteration boundaries are exactly where the engine drains
    // the delivery (the loop would otherwise run for seconds).
    // On the defect base the drain aborts the line, the fallback
    // loop consumes the armed jump through the plain finish, and
    // the process EXITS with the line's status; the fixed loop
    // routes the jump through the hangup finish and dies by the
    // signal.
    session.send_line("i985=0; while (( i985 < 20000 )); do (( i985 += 1 )); done");
    std::thread::sleep(Duration::from_millis(700));
    let sent_at = Instant::now();
    send_hangup(pid);
    let status = session.wait_status();
    assert_eq!(
        status.signal(),
        Some("Hangup"),
        "an execution-period hangup must die BY SIGHUP, not exit plainly, got {status:?}"
    );
    assert!(
        sent_at.elapsed() < Duration::from_secs(10),
        "the execution-period hangup death must follow the drain, took {:?}",
        sent_at.elapsed()
    );
    let history = std::fs::read_to_string(home.join(".niubash_history"))
        .expect("history file exists after the session ends");
    assert!(
        history.contains("i985"),
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
fn issue985_trapped_hangup_runs_trap_and_the_line_resumes() {
    if !require_pty_or_skip("issue985_trapped_hangup") {
        return;
    }
    let (mut session, home) = spawn_seeded(
        "trapped",
        "trap 'echo HUPTRAP >> \"$HOME/huptrap.txt\"' HUP",
    );
    let pid = shell_pid(&mut session, &home);
    // Type but do not submit a line, then hang up: the trap must run
    // at the prompt WITHOUT the line being submitted first (on the
    // defect base the parked read never returns, so the trap can only
    // run late, piggybacked on the next line's execution).
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
    // Still alive, and the unsubmitted line still executes.
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
fn issue985_ignored_hangup_leaves_the_session_alive() {
    if !require_pty_or_skip("issue985_ignored_hangup") {
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

#[test]
fn issue985_eof_ends_session_with_history_and_trap() {
    if !require_pty_or_skip("issue985_eof_control") {
        return;
    }
    let (mut session, home) = spawn_seeded("eof", "");
    type_marker_and_settle(&mut session, "EOF_MARKER");
    // Plain EOF (Ctrl-D's driver shape): the session ends through the
    // plain finish with the last command's status, the history file
    // is written and the EXIT trap runs. Control cell: green before
    // and after the hangup fix.
    //
    // The typed line itself is NOT asserted here: on the base the
    // fallback's plain finish writes only the session's synced
    // entries, and the just-executed line reaches them at the NEXT
    // prompt's sync — which an EOF end never runs — so the base
    // loses it (measured; the hangup route's flush syncs first,
    // which is why the hangup cells above can assert the typed
    // line). That EOF history gap is an adjacent plain-finish
    // defect outside this issue's hangup scope; it is recorded in
    // the lane report for the captain rather than pinned here.
    session.close_input();
    let status = session.wait_status();
    assert!(
        status.signal().is_none() && status.exit_code() == 0,
        "a plain EOF ends the session with the last status, got {status:?}"
    );
    let history = std::fs::read_to_string(home.join(".niubash_history"))
        .expect("history file exists after the session ends");
    assert!(
        history.contains("seedA") && history.contains("seedB"),
        "the EOF flush kept the seeded base lines: {history:?}"
    );
    let trapmark = std::fs::read_to_string(home.join("trapmark.txt"))
        .expect("trap mark file exists after the session ends");
    assert!(
        trapmark.contains("EXITTRAP"),
        "the EOF exit ran the EXIT trap: {trapmark:?}"
    );
}
