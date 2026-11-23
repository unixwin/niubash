//! `niu skill install` / `niu skill status` end-to-end (niubash#188).
//!
//! Runs the built launcher against a throwaway skills root so the real
//! home (and any real agent skill dir) is never touched. The doctor row is
//! intentionally not exercised here: it resolves the real home directory.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn niu_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_niu"))
}

fn run_skill(args: &[&str]) -> (String, String, Option<i32>) {
    let output = Command::new(niu_binary())
        .args(args)
        .output()
        .expect("spawn niu");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code(),
    )
}

/// A unique per-test skills root under the system temp dir.
fn temp_skills_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "niu-skill-bundle-test-{}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn install_writes_bundle_and_status_reports_current() {
    let root = temp_skills_root("install");
    let target = root.to_string_lossy().replace('\\', "/");
    let (stdout, stderr, code) = run_skill(&["skill", "install", "--target", &target]);
    assert_eq!(code, Some(0), "install failed: {stderr}");
    assert!(stdout.contains("installed 6 file(s)"), "stdout: {stdout}");

    let bundle = root.join("niubash");
    for rel in [
        "SKILL.md",
        "agents/openai.yaml",
        "references/paths.md",
        "references/prompt-plugins.md",
        "references/quickref.md",
        "references/wpm.md",
    ] {
        assert!(bundle.join(rel).is_file(), "missing installed file {rel}");
    }
    // The installed SKILL.md carries the generated data regions.
    let skill = fs::read_to_string(bundle.join("SKILL.md")).unwrap();
    assert!(skill.contains("BEGIN GENERATED:capability-snapshot"));
    assert!(skill.contains("BEGIN GENERATED:builtin-names"));

    let (stdout, stderr, code) = run_skill(&["skill", "status", "--target", &target]);
    assert_eq!(code, Some(0), "status failed: {stderr}");
    assert!(stdout.contains("current"), "stdout: {stdout}");
    assert!(stdout.contains("sha256 "), "digest line missing: {stdout}");
    // Status names the bundle's own version beside the digest (#1083):
    // the stamp the generator wrote into SKILL.md from the package version.
    let version_line = format!(
        "bundle: niubash skill {}, sha256 ",
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        stdout.contains(&version_line),
        "bundle version missing from status: {stdout}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn skill_md_prose_carries_no_stale_self_descriptions() {
    // #1083: the hand-written debugging section called 1.4.4 "this
    // bundle's version" long after the bundle moved on — the false
    // self-description must not come back.
    // #996: the identity check taught `uname -s`, which under real niu
    // always answers with the MSYS persona and so always cried "fell
    // back to Git Bash"; the probe is $NIU_SHELL now.
    let root = temp_skills_root("prose");
    let target = root.to_string_lossy().replace('\\', "/");
    let (stdout, stderr, code) = run_skill(&["skill", "install", "--target", &target]);
    assert_eq!(code, Some(0), "install failed: {stderr}");
    assert!(stdout.contains("installed"), "stdout: {stdout}");
    let skill = fs::read_to_string(root.join("niubash").join("SKILL.md")).unwrap();
    assert!(
        !skill.contains("(this bundle's version)"),
        "stale 1.4.4 self-description is back in SKILL.md"
    );
    assert!(
        !skill.contains("check it once per session with `uname -s`"),
        "the uname -s fallback probe is back in SKILL.md"
    );
    assert!(
        skill.contains("check it once per session with `$NIU_SHELL`"),
        "the $NIU_SHELL identity probe is missing from SKILL.md"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn status_reports_outdated_and_missing() {
    let root = temp_skills_root("drift");
    let target = root.to_string_lossy().replace('\\', "/");

    // Not installed yet.
    let (stdout, _, code) = run_skill(&["skill", "status", "--target", &target]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("not installed"), "stdout: {stdout}");

    // Install, then hand-edit one file: installed but outdated.
    let (stdout, _, _) = run_skill(&["skill", "install", "--target", &target]);
    assert!(stdout.contains("installed"));
    fs::write(root.join("niubash").join("SKILL.md"), "# hand-edited\n").unwrap();
    let (stdout, _, code) = run_skill(&["skill", "status", "--target", &target]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("outdated") && stdout.contains("SKILL.md"),
        "stdout: {stdout}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unknown_verb_is_an_error_with_usage() {
    let (_, stderr, code) = run_skill(&["skill", "frobnicate"]);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("unknown skill verb 'frobnicate'"),
        "{stderr}"
    );
}

#[test]
fn missing_target_value_is_a_usage_error() {
    let (_, stderr, code) = run_skill(&["skill", "install", "--target"]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("--target requires a value"), "{stderr}");
}
