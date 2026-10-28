//! niubash#655 regression: `$BASH` is a plain shell variable — GNU does
//! not export it (variables.c seeds BASH without the export attribute;
//! `export -p` in a clean environment lists only OLDPWD/PWD/SHLVL), and
//! rubash#1325 aligned the standalone rubash main accordingly. The niubash
//! runtime used to re-export it through `export_env`, so every child
//! process saw a `BASH=` entry GNU never provides.
//!
//! Two faces are pinned:
//! 1. `export -p` (the child-visibility contract: exported variables are
//!    exactly what children inherit) must not list BASH;
//! 2. the in-shell detection probe `[ -n "$BASH" ]` — the whole reason the
//!    variable is seeded (scripts probe it to detect bash, bash(1) BASH) —
//!    must still succeed with the unexported shell variable.
//!
//! The child runs with a cleared environment so the assertion cannot be
//! confused by whatever the test harness itself inherited.

use std::path::PathBuf;
use std::process::Command;

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("release");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niu" });
    fallback
}

fn niu_c(script: &str) -> (bool, String) {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(script)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niu");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn issue655_export_p_does_not_list_bash() {
    let (ok, stdout) = niu_c("export -p");
    assert!(ok, "niu -c 'export -p' failed");
    let exported_bash = stdout.lines().any(|l| {
        l.trim_start().starts_with("declare -x BASH=") || l.trim_start().starts_with("export BASH=")
    });
    assert!(
        !exported_bash,
        "export -p listed BASH as exported — GNU seeds it as a plain \
         (non-exported) shell variable:\n{stdout}"
    );
}

#[test]
fn issue655_bash_detection_probe_still_works() {
    let (ok, stdout) = niu_c("[ -n \"$BASH\" ] && echo DETECTED || echo MISSING");
    assert!(ok, "niu -c detection probe failed");
    assert!(
        stdout.contains("DETECTED"),
        "[ -n \"$BASH\" ] must still detect bash in-shell (plain shell \
         variable, no export attribute needed):\n{stdout}"
    );
    assert!(
        !stdout.contains("MISSING"),
        "detection probe printed MISSING"
    );
}
