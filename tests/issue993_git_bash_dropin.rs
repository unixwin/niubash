//! niubash#993 — Git Bash drop-in report: triage bundle.
//!
//! The umbrella issue splits by face (engine / WinuxCmd / host). This file
//! pins only the faces fixed in niubash itself:
//! - §11 `COMSPEC`: the host mirrors the Windows `ComSpec` process variable
//!   under its canonical spelling before the session is built, so `$COMSPEC`
//!   reads in-shell (the engine's variable table is case-sensitive).
//! - §8 quoted arguments: the README claim "quoted arguments are never
//!   rewritten" was false — a quoted `/c/...` reaches a native child as
//!   `C:\...` (measured under wine, niubash#993) — the wording is corrected
//!   here and pinned so it cannot silently revert.
//! - §12 `.bat`/`.cmd` launcher precedence: the PATHEXT-before-POSIX rule
//!   was undocumented; it is now stated in the Windows Path Contract and
//!   pinned here.
//!
//! Everything else in #993 (§1 `set -u` + `${X:=v}`, §2 `cmd //c`, §4
//! `command -v` for emulated commands, §5 `/dev` devices, §6 `pwd -W`,
//! §7 realpath/readlink spelling, §9 ssh paths, §3 permission model, §10
//! tool provenance, S1–S11) lives in rubash or WinuxCmd, or needs a
//! maintainer decision first; the triage table is in the PR body.

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

fn run_c(command: &str, envs: &[(&str, &str)]) -> (String, String, i32) {
    let mut cmd = Command::new(niu_binary());
    cmd.arg("-c").arg(command);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("spawn niu -c");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// §11: a mixed-case `ComSpec` in the process environment is mirrored under
/// the canonical `COMSPEC` spelling, so the shell variable reads in-shell.
#[test]
fn comspec_mirrored_from_mixed_case_spelling() {
    let (stdout, _stderr, code) = run_c(
        "echo \"${COMSPEC:-unset}\"",
        &[("ComSpec", r"C:\fake-993\cmd.exe")],
    );
    assert_eq!(code, 0);
    assert_eq!(stdout, "C:\\fake-993\\cmd.exe\n");
}

/// §11: the old spelling stays readable in-shell under the same value —
/// the mirror renames the process-env entry, it must not take `$ComSpec`
/// away with it.
#[test]
fn mixed_case_comspec_stays_readable() {
    let (stdout, _stderr, code) = run_c(
        "echo \"${ComSpec:-unset}\"",
        &[("ComSpec", r"C:\fake-993\cmd.exe")],
    );
    assert_eq!(code, 0);
    assert_eq!(stdout, "C:\\fake-993\\cmd.exe\n");
}

/// §11: an explicitly set `COMSPEC` always wins and is never overwritten
/// by the mirror.
#[test]
fn explicit_comspec_is_never_overwritten() {
    let (stdout, _stderr, code) = run_c(
        "echo \"${COMSPEC:-unset}\"",
        &[("COMSPEC", r"C:\explicit-993\cmd.exe")],
    );
    assert_eq!(code, 0);
    assert_eq!(stdout, "C:\\explicit-993\\cmd.exe\n");
}

fn read_doc(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// §8: the README must not claim quoted arguments are never rewritten —
/// a quoted `/c/...` reaches a native child as `C:\...` (measured).
#[test]
fn readme_no_longer_claims_quoted_arguments_are_never_rewritten() {
    let text = read_doc("README.md");
    assert!(
        !text.contains("quoted arguments are never rewritten"),
        "the false §8 claim must not return"
    );
    assert!(
        !text.contains("quoted arguments never rewritten"),
        "the false §8 table claim must not return"
    );
    assert!(
        text.contains("when they spell a path"),
        "the corrected wording must state the measured rule"
    );
}

/// §8 (zh): the same correction in README-zh.md.
#[test]
fn readme_zh_no_longer_claims_quoted_arguments_are_never_rewritten() {
    let text = read_doc("README-zh.md");
    assert!(
        !text.contains("引号参数永不改写"),
        "the false §8 claim must not return in the Chinese README"
    );
    assert!(
        text.contains("只要拼出路径就会被翻译"),
        "the corrected wording must state the measured rule"
    );
}

/// §12: the Windows Path Contract must state the PATHEXT-before-POSIX
/// launcher rule.
#[test]
fn path_contract_documents_pathetxt_before_posix() {
    let text = read_doc("docs/src/windows-path-contract.md");
    assert!(
        text.contains("PATHEXT before POSIX"),
        "the contract must name the launcher rule"
    );
    assert!(
        text.contains("command -v gradle"),
        "the contract must pin the observable (command -v reports the .bat)"
    );
}
