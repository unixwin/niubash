//! niubash#667: the prompt terminal fallback seeded COLUMNS=80 through
//! the exporting env write, so every child process inherited a phantom
//! COLUMNS=80 even with a real terminal present. GNU bash keeps COLUMNS
//! a plain shell variable: the shell maintains it for its own prompt
//! math, and it reaches a child's environment only when the user
//! exports it. The seed must stay (the prompt width fallback needs the
//! value in-shell) but must not carry the export attribute — the same
//! shape as the niubash#655 BASH seed.

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

fn run_c(command: &str) -> (String, String, i32) {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(command)
        .env_remove("COLUMNS")
        .output()
        .expect("spawn niu -c");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// The child environment must not contain an exported COLUMNS.
#[test]
fn columns_not_in_child_environment() {
    let (stdout, _stderr, code) = run_c("env");
    assert_eq!(code, 0);
    assert!(
        !stdout.lines().any(|line| line.starts_with("COLUMNS=")),
        "COLUMNS must not be exported, env was:\n{stdout}"
    );
}

/// ... while the $COLUMNS shell variable itself keeps the seeded
/// fallback value in-shell (the prompt width path reads it).
#[test]
fn columns_variable_still_seeded_in_shell() {
    let (stdout, _stderr, code) = run_c("echo $COLUMNS");
    assert_eq!(code, 0);
    assert_eq!(stdout, "80\n");
}

/// The user's own export state is respected both ways: an explicit
/// `export` reaches the child, a plain assignment stays unexported
/// (the seeded fallback must behave like the plain assignment).
#[test]
fn user_columns_export_state_is_respected() {
    let (stdout, _stderr, code) = run_c("export COLUMNS=200; env");
    assert_eq!(code, 0);
    assert!(
        stdout.lines().any(|line| line == "COLUMNS=200"),
        "an explicitly exported COLUMNS must reach the child, env was:\n{stdout}"
    );

    let (stdout, _stderr, code) = run_c("COLUMNS=200; env");
    assert_eq!(code, 0);
    assert!(
        !stdout.lines().any(|line| line.starts_with("COLUMNS=")),
        "a plain COLUMNS assignment must stay unexported, env was:\n{stdout}"
    );
}
