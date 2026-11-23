//! niubash#1084: `niu skill install/status --target dir:<path>` must
//! resolve a POSIX-spelled path (leading `/`) through the shell's path
//! classifier — the same answer `cd` gives — instead of handing the
//! spelling to Win32 verbatim, where a leading `/` means "root of the
//! current drive". Before the fix, `dir:/tmp/<probe>` installed into
//! `<cwd-drive>:\tmp\<probe>` and the reported state depended on the
//! drive the querying process happened to sit on.
//!
//! The cross-drive half of the property (install from F:, status from
//! C:) needs two drives and is verified on the X299 bench; this test
//! pins the machine-local half: with TMPDIR anchored at a known base,
//! the bundle lands under that base — never at the cwd drive's root —
//! and status agrees from a different cwd.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push("niu.exe");
    fallback
}

fn unique_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "niu-issue1084-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_skill(verb: &str, target: &str, cwd: &Path, tmp_base: &Path) -> Output {
    Command::new(niu_binary())
        .args(["skill", verb, "--target", target])
        .current_dir(cwd)
        .env("TMPDIR", tmp_base)
        .env_remove("NIU_ROOT")
        .env_remove("RUBASH_ROOT")
        .env_remove("__RUBASH_SHELL_ROOT")
        .output()
        .expect("spawn niu skill")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn drive_root_of(path: &Path) -> PathBuf {
    path.ancestors()
        .last()
        .map(|root| root.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("C:\\"))
}

#[test]
fn tmp_spelling_installs_under_tmpdir_and_status_agrees_from_any_cwd() {
    let tmp_base = unique_dir("tmpbase");
    let cwd_a = unique_dir("cwd-a");
    let cwd_b = unique_dir("cwd-b");
    let probe = "issue1084-probe";
    let target = format!("dir:/tmp/{probe}");

    let install = run_skill("install", &target, &cwd_a, &tmp_base);
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    // The bundle is under the TMPDIR-anchored /tmp, in the shell's
    // meaning of /tmp — with the niubash/ bundle dir below the target.
    let landed = tmp_base.join(probe).join("niubash").join("SKILL.md");
    assert!(landed.is_file(), "bundle must land at {}", landed.display());
    assert!(
        stdout(&install).contains(&tmp_base.display().to_string().replace('\\', "/")),
        "install must report the resolved native target, got: {}",
        stdout(&install)
    );

    // The pre-fix landing — the cwd drive's root — must not exist.
    let wrong = drive_root_of(&cwd_a).join("tmp").join(probe);
    assert!(
        !wrong.exists(),
        "bundle must not land on the cwd drive root at {}",
        wrong.display()
    );

    // Status from a different cwd reports the same state.
    let status = run_skill("status", &target, &cwd_b, &tmp_base);
    assert!(
        status.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(
        stdout(&status).contains("current"),
        "status from another cwd must agree, got: {}",
        stdout(&status)
    );

    let _ = std::fs::remove_dir_all(&tmp_base);
    let _ = std::fs::remove_dir_all(&cwd_a);
    let _ = std::fs::remove_dir_all(&cwd_b);
    let _ = std::fs::remove_dir_all(&wrong);
}

#[test]
fn native_spelling_is_the_control_and_stays_put() {
    let tmp_base = unique_dir("tmpbase");
    let cwd_a = unique_dir("cwd-a");
    let cwd_b = unique_dir("cwd-b");
    let native = tmp_base.join("issue1084-native");
    let target = format!("dir:{}", native.display().to_string().replace('\\', "/"));

    let install = run_skill("install", &target, &cwd_a, &tmp_base);
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(native.join("niubash").join("SKILL.md").is_file());

    let status = run_skill("status", &target, &cwd_b, &tmp_base);
    assert!(
        stdout(&status).contains("current"),
        "native target status must agree, got: {}",
        stdout(&status)
    );

    let _ = std::fs::remove_dir_all(&tmp_base);
    let _ = std::fs::remove_dir_all(&cwd_a);
    let _ = std::fs::remove_dir_all(&cwd_b);
}
