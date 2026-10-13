//! niubash#286 regression: the closed-stdout contract must hold for EVERY
//! non-interactive execution shape, not only `niu -c '<loop>'`.
//!
//! Issue #286 reported the `-c` shape (host_contract::closed_stdout_pipe…
//! red on 0830c78): with the reader gone, niu kept looping forever and
//! spammed `niu: line 1: Broken pipe` — one engine diagnostic per failed
//! write, forever, because Windows has no SIGPIPE to kill the producer and
//! the host had no convergence of its own. The host-level fix is the
//! piped-stdout pump (src/piped_stdout.rs, niubash#245/#256): every process
//! stdout writer — engine builtins AND external children, for `-c`, script
//! files, and stdin scripts alike — flows through one wrapper pipe; the
//! pump is the only writer to the real downstream, so a dead reader costs
//! exactly ONE diagnostic and one status-1 exit. The `-c` builtin-loop and
//! `yes` shapes are pinned by tests/broken_stdout_pipe.rs; this file pins
//! the shapes #286's report implies but nothing yet guards:
//!
//! - a script FILE with an unbounded producer (`niu loop.sh`)
//! - a stdin SCRIPT with an unbounded producer (bare `niu`, script on stdin)
//! - an EXTERNAL CHILD as the unbounded producer (inherited-handle path)
//! - stderr ALSO gone when the pump reports (the diagnostic write must not
//!   be able to hang or wedge the exit)
//!
//! GNU oracle (WSL bash): `bash -c 'while true; do echo x; done' | head -c
//! 16` dies promptly via SIGPIPE with ZERO stderr bytes. niu's vendored
//! adaptation (rubash#455 semantics: a failed write is an ordinary command
//! failure) keeps the prompt termination and the silence-about-errno, and
//! deliberately spends one descriptive diagnostic plus status 1 instead of
//! signal death — the shapes below pin exactly that.
//!
//! Every run is deadline-bounded: the pre-fix failure mode was "niu never
//! exits", so a hang must fail the test, never pass it.

#![cfg(windows)]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const EXIT_DEADLINE: Duration = Duration::from_secs(15);

/// The issue's unbounded producer, as a file body and as a stdin script.
const PRODUCER_SCRIPT: &str = "i=0\nwhile true; do echo line-$i; i=$((i+1)); done\n";

/// The same producer with an external child doing the writing: exercises
/// the wrapper-handle inheritance (script children must land on the pump's
/// FIFO too, or a dead reader would loop THEM forever).
const EXTERNAL_PRODUCER_SCRIPT: &str =
    "i=0\nwhile true; do cmd /c echo line-$i; i=$((i+1)); done\n";

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
        let dir = std::env::temp_dir().join(format!("niubash-286-{tag}-{}", std::process::id()));
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

struct ClosedReaderRun {
    code: Option<i32>,
    stderr: Option<String>,
}

/// Spawn niu with both streams piped, let the child produce, read a first
/// chunk of stdout, then drop the reader (and optionally stderr's reader
/// too) and wait for niu to exit ON ITS OWN. `argv` and `stdin_body` pick
/// the execution shape under test.
fn run_with_reader_gone(
    scratch: &ScratchDir,
    argv: &[String],
    stdin_body: Option<&str>,
    drop_stderr_too: bool,
) -> ClosedReaderRun {
    let home = scratch.home();
    let start = scratch.start();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let mut command = Command::new(niu_binary());
    command
        .args(argv)
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(if stdin_body.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().expect("spawn niu");

    if let Some(body) = stdin_body {
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(body.as_bytes()).expect("feed stdin script");
        drop(stdin); // EOF: the script is fully delivered
    }

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut buffer = [0_u8; 32];
    let _ = stdout.read(&mut buffer);
    drop(stdout); // the reader is gone; every later downstream write fails

    let mut stderr_pipe = child.stderr.take();
    if drop_stderr_too {
        drop(stderr_pipe.take());
    }

    let code = wait_for_own_exit(&mut child);
    let stderr = match stderr_pipe {
        Some(mut handle) => {
            let mut text = String::new();
            handle.read_to_string(&mut text).expect("read child stderr");
            Some(text)
        }
        None => None,
    };
    ClosedReaderRun { code, stderr }
}

/// Poll until the child exits by itself; kill and return None on deadline —
/// the pre-fix failure mode was precisely "never exits".
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

fn assert_contract(run: &ClosedReaderRun, context: &str) {
    assert_eq!(
        run.code,
        Some(1),
        "{context}: niu must exit by itself with the rubash#455-shaped status 1"
    );
    if let Some(stderr) = &run.stderr {
        assert!(
            !stderr.contains("Broken pipe")
                && !stderr.contains("os error 232")
                && !stderr.contains("管道正在被关闭"),
            "{context}: stderr must not carry raw OS broken-pipe spam: {stderr:?}"
        );
        let diagnostics = stderr
            .lines()
            .filter(|line| line.contains("write error"))
            .count();
        assert_eq!(
            diagnostics, 1,
            "{context}: expected exactly ONE diagnostic, got {diagnostics}: {stderr:?}"
        );
    }
}

/// #286 shape 1: the producer loop comes from a script FILE. The `-c` shape
/// was pinned by #256's own suite; the file shape reached the same unguarded
/// engine loop before the pump existed.
#[test]
fn script_file_producer_loop_exits_once_when_reader_closes() {
    let scratch = ScratchDir::new("file");
    std::fs::create_dir_all(scratch.start()).unwrap();
    std::fs::write(scratch.start().join("loop.sh"), PRODUCER_SCRIPT).expect("write loop.sh");
    let run = run_with_reader_gone(&scratch, &[String::from("loop.sh")], None, false);
    assert_contract(&run, "script-file while-true loop");
}

/// #286 shape 2: bare `niu` with the producer arriving on stdin — the
/// run_stdin_script route (no `-c` word at all).
#[test]
fn stdin_script_producer_loop_exits_once_when_reader_closes() {
    let scratch = ScratchDir::new("stdin");
    let run = run_with_reader_gone(&scratch, &[], Some(PRODUCER_SCRIPT), false);
    assert_contract(&run, "stdin-script while-true loop");
}

/// #286 shape 3: the unbounded producer is an EXTERNAL child (`cmd /c echo`
/// per iteration). Children inherit the process stdout handle, so they must
/// inherit the pump's wrapper end — a dead reader ends the whole process,
/// not just the builtins.
#[test]
fn external_child_producer_loop_exits_once_when_reader_closes() {
    let scratch = ScratchDir::new("external");
    let run = run_with_reader_gone(
        &scratch,
        &[String::from("-c"), EXTERNAL_PRODUCER_SCRIPT.to_string()],
        None,
        false,
    );
    assert_contract(&run, "external-child while-true loop");
}

/// #286 shape 4: stderr is gone too when the pump wants to report. The
/// diagnostic write fails into the void and the status-1 exit must still
/// land inside the deadline — no second hang behind the first broken pipe.
#[test]
fn exits_even_when_stderr_is_gone_too() {
    let scratch = ScratchDir::new("stderr");
    let run = run_with_reader_gone(
        &scratch,
        &[
            String::from("-c"),
            PRODUCER_SCRIPT.replace('\n', "; ").trim_end().to_string(),
        ],
        None,
        true,
    );
    assert_eq!(
        run.code,
        Some(1),
        "niu must exit by itself even when both output streams are gone"
    );
}
