//! niubash#587: the legacy `niu -c -l <cmd>` route must converge with
//! the engine route (`niu -lc`): the login flag marks the shell login
//! (`shopt login_shell` on), the login profile chain runs, and the
//! env file runs — instead of the flag being swallowed with all
//! startup files silently dropped. An `exit` in the profile ends the
//! invocation with its status (niubash#402), like every other route.
//!
//! GNU note: login state is NOT part of `$-`. GNU bash's
//! `shell_flags[]` table (flags.c:165 in the 5.3 sources) has no `l`
//! entry at all — its only `l` row is `#if 0`'d out for
//! lexical_scoping — and the 5.3.20 oracle prints `flags=hBc` with
//! `login_shell` on for `-lc`, `-lic`, and `-l <script>` alike. An
//! earlier revision of this pin asserted `$-` contains `l`; that
//! premise contradicts GNU and was corrected (the product already
//! matched GNU: legacy route `$-` = `hBc`, same as the engine route).

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
        "niu-issue587-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).unwrap();
    home
}

fn run_niu(home: &Path, args: &[&str], envs: &[(&str, &Path)]) -> Output {
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

const PROBE: &str = "echo \"MAIN profile=${PROFILE_RAN:-unset} flags=$-\"; \
                     if shopt -q login_shell; then echo LOGIN-SHELL-ON; \
                     else echo LOGIN-SHELL-OFF; fi";

fn seed_profile_and_envfile(home: &Path) -> PathBuf {
    std::fs::write(
        home.join(".bash_profile"),
        "echo PROFILE-RAN\nexport PROFILE_RAN=1\n",
    )
    .unwrap();
    let envfile = home.join("envfile");
    std::fs::write(&envfile, "echo BASHENV-RAN\n").unwrap();
    envfile
}

#[test]
fn legacy_c_l_sources_profile_and_env_file() {
    let home = unique_home("basic");
    let envfile = seed_profile_and_envfile(&home);
    let output = run_niu(&home, &["-c", "-l", PROBE], &[("BASH_ENV", &envfile)]);
    let stdout = stdout_of(&output);
    assert!(output.status.success(), "stdout: {stdout}");
    assert!(
        stdout.contains("PROFILE-RAN"),
        "profile chain must run: {stdout}"
    );
    assert!(
        stdout.contains("BASHENV-RAN"),
        "env file must run: {stdout}"
    );
    assert!(
        stdout.contains("profile=1"),
        "profile export must be visible: {stdout}"
    );
    // Login state is expressed by `shopt login_shell`, never by `$-`
    // (see the GNU note in the header): the probe must report the
    // login shell on, and `$-` must keep the GNU `-c` shape `hBc`
    // with no `l` — identical to the engine route (`niu -lc`) and
    // to the GNU oracle.
    assert!(
        stdout.contains("LOGIN-SHELL-ON"),
        "login_shell must be on: {stdout}"
    );
    let flags = stdout
        .split("flags=")
        .nth(1)
        .map(|rest| rest.lines().next().unwrap_or("").trim())
        .unwrap_or("");
    assert!(
        !flags.contains('l'),
        "$- must not gain an `l` (GNU never puts one there): {stdout}"
    );
    assert_eq!(flags, "hBc", "$- must match the GNU -c shape: {stdout}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn legacy_c_long_login_word_behaves_the_same() {
    let home = unique_home("long");
    let envfile = seed_profile_and_envfile(&home);
    let output = run_niu(&home, &["-c", "--login", PROBE], &[("BASH_ENV", &envfile)]);
    let stdout = stdout_of(&output);
    assert!(stdout.contains("PROFILE-RAN"), "profile must run: {stdout}");
    assert!(stdout.contains("profile=1"), "{stdout}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn legacy_c_without_login_keeps_the_fast_path() {
    let home = unique_home("nologin");
    let envfile = seed_profile_and_envfile(&home);
    // Plain `-c` (engine route) must NOT start sourcing the profile
    // just because this fix exists; the env file still runs.
    let output = run_niu(&home, &["-c", PROBE], &[("BASH_ENV", &envfile)]);
    let stdout = stdout_of(&output);
    assert!(
        !stdout.contains("PROFILE-RAN"),
        "non-login -c must not source the profile: {stdout}"
    );
    assert!(
        stdout.contains("BASHENV-RAN"),
        "non-login -c still sources BASH_ENV: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn legacy_c_l_profile_exit_ends_the_invocation() {
    let home = unique_home("exit");
    std::fs::write(home.join(".bash_profile"), "echo PROFILE-RAN\nexit 7\n").unwrap();
    let output = run_niu(&home, &["-c", "-l", "echo SHOULD-NOT-RUN"], &[]);
    let stdout = stdout_of(&output);
    assert_eq!(
        output.status.code(),
        Some(7),
        "profile exit status must propagate: {stdout}"
    );
    assert!(stdout.contains("PROFILE-RAN"), "{stdout}");
    assert!(
        !stdout.contains("SHOULD-NOT-RUN"),
        "command must not run after a profile exit: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
