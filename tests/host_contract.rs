//! Windows-native host contract tests for the `winuxsh -c` surface.
//!
//! These tests intentionally exercise the built binary instead of internal
//! helpers: this is the contract humans and agents rely on.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

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
        .env("ZDOTDIR", &home)
        .output()
        .unwrap_or_else(|err| panic!("spawn winuxsh script file: {err}"));
    assert_success(&output, "script-file .winshrc isolation");
    assert_eq!(normalize_text(&output.stdout), "script-ok");

    let mut child = Command::new(winuxsh_binary())
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("ZDOTDIR", &home)
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
fn native_backslash_drive_paths_work_for_winuxcmd_and_cd() {
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
        .env("ZDOTDIR", &home)
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
        .env("ZDOTDIR", &home)
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
        .env("ZDOTDIR", &home)
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
        .env("ZDOTDIR", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn winuxsh: {err}"));

    let mut stdout = child.stdout.take().unwrap();
    let mut buffer = [0_u8; 32];
    let _ = stdout.read(&mut buffer).unwrap_or(0);
    drop(stdout);

    let mut stderr = String::new();
    if let Some(mut child_stderr) = child.stderr.take() {
        child_stderr.read_to_string(&mut stderr).unwrap();
    }
    let _ = child.wait().unwrap();

    assert!(
        !stderr.contains("Broken pipe")
            && !stderr.contains("os error 232")
            && !stderr.contains("管道正在被关闭"),
        "stderr should not contain scary broken pipe text: {stderr:?}"
    );

    let _ = std::fs::remove_dir_all(temp);
}

fn run_winuxsh(script: &str, cwd: &Path, home: &Path, extra_env: &[(&str, String)]) -> Output {
    let mut command = Command::new(winuxsh_binary());
    command
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ZDOTDIR", home);

    for (key, value) in extra_env {
        command.env(key, value);
    }

    command
        .output()
        .unwrap_or_else(|err| panic!("spawn winuxsh: {err}"))
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
    value.trim_end_matches('/').to_string()
}

fn shell_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
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
