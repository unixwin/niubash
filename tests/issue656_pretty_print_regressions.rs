//! niubash#656 — `--pretty-print` under niu printed nothing at all
//! for time / subshell / background blocks: the host carried a mirror
//! of the engine binary's pretty-print loop whose emptiness table had
//! 16 fields (the engine's has 20), so whole commands of those kinds
//! were judged empty and swallowed, and the mirror had no parse-error
//! arm. The loop now lives in the engine library
//! (`rubash::parser::ast_print::pretty_print_input`, rubash#1977) and
//! this host calls it.
//!
//! Expectations measured against GNU bash 5.3.20 --pretty-print on
//! the same input (identical bytes to the rubash binary's output).

use std::fs;
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
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niu" });
    fallback
}

fn scratch(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "niubash-issue656-{tag}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_pretty_print(dir: &PathBuf, script_body: &str) -> (String, String, i32) {
    let script = dir.join("case.sh");
    fs::write(&script, script_body).unwrap();
    let output = Command::new(niu_binary())
        .arg("--pretty-print")
        .arg(&script)
        .current_dir(dir)
        .output()
        .expect("spawn niu --pretty-print");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// The three audited shapes: each printed as its canonical form
/// instead of vanishing.
#[test]
fn pretty_print_time_subshell_background_and_inverted_blocks() {
    let dir = scratch("blocks");
    let (stdout, _stderr, code) = run_pretty_print(
        &dir,
        "time echo hi\n(echo subshell)\necho bg &\n! echo inv\ntime -p echo t2\n",
    );
    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        "time echo hi\n( echo subshell )\necho bg &\n! echo inv\ntime -p echo t2\n\n"
    );
}

/// A plain command still prints (the mirror's non-empty path), so the
/// pin above cannot pass vacuously.
#[test]
fn pretty_print_plain_command_control() {
    let dir = scratch("plain");
    let (stdout, _stderr, code) = run_pretty_print(&dir, "echo plain\n");
    assert_eq!(code, 0);
    assert_eq!(stdout, "echo plain\n\n");
}

/// GNU pretty_print_loop returns EXECUTION_FAILURE when the input
/// stops at a parse error: prior commands have printed, the broken
/// command prints nothing (in particular not the engine's internal
/// parse-error marker assignments).
#[test]
fn pretty_print_parse_error_stops_with_rc1() {
    let dir = scratch("err");
    let (stdout, _stderr, code) = run_pretty_print(&dir, "echo before\nif then fi\necho after\n");
    assert_eq!(code, 1);
    assert_eq!(stdout, "echo before\n");
    assert!(!stdout.contains("__RUBASH_PARSE"));
}

/// niubash#656 flake guard: on Windows the commands printed above
/// travel through the piped-stdout pump (src/piped_stdout.rs,
/// niubash#245/#256), and the parse-error arm used to leave via a
/// bare `std::process::exit`, skipping the pump drain that `main`
/// runs for every returning path. Whether the pump forwarded the
/// buffered bytes before the process died was a scheduling race —
/// CI sampled the pin above red 2 of 4 runs, always with stdout
/// completely empty and the exit code still 1. The arm now leaves
/// through `piped_stdout::finish_and_exit`. One green run proves
/// nothing about a race, so repeat the shape: at the sampled ~1/2
/// per-run red rate a regression escapes 16 rounds with probability
/// ~1/65536.
#[test]
fn pretty_print_parse_error_output_survives_exit_repeatedly() {
    let dir = scratch("errloop");
    for round in 0..16 {
        let (stdout, _stderr, code) =
            run_pretty_print(&dir, "echo before\nif then fi\necho after\n");
        assert_eq!(code, 1, "round {round}");
        assert_eq!(stdout, "echo before\n", "round {round}");
    }
}
