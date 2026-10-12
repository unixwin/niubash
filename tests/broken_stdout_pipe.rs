//! niubash#245 regression: the external `niu -c '<producer>' | <reader that
//! leaves>` shape must TERMINATE — one diagnostic, non-zero status, no
//! unbounded `Broken pipe` spam.
//!
//! rubash#455 gave the engine GNU's parent-shell semantics for closed-output
//! writes (report, status 1, the list continues). That is correct for
//! pipeline members and finite scripts, but when the shell's OWN stdout is
//! the pipe whose reader went away, "continues" degenerates on Windows:
//! every later write fails again (ERROR_BROKEN_PIPE/ERROR_NO_DATA — no
//! SIGPIPE analogue kills the producer), so an unbounded builtin loop
//! printed one diagnostic per iteration forever and niu never exited. The
//! product layer now interposes a pump between the process's stdout writers
//! and the real pipe (src/piped_stdout.rs); when the downstream write fails,
//! niu reports once and exits 1.
//!
//! Semantics ownership vs rubash#455 (issue440_broken_pipe_shell_survival):
//! INSIDE a script a broken pipeline member is still an ordinary command
//! failure and the script keeps running (`inner_pipeline_*` below); the
//! process-level one-shot diagnostic + status 1 is niu's host contract for
//! its own stdout (host_contract::closed_stdout_pipe, this file).
//!
//! Every run is deadline-bounded: the pre-fix failure mode was "niu never
//! exits", so a hang must fail the test, never pass it.

#![cfg(windows)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const EXIT_DEADLINE: Duration = Duration::from_secs(15);

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

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("niubash-245-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        ScratchDir(dir)
    }

    fn home(&self) -> PathBuf {
        self.0.join("home")
    }

    fn start(&self) -> PathBuf {
        self.0.join("start")
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One deadline-bounded closed-stdout run: spawn `niu -c script` with both
/// streams piped, read exactly `read_bytes` from stdout, drop the reader,
/// then wait for the child to exit ON ITS OWN.
struct ClosedReaderRun {
    code: Option<i32>,
    stderr: String,
}

fn run_with_reader_gone(script: &str, read_bytes: usize, scratch: &ScratchDir) -> ClosedReaderRun {
    let home = scratch.home();
    let start = scratch.start();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let mut child = Command::new(niu_binary())
        .arg("-c")
        .arg(script)
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut buffer = vec![0_u8; read_bytes.max(1)];
    let _ = stdout.read(&mut buffer);
    drop(stdout); // the reader is gone; every later downstream write fails

    let code = wait_for_own_exit(&mut child);
    let mut stderr_handle = child.stderr.take().expect("piped stderr");
    let mut stderr = String::new();
    stderr_handle
        .read_to_string(&mut stderr)
        .expect("read child stderr");
    ClosedReaderRun { code, stderr }
}

/// Poll until the child exits by itself; kill and return None on deadline —
/// the pre-#245 failure mode was precisely "never exits".
fn wait_for_own_exit(child: &mut Child) -> Option<i32> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) if started.elapsed() < EXIT_DEADLINE => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }
}

fn assert_single_diagnostic(run: &ClosedReaderRun, context: &str) {
    assert_eq!(
        run.code,
        Some(1),
        "{context}: expected the rubash#455-shaped status 1 exit; stderr: {:?}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("Broken pipe")
            && !run.stderr.contains("os error 232")
            && !run.stderr.contains("管道正在被关闭"),
        "{context}: stderr must not carry raw OS broken-pipe spam: {:?}",
        run.stderr
    );
    let diagnostics = run
        .stderr
        .lines()
        .filter(|line| line.contains("write error"))
        .count();
    assert_eq!(
        diagnostics, 1,
        "{context}: expected exactly ONE diagnostic, got {diagnostics}: {:?}",
        run.stderr
    );
}

/// The issue's exact producer shape (`while true; do echo; done`): after the
/// reader leaves, niu must exit — once, with status 1, one diagnostic —
/// instead of looping on per-write failures forever.
#[test]
fn unbounded_builtin_loop_exits_once_when_reader_closes() {
    let scratch = ScratchDir::new("loop");
    let run = run_with_reader_gone(
        "i=0; while true; do echo line-$i; i=$((i+1)); done",
        32,
        &scratch,
    );
    assert_single_diagnostic(&run, "builtin while-true loop");
}

/// The task's minimal external repro shape (`niu -c 'yes' | head -c 1`):
/// the unbounded `yes` producer must terminate the same way.
#[test]
fn yes_producer_exits_once_when_reader_closes() {
    let scratch = ScratchDir::new("yes");
    let run = run_with_reader_gone("yes", 1, &scratch);
    assert_single_diagnostic(&run, "yes producer");
}

/// While the reader STAYS, the pump must be invisible: full output, in
/// order, through the drain — including output larger than the wrapper
/// pipe's buffer (guards the exit-time drain against truncation).
#[test]
fn live_reader_receives_full_output_through_the_pump() {
    let scratch = ScratchDir::new("drain");
    let home = scratch.home();
    let start = scratch.start();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let mut child = Command::new(niu_binary())
        .arg("-c")
        .arg("echo FIRST; seq 1 50000; echo LAST")
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut collected = String::new();
    stdout
        .read_to_string(&mut collected)
        .expect("read stdout to end");

    let code = wait_for_own_exit(&mut child);
    assert_eq!(code, Some(0), "clean run must still exit 0");
    let lines: Vec<&str> = collected.lines().collect();
    assert_eq!(lines.len(), 50_002, "every line must survive the pump");
    assert_eq!(lines.first(), Some(&"FIRST"));
    assert_eq!(lines.last(), Some(&"LAST"));
}

/// rubash#455 parity: a broken pipe INSIDE a script (`yes | head -c 1`) is
/// the pipeline's own GNU SIGPIPE shape — quiet, status 0, and the shell
/// exits cleanly. The process-level guard must not touch it (the pipeline
/// stages write to an internal pipe, not the shell's own stdout).
#[test]
fn inner_pipeline_broken_pipe_keeps_gnu_shape() {
    let scratch = ScratchDir::new("inner");
    let home = scratch.home();
    let start = scratch.start();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = Command::new(niu_binary())
        .arg("-c")
        .arg("yes | head -c 3; echo done-$?")
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    // head -c 3 keeps exactly three bytes of the `y\n` stream; the shell
    // then runs on (`done-0`) — GNU's SIGPIPE-shape for pipeline members.
    assert!(
        stdout.starts_with("y\ny"),
        "head -c 3 payload missing: {stdout:?}"
    );
    assert!(
        stdout.contains("done-0"),
        "the script must keep running after the inner broken pipe: {stdout:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !stderr.contains("write error"),
        "inner-pipeline SIGPIPE shape must stay quiet: {stderr:?}"
    );
}

/// A script that WRITES then FAILS (`echo hi; false`) still delivers its
/// output through the drain before the nonzero status lands.
#[test]
fn nonzero_script_status_still_delivers_output() {
    let scratch = ScratchDir::new("status");
    let home = scratch.home();
    let start = scratch.start();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let output = Command::new(niu_binary())
        .arg("-c")
        .arg("echo hi; exit 3")
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");

    assert_eq!(output.status.code(), Some(3));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(normalize(&stdout, &start), "hi");
}

fn normalize(stdout: &str, _cwd: &Path) -> String {
    stdout.trim_end_matches(['\r', '\n']).to_string()
}
