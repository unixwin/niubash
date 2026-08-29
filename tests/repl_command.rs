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

#[test]
fn command_mode_compound_commands_keep_home_paths_native() {
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
    assert!(stdout.contains("catrc:ok"), "stdout was {stdout:?}");
    assert!(
        !stdout.contains("/c/"),
        "command mode leaked slash-drive paths: {stdout:?}"
    );
    assert!(stdout.contains("PWD="), "stdout was {stdout:?}");
    let _ = std::fs::remove_dir_all(temp);
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

#[test]
fn gitstatus_daemon_returns_repo_snapshot_over_persistent_stdio() {
    let temp = unique_temp_dir("niubash-gitstatus-daemon");
    let repo = temp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init"][..],
        &["config", "user.email", "test@niubash"],
        &["config", "user.name", "Niubash Test"],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    std::fs::write(repo.join("new.txt"), "daemon\n").unwrap();

    let mut child = Command::new(niu_binary())
        .arg("--gitstatus-daemon")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{{\"id\":1,\"cwd\":{}}}",
            serde_json::to_string(&repo.to_string_lossy()).unwrap()
        )
        .unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert_success(&output, "gitstatus daemon");
    let stdout = stdout_text(&output);
    assert!(stdout.contains(r#""id":1"#), "{stdout}");
    assert!(stdout.contains(r#""untracked":1"#), "{stdout}");
    assert!(stdout.contains(r#""dirty":true"#), "{stdout}");
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
