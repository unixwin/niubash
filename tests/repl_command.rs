//! Binary-level tests for the non-interactive REPL command surface.
use std::io::Write;
use std::path::{Path, PathBuf};
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

#[test]
fn repl_command_loads_primary_rc_aliases_after_long_path_setup() {
    let temp = unique_temp_dir("niubash-repl-command-primary-aliases");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(home.join("bin")).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::write(
        home.join(".niubashrc"),
        r#"
__test_path_prepend() {
  [ -n "$1" ] || return 0
  [ -d "$1" ] || return 0
  case ";$PATH;" in
    *";$1;"*) ;;
    *) PATH="$1;$PATH" ;;
  esac
}
__test_path_prepend "$HOME/bin"
alias l='printf "alias-l:ok\n"'
alias ll='printf "alias-ll:ok\n"'
unset -f __test_path_prepend
export PATH
"#,
    )
    .unwrap();

    let long_path = (0..1_200)
        .map(|index| format!("C:/niubash-test/path{index:04}"))
        .collect::<Vec<_>>()
        .join(";");
    let output = Command::new(niu_binary())
        .args(["-C", "l; ll; alias l; alias ll"])
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", long_path)
        .output()
        .unwrap_or_else(|err| panic!("failed to run primary rc alias test: {err}"));

    assert_success(&output, "repl command primary rc aliases");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("alias-l:ok"),
        "alias l was not expanded: {stdout:?}"
    );
    assert!(
        stdout.contains("alias-ll:ok"),
        "alias ll was not expanded: {stdout:?}"
    );
    assert!(
        stdout.contains("alias l="),
        "alias l was not loaded: {stdout:?}"
    );
    assert!(
        stdout.contains("alias ll="),
        "alias ll was not loaded: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn command_mode_keeps_script_semantics_without_repl_startup() {
    let temp = unique_temp_dir("niubash-command-mode");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::write(home.join(".winshrc"), "export NIU_REPL_COMMAND_RC=loaded\n").unwrap();

    let output = run_niu(
        &[
            "-c",
            "echo rc:$NIU_REPL_COMMAND_RC precmd:$NIU_REPL_PRECMD_RAN preexec:$NIU_REPL_PREEXEC_RAN",
        ],
        &start,
        &home,
    );

    assert_success(&output, "command mode");
    assert_eq!(
        stdout_text(&output).trim(),
        "rc: precmd: preexec:",
        "ordinary -c must stay on the script command path"
    );
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn command_mode_can_source_user_winshrc_explicitly() {
    let temp = unique_temp_dir("niubash-command-mode-source-winshrc");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::write(
        home.join(".winshrc"),
        "export NIU_EXPLICIT_SOURCE_RC=loaded\n",
    )
    .unwrap();

    let output = run_niu(
        &["-c", "source ~/.winshrc; echo rc:$NIU_EXPLICIT_SOURCE_RC"],
        &start,
        &home,
    );

    assert_success(&output, "command mode explicit source ~/.winshrc");
    assert_eq!(stdout_text(&output).trim(), "rc:loaded");
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn repl_command_cat_expands_tilde_paths_through_normal_command_resolution() {
    let temp = unique_temp_dir("niubash-repl-command-cat-tilde");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::write(home.join(".winshrc"), "export NIU_TILDE_CAT_RC=loaded\n").unwrap();

    let output = run_niu(&["-C", "cat ~/.winshrc"], &start, &home);

    assert_success(&output, "repl command cat tilde expansion");
    assert_eq!(
        stdout_text(&output).trim(),
        "export NIU_TILDE_CAT_RC=loaded"
    );
    let _ = std::fs::remove_dir_all(temp);
}

/// Two contracts ride on this compound `-c` line, split by the rubash#556
/// (P0 rubash#537) ruling (2026-10-12, owner-adjudicated; full archaeology in
/// the rubash PR): PWD form is shell semantics, so `$PWD` and `pwd` render the
/// POSIX persona form (`/c/...`) unconditionally — the native-style hatch that
/// starved nvm's find_up loop is retired engine-side AND host-side (niu's
/// host cwd sync stamps the same canonical renderer). Native drive spelling
/// stays correct only at the Win32 spawn boundary (path contract): the
/// tilde-expanded argv handed to the external `cat.cmd` must remain usable,
/// and the shim fails the command (`exit /b 12`) if it ever sees a slash-drive
/// first argument. The old `command_mode_compound_commands_keep_home_paths_
/// native` pin asserted the pre-#556 native PWD rendering and was red on
/// every niubash PR (#278 first) once `branch = master` pulled #556 in.
/// The assertions pin the persona FORM (drive prefix + home basename) rather
/// than full-path bytes: a first-cut full-string comparison was red on the
/// CI merge-ref (PR #283's first run) while green locally — environment
/// path-spelling variance is not what this test adjudicates.
#[test]
fn command_mode_compound_commands_posix_pwd_and_native_spawn_argv() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("niubash-command-mode-native-home-paths");
    let home = temp.join("home");
    let start = temp.join("start");
    let bin = temp.join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(home.join(".niubashrc"), "# primary marker\n").unwrap();
    std::fs::write(
        bin.join("cat.cmd"),
        "@echo off\r\nset \"arg=%~1\"\r\necho arg=%arg%\r\nif \"%arg:~0,3%\"==\"/c/\" exit /b 12\r\nset \"fsarg=%arg:/=\\%\"\r\ntype \"%fsarg%\"\r\n",
    )
    .unwrap();

    let old_path = std::env::var_os("PATH");
    let mut paths = vec![bin.clone()];
    if let Some(old_path) = old_path {
        paths.extend(std::env::split_paths(&old_path));
    }
    let output = Command::new(niu_binary())
        .args([
            "-c",
            "cd ~; echo PWD=$PWD; pwd; cat ~/.niubashrc >/dev/null && echo catrc:ok",
        ])
        .current_dir(&start)
        .env("HOME", "")
        .env("USERPROFILE", &home)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .output()
        .unwrap_or_else(|err| panic!("failed to run niubash command mode native home test: {err}"));

    assert_success(&output, "command mode compound native home paths");
    let stdout = stdout_text(&output);
    // Spawn boundary (unchanged by rubash#556): the external cat.cmd must
    // receive a usable native-form path, not the persona display form.
    assert!(stdout.contains("catrc:ok"), "stdout was {stdout:?}");
    // PWD rendering (rubash#556/#537): POSIX persona form, unconditionally.
    // Pin the FORM, not the full spelling: the drive prefix and the hermetic
    // home basename are stable across environments, while full-path equality
    // between the test-side string and the engine's rendering is not the
    // contract under test (CI runners and the engine may disagree on 8.3
    // short-name vs long-name spellings of the same directory).
    let home_drive = drive_letter(&home).expect("temp home sits on a drive letter");
    let pwd_line = stdout
        .lines()
        .find(|line| line.starts_with("PWD="))
        .unwrap_or_else(|| panic!("compound -c must echo PWD=: {stdout:?}"));
    assert!(
        pwd_line.starts_with(&format!("PWD=/{home_drive}/")),
        "$PWD must render the POSIX persona form (rubash#556/#537): {stdout:?}"
    );
    assert!(
        pwd_line.ends_with("/home"),
        "cd ~ must land in the hermetic home, in persona form: {stdout:?}"
    );
    assert!(
        !stdout.contains(&format!("PWD={}:/", home_drive.to_ascii_uppercase()))
            && !stdout.contains(&format!("PWD={home_drive}:/")),
        "PWD must not render native drive form (rubash#556 retired the hatch): {stdout:?}"
    );
    assert!(
        stdout.lines().any(|line| {
            line.starts_with(&format!("/{home_drive}/")) && line.ends_with("/home")
        }),
        "pwd must render the POSIX persona form (rubash#556/#537): {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(temp);
}

/// The lowercased drive letter of a Windows path, for persona prefixes.
fn drive_letter(path: &Path) -> Option<char> {
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    (bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic())
        .then(|| (bytes[0] as char).to_ascii_lowercase())
}

#[test]
fn repl_command_file_commands_expand_tilde_paths_through_normal_command_resolution() {
    let temp = unique_temp_dir("niubash-repl-command-file-builtins-tilde");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::write(home.join(".niubashrc"), "").unwrap();

    let output = run_niu(
        &[
            "-C",
            "mkdir -p ~/builtins/empty; touch ~/builtins/source.txt; cp ~/builtins/source.txt ~/builtins/copy.txt; rm ~/builtins/source.txt; rmdir ~/builtins/empty",
        ],
        &start,
        &home,
    );

    assert_success(&output, "repl command file command tilde expansion");
    assert!(home.join("builtins").join("copy.txt").is_file());
    assert!(!home.join("builtins").join("source.txt").exists());
    assert!(!home.join("builtins").join("empty").exists());
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn command_mode_sets_shell_to_current_exe_when_missing() {
    let temp = unique_temp_dir("niubash-command-mode-shell-env");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let bin = niu_binary();
    let output = Command::new(&bin)
        .args(["-c", "printf '<%s>\\n' \"$SHELL\""])
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env_remove("SHELL")
        .env_remove("BASH")
        .output()
        .unwrap_or_else(|err| panic!("failed to run niubash shell env test: {err}"));

    assert_success(&output, "command mode default SHELL");
    assert_eq!(
        stdout_text(&output).trim(),
        format!("<{}>", display_path(&bin))
    );
    let _ = std::fs::remove_dir_all(temp);
}

fn run_niu(args: &[&str], start: &Path, home: &Path) -> Output {
    let mut command = Command::new(niu_binary());
    command
        .args(args)
        .current_dir(start)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("NIU_REPL_COMMAND_RC")
        .env_remove("NIU_REPL_PRECMD_RAN")
        .env_remove("NIU_REPL_PREEXEC_RAN");

    command
        .output()
        .unwrap_or_else(|err| panic!("failed to run niubash {args:?}: {err}"))
}

/// Run niu with piped stdin (the interactive `-i` driver reads commands from
/// the pipe until EOF; prompts go to stderr, rc/command output to stdout).
fn run_niu_with_stdin(args: &[&str], stdin_data: &str, start: &Path, home: &Path) -> Output {
    let mut child = Command::new(niu_binary())
        .args(args)
        .current_dir(start)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("failed to spawn niubash {args:?}: {err}"));
    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        stdin
            .write_all(stdin_data.as_bytes())
            .and_then(|()| stdin.flush())
            .expect("write niu stdin");
    }
    // Dropping stdin closes the pipe: the interactive stdin driver sees EOF
    // and the shell exits like GNU `bash -i < file`.
    drop(child.stdin.take());
    child
        .wait_with_output()
        .unwrap_or_else(|err| panic!("failed to wait for niubash {args:?}: {err}"))
}

// ---------------------------------------------------------------------------
// niubash#146: GNU decides "interactive" from the -i flag, never from the
// shape of stdin (shell.c option parsing sets forced_interactive before
// run_startup_files sources ~/.bashrc). `printf 'cmd\n' | niu -i` must load
// the interactive rc exactly like a terminal session does.
// ---------------------------------------------------------------------------

#[test]
fn piped_dash_i_sources_startup_rc() {
    let temp = unique_temp_dir("niubash-piped-i-rc");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::write(home.join(".niubashrc"), "echo RC_146_MARK\n").unwrap();

    let output = run_niu_with_stdin(&["-i"], "echo RC_146_DONE\n", &start, &home);

    assert_success(&output, "piped -i rc loading");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("RC_146_MARK"),
        "piped -i skipped ~/.niubashrc (niubash#146): {stdout:?}"
    );
    assert!(
        stdout.contains("RC_146_DONE"),
        "piped -i did not run the piped command: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn piped_dash_i_norc_and_rcfile_options_apply() {
    let temp = unique_temp_dir("niubash-piped-i-rcfile");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::write(home.join(".niubashrc"), "echo RC_146_DEFAULT\n").unwrap();
    let rcfile = home.join("alt-rc");
    std::fs::write(&rcfile, "export RC_146_ALTFROM=altfile\n").unwrap();
    let rcfile = rcfile.to_string_lossy().replace('\\', "/").to_string();

    // --norc: neither the default rc nor anything else runs.
    let output = run_niu_with_stdin(
        &["--norc", "-i"],
        "echo n:${RC_146_DEFAULT-set}\n",
        &start,
        &home,
    );
    assert_success(&output, "piped --norc -i");
    let stdout = stdout_text(&output);
    assert!(
        !stdout.contains("RC_146_DEFAULT"),
        "--norc -i still sourced ~/.niubashrc: {stdout:?}"
    );

    // --rcfile: the alternate file is sourced instead.
    let output = run_niu_with_stdin(
        &["--rcfile", &rcfile, "-i"],
        "echo alt:[$RC_146_ALTFROM]\n",
        &start,
        &home,
    );
    assert_success(&output, "piped --rcfile -i");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("alt:[altfile]"),
        "--rcfile -i did not source the alternate rc (niubash#146): {stdout:?}"
    );
    assert!(
        !stdout.contains("RC_146_DEFAULT"),
        "--rcfile -i also sourced the default rc: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(temp);
}

// ---------------------------------------------------------------------------
// niubash#148: launcher words keep their meaning wherever they stand among
// the leading option words (GNU parse_shell_options is one left-to-right
// pass). `niu --norc -C 'cmd'` used to fall to the engine parser, which has
// no -C REPL-command flag (GNU -C is noclobber), and then treated the
// command string as a missing script file.
// ---------------------------------------------------------------------------

#[test]
fn repl_command_accepts_leading_options_in_any_order() {
    let temp = unique_temp_dir("niubash-repl-command-leading-options");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::write(home.join(".niubashrc"), "echo RC_148_DEFAULT_RAN\n").unwrap();
    let rcfile = home.join("alt148");
    std::fs::write(&rcfile, "export RC_148_ALTFROM=altfile\n").unwrap();
    let rcfile = rcfile.to_string_lossy().replace('\\', "/").to_string();

    // --norc before -C: dispatches to the REPL command, rc suppressed.
    let output = run_niu(
        &["--norc", "-C", "echo order:[$RC_148_ALTFROM]"],
        &start,
        &home,
    );
    assert_success(&output, "--norc before -C");
    let stdout = stdout_text(&output);
    assert_eq!(
        stdout.trim(),
        "order:[]",
        "--norc -C must dispatch to the REPL command without sourcing rc: {stdout:?}"
    );

    // --rcfile before -C: the alternate rc is sourced for the command.
    let output = run_niu(
        &["--rcfile", &rcfile, "-C", "echo order:[$RC_148_ALTFROM]"],
        &start,
        &home,
    );
    assert_success(&output, "--rcfile before -C");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("order:[altfile]"),
        "--rcfile before -C did not apply to the REPL command: {stdout:?}"
    );
    assert!(
        !stdout.contains("RC_148_DEFAULT_RAN"),
        "--rcfile before -C also sourced the default rc: {stdout:?}"
    );

    // Plain -C still loads the default rc (baseline, unchanged).
    let output = run_niu(&["-C", "echo plain:[$RC_148_ALTFROM]"], &start, &home);
    assert_success(&output, "plain -C rc");
    assert!(
        stdout_text(&output).contains("RC_148_DEFAULT_RAN"),
        "plain -C stopped sourcing ~/.niubashrc"
    );
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn invalid_leading_option_before_repl_command_keeps_gnu_usage_surface() {
    let temp = unique_temp_dir("niubash-repl-command-invalid-option");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_niu(&["--bogus-flag", "-C", "echo x"], &start, &home);
    assert_eq!(
        output.status.code(),
        Some(2),
        "invalid option before -C must be a usage error (EX_BADUSAGE), got {:?}",
        output.status.code()
    );
    let stderr = stderr_text(&output);
    assert!(
        stderr.contains("bash: --bogus-flag: invalid option"),
        "missing GNU invalid-option diagnostic: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(temp);
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        stdout_text(output),
        stderr_text(output)
    );
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n")
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
}
