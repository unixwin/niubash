//! unixwin/rubash#2860 (host half of niubash#431's arm retirement):
//! the stray-close arm of `Shell::execute_script_inner` was the
//! host's last direct `script_driver` call; it now hands tripped
//! inputs to `Session::run` like the unclosed and history/alias
//! arms above it. These pins hold the arm's observable contract
//! across the migration:
//!
//! * the offending line echoes VERBATIM even though the session
//!   is named (the `-c` name, the script file) — the engine half
//!   of #2860 stopped `Session::run` from handing the parse the
//!   source NAME as its diagnostic text, which echoed the name
//!   itself (`` `<name>' ``) or a token reconstruction;
//! * GNU's line-unit prefix rule (parse.y:6814
//!   print_offending_line; the #1138 unit voiding): a complete
//!   earlier line runs, the offending line's own commands —
//!   including complete ones before the stray token — never do;
//! * a stdin stream STOPS at the tripped batch: `Session::run`
//!   drains the executor pendings into its outcome, so the arm
//!   folds the outcome's stop facts (`exit_jump_raw ||
//!   parse_error`) into the host jump record the batch feeder
//!   reads — without the fold the feeder would read past the
//!   error and run later input.
//!
//! The #431 file keeps pinning the original shapes; this file
//! pins the migration-specific surfaces (multi-line `-c`, the
//! feeder stop, the named script file).

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

fn run_c(command: &str) -> (String, String, i32) {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(command)
        .output()
        .expect("spawn niu -c");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// Feed `input` to niu as a non-interactive stdin script (the
/// no-flag `niu` route: GNU shell.c:780-786 read_from_stdin).
fn run_stdin_script(input: &str) -> (String, String, i32) {
    let dir = std::env::temp_dir().join(format!("niu-issue2860-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let input_path = dir.join("input.sh");
    std::fs::write(&input_path, input).expect("write stdin input");
    let stdin = std::fs::File::open(&input_path).expect("open stdin input");
    let output = Command::new(niu_binary())
        .stdin(Stdio::from(stdin))
        .output()
        .expect("spawn niu stdin script");
    let result = (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    );
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Multi-line `-c`: the complete first line runs, the offending
/// second line echoes verbatim under the `-c` name, rc 2.
#[test]
fn c_multiline_prefix_runs_and_echo_is_verbatim() {
    let (stdout, stderr, code) = run_c("echo ok\necho )bad");
    assert_eq!(stdout, "ok\n", "the complete earlier line runs");
    assert_eq!(
        stderr,
        concat!(
            "niu: -c: line 2: syntax error near unexpected token `)'\n",
            "niu: -c: line 2: `echo )bad'\n"
        ),
        "the echo is the input line, not the `-c` name or a reconstruction"
    );
    assert_eq!(code, 2);
}

/// Same-line prefix: GNU's reader unit is the line, so `echo ok`
/// before the stray token on the SAME line never runs — on either
/// of the contract's routes.
#[test]
fn c_same_line_prefix_never_runs() {
    let (stdout, stderr, code) = run_c("echo ok; echo )bad");
    assert_eq!(stdout, "", "no same-line prefix execution");
    assert_eq!(
        stderr,
        concat!(
            "niu: -c: line 1: syntax error near unexpected token `)'\n",
            "niu: -c: line 1: `echo ok; echo )bad'\n"
        )
    );
    assert_eq!(code, 2);
}

/// The feeder stop: a tripped batch ends the stdin stream (GNU
/// reader_loop unwinds on a read-time parse error). Later input
/// must not run — this is what the arm's classification fold
/// into `script_exit_jump` carries after the migration.
#[test]
fn stdin_stray_close_stops_the_stream() {
    let (stdout, stderr, code) = run_stdin_script("echo a;;\necho after\n");
    assert_eq!(stdout, "", "`after` must never run: {stdout:?}");
    assert!(
        stderr.contains(": line 1: syntax error near unexpected token `;;'\n")
            && stderr.contains("`echo a;;'\n"),
        "verbatim echo on the tripped batch: {stderr:?}"
    );
    assert_eq!(code, 2);
}

/// The named script file: the diagnostic names the script, the
/// echo is the input's line — never the script's own name echoed
/// as the offending line (the pre-#2860 `Session::run` defect
/// this migration's engine half fixes).
#[test]
fn script_file_named_source_echoes_the_input_line() {
    let dir = std::env::temp_dir().join(format!("niu-issue2860-f-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let script = dir.join("stray2860.sh");
    std::fs::write(&script, b"echo ok\necho )bad\n").expect("write script");

    let output = Command::new(niu_binary())
        .arg(&script)
        .output()
        .expect("spawn niu script");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(": line 2: syntax error near unexpected token `)'\n")
            && stderr.contains("`echo )bad'\n"),
        "verbatim echo under the script's name: {stderr:?}"
    );
    assert!(
        !stderr.contains("`stray2860.sh'"),
        "the script name must not be echoed as the offending line: {stderr:?}"
    );
    assert_eq!(output.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}
