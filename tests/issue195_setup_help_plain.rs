//! niubash#195: `niu setup` output must degrade to plain text when stdout
//! is not a terminal. `--help` answers in plain text everywhere (piped or
//! not), and the non-tty wizard banner drops the ANSI pixel-art logo.
use std::path::PathBuf;
use std::process::{Command, Output};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niubash" });
    fallback
}

fn run_niu(args: &[&str]) -> Output {
    Command::new(niu_binary())
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to run niubash {args:?}: {err}"))
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn noninteractive_setup_welcome_degrades_to_plain_text() {
    // A piped `niu setup` used to paint the full ANSI pixel-art logo
    // (niubash#195). With stdout not a tty the welcome banner must be
    // plain text while the wizard still completes.
    let temp = std::env::temp_dir().join(format!(
        "niubash-issue195-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&temp);
    let home = temp.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(niu_binary())
        .args(["setup"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("NIU_PLUGIN_SOURCES_ROOT", temp.join("sources"))
        .env("NIU_LANG", "en")
        .output()
        .expect("run niu setup");
    assert!(
        output.status.success(),
        "non-interactive setup failed:\n{}",
        stdout_text(&output)
    );
    let stdout = stdout_text(&output);
    assert!(
        !stdout.contains('\u{1b}'),
        "non-tty setup output must contain no ANSI escape sequences, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Welcome to Niubash"),
        "welcome text should survive the plain-text degradation, got:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn setup_help_is_plain_text_and_never_prints_ansi_art() {
    let output = run_niu(&["setup", "--help"]);
    assert!(
        output.status.success(),
        "setup --help failed: {}",
        output.status
    );
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("niu setup"),
        "--help should print plain-text usage, got:\n{stdout}"
    );
    assert!(
        !stdout.contains('\u{1b}'),
        "--help output must contain no ANSI escape sequences, got:\n{stdout}"
    );
}

#[test]
fn setup_help_shorthand_h_is_plain_text_too() {
    let output = run_niu(&["setup", "-h"]);
    assert!(
        output.status.success(),
        "setup -h failed: {}",
        output.status
    );
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("Usage"),
        "-h should print usage, got:\n{stdout}"
    );
    assert!(
        !stdout.contains('\u{1b}'),
        "-h output must contain no ANSI escape sequences, got:\n{stdout}"
    );
}
