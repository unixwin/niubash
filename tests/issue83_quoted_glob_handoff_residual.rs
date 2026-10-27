//! niubash#83 (W5 residual / characterization pin): on Windows, quoted
//! glob arguments to WinuxCmd-list commands called by bare name were
//! observed expanded (`ls "*.txt"` listed files, `rm "*.tmp"` deleted
//! files) where Git Bash passed them literally.
//!
//! Recorded disposition: closed not_planned, and the reporter then
//! WITHDREW the report pending re-verification — the expansion may
//! have come from MSYS-built binaries' own argv globbing on that
//! machine rather than from niubash's handoff, and niubash-side fault
//! was never confirmed. What is pinnable, and what this file pins, is
//! the shell-side invariant that bounds the question: the shell itself
//! NEVER glob-expands a quoted pattern — builtins and external
//! commands alike receive the literal word. If a future Windows-side
//! repro confirms expansion after the handoff, it belongs to the
//! command/argv layer; the shell-side half pinned here must still hold.

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

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("niu-83-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(dir.join("a.txt"), "a\n").expect("fixture a.txt");
    std::fs::write(dir.join("b.txt"), "b\n").expect("fixture b.txt");
    dir
}

fn run_in(dir: &std::path::Path, script: &str) -> Output {
    Command::new(niu_binary())
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .env("HOME", dir)
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu")
}

#[test]
fn issue83_quoted_glob_stays_literal_for_builtin() {
    let dir = scratch_dir("builtin");
    let out = run_in(&dir, r#"printf '[%s]\n' "*.txt""#);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[*.txt]\n");
}

#[cfg(not(windows))]
#[test]
fn issue83_quoted_glob_stays_literal_for_external_command() {
    // A PATH-resolved external helper must receive the quoted pattern
    // verbatim: with two matching files present, any shell-side
    // expansion would deliver `a.txt b.txt` instead of `*.txt`.
    let dir = scratch_dir("external");
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    let helper = bin_dir.join("showargs83");
    std::fs::write(
        &helper,
        "#!/bin/sh\nfor a in \"$@\"; do printf '<%s>\\n' \"$a\"; done\n",
    )
    .expect("helper script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755))
            .expect("chmod helper");
    }
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(niu_binary())
        .arg("-c")
        .arg(r#"showargs83 "*.txt""#)
        .current_dir(&dir)
        .env("HOME", &dir)
        .env("PATH", path)
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "<*.txt>\n");
}
