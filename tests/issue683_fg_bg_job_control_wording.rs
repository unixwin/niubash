//! niubash#683 (Package 10 / V3 three-face evidence): an interactive face's
//! no-job `fg`/`bg` error must carry GNU's `current: no such job` wording,
//! not the non-interactive `no job control` refusal.
//!
//! GNU baseline (WSL GNU bash 5.3.x, captured 2026-10-29):
//! * non-interactive — `bash -c fg` -> `fg: no job control` (fg_bg.def
//!   `job_control == 0` branch);
//! * interactive — `bash -i -c fg` -> `fg: current: no such job`,
//!   `bash -i -c bg` -> `bg: current: no such job` (initialize_job_control
//!   keeps `m` for interactive shells, jobs.c:4869).
//!
//! The niubash REPL faces (the pty REPL, `--noediting`, and the `-C`
//! REPL-command face — all `Shell::enter_interactive` entry points) used to
//! leave the engine's `monitor` off because the engine applies its
//! interactive job-control default only at `run_interactive_stdin`
//! (rubash script_driver.rs:4203-4209, rubash#1991) — the entry only the
//! piped `-i` face reaches. `niu -C fg` therefore printed the
//! non-interactive `no job control` wording while `niu -i` (piped) printed
//! the GNU one. `enter_interactive` now mirrors the engine's default (with
//! the same explicit-transport escape hatch), so every interactive face
//! converges. The `-c` face must keep the non-interactive wording — that
//! side is GNU-correct and pins here as an over-reach guard.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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

/// Fresh HOME/USERPROFILE/LOCALAPPDATA/APPDATA/TEMP sandbox so no real
/// profile, history, or plugin state is touched (same shape as
/// defaults_floor.rs / tests/interactive/driver.rs).
fn sandbox(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "niu-issue683-{name}-{}-{nanos}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sandboxed_command(name: &str, home: &Path) -> Command {
    let mut command = Command::new(niu_binary());
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LOCALAPPDATA", home)
        .env("APPDATA", home)
        .env("TEMP", home)
        .env("NIU_PLUGIN_SOURCES_ROOT", home);
    command.current_dir(home);
    let _ = name;
    command
}

fn combined(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

/// The `-C` REPL-command face shares `enter_interactive` with the pty REPL,
/// so it renders the interactive face's message without needing a terminal.
#[test]
fn repl_command_face_fg_no_jobs_reports_no_such_job() {
    let home = sandbox("fg");
    let output = sandboxed_command("fg", &home)
        .args(["--norc", "-C", "fg"])
        .output()
        .expect("spawn niu -C fg");
    let _ = fs::remove_dir_all(&home);

    let text = combined(&output);
    assert!(
        text.contains("fg: current: no such job"),
        "interactive -C face must report GNU's no-such-job wording, got: {text:?}"
    );
    assert!(
        !text.contains("no job control"),
        "interactive -C face must not fall back to the non-interactive wording, got: {text:?}"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "a failing fg reports execution failure, got: {text:?}"
    );
}

#[test]
fn repl_command_face_bg_no_jobs_reports_no_such_job() {
    let home = sandbox("bg");
    let output = sandboxed_command("bg", &home)
        .args(["--norc", "-C", "bg"])
        .output()
        .expect("spawn niu -C bg");
    let _ = fs::remove_dir_all(&home);

    let text = combined(&output);
    assert!(
        text.contains("bg: current: no such job"),
        "interactive -C face must report GNU's no-such-job wording for bg too, got: {text:?}"
    );
    assert!(
        !text.contains("no job control"),
        "interactive -C face must not fall back to the non-interactive wording, got: {text:?}"
    );
}

/// The piped `-i` face already drove the engine's interactive stdin entry
/// (rubash#1991's default) and printed the GNU wording before the fix; pin
/// it so the convergence can never regress in the other direction.
#[test]
fn piped_interactive_face_fg_no_jobs_reports_no_such_job() {
    let home = sandbox("piped-i");
    let input = home.join("fg-input.sh");
    fs::write(&input, "fg\n").unwrap();
    let stdin = fs::File::open(&input).unwrap();
    let output = sandboxed_command("piped-i", &home)
        .args(["--norc", "-i"])
        .stdin(Stdio::from(stdin))
        .output()
        .expect("spawn niu --norc -i with piped fg");
    let _ = fs::remove_dir_all(&home);

    let text = combined(&output);
    assert!(
        text.contains("fg: current: no such job"),
        "piped -i face must keep GNU's no-such-job wording, got: {text:?}"
    );
}

/// Over-reach guard: the non-interactive `-c` face has `monitor` off by
/// GNU's own default, so its `fg` refusal is the CORRECT `no job control`
/// branch (fg_bg.def job_control == 0). The convergence must not touch it.
#[test]
fn noninteractive_c_face_keeps_no_job_control() {
    let home = sandbox("dash-c");
    let output = sandboxed_command("dash-c", &home)
        .args(["--norc", "-c", "fg"])
        .output()
        .expect("spawn niu --norc -c fg");
    let _ = fs::remove_dir_all(&home);

    let text = combined(&output);
    assert!(
        text.contains("fg: no job control"),
        "non-interactive -c face keeps GNU's no-job-control wording, got: {text:?}"
    );
    assert!(
        !text.contains("no such job"),
        "non-interactive -c face must not grow the interactive wording, got: {text:?}"
    );
}

/// The engine's escape hatch stands on the REPL faces too: an explicit
/// startup transport value (`+m`) leaves `monitor` off, and `fg` falls back
/// to the monitor-off refusal exactly like GNU's `bash +m -i`.
#[test]
fn repl_command_face_explicit_plus_m_still_disables_job_control() {
    let home = sandbox("plus-m");
    let output = sandboxed_command("plus-m", &home)
        .args(["+m", "--norc", "-C", "fg"])
        .output()
        .expect("spawn niu +m --norc -C fg");
    let _ = fs::remove_dir_all(&home);

    let text = combined(&output);
    assert!(
        text.contains("fg: no job control"),
        "an explicit +m transport value must keep the monitor-off refusal, got: {text:?}"
    );
}
