//! unixwin/niubash#863 (GAP-1): the bare-stdin batch feeder
//! (`run_stdin_script` in src/main.rs) stops the stream on the
//! engine's reported unwinds only — it no longer re-derives the
//! errexit stop host-side from status + SHELLOPTS +
//! last_command_inverted.
//!
//! Before GAP-1 (rubash#2795) the Session-contract arm of
//! `execute_script_inner` reported an errexit break as
//! `Flow::Completed` (the fold), so the feeder needed its own
//! errexit clause to stop the stream for batches routed through
//! that arm. The clause over-stopped: it also fired on a nonzero
//! batch whose failing command sat in an errexit-exempt position
//! (a non-final `&&`/`||` member), where GNU's reader_loop reads
//! on. With the classification contract landed, every route
//! reports its own unwind — the Session arm records the
//! classification's adjudicated unwind in `script_exit_jump`, the
//! flattened fast path records the ExitCode unwind, and the driver
//! routes arm the engine's exit-jump pending — so the clause is
//! deleted and these pins lock both halves of the result: a real
//! errexit break still stops the stream on every route, and an
//! exempt failure no longer does.
//!
//! Oracle: GNU bash 5.3.20 (stdin streams), verified 2026-11-04 —
//! `set -e` + `false && echo hi` reads on (rc 0), while the
//! alias-routed `set -e` break stops the stream at rc 1. The
//! plain-route stop and the ordinary-nonzero continuation are
//! pinned by tests/issue576_stdin_nonzero_batch_continues.rs.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niu" });
    fallback
}

fn stdout_text(input: &[u8]) -> (String, String, i32) {
    let dir = std::env::temp_dir().join(format!(
        "niu-issue863-gap1-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let mut child = Command::new(niu_binary())
        .current_dir(&dir)
        .env("HOME", &dir)
        .env("USERPROFILE", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input)
        .expect("feed stdin script");
    let output = child.wait_with_output().expect("wait niu");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

#[test]
fn issue863_gap1_feeder_errexit_break_on_tracked_route_stops_stream() {
    // The `f` batch is admitted to the Session-contract (tracked)
    // arm of execute_script_inner because it uses a live alias with
    // expand_aliases on. Its errexit break reaches the feeder only
    // through the classification record (script_exit_jump, set from
    // `classification.unwind`): under the pre-GAP-1 Flow fold this
    // batch reported Completed and the feeder's own (now deleted)
    // errexit clause was the only thing that stopped the stream.
    let (out, err, rc) =
        stdout_text(b"shopt -s expand_aliases\nalias f='false'\nset -e\nf\necho NO\n");
    assert_eq!(rc, 1, "stderr: {err:?}");
    assert!(
        !out.contains("NO"),
        "input after the errexit break must not run; stdout: {out:?}"
    );
}

#[test]
fn issue863_gap1_feeder_errexit_exempt_failure_does_not_stop_stream() {
    // `false` fails as a non-final `&&` member — an errexit-exempt
    // position (POSIX errexit / execute_cmd.c: the failing command
    // is part of a && list, not the command following the final &&).
    // No unwind occurs, so the stream reads on and the last batch's
    // status decides: GNU prints ALIVE and exits 0. The feeder's
    // deleted host-side clause stopped the stream here (rc 1, no
    // ALIVE) because it re-derived the stop from the bare status.
    let (out, err, rc) = stdout_text(b"set -e\nfalse && echo hi\necho ALIVE\n");
    assert_eq!(rc, 0, "last command decides the status; stderr: {err:?}");
    assert!(out.contains("ALIVE"), "stdout: {out:?}");
    assert!(
        !out.contains("hi"),
        "the exempt list still short-circuits; stdout: {out:?}"
    );
}
