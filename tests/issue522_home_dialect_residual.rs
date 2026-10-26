//! niubash#522 (W5 residual / characterization pin): on Windows,
//! `$HOME` is the Windows dialect form (`C:/Users/cmx`) while `$PWD`
//! is the POSIX dialect form (`/c/Users/cmx`), so in the home directory
//! `[ "$HOME" = "$PWD" ]` is false where GNU makes it true.
//!
//! Ruling (do not "fix"): option 2, ruled 2026-10-24 and recorded in
//! ~/workspace/rubash-fix3/DECISIONS.md — `$HOME` KEEPS the Windows
//! form (external tools key off it; the already-pinned external
//! contract is not overturned), and the confusion is resolved by
//! documentation + conversion guidance (landed via niubash PR #552).
//! Behavior does not change.
//!
//! Pinned here:
//! - POSIX hosts: the GNU invariant holds — a caller-provided HOME is
//!   honored verbatim (`normalize_executor_home_env` in
//!   crates/niubash-runtime/src/shell.rs) and after `cd ~`,
//!   `$HOME == $PWD`.
//! - Windows (cfg-gated, not run on this host): the ruled split itself
//!   — after `cd ~`, `$HOME` keeps the drive-letter slash form while
//!   `$PWD` is the POSIX `/c/...` form, and the two differ. This arm is
//!   the deliberate-deviation record; it exists so a future "helpful"
//!   POSIX-ification of $HOME fails loudly against the 2026-10-24
//!   ruling instead of silently breaking the external contract.

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

fn scratch_home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("niu-522-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch home");
    dir.canonicalize().expect("canonical home")
}

fn run_with_home(home: &std::path::Path, script: &str) -> Output {
    Command::new(niu_binary())
        .arg("-c")
        .arg(script)
        .env("HOME", home)
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu")
}

#[cfg(not(windows))]
#[test]
fn issue522_posix_home_honored_verbatim_and_matches_pwd() {
    let home = scratch_home();
    let out = run_with_home(
        &home,
        r#"printf 'HOME=<%s>\n' "$HOME"; cd ~; if [ "$HOME" = "$PWD" ]; then echo same; else echo diff; fi"#,
    );
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(
        stdout,
        format!("HOME=<{}>\nsame\n", home.display()),
        "caller-provided POSIX HOME must be honored verbatim and equal $PWD after cd ~"
    );
}

#[cfg(windows)]
#[test]
fn issue522_windows_home_keeps_windows_form_while_pwd_is_posix() {
    let home = scratch_home();
    let out = run_with_home(
        &home,
        r#"cd ~; printf 'HOME=<%s>\nPWD=<%s>\n' "$HOME" "$PWD"; if [ "$HOME" = "$PWD" ]; then echo same; else echo diff; fi"#,
    );
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let home_val = stdout
        .lines()
        .find_map(|l| l.strip_prefix("HOME=<"))
        .and_then(|l| l.strip_suffix('>'))
        .expect("HOME line");
    let pwd_val = stdout
        .lines()
        .find_map(|l| l.strip_prefix("PWD=<"))
        .and_then(|l| l.strip_suffix('>'))
        .expect("PWD line");
    let hb = home_val.as_bytes();
    assert!(
        hb.len() > 3 && hb[1] == b':' && hb[2] == b'/',
        "$HOME must keep the Windows drive form (X:/...), got {home_val}"
    );
    let pb = pwd_val.as_bytes();
    assert!(
        pb.len() > 3 && pb[0] == b'/' && pb[2] == b'/' && pb[1].is_ascii_alphabetic(),
        "$PWD must be the POSIX dialect form (/x/...), got {pwd_val}"
    );
    assert!(
        stdout.contains("diff"),
        "the ruled #522 split: $HOME and $PWD differ in the home directory on Windows"
    );
}
