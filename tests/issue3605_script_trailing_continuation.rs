//! rubash#3605 / run-parser parser1.sub: a script-final `\<newline>`
//! is a line continuation that joins with end-of-input — GNU
//! parse.y:2842 shell_getc elides the pair and keeps reading past the
//! physical line end, so `echo AAA\`+`\n` prints `AAA`. Only a
//! backslash sitting at bare EOF stays literal (parse.y:2739-2743).
//!
//! execute_script_inner's script.trim() erased the trailing newline
//! and turned the first form into the second. The pin covers both
//! faces plus the regression that motivated the trim (leading blank
//! lines still charge line_offset).

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push("niu");
    fallback
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_script(body: &str) -> (String, String, i32) {
    let dir = unique_temp_dir("niu-trim");
    let script = dir.join("probe.sh");
    std::fs::write(&script, body).unwrap();
    let output = Command::new(niu_binary())
        .arg(&script)
        .env("HOME", &dir)
        .env("USERPROFILE", &dir)
        .env("TERM", "dumb")
        .env_remove("BASH_ENV")
        .env_remove("NIU_ENV")
        .env_remove("NIU_INVOKED_AS")
        .env_remove("BASH_ARGV0")
        .output()
        .expect("run niu");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let status = output.status.code().unwrap_or(-1);
    let _ = std::fs::remove_dir_all(&dir);
    (stdout, stderr, status)
}

#[test]
fn terminated_trailing_backslash_joins_with_eof() {
    let (stdout, stderr, _) = run_script("echo AAA\\\n");
    assert_eq!(stdout, "AAA\n", "stderr: {stderr}");
}

#[test]
fn mid_script_backslash_newline_still_joins() {
    let (stdout, stderr, _) = run_script("echo AAA\\\necho BBB\n");
    assert_eq!(stdout, "AAAecho BBB\n", "stderr: {stderr}");
}

#[test]
fn unterminated_trailing_backslash_stays_literal() {
    let (stdout, stderr, _) = run_script("echo AAA\\");
    assert_eq!(stdout, "AAA\\\n", "stderr: {stderr}");
}

#[test]
fn leading_blank_lines_still_charge_line_offset() {
    let (stdout, stderr, _) = run_script("\n\ndo echo x\n");
    assert_eq!(stdout, "", "unexpected stdout: {stdout}");
    assert!(
        stderr.contains("line 3"),
        "leading blanks must charge line_offset, got stderr: {stderr}"
    );
}
