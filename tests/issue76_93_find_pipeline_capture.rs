//! niubash#76 / #93 regression: `find` as the FIRST stage of a pipeline
//! inside a command substitution lost its entire output, and the same
//! shape reached through process substitution + `eval` leaked find's
//! output onto the shell's stdout while the reader saw EOF. Other
//! producers (ls/seq/printf) in the same position worked, and find alone
//! or in a top-level pipeline worked. #93 is the v1.1.1 re-report of the
//! same two halves, so both are pinned here.
//!
//! Expectations are GNU's: the substitution captures every line, and
//! the process-substitution loop counts every directory with nothing
//! leaking to stdout. The tests use the `find`/`wc`/`cat`/`sort` the
//! shell resolves from PATH and skip when they are not resolvable.

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

/// Fixture directory with exactly two subdirectories (3 directories in
/// total including itself). Returns None when the external tools the
/// shapes need are not resolvable from the shell's PATH.
fn fixture() -> Option<PathBuf> {
    for tool in ["find", "wc", "cat", "sort"] {
        let (ok, out) = run_c(&format!("command -v {tool}"));
        if !ok || out.trim().is_empty() {
            return None;
        }
    }
    let dir = std::env::temp_dir().join(format!("niu-w2-find-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("d1")).unwrap();
    std::fs::create_dir_all(dir.join("d2")).unwrap();
    Some(dir)
}

#[test]
fn issue76_find_first_stage_in_substitution_pipeline_is_captured() {
    let Some(dir) = fixture() else { return };
    let d = dir.display();
    let (ok, out) = run_c(&format!(
        "x=$(find {d} -maxdepth 1 -type d | wc -l); echo \"count=$x\""
    ));
    assert!(ok);
    assert_eq!(out, "count=3\n");
    let (ok, out) = run_c(&format!(
        "x=$(find {d} -maxdepth 1 -type d | cat); printf '%s\\n' \"$x\" | wc -l"
    ));
    assert!(ok);
    assert_eq!(out.trim(), "3");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn issue93_procsub_eval_find_output_does_not_leak() {
    let Some(dir) = fixture() else { return };
    let d = dir.display();
    let (ok, out) = run_c(&format!(
        "c=0; while IFS= read -r _; do c=$((c+1)); done \
         < <(eval \"find {d} -maxdepth 1 -type d\" | sort); echo \"c=$c\""
    ));
    assert!(ok);
    // Exactly the counter line: any leaked find output would appear here.
    assert_eq!(out, "c=3\n");
    let _ = std::fs::remove_dir_all(&dir);
}
