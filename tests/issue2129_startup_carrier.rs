//! rubash#2129 (niu companion): the startup funnel must take its
//! shape from the process's OWN invocation, never from inherited
//! startup-shape carriers.
//!
//! The defect, as field-reported against rubash#2129: an interactive
//! session exports its shape carriers (`__RUBASH_INTERACTIVE`,
//! `__RUBASH_LOGIN_SHELL`). A startup file that respawns the shell —
//! fzf's `completion.bash` runs
//! `__fzf_orig_completion < <(complete -p $d_cmds $a_cmds $v_cmds
//! unalias kill ssh 2>/dev/null)`, and niu's procsub is a re-exec of
//! `niu -c 'complete -p ...'` — hands the child those carriers. The
//! child's startup funnel read the login carrier back, adopted the
//! parent's login shape, re-ran the profile chain (which chains into
//! the rc, which hits the fzf line again), and recursed without
//! bound: every generation blocked reading its child's pipe.
//!
//! The niu-side fix has three parts, all pinned here:
//! 1. Carrier normalization: the session's login classification is
//!    host state derived from its own parse (argv[0] `-` seed,
//!    `-l`/`--login`, the legacy `-c -l` mark); an inherited
//!    `__RUBASH_LOGIN_SHELL` is scrubbed at construction when the
//!    session did not earn login, and an inherited
//!    `__RUBASH_INTERACTIVE` is scrubbed unconditionally (every host
//!    face that earns the interactive shape writes it after
//!    classification).
//! 2. The respawn door: a respawned internal child (the engine's
//!    `__RUBASH_COPROC_CHILD` / `__RUBASH_SHELL_PID` marker pair, the
//!    same test `Executor::new` applies) sources NOTHING in either
//!    startup funnel — the parent session already ran startup.
//! 3. Guard pins: a session that earns login from its own argv still
//!    runs the profile chain (both `-lc` and the legacy `-c -l`
//!    spellings), and a genuine nested interactive child of a login
//!    session keeps its OWN startup branch (rc, not the parent's
//!    profile chain).
//!
//! Every run is watchdog-bounded with counter-file generation
//! counting, so a regression shows up as a failed count assertion
//! (and a killed process tree), never as a hung test suite.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

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
        let base =
            std::env::temp_dir().join(format!("niu-2129-{label}-{}-{nanos}", std::process::id()));
        let home = base.join("home");
        let start = base.join("start");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&start).unwrap();
        Self { home, start }
    }

    /// A login profile that counts its own sourcings.
    fn write_profile_counter(&self) {
        std::fs::write(
            self.home.join(".bash_profile"),
            "printf 'x\\n' >> \"$HOME/profile.count\"\n",
        )
        .unwrap();
    }

    /// An interactive rc that counts its own sourcings.
    fn write_rc_counter(&self) {
        std::fs::write(
            self.home.join(".niubashrc"),
            "printf 'x\\n' >> \"$HOME/rc.count\"\n",
        )
        .unwrap();
    }

    fn count(&self, name: &str) -> usize {
        std::fs::read_to_string(self.home.join(name))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.home.parent().unwrap());
    }
}

/// Run `niu` watchdog-bounded with a scrubbed startup environment:
/// HOME/USERPROFILE point at the fixture home and every startup-shape
/// carrier starts REMOVED, so each test adds exactly the carriers it
/// means to exercise. Returns the output plus whether the watchdog
/// had to kill the run (a hang is a test failure with evidence, not
/// a stuck suite).
fn run_niu(
    args: &[&str],
    home: &Path,
    cwd: &Path,
    envs: &[(&str, String)],
    input: Option<&str>,
    timeout: Duration,
) -> (Output, bool) {
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
        .env_remove("__RUBASH_LOGIN_SHELL")
        .env_remove("__RUBASH_INTERACTIVE")
        .env_remove("__RUBASH_SHELL_PID")
        .env_remove("__RUBASH_COPROC_CHILD")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group so a runaway respawn tree can be killed
        // as a unit on watchdog expiry.
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .unwrap_or_else(|err| panic!("spawn niu: {err}"));
    if let Some(input) = input {
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(input.as_bytes());
        drop(stdin);
    } else {
        drop(child.stdin.take());
    }
    let stdout_pipe = child.stdout.take().unwrap();
    let stderr_pipe = child.stderr.take().unwrap();
    // Reader threads report through channels: a descendant that keeps
    // a pipe write-end open past the child's death must never hang
    // the test on a join — collection below is grace-bounded and a
    // missing report degrades to empty output (the watchdog flag and
    // the counter files carry the verdict in that case).
    let (stdout_tx, stdout_rx) = std::sync::mpsc::channel();
    let (stderr_tx, stderr_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut pipe = stdout_pipe;
        let _ = pipe.read_to_end(&mut buf);
        let _ = stdout_tx.send(buf);
    });
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut pipe = stderr_pipe;
        let _ = pipe.read_to_end(&mut buf);
        let _ = stderr_tx.send(buf);
    });
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().unwrap() {
            Some(status) => break status,
            None => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    kill_tree(&mut child);
                    break child.wait().unwrap();
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };
    let output = Output {
        status,
        stdout: stdout_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_default(),
        stderr: stderr_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_default(),
    };
    (output, timed_out)
}

/// Kill the run's whole process tree. Membership is decided by
/// process GROUP, not ancestry: the child was spawned as a group
/// leader (`process_group(0)`) and every respawn generation inherits
/// the group, while killing chain links by ancestry orphans the tail
/// segments (reparented to init, still self-sustaining). An external
/// `kill -<pgid>` is not used either — the negative-pid argument is
/// parsed inconsistently across `kill` implementations — so members
/// are enumerated through /proc and signalled by positive pid. The
/// sweep repeats because a live generation keeps spawning during it.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pgid = child.id();
        for _ in 0..50 {
            let members = process_group_members(pgid);
            if members.is_empty() {
                break;
            }
            for pid in members {
                let _ = Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .status();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let _ = child.kill();
}

#[cfg(unix)]
fn process_group_members(pgid: u32) -> Vec<u32> {
    let mut members = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            // stat: pid (comm) state ppid pgrp ... — comm may contain
            // spaces/parens, so fields are counted from the LAST ')'.
            let Some(close) = stat.rfind(')') else {
                continue;
            };
            let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
            if rest.get(2).and_then(|s| s.parse::<u32>().ok()) == Some(pgid) {
                members.push(pid);
            }
        }
    }
    members
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

const BODY: &str = "printf body-ok";
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Part 1: an inherited login carrier alone must not make a plain
/// `-c` child adopt the login shape (the profile chain stays cold).
#[test]
fn inherited_login_carrier_does_not_adopt_profile_on_dash_c() {
    let fixture = Fixture::new("login-carrier");
    fixture.write_profile_counter();
    fixture.write_rc_counter();
    let (output, timed_out) = run_niu(
        &["-c", BODY],
        &fixture.home,
        &fixture.start,
        &[("__RUBASH_LOGIN_SHELL", "1".to_string())],
        None,
        RUN_TIMEOUT,
    );
    assert!(!timed_out, "plain -c must terminate");
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert_eq!(stdout_of(&output), "body-ok");
    assert_eq!(
        fixture.count("profile.count"),
        0,
        "an inherited __RUBASH_LOGIN_SHELL must not source the profile chain"
    );
    assert_eq!(fixture.count("rc.count"), 0);
}

/// Part 1: the issue's minimal repro shape — BOTH carriers inherited
/// by a `-c` child. Neither the profile chain nor the rc may run.
#[test]
fn inherited_both_carriers_do_not_adopt_on_dash_c() {
    let fixture = Fixture::new("both-carriers");
    fixture.write_profile_counter();
    fixture.write_rc_counter();
    let (output, timed_out) = run_niu(
        &["-c", BODY],
        &fixture.home,
        &fixture.start,
        &[
            ("__RUBASH_LOGIN_SHELL", "1".to_string()),
            ("__RUBASH_INTERACTIVE", "1".to_string()),
        ],
        None,
        RUN_TIMEOUT,
    );
    assert!(!timed_out, "the two-carrier -c shape must terminate");
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert_eq!(stdout_of(&output), "body-ok");
    assert_eq!(fixture.count("profile.count"), 0);
    assert_eq!(fixture.count("rc.count"), 0);
}

/// Part 1 (guard): the interactive carrier alone on the `-c` route
/// likewise sources no startup file.
#[test]
fn inherited_interactive_carrier_alone_does_not_adopt_on_dash_c() {
    let fixture = Fixture::new("interactive-carrier");
    fixture.write_profile_counter();
    fixture.write_rc_counter();
    let (output, timed_out) = run_niu(
        &["-c", BODY],
        &fixture.home,
        &fixture.start,
        &[("__RUBASH_INTERACTIVE", "1".to_string())],
        None,
        RUN_TIMEOUT,
    );
    assert!(!timed_out);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert_eq!(stdout_of(&output), "body-ok");
    assert_eq!(fixture.count("profile.count"), 0);
    assert_eq!(fixture.count("rc.count"), 0);
}

/// Part 2: a respawned internal child sources NOTHING on the `-c`
/// route — no profile chain even with the login carrier present, and
/// no BASH_ENV file either. Both marker spellings are exercised.
#[test]
fn respawn_child_sources_nothing_on_dash_c() {
    for marker in ["__RUBASH_SHELL_PID", "__RUBASH_COPROC_CHILD"] {
        let fixture = Fixture::new("respawn-c");
        fixture.write_profile_counter();
        fixture.write_rc_counter();
        let env_file = fixture.home.join("agent.env");
        std::fs::write(&env_file, "printf 'x\\n' >> \"$HOME/bashenv.count\"\n").unwrap();
        let marker_value = if marker == "__RUBASH_SHELL_PID" {
            std::process::id().to_string()
        } else {
            "1".to_string()
        };
        let (output, timed_out) = run_niu(
            &["-c", BODY],
            &fixture.home,
            &fixture.start,
            &[
                (marker, marker_value),
                ("__RUBASH_LOGIN_SHELL", "1".to_string()),
                ("BASH_ENV", env_file.to_string_lossy().into_owned()),
            ],
            None,
            RUN_TIMEOUT,
        );
        assert!(!timed_out, "{marker}: respawn child must terminate");
        assert!(output.status.success(), "{marker}: {}", stdout_of(&output));
        assert_eq!(stdout_of(&output), "body-ok", "{marker}");
        assert_eq!(
            fixture.count("profile.count"),
            0,
            "{marker}: a respawned child sources no profile chain"
        );
        assert_eq!(
            fixture.count("bashenv.count"),
            0,
            "{marker}: a respawned child sources no BASH_ENV file"
        );
        assert_eq!(fixture.count("rc.count"), 0, "{marker}");
    }
}

/// Part 2, interactive funnel: a respawn-marked process that reaches
/// the interactive startup files sources none of them.
#[test]
fn respawn_child_skips_interactive_startup_files() {
    let fixture = Fixture::new("respawn-interactive");
    fixture.write_profile_counter();
    fixture.write_rc_counter();
    let (output, timed_out) = run_niu(
        &["-i", "-c", BODY],
        &fixture.home,
        &fixture.start,
        &[
            ("__RUBASH_SHELL_PID", std::process::id().to_string()),
            ("__RUBASH_LOGIN_SHELL", "1".to_string()),
            ("__RUBASH_INTERACTIVE", "1".to_string()),
        ],
        None,
        RUN_TIMEOUT,
    );
    assert!(!timed_out);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert_eq!(stdout_of(&output), "body-ok");
    assert_eq!(fixture.count("profile.count"), 0);
    assert_eq!(fixture.count("rc.count"), 0);
}

/// Part 3 (guard): login earned from the session's OWN argv still
/// runs the profile chain — in both spellings, the engine route's
/// `-lc` and the launcher's legacy `-c -l`.
#[test]
fn own_login_flag_still_sources_profile_on_dash_c() {
    for args in [
        vec!["-l", "-c", BODY],
        vec!["-c", "-l", BODY],
        vec!["--login", "-c", BODY],
    ] {
        let fixture = Fixture::new("own-login");
        fixture.write_profile_counter();
        fixture.write_rc_counter();
        let (output, timed_out) =
            run_niu(&args, &fixture.home, &fixture.start, &[], None, RUN_TIMEOUT);
        assert!(!timed_out, "{args:?}");
        assert!(output.status.success(), "{args:?}: {}", stdout_of(&output));
        assert_eq!(stdout_of(&output), "body-ok", "{args:?}");
        assert_eq!(
            fixture.count("profile.count"),
            1,
            "{args:?}: own -l must source the profile chain exactly once"
        );
        assert_eq!(
            fixture.count("rc.count"),
            0,
            "{args:?}: the login branch replaces the rc branch"
        );
    }
}

/// Part 3 (guard): a genuine nested interactive child of a login
/// session keeps its OWN startup branch. The outer session is a real
/// login shell (its executor exports the login carrier to children —
/// the inheritance that caused the defect); the inner `-i` child did
/// not earn login, so it must take the rc branch, not adopt the
/// parent's profile chain.
#[test]
fn nested_interactive_child_keeps_its_own_rc_branch() {
    let fixture = Fixture::new("nested-branch");
    std::fs::write(
        fixture.home.join(".bash_profile"),
        "export STARTUP_MARKER=from_profile\nprintf 'x\\n' >> \"$HOME/profile.count\"\n",
    )
    .unwrap();
    std::fs::write(
        fixture.home.join(".niubashrc"),
        "export STARTUP_MARKER=from_rc\nprintf 'x\\n' >> \"$HOME/rc.count\"\n",
    )
    .unwrap();
    let inner = "\"$NIU_UNDER_TEST\" -i -c 'printf %s \"${STARTUP_MARKER:-unset}\"'";
    let (output, timed_out) = run_niu(
        &["-l", "-c", inner],
        &fixture.home,
        &fixture.start,
        &[(
            "NIU_UNDER_TEST",
            niu_binary().to_string_lossy().into_owned(),
        )],
        None,
        RUN_TIMEOUT,
    );
    assert!(!timed_out);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert_eq!(
        stdout_of(&output),
        "from_rc",
        "the nested -i child must take its own rc branch, not the inherited login branch"
    );
    assert_eq!(
        fixture.count("profile.count"),
        1,
        "only the outer login session sources the profile"
    );
    assert_eq!(
        fixture.count("rc.count"),
        1,
        "the inner child sources the rc exactly once"
    );
}

/// End-to-end, the field shape: an interactive LOGIN niu whose
/// profile chains into an rc carrying the fzf procsub line completes
/// startup, answers commands, and the rc body runs in exactly ONE
/// generation (the procsub child sources nothing and does not
/// recurse).
#[test]
fn fzf_shape_interactive_login_startup_terminates() {
    let fixture = Fixture::new("fzf-shape");
    std::fs::write(fixture.home.join(".bash_profile"), ". \"$HOME/.bashrc\"\n").unwrap();
    std::fs::write(
        fixture.home.join(".bashrc"),
        "printf 'x\\n' >> \"$HOME/generations\"\n\
         __fzf_orig_completion() { local _l; while IFS= read -r _l; do :; done; }\n\
         __fzf_orig_completion < <(complete -p unalias kill ssh 2>/dev/null)\n",
    )
    .unwrap();
    let (output, timed_out) = run_niu(
        &["-l", "-i"],
        &fixture.home,
        &fixture.start,
        &[],
        Some("echo NIU_ALIVE\nexit\n"),
        RUN_TIMEOUT,
    );
    assert!(
        !timed_out,
        "interactive login startup with the fzf procsub line must terminate \
         (generations so far: {})",
        fixture.count("generations")
    );
    assert!(
        stdout_of(&output).contains("NIU_ALIVE"),
        "the shell must answer commands after startup: {}",
        stdout_of(&output)
    );
    assert_eq!(
        fixture.count("generations"),
        1,
        "the rc body must run in exactly one generation — the procsub \
         respawn child sources nothing"
    );
}
