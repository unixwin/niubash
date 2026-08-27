//! Regression tests for background (`&`) and coproc self-spawn argv.
//!
//! Rubash spawns the host executable for background jobs and coprocs. The
//! spawned argv must stick to the common `-c <script>` contract; a leading
//! `--` is a rubash-CLI-only convention that the winuxsh launcher rejects as
//! a script-file argument ("unknown argument '--' (not a script file)").

use std::path::PathBuf;
use std::process::Command;

fn winuxsh_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_winuxsh"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) {
        "winuxsh.exe"
    } else {
        "winuxsh"
    });
    fallback
}

fn run_winuxsh(script: &str) -> (String, String) {
    let output = Command::new(winuxsh_binary())
        .arg("-c")
        .arg(script)
        .env("WINUXSH_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn winuxsh");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
fn background_command_keeps_dash_dash_args_out_of_host_cli() {
    let (stdout, stderr) = run_winuxsh("echo --connect-timeout 5 --max-time 10 & wait; echo done");

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
        run_winuxsh("coproc C { echo --marker; }; read line <&${C[0]}; echo got=$line");

    assert!(
        !stderr.contains("unknown argument"),
        "coproc spawn leaked argv into the host CLI: {stderr}"
    );
    assert!(
        stdout.contains("got=--marker"),
        "coproc output missing, stdout: {stdout}"
    );
}
