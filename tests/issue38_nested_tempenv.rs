//! niubash#38 regression: a temporary environment assignment in front of
//! a command must be exported to that child process — including when the
//! child is a nested copy of the shell itself. The broken build showed
//! the value to native children but a nested shell child saw it empty,
//! which broke one-command isolation patterns such as
//! `SOME_ROOT=... niu plugin update ...`.

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

#[test]
fn issue38_temporary_assignment_reaches_nested_shell_child() {
    let niu = niu_binary();
    let script = format!(
        "FOO_NESTED_PROBE=bar \"{}\" -c 'printf \"FOO=%s\\n\" \"$FOO_NESTED_PROBE\"'",
        niu.display()
    );
    let output = Command::new(&niu)
        .arg("-c")
        .arg(script)
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niu");
    assert!(
        output.status.success(),
        "niu -c failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "FOO=bar\n");
}
