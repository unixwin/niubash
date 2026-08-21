//! Windows-native host contract tests for the `winuxsh -c` surface.
//!
//! These tests intentionally exercise the built binary instead of internal
//! helpers: this is the contract humans and agents rely on.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn winuxsh_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_winuxsh"));
    if p.exists() {
        return p;
    }

    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) {
        "winuxsh.exe"
    } else {
        "winuxsh"
    });
    fallback
}

#[test]
fn cwd_cd_pwd_and_windows_child_process_agree() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-cwd");
    let home = temp.join("home");
    let start = temp.join("start");
    let target = temp.join("target");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::create_dir_all(&target).unwrap();

    let target_shell_path = shell_path(&target);
    let script = format!("cd {}; pwd; cmd.exe /C cd", shell_quote(&target_shell_path));
    let output = run_winuxsh(&script, &start, &home, &[]);
    assert_success(&output, "cwd contract");

    let stdout = stdout_lines(&output);
    assert_eq!(stdout.len(), 2, "stdout was {stdout:?}");
    assert_same_path(&stdout[0], &target_shell_path);
    assert_same_path(&stdout[1], &target_shell_path);
    assert!(
        !stdout[0].starts_with("/c/"),
        "pwd must prefer Windows-native drive paths, got {:?}",
        stdout[0]
    );

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn history_and_fc_use_the_host_history_provider() {
    let temp = unique_temp_dir("winuxsh-host-history-provider");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    let history = home.join(".winuxsh_history");
    std::fs::write(&history, "first command\nsecond command\n").unwrap();

    let output = run_winuxsh("history; fc 2", &start, &home, &[]);
    assert_success(&output, "host history provider");
    let stdout = normalize_text(&output.stdout);
    assert!(stdout.contains("1  first command"), "stdout was {stdout:?}");
    assert!(
        stdout.contains("2  second command"),
        "stdout was {stdout:?}"
    );

    let saved = run_winuxsh("history -s third command", &start, &home, &[]);
    assert_success(&saved, "host history save");
    assert!(std::fs::read_to_string(&history)
        .unwrap()
        .contains("third command"));

    let deleted = run_winuxsh("history -d 2", &start, &home, &[]);
    assert_success(&deleted, "host history delete");
    let after_delete = std::fs::read_to_string(&history).unwrap();
    assert!(!after_delete.contains("second command"));
    assert!(after_delete.contains("third command"));

    let cleared = run_winuxsh("history -c", &start, &home, &[]);
    assert_success(&cleared, "host history clear");
    assert_eq!(std::fs::read_to_string(&history).unwrap(), "");
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn host_shell_keeps_rubash_history_storage_disabled() {
    let temp = unique_temp_dir("winuxsh-host-history-owner");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    let rubash_history = temp.join("rubash-history");

    let output = run_winuxsh(
        "test -z \"$HISTFILE\"",
        &start,
        &home,
        &[("HISTFILE", rubash_history.to_string_lossy().into_owned())],
    );

    assert_success(&output, "host history ownership");
    assert!(
        !rubash_history.exists(),
        "Rubash must not create a second host history file"
    );
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn bash_style_noexec_option_reaches_rubash() {
    let temp = unique_temp_dir("winuxsh-host-noexec");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = Command::new(winuxsh_binary())
        .args(["-n", "-c", "printf should-not-run"])
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .unwrap();

    assert_success(&output, "Bash -n host invocation");
    assert!(
        output.stdout.is_empty(),
        "-n unexpectedly executed the command"
    );
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn bash_style_errexit_option_reaches_rubash() {
    let temp = unique_temp_dir("winuxsh-host-errexit");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = Command::new(winuxsh_binary())
        .args(["-e", "-c", "false; printf should-not-run"])
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "-e continued after failure");
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn slash_drive_paths_are_compat_input_not_default_output() {
    if !cfg!(windows) {
        return;
    }

    let users = PathBuf::from(r"C:\Users");
    if !users.is_dir() {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-slash-drive");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh("cd /c/Users; pwd", &start, &home, &[]);
    assert_success(&output, "slash-drive cwd contract");

    let stdout = stdout_lines(&output);
    assert_eq!(stdout.len(), 1, "stdout was {stdout:?}");
    assert_same_path(&stdout[0], "C:/Users");
    assert!(
        !stdout[0].starts_with("/c/"),
        "compat input must not become default output, got {:?}",
        stdout[0]
    );

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn slash_drive_mktemp_template_creates_file() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-slash-drive-mktemp");
    let home = temp.join("home");
    let start = temp.join("start");
    let output_dir = temp.join("backups").join("winuxsh-phase12");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let Some(output_dir_slash_drive) = slash_drive_path(&output_dir) else {
        let _ = std::fs::remove_dir_all(temp);
        return;
    };

    let output = run_winuxsh(
        &format!(
            "mkdir -p {dir} && mktemp {dir}/test.XXXXXX.tmp",
            dir = shell_quote(&output_dir_slash_drive)
        ),
        &start,
        &home,
        &[],
    );
    assert_success(&output, "slash-drive mktemp template");

    let stdout = stdout_lines(&output);
    assert_eq!(stdout.len(), 1, "stdout was {stdout:?}");
    assert!(
        !stdout[0].starts_with("/c/"),
        "mktemp should print Windows-native output, got {:?}",
        stdout[0]
    );
    assert!(
        PathBuf::from(stdout[0].replace('/', "\\")).is_file(),
        "mktemp output should name an existing file: {:?}",
        stdout[0]
    );

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn winshrc_does_not_run_for_non_interactive_modes() {
    let temp = unique_temp_dir("winuxsh-host-winshrc");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    std::fs::write(
        home.join(".winshrc"),
        r#"
echo SHOULD_NOT_PRINT
"#,
    )
    .unwrap();

    let output = run_winuxsh("echo command-ok", &start, &home, &[]);
    assert_success(&output, "command-mode .winshrc isolation");
    assert_eq!(normalize_text(&output.stdout), "command-ok");

    let script = temp.join("script.sh");
    std::fs::write(&script, "echo script-ok\n").unwrap();
    let output = Command::new(winuxsh_binary())
        .arg(&script)
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .unwrap_or_else(|err| panic!("spawn winuxsh script file: {err}"));
    assert_success(&output, "script-file .winshrc isolation");
    assert_eq!(normalize_text(&output.stdout), "script-ok");

    let mut child = Command::new(winuxsh_binary())
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn winuxsh stdin script: {err}"));
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"echo stdin-ok\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_success(&output, "stdin-script .winshrc isolation");
    assert_eq!(normalize_text(&output.stdout), "stdin-ok");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn command_mode_sets_bash_execution_string() {
    let temp = unique_temp_dir("winuxsh-host-bash-execution-string");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let script = "printf '%s' \"$BASH_EXECUTION_STRING\"";
    let output = run_winuxsh(script, &start, &home, &[]);
    assert_success(&output, "BASH_EXECUTION_STRING");
    assert_eq!(normalize_text(&output.stdout), script);

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn rubash_executor_sees_winuxsh_shell_name() {
    let temp = unique_temp_dir("winuxsh-host-shell-name");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh("printf '%s' \"$__RUBASH_SHELL_NAME\"", &start, &home, &[]);
    assert_success(&output, "rubash shell name");
    assert!(
        normalize_text(&output.stdout)
            .to_ascii_lowercase()
            .contains("winuxsh"),
        "stdout was {:?}",
        normalize_text(&output.stdout)
    );

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn starship_receives_bash_shell_identity() {
    let temp = unique_temp_dir("winuxsh-host-starship-shell");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh("printf '%s' \"$STARSHIP_SHELL\"", &start, &home, &[]);
    assert_success(&output, "Starship shell name");
    assert_eq!(normalize_text(&output.stdout), "bash");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn exit_trap_runs_for_non_interactive_modes() {
    let temp = unique_temp_dir("winuxsh-host-exit-trap");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let command_marker = temp.join("command-marker");
    let output = run_winuxsh(
        &format!(
            "trap 'printf command > {}' EXIT; true",
            shell_quote(&shell_path(&command_marker))
        ),
        &start,
        &home,
        &[],
    );
    assert_success(&output, "command-mode EXIT trap");
    assert_eq!(std::fs::read_to_string(&command_marker).unwrap(), "command");

    let script_marker = temp.join("script-marker");
    let script = temp.join("script.sh");
    std::fs::write(
        &script,
        format!(
            "trap 'printf script > {}' EXIT\ntrue\n",
            shell_quote(&shell_path(&script_marker))
        ),
    )
    .unwrap();
    let output = Command::new(winuxsh_binary())
        .arg(&script)
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .unwrap_or_else(|err| panic!("spawn winuxsh script file: {err}"));
    assert_success(&output, "script-file EXIT trap");
    assert_eq!(std::fs::read_to_string(&script_marker).unwrap(), "script");

    let stdin_marker = temp.join("stdin-marker");
    let mut child = Command::new(winuxsh_binary())
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn winuxsh stdin script: {err}"));
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(
            format!(
                "trap 'printf stdin > {}' EXIT\ntrue\n",
                shell_quote(&shell_path(&stdin_marker))
            )
            .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_success(&output, "stdin-script EXIT trap");
    assert_eq!(std::fs::read_to_string(&stdin_marker).unwrap(), "stdin");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn internal_pipeline_helpers_work_when_rubash_is_embedded() {
    let temp = unique_temp_dir("winuxsh-host-internal-pipeline");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let mut child = Command::new(winuxsh_binary())
        .arg("--internal-wc")
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn winuxsh internal wc: {err}"));
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"one\ntwo\nthree\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_success(&output, "embedded internal wc helper");
    assert_eq!(normalize_text(&output.stdout), "3");

    let output = run_winuxsh("yes ok | head -n 3 | wc", &start, &home, &[]);
    assert_success(&output, "embedded internal pipeline helpers");
    assert!(
        normalize_text(&output.stdout).starts_with('3'),
        "stdout was {:?}",
        normalize_text(&output.stdout)
    );

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn temporary_assignment_reaches_nested_winuxsh_child() {
    let temp = unique_temp_dir("winuxsh-host-nested-env");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh(
        &format!(
            "FOO_WINUXSH_PROBE=bar {} -c 'printf FOO=$FOO_WINUXSH_PROBE'",
            shell_quote(&shell_path(&winuxsh_binary()))
        ),
        &start,
        &home,
        &[],
    );
    assert_success(&output, "temporary assignment reaches nested winuxsh");
    assert_eq!(normalize_text(&output.stdout), "FOO=bar");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn rubash_setopt_is_visible_when_sourcing_startup_rc() {
    let temp = unique_temp_dir("winuxsh-host-setopt");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let rc = home.join(".winshrc");
    std::fs::write(
        &rc,
        r#"
plugins=(
    git
    completion
)
setopt hist_ignore_dups
setopt hist_ignore_space prompt_subst prompt_percent brace_expand tilde_expand variable_expand command_subst arith_expand monitor
unsetopt prompt_percent
export SETOPT_RC_OK=ok
"#,
    )
    .unwrap();

    let output = run_winuxsh(
        &format!(
            "source {}; echo $SETOPT_RC_OK; setopt",
            shell_quote(&shell_path(&rc))
        ),
        &start,
        &home,
        &[],
    );
    assert_success(&output, "source .winshrc setopt");
    assert_eq!(normalize_text(&output.stderr), "");

    let stdout = stdout_lines(&output);
    assert!(stdout.contains(&"ok".to_string()), "stdout was {stdout:?}");
    assert!(
        stdout.contains(&"hist_ignore_dups".to_string()),
        "stdout was {stdout:?}"
    );
    assert!(
        stdout.contains(&"hist_ignore_space".to_string()),
        "stdout was {stdout:?}"
    );
    assert!(
        !stdout.contains(&"prompt_percent".to_string()),
        "stdout was {stdout:?}"
    );

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn native_backslash_drive_paths_work_for_cd() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-native-path");
    let home = temp.join("home");
    let start = temp.join("start");
    let target = temp.join("target");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::create_dir_all(&target).unwrap();

    let target_native_path = native_path(&target);
    let output = run_winuxsh(
        &format!("cd {}; pwd", target_native_path),
        &start,
        &home,
        &[],
    );
    assert_success(&output, "native backslash path cd");
    let stdout = stdout_lines(&output);
    assert_eq!(stdout.len(), 1, "stdout was {stdout:?}");
    assert_same_path(&stdout[0], &shell_path(&target));

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn drive_only_cd_and_bare_drive_commands_switch_to_drive_root() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-drive-switch");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let start_native_path = native_path(&start);
    let bytes = start_native_path.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        let _ = std::fs::remove_dir_all(temp);
        return;
    }

    let drive = (bytes[0] as char).to_ascii_uppercase();
    let drive_root = format!("{drive}:/");
    let start_shell_path = shell_path(&start);

    let output = run_winuxsh(
        &format!("cd {drive}:; pwd; cmd.exe /C cd"),
        &start,
        &home,
        &[],
    );
    assert_success(&output, "drive-only cd");
    let stdout = stdout_lines(&output);
    assert_eq!(stdout.len(), 2, "stdout was {stdout:?}");
    assert_same_path(&stdout[0], &drive_root);
    assert_same_path(&stdout[1], &drive_root);

    let output = run_winuxsh(&format!("{drive}:; pwd; cmd.exe /C cd"), &start, &home, &[]);
    assert_success(&output, "bare drive command");
    let stdout = stdout_lines(&output);
    assert_eq!(stdout.len(), 2, "stdout was {stdout:?}");
    assert_same_path(&stdout[0], &drive_root);
    assert_same_path(&stdout[1], &drive_root);

    let output = run_winuxsh(
        &format!(
            "cd {drive}: && cmd.exe /C cd; cd {start}; {drive}: && cmd.exe /C cd",
            drive = drive,
            start = shell_quote(&start_shell_path)
        ),
        &start,
        &home,
        &[],
    );
    assert_success(&output, "drive commands in and-or lists");
    let stdout = stdout_lines(&output);
    assert_eq!(stdout.len(), 2, "stdout was {stdout:?}");
    assert_same_path(&stdout[0], &drive_root);
    assert_same_path(&stdout[1], &drive_root);

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
#[ignore = "requires real winuxcmd.exe command links; run with --ignored"]
fn native_backslash_drive_paths_work_for_winuxcmd() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-native-path-winuxcmd");
    let home = temp.join("home");
    let start = temp.join("start");
    let target = temp.join("target");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("marker.txt"), "ok").unwrap();
    let winuxcmd = real_winuxcmd_for_test()
        .unwrap_or_else(|| panic!("real winuxcmd.exe with command links is required"));
    let target_native_path = native_path(&target);
    let output = run_winuxsh(
        &format!("ls {}", target_native_path),
        &start,
        &home,
        &[("WINUXCMD_PATH", native_path(&winuxcmd))],
    );
    assert_success(&output, "native backslash path ls");
    let stdout = stdout_lines(&output);
    assert!(
        stdout.iter().any(|line| line.contains("marker.txt")),
        "ls output did not include marker.txt: {stdout:?}"
    );

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn installed_winuxcmd_links_back_logical_bin_namespaces_without_root_copies() {
    if !cfg!(windows) {
        return;
    }

    let Some(winuxcmd) = real_winuxcmd_for_test() else {
        return;
    };
    let Some(winuxcmd_dir) = winuxcmd.parent() else {
        return;
    };
    if !winuxcmd_dir.join("ls.exe").is_file() {
        return;
    }

    let temp = unique_temp_dir("winuxsh-installed-winuxcmd-logical-bin");
    let home = temp.join("home");
    let start = temp.join("start");
    let root = temp.join("root");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh(
        "test -x /usr/bin/ls; test -f /usr/bin/ls; /bin/ls /etc >/dev/null; printf x > /etc/path-contract; test -f /etc/path-contract",
        &start,
        &home,
        &[
            ("WINUXCMD_PATH", native_path(&winuxcmd)),
            ("WINUXSH_ROOT", native_path(&root)),
        ],
    );
    assert_success(&output, "installed WinuxCmd logical bin provider");
    assert!(
        !root.join("usr").join("bin").join("ls.exe").exists(),
        "WinuxCmd links must stay in the installed provider directory"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("etc").join("path-contract")).unwrap(),
        "x"
    );
    assert!(!root.join(".wpm").exists());

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn redirected_recursive_cp_uses_path_command_not_winuxsh_native_builtin() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-cp-native");
    let home = temp.join("home");
    let start = temp.join("start");
    let source = start.join("source");
    let dest = start.join("dest");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(source.join("sub")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(source.join("sub").join("file.txt"), "ok").unwrap();

    let script = format!(
        "cp --version 2>&1; cp -R {}/. {}/ 2>&1; test -f {}/sub/file.txt",
        shell_quote(&shell_path(&source)),
        shell_quote(&shell_path(&dest)),
        shell_quote(&shell_path(&dest))
    );
    let output = run_winuxsh(&script, &start, &home, &[]);

    assert_success(&output, "redirected recursive cp path dispatch");
    let stdout = stdout_lines(&output);
    assert!(
        stdout
            .first()
            .is_some_and(|line| line.starts_with("cp (") && !line.contains("winuxsh native")),
        "cp --version should come from PATH, not winuxsh native cp, got {stdout:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("sub").join("file.txt")).unwrap(),
        "ok"
    );
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn path_lookup_finds_windows_pathextext_commands() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-path");
    let home = temp.join("home");
    let start = temp.join("start");
    let bin = temp.join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        bin.join("hostcontractprobe.cmd"),
        "@echo off\r\necho path-ok\r\n",
    )
    .unwrap();

    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{};{}", native_path(&bin), old_path);
    let output = run_winuxsh(
        "hostcontractprobe",
        &start,
        &home,
        &[
            ("PATH", path),
            ("PATHEXT", ".COM;.EXE;.BAT;.CMD".to_string()),
        ],
    );
    assert_success(&output, "PATH contract");
    assert_eq!(normalize_text(&output.stdout), "path-ok");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn sourced_rc_keeps_winuxcmd_visible_to_windows_children() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-winuxcmd-child-path");
    let home = temp.join("home");
    let start = temp.join("start");
    let bin = temp.join("winuxcmd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["winuxcmd.exe", "ls.exe", "cat.exe", "grep.exe", "ln.exe"] {
        std::fs::write(bin.join(name), b"").unwrap();
    }
    std::fs::write(
        home.join(".winshrc"),
        r#"export PATH="$PATH;C:\does-not-exist-winuxsh-test""#,
    )
    .unwrap();

    let old_path = std::env::var("PATH").unwrap_or_default();
    let output = run_winuxsh(
        "source ~/.winshrc; cmd.exe /D /C set PATH",
        &start,
        &home,
        &[
            ("PATH", old_path),
            ("PATHEXT", ".COM;.EXE;.BAT;.CMD".to_string()),
            ("WINUXCMD_PATH", native_path(&bin.join("winuxcmd.exe"))),
        ],
    );

    assert_success(&output, "source rc keeps winuxcmd on child PATH");
    let stdout = normalize_text(&output.stdout);
    let normalized_stdout = stdout.replace('\\', "/").to_ascii_lowercase();
    let expected_winuxcmd_dirs = [
        native_path(&bin).replace('\\', "/").to_ascii_lowercase(),
        slash_drive_path(&bin)
            .unwrap_or_else(|| shell_path(&bin))
            .to_ascii_lowercase(),
    ];
    assert!(
        expected_winuxcmd_dirs
            .iter()
            .any(|path| normalized_stdout.contains(path)),
        "child PATH did not contain winuxcmd dir {:?}: {stdout:?}",
        expected_winuxcmd_dirs
    );
    assert!(
        normalized_stdout.contains("c:/does-not-exist-winuxsh-test"),
        "stdout was {stdout:?}"
    );

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn exported_env_reaches_windows_child_processes() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-env");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh(
        "export WINUXSH_HOST_CONTRACT=ok; cmd.exe /C echo %WINUXSH_HOST_CONTRACT%",
        &start,
        &home,
        &[],
    );
    assert_success(&output, "env contract");
    assert_eq!(normalize_text(&output.stdout), "ok");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn tilde_resolves_to_normal_windows_home() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-home");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh("cd ~; pwd; cmd.exe /C cd", &start, &home, &[]);
    assert_success(&output, "home contract");

    let expected_home = shell_path(&home);
    let stdout = stdout_lines(&output);
    assert_eq!(stdout.len(), 2, "stdout was {stdout:?}");
    assert_same_path(&stdout[0], &expected_home);
    assert_same_path(&stdout[1], &expected_home);

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn stdout_stderr_and_exit_code_are_preserved() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-stdio");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh("echo out; echo err >&2; exit 7", &start, &home, &[]);
    assert_eq!(
        output.status.code(),
        Some(7),
        "expected exit code 7, got {:?}; stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(normalize_text(&output.stdout), "out");
    assert_eq!(normalize_text(&output.stderr), "err");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn piped_stdin_without_args_runs_plain_script_surface() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-piped-stdin");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let mut child = Command::new(winuxsh_binary())
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn winuxsh: {err}"));

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"printf 'alpha\\nbeta\\n' | grep alpha\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert_success(&output, "piped stdin script surface");
    assert_eq!(normalize_text(&output.stdout), "alpha");
    assert_eq!(normalize_text(&output.stderr), "");
    assert_no_terminal_controls(&output.stdout);
    assert_no_terminal_controls(&output.stderr);

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn piped_stdin_without_args_runs_multiline_compound_block() {
    if !cfg!(windows) {
        return;
    }
    let temp = unique_temp_dir("winuxsh-host-piped-stdin-multiline");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    let mut child = Command::new(winuxsh_binary())
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn winuxsh: {err}"));
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"if true; then\n  echo block-ok\nfi\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_success(&output, "piped stdin multiline block");
    assert_eq!(normalize_text(&output.stdout), "block-ok");
    assert_eq!(normalize_text(&output.stderr), "");
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn piped_stdin_without_args_runs_heredoc_as_one_chunk() {
    if !cfg!(windows) {
        return;
    }
    let temp = unique_temp_dir("winuxsh-host-piped-stdin-heredoc");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    let mut child = Command::new(winuxsh_binary())
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn winuxsh: {err}"));
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"cat <<EOF\nheredoc-ok\nEOF\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_success(&output, "piped stdin heredoc");
    assert_eq!(normalize_text(&output.stdout), "heredoc-ok");
    assert_eq!(normalize_text(&output.stderr), "");
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn piped_stdin_child_script_reads_unconsumed_parent_input() {
    if !cfg!(windows) {
        return;
    }
    let temp = unique_temp_dir("winuxsh-host-stdin-child");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    let sub = start.join("input-line.sub");
    std::fs::write(&sub, "read line\necho line read by $0 was \\`$line\\'\n").unwrap();
    let sub_display = shell_path(&sub);
    let sub_arg = shell_quote(&sub_display);
    let script = format!(
        "echo before calling input-line.sub\n${{THIS_SH}} {sub_arg}\nthis line for input-line.sub\necho finished with input-line.sub\n"
    );
    let mut child = Command::new(winuxsh_binary())
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("THIS_SH", shell_path(&winuxsh_binary()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn winuxsh: {err}"));
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_success(&output, "stdin child inherits unread input");
    assert_eq!(
        normalize_text(&output.stdout),
        format!(
            "before calling input-line.sub\nline read by {sub_display} was `this line for input-line.sub'\nfinished with input-line.sub"
        )
    );
    assert_eq!(normalize_text(&output.stderr), "");
    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn command_mode_grep_capture_stays_plain() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-grep-capture");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh("printf 'alpha\nbeta\n' | grep alpha", &start, &home, &[]);
    assert_success(&output, "captured grep");
    assert_eq!(normalize_text(&output.stdout), "alpha");
    assert_no_terminal_controls(&output.stdout);
    assert_no_terminal_controls(&output.stderr);

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn command_mode_pipeline_first_stage_reads_host_stdin() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-pipeline-stdin");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let mut child = Command::new(winuxsh_binary())
        .arg("-c")
        .arg("grep alpha | cat")
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn winuxsh: {err}"));

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"alpha\nbeta\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert_success(&output, "command mode host stdin pipeline");
    assert_eq!(normalize_text(&output.stdout), "alpha");
    assert_eq!(normalize_text(&output.stderr), "");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn script_file_args_populate_positional_parameters() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-script-args");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    let script = start.join("args.sh");
    std::fs::write(
        &script,
        "printf 'zero=%s n=%s one=%s all=%s\\n' \"$0\" \"$#\" \"$1\" \"$*\"\n",
    )
    .unwrap();

    let output = Command::new(winuxsh_binary())
        .arg(&script)
        .arg("first")
        .arg("second")
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .unwrap_or_else(|err| panic!("spawn winuxsh: {err}"));

    assert_success(&output, "script args");
    let stdout = normalize_text(&output.stdout);
    assert!(stdout.contains("zero="), "{stdout}");
    assert!(stdout.contains("n=2"), "{stdout}");
    assert!(stdout.contains("one=first"), "{stdout}");
    assert!(stdout.contains("all=first second"), "{stdout}");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn script_file_while_read_redirect_does_not_wait_for_parent_stdin() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-script-read-redirect");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    std::fs::write(start.join("list.txt"), "alpha\nbeta\n").unwrap();
    let script = start.join("read-list.sh");
    std::fs::write(
        &script,
        "count=0\nwhile IFS= read -r file; do\n  printf '<%s>\\n' \"$file\"\n  count=$((count + 1))\ndone < list.txt\nprintf 'count=%s\\n' \"$count\"\n",
    )
    .unwrap();

    let mut child = Command::new(winuxsh_binary())
        .arg(&script)
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn winuxsh script read redirect: {err}"));
    let _open_parent_stdin = child.stdin.take().unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "script exited with {status}");
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("script read from parent stdin instead of redirected list.txt");
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    drop(_open_parent_stdin);
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(
        normalize_text(stdout.as_bytes()),
        "<alpha>\n<beta>\ncount=2"
    );
    assert_eq!(normalize_text(stderr.as_bytes()), "");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn slash_drive_script_file_argument_executes_script() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-slash-drive-script");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();
    let script = start.join("script.sh");
    std::fs::write(&script, "echo slash-drive-script-ok\n").unwrap();
    let Some(script_slash_drive) = slash_drive_path(&script) else {
        let _ = std::fs::remove_dir_all(temp);
        return;
    };

    let output = Command::new(winuxsh_binary())
        .arg(script_slash_drive)
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .unwrap_or_else(|err| panic!("spawn winuxsh slash-drive script: {err}"));

    assert_success(&output, "slash-drive script file");
    assert_eq!(normalize_text(&output.stdout), "slash-drive-script-ok");
    assert_eq!(normalize_text(&output.stderr), "");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn command_mode_accepts_base_prefixed_arithmetic_in_function_body() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-arithmetic-base");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh(
        "f() { value=$((16#de)); printf '%s\\n' \"$value\"; }; f",
        &start,
        &home,
        &[],
    );
    assert_success(&output, "base-prefixed arithmetic");
    assert_eq!(normalize_text(&output.stdout), "222");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn command_mode_parameter_pattern_removal_handles_escaped_quotes() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-param-pattern");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh(
        r##"line='<rect x="0" fill="#fe0000"/>'; rest=${line#*fill=\"}; printf '%s\n' "${rest%%\"*}""##,
        &start,
        &home,
        &[],
    );
    assert_success(&output, "parameter pattern removal");
    assert_eq!(normalize_text(&output.stdout), "#fe0000");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn command_mode_set_positional_splits_custom_ifs() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-custom-ifs");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = run_winuxsh(
        r#"line='a"b"c'; old=$IFS; IFS='"'; set -- $line; IFS=$old; printf 'n=%s one=%s two=%s three=%s\n' "$#" "$1" "$2" "$3""#,
        &start,
        &home,
        &[],
    );
    assert_success(&output, "custom IFS set --");
    assert_eq!(normalize_text(&output.stdout), "n=3 one=a two=b three=c");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn closed_stdout_pipe_does_not_print_broken_pipe_error() {
    if !cfg!(windows) {
        return;
    }

    let temp = unique_temp_dir("winuxsh-host-broken-pipe");
    let home = temp.join("home");
    let start = temp.join("start");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let mut child = Command::new(winuxsh_binary())
        .arg("-c")
        .arg("i=0; while true; do echo line-$i; i=$((i+1)); done")
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn winuxsh: {err}"));

    let mut stdout = child.stdout.take().unwrap();
    let mut buffer = [0_u8; 32];
    let _ = stdout.read(&mut buffer).unwrap_or(0);
    drop(stdout);

    let status = wait_for_child_exit(&mut child, Duration::from_secs(3));

    let mut stderr = String::new();
    if let Some(mut child_stderr) = child.stderr.take() {
        child_stderr.read_to_string(&mut stderr).unwrap();
    }

    let _ = std::fs::remove_dir_all(temp);

    assert!(
        status.is_some(),
        "winuxsh did not exit after stdout pipe closed; stderr: {stderr:?}"
    );

    assert!(
        !stderr.contains("Broken pipe")
            && !stderr.contains("os error 232")
            && !stderr.contains("管道正在被关闭"),
        "stderr should not contain scary broken pipe text: {stderr:?}"
    );
}

fn run_winuxsh(script: &str, cwd: &Path, home: &Path, extra_env: &[(&str, String)]) -> Output {
    let mut command = Command::new(winuxsh_binary());
    command
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home);

    for (key, value) in extra_env {
        command.env(key, value);
    }

    command
        .output()
        .unwrap_or_else(|err| panic!("spawn winuxsh: {err}"))
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
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

fn stdout_lines(output: &Output) -> Vec<String> {
    normalize_text(&output.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn normalize_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .trim()
        .to_string()
}

fn assert_no_terminal_controls(bytes: &[u8]) {
    for byte in bytes {
        assert_ne!(*byte, 0x1b, "unexpected ANSI escape in output");
        assert!(
            *byte >= 0x20 || matches!(*byte, b'\t' | b'\n' | b'\r'),
            "unexpected control byte 0x{byte:02X} in output"
        );
    }
}

fn assert_same_path(actual: &str, expected: &str) {
    assert_eq!(
        comparable_path(actual),
        comparable_path(expected),
        "path mismatch: actual={actual:?}, expected={expected:?}"
    );
}

fn comparable_path(value: &str) -> String {
    let mut value = value.trim().replace('\\', "/");
    if cfg!(windows) && value.len() >= 2 && value.as_bytes()[1] == b':' {
        let drive = value[0..1].to_ascii_uppercase();
        value.replace_range(0..1, &drive);
    }
    let normalized = value.trim_end_matches('/').to_string();
    // cmd.exe may report 8.3 short-name paths (e.g. RUNNER~1); expand them
    // through the filesystem so both sides compare as long names.
    if cfg!(windows) {
        if let Ok(expanded) = std::fs::canonicalize(normalized.replace('/', "\\")) {
            let mut expanded = expanded.to_string_lossy().replace('\\', "/");
            if expanded.len() >= 2 && expanded.as_bytes()[1] == b':' {
                let drive = expanded[0..1].to_ascii_uppercase();
                expanded.replace_range(0..1, &drive);
            }
            return expanded.trim_end_matches('/').to_string();
        }
    }
    normalized
}

fn shell_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn slash_drive_path(path: &Path) -> Option<String> {
    let value = shell_path(path);
    let bytes = value.as_bytes();
    if cfg!(windows)
        && bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == b'/'
    {
        Some(format!(
            "/{}{}",
            (bytes[0] as char).to_ascii_lowercase(),
            &value[2..]
        ))
    } else {
        None
    }
}

fn real_winuxcmd_for_test() -> Option<PathBuf> {
    std::env::var_os("WINUXCMD_PATH")
        .and_then(|path| resolve_winuxcmd_test_path(PathBuf::from(path)))
        .or_else(|| {
            let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()?
                .to_path_buf();
            [
                repo_root.join("WinuxCmd/build-vs/winuxcmd.exe"),
                repo_root.join("WinuxCmd/build-vs-release/winuxcmd.exe"),
                repo_root.join("winuxsh/dist/winuxsh-v0.7.1-win-x64/winuxcmd/winuxcmd.exe"),
                repo_root.join("winuxsh/dist/winuxsh-v0.7.0-win-x64/winuxcmd/winuxcmd.exe"),
            ]
            .into_iter()
            .find(|path| winuxcmd_test_path_has_command_links(path))
        })
        .or_else(|| find_winuxcmd_on_path())
}

fn resolve_winuxcmd_test_path(path: PathBuf) -> Option<PathBuf> {
    if path.is_dir() {
        let exe = path.join("winuxcmd.exe");
        return winuxcmd_test_path_has_command_links(&exe).then_some(exe);
    }
    winuxcmd_test_path_has_command_links(&path).then_some(path)
}

fn winuxcmd_test_path_has_command_links(path: &Path) -> bool {
    if !path.is_file()
        || !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("winuxcmd.exe"))
    {
        return false;
    }
    let Some(dir) = path.parent() else {
        return false;
    };
    ["ls.exe", "cat.exe", "grep.exe", "ln.exe"]
        .into_iter()
        .all(|name| dir.join(name).is_file())
}

fn find_winuxcmd_on_path() -> Option<PathBuf> {
    let output = Command::new("where.exe")
        .arg("winuxcmd.exe")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| PathBuf::from(line.trim()))
        .find(|path| winuxcmd_test_path_has_command_links(path))
}

fn native_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
}
