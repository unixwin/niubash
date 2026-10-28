//! niubash#527 (W5 residual / characterization pin): under
//! `shopt -s lastpipe`, a brace group `{ ...; }` as the last pipeline
//! element was silently never executed — output and variable side
//! effects both lost (`echo hi | { read x; }` left `x` empty).
//!
//! Recorded disposition: this niubash filing was a DUPLICATE of the
//! canonical engine issue unixwin/rubash#1320 (so recorded on the
//! issue itself), closed not_planned here in favor of the engine
//! tracker. rubash#1320 is closed/completed — fixed engine-side by
//! PR #1433 (commit 283d95ea, "dispatch brace-group last element to
//! execute_brace_group_pipeline"). The behavior therefore stands at
//! GNU parity, and this pin, run through the niu binary in the
//! issue's own script-file repro form, keeps it there: if the brace
//! group is ever dropped again, these tests fail here first.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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

fn run_script(tag: &str, body: &str) -> (Output, PathBuf) {
    let dir = std::env::temp_dir().join(format!("niu-527-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let script = dir.join("probe.sh");
    std::fs::write(&script, body).expect("write script");
    let out = Command::new(niu_binary())
        .arg(&script)
        .current_dir(&dir)
        .env("HOME", &dir)
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");
    (out, dir)
}

#[test]
fn issue527_lastpipe_brace_group_executes_and_assigns() {
    let (out, dir) = run_script(
        "assign",
        "shopt -s lastpipe\necho hi | { read x; }\necho \"x=[$x]\"\n",
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x=[hi]\n");
}

#[test]
fn issue527_lastpipe_brace_group_produces_output() {
    let (out, dir) = run_script("output", "shopt -s lastpipe\necho hi | { echo grouped; }\n");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "grouped\n");
}

#[test]
fn issue527_brace_group_without_lastpipe_still_works() {
    // Control shape from the issue: identical in both shells with and
    // without the fix — guards against the pin passing vacuously.
    let (out, dir) = run_script("control", "echo hi | { read x; echo \"got [$x]\"; }\n");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "got [hi]\n");
}
