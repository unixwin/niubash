//! niubash#589: the NIU_ENV / BASH_ENV value is expanded before the
//! file is opened, with GNU's rules — tilde (including `~user` and the
//! Windows-first `~\` spelling) and parameter expansion (`$VAR`,
//! `${VAR}`) against the shell variable table. Previously only "~"
//! and "~/" were expanded, so `$HOME/...` named a literal file that
//! did not exist and the env file was skipped silently. A missing
//! file after expansion is still skipped silently (FEVAL_ENOENTOK).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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

fn unique_home(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!(
        "niu-issue589-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("envfile"), "echo ENVFILE-RAN\n").unwrap();
    home
}

fn run_niu(home: &Path, args: &[&str], envs: &[(&str, String)]) -> Output {
    let mut command = Command::new(niu_binary());
    command
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("USERPROFILE", home)
        .stdin(Stdio::null());
    for (name, value) in envs {
        command.env(name, value);
    }
    command.output().expect("spawn niu")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn dollar_home_form_expands() {
    let home = unique_home("dollar");
    let output = run_niu(
        &home,
        &["-c", "echo MAIN"],
        &[("BASH_ENV", "$HOME/envfile".to_string())],
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("ENVFILE-RAN"),
        "BASH_ENV='$HOME/envfile' must be expanded and sourced: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn braced_home_form_expands() {
    let home = unique_home("braced");
    let output = run_niu(
        &home,
        &["-c", "echo MAIN"],
        &[("NIU_ENV", "${HOME}/envfile".to_string())],
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("ENVFILE-RAN"),
        "NIU_ENV='${{HOME}}/envfile' must be expanded and sourced: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn tilde_backslash_form_expands() {
    let home = unique_home("tilde-backslash");
    let output = run_niu(
        &home,
        &["-c", "echo MAIN"],
        &[("NIU_ENV", "~\\envfile".to_string())],
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("ENVFILE-RAN"),
        "NIU_ENV='~\\envfile' must resolve against the home: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn tilde_user_form_expands_for_the_current_user() {
    let home = unique_home("tilde-user");
    // The child sees USER=<name> and a fake HOME; ~<name> must resolve
    // to the shell home exactly like bare ~.
    let output = run_niu(
        &home,
        &["-c", "echo MAIN"],
        &[
            ("NIU_ENV", "~niu589user/envfile".to_string()),
            ("USER", "niu589user".to_string()),
        ],
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("ENVFILE-RAN"),
        "~user for the current user must resolve to the home: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn custom_variable_form_expands() {
    let home = unique_home("custom-var");
    let output = run_niu(
        &home,
        &["-c", "echo MAIN"],
        &[
            ("NIU_ENV", "$MYDIR/envfile".to_string()),
            ("MYDIR", home.display().to_string()),
        ],
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("ENVFILE-RAN"),
        "NIU_ENV='$MYDIR/envfile' must expand MYDIR: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn missing_file_after_expansion_stays_silent() {
    let home = unique_home("missing");
    let output = run_niu(
        &home,
        &["-c", "echo MAIN"],
        &[("BASH_ENV", "$HOME/no-such-envfile".to_string())],
    );
    assert!(output.status.success());
    assert_eq!(stdout_of(&output).trim(), "MAIN");
    assert!(
        stderr_of(&output).is_empty(),
        "a missing env file is silent (FEVAL_ENOENTOK): {}",
        stderr_of(&output)
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn unresolvable_tilde_user_stays_literal_and_silent() {
    let home = unique_home("bad-user");
    let output = run_niu(
        &home,
        &["-c", "echo MAIN"],
        &[("NIU_ENV", "~niu589definitelynosuchuser/envfile".to_string())],
    );
    assert!(output.status.success());
    assert_eq!(stdout_of(&output).trim(), "MAIN");
    let _ = std::fs::remove_dir_all(&home);
}
