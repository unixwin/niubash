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
    let mut s =
        NiuSession::spawn_shared_home(&root, rc, &[], &[], (120, 30), Duration::from_secs(20));
    s.wait_ready();
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
