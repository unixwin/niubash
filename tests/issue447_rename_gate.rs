//! niubash#447 regression: the brand rename gate
//! (`scripts/check-rename-clean.sh`) used to fail on a pristine master
//! because its residue baseline was stale — every new change saw a
//! false red unrelated to its diff. The acceptance in #447: the gate is
//! green on a clean tree, and goes red only for genuinely new residue.
//! This test runs the gate itself. niubash#1104: a missing `sh` used to
//! skip silently; it is now a hard failure in CI and by default locally,
//! with `NIU_TEST_SKIP_RENAME_GATE` as the explicit, visible local
//! opt-out (the gate's documented runners are Git Bash and the shell's
//! own `-c` mode).

use std::path::PathBuf;
use std::process::Command;

/// Environment variable that explicitly opts out of the rename-gate pin
/// for a local run (niubash#1104). CI never honors it.
const SKIP_RENAME_GATE_ENV: &str = "NIU_TEST_SKIP_RENAME_GATE";

fn ci_env_is_truthy() -> bool {
    std::env::var("CI").is_ok_and(|v| !v.is_empty() && v != "false")
}

/// `which_sh` with the niubash#1104 floor applied: a missing `sh` is a
/// hard failure in CI and by default locally; the only skip is the
/// explicit, visible opt-out in `SKIP_RENAME_GATE_ENV`. On success the
/// resolved path is printed so CI logs can prove the pin executed.
fn require_sh() -> Option<PathBuf> {
    if let Ok(sh) = which_sh() {
        eprintln!("rename gate sh: {}", sh.display());
        return Some(sh);
    }
    if ci_env_is_truthy() {
        panic!(
            "an `sh` is required in CI to run the rename gate but none was found \
             (PATH or the Git for Windows install locations); \
             refusing to let this pin pass without executing (niubash#1104)"
        );
    }
    if std::env::var_os(SKIP_RENAME_GATE_ENV).is_some() {
        eprintln!(
            "SKIP: {SKIP_RENAME_GATE_ENV} is set and no `sh` was found; \
             this pin did not execute (niubash#1104)"
        );
        return None;
    }
    panic!(
        "no `sh` found (PATH or the Git for Windows install locations); \
         to skip this pin explicitly on a local run, set {SKIP_RENAME_GATE_ENV}=1 (niubash#1104)"
    );
}

#[test]
fn issue447_rename_gate_is_green_on_the_current_tree() {
    let Some(sh) = require_sh() else { return };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(sh)
        .arg("scripts/check-rename-clean.sh")
        .current_dir(&root)
        .output()
        .expect("spawn rename gate");
    assert!(
        output.status.success(),
        "rename gate failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn which_sh() -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for dir in std::env::split_paths(&path) {
        for name in ["sh", "sh.exe"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    // Git for Windows puts sh.exe under Git\bin / Git\usr\bin, which are
    // not on the test process PATH on stock runners even though Git Bash
    // is this gate's documented runner (niubash#1104).
    #[cfg(windows)]
    for candidate in [
        "C:\\Program Files\\Git\\bin\\sh.exe",
        "C:\\Program Files\\Git\\usr\\bin\\sh.exe",
        "C:\\Program Files (x86)\\Git\\bin\\sh.exe",
        "C:\\Program Files (x86)\\Git\\usr\\bin\\sh.exe",
    ] {
        let candidate = PathBuf::from(candidate);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}
