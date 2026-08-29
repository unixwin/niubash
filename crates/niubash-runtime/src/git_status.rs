//! Git repository status for Niubash prompt segments.
//!
//! Runs `git` sub-processes (read-only, with `GIT_OPTIONAL_LOCKS=0`) to gather
//! branch, dirty-state, staged/unstaged/untracked counts, ahead/behind, stashes,
//! and merge-conflict counts. Prompt rendering never runs git directly: it
//! reads a stable snapshot while a persistent gitstatus helper process keeps
//! status warm for later prompts.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const GIT_TIMEOUT: Duration = if cfg!(debug_assertions) {
    Duration::from_millis(2000)
} else {
    Duration::from_millis(200)
};
const CACHE_TTL: Duration = Duration::from_millis(5000);

#[derive(Debug, Clone)]
pub struct GitStatusSnapshot {
    status: Option<GitRepoStatus>,
    state: GitStatusSnapshotState,
}

impl GitStatusSnapshot {
    pub fn status(&self) -> Option<&GitRepoStatus> {
        self.status.as_ref()
    }

    pub fn into_status(self) -> Option<GitRepoStatus> {
        self.status
    }

    pub fn state(&self) -> GitStatusSnapshotState {
        self.state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatusSnapshotState {
    Fresh,
    Stale,
    Pending,
    None,
}

impl GitStatusSnapshotState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Pending => "pending",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
struct GitStatusCacheEntry {
    status: Option<GitRepoStatus>,
    cached_at: Instant,
    dirty: bool,
}

#[derive(Debug, Clone)]
struct GitStatusRequest {
    cwd: PathBuf,
    generation: u64,
}

#[derive(Debug, Default)]
struct GitStatusWorkerState {
    cache: HashMap<PathBuf, GitStatusCacheEntry>,
    queue: VecDeque<GitStatusRequest>,
    pending: HashSet<PathBuf>,
    in_flight: HashMap<PathBuf, u64>,
    latest_requested: HashMap<PathBuf, u64>,
    next_generation: u64,
    clear_generation: u64,
}

#[derive(Debug)]
struct GitStatusService {
    state: Mutex<GitStatusWorkerState>,
    ready: Condvar,
}

static GIT_STATUS_SERVICE: OnceLock<Arc<GitStatusService>> = OnceLock::new();

fn service() -> Arc<GitStatusService> {
    GIT_STATUS_SERVICE
        .get_or_init(|| {
            let service = Arc::new(GitStatusService {
                state: Mutex::new(GitStatusWorkerState::default()),
                ready: Condvar::new(),
            });
            start_worker(service.clone());
            service
        })
        .clone()
}

/// Aggregated git status for the current working directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRepoStatus {
    pub branch: Option<String>,
    pub dirty: bool,
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub deleted: usize,
    pub ahead: usize,
    pub behind: usize,
    pub stashes: usize,
    pub conflicts: usize,
}

impl GitRepoStatus {
    pub fn compact_status(&self) -> String {
        self.compact_status_with(&GitPromptSymbols::default())
    }

    /// Render compact status using user-configurable symbols. Each
    /// non-empty segment is joined by `separator`. Empty symbol format
    /// strings suppress that segment (oh-my-posh / starship style).
    pub fn compact_status_with(&self, symbols: &GitPromptSymbols) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(p) = symbols.render(&symbols.conflicts, self.conflicts) {
            parts.push(p);
        }
        if let Some(p) = symbols.render(&symbols.staged, self.staged) {
            parts.push(p);
        }
        if let Some(p) = symbols.render(&symbols.unstaged, self.unstaged) {
            parts.push(p);
        }
        if let Some(p) = symbols.render(&symbols.deleted, self.deleted) {
            parts.push(p);
        }
        if let Some(p) = symbols.render(&symbols.ahead, self.ahead) {
            parts.push(p);
        }
        if let Some(p) = symbols.render(&symbols.behind, self.behind) {
            parts.push(p);
        }
        if let Some(p) = symbols.render(&symbols.untracked, self.untracked) {
            parts.push(p);
        }
        if let Some(p) = symbols.render(&symbols.stashes, self.stashes) {
            parts.push(p);
        }
        parts.join(&symbols.separator)
    }
}

/// User-configurable symbols used by `compact_status_with`.
///
/// Each format string supports `{n}` for the count. An empty string
/// suppresses that segment entirely, so users who want a quieter prompt
/// (oh-my-posh / starship style) can blank out individual symbols.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPromptSymbols {
    pub staged: String,
    pub unstaged: String,
    pub untracked: String,
    pub deleted: String,
    pub ahead: String,
    pub behind: String,
    pub stashes: String,
    pub conflicts: String,
    pub separator: String,
}

impl Default for GitPromptSymbols {
    fn default() -> Self {
        Self {
            staged: "●".to_string(),
            unstaged: "✚".to_string(),
            untracked: "?".to_string(),
            deleted: "✖".to_string(),
            ahead: "↑".to_string(),
            behind: "↓".to_string(),
            // The dollar sign collides visually with the prompt indicator, so
            // keep a flag-style stash marker.
            stashes: "⚑".to_string(),
            conflicts: "✖".to_string(),
            separator: " ".to_string(),
        }
    }
}

impl GitPromptSymbols {
    fn render(&self, fmt: &str, n: usize) -> Option<String> {
        if n == 0 || fmt.is_empty() {
            return None;
        }
        Some(fmt.replace("{n}", &n.to_string()))
    }
}

pub fn collect(cwd: &Path) -> Option<GitRepoStatus> {
    let status = collect_uncached(cwd);
    store_snapshot(cwd.to_path_buf(), status.clone(), false);
    status
}

fn collect_uncached(cwd: &Path) -> Option<GitRepoStatus> {
    if !is_likely_git_repo(cwd) {
        return None;
    }
    let status_output = run_git(&["status", "--porcelain", "-b"], cwd, GIT_TIMEOUT, 4 * 1024)?;
    let status_stdout = String::from_utf8(status_output.stdout).ok()?;
    let mut lines = status_stdout.lines();
    let branch_line = lines.next()?;
    let branch = parse_branch_line(branch_line);
    let (ahead0, behind0) = parse_ahead_behind_from_branch_line(branch_line);
    let mut staged = 0usize;
    let mut unstaged = 0usize;
    let mut untracked = 0usize;
    let mut deleted = 0usize;
    let mut conflicts = 0usize;
    for line in lines {
        let line = line.as_bytes();
        if line.is_empty() {
            continue;
        }
        let x = line.first().copied().unwrap_or(b' ');
        let y = line.get(1).copied().unwrap_or(b' ');
        match x {
            b'M' | b'A' | b'R' | b'C' => staged += 1,
            b'D' => {
                staged += 1;
                deleted += 1;
            }
            b'U' => conflicts += 1,
            _ => {}
        }
        match y {
            b'M' => unstaged += 1,
            b'D' => {
                unstaged += 1;
                deleted += 1;
            }
            b'?' => untracked += 1,
            b'U' => conflicts += 1,
            _ => {}
        }
    }
    let dirty = staged > 0 || unstaged > 0 || untracked > 0 || deleted > 0 || conflicts > 0;
    let (ahead, behind) = if ahead0 == 0 && behind0 == 0 && branch.is_some() {
        (
            count_commits(cwd, "@{upstream}..HEAD").unwrap_or(0),
            count_commits(cwd, "HEAD..@{upstream}").unwrap_or(0),
        )
    } else {
        (ahead0, behind0)
    };
    let stashes = count_stashes(cwd);
    Some(GitRepoStatus {
        branch,
        dirty,
        staged,
        unstaged,
        untracked,
        deleted,
        ahead,
        behind,
        stashes,
        conflicts,
    })
}

/// Non-blocking snapshot for the prompt render path.
///
/// Returns the cached value immediately if one exists for this cwd. If the
/// value is missing, stale, or marked dirty, it schedules the long-running git
/// work on the persistent worker. Prompt rendering never starts git, never
/// waits for git, and never receives a callback that repaints the active line.
pub fn snapshot_for_prompt(cwd: &Path) -> GitStatusSnapshot {
    let cwd = cwd.to_path_buf();
    let svc = service();
    let mut state = svc.state.lock().unwrap();
    let now = Instant::now();
    if let Some(entry) = state.cache.get(&cwd).cloned() {
        let stale = entry.dirty || now.duration_since(entry.cached_at) >= CACHE_TTL;
        if stale {
            enqueue_refresh_locked(&svc, &mut state, cwd, false);
        }
        return GitStatusSnapshot {
            status: entry.status,
            state: if stale {
                GitStatusSnapshotState::Stale
            } else {
                GitStatusSnapshotState::Fresh
            },
        };
    }

    enqueue_refresh_locked(&svc, &mut state, cwd, false);
    GitStatusSnapshot {
        status: None,
        state: GitStatusSnapshotState::Pending,
    }
}

pub fn collect_for_prompt(cwd: &Path) -> Option<GitRepoStatus> {
    snapshot_for_prompt(cwd).into_status()
}

/// Mark one cwd as likely changed and schedule a refresh. The current prompt
/// keeps using the previous stable snapshot until the worker publishes a new
/// one.
pub fn mark_dirty(cwd: &Path) {
    let cwd = cwd.to_path_buf();
    let svc = service();
    let mut state = svc.state.lock().unwrap();
    if let Some(entry) = state.cache.get_mut(&cwd) {
        entry.dirty = true;
    }
    enqueue_refresh_locked(&svc, &mut state, cwd, true);
}

pub fn request_refresh(cwd: &Path) {
    let cwd = cwd.to_path_buf();
    let svc = service();
    let mut state = svc.state.lock().unwrap();
    enqueue_refresh_locked(&svc, &mut state, cwd, false);
}

/// Clear the cached git status (e.g., when a hook or filesystem event might have changed the work-tree).
pub fn clear_cache() {
    let svc = service();
    let mut state = svc.state.lock().unwrap();
    state.cache.clear();
    state.queue.clear();
    state.pending.clear();
    state.in_flight.clear();
    state.latest_requested.clear();
    state.next_generation = state.next_generation.saturating_add(1);
    state.clear_generation = state.next_generation;
}

fn store_snapshot(cwd: PathBuf, status: Option<GitRepoStatus>, dirty: bool) {
    let svc = service();
    let mut state = svc.state.lock().unwrap();
    state.next_generation = state.next_generation.saturating_add(1);
    state.cache.insert(
        cwd,
        GitStatusCacheEntry {
            status,
            cached_at: Instant::now(),
            dirty,
        },
    );
}

fn enqueue_refresh_locked(
    service: &GitStatusService,
    state: &mut GitStatusWorkerState,
    cwd: PathBuf,
    force_dirty: bool,
) {
    state.next_generation = state.next_generation.saturating_add(1);
    let generation = state.next_generation;
    state.latest_requested.insert(cwd.clone(), generation);
    if force_dirty {
        if let Some(entry) = state.cache.get_mut(&cwd) {
            entry.dirty = true;
        }
    }
    if state.pending.contains(&cwd) || state.in_flight.contains_key(&cwd) {
        return;
    }
    state.pending.insert(cwd.clone());
    state.queue.push_back(GitStatusRequest { cwd, generation });
    service.ready.notify_one();
}

fn start_worker(service: Arc<GitStatusService>) {
    let _ = std::thread::Builder::new()
        .name("niubash-gitstatus".to_string())
        .spawn(move || {
            let mut daemon = GitStatusDaemonClient::spawn();
            loop {
                let request = {
                    let mut state = service.state.lock().unwrap();
                    loop {
                        if let Some(request) = state.queue.pop_front() {
                            state.pending.remove(&request.cwd);
                            state
                                .in_flight
                                .insert(request.cwd.clone(), request.generation);
                            break request;
                        }
                        state = service.ready.wait(state).unwrap();
                    }
                };

                let status = match daemon
                    .as_mut()
                    .and_then(|client| client.collect(&request.cwd))
                {
                    Some(status) => status,
                    None => {
                        daemon = GitStatusDaemonClient::spawn();
                        daemon
                            .as_mut()
                            .and_then(|client| client.collect(&request.cwd))
                            .unwrap_or_else(|| collect_uncached(&request.cwd))
                    }
                };

                let mut state = service.state.lock().unwrap();
                if request.generation < state.clear_generation {
                    state.in_flight.remove(&request.cwd);
                    continue;
                }
                state.in_flight.remove(&request.cwd);
                let latest = state
                    .latest_requested
                    .get(&request.cwd)
                    .copied()
                    .unwrap_or(request.generation);
                let needs_rerun = latest > request.generation;
                state.cache.insert(
                    request.cwd.clone(),
                    GitStatusCacheEntry {
                        status,
                        cached_at: Instant::now(),
                        dirty: needs_rerun,
                    },
                );
                if needs_rerun && !state.pending.contains(&request.cwd) {
                    state.pending.insert(request.cwd.clone());
                    state.queue.push_back(GitStatusRequest {
                        cwd: request.cwd.clone(),
                        generation: latest,
                    });
                    service.ready.notify_one();
                }
            }
        });
}

#[derive(Debug, Serialize, Deserialize)]
struct GitStatusDaemonRequest {
    id: u64,
    cwd: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GitStatusDaemonResponse {
    id: u64,
    status: Option<GitRepoStatus>,
}

struct GitStatusDaemonClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl GitStatusDaemonClient {
    fn spawn() -> Option<Self> {
        if std::env::var_os("NIU_GITSTATUS_DAEMON_DISABLED").is_some() {
            return None;
        }
        let exe = std::env::current_exe().ok()?;
        let mut child = Command::new(exe)
            .arg("--gitstatus-daemon")
            .env("NIU_GITSTATUS_DAEMON_CHILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        Some(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
        })
    }

    fn collect(&mut self, cwd: &Path) -> Option<Option<GitRepoStatus>> {
        self.next_id = self.next_id.saturating_add(1);
        let request = GitStatusDaemonRequest {
            id: self.next_id,
            cwd: cwd.to_string_lossy().to_string(),
        };
        serde_json::to_writer(&mut self.stdin, &request).ok()?;
        self.stdin.write_all(b"\n").ok()?;
        self.stdin.flush().ok()?;

        let mut line = String::new();
        if self.stdout.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let response: GitStatusDaemonResponse = serde_json::from_str(&line).ok()?;
        (response.id == request.id).then_some(response.status)
    }
}

impl Drop for GitStatusDaemonClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn run_daemon_stdio() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_daemon_protocol(stdin.lock(), stdout.lock())
}

fn run_daemon_protocol<R, W>(reader: R, mut writer: W) -> anyhow::Result<()>
where
    R: std::io::Read,
    W: Write,
{
    let reader = BufReader::new(reader);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: GitStatusDaemonRequest = serde_json::from_str(&line)?;
        let status = collect_uncached(Path::new(&request.cwd));
        let response = GitStatusDaemonResponse {
            id: request.id,
            status,
        };
        serde_json::to_writer(&mut writer, &response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}

fn is_likely_git_repo(mut dir: &Path) -> bool {
    loop {
        let git = dir.join(".git");
        if git.is_dir() || git.is_file() {
            return true;
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => return false,
        }
    }
}

fn run_git(args: &[&str], cwd: &Path, timeout: Duration, max_stdout: usize) -> Option<Output> {
    let cwd = cwd.to_owned();
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    let _handle = std::thread::spawn(move || {
        let result = Command::new("git")
            .args(&args)
            .current_dir(&cwd)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        let _ = tx.send(result);
    });
    let output = match rx.recv_timeout(timeout) {
        Ok(Ok(o)) => o,
        _ => return None, // timed out, git hung, or sender dropped
    };
    if !output.status.success() || output.stdout.len() > max_stdout {
        None
    } else {
        Some(output)
    }
}

fn parse_branch_line(line: &str) -> Option<String> {
    let r = line.trim().strip_prefix("## ")?;
    if r.starts_with("HEAD ") || r.contains("(no branch)") {
        return r
            .split_whitespace()
            .find(|w| w.len() >= 7 && w.chars().all(|c| c.is_ascii_hexdigit()))
            .map(|h| h.chars().take(7).collect());
    }
    // Empty initial repo: `## No commits yet on master`
    if let Some(rest) = r.strip_prefix("No commits yet on ") {
        return Some(rest.trim().to_string());
    }
    Some(r.split("...").next().unwrap_or(r).to_string())
}

fn parse_ahead_behind_from_branch_line(line: &str) -> (usize, usize) {
    if let Some(b) = line.find('[') {
        let inner = line[b + 1..].split(']').next().unwrap_or("");
        let mut a = 0usize;
        let mut b = 0usize;
        for p in inner.split(',') {
            let p = p.trim();
            if let Some(n) = p.strip_prefix("ahead ") {
                a = n.trim().parse().unwrap_or(0);
            } else if let Some(n) = p.strip_prefix("behind ") {
                b = n.trim().parse().unwrap_or(0);
            }
        }
        return (a, b);
    }
    (0, 0)
}

fn count_commits(cwd: &Path, range: &str) -> Option<usize> {
    let output = run_git(&["rev-list", "--count", range], cwd, GIT_TIMEOUT, 1024)?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

fn count_stashes(cwd: &Path) -> usize {
    if run_git(
        &["rev-parse", "--verify", "--quiet", "refs/stash"],
        cwd,
        GIT_TIMEOUT,
        128,
    )
    .is_none()
    {
        return 0;
    }
    match run_git(&["stash", "list"], cwd, GIT_TIMEOUT, 8192) {
        Some(o) => String::from_utf8(o.stdout)
            .unwrap_or_default()
            .lines()
            .count(),
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn init_temp_repo() -> std::path::PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_owned();
        for args in [
            &["init"][..],
            &["config", "user.email", "test@niubash"],
            &["config", "user.name", "Niubash Test"],
        ] {
            let o = Command::new("git")
                .args(args)
                .current_dir(&p)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .unwrap();
            assert!(o.status.success());
        }
        std::mem::forget(dir);
        p
    }

    #[test]
    fn git_status_outside_repo_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(collect(dir.path()).is_none());
    }

    #[test]
    fn git_status_empty_repo_shows_master_branch() {
        let dir = init_temp_repo();
        let s = collect(&dir).expect("git repo");
        assert!(matches!(s.branch.as_deref(), Some("master") | Some("main")));
        assert!(!s.dirty);
    }

    #[test]
    fn git_status_detects_dirty_working_tree() {
        let dir = init_temp_repo();
        let mut f = std::fs::File::create(dir.join("new.txt")).unwrap();
        writeln!(f, "hello").unwrap();
        drop(f);
        let s = collect(&dir).expect("git repo");
        assert!(s.dirty);
        assert_eq!(s.untracked, 1);
    }

    #[test]
    fn git_status_detects_staged_changes() {
        let dir = init_temp_repo();
        std::fs::write(dir.join("s.txt"), b"x").unwrap();
        let o = Command::new("git")
            .args(["add", "s.txt"])
            .current_dir(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(o.status.success());
        let s = collect(&dir).expect("git repo");
        assert!(s.dirty);
        assert_eq!(s.staged, 1);
    }

    #[test]
    fn prompt_snapshot_cold_path_returns_pending_without_git_result() {
        let dir = init_temp_repo();
        clear_cache();

        let snapshot = snapshot_for_prompt(&dir);

        assert_eq!(snapshot.state(), GitStatusSnapshotState::Pending);
        assert!(snapshot.status().is_none());
    }

    #[test]
    fn prompt_snapshot_reads_preheated_cache_without_refreshing_git_inline() {
        let dir = init_temp_repo();
        clear_cache();
        let warmed = collect(&dir).expect("git repo");

        let snapshot = snapshot_for_prompt(&dir);

        assert_eq!(snapshot.state(), GitStatusSnapshotState::Fresh);
        assert_eq!(snapshot.status(), Some(&warmed));
    }

    #[test]
    fn dirty_snapshot_keeps_previous_status_for_prompt() {
        let dir = init_temp_repo();
        clear_cache();
        let warmed = collect(&dir).expect("git repo");

        mark_dirty(&dir);
        let snapshot = snapshot_for_prompt(&dir);

        assert!(matches!(
            snapshot.state(),
            GitStatusSnapshotState::Fresh | GitStatusSnapshotState::Stale
        ));
        assert_eq!(snapshot.status(), Some(&warmed));
    }

    #[test]
    fn git_status_compact_format() {
        let s = GitRepoStatus {
            branch: Some("main".into()),
            dirty: true,
            staged: 2,
            unstaged: 1,
            untracked: 3,
            deleted: 1,
            ahead: 1,
            behind: 2,
            stashes: 1,
            conflicts: 0,
        };
        let c = s.compact_status();
        // Default is boolean format (no {n}); symbols repeat based on count.
        // staged=2 → "●●", unstaged=1 → "✚", untracked=3 → "???", etc.
        assert!(c.contains("●")); // staged symbol
        assert!(c.contains("✚")); // unstaged symbol
        assert!(c.contains("?")); // untracked literal
        assert!(c.contains("↑")); // ahead
        assert!(c.contains("↓")); // behind
        assert!(c.contains("⚑")); // stashes
    }

    #[test]
    fn git_status_clean_compact_empty() {
        let s = GitRepoStatus {
            branch: Some("main".into()),
            dirty: false,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            deleted: 0,
            ahead: 0,
            behind: 0,
            stashes: 0,
            conflicts: 0,
        };
        assert!(s.compact_status().is_empty());
    }

    #[test]
    fn git_status_compact_format_with_custom_symbols() {
        let mut symbols = GitPromptSymbols::default();
        symbols.staged = "+{n}".to_string();
        symbols.unstaged = "*{n}".to_string();
        symbols.untracked = "?{n}".to_string();
        symbols.ahead = "u{n}".to_string();
        symbols.behind = "d{n}".to_string();
        symbols.stashes = "s{n}".to_string();

        let s = GitRepoStatus {
            branch: Some("main".into()),
            dirty: true,
            staged: 2,
            unstaged: 1,
            untracked: 3,
            deleted: 0,
            ahead: 1,
            behind: 2,
            stashes: 1,
            conflicts: 0,
        };
        let c = s.compact_status_with(&symbols);
        assert!(c.contains("+2"));
        assert!(c.contains("*1"));
        assert!(c.contains("u1"));
        assert!(c.contains("d2"));
        assert!(c.contains("?3"));
        assert!(c.contains("s1"));
    }

    #[test]
    fn git_status_compact_with_empty_symbols_hides_segments() {
        let mut symbols = GitPromptSymbols::default();
        // Quiet style: hide staged/unstaged/untracked/stashes; keep only ahead/behind.
        // ahead/behind keep the count form so the test sees the count.
        symbols.staged = String::new();
        symbols.unstaged = String::new();
        symbols.untracked = String::new();
        symbols.stashes = String::new();
        symbols.ahead = "↑{n}".to_string();
        symbols.behind = "↓{n}".to_string();

        let s = GitRepoStatus {
            branch: Some("main".into()),
            dirty: true,
            staged: 5,
            unstaged: 3,
            untracked: 2,
            deleted: 0,
            ahead: 1,
            behind: 0,
            stashes: 7,
            conflicts: 0,
        };
        let c = s.compact_status_with(&symbols);
        assert!(!c.contains("●"));
        assert!(!c.contains("✚"));
        assert!(!c.contains("?"));
        assert!(!c.contains("⚑"));
        assert!(c.contains("↑1"));
    }
}
