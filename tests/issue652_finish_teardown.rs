//! niubash#652: niu's exit paths never ran the engine's finish steps.
//! The host funnel ran only the EXIT trap, so the end-of-stream
//! job-notification flush (rubash#1853) and the persistent output
//! process-substitution close (rubash#1336) had no host counterpart:
//! `exec > >(cat > LOG); echo ...` lost the whole log (the file was
//! never even created), and a signal death reaped during a stream's
//! last command stayed silent.
//!
//! Fix: the one-shot / host-stream route tails (script file, -c, the
//! host stdin driver) now finish through the engine's shared
//! `script_driver::finish_shell` (`Shell::finish_with_engine_finish`),
//! and the shared trap funnel (`finish_with_exit_trap`, used by the
//! REPL exit and startup exits) closes persistent output procsubs
//! after the trap. GNU/rubash parity shapes below were verified
//! against rubash (finish_shell) and GNU bash 5.3.20 on Linux.

use std::path::PathBuf;
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
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niu" });
    fallback
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("niu-issue652-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_niu_in(dir: &PathBuf, args: &[&str], stdin: Option<&str>) -> Output {
    let mut cmd = Command::new(niu_binary());
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn niu");
    if let Some(input) = stdin {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().expect("wait niu")
}

/// The audit probe P9 shape, -c form: the deferred procsub body must
/// run at shell exit and deliver its accumulated input to LOG.
#[test]
fn procsub_output_delivered_at_c_exit() {
    let dir = unique_temp_dir("c");
    let output = run_niu_in(
        &dir,
        &["-c", "exec > >(cat > LOG); echo hello-procsub"],
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = std::fs::read_to_string(dir.join("LOG")).unwrap_or_else(|e| {
        panic!(
            "LOG must exist after niu -c exit: {e}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(log, "hello-procsub\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Same shape through a script file operand.
#[test]
fn procsub_output_delivered_at_script_exit() {
    let dir = unique_temp_dir("script");
    std::fs::write(dir.join("s.sh"), "exec > >(cat > LOG)\necho hello-script\n").unwrap();
    let output = run_niu_in(&dir, &["s.sh"], None);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = std::fs::read_to_string(dir.join("LOG")).unwrap_or_else(|e| {
        panic!(
            "LOG must exist after script exit: {e}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(log, "hello-script\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Same shape fed through the host stdin driver (bare `niu` with a
/// piped stdin), whose tail is the third swapped finish route.
#[test]
fn procsub_output_delivered_at_stdin_driver_end() {
    let dir = unique_temp_dir("stdin");
    let output = run_niu_in(&dir, &[], Some("exec > >(cat > LOG)\necho hello-stdin\n"));
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = std::fs::read_to_string(dir.join("LOG")).unwrap_or_else(|e| {
        panic!(
            "LOG must exist after stdin stream end: {e}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(log, "hello-stdin\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The end-of-stream job-notification flush (finish_shell step 2,
/// rubash#1853): a background job killed by a signal while the last
/// foreground command waits is reaped during that wait; with no
/// following command, only the stream-end flush prints its notice.
/// Verified shape: rubash prints `Killed ... /bin/sleep 5` at stream
/// end on this exact input; niu printed nothing before the fix.
#[test]
fn stream_end_job_notification_flushed() {
    if !cfg!(unix) {
        return;
    }
    let dir = unique_temp_dir("job");
    let output = run_niu_in(
        &dir,
        &[
            "-c",
            "/bin/sleep 5 & p=$!; (/bin/sleep 0.1; kill -9 $p) & /bin/sleep 0.5 | /bin/cat",
        ],
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("Killed") && stderr.contains("/bin/sleep 5"),
        "stream-end notice must be flushed, stderr was: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
