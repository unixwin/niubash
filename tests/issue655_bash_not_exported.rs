//! niubash#655: niu exported BASH into the environment; rubash and
//! GNU do not. GNU seeds $BASH as a plain (unexported) shell variable
//! (variables.c), and rubash's own main does the same deliberately
//! (rubash#1325). The host seed used `export_env`, so every child
//! process saw BASH=<niu path> in its environment (audit probe P8).

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
        .env_remove("BASH")
        .output()
        .expect("spawn niu -c");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// The child environment must not contain an exported BASH.
#[test]
fn bash_not_in_child_environment() {
    let (stdout, _stderr, code) = run_c("env");
    assert_eq!(code, 0);
    assert!(
        !stdout.lines().any(|line| line.starts_with("BASH=")),
        "BASH must not be exported, env was:\n{stdout}"
    );
}

/// ... while the $BASH shell variable itself stays set inside the
/// shell (scripts probe it to detect bash).
#[test]
fn bash_variable_still_set_in_shell() {
    let (stdout, _stderr, code) = run_c("echo ${BASH:+is-set}");
    assert_eq!(code, 0);
    assert_eq!(stdout, "is-set\n");
}
