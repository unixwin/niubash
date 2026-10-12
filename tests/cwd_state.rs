//! Contract tests for `--cwd-state`, the directory memory an agent host uses
//! to make a chain of one-shot `niu -c` invocations behave like one shell.
//!
//! The option is explicit on purpose: a plain `niu -c` must keep the inherited
//! working directory and touch no state file. The tests below pin both the
//! resume/record behavior and that negative contract, using builtins only so
//! the fast test suite never needs winuxcmd command links.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// One scratch directory per test, so parallel test threads never share state.
fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "niu-cwd-state-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        label
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create scratch directory");
    path
}

/// Run niu from `cwd` with an explicit argv, returning stdout, stderr, and the
/// exit code.
fn run_niu(cwd: &Path, args: &[&str]) -> (String, String, i32) {
    let output = Command::new(niu_binary())
        .args(args)
        .current_dir(cwd)
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niubash");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

/// Shell `pwd` output and a host path differ in separator and case on Windows;
/// compare both normalized.
fn normalize(path: &str) -> String {
    path.trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// The directory recorded in a state file, if the file holds valid JSON.
fn recorded_cwd(state: &Path) -> Option<String> {
    let content = std::fs::read_to_string(state).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).expect("state file is JSON");
    Some(value.get("cwd")?.as_str()?.to_string())
}

fn state_arg(path: &Path) -> String {
    path.to_str().expect("state path is UTF-8").to_string()
}

#[test]
fn cwd_state_records_then_resumes_the_directory() {
    let scratch = scratch("roundtrip");
    let state = scratch.join("state.json");
    let target = scratch.join("target-dir");
    std::fs::create_dir_all(&target).unwrap();
    let target_shell = target.to_str().unwrap().replace('\\', "/");

    let (stdout, _, code) = run_niu(
        &scratch,
        &[
            "--cwd-state",
            &state_arg(&state),
            "-c",
            &format!("cd {target_shell}; pwd"),
        ],
    );
    assert_eq!(code, 0);
    assert_eq!(normalize(&stdout), normalize(&target_shell));
    assert_eq!(
        recorded_cwd(&state).as_deref().map(normalize),
        Some(normalize(&target_shell)),
        "the first invocation must record where it ended"
    );

    // The second invocation inherits the scratch directory but must resume the
    // recorded one.
    let (stdout, _, code) = run_niu(&scratch, &["--cwd-state", &state_arg(&state), "-c", "pwd"]);
    assert_eq!(code, 0);
    assert_eq!(normalize(&stdout), normalize(&target_shell));
}

#[test]
fn cwd_state_records_even_when_the_command_fails() {
    let scratch = scratch("failing");
    let state = scratch.join("state.json");
    let target = scratch.join("failed-into");
    std::fs::create_dir_all(&target).unwrap();
    let target_shell = target.to_str().unwrap().replace('\\', "/");

    let (_, _, code) = run_niu(
        &scratch,
        &[
            "--cwd-state",
            &state_arg(&state),
            "-c",
            &format!("cd {target_shell}; false"),
        ],
    );
    assert_eq!(code, 1, "the command's own exit code is preserved");
    assert_eq!(
        recorded_cwd(&state).as_deref().map(normalize),
        Some(normalize(&target_shell)),
        "a failing command must still record, or the first failure loses the directory"
    );
}

#[test]
fn cwd_state_includes_an_exit_trap_directory_change() {
    let scratch = scratch("exit-trap");
    let state = scratch.join("state.json");
    let target = scratch.join("trap-dir");
    std::fs::create_dir_all(&target).unwrap();
    let target_shell = target.to_str().unwrap().replace('\\', "/");

    let (_, _, code) = run_niu(
        &scratch,
        &[
            "--cwd-state",
            &state_arg(&state),
            "-c",
            &format!("trap 'cd {target_shell}' EXIT; true"),
        ],
    );
    assert_eq!(code, 0);
    assert_eq!(
        recorded_cwd(&state).as_deref().map(normalize),
        Some(normalize(&target_shell)),
        "recording runs after the EXIT trap, so the trap's directory wins"
    );
}

#[test]
fn cwd_state_does_not_leak_a_subshell_directory_change() {
    let scratch = scratch("subshell");
    let state = scratch.join("state.json");
    let other = scratch.join("other-dir");
    std::fs::create_dir_all(&other).unwrap();
    let other_shell = other.to_str().unwrap().replace('\\', "/");

    let (_, _, code) = run_niu(
        &scratch,
        &[
            "--cwd-state",
            &state_arg(&state),
            "-c",
            &format!("( cd {other_shell}; pwd )"),
        ],
    );
    assert_eq!(code, 0);
    assert_eq!(
        recorded_cwd(&state).as_deref().map(normalize),
        Some(normalize(scratch.to_str().unwrap())),
        "a subshell's cd must not become the recorded directory"
    );
}

#[test]
fn cwd_state_falls_back_when_the_recorded_directory_is_gone() {
    let scratch = scratch("stale");
    let state = scratch.join("state.json");
    std::fs::write(
        &state,
        r#"{"cwd":"D:/niu-cwd-state-does-not-exist","updatedAt":1}"#,
    )
    .unwrap();

    let (stdout, _, code) = run_niu(&scratch, &["--cwd-state", &state_arg(&state), "-c", "pwd"]);
    assert_eq!(code, 0, "a stale state file is never a command failure");
    assert_eq!(normalize(&stdout), normalize(scratch.to_str().unwrap()));
    assert_eq!(
        recorded_cwd(&state).as_deref().map(normalize),
        Some(normalize(scratch.to_str().unwrap())),
        "the run replaces the stale record, so the state heals itself"
    );
}

#[test]
fn cwd_state_reads_a_hand_written_bare_path() {
    let scratch = scratch("bare-path");
    let state = scratch.join("state.json");
    let target = scratch.join("hand-written");
    std::fs::create_dir_all(&target).unwrap();
    let target_shell = target.to_str().unwrap().replace('\\', "/");
    std::fs::write(&state, format!("{target_shell}\n")).unwrap();

    let (stdout, _, code) = run_niu(&scratch, &["--cwd-state", &state_arg(&state), "-c", "pwd"]);
    assert_eq!(code, 0);
    assert_eq!(normalize(&stdout), normalize(&target_shell));
}

#[test]
fn cwd_state_creates_missing_parent_directories() {
    let scratch = scratch("missing-parent");
    let state = scratch.join("deep").join("nested").join("state.json");

    let (_, _, code) = run_niu(&scratch, &["--cwd-state", &state_arg(&state), "-c", "true"]);
    assert_eq!(code, 0);
    assert!(state.is_file(), "the recorder creates its own directories");
}

#[test]
fn cwd_state_is_opt_in() {
    let scratch = scratch("opt-in");
    let state = scratch.join("state.json");

    let (stdout, _, code) = run_niu(&scratch, &["-c", "pwd"]);
    assert_eq!(code, 0);
    assert_eq!(normalize(&stdout), normalize(scratch.to_str().unwrap()));
    assert!(
        !state.exists(),
        "without --cwd-state niu must not create or touch a state file"
    );
}

#[test]
fn cwd_state_does_not_consume_lookalike_positional_parameters() {
    let scratch = scratch("positional");
    let state = scratch.join("state.json");

    // `-c <cmd> <name> <params...>`: the trailing `--cwd-state` is a positional
    // parameter, not an option, because the option scan stops at `-c`.
    let (stdout, _, code) = run_niu(
        &scratch,
        &[
            "--cwd-state",
            &state_arg(&state),
            "-c",
            "echo \"p1=$1\"",
            "niu",
            "--cwd-state",
        ],
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("p1=--cwd-state"),
        "positional parameters must survive the option scan: {stdout}"
    );
}

#[test]
fn cwd_state_resolves_a_relative_state_path_once() {
    let scratch = scratch("relative-state");
    let other = scratch.join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    let other_shell = other.to_str().unwrap().replace('\\', "/");

    // The record is written after the command moved the shell, so a relative
    // state path has to stay pinned to the directory niu was started in.
    let (stdout, _, code) = run_niu(
        &scratch,
        &[
            "--cwd-state",
            "relative.json",
            "-c",
            &format!("cd {other_shell}; pwd"),
        ],
    );
    assert_eq!(code, 0);
    assert_eq!(normalize(&stdout), normalize(&other_shell));
    assert_eq!(
        recorded_cwd(&scratch.join("relative.json"))
            .as_deref()
            .map(normalize),
        Some(normalize(&other_shell)),
        "a relative state path must not follow the command's cd"
    );
    assert!(
        !other.join("relative.json").exists(),
        "the record must not land beside the directory the command moved into"
    );
}

#[test]
fn cwd_state_requires_its_argument() {
    let scratch = scratch("missing-argument");
    let (_, stderr, code) = run_niu(&scratch, &["--cwd-state"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("--cwd-state"),
        "the error must name the option: {stderr}"
    );
}
