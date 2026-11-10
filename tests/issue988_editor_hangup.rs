//! niubash#988 — unix: the line-editor REPL loop must answer an
//! execution-period SIGHUP by dying BY the signal, like the
//! `--noediting` fallback loop does since #985 — not by running the
//! interrupted command to its end and exiting plainly with that
//! command's status.
//!
//! The defect is the editor loop's half of the #985 mechanism. A
//! SIGHUP that lands while a submitted line executes is drained at
//! the next engine boundary; the untrapped termsig adjudication
//! records the death (`hangup_death_pending`) and unwinds with
//! 128+SIGHUP, arming the session exit jump. The fallback loop
//! routes that jump through the hangup finish (history flush, then
//! the single-guard finish that dies by the signal). The editor
//! loop's jump consumption points — after a submitted line, and
//! after a widget-accepted line — consumed the same jump through
//! the PLAIN session finish, whose two-guard composition drops the
//! termsig mark with its first guard and resolves the finish to the
//! interrupted line's own status: measured on the base, the session
//! exits 0 after an interrupted loop, and exits 3 after an
//! interrupted command that itself ends 3. GNU bash 5.3 dies by
//! SIGHUP in this shape regardless of the command's own status
//! (sig.c `termsig_handler`: `maybe_save_shell_history`,
//! `hangup_all_jobs`, `run_exit_trap`, then `kill_shell` re-raises
//! the signal; the niu boundary model dies at the command-end
//! boundary instead of inside the wait — the timing half is the
//! engine waiting-surface topic, not this issue).
//!
//! These cells drive an editor (ruline) session on a pty in chain
//! form: spawn -> type a marker command -> settle -> start a long
//! command -> deliver the hangup to the shell pid mid-execution ->
//! assert the process died BY SIGHUP (the wait status carries the
//! signal, not an exit code), that the session history (seed lines
//! + the typed line) and the EXIT trap marker landed on disk.
//! Control cells pin the adjudication edges the routing must not
//! disturb: a TRAPPED hangup during execution runs its action and
//! the session survives; an IGNORED hangup leaves the session fully
//! alive; a hangup at the parked prompt keeps the #974 signal death
//! (that shape was already correct — it is the boundary control for
//! this file, not part of this fix).
//!
//! Unix-only: the cells drive the session leader by `kill(2)`/SIGHUP,
//! which have no Windows spelling. Run with
//! `cargo test --test issue988_editor_hangup --locked` (the pty
//! probe skips on hosts without a pseudo terminal unless
//! NIU_INTERACTIVE_TESTS=1 forces it).
#![cfg(unix)]

#[path = "interactive/driver.rs"]
mod driver;

use driver::{default_rc, niu_binary, require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn unique_root(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "niu-issue988-{prefix}-{}-{nanos}",
        std::process::id()
    ))
}

/// Spawn an editor-path session (no `--noediting`) over a fresh home
/// whose `.niubash_history` is seeded and whose rc arms an EXIT trap
/// writing `$HOME/trapmark.txt`, plus any cell-specific rc lines.
/// Returns session + home path.
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
        NiuSession::spawn_shared_home_with_args(&root, &rc, &[], &[], (120, 30), DEFAULT_TIMEOUT);
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

/// Type the marker command and settle at the prompt.
fn type_marker_and_settle(session: &mut NiuSession, marker: &str) {
    session.send_line(&format!("echo {marker}"));
    session.expect_prompt();
    std::thread::sleep(Duration::from_millis(400));
    session.drain_screen();
}

/// The shared hangup-death assertions: the wait status is a SIGHUP
/// death (not an exit code), the flush kept the seeds and gained the
/// typed line, and the EXIT trap ran.
fn assert_hangup_death(session: &mut NiuSession, home: &Path, typed_fragment: &str) {
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
        history.contains(typed_fragment),
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
fn issue988_execution_period_hangup_dies_by_signal_not_plain_exit() {
    if !require_pty_or_skip("issue988_execution_period_hangup") {
        return;
    }
    let (mut session, home) = spawn_seeded("execperiod", "");
    let pid = shell_pid(&mut session, &home);
    // The hangup lands while a command owns the terminal; it is
    // drained at the next engine boundary. The long command is a
    // pure builtin arithmetic loop: the pin harness's cleared PATH
    // has no external `sleep` on this host, and the loop's dense
    // per-iteration boundaries are exactly where the engine drains
    // the delivery. On the defect base the drain aborts the line,
    // the editor loop consumes the armed jump through the plain
    // finish, and the process EXITS with the line's status; the
    // fixed loop routes the jump through the hangup finish and
    // dies by the signal.
    session.send_line("i988=0; while (( i988 < 20000 )); do (( i988 += 1 )); done");
    std::thread::sleep(Duration::from_millis(700));
    let sent_at = Instant::now();
    send_hangup(pid);
    assert_hangup_death(&mut session, &home, "i988");
    assert!(
        sent_at.elapsed() < Duration::from_secs(10),
        "the execution-period hangup death must follow the drain, took {:?}",
        sent_at.elapsed()
    );
}

#[test]
fn issue988_execution_period_command_ending_3_still_dies_by_signal() {
    if !require_pty_or_skip("issue988_execution_period_status3") {
        return;
    }
    let (mut session, home) = spawn_seeded("status3", "");
    let pid = shell_pid(&mut session, &home);
    // The interrupted command is an external child (this binary in
    // `-c` mode) that runs for seconds and itself exits 3. The
    // hangup goes only to the shell pid: the child runs to its own
    // completion, the drain after the wait records the death with
    // the child's status as the trap-entry capture — on the defect
    // base the plain finish resolves to that capture and the
    // process EXITS 3; GNU dies by the signal regardless of the
    // command's status, and so must the fixed loop. The child's
    // loop is wall-clock bounded (SECONDS), not iteration counted,
    // so its runtime does not depend on host speed.
    let child = format!(
        "\"{}\" -c 'end988=$((SECONDS+6)); while (( SECONDS < end988 )); do :; done; exit 3'",
        niu_binary().display()
    );
    session.send_line(&child);
    std::thread::sleep(Duration::from_millis(800));
    let sent_at = Instant::now();
    send_hangup(pid);
    assert_hangup_death(&mut session, &home, "exit 3");
    assert!(
        sent_at.elapsed() < Duration::from_secs(15),
        "the status-3 hangup death must follow the child wait, took {:?}",
        sent_at.elapsed()
    );
}

#[test]
fn issue988_trapped_hangup_during_execution_runs_trap_and_session_survives() {
    if !require_pty_or_skip("issue988_trapped_execution") {
        return;
    }
    let (mut session, home) = spawn_seeded(
        "trapped",
        "trap 'echo HUPTRAP >> \"$HOME/huptrap.txt\"' HUP",
    );
    let pid = shell_pid(&mut session, &home);
    // A trapped hangup is adjudicated by the engine's boundary
    // dispatch, not by the death routing: the action runs, no death
    // mark is recorded, and the session must reach a fresh prompt
    // and exit cleanly. This cell guards the fix's discrimination —
    // a routing that mistook a trapped delivery for the untrapped
    // death would kill the session here. The loop is shorter than
    // the defect cells' (which the drain aborts): this one runs to
    // completion, so it must fit the driver's prompt timeout even
    // on a loaded host; if a fast host finishes it before the
    // hangup lands, the cell degrades to the parked trapped shape
    // and the same assertions hold.
    session.send_line("i988=0; while (( i988 < 5000 )); do (( i988 += 1 )); done");
    std::thread::sleep(Duration::from_millis(600));
    send_hangup(pid);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if std::fs::read_to_string(home.join("huptrap.txt"))
            .is_ok_and(|text| text.contains("HUPTRAP"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the trapped HUP did not run during execution\n{}",
            session.transcript()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    session.expect_prompt();
    session.send_line("echo STILL_ALIVE_$((3*3))");
    session.expect("STILL_ALIVE_9");
    session.send_line("exit");
    let status = session.wait_status();
    assert!(
        status.signal().is_none() && status.exit_code() == 0,
        "a trapped hangup leaves a clean session to exit 0, got {status:?}"
    );
}

#[test]
fn issue988_ignored_hangup_during_execution_leaves_session_alive() {
    if !require_pty_or_skip("issue988_ignored_execution") {
        return;
    }
    let (mut session, home) = spawn_seeded("ignored", "trap '' HUP");
    let pid = shell_pid(&mut session, &home);
    // Short loop, as in the trapped control: it runs to completion
    // under the ignored delivery.
    session.send_line("i988=0; while (( i988 < 5000 )); do (( i988 += 1 )); done");
    std::thread::sleep(Duration::from_millis(600));
    send_hangup(pid);
    session.expect_prompt();
    session.send_line("echo STILL_ALIVE_$((4*4))");
    session.expect("STILL_ALIVE_16");
    session.send_line("exit");
    let status = session.wait_status();
    assert!(
        status.signal().is_none() && status.exit_code() == 0,
        "an ignored hangup leaves a clean session to exit 0, got {status:?}"
    );
}

#[test]
fn issue988_parked_hangup_dies_by_signal_boundary_control() {
    if !require_pty_or_skip("issue988_parked_control") {
        return;
    }
    // Boundary control: the parked-prompt (idle-read) editor shape
    // was already correct before this fix (#974's doorbell break +
    // prompt dispatch); the execution-period fix must not disturb
    // it. On the defect base this cell is green — it discriminates
    // the fix's blast radius, not the defect.
    let (mut session, home) = spawn_seeded("parked", "");
    let pid = shell_pid(&mut session, &home);
    type_marker_and_settle(&mut session, "HUP_MARKER_PARKED");
    let sent_at = Instant::now();
    send_hangup(pid);
    assert_hangup_death(&mut session, &home, "echo HUP_MARKER_PARKED");
    assert!(
        sent_at.elapsed() < Duration::from_secs(8),
        "the parked hangup death must be prompt, took {:?}",
        sent_at.elapsed()
    );
}
