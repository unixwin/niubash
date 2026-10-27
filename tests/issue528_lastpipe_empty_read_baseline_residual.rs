//! niubash#528 (W5 residual / characterization pin): under
//! `shopt -s lastpipe`, an EOF `read` as the last pipeline stage.
//!
//! Filed claim: GNU bash dies by SIGHUP semantics on
//! `printf '' | read x` — rc=129 plus a `<pid> Hangup | read x`
//! notice on stderr — while niu returned rc=1 silently.
//!
//! Adjudication (2026-10-26, bf-w5 ISSUE528-VERDICT): the filed
//! 129 form is NOT the glibc GNU baseline; it was an artifact of
//! the reporter's device session (a real SIGHUP delivered to that
//! session's process group). On the baseline — GNU bash 5.3.20
//! oracle, 52 runs across direct / `timeout` / `-c` / `set -m` /
//! PTY / PTY+`-i` forms — the EOF read simply returns 1 in the
//! current shell with no notice, and a fed read returns 0 with
//! the value visible afterwards; niu agrees item for item
//! (42 runs across two trees, stderr empty throughout). The
//! engine side ruled the same a day earlier: rubash commit
//! ded82c0a (`tests/issue1217_lastpipe_eof_regressions.rs`) pins
//! rc=1 with no job-control notification for rubash#1217, of
//! which this niubash filing was a duplicate (closed not_planned
//! in favor of the engine tracker). These pins hold that ruled
//! baseline through the niu binary in the issue's script-file
//! repro form.
//!
//! Citation correction: a previous version of this header cited
//! engine commit 78f8ac45 as a "#1217 SIGHUP fix" delivering
//! rc=129. That commit is in fact a `read -t 0` input_avail
//! polling fix, unrelated to SIGHUP/lastpipe death semantics;
//! the engine has never implemented a SIGHUP death for a
//! lastpipe EOF read. The citation is removed.
//!
//! The earlier "adjacent divergence" note (GNU prefixes the
//! notice with the script name, the engine with `bash:`) is
//! void: under baseline conditions neither shell prints a
//! notice, so there is no prefix to diverge.

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

fn run_script(tag: &str, body: &str) -> (Output, PathBuf) {
    let dir = std::env::temp_dir().join(format!("niu-528-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let script = dir.join("probe.sh");
    std::fs::write(&script, body).expect("write script");
    let out = Command::new(niu_binary())
        .arg(&script)
        .current_dir(&dir)
        .env("HOME", &dir)
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");
    (out, dir)
}

#[test]
fn issue528_lastpipe_empty_pipe_read_reports_rc1_no_hangup_notice() {
    // Baseline form aligned with the rubash#1217 pin (ded82c0a):
    // with lastpipe on and job control off, an EOF read as the
    // last pipeline stage returns 1 in the current shell and no
    // job-control notification appears. (The rc=129 + Hangup form
    // in the original filing was a device-session artifact; see
    // the header adjudication.)
    let (out, dir) = run_script(
        "empty",
        "shopt -s lastpipe\nprintf '' | read x; echo \"rc=$? x=[$x]\"\n",
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "rc=1 x=[]\n");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("Hangup"),
        "baseline form must not produce a Hangup notice, got: {stderr:?}"
    );
}

#[test]
fn issue528_lastpipe_pipefail_true_pipe_read_reports_rc1() {
    let (out, dir) = run_script(
        "pipefail",
        "shopt -s lastpipe\nset -o pipefail; true | read x; echo \"rc=$? x=[$x]\"\n",
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "rc=1 x=[]\n");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("Hangup"),
        "baseline form must not produce a Hangup notice, got: {stderr:?}"
    );
}

#[test]
fn issue528_lastpipe_fed_pipe_read_reports_rc0_value_visible() {
    // Positive control (mirrors the fed half of the rubash#1217
    // pin): with lastpipe on, a fed read runs in the current
    // shell, so rc is 0 and the value is visible afterwards.
    // This proves lastpipe actually engaged — the empty-read
    // rc=1 pins above alone cannot distinguish lastpipe on
    // from off.
    let (out, dir) = run_script(
        "fed",
        "shopt -s lastpipe\necho hi | read y; echo \"rc=$? y=[$y]\"\n",
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "rc=0 y=[hi]\n");
}

#[test]
fn issue528_procsub_empty_read_stays_rc1_no_hangup() {
    // Scope pin from the issue: process substitution is NOT the
    // lastpipe pipeline path — rc=1 in both shells, no Hangup notice.
    let (out, dir) = run_script("procsub", "read x < <(printf ''); echo \"rc=$? x=[$x]\"\n");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "rc=1 x=[]\n");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("Hangup"),
        "procsub shape must not produce a Hangup notice, got: {stderr:?}"
    );
}
