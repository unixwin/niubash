//! Platform-independent per-repository cache for the VCS prompt segment
//! (niubash#592).
//!
//! The Windows factory prompt's git segment used to run one bounded
//! `git status` on *every* repaint — resizes, completion redraws and other
//! state-irrelevant repaints all paid a full spawn, which on a real Windows
//! machine is the 100 ms class, not the few-ms class a Linux box suggests.
//! This module is the "when to compute" half of the fix; the "how to ask
//! git" half (one bounded, isolated `git status --porcelain=v2 --branch`,
//! every isolation rule intact) stays in `git_prompt::read_status` and is
//! injected here as the compute function. Fidelity is unchanged — the
//! user's own git still adjudicates every state — only the schedule moves:
//!
//!  - **the drawing path never spawns and never waits.** `segment()` reads
//!    the cache and returns the last rendered value (or `None` before the
//!    first fill). A stale or missing entry schedules exactly one
//!    asynchronous refill on a host-owned worker thread and the possibly
//!    stale value is what gets painted (stale-while-revalidate, the
//!    powerlevel10k/gitstatus shape).
//!  - **one spawn per repository per command cycle at most.** A fill is
//!    scheduled only when the entry is stale: its fingerprint moved
//!    (`.git/HEAD` content or `.git/index` mtime/size) or a command cycle
//!    completed since it was computed (`note_command_cycle`, called once
//!    per prompt cycle by the host — commands can edit tracked files
//!    without touching HEAD or the index, so the cycle mark is the only
//!    honest catch-all). Repaint storms inside one cycle all read the
//!    cache; in-flight refills are de-duplicated per repository.
//!  - **outside a repository, nothing is scheduled at all** — the worktree
//!    walk below is the segment's entire cost there, as before.
//!
//! This is deliberately not the niubash#145 shape: that design parked a
//! worker the main thread *waited on before the first paint*. Here the
//! main line never waits for the worker — not for the first paint, not
//! ever; the worker writes the cache and the value surfaces at the next
//! repaint through the prompt's existing re-render channel (the segment
//! engine re-renders on every reedline repaint, the gitbash prompt on the
//! next cycle). A change made by an external process between commands is
//! therefore visible at most one command cycle late — the documented
//! bound, in exchange for a prompt that can never block on git.
//!
//! The worker thread is created lazily on the first schedule and lives
//! inside the cache instance: dropping the instance closes the job
//! channel and the thread exits; the process-global instance ends with
//! the process. The child git processes it runs stay bounded by
//! `read_status`'s own timeout, so nothing outlives the session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock, Weak};

/// The compute half: given a worktree root, produce the rendered segment
/// (`branch` / `branch*` / short SHA), or `None` when git cannot answer.
/// Production injects `git_prompt`'s bounded read; tests inject fakes.
pub type SegmentCompute = Arc<dyn Fn(&Path) -> Option<String> + Send + Sync>;

/// Upper bound on remembered repositories. A session that wanders through
/// more worktrees than this evicts cold entries (never one in flight).
const MAX_ENTRIES: usize = 128;

// ---- Repository layout (pure filesystem, no spawn) ----

/// The working tree containing `cwd`, if any: the nearest ancestor holding
/// a `.git` entry — a directory in an ordinary clone, a file in linked
/// worktrees and submodules. Cheaper than asking git, and outside a
/// repository it is the entire cost of the segment: nothing is spawned.
pub fn find_worktree(cwd: &Path) -> Option<PathBuf> {
    std::iter::successors(Some(cwd), |dir| dir.parent())
        .find(|dir| {
            let marker = dir.join(".git");
            marker.is_dir() || marker.is_file()
        })
        .map(Path::to_path_buf)
}

/// The resolved git directories of a worktree, following git's own layout
/// rules: `.git` may be a directory (ordinary clone) or a file containing
/// `gitdir: <path>` (linked worktrees, submodules); the gitdir may in turn
/// hold a `commondir` file naming the shared directory (linked worktrees
/// keep their per-worktree HEAD and index in the gitdir, shared state in
/// the common dir).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDirs {
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

/// Resolve a path read from a `gitdir:`/`commondir` file: absolute paths
/// pass through, relative ones resolve against `base`.
fn resolve_link(base: &Path, raw: &str) -> PathBuf {
    let candidate = PathBuf::from(raw.trim());
    if candidate.is_absolute() {
        candidate
    } else {
        base.join(candidate)
    }
}

/// Read a `<name>: <path>` pointer file (`.git` itself, or `commondir`).
fn read_pointer(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let first = text.lines().next()?;
    if key.is_empty() {
        Some(first.trim().to_string())
    } else {
        first
            .strip_prefix(key)
            .map(|rest| rest.trim().to_string())
    }
}

pub fn resolve_git_dirs(worktree: &Path) -> Option<GitDirs> {
    let marker = worktree.join(".git");
    let git_dir = if marker.is_dir() {
        marker
    } else if marker.is_file() {
        let target = read_pointer(&marker, "gitdir:")?;
        resolve_link(worktree, &target)
    } else {
        return None;
    };
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => {
            let raw = text.lines().next().unwrap_or("").trim();
            if raw.is_empty() {
                git_dir.clone()
            } else {
                resolve_link(&git_dir, raw)
            }
        }
        Err(_) => git_dir.clone(),
    };
    Some(GitDirs {
        git_dir,
        common_dir,
    })
}

// ---- Invalidation fingerprint ----

/// What changed since the last fill, in the cheapest checkable form: the
/// content of the per-worktree `HEAD` (branch switches and detach moves
/// rewrite it) plus the index's mtime and size (`git add` and friends
/// rewrite it). Tracked-file edits move neither — those are caught by the
/// command-cycle mark, which is why both halves exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFingerprint {
    head: String,
    index: Option<(std::time::SystemTime, u64)>,
}

pub fn fingerprint(worktree: &Path) -> Option<RepoFingerprint> {
    let dirs = resolve_git_dirs(worktree)?;
    let head = std::fs::read_to_string(dirs.git_dir.join("HEAD"))
        .map(|text| text.trim().to_string())
        .unwrap_or_default();
    let index = std::fs::metadata(dirs.git_dir.join("index"))
        .ok()
        .and_then(|meta| meta.modified().ok().map(|mtime| (mtime, meta.len())));
    Some(RepoFingerprint { head, index })
}

// ---- Status parsing (shared with git_prompt) ----

/// Parse `git status --porcelain=v2 --branch` output:
///
/// ```text
/// # branch.oid <oid>|(initial)
/// # branch.head <name>|(detached)
/// 1 <XY> … | 2 … | u … | ? …
/// ```
///
/// The branch headers come first, so a capped read still answers. Any
/// status entry (tracked, renamed/copied, unmerged, untracked) marks the
/// segment dirty. A detached HEAD renders the short SHA, and an unborn
/// branch renders its branch name the way Git Bash does.
pub fn parse_status(bytes: &[u8]) -> Option<(String, bool)> {
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

/// Render a parsed status the way the segment shows it: `branch`, with
/// the dirty star appended when anything is dirty.
pub fn render_segment(name: &str, dirty: bool) -> String {
    if dirty {
        format!("{name}*")
    } else {
        name.to_string()
    }
}

// ---- Cache core ----

#[derive(Debug)]
struct Entry {
    /// The last rendered segment; meaningful only when `has_value`.
    value: Option<String>,
    /// A fill has completed at least once (its `value`, possibly `None`
    /// for "git cannot answer here", is a real answer, not a placeholder).
    has_value: bool,
    /// Fingerprint sampled when the last fill completed.
    fingerprint: Option<RepoFingerprint>,
    /// Command cycle in which the last fill was scheduled.
    cycle: u64,
    in_flight: bool,
    /// The (fingerprint, cycle) pair the in-flight/last fill was scheduled
    /// for — a repaint with the same pair never schedules a second spawn.
    scheduled_for: Option<(Option<RepoFingerprint>, u64)>,
}

impl Entry {
    fn placeholder() -> Self {
        Self {
            value: None,
            has_value: false,
            fingerprint: None,
            cycle: 0,
            in_flight: false,
            scheduled_for: None,
        }
    }
}

struct Job {
    key: PathBuf,
    worktree: PathBuf,
    cycle: u64,
}

struct Inner {
    compute: SegmentCompute,
    entries: Mutex<HashMap<PathBuf, Entry>>,
    cycle: AtomicU64,
    fills: AtomicU64,
    sender: Mutex<Option<mpsc::Sender<Job>>>,
}

/// A per-repository segment cache with one async worker thread. Cheap to
/// consult from the drawing path: two small file reads (HEAD + index stat)
/// and a short lock, never a spawn, never a wait.
pub struct VcsCache {
    inner: Arc<Inner>,
}

impl VcsCache {
    pub fn new(compute: SegmentCompute) -> Self {
        Self {
            inner: Arc::new(Inner {
                compute,
                entries: Mutex::new(HashMap::new()),
                cycle: AtomicU64::new(0),
                fills: AtomicU64::new(0),
                sender: Mutex::new(None),
            }),
        }
    }

    /// The segment for `cwd`: the cached value when fresh, the stale value
    /// (or `None`) while a refill is scheduled or in flight, and `None`
    /// outside a repository. Never spawns, never blocks on git.
    pub fn segment(&self, cwd: &Path) -> Option<String> {
        let worktree = find_worktree(cwd)?;
        let key = std::fs::canonicalize(&worktree).unwrap_or_else(|_| worktree.clone());
        let fp = fingerprint(&worktree);
        let cycle = self.inner.cycle.load(Ordering::SeqCst);
        let mut job: Option<Job> = None;
        let result = {
            let mut entries = self.inner.entries.lock().unwrap_or_else(|e| e.into_inner());
            evict_locked(&mut entries, &key);
            match entries.get_mut(&key) {
                Some(entry)
                    if entry.has_value && entry.fingerprint == fp && entry.cycle == cycle =>
                {
                    entry.value.clone()
                }
                Some(entry) => {
                    if !entry.in_flight && entry.scheduled_for != Some((fp.clone(), cycle)) {
                        entry.in_flight = true;
                        entry.scheduled_for = Some((fp.clone(), cycle));
                        job = Some(Job {
                            key: key.clone(),
                            worktree: worktree.clone(),
                            cycle,
                        });
                    }
                    entry.value.clone()
                }
                None => {
                    let mut entry = Entry::placeholder();
                    entry.in_flight = true;
                    entry.scheduled_for = Some((fp.clone(), cycle));
                    entries.insert(key.clone(), entry);
                    job = Some(Job {
                        key: key.clone(),
                        worktree: worktree.clone(),
                        cycle,
                    });
                    None
                }
            }
        };
        if let Some(job) = job {
            self.dispatch(job);
        }
        result
    }

    /// Mark one completed command cycle: every cached repository becomes
    /// stale for scheduling purposes (at most one refill each), because a
    /// command may have edited tracked files without moving HEAD or the
    /// index. Called once per prompt cycle by the host.
    pub fn note_command_cycle(&self) {
        self.inner.cycle.fetch_add(1, Ordering::SeqCst);
    }

    /// The current command-cycle counter (diagnostics/tests).
    pub fn cycle(&self) -> u64 {
        self.inner.cycle.load(Ordering::SeqCst)
    }

    /// How many fills the worker has completed (diagnostics/tests — this
    /// is the spawn counter the #592 budget gates key on: one fill is one
    /// bounded `git status` in production).
    pub fn fill_count(&self) -> u64 {
        self.inner.fills.load(Ordering::SeqCst)
    }

    /// Hand a job to the worker, starting the thread on first use. The
    /// caller never waits: `mpsc::Sender::send` on an unbounded channel
    /// is a queue push, not a rendezvous.
    fn dispatch(&self, job: Job) {
        let sender = {
            let mut slot = self.inner.sender.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_none() {
                let (tx, rx) = mpsc::channel::<Job>();
                let weak = Arc::downgrade(&self.inner);
                let spawned = std::thread::Builder::new()
                    .name("niu-vcs-cache".to_string())
                    .spawn(move || worker_loop(weak, rx));
                match spawned {
                    Ok(_) => *slot = Some(tx),
                    Err(_) => {
                        // No worker can ever run: un-wedge the entry so a
                        // later repaint can retry instead of believing a
                        // fill is coming.
                        let mut entries =
                            self.inner.entries.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(entry) = entries.get_mut(&job.key) {
                            entry.in_flight = false;
                            entry.scheduled_for = None;
                        }
                        return;
                    }
                }
            }
            slot.as_ref().expect("sender installed above").clone()
        };
        if let Err(err) = sender.send(job) {
            // The worker is gone (only possible while the cache itself is
            // being torn down): same un-wedge as above, for this job's
            // entry only.
            let key = err.0.key;
            let mut entries = self.inner.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = entries.get_mut(&key) {
                entry.in_flight = false;
                entry.scheduled_for = None;
            }
        }
    }
}

/// Drop cold entries when the map grows past `MAX_ENTRIES`; the entry for
/// `keep` and any in-flight entry are never evicted.
fn evict_locked(entries: &mut HashMap<PathBuf, Entry>, keep: &Path) {
    while entries.len() > MAX_ENTRIES {
        let victim = entries
            .iter()
            .find(|(key, entry)| key.as_path() != keep && !entry.in_flight)
            .map(|(key, _)| key.clone());
        match victim {
            Some(key) => {
                entries.remove(&key);
            }
            None => break,
        }
    }
}

fn worker_loop(weak: Weak<Inner>, rx: mpsc::Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let value = (inner.compute)(&job.worktree);
        // Sample the fingerprint *after* the read: the stored pair then
        // describes the tree the value was computed from as closely as a
        // lock-free read allows; a change during the read simply fails the
        // next freshness check and schedules one more fill.
        let fp = fingerprint(&job.worktree);
        {
            let mut entries = inner.entries.lock().unwrap_or_else(|e| e.into_inner());
            let entry = entries.entry(job.key.clone()).or_insert_with(Entry::placeholder);
            entry.value = value;
            entry.has_value = true;
            entry.fingerprint = fp;
            entry.cycle = job.cycle;
            entry.in_flight = false;
            entry.scheduled_for = None;
        }
        inner.fills.fetch_add(1, Ordering::SeqCst);
    }
}

// ---- Process-global instance (the host's one cache) ----

static GLOBAL: OnceLock<VcsCache> = OnceLock::new();

/// Install (once) and return the process-global cache. The first caller
/// provides the compute function; production's only caller is
/// `git_prompt::branch_segment`, which always injects the same bounded
/// read, so the "first wins" rule is not a configuration surface.
pub fn install_global(compute: SegmentCompute) -> &'static VcsCache {
    GLOBAL.get_or_init(|| VcsCache::new(compute))
}

/// The installed global cache, if any.
pub fn global() -> Option<&'static VcsCache> {
    GLOBAL.get()
}

/// Host hook: one completed command cycle. A no-op until the cache is
/// installed (nothing is cached yet, so nothing can be stale).
pub fn note_command_cycle() {
    if let Some(cache) = GLOBAL.get() {
        cache.note_command_cycle();
    }
}

/// Fills completed by the global cache (0 before installation); the
/// diagnostics counter the prompt budget tests assert on.
pub fn global_fill_count() -> u64 {
    GLOBAL.get().map_or(0, VcsCache::fill_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

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

    /// The synchronous reference read — production's shape (one bounded
    /// `git status`, parsed) minus the scratch file and console handling.
    /// The equivalence matrix keys on this: whatever the cache settles to
    /// must equal what a direct read answers, byte for byte.
    fn reference_segment(worktree: &Path) -> Option<String> {
        reference_segment_env(worktree, &[])
    }

    fn reference_segment_env(worktree: &Path, env: &[(&str, &Path)]) -> Option<String> {
        let mut command = Command::new("git");
        command
            .arg("-c")
            .arg("core.fsmonitor=false")
            .arg("--no-optional-locks")
            .args(["status", "--porcelain=v2", "--branch"])
            .current_dir(worktree)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0");
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().ok()?;
        parse_status(&output.stdout).map(|(name, dirty)| render_segment(&name, dirty))
    }

    fn real_compute() -> SegmentCompute {
        Arc::new(|worktree: &Path| reference_segment(worktree))
    }

    /// Poll `segment()` until it equals `expected` *and* at least one fill
    /// has completed (a cold `None` must not satisfy a `None` expectation
    /// before the worker even ran), or the deadline passes.
    fn wait_for_segment(
        cache: &VcsCache,
        cwd: &Path,
        expected: &Option<String>,
        timeout: Duration,
    ) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let got = cache.segment(cwd);
            if &got == expected && cache.fill_count() >= 1 {
                return got;
            }
            if Instant::now() >= deadline {
                return got;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
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
    fn find_worktree_walks_up_and_accepts_git_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(find_worktree(tmp.path()).is_none());
        if !repo_with_commit(tmp.path()) {
            return;
        }
        let nested = tmp.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(find_worktree(&nested), Some(tmp.path().to_path_buf()));

        // A `.git` *file* (linked worktree / submodule shape) also marks a
        // worktree root.
        let fake = tempfile::tempdir().unwrap();
        std::fs::write(fake.path().join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(find_worktree(fake.path()), Some(fake.path().to_path_buf()));
    }

    #[test]
    fn resolve_git_dirs_plain_repo() {
        let tmp = tempfile::tempdir().unwrap();
        if !init_repo(tmp.path()) {
            return;
        }
        let dirs = resolve_git_dirs(tmp.path()).unwrap();
        assert_eq!(dirs.git_dir, tmp.path().join(".git"));
        assert_eq!(dirs.common_dir, dirs.git_dir);
    }

    #[test]
    fn resolve_git_dirs_linked_worktree_follows_gitdir_and_commondir() {
        let main = tempfile::tempdir().unwrap();
        if !repo_with_commit(main.path()) {
            return;
        }
        let linked_parent = tempfile::tempdir().unwrap();
        let linked_path = linked_parent.path().join("wt");
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
        let dirs = resolve_git_dirs(&linked_path).unwrap();
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        assert_eq!(
            canon(&dirs.git_dir),
            canon(&main.path().join(".git/worktrees/wt"))
        );
        assert_eq!(canon(&dirs.common_dir), canon(&main.path().join(".git")));
        // Per-worktree state lives in the gitdir, not the common dir.
        assert!(dirs.git_dir.join("HEAD").is_file());
    }

    #[test]
    fn fingerprint_tracks_head_and_index() {
        let tmp = tempfile::tempdir().unwrap();
        if !init_repo(tmp.path()) {
            return;
        }
        let before = fingerprint(tmp.path()).unwrap();
        assert_eq!(before, fingerprint(tmp.path()).unwrap(), "reads are stable");

        // `git add` creates/rewrites the index.
        std::fs::write(tmp.path().join("f.txt"), b"x").unwrap();
        assert!(git(tmp.path(), &["add", "f.txt"]));
        let after_add = fingerprint(tmp.path()).unwrap();
        assert_ne!(before, after_add, "index move must move the fingerprint");

        // A branch switch rewrites HEAD's content.
        assert!(git(tmp.path(), &["switch", "-c", "other"]));
        let after_switch = fingerprint(tmp.path()).unwrap();
        assert_ne!(after_add, after_switch, "HEAD move must move the fingerprint");
    }

    #[test]
    fn redraws_within_one_cycle_spawn_once_and_outside_never() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let cache = VcsCache::new(Arc::new(move |worktree: &Path| {
            counter.fetch_add(1, Ordering::SeqCst);
            reference_segment(worktree)
        }));

        // Cold: the first look schedules the one fill of this cycle and
        // answers nothing yet (the drawing path never waits).
        assert_eq!(cache.segment(tmp.path()), None);
        for _ in 0..20 {
            let _ = cache.segment(tmp.path());
        }
        let settled = wait_for_segment(
            &cache,
            tmp.path(),
            &Some("main".to_string()),
            Duration::from_secs(10),
        );
        assert_eq!(settled, Some("main".to_string()));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "20 redraws, one spawn");
        assert_eq!(cache.fill_count(), 1);

        // Same cycle, warm: redraws are pure cache reads.
        for _ in 0..20 {
            assert_eq!(cache.segment(tmp.path()), Some("main".to_string()));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // A completed command cycle buys exactly one refill.
        cache.note_command_cycle();
        let _ = cache.segment(tmp.path());
        let deadline = Instant::now() + Duration::from_secs(10);
        while cache.fill_count() < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(cache.fill_count(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "one spawn per cycle");

        // Outside a repository: no fill is ever scheduled.
        let outside = tempfile::tempdir().unwrap();
        for _ in 0..5 {
            assert_eq!(cache.segment(outside.path()), None);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn in_flight_refills_are_deduplicated() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let cache = VcsCache::new(Arc::new(move |_: &Path| {
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(250));
            Some("main".to_string())
        }));
        // A repaint storm inside the fill's flight window: every call sees
        // the in-flight marker, none schedules a second compute.
        for _ in 0..20 {
            assert_eq!(cache.segment(tmp.path()), None);
        }
        let settled = wait_for_segment(
            &cache,
            tmp.path(),
            &Some("main".to_string()),
            Duration::from_secs(10),
        );
        assert_eq!(settled, Some("main".to_string()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stale_value_is_served_while_the_refill_runs() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let cache = VcsCache::new(Arc::new(move |_: &Path| {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Some("main".to_string())
            } else {
                // Slow refill: the stale read below must not wait for it.
                std::thread::sleep(Duration::from_millis(300));
                Some("main*".to_string())
            }
        }));
        let settled = wait_for_segment(
            &cache,
            tmp.path(),
            &Some("main".to_string()),
            Duration::from_secs(10),
        );
        assert_eq!(settled, Some("main".to_string()));

        cache.note_command_cycle();
        // Immediately after the cycle mark, the drawing path still paints
        // the stale value — it does not block on the refill.
        assert_eq!(cache.segment(tmp.path()), Some("main".to_string()));
        let updated = wait_for_segment(
            &cache,
            tmp.path(),
            &Some("main*".to_string()),
            Duration::from_secs(10),
        );
        assert_eq!(updated, Some("main*".to_string()));
    }

    /// The counting-fake-git probe (unix): the compute runs a shim `git`
    /// that appends one line to a counter file per invocation, so the
    /// assertion counts real child processes, not closure calls.
    #[cfg(unix)]
    #[test]
    fn counting_fake_git_probe() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        let bin = tempfile::tempdir().unwrap();
        let counter = bin.path().join("count");
        let shim = bin.path().join("git");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nprintf 'x\\n' >> '{}'\nprintf '%s\\n' \
                 '# branch.oid 1111111111111111111111111111111111111111' \
                 '# branch.head main'\n",
                counter.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cache = VcsCache::new(Arc::new(move |worktree: &Path| {
            let output = Command::new(&shim)
                .current_dir(worktree)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()?;
            parse_status(&output.stdout).map(|(name, dirty)| render_segment(&name, dirty))
        }));
        let spawns = |counter: &Path| {
            std::fs::read_to_string(counter)
                .map(|text| text.lines().count())
                .unwrap_or(0)
        };
        for _ in 0..20 {
            let _ = cache.segment(tmp.path());
        }
        let settled = wait_for_segment(
            &cache,
            tmp.path(),
            &Some("main".to_string()),
            Duration::from_secs(10),
        );
        assert_eq!(settled, Some("main".to_string()));
        assert_eq!(spawns(&counter), 1, "20 redraws inside a repo: one spawn");

        cache.note_command_cycle();
        let _ = cache.segment(tmp.path());
        let deadline = Instant::now() + Duration::from_secs(10);
        while cache.fill_count() < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(spawns(&counter), 2, "one more cycle: one more spawn");

        let outside = tempfile::tempdir().unwrap();
        for _ in 0..5 {
            assert_eq!(cache.segment(outside.path()), None);
        }
        assert_eq!(spawns(&counter), 2, "outside a repo: zero spawns");
    }

    // ---- Steady-state equivalence matrix (real git fixtures) ----
    //
    // For every fixture shape, the cache's settled value must equal the
    // synchronous reference read byte for byte — the async schedule may
    // change *when* the answer arrives, never *what* it is.

    fn assert_equivalent(cwd: &Path, worktree: &Path) {
        let expected = reference_segment(worktree);
        let cache = VcsCache::new(real_compute());
        let got = wait_for_segment(&cache, cwd, &expected, Duration::from_secs(15));
        assert_eq!(
            got, expected,
            "cache steady state must equal the direct read at {cwd:?}"
        );
    }

    #[test]
    fn equivalence_plain_clean_and_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        let sub = tmp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert_equivalent(&sub, tmp.path());

        std::fs::write(tmp.path().join("tracked.txt"), b"v1").unwrap();
        assert!(git(tmp.path(), &["add", "tracked.txt"]));
        assert!(git(tmp.path(), &["commit", "--quiet", "-m", "add"]));
        std::fs::write(tmp.path().join("tracked.txt"), b"v2").unwrap();
        std::fs::write(tmp.path().join("untracked.txt"), b"x").unwrap();
        assert_equivalent(tmp.path(), tmp.path());
    }

    #[test]
    fn equivalence_unborn_and_detached() {
        let unborn = tempfile::tempdir().unwrap();
        if !init_repo(unborn.path()) {
            return;
        }
        assert_equivalent(unborn.path(), unborn.path());

        let detached = tempfile::tempdir().unwrap();
        if !repo_with_commit(detached.path())
            || !git(detached.path(), &["switch", "--detach", "HEAD"])
        {
            return;
        }
        assert_equivalent(detached.path(), detached.path());
    }

    #[test]
    fn equivalence_linked_worktree() {
        let main = tempfile::tempdir().unwrap();
        if !repo_with_commit(main.path()) {
            return;
        }
        let linked_parent = tempfile::tempdir().unwrap();
        let linked_path = linked_parent.path().join("wt");
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
        assert_equivalent(&linked_path, &linked_path);
    }

    #[test]
    fn equivalence_submodule() {
        let sub_repo = tempfile::tempdir().unwrap();
        if !repo_with_commit(sub_repo.path()) {
            return;
        }
        std::fs::write(sub_repo.path().join("in-sub.txt"), b"x").unwrap();
        if !git(sub_repo.path(), &["add", "in-sub.txt"])
            || !git(sub_repo.path(), &["commit", "--quiet", "-m", "content"])
        {
            return;
        }
        let super_repo = tempfile::tempdir().unwrap();
        if !init_repo(super_repo.path()) {
            return;
        }
        let added = Command::new("git")
            .arg("-C")
            .arg(super_repo.path())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                sub_repo.path().to_str().unwrap(),
                "sub",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !added || !git(super_repo.path(), &["commit", "--quiet", "-m", "add sub"]) {
            return;
        }
        // From inside the submodule, the segment is the submodule's own.
        assert_equivalent(
            &super_repo.path().join("sub"),
            &super_repo.path().join("sub"),
        );
        // From the superproject root, it is the superproject's.
        assert_equivalent(super_repo.path(), super_repo.path());
    }

    #[test]
    fn equivalence_sparse_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        if !init_repo(tmp.path()) {
            return;
        }
        std::fs::create_dir_all(tmp.path().join("keep")).unwrap();
        std::fs::create_dir_all(tmp.path().join("skip")).unwrap();
        std::fs::write(tmp.path().join("keep/f.txt"), b"a").unwrap();
        std::fs::write(tmp.path().join("skip/f.txt"), b"b").unwrap();
        if !git(tmp.path(), &["add", "-A"])
            || !git(tmp.path(), &["commit", "--quiet", "-m", "init"])
            || !git(tmp.path(), &["sparse-checkout", "set", "keep"])
        {
            return;
        }
        std::fs::write(tmp.path().join("keep/f.txt"), b"a2").unwrap();
        assert_equivalent(tmp.path(), tmp.path());
    }

    #[test]
    fn equivalence_large_repository() {
        let tmp = tempfile::tempdir().unwrap();
        if !init_repo(tmp.path()) {
            return;
        }
        for i in 0..1200 {
            let dir = tmp.path().join(format!("d{}", i % 20));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
        }
        if !git(tmp.path(), &["add", "-A"])
            || !git(tmp.path(), &["commit", "--quiet", "-m", "big"])
        {
            return;
        }
        std::fs::write(tmp.path().join("d3/f3.txt"), b"changed").unwrap();
        assert_equivalent(tmp.path(), tmp.path());
    }

    #[test]
    fn equivalence_dubious_ownership_refusal_matches() {
        // A repository owned by someone else: git refuses (dubious
        // ownership) and both the direct read and the cache must answer
        // "no segment" — the refusal is git's call, not the cache's.
        // Needs the ability to chown the fixture away (root in the dev
        // VM/CI); where that is impossible the fixture skips itself.
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        let chowned = Command::new("chown")
            .args(["-R", "65534:65534"])
            .arg(tmp.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !chowned {
            eprintln!("dubious-ownership fixture skipped: cannot chown the fixture");
            return;
        }
        // Isolate git's config view: an ambient safe.directory entry would
        // mask the refusal and the fixture would stop testing anything.
        let isolated_home = tempfile::tempdir().unwrap();
        let env: [(&str, &Path); 2] = [
            ("HOME", isolated_home.path()),
            ("GIT_CONFIG_NOSYSTEM", Path::new("1")),
        ];
        let expected = reference_segment_env(tmp.path(), &env);
        if expected.is_some() {
            eprintln!(
                "dubious-ownership fixture skipped: git answered anyway \
                 (ownership check not in force in this environment)"
            );
            return;
        }
        let home = isolated_home.path().to_path_buf();
        let cache = VcsCache::new(Arc::new(move |worktree: &Path| {
            reference_segment_env(
                worktree,
                &[
                    ("HOME", home.as_path()),
                    ("GIT_CONFIG_NOSYSTEM", Path::new("1")),
                ],
            )
        }));
        let got = wait_for_segment(&cache, tmp.path(), &None, Duration::from_secs(15));
        assert_eq!(got, None, "a refused repository renders no segment");
    }
}
