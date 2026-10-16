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
//!  - **cost**: a bounded `git status --porcelain=v2 --branch` (~20-35 ms
//!    with our 14 ms git) happens only inside a repository — outside one the
//!    segment costs a few `stat`s and spawns nothing at all, which is already
//!    better than `__git_ps1`, that forks several times even in `$HOME`;
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
//! prompt*. The child here is scoped to one segment render, is never waited
//! on by anything else, and its reader thread dies with it; the startup
//! regression tests keep asserting that nothing is waited on before the first
//! paint.
//!
//! Segments outside a repository are empty (Git Bash prints no branch there
//! either), and a detached HEAD shows the short SHA the way `__git_ps1`
//! does. This module is a leaf: no shell state, no executor, cwd in/cwd out.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Hard bound for the prompt's git call; the child is killed past it. Two
/// seconds is far above a warm `status` (~20-35 ms) and only reached by a
/// pathological repository (a hook that hangs, a dead network share).
const STATUS_TIMEOUT: Duration = Duration::from_millis(2000);

/// Read budget for the status call. Porcelain v2 prints the `# branch.*`
/// headers first, so the head of the stream already answers the prompt; the
/// cap keeps the pipe and the reader bounded even in a repository with tens
/// of thousands of changed entries.
const STATUS_READ_CAP: usize = 8 * 1024;

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
    let mut left = format!(
        "{GREEN}{}@{}{RESET} {YELLOW}{}{RESET}",
        user_name(),
        host_name(),
        display_cwd(cwd),
    );
    if let Some(branch) = branch_segment(cwd) {
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

/// The working tree containing `cwd`, if any: the nearest ancestor holding a
/// `.git` entry — a directory in an ordinary clone, a file in linked
/// worktrees and submodules. Cheaper than asking git, and outside a
/// repository it is the entire cost of the segment: nothing is spawned.
fn find_worktree(cwd: &Path) -> Option<PathBuf> {
    std::iter::successors(Some(cwd), |dir| dir.parent())
        .find(|dir| {
            let marker = dir.join(".git");
            marker.is_dir() || marker.is_file()
        })
        .map(Path::to_path_buf)
}

/// The git segment of the prompt: `branch` / `branch*` (dirty) / short SHA
/// (detached) — `None` outside a repository, when git is unavailable, or when
/// the read fails or times out. Failures are silent by design: the prompt
/// degrades to "no branch" and the terminal stays clean.
///
/// Public so the segment prompt engine can drive it
/// (`SegmentId::Vcs`, niubash#331): the oh-my-niu bundle's
/// `[segments.git] id = "vcs"` declaration resolves to this renderer. The
/// dirty `*` suffix is the dirty-state marker; the segment engine keys its
/// `[git_dirty]` yellow off exactly that star.
pub fn branch_segment(cwd: &Path) -> Option<String> {
    let worktree = find_worktree(cwd)?;
    let (name, dirty) = read_status(&worktree)?;
    Some(if dirty { format!("{name}*") } else { name })
}

/// One read-only `git status` for the prompt, with every isolation rule from
/// the module docs applied. Returns the branch name (or detached short SHA)
/// plus whether anything is dirty.
fn read_status(worktree: &Path) -> Option<(String, bool)> {
    // stdout goes to a scratch file, not a pipe: a pipe needs either a drain
    // thread (whose hand-off costs a scheduler quantum on Windows, ~15 ms,
    // which would double the segment) or risks wedging the child once it
    // writes past the pipe buffer. The wait below is a precise Win32 wait on
    // the process handle, so nothing polls either.
    let scratch = std::env::temp_dir().join(format!("niu-prompt-git-{}.out", std::process::id()));
    let mut command = Command::new("git");
    command
        // Never start or refresh an fsmonitor daemon from a prompt: those
        // daemons outlive the shell and hold handles on the installation's
        // own binaries.
        .arg("-c")
        .arg("core.fsmonitor=false")
        // GIT_OPTIONAL_LOCKS=0 as a flag: a prompt must never take
        // index.lock or refresh the index (oh-my-zsh documents the same
        // rule for its prompt wrapper).
        .arg("--no-optional-locks")
        .args(["status", "--porcelain=v2", "--branch"])
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Discarded, not inherited: a broken repository, a dubious-ownership
        // refusal or a hook's chatter must never reach the screen.
        .stderr(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    // Deliberately no CREATE_NO_WINDOW: it makes Windows allocate a fresh
    // hidden console for the child, measured at ~28 ms per prompt (21 ms ->
    // 50 ms for the whole call), which doubles the segment. A console child
    // inherits this shell's console and shows no window — and niu is a console
    // program by design (ConPTY and Windows Terminal included), so the flag
    // would only help a console-less parent that cannot run niu anyway.
    // GetConsoleWindow() is not a usable guard here: under ConPTY it reports
    // no window, which would put the cost straight back on the default path.
    let sink = match std::fs::File::create(&scratch) {
        Ok(file) => file,
        Err(_) => return None,
    };
    command.stdout(Stdio::from(sink));
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            let _ = std::fs::remove_file(&scratch);
            return None;
        }
    };
    let finished = wait_with_timeout(&mut child, STATUS_TIMEOUT);
    if !finished {
        let _ = child.kill();
    }
    let _ = child.wait();
    // Bounded read: the branch headers are at the front of the stream, so a
    // capped read still answers even in a repository with tens of thousands
    // of changed entries.
    let bytes = {
        use std::io::Read;
        let mut buffer = Vec::with_capacity(STATUS_READ_CAP);
        if let Ok(file) = std::fs::File::open(&scratch) {
            let _ = file.take(STATUS_READ_CAP as u64).read_to_end(&mut buffer);
        }
        buffer
    };
    let _ = std::fs::remove_file(&scratch);
    if !finished {
        return None;
    }
    parse_status(&bytes)
}

/// Wait for `child` to exit, up to `timeout`. Returns whether it exited by
/// itself; a timeout leaves the child running for the caller to kill.
///
/// Win32 `WaitForSingleObject` wakes the instant the process exits — polling
/// `try_wait` in a sleep loop costs a scheduler quantum (~15 ms) per hop on
/// Windows, which would double the segment's cost.
fn wait_with_timeout(child: &mut std::process::Child, timeout: Duration) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        let handle = child.as_raw_handle() as HANDLE;
        // SAFETY: `handle` is the live process handle owned by `child`.
        let status = unsafe { WaitForSingleObject(handle, timeout.as_millis() as u32) };
        return status == WAIT_OBJECT_0 || status != WAIT_TIMEOUT;
    }
    #[cfg(not(windows))]
    {
        // The prompt surface is Windows-only; keep the module portable.
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }
}

/// Parse `git status --porcelain=v2 --branch` output:
///
/// ```text
/// # branch.oid <oid>|(initial)
/// # branch.head <name>|(detached)
/// 1 <XY> … | 2 … | u … | ? …
/// ```
///
/// The branch headers come first, so a capped read still answers. Any status
/// entry (tracked, renamed/copied, unmerged, untracked) marks the segment
/// dirty — the same set libgit2 was asked for before. A detached HEAD renders
/// the short SHA, and an unborn branch (fresh `git init`, where the previous
/// implementation showed nothing at all because libgit2's `head()` errors)
/// renders its branch name the way Git Bash does.
fn parse_status(bytes: &[u8]) -> Option<(String, bool)> {
    let text = String::from_utf8_lossy(bytes);
    let mut head: Option<&str> = None;
    let mut oid: Option<&str> = None;
    let mut dirty = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# branch.oid ") {
            oid = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("# branch.head ") {
            head = Some(rest.trim());
        } else if line.starts_with("1 ")
            || line.starts_with("2 ")
            || line.starts_with("u ")
            || line.starts_with("? ")
        {
            dirty = true;
        }
    }
    let name = match head? {
        "(detached)" => {
            let oid = oid?;
            if oid == "(initial)" {
                return None;
            }
            oid.chars().take(7).collect()
        }
        name if !name.is_empty() => name.to_string(),
        _ => return None,
    };
    Some((name, dirty))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn no_repository_marker_means_no_worktree() {
        // Outside a repository the segment must not spawn anything: this is
        // the whole cost of the feature in `$HOME` and friends.
        let tmp = tempfile::tempdir().unwrap();
        assert!(find_worktree(tmp.path()).is_none());
    }

    #[test]
    fn clean_repo_shows_branch_without_dirty_mark() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        let left = render(tmp.path());
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
        let left = render(tmp.path());
        assert!(left.contains("(main*)"), "{left:?}");
    }

    #[test]
    fn untracked_file_marks_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        std::fs::write(tmp.path().join("brand-new.txt"), b"x").unwrap();
        let left = render(tmp.path());
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
        let left = render(tmp.path());
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
        let left = render(tmp.path());
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
        assert!(find_worktree(&linked_path).is_some());
        let left = render(&linked_path);
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
    fn parse_status_reads_branch_and_dirt() {
        let clean = b"# branch.oid 1111111111111111111111111111111111111111\n\
                      # branch.head main\n";
        assert_eq!(parse_status(clean), Some(("main".to_string(), false)));

        let dirty = b"# branch.oid 1111111111111111111111111111111111111111\n\
                      # branch.head main\n\
                      1 .M N... 100644 100644 100644 1111111 2222222 tracked.txt\n";
        assert_eq!(parse_status(dirty), Some(("main".to_string(), true)));

        let untracked = b"# branch.oid 1111111111111111111111111111111111111111\n\
                          # branch.head main\n\
                          ? new.txt\n";
        assert_eq!(parse_status(untracked), Some(("main".to_string(), true)));

        let unmerged =
            b"# branch.head side\nu UU N... 100644 100644 100644 100644 1111 2222 3333 c.txt\n";
        assert_eq!(parse_status(unmerged), Some(("side".to_string(), true)));

        let detached = b"# branch.oid 2222222222222222222222222222222222222222\n\
                         # branch.head (detached)\n";
        assert_eq!(parse_status(detached), Some(("2222222".to_string(), false)));

        let unborn = b"# branch.oid (initial)\n# branch.head main\n";
        assert_eq!(parse_status(unborn), Some(("main".to_string(), false)));

        // A capped or unexpected stream answers nothing rather than guessing.
        assert_eq!(parse_status(b""), None);
        assert_eq!(parse_status(b"warning: something\n"), None);
    }

    #[test]
    fn prompt_is_within_the_render_budget() {
        // 120ms budget (owner red line). The segment now spawns the user's
        // git inside a repository (~20-35 ms warm with our own 14 ms git), so
        // this catches a per-prompt regression that would blow the budget
        // (a hanging child, an unbounded status sweep).
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
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
