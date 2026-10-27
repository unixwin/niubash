//! unixwin/niubash#562: a non-interactive empty/stdin run paid the full
//! interactive-stack initialization (prompt backend, completion directory
//! scan, completion snapshot) even though no interactive session could
//! ever start. The fix adds a non-interactive fast constructor for stdin
//! shells and uses it only where the invocation is provably
//! non-interactive (no `-i`, non-terminal stdio).
//!
//! These tests pin the behavior contract that must NOT move with the fast
//! path — the construction-level pins live in niubash-runtime's shell.rs
//! unit tests (`issue562_*`):
//! - a bare empty run stays quiet and succeeds;
//! - stdin scripts still execute with the `s` flag in `$-` (#548) and
//!   propagate exit codes;
//! - stdin diagnostics keep the argv[0] name and absolute line numbers
//!   (#431);
//! - the `-s` route still sources $BASH_ENV, and the `-l` route still
//!   sources the login profile chain (first existing of ~/.bash_profile,
//!   ~/.bash_login, ~/.profile), with --noprofile still suppressing it;
//! - `-i` over a pipe still takes the interactive stdin driver (`i` in
//!   `$-`), i.e. it does NOT fall into the fast path.

use std::io::Write;
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

struct Fixture {
    home: PathBuf,
    start: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "niu-issue562-{label}-{}-{nanos}",
            std::process::id()
        ));
        let home = base.join("home");
        let start = base.join("start");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&start).unwrap();
        Self { home, start }
    }

    fn write_home_file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.home.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.home.parent().unwrap());
    }
}

/// Run `niu` with HOME pointed at the fixture and the startup-file
/// variables at a known state. `input: None` gives the child a null
/// stdin (the measured "empty run" form); `Some` pipes the script in.
fn run_niu(
    args: &[&str],
    home: &Path,
    cwd: &Path,
    envs: &[(&str, String)],
    input: Option<&str>,
) -> Output {
    let mut command = Command::new(niu_binary());
    command
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("SSH_CLIENT")
        .env_remove("SSH2_CLIENT")
        .env_remove("BASH_ENV")
        .env_remove("NIU_ENV")
        .env_remove("ENV")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    for (key, value) in envs {
        command.env(key, value);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|err| panic!("spawn niu: {err}"));
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn issue562_empty_run_is_quiet_and_succeeds() {
    let fixture = Fixture::new("empty");
    let output = run_niu(&[], &fixture.home, &fixture.start, &[], None);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    assert_eq!(stdout_of(&output), "");
    assert_eq!(stderr_of(&output), "");
}

#[test]
fn issue562_bare_stdin_script_keeps_flags_and_exit_code() {
    let fixture = Fixture::new("bare-stdin");
    let output = run_niu(
        &[],
        &fixture.home,
        &fixture.start,
        &[],
        Some("printf 'FLAGS=%s\\n' \"$-\"\nexit 3\n"),
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "stderr: {}",
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    let flags = stdout
        .lines()
        .find_map(|line| line.strip_prefix("FLAGS="))
        .unwrap_or_else(|| panic!("no FLAGS line in stdout: {stdout:?}"));
    assert!(
        flags.contains('s'),
        "stdin script keeps the s flag: {flags}"
    );
    assert!(
        !flags.contains('i'),
        "stdin script is not interactive: {flags}"
    );
}

#[test]
fn issue562_bare_stdin_diagnostic_keeps_line_number() {
    let fixture = Fixture::new("bare-diag");
    let output = run_niu(
        &[],
        &fixture.home,
        &fixture.start,
        &[],
        Some("echo first\n)\n"),
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        stderr_of(&output)
    );
    assert!(
        stderr_of(&output).contains("line 2"),
        "diagnostic keeps the absolute line: {}",
        stderr_of(&output)
    );
}

#[test]
fn issue562_dash_s_route_still_sources_bash_env() {
    let fixture = Fixture::new("bash-env");
    let env_file = fixture.write_home_file("env.sh", "export STARTUP_MARKER=from-bash-env\n");
    let output = run_niu(
        &["-s"],
        &fixture.home,
        &fixture.start,
        &[("BASH_ENV", env_file.to_string_lossy().into_owned())],
        Some("printf 'M=%s\\n' \"${STARTUP_MARKER:-unset}\"\n"),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).contains("M=from-bash-env"),
        "stdout: {}",
        stdout_of(&output)
    );
}

#[test]
fn issue562_dash_l_route_still_sources_login_profile_chain() {
    let fixture = Fixture::new("login-chain");
    fixture.write_home_file(".bash_profile", "export STARTUP_MARKER=from-bash-profile\n");
    fixture.write_home_file(".profile", "export STARTUP_MARKER=from-profile\n");
    let script = "printf 'M=%s\\n' \"${STARTUP_MARKER:-unset}\"\n";

    // First existing file in the chain wins (~/.bash_profile here).
    let output = run_niu(
        &["-l", "-s"],
        &fixture.home,
        &fixture.start,
        &[],
        Some(script),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).contains("M=from-bash-profile"),
        "stdout: {}",
        stdout_of(&output)
    );

    // --noprofile suppresses the chain on the same fast-path route.
    let output = run_niu(
        &["-l", "--noprofile", "-s"],
        &fixture.home,
        &fixture.start,
        &[],
        Some(script),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).contains("M=unset"),
        "stdout: {}",
        stdout_of(&output)
    );
}

#[test]
fn issue562_dash_i_piped_stdin_stays_interactive() {
    let fixture = Fixture::new("dash-i");
    let output = run_niu(
        &["-i"],
        &fixture.home,
        &fixture.start,
        &[],
        Some("printf 'FLAGS=%s\\n' \"$-\"\nexit\n"),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    let flags = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("FLAGS="))
        .unwrap_or_else(|| panic!("no FLAGS line in stdout: {stdout:?}"));
    assert!(
        flags.contains('i'),
        "-i keeps the interactive flag: {flags}"
    );
}
