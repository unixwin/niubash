//! unixwin/niubash#576: the host's own stdin driver (`run_stdin_script`)
//! treated ANY nonzero batch as process exit — the first failing command
//! ended the whole stream, later input never ran, and the EXIT trap fired
//! at the failure point instead of at EOF.
//!
//! GNU (shell.c read_from_stdin / reader_loop): without `-e`, a
//! non-interactive stdin script reads the stream to EOF, the process
//! status is the LAST command's status, and the EXIT trap runs exactly
//! once at the end. A batch that unwinds (top-level `exit`, errexit, a
//! syntax error) is what stops the stream early — an ordinary nonzero
//! status is not. The engine-driven `-s` route already behaved this way;
//! the bare-stdin host driver now matches it.
//!
//! Oracle: GNU bash 5.3.20, verified 2026-10-26 (cluster gnu-expectations).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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

fn run_stdin(input: &[u8]) -> (Vec<u8>, Vec<u8>, i32) {
    let dir = std::env::temp_dir().join(format!(
        "niu-issue576-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let mut child = Command::new(niu_binary())
        .current_dir(&dir)
        .env("HOME", &dir)
        .env("USERPROFILE", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input)
        .expect("feed stdin script");
    let output = child.wait_with_output().expect("wait niu");
    let _ = std::fs::remove_dir_all(&dir);
    (
        output.stdout,
        output.stderr,
        output.status.code().unwrap_or(-1),
    )
}

fn stdout_text(input: &[u8]) -> (String, String, i32) {
    let (out, err, rc) = run_stdin(input);
    (
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
        rc,
    )
}

#[test]
fn issue576_nonzero_batch_does_not_end_stream() {
    let (out, err, rc) = stdout_text(b"false\necho SHOULD_PRINT\n");
    assert_eq!(rc, 0, "last command decides the status; stderr: {err:?}");
    assert!(out.contains("SHOULD_PRINT"), "stdout: {out:?}");
}

#[test]
fn issue576_last_batch_status_wins() {
    let (out, err, rc) = stdout_text(b"false\necho SHOULD_PRINT\nfalse\n");
    assert_eq!(rc, 1, "stderr: {err:?}");
    assert!(out.contains("SHOULD_PRINT"), "stdout: {out:?}");
}

#[test]
fn issue576_exit_trap_runs_once_at_eof_not_at_first_failure() {
    let (out, err, rc) = stdout_text(b"trap 'echo TRAPPED' EXIT\nfalse\necho AFTER\n");
    assert_eq!(rc, 0, "stderr: {err:?}");
    let trapped = out.matches("TRAPPED").count();
    assert_eq!(trapped, 1, "EXIT trap exactly once; stdout: {out:?}");
    assert!(
        out.find("AFTER").unwrap() < out.find("TRAPPED").unwrap(),
        "trap output comes after the whole stream ran; stdout: {out:?}"
    );
}

#[test]
fn issue576_top_level_exit_still_stops_stream() {
    let (out, err, rc) = stdout_text(b"echo A\nexit 3\necho NO\n");
    assert_eq!(rc, 3, "stderr: {err:?}");
    assert!(out.contains('A'), "stdout: {out:?}");
    assert!(
        !out.contains("NO"),
        "input after exit must not run; stdout: {out:?}"
    );
}

#[test]
fn issue576_exit_runs_trap_once_and_stops() {
    let (out, err, rc) = stdout_text(b"trap 'echo TRAPPED' EXIT\nexit 3\necho NO\n");
    assert_eq!(rc, 3, "stderr: {err:?}");
    assert_eq!(out.matches("TRAPPED").count(), 1, "stdout: {out:?}");
    assert!(!out.contains("NO"), "stdout: {out:?}");
}

#[test]
fn issue576_errexit_unwind_stops_stream() {
    let (out, err, rc) = stdout_text(b"set -e\nfalse\necho NO\n");
    assert_eq!(rc, 1, "stderr: {err:?}");
    assert!(!out.contains("NO"), "stdout: {out:?}");
}

#[test]
fn issue576_syntax_error_stops_stream_with_2() {
    let (out, err, rc) = stdout_text(b"echo before\n)\necho after\n");
    assert_eq!(rc, 2, "stderr: {err:?}");
    assert!(out.contains("before"), "stdout: {out:?}");
    assert!(!out.contains("after"), "stdout: {out:?}");
}

#[test]
fn issue576_pending_flushed_at_eof_keeps_its_status() {
    // No trailing newline: the final line is only executed by the EOF
    // flush of the gathered batch; its status is the process status.
    let (out, err, rc) = stdout_text(b"echo hi\nfalse");
    assert_eq!(rc, 1, "stderr: {err:?}");
    assert!(out.contains("hi"), "stdout: {out:?}");

    let (out, err, rc) = stdout_text(b"trap 'echo TRAPPED' EXIT\nfalse");
    assert_eq!(rc, 1, "stderr: {err:?}");
    assert_eq!(out.matches("TRAPPED").count(), 1, "stdout: {out:?}");
}

#[test]
fn issue576_multiline_gather_unaffected() {
    let (out, err, rc) = stdout_text(b"echo start\nif true; then\necho INIF\nfi\necho done\n");
    assert_eq!(rc, 0, "stderr: {err:?}");
    for want in ["start", "INIF", "done"] {
        assert!(out.contains(want), "missing {want}; stdout: {out:?}");
    }
    // A compound command spanning the whole stream, nonzero at EOF flush.
    let (_out, err, rc) = stdout_text(b"if true; then\nfalse\nfi");
    assert_eq!(rc, 1, "stderr: {err:?}");
}
