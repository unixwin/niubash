//! B5-06: $BASH_ARGV0 seeding across invocation routes.
//!
//! GNU seeds $0 from $BASH_ARGV0 at shell startup, before any startup
//! file runs (BASH_ENV and the rc files already observe the caller's
//! value), on every route. A `-c` name argument outranks the seed for
//! the command body; a script operand rebinds $0 to the script name.
//! niu's constructors seed the name from argv[0], clobbering the
//! inherited variable; these tests pin the GNU precedence per route.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

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

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn base_command(home: &PathBuf) -> Command {
    let mut command = Command::new(niu_binary());
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("TERM", "dumb")
        .env_remove("BASH_ENV")
        .env_remove("NIU_ENV")
        .env_remove("BASH_ARGV0");
    command
}

fn run_with_stdin(
    home: &PathBuf,
    args: &[&str],
    stdin_text: &str,
    extra_env: &[(&str, &str)],
) -> Output {
    let mut command = base_command(home);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn niu");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_text.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait for niu")
}

fn run_capture(home: &PathBuf, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut command = base_command(home);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().expect("run niu")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn b5_06_stdin_route_honors_bash_argv0() {
    let home = unique_temp_dir("niu-b5-argv0-stdin");
    let output = run_with_stdin(
        &home,
        &[],
        "echo zero=$0 var=${BASH_ARGV0}\n",
        &[("BASH_ARGV0", "customname")],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout(&output).trim_end(), "zero=customname var=customname");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn b5_06_dash_s_route_honors_bash_argv0() {
    let home = unique_temp_dir("niu-b5-argv0-dash-s");
    let output = run_with_stdin(
        &home,
        &["-s"],
        "echo zero=$0 var=${BASH_ARGV0}\n",
        &[("BASH_ARGV0", "customname")],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout(&output).trim_end(), "zero=customname var=customname");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn b5_06_bash_env_observes_seeded_argv0_on_stdin_route() {
    let home = unique_temp_dir("niu-b5-argv0-env-stdin");
    let env_file = home.join("env.sh");
    std::fs::write(&env_file, "echo in_env zero=$0 var=${BASH_ARGV0}\n").unwrap();
    let output = run_with_stdin(
        &home,
        &[],
        "echo body zero=$0\n",
        &[
            ("BASH_ARGV0", "customname"),
            ("BASH_ENV", env_file.to_str().unwrap()),
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout(&output).trim_end(),
        "in_env zero=customname var=customname\nbody zero=customname"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn b5_06_bash_env_observes_seeded_argv0_on_c_route() {
    let home = unique_temp_dir("niu-b5-argv0-env-c");
    let env_file = home.join("env.sh");
    std::fs::write(&env_file, "echo in_env zero=$0 var=${BASH_ARGV0}\n").unwrap();
    let output = run_capture(
        &home,
        &["-c", "echo body zero=$0"],
        &[
            ("BASH_ARGV0", "customname"),
            ("BASH_ENV", env_file.to_str().unwrap()),
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout(&output).trim_end(),
        "in_env zero=customname var=customname\nbody zero=customname"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn b5_06_c_name_argument_outranks_bash_argv0() {
    let home = unique_temp_dir("niu-b5-argv0-c-name");
    let output = run_capture(
        &home,
        &["-c", "echo zero=$0", "myname"],
        &[("BASH_ARGV0", "customname")],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout(&output).trim_end(), "zero=myname");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn b5_06_piped_interactive_route_honors_bash_argv0() {
    let home = unique_temp_dir("niu-b5-argv0-pi");
    std::fs::write(home.join(".niubashrc"), "PS1=''\nPS2=''\n").unwrap();
    let output = run_with_stdin(
        &home,
        &["-i"],
        "echo zero=$0 var=${BASH_ARGV0}\nexit\n",
        &[("BASH_ARGV0", "customname")],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout(&output).contains("zero=customname var=customname"),
        "stdout was: {}",
        stdout(&output)
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn b5_06_script_route_still_rebinds_to_script_name() {
    let home = unique_temp_dir("niu-b5-argv0-script");
    let script = home.join("s.sh");
    std::fs::write(&script, "echo zero=$0\n").unwrap();
    let output = run_capture(
        &home,
        &[script.to_str().unwrap()],
        &[("BASH_ARGV0", "customname")],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = stdout(&output);
    assert!(
        text.trim_end().ends_with("s.sh"),
        "script route must keep $0 = the script operand, got: {text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
