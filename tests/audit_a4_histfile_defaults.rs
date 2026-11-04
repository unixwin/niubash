//! Deep-audit A4-01 regression pins: the HISTFILE default on the
//! interactive routes that do not pass through the REPL.
//!
//! GNU defaults HISTFILE once at shell_initialize (variables.c
//! `set_if_not`), before the startup files, for every interactive
//! shell. niu asserted that default only inside `enter_interactive`
//! (the REPL route), so a piped `-i` session and `niu -i -c` ran with
//! HISTFILE unset: no history file was loaded or written, and the
//! bare `history -a/-w/-r/-n` forms failed with "HISTFILE: parameter
//! null or not set". `Shell::default_histfile_if_unset` now asserts
//! the same default on those routes, before their startup files run.
//!
//! (A4-02 — an exported HISTFILE not seeding the REPL provider's
//! backing file — is report-only: it collides with the adjudicated
//! niubash#571/#661 "configured path is the file of record" doctrine
//! and is parked for an owner decision. The last test here only pins
//! the pre-existing engine-level behavior of the piped route, which
//! was never broken.)

use std::io::Write;
use std::path::PathBuf;
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

fn temp_home(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!(
        "niu-a4-histfile-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// Run `niu -i` with `input` on a piped stdin in a clean env whose
/// HOME is `home`; extra env pairs are layered on top.
fn run_piped_interactive(home: &PathBuf, extra_env: &[(&str, &str)], input: &str) -> Output {
    let mut command = Command::new(niu_binary());
    command
        .arg("-i")
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in extra_env {
        command.env(name, value);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn a4_01_piped_interactive_defaults_histfile_and_persists() {
    let home = temp_home("default");
    let output = run_piped_interactive(&home, &[], "echo a4_marker_cmd\nexit\n");
    assert!(output.status.success(), "{output:?}");
    let history = home.join(".niubash_history");
    let text = std::fs::read_to_string(&history)
        .unwrap_or_else(|err| panic!("default history file missing: {err}"));
    assert!(
        text.contains("echo a4_marker_cmd"),
        "session commands persist to the default file: {text:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a4_01_piped_interactive_history_write_form_resolves() {
    let home = temp_home("writeform");
    let output = run_piped_interactive(&home, &[], "echo one\nhistory -w\nexit\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("parameter null or not set"),
        "bare `history -w` must resolve $HISTFILE: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a4_01_dash_i_dash_c_sees_the_histfile_default() {
    let home = temp_home("ic");
    let output = Command::new(niu_binary())
        .args(["-i", "-c", "echo \"HF=[$HISTFILE]\""])
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(".niubash_history"),
        "-i -c must see the defaulted HISTFILE: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a4_01_noninteractive_never_gains_a_histfile_default() {
    let home = temp_home("plain");
    let output = Command::new(niu_binary())
        .args(["-c", "echo \"HF=[$HISTFILE]\""])
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("HF=[]"),
        "plain -c must not gain a HISTFILE default: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn inherited_histfile_still_drives_the_piped_route() {
    let home = temp_home("inherited");
    let custom = home.join("custom_history");
    let custom_arg = custom.to_string_lossy().into_owned();
    let output = run_piped_interactive(
        &home,
        &[("HISTFILE", &custom_arg)],
        "echo a4_custom_cmd\nexit\n",
    );
    assert!(output.status.success(), "{output:?}");
    let text = std::fs::read_to_string(&custom).unwrap();
    assert!(text.contains("echo a4_custom_cmd"), "{text:?}");
    let _ = std::fs::remove_dir_all(&home);
}
