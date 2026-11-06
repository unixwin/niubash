//! unixwin/niubash#950: the stdin feeder's tracked (history/alias)
//! arm did not stop the stream on a parse error.
//!
//! The arm's stop fold in `execute_script_inner` recorded only the
//! classification's unwind verdict. A syntax error produces no
//! unwind, and `Session::run` has already drained the executor
//! pendings by the time the fold reads the outcome, so the feeder
//! saw no stop record and read on past the bad batch, exiting 0.
//! GNU's reader_loop unwinds on the syntax error: the stream stops,
//! the exit status is 2, and nothing after the bad batch runs. The
//! fold now also takes `classification.parse_error` — the same
//! parse-error term the unclosed/stray arms fold (found by the
//! rubash#2860 lane's end-to-end stdin differential, 2026-11-06;
//! pre-existing on master, not a migration regression).
//!
//! Oracle: GNU bash 5.3.20, piped stdin, verified against the
//! streams below — batch 2 admitted by the tracked arm because
//! batch 1 lit the live alias flag.

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
        "niu-issue950-{}-{:?}",
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
fn issue950_tracked_arm_parse_error_stops_the_stream() {
    // The filed stream: batch 2 pairs a live-alias call with a stray
    // close. The batch fails to parse as a whole (nothing in it
    // runs, not even the alias call), and the feeder must stop:
    // no `after`, exit status 2.
    let (out, err, rc) =
        stdout_text(b"shopt -s expand_aliases; alias x='echo hi'\nx; echo )bad\necho after\n");
    assert_eq!(rc, 2, "stdout: {out:?} stderr: {err:?}");
    assert!(
        err.contains("syntax error"),
        "the parse error must be reported; stderr: {err:?}"
    );
    assert!(
        !out.contains("after"),
        "input after the bad batch must not run; stdout: {out:?}"
    );
    assert!(
        !out.contains("hi"),
        "the bad batch parses as a whole or not at all; stdout: {out:?}"
    );
}

#[test]
fn issue950_tracked_arm_parse_error_stops_after_a_clean_tracked_batch() {
    // A clean tracked batch first (the alias fires, `hi` prints),
    // then the same bad batch: the stream still stops at it.
    let (out, err, rc) =
        stdout_text(b"shopt -s expand_aliases; alias x='echo hi'\nx\nx; echo )bad\necho after\n");
    assert_eq!(rc, 2, "stdout: {out:?} stderr: {err:?}");
    assert_eq!(
        out.matches("hi").count(),
        1,
        "the clean batch runs exactly once; stdout: {out:?}"
    );
    assert!(
        !out.contains("after"),
        "input after the bad batch must not run; stdout: {out:?}"
    );
}
