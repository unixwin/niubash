//! niubash#67 regression: when a pipeline stage is an external command
//! resolved by name through PATH, its arguments must reach it unchanged.
//! A bracket character-class argument was mangled on that path, so
//! `printf 'Hello World\n' | tr '[:upper:]' '[:lower:]'` silently acted
//! as the identity mapping (rc=0, wrong bytes); the same command via an
//! absolute path, via redirection, or with `A-Z` ranges worked.
//!
//! The test drives whatever `tr` the shell resolves from PATH — the
//! bundled command set on Windows, the system one elsewhere — and skips
//! when no `tr` is resolvable in the test environment.

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

fn run_c(script: &str) -> (bool, String) {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(script)
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niu");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn issue67_pipeline_stage_receives_bracket_class_argument_intact() {
    let (ok, probe) = run_c("command -v tr");
    if !ok || probe.trim().is_empty() {
        return; // no tr resolvable here; nothing meaningful to pin
    }
    let (ok, out) = run_c("printf 'Hello World\\n' | tr '[:upper:]' '[:lower:]'");
    assert!(ok);
    assert_eq!(out, "hello world\n");
}
