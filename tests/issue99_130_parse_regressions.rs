//! niubash#99 / #130 parse regressions.
//!
//! - #99: an escaped quote inside a double-quoted word was dropped —
//!   `echo "1\""` printed `1` instead of GNU's `1"`, and `echo "1\"2"`
//!   printed `12` instead of `1"2`.
//! - #130: a `#` comment on the same line as a function definition's
//!   opening brace made the whole script fail with `syntax error near
//!   unexpected token '('` — both the multi-line form
//!   (`f() { # note` …) and the one-line form (`f() { echo x; } # note`).
//!   Other compound commands tolerated trailing comments; only the
//!   function-header brace line broke.

use std::path::PathBuf;
use std::process::Command;

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

fn run_c(script: &str) -> String {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(script)
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niu");
    assert!(
        output.status.success(),
        "niu -c failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_file(script: &str, tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("niu-w2-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("repro.sh");
    std::fs::write(&path, script).unwrap();
    let output = Command::new(niu_binary())
        .arg(&path)
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niu");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn issue99_escaped_quote_inside_double_quotes_survives() {
    let out = run_c("echo \"1\\\"\"\necho \"1\\\"2\"");
    assert_eq!(out, "1\"\n1\"2\n");
}

#[test]
fn issue130_comment_after_function_opening_brace() {
    let out = run_file("f() { # note\n  echo x\n}\nf\n", "i130a");
    assert_eq!(out, "x\n");
}

#[test]
fn issue130_comment_after_one_line_function() {
    let out = run_file("f() { echo x; } # note\nf\n", "i130b");
    assert_eq!(out, "x\n");
}
