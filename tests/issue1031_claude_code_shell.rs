//! niubash#1031 regression: `niu doctor` must name the bash an agent host
//! will actually run. Claude Code picks its Bash with a probe that runs
//! before the git on PATH — with Git for Windows installed it runs Git
//! Bash, and a `CLAUDE_CODE_GIT_BASH_PATH` that names no file is ignored
//! silently. The doctor row makes the fallback visible before the agent
//! writes commands for the wrong shell.
//!
//! The row is Windows-only and shows only when a Claude Code config dir
//! exists, so the legs stage `CLAUDE_CONFIG_DIR` explicitly and neutralise
//! the parent environment (`CLAUDE_CODE_GIT_BASH_PATH` empty, a fresh
//! `HOME`/`USERPROFILE`) so no machine state leaks into the verdict.

#![cfg(windows)]

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
    fallback.push("niu.exe");
    fallback
}

/// The built forwarding shim (`src/bin/bash.rs`, niubash#464): a face
/// whose bytes match neither niu.exe nor any staged payload, recognised
/// as niu only by the behavior probe.
fn bash_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_bash"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push("bash.exe");
    fallback
}

/// A fresh scratch dir per test (pid-unique, so parallel runs cannot
/// collide). Removed at the end of each test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("niu-issue1031-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn stage_settings(dir: &PathBuf, value: &str) -> PathBuf {
    let config = dir.join("claude");
    std::fs::create_dir_all(&config).unwrap();
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    std::fs::write(
        config.join("settings.json"),
        format!("{{\"env\": {{\"CLAUDE_CODE_GIT_BASH_PATH\": \"{escaped}\"}}}}"),
    )
    .unwrap();
    config
}

fn doctor_with_config(config_dir: &PathBuf, home: &PathBuf) -> (bool, String) {
    let output = Command::new(niu_binary())
        .arg("doctor")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .env("CLAUDE_CODE_GIT_BASH_PATH", "")
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niu doctor");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn issue1031_doctor_flags_dangling_claude_code_path() {
    let dir = scratch("dangling");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let config = stage_settings(
        &dir,
        r"@@windows.home@@\AppData\Local\Programs\Niubash\winuxcmd\bin\bash.exe",
    );
    let (ok, stdout) = doctor_with_config(&config, &home);
    assert!(ok, "niu doctor failed:\n{stdout}");
    assert!(
        stdout.contains("claude code shell"),
        "doctor has no claude code shell row:\n{stdout}"
    );
    assert!(
        stdout.contains("names no file"),
        "dangling value not flagged:\n{stdout}"
    );
    assert!(
        stdout.contains("unrendered template token"),
        "unrendered token not named:\n{stdout}"
    );
    assert!(
        stdout.contains("set env.CLAUDE_CODE_GIT_BASH_PATH"),
        "no fix hint for the dangling value:\n{stdout}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn issue1031_doctor_ok_for_niu_bash_copy() {
    let dir = scratch("ok");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // A byte-identical copy of niu under an accepted name is niu: the
    // installed face is `winuxcmd\bin\bash.exe`, a hardlink to niu.exe.
    let bash = dir.join("bash.exe");
    std::fs::copy(niu_binary(), &bash).unwrap();
    let config = stage_settings(&dir, &bash.to_string_lossy());
    let (ok, stdout) = doctor_with_config(&config, &home);
    assert!(ok, "niu doctor failed:\n{stdout}");
    assert!(
        stdout.contains("claude code shell"),
        "doctor has no claude code shell row:\n{stdout}"
    );
    assert!(
        stdout.contains("niu ("),
        "byte-identical niu bash.exe not recognised as niu:\n{stdout}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn issue1031_doctor_hidden_without_config_dir() {
    let dir = scratch("hidden");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // A config dir that does not exist, and a home with no `.claude`:
    // the host never runs here, so the row must stay hidden.
    let (ok, stdout) = doctor_with_config(&dir.join("no-such-claude"), &home);
    assert!(ok, "niu doctor failed:\n{stdout}");
    assert!(
        !stdout.contains("claude code shell"),
        "row printed with no Claude Code config dir:\n{stdout}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn issue1031_doctor_ok_for_shim_face() {
    let dir = scratch("shim");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // The built shim face: its bytes match neither niu.exe nor any
    // staged payload, so only the behavior probe can recognise it — it
    // re-execs niu, which reports itself via $NIU_SHELL.
    let config = stage_settings(&dir, &bash_binary().to_string_lossy());
    let (ok, stdout) = doctor_with_config(&config, &home);
    assert!(ok, "niu doctor failed:\n{stdout}");
    assert!(
        stdout.contains("claude code shell"),
        "doctor has no claude code shell row:\n{stdout}"
    );
    assert!(
        stdout.contains("niu ("),
        "shim face not recognised as niu:\n{stdout}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
