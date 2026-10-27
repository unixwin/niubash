//! Live, file-backed history for the interactive Reedline session.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::SystemTime,
};

use reedline::{
    FileBackedHistory, History, HistoryItem, HistoryItemId, HistorySessionId, ReedlineError,
    Result, SearchDirection, SearchQuery,
};

/// On Windows, reset the DACL of the history file to grant the current
/// process full access. This handles the case where the file was created
/// by a different security context (e.g., Codex sandbox) and has
/// restrictive ACLs that cause "Access Denied" (os error 5) on read.
///
/// Setting a NULL DACL with the PROTECTED flag removes all access control,
/// which is safe for user-local state files like shell history.
#[cfg(windows)]
fn reset_history_file_dacl(path: &std::path::Path) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let path_wide: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        windows_sys::Win32::Security::Authorization::SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT,
            windows_sys::Win32::Security::DACL_SECURITY_INFORMATION
                | windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(), // pSidOwner
            std::ptr::null_mut(), // pSidGroup
            std::ptr::null_mut(), // pDacl (NULL = full access)
            std::ptr::null_mut(), // pSacl
        );
        // Best-effort: ignore errors; the caller will report the real I/O error if retry still fails.
    }
}

/// Best-effort fix: if the file exists and we suspect ACL issues, reset the DACL.
/// On non-Windows platforms this is a no-op.
fn ensure_history_file_accessible(path: &std::path::Path) {
    #[cfg(windows)]
    {
        // Only attempt if the file exists (Path::exists returns false on any error, including permission denied).
        // Try to get metadata; if it fails with PermissionDenied, fix the DACL.
        match std::fs::metadata(path) {
            Ok(_) => {} // File exists and is accessible – nothing to do.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                reset_history_file_dacl(path);
            }
            _ => {} // Not found or other error – nothing to fix here.
        }
    }
    let _ = path; // suppress unused-variable warning on non-windows
}

/// Adapter exposing the host Reedline history to Rubash builtins.
pub(crate) struct RubashHistoryProvider {
    inner: LiveFileBackedHistory,
}

impl std::fmt::Debug for RubashHistoryProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RubashHistoryProvider")
            .finish_non_exhaustive()
    }
}

impl Drop for RubashHistoryProvider {
    fn drop(&mut self) {
        // GNU bash ends every session that remembers history by writing
        // the final list to $HISTFILE (bashhist.c exit_shell ->
        // write_history; the shell overwrites HISTFILE unless histappend
        // is set). niu's hosted history syncs every save immediately, so
        // this write is normally a no-op — but it is also the only thing
        // that carries a GNU `history -c` to the file: the builtin clears
        // the in-memory list only (builtins/history.def:185, niubash#182),
        // and bash's file-side counterpart of a cleared list is exactly
        // this exit write. Best-effort like every history-file write: it
        // never fails the session and never reaches stderr. Sessions that
        // terminate through std::process::exit (nonzero `-c` statuses, the
        // exit-jump path) skip Drop and keep the pre-clear file, the same
        // as the pre-existing drop-time sync.
        self.flush_at_session_end();
    }
}

impl RubashHistoryProvider {
    /// Build the host history provider.
    ///
    /// Infallible by contract: a history file that cannot be opened (denied
    /// ACL, sandboxed restricted token, path is a directory, unwritable
    /// parent) degrades to an in-memory history with a `log::warn!`
    /// diagnostic instead of aborting shell startup. GNU bash treats the
    /// history file the same way: `bashhist.c:320 load_history()` reads the
    /// file only when `file_exists()` succeeds and tolerates every other
    /// I/O failure (it retries `read_history` only on `EINTR`), so an
    /// unreadable `HISTFILE` never stops the shell.
    pub(crate) fn with_file(capacity: usize, path: PathBuf) -> Self {
        Self {
            inner: LiveFileBackedHistory::with_file(capacity, path),
        }
    }

    /// Session-end history write (the GNU exit path). See
    /// [`LiveFileBackedHistory::flush_at_session_end`] for the semantics.
    pub(crate) fn flush_at_session_end(&mut self) {
        self.inner.flush_at_session_end();
    }

    /// The ledger shared with this provider's editor twin (niubash#569).
    pub(crate) fn ledger(&self) -> Arc<Mutex<SessionLedger>> {
        self.inner.ledger()
    }
}

impl rubash::history::HistoryProvider for RubashHistoryProvider {
    fn entries(&mut self) -> io::Result<Vec<String>> {
        let items = self
            .inner
            .search(SearchQuery::all_that_contain_rev(String::new()))
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(items
            .into_iter()
            .rev()
            .map(|item| item.command_line)
            .collect())
    }

    fn clear(&mut self) -> io::Result<()> {
        // GNU `history -c` (builtins/history.def:185) clears the in-memory
        // list ONLY and leaves the file untouched — the file is written by
        // `-a`/`-w` explicitly. Routing -c through reedline's truncating
        // clear erased the file, so a bash-it theme's per-prompt
        // `-c && -r` cycle loaded nothing back (niubash#182).
        self.inner
            .clear_memory()
            .map_err(|error| io::Error::other(error.to_string()))
    }

    fn append(&mut self, command: String) -> io::Result<()> {
        self.inner
            .save(HistoryItem::from_command_line(command))
            .map(|_| ())
            .map_err(|error| io::Error::other(error.to_string()))
    }

    fn replace(&mut self, entries: Vec<String>) -> io::Result<()> {
        // Full rewrite (`history -d`, the interactive recording veto, the
        // rewrite of a recorded raw line to its history expansion):
        // memory AND file end up holding exactly `entries`. This cannot be
        // a clear plus per-entry appends — reedline's save drops an entry
        // identical to the previous one, and the expansion rewrite
        // records a text identical to the previous entry by definition
        // (rubash#1248) — so it goes through the staging rebuild in
        // `LiveFileBackedHistory::replace_all`.
        self.inner
            .replace_all(entries)
            .map_err(|error| io::Error::other(error.to_string()))
    }

    fn write_history(&mut self, path: &str) -> io::Result<()> {
        ensure_history_file_accessible(std::path::Path::new(path));
        let entries = self.entries()?;
        if entries.is_empty() {
            std::fs::write(path, "")?;
        } else {
            // Encode embedded newlines the way reedline stores them, so
            // a multi-line entry survives the round trip (the old raw
            // join wrote literal newlines into the file).
            let encoded: Vec<String> = entries.iter().map(|e| encode_history_line(e)).collect();
            std::fs::write(path, encoded.join("\n") + "\n")?;
        }
        // niubash#569: -w commits this session's pending lines. To the
        // backing file it is an authoritative rewrite, so the cleared
        // set resets too and the base moves to the rewritten file; to a
        // foreign file only the pending set commits (the backing file
        // still holds the cleared lines for the session-end removal).
        let requested = std::path::Path::new(path);
        let is_backing = self
            .inner
            .backing_file()
            .is_some_and(|backing| backing == requested);
        let ledger = self.inner.ledger();
        if let Ok(mut ledger) = ledger.lock() {
            if is_backing {
                ledger.reset(entries.len());
            } else {
                ledger.commit();
            }
        }
        Ok(())
    }

    fn read_history(&mut self, path: &str) -> io::Result<()> {
        ensure_history_file_accessible(std::path::Path::new(path));
        // Fast path (niubash#182): when the requested file IS this history's
        // backing file, rebuild the reader from disk. The previous path
        // (replace = clear + per-entry saves) made reedline re-append the
        // whole file to itself on every save, so a bash-it theme's per-prompt
        // `history -c && history -r` doubled the history each cycle.
        let requested = std::path::Path::new(path);
        if self
            .inner
            .backing_file()
            .is_some_and(|backing| backing == requested)
            && self.inner.reload_from_file().is_ok()
        {
            return Ok(());
        }
        // Fallback (memory-only degraded session, or a foreign path): load
        // into the in-memory list only.
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let entries: Vec<String> = content
            .lines()
            .map(|l| l.to_string())
            .filter(|l| !l.trim().is_empty())
            .collect();
        self.replace(entries)
    }

    fn append_history(&mut self, path: &str) -> io::Result<()> {
        ensure_history_file_accessible(std::path::Path::new(path));
        // niubash#569/#570: -a appends this session's pending lines
        // (bashhist.c appends history_lines_this_session), judged by the
        // ledger, not by content. The old content check subtracted any
        // line already present in the file — which dropped a second
        // session's same-text line (#570) — and it re-appended lines the
        // live sync had already written, which is what broke the bash-it
        // theme interlock (niubash#182): codeword/gitline run
        // `history -a && history -c && history -r` from PROMPT_COMMAND
        // on every prompt, and re-appending the whole list doubled the
        // file each cycle, which `history -r` then loaded back into
        // memory — a self-amplifying loop that wedged the session.
        // To the backing file, only lines the live sync has not yet
        // written are appended (the common case is a no-op); to a foreign
        // file, all pending lines are appended (the foreign file never
        // saw the live sync). Either way the pending set commits: a
        // later -c must not remove lines -a already wrote.
        let requested = std::path::Path::new(path);
        let is_backing = self
            .inner
            .backing_file()
            .is_some_and(|backing| backing == requested);
        let ledger = self.inner.ledger();
        let to_append: Vec<String> = {
            let ledger = ledger
                .lock()
                .map_err(|_| io::Error::other("history ledger lock poisoned"))?;
            if is_backing {
                ledger
                    .pending
                    .iter()
                    .filter(|p| !p.synced)
                    .map(|p| p.text.clone())
                    .collect()
            } else {
                ledger.pending.iter().map(|p| p.text.clone()).collect()
            }
        };
        if to_append.is_empty() {
            // Still a commit: -a commits the pending set even when
            // there is nothing new to write (the common all-synced case).
            // Returning early without committing would leave the lines
            // pending, so a later -c would remove lines -a already
            // covered — the exact bug this ledger exists to prevent.
            if let Ok(mut ledger) = ledger.lock() {
                ledger.commit();
            }
            return Ok(());
        }
        for line in &to_append {
            append_line_to_file(std::path::Path::new(path), line)?;
        }
        if is_backing {
            // The backing file grew outside the reader's accounting:
            // rebuild it so a later reedline sync does not re-append
            // these lines (the #182 dedupe hazard). An accounting
            // rebuild, not a re-read: a clear's view hide survives it
            // (niubash#614).
            let _ = self.inner.reload_from_file_keeping_view();
        }
        // Commit only after the append succeeded: on failure the lines
        // stay pending, so a later -a or the session-end write can still
        // place them (committing first would lose them from the file).
        if let Ok(mut ledger) = ledger.lock() {
            ledger.commit();
        }
        Ok(())
    }

    fn read_new_history(&mut self, path: &str) -> io::Result<()> {
        ensure_history_file_accessible(std::path::Path::new(path));
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let existing = self.entries()?;
        let existing_set: std::collections::HashSet<&str> =
            existing.iter().map(|s| s.as_str()).collect();
        let missing: Vec<String> = content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !existing_set.contains(l.as_str()))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        // Fast path (niubash#182, corrected niubash#571): the missing lines
        // are, by construction, lines already in the backing file that this
        // session's reader has not seen yet — so the only correct fast path
        // is rebuilding the reader from disk, exactly like `read_history`'s
        // fast path. Appending them to the file first (the pre-#571 shape)
        // duplicated every one of them: a second session's line landed twice
        // after a single `history -n`. Routing them through per-entry saves
        // is still wrong for the original #182 reason (reedline re-appends
        // its whole unwritten buffer on every save), so the reload is the
        // whole fix, not an optimization on top of an append.
        let requested = std::path::Path::new(path);
        if self
            .inner
            .backing_file()
            .is_some_and(|backing| backing == requested)
            && self.inner.reload_from_file().is_ok()
        {
            return Ok(());
        }
        // Fallback (memory-only degraded session, or a foreign path).
        for line in missing {
            self.append(line)?;
        }
        Ok(())
    }
}

const HISTORY_LOCK_ERROR: &str = "history mutex is poisoned";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FileSignature {
    exists: bool,
    len: u64,
    modified: Option<SystemTime>,
}

struct HistoryState {
    history: FileBackedHistory,
    signature: FileSignature,
    /// File backing for this history. `None` once the shell degraded to a
    /// memory-only history because the file was or became inaccessible.
    path: Option<PathBuf>,
    /// Set once an I/O failure has been reported for the current file
    /// backing, so a degraded session does not warn on every command. GNU
    /// bash stays silent about history file I/O failures.
    warned_io_failure: bool,
    /// niubash#614: length of the raw reader prefix hidden from list
    /// queries by this session's latest `history -c`. The raw entries
    /// still hold the cleared lines (the file keeps them until the
    /// session-end write, niubash#569); list queries answer from the
    /// suffix past this prefix instead.
    hidden_prefix: usize,
    /// The ledger view generation `hidden_prefix` was taken from (see
    /// [`SessionLedger::note_cleared`]).
    applied_view_gen: u64,
}

/// A Reedline file history that is visible across concurrently running shells.
///
/// Reedline's `FileBackedHistory` reads the file once and only syncs on drop.
/// That is fine for a single process, but makes another terminal invisible
/// until one of the shells exits. This wrapper syncs after saves and reloads
/// the file before queries when another process has changed it.
///
/// The history file is auxiliary state: a sandboxed restricted token may be
/// able to execute commands while being unable to open, stat, or update the
/// user's profile history file. Every file failure therefore degrades the
/// backing to memory-only with a `log::warn!` diagnostic instead of failing
/// the operation or the shell, mirroring GNU bash's tolerance in
/// `bashhist.c:320 load_history()`.
pub(crate) struct LiveFileBackedHistory {
    capacity: usize,
    state: Mutex<HistoryState>,
    /// Shared with the provider/editor twin over the same file (niubash#569).
    ledger: Arc<Mutex<SessionLedger>>,
}

/// `FileBackedHistory::new` only rejects `capacity == usize::MAX`; clamp so
/// the memory-only fallback is always constructible.
fn in_memory_history(capacity: usize) -> FileBackedHistory {
    let clamped = capacity.min(usize::MAX - 1);
    FileBackedHistory::new(clamped)
        .unwrap_or_else(|_| FileBackedHistory::new(reedline::HISTORY_SIZE).expect("history"))
}

/// Report a history-file I/O failure once per file backing. The message goes
/// through `log::warn!` so `-c`/script stdout and stderr stay byte-stable.
fn warn_io_failure(state: &mut HistoryState, context: &str, error: impl std::fmt::Display) {
    if !state.warned_io_failure {
        state.warned_io_failure = true;
        log::warn!("{context}: {error}");
    }
}

/// Best-effort staleness signature: an inaccessible file yields the default
/// (nonexistent) signature instead of an error, with a one-time warning.
fn file_signature_lossy(path: &Path, state: &mut HistoryState) -> FileSignature {
    match file_signature(path) {
        Ok(signature) => signature,
        Err(error) => {
            warn_io_failure(
                state,
                &format!("history file {} is not accessible", path.display()),
                error,
            );
            FileSignature::default()
        }
    }
}

impl LiveFileBackedHistory {
    /// Build the live history. Infallible: a history file that cannot be
    /// opened degrades to an in-memory history (see the struct docs for the
    /// GNU citation). A file that opens but cannot be stat-ed (restricted
    /// token denying `metadata` while `CreateFileW` succeeded) degrades the
    /// same way instead of aborting shell startup with `os error 5`.
    ///
    /// There is exactly one sharing semantics (niubash#572): every session
    /// refreshes from the shared file, so cross-session lines are visible
    /// to navigation and builtins alike — the retired NIU_HISTORY_MODE
    /// switch and its private-mode capacity split are gone.
    pub(crate) fn with_file(capacity: usize, path: PathBuf) -> Self {
        let base_lines = count_file_lines(&path);
        Self::with_shared_ledger(
            capacity,
            path,
            Arc::new(Mutex::new(SessionLedger::new(base_lines))),
        )
    }

    /// Build the live history over a ledger shared with a twin instance
    /// (niubash#569): in a REPL the provider instance and the editor
    /// history instance are distinct objects over one file, and only a
    /// shared ledger lets -c (provider side) account for lines the editor
    /// side recorded. The base comes from the ledger, not this call.
    pub(crate) fn with_shared_ledger(
        capacity: usize,
        path: PathBuf,
        ledger: Arc<Mutex<SessionLedger>>,
    ) -> Self {
        // On Windows, ensure the history file has accessible ACLs before opening.
        // Codex sandbox and similar environments may create files with restrictive
        // permissions that block read access (os error 5).
        ensure_history_file_accessible(&path);

        let state = match FileBackedHistory::with_file(capacity, path.clone()) {
            Ok(history) => {
                let mut state = HistoryState {
                    history,
                    signature: FileSignature::default(),
                    path: Some(path.clone()),
                    warned_io_failure: false,
                    hidden_prefix: 0,
                    applied_view_gen: 0,
                };
                // The file opened, but a restricted token can still deny the
                // metadata read; never let that abort the shell.
                state.signature = file_signature_lossy(&path, &mut state);
                state
            }
            Err(error) => {
                // History is auxiliary state. A restricted token may be
                // able to execute in the workspace while being unable to
                // open or update the user's profile history file. Keep the
                // shell usable with an in-memory history in that case.
                let mut state = HistoryState {
                    history: in_memory_history(capacity),
                    signature: FileSignature::default(),
                    path: None,
                    warned_io_failure: true,
                    hidden_prefix: 0,
                    applied_view_gen: 0,
                };
                warn_io_failure(
                    &mut state,
                    &format!(
                        "history file {} unavailable; using in-memory history",
                        path.display()
                    ),
                    error,
                );
                state
            }
        };
        Self {
            capacity,
            state: Mutex::new(state),
            ledger,
        }
    }

    /// The ledger shared with this history's twin instance, if any.
    pub(crate) fn ledger(&self) -> Arc<Mutex<SessionLedger>> {
        Arc::clone(&self.ledger)
    }

    fn lock(&self) -> Result<MutexGuard<'_, HistoryState>> {
        self.state
            .lock()
            .map_err(|_| ReedlineError::from(history_lock_error()))
    }

    fn lock_mut(&mut self) -> Result<&mut HistoryState> {
        self.state
            .get_mut()
            .map_err(|_| ReedlineError::from(history_lock_error()))
    }

    fn refresh_if_stale(capacity: usize, state: &mut HistoryState) {
        let Some(path) = state.path.clone() else {
            return;
        };

        // On Windows, fix ACLs before re-reading the file from disk.
        ensure_history_file_accessible(&path);

        let signature = match file_signature(&path) {
            Ok(signature) => signature,
            Err(error) => {
                warn_io_failure(
                    state,
                    &format!("history file {} became inaccessible", path.display()),
                    error,
                );
                return;
            }
        };
        if signature == state.signature {
            return;
        }

        // Preserve commands submitted by this process before replacing the
        // in-memory view with the latest file contents.
        if let Err(error) = state.history.sync() {
            warn_io_failure(state, "history flush before reload failed", error);
        }
        match FileBackedHistory::with_file(capacity, path.clone()) {
            Ok(history) => {
                state.history = history;
                state.signature = file_signature_lossy(&path, state);
            }
            Err(error) => {
                // Keep the current in-memory view; the file stays stale for
                // this session instead of failing history operations.
                warn_io_failure(
                    state,
                    &format!("history file {} could not be reloaded", path.display()),
                    error,
                );
            }
        }
    }

    /// Apply the ledger's latest list-view generation to this instance
    /// (niubash#614). Reedline's sync/refresh always merges the whole
    /// file back into the reader, and the file deliberately keeps a
    /// cleared session's lines until the session-end write
    /// (niubash#569) — so without a view filter, the first list query
    /// after any later append rebuilt the reader from the file and the
    /// whole pre-clear list resurrected. The filter is a prefix length,
    /// not a text set: the live sync keeps the raw list equal to the
    /// file, so everything the clear emptied is exactly the raw prefix
    /// that predates it, in this instance and in the twin alike.
    fn sync_view(state: &mut HistoryState, ledger: &Mutex<SessionLedger>) {
        if let Ok(ledger) = ledger.lock() {
            if ledger.view_gen != state.applied_view_gen {
                state.hidden_prefix = ledger.view_hidden_len;
                state.applied_view_gen = ledger.view_gen;
            }
        }
    }

    /// The hidden prefix clamped to the reader's current length, plus
    /// that length. If the file shrank below the clear point (a foreign
    /// rewrite), everything currently in the list predates the clear.
    fn view_bounds(state: &HistoryState) -> (usize, usize) {
        let raw_len = state
            .history
            .count(SearchQuery::all_that_contain_rev(String::new()))
            .map(|count| count as usize)
            .unwrap_or(0);
        (state.hidden_prefix.min(raw_len), raw_len)
    }

    /// Answer a list query from the post-clear view (niubash#614). The
    /// query runs as a full scan through the reader's own filter
    /// machinery — so time and extra-info filters keep the reader's
    /// exact behavior, including its unsupported-feature errors — then
    /// the hidden prefix is dropped, ids are re-based onto the view,
    /// and the original id window and limit are applied in view space
    /// with the reader's own window arithmetic (file_backed.rs: bounds
    /// are exclusive outside, inclusive inside; a non-positive limit
    /// behaves as unbounded, matching its `as usize` wrap).
    fn view_search(state: &HistoryState, query: SearchQuery) -> Result<Vec<HistoryItem>> {
        let (hidden, raw_len) = Self::view_bounds(state);
        let view_len = raw_len - hidden;
        let direction = query.direction;
        let (min_id, max_id) = {
            let start = query.start_id.map(|id| id.0);
            let end = query.end_id.map(|id| id.0);
            if let SearchDirection::Backward = direction {
                (end, start)
            } else {
                (start, end)
            }
        };
        let min_id = min_id.map(|id| id + 1).unwrap_or(0);
        let max_id = max_id.map(|id| id - 1).unwrap_or(view_len as i64 - 1);
        let intrinsic_limit = max_id - min_id + 1;
        let limit = match query.limit {
            Some(given) => std::cmp::min(intrinsic_limit, given),
            None => intrinsic_limit,
        };
        let scan = SearchQuery {
            direction,
            start_time: query.start_time,
            end_time: query.end_time,
            start_id: None,
            end_id: None,
            limit: None,
            filter: query.filter,
        };
        let mut items: Vec<HistoryItem> = state
            .history
            .search(scan)?
            .into_iter()
            .filter_map(|mut item| {
                let raw_id = item.id?.0;
                if raw_id < hidden as i64 {
                    return None;
                }
                item.id = Some(HistoryItemId::new(raw_id - hidden as i64));
                Some(item)
            })
            .collect();
        if max_id < 0 || min_id > view_len as i64 - 1 {
            return Ok(Vec::new());
        }
        items.retain(|item| {
            let id = item.id.map(|id| id.0).unwrap_or(-1);
            id >= min_id && (intrinsic_limit <= 0 || id - min_id < intrinsic_limit)
        });
        let take = if limit < 0 {
            usize::MAX
        } else {
            limit as usize
        };
        items.truncate(take);
        Ok(items)
    }

    /// Sync the reader to the file and report whether the file actually
    /// received the entries (niubash#569: the ledger needs this to know
    /// which saved lines are already in the file and which are still
    /// pending). A memory-only session has no file, so it never syncs.
    /// The failure itself is still swallowed: GNU bash never fails a
    /// command because the history file cannot be written — append/exit
    /// writes report nothing (bashhist.c), and only the explicit
    /// `history -w`/`-a` builtins surface I/O errors. Keep the entries in
    /// memory, warn once, and let later reads still see them.
    fn sync_state(state: &mut HistoryState) -> bool {
        let synced = match state.path {
            Some(_) => match state.history.sync() {
                Ok(()) => true,
                Err(error) => {
                    warn_io_failure(state, "history file sync failed", error);
                    false
                }
            },
            None => false,
        };
        let path = state.path.clone();
        state.signature = match path.as_deref() {
            Some(path) => file_signature_lossy(path, state),
            None => FileSignature::default(),
        };
        synced
    }

    /// Replace the in-memory view with the file's contents WITHOUT writing
    /// anything back — the provider-side `history -r`/`-n` fast path.
    ///
    /// Re-feeding the file through per-entry saves cannot be used here:
    /// reedline's `sync` appends its unwritten buffer to the file verbatim
    /// (no dedupe), so a `-c`+`-r` cycle re-appended the entire file every
    /// time it ran — the second half of the bash-it theme interlock
    /// (niubash#182). Rebuilding the reader from disk (as
    /// `refresh_if_stale` already does) leaves the file byte-identical,
    /// matching histfile.c read_history_range, which only ever appends to
    /// the in-memory list.
    ///
    /// Fails (without touching state) when this session degraded to
    /// memory-only history, so the caller can fall back to the in-memory
    /// replace path.
    pub(crate) fn reload_from_file(&mut self) -> Result<()> {
        self.reload_from_file_inner(true)
    }

    /// Reader rebuild that is NOT an explicit re-read — the `history
    /// -a` accounting rebuild in `append_history`, which only repairs
    /// the reader's on-disk accounting after appending pending lines
    /// itself. The list view is untouched, so a clear's hide survives
    /// it (niubash#614): `-a` never loads lines into the list in GNU.
    pub(crate) fn reload_from_file_keeping_view(&mut self) -> Result<()> {
        self.reload_from_file_inner(false)
    }

    fn reload_from_file_inner(&mut self, consume_clear: bool) -> Result<()> {
        let capacity = self.capacity;
        // Clone the ledger handle before taking the state lock (see save).
        let ledger = Arc::clone(&self.ledger);
        let state = self.lock_mut()?;
        let Some(path) = state.path.clone() else {
            return Err(ReedlineError::from(history_lock_error()));
        };
        match FileBackedHistory::with_file(capacity, path.clone()) {
            Ok(history) => {
                state.history = history;
                state.signature = file_signature_lossy(&path, state);
                if consume_clear {
                    // An explicit re-read (`history -r`/`-n`) loads the
                    // file back into the list on purpose, so the clear's
                    // view hide ends here — for both instances, via the
                    // ledger generation (niubash#614).
                    if let Ok(mut ledger) = ledger.lock() {
                        ledger.note_view_reloaded();
                    }
                    Self::sync_view(state, &ledger);
                }
                Ok(())
            }
            Err(error) => Err(ReedlineError::from(io::Error::other(error.to_string()))),
        }
    }

    /// Replace the whole list with exactly `entries` — the
    /// `HistoryProvider::replace` contract (`history -d`, the interactive
    /// recording veto, and pre_process_line's rewrite of a recorded raw
    /// line to its history expansion).
    ///
    /// This cannot be built from per-entry saves: reedline's
    /// `FileBackedHistory::save` drops an entry identical to the one
    /// before it ("only saves a value if it's different than the last
    /// value") — editor UX for typed lines, not list semantics. GNU's
    /// history list holds consecutive duplicates, and the expansion
    /// rewrite records a text that is by definition equal to the previous
    /// entry (`!!` expands to it), so a clear-and-save rewrite silently
    /// lost exactly that entry (rubash#1248). Reedline's load path
    /// applies no such filter, so the exact list is staged through a
    /// file and the reader rebuilt from it — the same rebuild
    /// [`Self::reload_from_file`] performs. A degraded memory-only
    /// session stages through a scratch file in the temp dir and keeps
    /// the rebuilt reader bound to it (the wrapper still reports no
    /// backing file); if even the staging fails there, fall back to the
    /// clear-and-save rewrite rather than failing the caller.
    pub(crate) fn replace_all(&mut self, entries: Vec<String>) -> Result<()> {
        let capacity = self.capacity;
        // Clone the ledger handle before taking the state lock (see save).
        let ledger = Arc::clone(&self.ledger);
        let state = self.lock_mut()?;
        let staged_path = match state.path.clone() {
            Some(path) => path,
            None => std::env::temp_dir()
                .join(format!("niubash-history-stage-{}.tmp", std::process::id())),
        };
        // Reedline's on-disk escaping (file_backed.rs `encode_entry`): an
        // embedded newline is stored as the literal four characters
        // `<\n>`, and the load path decodes it back.
        let mut text = String::new();
        for entry in &entries {
            text.push_str(&entry.replace('\n', "<\\n>"));
            text.push('\n');
        }
        let rebuilt = std::fs::write(&staged_path, text)
            .map_err(ReedlineError::from)
            .and_then(|()| FileBackedHistory::with_file(capacity, staged_path));
        match rebuilt {
            Ok(history) => {
                state.history = history;
                if let Some(path) = state.path.clone() {
                    state.signature = file_signature_lossy(&path, state);
                }
                // niubash#569: the file now holds exactly `entries`, so
                // this session's pending and cleared sets both reset and
                // the base moves to the rewritten file's line count.
                // niubash#614: the reset also ends any clear hide — the
                // rewritten list is the view, in full.
                if let Ok(mut ledger) = ledger.lock() {
                    ledger.reset(entries.len());
                }
                Self::sync_view(state, &ledger);
                Ok(())
            }
            Err(error) => {
                if state.path.is_some() {
                    return Err(error);
                }
                // Memory-only staging failed: history is auxiliary state,
                // so degrade to the clear-and-save rewrite (which cannot
                // hold consecutive duplicates) instead of failing.
                state.history.clear()?;
                for entry in entries {
                    let _ = state.history.save(HistoryItem::from_command_line(entry));
                }
                Self::sync_state(state);
                // The rewrite is authoritative here too (niubash#614).
                if let Ok(mut ledger) = ledger.lock() {
                    ledger.note_view_reloaded();
                }
                Self::sync_view(state, &ledger);
                Ok(())
            }
        }
    }

    /// GNU `history -c`: empty the in-memory list WITHOUT touching the
    /// backing file (builtins/history.def:185 clears entries/timestamps
    /// only; the file is written by `-a`/`-w` explicitly).
    ///
    /// Reedline's `FileBackedHistory::clear` also truncates the file, so
    /// the file bytes are snapshotted and restored around it
    /// (niubash#182). The signature is refreshed either way so
    /// `refresh_if_stale` does not mistake the restore for an external
    /// change and reload the list the theme just cleared.
    pub(crate) fn clear_memory(&mut self) -> Result<()> {
        let capacity = self.capacity;
        // Clone the ledger handle before taking the state lock (see save).
        let ledger = Arc::clone(&self.ledger);
        let state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, state);
        // niubash#614: the raw list length at clear time — post-refresh
        // the reader equals the file — is the prefix the ledger records
        // for the view hide (see SessionLedger::note_cleared).
        let view_len_at_clear = state
            .history
            .count(SearchQuery::all_that_contain_rev(String::new()))
            .map(|count| count as usize)
            .unwrap_or(0);
        let snapshot = state
            .path
            .as_deref()
            .and_then(|path| std::fs::read(path).ok());
        state.history.clear()?;
        let outcome = match (state.path.clone(), snapshot) {
            (Some(path), Some(bytes)) => {
                if std::fs::write(&path, &bytes).is_ok() {
                    state.signature = file_signature_lossy(&path, state);
                    Ok(())
                } else {
                    // Could not restore: degrade the signature so the next
                    // query reloads whatever is on disk.
                    state.signature = file_signature_lossy(&path, state);
                    Ok(())
                }
            }
            (path, _) => {
                state.signature = match path.as_deref() {
                    Some(path) => file_signature_lossy(path, state),
                    None => FileSignature::default(),
                };
                Ok(())
            }
        };
        // niubash#569: -c drops this session's pending lines from the
        // session's set. The file is untouched mid-session (the pinned
        // #182 behavior); the session-end write removes exactly these
        // lines from the file instead of truncating it. Committed lines
        // (-a/-w already ran) are not in pending, so they survive.
        // niubash#614: the same call records the view hide, applied to
        // this instance now and to the editor twin at its next query.
        if let Ok(mut ledger) = ledger.lock() {
            ledger.note_cleared(view_len_at_clear);
        }
        Self::sync_view(state, &ledger);
        outcome
    }

    /// The file this history is backed by, when it has one.
    pub(crate) fn backing_file(&self) -> Option<PathBuf> {
        self.state.lock().ok().and_then(|state| state.path.clone())
    }

    /// Session-end history write: leave the backing file holding exactly
    /// what the session's final in-memory list holds (the file-side
    /// counterpart of bashhist.c exit_shell -> write_history).
    ///
    /// Every provider save already syncs to the file, so the list and the
    /// file can only diverge through GNU `history -c`, which clears the
    /// in-memory list and deliberately leaves the file alone
    /// (builtins/history.def:185 — the bash-it theme `-c && -r` cycle
    /// reloads those lines mid-session, niubash#182). bash still ends the
    /// session by writing the list to HISTFILE, so a list that `-c`
    /// emptied truncates the file at exit; a non-empty final list takes
    /// the `-a` dedupe append instead of a blind overwrite, so lines
    /// another shell appended after our last sync are never rolled back.
    ///
    /// Best-effort like every history-file write: a failure warns once and
    /// never reaches stderr (`-c` one-shot stderr stays byte-stable).
    pub(crate) fn flush_at_session_end(&mut self) {
        let capacity = self.capacity;
        // Clone the ledger handle before taking the state lock (see save).
        let ledger = Arc::clone(&self.ledger);
        let Ok(state) = self.state.get_mut() else {
            return;
        };
        let Some(path) = state.path.clone() else {
            // Degraded memory-only session: no backing file to reconcile,
            // and none must be created.
            return;
        };
        // See the file as it is now: another shell may have appended since
        // the last sync, and the empty-list decision below must not erase
        // those lines.
        Self::refresh_if_stale(capacity, state);
        let entries: Vec<String> = match state
            .history
            .search(SearchQuery::all_that_contain_rev(String::new()))
        {
            Ok(items) => items
                .into_iter()
                .rev()
                .map(|item| item.command_line)
                .collect(),
            Err(error) => {
                warn_io_failure(state, "history list read at session end failed", error);
                return;
            }
        };
        // Snapshot the ledger without holding its lock across file I/O.
        let (unsynced, cleared, base_lines) = match ledger.lock() {
            Ok(ledger) => (
                ledger
                    .pending
                    .iter()
                    .filter(|p| !p.synced)
                    .map(|p| p.text.clone())
                    .collect::<Vec<String>>(),
                ledger.cleared.clone(),
                ledger.base_lines,
            ),
            Err(_) => (Vec::new(), Vec::new(), 0),
        };
        if entries.is_empty() && cleared.is_empty() {
            // Only an existing file needs truncating; a session without
            // history must not create an empty one. (Pinned by
            // session_end_write_carries_a_cleared_list_to_the_file: a
            // cleared list with nothing pending truncates, exactly as
            // before.) When cleared is non-empty the file is reconciled
            // by removal below instead — truncating would delete seeds
            // and foreign lines, which
            // session_end_write_never_rolls_back_foreign_file_lines
            // forbids.
            if !path.exists() {
                return;
            }
            if let Err(error) = std::fs::write(&path, "") {
                warn_io_failure(state, "history file truncate at session end failed", error);
                return;
            }
            if let Ok(mut ledger) = ledger.lock() {
                ledger.reset(0);
            }
            state.signature = file_signature_lossy(&path, state);
            return;
        }
        // niubash#569: reconcile the file with this session's ledger.
        // (a) Append pending lines the live sync never wrote (the
        // read-only-recovery shape: save succeeded in memory, sync
        // failed, and the file became writable again by exit time).
        // Write them THROUGH the reader, not by raw appends: reedline's
        // unwritten buffer holds exactly the lines whose live sync
        // failed, and sync() advances its on-disk accounting as it
        // writes them. A raw append leaves that accounting stale, so
        // the reader's Drop-time sync writes the same lines a second
        // time (reedline's FileBackedHistory syncs on drop) — the
        // duplication
        // session_end_write_appends_missing_entries_without_duplicates
        // catches where the read-only leg behaves (Windows CI; the
        // local uid-0 artifact masks the leg and hides this one).
        let mut appended_ok = unsynced.is_empty();
        let mut file_changed = false;
        if !unsynced.is_empty() {
            match state.history.sync() {
                Ok(()) => {
                    appended_ok = true;
                    file_changed = true;
                }
                Err(error) => {
                    warn_io_failure(state, "history file append at session end failed", error);
                }
            }
        }
        // (b) Remove exactly the lines this session cleared, as a
        // forward subsequence from the ledger base. Lines before the
        // base predate the session (seeds, other sessions' lines) and
        // are never touched; lines committed by -a/-w left the pending
        // set before -c, so they are not in `cleared` and survive.
        let mut removed_ok = cleared.is_empty();
        if !cleared.is_empty() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => {
                    let file_lines: Vec<String> = contents.lines().map(|l| l.to_string()).collect();
                    let kept = remove_cleared_lines(file_lines, &cleared, base_lines);
                    let removed = contents.lines().count() - kept.len();
                    if removed > 0 {
                        let rewritten = if kept.is_empty() {
                            String::new()
                        } else {
                            kept.join("\n") + "\n"
                        };
                        match std::fs::write(&path, rewritten) {
                            Ok(()) => {
                                removed_ok = true;
                                file_changed = true;
                            }
                            Err(error) => {
                                warn_io_failure(
                                    state,
                                    "history file clear at session end failed",
                                    error,
                                );
                            }
                        }
                    } else {
                        // Nothing to remove: the cleared lines are already
                        // gone (another session rewrote the file, or -w
                        // already truncated it). Nothing to reconcile.
                        removed_ok = true;
                    }
                }
                Err(error) => {
                    warn_io_failure(state, "history file read at session end failed", error);
                }
            }
        }
        if file_changed {
            // Rebuild the reader from the reconciled file so its
            // in-memory list matches the file (step (b) may have removed
            // lines the reader still holds). The replaced reader drops
            // with an empty unwritten buffer — step (a) synced it, or
            // there was nothing pending — so its Drop-time sync writes
            // nothing. The rebuild cannot resurrect cleared lines —
            // they are already gone from the file — and seeds
            // reappearing in memory are committed file lines, harmless
            // at session end.
            if let Ok(history) = FileBackedHistory::with_file(capacity, path.clone()) {
                state.history = history;
            }
        }
        if let Ok(mut ledger) = ledger.lock() {
            ledger.settle(appended_ok, removed_ok, count_file_lines(&path));
        }
        state.signature = file_signature_lossy(&path, state);
    }
}

/// Encode one history line exactly as reedline stores it on disk (an
/// embedded newline becomes the literal four characters `<\n>`).
fn encode_history_line(text: &str) -> String {
    text.replace('\n', "<\\n>")
}

/// Append one history line to the file using reedline's on-disk encoding,
/// without touching the reader. The reader's own accounting is rebuilt by
/// the caller afterwards, so this never leaves a stale `len_on_disk`.
fn append_line_to_file(path: &Path, text: &str) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", encode_history_line(text))?;
    Ok(())
}

/// Count the lines of a file the way reedline's load path counts entries
/// (a trailing partial line still counts). Used for ledger bases; an
/// unreadable file counts as empty.
fn count_file_lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|contents| contents.lines().count())
        .unwrap_or(0)
}

/// Remove `cleared` lines from raw (still-encoded) file lines as a
/// subsequence starting at `start`: for each cleared line in order, drop
/// its first occurrence at or after the previous drop. Lines before
/// `start` predate the session and are never its own; a cleared line with
/// no occurrence left is simply gone already. Multiplicity is honored:
/// clearing the same line twice removes two occurrences.
fn remove_cleared_lines(file_lines: Vec<String>, cleared: &[String], start: usize) -> Vec<String> {
    let encoded: Vec<String> = cleared.iter().map(|c| encode_history_line(c)).collect();
    let mut out = Vec::with_capacity(file_lines.len());
    let mut next = 0usize;
    for (index, line) in file_lines.into_iter().enumerate() {
        if index >= start && next < encoded.len() && line == encoded[next] {
            next += 1;
        } else {
            out.push(line);
        }
    }
    out
}

/// One line this session saved, plus whether it reached the backing file.
struct PendingLine {
    text: String,
    synced: bool,
}

/// A session's own history bookkeeping, shared between the provider
/// instance (which runs -a/-c/-w/-r and the session-end write) and the
/// editor history instance (which records typed lines). The two instances
/// are distinct objects over one file, so the ledger — not either
/// instance's memory — is what makes -c and the session-end write agree
/// about which lines this session owns.
///
/// GNU model this implements (bashhist.c): a session's typed lines are
/// "pending" until an explicit -a/-w commits them; `history -c` drops the
/// pending set, and the session-end write must not contain lines that
/// were pending at clear time. niu's live sync writes pending lines to
/// the file immediately (that is what makes them visible cross-session),
/// so -c's file effect is deferred to the session-end write, which
/// removes exactly the cleared lines instead of truncating the file.
pub(crate) struct SessionLedger {
    pending: Vec<PendingLine>,
    cleared: Vec<String>,
    base_lines: usize,
    /// niubash#614: generation counter for the session's list view.
    /// `-c` bumps it and records how many raw entries the clear hides
    /// (`view_hidden_len`); an explicit re-read (`-r`/`-n`) or an
    /// authoritative rewrite bumps it back to a zero hide. Each history
    /// instance applies the latest generation to its own reader before
    /// answering list queries, so lines this session cleared stay out
    /// of the in-session list even though they remain in the file until
    /// the session-end write removes them.
    view_gen: u64,
    view_hidden_len: usize,
}

impl SessionLedger {
    fn new(base_lines: usize) -> Self {
        Self {
            pending: Vec::new(),
            cleared: Vec::new(),
            base_lines,
            view_gen: 0,
            view_hidden_len: 0,
        }
    }

    fn note_saved(&mut self, text: String, synced: bool) {
        self.pending.push(PendingLine { text, synced });
    }

    /// Commit pending lines (-a/-w): they are part of the file now, so a
    /// later -c must not remove them.
    fn commit(&mut self) {
        self.pending.clear();
    }

    /// -c: pending lines leave the session's set and await removal from
    /// the file at session end. Committed lines are untouched.
    ///
    /// niubash#614: the clear also hides the whole in-memory list from
    /// this session's list view until an explicit re-read — GNU -c
    /// empties the list, seeds included. `view_len` is the raw entry
    /// count at clear time. The live sync keeps every instance's raw
    /// list equal to the file, so that count is a prefix length in each
    /// instance's raw list; lines appended afterwards (by any session)
    /// land past the prefix and stay visible.
    fn note_cleared(&mut self, view_len: usize) {
        self.cleared.extend(self.pending.drain(..).map(|p| p.text));
        self.view_hidden_len = view_len;
        self.view_gen += 1;
    }

    /// An explicit re-read (`history -r`/`-n`) loads file lines back
    /// into the list on purpose — the bash-it `-c && -r` cycle depends
    /// on it (niubash#182) — so the clear's view hide ends here.
    fn note_view_reloaded(&mut self) {
        self.view_hidden_len = 0;
        self.view_gen += 1;
    }

    /// Authoritative rewrite (-w to the backing file, replace): the file
    /// now holds exactly what memory holds, so both sets reset and the
    /// base moves to the rewritten file's line count.
    fn reset(&mut self, base_lines: usize) {
        self.pending.clear();
        self.cleared.clear();
        self.base_lines = base_lines;
        // The rewritten file IS the list now; a stale clear hide would
        // mask its first entries (niubash#614).
        self.view_hidden_len = 0;
        self.view_gen += 1;
    }

    /// Settle at session end: pending lines are committed when their file
    /// write succeeded; when it failed, the unsynced ones stay pending for
    /// a later explicit flush. Cleared lines are consumed when their
    /// removal succeeded.
    fn settle(&mut self, appended_ok: bool, removed_ok: bool, base_lines: usize) {
        if appended_ok {
            self.pending.clear();
        } else {
            self.pending.retain(|p| !p.synced);
        }
        if removed_ok {
            self.cleared.clear();
        }
        self.base_lines = base_lines;
    }
}

impl History for LiveFileBackedHistory {
    fn save(&mut self, item: HistoryItem) -> Result<HistoryItem> {
        let capacity = self.capacity;
        // Clone the ledger handle before taking the state lock: the lock
        // borrows `self` mutably, so the handle must already be owned.
        let ledger = Arc::clone(&self.ledger);
        let state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, state);
        let text = item.command_line.clone();
        if text.is_empty() {
            // Reedline drops empty saves, and GNU records no empty lines
            // either — keep that one filter.
            return Ok(item);
        }
        // niubash#570: reedline's save drops an entry identical to the
        // previous one, but GNU history is positional — consecutive
        // duplicates in one session, and the same line typed in two
        // sessions, are all kept (dual-same probe: two sessions typing
        // `echo SAMECMD` must leave two copies in the file). When the new
        // line equals the last one, bypass reedline's filter: append the
        // encoded line to the file and rebuild the reader from disk. The
        // load path applies no duplicate filter, so the rebuilt list holds
        // both copies, and the returned item carries the new entry's id
        // (reedline's engine records it as `history_last_run_id`).
        let last_is_same = state
            .history
            .search(SearchQuery::last_with_search(
                reedline::SearchFilter::anything(None),
            ))
            .ok()
            .and_then(|mut items| items.pop())
            .is_some_and(|last| last.command_line == text);
        if last_is_same {
            if let Some(path) = state.path.clone() {
                if append_line_to_file(&path, &text).is_ok() {
                    if let Ok(history) = FileBackedHistory::with_file(capacity, path.clone()) {
                        state.history = history;
                        state.signature = file_signature_lossy(&path, state);
                        // The line is in the file now, by construction.
                        if let Ok(mut ledger) = ledger.lock() {
                            ledger.note_saved(text, true);
                        }
                        let saved = state
                            .history
                            .search(SearchQuery::last_with_search(
                                reedline::SearchFilter::anything(None),
                            ))
                            .ok()
                            .and_then(|mut items| items.pop())
                            .unwrap_or(item);
                        return Ok(saved);
                    }
                }
            }
            // Degraded fallback (no backing file, or the file could not be
            // appended/rebuilt): reedline's drop stands, exactly the
            // pre-existing behavior for a session with no file to write.
            // Nothing is recorded: the line is in neither memory nor file.
            // (The save takes the item by value and its result is dropped,
            // so the original is cloned for it and returned as-is.)
            let _ = state.history.save(item.clone());
            Self::sync_state(state);
            return Ok(item);
        }
        let saved = state.history.save(item)?;
        let synced = Self::sync_state(state);
        if let Ok(mut ledger) = ledger.lock() {
            ledger.note_saved(text, synced);
        }
        Ok(saved)
    }

    fn load(&self, id: HistoryItemId) -> Result<HistoryItem> {
        let capacity = self.capacity;
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &mut state);
        Self::sync_view(&mut state, &self.ledger);
        let (hidden, _) = Self::view_bounds(&state);
        if hidden == 0 {
            return state.history.load(id);
        }
        // niubash#614: view ids are raw ids shifted by the hidden
        // prefix. The view is a suffix of the raw list, so an id
        // outside the view is outside the raw list too and the
        // reader's own "Item does not exist" error stands.
        let mut item = state
            .history
            .load(HistoryItemId::new(id.0 + hidden as i64))?;
        item.id = Some(id);
        Ok(item)
    }

    fn count(&self, query: SearchQuery) -> Result<i64> {
        let capacity = self.capacity;
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &mut state);
        Self::sync_view(&mut state, &self.ledger);
        if state.hidden_prefix == 0 {
            return state.history.count(query);
        }
        Ok(Self::view_search(&state, query)?.len() as i64)
    }

    fn search(&self, query: SearchQuery) -> Result<Vec<HistoryItem>> {
        let capacity = self.capacity;
        let mut state = self.lock()?;
        Self::refresh_if_stale(capacity, &mut state);
        Self::sync_view(&mut state, &self.ledger);
        if state.hidden_prefix == 0 {
            return state.history.search(query);
        }
        Self::view_search(&state, query)
    }

    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> Result<()> {
        let capacity = self.capacity;
        let state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, state);
        state.history.update(id, updater)
    }

    fn clear(&mut self) -> Result<()> {
        let capacity = self.capacity;
        let state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, state);
        state.history.clear()?;
        // The reader is empty now; a stale view hide must not mask
        // entries recorded after this clear (niubash#614).
        state.hidden_prefix = 0;
        let path = state.path.clone();
        state.signature = match path.as_deref() {
            Some(path) => file_signature_lossy(path, state),
            None => FileSignature::default(),
        };
        Ok(())
    }

    fn delete(&mut self, id: HistoryItemId) -> Result<()> {
        let capacity = self.capacity;
        let state = self.lock_mut()?;
        Self::refresh_if_stale(capacity, state);
        state.history.delete(id)
    }

    fn sync(&mut self) -> io::Result<()> {
        let state = self.state.get_mut().map_err(|_| history_lock_error())?;
        Self::sync_state(state);
        Ok(())
    }

    fn session(&self) -> Option<HistorySessionId> {
        self.state
            .lock()
            .ok()
            .map(|state| state.history.session())
            .unwrap_or(None)
    }
}

fn history_lock_error() -> io::Error {
    io::Error::other(HISTORY_LOCK_ERROR)
}

impl Drop for LiveFileBackedHistory {
    fn drop(&mut self) {
        let _ = self.sync();
    }
}

fn file_signature(path: &Path) -> io::Result<FileSignature> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(FileSignature {
            exists: true,
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(FileSignature::default()),
        #[cfg(windows)]
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            // On Windows, the history file may have restrictive ACLs from a
            // different security context (e.g., Codex sandbox). Reset the
            // DACL and retry once.
            reset_history_file_dacl(path);
            match fs::metadata(path) {
                Ok(metadata) => Ok(FileSignature {
                    exists: true,
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                }),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(FileSignature::default()),
                Err(e) => Err(e),
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reedline::{HistoryItem, SearchQuery};
    use rubash::history::HistoryProvider;

    #[test]
    fn unavailable_history_file_falls_back_to_memory() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("history-parent");
        std::fs::write(&parent, "not a directory").unwrap();
        let path = parent.join("history");

        let mut history = LiveFileBackedHistory::with_file(100, path);
        history
            .save(HistoryItem::from_command_line("echo fallback"))
            .unwrap();

        let commands = history
            .search(SearchQuery::all_that_contain_rev(String::new()))
            .unwrap()
            .into_iter()
            .map(|item| item.command_line)
            .collect::<Vec<_>>();
        assert_eq!(commands, vec!["echo fallback"]);
        assert_eq!(std::fs::read_to_string(parent).unwrap(), "not a directory");
    }

    #[test]
    fn directory_history_path_degrades_to_memory() {
        // A directory where the history file should be is the classic
        // restricted-token / misconfigured-profile shape: the file cannot be
        // opened at all. The shell must still get a working in-memory
        // history (niubash#134: construction must never fail).
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history-dir");
        std::fs::create_dir_all(&path).unwrap();

        let mut history = LiveFileBackedHistory::with_file(100, path.clone());
        history
            .save(HistoryItem::from_command_line("echo dir"))
            .unwrap();

        let commands = history
            .search(SearchQuery::all_that_contain_rev(String::new()))
            .unwrap()
            .into_iter()
            .map(|item| item.command_line)
            .collect::<Vec<_>>();
        assert_eq!(commands, vec!["echo dir"]);
        assert!(path.is_dir(), "directory must be left untouched");
    }

    #[test]
    fn provider_construction_is_infallible_for_unopenable_files() {
        // The `-c` one-shot contract (README/dsh agents): the history
        // provider must construct even when the history path is unusable.
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("blocker");
        std::fs::write(&parent, "file, not a dir").unwrap();
        let mut provider = RubashHistoryProvider::with_file(100, parent.join(".niubash_history"));
        let entries = provider.entries().unwrap();
        assert!(entries.is_empty());
        provider.append("memory only".to_string()).unwrap();
        assert_eq!(provider.entries().unwrap(), vec!["memory only"]);
    }

    #[test]
    fn provider_replace_preserves_consecutive_duplicates() {
        // rubash#1248: the provider `replace` contract is an exact list
        // rewrite. The interactive expansion rewrite calls it with a list
        // whose last two entries are identical (the expansion of `!!`
        // replaces the recorded raw line) — reedline's save-level
        // duplicate filter must not eat the second one.
        use rubash::history::HistoryProvider as _;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.append("T1248=${T1248}alpha".to_string()).unwrap();
        provider.append("!!".to_string()).unwrap();
        provider
            .replace(vec![
                "T1248=${T1248}alpha".to_string(),
                "T1248=${T1248}alpha".to_string(),
            ])
            .unwrap();
        assert_eq!(
            provider.entries().unwrap(),
            ["T1248=${T1248}alpha", "T1248=${T1248}alpha"]
        );
        // A later append still lands after the rewritten pair.
        provider.append("echo next".to_string()).unwrap();
        assert_eq!(
            provider.entries().unwrap(),
            ["T1248=${T1248}alpha", "T1248=${T1248}alpha", "echo next"]
        );
        // And the backing file holds exactly the same three lines.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "T1248=${T1248}alpha\nT1248=${T1248}alpha\necho next\n"
        );
    }

    #[test]
    fn provider_replace_preserves_duplicates_when_memory_only() {
        // The degraded memory-only provider stages its replace through a
        // scratch file; the exact-list contract holds there too.
        use rubash::history::HistoryProvider as _;
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("blocker");
        std::fs::write(&parent, "file, not a dir").unwrap();
        let mut provider = RubashHistoryProvider::with_file(100, parent.join(".niubash_history"));
        provider.append("same".to_string()).unwrap();
        provider
            .replace(vec!["same".to_string(), "same".to_string()])
            .unwrap();
        assert_eq!(provider.entries().unwrap(), ["same", "same"]);
    }

    #[test]
    fn saved_entries_are_visible_to_another_history_instance() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut first = LiveFileBackedHistory::with_file(100, path.clone());
        let second = LiveFileBackedHistory::with_file(100, path);

        first
            .save(HistoryItem::from_command_line("cd first"))
            .unwrap();
        first
            .save(HistoryItem::from_command_line("cd second"))
            .unwrap();

        let matches = second
            .search(SearchQuery::all_that_contain_rev("cd".to_string()))
            .unwrap();
        let commands: Vec<_> = matches
            .into_iter()
            .map(|entry| entry.command_line)
            .collect();
        assert_eq!(commands, vec!["cd second", "cd first"]);
    }

    #[test]
    fn consecutive_duplicate_saves_are_kept_positionally() {
        // niubash#570: GNU history is positional — typing the same line
        // twice in a row keeps both copies. Reedline's save drops an entry
        // identical to the previous one, so the wrapper appends it to the
        // file directly and rebuilds the reader instead.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut history = LiveFileBackedHistory::with_file(100, path.clone());

        history
            .save(HistoryItem::from_command_line("echo same"))
            .unwrap();
        history
            .save(HistoryItem::from_command_line("echo same"))
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "echo same\necho same\n",
            "both consecutive copies must reach the file"
        );
        let commands = history
            .search(SearchQuery::all_that_contain_rev(String::new()))
            .unwrap()
            .into_iter()
            .map(|item| item.command_line)
            .collect::<Vec<_>>();
        assert_eq!(commands, vec!["echo same", "echo same"]);
    }

    #[test]
    fn same_line_from_two_instances_is_kept_twice() {
        // niubash#570 (dual-same probe): two sessions typing the identical
        // line must leave two copies in the shared file. Before the fix the
        // second session's save saw the first session's line as its last
        // entry and reedline dropped the second copy.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "SEEDONE\nSEEDTWO\n").unwrap();

        let mut first = LiveFileBackedHistory::with_file(100, path.clone());
        let mut second = LiveFileBackedHistory::with_file(100, path.clone());
        first
            .save(HistoryItem::from_command_line("echo SAMECMD"))
            .unwrap();
        second
            .save(HistoryItem::from_command_line("echo SAMECMD"))
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "SEEDONE\nSEEDTWO\necho SAMECMD\necho SAMECMD\n",
            "the shared file must hold both sessions' copies"
        );
        let commands = second
            .search(SearchQuery::all_that_contain_rev(String::new()))
            .unwrap()
            .into_iter()
            .map(|item| item.command_line)
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            vec!["echo SAMECMD", "echo SAMECMD", "SEEDTWO", "SEEDONE"]
        );
    }

    #[test]
    fn saving_history_updates_the_file_before_drop() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut history = LiveFileBackedHistory::with_file(100, path.clone());

        history
            .save(HistoryItem::from_command_line("echo live"))
            .unwrap();

        assert_eq!(std::fs::read_to_string(path).unwrap(), "echo live\n");
    }

    #[test]
    fn live_history_refreshes_from_later_external_writes() {
        // niubash#572: there is exactly one sharing semantics now. The
        // retired private mode used to ignore later external writes (its
        // refresh short-circuited for every non-shared mode), and the
        // retired session mode documented — but did not implement — that
        // "builtins can observe later file updates". Both promises are
        // replaced by the uniform rule: a query refreshes from the file,
        // so another session's lines are visible here.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "old-one\nold-two\n").unwrap();

        let mut first = LiveFileBackedHistory::with_file(100, path.clone());
        first
            .save(HistoryItem::from_command_line("this-session"))
            .unwrap();

        // Another session appends after this one started.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"from-another-session\n")
            .unwrap();

        let commands = first
            .search(SearchQuery::everything(
                reedline::SearchDirection::Forward,
                None,
            ))
            .unwrap()
            .into_iter()
            .map(|item| item.command_line)
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            vec!["old-one", "old-two", "this-session", "from-another-session"]
        );
    }

    #[test]
    fn append_history_does_not_duplicate_already_persisted_entries() {
        // niubash#182: GNU `history -a` appends only what the file lacks
        // (bashhist.c appends history_lines_this_session). The live history
        // syncs every save to the file, so `-a` must be a no-op here —
        // bash-it themes (codeword/gitline) run `history -a` on every
        // prompt, and re-appending the whole list doubled the file each
        // cycle.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "from-disk\n").unwrap();

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.append("echo one".to_string()).unwrap();
        provider.append("echo two".to_string()).unwrap();

        provider.append_history(path.to_str().unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "from-disk\necho one\necho two\n",
            "entries already synced to the file are not re-appended"
        );

        // Idempotent: a second -a with nothing new must not re-append.
        provider.append_history(path.to_str().unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "from-disk\necho one\necho two\n",
            "a second -a must not duplicate entries"
        );
    }

    #[test]
    fn read_history_leaves_the_file_byte_identical() {
        // niubash#182 interlock: the theme cycle is `history -a && history
        // -c && history -r` per prompt. `-r` rebuilds the reader from disk
        // instead of re-saving each line (which re-appended the whole file
        // to itself), so the cycle reaches a fixed point.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "loaded-one\nloaded-two\n").unwrap();

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.read_history(path.to_str().unwrap()).unwrap();
        assert_eq!(
            provider.entries().unwrap(),
            vec!["loaded-one", "loaded-two"]
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "loaded-one\nloaded-two\n",
            "-r must not write the file"
        );

        // The full theme cycle reaches a fixed point: -c clears memory, -r
        // re-reads the same file, and neither grows it.
        for _ in 0..3 {
            provider.clear().unwrap();
            provider.read_history(path.to_str().unwrap()).unwrap();
            provider.append_history(path.to_str().unwrap()).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "loaded-one\nloaded-two\n",
            "repeated -a/-c/-r cycles must not grow the history"
        );
    }

    #[test]
    fn read_new_history_appends_only_missing_lines_without_duplication() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "loaded-one\nloaded-two\nloaded-three\n").unwrap();

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.read_new_history(path.to_str().unwrap()).unwrap();
        assert_eq!(
            provider.entries().unwrap(),
            vec!["loaded-one", "loaded-two", "loaded-three"]
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "loaded-one\nloaded-two\nloaded-three\n",
            "-n must not duplicate lines already on disk"
        );
    }

    #[test]
    fn read_new_history_does_not_duplicate_lines_from_another_session() {
        // niubash#571: `history -n` exists to pick up lines another session
        // appended. Those lines are already in the backing file, so the
        // backing-file fast path must only reload the reader — appending
        // them first duplicated every picked-up line in the file.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "loaded-one\n").unwrap();

        let mut first = RubashHistoryProvider::with_file(100, path.clone());
        let mut second = RubashHistoryProvider::with_file(100, path.clone());
        second.append("from-second".to_string()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "loaded-one\nfrom-second\n"
        );

        first.read_new_history(path.to_str().unwrap()).unwrap();
        assert_eq!(first.entries().unwrap(), vec!["loaded-one", "from-second"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "loaded-one\nfrom-second\n",
            "-n must not duplicate the line it just picked up"
        );
    }

    #[test]
    fn clear_leaves_the_file_and_later_append_adds_only_new_entries() {
        // GNU `history -c` clears memory only; the file keeps its lines.
        // A later `history -a` appends exactly the new session entry.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.append("echo gone".to_string()).unwrap();
        provider.clear().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "echo gone\n",
            "-c must not touch the file"
        );
        assert!(provider.entries().unwrap().is_empty(), "-c clears memory");

        provider.append("echo kept".to_string()).unwrap();
        provider.append_history(path.to_str().unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "echo gone\necho kept\n",
            "-a appends only the new session entry"
        );

        // And the theme's -c/-r cycle still reaches a fixed point on it.
        for _ in 0..3 {
            provider.clear().unwrap();
            provider.read_history(path.to_str().unwrap()).unwrap();
            provider.append_history(path.to_str().unwrap()).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "echo gone\necho kept\n"
        );
    }

    #[test]
    fn append_history_after_write_does_not_duplicate() {
        // `history -w` writes the whole list; the next -a has nothing new.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.append("echo written".to_string()).unwrap();
        provider.write_history(path.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "echo written\n");

        provider.append_history(path.to_str().unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "echo written\n",
            "-a after -w must not duplicate"
        );
    }

    #[test]
    fn session_end_write_carries_a_cleared_list_to_the_file() {
        // Interactive `history -c` as the last command of a session leaves
        // an EMPTY history file. The
        // -c builtin itself clears memory only (builtins/history.def:185,
        // niubash#182); bash's file-side counterpart of a cleared list is
        // the exit write (bashhist.c exit_shell -> write_history), which
        // the provider performs when the session ends.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "kept-one\nkept-two\n").unwrap();

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.clear().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "kept-one\nkept-two\n",
            "-c itself must not touch the file"
        );

        drop(provider);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "",
            "the session-end write carries the cleared list to the file"
        );
    }

    #[test]
    fn session_end_write_never_rolls_back_foreign_file_lines() {
        // -c mid-session, then another shell appends to the file before
        // this session ends (the concurrent-shells contract of
        // LiveFileBackedHistory): the session-end write must keep those
        // lines instead of blindly overwriting with the cleared list.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "kept-one\n").unwrap();

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.clear().unwrap();
        std::fs::write(&path, "kept-one\nfrom-another-shell\n").unwrap();

        drop(provider);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "kept-one\nfrom-another-shell\n",
            "lines another shell appended survive the session-end write"
        );
    }

    #[test]
    fn session_end_write_removes_lines_cleared_this_session() {
        // niubash#569 (h5 probe): lines typed, then cleared with -c, then
        // the session exits — the live sync already wrote them to the
        // file mid-session, so the session-end write must remove exactly
        // those lines instead of leaving them to resurrect for the next
        // session. Lines typed after the clear survive.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "SEEDONE\nSEEDTWO\n").unwrap();

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.append("MARKER".to_string()).unwrap();
        provider.append("AFTER".to_string()).unwrap();
        provider.clear().unwrap();
        provider.append("KEPT".to_string()).unwrap();

        drop(provider);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "SEEDONE\nSEEDTWO\nKEPT\n",
            "cleared lines must not survive the session-end write"
        );
    }

    #[test]
    fn session_end_write_keeps_lines_committed_before_clear() {
        // niubash#569: -a commits pending lines, so a later -c must not
        // remove them — only lines still pending at clear time are
        // removed at session end.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "SEEDONE\nSEEDTWO\n").unwrap();

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.append("COMMITTED".to_string()).unwrap();
        provider.append_history(path.to_str().unwrap()).unwrap();
        provider.append("DOOMED".to_string()).unwrap();
        provider.clear().unwrap();

        drop(provider);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "SEEDONE\nSEEDTWO\nCOMMITTED\n",
            "-a-committed lines survive -c and the session-end write"
        );
    }

    #[test]
    fn session_end_write_after_clear_removes_only_cleared_lines() {
        // niubash#569: a session that cleared its only line and then
        // exits with an empty list must remove that line without
        // truncating the file — the legacy truncate branch applies only
        // when nothing was cleared (pinned by
        // session_end_write_carries_a_cleared_list_to_the_file).
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "SEEDONE\nSEEDTWO\n").unwrap();

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.append("GONE".to_string()).unwrap();
        provider.clear().unwrap();

        drop(provider);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "SEEDONE\nSEEDTWO\n",
            "the cleared line is gone but the seeds survive"
        );
    }

    #[test]
    fn shared_ledger_lets_the_twin_account_for_cleared_lines() {
        // niubash#569: in a REPL the provider and the editor are distinct
        // history objects over one file. Lines the first instance records
        // must be removable by the second instance's -c at session end —
        // which only works when both share one ledger.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "SEEDONE\n").unwrap();

        let mut editor = LiveFileBackedHistory::with_file(100, path.clone());
        let ledger = editor.ledger();
        let mut provider_view =
            LiveFileBackedHistory::with_shared_ledger(100, path.clone(), ledger);
        editor
            .save(HistoryItem::from_command_line("TYPED"))
            .unwrap();
        provider_view.clear_memory().unwrap();
        provider_view.flush_at_session_end();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "SEEDONE\n",
            "the twin's -c removes the line the editor recorded"
        );
    }

    #[test]
    fn session_end_write_appends_missing_entries_without_duplicates() {
        // A non-empty final list takes the -a dedupe append. The real
        // divergence it repairs: a transient mid-session sync failure (the
        // file locked by another process, a read-only DACL) left session
        // entries in memory only. Simulated here with a read-only file
        // while the second entry is saved.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.append("echo synced".to_string()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "echo synced\n");

        let original = std::fs::metadata(&path).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(&path, readonly).unwrap();
        provider.append("echo lost".to_string()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "echo synced\n",
            "the read-only file kept the session entry memory-only"
        );
        std::fs::set_permissions(&path, original).unwrap();

        drop(provider);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "echo synced\necho lost\n",
            "the session-end append restores only the missing entry"
        );
    }

    #[test]
    fn issue614_cleared_list_does_not_resurrect_on_later_queries() {
        // niubash#614: after `history -c`, the in-session list must stay
        // cleared even though the file keeps the cleared lines until the
        // session-end write (niubash#569) and every later save/query
        // refreshes the reader from that file. Before the fix, the
        // first query after any later append rebuilt the reader from
        // the file and the whole pre-clear list came back — GNU lists
        // only the post-clear lines (oracle: bash 5.3.20 pty probe,
        // seeds + `echo MARKER` + `history -c` + `history` lists just
        // `history`).
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "SEEDONE\nSEEDTWO\n").unwrap();

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        let mut editor =
            LiveFileBackedHistory::with_shared_ledger(100, path.clone(), provider.ledger());
        // The editor records the typed lines; the provider runs -c.
        editor
            .save(HistoryItem::from_command_line("echo MARKER"))
            .unwrap();
        editor
            .save(HistoryItem::from_command_line("history -c"))
            .unwrap();
        provider.clear().unwrap();
        assert!(
            provider.entries().unwrap().is_empty(),
            "-c empties the list"
        );

        // The next typed line is live-synced to the file, which makes
        // every later query refresh the reader from the file — the
        // exact moment the cleared lines used to resurrect.
        editor
            .save(HistoryItem::from_command_line("history"))
            .unwrap();
        assert_eq!(
            provider.entries().unwrap(),
            vec!["history"],
            "the cleared lines must not resurrect in the builtin list"
        );
        // The editor twin's own view (navigation) agrees.
        let editor_view = editor
            .search(SearchQuery::everything(
                reedline::SearchDirection::Forward,
                None,
            ))
            .unwrap()
            .into_iter()
            .map(|item| item.command_line)
            .collect::<Vec<_>>();
        assert_eq!(editor_view, vec!["history"]);

        editor
            .save(HistoryItem::from_command_line("echo AFTER"))
            .unwrap();
        assert_eq!(provider.entries().unwrap(), vec!["history", "echo AFTER"]);

        // Mid-session the file still holds the cleared lines: -c's
        // file effect stays deferred to the session-end write (#569).
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "SEEDONE\nSEEDTWO\necho MARKER\nhistory -c\nhistory\necho AFTER\n"
        );
        drop(editor);
        drop(provider);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "SEEDONE\nSEEDTWO\nhistory\necho AFTER\n",
            "the session-end write still removes exactly the cleared lines"
        );
    }

    #[test]
    fn issue614_clear_hides_only_what_predates_the_clear() {
        // niubash#614, dual-session interleave (the #583 report §5.1
        // probe shape): lines another session appended BEFORE this
        // session's -c are part of what the clear emptied and stay
        // hidden; lines appended AFTER the clear are new history and
        // show. The other session's own view is unaffected.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");

        let mut a = RubashHistoryProvider::with_file(100, path.clone());
        let mut b = RubashHistoryProvider::with_file(100, path.clone());
        b.append("B-BEFORE".to_string()).unwrap();
        a.append("A-OWN".to_string()).unwrap();
        a.clear().unwrap();
        b.append("B-AFTER".to_string()).unwrap();
        a.append("A-AFTER".to_string()).unwrap();

        assert_eq!(a.entries().unwrap(), vec!["B-AFTER", "A-AFTER"]);
        assert_eq!(
            b.entries().unwrap(),
            vec!["B-BEFORE", "A-OWN", "B-AFTER", "A-AFTER"]
        );
    }

    #[test]
    fn issue614_explicit_reread_restores_the_cleared_list() {
        // niubash#614: an explicit `history -r` after `-c` loads the
        // file back into the list on purpose (GNU; the bash-it
        // `-a && -c && -r` cycle depends on it, niubash#182), so the
        // clear's view hide ends at the re-read.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "loaded-one\nloaded-two\n").unwrap();

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.clear().unwrap();
        assert!(provider.entries().unwrap().is_empty());

        provider.read_history(path.to_str().unwrap()).unwrap();
        assert_eq!(
            provider.entries().unwrap(),
            vec!["loaded-one", "loaded-two"]
        );
        provider.append("echo new".to_string()).unwrap();
        assert_eq!(
            provider.entries().unwrap(),
            vec!["loaded-one", "loaded-two", "echo new"]
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "loaded-one\nloaded-two\necho new\n"
        );
    }

    #[test]
    fn session_end_write_skips_a_degraded_session() {
        // A memory-only session (unopenable file) has no backing file to
        // reconcile — and must not create one at session end.
        let temp = tempfile::tempdir().unwrap();
        let blocker = temp.path().join("blocker");
        std::fs::write(&blocker, "file, not a dir").unwrap();
        let path = blocker.join(".niubash_history");

        let mut provider = RubashHistoryProvider::with_file(100, path.clone());
        provider.append("echo memory".to_string()).unwrap();
        drop(provider);
        assert!(
            !path.exists(),
            "a degraded session must not materialize a history file"
        );
    }
}
