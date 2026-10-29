//! niubash#694: the one-shot text faces fed by `invocation_input`
//! (`--dump-strings`, `--dump-po-strings`, `--pretty-print`) read
//! their script operand through an anyhow chain, so a missing
//! file landed as a bare rc 1 with no diagnostic. GNU (and niu's
//! own execution path, niubash#591) answer an open failure with
//! the execution caliber: 127 + `No such file or directory` for
//! a missing file, 126 for a directory or a permission failure,
//! each naming the operand as given — and read the bytes, so a
//! non-UTF-8 script is dumped/pretty-printed, not refused.
//!
//! These pins drive the real binary per face: missing operand,
//! directory operand, an existing file (no-regression), a
//! Latin-1 file, a slash-less name found through $PATH, the
//! stdin byte face, and the `-c` control face.

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

fn run(args: &[&str]) -> (String, String, i32) {
    let output = Command::new(niu_binary())
        .args(args)
        .output()
        .expect("spawn niu");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("niu-issue694-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// The stderr line with its terminator stripped (the line's
/// bytes are the pin; the terminator spelling is the pipe's).
fn stderr_line(stderr: &str) -> &str {
    stderr.trim_end_matches(['\r', '\n'])
}

#[test]
fn dump_strings_missing_file_is_127_with_diagnostic() {
    let dir = temp_dir("dump-missing");
    let missing = dir.join("no-such-script.sh");
    let name = missing.to_string_lossy().into_owned();
    let (_stdout, stderr, code) = run(&["--dump-strings", &name]);
    assert_eq!(code, 127, "stderr was: {stderr}");
    assert_eq!(
        stderr_line(&stderr),
        format!("niu: {name}: No such file or directory")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dump_po_strings_missing_file_is_127_with_diagnostic() {
    let dir = temp_dir("dumppo-missing");
    let missing = dir.join("no-such-script.sh");
    let name = missing.to_string_lossy().into_owned();
    let (_stdout, stderr, code) = run(&["--dump-po-strings", &name]);
    assert_eq!(code, 127, "stderr was: {stderr}");
    assert_eq!(
        stderr_line(&stderr),
        format!("niu: {name}: No such file or directory")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pretty_print_missing_file_is_127_with_diagnostic() {
    let dir = temp_dir("pretty-missing");
    let missing = dir.join("no-such-script.sh");
    let name = missing.to_string_lossy().into_owned();
    let (_stdout, stderr, code) = run(&["--pretty-print", &name]);
    assert_eq!(code, 127, "stderr was: {stderr}");
    assert_eq!(
        stderr_line(&stderr),
        format!("niu: {name}: No such file or directory")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dump_strings_directory_is_126_with_diagnostic() {
    let dir = temp_dir("dump-dir");
    let name = dir.to_string_lossy().into_owned();
    let (_stdout, stderr, code) = run(&["--dump-strings", &name]);
    assert_eq!(code, 126, "stderr was: {stderr}");
    assert_eq!(stderr_line(&stderr), format!("niu: {name}: Is a directory"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pretty_print_directory_is_126_with_diagnostic() {
    let dir = temp_dir("pretty-dir");
    let name = dir.to_string_lossy().into_owned();
    let (_stdout, stderr, code) = run(&["--pretty-print", &name]);
    assert_eq!(code, 126, "stderr was: {stderr}");
    assert_eq!(stderr_line(&stderr), format!("niu: {name}: Is a directory"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dump_strings_existing_file_still_dumps() {
    let dir = temp_dir("dump-ok");
    let script = dir.join("real.sh");
    std::fs::write(&script, "echo $\"hello\"\n").expect("write script");
    let name = script.to_string_lossy().into_owned();
    let (stdout, _stderr, code) = run(&["--dump-strings", &name]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"hello\""), "dump output was: {stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pretty_print_existing_file_still_prints() {
    let dir = temp_dir("pretty-ok");
    let script = dir.join("real.sh");
    std::fs::write(&script, "echo hello\n").expect("write script");
    let name = script.to_string_lossy().into_owned();
    let (stdout, _stderr, code) = run(&["--pretty-print", &name]);
    assert_eq!(code, 0);
    assert!(stdout.contains("echo hello"), "pretty output was: {stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dump_strings_latin1_file_dumps_instead_of_failing_the_read() {
    let dir = temp_dir("dump-latin1");
    let script = dir.join("latin1.sh");
    std::fs::write(&script, b"echo caf\xe9 $\"oui\"\n").expect("write script");
    let name = script.to_string_lossy().into_owned();
    let (stdout, stderr, code) = run(&["--dump-strings", &name]);
    assert_eq!(code, 0, "stderr was: {stderr}");
    assert!(stdout.contains("\"oui\""), "dump output was: {stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pretty_print_latin1_file_prints_instead_of_failing_the_read() {
    let dir = temp_dir("pretty-latin1");
    let script = dir.join("latin1.sh");
    std::fs::write(&script, b"echo caf\xe9 $\"oui\"\n").expect("write script");
    let name = script.to_string_lossy().into_owned();
    let (stdout, stderr, code) = run(&["--pretty-print", &name]);
    assert_eq!(code, 0, "stderr was: {stderr}");
    assert!(stdout.contains("oui"), "pretty output was: {stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dump_strings_finds_a_slashless_name_on_path() {
    let dir = temp_dir("dump-path");
    std::fs::write(dir.join("onpath.sh"), "echo $\"frompath\"\n").expect("write script");
    let mut paths = vec![dir.clone()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    let joined = std::env::join_paths(paths).expect("join PATH");
    let output = Command::new(niu_binary())
        .args(["--dump-strings", "onpath.sh"])
        .env("PATH", joined)
        .current_dir(std::env::temp_dir())
        .output()
        .expect("spawn niu");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code().unwrap_or(-1), 0);
    assert!(stdout.contains("\"frompath\""), "dump output was: {stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dump_strings_stdin_latin1_dumps_instead_of_failing_the_read() {
    let mut child = Command::new(niu_binary())
        .arg("--dump-strings")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(b"echo caf\xe9 $\"oui\"\n")
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait niu");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code().unwrap_or(-1), 0);
    assert!(stdout.contains("\"oui\""), "dump output was: {stdout}");
}

#[test]
fn dump_strings_command_string_face_is_unchanged() {
    let (stdout, _stderr, code) = run(&["--dump-strings", "-c", "echo $\"viarc\""]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"viarc\""), "dump output was: {stdout}");
}
