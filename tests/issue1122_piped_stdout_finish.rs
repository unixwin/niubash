#![cfg(windows)]

//! Pins for niubash#1122: a one-shot `niu -c` whose stdout is a pipe must
//! exit as soon as its foreground work is done, even when a background
//! job (or a never-ending grandchild) still holds the inherited
//! stdout-pump pipe. Before the fix, `piped_stdout::finish()` waited for
//! a pump EOF that only arrives when the LAST descendant exits, so
//! `niu -c "ping -n 16 ... & echo done"` hung for the ping's whole life
//! (and a gradle daemon made it hang forever).
//!
//! Harness note: the tests must not read niu's pipes to EOF after it
//! exits. Background descendants inherit the test's pipe write ends too
//! (the same inheritance mechanism as the bug itself), so EOF can be a
//! descendant's lifetime away — or, for `ping -t`, never. Output is
//! therefore collected from chunk-reader threads with a short grace
//! window after niu's own exit; everything the pump forwarded before
//! the exit is already in the pipe buffer and is collected.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

/// How long to wait for niu ITSELF to exit. The fixed shell exits in
/// well under a second in every shape below; the pre-fix shell needed
/// the background job's whole lifetime (15s for `ping -n 16`).
const PROMPT_DEADLINE: Duration = Duration::from_secs(10);

/// Grace window for collecting piped output after niu exited.
const OUTPUT_GRACE: Duration = Duration::from_secs(3);

fn niu_binary() -> PathBuf {
    if let Ok(path) = std::env::var("NIU_TEST_BINARY") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_BIN_EXE_niu"))
}

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> ScratchDir {
        let path = std::env::temp_dir().join(format!("niubash-1122-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch dir");
        ScratchDir(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Spawn a background thread forwarding `reader`'s bytes in chunks.
fn chunk_reader<R: Read + Send + 'static>(mut reader: R) -> Receiver<Vec<u8>> {
    let (sender, receiver) = channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(size) => {
                    if sender.send(buffer[..size].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    receiver
}

/// Accumulate chunks until the stream closes or the grace window ends.
fn collect(receiver: &Receiver<Vec<u8>>, grace: Duration) -> Vec<u8> {
    let deadline = Instant::now() + grace;
    let mut out = Vec::new();
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match receiver.recv_timeout(deadline - now) {
            Ok(chunk) => out.extend_from_slice(&chunk),
            Err(_) => break, // closed (EOF) or grace expired
        }
    }
    out
}

/// Poll for the child's OWN exit; kill it if it overruns the deadline.
/// Returns (status, exited_on_its_own).
fn wait_for_own_exit(child: &mut Child, deadline: Duration) -> (ExitStatus, bool) {
    let started = Instant::now();
    loop {
        match child.try_wait().expect("poll child") {
            Some(status) => return (status, true),
            None => {
                if started.elapsed() > deadline {
                    let _ = child.kill();
                    let status = child.wait().expect("reap killed child");
                    return (status, false);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

struct RunOutput {
    status: ExitStatus,
    exited: bool,
    stdout: String,
    stderr: String,
}

fn run_niu(args: &[&str], script: &str) -> RunOutput {
    let mut child = Command::new(niu_binary())
        .args(args)
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");
    let stdout_rx = chunk_reader(child.stdout.take().expect("piped stdout"));
    let stderr_rx = chunk_reader(child.stderr.take().expect("piped stderr"));
    let (status, exited) = wait_for_own_exit(&mut child, PROMPT_DEADLINE);
    let stdout = String::from_utf8_lossy(&collect(&stdout_rx, OUTPUT_GRACE)).into_owned();
    let stderr = String::from_utf8_lossy(&collect(&stderr_rx, OUTPUT_GRACE)).into_owned();
    RunOutput {
        status,
        exited,
        stdout,
        stderr,
    }
}

/// The reported shape: background job's output redirected to a file,
/// the shell's own stdout is the pipe. The shell must exit at once and
/// its foreground output must be delivered through the pump.
#[test]
fn background_job_output_to_file_lets_shell_exit_promptly() {
    let scratch = ScratchDir::new("bgfile");
    let bg_log = scratch.path("bg.log");
    let script = format!(
        "ping -n 16 127.0.0.1 > {} 2>&1 & echo FG-DONE",
        bg_log.to_string_lossy().replace('\\', "/")
    );
    let out = run_niu(&["-c"], &script);
    assert!(
        out.exited,
        "niu must exit promptly with a background job running; \
         status={:?} stdout={:?} stderr={:?}",
        out.status, out.stdout, out.stderr
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {:?}", out.stderr);
    assert!(
        out.stdout.contains("FG-DONE"),
        "foreground output must be delivered: {:?}",
        out.stdout
    );
    let bg = fs::read_to_string(&bg_log).unwrap_or_default();
    assert!(
        bg.contains("Pinging") || bg.contains("ping"),
        "background job must have started writing its file: {bg:?}"
    );
}

/// Variant: the background job inherits the shell's stdout (writes into
/// the pump pipe). The shell must still exit at once; bytes the job
/// writes after the shell exits may be lost (documented #1122 fix
/// tradeoff), so only the foreground line is asserted.
#[test]
fn background_job_inheriting_stdout_lets_shell_exit_promptly() {
    let out = run_niu(&["-c"], "ping -n 16 127.0.0.1 & echo FG-DONE");
    assert!(
        out.exited,
        "niu must exit promptly even when the job inherits stdout; \
         status={:?} stdout={:?} stderr={:?}",
        out.status, out.stdout, out.stderr
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {:?}", out.stderr);
    assert!(
        out.stdout.contains("FG-DONE"),
        "foreground output must be delivered: {:?}",
        out.stdout
    );
}

/// The gradle-daemon shape: a NEVER-ending background child. The shell
/// must exit promptly anyway. The child is cleaned up by its exact PID
/// (parsed from `$!`) before any content assertion can fail.
#[test]
fn never_ending_background_child_lets_shell_exit_promptly() {
    let scratch = ScratchDir::new("forever");
    let bg_log = scratch.path("bg.log");
    let script = format!(
        "ping -t 127.0.0.1 > {} 2>&1 & echo BGPID=$!; echo FG-DONE",
        bg_log.to_string_lossy().replace('\\', "/")
    );
    let out = run_niu(&["-c"], &script);
    // Cleanup first: kill the never-ending child by its exact PID.
    let bg_pid = out.stdout.lines().find_map(|line| {
        line.strip_prefix("BGPID=")
            .and_then(|value| value.trim().parse::<u32>().ok())
    });
    if let Some(pid) = bg_pid {
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    assert!(
        out.exited,
        "niu must exit promptly with a never-ending background child; \
         status={:?} stdout={:?} stderr={:?}",
        out.status, out.stdout, out.stderr
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {:?}", out.stderr);
    assert!(
        out.stdout.contains("FG-DONE"),
        "foreground output must be delivered: {:?}",
        out.stdout
    );
    assert!(bg_pid.is_some(), "$! must be reported: {:?}", out.stdout);
}

/// Output produced BEFORE the exit decision must be fully delivered
/// even when the downstream reader is slow: the pump may block on its
/// write side, but it must not drop or truncate foreground output.
#[test]
fn slow_reader_still_receives_complete_foreground_output() {
    let mut child = Command::new(niu_binary())
        .args([
            "-c",
            "i=0; while (( i < 50000 )); do echo \"line-$i\"; i=$((i+1)); done",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let _stderr_rx = chunk_reader(child.stderr.take().expect("piped stderr"));
    let drain = std::thread::spawn(move || {
        let mut collected = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    collected.extend_from_slice(&buffer[..size]);
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        collected
    });
    let (status, exited) = wait_for_own_exit(&mut child, Duration::from_secs(120));
    let bytes = drain.join().expect("drain thread");
    assert!(exited, "niu must finish its foreground loop: {status:?}");
    assert_eq!(status.code(), Some(0));
    let text = String::from_utf8(bytes).expect("utf8 output");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        50000,
        "every foreground line must be delivered exactly once"
    );
    assert_eq!(lines.first(), Some(&"line-0"));
    assert_eq!(lines.last(), Some(&"line-49999"));
}

/// Reader-gone with a background job present (the #286/#1122 overlap):
/// exactly one write-error diagnostic and an immediate exit(1) via the
/// pump — no hang, no raw OS-error spam.
#[test]
fn reader_gone_with_background_job_still_exits_at_once() {
    let scratch = ScratchDir::new("gone");
    let bg_log = scratch.path("bg.log");
    let script = format!(
        "ping -n 30 127.0.0.1 > {} 2>&1 & while true; do echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done",
        bg_log.to_string_lossy().replace('\\', "/")
    );
    let mut child = Command::new(niu_binary())
        .arg("-c")
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let stderr_rx = chunk_reader(child.stderr.take().expect("piped stderr"));
    let mut first = [0_u8; 64];
    let read = stdout.read(&mut first).expect("read first bytes");
    assert!(read > 0, "some output must arrive before the drop");
    drop(stdout);
    let (status, exited) = wait_for_own_exit(&mut child, PROMPT_DEADLINE);
    assert!(
        exited,
        "reader-gone must exit at once even with a background job: {status:?}"
    );
    assert_eq!(status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&collect(&stderr_rx, OUTPUT_GRACE)).into_owned();
    let diagnostics = stderr.matches("write error").count();
    assert_eq!(
        diagnostics, 1,
        "exactly one write-error diagnostic expected: {stderr:?}"
    );
    for marker in ["Broken pipe", "os error 232", "管道正在被关闭"] {
        assert!(
            !stderr.contains(marker),
            "raw OS error text must not leak: {stderr:?}"
        );
    }
}

/// The script-file route funnels through the same finish path.
#[test]
fn script_file_route_lets_shell_exit_promptly() {
    let scratch = ScratchDir::new("script");
    let bg_log = scratch.path("bg.log");
    let script_path = scratch.path("job.sh");
    fs::write(
        &script_path,
        format!(
            "ping -n 16 127.0.0.1 > {} 2>&1 &\necho FG-DONE\n",
            bg_log.to_string_lossy().replace('\\', "/")
        ),
    )
    .expect("write script");
    let script_arg = script_path.to_string_lossy().replace('\\', "/");
    let out = run_niu(&[], &script_arg);
    assert!(
        out.exited,
        "script-file route must exit promptly; status={:?} stdout={:?} stderr={:?}",
        out.status, out.stdout, out.stderr
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {:?}", out.stderr);
    assert!(
        out.stdout.contains("FG-DONE"),
        "foreground output must be delivered: {:?}",
        out.stdout
    );
}

/// The interactive shell does not install the one-shot drain path; a
/// piped-stdin REPL must still run commands and exit on `exit`.
#[test]
fn interactive_shell_still_runs_repl_commands() {
    use std::io::Write;
    let mut child = Command::new(niu_binary())
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu -i");
    let stdout_rx = chunk_reader(child.stdout.take().expect("piped stdout"));
    let _stderr_rx = chunk_reader(child.stderr.take().expect("piped stderr"));
    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin
            .write_all(b"echo INTERACTIVE-OK\nexit\n")
            .expect("write repl input");
    }
    let (status, exited) = wait_for_own_exit(&mut child, PROMPT_DEADLINE);
    let stdout = String::from_utf8_lossy(&collect(&stdout_rx, OUTPUT_GRACE)).into_owned();
    assert!(
        exited,
        "interactive shell must exit on `exit`: status={status:?} stdout={stdout:?}"
    );
    assert_eq!(status.code(), Some(0));
    assert!(
        stdout.contains("INTERACTIVE-OK"),
        "repl must run commands: {stdout:?}"
    );
}
