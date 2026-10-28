#![cfg(windows)]

//! D-05 spawn-count cell (RC audit phase 2) — end-to-end evidence for
//! niubash#643, the #592 S2 completion tracker.
//!
//! The in-process gates in `shell.rs` count `git_prompt::read_status`
//! calls on a test thread. This cell counts the thing those gates cannot
//! see: actual `git.exe` process creations by the real interactive
//! binary, observed from outside the process. A logging shim `git.exe`
//! is compiled on the fly (rustc, std-only), placed first in the child
//! session's PATH, and forwards every invocation to the real git after
//! appending its argv to a log file. The session runs under ConPTY via
//! the shared interactive driver, in the factory-default configuration
//! (no PS1 claim, so the "gitbash" prompt face renders), inside a
//! controlled one-commit repository.
//!
//! Scenario A: start in the repo, first prompt + 9 empty Enters = 10
//! prompt passes; report total git spawns and `status` spawns.
//! Scenario B: start outside any repository, first prompt + 3 empty
//! Enters; the prompt must never spawn (worktree probe short-circuits).
//!
//! The cell is measurement tooling on an audit branch, not a landed
//! gate: it only runs when `D05_CELL=1` (the workflow_dispatch workflow
//! `.github/workflows/d05-spawn-count.yml` sets it, together with
//! `NIU_INTERACTIVE_TESTS=1`). Result lines are printed and, when
//! `D05_OUT` names a file, appended there verbatim.

#[path = "interactive/driver.rs"]
mod driver;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use driver::{require_pty_or_skip, NiuSession, HEAVY_TIMEOUT};

/// The shim: log argv (unit-separator joined), then forward to the real
/// git with inherited stdio and propagate its exit code.
const SHIM_RS: &str = r#"
use std::io::Write;

fn main() {
    let log = std::env::var("NIU_D05_SHIM_LOG").unwrap_or_default();
    let real = std::env::var("NIU_D05_REAL_GIT").unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !log.is_empty() {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            let _ = writeln!(f, "{}", args.join("\u{1f}"));
        }
    }
    let status = std::process::Command::new(&real)
        .args(&args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status();
    match status {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(_) => std::process::exit(127),
    }
}
"#;

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("niu-d05-{tag}-{}-{nanos}", std::process::id()))
}

/// Locate the real git.exe on this host's PATH (the test process PATH,
/// before any shim directory exists).
fn real_git_exe() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH set");
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("git.exe");
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("no git.exe found on the test process PATH");
}

fn run_git(real_git: &Path, dir: &Path, args: &[&str]) -> bool {
    Command::new(real_git)
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=d05", "-c", "user.email=d05@example.com"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Compile the shim with the host rustc; returns the shim directory.
fn build_shim(shim_root: &Path) -> PathBuf {
    let src = shim_root.join("git_shim.rs");
    let exe = shim_root.join("git.exe");
    std::fs::write(&src, SHIM_RS).expect("write shim source");
    let status = Command::new("rustc")
        .arg("-O")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("run rustc for the git shim");
    assert!(status.success(), "rustc failed to build the git shim");
    assert!(exe.is_file(), "shim git.exe missing after rustc");
    shim_root.to_path_buf()
}

/// Count (total, status) spawns recorded in the shim log so far.
fn count_spawns(log: &Path) -> (usize, usize) {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let mut total = 0usize;
    let mut status = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        if line.split('\u{1f}').next() == Some("status") {
            status += 1;
        }
    }
    (total, status)
}

/// Soft-wait until the accumulated drained output contains `needle`
/// (returns the number of occurrences seen). Never panics: ConPTY
/// repaint timing is host-dependent, and the shim log — not the
/// screen — is the cell's evidence.
fn soft_wait(session: &mut NiuSession, needle: &str, within: Duration) -> usize {
    let deadline = Instant::now() + within;
    let mut acc = String::new();
    loop {
        acc.push_str(&session.drain_screen());
        let seen = acc.matches(needle).count();
        if seen > 0 {
            return seen;
        }
        if Instant::now() >= deadline {
            return 0;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn emit(line: String) {
    eprintln!("{line}");
    if let Ok(out) = std::env::var("D05_OUT") {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(out)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn run_scenario(
    tag: &str,
    in_repo: bool,
    enters: usize,
    shim_dir: &Path,
    real_git: &Path,
    shim_log: &Path,
) -> (usize, usize) {
    let root = unique_dir(tag);
    let start = root.join("start");
    std::fs::create_dir_all(&start).expect("create session start dir");
    if in_repo {
        assert!(
            run_git(real_git, &start, &["init", "-b", "main"]),
            "git init"
        );
        std::fs::write(start.join("seed.txt"), "seed\n").expect("write seed file");
        assert!(run_git(real_git, &start, &["add", "seed.txt"]), "git add");
        assert!(
            run_git(real_git, &start, &["commit", "-m", "seed"]),
            "git commit"
        );
    }
    let _ = std::fs::remove_file(shim_log);

    let system_root = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    let git_dir = real_git.parent().expect("real git dir").to_path_buf();
    let path_dirs = vec![
        shim_dir.to_path_buf(),
        git_dir,
        system_root.join("System32"),
        system_root.clone(),
    ];
    let extra_env: Vec<(String, String)> = vec![
        (
            "NIU_D05_REAL_GIT".to_string(),
            real_git.to_string_lossy().into_owned(),
        ),
        (
            "NIU_D05_SHIM_LOG".to_string(),
            shim_log.to_string_lossy().into_owned(),
        ),
    ];
    // No PS1 in the rc: the factory-default "gitbash" prompt face must
    // be the renderer under measurement (a PS1 claim would take the
    // claimed-prompt path instead).
    let rc = "NIU_DISABLE_DEFAULT_PLUGINS=1\n";
    let mut session =
        NiuSession::spawn_shared_home(&root, rc, &extra_env, &path_dirs, (120, 30), HEAVY_TIMEOUT);

    // First frame: in a repo the prompt carries "(main)"; outside, the
    // bare "$ " line appears with no branch segment.
    let marker = if in_repo { "(main)" } else { "$ " };
    let first = soft_wait(&mut session, marker, Duration::from_secs(60));
    emit(format!(
        "D05_{tag} first_frame_marker_occurrences={first} (soft)"
    ));
    for _ in 0..enters {
        session.press_enter();
        let _ = soft_wait(&mut session, marker, Duration::from_secs(20));
    }
    // Let any in-flight pass settle before counting.
    std::thread::sleep(Duration::from_millis(750));
    let (total, status) = count_spawns(shim_log);
    let passes = enters + 1;
    emit(format!(
        "D05_{tag} passes={passes} total_git_spawns={total} status_spawns={status}"
    ));
    session.send_line("exit");
    let _ = session.wait_exit();
    (total, status)
}

#[test]
fn d05_spawn_count_cell() {
    if std::env::var_os("D05_CELL").is_none() {
        eprintln!("SKIP d05_spawn_count_cell: D05_CELL not set (audit-branch measurement cell)");
        return;
    }
    if !require_pty_or_skip("d05_spawn_count_cell") {
        return;
    }
    let real_git = real_git_exe();
    emit(format!(
        "D05_ENV real_git={} niu={}",
        real_git.display(),
        driver::niu_binary().display()
    ));
    let shim_root = unique_dir("shim");
    std::fs::create_dir_all(&shim_root).expect("create shim root");
    let shim_dir = build_shim(&shim_root);
    let shim_log = shim_root.join("git-spawns.log");

    // Scenario A: controlled repository, 1 + 9 = 10 prompt passes.
    let (a_total, a_status) = run_scenario("A", true, 9, &shim_dir, &real_git, &shim_log);
    // Scenario B: outside any repository, 1 + 3 = 4 prompt passes.
    let (b_total, b_status) = run_scenario("B", false, 3, &shim_dir, &real_git, &shim_log);

    emit(format!(
        "D05_SUMMARY A_status={a_status} A_total={a_total} B_status={b_status} B_total={b_total}"
    ));
    // The one hard invariant: outside a repository the prompt never
    // spawns git at all (the worktree probe is pure filesystem).
    assert_eq!(
        b_status, 0,
        "outside a repository the prompt must not spawn `git status`"
    );
    assert!(
        a_status >= 1,
        "inside a repository the prompt must consult git at least once"
    );
}
