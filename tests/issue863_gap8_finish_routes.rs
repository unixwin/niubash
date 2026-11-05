//! niubash#863 GAP-8: every finish route drives the engine finish
//! guard (`Session::begin_finish`) — the EXIT trap runs exactly
//! once per route, observes the route's pending status as its
//! entry `$?`, and the trap's own `exit N` decides the process
//! code. Before GAP-8 the host composed the finish by hand five
//! ways, and the three REPL tails ran the trap with a hard-coded
//! 0/1 and discarded the result (`let _ =`), so EOF on a session
//! whose last command failed still exited 0 with the trap
//! observing 0 (captain adjudication D2 corrects that: EOF's
//! pending status is the last command's status, GNU
//! `exit_shell(last_command_exit_value)`).
//!
//! The trap shape is the same everywhere below: record the
//! observed `$?`, then `exit 7`. Every case asserts the observed
//! status, exactly one observation (the trap ran once), and the
//! process code 7.

#[path = "interactive/driver.rs"]
mod driver;

use driver::{default_rc, require_pty_or_skip, NiuSession, CTRL_D, DEFAULT_TIMEOUT};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const TRAP: &str = "trap 'echo obs=$? >> \"$HOME/obs.txt\"; exit 7' EXIT";

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niu" });
    fallback
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("niu-gap8-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_niu_in(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    run_niu_with_env(dir, args, stdin, &[])
}

fn run_niu_with_env(
    dir: &Path,
    args: &[&str],
    stdin: Option<&str>,
    extra_env: &[(&str, &str)],
) -> Output {
    let mut cmd = Command::new(niu_binary());
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().expect("spawn niu");
    if let Some(input) = stdin {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin piped")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    child.wait_with_output().expect("wait for niu")
}

/// The trap's observation file: exactly one `obs=<status>` line
/// (a second line would mean the trap ran twice).
fn assert_single_observation(dir: &Path, expected_status: i32) {
    let obs = std::fs::read_to_string(dir.join("obs.txt")).unwrap_or_else(|e| {
        panic!("obs.txt must exist after the finish (trap ran): {e}");
    });
    let lines: Vec<&str> = obs.lines().collect();
    assert_eq!(
        lines,
        vec![format!("obs={expected_status}").as_str()],
        "the EXIT trap must run exactly once and observe the route's pending status"
    );
}

fn assert_code(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "the trap's `exit 7` must decide the process code (stderr: {})",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------
// One-shot routes (binary-driven)
// ---------------------------------------------------------------------

#[test]
fn one_shot_c_route_trap_observes_status_and_decides_code() {
    let dir = unique_temp_dir("c");
    let script = format!("{TRAP}; (exit 3)");
    let output = run_niu_in(&dir, &["-c", &script], None);
    assert_code(&output, 7);
    assert_single_observation(&dir, 3);
}

#[test]
fn one_shot_script_route_trap_observes_status_and_decides_code() {
    let dir = unique_temp_dir("script");
    std::fs::write(dir.join("prog.sh"), format!("{TRAP}\n(exit 3)\n")).unwrap();
    let output = run_niu_in(&dir, &["prog.sh"], None);
    assert_code(&output, 7);
    assert_single_observation(&dir, 3);
}

#[test]
fn stdin_route_trap_observes_status_and_decides_code() {
    let dir = unique_temp_dir("stdin");
    let script = format!("{TRAP}\n(exit 3)\n");
    let output = run_niu_in(&dir, &[], Some(&script));
    assert_code(&output, 7);
    assert_single_observation(&dir, 3);
}

#[test]
fn startup_env_exit_route_trap_observes_status_and_decides_code() {
    let dir = unique_temp_dir("bashenv");
    // The startup file arms the trap and exits before the -c body:
    // finish_startup_exit runs the finish with the startup status.
    std::fs::write(dir.join("env.sh"), format!("{TRAP}\nexit 5\n")).unwrap();
    let env_path = dir.join("env.sh").to_string_lossy().into_owned();
    let output = run_niu_with_env(
        &dir,
        &["-c", "echo ran > ran.txt"],
        None,
        &[("BASH_ENV", &env_path)],
    );
    assert_code(&output, 7);
    assert_single_observation(&dir, 5);
    assert!(
        !dir.join("ran.txt").exists(),
        "the startup exit must pre-empt the -c body"
    );
}

// ---------------------------------------------------------------------
// REPL routes (PTY-driven)
// ---------------------------------------------------------------------

fn spawn_with_trap(prefix: &str) -> NiuSession {
    let rc = format!("{}\n{TRAP}\n", default_rc());
    let mut session = NiuSession::spawn_custom(prefix, &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    session.wait_ready();
    session
}

#[test]
fn repl_exit_route_trap_observes_status_and_decides_code() {
    if !require_pty_or_skip("repl_exit_route_trap_observes_status_and_decides_code") {
        return;
    }
    let mut session = spawn_with_trap("gap8-repl-exit");
    session.send_line("exit 4");
    assert_eq!(session.wait_exit(), 7);
    assert_single_observation(session.home(), 4);
}

#[test]
fn repl_eof_route_trap_observes_last_status_and_decides_code() {
    if !require_pty_or_skip("repl_eof_route_trap_observes_last_status_and_decides_code") {
        return;
    }
    let mut session = spawn_with_trap("gap8-repl-eof");
    session.send_line("(exit 3)");
    session.expect_prompt();
    // Let the prompt's two-phase paint finish before the EOF byte:
    // a Ctrl-D that lands mid-redraw is consumed as an edit event
    // instead of reaching read_line as the EOF signal.
    std::thread::sleep(std::time::Duration::from_millis(500));
    session.send(CTRL_D);
    assert_eq!(session.wait_exit(), 7);
    assert_single_observation(session.home(), 3);
}

#[test]
fn repl_noediting_eof_route_trap_observes_last_status_and_decides_code() {
    if !require_pty_or_skip("repl_noediting_eof_route_trap_observes_last_status_and_decides_code") {
        return;
    }
    let rc = format!("{}\n{TRAP}\n", default_rc());
    let mut session = NiuSession::spawn_custom_with_args(
        "gap8-noedit-eof",
        &rc,
        &["--noediting"],
        &[],
        (120, 30),
        DEFAULT_TIMEOUT,
    );
    session.expect_prompt();
    session.send_line("(exit 3)");
    session.expect_prompt();
    session.send(CTRL_D);
    assert_eq!(session.wait_exit(), 7);
    assert_single_observation(session.home(), 3);
}

/// niubash#646 over the guard: the REPL's session-end history
/// write consults the HIST* values as the EXIT trap left them —
/// a trap that rewrites HISTFILE governs where the provider
/// flush lands the session's lines.
#[test]
fn repl_history_flush_observes_post_trap_histfile() {
    if !require_pty_or_skip("repl_history_flush_observes_post_trap_histfile") {
        return;
    }
    let rc = format!(
        "{}\ntrap 'HISTFILE=\"$HOME/hist_b.txt\"' EXIT\n",
        default_rc()
    );
    let mut session =
        NiuSession::spawn_custom("gap8-hist646", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    session.wait_ready();
    session.send_line("echo gap8-history-marker");
    session.expect_prompt();
    session.send_line("exit");
    assert_eq!(session.wait_exit(), 0);

    let home = session.home().to_path_buf();
    let hist_b = std::fs::read_to_string(home.join("hist_b.txt")).unwrap_or_else(|e| {
        panic!("the provider flush must land in the trap-rewritten HISTFILE: {e}")
    });
    // The session-end write is the reconcile of the session's whole
    // list (both submitted lines), targeted at the post-trap
    // HISTFILE. Had the flush consulted the pre-trap settings, this
    // file would never have been created — that targeting is the
    // discriminating observable. (The pre-trap file legitimately
    // holds the same lines too: the provider syncs every save to
    // its then-current file during the session — the #773 design,
    // not the session-end write — so no absence is asserted there.)
    assert!(
        hist_b.contains("echo gap8-history-marker") && hist_b.contains("exit"),
        "the reconciled session list must be written to the post-trap HISTFILE, got: {hist_b}"
    );
}
