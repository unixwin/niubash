//! niubash#191: the REPL must CONSUME the engine's re-armed PROMPT_COMMAND
//! exit jump instead of re-arming the prompt. The engine half shipped in
//! rubash#433 (wt99): `run_prompt_command_text` re-arms `exit_jump_pending`
//! when the PC text raises a top-level unwind (`exit`, or the errexit break;
//! evalstring.c:396-403 + :618-619 re-raise after the `out:` cleanup), and
//! the engine's own `run_interactive_stdin` reader consumes it. The niu-side
//! gap was the product REPL: `run_bash_prompt_command` dropped the re-armed
//! state, so a `PROMPT_COMMAND='exit'` session kept prompting forever.
//!
//! GNU anchors (bash.git @b4608166, Bash-5.3 patch 15):
//! - the jump unwinds reader_loop: no further prompt, no further read, and
//!   the jump's status is the session status (`exit 5` -> rc 5; parse.y:3021
//!   restore is longjmped past).
//! - a PC that merely fails (no errexit) is NOT an unwind: the shell prompts
//!   again with the pre-PC `$?` (parse.y:7313/:7315 restore).
//! - PROMPT_COMMAND only ever runs at prompts: non-interactive `-c`/script
//!   shells never execute it, so an `exit`-armed PC must not disturb them.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
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

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
}

/// One piped-`-i` interactive session (the rubash#433 harness shape, at the
/// niu level): `niu -i --norc` with the given lines on stdin, stdin closed
/// after them. Piped `-i` drives the engine's interactive stdin reader
/// (src/main.rs delegates there because reedline needs a terminal), so these
/// pins guard the engine consumption contract the REPL fix leans on.
fn run_interactive(lines: &str) -> std::process::Output {
    let home = unique_temp_dir("niu-issue191-home");
    std::fs::create_dir_all(&home).unwrap();
    let mut child = Command::new(niu_binary())
        .args(["-i", "--norc"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .env_remove("PROMPT_COMMAND")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu -i");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(lines.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// One `niu --norc <args>` run with a hermetic HOME.
fn run_niu(home_env: bool, args: &[&str]) -> std::process::Output {
    let home = unique_temp_dir("niu-issue191-home");
    std::fs::create_dir_all(&home).unwrap();
    let mut command = Command::new(niu_binary());
    command
        .args(args)
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .env_remove("PROMPT_COMMAND")
        .stdin(Stdio::null());
    if home_env {
        command.env("HOME", &home).env("USERPROFILE", &home);
    }
    command
        .output()
        .unwrap_or_else(|err| panic!("spawn niu: {err}"))
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Piped `-i`: PC='exit 5' set on line 1 kills the session at the next
/// pre-prompt pass with rc 5 — the follow-up line is never read or run.
#[test]
fn piped_i_pc_exit_ends_session_with_jump_status() {
    let output = run_interactive("PROMPT_COMMAND='exit 5'\necho AFTER-PC\n");
    assert_eq!(
        output.status.code(),
        Some(5),
        "stderr: {}",
        stderr_of(&output)
    );
    assert!(
        !stdout_of(&output).contains("AFTER-PC"),
        "the line after the PC exit must never run: {:?}",
        stdout_of(&output)
    );
}

/// Piped `-i` control: a PC that merely fails is not an unwind; the session
/// survives and the follow-up line runs.
#[test]
fn piped_i_pc_plain_failure_keeps_session() {
    let output = run_interactive("PROMPT_COMMAND='false'\necho AFTER-FAIL\n");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).contains("AFTER-FAIL"),
        "the shell must prompt again after a plain failing PC: {:?}",
        stdout_of(&output)
    );
}

/// Non-interactive `-c`: PROMPT_COMMAND never runs (it is a pre-PROMPT hook
/// and `-c` renders no prompt), so an `exit`-armed PC must not kill the run
/// — the trailing command executes and the exit status is the command's own.
#[test]
fn c_mode_never_executes_prompt_command() {
    let output = run_niu(
        true,
        &["--norc", "-c", "PROMPT_COMMAND='exit 5'; echo C-RAN"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    assert_eq!(stdout_of(&output).trim(), "C-RAN");
}

/// Script form: same contract for `niu script` — the PC assignment is inert,
/// and the script's own status is the session status.
#[test]
fn script_mode_never_executes_prompt_command() {
    let dir = unique_temp_dir("niu-issue191-script");
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("pc-inert.sh");
    std::fs::write(&script, "PROMPT_COMMAND='exit 5'\necho SCRIPT-RAN\n").unwrap();
    let script = script.to_string_lossy().replace('\\', "/");
    let output = run_niu(true, &["--norc", &script]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    assert_eq!(stdout_of(&output).trim(), "SCRIPT-RAN");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Control for the consumption contract: a plain `exit 5` in `-c` still ends
/// the shell with 5 (the engine's script-level jump propagation, rubash#457,
/// must be untouched by the REPL-side consumption).
#[test]
fn c_mode_plain_exit_status_unchanged() {
    let output = run_niu(true, &["--norc", "-c", "exit 5"]);
    assert_eq!(output.status.code(), Some(5));
}
