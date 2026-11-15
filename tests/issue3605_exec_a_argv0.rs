//! rubash#3605 / exec5.sub: `exec -a NAME bash -c 'echo $0'` must
//! report NAME — GNU shell.c:1797-1799 set_shell_name binds
//! dollar_vars[0] to argv[0]'s basename VERBATIM, without filtering
//! to known shell faces. The previous default_script_name_from
//! collapsed every non-face argv[0] to "niu".
//!
//! Unix-only: CommandExt::arg0 is the exec -a equivalent for a
//! spawned child; Windows has no argv[0] override on CreateProcess.

#![cfg(unix)]

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push("niu");
    fallback
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_with_argv0(argv0: &str, args: &[&str]) -> String {
    let home = unique_temp_dir("niu-exec-a");
    let mut command = Command::new(niu_binary());
    command
        .arg0(argv0)
        .args(args)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("TERM", "dumb")
        .env_remove("BASH_ENV")
        .env_remove("NIU_ENV")
        .env_remove("NIU_INVOKED_AS")
        .env_remove("BASH_ARGV0");
    let output = command.output().expect("run niu");
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        output.stderr.is_empty(),
        "argv0 {argv0:?} {args:?}: stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn exec_a_argv0_binds_dollar0_verbatim() {
    let stdout = run_with_argv0("ohio-state", &["-c", "echo this is $0"]);
    assert_eq!(stdout, "this is ohio-state\n", "exec -a name not bound");
}

#[test]
fn plain_argv0_keeps_the_product_name() {
    let stdout = run_with_argv0("niu", &["-c", "echo this is $0"]);
    assert_eq!(stdout, "this is niu\n");
}

#[test]
fn face_argv0_and_login_mark_still_seed() {
    let stdout = run_with_argv0("bash", &["-c", "echo this is $0"]);
    assert_eq!(stdout, "this is bash\n");
    let stdout = run_with_argv0("-custom", &["-c", "echo this is $0"]);
    assert_eq!(stdout, "this is custom\n", "login '-' mark not stripped");
}
