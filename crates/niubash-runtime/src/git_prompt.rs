//! Windows-only built-in "Git Bash compatible" default prompt.
//!
//! Owner adjudication (2026-10-12, niubash 1.4.0 cycle): the factory default
//! prompt on Windows mirrors the Git Bash skeleton users already know —
//! green `user@host`, yellow cwd, cyan branch (+ dirty `*`), `$ ` — with the
//! MINGW64 environment segment deliberately omitted (that is MinGW identity,
//! not ours) and no static product tag (a single-environment shell has no
//! disambiguation need; identity lives in the banner and `niu --version`).
//!
//! The branch/dirty segment reads the repository with the user's own `git`,
//! bounded and isolated, instead of an in-process library:
//!
//!  - **fidelity** is the product promise: sparse checkouts, submodules,
//!    linked worktrees, partial clones and fsmonitor state can only be
//!    answered by the git the user actually runs (the libgit2 prompt silently
//!    dropped the branch segment on old-owner repo trees once already);
//!  - **cost**: a bounded `git status --porcelain=v2 --branch` runs at
//!    most once per command cycle per repository, on a background
//!    refresh thread the draw never waits on (the cross-pass cache
//!    core in `git_prompt_cache`, niubash#685/#643). A prompt pass
//!    only reads the cached segment — fresh while the repo
//!    fingerprint (HEAD contents + index stat) still matches and no
//!    command cycle has completed since it was computed; a stale or
//!    cold entry is served as-is for that pass while its refresh
//!    lands — for the next pass, or (niubash#787) for the displayed
//!    pass itself: the landing re-stamps the pass's live Vcs cell
//!    and a single editor repaint carries the segment to the idle
//!    line. Repaints within a pass re-read the pass's stamp, never
//!    the cache, and never spawn. Outside a
//!    repository the segment spawns nothing at all, which is already
//!    better than `__git_ps1`, that forks several times even in
//!    `$HOME`;
//!  - **isolation**, one rule per incident: never through a shell (a missing
//!    git is an OS error, not a "command not found" on the terminal), stdout
//!    piped and stderr discarded (git warnings can never reach the screen),
//!    `--no-optional-locks` + `GIT_OPTIONAL_LOCKS=0` (a prompt must never take
//!    `index.lock`), `-c core.fsmonitor=false` (a prompt must never start or
//!    poke an fsmonitor daemon), `GIT_TERMINAL_PROMPT=0`, no console window,
//!    and the child is killed when it outlives the timeout.
//!
//! This is not the niubash#145 shape: that was a persistent worker thread
//! plus a daemon child that the main thread *waited on before the first
//! prompt*. The child here is scoped to one cache refresh, runs on a
//! detached thread spawned by that refresh, and no prompt pass ever
//! waits on it; the startup regression tests keep asserting that nothing
//! is waited on before the first paint.
//!
//! Segments outside a repository are empty (Git Bash prints no branch there
//! either), and a detached HEAD shows the short SHA the way `__git_ps1`
//! does. This module is a leaf: no shell state, no executor, cwd in/cwd out.

use std::path::Path;

/// Cyan (`\x1b[36m`) — Git Bash's `__git_ps1` branch colour.
const CYAN: &str = "\x1b[36m";
/// Green (`\x1b[32m`) — Git Bash's `user@host` colour.
const GREEN: &str = "\x1b[32m";
/// Yellow (`\x1b[33m`) — Git Bash's cwd colour.
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

/// Render the factory-default left prompt for `cwd`.
///
/// Shape (owner-approved, two-row per Git Bash's own layout — owner note
/// 2026-10-14): row one is `green user@host  yellow cwd  cyan (branch*)`,
/// row two is the `$ ` input line on its own row; segments separated by
/// single spaces, branch segment (and its row space) empty outside a
/// repository. The prompt char is `$ ` (Git Bash's non-root char; niu does
/// not run elevated by design, `PrivilegesRequired=lowest`). `\r\n` keeps
/// the raw-terminal column honest (a bare `\n` would land row two at the
/// break column under ConPTY).
pub fn render(cwd: &Path) -> String {
    render_with_segment(cwd, branch_segment(cwd).as_deref())
}

/// Render the factory prompt with an explicit segment value instead
/// of reading the cache: the prompt's live Vcs channel
/// (niubash#787) rebuilds the baked prompt from a landed value
/// through this entry, so a mid-pass landing and a pass-start read
/// produce byte-identical prompts for the same value.
pub(crate) fn render_with_segment(cwd: &Path, branch: Option<&str>) -> String {
    let mut left = format!(
        "{GREEN}{}@{}{RESET} {YELLOW}{}{RESET}",
        user_name(),
        host_name(),
        display_cwd(cwd),
    );
    if let Some(branch) = branch {
        left.push(' ');
        // Git Bash's __git_ps1 shape: the branch rides in parentheses —
        // `(master*)`, dirty star inside (owner note 2026-10-14: next to
        // MINGW64's `(fix/...`)` the bare word read as missing something).
        // `branch_segment` stays bare: the Vcs segment keys its dirty
        // colour off the trailing star.
        left.push_str(&format!("{CYAN}({branch}){RESET}"));
    }
    left.push_str("\r\n$ ");
    left
}

/// `C:/...` -> `/c/...` (absolute drive paths only; relative paths and UNC
/// spellings pass through unchanged — the persona form is a display
/// contract, not a path rewrite).
fn to_posix_drive(text: &str) -> String {
    let bytes = text.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/' {
        let drive = (bytes[0] as char).to_lowercase();
        format!("/{drive}{}", &text[2..])
    } else {
        text.to_string()
    }
}

/// `USERNAME@COMPUTERNAME` — the same sources Git Bash's `\u@\h` resolves
/// on Windows (getpwuid via MSYS maps to the Windows account name).
fn user_name() -> String {
    std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string())
}

fn host_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "localhost".to_string())
}

/// The cwd in Git Bash's POSIX spelling: `C:\Users\me` renders `/c/Users/me`
/// and anything under the home directory abbreviates to `~/...` the way
/// `\w` does. Drives lowercase to match the persona (`/c/...`, rubash#556).
fn display_cwd(cwd: &Path) -> String {
    let posix = to_posix_drive(&cwd.to_string_lossy().replace('\\', "/"));
    match std::env::var("USERPROFILE") {
        Ok(home) if !home.is_empty() => {
            let home = to_posix_drive(&home.replace('\\', "/"));
            if let Some(rest) = posix.strip_prefix(&home) {
                let rest = rest.trim_start_matches('/');
                return if rest.is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{rest}")
                };
            }
        }
        _ => {}
    }
    posix
}

/// The git segment of the prompt: `branch` / `branch*` (dirty) / short SHA
/// (detached) — `None` outside a repository, when git is unavailable, or
/// when the last read failed or timed out. Failures are silent by design:
/// the prompt degrades to "no branch" and the terminal stays clean.
///
/// The value comes from the cross-pass cache core (`git_prompt_cache`,
/// niubash#685/#643): this call never spawns and never waits — on a cold
/// or stale entry it returns the cached value (or `None`) and a
/// background refresh lands the fresh value for the next call.
///
/// Public so the segment prompt engine can drive it
/// (`SegmentId::Vcs`, niubash#331): the oh-my-niu bundle's
/// `[segments.git] id = "vcs"` declaration resolves to this renderer. The
/// dirty `*` suffix is the dirty-state marker; the segment engine keys its
/// `[git_dirty]` yellow off exactly that star.
pub fn branch_segment(cwd: &Path) -> Option<String> {
    crate::git_prompt_cache::branch_segment(cwd)
}

/// Mark the cached segment for the repository containing `cwd` dirty:
/// the host's command-cycle invalidation, delegated to the cache core.
pub(crate) fn invalidate_segment_for_cwd(cwd: &Path) {
    crate::git_prompt_cache::invalidate_segment_for_cwd(cwd);
}

// Test-build hooks of the cache core, re-exported so the prompt-face
// probes (this module's tests, `prompt_segments`' budget gate and
// `shell`'s #592 cells) keep keying their counts through this module
// exactly as they did when the cache lived here.
#[cfg(test)]
pub(crate) use crate::git_prompt_cache::{
    read_status_call_count, wait_for_segment_cache_idle, worktree_key_for,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    /// Fixtures are real repositories: the segment now reads the user's git,
    /// so the tests drive the same path production does. A `false` means git
    /// is not on PATH; the fixture then aborts quietly instead of failing a
    /// machine that has no git.
    fn git(dir: &Path, args: &[&str]) -> bool {
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

    fn init_repo(dir: &Path) -> bool {
        git(dir, &["init", "--quiet", "--initial-branch=main", "."])
    }

    fn repo_with_commit(dir: &Path) -> bool {
        init_repo(dir)
            && git(dir, &["add", "-A"])
            && git(dir, &["commit", "--quiet", "--allow-empty", "-m", "init"])
    }

    /// Drive one segment read to its settled value under the #685
    /// cache: the first call only triggers the background refresh, so
    /// wait for the landing (event-driven, never a poll), then read
    /// again.
    fn settled_segment(dir: &Path) -> Option<String> {
        let _ = branch_segment(dir);
        assert!(
            wait_for_segment_cache_idle(Duration::from_secs(30)),
            "the segment refresh must land"
        );
        branch_segment(dir)
    }

    /// `render` over a settled cache (see `settled_segment`).
    fn settled_render(dir: &Path) -> String {
        let _ = settled_segment(dir);
        render(dir)
    }

    fn git_stdout(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => String::new(),
        }
    }

    #[test]
    fn non_repository_renders_without_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let left = render(tmp.path());
        assert!(left.contains("$ "), "{left:?}");
        assert!(
            !left.contains("\u{1b}[36m"),
            "no cyan outside a repo: {left:?}"
        );
        assert!(left.contains(&user_name()), "{left:?}");
    }

    #[test]
    fn clean_repo_shows_branch_without_dirty_mark() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        let left = settled_render(tmp.path());
        assert!(left.contains("(main)"), "{left:?}");
        assert!(!left.contains("(main*)"), "{left:?}");
    }

    #[test]
    fn dirty_worktree_appends_the_star() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        std::fs::write(tmp.path().join("tracked.txt"), b"v1").unwrap();
        if !git(tmp.path(), &["add", "tracked.txt"])
            || !git(tmp.path(), &["commit", "--quiet", "-m", "add"])
        {
            return;
        }
        std::fs::write(tmp.path().join("tracked.txt"), b"v2").unwrap();
        let left = settled_render(tmp.path());
        assert!(left.contains("(main*)"), "{left:?}");
    }

    #[test]
    fn untracked_file_marks_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        std::fs::write(tmp.path().join("brand-new.txt"), b"x").unwrap();
        let left = settled_render(tmp.path());
        assert!(left.contains("(main*)"), "{left:?}");
    }

    #[test]
    fn unborn_head_shows_the_branch() {
        // A fresh `git init` has no commit yet; the previous libgit2 read
        // errored here and the segment vanished, which is not what Git Bash
        // does (owner-visible: a brand-new repository showed no branch).
        let tmp = tempfile::tempdir().unwrap();
        if !init_repo(tmp.path()) {
            return;
        }
        let left = settled_render(tmp.path());
        assert!(left.contains("(main)"), "{left:?}");
    }

    #[test]
    fn detached_head_shows_the_short_sha() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) || !git(tmp.path(), &["switch", "--detach", "HEAD"]) {
            return;
        }
        let full = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
        if full.len() < 7 {
            return;
        }
        let left = settled_render(tmp.path());
        assert!(left.contains(&full[..7]), "{left:?}");
    }

    #[test]
    fn linked_worktree_reads_its_own_branch() {
        // Linked worktrees mark the repository with a `.git` *file*; the walk
        // has to accept that form and read the worktree's own HEAD.
        let main = tempfile::tempdir().unwrap();
        if !repo_with_commit(main.path()) {
            return;
        }
        let linked = tempfile::tempdir().unwrap();
        let linked_path = linked.path().join("wt");
        if !git(
            main.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                linked_path.to_str().unwrap(),
                "-b",
                "side",
            ],
        ) {
            return;
        }
        assert!(crate::git_prompt_cache::find_worktree(&linked_path).is_some());
        let left = settled_render(&linked_path);
        assert!(left.contains("(side)"), "{left:?}");
    }

    #[test]
    fn home_directory_abbreviates_to_tilde() {
        let home = std::env::var("USERPROFILE").unwrap();
        let sub = Path::new(&home).join("Docs");
        let shown = display_cwd(&sub);
        assert_eq!(shown, "~/Docs");
    }

    #[test]
    fn drive_path_renders_in_posix_persona_form() {
        let cwd = Path::new("D:\\repo\\niubash");
        assert_eq!(display_cwd(cwd), "/d/repo/niubash");
    }

    #[test]
    fn prompt_is_within_the_render_budget() {
        // 120ms budget (owner red line). Since #685 a render only reads
        // the segment cache, so this catches a regression that puts a
        // spawn or a wait back on the draw path.
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        let _ = settled_segment(tmp.path());
        let start = std::time::Instant::now();
        for _ in 0..10 {
            let _ = render(tmp.path());
        }
        let per_call = start.elapsed() / 10;
        assert!(
            per_call.as_millis() < 120,
            "per-prompt render took {per_call:?}, budget is 120ms"
        );
    }
}
