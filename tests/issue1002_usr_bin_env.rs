//! Regression tests for unixwin/niubash#1002:
//! `env.exe` must be provisioned alongside `bash.exe` in `winuxcmd/usr/bin`
//! (and `winuxcmd/bin` when present) so `#!/usr/bin/env bash` git hooks and
//! `env bash ...` invocations work out of the box.

use std::path::{Path, PathBuf};
use std::process::Command;

fn unique_temp_dir(prefix: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}-{}-{id}", std::process::id()))
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
/// run without staging is a hard failure, so this pin can never again
/// pass without executing.
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

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join(if cfg!(windows) { "niu.exe" } else { "niu" })
}

fn stage_env_and_bash(root: &Path, winuxcmd_src: &Path, niu_src: &Path) {
    let usr_bin = root.join("winuxcmd").join("usr").join("bin");
    let bin = root.join("winuxcmd").join("bin");
    std::fs::create_dir_all(&usr_bin).unwrap();
    std::fs::create_dir_all(&bin).unwrap();

    std::fs::copy(niu_src, root.join("niu.exe")).unwrap();
    std::fs::copy(winuxcmd_src, usr_bin.join("winuxcmd.exe")).unwrap();
    for dir in [&usr_bin, &bin] {
        let bash = dir.join("bash.exe");
        if std::fs::hard_link(root.join("niu.exe"), &bash).is_err() {
            std::fs::copy(root.join("niu.exe"), &bash).unwrap();
        }
    }

    let report = niubash_runtime::winuxcmd::reconcile_env_faces(&root.join("winuxcmd"), true);
    assert!(report.failed.is_empty(), "{report:?}");
    assert!(
        usr_bin.join("env.exe").is_file(),
        "reconcile_env_faces must provision winuxcmd/usr/bin/env.exe"
    );
    assert!(
        bin.join("env.exe").is_file(),
        "reconcile_env_faces must provision winuxcmd/bin/env.exe"
    );
}

#[test]
fn reconcile_env_faces_provisions_usr_bin_and_bin_env_alongside_bash() {
    let temp = unique_temp_dir("niubash-issue1002-reconcile");
    let winuxcmd_root = temp.join("winuxcmd");
    let usr_bin = winuxcmd_root.join("usr").join("bin");
    let bin = winuxcmd_root.join("bin");
    std::fs::create_dir_all(&usr_bin).unwrap();
    std::fs::create_dir_all(&bin).unwrap();

    let engine = usr_bin.join("winuxcmd.exe");
    std::fs::write(&engine, b"synthetic winuxcmd image").unwrap();
    std::fs::write(usr_bin.join("bash.exe"), b"synthetic bash").unwrap();
    std::fs::write(bin.join("bash.exe"), b"synthetic bash").unwrap();

    let report = niubash_runtime::winuxcmd::reconcile_env_faces(&winuxcmd_root, true);
    assert_eq!(report.created.len(), 2, "{report:?}");
    assert!(report.failed.is_empty(), "{report:?}");

    for target in [usr_bin.join("env.exe"), bin.join("env.exe")] {
        assert!(
            target.is_file(),
            "{} must exist after reconcile_env_faces",
            target.display()
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"synthetic winuxcmd image",
            "{} must carry the winuxcmd.exe bytes",
            target.display()
        );
    }

    // Idempotent second run reports already_current.
    let again = niubash_runtime::winuxcmd::reconcile_env_faces(&winuxcmd_root, false);
    assert!(again.created.is_empty(), "{again:?}");
    assert_eq!(again.already_current.len(), 2, "{again:?}");

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn env_exe_executes_bash_c_and_shebang_script_with_args() {
    if !cfg!(windows) {
        return;
    }
    // niubash#1104: missing staging used to skip this pin with only an
    // eprintln; it now goes through the same hard floor as issue1067.
    let Some(winuxcmd_src) = require_real_winuxcmd() else {
        return;
    };

    let temp = unique_temp_dir("niubash-issue1002-exec");
    stage_env_and_bash(&temp, &winuxcmd_src, &niu_binary());

    let usr_bin = temp.join("winuxcmd").join("usr").join("bin");
    let bin = temp.join("winuxcmd").join("bin");
    let hook = temp.join("pre-commit");
    std::fs::write(
        &hook,
        "#!/usr/bin/env bash\nprintf 'hook:%s:%s:%s\\n' \"$0\" \"$1\" \"$2\"\n",
    )
    .unwrap();

    for dir in [&usr_bin, &bin] {
        let env_exe = dir.join("env.exe");
        let isolated_path = format!("{};C:\\Windows\\System32", dir.display());

        let inline = Command::new(&env_exe)
            .env("PATH", &isolated_path)
            .args([
                "bash",
                "-c",
                "printf 'inline:%s:%s\\n' \"$0\" \"$1\"",
                "hook-arg0",
                "arg1",
            ])
            .output()
            .expect("spawn env.exe bash -c");
        assert!(
            inline.status.success(),
            "env.exe bash -c failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&inline.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&inline.stdout).replace("\r\n", "\n"),
            "inline:hook-arg0:arg1\n"
        );

        let script = Command::new(&env_exe)
            .env("PATH", &isolated_path)
            .arg("bash")
            .arg(&hook)
            .args(["first", "second"])
            .output()
            .expect("spawn env.exe bash <hook> <args>");
        assert!(
            script.status.success(),
            "env.exe bash <script> failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&script.stderr)
        );
        let stdout = String::from_utf8_lossy(&script.stdout).replace("\r\n", "\n");
        assert!(
            stdout.contains("pre-commit:first:second\n"),
            "unexpected hook stdout from {}: {stdout:?}",
            dir.display()
        );
    }

    let _ = std::fs::remove_dir_all(&temp);
}
