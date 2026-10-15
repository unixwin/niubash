//! Startup-file coverage matrix (rc / BASH_ENV / NIU_ENV / ENV / --rcfile /
//! --norc / --posix / ssh `-c`), exercised end-to-end against the built
//! `niu` binary.
//!
//! GNU reference — bash 5.3 `shell.c:1147 run_startup_files()`:
//! - shell.c:1156-1180: top-level non-interactive `-c` started by sshd
//!   (`SSH_CLIENT`/`SSH2_CLIENT`) sources the bashrc file instead of
//!   BASH_ENV and returns; `--norc` disables that branch.
//! - shell.c:1214-1220: non-interactive, non-posix, bash-mode shells source
//!   `$BASH_ENV`; posix/sh non-interactive shells source nothing.
//! - shell.c:1235-1239 / 1241-1246: interactive bash sources the rc file
//!   (`--rcfile` overrides); interactive posix/sh sources `$ENV` only.
//! - shell.c:1103 execute_env_file + evalfile.c:120 (FEVAL_ENOENTOK): a
//!   missing env file is skipped silently.
//! - `-i` forces the interactive branch even with `-c` (shell.c option
//!   parsing sets forced_interactive before run_startup_files).
//! - The sshd/rshd `-c` bashrc case (shell.c:1156-1180) is compiled out of
//!   the reference build (`config-top.h:108` leaves `SSH_SOURCE_BASHRC`
//!   undefined, so `run_by_ssh = 0`) and its isnetconn(stdin) half needs a
//!   socket on stdin — so `SSH_CLIENT` in the environment does NOT switch
//!   the startup files. Pinned here against the WSL GNU Bash 5.3.0 oracle.
//!
//! These run in their own process per test with an explicit environment, so
//! they stay independent of the ambient machine env and of the lib-test
//! suite's process-state lock.

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
            "niu-startup-{label}-{}-{nanos}",
            std::process::id()
        ));
        let home = base.join("home");
        let start = base.join("start");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&start).unwrap();
        Self { home, start }
    }

    fn write_rc(&self, marker: &str) {
        std::fs::write(
            self.home.join(".niubashrc"),
            format!("export STARTUP_MARKER={marker}\n"),
        )
        .unwrap();
    }

    fn write_file(&self, name: &str, marker: &str) -> PathBuf {
        let path = self.home.join(name);
        std::fs::write(&path, format!("export STARTUP_MARKER={marker}\n")).unwrap();
        path
    }

    fn path(&self) -> &Path {
        &self.home
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.home.parent().unwrap());
    }
}

/// Run `niu` with a clean startup-relevant environment: HOME/USERPROFILE
/// point at the fixture home, and every startup-file variable the matrix
/// exercises starts from a known state.
fn run_niu(args: &[&str], home: &Path, cwd: &Path, envs: &[(&str, String)]) -> Output {
    run_niu_with_input(args, home, cwd, envs, None)
}

fn run_niu_with_input(
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

#[test]
fn restricted_invocation_defers_restrictions_until_startup_in_every_dispatch() {
    let fixture = Fixture::new("restricted-dispatch");
    let startup = "cd \"$HOME\"\nPATH=restricted-startup-path\nSTARTUP_COUNT=$(( ${STARTUP_COUNT:-0} + 1 ))\nprintf 'startup:%s\\n' \"$STARTUP_COUNT\"\n";
    std::fs::write(fixture.home.join(".niubashrc"), startup).unwrap();
    let env_file = fixture.home.join("restricted.env");
    std::fs::write(&env_file, startup).unwrap();
    let script = fixture.home.join("restricted.niu");
    for body in [
        "printf 'body:%s:%s\\n' \"$STARTUP_COUNT\" \"$PATH\"; cd .",
        "printf 'body:%s:%s\\n' \"$STARTUP_COUNT\" \"$PATH\"; PATH=body-path",
    ] {
        std::fs::write(&script, body).unwrap();
        let script_name = script.to_str().unwrap();
        let cases = [
            (vec!["-r", "-c", body], None),
            (vec!["-r", "-i", "-c", body], None),
            (vec!["-r", script_name], None),
            (vec!["-r", "-i", script_name], None),
            (vec!["-r", "-s"], Some(format!("{body}\n"))),
            (vec!["-r"], Some(format!("{body}\n"))),
            (vec!["-r", "-i"], Some(format!("{body}\nexit $?\n"))),
            (vec!["-r", "-i", "-s"], Some(format!("{body}\nexit $?\n"))),
            (vec!["-r", "-C", body], None),
        ];
        for env_name in ["BASH_ENV", "NIU_ENV"] {
            for (args, input) in &cases {
                let output = run_niu_with_input(
                    args,
                    fixture.path(),
                    &fixture.start,
                    &[(env_name, env_file.to_string_lossy().into_owned())],
                    input.as_deref(),
                );
                let stdout = stdout_of(&output);
                let stderr = stderr_of(&output);
                assert_eq!(
                    stdout.matches("startup:1").count(),
                    1,
                    "{args:?}: {stdout}\n{stderr}"
                );
                assert!(
                    stdout.contains("body:1:restricted-startup-path"),
                    "{args:?}: {stdout}\n{stderr}"
                );
                assert!(!output.status.success(), "{args:?}: {stdout}\n{stderr}");
                let diagnostic = if body.ends_with("PATH=body-path") {
                    "PATH: readonly variable"
                } else {
                    "cd: restricted"
                };
                assert!(stderr.contains(diagnostic), "{args:?}: {stderr}");
            }
        }
    }
}

#[test]
fn restricted_norc_and_missing_startup_still_restrict_the_body() {
    let fixture = Fixture::new("restricted-norc");
    for rc_exists in [false, true] {
        if rc_exists {
            fixture.write_rc("must-not-run");
        }
        for args in [
            vec![
                "--norc",
                "-r",
                "-i",
                "-c",
                "echo ${STARTUP_MARKER:-unset}; cd .",
            ],
            vec!["--norc", "-r", "-C", "echo ${STARTUP_MARKER:-unset}; cd ."],
            vec!["--norc", "-r", "-i"],
            vec!["-r", "-c", "echo ${STARTUP_MARKER:-unset}; cd ."],
            vec!["-r", "-s"],
        ] {
            let input = (args.last() == Some(&"-i") || args.last() == Some(&"-s"))
                .then_some("echo ${STARTUP_MARKER:-unset}; cd .\nexit $?\n");
            let output = run_niu_with_input(&args, fixture.path(), &fixture.start, &[], input);
            assert_eq!(stdout_of(&output), "unset", "{args:?}");
            assert!(!output.status.success(), "{args:?}");
            assert!(stderr_of(&output).contains("restricted"), "{args:?}");
        }
    }
}

#[test]
fn startup_set_r_is_not_cleared_when_invocation_is_unrestricted() {
    let fixture = Fixture::new("startup-set-r");
    let startup = "set -r\n";
    std::fs::write(fixture.home.join(".niubashrc"), startup).unwrap();
    let env_file = fixture.home.join("restricted.env");
    std::fs::write(&env_file, startup).unwrap();
    for args in [
        vec!["-c", "cd ."],
        vec!["-i", "-c", "cd ."],
        vec!["-C", "cd ."],
    ] {
        let output = run_niu(
            &args,
            fixture.path(),
            &fixture.start,
            &[("BASH_ENV", env_file.to_string_lossy().into_owned())],
        );
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr_of(&output).contains("restricted"), "{args:?}");
    }
}

#[test]
fn restricted_is_not_a_valid_named_invocation_option() {
    let fixture = Fixture::new("restricted-invalid-option");
    fixture.write_rc("must-not-run");
    for args in [
        vec!["-o", "restricted", "-c", "echo body"],
        vec!["-o", "restricted", "-C", "echo body"],
        vec!["-c", "-o", "restricted", "echo body"],
    ] {
        let output = run_niu(&args, fixture.path(), &fixture.start, &[]);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(stdout_of(&output).is_empty(), "{args:?}");
        assert!(
            stderr_of(&output).contains("restricted: invalid shell option name"),
            "{args:?}: {}",
            stderr_of(&output)
        );
    }
}

#[test]
fn restricted_startup_respects_rcfile_posix_and_norc_env_boundaries() {
    let fixture = Fixture::new("restricted-startup-boundaries");
    fixture.write_rc("wrong-rc");
    let startup = fixture.home.join("custom.rc");
    std::fs::write(
        &startup,
        "cd \"$HOME\"\nPATH=boundary-path\nset +r\nprintf 'custom-startup\\n'\n",
    )
    .unwrap();
    let startup_name = startup.to_str().unwrap();
    let body = "printf 'body:%s\\n' \"$PATH\"; cd .";
    for (args, envs, expected_startup) in [
        (
            vec!["--rcfile", startup_name, "-r", "-i", "-c", body],
            vec![],
            true,
        ),
        (
            vec!["--rcfile", startup_name, "-r", "-C", body],
            vec![],
            true,
        ),
        (
            vec!["--norc", "--rcfile", startup_name, "-r", "-C", body],
            vec![],
            false,
        ),
        (
            vec!["--norc", "-r", "-c", body],
            vec![("BASH_ENV", startup_name.to_owned())],
            true,
        ),
        (
            vec!["--posix", "--norc", "-r", "-i", "-c", body],
            vec![("ENV", startup_name.to_owned())],
            true,
        ),
        (vec!["--posix", "-r", "-i", "-c", body], vec![], false),
    ] {
        let output = run_niu(&args, fixture.path(), &fixture.start, &envs);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr_of(&output).contains("cd: restricted"), "{args:?}");
        let stdout = stdout_of(&output);
        assert_eq!(
            stdout.matches("custom-startup").count(),
            usize::from(expected_startup),
            "{args:?}: {stdout}"
        );
        if expected_startup {
            assert!(stdout.contains("body:boundary-path"), "{args:?}: {stdout}");
        }
        assert!(!stdout.contains("wrong-rc"), "{args:?}: {stdout}");
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
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

const PROBE: &str = "echo ${STARTUP_MARKER:-unset}";

fn seed_ignored_rc_files(fixture: &Fixture) -> Vec<(PathBuf, Vec<u8>)> {
    [".winuxshrc", ".winshrc", ".bashrc"]
        .into_iter()
        .map(|name| {
            let path = fixture.home.join(name);
            let content = format!(
                "export STARTUP_MARKER={name}\nprintf executed >> \"$HOME/unexpected-rc-execution\"\n"
            )
            .into_bytes();
            std::fs::write(&path, &content).unwrap();
            (path, content)
        })
        .collect()
}

fn assert_ignored_rc_files_unchanged(fixture: &Fixture, files: &[(PathBuf, Vec<u8>)]) {
    for (path, original) in files {
        assert_eq!(&std::fs::read(path).unwrap(), original, "{path:?}");
    }
    assert!(!fixture.home.join("unexpected-rc-execution").exists());
}

/// Interactive REPL startup sources ~/.niubashrc located through $HOME.
#[test]
fn interactive_repl_sources_home_niubashrc() {
    let fixture = Fixture::new("repl-home");
    fixture.write_rc("from_rc");
    let output = run_niu(&["-C", PROBE], fixture.path(), &fixture.start, &[]);
    assert_success(&output, "repl rc");
    assert_eq!(stdout_of(&output), "from_rc");
}

/// Startup must be read-only with respect to the user's primary rc. A fresh
/// home must not acquire a generated `~/.niubashrc` merely because niu starts.
#[test]
fn startup_does_not_create_primary_rc_in_a_fresh_home() {
    let fixture = Fixture::new("fresh-home-no-write");
    let rc = fixture.home.join(".niubashrc");
    assert!(!rc.exists(), "fixture must start without {rc:?}");

    let output = run_niu(
        &["-C", "echo startup-ok"],
        fixture.path(),
        &fixture.start,
        &[],
    );
    assert_success(&output, "fresh-home startup");
    assert_eq!(stdout_of(&output), "startup-ok");
    assert!(!rc.exists(), "startup must not create {rc:?}");
}

/// A broken primary rc is user-owned input. Startup may report its parse
/// error, but it must not repair, truncate, or replace the file implicitly.
#[test]
fn startup_does_not_repair_or_rewrite_primary_rc() {
    let fixture = Fixture::new("broken-primary-no-repair");
    let ignored = seed_ignored_rc_files(&fixture);
    let rc = fixture.home.join(".niubashrc");
    let original = "if true; then\n  export STARTUP_MARKER=fragment\n";
    std::fs::write(&rc, original).unwrap();

    let output = run_niu(
        &["-C", "echo startup-ok"],
        fixture.path(),
        &fixture.start,
        &[],
    );
    assert_success(&output, "broken-primary startup");
    assert_eq!(stdout_of(&output), "startup-ok");
    assert_eq!(std::fs::read_to_string(&rc).unwrap(), original);
    assert_ignored_rc_files_unchanged(&fixture, &ignored);
}

#[test]
fn startup_without_primary_ignores_legacy_and_bash_rc_files() {
    for args in [vec!["-C", PROBE], vec!["-i", "-c", PROBE]] {
        let fixture = Fixture::new("primary-only-missing");
        let ignored = seed_ignored_rc_files(&fixture);
        let output = run_niu(&args, fixture.path(), &fixture.start, &[]);
        assert_success(&output, "primary-only startup");
        assert_eq!(stdout_of(&output), "unset");
        assert!(!fixture.home.join(".niubashrc").exists());
        assert_ignored_rc_files_unchanged(&fixture, &ignored);
    }
}

#[test]
fn startup_primary_is_exclusive_and_not_rewritten() {
    for original in ["export STARTUP_MARKER=primary\n", ""] {
        let fixture = Fixture::new("primary-only-existing");
        let ignored = seed_ignored_rc_files(&fixture);
        let rc = fixture.home.join(".niubashrc");
        std::fs::write(&rc, original).unwrap();
        let output = run_niu(&["-C", PROBE], fixture.path(), &fixture.start, &[]);
        assert_success(&output, "exclusive primary startup");
        assert_eq!(
            stdout_of(&output),
            if original.is_empty() {
                "unset"
            } else {
                "primary"
            }
        );
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), original);
        assert_ignored_rc_files_unchanged(&fixture, &ignored);
    }
}

/// `--rcfile` overrides the default interactive rc file (GNU bashrc_file).
#[test]
fn rcfile_overrides_default_rc() {
    let fixture = Fixture::new("rcfile");
    let ignored = seed_ignored_rc_files(&fixture);
    fixture.write_rc("from_default_rc");
    let primary_before = std::fs::read(fixture.home.join(".niubashrc")).unwrap();
    let alt = fixture.write_file("alt.rc", "from_rcfile");
    let alt_before = std::fs::read(&alt).unwrap();
    let output = run_niu(
        &["--rcfile", alt.to_string_lossy().as_ref(), "-C", PROBE],
        fixture.path(),
        &fixture.start,
        &[],
    );
    assert_success(&output, "rcfile");
    assert_eq!(stdout_of(&output), "from_rcfile");
    assert_eq!(std::fs::read(&alt).unwrap(), alt_before);
    assert_eq!(
        std::fs::read(fixture.home.join(".niubashrc")).unwrap(),
        primary_before
    );
    assert_ignored_rc_files_unchanged(&fixture, &ignored);
}

#[test]
fn missing_rcfile_never_falls_back_or_creates_configuration() {
    let fixture = Fixture::new("missing-rcfile");
    let ignored = seed_ignored_rc_files(&fixture);
    fixture.write_rc("from_default_rc");
    let primary_before = std::fs::read(fixture.home.join(".niubashrc")).unwrap();
    let missing = fixture.home.join("missing.rc");
    let output = run_niu(
        &["--rcfile", missing.to_string_lossy().as_ref(), "-C", PROBE],
        fixture.path(),
        &fixture.start,
        &[],
    );
    assert_success(&output, "missing explicit rcfile");
    assert_eq!(stdout_of(&output), "unset");
    assert!(!missing.exists());
    assert_eq!(
        std::fs::read(fixture.home.join(".niubashrc")).unwrap(),
        primary_before
    );
    assert_ignored_rc_files_unchanged(&fixture, &ignored);
}

#[test]
fn startup_mode_boundaries_do_not_probe_other_rc_files() {
    for flags in [vec!["--norc", "-C"], vec!["--posix", "-C"], vec!["-c"]] {
        let fixture = Fixture::new("rc-mode-boundary");
        let ignored = seed_ignored_rc_files(&fixture);
        fixture.write_rc("must_not_run");
        let primary = fixture.home.join(".niubashrc");
        let original = std::fs::read(&primary).unwrap();
        let mut args = flags;
        args.push(PROBE);
        let output = run_niu(&args, fixture.path(), &fixture.start, &[]);
        assert_success(&output, "rc mode boundary");
        assert_eq!(stdout_of(&output), "unset");
        assert_eq!(std::fs::read(&primary).unwrap(), original);
        assert_ignored_rc_files_unchanged(&fixture, &ignored);
    }
}

#[test]
fn norc_overrides_explicit_rcfile_without_modifying_it() {
    let fixture = Fixture::new("norc-rcfile");
    let ignored = seed_ignored_rc_files(&fixture);
    let alt = fixture.write_file("alt.rc", "must_not_run");
    let original = std::fs::read(&alt).unwrap();
    let output = run_niu(
        &[
            "--norc",
            "--rcfile",
            alt.to_string_lossy().as_ref(),
            "-C",
            PROBE,
        ],
        fixture.path(),
        &fixture.start,
        &[],
    );
    assert_success(&output, "norc with explicit rcfile");
    assert_eq!(stdout_of(&output), "unset");
    assert_eq!(std::fs::read(&alt).unwrap(), original);
    assert!(!fixture.home.join(".niubashrc").exists());
    assert_ignored_rc_files_unchanged(&fixture, &ignored);
}

/// `--norc` skips the interactive rc (GNU no_rc gates execute_bashrc_file).
#[test]
fn norc_skips_interactive_rc() {
    let fixture = Fixture::new("norc");
    fixture.write_rc("from_rc");
    let output = run_niu(
        &["--norc", "-C", PROBE],
        fixture.path(),
        &fixture.start,
        &[],
    );
    assert_success(&output, "norc");
    assert_eq!(stdout_of(&output), "unset");
}

/// Non-interactive `-c` sources $BASH_ENV (GNU shell.c:1218).
#[test]
fn bash_env_sourced_for_non_interactive_c() {
    let fixture = Fixture::new("bashenv-c");
    let env_file = fixture.write_file("agent.env", "from_bash_env");
    let output = run_niu(
        &["-c", PROBE],
        fixture.path(),
        &fixture.start,
        &[("BASH_ENV", env_file.to_string_lossy().into_owned())],
    );
    assert_success(&output, "bash env -c");
    assert_eq!(stdout_of(&output), "from_bash_env");
}

/// NIU_ENV takes precedence over BASH_ENV (niubash extension).
#[test]
fn niu_env_takes_precedence_over_bash_env() {
    let fixture = Fixture::new("niuenv-precedence");
    let niu = fixture.write_file("niu.env", "from_niu_env");
    let bash = fixture.write_file("bash.env", "from_bash_env");
    let output = run_niu(
        &["-c", PROBE],
        fixture.path(),
        &fixture.start,
        &[
            ("NIU_ENV", niu.to_string_lossy().into_owned()),
            ("BASH_ENV", bash.to_string_lossy().into_owned()),
        ],
    );
    assert_success(&output, "niu env precedence");
    assert_eq!(stdout_of(&output), "from_niu_env");
}

/// A missing BASH_ENV file is skipped silently (GNU evalfile.c:120
/// FEVAL_ENOENTOK): `-c` stdout/stderr stay byte-stable for one-shot agents.
#[test]
fn missing_bash_env_is_silent() {
    let fixture = Fixture::new("bashenv-missing");
    let missing = fixture.home.join("does-not-exist.env");
    let output = run_niu(
        &["-c", "printf agent-ok"],
        fixture.path(),
        &fixture.start,
        &[("BASH_ENV", missing.to_string_lossy().into_owned())],
    );
    assert_success(&output, "missing bash env");
    assert_eq!(stdout_of(&output), "agent-ok");
    assert_eq!(stderr_of(&output), "", "missing env file must be silent");
}

/// POSIX mode drops BASH_ENV for non-interactive shells (GNU shell.c:1216
/// gates on posixly_correct == 0); NIU_ENV stays available as the niubash
/// extension.
#[test]
fn posix_non_interactive_skips_bash_env() {
    let fixture = Fixture::new("posix-bashenv");
    let bash = fixture.write_file("bash.env", "from_bash_env");
    let output = run_niu(
        &["--posix", "-c", PROBE],
        fixture.path(),
        &fixture.start,
        &[("BASH_ENV", bash.to_string_lossy().into_owned())],
    );
    assert_success(&output, "posix bash env suppression");
    assert_eq!(stdout_of(&output), "unset");

    let niu = fixture.write_file("niu.env", "from_niu_env");
    let output = run_niu(
        &["--posix", "-c", PROBE],
        fixture.path(),
        &fixture.start,
        &[
            ("BASH_ENV", bash.to_string_lossy().into_owned()),
            ("NIU_ENV", niu.to_string_lossy().into_owned()),
        ],
    );
    assert_success(&output, "posix niu env extension");
    assert_eq!(stdout_of(&output), "from_niu_env");
}

/// Interactive POSIX shells source `$ENV` instead of the rc file (GNU
/// shell.c:1244-1245); `--rcfile` is ignored in this branch.
#[test]
fn posix_interactive_sources_env_not_rc() {
    let fixture = Fixture::new("posix-interactive");
    fixture.write_rc("from_rc");
    let env_file = fixture.write_file("posix.env", "from_env");
    let output = run_niu(
        &["--posix", "-C", PROBE],
        fixture.path(),
        &fixture.start,
        &[("ENV", env_file.to_string_lossy().into_owned())],
    );
    assert_success(&output, "posix interactive env");
    assert_eq!(stdout_of(&output), "from_env");
}

/// Interactive POSIX with `$ENV` unset sources nothing at all — the default
/// rc file is a bash-mode concept (shell.c:1241-1246 has no bashrc branch).
#[test]
fn posix_interactive_without_env_sources_nothing() {
    let fixture = Fixture::new("posix-no-env");
    fixture.write_rc("from_rc");
    let output = run_niu(
        &["--posix", "-C", PROBE],
        fixture.path(),
        &fixture.start,
        &[],
    );
    assert_success(&output, "posix interactive without env");
    assert_eq!(stdout_of(&output), "unset");
}

/// `-i` forces the interactive startup branch even for `-c` (GNU
/// forced_interactive): the rc file is sourced and BASH_ENV ignored.
#[test]
fn interactive_dash_c_sources_rc_not_bash_env() {
    let fixture = Fixture::new("i-c");
    fixture.write_rc("from_rc");
    let bash = fixture.write_file("bash.env", "from_bash_env");
    let output = run_niu(
        &["-i", "-c", PROBE],
        fixture.path(),
        &fixture.start,
        &[("BASH_ENV", bash.to_string_lossy().into_owned())],
    );
    assert_success(&output, "interactive -c rc");
    assert_eq!(stdout_of(&output), "from_rc");
}

/// `SSH_CLIENT` in the environment does NOT switch the startup files: the
/// shell.c:1156-1180 sshd branch is compiled out of the reference build
/// (config-top.h:108, `SSH_SOURCE_BASHRC` undefined → `run_by_ssh = 0`) and
/// the isnetconn(stdin) half needs a socket on stdin. Pinned against the
/// WSL GNU Bash 5.3.0 oracle: `SSH_CLIENT=... bash -c` sources $BASH_ENV,
/// not ~/.bashrc, top-level (SHLVL unset) or nested (SHLVL=2) alike.
#[test]
fn ssh_client_does_not_switch_startup_files() {
    let fixture = Fixture::new("ssh-c");
    fixture.write_rc("from_rc");
    let bash = fixture.write_file("bash.env", "from_bash_env");

    let output = run_niu(
        &["-c", PROBE],
        fixture.path(),
        &fixture.start,
        &[
            ("SSH_CLIENT", "10.0.0.1 51234 22".to_string()),
            ("BASH_ENV", bash.to_string_lossy().into_owned()),
        ],
    );
    assert_success(&output, "ssh -c");
    assert_eq!(stdout_of(&output), "from_bash_env");

    let output = run_niu(
        &["-c", PROBE],
        fixture.path(),
        &fixture.start,
        &[
            ("SSH_CLIENT", "10.0.0.1 51234 22".to_string()),
            ("SHLVL", "2".to_string()),
            ("BASH_ENV", bash.to_string_lossy().into_owned()),
        ],
    );
    assert_success(&output, "nested ssh -c");
    assert_eq!(stdout_of(&output), "from_bash_env");
}

/// Script files are non-interactive: BASH_ENV is sourced before the script
/// body runs (GNU run_startup_files runs before the script executes).
#[test]
fn script_mode_sources_bash_env() {
    let fixture = Fixture::new("script-bashenv");
    let env_file = fixture.write_file("script.env", "from_bash_env");
    let script = fixture.home.join("probe.sh");
    let mut file = std::fs::File::create(&script).unwrap();
    writeln!(file, "echo ${{STARTUP_MARKER:-unset}}").unwrap();
    drop(file);
    let output = run_niu(
        &[script.to_string_lossy().as_ref()],
        fixture.path(),
        &fixture.start,
        &[("BASH_ENV", env_file.to_string_lossy().into_owned())],
    );
    assert_success(&output, "script bash env");
    assert_eq!(stdout_of(&output), "from_bash_env");
}

/// `$ENV` expands a leading `~` against the shell home (GNU
/// execute_env_file runs the value through expand_string_unsplit, tilde
/// included).
#[test]
fn env_value_expands_tilde() {
    let fixture = Fixture::new("env-tilde");
    fixture.write_rc("from_rc");
    std::fs::write(
        fixture.home.join("posix.env"),
        "export STARTUP_MARKER=from_tilde_env\n",
    )
    .unwrap();
    let output = run_niu(
        &["--posix", "-C", PROBE],
        fixture.path(),
        &fixture.start,
        &[("ENV", "~/posix.env".to_string())],
    );
    assert_success(&output, "env tilde");
    assert_eq!(stdout_of(&output), "from_tilde_env");
}

/// niubash#157: the rc the setup wizard writes must parse cleanly under the
/// engine. `niu setup --preset recommended` regressed to an unclosed
/// `${USERPROFILE//\}` expansion in the HOME bootstrap, and because the file
/// is parsed as a whole, every later alias silently died while each
/// interactive startup printed two syntax-error lines. `niu -n` (noexec)
/// parses the file without executing it — exit 0 with empty stderr is the
/// syntax-validity contract for everything the wizard emits.
#[test]
fn setup_preset_rc_parses_clean_under_noexec() {
    let fixture = Fixture::new("setup-rc-syntax");
    let output = run_niu(
        &["setup", "--preset", "recommended"],
        fixture.path(),
        &fixture.start,
        &[],
    );
    assert_success(&output, "niu setup --preset recommended");
    let rc = fixture.home.join(".niubashrc");
    assert!(rc.is_file(), "setup must write {rc:?}");
    let check = run_niu(
        &["-n", rc.to_string_lossy().as_ref()],
        fixture.path(),
        &fixture.start,
        &[],
    );
    assert!(
        check.status.success(),
        "`niu -n` on the generated rc must exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        check.status.code(),
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(
        stderr_of(&check).is_empty(),
        "generated rc must parse without diagnostics: {}",
        stderr_of(&check)
    );
}
