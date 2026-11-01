//! The git prompt segment's cache core: platform-independent, and the
//! prompt's single git spawn point (niubash#592 phase one, niubash#685,
//! niubash#643).
//!
//! The segment's data still comes from the user's own `git` — fidelity
//! is the product promise (sparse checkouts, submodules, linked
//! worktrees, partial clones and fsmonitor state can only be answered
//! by the git the user actually runs; the libgit2 prompt silently
//! dropped the branch segment on old-owner repo trees once already).
//! What this layer changes is *when* the answer is computed and *who
//! waits for it*: nobody. A draw only ever reads the cache; the one
//! bounded spawn runs on a scoped background refresh thread.
//!
//! Freshness contract for one worktree's entry — the ruled #592
//! phase-one contract, exactly two inputs:
//!
//!  - **repo fingerprint**: the `.git/HEAD` contents plus the index
//!    file's stat (mtime/size), both read through the gitdir
//!    resolution so linked worktrees and submodules (a `.git` *file*)
//!    fingerprint their own HEAD and index. Branch switches and
//!    staging move one of the two, so the next read notices without
//!    any host cooperation.
//!  - **command-cycle mark**: the host marks the current repository's
//!    entry dirty after each completed interactive command cycle
//!    ([`invalidate_segment_for_cwd`]). A command can rewrite worktree
//!    files without touching HEAD or the index — exactly what the
//!    fingerprint cannot see.
//!
//! An entry is served without a refresh only while its fingerprint
//! still matches and no cycle mark has landed since it was computed.
//! Otherwise the read serves the entry as-is (the previous value, or
//! `None` on a cold cache — the first-draw face is an empty segment,
//! never a wait) and at most one background refresh per worktree
//! recomputes it: at most one spawn per command cycle per repository,
//! and redraw storms inside a cycle cost zero spawns. Worktree edits
//! made entirely outside this shell move neither input, so they are
//! picked up at the next command cycle at the latest — the design's
//! stated bound, deliberately not papered over with a timer: a TTL
//! would spawn on idle redraws, outside the per-cycle budget the
//! design promises.
//!
//! Redraw channel: a landed refresh writes the cache — the value
//! every prompt draw reads — and notifies the landing observers
//! registered with [`add_landing_observer`]. Within a pass, repaints
//! re-read the pass's stamped value (niubash#592 S2): renders never
//! touch the cache. The one mid-pass channel is the host's
//! (niubash#787): the interactive host observes the landing,
//! re-stamps the displayed pass's live Vcs cell in place, and
//! requests a single editor repaint — a host write on the landing
//! thread, not a render read, so the stamp contract holds.
//!
//! Worker hygiene: a refresh thread is scoped to one recompute — it
//! holds no engine lock, touches the cache lock only to store its
//! result, and exits when it lands. Nothing is joined and nothing is
//! parked across prompt sessions, so shell exit never waits on a
//! refresh; a git child outliving its shell is still bounded by the
//! status timeout below.
//!
//! Isolation of the one spawn, one rule per incident: never through a
//! shell (a missing git is an OS error, not a "command not found" on
//! the terminal), stdout piped to a scratch file and stderr discarded
//! (git warnings can never reach the screen), `--no-optional-locks` +
//! `GIT_OPTIONAL_LOCKS=0` (a prompt must never take `index.lock`),
//! `-c core.fsmonitor=false` (a prompt must never start or poke an
//! fsmonitor daemon), `GIT_TERMINAL_PROMPT=0`, no console window, and
//! the child is killed when it outlives the timeout. Outside a
//! repository the segment spawns nothing at all.
//!
//! This module is the platform-independent core the #592 design
//! called for: the Windows product faces (the gitbash factory prompt
//! in `git_prompt`, the segment engine in `prompt_segments`) read
//! through it, and its contract is pinned by the `issue643_*` tests
//! below on every platform.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Hard bound for the prompt's git call; the child is killed past it. The
/// bound only bites on a pathological repository (a hook that hangs, a dead
/// network share); a healthy `status` finishes far inside it.
const STATUS_TIMEOUT: Duration = Duration::from_millis(2000);

/// Read budget for the status call. Porcelain v2 prints the `# branch.*`
/// headers first, so the head of the stream already answers the prompt; the
/// cap keeps the pipe and the reader bounded even in a repository with tens
/// of thousands of changed entries.
const STATUS_READ_CAP: usize = 8 * 1024;

/// The working tree containing `cwd`, if any: the nearest ancestor holding a
/// `.git` entry — a directory in an ordinary clone, a file in linked
/// worktrees and submodules. Cheaper than asking git, and outside a
/// repository it is the entire cost of the segment: nothing is spawned.
/// A bare repository has no worktree and no `.git` entry inside it, so
/// it resolves to `None` here like any other non-worktree directory.
pub(crate) fn find_worktree(cwd: &Path) -> Option<PathBuf> {
    std::iter::successors(Some(cwd), |dir| dir.parent())
        .find(|dir| {
            let marker = dir.join(".git");
            marker.is_dir() || marker.is_file()
        })
        .map(Path::to_path_buf)
}

/// The git segment of the prompt: `branch` / `branch*` (dirty) / short SHA
/// (detached) — `None` outside a repository, when git is unavailable, or
/// when the last read failed or timed out. Failures are silent by design:
/// the prompt degrades to "no branch" and the terminal stays clean.
///
/// The value comes from the cross-pass cache: this call never spawns on
/// the draw path and never waits — on a cold or stale entry it returns
/// the cached value (or `None`) and a background refresh lands the
/// fresh value for the next call.
pub fn branch_segment(cwd: &Path) -> Option<String> {
    let worktree = find_worktree(cwd)?;
    cached_segment(&worktree)
}

/// What the cache can know about a repository without spawning: the
/// HEAD file's contents (the branch ref or detached oid) and the index
/// file's stat. Both are read through the gitdir resolution so linked
/// worktrees and submodules (a `.git` *file*) fingerprint their own
/// HEAD and index, not the main worktree's.
#[derive(Clone, PartialEq, Eq)]
struct RepoFingerprint {
    head: Option<String>,
    index: Option<(std::time::SystemTime, u64)>,
}

/// Resolve a worktree's gitdir: the `.git` directory itself, or the
/// target of a `.git` file's `gitdir:` line (linked worktrees,
/// submodules), relative to the worktree when spelled relative.
fn resolve_gitdir(worktree: &Path) -> Option<PathBuf> {
    let dot = worktree.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    let path = Path::new(target);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        worktree.join(path)
    })
}

/// Resolve a gitdir's commondir (the shared `.git` for linked
/// worktrees; the gitdir itself when no `commondir` file exists).
fn resolve_commondir(gitdir: &Path) -> PathBuf {
    if let Ok(text) = std::fs::read_to_string(gitdir.join("commondir")) {
        let target = text.trim();
        let path = Path::new(target);
        if path.is_absolute() {
            return path.to_path_buf();
        }
        return gitdir.join(path);
    }
    gitdir.to_path_buf()
}

/// Fingerprint the repository at `worktree`. The index is per-worktree
/// state, so its stat is taken at the gitdir first; the commondir is
/// the fallback for layouts that keep it there. A missing index (a
/// fresh `git init` has none) fingerprints as `None`.
fn repo_fingerprint(worktree: &Path) -> RepoFingerprint {
    let Some(gitdir) = resolve_gitdir(worktree) else {
        return RepoFingerprint {
            head: None,
            index: None,
        };
    };
    let head = std::fs::read_to_string(gitdir.join("HEAD"))
        .ok()
        .map(|text| text.trim().to_string());
    let stat = |path: PathBuf| {
        std::fs::metadata(path)
            .ok()
            .and_then(|meta| meta.modified().ok().map(|mtime| (mtime, meta.len())))
    };
    let index =
        stat(gitdir.join("index")).or_else(|| stat(resolve_commondir(&gitdir).join("index")));
    RepoFingerprint { head, index }
}

struct CachedSegment {
    /// The segment body exactly as `branch_segment` returns it; a
    /// stored `None` is a computed result ("no segment": git missing,
    /// failed or timed out), not a missing value.
    value: Option<String>,
    fingerprint: RepoFingerprint,
    /// Bumped by every command-cycle invalidation that lands while
    /// this entry lives; a refresh records the count it started under
    /// and only clears `dirty` when the count is unchanged on landing,
    /// so an invalidation mid-refresh is never lost.
    cycles: u64,
    dirty: bool,
    refreshing: bool,
}

#[derive(Default)]
struct SegmentCache {
    entries: std::collections::HashMap<PathBuf, CachedSegment>,
}

static SEGMENT_CACHE: std::sync::LazyLock<(std::sync::Mutex<SegmentCache>, std::sync::Condvar)> =
    std::sync::LazyLock::new(|| {
        (
            std::sync::Mutex::new(SegmentCache::default()),
            std::sync::Condvar::new(),
        )
    });

fn segment_cache_lock() -> std::sync::MutexGuard<'static, SegmentCache> {
    SEGMENT_CACHE
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Serve the segment for `worktree` from the cache, launching one
/// background refresh when the entry is cold or stale. Never blocks on
/// git; the only file inspection is the fingerprint (see above).
fn cached_segment(worktree: &Path) -> Option<String> {
    let fingerprint = repo_fingerprint(worktree);
    let mut launch: Option<(RepoFingerprint, u64)> = None;
    let value = {
        let mut cache = segment_cache_lock();
        match cache.entries.get_mut(worktree) {
            Some(entry) => {
                let fresh = !entry.dirty && entry.fingerprint == fingerprint;
                if !fresh && !entry.refreshing {
                    entry.refreshing = true;
                    launch = Some((fingerprint, entry.cycles));
                }
                entry.value.clone()
            }
            None => {
                cache.entries.insert(
                    worktree.to_path_buf(),
                    CachedSegment {
                        value: None,
                        fingerprint: fingerprint.clone(),
                        cycles: 0,
                        dirty: true,
                        refreshing: true,
                    },
                );
                launch = Some((fingerprint, 0));
                None
            }
        }
    };
    if let Some((fingerprint, cycles)) = launch {
        launch_refresh(worktree.to_path_buf(), fingerprint, cycles);
    }
    value
}

/// Live refresh threads per worktree right now (test builds only):
/// the worker hygiene gauge — a refresh thread exists only between
/// its launch and its landing, so a worktree's count returns to zero
/// whenever its refreshes have all landed, however many prompt
/// sessions have come and gone. Keyed per worktree like the spawn
/// counter below, so parallel tests never pollute each other's gauge.
#[cfg(test)]
static LIVE_REFRESHES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, usize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Number of refresh threads currently running for `worktree` (test
/// builds only).
#[cfg(test)]
pub(crate) fn live_refresh_count(worktree: &Path) -> usize {
    LIVE_REFRESHES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(worktree)
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
struct LiveRefreshGuard(PathBuf);

#[cfg(test)]
impl LiveRefreshGuard {
    fn start(worktree: &Path) -> Self {
        let mut counts = LIVE_REFRESHES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *counts.entry(worktree.to_path_buf()).or_insert(0) += 1;
        Self(worktree.to_path_buf())
    }
}

#[cfg(test)]
impl Drop for LiveRefreshGuard {
    fn drop(&mut self) {
        let mut counts = LIVE_REFRESHES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = counts.get_mut(&self.0) {
            *count = count.saturating_sub(1);
        }
    }
}

/// Recompute one worktree's segment on a detached thread. The thread
/// holds no engine lock and touches the cache lock only to store its
/// result; the spawn inside `read_status` keeps every isolation rule
/// from the module docs.
fn launch_refresh(worktree: PathBuf, fingerprint: RepoFingerprint, cycles: u64) {
    // The closure takes `worktree`; the spawn-failure fallback below
    // still needs the key, so keep a clone for it.
    let fallback_key = worktree.clone();
    let spawned = std::thread::Builder::new()
        .name("niu-git-prompt-refresh".to_string())
        .spawn(move || {
            #[cfg(test)]
            let _live = LiveRefreshGuard::start(&worktree);
            let computed = read_status(&worktree).map(|(name, status_dirty)| {
                if status_dirty {
                    format!("{name}*")
                } else {
                    name
                }
            });
            let landed = {
                let mut cache = segment_cache_lock();
                let mut landed = None;
                if let Some(entry) = cache.entries.get_mut(&worktree) {
                    entry.value = computed;
                    entry.fingerprint = fingerprint;
                    entry.dirty = entry.cycles != cycles;
                    entry.refreshing = false;
                    landed = Some(entry.value.clone());
                }
                SEGMENT_CACHE.1.notify_all();
                landed
            };
            // The refresh proper ends at the store above: in test
            // builds the live-refresh gauge stops counting here, so
            // the niubash#643 no-leak pin measures recompute threads
            // exactly as it did before the landing publication
            // existed. The publication below is host-channel work on
            // this same thread and exits with it; nothing is parked.
            #[cfg(test)]
            drop(_live);
            // niubash#787: a landing is published to the registered
            // observers (the interactive host's idle-repaint channel)
            // after the value is stored and the cache lock released:
            // an observer may read the cache, and must never run
            // under its lock.
            if let Some(value) = landed {
                notify_landing_observers(&worktree, &value);
            }
        });
    if spawned.is_err() {
        // The refresh could not start; unstick the entry so a later
        // pass retries instead of waiting on a landing that never
        // comes.
        let mut cache = segment_cache_lock();
        if let Some(entry) = cache.entries.get_mut(&fallback_key) {
            entry.refreshing = false;
        }
        SEGMENT_CACHE.1.notify_all();
    }
}

/// A landing observer: notified with `(worktree, landed value)` every
/// time a refresh lands a value in the cache (niubash#787).
pub(crate) type LandingObserver = std::sync::Arc<dyn Fn(&Path, &Option<String>) + Send + Sync>;

/// Observers notified every time a refresh lands a value in the
/// cache (niubash#787): the interactive host's idle-repaint channel
/// subscribes here so a landing can re-stamp the displayed prompt
/// pass and request one editor repaint. Observers run on the
/// refresh thread, after the value is stored and the cache lock
/// released; they must be cheap and must never wait on the cache.
static LANDING_OBSERVERS: std::sync::Mutex<Vec<LandingObserver>> =
    std::sync::Mutex::new(Vec::new());

/// Register a landing observer. Registering the same observer
/// instance twice is a no-op (the host re-registers on every editor
/// build with one shared closure).
pub(crate) fn add_landing_observer(observer: LandingObserver) {
    let mut observers = LANDING_OBSERVERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !observers
        .iter()
        .any(|o| std::sync::Arc::ptr_eq(o, &observer))
    {
        observers.push(observer);
    }
}

fn notify_landing_observers(worktree: &Path, value: &Option<String>) {
    let observers = LANDING_OBSERVERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    for observer in observers {
        observer(worktree, value);
    }
}

/// Mark the cached segment for the repository containing `cwd` dirty:
/// the host's command-cycle invalidation (see the freshness contract
/// above). A repository with no entry needs no mark — its next pass
/// computes cold anyway.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn invalidate_segment_for_cwd(cwd: &Path) {
    let Some(worktree) = find_worktree(cwd) else {
        return;
    };
    let mut cache = segment_cache_lock();
    if let Some(entry) = cache.entries.get_mut(&worktree) {
        entry.cycles += 1;
        entry.dirty = true;
    }
}

/// The cache key `branch_segment` resolves for a path (test builds
/// only): probes key their spawn counts by the resolved worktree,
/// never by a fixture path's own spelling. Its consumers key through
/// the Windows prompt face (`git_prompt`'s re-export), so on other
/// platforms it has no caller in this crate's own tests.
#[cfg(test)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn worktree_key_for(cwd: &Path) -> Option<PathBuf> {
    find_worktree(cwd)
}

/// Block until no worktree has a refresh in flight (test builds only):
/// the deterministic landing point for cache assertions — event-driven
/// on the cache condvar, never a sleep or a poll.
#[cfg(test)]
pub(crate) fn wait_for_segment_cache_idle(timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    let mut cache = segment_cache_lock();
    loop {
        if cache.entries.values().all(|entry| !entry.refreshing) {
            return true;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let (guard, _) = SEGMENT_CACHE
            .1
            .wait_timeout(cache, deadline - now)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache = guard;
    }
}

// Spawn counter for the niubash#592/#643 acceptance probes (test builds
// only): `read_status` is the prompt's only git spawn point and runs
// exactly once per spawn, so counting its entries counts prompt git
// spawns exactly. The count is kept per worktree: the spawn runs on a
// refresh thread, not the caller's, so a thread-local count cannot see
// it — and per-worktree keys keep parallel tests isolated exactly as a
// thread-local count would, because every probe drives its own fixture
// repository. Production builds carry no counter.
#[cfg(test)]
static READ_STATUS_CALLS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, usize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Number of `read_status` calls for `worktree` so far (test builds
/// only).
#[cfg(test)]
pub(crate) fn read_status_call_count(worktree: &Path) -> usize {
    READ_STATUS_CALLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(worktree)
        .copied()
        .unwrap_or(0)
}

/// One read-only `git status` for the prompt, with every isolation rule from
/// the module docs applied. Returns the branch name (or detached short SHA)
/// plus whether anything is dirty.
fn read_status(worktree: &Path) -> Option<(String, bool)> {
    #[cfg(test)]
    {
        let mut counts = READ_STATUS_CALLS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *counts.entry(worktree.to_path_buf()).or_insert(0) += 1;
    }
    // stdout goes to a scratch file, not a pipe: a pipe needs either a drain
    // thread (whose hand-off costs a scheduler quantum on Windows, ~15 ms,
    // which would double the segment) or risks wedging the child once it
    // writes past the pipe buffer. The wait below is a precise Win32 wait on
    // the process handle, so nothing polls either. The per-call sequence
    // keeps concurrent refreshes (two repositories in one session) out of
    // each other's scratch.
    static SCRATCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scratch =
        std::env::temp_dir().join(format!("niu-prompt-git-{}-{}.out", std::process::id(), seq));
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
    crate::console_guard::suppress_orphan_console(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => {
            crate::console_guard::adopt_orphan_console(child.id());
            child
        }
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
        status == WAIT_OBJECT_0 || status != WAIT_TIMEOUT
    }
    #[cfg(not(windows))]
    {
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
pub(crate) fn parse_status(bytes: &[u8]) -> Option<(String, bool)> {
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

    /// What the landing-observer fixture records: the
    /// (worktree, landed value) pairs its observer saw.
    type SeenLandings = std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, Option<String>)>>>;

    /// Fixtures are real repositories: the segment reads the user's git,
    /// so the tests drive the same path production does. A `false` means
    /// git is not on PATH; the fixture then aborts quietly instead of
    /// failing a machine that has no git.
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

    /// Same as `git`, but through an explicit git binary (the shim test
    /// drives fixture plumbing through the real git so its PATH window
    /// only ever sees prompt spawns in the log).
    #[cfg(unix)]
    fn git_with(binary: &Path, dir: &Path, args: &[&str]) -> bool {
        Command::new(binary)
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

    /// Drive one segment read to its settled value: the first call only
    /// triggers the background refresh, so wait for the landing
    /// (event-driven, never a poll), then read again.
    fn settled_segment(dir: &Path) -> Option<String> {
        let _ = branch_segment(dir);
        assert!(
            wait_for_segment_cache_idle(Duration::from_secs(30)),
            "the segment refresh must land"
        );
        branch_segment(dir)
    }

    fn spawns(worktree: &Path) -> usize {
        read_status_call_count(worktree)
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

    // ---- niubash#643: the #592 phase-one contract, pinned on the
    // platform-independent core (the #685 cache tests, re-pinned under
    // this issue, plus the faces the audit found unpinned). Every count
    // below is a spawn count — the platform-independent evidence.

    #[test]
    fn issue643_first_draw_before_fill_is_empty_then_lands() {
        // The design's first-draw face: a cold entry draws an empty
        // segment rather than waiting for git; the value lands
        // asynchronously for the next draw.
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        assert_eq!(
            branch_segment(tmp.path()),
            None,
            "a cold cache serves no segment instead of waiting"
        );
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(spawns(tmp.path()), 1, "the cold draw costs one spawn");
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main"));
    }

    #[test]
    fn issue787_landing_notifies_observers_once_with_the_landed_value() {
        // niubash#787: the idle-repaint channel's source — a landing
        // publishes (worktree, landed value) to the registered
        // observers exactly once, and observing costs no spawn.
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        let seen: SeenLandings = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = {
            let seen = seen.clone();
            std::sync::Arc::new(move |worktree: &Path, value: &Option<String>| {
                seen.lock()
                    .unwrap()
                    .push((worktree.to_path_buf(), value.clone()));
            }) as LandingObserver
        };
        add_landing_observer(recorder);
        assert_eq!(
            branch_segment(tmp.path()),
            None,
            "a cold cache serves no segment instead of waiting"
        );
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        let mine: Vec<Option<String>> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(worktree, _)| worktree == tmp.path())
            .map(|(_, value)| value.clone())
            .collect();
        assert_eq!(
            mine,
            vec![Some("main".to_string())],
            "exactly one landing notice, carrying the landed value"
        );
        assert_eq!(spawns(tmp.path()), 1, "observing a landing spawns nothing");
    }

    #[test]
    fn issue643_twenty_redraws_after_settle_spawn_zero() {
        // The design's headline count: redraws inside a repository,
        // with an unchanged fingerprint and no command cycle in
        // between, never spawn — twenty in a row here.
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        assert_eq!(settled_segment(tmp.path()).as_deref(), Some("main"));
        for _ in 0..20 {
            assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main"));
        }
        assert_eq!(
            spawns(tmp.path()),
            1,
            "fingerprint-fresh redraws never spawn"
        );
    }

    #[test]
    fn issue643_command_cycle_costs_one_spawn_and_dirty_flip_follows() {
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
        assert_eq!(settled_segment(tmp.path()).as_deref(), Some("main"));
        assert_eq!(spawns(tmp.path()), 1);
        // A worktree edit the fingerprint cannot see (it moves neither
        // HEAD nor the index), then the host's command-cycle mark.
        std::fs::write(tmp.path().join("tracked.txt"), b"v2").unwrap();
        invalidate_segment_for_cwd(tmp.path());
        // The stale value is served while the refresh runs — the read
        // never waits for the new state.
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main"));
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(
            spawns(tmp.path()),
            2,
            "one command cycle costs exactly one background spawn"
        );
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main*"));
        // The flip back to clean follows the same one-spawn cycle.
        std::fs::write(tmp.path().join("tracked.txt"), b"v1").unwrap();
        invalidate_segment_for_cwd(tmp.path());
        let _ = branch_segment(tmp.path());
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(spawns(tmp.path()), 3);
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main"));
    }

    #[test]
    fn issue643_inflight_refresh_dedups_and_marks_are_never_lost() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        assert_eq!(settled_segment(tmp.path()).as_deref(), Some("main"));
        assert_eq!(spawns(tmp.path()), 1);
        // Cycle A: the mark launches one refresh; a storm of reads
        // while it is in flight must not launch another.
        invalidate_segment_for_cwd(tmp.path());
        for _ in 0..10 {
            let _ = branch_segment(tmp.path());
        }
        // Cycle B lands mid-flight (if the flight is still out): the
        // mark is recorded in the entry's cycle count, so the landing
        // must not clear the dirty bit — one more refresh follows, and
        // reads after it are hits again. If cycle A's refresh already
        // landed, cycle B simply costs its own single refresh; either
        // way the total is bounded by one spawn per cycle.
        invalidate_segment_for_cwd(tmp.path());
        let _ = branch_segment(tmp.path());
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        let _ = branch_segment(tmp.path());
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        let total = spawns(tmp.path());
        assert!(
            (2..=3).contains(&total),
            "one spawn per command cycle, never more: total={total}"
        );
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main"));
        let settled_at = spawns(tmp.path());
        for _ in 0..5 {
            let _ = branch_segment(tmp.path());
        }
        assert_eq!(
            spawns(tmp.path()),
            settled_at,
            "once the marks are drained, reads are hits"
        );
    }

    #[test]
    fn issue643_head_move_by_checkout_invalidates_by_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        assert_eq!(settled_segment(tmp.path()).as_deref(), Some("main"));
        if !git(tmp.path(), &["switch", "--quiet", "-c", "side"]) {
            return;
        }
        // No command-cycle mark: the fingerprint alone notices HEAD
        // moving, triggers one refresh, and the settled value follows.
        let _ = branch_segment(tmp.path());
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(spawns(tmp.path()), 2);
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("side"));
    }

    #[test]
    fn issue643_staging_and_commit_each_cost_one_background_spawn() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) {
            return;
        }
        assert_eq!(settled_segment(tmp.path()).as_deref(), Some("main"));
        assert_eq!(spawns(tmp.path()), 1);
        // Staging rewrites the index (its stat moves) without touching
        // HEAD: the fingerprint alone must notice, once.
        std::fs::write(tmp.path().join("staged.txt"), b"x").unwrap();
        if !git(tmp.path(), &["add", "staged.txt"]) {
            return;
        }
        let _ = branch_segment(tmp.path());
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(spawns(tmp.path()), 2, "staging costs exactly one spawn");
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main*"));
        // The commit moves the repository on (the index is rewritten
        // with the recorded tree): one more refresh, back to clean.
        if !git(tmp.path(), &["commit", "--quiet", "-m", "stage it"]) {
            return;
        }
        let _ = branch_segment(tmp.path());
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(spawns(tmp.path()), 3, "the commit costs exactly one spawn");
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main"));
    }

    #[test]
    fn issue643_unstage_and_restore_invalidate_by_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        if !init_repo(tmp.path()) {
            return;
        }
        std::fs::write(tmp.path().join("tracked.txt"), b"v1").unwrap();
        if !git(tmp.path(), &["add", "tracked.txt"])
            || !git(tmp.path(), &["commit", "--quiet", "-m", "add"])
        {
            return;
        }
        assert_eq!(settled_segment(tmp.path()).as_deref(), Some("main"));
        assert_eq!(spawns(tmp.path()), 1);
        // Worktree edit: only the cycle mark can see it.
        std::fs::write(tmp.path().join("tracked.txt"), b"v2").unwrap();
        invalidate_segment_for_cwd(tmp.path());
        let _ = branch_segment(tmp.path());
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(spawns(tmp.path()), 2);
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main*"));
        // Stage it: the index stat moves.
        if !git(tmp.path(), &["add", "tracked.txt"]) {
            return;
        }
        let _ = branch_segment(tmp.path());
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(spawns(tmp.path()), 3);
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main*"));
        // Unstage + restore (`reset --hard` rewrites the index and the
        // file): the fingerprint notices again, and the star goes away.
        if !git(tmp.path(), &["reset", "--quiet", "--hard", "HEAD"]) {
            return;
        }
        let _ = branch_segment(tmp.path());
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(spawns(tmp.path()), 4);
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main"));
    }

    #[test]
    fn issue643_repo_switch_keeps_entries_independent() {
        let repo_a = tempfile::tempdir().unwrap();
        if !repo_with_commit(repo_a.path()) {
            return;
        }
        let repo_b = tempfile::tempdir().unwrap();
        if !init_repo(repo_b.path()) {
            return;
        }
        if !git(repo_b.path(), &["switch", "--quiet", "-c", "other"])
            || !git(
                repo_b.path(),
                &["commit", "--quiet", "--allow-empty", "-m", "init"],
            )
        {
            return;
        }
        // Each repository is cold on its own first draw.
        assert_eq!(branch_segment(repo_a.path()), None);
        assert_eq!(branch_segment(repo_b.path()), None);
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(branch_segment(repo_a.path()).as_deref(), Some("main"));
        assert_eq!(branch_segment(repo_b.path()).as_deref(), Some("other"));
        assert_eq!(spawns(repo_a.path()), 1);
        assert_eq!(spawns(repo_b.path()), 1);
        // Alternating reads are per-repo hits: switching back never
        // recomputes the repository you returned to.
        for _ in 0..5 {
            assert_eq!(branch_segment(repo_a.path()).as_deref(), Some("main"));
            assert_eq!(branch_segment(repo_b.path()).as_deref(), Some("other"));
        }
        assert_eq!(spawns(repo_a.path()), 1);
        assert_eq!(spawns(repo_b.path()), 1);
        // A subdirectory of a repository shares the root's entry.
        let sub = repo_a.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert_eq!(branch_segment(&sub).as_deref(), Some("main"));
        assert_eq!(spawns(repo_a.path()), 1);
    }

    #[test]
    fn issue643_non_repo_dir_renders_no_segment() {
        // Outside a repository the segment must not spawn anything:
        // `find_worktree` short-circuits before the cache or the spawn
        // point can be reached, which the `None` resolution pins
        // structurally (the shim pin below counts it end-to-end).
        let tmp = tempfile::tempdir().unwrap();
        assert!(find_worktree(tmp.path()).is_none());
        for _ in 0..3 {
            assert_eq!(branch_segment(tmp.path()), None);
        }
    }

    #[test]
    fn issue643_bare_repo_yields_no_segment() {
        // A bare repository is a gitdir, not a worktree: there is no
        // worktree status to read (`git status` itself refuses there),
        // so the segment stays empty and nothing spawns — from the
        // bare root and from inside its subdirectories alike.
        let tmp = tempfile::tempdir().unwrap();
        if !git(
            tmp.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                "bare.git",
            ],
        ) {
            return;
        }
        let bare = tmp.path().join("bare.git");
        assert!(find_worktree(&bare).is_none());
        assert_eq!(branch_segment(&bare), None);
        let hooks = bare.join("hooks");
        assert!(find_worktree(&hooks).is_none());
        assert_eq!(branch_segment(&hooks), None);
    }

    #[test]
    fn issue643_submodule_git_file_indirection_reads_own_head() {
        // A submodule's `.git` is a *file* pointing into the parent's
        // `.git/modules/...`: the walk must accept that form, the
        // fingerprint must read the submodule's own HEAD/index there,
        // and the segment must follow the submodule's state.
        let src = tempfile::tempdir().unwrap();
        if !repo_with_commit(src.path()) {
            return;
        }
        let parent = tempfile::tempdir().unwrap();
        if !repo_with_commit(parent.path()) {
            return;
        }
        let added = Command::new("git")
            .arg("-C")
            .arg(parent.path())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "protocol.file.allow=always"])
            .args(["submodule", "add", "--quiet"])
            .arg(src.path())
            .arg("sub")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !added {
            return;
        }
        let sub = parent.path().join("sub");
        assert!(sub.join(".git").is_file(), "the submodule marker is a file");
        // A fresh submodule is asserted at a detached HEAD: the
        // segment is the short SHA. Some git builds leave the freshly
        // added submodule on its default branch instead (environment
        // flake, same signature on this machine and the Windows CI
        // runner), so detach explicitly at the recorded HEAD — the
        // fixture must not depend on the local git's checkout choice.
        let full = git_stdout(&sub, &["rev-parse", "HEAD"]);
        if full.len() < 7 {
            return;
        }
        if !git(
            &sub,
            &[
                "-c",
                "advice.detachedHead=false",
                "checkout",
                "--quiet",
                "--detach",
                &full,
            ],
        ) {
            return;
        }
        assert_eq!(
            settled_segment(&sub).as_deref(),
            Some(full[..7].to_string()).as_deref()
        );
        assert_eq!(spawns(&sub), 1);
        // Switching branch inside the submodule moves its own HEAD
        // (under the parent's .git/modules): the fingerprint follows.
        if !git(&sub, &["switch", "--quiet", "-c", "subwork"]) {
            return;
        }
        let _ = branch_segment(&sub);
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(spawns(&sub), 2);
        assert_eq!(branch_segment(&sub).as_deref(), Some("subwork"));
        // The parent's entry is untouched by the submodule's life.
        assert_eq!(settled_segment(parent.path()).as_deref(), Some("main*"));
    }

    #[test]
    fn issue643_linked_worktree_git_file_reads_own_branch() {
        // Linked worktrees mark the repository with a `.git` *file*;
        // the walk has to accept that form and read the worktree's
        // own HEAD (its gitdir lives under the main repo's
        // `.git/worktrees/...`, with a commondir back-reference).
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
        assert_eq!(settled_segment(&linked_path).as_deref(), Some("side"));
        assert_eq!(spawns(&linked_path), 1);
    }

    #[test]
    fn issue643_detached_head_shows_the_short_sha() {
        let tmp = tempfile::tempdir().unwrap();
        if !repo_with_commit(tmp.path()) || !git(tmp.path(), &["switch", "--detach", "HEAD"]) {
            return;
        }
        let full = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
        if full.len() < 7 {
            return;
        }
        assert_eq!(
            settled_segment(tmp.path()).as_deref(),
            Some(full[..7].to_string()).as_deref()
        );
    }

    #[test]
    fn issue643_unborn_head_and_first_index_appearance() {
        // A fresh `git init` has no commit and no index file at all:
        // the fingerprint's index leg is `None`, and the segment still
        // renders the unborn branch the way Git Bash does.
        let tmp = tempfile::tempdir().unwrap();
        if !init_repo(tmp.path()) {
            return;
        }
        assert!(!tmp.path().join(".git/index").exists());
        assert_eq!(settled_segment(tmp.path()).as_deref(), Some("main"));
        assert_eq!(spawns(tmp.path()), 1);
        // The first `git add` creates the index: the fingerprint's
        // index leg goes None -> Some, which must invalidate.
        std::fs::write(tmp.path().join("new.txt"), b"x").unwrap();
        if !git(tmp.path(), &["add", "new.txt"]) {
            return;
        }
        let _ = branch_segment(tmp.path());
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(spawns(tmp.path()), 2);
        assert_eq!(branch_segment(tmp.path()).as_deref(), Some("main*"));
    }

    #[test]
    fn issue643_refresh_worker_does_not_leak_threads() {
        // Refresh threads are scoped to one recompute: after every
        // landing the live-thread gauge is back at zero, across
        // repeated cycles and across repositories — nothing is parked
        // between prompt sessions, and nothing needs joining at exit.
        let repo_a = tempfile::tempdir().unwrap();
        if !repo_with_commit(repo_a.path()) {
            return;
        }
        let repo_b = tempfile::tempdir().unwrap();
        if !repo_with_commit(repo_b.path()) {
            return;
        }
        assert_eq!(settled_segment(repo_a.path()).as_deref(), Some("main"));
        assert_eq!(settled_segment(repo_b.path()).as_deref(), Some("main"));
        assert_eq!(
            live_refresh_count(repo_a.path()),
            0,
            "no thread survives a landing"
        );
        assert_eq!(live_refresh_count(repo_b.path()), 0);
        for _ in 0..5 {
            invalidate_segment_for_cwd(repo_a.path());
            invalidate_segment_for_cwd(repo_b.path());
            let _ = branch_segment(repo_a.path());
            let _ = branch_segment(repo_b.path());
            assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
            assert_eq!(live_refresh_count(repo_a.path()), 0);
            assert_eq!(live_refresh_count(repo_b.path()), 0);
        }
        assert_eq!(spawns(repo_a.path()), 6, "one cold + five cycle spawns");
        assert_eq!(spawns(repo_b.path()), 6);
    }

    /// The counting-git-shim acceptance (niubash#643): a fake `git`
    /// placed first on PATH counts every process the prompt spawns,
    /// end to end, and delegates to the real git so repository state
    /// stays real. Counts only the prompt's own signature — the
    /// porcelain status call, keyed by the directory it ran in — so
    /// fixture plumbing and other tests' repositories cannot pollute
    /// the numbers. (The in-process `read_status` counter the other
    /// pins use counts the same single spawn point from the inside;
    /// this pin proves the spawn really is a PATH-resolved child
    /// process — the user's own git — once per cycle.)
    #[cfg(unix)]
    #[test]
    fn issue643_counting_git_shim_on_path_counts_prompt_spawns() {
        use std::os::unix::fs::PermissionsExt;

        struct PathGuard(Option<std::ffi::OsString>);
        impl Drop for PathGuard {
            fn drop(&mut self) {
                match &self.0 {
                    Some(old) => std::env::set_var("PATH", old),
                    None => std::env::remove_var("PATH"),
                }
                std::env::remove_var("NIU643_SHIM_LOG");
            }
        }

        // Resolve the real git before the shim shadows the name.
        let old_path = std::env::var_os("PATH");
        let real_git = old_path
            .as_ref()
            .and_then(|path| {
                std::env::split_paths(path).find_map(|dir| {
                    let candidate = dir.join("git");
                    candidate.is_file().then_some(candidate)
                })
            })
            .expect("git must be on PATH for the fixture repositories");

        let shim_dir = tempfile::tempdir().unwrap();
        let log_path = shim_dir.path().join("spawns.log");
        let shim_path = shim_dir.path().join("git");
        let script = format!(
            "#!/bin/sh\nprintf '%s|%s\\n' \"$PWD\" \"$*\" >> \"$NIU643_SHIM_LOG\"\nexec \"{}\" \"$@\"\n",
            real_git.display()
        );
        std::fs::write(&shim_path, script).unwrap();
        std::fs::set_permissions(&shim_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let repo = tempfile::tempdir().unwrap();
        if !git_with(
            &real_git,
            repo.path(),
            &["init", "--quiet", "--initial-branch=main", "."],
        ) || !git_with(
            &real_git,
            repo.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "init"],
        ) {
            return;
        }
        let outside = tempfile::tempdir().unwrap();

        let _guard = PathGuard(old_path.clone());
        std::env::set_var("NIU643_SHIM_LOG", &log_path);
        let mut shimmed = std::ffi::OsString::from(shim_dir.path().as_os_str());
        if let Some(old) = &old_path {
            shimmed.push(":");
            shimmed.push(old);
        }
        std::env::set_var("PATH", shimmed);

        let prompt_spawns = |log: &Path, worktree: &Path| -> usize {
            std::fs::read_to_string(log)
                .unwrap_or_default()
                .lines()
                .filter(|line| {
                    let Some((cwd, args)) = line.split_once('|') else {
                        return false;
                    };
                    Path::new(cwd) == worktree && args.contains("--porcelain=v2")
                })
                .count()
        };

        // Cold draw: empty face, exactly one real child process.
        assert_eq!(branch_segment(repo.path()), None);
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(prompt_spawns(&log_path, repo.path()), 1);
        assert_eq!(branch_segment(repo.path()).as_deref(), Some("main"));
        // Twenty redraws: still one.
        for _ in 0..20 {
            let _ = branch_segment(repo.path());
        }
        assert_eq!(prompt_spawns(&log_path, repo.path()), 1);
        // One command cycle: exactly one more.
        invalidate_segment_for_cwd(repo.path());
        let _ = branch_segment(repo.path());
        assert!(wait_for_segment_cache_idle(Duration::from_secs(30)));
        assert_eq!(prompt_spawns(&log_path, repo.path()), 2);
        // Outside a repository the prompt spawns nothing at all.
        assert_eq!(branch_segment(outside.path()), None);
        assert_eq!(prompt_spawns(&log_path, repo.path()), 2);
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
}
