//! niubash#149 — exported POSIX-style path values across the child-process
//! boundary.
//!
//! GNU oracle (WSL GNU Bash 5.3.0, `variables.c` make_env_array_from_var_list):
//! every exported variable's bytes are copied verbatim into the child
//! environment; the shell never rewrites, re-renders, or substitutes
//! path-looking values at exec time.
//!
//! Measured boundary behavior of the niubash session shell (niubash a616bc4 +
//! rubash master, 2026-09-28):
//!
//! - Arbitrary exported variables (`NVM_DIR`, ...): already verbatim. The
//!   `exported_posix_path_variables_reach_children_verbatim` test pins this
//!   so the class cannot silently regress.
//! - The HOME family: NOT verbatim. rubash
//!   `src/executor/path.rs` `apply_required_windows_child_environment()`
//!   unconditionally overwrites the child's HOME/USERPROFILE/HOMEDRIVE/
//!   HOMEPATH/APPDATA/LOCALAPPDATA with the USERPROFILE-derived native home,
//!   discarding an explicitly exported `HOME=/d/...` (verified with
//!   cmd/python/child-sh observers; every historical binary back to 1.1.3
//!   does the same). That clobber lives in the embedded rubash engine, not
//!   in niubash host code, so the fix belongs upstream in unixwin/rubash
//!   (rubash#329 decomposition source (a)); the ignored test below is the
//!   gate that flips green when the upstream fix lands in the pinned
//!   rubash revision.

#[cfg(windows)]
use std::fs;
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::process::{Command, Stdio};

// The boundary tests below are Windows-gated; on unix the helpers would be
// dead weight, so they follow the same gate.
#[cfg(windows)]
fn niu_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_niu"))
}

/// Runs a script body through the built niu binary and returns trimmed
/// stdout. The body is written to an LF-terminated file so no host-shell
/// quoting can perturb the bytes under test.
#[cfg(windows)]
fn run_script(body: &str) -> String {
    let dir = std::env::temp_dir().join("niubash-issue149");
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join("probe.sh");
    fs::write(&script, format!("{body}\n")).unwrap();
    let output = Command::new(niu_binary())
        .arg(&script)
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");
    assert!(
        output.status.success(),
        "niu failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// GNU contract, already honored: an arbitrary exported POSIX-style path
/// value reaches a Windows child byte-for-byte (no drive-form rewrite, no
/// path-shape heuristic). cmd.exe is the observer so the test needs no
/// winuxcmd command links.
#[test]
#[cfg(windows)]
fn exported_posix_path_variables_reach_children_verbatim() {
    let out = run_script(
        "export NVM_DIR=/d/some/nvm-149\n\
         cmd /c \"echo %NVM_DIR%\"",
    );
    assert_eq!(out, "/d/some/nvm-149");
}

/// GNU contract for the HOME family: `export HOME=/d/...` must reach Windows
/// children verbatim (GNU variables.c copies the value cell bytes; an
/// explicitly exported HOME is the caller's choice). Currently RED because
/// rubash's `apply_required_windows_child_environment` overrides the child's
/// HOME with the USERPROFILE-derived native home; run with `--ignored` to
/// check the upstream gate. Owner: unixwin/rubash (niubash#149 / rubash#329
/// decomposition source (a)). Delete the ignore attribute when the pinned
/// rubash revision carries the fix.
#[test]
#[cfg(windows)]
#[ignore = "red until rubash seeds the required-Windows HOME family only when missing (niubash#149)"]
fn exported_home_reaches_children_verbatim() {
    let out = run_script(
        "export HOME=/d/some/niu-home-149\n\
         cmd /c \"echo %HOME%\"",
    );
    assert_eq!(
        out, "/d/some/niu-home-149",
        "exported POSIX HOME must cross the child boundary byte-for-byte (GNU verbatim export)"
    );
}

/// Corollary of the HOME contract: the nvm-class sed splice stays colon-free
/// only while the child sees the POSIX form. A drive-letter HOME (either
/// `D:/x` or `D:\\x`) puts a colon inside `s:^$HOME:` patterns and produces
/// `NVM_DIR=""` (nvm install.sh) or bootstrap deaths (webinstall.dev).
#[test]
#[cfg(windows)]
#[ignore = "red until rubash seeds the required-Windows HOME family only when missing (niubash#149)"]
fn child_home_form_keeps_sed_splices_colon_free() {
    let out = run_script(
        "export HOME=/d/some/niu-home-149\n\
         \"${THIS_SH}\" -c 'case \"$HOME\" in *:*) echo COLON_FORM ;; *) echo POSIX_FORM ;; esac'",
    );
    assert_eq!(out, "POSIX_FORM");
}
