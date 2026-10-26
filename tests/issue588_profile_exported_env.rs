//! niubash#588: NIU_ENV / BASH_ENV are looked up in the executor
//! variable table first (process environment as fallback) — the same
//! read as $ENV's execute_env_file — so a BASH_ENV exported BY the
//! login profile takes effect, matching GNU (profile chain first, env
//! file second). NIU_ENV keeps precedence over BASH_ENV.

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
        "niu-issue588-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).unwrap();
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

#[test]
fn bash_env_exported_by_the_profile_is_honored() {
    let home = unique_home("profile-bash-env");
    let envfile = home.join("envfile");
    std::fs::write(&envfile, "echo ENVFILE-RAN\n").unwrap();
    std::fs::write(
        home.join(".bash_profile"),
        format!("echo PROFILE-RAN\nexport BASH_ENV={}\n", envfile.display()),
    )
    .unwrap();
    let output = run_niu(&home, &["-lc", "echo MAIN"], &[]);
    let stdout = stdout_of(&output);
    // GNU order: profile, then the env file it named, then the command.
    let profile_at = stdout.find("PROFILE-RAN").expect("profile must run");
    let env_at = stdout
        .find("ENVFILE-RAN")
        .expect("profile-exported BASH_ENV must be sourced");
    let main_at = stdout.find("MAIN").expect("command must run");
    assert!(
        profile_at < env_at && env_at < main_at,
        "expected PROFILE-RAN < ENVFILE-RAN < MAIN: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn niu_env_exported_by_the_profile_beats_process_bash_env() {
    let home = unique_home("profile-niu-env");
    let niu_envfile = home.join("niu-envfile");
    std::fs::write(&niu_envfile, "echo NIUENV-RAN\n").unwrap();
    let bash_envfile = home.join("bash-envfile");
    std::fs::write(&bash_envfile, "echo BASHENV-RAN\n").unwrap();
    std::fs::write(
        home.join(".bash_profile"),
        format!("export NIU_ENV={}\n", niu_envfile.display()),
    )
    .unwrap();
    let output = run_niu(
        &home,
        &["-lc", "echo MAIN"],
        &[("BASH_ENV", bash_envfile.display().to_string())],
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("NIUENV-RAN"),
        "profile-exported NIU_ENV must be sourced: {stdout}"
    );
    assert!(
        !stdout.contains("BASHENV-RAN"),
        "NIU_ENV precedence must hold over the process BASH_ENV: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn process_env_bash_env_still_works_without_a_profile() {
    let home = unique_home("process-only");
    let envfile = home.join("envfile");
    std::fs::write(&envfile, "echo ENVFILE-RAN\n").unwrap();
    let output = run_niu(
        &home,
        &["-c", "echo MAIN"],
        &[("BASH_ENV", envfile.display().to_string())],
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("ENVFILE-RAN"),
        "process-environment BASH_ENV must keep working: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
