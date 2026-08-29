//! Regression tests for background (`&`) and coproc self-spawn argv.
//!
//! Rubash spawns the host executable for background jobs and coprocs. The
//! spawned argv must stick to the common `-c <script>` contract; a leading
//! `--` is a rubash-CLI-only convention that the niubash launcher rejects as
//! a script-file argument ("unknown argument '--' (not a script file)").

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
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niubash" });
    fallback
}

fn run_niu(script: &str) -> (String, String) {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(script)
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niubash");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
fn background_command_keeps_dash_dash_args_out_of_host_cli() {
    let (stdout, stderr) = run_niu("echo --connect-timeout 5 --max-time 10 & wait; echo done");

    assert!(
        !stderr.contains("unknown argument"),
        "background spawn leaked argv into the host CLI: {stderr}"
    );
    assert!(
        stdout.contains("--connect-timeout 5 --max-time 10"),
        "background command lost its arguments, stdout: {stdout}"
    );
    assert!(stdout.contains("done"), "foreground part missing: {stdout}");
}

#[test]
fn coproc_command_keeps_dash_dash_args_out_of_host_cli() {
    let (stdout, stderr) =
        run_niu("coproc C { echo --marker; }; read line <&${C[0]}; echo got=$line");

    assert!(
        !stderr.contains("unknown argument"),
        "coproc spawn leaked argv into the host CLI: {stderr}"
    );
    assert!(
        stdout.contains("got=--marker"),
        "coproc output missing, stdout: {stdout}"
    );
}
