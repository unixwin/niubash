//! niubash#956: `$-` lacks `H` (histexpand) on interactive one-shot
//! forms, while the stdin REPL form carries it.
//!
//! GNU reference — bash 5.3 `builtins/set.def:80`: `H` is the
//! `histexpand` letter in `$-`; `shell.c:1830` `init_interactive` /
//! `shell.c:1866` `init_interactive_script` cover every interactive
//! entry form, so the interactive default (histexpand on) holds for
//! `-i script` and `-i -c` exactly as for the REPL forms.
//!
//! Measured GNU 5.3.20 matrix (`$-`):
//! - `-i script` -> `hiBH`
//! - `-i -c`     -> `hiBHc`
//! - `--norc -i -c`, `--login -i -c`, `--rcfile … -i -c` -> `hiBHc`
//! - plain `-c`  -> `hBc` (control: no H, no i)
//!
//! niu asserted the histexpand default only on the REPL entries, so
//! the one-shot interactive dispatches showed `hiB`/`hiBc` where GNU
//! shows `hiBH`/`hiBHc`.
//!
//! NOTE — the `m` face is NOT in this stem: niu's stdin REPL showed
//! `himBHs` in a no-tty probe where GNU shows `hiBHs`, but that flag
//! is set by the engine's interactive stdin driver
//! (script_driver.rs `run_interactive_stdin`, unconditional monitor
//! default), not by any host init — it is tracked engine-side as
//! rubash#2948 and this stem does not pin it.
//!
//! These run the built `niu` binary in its own process per case with
//! an explicit environment (the startup_files.rs pattern), so they
//! stay independent of the ambient machine env.

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
            "niu-issue956-{label}-{}-{nanos}",
            std::process::id()
        ));
        let home = base.join("home");
        let start = base.join("start");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&start).unwrap();
        Self { home, start }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.home.parent().unwrap());
    }
}

/// Run `niu` with a clean startup environment. `NIU_RC_POLICY` starts
/// REMOVED (the built-in default is what these cells exercise).
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
        .env_remove("NIU_RC_POLICY")
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

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `$-` carries `H` (histexpand, set.def:80) on every interactive
/// form. GNU's init_interactive / init_interactive_script defaults
/// cover `-i script` and `-i -c` exactly like the REPL forms; niu
/// asserted the default only on the REPL entries, so the one-shot
/// interactive dispatches showed `hiB` where GNU shows `hiBH`.
#[test]
fn issue956_dash_i_forms_carry_histexpand_flag() {
    let fixture = Fixture::new("histexpand-flags");
    let script = fixture.home.join("probe.sh");
    std::fs::write(&script, "echo \"FLAGS=$-\"\n").unwrap();

    // -i script form: GNU hiBH.
    let output = run_niu(
        &["-i", script.to_string_lossy().as_ref()],
        &fixture.home,
        &fixture.start,
        &[],
        None,
    );
    assert_success(&output, "issue956 -i script flags");
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("FLAGS=hiBH"),
        "-i script must carry H in $-: {stdout}"
    );

    // -i -c form: GNU hiBHc.
    let output = run_niu(
        &["-i", "-c", "echo \"FLAGS=$-\""],
        &fixture.home,
        &fixture.start,
        &[],
        None,
    );
    assert_success(&output, "issue956 -i -c flags");
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("FLAGS=hiBHc"),
        "-i -c must carry H in $-: {stdout}"
    );

    // --norc does not touch the option default (GNU: hiBHc under
    // --norc too — histexpand is not an rc-file behavior).
    let output = run_niu(
        &["--norc", "-i", "-c", "echo \"FLAGS=$-\""],
        &fixture.home,
        &fixture.start,
        &[],
        None,
    );
    assert_success(&output, "issue956 --norc -i -c flags");
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("FLAGS=hiBHc"),
        "--norc -i -c must carry H in $-: {stdout}"
    );

    // Login -i -c: GNU hiBHc (profile chain runs, H still on).
    std::fs::write(
        fixture.home.join(".bash_profile"),
        "export M_PROFILE=profile\n",
    )
    .unwrap();
    let output = run_niu(
        &["-l", "-i", "-c", "echo \"FLAGS=$-\""],
        &fixture.home,
        &fixture.start,
        &[],
        None,
    );
    assert_success(&output, "issue956 login -i -c flags");
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("FLAGS=hiBHc"),
        "login -i -c must carry H in $-: {stdout}"
    );

    // --rcfile -i -c: GNU hiBHc.
    let rcfile = fixture.home.join("myrc");
    std::fs::write(&rcfile, "export M_RC=rc\n").unwrap();
    let output = run_niu(
        &[
            "--rcfile",
            rcfile.to_string_lossy().as_ref(),
            "-i",
            "-c",
            "echo \"FLAGS=$-\"",
        ],
        &fixture.home,
        &fixture.start,
        &[],
        None,
    );
    assert_success(&output, "issue956 --rcfile -i -c flags");
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("FLAGS=hiBHc"),
        "--rcfile -i -c must carry H in $-: {stdout}"
    );

    // Guard: an explicit startup `+H` is the user's word and stands
    // (GNU: `bash +H -i -c 'echo $-'` -> `hiBc`) — the interactive
    // default must not override the transport.
    let output = run_niu(
        &["+H", "-i", "-c", "echo \"FLAGS=$-\""],
        &fixture.home,
        &fixture.start,
        &[],
        None,
    );
    assert_success(&output, "issue956 +H -i -c flags");
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("FLAGS=hiBc"),
        "+H -i -c must keep H off: {stdout}"
    );

    // Control: non-interactive `-c` has no H (GNU: hBc, histexpand off).
    let output = run_niu(
        &["-c", "echo \"FLAGS=$-\""],
        &fixture.home,
        &fixture.start,
        &[],
        None,
    );
    assert_success(&output, "issue956 plain -c flags");
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("FLAGS=hBc"),
        "plain -c must not carry H: {stdout}"
    );
}
