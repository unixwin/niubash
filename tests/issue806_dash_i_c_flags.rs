//! niubash#806 (R4): `niu -ic` must report `i` in `$-`, and `--rcfile`
//! must be accepted as the interactive rc path.
//!
//! GNU oracle (WSL GNU Bash 5.3.20, `wsl -e` direct invocation — the
//! default `wsl <cmd>` form routes through an intermediate `/bin/sh`
//! that expands `$-` itself and corrupts the baseline):
//!   `bash --noprofile --norc -i -c 'echo $-'` -> `himBHc`
//!   `bash --noprofile --norc    -c 'echo $-'` -> `hBc`
//!   `bash --rcfile <rc> -i -c 'echo $-'`      -> accepted, rc sourced
//! GNU cites: shell.c parse_shell_options' `-i` arm sets
//! forced_interactive; shell.c:672 change_flag ('i', FLAG_ON) for every
//! interactive shell; flags.c:174 maps the `$-` letter to that global.

use std::path::PathBuf;
use std::process::Command;

fn niu_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_niu"))
}

fn dash_of(args: &[&str]) -> String {
    let output = Command::new(niu_binary())
        .args(args)
        .env_remove("NIU_STARTUP_TRACE")
        .output()
        .expect("spawn niu");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    stdout
        .lines()
        .find(|line| line.starts_with("DASH="))
        .unwrap_or_else(|| {
            panic!(
                "no DASH= line for {args:?}: stdout={stdout:?} stderr={:?}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .trim_start_matches("DASH=")
        .to_string()
}

#[test]
fn dash_i_c_reports_the_interactive_flag() {
    // The clustered and split spellings parse to the same invocation.
    for args in [
        vec!["--noprofile", "-ic", "echo DASH=$-"],
        vec!["--noprofile", "-i", "-c", "echo DASH=$-"],
        vec!["--noprofile", "-ci", "echo DASH=$-"],
    ] {
        let flags = dash_of(&args);
        assert!(
            flags.contains('i'),
            "`{args:?}`: `$-` must contain i (GNU: himBHc), got {flags:?}"
        );
        assert!(
            flags.contains('c'),
            "`{args:?}`: `$-` must contain c, got {flags:?}"
        );
    }
}

#[test]
fn plain_dash_c_stays_non_interactive() {
    let flags = dash_of(&["--noprofile", "-c", "echo DASH=$-"]);
    assert!(
        !flags.contains('i'),
        "plain `-c` is not interactive (GNU: hBc), got {flags:?}"
    );
}

#[test]
fn rcfile_is_accepted_and_used_as_the_interactive_rc() {
    // `--rcfile <f> -i -c 'cmd'` is the supported GNU shape: the engine's
    // invocation parse consumes the option and its value, the host sources
    // the file on the interactive startup branch, and the command runs.
    // (The `-ic` cluster AFTER a long-option value additionally needs the
    // engine-side normalizer fix tracked in niubash#806 R4; the split
    // spelling is the niubash-owned acceptance here.)
    let dir = tempfile_dir();
    let rc = dir.join("rc.sh");
    std::fs::write(&rc, "echo RC-SOURCED\nexport IN_RCFILE=1\n").unwrap();

    let output = Command::new(niu_binary())
        .arg("--noprofile")
        .arg("--rcfile")
        .arg(&rc)
        .arg("-i")
        .arg("-c")
        .arg("echo DASH=$- IN=${IN_RCFILE:-missing}")
        .output()
        .expect("spawn niu");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("RC-SOURCED"),
        "the rcfile must be sourced: {stdout:?} / stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("DASH=h"),
        "the -c command still runs with its flags: {stdout:?}"
    );
    assert!(
        stdout.contains("IN=1"),
        "the rc's exports reach the command string: {stdout:?}"
    );
    assert_eq!(output.status.code(), Some(0));
}

/// A throwaway directory; `tempfile` is not a root dev-dependency, and a
/// per-test unique path under the std temp dir is enough for one file.
fn tempfile_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "niu806-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
