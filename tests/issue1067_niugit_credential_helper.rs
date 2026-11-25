//! Regression tests for unixwin/niubash#1067:
//! Bundled `niu-git` must inherit host Git Credential Manager (or fall back to
//! bundled `wincred`) when the user has not explicitly configured
//! `credential.helper`, without overriding user-configured helpers or breaking
//! `GIT_TERMINAL_PROMPT=0` non-interactive fast-fail.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn unique_temp_dir(prefix: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}-{}-{id}", std::process::id()))
}

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("niu.exe")
}

fn find_real_winuxcmd() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("WINUXCMD_PATH") {
        let p = PathBuf::from(raw);
        if p.is_file() {
            return Some(p);
        }
    }
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
        .find(|path| path.is_file())
}

/// Environment variable that explicitly opts out of the winuxcmd staging
/// requirement for a local run (niubash#1104). CI never honors it: a CI
/// run without staging is a hard failure, so these pins can never again
/// pass without executing — the failure mode that let #1100's host-only
/// red escape CI.
const SKIP_STAGING_ENV: &str = "NIU_TEST_SKIP_WINUXCMD_STAGING";

fn ci_env_is_truthy() -> bool {
    std::env::var("CI").is_ok_and(|v| !v.is_empty() && v != "false")
}

/// `find_real_winuxcmd` with the niubash#1104 floor applied: missing
/// staging is a hard failure in CI and by default locally; the only skip
/// is the explicit, visible opt-out in `SKIP_STAGING_ENV`. On success the
/// staged path is printed so CI logs can prove the pin executed instead
/// of early-returning (an early return also reports `ok`).
fn require_real_winuxcmd() -> Option<PathBuf> {
    if let Some(path) = find_real_winuxcmd() {
        eprintln!("winuxcmd staging: {}", path.display());
        return Some(path);
    }
    if ci_env_is_truthy() {
        panic!(
            "winuxcmd staging is required in CI but no winuxcmd.exe was found \
             (WINUXCMD_PATH unset and `where.exe winuxcmd.exe` found nothing); \
             refusing to let this pin pass without executing (niubash#1104)"
        );
    }
    if std::env::var_os(SKIP_STAGING_ENV).is_some() {
        eprintln!(
            "SKIP: {SKIP_STAGING_ENV} is set and no winuxcmd.exe staging was found; \
             this pin did not execute (niubash#1104)"
        );
        return None;
    }
    panic!(
        "no winuxcmd.exe staging found (set WINUXCMD_PATH or put winuxcmd.exe on PATH); \
         to skip this pin explicitly on a local run, set {SKIP_STAGING_ENV}=1 (niubash#1104)"
    );
}

struct StagedShell {
    root: PathBuf,
    niu_exe: PathBuf,
    winuxcmd_usr_bin: PathBuf,
    program_files: PathBuf,
    empty_dir: PathBuf,
    gitconfig: PathBuf,
}

impl Drop for StagedShell {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn stage_niu_with_niugit(tag: &str, with_wincred: bool) -> Option<StagedShell> {
    let winuxcmd_src = require_real_winuxcmd()?;
    let root = unique_temp_dir(tag);
    let winuxcmd_root = root.join("winuxcmd");
    let usr_bin = winuxcmd_root.join("usr").join("bin");
    let bin = winuxcmd_root.join("bin");
    std::fs::create_dir_all(&usr_bin).unwrap();
    std::fs::create_dir_all(&bin).unwrap();

    let niu_exe = root.join("niu.exe");
    std::fs::copy(niu_binary(), &niu_exe).unwrap();
    let winuxcmd_exe = usr_bin.join("winuxcmd.exe");
    std::fs::copy(&winuxcmd_src, &winuxcmd_exe).unwrap();
    std::fs::write(usr_bin.join("git.exe"), b"stub-niugit").unwrap();

    if with_wincred {
        let git_core = winuxcmd_root
            .join("opt")
            .join("niugit")
            .join("mingw64")
            .join("libexec")
            .join("git-core");
        std::fs::create_dir_all(&git_core).unwrap();
        std::fs::write(git_core.join("git-credential-wincred.exe"), b"stub-wincred").unwrap();
    }

    let program_files = root.join("Program Files");
    let empty_dir = root.join("empty");
    std::fs::create_dir_all(&program_files).unwrap();
    std::fs::create_dir_all(&empty_dir).unwrap();
    let gitconfig = root.join("user.gitconfig");
    std::fs::write(&gitconfig, "").unwrap();

    Some(StagedShell {
        root,
        niu_exe,
        winuxcmd_usr_bin: usr_bin,
        program_files,
        empty_dir,
        gitconfig,
    })
}

fn base_niu_command(stage: &StagedShell, script: &str) -> Command {
    let mut cmd = Command::new(&stage.niu_exe);
    cmd.arg("-c").arg(script);
    cmd.env("NIU_SKIP_WINUXCMD_ACTIVATION", "1");
    cmd.env("PATH", &stage.empty_dir);
    cmd.env("ProgramFiles", &stage.program_files);
    cmd.env("ProgramFiles(x86)", &stage.empty_dir);
    cmd.env("LOCALAPPDATA", &stage.empty_dir);
    cmd.env("USERPROFILE", &stage.empty_dir);
    cmd.env("HOME", &stage.empty_dir);
    cmd.env("GIT_CONFIG_GLOBAL", &stage.gitconfig);
    cmd.env_remove("WINUXCMD_PATH");
    cmd.env_remove("GIT_CONFIG_COUNT");
    cmd.env_remove("GIT_CONFIG_KEY_0");
    cmd.env_remove("GIT_CONFIG_VALUE_0");
    cmd.env_remove("GIT_CONFIG_PARAMETERS");
    cmd.env_remove("GIT_ASKPASS");
    cmd.env_remove("GIT_CONFIG_NOSYSTEM");
    cmd
}

fn run_stdout(cmd: &mut Command) -> String {
    let out = cmd.output().expect("spawn niu");
    assert!(
        out.status.success(),
        "niu failed: status={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn seeds_host_gcm_when_present_and_unconfigured() {
    let Some(stage) = stage_niu_with_niugit("niubash-1067-host-gcm", true) else {
        return;
    };
    let local_app_data = stage.root.join("LocalAppData");
    let gcm = local_app_data
        .join("Programs")
        .join("Git")
        .join("mingw64")
        .join("bin")
        .join("git-credential-manager.exe");
    std::fs::create_dir_all(gcm.parent().unwrap()).unwrap();
    std::fs::write(&gcm, b"stub-gcm").unwrap();

    let mut cmd = base_niu_command(
        &stage,
        r#"printf '%s|%s|%s\n' "${GIT_CONFIG_COUNT:-}" "${GIT_CONFIG_KEY_0:-}" "${GIT_CONFIG_VALUE_0:-}""#,
    );
    cmd.env("LOCALAPPDATA", &local_app_data);
    let out = run_stdout(&mut cmd);
    let expected_val = format!("!'{}'", gcm.to_string_lossy().replace('\\', "/"));
    assert_eq!(out, format!("1|credential.helper|{expected_val}"));
}

#[test]
fn falls_back_to_bundled_wincred_when_host_gcm_absent() {
    let Some(stage) = stage_niu_with_niugit("niubash-1067-wincred", true) else {
        return;
    };
    let mut cmd = base_niu_command(
        &stage,
        r#"printf '%s|%s|%s\n' "${GIT_CONFIG_COUNT:-}" "${GIT_CONFIG_KEY_0:-}" "${GIT_CONFIG_VALUE_0:-}""#,
    );
    let out = run_stdout(&mut cmd);
    assert_eq!(out, "1|credential.helper|wincred");
}

#[test]
fn does_not_override_user_unscoped_credential_helper() {
    let Some(stage) = stage_niu_with_niugit("niubash-1067-user-helper", true) else {
        return;
    };
    std::fs::write(&stage.gitconfig, "[credential]\n\thelper = store\n").unwrap();

    let mut cmd = base_niu_command(&stage, r#"printf '%s\n' "${GIT_CONFIG_COUNT:-UNSET}""#);
    let out = run_stdout(&mut cmd);
    assert_eq!(out, "UNSET");
}

#[test]
fn scoped_host_helper_does_not_suppress_fallback_helper() {
    let Some(stage) = stage_niu_with_niugit("niubash-1067-scoped-helper", true) else {
        return;
    };
    std::fs::write(
        &stage.gitconfig,
        "[credential \"https://github.com\"]\n\thelper = !gh auth git-credential\n",
    )
    .unwrap();

    let mut cmd = base_niu_command(
        &stage,
        r#"printf '%s|%s|%s\n' "${GIT_CONFIG_COUNT:-}" "${GIT_CONFIG_KEY_0:-}" "${GIT_CONFIG_VALUE_0:-}""#,
    );
    let out = run_stdout(&mut cmd);
    assert_eq!(out, "1|credential.helper|wincred");
}

#[test]
fn does_not_seed_when_user_git_is_first_on_path() {
    let Some(stage) = stage_niu_with_niugit("niubash-1067-user-git", true) else {
        return;
    };
    let user_git_dir = stage.root.join("user-git");
    std::fs::create_dir_all(&user_git_dir).unwrap();
    std::fs::write(user_git_dir.join("git.exe"), b"user-git").unwrap();

    let mut cmd = base_niu_command(&stage, r#"printf '%s\n' "${GIT_CONFIG_COUNT:-UNSET}""#);
    cmd.env("PATH", &user_git_dir);
    let out = run_stdout(&mut cmd);
    assert_eq!(out, "UNSET");
}

#[test]
fn appends_to_existing_git_config_count_and_respects_git_askpass() {
    let Some(stage) = stage_niu_with_niugit("niubash-1067-append-askpass", true) else {
        return;
    };

    // Append to existing GIT_CONFIG_COUNT=1.
    let mut append_cmd = base_niu_command(
        &stage,
        r#"printf '%s|%s|%s|%s|%s\n' "${GIT_CONFIG_COUNT:-}" "${GIT_CONFIG_KEY_0:-}" "${GIT_CONFIG_VALUE_0:-}" "${GIT_CONFIG_KEY_1:-}" "${GIT_CONFIG_VALUE_1:-}""#,
    );
    append_cmd.env("GIT_CONFIG_COUNT", "1");
    append_cmd.env("GIT_CONFIG_KEY_0", "user.name");
    append_cmd.env("GIT_CONFIG_VALUE_0", "Alice");
    let out = run_stdout(&mut append_cmd);
    assert_eq!(out, "2|user.name|Alice|credential.helper|wincred");

    // Respect GIT_ASKPASS when set.
    let mut askpass_cmd = base_niu_command(&stage, r#"printf '%s\n' "${GIT_CONFIG_COUNT:-UNSET}""#);
    askpass_cmd.env("GIT_ASKPASS", "custom-askpass.exe");
    let out_askpass = run_stdout(&mut askpass_cmd);
    assert_eq!(out_askpass, "UNSET");

    // Ensure winuxcmd_usr_bin is not unused warning.
    let _ = Path::new(&stage.winuxcmd_usr_bin);
}
