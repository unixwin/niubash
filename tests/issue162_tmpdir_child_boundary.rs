//! niubash#162 — launching the opencode TUI (a Bun-compiled standalone
//! executable) from niubash failed with
//! `Failed to open library "B:/~BUN/root/opentui-*.dll": error code 126`
//! while cmd/PowerShell worked.
//!
//! Root cause (measured 2026-10-02, opencode 1.18.31 on this machine): the
//! embedded rubash engine injected a default TMPDIR and EXPORTED it to every
//! child in POSIX slash-drive form (`/c/Users/.../Temp`). Bun prefers TMPDIR
//! over TEMP/TMP when extracting its embedded native DLLs; `/c/...` is not a
//! Windows path (it resolves drive-relative to `<cwd>:\c\...`), so bunfs's
//! `B:` virtual-drive extraction failed and dlopen fell back to the embedded
//! virtual path `B:/~BUN/root/...` -> ERROR_MOD_NOT_FOUND (126). Empirical
//! matrix with the exact niu child environment: `/c/...` TMPDIR -> 126 on
//! every run; native `C:/...`, backslash `C:\...`, and NO TMPDIR -> TUI
//! initializes.
//!
//! GNU contract (variables.c initialize_shell_variables): bash never invents
//! a TMPDIR and never exports one it did not import — children of a fresh
//! Windows shell see TEMP/TMP and no TMPDIR at all. Fix lives in rubash
//! (executor/init.rs injection is now shell-only; child_env_value +
//! apply_env_command_environment cross the boundary in Windows-native form);
//! these tests pin the boundary at the niubash product level.

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

/// Runs a script body through the built niu binary with a controlled
/// temp-related parent environment (TMPDIR absent, TEMP/TMP pinned) and
/// returns trimmed stdout. The body is written to a unique LF-terminated
/// file so no host-shell quoting perturbs the bytes under test and parallel
/// tests cannot race on one script path.
#[cfg(windows)]
fn run_script(body: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join("niubash-issue162");
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join(format!("probe-{}-{seq}.sh", std::process::id()));
    fs::write(&script, format!("{body}\n")).unwrap();
    let output = Command::new(niu_binary())
        .arg(&script)
        .env_remove("TMPDIR")
        .env("TEMP", r"T:\niu162-temp")
        .env("TMP", r"T:\niu162-tmp")
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");
    let _ = fs::remove_file(&script);
    assert!(
        output.status.success(),
        "niu failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// cmd.exe prints an undefined `%VAR%` reference verbatim, so `%TMPDIR%` in
/// the output means the foreign child's environment block has no TMPDIR.
///
/// Before the fix this printed `/t/niu162-temp` (slash-drive form of the
/// injected default) and broke every Bun-compiled child (niubash#162).
#[test]
#[cfg(windows)]
fn foreign_children_get_no_injected_tmpdir() {
    let out = run_script(r#"cmd /c "echo [%TMPDIR%]""#);
    assert_eq!(
        out, "[%TMPDIR%]",
        "a fresh niubash must not export an invented TMPDIR (GNU imports TMPDIR, never creates it)"
    );
}

/// TEMP/TMP cross the child boundary byte-for-byte — a foreign child must be
/// able to find the real Windows temp directory exactly as under cmd.
#[test]
#[cfg(windows)]
fn temp_and_tmp_reach_children_verbatim() {
    let out = run_script(r#"cmd /c "echo %TEMP% %TMP%""#);
    assert_eq!(out, r"T:\niu162-temp T:\niu162-tmp");
}

/// A genuinely exported TMPDIR (user choice) still reaches children, but in
/// the Windows-native forward-slash form that foreign executables can use —
/// never the slash-drive `/c/...` display form that killed bunfs.
#[test]
#[cfg(windows)]
fn exported_tmpdir_crosses_in_windows_native_form() {
    let out = run_script(r#"export TMPDIR=/c/Niu162Probe; cmd /c "echo %TMPDIR%""#);
    assert_eq!(
        out, "C:/Niu162Probe",
        "exported TMPDIR must cross the child boundary in Windows-native form"
    );
}

/// The shell-side fixture survives: `$TMPDIR` still expands inside scripts
/// (GNU suites write unquoted `$TMPDIR/...` paths), it just is not exported
/// unless the user says so.
#[test]
#[cfg(windows)]
fn tmpdir_still_expands_inside_the_shell() {
    let out = run_script(r#"test -n "$TMPDIR" && echo SET || echo UNSET"#);
    assert_eq!(out, "SET");
}
