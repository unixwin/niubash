//! niubash#1079 regression: a process substitution or coproc whose reader
//! stops early must die the GNU way — silently, with `wait` reporting 141.
//!
//! A `<( )` body runs in a child the engine respawns as `niu -c '<body>'`
//! with its stdout on the substitution pipe. In GNU that child is a forked
//! subshell and SIGPIPE kills it mid-write: no diagnostic, status 128+13.
//! niu's respawned child arrives as a plain `-c`, so the #245 piped-stdout
//! pump (src/piped_stdout.rs) used to treat its failed downstream write as
//! the user's own broken stdout: it printed
//! `niu: stdout: write error: pipe closed by downstream reader` and exited
//! 1 — once per early-closed shape, and `wait` reported the wrong status.
//! The pump now asks rubash's parentage-validated provenance check (read in
//! `install_for_argv`, before the engine imports and scrubs the keys): a
//! validated respawn keeps the pump's FIFO but takes the GNU child-SIGPIPE
//! shape on a failed write — quiet exit 141.
//!
//! The #245 control at the bottom pins the other side of the gate: a
//! DIRECT `niu -c '…' | <reader that leaves>` carries no provenance claim,
//! so it must still print the one diagnostic and exit 1.
//!
//! Every run is deadline-bounded: a hang must fail the test, never pass it.

#![cfg(windows)]

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const EXIT_DEADLINE: Duration = Duration::from_secs(30);

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
        let dir = std::env::temp_dir().join(format!("niubash-1079-{tag}-{}", std::process::id()));
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

struct ScriptRun {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run `niu -c <script>` to completion with both streams piped and return
/// exactly what the OUTER shell printed. The early close under test happens
/// inside the script, between the respawned child and its reader.
fn run_script(scratch: &ScratchDir, script: &str) -> ScriptRun {
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

    let code = wait_for_own_exit(&mut child);
    let mut stdout_handle = child.stdout.take().expect("piped stdout");
    let mut stdout = String::new();
    stdout_handle
        .read_to_string(&mut stdout)
        .expect("read child stdout");
    let mut stderr_handle = child.stderr.take().expect("piped stderr");
    let mut stderr = String::new();
    stderr_handle
        .read_to_string(&mut stderr)
        .expect("read child stderr");
    ScriptRun {
        code,
        stdout,
        stderr,
    }
}

/// Poll until the child exits by itself; kill and return None on deadline.
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

fn assert_quiet(run: &ScriptRun, expected_stdout: &str, context: &str) {
    assert_eq!(
        run.code,
        Some(0),
        "{context}: the outer shell must exit 0; stderr: {:?}",
        run.stderr
    );
    assert_eq!(
        run.stdout, expected_stdout,
        "{context}: stdout must be byte-exact the GNU output"
    );
    assert_eq!(
        run.stderr, "",
        "{context}: no diagnostic may escape the early-closed child"
    );
}

/// Reporter shape 1 (external-reader form): `head -1` takes one line and
/// leaves; the producer's later writes hit a readerless pipe. Premise-
/// guarded like issue207's external-pipeline pin: `head` must resolve
/// through niu (on hosts without the WinuxCmd links it does not exist).
#[test]
fn procsub_head_reader_leaves_quietly() {
    let scratch = ScratchDir::new("head");
    let probe = run_script(&scratch, "head --version </dev/null");
    if probe.code != Some(0) {
        eprintln!(
            "skip procsub_head_reader_leaves_quietly: premise missing — \
             `head --version` via niu failed (code: {:?}, stderr: {:?}); \
             this pin needs the WinuxCmd `head` link",
            probe.code, probe.stderr
        );
        return;
    }
    let run = run_script(
        &scratch,
        "head -1 < <(for i in 1 2 3; do echo \"$i\"; sleep 0.3; done)\nsleep 1\necho done",
    );
    assert_quiet(&run, "1\ndone\n", "head -1 < <(...)");
}

/// Reporter shape 1 (pure-shell form, no external reader): `read` takes
/// the first line off the substitution fd and the redirect closes when the
/// command returns — the producer's next write has no reader. Runs
/// everywhere; this is the always-on pin of the same mechanism.
#[test]
fn procsub_read_one_line_then_fd_closes_quietly() {
    let scratch = ScratchDir::new("readone");
    let run = run_script(
        &scratch,
        "read -r first < <(for i in 1 2 3; do echo \"$i\"; sleep 0.3; done); \
         echo \"first: $first\"\nsleep 1\necho done",
    );
    assert_quiet(&run, "first: 1\ndone\n", "read -r < <(...)");
}

/// Reporter shape 2: the `case` word names the substitution path but never
/// opens it; the child still tries to deliver its line.
#[test]
fn procsub_case_word_never_reads_quietly() {
    let scratch = ScratchDir::new("case");
    let run = run_script(
        &scratch,
        "case <(echo hi) in *) echo case ;; esac\nsleep 1\necho done",
    );
    assert_quiet(&run, "case\ndone\n", "case <(echo hi)");
}

/// Reporter shape 3: the reader takes one line off fd 3, closes it, and
/// `wait` must report the GNU SIGPIPE status for the cut-off producer.
#[test]
fn procsub_fd_closed_early_wait_reports_141() {
    let scratch = ScratchDir::new("fd3");
    let run = run_script(
        &scratch,
        "exec 3< <(echo a; sleep 0.5; echo b); read -r -u 3 x; exec 3<&-; \
         wait $!; echo \"wait: $?\"",
    );
    assert_quiet(&run, "wait: 141\n", "exec 3< <(...) early close");
}

/// Reporter shape 4: a coproc respawns through the same path; closing its
/// read fd early must leave `wait` reporting 141 and no diagnostic.
#[test]
fn coproc_reader_closes_early_wait_reports_141() {
    let scratch = ScratchDir::new("coproc");
    let run = run_script(
        &scratch,
        "coproc { echo a; sleep 0.5; echo b; }\n\
         read -r x <&\"${COPROC[0]}\"; exec {COPROC[0]}<&-; \
         wait $COPROC_PID; echo \"coproc wait: $?\"",
    );
    assert_quiet(&run, "coproc wait: 141\n", "coproc early close");
}

/// The reporter's full script, end to end: stdout byte-exact the GNU
/// output and stderr empty across all four shapes in sequence.
#[test]
fn reporter_script_matches_gnu_byte_for_byte() {
    let scratch = ScratchDir::new("full");
    let probe = run_script(&scratch, "head --version </dev/null");
    if probe.code != Some(0) {
        eprintln!(
            "skip reporter_script_matches_gnu_byte_for_byte: premise missing \
             — `head` does not resolve via niu on this host"
        );
        return;
    }
    let script = "\
head -1 < <(for i in 1 2 3; do echo \"$i\"; sleep 0.3; done)
sleep 1
case <(echo hi) in *) echo case ;; esac
sleep 1
exec 3< <(echo a; sleep 0.5; echo b); read -r -u 3 x; exec 3<&-; wait $!; echo \"wait: $?\"
coproc { echo a; sleep 0.5; echo b; }
read -r x <&\"${COPROC[0]}\"; exec {COPROC[0]}<&-; wait $COPROC_PID; echo \"coproc wait: $?\"";
    let run = run_script(&scratch, script);
    assert_quiet(
        &run,
        "1\ncase\nwait: 141\ncoproc wait: 141\n",
        "reporter script",
    );
}

/// #245 control: a DIRECT `niu -c '<unbounded producer>'` whose reader
/// leaves carries no provenance claim, so the pump must keep the #245
/// contract — exactly one diagnostic, exit status 1. This is the shape
/// that proves the #1079 quiet-141 policy did not leak across the gate.
#[test]
fn direct_shell_broken_stdout_still_reports_once_and_exits_1() {
    let scratch = ScratchDir::new("control245");
    let home = scratch.home();
    let start = scratch.start();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&start).unwrap();

    let mut child = Command::new(niu_binary())
        .arg("-c")
        .arg("while :; do echo y; done")
        .current_dir(&start)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut buffer = [0_u8; 32];
    let _ = stdout.read(&mut buffer);
    drop(stdout); // the reader is gone; every later downstream write fails

    let code = wait_for_own_exit(&mut child);
    let mut stderr_handle = child.stderr.take().expect("piped stderr");
    let mut stderr = String::new();
    stderr_handle
        .read_to_string(&mut stderr)
        .expect("read child stderr");

    assert_eq!(
        code,
        Some(1),
        "#245 control: a direct niu must exit by itself with status 1"
    );
    let diagnostics = stderr
        .lines()
        .filter(|line| line.contains("write error"))
        .count();
    assert_eq!(
        diagnostics, 1,
        "#245 control: expected exactly ONE diagnostic, got {diagnostics}: {stderr:?}"
    );
}
