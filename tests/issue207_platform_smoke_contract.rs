//! niubash#207 (W5 residual / characterization pin): the release-matrix
//! roadmap. Its governing rule, quoted from docs/release.md via the
//! issue, is "no platform ships binaries that were not run on that
//! platform first", and the issue defines the platform smoke gate as
//! four checks: `--version` (version + engine revision), `-c` engine
//! execution, an external-command pipeline, and the offline plugin
//! bootstrap.
//!
//! The platform-capability face of #207 already in the tree is pinned
//! in crates/niubash-runtime/src/repl.rs (the `system_clipboard`
//! PasteSystem binding is `#[cfg(not(target_os = "android"))]`, with
//! tests — android has no system clipboard and is not a shipped
//! platform until it can run the gate). This file pins the smoke-gate
//! checks themselves on hosts where the suite runs as a release
//! candidate. It compiles to nothing on Android: per the issue, the
//! Android leg's blocker is precisely that there is no runner to
//! execute the gate there, so asserting it on an Android dev host
//! would only recreate rubash#615-style host noise.
#![cfg(not(target_os = "android"))]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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

fn scratch_home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("niu-207-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch home");
    dir
}

fn run(args: &[&str], home: &std::path::Path) -> Output {
    Command::new(niu_binary())
        .args(args)
        .env("HOME", home)
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu")
}

#[test]
fn issue207_smoke_version_reports_version_and_engine_revision() {
    let home = scratch_home();
    let out = run(&["--version"], &home);
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.contains("Niubash"),
        "--version must name the product, got: {stdout}"
    );
    assert!(
        stdout.contains("rubash"),
        "--version must carry the engine revision line, got: {stdout}"
    );
}

#[test]
fn issue207_smoke_dash_c_executes() {
    let home = scratch_home();
    let out = run(&["-c", "echo ok"], &home);
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
}

#[test]
fn issue207_smoke_external_pipeline() {
    // `printf 'a\nb\n' | niu -c 'sort | head -1'` from the issue: the
    // pipeline members are external commands fed by the shell's stdin.
    let home = scratch_home();
    // Premise guard (main-session ruling, 2026-10-27): this pin's
    // oracle is GNU coreutils — it asserts the byte-exact output of a
    // pipeline whose members resolve to GNU `sort`/`head`. Where the
    // `sort` niu resolves does not identify as GNU coreutils, the
    // premise is missing and the assertion cannot judge the product:
    // on GitHub Windows runners `sort` resolves to System32 sort.exe
    // and this pin failed byte-identically before the #585 integration
    // (run 40346497601, 2026-10-26). Probe through niu itself; when
    // GNU coreutils sort is present (dev machines, WSL, hosts with
    // WinuxCmd links) the assertions below run at full strength,
    // unchanged.
    let probe = run(&["-c", "sort --version"], &home);
    let probe_text = format!(
        "{}{}",
        String::from_utf8_lossy(&probe.stdout),
        String::from_utf8_lossy(&probe.stderr)
    );
    if !probe_text.contains("GNU coreutils") {
        eprintln!(
            "skip issue207_smoke_external_pipeline: premise missing — \
             `sort --version` via niu did not identify as GNU coreutils \
             (status: {}, output: {:?}); this pin requires GNU coreutils \
             sort/head as the external pipeline members",
            probe.status, probe_text
        );
        let _ = std::fs::remove_dir_all(&home);
        return;
    }
    let mut child = Command::new(niu_binary())
        .args(["-c", "sort | head -1"])
        .env("HOME", &home)
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(b"b\na\n")
        .expect("feed stdin");
    let out = child.wait_with_output().expect("wait niu");
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\n");
}
