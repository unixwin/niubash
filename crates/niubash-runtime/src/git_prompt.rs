//! Windows-only built-in "Git Bash compatible" default prompt.
//!
//! Owner adjudication (2026-10-12, niubash 1.4.0 cycle): the factory default
//! prompt on Windows mirrors the Git Bash skeleton users already know —
//! green `user@host`, yellow cwd, cyan branch (+ dirty `*`), `$ ` — with the
//! MINGW64 environment segment deliberately omitted (that is MinGW identity,
//! not ours) and no static product tag (a single-environment shell has no
//! disambiguation need; identity lives in the banner and `niu --version`).
//!
//! The branch/dirty segment reads the repository through git2
//! (owner-approved library dependency, adjudication option d) instead of a
//! git child process: the prompt path never spawns (the niubash#145 red
//! line), and the in-process call lands tens of milliseconds versus Git
//! Bash's per-prompt `__git_ps1` fork — the "smoother than them" deliverable.
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
/// Shape (owner-approved): `green user@host  yellow cwd  cyan branch[*] $ `,
/// segments separated by single spaces, branch segment empty outside a
/// repository. The trailing prompt char is `$ ` (Git Bash's non-root char;
/// niu does not run elevated by design, `PrivilegesRequired=lowest`).
pub fn render(cwd: &Path) -> String {
    let mut left = format!(
        "{GREEN}{}@{}{RESET} {YELLOW}{}{RESET}",
        user_name(),
        host_name(),
        display_cwd(cwd),
    );
    if let Some(branch) = branch_segment(cwd) {
        left.push(' ');
        left.push_str(&format!("{CYAN}{branch}{RESET}"));
    }
    left.push_str(" $ ");
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
/// (detached) — `None` outside a repository. Opens the worktree containing
/// `cwd` (git2 walks parents the way `discover` does), reads HEAD, and only
/// runs the status sweep when a worktree exists (bare repos and detached
/// cores skip it — they have nothing to be dirty against).
///
/// Public so the segment prompt engine can drive it
/// (`SegmentId::Vcs`, niubash#331): the oh-my-niu bundle's
/// `[segments.git] id = "vcs"` declaration resolves to this renderer.
/// In-process git2 only — the segment path must never spawn a git child
/// (niubash#145 red line, niubash#604 lesson). The dirty `*` suffix is the
/// dirty-state marker; the segment engine keys its `[git_dirty]` yellow off
/// exactly that star.
pub fn branch_segment(cwd: &Path) -> Option<String> {
    let repo = git2::Repository::discover(cwd).ok()?;
    let head = repo.head().ok()?;
    let name = if head.is_branch() {
        head.shorthand()?.to_string()
    } else {
        // Detached: Git Bash's __git_ps1 shows the short SHA in parens;
        // keep the bare short SHA (our skeleton, ours to spell).
        let commit = head.peel_to_commit().ok()?;
        let sha = commit.as_object().short_id().ok()?;
        sha.as_str().unwrap_or_default().to_string()
    };
    let dirty = repo_is_dirty(&repo);
    Some(if dirty { format!("{name}*") } else { name })
}

/// Dirty sweep, bounded: untracked-only repositories (the common "just
/// cloned, never built" case) answer from the index/worktree diff without
/// recursing ignore-heavy trees. libgit2 sorts out `.gitignore` natively.
fn repo_is_dirty(repo: &git2::Repository) -> bool {
    let statuses = match repo.statuses(None) {
        Ok(statuses) => statuses,
        Err(_) => return false,
    };
    statuses.iter().any(|entry| {
        let s = entry.status();
        s.intersects(
            git2::Status::WT_MODIFIED
                | git2::Status::WT_DELETED
                | git2::Status::WT_RENAMED
                | git2::Status::WT_NEW
                | git2::Status::INDEX_MODIFIED
                | git2::Status::INDEX_DELETED
                | git2::Status::INDEX_RENAMED
                | git2::Status::INDEX_NEW
                | git2::Status::CONFLICTED,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let mut index = repo.index().unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let left = render(tmp.path());
        let branch = repo.head().unwrap().shorthand().unwrap().to_string();
        assert!(left.contains(&branch), "{left:?}");
        assert!(!left.contains(&format!("{branch}*")), "{left:?}");
    }

    #[test]
    fn dirty_worktree_appends_the_star() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let mut index = repo.index().unwrap();
        let file = tmp.path().join("tracked.txt");
        std::fs::write(&file, b"v1").unwrap();
        index.add_path(Path::new("tracked.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        std::fs::write(&file, b"v2").unwrap();
        let left = render(tmp.path());
        let branch = repo.head().unwrap().shorthand().unwrap().to_string();
        assert!(left.contains(&format!("{branch}*")), "{left:?}");
    }

    #[test]
    fn detached_head_shows_the_short_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let mut index = repo.index().unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        repo.set_head_detached(oid).unwrap();

        let left = render(tmp.path());
        let short = &oid.to_string()[..7];
        assert!(left.contains(short), "{left:?}");
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
        // 120ms budget (owner red line); a warm git2 open+status on a small
        // repo lands in single-digit ms. 10 is generous and still catches a
        // per-prompt subprocess regression, which would blow past it.
        let tmp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let mut index = repo.index().unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

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
