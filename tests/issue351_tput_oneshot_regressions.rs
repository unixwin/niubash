//! niubash#351 — One-shot (`niu -c`) shells must not spawn `tput`.
//!
//! niubash-runtime's shell setup used to construct the interactive
//! prompt backend even for one-shot shells; its terminal-width probe
//! can spawn `tput` (twice) on non-tty Unix. The fix gives one-shot
//! shells a terminal-free placeholder backend. This test plants a
//! logging `tput` spy at the front of PATH and runs `niu -c true`
//! with piped (non-tty) stdio: the spy must stay silent and the
//! command must succeed.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("niu351-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

#[test]
fn oneshot_shell_never_invokes_tput() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir("spy");
    let log = dir.join("tput.log");
    let spy = dir.join("tput");
    std::fs::write(
        &spy,
        format!(
            "#!/bin/sh\necho \"tput $*\" >> \"{}\"\nexit 0\n",
            log.display()
        ),
    )
    .expect("write tput spy");
    std::fs::set_permissions(&spy, std::fs::Permissions::from_mode(0o755)).expect("chmod spy");

    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_niu"))
        .env("PATH", path)
        .arg("-c")
        .arg("true")
        .output()
        .expect("run niu");
    assert!(
        output.status.success(),
        "niu -c true must succeed; status: {:?}",
        output.status
    );
    assert!(
        !log.exists(),
        "a one-shot shell must not invoke tput; spy log: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
