//! niubash#787 regression: at a cold start inside a repository, the
//! git prompt segment's background refresh landed in the cache but
//! nothing repainted the idle prompt line — the segment stayed
//! invisible until the user's first keypress generated a new frame.
//! The #643 Windows acceptance (PR #757, observation A) proved the
//! spawn itself completed before any keypress; only the repaint
//! trigger was missing.
//!
//! Contract pinned here: with ZERO input after startup, a frame
//! carrying the branch segment arrives on its own within a bounded
//! wait. The mechanism (cache landing observer → live-cell re-stamp
//! → one editor repaint) is pinned cross-platform by the runtime
//! unit tests (`idle_repaint`, `git_prompt_cache::issue787_*`,
//! `prompt_segments::issue787_*`, `prompt::issue787_*`); this
//! end-to-end pin runs on the face that ships the segment by
//! default — the Windows gitbash factory floor. Off-Windows the
//! factory floor is the template prompt, which carries no Vcs
//! surface on any code, so the test reports and returns there
//! instead of pretending to measure the segment.
//!
//! Driver: [`mod driver`] (portable-pty; the reader thread answers
//! the painter's `ESC[6n` cursor-position query, without which
//! nothing paints).

#[path = "interactive/driver.rs"]
mod driver;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use driver::{require_pty_or_skip, NiuSession};

fn git(dir: &std::path::Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn unique_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("niu-{tag}-{}-{nanos}", std::process::id()))
}

#[test]
fn issue787_git_segment_appears_at_idle_without_a_keypress() {
    if !require_pty_or_skip("issue787_git_segment_appears_at_idle_without_a_keypress") {
        return;
    }
    if !cfg!(windows) {
        // The segment face this pin measures is the Windows factory
        // floor; off-Windows the floor prompt has no git segment on
        // any code (see the file header). The cross-platform proof
        // lives in the runtime unit tests named there.
        eprintln!(
            "issue787 PTY pin: the factory floor carries no git segment off-Windows; \
             mechanism pins in the runtime unit tests cover this platform"
        );
        return;
    }
    // Stage the session root with a real repository as its start
    // directory (spawn_shared_home lays out root/home + root/start).
    let root = unique_root("issue787-idle-repaint");
    let start = root.join("start");
    std::fs::create_dir_all(&start).unwrap();
    if !git(&start, &["init", "--quiet", "--initial-branch=main", "."])
        || !git(&start, &["add", "-A"])
        || !git(
            &start,
            &["commit", "--quiet", "--allow-empty", "-m", "init"],
        )
    {
        eprintln!("issue787 PTY pin: git is unavailable; skipping");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }
    // No PS1 claim: the factory floor renders, git segment included.
    let rc = "NIU_DISABLE_DEFAULT_PLUGINS=1\n";
    // The segment is `git_prompt` spawning `git` through the session
    // PATH, and the sandbox PATH (System32 + Windows only) has no git
    // — without this override the refresh lands `None` and the pin can
    // never see the segment (same gap interactive.rs's gitbash-floor
    // test fixed for niubash#420). Discover the host git the fixture
    // helper itself resolves and prepend its dir to the session PATH.
    let git_where = Command::new("where.exe")
        .arg("git")
        .stdin(Stdio::null())
        .output()
        .expect("locate git for the session PATH");
    let git_exe = String::from_utf8_lossy(&git_where.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .expect("git.exe path from where.exe")
        .to_string();
    let git_dir = std::path::Path::new(&git_exe)
        .parent()
        .expect("git.exe parent dir")
        .to_string_lossy()
        .to_string();
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let session_path = format!(
        "{git_dir};{};{}",
        system_root.join("System32").display(),
        system_root.display()
    );
    let extra_env = vec![("PATH".to_string(), session_path)];
    let mut s = NiuSession::spawn_shared_home(
        &root,
        rc,
        &extra_env,
        &[],
        (120, 30),
        Duration::from_secs(20),
    );
    // This rc sets no PS1, so the session paints the factory floor's
    // `user@localhost … $ ` instead of the P1> sentinel — readiness is
    // the floor's own startup sequence (banner + floor prompt), never
    // `wait_ready`'s PS1 sentinel (niubash#789 test-face fix: the old
    // wait self-deadlocked at the sentinel this session never prints).
    s.wait_ready_factory_floor();
    // Zero input from here on. The cold-start refresh lands on its
    // own (bounded by the cache's 2s status timeout plus one editor
    // poll interval); the landing must repaint the idle line so a
    // frame carrying the branch arrives without any keypress.
    // Before the fix no such frame ever arrived and this expect
    // timed out.
    let frame = s.expect("main");
    assert!(
        frame.contains("main"),
        "the git segment must reach the idle prompt with no keypress:\n{frame:?}"
    );
}
