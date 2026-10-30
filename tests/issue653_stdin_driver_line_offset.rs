//! niubash#653: the host stdin driver lost physical line numbers.
//! `execute_script_inner`'s unclosed-syntax / heredoc gate arm called
//! the engine's `run_source_with_line_offset` with a hardcoded offset
//! of 0 instead of the batch's real `line_offset` (the tracked
//! history arm in the same function passes it correctly), so a bare
//! `niu < script` reported an unclosed quote on physical line 3 as
//! "line 1". `niu -s` and rubash both report line 3 (audit probe P4b).

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

fn run_bare_stdin(input: &str) -> (String, String, i32) {
    let mut child = Command::new(niu_binary())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait niu");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// The audit P4b shape: two good lines, then an unclosed quote on
/// physical line 3. The diagnostic must name line 3, not line 1.
#[test]
fn bare_stdin_unclosed_quote_reports_physical_line() {
    let (stdout, stderr, code) = run_bare_stdin("echo a\necho b\necho \"x\n");
    assert_eq!(stdout, "a\nb\n", "earlier lines still run: {stdout:?}");
    assert_eq!(code, 2, "unclosed quote is a read-time error: {stderr:?}");
    assert!(
        stderr.contains("line 3"),
        "diagnostic must report physical line 3, stderr was: {stderr:?}"
    );
    assert!(
        !stderr.contains("line 1"),
        "diagnostic must not collapse to line 1, stderr was: {stderr:?}"
    );
}

/// The heredoc half of the same gate arm: a heredoc delimiter with an
/// unclosed quote on physical line 2 (after one consumed line, so the
/// batch offset is nonzero) must also report its physical line.
#[test]
fn bare_stdin_heredoc_gate_reports_physical_line() {
    let (stdout, stderr, code) = run_bare_stdin("echo a\ncat <<'EO\"F\n");
    assert_eq!(stdout, "a\n", "the consumed line still runs: {stdout:?}");
    assert_ne!(
        code, 0,
        "unclosed heredoc delimiter quote must fail: {stderr:?}"
    );
    assert!(
        stderr.contains("line 2"),
        "diagnostic must report physical line 2, stderr was: {stderr:?}"
    );
}
