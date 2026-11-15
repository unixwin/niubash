//! niubash#993 host-face leftovers (tracker #996): pin the audited
//! dispositions for S3/S6/S8 so a future "parity" edit cannot silently
//! regress them.
//!
//! * S3 `uname -s`/`uname -r` — the `MSYS_NT` (or `MSYSTEM`-derived)
//!   persona is intentional, not a missed `MINGW64_NT`: the docs use the
//!   `MSYS_NT` label as niu's identity signature against Git Bash
//!   (`docs/src/agents.md`, `docs/src/platform-support.md`), and there is
//!   no MSYS2 runtime whose version `uname -r` could honestly report, so
//!   `-r` reports the OS version. On unix the builtin prints utsname
//!   verbatim, which is also covered.
//! * S6 `git --version` — the `.niugit` suffix is compiled into the
//!   bundled niugit binary (wpm packaging, upstream of this repo); no
//!   host-side change can remove it without a forbidden output-mangling
//!   shim. Documented in `docs/src/platform-support.md`.
//! * S8 `bind -l` / `completion_strip_exe` — `bind -l`'s 176 entries are
//!   captured verbatim from the GNU bash 5.3.20 oracle (Git Bash's 177th
//!   is a Git-for-Windows patch artifact, not GNU), and
//!   `completion_strip_exe` is a Cygwin/Git-for-Windows extension
//!   upstream GNU has no such option for, so `shopt` must keep rejecting
//!   it (engine ruling: rubash src/builtins/shopt/support.rs).

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

fn niu_c(script: &str) -> (String, String) {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(script)
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niu");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// S8a: `bind -l` lists exactly the GNU 5.3.20 oracle's 176 readline
/// function names. Git Bash's 177th entry is a Git-for-Windows patch
/// artifact — matching upstream GNU is the spec.
#[test]
fn issue993_bind_l_matches_gnu_funmap_count() {
    let (stdout, stderr) = niu_c("bind -l | wc -l");
    let count: u32 = stdout
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("bind -l | wc -l not numeric: {stdout:?} stderr: {stderr}"));
    assert_eq!(
        count, 176,
        "bind -l diverged from the GNU 5.3.20 oracle count: {stderr}"
    );
}

/// S8b: `shopt completion_strip_exe` stays rejected — it is a
/// Cygwin/Git-for-Windows extension upstream GNU 5.3 does not have.
#[test]
fn issue993_shopt_rejects_completion_strip_exe() {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg("shopt -s completion_strip_exe")
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niu");
    assert!(
        !output.status.success(),
        "completion_strip_exe was accepted; it must stay rejected \
         (Git-for-Windows extension, not GNU):\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let (stdout, stderr) = niu_c("shopt | grep -c completion_strip_exe; true");
    assert_eq!(
        stdout.trim().lines().next(),
        Some("0"),
        "completion_strip_exe must not be enumerated by shopt: {stderr}"
    );
}

/// S3: `uname -s` reports the persona/kernel label honestly — the MSYS
/// persona on Windows, utsname verbatim on unix. It must NOT print the
/// Git-Bash-specific `MINGW64_NT` unless `MSYSTEM=MINGW64` was actually
/// inherited (niu does not set it itself).
#[test]
fn issue993_uname_s_is_persona_not_git_bash_impersonation() {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg("uname -s")
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .env_remove("MSYSTEM")
        .output()
        .expect("spawn niu");
    assert!(output.status.success());
    let sysname = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cfg!(windows) {
        assert!(
            sysname.starts_with("MSYS_NT"),
            "with MSYSTEM unset the Windows persona reports MSYS_NT, \
             got {sysname:?} — do not impersonate MINGW64_NT"
        );
    } else {
        assert!(
            !sysname.is_empty() && !sysname.contains("MINGW64"),
            "unexpected uname -s on unix: {sysname:?}"
        );
    }
}
