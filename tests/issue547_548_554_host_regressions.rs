//! Regression pins for niubash host-surface issues #547, #548 and #554.
//! (The remaining niubash rows of the backfill wave are ENGINE-DUP and
//! are pinned on the rubash side per the cross-repo convention; #531's
//! backup-retention pin lives in `issue531_backup_retention.rs`.)
//!
//! #547: running a DIRECTORY as the script argument returned rc 1 and
//! leaked the raw Rust io error: `niu: Is a directory (os error 21)` —
//! the file name was lost as well. GNU bash 5.3.20 / standalone rubash:
//! `niu: /tmp: Is a directory`, rc 126 (the conventional
//! found-but-not-executable status).
//!
//! #548: when niu reads commands from stdin, `$-` must carry the `s`
//! flag (commands read from stdin) — GNU bash and standalone rubash
//! both report `hBs`; niu reported `hB`. Repro:
//!   printf 'echo $-\n' | niu
//!
//! #554: `niu plugin --help` omitted the `migrate-rc` verb from its
//! Commands list although the verb exists and two docs pages cite it
//! as the diagnostic entry point. The pin only asserts the verb is
//! listed; the help text itself may be reworded.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

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

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("niubash-bfd-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn temp_home(tag: &str) -> (PathBuf, PathBuf) {
    let root = unique_temp_dir(tag);
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    (root, home)
}

#[test]
fn issue547_directory_as_script_is_126_with_filename() {
    let (root, home) = temp_home("547");
    let target = root.join("somedir");
    std::fs::create_dir_all(&target).expect("script-arg dir");
    let target_str = target.to_string_lossy().into_owned();
    let output = Command::new(niu_binary())
        .arg(&target)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("spawn niu");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        output.status.code(),
        Some(126),
        "a directory as script argument is 126 (found, not executable); stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("Is a directory"),
        "diagnostic names the errno, got: {stderr:?}"
    );
    assert!(
        stderr.contains(&target_str),
        "diagnostic keeps the file name, got: {stderr:?}"
    );
    assert!(
        !stderr.contains("(os error"),
        "raw Rust io error formatting must not leak, got: {stderr:?}"
    );
}

#[test]
fn issue548_dollar_dash_has_s_flag_when_reading_stdin() {
    let (root, home) = temp_home("548");
    let start = root.join("start");
    std::fs::create_dir_all(&start).expect("start dir");
    let mut child = Command::new(niu_binary())
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"echo $-\n")
        .unwrap();
    let output = child.wait_with_output().expect("wait niu");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&root);
    let flags_line = stdout
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with('h') && l.chars().all(|c| c.is_ascii_alphabetic()));
    assert!(
        matches!(flags_line, Some(l) if l.contains('s')),
        "$- must include the s flag when commands come from stdin (GNU/rubash: hBs), stdout: {stdout:?}"
    );
}

#[test]
fn issue554_plugin_help_lists_migrate_rc() {
    let (root, home) = temp_home("554");
    let output = Command::new(niu_binary())
        .args(["plugin", "--help"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("spawn niu");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        stdout.contains("migrate-rc"),
        "niu plugin --help must list the migrate-rc verb; stdout: {stdout:?} stderr: {stderr:?}"
    );
}
